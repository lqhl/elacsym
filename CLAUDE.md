# Elacsym - Claude 工作指南

> 本文档专为 Claude Code 准备，用于跨会话工作时快速上下文恢复

**最后更新**: 2025-10-05
**项目状态**: 🚀 **Phase 3 进行中！** P1-2 Background Compaction Manager 完成

---

## 📋 快速状态检查

### ✅ Phase 1: MVP (100% 完成)
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

### ✅ Phase 2: Advanced Features (100% 完成)
- [x] **Segment 文档读取功能** (Session 5)
- [x] **Foyer 缓存集成（Memory + Disk）** (Session 5)
- [x] **完整查询流程：索引搜索 → 读取 Segment → 返回文档** (Session 5)
- [x] **属性过滤执行器** (Session 5-6)
  - FilterExecutor with Eq, Ne, Gt, Gte, Lt, Lte, Contains, ContainsAny
- [x] **Tantivy 全文搜索** (Session 6)
  - BM25 算法
  - 单字段和多字段搜索
  - 每个字段可配置权重
- [x] **RRF 融合算法** (Session 6)
  - src/query/fusion.rs - 完整实现
  - 支持向量 + 全文混合搜索
- [x] **高级全文配置** (Session 6)
  - Language, stemming, stopwords, case sensitivity
  - FullTextConfig enum (向后兼容)
- [x] **Write-Ahead Log (WAL)** (Session 6)
  - MessagePack + CRC32 格式
  - 崩溃安全的写入
  - 集成到 upsert 流程

**✨ 当前可用功能**:
- ✅ 创建 namespace (PUT /v1/namespaces/:namespace)
- ✅ 插入文档 (POST /v1/namespaces/:namespace/upsert) - **带 WAL 保护！**
- ✅ 向量查询 - 返回完整文档
- ✅ 全文搜索 - BM25 + 多字段 + 权重
- ✅ 混合搜索 - RRF 融合向量 + 全文结果
- ✅ 属性过滤 - 所有常见操作符
- ✅ 缓存加速 - segments 自动缓存到 Memory/Disk
- ✅ 服务器运行在端口 3000

### 🎯 Phase 3: Production Readiness (进行中)

#### 🔴 P0 - 生产必需
1. ✅ **WAL Recovery** - 启动时重放未提交操作 (Session 6)
2. **WAL Rotation** - 防止 WAL 无限增长 🔜
3. **Tantivy Analyzer Config** - 应用高级全文配置
4. **Error Recovery** - 优雅处理损坏数据
5. **Integration Tests** - 端到端测试

#### 🟡 P1 - 性能与可靠性
1. ✅ **LSM-tree Compaction** - 合并小 segments (Session 6)
2. ✅ **Background Compaction Manager** - 自动后台压缩 (Session 7)
3. **Metrics & Monitoring** - Prometheus 指标 🔜
4. **Benchmarks** - 性能测试套件
5. **Query Optimizer** - 基于代价的查询计划

#### 🟢 P2 - 高级功能
1. **Distributed Mode** - 多节点部署
2. **Replication** - 数据冗余
3. **Snapshot & Restore** - 备份/恢复
4. **Query Caching** - 缓存查询结果
5. **Bulk Import** - 快速批量导入

---

## 🎯 项目核心目标

构建一个**开源的、基于对象存储的向量数据库**，inspired by turbopuffer：

### 关键特性
1. **成本优化**: 使用 S3 存储冷数据，成本降低 100x
2. **高性能**: RaBitQ 量化 + 多级缓存 + RRF 融合
3. **混合搜索**: 向量 + 全文 + 属性过滤
4. **可扩展**: Serverless 友好架构
5. **可靠性**: WAL 保证写入不丢失

### 技术栈
- **存储**: S3 (aws-sdk-s3) + Local FS
- **索引**: RaBitQ-rs (量化向量索引)
- **缓存**: Foyer (memory + disk)
- **全文**: Tantivy (BM25)
- **格式**: Arrow + Parquet (列式存储)
- **API**: Axum
- **WAL**: MessagePack + CRC32

---

## 🏗️ 架构概览

