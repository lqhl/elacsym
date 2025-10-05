# Elacsym 分布式架构设计

> 参考 turbopuffer 架构，实现水平可扩展的向量数据库

**设计日期**: 2025-10-05
**状态**: 🚧 设计中

---

## 一、架构概览

### 1.1 节点类型

```
┌─────────────────────────────────────────────────────────────┐
│                      Load Balancer                          │
│              (HAProxy / AWS ALB / Nginx)                    │
└─────────────────────────────────────────────────────────────┘
              │                              │
              │                              │
      ┌───────▼────────┐           ┌────────▼────────┐
      │  Query Nodes   │           │  Indexer Nodes  │
      │   (Stateless)  │           │   (Stateful)    │
      │                │           │                 │
      │  • 处理查询     │           │  • 处理写入      │
      │  • 读取索引     │           │  • 构建索引      │
      │  • 缓存优化     │           │  • Compaction   │
      └────────┬───────┘           └────────┬────────┘
               │                            │
               │                            │
               └──────────┬─────────────────┘
                          │
              ┌───────────▼───────────────────┐
              │     Object Storage (S3)       │
              │                               │
              │  /{namespace}/                │
              │    ├── manifest.json          │
              │    ├── wal/                   │
              │    │   ├── 00001.log          │
              │    │   └── 00002.log          │
              │    ├── segments/              │
              │    │   ├── seg_001.parquet    │
              │    │   ├── seg_001.rabitq     │
              │    │   └── seg_001.tantivy/   │
              │    └── global_index/          │
              │        ├── centroids.rabitq   │
              │        └── metadata.json      │
              └───────────────────────────────┘
                          ▲
                          │
              ┌───────────┴───────────────────┐
              │   Metadata Coordinator        │
              │   (etcd / Consul / DynamoDB)  │
              │                               │
              │  • Namespace 锁管理            │
              │  • Manifest 版本控制           │
              │  • Indexer 任务分配           │
              │  • Leader Election            │
              └───────────────────────────────┘
```

---

## 二、核心组件设计

### 2.1 Query Node（查询节点）

**职责**:
- 处理所有查询请求（vector search / full-text / hybrid）
- 从 S3 读取 manifest、索引、segments
- 本地缓存优化（Foyer: Memory + NVMe SSD）
- **完全无状态**，可任意扩缩容

**查询流程**:
```rust
1. 接收查询请求
   ↓
2. 从 S3/Cache 读取 manifest
   ↓
3. 根据查询类型选择索引策略:
   a) Vector Query:
      - 读取全局 centroids 索引（S3/Cache）
      - 确定候选 clusters
      - 读取对应 segment 索引（S3/Cache）
      - 精排获取 top-k

   b) Full-Text Query:
      - 读取每个 segment 的 Tantivy 索引
      - 并行查询所有 segments
      - 合并结果（BM25 分数）

   c) Hybrid Query:
      - 并行执行 vector + full-text
      - RRF 融合
   ↓
4. 读取 segments 获取完整文档（S3/Cache）
   ↓
5. 应用过滤器
   ↓
6. 返回结果
```

**水平扩展**:
- 无状态设计，LB 随机路由即可
- 缓存各自独立，通过 S3 作为 source of truth

---

### 2.2 Indexer Node（索引节点）

**职责**:
- 处理所有写入请求（upsert / delete）
- 写入 WAL 到 S3
- 生成 segments + 对应索引
- 后台 compaction（合并小 segments）
- 构建/更新全局索引（centroids）

**写入流程**:
```rust
1. 接收 upsert 请求
   ↓
2. 获取 namespace 写锁（etcd lease）
   ↓
3. 写入 WAL 到 S3:
   - Key: {namespace}/wal/{timestamp}_{node_id}.log
   - MessagePack + CRC32 格式
   - 原子写入（S3 PUT）
   ↓
4. 批量写入达到阈值后 flush:
   a) 生成 segment:
      - seg_xxx.parquet (文档数据)

   b) 构建 segment 索引:
      - seg_xxx.rabitq (向量索引)
      - seg_xxx.tantivy/ (全文索引目录)

   c) 上传到 S3:
      - 并行上传 parquet + indexes
   ↓
5. 更新 manifest (CAS 原子操作):
   - 读取当前 manifest (version N)
   - 添加新 segment info
   - 写入新 manifest (version N+1)
   - 如果 CAS 失败，重试
   ↓
6. 删除已提交的 WAL 文件
   ↓
7. 释放写锁
   ↓
8. 返回成功
```

