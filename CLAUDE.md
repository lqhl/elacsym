# Elacsym - Claude Code Context

> This document is designed for Claude Code AI assistant to quickly restore context across sessions.

**Last Updated**: 2025-10-06
**Project Status**: 🎉 **Production Ready** - All P0 features complete!

---

## Quick Status Check

### ✅ Phase 1: MVP (100% Complete)
- [x] Storage abstraction (S3 + Local FS)
- [x] Core types and error handling
- [x] Manifest persistence with versioning
- [x] Segment Parquet read/write
- [x] RaBitQ vector index integration
- [x] Namespace manager
- [x] HTTP API (Axum server)
- [x] Design documentation

###✅ Phase 2: Advanced Features (100% Complete)
- [x] Segment document retrieval
- [x] Foyer cache integration (Memory + Disk)
- [x] Attribute filtering (all operators)
- [x] Tantivy full-text search (BM25 + multi-field)
- [x] RRF fusion for hybrid search
- [x] Advanced full-text config (18 languages, stemming, stopwords)
- [x] Write-Ahead Log (WAL) with CRC32

### ✅ Phase 3: Production Readiness (P0 100% Complete)
#### P0 - Critical ✅
- [x] WAL Recovery - Replay uncommitted operations on startup
- [x] WAL Rotation - Prevent unbounded growth (100MB limit, keep last 5)
- [x] Error Recovery - Graceful corruption handling
- [x] Integration Tests - End-to-end testing (60/60 tests passing)
- [x] Tantivy Analyzer Config - Apply language-specific analyzers
- [x] LSM-tree Compaction - Merge small segments
- [x] Background Compaction Manager - Automatic segment merging

#### P1 - Performance & Reliability (40% Complete)
- [x] LSM-tree Compaction - Merge small segments
- [x] Background Compaction Manager - Auto-trigger compaction
- [ ] Metrics & Monitoring - Prometheus metrics
- [ ] Benchmarks - Performance testing suite
- [ ] Query Optimizer - Cost-based query planning

#### P2 - Advanced Features (Planned)
- [ ] Distributed Mode - Multi-node deployment (partial: sharding implemented)
- [ ] Replication - Data redundancy
- [ ] Snapshot & Restore - Backup/recovery
- [ ] Query Caching - Cache query results
- [ ] Bulk Import - Fast batch loading

**✨ Currently Available Features**:
- ✅ Create namespace (PUT /v1/namespaces/:namespace)
- ✅ Upsert documents (POST /v1/namespaces/:namespace/upsert) - WAL protected!
- ✅ Vector search - Returns full documents
- ✅ Full-text search - BM25 + multi-field + weights
- ✅ Hybrid search - RRF fusion of vector + full-text results
- ✅ Attribute filtering - All common operators
- ✅ Cache acceleration - Automatic segment caching to Memory/Disk
- ✅ Server runs on port 3000

---

## 🎯 Project Core Goals

Build an **open-source, object-storage-based vector database**, inspired by turbopuffer:

### Key Features
1. **Cost Optimization**: Use S3 for cold data, 100× cost reduction
2. **High Performance**: RaBitQ quantization + multi-tier caching + RRF fusion
3. **Hybrid Search**: Vector + full-text + attribute filtering
4. **Scalable**: Serverless-friendly architecture
5. **Reliability**: WAL ensures no data loss

### Tech Stack
- **Storage**: S3 (aws-sdk-s3) + Local FS
- **Index**: RaBitQ-rs (quantized vector index)
- **Cache**: Foyer (memory + disk)
- **Full-Text**: Tantivy (BM25)
- **Format**: Arrow + Parquet (columnar storage)
- **API**: Axum
- **WAL**: MessagePack + CRC32

---

## 🏗️ Architecture Overview

