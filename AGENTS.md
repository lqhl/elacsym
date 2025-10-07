# AGENTS.md - AI Agent Collaboration Guide

> Context document for AI agents (Codex, Claude Code, etc.) working on Elacsym

**Last Updated**: 2025-10-07
**Current Status**: 🎉 Distributed deployment ready!

---

## 📋 Quick Context Restore

### What is Elacsym?
Open-source, S3-based vector database with hybrid search (vector + full-text + filters).

### Recent Major Achievement
**PR: `codex/deploy-and-test-elacsym-with-minio`** (Merged 2025-10-07)
- ✅ Distributed deployment with indexer/query node roles
- ✅ Centralized configuration system (`src/config.rs`)
- ✅ Role-aware compaction gating
- ✅ S3-compatible storage (MinIO support)
- ✅ Enhanced multi-node testing (60/60 tests passing)

### Current Architecture
```
Single-Node Mode:
  ├── Indexer + Query (combined)
  ├── Local/S3 storage
  └── Local WAL

Distributed Mode:
  ├── Indexer Nodes (sharded by namespace)
  │   ├── Handle writes (upsert)
  │   ├── Run compaction
  │   └── Own namespace data
  └── Query Nodes (stateless)
      ├── Forward reads to responsible indexers
      └── No compaction or writes
```

---

## 🔑 Key Files for Agents

### Core Infrastructure
- `src/config.rs` - Configuration system (TOML + env vars)
- `src/main.rs` - Server initialization and role detection
- `src/api/state.rs` - Node roles (Indexer/Query)
- `src/namespace/mod.rs` - Namespace management + compaction gating
- `src/sharding.rs` - Consistent hashing for namespace distribution

### Storage & Persistence
- `src/storage/mod.rs` - Storage abstraction (S3/Local)
- `src/wal/mod.rs` - Local WAL implementation
- `src/wal/s3.rs` - S3-backed WAL (distributed mode)

### Indexes & Query
- `src/index/vector.rs` - RaBitQ vector index
- `src/index/fulltext.rs` - Tantivy full-text index
- `src/query/fusion.rs` - RRF hybrid search

### Documentation
- `CLAUDE.md` - Detailed project context for Claude Code
- `docs/architecture.md` - System design reference
- `docs/deployment.md` - Production deployment guide
- `README.md` - Project overview

---

## 🧠 Agent-Specific Notes

### For Codex (OpenAI Agents)
**Strengths Demonstrated**:
- Comprehensive refactoring (270 lines in `main.rs`)
- Configuration system design
- Multi-node testing infrastructure
- Breaking API changes with migration path

**Working Style**:
- Systematic: Config → Implementation → Testing
- Good validation (e.g., role mismatch detection)
- Thorough test coverage (integration + unit)

### For Claude Code (Anthropic)
**Strengths Demonstrated**:
- Fast iteration on core features (WAL, compaction, RRF)
- Performance-focused implementation
- Good error handling patterns
- Clear documentation updates

**Working Style**:
- Incremental: Feature → Test → Document
- Strong async/await expertise
- Excellent Rust idioms

---

## 🚀 Collaboration Workflow

### Before Starting Work
1. **Read context**: `CLAUDE.md` + `AGENTS.md` (this file)
2. **Check current state**: `git status`, `cargo test`
3. **Review recent PRs**: `git log --oneline -10`
4. **Announce intent**: Update "Current Work" section below

### During Development
1. **Follow conventions**: See `CLAUDE.md` "Code Conventions"
2. **Test continuously**: `cargo test --lib <module>`
3. **Document changes**: Update CLAUDE.md changelog
4. **Commit frequently**: Small, atomic commits

### After Completion
1. **Run full test suite**: `cargo test`
2. **Update documentation**: CLAUDE.md, AGENTS.md, README.md
3. **Create PR**: Clear description + test evidence
4. **Update roadmap**: Mark completed tasks

---

## 📊 Current Work (2025-10-07)

### In Progress
None - awaiting next task selection.

### Blocked
None

### Recently Completed
- ✅ Distributed deployment (Codex)
- ✅ Role-aware compaction (Codex)
- ✅ Configuration system (Codex)
- ✅ Multi-node testing (Codex)

---

## 🎯 Priority Task List

### 🔴 P0 - Critical Issues from PR Review
1. **Role validation in `src/main.rs:115`**
   - Add assertion: configured role matches distributed role
   - Prevent silent misconfigurations
   - **Estimated effort**: 30 min
   - **Files**: `src/main.rs`, add test in `tests/config_validation_test.rs`

2. **S3 WAL rotation atomicity**
   - Use temporary keys + rename for atomic operations
   - Prevent data loss during rotation failures
   - **Estimated effort**: 1 hour
   - **Files**: `src/wal/s3.rs`

3. **WAL API migration guide**
   - Document breaking change (WAL now mandatory)
   - Provide upgrade path for existing users
   - **Estimated effort**: 30 min
   - **Files**: `docs/CHANGELOG.md` (new), `README.md`

### 🟡 P1 - Performance & Observability
4. **Prometheus metrics** (from CLAUDE.md Phase 3 P1)
   - Query/upsert duration histograms
   - Cache hit rate gauge
   - WAL size gauge
   - Segment count gauge
   - **Estimated effort**: 3 hours
   - **Files**: `src/metrics/mod.rs` (new), `src/api/mod.rs`, `src/main.rs`

5. **Benchmark suite**
   - Criterion.rs benchmarks
   - Vector search performance
   - Hybrid search performance
   - Write throughput
   - **Estimated effort**: 4 hours
   - **Files**: `benches/` (new directory)

