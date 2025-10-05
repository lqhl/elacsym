# Elacsym 设计文档

> 基于 Object Storage 的开源向量数据库 - MyScale 反向拼写

## 一、技术栈

| 组件 | 技术选型 | 说明 |
|------|---------|------|
| HTTP 框架 | `axum` | 高性能异步 Web 框架 |
| 对象存储 | `aws-sdk-s3` + 本地 FS | 支持 S3 兼容存储 + 本地开发 |
| 向量索引 | `rabitq-rs` | RaBitQ 量化 + HNSW/IVF |
| 混合缓存 | `foyer` | Memory + Disk 统一缓存 |
| 全文搜索 | `tantivy` | Rust 原生倒排索引 |
| 序列化 | `arrow` / `parquet` | 列式存储格式 |
| 配置 | `serde` + `config` | TOML/YAML 配置 |
| 日志 | `tracing` | 结构化日志 |

---

## 二、核心概念

### 2.1 数据模型

```
Namespace (命名空间)
  └── Documents (文档集合)
        ├── id: u64 (唯一标识)
        ├── vector: Vec<f32> (向量字段)
        └── attributes: HashMap<String, Value> (属性字段)
              ├── String
              ├── Number (i64, f64)
              ├── Boolean
              └── Array<String>
```

### 2.2 存储布局

```
S3/Object Storage:
  /{namespace}/
    ├── manifest.json              # 元数据：版本、schema、segment 列表
    ├── segments/
    │     ├── seg_00001.parquet    # 文档数据（列式存储）
    │     ├── seg_00002.parquet
    │     └── ...
    ├── indexes/
    │     ├── vector_index.bin     # RaBitQ 索引文件
    │     └── fulltext_index.bin   # Tantivy 索引文件
    └── wal/
          ├── 00001.log            # 写入日志（可选）
          └── 00002.log
```

---

## 三、核心流程设计

### 3.1 写入流程

#### 3.1.1 Upsert 接口

```rust
POST /v1/namespaces/{namespace}/upsert
Content-Type: application/json

{
  "upsert": [
    {
      "id": 1,
      "vector": [0.1, 0.2, 0.3, ...],  // 可选，如果省略则不更新向量
      "attributes": {
        "title": "Document title",
        "category": "tech",
        "published": true,
        "tags": ["rust", "database"]
      }
    },
    ...
  ]
}
```

#### 3.1.2 写入流程图

```
┌─────────────────────────────────────────────────────────────┐
│  Client Request: Upsert Documents                           │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 1: Validation & Schema Check                          │
│  - 检查 namespace 是否存在（不存在则创建）                    │
│  - 验证 vector 维度是否一致                                   │
│  - 验证 attributes 类型与 schema 是否匹配                     │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 2: Buffer in Memory                                    │
│  - 将文档暂存到内存缓冲区（按 namespace 分组）                │
│  - 达到阈值（如 10MB 或 10000 条）触发 flush                  │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 3: Write Segment to Object Storage                    │
│  - 转换为 Arrow RecordBatch                                  │
│  - 写入 Parquet 文件：                                        │
│    * id 列：UInt64                                           │
│    * vector 列：FixedSizeList<Float32>                       │
│    * attributes：每个属性一列                                │
│  - 上传到 S3: /{namespace}/segments/seg_{timestamp}.parquet │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 4: Update Indexes (Async)                             │
│  并行执行三个任务：                                           │
│  ┌────────────────────┐  ┌─────────────────────────────┐   │
│  │ 4.1 Vector Index   │  │ 4.2 Attribute Index         │   │
│  │ - 加载现有索引      │  │ - 更新内存中的 B-Tree/Hash  │   │
│  │ - rabitq.add()     │  │ - 支持范围查询、相等匹配     │   │
│  │ - 序列化并上传      │  └─────────────────────────────┘   │
│  └────────────────────┘                                      │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ 4.3 Full-Text Index (如果配置了 full-text 属性)       │   │
│  │ - Tantivy IndexWriter.add_document()                │   │
│  │ - Commit 并上传索引文件                               │   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 5: Update Manifest                                     │
│  - 读取现有 manifest.json                                    │
│  - 添加新 segment 信息：                                      │
│    * segment_id, file_path, row_count, min_id, max_id      │
│  - 更新 version (版本号递增)                                 │
│  - 原子性写入（先写 manifest.tmp，再 rename）                │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 6: Invalidate Cache                                    │
│  - 删除 foyer 中与该 namespace 相关的缓存项                   │
│  - 下次查询时会重新加载最新数据                               │
└─────────────────────────────────────────────────────────────┘
                          ↓
                  Return Success Response
```

