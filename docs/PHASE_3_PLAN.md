# Phase 3: Production Readiness - Implementation Plan

**Created**: 2025-10-05
**Status**: Planning
**Goal**: Make Elacsym production-ready with durability, reliability, and performance

---

## 📊 Current Status

### ✅ Completed (Phase 1-2)
- Complete HTTP API (create namespace, upsert, query)
- Vector search with RaBitQ
- Full-text search with Tantivy (BM25, multi-field)
- Hybrid search with RRF fusion
- Attribute filtering (all operators)
- Foyer cache (Memory + Disk)
- Write-Ahead Log (WAL) for crash safety
- Advanced full-text configuration

### ❌ Gaps for Production
1. **WAL recovery not implemented** - data loss on crash
2. **WAL unbounded growth** - disk space issues
3. **No compaction** - unlimited segment growth
4. **No monitoring** - cannot observe system health
5. **Limited testing** - no integration tests
6. **No performance benchmarks** - unknown limits

---

## 🎯 Phase 3 Objectives

### P0 - Critical (Must Have)
Make the system **safe** and **reliable** for production use.

### P1 - Important (Should Have)
Make the system **fast** and **observable**.

### P2 - Nice to Have (Could Have)
Make the system **scalable** and **feature-rich**.

---

## 🔴 P0 Tasks - Critical for Production

### P0-1: WAL Recovery ⭐⭐⭐

**Why**: Without recovery, crashes cause data loss. Unacceptable for production.

**Effort**: 2-3 hours
**Dependencies**: None

**Implementation**:

1. **Add replay method to WalManager**
   - File: `src/wal/mod.rs`
   - Method: `pub async fn replay(&self) -> Result<Vec<WalOperation>>`
   - Logic: Read all entries from WAL, return operations list

2. **Create internal upsert method**
   - File: `src/namespace/mod.rs`
   - Method: `async fn upsert_internal(&self, documents: Vec<Document>) -> Result<usize>`
   - Logic: Same as `upsert()` but **without WAL writes** (to avoid recursion)

3. **Implement recovery in Namespace::load()**
   ```rust
   // After creating WAL
   let wal = WalManager::new(&wal_dir).await?;
   let operations = wal.replay().await?;

   // Replay operations
   for op in operations {
       match op {
           WalOperation::Upsert { documents } => {
               self.upsert_internal(documents).await?;
           }
           WalOperation::Delete { ids } => {
               // Handle deletes (future work)
           }
           _ => {}
       }
   }

   // Truncate WAL after successful replay
   wal.truncate().await?;
   ```

4. **Add integration test**
   - Test: Write data → Kill process → Restart → Verify data
   - Use `tempfile::TempDir` for isolation

**Success Criteria**:
- [ ] WAL entries are replayed on startup
- [ ] Data survives crash (simulated by not truncating WAL)
- [ ] Integration test passes

---

### P0-2: WAL Rotation ⭐⭐

**Why**: Unbounded WAL growth will fill disk. Need automatic rotation.

**Effort**: 2-3 hours
**Dependencies**: P0-1 (recovery logic)

**Implementation**:

1. **Add rotation logic to WalManager**
   - Check WAL file size before each append
   - If > 100MB, rotate to new file
   - File naming: `wal_000001.log`, `wal_000002.log`, ...

2. **Update append() method**
   ```rust
   pub async fn append(&mut self, operation: WalOperation) -> Result<u64> {
       // Check if rotation needed
       if self.should_rotate().await? {
           self.rotate().await?;
       }

       // ... existing append logic ...
   }

   async fn should_rotate(&self) -> Result<bool> {
       let metadata = self.current_file.metadata().await?;
       Ok(metadata.len() > 100 * 1024 * 1024) // 100MB
   }

   async fn rotate(&mut self) -> Result<()> {
       // Close current file
       // Increment sequence number
       // Open new file with incremented name
       // Write header
   }
   ```

3. **Add cleanup logic**
   - Keep only last N WAL files (e.g., N=5)
   - Delete older files during rotation

4. **Update truncate() to handle multiple files**
   - Truncate all WAL files, not just current one

**Success Criteria**:
- [ ] WAL rotates when > 100MB
- [ ] Old WAL files are cleaned up
- [ ] Recovery works across multiple WAL files

---

### P0-3: Error Recovery ⭐⭐

**Why**: Corrupted data should not crash the system.

**Effort**: 1-2 hours
**Dependencies**: None

**Implementation**:

