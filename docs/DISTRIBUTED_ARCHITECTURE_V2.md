# Elacsym 分布式架构设计 V2

> 简化版设计：无外部依赖，S3 作为唯一真相来源

**设计日期**: 2025-10-06
**状态**: ✅ 最终设计

---

## 一、核心设计原则

1. **每个 namespace 一个 indexer** - 通过一致性哈希分配，无需分布式锁
2. **S3 是唯一真相来源** - 不依赖 etcd/Consul/DynamoDB
3. **每次写入一个 WAL 文件** - 用户控制批量，系统保证原子性
4. **无 GlobalVectorIndex** - segments 数量少（个位数），直接并行查询

---

## 二、架构图

```
┌─────────────────────────────────────────────────────────────┐
│                    Load Balancer (ALB)                      │
│                   (Consistent Hashing)                       │
└─────────────────────────────────────────────────────────────┘
              │                              │
              │                              │
      ┌───────▼────────┐           ┌────────▼────────┐
      │  Query Node 1  │           │  Query Node 2   │
      │   (Stateless)  │           │   (Stateless)   │
      │                │           │                 │
      │  • 处理查询     │           │  • 处理查询      │
      │  • 读取索引     │           │  • 读取索引      │
      │  • 缓存优化     │           │  • 缓存优化      │
      └────────────────┘           └─────────────────┘
              │                              │
              └──────────┬───────────────────┘
                         │
              ┌──────────▼───────────────────┐
              │     Indexer Cluster          │
              │  (Namespace Sharding)        │
              │                              │
              │  Indexer 1: ns hash % 3 = 0 │
              │  Indexer 2: ns hash % 3 = 1 │
              │  Indexer 3: ns hash % 3 = 2 │
              └──────────┬───────────────────┘
                         │
              ┌──────────▼───────────────────┐
              │         S3 Storage           │
              │   (Source of Truth)          │
              │                              │
              │  • Manifests (versioned)     │
              │  • WAL files                 │
              │  • Segments + Indexes        │
              └──────────────────────────────┘
```

---

## 三、存储布局

```
s3://elacsym-data/
  /{namespace}/

    # 版本化 Manifest
    ├── manifests/
    │   ├── v00000001.json
    │   ├── v00000002.json
    │   ├── v00000123.json      # 最新版本
    │   └── current.txt         # 包含最新版本号 "123"

    # WAL（每次写入一个文件）
    ├── wal/
    │   ├── 1728129600000_indexer1.log   # {timestamp_ms}_{indexer_id}.log
    │   ├── 1728129601234_indexer1.log
    │   └── 1728129605678_indexer1.log

    # Segments（数据 + per-segment 索引）
    └── segments/
        # Segment 001
        ├── seg_001.parquet              # 文档数据
        ├── seg_001.rabitq               # 向量索引
        ├── seg_001_title.tantivy.tar.gz # 全文索引（title 字段）
        └── seg_001_desc.tantivy.tar.gz  # 全文索引（desc 字段）
```

---

## 四、Manifest 版本化设计

### 4.1 为什么需要版本化？

**问题**：S3 没有原生的 CAS（Compare-And-Swap）操作

**解决方案**：使用版本化文件名 + `current.txt` 指针

### 4.2 Manifest 结构

```json
// manifests/v00000123.json
{
  "version": 123,
  "namespace": "my_ns",
  "schema": { ... },
  "segments": [
    {
      "segment_id": "seg_001",
      "file_path": "segments/seg_001.parquet",
      "vector_index_path": "segments/seg_001.rabitq",
      "fulltext_index_paths": {
        "title": "segments/seg_001_title.tantivy.tar.gz"
      },
      "row_count": 10000,
      "id_range": [1, 10000],
      "tombstones": []
    }
  ],
  "created_at": "2025-10-06T12:00:00Z",
  "created_by": "indexer-1"
}
```

```
// manifests/current.txt
123
```

### 4.3 读取流程

```rust
// 1. 读取 current.txt 获取最新版本号
let current_version = storage.get("{namespace}/manifests/current.txt").await?;
let version: u64 = current_version.parse()?;

// 2. 读取对应版本的 manifest
let manifest_key = format!("{namespace}/manifests/v{version:08}.json");
let manifest = storage.get(&manifest_key).await?;

// 3. 如果读取失败（版本号更新了），重试
```

**优点**:
- 无需 CAS 操作
- S3 PUT 是原子的
- 可以保留历史版本（回滚、审计）

### 4.4 写入流程（乐观锁）

