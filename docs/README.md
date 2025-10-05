# Elacsym Documentation

> An open-source vector database built on object storage

## Overview

Elacsym (MyScale spelled backwards) is a vector database designed for:
- **Cost-effective storage**: Leverage S3/object storage for cold data
- **High performance**: Hybrid cache (memory + disk) for hot queries
- **Scalability**: Serverless-friendly architecture
- **Full-text search**: Integrated Tantivy for text search

## Architecture

See [DESIGN.md](./DESIGN.md) for detailed design documentation.

## Getting Started

### Prerequisites

- Rust 1.75+
- S3-compatible storage (AWS S3, MinIO, etc.) or local filesystem

### Build

```bash
cargo build --release
```

### Run

```bash
# Using local storage
RUST_LOG=info ./target/release/elacsym

# Using S3 storage
# Configure config.toml first
./target/release/elacsym
```

### Example Usage

```bash
# Create namespace
curl -X PUT http://localhost:3000/v1/namespaces/my_namespace \
  -H "Content-Type: application/json" \
  -d '{
    "schema": {
      "vector_dim": 768,
      "vector_metric": "cosine",
      "attributes": {
        "title": {"type": "string", "full_text": true},
        "category": {"type": "string", "indexed": true}
      }
    }
  }'

# Upsert documents
curl -X POST http://localhost:3000/v1/namespaces/my_namespace/upsert \
  -H "Content-Type: application/json" \
  -d '{
    "documents": [
      {
        "id": 1,
        "vector": [0.1, 0.2, ...],
        "attributes": {
          "title": "Rust Vector Database",
          "category": "tech"
        }
      }
    ]
  }'

# Query
curl -X POST http://localhost:3000/v1/namespaces/my_namespace/query \
  -H "Content-Type: application/json" \
  -d '{
    "vector": [0.1, 0.2, ...],
    "top_k": 10,
    "filter": {
      "type": "and",
      "conditions": [
        {"field": "category", "op": "eq", "value": "tech"}
      ]
    }
  }'
```

## Features

- [x] S3 + Local storage backend
- [x] Parquet-based segment storage
- [ ] RaBitQ vector index integration
- [ ] Foyer hybrid cache
- [ ] Tantivy full-text search
- [ ] Tombstone-based deletion
- [ ] LSM-tree style compaction
- [ ] Hybrid search (vector + full-text)
- [ ] Distributed deployment

## License

Apache-2.0