1. **Handle CRC mismatches gracefully**
   - Current: Returns error and stops
   - New: Log error, skip entry, continue reading

2. **Handle truncated WAL entries**
   - If EOF during entry read, treat as incomplete
   - Log warning, discard partial entry

3. **Add corruption detection to Parquet reads**
   - Wrap Parquet reads in error handling
   - Return empty vec if corrupted, log error

4. **Add health check endpoint**
   - `GET /health` - Returns system status
   - Include WAL status, cache status, segment count

**Success Criteria**:
- [ ] Corrupted WAL entries don't crash server
- [ ] Health endpoint returns useful diagnostics

---

### P0-4: Integration Tests ⭐⭐⭐

**Why**: Unit tests don't catch integration bugs. Need end-to-end testing.

**Effort**: 3-4 hours
**Dependencies**: None

**Implementation**:

1. **Create tests/integration directory**
   - `tests/integration/api_test.rs`
   - `tests/integration/crash_recovery_test.rs`
   - `tests/integration/compaction_test.rs`

2. **API Integration Tests**
   ```rust
   #[tokio::test]
   async fn test_full_workflow() {
       // Start server
       // Create namespace
       // Insert 1000 documents
       // Query by vector
       // Query by full-text
       // Hybrid query
       // Verify results
   }
   ```

3. **Crash Recovery Test**
   ```rust
   #[tokio::test]
   async fn test_crash_recovery() {
       // Create namespace
       // Insert data
       // Manually skip WAL truncate
       // Restart namespace
       // Verify data recovered
   }
   ```

4. **Concurrency Test**
   ```rust
   #[tokio::test]
   async fn test_concurrent_writes() {
       // Spawn 10 concurrent upsert tasks
       // Each writes 100 documents
       // Verify all 1000 documents present
   }
   ```

**Success Criteria**:
- [ ] At least 5 integration tests
- [ ] Tests cover happy path and error cases
- [ ] Tests run in CI

---

### P0-5: Tantivy Analyzer Configuration ⭐

**Why**: Advanced full-text config (language, stemming) is parsed but not applied.

**Effort**: 2 hours
**Dependencies**: None

**Implementation**:

1. **Update FullTextIndex::new() to accept config**
   ```rust
   pub fn new(field_name: String, config: &FullTextConfig) -> Result<Self> {
       let mut schema_builder = Schema::builder();

       // Configure text field with analyzer based on config
       let text_options = match config {
           FullTextConfig::Simple(_) => {
               TextOptions::default()
                   .set_indexing_options(...)
                   .set_stored()
           }
           FullTextConfig::Advanced { language, stemming, remove_stopwords, .. } => {
               let analyzer = match language.as_str() {
                   "english" => /* English analyzer with stemming */,
                   "chinese" => /* Chinese analyzer */,
                   _ => /* Default */
               };
               TextOptions::default()
                   .set_indexing_options(TextFieldIndexing::default()
                       .set_tokenizer(analyzer))
                   .set_stored()
           }
       };

       let text_field = schema_builder.add_text_field(&field_name, text_options);
       // ...
   }
   ```

2. **Update Namespace to pass config**
   ```rust
   // In create() and load()
   if attr_schema.full_text.is_enabled() {
       let index = FullTextIndex::new(
           field_name.clone(),
           &attr_schema.full_text
       )?;
       fulltext_indexes.insert(field_name.clone(), index);
   }
   ```

3. **Add test with non-English text**
   - Test Chinese text with Chinese analyzer
   - Test stemming (search "running" matches "run")

**Success Criteria**:
- [ ] Language-specific analyzers work
- [ ] Stemming is applied when enabled
- [ ] Stopwords are removed when enabled

---

## 🟡 P1 Tasks - Performance & Reliability

### P1-1: LSM-tree Compaction ⭐⭐⭐

**Why**: Unlimited segment growth degrades query performance.

**Effort**: 6-8 hours
**Dependencies**: None

**Implementation**:

1. **Create compaction module**
   - File: `src/namespace/compaction.rs`

2. **Implement compaction trigger**
   ```rust
   impl Namespace {
       async fn should_compact(&self) -> bool {
           let manifest = self.manifest.read().await;
           manifest.segments.len() > 100 // Configurable threshold
       }
   }
   ```

3. **Implement compaction logic**
   ```rust
   async fn compact(&self) -> Result<()> {
       // 1. Select segments to merge (e.g., smallest N segments)
       // 2. Read all documents from selected segments
       // 3. Write merged data to new segment
       // 4. Rebuild vector index with merged data
       // 5. Atomically update manifest (remove old segments, add new)
       // 6. Delete old segment files
   }
   ```