```rust
// 1. 读取当前版本
let current_version = read_current_version(namespace).await?;

// 2. 生成新版本
let new_version = current_version + 1;
let new_manifest_key = format!("{namespace}/manifests/v{new_version:08}.json");

// 3. 写入新 manifest
storage.put(&new_manifest_key, new_manifest_json).await?;

// 4. 原子更新 current.txt（最后一步）
// 注意：这里可能有并发冲突
storage.put("{namespace}/manifests/current.txt", new_version.to_string()).await?;

// 5. 如果其他 indexer 也在写，可能写入了 v124, v125...
//    但 current.txt 只会指向最后成功的版本
//    未指向的版本可以当作"草稿"，定期清理
```

**冲突处理**:
- 如果两个 indexer 同时写入（理论上不应该，因为有 namespace sharding）
- 会产生多个版本文件（如 v124, v125）
- `current.txt` 最终指向最后写入的版本
- 定期清理未使用的版本文件

---

## 五、Namespace 分片（一致性哈希）

### 5.1 为什么需要分片？

**目标**：每个 namespace 只有一个 indexer 负责写入

**方案**：一致性哈希

```rust
// 计算 namespace 应该分配给哪个 indexer
fn get_indexer_for_namespace(namespace: &str, num_indexers: usize) -> usize {
    let hash = seahash::hash(namespace.as_bytes());
    (hash % num_indexers as u64) as usize
}

// 示例
get_indexer_for_namespace("user_123", 3) -> indexer-1
get_indexer_for_namespace("product_db", 3) -> indexer-2
```

### 5.2 部署配置

**Indexer 节点**:
```toml
# indexer-1.toml
[indexer]
node_id = "indexer-1"
total_nodes = 3      # 集群总共 3 个 indexer
node_index = 0       # 本节点索引（0, 1, 2）
```

**路由逻辑**:
```rust
impl IndexerCluster {
    fn should_handle(&self, namespace: &str) -> bool {
        let target_index = get_indexer_for_namespace(namespace, self.total_nodes);
        target_index == self.node_index
    }
}

// Indexer 接收到写入请求
if !cluster.should_handle(namespace) {
    return Err(Error::WrongIndexer {
        namespace,
        correct_indexer: cluster.get_indexer_id(namespace),
    });
}
```

### 5.3 故障转移

**问题**：如果 indexer-1 宕机，负责的 namespaces 怎么办？

**方案 1**：手动重新分配
- 修改 `total_nodes = 2`（剩余节点）
- 重启所有 indexer
- namespace 重新哈希分配

**方案 2**：虚拟节点（一致性哈希环）
```rust
// 每个物理节点映射到多个虚拟节点
let virtual_nodes = vec![
    ("indexer-1-v1", "indexer-1"),
    ("indexer-1-v2", "indexer-1"),
    ("indexer-2-v1", "indexer-2"),
    // ...
];

// namespace 哈希到虚拟节点，再映射到物理节点
```

**方案 3**：备用 indexer（推荐）
- 保留一个 standby indexer
- 检测到故障后，standby 接管失败节点的范围

---

## 六、写入流程（带 WAL）

### 6.1 完整流程

```rust
// 用户调用 upsert API
POST /v1/namespaces/my_ns/upsert
{
  "upsert": [
    {"id": 1, "vector": [0.1, 0.2], "attributes": {"title": "Doc 1"}},
    {"id": 2, "vector": [0.3, 0.4], "attributes": {"title": "Doc 2"}}
  ]
}

// Indexer 处理流程:

1. 检查 namespace 归属
   if !should_handle("my_ns") {
       return 302 Redirect to correct indexer
   }

2. 写入 WAL 到 S3
   wal_key = "my_ns/wal/{timestamp}_{indexer_id}.log"
   storage.put(wal_key, serialize(WalOperation::Upsert { documents }))

   ✅ WAL 写入成功 = 数据已持久化（不会丢失）

3. 生成 Segment + Indexes
   - segment_path = "my_ns/segments/seg_{timestamp}.parquet"
   - vector_index_path = "my_ns/segments/seg_{timestamp}.rabitq"
   - fulltext_index_paths = { "title": "my_ns/segments/seg_{timestamp}_title.tantivy.tar.gz" }

   并行上传:
     storage.put(segment_path, parquet_data)
     storage.put(vector_index_path, rabitq_data)
     storage.put(fulltext_index_path, tantivy_data)

4. 更新 Manifest（乐观锁）
   current_version = read_current_version("my_ns")
   new_version = current_version + 1

   new_manifest = {
       "version": new_version,
       "segments": [...old_segments, new_segment_info]
   }

   storage.put("my_ns/manifests/v{new_version:08}.json", new_manifest)
   storage.put("my_ns/manifests/current.txt", new_version)

   ✅ Manifest 更新成功 = 数据已可见

5. 删除 WAL 文件
   storage.delete(wal_key)

6. 返回成功
```

