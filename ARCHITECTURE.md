# elacsym 架构设计

## 概述

elacsym 是一个基于 Serverless 架构的开源向量数据库，底层使用 [RaBitQ-RS](https://github.com/lqhl/rabitq-rs) 作为 ANN 算法引擎。设计参考 Pinecone Serverless 架构，实现存储与计算分离，提供高性能、低成本的向量检索服务。

## 核心设计理念

### 1. 存储与计算分离
- **存储层**：使用对象存储（S3/MinIO）作为索引的持久化存储
- **计算层**：查询执行器按需加载相关索引分片，无需维护完整索引

### 2. 几何分区策略
- 使用 RaBitQ-RS 的 IVF（Inverted File）索引实现向量空间分区
- 每个分区维护一个 centroid 向量
- 新向量根据最近 centroid 分配到对应分区
- 生成不可变的、独立的索引段（slabs）

### 3. 双处理管道
- **索引构建器（Index Builder）**：构建和优化 RaBitQ 索引
- **新鲜度层（Freshness Layer）**：处理最新写入的数据，提供秒级查询新鲜度

## 系统架构

```
┌─────────────────────────────────────────────────────────────┐
│                        API Gateway                          │
│                    (REST/gRPC API)                          │
└──────────────────────┬──────────────────────────────────────┘
                       │
         ┌─────────────┴──────────────┐
         │                            │
┌────────▼─────────┐       ┌──────────▼──────────┐
│  Write Pipeline  │       │   Query Executor    │
│                  │       │                     │
│ - Write Log      │       │ - Index Loader      │
│ - Sequencing     │       │ - RaBitQ Search     │
│ - Freshness Idx  │       │ - Result Merger     │
└────────┬─────────┘       └──────────┬──────────┘
         │                            │
         │                  ┌─────────┴─────────┐
         │                  │                   │
         │         ┌────────▼────────┐  ┌───────▼────────┐
         │         │  Cache Layer    │  │  Freshness     │
         │         │                 │  │  Layer         │
         │         │ - Hot Segments  │  │                │
         │         └────────┬────────┘  └────────────────┘
         │                  │
┌────────▼──────────────────▼────────────────────────────────┐
│                    Metadata Service                        │
│  - Index Metadata  - Namespace Info  - Partition Mappings  │
└────────┬───────────────────────────────────────────────────┘
         │
┌────────▼──────────────────────────────────────────────────┐
│              Index Builder (Background)                    │
│                                                            │
│  - Tail Mutation Log                                       │
│  - Build RaBitQ Indexes (IVF+RaBitQ)                      │
│  - Generate Immutable Slabs                                │
│  - Upload to Blob Storage                                  │
└────────┬───────────────────────────────────────────────────┘
         │
┌────────▼──────────────────────────────────────────────────┐
│                  Blob Storage (S3/MinIO)                   │
│                                                            │
│  - Index Segments (Slabs)                                  │
│  - Mutation Logs                                           │
│  - Checkpoints                                             │
└────────────────────────────────────────────────────────────┘
```

## 核心组件

### 1. API Gateway
- 提供 REST 和 gRPC 接口
- 支持操作：
  - `upsert`: 插入/更新向量
  - `query`: 向量检索
  - `delete`: 删除向量
  - `fetch`: 根据 ID 获取向量
  - 命名空间管理

### 2. Write Pipeline
**Write Log**:
- 所有写入操作记录到 WAL（Write-Ahead Log）
- 提供持久化保证和故障恢复

**Sequencing Service**:
- 为每个写入操作分配全局递增序列号
- 保证操作的顺序性

**Freshness Index**:
- 对最新未索引数据构建轻量级索引
- 使用 RaBitQ-RS 快速构建小规模索引
- 提供秒级查询新鲜度

### 3. Index Builder
**功能**:
- 后台异步处理，tail mutation log
- 使用 RaBitQ-RS 训练和构建索引：
  ```rust
  IvfRabitqIndex::train(
      &vectors,
      nlist,        // 分区数量
      total_bits,   // 量化位数（3-8 bits）
      metric,       // L2 或 Inner Product
      rotator,      // 使用 FHT 旋转
      seed,
      use_faster_config  // 大数据集加速
  )
  ```
- 生成不可变的索引段（slabs）
- 上传到 Blob Storage，带 CRC32 校验

**分区策略**:
- 初始分区数：`sqrt(N)` 或固定值（如 1024）
- 动态分裂：当分区过大时自动分裂
- 每个 slab 包含：
  - RaBitQ 索引数据
  - 分区 metadata
  - 向量 ID 映射

### 4. Query Executor
**查询流程**:
1. 解析查询参数（top_k, nprobe, filter）
2. 根据 namespace 定位相关分区
3. 从 Cache/Blob Storage 加载索引段
4. 使用 RaBitQ-RS 执行搜索：
   ```rust
   let params = SearchParams::new(top_k, nprobe);
   let results = index.search(&query_vector, params)?;
   ```
5. 查询 Freshness Layer
6. 合并结果并排序
7. 应用 metadata filter

**Cache Layer**:
- LRU 缓存热点索引段
- 减少 Blob Storage 访问
- 支持多级缓存（内存 + 本地磁盘）

### 5. Metadata Service
- 存储索引元数据
- Namespace → Partition 映射
- Partition → Slab 文件映射
- 向量 ID → (Partition, Offset) 映射
- 使用 RocksDB 或 FoundationDB 存储

### 6. Freshness Layer
- 维护最新写入数据的内存索引
- 定期与主索引合并
- 保证查询结果包含最新数据
- 使用 RaBitQ-RS 构建小规模索引

## 数据模型

### Namespace
- 硬隔离的数据分区
- 每个 namespace 独立的索引和元数据
- 支持多租户场景

### Slab（索引段）
```rust
struct Slab {
    id: SlabId,
    namespace: String,
    partition_id: u32,
    vector_count: usize,
    dimension: usize,
    created_at: Timestamp,

    // RaBitQ 索引数据
    index_data: Vec<u8>,

    // 向量 ID 列表
    vector_ids: Vec<String>,

    // Metadata
    metadata: Vec<Metadata>,

    // 校验和
    checksum: u32,
}
```

### Vector
```rust
struct Vector {
    id: String,
    values: Vec<f32>,
    metadata: HashMap<String, MetadataValue>,
    namespace: String,
}
```

## RaBitQ-RS 集成

### 索引配置
- **nlist**: 分区数量，建议 `sqrt(N)` 到 `4*sqrt(N)`
- **total_bits**: 量化位数，3-8 bits（推荐 4-6 bits）
- **metric**: 距离度量（L2 或 Inner Product）
- **rotator**: 使用 FHT（Fast Hadamard Transform）
- **faster_config**: 数据量 >100K 时启用，加速 100-500x

### 性能优化
- SIMD 加速距离计算
- 内存压缩率 32x（相比原始向量）
- 精度高于 PQ/SQ

## 可扩展性

### 水平扩展
- Query Executor 无状态，可任意扩展
- Index Builder 并行处理不同分区
- Metadata Service 支持分片

### 存储扩展
- 基于对象存储，接近无限容量
- 冷数据自动归档
- 按需加载索引段

### 成本优化
- 冷租户零成本（不加载索引）
- 按实际计算资源付费
- 对象存储成本远低于 EBS

## 查询性能

### 延迟优化
- 缓存热点索引段：< 10ms
- 冷启动（从 S3 加载）：< 100ms
- Freshness Layer 查询：< 5ms

### 吞吐优化
- 批量查询支持
- 异步 I/O
- 并行搜索多个分区

## 一致性保证

### 写入
- 强一致性：Write Log + Sequencing
- 持久化保证：WAL fsync

### 读取
- 最终一致性：查询可能滞后几秒
- 新鲜度保证：Freshness Layer 提供秒级新鲜度

## 技术栈

- **语言**: Rust
- **ANN 引擎**: RaBitQ-RS
- **对象存储**: S3 兼容接口（AWS S3, MinIO, Ceph）
- **元数据存储**: RocksDB / FoundationDB
- **API 框架**:
  - REST: Axum
  - gRPC: Tonic
- **序列化**: Protocol Buffers
- **日志**: Tracing
- **监控**: Prometheus + Grafana

## 部署架构

### Serverless 部署
- Query Executor: AWS Lambda / Kubernetes Jobs
- Index Builder: Kubernetes CronJob / Fargate
- Metadata Service: RDS / DynamoDB
- Storage: S3

### 自托管部署
- Query Executor: Kubernetes Deployment
- Index Builder: Kubernetes StatefulSet
- Metadata Service: RocksDB (本地) / TiKV (分布式)
- Storage: MinIO / Ceph

## 后续优化方向

1. **索引质量**
   - 自适应分区数量
   - 在线索引重建
   - 增量索引更新

2. **查询优化**
   - 查询计划优化
   - 自适应 nprobe
   - GPU 加速

3. **高可用**
   - 多区域复制
   - 故障自动恢复
   - 灰度发布

4. **功能扩展**
   - Hybrid Search（向量 + 关键词）
   - Sparse Vector 支持
   - 多向量查询

## 参考资料

- [RaBitQ-RS](https://github.com/lqhl/rabitq-rs)
- [Pinecone Serverless Architecture](https://www.pinecone.io/blog/serverless-architecture/)
- [IVF (Inverted File) Index](https://github.com/facebookresearch/faiss/wiki/Faiss-indexes)