**Compaction 流程**:
```rust
Background Task (每小时):

1. 检查是否需要 compaction:
   - segments 数量 > 100
   - 总文档数 > 1M
   ↓
2. 获取 namespace 写锁（排他）
   ↓
3. 选择需要合并的 segments (如最老的 10 个)
   ↓
4. 合并数据:
   - 读取所有选中 segments
   - 去重 + 应用 tombstones
   - 生成新的大 segment
   ↓
5. 重建索引:
   - 构建新 segment 的 RaBitQ + Tantivy 索引
   - 上传到 S3
   ↓
6. 重建全局索引:
   - 从所有 segments 提取向量
   - K-means 生成 centroids
   - 构建全局 RaBitQ 索引
   - 上传 {namespace}/global_index/centroids.rabitq
   ↓
7. 原子更新 manifest:
   - 移除旧 segments
   - 添加新 segment
   - 更新 global_index 路径
   ↓
8. 删除旧 segment 文件（S3）
   ↓
9. 释放写锁
```

**高可用**:
- 多个 indexer 节点通过分布式锁协调
- 同一时刻每个 namespace 只有一个 indexer 在写入
- Indexer 节点故障时，其他节点通过锁超时接管

---

### 2.3 存储布局（S3）

#### 新的目录结构

```
s3://elacsym-data/
  /{namespace}/

    # 元数据
    ├── manifest.json              # 版本化元数据
    │   {
    │     "version": 123,
    │     "namespace": "my_ns",
    │     "schema": { ... },
    │     "segments": [
    │       {
    │         "segment_id": "seg_001",
    │         "file_path": "segments/seg_001.parquet",
    │         "vector_index_path": "segments/seg_001.rabitq",
    │         "fulltext_indexes": {
    │           "title": "segments/seg_001_title.tantivy/"
    │         },
    │         "row_count": 10000,
    │         "id_range": [1, 10000]
    │       }
    │     ],
    │     "global_index": {
    │       "vector_centroids": "global_index/centroids_v123.rabitq",
    │       "updated_at": "2025-10-05T12:00:00Z"
    │     }
    │   }

    # WAL（Write-Ahead Log）
    ├── wal/
    │   ├── 1728129600000_indexer1.log    # {timestamp}_{node_id}.log
    │   ├── 1728129601000_indexer1.log
    │   └── 1728129605000_indexer2.log    # 不同节点的 WAL

    # Segments（数据 + 索引）
    ├── segments/
    │   # Segment 001
    │   ├── seg_001.parquet                # 文档数据
    │   ├── seg_001.rabitq                 # 向量索引（二进制）
    │   ├── seg_001_title.tantivy/         # 全文索引目录
    │   │   ├── meta.json
    │   │   ├── .managed.json
    │   │   └── {uuid}.{idx,pos,term,...}  # Tantivy 索引文件
    │   │
    │   # Segment 002
    │   ├── seg_002.parquet
    │   ├── seg_002.rabitq
    │   └── seg_002_title.tantivy/

    # 全局索引（加速查询）
    └── global_index/
        ├── centroids_v123.rabitq          # 全局 centroids 索引
        ├── metadata_v123.json             # 索引元数据
        └── schema_v1.json                 # Schema 快照
```

#### 关键变化

1. **Per-Segment Indexes**:
   - ✅ 每个 segment 独立的 RaBitQ 索引文件
   - ✅ 每个 segment 每个字段独立的 Tantivy 索引目录
   - ✅ 写入时立即构建，无需全局重建

2. **WAL 到 S3**:
   - ✅ WAL 文件直接写入 S3
   - ✅ 文件名包含 timestamp + node_id（避免冲突）
   - ✅ 支持多 indexer 并发写入不同 namespace

3. **Global Index**:
   - ✅ Centroids 索引用于快速确定候选 clusters
   - ✅ 版本化（避免并发更新冲突）
   - ✅ Query 节点优先级：global_index > segment indexes

---

### 2.4 Metadata Coordinator（元数据协调器）

**选型**: etcd / Consul / DynamoDB

**职责**:

1. **Namespace 写锁管理**:
   ```rust
   // 获取写锁（10s TTL）
   let lease_id = etcd.grant_lease(10).await?;
   etcd.put_with_lease(
       format!("/locks/namespaces/{}/write", ns),
       node_id,
       lease_id
   ).await?;

   // 心跳续租
   etcd.keep_alive(lease_id).await?;

   // 释放锁
   etcd.delete(format!("/locks/namespaces/{}/write", ns)).await?;
   ```