### 6.2 WAL 格式（每个文件）

```
File: my_ns/wal/1728129600123_indexer1.log

[MessagePack Serialized WalOperation]
[CRC32 Checksum (4 bytes)]
```

**单次写入，单个文件**：
- 优点：原子性强，S3 PUT 立即可见
- 缺点：文件多（但会很快被删除）
- 用户控制批量：应用层合并多个 documents 到一个 upsert 请求

---

## 七、查询流程（Query Node）

### 7.1 完整流程

```rust
// 用户查询
POST /v1/namespaces/my_ns/query
{
  "vector": [0.1, 0.2, ...],
  "full_text": {"query": "rust database", "fields": ["title"]},
  "filter": {"category": {"eq": "tech"}},
  "top_k": 10
}

// Query Node 处理:

1. 读取最新 Manifest
   current_version = storage.get("my_ns/manifests/current.txt")
   manifest = storage.get("my_ns/manifests/v{current_version:08}.json")

   // Cache manifest for 5s to reduce S3 requests

2. 应用过滤器（可选）
   if filter.is_some() {
       filtered_ids = apply_filter(manifest.segments, filter)
   }

3. 并行查询所有 segment 索引
   // 向量搜索
   if query.vector.is_some() {
       let vector_results: Vec<_> = manifest.segments
           .par_iter()  // rayon 并行
           .map(|seg| {
               // 从缓存或 S3 加载索引
               let index = load_vector_index(seg.vector_index_path);
               index.search(query.vector, top_k * 2)
           })
           .flatten()
           .collect();

       // 合并结果
       vector_results.sort_by_score();
       vector_results.truncate(top_k);
   }

   // 全文搜索
   if query.full_text.is_some() {
       let fulltext_results: Vec<_> = manifest.segments
           .par_iter()
           .flat_map(|seg| {
               let index = load_fulltext_index(seg, field_name);
               index.search(query_text, top_k * 2)
           })
           .collect();

       fulltext_results.sort_by_score();
       fulltext_results.truncate(top_k);
   }

4. RRF 融合（如果是混合搜索）
   final_results = rrf_fusion(vector_results, fulltext_results, top_k);

5. 读取完整文档
   documents = read_documents_from_segments(final_results.ids);

6. 返回结果
```

### 7.2 缓存策略（Foyer）

```rust
// Memory Cache (1GB)
- Manifest (5s TTL)
- 热点 segment 的索引文件

// Disk Cache (100GB NVMe)
- Segment parquet 文件
- Vector indexes (.rabitq)
- Full-text indexes (.tantivy.tar.gz)

// 缓存 Key
cache_key = "{namespace}/{file_path}"
```

**缓存更新**:
- Manifest 变化时，旧 segment 索引仍然有效（immutable）
- 新 segment 按需加载
- LRU 淘汰不常用的 segments

---

## 八、WAL Recovery（启动时）

### 8.1 场景

**问题**：Indexer 崩溃后，可能有已写入 WAL 但未提交到 Manifest 的数据

**恢复流程**:

```rust
// Indexer 启动时
async fn recover_namespace(namespace: &str) -> Result<()> {
    // 1. 列出所有 WAL 文件
    let wal_files = storage.list(&format!("{namespace}/wal/")).await?;

    if wal_files.is_empty() {
        return Ok(()); // 无需恢复
    }

    tracing::info!("Found {} WAL files for {}, starting recovery", wal_files.len(), namespace);

    // 2. 读取并重放每个 WAL 文件
    for wal_file in wal_files {
        let operation = read_wal_entry(&wal_file).await?;

        match operation {
            WalOperation::Upsert { documents } => {
                // 重新执行写入流程（幂等性）
                // 注意：不要再写 WAL（避免递归）
                upsert_without_wal(namespace, documents).await?;
            }
        }

        // 3. 删除已重放的 WAL
        storage.delete(&wal_file).await?;
    }

    tracing::info!("Recovery completed for {}", namespace);
    Ok(())
}
```

### 8.2 幂等性保证

