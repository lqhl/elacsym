# Elacsym - Claude 工作指南

> 本文档专为 Claude Code 准备，用于跨会话工作时快速上下文恢复

**最后更新**: 2025-10-05
**项目状态**: 🚀 **Phase 1.5 完成！** 查询流程端到端可用，带缓存优化

---

## 📋 快速状态检查

### ✅ 已完成
- [x] 项目结构和依赖配置
- [x] 存储抽象层（S3 + Local FS）
- [x] 核心类型系统 (types.rs, error.rs)
- [x] Manifest 数据结构和持久化（带测试）
- [x] Segment Parquet 读写（带测试）
- [x] RaBitQ 向量索引集成（带包装层和测试）
- [x] Namespace 管理器（整合所有组件，带测试）
- [x] NamespaceManager 状态管理
- [x] HTTP API Handlers 完整实现
- [x] Axum 服务器集成
- [x] API 路由框架
- [x] 设计文档 (docs/DESIGN.md)
- [x] CLAUDE.md 工作指南
- [x] **Segment 文档读取功能** ✨ NEW (Session 5)
- [x] **Foyer 缓存集成（Memory + Disk）** ✨ NEW (Session 5)
- [x] **完整查询流程：索引搜索 → 读取 Segment → 返回文档** ✨ NEW (Session 5)

**✨ 当前可用功能**:
- ✅ 创建 namespace (PUT /v1/namespaces/:namespace)
- ✅ 插入文档 (POST /v1/namespaces/:namespace/upsert)
- ✅ 向量查询 (POST /v1/namespaces/:namespace/query) - **返回完整文档！**
- ✅ 缓存加速（segments 自动缓存到 Memory/Disk）
- ✅ 服务器运行在端口 3000

### 🎯 下一步（Phase 2 - 高优先级）

### 📅 待办
- [ ] **属性过滤执行器** - QueryRequest 类型已定义，需要实现执行逻辑
- [ ] **Tantivy 全文搜索** - 集成 BM25
- [ ] **混合搜索 RRF** - Late Fusion 融合算法
- [ ] **WAL 写入日志** - 保证一致性（生产必需）
- [ ] Tombstone 删除机制
- [ ] LSM-tree 风格的 Compaction
- [ ] 分布式支持

---

## 🎯 项目核心目标

构建一个**开源的、基于对象存储的向量数据库**，inspired by turbopuffer：

### 关键特性
1. **成本优化**: 使用 S3 存储冷数据，成本降低 100x
2. **高性能**: RaBitQ 量化 + 多级缓存
3. **混合搜索**: 向量 + 全文 + 属性过滤
4. **可扩展**: Serverless 友好架构

### 技术栈
- **存储**: S3 (aws-sdk-s3) + Local FS
- **索引**: RaBitQ-rs (量化向量索引)
- **缓存**: Foyer (memory + disk)
- **全文**: Tantivy
- **格式**: Arrow + Parquet (列式存储)
- **API**: Axum

---

## 🏗️ 架构概览

```
┌─────────────────────────────────────────┐
│         HTTP API (Axum)                 │
├─────────────────────────────────────────┤
│  NamespaceManager (核心协调器)          │
│  ├── WriteCoordinator                   │
│  └── QueryExecutor                      │
├─────────────────────────────────────────┤
│  Index Layer                            │
│  ├── VectorIndex (RaBitQ)               │
│  └── FullTextIndex (Tantivy)            │
├─────────────────────────────────────────┤
│  Cache Layer (Foyer)                    │
│  ├── Memory (4GB)                       │
│  └── Disk (100GB NVMe)                  │
├─────────────────────────────────────────┤
│  Segment Manager                        │
│  ├── SegmentWriter (Parquet)            │
│  └── SegmentReader (Parquet)            │
├─────────────────────────────────────────┤
│  Storage Backend                        │
│  ├── S3Storage                          │
│  └── LocalStorage                       │
└─────────────────────────────────────────┘
```

---

## 📂 代码结构

```
src/
├── api/
│   ├── mod.rs           # API 路由
│   └── handlers.rs      # HTTP handlers
├── cache/
│   └── mod.rs           # Foyer 缓存封装
├── index/
│   ├── vector.rs        # RaBitQ 索引
│   └── fulltext.rs      # Tantivy 索引
├── manifest/
│   └── mod.rs           # Namespace 元数据
├── segment/
│   └── mod.rs           # Parquet 段管理
├── storage/
│   ├── mod.rs           # 存储抽象
│   ├── s3.rs            # S3 实现
│   └── local.rs         # 本地 FS 实现
├── query/
│   └── mod.rs           # 查询类型定义
├── types.rs             # 核心类型
├── error.rs             # 错误类型
├── lib.rs               # 库入口
└── main.rs              # 服务器入口
```