4. **Add background compaction task**
   ```rust
   // In main.rs or namespace manager
   tokio::spawn(async move {
       loop {
           tokio::time::sleep(Duration::from_secs(3600)).await; // Every hour

           for namespace in namespaces.iter() {
               if namespace.should_compact().await {
                   if let Err(e) = namespace.compact().await {
                       tracing::error!("Compaction failed: {}", e);
                   }
               }
           }
       }
   });
   ```

5. **Add configuration**
   ```toml
   [compaction]
   max_segments = 100
   interval_secs = 3600
   min_segment_size = 10485760  # 10MB
   ```

**Success Criteria**:
- [ ] Compaction merges small segments
- [ ] Vector index is rebuilt after compaction
- [ ] Queries work correctly after compaction
- [ ] Old segments are deleted

---

### P1-2: Metrics & Monitoring ⭐⭐

**Why**: Can't optimize what you can't measure.

**Effort**: 3-4 hours
**Dependencies**: None

**Implementation**:

1. **Add prometheus dependency**
   ```toml
   prometheus = "0.13"
   ```

2. **Create metrics module**
   - File: `src/metrics/mod.rs`
   ```rust
   use prometheus::{Registry, Histogram, Gauge, Counter};

   pub struct Metrics {
       pub query_duration: Histogram,
       pub upsert_duration: Histogram,
       pub cache_hit_rate: Gauge,
       pub segment_count: Gauge,
       pub wal_size: Gauge,
       pub index_size: Gauge,
   }
   ```

3. **Instrument code**
   ```rust
   // In Namespace::query()
   let timer = metrics.query_duration.start_timer();
   let results = /* query logic */;
   timer.observe_duration();

   // In Namespace::upsert()
   let timer = metrics.upsert_duration.start_timer();
   /* upsert logic */
   timer.observe_duration();
   ```

4. **Add metrics endpoint**
   ```rust
   // GET /metrics - Prometheus format
   async fn metrics() -> String {
       let encoder = TextEncoder::new();
       let metric_families = prometheus::gather();
       encoder.encode_to_string(&metric_families).unwrap()
   }
   ```

5. **Add Grafana dashboard JSON**
   - File: `grafana/elacsym-dashboard.json`
   - Include panels for latency, throughput, cache hit rate, segment count

**Success Criteria**:
- [ ] Metrics endpoint returns Prometheus format
- [ ] Key metrics are tracked (query latency, cache hit rate)
- [ ] Grafana dashboard loads

---

### P1-3: Benchmarks ⭐⭐

**Why**: Need to know performance characteristics and regression detection.

**Effort**: 2-3 hours
**Dependencies**: None

**Implementation**:

1. **Create benchmark suite**
   - File: `benches/query_benchmark.rs`
   ```rust
   use criterion::{criterion_group, criterion_main, Criterion};

   fn bench_vector_query(c: &mut Criterion) {
       // Setup: Create namespace with 100k vectors
       // Benchmark: Query with random vector
       c.bench_function("vector_query_100k", |b| {
           b.iter(|| {
               // Query
           });
       });
   }

   criterion_group!(benches, bench_vector_query);
   criterion_main!(benches);
   ```

2. **Add benchmarks for**:
   - Vector query (varying dataset sizes: 10k, 100k, 1M)
   - Full-text query
   - Hybrid query
   - Upsert (batch sizes: 1, 10, 100, 1000)
   - Cache hit vs miss

3. **Add benchmark CI job**
   ```yaml
   # .github/workflows/bench.yml
   - name: Run benchmarks
     run: cargo bench --bench query_benchmark
   ```

**Success Criteria**:
- [ ] Benchmarks run successfully
- [ ] Results are reproducible
- [ ] Can detect performance regressions

---

### P1-4: Query Optimizer ⭐

**Why**: Some query plans are more efficient than others.

**Effort**: 4-5 hours
**Dependencies**: P1-2 (metrics for cost estimation)

**Implementation**:

1. **Create optimizer module**
   - File: `src/query/optimizer.rs`

2. **Implement cost-based decisions**
   ```rust
   impl QueryOptimizer {
       fn optimize(&self, request: &QueryRequest) -> ExecutionPlan {
           // If filter is very selective, apply filter first
           if self.filter_selectivity(&request.filter) < 0.1 {
               ExecutionPlan::FilterFirst
           } else if request.vector.is_some() && request.full_text.is_none() {
               ExecutionPlan::VectorOnly
           } else {
               ExecutionPlan::Parallel
           }
       }
   }
   ```