**问题**：如果 WAL 被重放多次怎么办？

**方案**：使用确定性 segment_id

```rust
// 不要用随机 UUID
// ❌ segment_id = uuid::Uuid::new_v4();

// 使用确定性 ID（基于 WAL 文件名）
// ✅ segment_id = hash(wal_filename);

let segment_id = format!("seg_{}",
    seahash::hash(wal_filename.as_bytes())
);

// 重放时检查 manifest 是否已包含此 segment
if manifest.segments.iter().any(|s| s.segment_id == segment_id) {
    tracing::warn!("Segment {} already exists, skipping replay", segment_id);
    continue;
}
```

---

## 九、Compaction（后台任务）

### 9.1 触发条件

```rust
// 每个 namespace 独立检查
if namespace.segment_count() > 10 {
    compact(namespace).await?;
}
```

**注意**：segments 数量通常很少（个位数），compaction 频率低

### 9.2 流程

```rust
async fn compact(namespace: &str) -> Result<()> {
    // 1. 读取当前 manifest
    let manifest = load_latest_manifest(namespace).await?;

    // 2. 选择最小的 N 个 segments（如 5 个）
    let to_merge = manifest.segments
        .iter()
        .sorted_by_key(|s| s.row_count)
        .take(5)
        .cloned()
        .collect();

    // 3. 合并数据
    let merged_docs = read_and_merge_segments(&to_merge).await?;

    // 4. 生成新 segment + indexes
    let new_segment_id = format!("compact_{}", Utc::now().timestamp_millis());
    let (segment_path, index_paths) = write_segment_with_indexes(
        namespace,
        &new_segment_id,
        &merged_docs
    ).await?;

    // 5. 更新 manifest（乐观锁）
    let current_version = read_current_version(namespace).await?;
    let new_version = current_version + 1;

    let new_manifest = Manifest {
        version: new_version,
        segments: manifest.segments
            .iter()
            .filter(|s| !to_merge.contains(s))  // 移除旧 segments
            .chain(std::iter::once(&new_segment_info))  // 添加新 segment
            .cloned()
            .collect(),
        ..manifest
    };

    write_manifest(namespace, new_version, &new_manifest).await?;

    // 6. 删除旧 segment 文件
    for old_seg in to_merge {
        storage.delete(&old_seg.file_path).await?;
        storage.delete(&old_seg.vector_index_path).await?;
        // ...
    }

    Ok(())
}
```

---

## 十、配置文件

```toml
# config.toml

[server]
mode = "query"          # "query" | "indexer" | "combined"
port = 3000
host = "0.0.0.0"

[storage]
backend = "s3"          # "s3" | "local"
bucket = "elacsym-data"
region = "us-west-2"

[indexer]
# Only for indexer nodes
node_id = "indexer-1"
total_nodes = 3
node_index = 0          # 0, 1, 2

[cache]
memory_size_mb = 1024   # 1GB
disk_size_gb = 100      # 100GB
disk_path = "/mnt/nvme/cache"

[compaction]
enabled = true
check_interval_secs = 3600
max_segments = 10       # 触发 compaction 的阈值
```

---

## 十一、API 设计

### 11.1 写入 API（路由到正确的 Indexer）

```http
POST /v1/namespaces/{namespace}/upsert

# 如果请求到错误的 indexer
HTTP/1.1 307 Temporary Redirect
Location: http://indexer-2:3000/v1/namespaces/{namespace}/upsert
X-Correct-Indexer: indexer-2
```

**客户端逻辑**:
```rust
// Smart client 缓存 namespace -> indexer 映射
let indexer = client.get_indexer_for_namespace(namespace);
let response = client.post(format!("{indexer}/v1/namespaces/{namespace}/upsert"))
    .send()
    .await?;

if response.status() == 307 {
    // 更新缓存
    let correct_indexer = response.headers().get("X-Correct-Indexer");
    client.update_mapping(namespace, correct_indexer);
    // 重试
}
```

### 11.2 查询 API（任意 Query Node）

```http
POST /v1/namespaces/{namespace}/query
{
  "vector": [0.1, 0.2, ...],
  "top_k": 10
}

# 任何 query node 都可以处理
HTTP/1.1 200 OK
{
  "results": [...],
  "took_ms": 15
}
```

---

## 十二、部署示例

### 12.1 单节点模式（开发）

```bash
# Combined mode: 既是 indexer 也是 query node
./elacsym --config config.toml --mode combined

# 所有 namespace 都由本节点处理
```

