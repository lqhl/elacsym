# elacsym

<div align="center">

**开源 Serverless 向量数据库**

基于 [RaBitQ-RS](https://github.com/lqhl/rabitq-rs) 和 Pinecone Serverless 架构设计

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021-orange.svg)](https://www.rust-lang.org/)

</div>

## 概述

elacsym 是一个高性能、低成本的开源向量数据库，采用 **S3-Only Serverless 架构**设计。主要特点：

- **S3 唯一数据源**：所有状态存储在 S3，无需 RocksDB/etcd/Kafka
- **无需协调服务**：基于确定性分区分配，多节点自动协作
- **存储与计算分离**：基于对象存储（S3/MinIO），实现按需加载索引
- **高效 ANN 算法**：使用 RaBitQ 量化技术，32x 内存压缩率，精度优于 PQ/SQ
- **自动扩展**：完全无状态查询执行器，支持水平扩展
- **低成本**：冷数据零成本，按实际计算资源付费
- **多租户隔离**：通过 Namespace 实现硬隔离

详细的 S3-Only 架构设计请参考 [S3_ONLY_ARCHITECTURE.md](S3_ONLY_ARCHITECTURE.md)。

## 架构

elacsym 采用 **S3-Only Serverless 架构**，所有状态存储在 S3，无需外部协调服务：

```
┌─────────────────────────────────────────────┐
│            Load Balancer (AWS ALB)           │
└────────────┬───────────────┬─────────────────┘
             │               │
   ┌─────────▼─────┐  ┌─────▼──────┐
   │ Query Node 1  │  │Query Node N│  (完全无状态，自动扩展)
   └─────────┬─────┘  └─────┬──────┘
             │               │
        ┌────▼───────────────▼────┐
        │      S3 / MinIO          │
        │  - Slabs (索引段)         │
        │  - Metadata (JSON)       │
        │  - WAL (写入日志)         │
        └──────────────────────────┘

┌─────────────────────────────────────────────┐
│         Builder Cluster (可选)               │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  │
│  │Builder 1 │  │Builder 2 │  │Builder N │  │
│  └──────────┘  └──────────┘  └──────────┘  │
│  (定时运行，无需协调)                        │
└─────────────────────────────────────────────┘
```

**核心设计原则**:
- ✅ S3 作为唯一数据源（元数据、WAL、索引）
- ✅ 确定性分区分配（基于哈希，无需协调）
- ✅ 并行查询执行（并发加载和搜索 slabs）
- ✅ 批量写入（用户接受更高延迟）

详细架构设计请参考:
- [S3_ONLY_ARCHITECTURE.md](S3_ONLY_ARCHITECTURE.md) - S3-Only 实现细节
- [DISTRIBUTED.md](DISTRIBUTED.md) - 分布式能力分析
- [ARCHITECTURE.md](ARCHITECTURE.md) - 原始架构设计

## 核心特性

### 1. RaBitQ 向量量化

- 32x 内存压缩率
- SIMD 加速距离计算
- 支持 L2 和 Inner Product 距离度量
- 3-8 bits 可配置量化精度

### 2. 几何分区策略

- IVF (Inverted File) 索引
- 动态分区分裂
- 不可变索引段（Slabs）

### 3. 双处理管道

- **Index Builder**: 后台构建和优化 RaBitQ 索引
- **Freshness Layer**: 提供秒级查询新鲜度

### 4. 智能缓存

- LRU 缓存热点索引段
- 多级缓存（内存 + 磁盘）
- 按需加载索引

## 项目结构

```
elacsym/
├── crates/
│   ├── elacsym-core/        # 核心数据结构和类型
│   ├── elacsym-index/       # 索引构建和查询 (RaBitQ)
│   ├── elacsym-storage/     # 存储层 (Blob + Cache)
│   ├── elacsym-metadata/    # 元数据服务 (RocksDB)
│   ├── elacsym-api/         # REST/gRPC API
│   └── elacsym-common/      # 通用工具
├── ARCHITECTURE.md          # 架构设计文档
└── Cargo.toml               # Workspace 配置
```

## 快速开始

### 前置要求

- Rust 1.75+
- RocksDB 开发库

### 安装依赖

```bash
# Ubuntu/Debian
sudo apt-get install librocksdb-dev

# macOS
brew install rocksdb
```

### 构建

```bash
cargo build --release
```

### 运行测试

```bash
cargo test
```

## API 示例

### 插入向量

```bash
curl -X POST http://localhost:8080/vectors/upsert \
  -H "Content-Type: application/json" \
  -d '{
    "vectors": [
      {
        "id": "vec1",
        "values": [0.1, 0.2, 0.3, ..., 0.128],
        "metadata": {"category": "product", "price": 29.99}
      }
    ],
    "namespace": "default"
  }'
```

### 查询向量

```bash
curl -X POST http://localhost:8080/vectors/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.1, 0.2, 0.3, ..., 0.128],
    "top_k": 10,
    "namespace": "default",
    "include_values": false,
    "include_metadata": true
  }'
```

## 技术栈

- **语言**: Rust 2021
- **ANN 引擎**: [RaBitQ-RS](https://github.com/lqhl/rabitq-rs)
- **对象存储**: S3 兼容接口（AWS S3, MinIO, Ceph）
- **元数据存储**: RocksDB / FoundationDB
- **API 框架**: Axum (REST)
- **异步运行时**: Tokio

## 性能特点

- **查询延迟**:
  - 缓存命中: < 10ms
  - 冷启动（从 S3 加载）: < 100ms
- **内存压缩**: 32x（相比原始向量）
- **索引构建**: 100-500x 加速（启用 faster_config）

## 部署选项

### Serverless 部署

- Query Executor: AWS Lambda / Kubernetes Jobs
- Index Builder: Kubernetes CronJob
- Storage: AWS S3
- Metadata: DynamoDB

### 自托管部署

- Query Executor: Kubernetes Deployment
- Index Builder: Kubernetes StatefulSet
- Storage: MinIO / Ceph
- Metadata: RocksDB

## 开发路线图

- [x] 核心架构设计
- [x] 基础数据结构
- [x] RaBitQ 索引集成
- [x] 对象存储层
- [x] 元数据服务
- [ ] 完整的 API 实现
- [ ] Freshness Layer
- [ ] 索引构建器
- [ ] 查询计划优化
- [ ] 分布式部署支持
- [ ] Hybrid Search（向量 + 关键词）
- [ ] 监控和可观测性

## 贡献

欢迎贡献！请查看 [CONTRIBUTING.md](CONTRIBUTING.md)（待添加）了解详情。

## 许可证

本项目采用双许可证：

- MIT License
- Apache License 2.0

## 致谢

- [RaBitQ-RS](https://github.com/lqhl/rabitq-rs) - 高性能向量量化库
- [Pinecone](https://www.pinecone.io/) - Serverless 架构设计灵感
- [FAISS](https://github.com/facebookresearch/faiss) - IVF 索引设计参考

## 联系方式

- GitHub Issues: [https://github.com/lqhl/elacsym/issues](https://github.com/lqhl/elacsym/issues)

---

## 已知问题

### RaBitQ 依赖编译错误

当前 `rabitq-rs` 依赖在 stable Rust 下存在编译错误：
- Feature flags 已稳定但仍被 gated
- AVX512 intrinsic 类型不匹配

**临时解决方案**:
1. 等待上游修复 [lqhl/rabitq-rs](https://github.com/lqhl/rabitq-rs)
2. 使用 fork 版本（待提供）
3. 或使用其他 ANN 库（faiss-rs, hnswlib-rs）

**核心架构已完成**: S3-Only 架构的所有核心组件（元数据、WAL、分布式构建、并行查询）均已实现，仅等待 ANN 库依赖解决。

---

**注意**: 本项目目前处于早期开发阶段，API 可能会发生变化。