2. **Manifest 版本控制**:
   - 使用 S3 的 Object Versioning + ETag
   - CAS (Compare-And-Swap) 原子更新:
     ```rust
     // 读取 manifest + ETag
     let (manifest, etag) = s3.get_with_etag("manifest.json").await?;

     // 修改 manifest
     manifest.version += 1;
     manifest.segments.push(new_segment);

     // 条件写入（如果 ETag 匹配）
     s3.put_if_match("manifest.json", manifest, etag).await?;
     // 如果失败 -> ETag 不匹配 -> 其他节点已更新 -> 重试
     ```

3. **Indexer 任务分配**:
   - Namespace → Indexer 映射（一致性哈希）
   - 故障转移（watch 节点健康状态）

4. **Leader Election**（可选）:
   - Compaction leader（避免多个节点同时 compact）
   - Global index builder leader

---

## 三、一致性保证

### 3.1 强一致性模型

**turbopuffer 承诺**: "if you perform a write, a subsequent query will immediately see the write"

**实现方式**:

1. **写入路径**:
   ```
   Write → WAL (S3) → Manifest Update (CAS) → Success
   ```
   - WAL 写入成功 = 数据已持久化
   - Manifest 更新成功 = 数据已可见

2. **查询路径**:
   ```
   Query → Read Latest Manifest (S3) → Read Segments + Indexes → Return
   ```
   - 总是读取最新的 manifest
   - S3 Read-After-Write Consistency 保证

3. **关键机制**:
   - **S3 一致性**: AWS S3 自 2020 年起保证 Read-After-Write 强一致性
   - **Manifest CAS**: 使用 ETag 防止并发更新丢失
   - **Namespace 锁**: 每个 namespace 同一时刻只有一个 writer

### 3.2 最终一致性模式（可选）

**场景**: 允许 stale read 换取低延迟

**实现**:
```rust
// Query 参数
{
  "consistency": "eventual",  // or "strong" (default)
  "max_staleness_ms": 5000    // 最多接受 5s 旧数据
}
```

**机制**:
- Query 节点缓存 manifest（5s TTL）
- 避免每次查询都访问 S3
- 适合对实时性要求不高的场景

---

## 四、关键技术实现

### 4.1 Per-Segment RaBitQ 索引

**当前问题**:
- 全局索引，所有向量在一个 index 中
- 添加向量需要重建整个索引

**新方案**:
```rust
// src/index/vector.rs

impl VectorIndex {
    /// Build and persist segment-level index to S3
    pub async fn build_and_persist(
        &mut self,
        storage: &dyn StorageBackend,
        segment_id: &str,
        namespace: &str,
    ) -> Result<String> {
        // 1. Build RaBitQ index (existing logic)
        self.build_index()?;

        // 2. Serialize index to binary format
        let index_bytes = self.serialize_rabitq_index()?;

        // 3. Upload to S3
        let index_path = format!(
            "{}/segments/{}.rabitq",
            namespace, segment_id
        );
        storage.put(&index_path, Bytes::from(index_bytes)).await?;

        Ok(index_path)
    }

    /// Load segment index from S3
    pub async fn load_from_storage(
        storage: &dyn StorageBackend,
        index_path: &str,
    ) -> Result<Self> {
        let data = storage.get(index_path).await?;
        Self::deserialize_rabitq_index(&data)
    }

    /// Serialize RaBitQ index to bytes
    fn serialize_rabitq_index(&self) -> Result<Vec<u8>> {
        // 包含:
        // - vectors (Vec<Vec<f32>>)
        // - id_map / reverse_map
        // - centroids
        // - quantized codes

        bincode::serialize(&SerializableIndex {
            dimension: self.dimension,
            metric: self.metric,
            vectors: self.vectors.clone(),
            id_map: self.id_map.clone(),
            reverse_map: self.reverse_map.clone(),
            // RaBitQ 内部状态（需要从库中提取）
        }).map_err(|e| Error::internal(format!("Serialize failed: {}", e)))
    }
}
```