### 12.2 生产模式（3 Indexer + 5 Query）

```yaml
# docker-compose.yml

services:
  # Indexer Nodes
  indexer-1:
    image: elacsym:latest
    command: --mode indexer --node-index 0 --total-nodes 3
    environment:
      - INDEXER_NODE_ID=indexer-1

  indexer-2:
    image: elacsym:latest
    command: --mode indexer --node-index 1 --total-nodes 3
    environment:
      - INDEXER_NODE_ID=indexer-2

  indexer-3:
    image: elacsym:latest
    command: --mode indexer --node-index 2 --total-nodes 3
    environment:
      - INDEXER_NODE_ID=indexer-3

  # Query Nodes (Stateless, auto-scaling)
  query:
    image: elacsym:latest
    command: --mode query
    deploy:
      replicas: 5
      update_config:
        parallelism: 2

  # Load Balancer
  lb:
    image: haproxy:latest
    ports:
      - "80:80"
    volumes:
      - ./haproxy.cfg:/usr/local/etc/haproxy/haproxy.cfg
```

---

## 十三、性能目标

| 指标 | 目标值 | 说明 |
|------|--------|------|
| 写入延迟 (p50) | < 200ms | WAL + Segment + Manifest |
| 写入延迟 (p99) | < 500ms | 包括 S3 上传时间 |
| 查询延迟 (冷) | < 500ms | 从 S3 加载索引 |
| 查询延迟 (热) | < 50ms | 索引已缓存 |
| Segments / Namespace | < 10 个 | Compaction 保证 |
| WAL 文件数 | < 5 个 | 快速提交和删除 |

---

## 十四、与 V1 设计的区别

| 特性 | V1 (复杂) | V2 (简化) |
|------|-----------|----------|
| **外部依赖** | etcd/Consul | 无 |
| **全局索引** | GlobalVectorIndex | 无（并行查询） |
| **Manifest 更新** | CAS + 分布式锁 | 版本化文件名 |
| **Namespace 分配** | 动态（需锁） | 静态（一致性哈希） |
| **WAL 批量** | 1MB / 10s | 每次写入一个文件 |
| **故障转移** | 自动（etcd） | 手动重配置 |
| **复杂度** | 高 | 低 |

---

## 十五、实施计划

### Phase 1: Manifest 版本化（1-2天）✅ 部分完成
- [x] Per-segment 索引持久化
- [x] S3WalManager 实现
- [ ] Manifest 版本化文件名
- [ ] `current.txt` 指针逻辑

### Phase 2: Namespace 分片（2-3天）
- [ ] 一致性哈希实现
- [ ] Indexer 节点路由逻辑
- [ ] 307 重定向 API
- [ ] Smart client

### Phase 3: Query Node 优化（2-3天）
- [ ] 并行查询所有 segments
- [ ] Manifest 缓存（5s TTL）
- [ ] 索引缓存优化

### Phase 4: 测试与文档（2-3天）
- [ ] 集成测试（多节点）
- [ ] 压力测试
- [ ] 部署文档
- [ ] API 文档

**总计**: ~10-14 天

---

## 十六、FAQ

### Q1: 如果两个 indexer 同时写入同一个 namespace 怎么办？

**A**: 理论上不应该发生（一致性哈希保证）。如果配置错误导致冲突：
- 会产生两个 manifest 版本（如 v124, v125）
- `current.txt` 指向最后写入的版本
- 另一个版本的 segment 文件会成为"孤儿"
- 定期 GC 任务清理未使用的 segments

### Q2: Query Node 如何保证读到最新数据？

**A**:
- `current.txt` 总是指向最新版本
- Query Node 读取 `current.txt` + 对应 manifest
- S3 保证 Read-After-Write 一致性（2020年起）
- 可选：缓存 manifest 5s（最终一致性）

### Q3: Segments 数量会无限增长吗？

**A**: 不会
- Compaction 定期合并小 segments
- 目标：每个 namespace < 10 个 segments
- 删除旧 segments 文件

### Q4: WAL 文件会堆积吗？

**A**: 不会
- 写入流程最后一步删除 WAL
- 正常情况下，WAL 存在时间 < 1s
- 只有崩溃时才会保留，启动时恢复

### Q5: 如何扩容 Indexer？

**A**:
1. 添加新 indexer 节点（如 indexer-4）
2. 更新所有节点配置 `total_nodes = 4`
3. 重启所有 indexer
4. Namespaces 重新哈希分配（部分迁移）

---

**设计完成！准备实施 🚀**