```
┌─────────────────────────────────────────┐
│         HTTP API (Axum)                 │
├─────────────────────────────────────────┤
│  NamespaceManager (Core Coordinator)    │
│  ├── WriteCoordinator (with WAL)        │
│  └── QueryExecutor (with RRF)           │
├─────────────────────────────────────────┤
│  Index Layer                            │
│  ├── VectorIndex (RaBitQ)               │
│  └── FullTextIndex (Tantivy BM25)       │
├─────────────────────────────────────────┤
│  Query Layer                            │
│  ├── FilterExecutor (attribute filters) │
│  └── RRF Fusion (hybrid search)         │
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

## 📂 Code Structure

```
src/
├── api/
│   ├── mod.rs           # API routes
│   └── handlers.rs      # HTTP handlers
├── cache/
│   └── mod.rs           # Foyer cache wrapper ✅
├── index/
│   ├── vector.rs        # RaBitQ index ✅
│   └── fulltext.rs      # Tantivy index ✅
├── manifest/
│   └── mod.rs           # Namespace metadata ✅
├── segment/
│   └── mod.rs           # Parquet segment manager ✅
├── storage/
│   ├── mod.rs           # Storage abstraction ✅
│   ├── s3.rs            # S3 implementation ✅
│   └── local.rs         # Local FS implementation ✅
├── query/
│   ├── mod.rs           # Query type definitions ✅
│   ├── executor.rs      # Attribute filters ✅
│   └── fusion.rs        # RRF fusion algorithm ✅
├── wal/
│   └── mod.rs           # Write-Ahead Log ✅
├── namespace/
│   ├── mod.rs           # Namespace management ✅
│   └── compaction.rs    # LSM-tree compaction ✅
├── types.rs             # Core types ✅
├── error.rs             # Error types ✅
├── lib.rs               # Library entry ✅
└── main.rs              # Server entry ✅
```

---

## 🔑 Key Design Decisions

### 1. Write Flow (with WAL)
```
Client → Validation →
  ↓ WAL Write + Sync (durability!) →
  ↓ Flush to S3 →
  ↓ Update Index →
  ↓ Update Manifest →
  ↓ Truncate WAL →
Return Success
```

- **WAL First**: All writes go to WAL, fsync before continuing
- **Atomic Commit**: Manifest update successful → truncate WAL
- **Crash Recovery**: On startup, read WAL and replay uncommitted operations

### 2. Query Flow (with RRF)
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

- **Late Fusion**: Vector and full-text execute independently, RRF merges results
- **Cache Priority**: Manifest/Index in Memory, Segment in Disk
- **Filter First**: Filter before search to reduce computation

### 3. RaBitQ Limitations
- ❌ **No incremental updates**: Adding new vectors requires index rebuild
- ❌ **No deletes**: Requires index rebuild
- ✅ **Strategy**: New writes append to new segment, periodic background compaction + rebuild index

### 4. Compaction Strategy
- **Trigger Conditions**: Segment count > 100 OR total docs > 1M
- **Background Task**: Merge small segments → rebuild index → update manifest
- **Atomicity**: Use version numbers + temporary files

---

## 🛠️ Code Conventions

### Error Handling
```rust
use crate::{Error, Result};

// Use Result<T> as return type
pub fn some_function() -> Result<()> {
    storage.get(key).await
        .map_err(|e| Error::storage(format!("failed to get: {}", e)))?;
    Ok(())
}
```

### Async Functions
```rust
// All I/O operations must be async
#[async_trait]
pub trait StorageBackend: Send + Sync {
    async fn get(&self, key: &str) -> Result<Bytes>;
}
```

### Testing
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_something() {
        // Use tempfile for temporary directories
    }
}
```

---

## 📝 Important File Locations

### Configuration
- `config.toml` - Server configuration
- `Cargo.toml` - Dependency management

### Documentation
- `docs/architecture.md` - **Core design doc** (must read!)
- `docs/api-reference.md` - Complete HTTP API documentation
- `docs/configuration.md` - Config file reference
- `docs/deployment.md` - Production deployment guide
- `docs/performance.md` - Performance tuning guide
- `docs/design-decisions.md` - Technical rationale + MyScale tribute
- `README.md` - Project homepage
- `CLAUDE.md` - This document

### Data Format Example
```json
// Manifest (manifest.json)
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

## 🚀 How to Continue Development

### 1. Restore Context
```bash
cd /data00/home/liuqin.v/workspace/elacsym
cat CLAUDE.md                    # Read this document
cat docs/architecture.md         # Read architecture
cargo check                      # Ensure compilation passes
git status                       # Check current changes
```

### 2. Next Priority Tasks

#### 🟡 P1 - Metrics & Monitoring

**Location**: `src/metrics/mod.rs` (new file)

**Tasks**:
- Prometheus metrics integration
  - query_duration_seconds (histogram)
  - upsert_duration_seconds (histogram)
  - cache_hit_rate (gauge)
  - segment_count (gauge)
  - wal_size_bytes (gauge)
- `/metrics` endpoint

#### 🟡 P1 - Benchmarks

**Location**: `benches/` (new directory)

**Tasks**:
- Criterion.rs benchmarks
- Vector search performance
- Full-text search performance
- Hybrid search performance
- Write throughput

### 3. Development Workflow

```bash
# 1. Start new feature
cargo check                      # Ensure builds
cargo test                       # Ensure tests pass