**全局 Centroids 索引**:
```rust
// src/index/global_vector.rs (NEW)

/// Global centroid index for fast cluster selection
pub struct GlobalVectorIndex {
    centroids: Vec<Vector>,         // K centroids
    segment_mapping: Vec<Vec<String>>, // centroid -> segment_ids
}

impl GlobalVectorIndex {
    /// Build from all segments
    pub async fn build_from_segments(
        segments: &[SegmentInfo],
        storage: &dyn StorageBackend,
        k: usize, // Number of centroids (e.g., 256)
    ) -> Result<Self> {
        // 1. Load all vectors from all segments
        let mut all_vectors = Vec::new();
        for seg in segments {
            let seg_data = storage.get(&seg.file_path).await?;
            // Extract vectors...
            all_vectors.extend(extract_vectors(&seg_data)?);
        }

        // 2. K-means clustering
        let centroids = kmeans(&all_vectors, k)?;

        // 3. Assign segments to centroids
        let mut segment_mapping = vec![Vec::new(); k];
        for seg in segments {
            // Determine which centroid(s) this segment belongs to
            let centroid_ids = assign_segment_to_centroids(seg, &centroids)?;
            for cid in centroid_ids {
                segment_mapping[cid].push(seg.segment_id.clone());
            }
        }

        Ok(Self { centroids, segment_mapping })
    }

    /// Query: return candidate segment IDs
    pub fn search_candidates(
        &self,
        query: &Vector,
        n_probe: usize,
    ) -> Vec<String> {
        // Find closest n_probe centroids
        let mut dists: Vec<_> = self.centroids.iter()
            .enumerate()
            .map(|(i, c)| (i, l2_distance(query, c)))
            .collect();
        dists.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

        // Collect all segments from top centroids
        let mut candidates = HashSet::new();
        for (centroid_id, _) in dists.iter().take(n_probe) {
            candidates.extend(self.segment_mapping[*centroid_id].iter().cloned());
        }

        candidates.into_iter().collect()
    }

    /// Persist to S3
    pub async fn save(
        &self,
        storage: &dyn StorageBackend,
        namespace: &str,
        version: u64,
    ) -> Result<String> {
        let path = format!("{}/global_index/centroids_v{}.rabitq", namespace, version);
        let bytes = bincode::serialize(self)?;
        storage.put(&path, Bytes::from(bytes)).await?;
        Ok(path)
    }
}
```

---

### 4.2 Per-Segment Tantivy 索引

**当前问题**:
- 全局内存索引 (`Index::create_in_ram()`)
- 不持久化

**新方案**:
```rust
// src/index/fulltext.rs

impl FullTextIndex {
    /// Build segment-level index and persist to S3
    pub async fn build_and_persist(
        field_name: String,
        config: FullTextConfig,
        documents: &[(DocId, String)],
        storage: &dyn StorageBackend,
        segment_id: &str,
        namespace: &str,
    ) -> Result<String> {
        // 1. Create temporary directory for Tantivy
        let temp_dir = std::env::temp_dir()
            .join(format!("tantivy_{}_{}", segment_id, field_name));
        std::fs::create_dir_all(&temp_dir)?;

        // 2. Build index on disk
        let mut index = Self::new_persistent(field_name.clone(), &temp_dir)?;
        index.add_documents(documents)?;

        // 3. Compress index directory to tarball
        let tarball = compress_directory(&temp_dir)?;

        // 4. Upload to S3 as a single file
        let index_path = format!(
            "{}/segments/{}_{}.tantivy.tar.gz",
            namespace, segment_id, field_name
        );
        storage.put(&index_path, Bytes::from(tarball)).await?;

        // 5. Cleanup
        std::fs::remove_dir_all(&temp_dir)?;

        Ok(index_path)
    }

    /// Load from S3
    pub async fn load_from_storage(
        storage: &dyn StorageBackend,
        index_path: &str,
        field_name: String,
    ) -> Result<Self> {
        // 1. Download tarball
        let tarball = storage.get(index_path).await?;

        // 2. Extract to temp directory
        let temp_dir = std::env::temp_dir()
            .join(format!("tantivy_{}", uuid::Uuid::new_v4()));
        decompress_tarball(&tarball, &temp_dir)?;

        // 3. Open Tantivy index
        Self::new_persistent(field_name, &temp_dir)
    }
}

/// Helper: compress directory to .tar.gz
fn compress_directory(dir: &Path) -> Result<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use tar::Builder;

    let mut buf = Vec::new();
    let gz = GzEncoder::new(&mut buf, Compression::default());
    let mut tar = Builder::new(gz);
    tar.append_dir_all(".", dir)?;
    tar.finish()?;
    drop(tar);

    Ok(buf)
}
```

**替代方案** (更高效):
- 不压缩，直接上传 Tantivy 目录内的所有文件
- Key 格式: `{namespace}/segments/{segment_id}_{field}.tantivy/{filename}`
- 查询时下载到本地缓存