#### 3.1.3 关键优化点

1. **批量写入**：内存缓冲区聚合小写入，减少 S3 PUT 次数
2. **异步索引更新**：Segment 写入后立即返回，索引在后台构建
3. **增量索引**：RaBitQ 支持增量添加，无需重建整个索引
4. **原子性**：Manifest 更新使用 write-then-rename 保证原子性

---

### 3.2 查询流程

#### 3.2.1 Query 接口

```rust
POST /v1/namespaces/{namespace}/query
Content-Type: application/json

{
  // 向量搜索（可选）
  "vector": [0.1, 0.2, 0.3, ...],
  "top_k": 10,
  "metric": "cosine",  // cosine | l2 | dot

  // 全文搜索（可选）
  "full_text": {
    "field": "title",
    "query": "rust database",
    "boost": 1.0  // BM25 权重
  },

  // 属性过滤（可选）
  "filter": {
    "type": "and",
    "conditions": [
      {"field": "category", "op": "eq", "value": "tech"},
      {"field": "published", "op": "eq", "value": true},
      {"field": "tags", "op": "contains", "value": "rust"}
    ]
  },

  // 返回字段
  "include_vector": false,
  "include_attributes": ["title", "category"]
}
```

---

### 3.3 纯向量搜索流程

```
┌─────────────────────────────────────────────────────────────┐
│  Client Request: Vector Search                              │
│  { "vector": [...], "top_k": 10 }                           │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 1: Load Manifest (with cache)                         │
│  - 尝试从 foyer 读取 manifest                                │
│  - Cache miss: 从 S3 下载并存入 foyer                        │
│  - 获取 segment 列表和索引位置                                │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 2: Load Vector Index                                  │
│  Key: "ns:{namespace}:vector_index"                         │
│  ┌──────────────────────────┐                               │
│  │ Cache Hit (Memory/Disk)  │                               │
│  │ - 直接返回索引对象        │                               │
│  └──────────────────────────┘                               │
│  ┌──────────────────────────┐                               │
│  │ Cache Miss               │                               │
│  │ - 从 S3 下载 index.bin    │                               │
│  │ - 反序列化为 RaBitQ Index │                               │
│  │ - 存入 foyer (Memory 层)  │                               │
│  └──────────────────────────┘                               │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 3: RaBitQ Search                                       │
│  - index.search(query_vector, top_k * 2)  // 过采样          │
│  - 返回候选集：Vec<(doc_id, distance)>                       │
│  - RaBitQ 使用量化 + HNSW，快速获得近似结果                   │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 4: Fetch Document Data                                │
│  - 根据 doc_id 确定所在的 segment                            │
│  - 批量读取相关 segments：                                    │
│    ┌────────────────────────────────┐                       │
│    │ 对每个 segment_id:             │                       │
│    │ - Key: "seg:{segment_id}"      │                       │
│    │ - 尝试从 foyer 读取             │                       │
│    │ - Miss: 从 S3 下载 parquet      │                       │
│    │ - 解析为 Arrow RecordBatch      │                       │
│    │ - 存入 foyer (Disk 层)          │                       │
│    └────────────────────────────────┘                       │
│  - 使用 Arrow compute API 过滤出目标行                        │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 5: Re-rank (可选)                                      │
│  - 使用完整向量重新计算距离（更精确）                          │
│  - 排序并取 top_k                                            │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 6: Construct Response                                  │
│  - 根据 include_vector, include_attributes 组装结果          │
│  - 返回 JSON                                                 │
└─────────────────────────────────────────────────────────────┘
```

**性能指标**：
- **热查询（缓存命中）**：< 20ms (1M 向量)
- **冷查询（缓存未命中）**：< 500ms (依赖 S3 延迟)

---

### 3.4 属性过滤查询流程

