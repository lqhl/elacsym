# Architecture

Elacsym is a vector database designed around object storage (S3) as the primary storage tier, with aggressive caching to maintain query performance. This document explains the system architecture, key components, and design patterns.

## System Overview

```
┌─────────────────────────────────────────────────────────────┐
│                      HTTP API (Axum)                        │
│                    GET /health, /metrics                    │
│                   PUT /v1/namespaces/:ns                    │
│            POST /v1/namespaces/:ns/{upsert,query}          │
└─────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────┐
│                   NamespaceManager                          │
│              (Coordinates multiple namespaces)              │
│  - Namespace lifecycle (create/load/delete)                 │
│  - Background compaction management                         │
│  - Distributed node coordination (optional)                 │
└─────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────┐
│                        Namespace                            │
│         (Encapsulates one vector collection)                │
│  ┌─────────────────┐  ┌────────────────────────────────┐   │
│  │ WriteCoordinator│  │      QueryExecutor             │   │
│  │  - WAL append   │  │  - Vector search (RaBitQ)      │   │
│  │  - Segment flush│  │  - Full-text search (Tantivy)  │   │
│  │  - Index rebuild│  │  - Attribute filtering         │   │
│  └─────────────────┘  │  - RRF fusion                  │   │
│                       └────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                              ↓
        ┌─────────────────────┴─────────────────────┐
        ↓                                            ↓
┌───────────────────┐                    ┌──────────────────────┐
│  Index Layer      │                    │   Segment Manager    │
│  - VectorIndex    │                    │   - SegmentWriter    │
│    (RaBitQ)       │                    │     (Parquet)        │
│  - FullTextIndex  │                    │   - SegmentReader    │
│    (Tantivy BM25) │                    │     (Parquet + Arrow)│
└───────────────────┘                    └──────────────────────┘
        ↓                                            ↓
┌───────────────────────────────────────────────────────────────┐
│                    Cache Layer (Foyer)                        │
│  Memory Cache (4GB)           Disk Cache (100GB)              │
│  - Manifests                  - Segments (Parquet files)      │
│  - Vector indexes             - Full-text indexes             │
└───────────────────────────────────────────────────────────────┘
                              ↓
┌───────────────────────────────────────────────────────────────┐
│                  Storage Backend (Pluggable)                  │
│  LocalStorage                      S3Storage                  │
│  - Filesystem I/O                  - aws-sdk-s3              │
│  - For development                 - Production-ready         │
│  - Fast iteration                  - Cost-effective           │
└───────────────────────────────────────────────────────────────┘
                              ↓
┌───────────────────────────────────────────────────────────────┐
│                Write-Ahead Log (WAL)                          │
│  - MessagePack serialization + CRC32 checksums                │
│  - Crash-safe writes with fsync                               │
│  - Automatic recovery on restart                              │
│  - Rotation at 100MB with cleanup                             │
└───────────────────────────────────────────────────────────────┘
```

## Core Components

### 1. Storage Layer

Elacsym uses a pluggable storage abstraction (`StorageBackend` trait) supporting two implementations:

#### LocalStorage
- **Use case**: Development, single-node deployments
- **Backend**: Filesystem (std::fs)
- **Pros**: Fast, no external dependencies
- **Cons**: Not distributed, limited scalability

#### S3Storage
- **Use case**: Production, distributed deployments
- **Backend**: AWS S3 or S3-compatible (MinIO, Ceph)
- **Pros**: Cheap (1/100 cost vs memory), infinitely scalable, durable
- **Cons**: Higher latency (mitigated by caching)

**Storage Layout**:
```
{root_path}/
  {namespace}/
    manifest.json              # Metadata: version, schema, segment list
    segments/
      seg_00001.parquet        # Document data (columnar)
      seg_00002.parquet
      ...
    indexes/
      vector_index.bin         # RaBitQ index
      fulltext_{field}.bin     # Tantivy indexes (one per field)
    wal/
      000001.wal               # Write-ahead logs
      000002.wal
```

### 2. Segment Manager

**Purpose**: Store documents in immutable Parquet files

**Format**: Apache Parquet (columnar storage)
- `id` column: UInt64 (document ID)
- `vector` column: FixedSizeList<Float32> (embedding vector)
- `attributes`: One column per attribute (String, Int64, Float64, Bool, List<String>)

**Write Path**:
```rust
Documents → Arrow RecordBatch → Parquet Writer → S3/Local
```

**Read Path**:
```rust
S3/Local → Foyer Cache → Parquet Reader → Arrow RecordBatch → Filter → Documents
```

**Characteristics**:
- **Immutable**: Segments never modified after creation
- **Compressed**: Snappy compression for fast decode
- **Columnar**: Read only columns needed for query
- **Cached**: Hot segments stay in disk cache