```
┌─────────────────────────────────────────┐
│         HTTP API (Axum)                 │
├─────────────────────────────────────────┤
│  NamespaceManager (核心协调器)          │
│  ├── WriteCoordinator (with WAL)        │
│  └── QueryExecutor (with RRF)           │
├─────────────────────────────────────────┤
│  Index Layer                            │
│  ├── VectorIndex (RaBitQ)               │
│  └── FullTextIndex (Tantivy BM25)       │
├─────────────────────────────────────────┤
│  Query Layer                            │
│  ├── FilterExecutor (属性过滤)          │
│  └── RRF Fusion (混合搜索)              │
├─────────────────────────────────────────┤
│  Cache Layer (Foyer)                    │
│  ├── Memory (Manifest/Index)            │
│  └── Disk (Segments)                    │
├─────────────────────────────────────────┤
│  Segment Manager                        │
│  ├── SegmentWriter (Parquet)            │
│  └── SegmentReader (Parquet)            │
├─────────────────────────────────────────┤
│  WAL (Write-Ahead Log)                  │
│  └── Crash-safe persistence             │
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
│   └── mod.rs           # Foyer 缓存封装 ✅
├── index/
│   ├── vector.rs        # RaBitQ 索引 ✅
│   └── fulltext.rs      # Tantivy 索引 ✅
├── manifest/
│   └── mod.rs           # Namespace 元数据 ✅
├── segment/
│   └── mod.rs           # Parquet 段管理 ✅
├── storage/
│   ├── mod.rs           # 存储抽象 ✅
│   ├── s3.rs            # S3 实现 ✅
│   └── local.rs         # 本地 FS 实现 ✅
├── query/
│   ├── mod.rs           # 查询类型定义 ✅
│   ├── executor.rs      # 属性过滤器 ✅ NEW
│   └── fusion.rs        # RRF 融合算法 ✅ NEW
├── wal/
│   └── mod.rs           # Write-Ahead Log ✅ NEW
├── namespace/
│   └── mod.rs           # Namespace 管理 ✅
├── types.rs             # 核心类型 ✅
├── error.rs             # 错误类型 ✅
├── lib.rs               # 库入口 ✅
└── main.rs              # 服务器入口 ✅
```

---

## 🔑 关键设计决策

### 1. **写入流程（带 WAL）**
```
Client → Validation →
  ↓ WAL Write + Sync (durability!) →
  ↓ Flush to S3 →
  ↓ Update Index →
  ↓ Update Manifest →
  ↓ Truncate WAL →
Return Success
```

- **WAL 优先**: 所有写入先写 WAL，fsync 后才继续
- **原子提交**: Manifest 更新成功后才 truncate WAL
- **崩溃恢复**: 启动时读取 WAL 重放未提交操作（TODO）

### 2. **查询流程（带 RRF）**
```
Parse Request →
  ↓ Apply Filter (if present) →
  ↓ Vector Search (if present) →
  ↓ Full-Text Search (if present) →
  ↓ RRF Fusion →
  ↓ Fetch Segments (with cache) →
  ↓ Assemble Documents →
Return Results
```

- **Late Fusion**: 向量和全文独立执行，RRF 合并结果
- **缓存优先**: Manifest/Index 在 Memory，Segment 在 Disk
- **过滤器前置**: 先过滤再搜索，减少计算量

### 3. **RaBitQ 限制**
- ❌ **不支持增量更新**: 添加新向量需要重建索引
- ❌ **不支持删除**: 需要重建索引
- ✅ **策略**: 新写入追加到新 segment，后台定期 compaction + 重建索引

### 4. **Compaction 策略（待实现）**
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
- `docs/SESSION_5_SUMMARY.md` - Cache 集成总结
- `docs/SESSION_6_SUMMARY.md` - 高级功能总结（RRF, WAL, 多字段）
- `docs/FULLTEXT_COMPARISON.md` - Turbopuffer 全文对比
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
    "attributes": {
      "title": {
        "type": "string",
        "full_text": {
          "language": "english",
          "stemming": true,
          "remove_stopwords": true
        }
      }
    }
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
cat docs/SESSION_6_SUMMARY.md   # 查看最新进展
cargo check                      # 确认编译通过
git status                       # 查看当前变更
```

### 2. Phase 3 优先级任务

#### 🔴 P0 - WAL Recovery (必须先做)

**位置**: `src/wal/mod.rs` + `src/namespace/mod.rs`

**任务**:
1. 实现 `WalManager::replay()`
   - 读取所有 WAL entries
   - 解析操作类型
   - 返回待重放的操作列表

2. 更新 `Namespace::load()`
   - 创建 WAL manager 后立即调用 replay()
   - 对每个 Upsert 操作执行内部逻辑
   - 完成后 truncate WAL

3. 添加测试
   - 写入数据 → 不 truncate → 关闭 → 重新加载 → 验证数据完整

**代码示例**:
```rust
impl WalManager {
    pub async fn replay(&self) -> Result<Vec<WalOperation>> {
        let entries = self.read_all().await?;
        Ok(entries.into_iter().map(|e| e.operation).collect())
    }
}