---

### 4.3 WAL 到 S3

**当前实现**: 本地文件系统 `wal/{namespace}/wal.log`

**新实现**:
```rust
// src/wal/s3_wal.rs (NEW)

use bytes::Bytes;
use crate::storage::StorageBackend;

/// S3-backed Write-Ahead Log
pub struct S3WalManager {
    namespace: String,
    node_id: String,
    storage: Arc<dyn StorageBackend>,
    sequence: AtomicU64,
}

impl S3WalManager {
    pub fn new(
        namespace: String,
        node_id: String,
        storage: Arc<dyn StorageBackend>,
    ) -> Self {
        Self {
            namespace,
            node_id,
            storage,
            sequence: AtomicU64::new(0),
        }
    }

    /// Append operation to WAL on S3
    pub async fn append(&self, op: WalOperation) -> Result<u64> {
        // 1. Serialize operation
        let mut buf = Vec::new();
        rmp_serde::encode::write(&mut buf, &op)?;

        // 2. Add CRC32 checksum
        let crc = crc32fast::hash(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());

        // 3. Generate unique key
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
        let timestamp = chrono::Utc::now().timestamp_millis();
        let key = format!(
            "{}/wal/{:020}_{}.log",
            self.namespace, timestamp, self.node_id
        );

        // 4. Write to S3 (atomic)
        self.storage.put(&key, Bytes::from(buf)).await?;

        Ok(seq)
    }

    /// List all WAL entries for this namespace
    pub async fn list_wal_files(&self) -> Result<Vec<String>> {
        let prefix = format!("{}/wal/", self.namespace);
        self.storage.list(&prefix).await
    }

    /// Replay WAL files
    pub async fn replay(&self) -> Result<Vec<WalOperation>> {
        let files = self.list_wal_files().await?;
        let mut operations = Vec::new();

        for file_key in files {
            let data = self.storage.get(&file_key).await?;

            // Parse and verify checksum
            if data.len() < 4 {
                tracing::warn!("WAL file {} too short, skipping", file_key);
                continue;
            }

            let (msg_data, crc_bytes) = data.split_at(data.len() - 4);
            let stored_crc = u32::from_le_bytes(crc_bytes.try_into().unwrap());
            let computed_crc = crc32fast::hash(msg_data);

            if stored_crc != computed_crc {
                tracing::error!("WAL file {} corrupted, skipping", file_key);
                continue;
            }

            // Deserialize
            let op: WalOperation = rmp_serde::from_slice(msg_data)?;
            operations.push(op);
        }

        Ok(operations)
    }

    /// Delete committed WAL files
    pub async fn truncate(&self) -> Result<()> {
        let files = self.list_wal_files().await?;

        for file_key in files {
            self.storage.delete(&file_key).await?;
        }

        Ok(())
    }
}
```

**优势**:
- ✅ 多节点可以并发写入不同 namespace
- ✅ WAL 持久化到 S3，节点故障不丢数据
- ✅ 通过 timestamp + node_id 避免文件名冲突

---

## 五、部署架构

### 5.1 单节点模式（开发/小规模）

```
┌────────────────────────────┐
│   Combined Node            │
│   (Indexer + Query)        │
│                            │
│   elacsym --mode=combined  │
└────────────┬───────────────┘
             │
             ▼
        S3 / Local FS
```

### 5.2 生产模式（高可用）

```
┌───────────────────────────────────────────────────────────┐
│                    Load Balancer (ALB)                    │
└───────────────────────────────────────────────────────────┘
            │                                  │
     ┌──────▼────────┐                 ┌──────▼────────┐
     │  Query Node 1 │                 │  Query Node 2 │
     │  (Auto-Scale) │                 │  (Auto-Scale) │
     └───────────────┘                 └───────────────┘
            │                                  │
            └──────────────┬───────────────────┘
                           │
              ┌────────────▼───────────┐
              │  Indexer Node (Multi)  │
              │  + etcd Cluster        │
              └────────────┬───────────┘
                           │
              ┌────────────▼───────────┐
              │         S3             │
              └────────────────────────┘
```

**扩展规则**:
- Query Nodes: 根据 QPS 自动扩缩容（CPU < 70%）
- Indexer Nodes: 根据写入 TPS + namespace 数量扩容
- etcd: 3-5 节点集群（奇数）

---

## 六、配置示例

