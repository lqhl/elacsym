# Design Decisions

This document explains the key technical decisions behind Elacsym and provides rationale for the chosen approaches.

## Why "Elacsym"?

**Elacsym = MyScale spelled backwards**

This project is named in tribute to [MyScale](https://github.com/myscale/MyScaleDB), a vector database project that taught valuable lessons about building database systems. MyScale was an ambitious attempt to create a ClickHouse-based vector database, but ultimately faced challenges in balancing complexity, performance, and operational overhead.

**Lessons learned from MyScale**:
1. **Simplicity over features**: Start with a focused MVP rather than comprehensive feature set
2. **Object storage is viable**: Cost-effective storage can work if caching is done right
3. **Choose battle-tested components**: RaBitQ, Tantivy, Foyer instead of custom implementations
4. **Rust for systems programming**: Memory safety and performance without C++ complexity
5. **Open source from day one**: Community feedback early and often

Elacsym takes these lessons and builds a simpler, more focused vector database that prioritizes:
- **Cost-effectiveness** (S3 storage)
- **Operational simplicity** (stateless nodes, no consensus)
- **Production-readiness** (WAL, crash recovery, compaction)
- **Developer experience** (clear APIs, good documentation)

## Core Design Principles

### 1. Object Storage as Primary Tier

**Decision**: Use S3/object storage instead of disk or memory

**Rationale**:
- **Cost**: S3 is 100× cheaper than memory ($0.023/GB/month vs $2-4/GB/month)
- **Scalability**: Virtually unlimited capacity
- **Durability**: 99.999999999% durability (11 nines)
- **Separation of compute and storage**: Scale independently

**Trade-offs**:
- Higher latency (5-50ms vs <1ms for local disk)
- **Mitigation**: Aggressive multi-tier caching (memory + disk)

**Inspiration**: [Turbopuffer](https://turbopuffer.com/blog/turbopuffer) pioneered this approach

### 2. Immutable Segments (LSM-tree Pattern)

**Decision**: Store documents in immutable Parquet segments, never modify

**Rationale**:
- **Simplicity**: No need for locks, MVCC, or coordination
- **Cacheable**: Immutable data is easy to cache (never invalidated)
- **Safe**: No risk of corruption from concurrent writes
- **Compaction**: Merge small segments in background (like LSM-trees)

**Trade-offs**:
- Write amplification (compaction rewrites data)
- **Mitigation**: Configurable compaction thresholds, background execution

**Inspiration**: LevelDB, RocksDB, Cassandra (all use LSM-trees)

### 3. RaBitQ for Vector Index

**Decision**: Use RaBitQ (binary quantization) instead of HNSW or IVF

**Rationale**:
- **Memory efficiency**: 32× compression vs full-precision vectors
- **Fast search**: Binary operations are CPU-efficient
- **Accuracy**: <5% recall loss vs exact search for most datasets

**Trade-offs**:
- No incremental updates (rebuild required)
- **Mitigation**: LSM-tree compaction handles rebuilds

**Why not HNSW?**
- HNSW uses ~4-8 bytes per dimension (quantized) = 768 dim × 4 = 3 KB per vector
- RaBitQ uses ~0.1 KB per vector (32× smaller)
- For 1M vectors: HNSW = 3 GB, RaBitQ = 100 MB

**Why not IVF?**
- IVF requires centroids (more memory)
- Lower recall than RaBitQ at same memory budget

**Reference**: [RaBitQ Paper](https://arxiv.org/abs/2405.12497)

### 4. Tantivy for Full-Text Search

**Decision**: Use Tantivy (Rust native) instead of Elasticsearch or Lucene

**Rationale**:
- **Rust native**: No JVM, easier deployment
- **Embeddable**: Run in same process (no network hop)
- **BM25 algorithm**: Industry-standard text relevance
- **Multi-language support**: 18 languages with stemming/stopwords

**Trade-offs**:
- Less mature than Lucene/Elasticsearch
- **Acceptable**: Tantivy is production-ready (used by Quickwit)

**Why not Elasticsearch?**
- Separate service (operational complexity)
- JVM memory overhead
- Overkill for embedded use case

### 5. RRF for Hybrid Search

**Decision**: Use Reciprocal Rank Fusion to merge vector and full-text results

**Rationale**:
- **Simple**: No machine learning required
- **Effective**: Outperforms simple score averaging
- **Standard**: Used by Turbopuffer, Azure Cognitive Search

**Formula**:
```
score(doc) = Σ weight_i / (k + rank_i(doc))
```

**Why not learned fusion?**
- Requires training data
- More complex to tune
- RRF is "good enough" (80/20 rule)

**Reference**: [RRF Paper](https://dl.acm.org/doi/10.1145/1571941.1572114)

### 6. Foyer for Caching

**Decision**: Use Foyer hybrid cache (memory + disk) instead of Redis or custom cache

**Rationale**:
- **Unified API**: Single interface for both tiers
- **Automatic promotion/demotion**: Memory ← → Disk
- **Compressed disk cache**: Save space without sacrificing speed
- **LRU eviction**: Simple and effective

**Why not Redis?**
- Separate service (operational overhead)
- No disk tier (memory-only or pay for Redis on Flash)
- Network hop adds latency

**Why not custom cache?**
- Foyer is battle-tested and well-maintained
- Complex cache logic (admission policies, eviction) already solved

**Reference**: [Foyer GitHub](https://github.com/foyer-rs/foyer)

### 7. Write-Ahead Log for Durability

**Decision**: MessagePack + CRC32 for WAL format

**Rationale**:
- **Simple**: Easy to implement and debug
- **Compact**: Binary format saves space
- **Checksummed**: CRC32 detects corruption
- **Fast**: Append-only writes

**Why not Protobuf/Cap'n Proto?**
- Overkill for simple use case
- MessagePack is "good enough"

**Why not plain JSON?**
- 2-3× larger (wastes disk I/O)
- Slower to parse

**Recovery Strategy**: Best-effort recovery (skip corrupted entries, stop at truncation)

### 8. No Distributed Consensus

**Decision**: Use consistent hashing for sharding, no Raft/Paxos

**Rationale**:
- **Simplicity**: No leader election, no split-brain
- **Shared storage**: S3 is the source of truth
- **Stateless nodes**: Any node can serve any query
- **Easy scaling**: Add/remove nodes without coordination

**Trade-offs**:
- No high availability for writes (single namespace = single indexer)
- **Future work**: Replication for HA (P2)

**Why not Raft?**
- Adds complexity (leader election, log replication)
- S3 already handles durability
- Consensus overhead slows down writes

**Inspiration**: Snowflake, Databricks (shared storage architectures)

### 9. Parquet for Segment Storage

**Decision**: Use Apache Parquet instead of custom binary format

**Rationale**:
- **Columnar**: Read only columns needed (saves I/O)
- **Compressed**: Snappy compression out of the box
- **Standard**: Widely supported (Arrow, Spark, DuckDB)
- **Fast**: Optimized C++ implementation

**Why not custom format?**
- Reinventing the wheel
- Parquet is industry-standard

**Why not Avro/ORC?**
- Parquet has better Rust support (arrow-rs)
- Parquet is more common in ML/AI space

### 10. No Deletes (Tombstones Only)

**Decision**: Support logical deletes via tombstones, compaction removes

**Rationale**:
- **Immutable segments**: Can't modify Parquet files
- **Simple**: Just track deleted IDs in manifest
- **Cleaned up**: Compaction physically removes deleted docs

**Trade-offs**:
- Deleted docs still occupy space until compaction
- **Acceptable**: Compaction runs regularly

**Future**: Explicit delete API (adds tombstone)

## Rejected Alternatives

### Why Not ClickHouse-Based (like MyScale)?

**Pros of ClickHouse**:
- Mature, battle-tested
- Great for analytics
- Built-in compression

**Cons**:
- Heavyweight (complex internals)
- Not designed for vector search
- Hard to embed

**Decision**: Build focused vector database from scratch

### Why Not Use Existing Vector DB?

**Options evaluated**:
- **Milvus**: Complex (Etcd, Pulsar dependencies), heavyweight
- **Weaviate**: Go-based, less Rust ecosystem integration
- **Qdrant**: Closest match, but different architecture (in-memory first)

**Decision**: Build Elacsym with focus on cost-effectiveness (object storage first)

### Why Not GPU Acceleration?

**Rationale**:
- RaBitQ is CPU-efficient (binary operations)
- GPU adds complexity (CUDA, hardware requirements)
- Most deployments don't have GPUs

**Future**: GPU support (P2) for very large indices (>100M vectors)

### Why Not WASM Plugins?

**Rationale**:
- Premature optimization (no users yet)
- Adds complexity
- Rust is already extensible (compile-time)

**Future**: Maybe (P3) if users request custom scoring/filtering

## Technical Debt and Future Work

### P1 - Performance & Reliability

1. **Prometheus Metrics**
   - Why: Observability is critical for production
   - Status: Planned, straightforward (use prometheus crate)

2. **Query Result Caching**
   - Why: Many queries are repeated (e.g., homepage)
   - Status: Easy with Foyer, just need cache key design

3. **Benchmarks**
   - Why: Need baseline performance numbers
   - Status: Planned, will use criterion.rs

### P2 - Advanced Features

1. **Replication (HA)**
   - Why: Single point of failure per namespace
   - Approach: Primary-backup with shared S3
   - Challenge: Coordination for failover

2. **Sparse Vectors**
   - Why: Many models produce sparse embeddings
   - Approach: Different index (inverted index style)

3. **Authentication**
   - Why: No built-in auth in v0.1.0
   - Approach: JWT tokens + API keys

4. **Multi-Vector Documents**
   - Why: Some use cases (e.g., ColBERT) need multiple vectors per doc
   - Approach: Extend schema to support Vec<Vec<f32>>

### P3 - Ecosystem

1. **Client SDKs** (Python, JavaScript, Go)
2. **Kubernetes Operator**
3. **Terraform Modules**
4. **Cloud Marketplace** (AWS, GCP, Azure)

## Architecture Influences

Elacsym's design is influenced by:

1. **Turbopuffer**: Object storage for vectors, caching strategy
2. **ClickHouse**: Columnar storage (Parquet), LSM-tree inspiration
3. **RocksDB/LevelDB**: Compaction logic, WAL design
4. **Snowflake**: Shared storage, stateless compute
5. **Vespa**: Hybrid search (vector + text)

## Conclusion

Elacsym makes deliberate trade-offs to achieve its goals:

**Prioritize**:
- Cost-effectiveness over raw speed
- Simplicity over features
- Operational ease over flexibility

**Accept**:
- Higher latency than in-memory systems (mitigated by caching)
- Write amplification from compaction (necessary trade-off for immutability)
- Limited HA in v0.1.0 (planned for P2)

**Avoid**:
- Distributed consensus complexity
- Heavyweight dependencies
- Premature optimization

The result is a vector database that is:
- **Affordable**: 100× cheaper than in-memory
- **Simple**: Few moving parts, easy to operate
- **Fast enough**: <100ms for most queries
- **Production-ready**: WAL, recovery, compaction, monitoring

## See Also

- [Architecture](architecture.md) - System design details
- [Performance](performance.md) - Tuning guide
- [Deployment](deployment.md) - Production setup
- [MyScale on GitHub](https://github.com/myscale/MyScaleDB) - The inspiration
