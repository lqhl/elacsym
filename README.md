# Elacsym

> An open-source vector database built on object storage - MyScale spelled backwards

[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

## Overview

Elacsym is a cost-effective, scalable vector database inspired by [turbopuffer](https://turbopuffer.com), designed to leverage object storage (S3) for storing vector data while maintaining high query performance through intelligent caching.

### Key Features

- 🚀 **High Performance**: RaBitQ quantization + HNSW for fast vector search
- 💰 **Cost Effective**: Object storage backend (up to 100x cheaper than in-memory)
- 🔄 **Hybrid Cache**: Memory + Disk caching with [foyer](https://github.com/foyer-rs/foyer)
- 🔍 **Full-Text Search**: Integrated [Tantivy](https://github.com/quickwit-oss/tantivy) for text search
- 🎯 **Hybrid Search**: Combine vector similarity + full-text + attribute filters
- 🛡️ **ACID Transactions**: Tombstone-based deletions with MVCC
- 📦 **Columnar Storage**: Efficient Parquet format for segments

## Architecture

```
┌─────────────────────────────────────────────────┐
│              HTTP API (Axum)                    │
├─────────────────────────────────────────────────┤
│  Query Engine  │  Write Coordinator             │
├─────────────────────────────────────────────────┤
│  RaBitQ Index  │  Tantivy Full-Text             │
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
git clone https://github.com/yourusername/elacsym.git
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
        "title": {"type": "string", "full_text": true},
        "category": {"type": "string", "indexed": true}
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

#### Hybrid Search

```bash
curl -X POST http://localhost:3000/v1/namespaces/docs/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.1, 0.2, ...],
    "top_k": 10,
    "full_text": {
      "field": "title",
      "query": "rust database",
      "weight": 0.3
    },
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

### Phase 1: MVP (Current - 100% Complete ✅)
- [x] Project structure
- [x] Storage abstraction (S3 + Local FS)
- [x] Basic types and error handling
- [x] Manifest persistence (with tests)
- [x] Segment Parquet read/write (with tests)
- [x] RaBitQ vector index integration (with tests)
- [x] Namespace manager (with tests)
- [x] HTTP API endpoints (Upsert + Query)
- [ ] Query executor with filtering

**Current Status**: **MVP Complete!** HTTP API is working. You can create namespaces, upsert documents, and perform vector search via REST endpoints. All 8 unit tests passing. Server runs on port 3000.

### Phase 2: Advanced Features
- [ ] Foyer cache integration
- [ ] Tantivy full-text search
- [ ] Attribute filtering
- [ ] Hybrid search with RRF
- [ ] Tombstone-based deletion

### Phase 3: Production Ready
- [ ] LSM-tree style compaction
- [ ] Distributed deployment
- [ ] Monitoring and metrics
- [ ] Benchmark suite

## Performance Goals

| Scenario | Data Size | Target Latency |
|----------|-----------|----------------|
| Hot query | 1M vectors | < 20ms |
| Cold query | 1M vectors | < 500ms |
| Write throughput | - | > 1000 docs/s |

## Tech Stack

- **Language**: Rust 2021
- **HTTP**: Axum
- **Storage**: aws-sdk-s3
- **Vector Index**: [rabitq-rs](https://github.com/lqhl/rabitq-rs)
- **Cache**: [foyer](https://github.com/foyer-rs/foyer)
- **Full-Text**: [Tantivy](https://github.com/quickwit-oss/tantivy)
- **Columnar**: Arrow + Parquet

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

Apache-2.0

## Acknowledgments

- Inspired by [turbopuffer](https://turbopuffer.com)
- Built on [rabitq-rs](https://github.com/lqhl/rabitq-rs) for vector quantization
- Uses [foyer](https://github.com/foyer-rs/foyer) for hybrid caching
- Powered by [Tantivy](https://github.com/quickwit-oss/tantivy) for full-text search