```toml
# config.toml

[server]
mode = "query"  # or "indexer" or "combined"
port = 3000
node_id = "node-1"  # 唯一标识

[storage]
backend = "s3"
bucket = "elacsym-data"
region = "us-west-2"
# 或 backend = "local"
# path = "./data"

[coordinator]
backend = "etcd"
endpoints = ["http://etcd1:2379", "http://etcd2:2379", "http://etcd3:2379"]
# 或 backend = "consul" / "dynamodb" / "none" (single-node)

[cache]
memory_size_mb = 1024      # 1GB memory cache
disk_size_gb = 100         # 100GB NVMe cache
disk_path = "/mnt/cache"

[indexer]
# Only for indexer nodes
flush_threshold_docs = 10000
flush_threshold_bytes = 10485760  # 10MB
wal_sync_interval_ms = 100

[compaction]
enabled = true
interval_secs = 3600
max_segments = 100
max_total_docs = 1000000

[query]
# Only for query nodes
max_concurrent_queries = 100
default_consistency = "strong"  # or "eventual"
```

---

## 七、实施计划

### Phase 1: Per-Segment Indexes（1-2 周）
- [x] 设计文档编写
- [ ] 实现 `VectorIndex::build_and_persist()` - RaBitQ 序列化
- [ ] 实现 `FullTextIndex::build_and_persist()` - Tantivy 打包上传
- [ ] 更新 `SegmentInfo` 添加 index paths
- [ ] 修改 `upsert_internal()` 写入时构建索引
- [ ] 单元测试

### Phase 2: WAL to S3（1 周）
- [ ] 实现 `S3WalManager`
- [ ] 支持 timestamp + node_id 文件命名
- [ ] 集成到 `Namespace::upsert()`
- [ ] WAL replay 逻辑更新
- [ ] 测试崩溃恢复

### Phase 3: Global Index（1 周）
- [ ] 实现 `GlobalVectorIndex`
- [ ] K-means clustering 算法
- [ ] Segment → Centroid 映射
- [ ] 集成到 compaction 流程
- [ ] 查询优化（先查全局索引）

### Phase 4: Node Roles（2 周）
- [ ] 实现 `NodeMode` enum (Query / Indexer / Combined)
- [ ] Query Node: 只读逻辑，禁用写入
- [ ] Indexer Node: 写入逻辑，可选禁用查询
- [ ] 命令行参数 `--mode`
- [ ] 健康检查 API

### Phase 5: Metadata Coordinator（2 周）
- [ ] etcd 集成（`etcd-client` crate）
- [ ] Namespace 写锁实现
- [ ] Manifest CAS 更新（S3 ETag）
- [ ] Leader election（compaction leader）
- [ ] 故障转移测试

### Phase 6: Testing & Documentation（1 周）
- [ ] 集成测试（多节点）
- [ ] 性能基准测试
- [ ] 文档更新
- [ ] 部署指南

**总计**: ~8-10 周（2-2.5 月）

---

## 八、风险与缓解

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| S3 延迟高 | 查询慢 | 多级缓存（Memory + NVMe）|
| WAL 写入慢 | 吞吐量低 | 批量写入 + 异步 flush |
| Manifest 冲突 | CAS 重试频繁 | Namespace 级别锁 |
| etcd 单点故障 | 写入不可用 | 3-5 节点集群 + 健康检查 |
| RaBitQ 不支持序列化 | 无法持久化 | Fork 库添加序列化支持 |
| Tantivy 目录大 | S3 传输慢 | 压缩 + 增量上传 |

---

## 九、性能目标

参考 turbopuffer 指标:

| 指标 | turbopuffer | Elacsym 目标 |
|------|-------------|-------------|
| 写入 QPS | ~10,000 vectors/s | 5,000-10,000 vectors/s |
| 写入延迟 (p50) | 285ms | < 300ms |
| 冷查询延迟 (1M docs) | ~500ms | < 1s |
| 热查询延迟 (p50) | 8ms | < 50ms |
| 水平扩展 | ✅ | ✅ |
| 强一致性 | ✅ | ✅ |

---

## 十、参考资料

- [turbopuffer Architecture](https://turbopuffer.com/docs/architecture)
- [AWS S3 Consistency](https://aws.amazon.com/s3/consistency/)
- [etcd Documentation](https://etcd.io/docs/)
- [RaBitQ Paper](https://arxiv.org/abs/2405.12497)
- [Tantivy Index Format](https://docs.rs/tantivy/latest/tantivy/)

---

**下一步**: 开始 Phase 1 实现 🚀