---

## 🔑 关键设计决策

### 1. **写入流程**（参考 docs/DESIGN.md）
```
Client → Validation → Buffer → Flush to S3 → Async Index Update → Update Manifest
```

- **立即持久化**: 所有写入都直接 flush 到 S3，即使只有 1 条记录
- **异步索引**: Segment 写入后立即返回，索引在后台更新
- **Tombstone**: 删除通过标记实现，不物理删除

### 2. **查询流程**
```
Load Manifest → Load Index → Search → Fetch Segments → Re-rank → Return
```

- **Late Fusion**: 向量搜索和全文搜索独立执行，最后用 RRF 融合
- **缓存优先**: Manifest/Index 在 Memory，Segment 在 Disk
- **Range Fetch**: 使用 S3 Range GET 只读取需要的行

### 3. **RaBitQ 限制**
- ❌ **不支持增量更新**: 添加新向量需要重建索引
- ❌ **不支持删除**: 需要重建索引
- ✅ **策略**: 新写入追加到新 segment，后台定期 compaction + 重建索引

### 4. **Compaction 策略**（参考 LSM-tree）
- **触发条件**: Segment 数量 > 100 或总大小超过阈值
- **后台任务**: 合并小 segments → 重建索引 → 更新 manifest
- **原子性**: 使用版本号 + 临时文件

---

## 🛠️ 代码约定

### 错误处理
```rust
use crate::{Error, Result};

// 使用 Result<T> 作为返回类型
pub fn some_function() -> Result<()> {
    storage.get(key).await
        .map_err(|e| Error::storage(format!("failed to get: {}", e)))?;
    Ok(())
}
```

### 异步函数
```rust
// 所有 I/O 操作必须是 async
#[async_trait]
pub trait StorageBackend: Send + Sync {
    async fn get(&self, key: &str) -> Result<Bytes>;
}
```

### 测试
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_something() {
        // 使用 tempfile 创建临时目录
    }
}
```

---

## 📝 重要文件位置

### 配置
- `config.toml` - 服务器配置
- `Cargo.toml` - 依赖管理

### 文档
- `docs/DESIGN.md` - **核心设计文档**（必读！）
- `docs/README.md` - 快速开始指南
- `README.md` - 项目首页
- `CLAUDE.md` - 本文档

### 数据格式
```json
// Manifest 示例 (manifest.json)
{
  "version": 123,
  "namespace": "my_ns",
  "schema": {
    "vector_dim": 768,
    "vector_metric": "cosine",
    "attributes": {...}
  },
  "segments": [
    {
      "segment_id": "seg_001",
      "file_path": "segments/seg_001.parquet",
      "row_count": 10000,
      "id_range": [1, 10000],
      "tombstones": []
    }
  ],
  "indexes": {
    "vector": "indexes/vector_index.bin"
  }
}
```

---

## 🚀 如何继续开发

### 1. 恢复上下文
```bash
cd /data00/home/liuqin.v/workspace/elacsym
cat CLAUDE.md                    # 读取本文档
cat docs/DESIGN.md               # 查看设计文档
cargo check                      # 确认编译通过
git status                       # 查看当前变更
```

### 2. 当前优先级任务

#### 🔴 P0 - 核心功能（MVP 必需）

**2.1 实现 Manifest 持久化**
- 位置: `src/manifest/mod.rs`
- 任务:
  - [ ] 添加 `ManifestManager` 结构
  - [ ] 实现 `load_manifest(namespace)` - 从 S3 读取
  - [ ] 实现 `save_manifest(manifest)` - 写入 S3（原子性）
  - [ ] 添加单元测试
- 关键点: 使用 `{namespace}/manifest.json` 作为 key

**2.2 实现 Segment Parquet 读写**
- 位置: `src/segment/mod.rs`
- 任务:
  - [ ] 完成 `documents_to_record_batch()` - Document → Arrow
  - [ ] 完成 `read_parquet()` - Parquet → Document
  - [ ] 处理 vector 列（FixedSizeList）
  - [ ] 处理动态 attributes 列
  - [ ] 添加集成测试
- 难点: Arrow schema 动态生成

**2.3 集成 RaBitQ**
- 位置: `src/index/vector.rs`
- 任务:
  - [ ] 研究 rabitq-rs API（查看 docs.rs）
  - [ ] 实现 `VectorIndex::new()` - 创建索引
  - [ ] 实现 `add()` - 批量添加向量
  - [ ] 实现 `search()` - ANN 搜索
  - [ ] 实现序列化/反序列化
  - [ ] 添加 benchmark
- 参考: https://docs.rs/rabitq/0.2.2/rabitq/

**2.4 创建 NamespaceManager**
- 位置: `src/namespace/mod.rs` (新文件)
- 任务:
  - [ ] 整合 Manifest + Storage + Index
  - [ ] 实现 `create_namespace(schema)`
  - [ ] 实现 `upsert(documents)` - 写入流程
  - [ ] 实现 `query(request)` - 查询流程
  - [ ] 使用 DashMap 缓存 Namespace 实例

**2.5 实现 API Handlers**
- 位置: `src/api/handlers.rs`
- 任务:
  - [ ] 实现 `create_namespace()` - 连接 NamespaceManager
  - [ ] 实现 `upsert()` - 调用 namespace.upsert()
  - [ ] 实现 `query()` - 调用 namespace.query()
  - [ ] 添加错误处理和日志

#### 🟡 P1 - 高级特性
- Foyer 缓存集成
- Tantivy 全文搜索
- 混合搜索（RRF）

#### 🟢 P2 - 优化
- Compaction 后台任务
- 性能优化
- 监控和 metrics

### 3. 开发工作流

```bash
# 1. 开始新功能
cargo check                      # 确保编译通过
cargo test                       # 确保测试通过