```
┌─────────────────────────────────────────────────────────────┐
│  Client Request: Filtered Query                             │
│  { "filter": {"field": "category", "op": "eq", "value": ... }}│
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 1: Parse Filter Expression                            │
│  - 解析为表达式树：                                           │
│    And(                                                      │
│      Eq("category", "tech"),                                │
│      Contains("tags", "rust")                               │
│    )                                                         │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 2: Attribute Index Lookup (可选优化)                   │
│  - 如果有属性索引（B-Tree/Hash），先过滤出候选 doc_id 集合    │
│  - 例如：category_index["tech"] → BitSet{1,5,9,...}         │
│  - 多个条件求交集/并集                                        │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 3: Scan Segments                                       │
│  策略选择：                                                   │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ 策略 A: 索引过滤（如果有属性索引）                      │  │
│  │ - 只读取候选 doc_id 所在的 segments                    │  │
│  │ - 使用 Parquet row group 统计信息跳过无关数据           │  │
│  └──────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ 策略 B: 全表扫描（无索引）                              │  │
│  │ - 遍历所有 segments                                    │  │
│  │ - 使用 Arrow compute 计算过滤表达式                    │  │
│  │ - Parquet 下推：利用 Parquet 统计信息提前剪枝          │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 4: Apply Filter on Arrow RecordBatch                   │
│  - 使用 arrow::compute::filter() 高效过滤                    │
│  - 返回满足条件的 doc_id 和 attributes                        │
└─────────────────────────────────────────────────────────────┘
                          ↓
                  Return Filtered Documents
```

**优化点**：
1. **Parquet 统计信息**：每个 row group 有 min/max 统计，可以跳过整个 row group
2. **属性索引**：高基数字段（如 user_id）建索引，快速定位
3. **Bloom Filter**：存储在 Parquet metadata，快速判断值是否存在

---

### 3.5 混合搜索流程（向量 + 全文 + 过滤）

```
┌─────────────────────────────────────────────────────────────┐
│  Client Request: Hybrid Search                              │
│  {                                                           │
│    "vector": [...],                                          │
│    "full_text": {"field": "title", "query": "rust db"},     │
│    "filter": {"field": "published", "op": "eq", "value": true}│
│  }                                                           │
└─────────────────────────────────────────────────────────────┘
                          ↓
        ┌─────────────────┴─────────────────┐
        ↓                                    ↓
┌───────────────────────┐      ┌─────────────────────────────┐
│ Path 1: Vector Search │      │ Path 2: Full-Text Search    │
│ (并行执行)             │      │ (并行执行)                   │
└───────────────────────┘      └─────────────────────────────┘
        ↓                                    ↓
┌───────────────────────┐      ┌─────────────────────────────┐
│ - Load vector index   │      │ - Load Tantivy index        │
│ - RaBitQ search       │      │ - Tantivy.search(query)     │
│ - 返回候选集合 A       │      │ - BM25 scoring              │
│   Vec<(id, distance)> │      │ - 返回候选集合 B             │
│                       │      │   Vec<(id, bm25_score)>     │
└───────────────────────┘      └─────────────────────────────┘
        ↓                                    ↓
        └─────────────────┬─────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 1: Merge Candidate Sets                               │
│  - 取两个集合的并集：candidate_ids = A.ids ∪ B.ids           │
│  - 保留各自的分数：                                           │
│    * vector_scores: Map<id, distance>                       │
│    * bm25_scores: Map<id, bm25_score>                       │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 2: Apply Attribute Filter                             │
│  - 对 candidate_ids 应用过滤条件                             │
│  - 使用属性索引或扫描 segments                                │
│  - 过滤后得到 filtered_ids                                   │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 3: Fetch Documents                                     │
│  - 批量读取 filtered_ids 对应的 segments                      │
│  - 获取完整向量和属性                                         │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 4: Hybrid Scoring (RRF - Reciprocal Rank Fusion)      │
│  对每个文档计算综合得分：                                      │
│                                                              │
│  score(id) = α * vector_score(id) + β * bm25_score(id)      │
│                                                              │
│  其中：                                                       │
│  - vector_score = 1 / (k + rank_vector(id))  // RRF         │
│  - bm25_score = 1 / (k + rank_bm25(id))                     │
│  - α, β 为权重参数（可配置，默认 0.5, 0.5）                  │
│  - k = 60 (RRF 常数)                                         │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│  Step 5: Re-rank & Return Top-K                              │
│  - 按综合得分排序                                             │
│  - 取 top_k 结果                                             │
│  - 构造响应                                                  │
└─────────────────────────────────────────────────────────────┘
```