impl Namespace {
    pub async fn load(...) -> Result<Self> {
        // ... 现有代码 ...

        let wal = WalManager::new(&wal_dir).await?;
        let operations = wal.replay().await?;

        for op in operations {
            match op {
                WalOperation::Upsert { documents } => {
                    // 重放 upsert（不写 WAL，避免递归）
                    self.upsert_internal(documents).await?;
                }
                _ => {}
            }
        }

        // 重放完成，truncate WAL
        wal.truncate().await?;

        // ... 返回 ...
    }
}
```

#### 🔴 P0 - WAL Rotation

**任务**:
- 当 WAL 文件 > 100MB 时自动轮转
- 保留最近 N 个 WAL 文件
- Cleanup 旧 WAL 文件

#### 🟡 P1 - Tantivy Analyzer Config

**任务**:
- 读取 `FullTextConfig` 设置
- 根据 language 选择 Tantivy analyzer
- 应用 stemming/stopwords 配置

#### 🟡 P1 - LSM-tree Compaction

**位置**: `src/namespace/compaction.rs` (新文件)

**任务**:
1. 实现 Compaction 触发逻辑
   - 监控 segment 数量
   - 后台任务定期检查

2. 实现 Compaction 流程
   - 选择需要合并的 segments
   - 合并数据到新 segment
   - 重建向量索引
   - 原子更新 manifest
   - 删除旧 segments

3. 添加配置项
   - `compaction.max_segments` = 100
   - `compaction.interval_secs` = 3600

#### 🟡 P1 - Metrics & Monitoring

**位置**: `src/metrics/mod.rs` (新文件)

**任务**:
- Prometheus metrics
  - query_duration_seconds (histogram)
  - upsert_duration_seconds (histogram)
  - cache_hit_rate (gauge)
  - segment_count (gauge)
  - wal_size_bytes (gauge)

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
# 更新 CLAUDE.md 的"变更日志"
# 更新 README.md 的 Roadmap
# 创建 SESSION_N_SUMMARY.md

# 5. 提交
git add -A
git commit -m "..."
git push
```

### 4. 常用命令