# 2. 实现功能
# ... 编写代码 ...

# 3. 测试
cargo test --lib <module>        # 单元测试
cargo test --test <integration>  # 集成测试

# 4. 更新文档
# 更新 CLAUDE.md 的"已完成"部分
# 更新 README.md 的 Roadmap
# 更新 docs/DESIGN.md（如有设计变更）

# 5. 提交前检查
cargo check
cargo clippy -- -D warnings
cargo fmt
```

### 4. 常用命令

```bash
# 编译检查
cargo check

# 运行测试
cargo test

# 运行服务器
cargo run

# 格式化代码
cargo fmt

# Lint
cargo clippy

# 查看依赖
cargo tree

# 更新依赖
cargo update
```

---

## 🐛 已知问题和待解决

### 当前问题
1. **Foyer API 变更**: foyer 0.12 的 API 与最新版本不兼容
   - 临时方案: cache/mod.rs 中实现了 stub
   - 计划: Phase 2 升级到 foyer 0.20+

2. **RaBitQ 不支持增量更新**
   - 影响: 每次添加向量需要重建索引
   - 缓解: 批量写入 + 后台 compaction

3. **Parquet 动态 schema**
   - 挑战: attributes 是动态的 HashMap
   - 方案: 在 Manifest 中定义 schema，创建 Arrow schema

### 技术债务
- [ ] 添加更多单元测试
- [ ] 实现 proper error recovery
- [ ] 添加 tracing spans
- [ ] 性能 profiling

---

## 📚 参考资源

### 文档
- [Turbopuffer Architecture](https://turbopuffer.com/docs/architecture)
- [RaBitQ Paper](https://arxiv.org/abs/2405.12497)
- [Arrow Rust 文档](https://docs.rs/arrow/latest/arrow/)
- [Parquet Rust 文档](https://docs.rs/parquet/latest/parquet/)
- [Tantivy Book](https://docs.rs/tantivy/latest/tantivy/)

### Crates.io
- rabitq: https://docs.rs/rabitq/0.2.2/rabitq/
- foyer: https://docs.rs/foyer/0.12.2/foyer/
- axum: https://docs.rs/axum/latest/axum/
- aws-sdk-s3: https://docs.rs/aws-sdk-s3/latest/aws_sdk_s3/

---

## 🔄 变更日志

### 2025-10-05 (Session 5 - 查询流程完善 + 缓存集成 ✅)
- ✅ 实现 Segment 文档读取功能
  - `SegmentReader::read_documents_by_ids()` - 按 ID 过滤读取
  - 利用 HashSet 高效查找
- ✅ 实现 Foyer 缓存集成
  - `CacheManager` 完整实现（替换 stub）
  - Memory + Disk 两层缓存
  - `get_or_fetch()` 模式简化缓存逻辑
  - 缓存键设计：`manifest:{ns}`, `vidx:{ns}`, `seg:{ns}:{seg_id}`
- ✅ 更新 Namespace::query() 完整流程
  - Step 1: 向量索引搜索 → 候选 doc_ids
  - Step 2: 按 segment 分组
  - Step 3: 从缓存/存储读取 segment 数据
  - Step 4: 提取文档并按顺序返回
- ✅ 更新 API handlers
  - 支持 `include_vector` / `include_attributes` 控制返回字段
  - 查询响应包含完整文档数据
- ✅ 集成到 main.rs
  - 环境变量 `ELACSYM_CACHE_PATH` 配置缓存路径
  - 环境变量 `ELACSYM_DISABLE_CACHE` 可禁用缓存
  - 缓存初始化失败时降级为无缓存模式
- ✅ 更新文档（README + CLAUDE.md）

**技术亮点**:
- **缓存策略学习 Turbopuffer**: Segment 数据缓存到 Disk，Manifest/Index 缓存到 Memory
- **查询流程完整**: 不再只返回 ID，而是完整的 Document 对象
- **优雅降级**: 缓存不可用时自动回退到直接存储读取
- **环境变量配置**: 灵活控制缓存行为

**测试状态**: 11/11 单元测试通过（新增 3 个缓存测试）

### 2025-10-05 (Session 4 - HTTP API 完成 ✅)
- ✅ 实现 NamespaceManager 状态管理
  - 多 namespace 管理与缓存
  - create_namespace / get_namespace / list_namespaces
- ✅ 实现 HTTP API handlers (`src/api/handlers.rs`)
  - create_namespace (PUT /v1/namespaces/:namespace)
  - upsert (POST /v1/namespaces/:namespace/upsert)
  - query (POST /v1/namespaces/:namespace/query)
- ✅ 更新 main.rs 集成 Axum 服务器
  - NamespaceManager 作为 State
  - 环境变量 ELACSYM_STORAGE_PATH 配置
- ✅ HTTP API 端到端测试
  - ✅ 健康检查
  - ✅ 创建 namespace
  - ✅ 插入 3 个文档
  - ✅ 向量查询 (67ms 响应)
- ✅ 添加 InvalidRequest 错误类型
- ✅ 更新文档（README 标记 **MVP 100% 完成**）

**技术亮点**:
- Axum State pattern 实现依赖注入
- 错误处理使用 (StatusCode, String) 元组
- Query 响应包含耗时统计（took_ms）
- **🎉 Phase 1 MVP 完成！服务器运行正常！**

**测试结果**: 8/8 单元测试通过 + HTTP API 集成测试通过

### 2025-10-05 (Session 3 - 深夜)
- ✅ 实现 RaBitQ 向量索引集成（`src/index/vector.rs`）
- ✅ 实现 Namespace 管理器（`src/namespace/mod.rs`）
- ✅ 添加向量索引测试（2个测试通过）
- ✅ 添加 Namespace 测试（2个测试通过）
- ✅ 所有测试通过（8/8 tests passed）

**技术亮点**:
- **RaBitQ 包装层**: 处理不支持增量更新的限制
  - 存储原始向量用于重建索引
  - DocId 映射（external ID ↔ internal index）
  - 懒加载索引构建
  - 自动生成质心（k-means++ style）
  - fvecs 文件格式写入
- **Namespace 整合**: 统一管理 Manifest + Storage + Index + Segments
  - 并发安全（RwLock）
  - 完整的 upsert 流程
  - 向量搜索功能

**代码统计**: ~800 行新代码，8 个测试全部通过

### 2025-10-05 (Session 2 - 晚上)
- ✅ 实现 ManifestManager 持久化（S3 读写）
- ✅ 实现 Segment Parquet 读写（完整的 Arrow 转换）
- ✅ 添加 Manifest 和 Segment 单元测试（全部通过）
- ✅ 更新 CLAUDE.md 和 README.md

**技术亮点**:
- Parquet 动态 schema 处理（支持动态attributes）
- FixedSizeList 处理向量字段
- Bytes 直接实现 ChunkReader（无需 Cursor）

### 2025-10-05 (Session 1 - 早上)
- ✅ 初始化项目结构
- ✅ 实现 Storage 抽象层（S3 + Local FS）
- ✅ 完成核心类型定义
- ✅ 编写设计文档 (docs/DESIGN.md)
- ✅ 创建 CLAUDE.md 工作指南

---

## 💡 提示

### 给未来的 Claude
1. **先读设计文档**: docs/DESIGN.md 有完整的写入/查询流程
2. **保持一致性**: 遵循现有的代码风格和错误处理模式
3. **测试优先**: 每个模块都应该有测试
4. **更新文档**: 完成功能后更新 CLAUDE.md 和 README.md
5. **性能意识**: 这是一个性能敏感的项目，注意避免不必要的拷贝和分配

### 调试技巧
```bash
# 启用详细日志
RUST_LOG=elacsym=debug,tower_http=debug cargo run

# 查看 S3 请求
RUST_LOG=aws_sdk_s3=debug cargo run

# 性能分析
cargo build --release
perf record ./target/release/elacsym
```

### 常见陷阱
- ❌ 忘记 `.await` 在异步函数中
- ❌ 使用 `unwrap()` 而不是 `?`
- ❌ 在 trait 中忘记 `Send + Sync`
- ❌ Parquet 文件路径使用绝对路径（应该相对于 namespace）

---

**祝编码愉快！记得经常提交并更新文档。**