6. **Distributed deployment docs**
   - MinIO setup instructions
   - Multi-node configuration examples
   - Troubleshooting guide
   - **Estimated effort**: 2 hours
   - **Files**: `docs/deployment.md`, `examples/distributed/` (new)

### 🟢 P2 - Nice to Have
7. **Query optimizer** (from CLAUDE.md Phase 3 P1)
   - Cost-based query planning
   - Index selection heuristics
   - **Estimated effort**: 8 hours
   - **Files**: `src/query/optimizer.rs` (new)

8. **HTTPS/TLS support**
   - Certificate configuration
   - Let's Encrypt integration
   - **Estimated effort**: 3 hours
   - **Files**: `src/main.rs`, `src/config.rs`

9. **Client SDKs**
   - Python client
   - JavaScript/TypeScript client
   - Go client
   - **Estimated effort**: 20+ hours
   - **Files**: `clients/` (new directory)

---

## 🧪 Testing Guidelines

### Before Merging
```bash
# Full test suite
cargo test

# Check compilation
cargo check

# Linting
cargo clippy -- -D warnings

# Format check
cargo fmt --check

# Test with different storage backends
ELACSYM_STORAGE_BACKEND=local cargo test
# (S3 tests require MinIO/localstack)
```

### Integration Test Patterns
```rust
// Always use TempDir for test isolation
let temp_dir = TempDir::new().unwrap();
let storage = Arc::new(LocalStorage::new(temp_dir.path()).unwrap());

// Always specify node_id
let manager = Arc::new(NamespaceManager::new(storage, "test-node".to_string()));

// Test multi-node scenarios with TestCluster helper
let cluster = TestCluster::new(3).await; // 3 indexers
```

---

## 🐛 Known Technical Debt

### From Recent PR
1. ❌ No role mismatch validation (P0)
2. ❌ S3 WAL lacks atomic rotation (P0)
3. ❌ Missing distributed mode example (P1)
4. ❌ No S3 WAL rotation failure tests (P1)

### General
1. ⚠️ No OpenAPI/Swagger docs for HTTP API
2. ⚠️ No tracing spans (only logs)
3. ⚠️ WAL batching for high-throughput scenarios
4. ⚠️ S3 WAL adds 10-50ms latency per operation

---

## 💡 Tips for AI Agents

### When Stuck
1. **Read the architecture doc**: `docs/architecture.md`
2. **Check similar code**: Use grep to find patterns
3. **Run isolated tests**: `cargo test --lib <module> -- --nocapture`
4. **Check error types**: `src/error.rs` for error handling patterns

### Common Gotchas
- ❌ Forgetting `.await` in async functions
- ❌ Using `unwrap()` instead of `?` in library code
- ❌ Missing `Send + Sync` bounds in traits
- ❌ Not updating WAL before storage operations
- ❌ Hardcoding paths instead of using config

### Performance Awareness
- RaBitQ requires index rebuild for updates → batch operations
- Tantivy full-text index uses disk → cache aggressively
- S3 latency → minimize round-trips
- WAL fsync → batch when possible (future work)

### Collaboration Etiquette
- 📝 **Document assumptions**: Add comments for non-obvious logic
- 🧪 **Test edge cases**: Empty inputs, large data, concurrent access
- 📊 **Benchmark changes**: Use `cargo bench` for perf-critical code
- 💬 **Explain tradeoffs**: Document why you chose approach A over B

---

## 🎓 Learning Resources

### Rust Patterns
- [Async Rust Book](https://rust-lang.github.io/async-book/)
- [Tokio Tutorial](https://tokio.rs/tokio/tutorial)
- [Error Handling in Rust](https://doc.rust-lang.org/book/ch09-00-error-handling.html)

### Dependencies
- [RaBitQ Paper](https://arxiv.org/abs/2405.12497) - Vector quantization
- [Tantivy Book](https://docs.rs/tantivy/latest/tantivy/) - Full-text search
- [Foyer Docs](https://docs.rs/foyer/latest/foyer/) - Hybrid cache
- [Arrow/Parquet Guide](https://arrow.apache.org/docs/format/Columnar.html)

### Similar Systems
- [Turbopuffer Architecture](https://turbopuffer.com/docs/architecture) - Design inspiration
- [Qdrant](https://qdrant.tech/) - Vector DB comparison
- [Milvus](https://milvus.io/) - Vector DB comparison

---

## 📞 Contact & Feedback

**Project Maintainer**: Qin Liu (lqgy2001@gmail.com)
**Repository**: https://github.com/lqhl/elacsym

When reporting issues:
1. Include `cargo --version` and `rustc --version`
2. Provide minimal reproduction case
3. Attach relevant logs (`RUST_LOG=debug`)
4. Mention which agent generated the code (Codex/Claude/etc.)

---

## 🔄 Version History

### v0.3.0 (2025-10-07) - Distributed Deployment
- Centralized configuration system
- Indexer/Query node roles
- S3-compatible storage (MinIO)
- Role-aware compaction
- Enhanced multi-node testing

### v0.2.0 (2025-10-06) - Production Readiness
- WAL recovery and rotation
- LSM-tree compaction
- Background compaction manager
- 60/60 tests passing

### v0.1.0 (2025-10-05) - MVP Complete
- Core namespace management
- Vector + full-text + hybrid search
- RRF fusion algorithm
- Foyer cache integration
- WAL for durability

---

**Happy collaborating! 🤖🤝🤖**