**混合搜索策略**：

1. **Early Fusion（预过滤）**：
   - 先应用过滤条件，缩小候选集
   - 然后在过滤结果上做向量/全文搜索
   - 适用于过滤后结果集较小的场景

2. **Late Fusion（后融合）**：
   - 各路搜索独立执行
   - 最后融合结果并排序
   - 适用于过滤后结果集仍然较大的场景
   - **Elacsym 采用此策略**（与 Turbopuffer 一致）

---

### 3.6 缓存策略（Foyer 集成）

#### 3.6.1 缓存层次

```rust
use foyer::{HybridCacheBuilder, Cache};

// 初始化混合缓存
let cache = HybridCacheBuilder::new()
    .memory(4 * 1024 * 1024 * 1024)  // 4GB 内存
    .with_shards(16)
    .with_eviction_config(EvictionConfig::lru())
    .storage()
        .with_capacity(100 * 1024 * 1024 * 1024)  // 100GB SSD
        .with_device_options(DirectFsDeviceOptions::new("/data/cache"))
        .with_compression(CompressionAlgorithm::Zstd)
    .build()
    .await?;
```

#### 3.6.2 缓存键设计

| 数据类型 | Cache Key | 存储层 | TTL |
|---------|-----------|--------|-----|
| Manifest | `manifest:{namespace}` | Memory | 5 min |
| Vector Index | `vidx:{namespace}` | Memory | 30 min |
| Full-Text Index | `ftidx:{namespace}:{field}` | Memory | 30 min |
| Segment Data | `seg:{namespace}:{segment_id}` | Disk | 1 hour |
| Query Result | `query:{hash(request)}` | Memory | 1 min |

#### 3.6.3 缓存失效策略

```rust
// 写入时失效相关缓存
async fn invalidate_on_write(namespace: &str) {
    cache.remove(&format!("manifest:{}", namespace)).await;
    cache.remove(&format!("vidx:{}", namespace)).await;
    // Segment 缓存保留，因为是不可变的
}
```

---

## 四、关键数据结构

### 4.1 Manifest 格式

```json
{
  "version": 123,
  "namespace": "my_namespace",
  "schema": {
    "vector_dim": 768,
    "vector_metric": "cosine",
    "attributes": {
      "title": {"type": "string", "full_text": true},
      "category": {"type": "string", "indexed": true},
      "score": {"type": "float"},
      "published": {"type": "bool"}
    }
  },
  "segments": [
    {
      "segment_id": "seg_001",
      "file_path": "segments/seg_001.parquet",
      "row_count": 10000,
      "id_range": [1, 10000],
      "created_at": "2025-10-04T10:00:00Z"
    }
  ],
  "indexes": {
    "vector": "indexes/vector_index.bin",
    "full_text": {
      "title": "indexes/fulltext_title.bin"
    }
  },
  "stats": {
    "total_docs": 50000,
    "total_size_bytes": 1073741824
  }
}
```

### 4.2 RaBitQ 索引集成

```rust
use rabitq::{RaBitQIndex, RaBitQBuilder};

// 构建索引
let index = RaBitQBuilder::new()
    .dimension(768)
    .metric(Metric::Cosine)
    .build()?;

// 添加向量
for (id, vector) in vectors {
    index.add(id, &vector)?;
}

// 搜索
let results = index.search(&query_vector, top_k)?;
// results: Vec<(doc_id, distance)>
```

### 4.3 Tantivy 全文索引集成