3. **Add execution plans**
   ```rust
   enum ExecutionPlan {
       FilterFirst,    // Filter → Vector/FT search
       VectorOnly,     // Skip full-text
       FullTextOnly,   // Skip vector
       Parallel,       // Vector || Full-text → RRF
   }
   ```

4. **Collect statistics**
   - Track filter selectivity
   - Track segment sizes
   - Use stats for cost estimation

**Success Criteria**:
- [ ] Optimizer chooses filter-first for selective filters
- [ ] Query performance improves on filtered queries

---

## 🟢 P2 Tasks - Advanced Features

### P2-1: Distributed Mode ⭐⭐⭐

**Why**: Single-node limits scalability.

**Effort**: 10-15 hours
**Dependencies**: P1-1 (compaction), P1-2 (metrics)

**Scope**:
- Shard data across multiple nodes
- Consistent hashing for shard assignment
- Query routing and aggregation
- Rebalancing on node add/remove

**Not in Scope**: This is Phase 4 work. Too complex for Phase 3.

---

### P2-2: Replication ⭐⭐

**Why**: High availability and fault tolerance.

**Effort**: 8-10 hours
**Dependencies**: P2-1 (distributed mode)

**Scope**:
- Replicate segments to N nodes
- Read from any replica
- Write to all replicas (or leader-follower)
- Failover on node failure

---

### P2-3: Bulk Import ⭐

**Why**: Faster initial loading of large datasets.

**Effort**: 2-3 hours
**Dependencies**: None

**Implementation**:

1. **Add bulk import endpoint**
   ```bash
   POST /v1/namespaces/:namespace/bulk
   Content-Type: application/x-ndjson

   {"id": 1, "vector": [...], "attributes": {...}}
   {"id": 2, "vector": [...], "attributes": {...}}
   ...
   ```

2. **Optimize for bulk**
   - Batch WAL writes
   - Batch index updates
   - Larger segments

**Success Criteria**:
- [ ] Can import 1M vectors in < 5 minutes

---

## 📅 Implementation Timeline

### Week 1: P0 Tasks (Critical)
- Day 1-2: WAL Recovery (P0-1)
- Day 3: WAL Rotation (P0-2)
- Day 4: Error Recovery (P0-3)
- Day 5: Integration Tests (P0-4)

### Week 2: P0 + P1 Tasks
- Day 1: Tantivy Analyzer Config (P0-5)
- Day 2-3: LSM Compaction (P1-1)
- Day 4: Metrics (P1-2)
- Day 5: Benchmarks (P1-3)

### Week 3: P1 + Polish
- Day 1-2: Query Optimizer (P1-4)
- Day 3-4: Documentation updates
- Day 5: Performance tuning

### Week 4+: P2 Tasks (Optional)
- Bulk import
- Advanced features as needed

---

## 🎯 Success Criteria for Phase 3

### Functional Requirements
- [ ] System recovers from crashes without data loss
- [ ] WAL doesn't grow unbounded
- [ ] Compaction runs automatically
- [ ] All integration tests pass
- [ ] Advanced full-text config works

### Non-Functional Requirements
- [ ] P99 query latency < 100ms (100k vectors)
- [ ] Write throughput > 1000 docs/s
- [ ] Cache hit rate > 80%
- [ ] System uptime > 99.9%

### Operational Requirements
- [ ] Prometheus metrics exposed
- [ ] Grafana dashboard available
- [ ] Documentation complete
- [ ] Benchmarks show acceptable performance

---

## 📚 References

- [Designing Data-Intensive Applications](https://dataintensive.net/) - Chapters on replication, partitioning
- [LSM-tree Paper](https://www.cs.umb.edu/~poneil/lsmtree.pdf) - Compaction strategies
- [PostgreSQL WAL Internals](https://www.postgresql.org/docs/current/wal-internals.html)

---

## 🚀 Getting Started

**To start Phase 3 implementation**:

1. Read this plan
2. Start with P0-1 (WAL Recovery) - highest priority
3. Create feature branch: `git checkout -b phase-3-wal-recovery`
4. Implement, test, commit
5. Create SESSION_7_SUMMARY.md when done
6. Move to next P0 task

**Remember**: P0 tasks are **blockers** for production. Don't skip them!

---

**Let's build production-grade Elacsym! 🚀**
