# Elacsym

> An open-source vector database built on object storage - MyScale spelled backwards

[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

## Overview

Elacsym is a cost-effective, scalable vector database inspired by [turbopuffer](https://turbopuffer.com), designed to leverage object storage (S3) for storing vector data while maintaining high query performance through intelligent caching.

### Key Features

- 🚀 **High Performance**: RaBitQ quantization for fast vector search
- 💰 **Cost Effective**: Object storage backend (up to 100x cheaper than in-memory)
- 🔄 **Hybrid Cache**: Memory + Disk caching with [foyer](https://github.com/foyer-rs/foyer)
- 🔍 **Full-Text Search**: BM25-based full-text search with [Tantivy](https://github.com/quickwit-oss/tantivy)
- 🎯 **Hybrid Search**: RRF fusion for vector + full-text search
- 🛡️ **Durability**: Write-Ahead Log (WAL) for crash safety
- 📦 **Columnar Storage**: Efficient Parquet format for segments
- ⚡ **Multi-Field Search**: Search across multiple text fields with weights

## Architecture

```
┌─────────────────────────────────────────────────┐
│              HTTP API (Axum)                    │
├─────────────────────────────────────────────────┤
│  Query Engine  │  Write Coordinator             │
│  ├─ RRF Fusion │  └─ WAL                        │
├─────────────────────────────────────────────────┤
│  RaBitQ Index  │  Tantivy Full-Text             │
│  └─ Vector ANN │  └─ BM25 + Multi-Field         │
├─────────────────────────────────────────────────┤
│  Foyer Cache (Memory + Disk)                    │
├─────────────────────────────────────────────────┤
│  Storage Layer (S3 / Local FS)                  │
└─────────────────────────────────────────────────┘
```

See [docs/DESIGN.md](docs/DESIGN.md) for detailed architecture.

## Quick Start

### Installation

```bash
git clone https://github.com/lqhl/elacsym.git
cd elacsym
cargo build --release
```

### Run Server

```bash
# Using local storage (development)
ELACSYM_STORAGE_PATH=./data cargo run --release

# Or without environment variable (uses ./data by default)
cargo run --release

# Server will start on http://0.0.0.0:3000
```

### API Examples

#### Create Namespace

```bash
curl -X PUT http://localhost:3000/v1/namespaces/docs \
  -H "Content-Type: application/json" \
  -d '{
    "schema": {
      "vector_dim": 128,
      "vector_metric": "l2",
      "attributes": {
        "title": {
          "type": "string",
          "full_text": {
            "language": "english",
            "stemming": true,
            "remove_stopwords": true
          }
        },
        "description": {
          "type": "string",
          "full_text": true
        },
        "category": {
          "type": "string",
          "indexed": true
        }
      }
    }
  }'
```

#### Insert Documents

```bash
curl -X POST http://localhost:3000/v1/namespaces/docs/upsert \
  -H "Content-Type: application/json" \
  -d '{
    "documents": [
      {
        "id": 1,
        "vector": [0.1, 0.2, ...],
        "attributes": {
          "title": "Rust Vector Database",
          "description": "Fast and efficient vector search",
          "category": "tech",
          "score": 4.5
        }
      }
    ]
  }'
```

#### Vector Search

```bash
curl -X POST http://localhost:3000/v1/namespaces/docs/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.1, 0.2, ...],
    "top_k": 10,
    "filter": {
      "type": "and",
      "conditions": [
        {"field": "category", "op": "eq", "value": "tech"},
        {"field": "score", "op": "gte", "value": 4.0}
      ]
    }
  }'
```

#### Multi-Field Full-Text Search

```bash
curl -X POST http://localhost:3000/v1/namespaces/docs/query \
  -H "Content-Type: application/json" \
  -d '{
    "full_text": {
      "fields": ["title", "description"],
      "query": "rust database",
      "weights": {
        "title": 2.0,
        "description": 1.0
      }
    },
    "top_k": 10
  }'
```

#### Hybrid Search (Vector + Full-Text with RRF)

```bash
curl -X POST http://localhost:3000/v1/namespaces/docs/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.1, 0.2, ...],
    "full_text": {
      "field": "title",
      "query": "rust database"
    },
    "top_k": 10,
    "filter": {
      "type": "and",
      "conditions": [
        {"field": "category", "op": "eq", "value": "tech"}
      ]
    }
  }'
```

## Configuration

Edit `config.toml`:

```toml
[server]
host = "0.0.0.0"
port = 3000

[storage]
backend = "s3"  # or "local"

[storage.s3]
bucket = "elacsym-data"
region = "us-west-2"
# endpoint = "http://localhost:9000"  # For MinIO

[cache]
memory_size = 4294967296  # 4GB
disk_size = 107374182400  # 100GB
```

## Development Roadmap

### ✅ Phase 1: MVP (100% Complete)
- [x] Project structure and dependencies
- [x] Storage abstraction (S3 + Local FS)
- [x] Core type system (types.rs, error.rs)
- [x] Manifest persistence (with tests)
- [x] Segment Parquet read/write (with tests)
- [x] RaBitQ vector index integration (with tests)
- [x] Namespace manager (with tests)
- [x] HTTP API endpoints (Upsert + Query)
- [x] Design documentation

### ✅ Phase 2: Advanced Features (100% Complete)
- [x] **Segment document retrieval**
- [x] **Foyer cache integration (Memory + Disk)**
- [x] **Attribute filtering** (FilterExecutor with all operators)
- [x] **Tantivy full-text search** (BM25 with multi-field support)
- [x] **RRF fusion** for hybrid search
- [x] **Advanced full-text config** (language, stemming, stopwords)
- [x] **Write-Ahead Log (WAL)** for durability

**Status**: All Phase 2 features implemented and tested!
- 17 unit tests passing
- Complete query pipeline: filter → vector search → full-text → RRF fusion
- WAL ensures crash-safe writes
- Multi-field full-text with per-field weights

### ✅ Phase 3: Production Readiness (P0 100% Complete!)

#### P0 - Critical for Production ✅
- [x] **WAL Recovery** - Replay uncommitted operations on startup ✅
- [x] **WAL Rotation** - Prevent unbounded WAL growth ✅
- [x] **Error Recovery** - Graceful handling of corruption ✅
- [x] **Integration Tests** - End-to-end testing (47/47 tests passing) ✅
- [x] **Tantivy Analyzer Config** - Apply advanced full-text settings ✅

#### P1 - Performance & Reliability
- [ ] **LSM-tree Compaction** - Merge small segments
- [ ] **Index Rebuild** - Rebuild vector index after compaction
- [ ] **Metrics & Monitoring** - Prometheus metrics
- [ ] **Benchmarks** - Performance testing suite
- [ ] **Query Optimizer** - Cost-based query planning

#### P2 - Advanced Features
- [ ] **Distributed Mode** - Multi-node deployment
- [ ] **Replication** - Data redundancy
- [ ] **Snapshot & Restore** - Backup/recovery
- [ ] **Query Caching** - Cache query results
- [ ] **Bulk Import** - Fast batch loading

### 📚 Phase 4: Ecosystem
- [ ] Client SDKs (Python, JavaScript, Go)
- [ ] Kubernetes Operator
- [ ] Cloud-native deployment guides
- [ ] Performance tuning guide

## Performance Goals

| Scenario | Data Size | Target Latency |
|----------|-----------|----------------|
| Hot query | 1M vectors | < 20ms |
| Cold query | 1M vectors | < 500ms |
| Write throughput | - | > 1000 docs/s |
| Hybrid search | 1M vectors | < 100ms |

## Tech Stack

- **Language**: Rust 2021
- **HTTP**: Axum
- **Storage**: aws-sdk-s3
- **Vector Index**: [rabitq-rs](https://github.com/lqhl/rabitq-rs)
- **Cache**: [foyer](https://github.com/foyer-rs/foyer)
- **Full-Text**: [Tantivy](https://github.com/quickwit-oss/tantivy)
- **Columnar**: Arrow + Parquet
- **WAL**: MessagePack + CRC32

## Recent Updates

### Session 8 (2025-10-05) - Tantivy Custom Analyzers 🔍
- ✅ **Custom Analyzer API** - `FullTextIndex::new_with_config()` accepting `FullTextConfig`
- ✅ **18 Language Support** - Arabic, Danish, Dutch, English, Finnish, French, German, Greek, Hungarian, Italian, Norwegian, Portuguese, Romanian, Russian, Spanish, Swedish, Tamil, Turkish
- ✅ **Configurable Filters** - Case-sensitive, stemming, stopword removal, token length limits
- ✅ **Analyzer Tests** - 6 integration tests covering stemming, stopwords, case sensitivity, multi-language
- ✅ **All Tests Passing** - 47/47 tests (35 unit + 6 analyzer + 6 WAL)

**P0 100% Complete!** All critical production-readiness tasks finished. Database now has advanced full-text search with multilingual support and configurable text analysis.

**Analyzer Features**:
- Conditional filter chains (8 combinations)
- Language-specific stemming and stopwords
- Case-sensitive/insensitive search
- Token length filtering (max 40 chars)

### Session 7 (2025-10-05) - WAL Recovery & Error Handling 🛡️
- ✅ **WAL Recovery** - Replay uncommitted operations on startup
- ✅ **WAL Rotation** - Auto-rotate at 100MB, keep last 5 files
- ✅ **Error Recovery** - Graceful corruption handling (CRC mismatch, truncation)
- ✅ **Health Check** - GET /health endpoint with system status
- ✅ **Integration Tests** - 41/41 tests passing (35 unit + 6 integration)
- ✅ **Rust Installation** - Setup complete, all dependencies resolved

**P0-4 Complete!** Database is now production-ready with comprehensive test coverage.

**Test Summary**:
- ✅ 35 library tests (all modules)
- ✅ 4 error recovery tests (corruption, truncation, unreasonable size)
- ✅ 2 WAL recovery tests (crash recovery, truncate after commit)

See [docs/ERROR_RECOVERY.md](docs/ERROR_RECOVERY.md) for details.

### Session 6 (2025-10-05) - Advanced Features Complete! 🎉
- ✅ Multi-field full-text search with per-field weights
- ✅ RRF (Reciprocal Rank Fusion) for hybrid search
- ✅ Advanced full-text schema configuration
- ✅ Write-Ahead Log (WAL) for crash-safe durability
- ✅ Attribute filtering (Eq, Ne, Gt, Gte, Lt, Lte, Contains, ContainsAny)
- ✅ Complete Foyer cache integration

See [docs/SESSION_6_SUMMARY.md](docs/SESSION_6_SUMMARY.md) for details.

## Documentation

- [Design Document](docs/DESIGN.md) - Architecture and design decisions
- [Session Summaries](docs/) - Development progress
  - [Session 5](docs/SESSION_5_SUMMARY.md) - Cache integration & query pipeline
  - [Session 6](docs/SESSION_6_SUMMARY.md) - Advanced features (RRF, WAL, multi-field)
- [Turbopuffer Comparison](docs/FULLTEXT_COMPARISON.md) - Full-text search design

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

Apache-2.0

## Acknowledgments

- Inspired by [turbopuffer](https://turbopuffer.com)
- Built on [rabitq-rs](https://github.com/lqhl/rabitq-rs) for vector quantization
- Uses [foyer](https://github.com/foyer-rs/foyer) for hybrid caching
- Powered by [Tantivy](https://github.com/quickwit-oss/tantivy) for full-text search