```rust
use tantivy::*;

// 创建 schema
let mut schema_builder = Schema::builder();
schema_builder.add_u64_field("id", INDEXED | STORED);
schema_builder.add_text_field("title", TEXT | STORED);
let schema = schema_builder.build();

// 写入文档
let mut index_writer = index.writer(50_000_000)?;
index_writer.add_document(doc!(
    id => 1u64,
    title => "Rust vector database"
))?;
index_writer.commit()?;

// 搜索
let searcher = reader.searcher();
let query_parser = QueryParser::for_index(&index, vec![title]);
let query = query_parser.parse_query("rust database")?;
let top_docs = searcher.search(&query, &TopDocs::with_limit(10))?;
```

---

## 五、API 设计完整示例

### 5.1 创建/更新命名空间

```http
PUT /v1/namespaces/{namespace}
Content-Type: application/json

{
  "schema": {
    "vector_dim": 768,
    "vector_metric": "cosine",  // cosine | l2 | dot
    "attributes": {
      "title": {"type": "string", "full_text": true},
      "category": {"type": "string", "indexed": true},
      "score": {"type": "float"},
      "tags": {"type": "array<string>"}
    }
  }
}
```

### 5.2 批量写入

```http
POST /v1/namespaces/{namespace}/upsert
Content-Type: application/json

{
  "documents": [
    {
      "id": 1,
      "vector": [0.1, 0.2, ...],
      "attributes": {
        "title": "Rust Vector Database",
        "category": "tech",
        "score": 4.5,
        "tags": ["rust", "database"]
      }
    }
  ]
}
```

### 5.3 复杂查询示例

```http
POST /v1/namespaces/{namespace}/query
Content-Type: application/json

{
  "vector": [0.1, 0.2, ...],
  "top_k": 20,
  "full_text": {
    "fields": ["title"],
    "query": "vector search",
    "weight": 0.3
  },
  "filter": {
    "and": [
      {"field": "category", "op": "eq", "value": "tech"},
      {"field": "score", "op": "gte", "value": 4.0},
      {"field": "tags", "op": "contains_any", "value": ["rust", "go"]}
    ]
  },
  "include_vector": false,
  "include_attributes": ["title", "score"]
}

// Response
{
  "results": [
    {
      "id": 1,
      "score": 0.95,
      "attributes": {
        "title": "Rust Vector Database",
        "score": 4.5
      }
    }
  ],
  "took_ms": 23
}
```

---

## 六、性能优化要点

### 6.1 写入优化
- **批处理**：聚合小写入，减少 S3 API 调用
- **异步索引**：写入 segment 后立即返回，索引后台更新
- **压缩**：Parquet 使用 Snappy/Zstd 压缩

### 6.2 查询优化
- **缓存预热**：首次查询时异步预加载常用索引
- **并行读取**：多个 segments 并行下载
- **Range Fetch**：S3 Range GET 只读取需要的行
- **RaBitQ 量化**：降低内存占用，加速距离计算

### 6.3 成本优化
- **分层存储**：热数据 Memory，温数据 SSD，冷数据 S3
- **按需加载**：只在查询时加载 segments
- **索引压缩**：RaBitQ 二进制量化大幅减少索引大小

---

## 七、后续扩展方向

1. **分片支持**：按 ID range 或 hash 分片，支持水平扩展
2. **副本机制**：读副本提高查询吞吐
3. **增量更新优化**：WAL + 定期 compaction
4. **更多索引类型**：稀疏向量、多向量
5. **GPU 加速**：使用 CUDA 加速距离计算

---

## 八、实现路线图

### Phase 1: 单机 MVP (6-8 周)
- [x] 存储层：S3 + 本地 FS 抽象
- [x] Segment 管理：Parquet 读写
- [x] RaBitQ 索引集成
- [x] Foyer 缓存集成
- [x] 基础 API：Upsert + Vector Search

### Phase 2: 高级特性 (4-6 周)
- [ ] Tantivy 全文搜索
- [ ] 属性过滤和索引
- [ ] 混合搜索（RRF）
- [ ] Query 性能优化

### Phase 3: 生产就绪 (6-8 周)
- [ ] 分布式部署
- [ ] 监控和日志
- [ ] Benchmark 和压测
- [ ] 文档和示例

---

**讨论要点**：
1. RaBitQ 索引是否需要周期性 compaction？
2. 属性索引是全部建还是按需建？
3. 混合搜索的权重如何配置更友好？
4. 是否需要支持 update/delete 操作（需要 MVCC）？