```bash
# 编译检查
cargo check

# 运行测试
cargo test

# 运行服务器
ELACSYM_STORAGE_PATH=./data cargo run

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
1. **WAL Recovery 未实现**
   - 影响: 崩溃后可能丢失未提交数据
   - 优先级: P0（生产必需）

2. **WAL 无限增长**
   - 影响: 磁盘空间耗尽
   - 优先级: P0

3. **Tantivy Analyzer 未配置**
   - 影响: 高级全文配置不生效
   - 优先级: P1

4. **RaBitQ 不支持增量更新**
   - 影响: 每次添加向量需要重建索引
   - 缓解: Compaction 后重建

5. **无 Compaction**
   - 影响: Segment 数量无限增长
   - 优先级: P1

### 技术债务
- [ ] 添加更多集成测试
- [ ] 实现 proper error recovery
- [ ] 添加 tracing spans
- [ ] 性能 profiling
- [ ] API 文档（OpenAPI/Swagger）

---

## 📚 参考资源

### 文档
- [Turbopuffer Architecture](https://turbopuffer.com/docs/architecture)
- [RaBitQ Paper](https://arxiv.org/abs/2405.12497)
- [RRF Paper](https://dl.acm.org/doi/10.1145/1571941.1572114)
- [Arrow Rust 文档](https://docs.rs/arrow/latest/arrow/)
- [Parquet Rust 文档](https://docs.rs/parquet/latest/parquet/)
- [Tantivy Book](https://docs.rs/tantivy/latest/tantivy/)

### Crates.io
- rabitq: https://docs.rs/rabitq/0.2.2/rabitq/
- foyer: https://docs.rs/foyer/0.12.2/foyer/
- axum: https://docs.rs/axum/latest/axum/
- aws-sdk-s3: https://docs.rs/aws-sdk-s3/latest/aws_sdk_s3/
- tantivy: https://docs.rs/tantivy/latest/tantivy/
- rmp-serde: https://docs.rs/rmp-serde/latest/rmp_serde/

---

## 🔄 变更日志

### 2025-10-05 (Session 7 - Background Compaction Manager ✅)
- ✅ 实现 CompactionConfig 配置结构
  - 可配置间隔、阈值、合并数量
  - 默认值：1小时间隔，100 segments 阈值
  - 测试友好配置支持
- ✅ 实现 CompactionManager 后台任务管理器
  - `src/namespace/compaction.rs` - 361 行
  - 自动后台检查和触发 compaction
  - 优雅启动/停止机制
  - 错误恢复和日志
- ✅ 集成到 NamespaceManager
  - 每个 namespace 自动启动 compaction manager
  - create_namespace/get_namespace 自动管理
  - 支持自定义配置
- ✅ 添加配置文件支持
  - config.toml [compaction] 节
  - interval_secs, max_segments, max_total_docs
- ✅ 完整测试覆盖
  - 4 个单元测试
  - 测试触发逻辑、生命周期、自动压缩
  - 39/39 全部测试通过

**代码统计**: +407 行, 4 个新测试

### 2025-10-05 (Session 6 - 高级功能完成 🎉)
- ✅ 实现多字段全文搜索
  - FullTextQuery enum (Single/Multi 变体)
  - 每字段可配置权重
  - 自动聚合多字段结果
- ✅ 实现 RRF 融合算法
  - `src/query/fusion.rs` - 215 行
  - 标准 k=60 参数
  - 支持可配置权重
  - 8 个单元测试
- ✅ 实现高级全文配置
  - FullTextConfig enum (向后兼容)
  - 支持 language, stemming, stopwords, case_sensitive
  - Helper 方法: is_enabled(), language(), 等
- ✅ 实现 Write-Ahead Log
  - `src/wal/mod.rs` - 404 行
  - MessagePack + CRC32 格式
  - append(), sync(), truncate()
  - 4 个单元测试（包括崩溃恢复）
- ✅ WAL 集成到 upsert 流程
  - WAL write → segment write → WAL truncate
  - 保证写入不丢失
- ✅ 更新文档
  - SESSION_6_SUMMARY.md (521 行)
  - 更新 README.md roadmap
  - 更新 CLAUDE.md

**代码统计**: +3696 行, 17 个新测试

### 2025-10-05 (Session 5 - 查询流程完善 + 缓存集成 ✅)
- ✅ 实现 Segment 文档读取
  - `read_documents_by_ids()` - HashSet 过滤
- ✅ 实现 Foyer 缓存集成
  - Memory + Disk 两层缓存
  - `get_or_fetch()` 模式
- ✅ 实现属性过滤
  - FilterExecutor - 318 行
  - 所有常见操作符
  - 5 个单元测试
- ✅ 更新 Namespace::query() 完整流程
- ✅ 集成到 main.rs
  - 环境变量配置

**测试状态**: 11/11 单元测试通过

### 2025-10-05 (Session 4 - HTTP API 完成 ✅)
- ✅ NamespaceManager 状态管理
- ✅ HTTP API handlers
- ✅ Axum 服务器集成
- ✅ 端到端测试通过

**测试状态**: 8/8 测试通过

### 2025-10-05 (Session 3 - 深夜)
- ✅ RaBitQ 向量索引集成
- ✅ Namespace 管理器
- ✅ 向量搜索功能

**代码统计**: ~800 行, 8 个测试通过

### 2025-10-05 (Session 2 - 晚上)
- ✅ ManifestManager 持久化
- ✅ Segment Parquet 读写
- ✅ 单元测试

### 2025-10-05 (Session 1 - 早上)
- ✅ 项目初始化
- ✅ Storage 抽象层
- ✅ 核心类型定义
- ✅ 设计文档

---

## 💡 提示

### 给未来的 Claude
1. **优先 WAL Recovery**: 这是 P0 任务，必须先实现
2. **参考 SESSION_6_SUMMARY.md**: 有详细的实现细节
3. **保持测试覆盖**: 每个新功能都要有测试
4. **更新文档**: 完成后创建 SESSION_N_SUMMARY.md
5. **性能意识**: 这是性能敏感项目

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
- ❌ WAL 和 upsert 递归调用（分离 upsert_internal）

---

**祝编码愉快！Phase 3 加油！🚀**