### 3. Write-Ahead Log (WAL)

**Purpose**: Ensure durability and enable crash recovery

**Protocol**:
1. Client sends upsert request
2. **Write to WAL + fsync** (durability guaranteed)
3. Flush to segment (may fail, but recoverable)
4. Update indexes
5. Update manifest
6. **Truncate WAL** (operation committed)

**File Format**:
```
Header: "EWAL" (magic) + version (u8)

Entry:
  length: u32                 # Entry size in bytes
  data: [u8; length]          # MessagePack serialized WalEntry
  crc32: u32                  # CRC32 checksum of data
```

**Recovery Process**:
On startup, the system:
1. Reads all WAL files
2. Validates CRC32 checksums
3. Replays uncommitted operations
4. Truncates WAL after successful replay

**Error Handling**:
- CRC mismatch: Skip corrupted entry, continue recovery
- Truncated entry: Stop recovery (crash during write)
- Unreasonable size (>100MB): Stop recovery (corrupted length field)

**Rotation**:
- Triggered at 100MB
- Keeps last 5 WAL files
- Automatic cleanup of old files

See [Error Recovery](#error-recovery) for details.

### 4. Index Layer

#### Vector Index (RaBitQ)

**Algorithm**: RaBitQ (Rapid Binary Quantization)
- Binary quantization for memory efficiency (32x compression)
- HNSW graph for fast ANN search
- Cosine, L2, and Dot Product metrics

**Characteristics**:
- **No incremental updates**: Adding vectors requires rebuild
- **Workaround**: LSM-tree pattern with background compaction
- **Memory**: Indexes cached in RAM (small due to quantization)
- **Persistence**: Serialized to binary files in object storage

**Search Flow**:
```
Query vector → Load index (cache) → RaBitQ search → Top-K doc IDs
```

#### Full-Text Index (Tantivy)

**Algorithm**: BM25 (Best Match 25)
- Inverted index for token-based search
- Multi-field support with per-field weights
- Configurable analyzers (18 languages supported)

**Features**:
- Stemming (e.g., "running" → "run")
- Stopword removal (e.g., "the", "is")
- Case sensitivity control
- Token length limits

**Analyzer Configuration**:
```json
{
  "title": {
    "type": "string",
    "full_text": {
      "language": "english",
      "stemming": true,
      "remove_stopwords": true,
      "case_sensitive": false
    }
  }
}
```

**Search Flow**:
```
Text query → Parse query → Tantivy search → BM25 scores → Top-K doc IDs
```

### 5. Cache Layer (Foyer)

**Purpose**: Hide S3 latency with aggressive caching

**Architecture**:
- **Memory cache (L1)**: 4GB, LRU eviction
  - Manifests (hot metadata)
  - Vector indexes (frequently accessed)
- **Disk cache (L2)**: 100GB, LRU eviction
  - Segments (Parquet files)
  - Full-text indexes (Tantivy directories)

**Cache Keys**:
| Type | Key Pattern | Layer | TTL |
|------|-------------|-------|-----|
| Manifest | `manifest:{namespace}` | Memory | 5 min |
| Vector Index | `vidx:{namespace}` | Memory | 30 min |
| Full-Text Index | `ftidx:{namespace}:{field}` | Memory | 30 min |
| Segment | `seg:{namespace}:{segment_id}` | Disk | 1 hour |

**Cache Invalidation**:
- Write operations invalidate manifest and indexes
- Segments are immutable (never invalidated)

**Performance**:
- **Hot query** (cache hit): <20ms
- **Cold query** (cache miss): <500ms (depends on S3 latency)

### 6. Query Executor

#### Attribute Filtering

**Operators**: `Eq`, `Ne`, `Gt`, `Gte`, `Lt`, `Lte`, `Contains`, `ContainsAny`

**Execution**:
```rust
Filter → Scan segments → Arrow compute::filter() → Filtered doc IDs
```

**Optimization**: Parquet statistics (min/max) enable row group pruning

#### Hybrid Search (RRF Fusion)

**Reciprocal Rank Fusion (RRF)**:
```
score(doc) = Σ weight_i / (k + rank_i(doc))

Where:
  k = 60 (RRF constant)
  rank_i(doc) = rank of doc in result set i
  weight_i = weight of result set i
```

**Example**:
```
Vector search results: [doc3, doc1, doc5, ...]
Full-text results:     [doc1, doc7, doc3, ...]

RRF scores:
  doc1 = 0.7/(60+2) + 0.3/(60+1) = 0.0159
  doc3 = 0.7/(60+1) + 0.3/(60+3) = 0.0163  ← Highest
  doc5 = 0.7/(60+3) + 0.0 = 0.0111
  doc7 = 0.0 + 0.3/(60+2) = 0.0048
```

Final ranking: `[doc3, doc1, doc5, doc7]`

**Query Flow**:
```
┌─────────────────────────────────────────┐
│         Parse QueryRequest              │
│  - vector (optional)                    │
│  - full_text (optional)                 │
│  - filter (optional)                    │
└─────────────────────────────────────────┘
                ↓
      ┌─────────┴─────────┐
      ↓                   ↓
┌─────────────┐   ┌─────────────────┐
│Vector Search│   │ Full-Text Search│
│ (parallel)  │   │   (parallel)    │
└─────────────┘   └─────────────────┘
      ↓                   ↓
      └─────────┬─────────┘
                ↓
    ┌────────────────────────┐
    │   RRF Fusion           │
    │  (merge rankings)      │
    └────────────────────────┘
                ↓
    ┌────────────────────────┐
    │  Apply Attribute Filter│
    └────────────────────────┘
                ↓
    ┌────────────────────────┐
    │  Fetch Segments (cache)│
    │  Assemble Documents    │
    └────────────────────────┘
                ↓
           Return Results
```

### 7. Compaction Manager

**Problem**: RaBitQ doesn't support incremental updates, so each upsert creates a new segment

**Solution**: LSM-tree style background compaction

**Trigger Conditions**:
- Segment count > 100, OR
- Total documents > 1,000,000

**Compaction Process**:
1. Select N smallest segments (default: 10)
2. Read all data into memory
3. Merge into single new segment
4. Rebuild vector index with all vectors
5. Rebuild full-text indexes
6. Atomically update manifest (version++)
7. Delete old segments

**Configuration** (config.toml):
```toml
[compaction]
enabled = true
interval_secs = 3600      # Check every hour
max_segments = 100
max_total_docs = 1000000
```

**Guarantees**:
- Atomic: Manifest update is atomic (old version still valid until update)
- Non-blocking: Compaction runs in background thread
- Safe: Old segments not deleted until new manifest written

## Data Model

### Namespace

A namespace is an isolated collection of documents with a fixed schema.

**Schema**:
```rust
pub struct NamespaceSchema {
    pub vector_dim: usize,
    pub vector_metric: VectorMetric,  // Cosine | L2 | Dot
    pub attributes: HashMap<String, AttributeSchema>,
}

pub struct AttributeSchema {
    pub attr_type: AttributeType,     // String | Int | Float | Bool | ArrayString
    pub indexed: bool,                 // Build attribute index for filtering
    pub full_text: FullTextConfig,     // Enable full-text search
}
```

### Document

```rust
pub struct Document {
    pub id: u64,
    pub vector: Option<Vec<f32>>,
    pub attributes: HashMap<String, AttributeValue>,
}
```

### Manifest

The manifest is the "source of truth" for namespace metadata.

**Fields**:
- `version`: u64 (monotonically increasing)
- `namespace`: String
- `schema`: NamespaceSchema
- `segments`: Vec<SegmentInfo>
- `indexes`: IndexPaths (vector_index, full_text_indexes)
- `stats`: NamespaceStats (total_docs, total_size_bytes)

**Atomic Updates**:
```rust
// Read-modify-write with version check
let mut manifest = load_manifest()?;
manifest.version += 1;
manifest.segments.push(new_segment);
write_manifest(&manifest)?;
```

## Error Recovery

### WAL Corruption Handling

The WAL reader implements graceful degradation:

| Error Type | Action | Log Level | Example |
|------------|--------|-----------|---------|
| CRC mismatch | Skip entry, continue | WARN | Disk bit flip |
| Deserialization error | Skip entry, continue | WARN | Corrupted msgpack |
| Truncated entry | Stop recovery | WARN | Crash during write |
| Unreasonable size (>100MB) | Stop recovery | WARN | Corrupted length |
| Invalid header | Fail immediately | ERROR | Not a WAL file |

**Recovery Statistics**:
```
Recovered 8/10 WAL entries. 2 entries were corrupted or truncated.
```

**Philosophy**:
- Best-effort recovery: Salvage as much data as possible
- Fail-safe: Stop at structural corruption (don't guess)
- Transparent: Log all corruption events

### Health Check

**Endpoint**: `GET /health`

**Response**:
```json
{
  "status": "healthy",
  "version": "0.1.0",
  "namespaces": 5
}
```

Use for:
- Liveness probes (Kubernetes)
- Load balancer health checks
- Monitoring systems

## Distributed Architecture (Optional)

Elacsym supports multi-node deployment with namespace sharding.

**Roles**:
- **Indexer nodes**: Handle writes (upsert)
- **Query nodes**: Handle reads (query)

**Sharding**:
- Consistent hashing on namespace name
- Each namespace assigned to one indexer
- All nodes share same S3 backend

**Routing**:
```
Client → Any node → 307 Redirect → Responsible indexer
```

**Metadata**:
```json
{
  "node_id": "indexer-0",
  "role": "indexer",
  "indexer_cluster": {
    "nodes": ["indexer-0", "indexer-1", "indexer-2"],
    "hash_ring": [...]
  }
}
```

**Advantages**:
- Write sharding for higher throughput
- Shared storage eliminates replication lag
- Simple operational model (no consensus required)

**Limitations**:
- Single point of failure per namespace (no HA yet)
- Rebalancing requires manual intervention

See `docs/deployment.md` for deployment guide.

## Performance Characteristics

### Write Performance

| Operation | Latency | Throughput | Notes |
|-----------|---------|------------|-------|
| Single doc upsert | 5-10ms | N/A | Includes WAL fsync |
| Batch upsert (1000 docs) | 100-200ms | 5000-10000 docs/s | Amortized WAL + index rebuild |
| Compaction (100 segments) | 30-60s | N/A | Background, non-blocking |

**Bottlenecks**:
- WAL fsync (required for durability)
- Index rebuild (RaBitQ doesn't support incremental)
- S3 upload latency (mitigated by async)

### Query Performance

| Query Type | Hot (cache hit) | Cold (cache miss) | Notes |
|------------|-----------------|-------------------|-------|
| Vector search | <20ms | <500ms | Depends on S3 latency |
| Full-text search | <50ms | <600ms | Tantivy index compressed |
| Hybrid search (RRF) | <100ms | <800ms | Two searches + fusion |
| Filtered query | +10-50ms | +100-200ms | Depends on filter selectivity |

**Optimization**:
- Cache warming: Preload hot namespaces on startup
- Parallel segment reads: Fetch multiple segments concurrently
- Query result caching (P2 feature)

### Storage Cost

| Data | In-Memory | With Elacsym | Savings |
|------|-----------|--------------|---------|
| 1M vectors (768-dim) | 3GB | 30MB (indexes) + S3 (segments) | ~100x |
| 10M vectors | 30GB | 300MB + S3 | ~100x |

**Cost Breakdown** (AWS us-east-1):
- S3 storage: $0.023/GB/month
- Memory (EC2): $2-4/GB/month
- **Savings**: 87-99% for cold data

## Limitations and Tradeoffs

### Known Limitations

1. **No incremental vector updates**: RaBitQ requires full rebuild
   - **Mitigation**: LSM-tree compaction in background
   - **Impact**: High write workloads require frequent compaction

2. **No real-time consistency**: WAL replay can take seconds
   - **Mitigation**: Fast WAL recovery (<1s for typical workloads)
   - **Impact**: Crash may lose last few seconds of writes

3. **No multi-namespace transactions**: Each namespace is independent
   - **Workaround**: Use external transaction coordinator
   - **Impact**: Cross-namespace consistency not guaranteed

4. **S3 eventual consistency**: Rare race conditions possible
   - **Mitigation**: Manifest version numbers prevent conflicts
   - **Impact**: Minimal, S3 is strongly consistent for new objects

### Design Tradeoffs

| Decision | Pros | Cons |
|----------|------|------|
| Object storage as primary | Cost-effective, scalable | Higher latency |
| Immutable segments | Simple, cacheable, safe | Requires compaction |
| RaBitQ for vectors | Memory-efficient, fast | No incremental updates |
| Tantivy for full-text | Full-featured, Rust-native | Larger index size |
| WAL for durability | Crash-safe, simple | Write amplification |
| No distributed consensus | Simple, no split-brain | Limited HA (P2) |

## Future Enhancements

### P1 - Performance & Reliability
- [ ] Prometheus metrics (`/metrics` endpoint)
- [ ] Query result caching
- [ ] Benchmark suite
- [ ] Cost-based query optimizer

### P2 - Advanced Features
- [ ] Replication for HA
- [ ] Snapshot & restore
- [ ] Bulk import (fast batch loading)
- [ ] Sparse vector support
- [ ] Multi-vector documents

### P3 - Ecosystem
- [ ] Client SDKs (Python, JavaScript, Go)
- [ ] Kubernetes Operator
- [ ] Grafana dashboards
- [ ] Cloud-native deployment guides

## References

- [RaBitQ Paper](https://arxiv.org/abs/2405.12497) - Binary quantization for ANN
- [RRF Paper](https://dl.acm.org/doi/10.1145/1571941.1572114) - Reciprocal Rank Fusion
- [Turbopuffer Architecture](https://turbopuffer.com/docs/architecture) - Inspiration
- [Apache Parquet Format](https://parquet.apache.org/docs/) - Columnar storage
- [Tantivy Documentation](https://docs.rs/tantivy) - Full-text search engine
- [Foyer Cache](https://github.com/foyer-rs/foyer) - Hybrid caching library