# 2. Implement feature
# ... write code ...

# 3. Test
cargo test --lib <module>        # Unit tests
cargo test --test <integration>  # Integration tests

# 4. Update documentation
# Update CLAUDE.md changelog
# Update README.md roadmap

# 5. Commit
git add -A
git commit -m "..."
git push
```

### 4. Common Commands

```bash
# Compilation check
cargo check

# Run tests
cargo test

# Run server
ELACSYM_STORAGE_PATH=./data cargo run

# Format code
cargo fmt

# Lint
cargo clippy

# View dependencies
cargo tree

# Update dependencies
cargo update
```

---

## 🐛 Known Issues and TODOs

### Current Issues
None! All P0 features complete.

### Technical Debt
- [ ] Add more integration tests (basic coverage exists)
- [ ] Add tracing spans for better debugging
- [ ] Performance profiling
- [ ] API documentation (OpenAPI/Swagger)
- [ ] Client SDKs (Python, JavaScript, Go)

---

## 📚 Reference Resources

### Documentation
- [Turbopuffer Architecture](https://turbopuffer.com/docs/architecture)
- [RaBitQ Paper](https://arxiv.org/abs/2405.12497)
- [RRF Paper](https://dl.acm.org/doi/10.1145/1571941.1572114)
- [Arrow Rust Docs](https://docs.rs/arrow/latest/arrow/)
- [Parquet Rust Docs](https://docs.rs/parquet/latest/parquet/)
- [Tantivy Book](https://docs.rs/tantivy/latest/tantivy/)

### Crates.io
- rabitq: https://docs.rs/rabitq/latest/rabitq/
- foyer: https://docs.rs/foyer/latest/foyer/
- axum: https://docs.rs/axum/latest/axum/
- aws-sdk-s3: https://docs.rs/aws-sdk-s3/latest/aws_sdk_s3/
- tantivy: https://docs.rs/tantivy/latest/tantivy/
- rmp-serde: https://docs.rs/rmp-serde/latest/rmp_serde/

---

## 🔄 Recent Sessions

### Session 8 (2025-10-06) - Multi-Node Testing Complete
- ✅ Fixed all test compilation errors (node_id parameter)
- ✅ Created multi-node test infrastructure
- ✅ 6 comprehensive multi-node integration tests
- ✅ All 60/60 tests passing

### Session 7 (2025-10-05) - Background Compaction Complete
- ✅ Implemented Background Compaction Manager (P1-2)
- ✅ CompactionConfig with configurable thresholds
- ✅ CompactionManager with automatic triggering
- ✅ Integration tests (60/60 passing)

### Session 6 (2025-10-05) - Advanced Features Complete
- ✅ Multi-field full-text search with per-field weights
- ✅ RRF fusion algorithm
- ✅ Advanced full-text configuration (language, stemming, stopwords)
- ✅ Write-Ahead Log (MessagePack + CRC32)
- ✅ WAL integration to upsert flow

### Session 5 (2025-10-05) - Cache Integration & Query Pipeline
- ✅ Segment document retrieval
- ✅ Foyer cache integration (Memory + Disk)
- ✅ Attribute filtering executor
- ✅ Complete query flow: index search → read segments → return documents

---

## 💡 Tips for Future Claude

1. **Read docs/architecture.md first**: Complete system design reference
2. **Check test status**: `cargo test` before making changes
3. **Maintain test coverage**: Every new feature needs tests
4. **Update documentation**: Create session summaries for major features
5. **Performance awareness**: This is a performance-sensitive project

### Debugging Tips
```bash
# Enable verbose logging
RUST_LOG=elacsym=debug,tower_http=debug cargo run

# View S3 requests
RUST_LOG=aws_sdk_s3=debug cargo run

# Performance profiling
cargo build --release
perf record ./target/release/elacsym
```

### Common Pitfalls
- ❌ Forgetting `.await` in async functions
- ❌ Using `unwrap()` instead of `?`
- ❌ Forgetting `Send + Sync` in traits
- ❌ WAL and upsert recursive calls (separate upsert_internal)

---

**Happy coding! Phase 3 P1 next! 🚀**
