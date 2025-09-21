# elacsym: Object-Storage-First Hybrid Search with Extended RaBitQ (Rust)

## 0) Executive Summary

**elacsym** is a horizontally scalable, multi-tenant search engine for **first-stage retrieval**. It combines:

* **Vector search**: IVF + **Extended RaBitQ** (ERQ).

  * **x-bit RaBitQ** for centroid/posting-list scan (default **x=1**).
  * **y-bit RaBitQ** for candidate reranking (default **y=8**).
  * Optional FP32 rerank for maximal recall.
* **Full-text search**: **Tantivy-powered** BM25 with native tokenization and filter-aware query planning.
* **Filtering/metadata**: zero-copy inverted indexes optimized for ranged reads on object stores.
* **Storage model**: S3-compatible **object storage as the source of truth** (+ NVMe/memory cache).
  Writes land in an **append-only WAL**, background indexers build per-part indices.

**Consistency**: strong read-after-write by default; configurable eventual reads for sub-10ms latencies.
**Cost model**: cache hot sets, fetch cold sets via few ranged GETs; compute is stateless per node.

---

## 1) Goals & Non-Goals

### Goals

* Object-storage-first architecture; no external consensus systems.
* High recall\@K at billion-scale via **ERQ** with provable trade-offs (as per the ERQ paper’s guarantees).
* Predictable cold latency via minimal object store round trips, fast warm hits via NVMe + RAM cache.
* Multi-tenant, namespace-scoped schemas, strong consistency, atomic batched upserts.
* Simple, language-agnostic HTTP API; easy client ports from existing “turbopuffer-style” integrations.

### Non-Goals

* Built-in second-stage neural rerankers (encouraged in userland).
* Heavy aggregations/analytics.
* Embedded embedding models.

---

## 2) High-Level Architecture

```
╔════════════╗       HTTPS        ╔═════════════════════════════════════════╗
║   Client   ║ ─────────────────▶ ║  elacsym Query Nodes (Rust, stateless)  ║
╚════════════╝                    ║   • WAL append (write-through cache)    ║
                                  ║   • Query planner (vector/FTS/filter)   ║
                                  ║   • NVMe + RAM cache (centroids, lists) ║
                                  ╚════════════╦════════════════════════════╝
                                                │ ranged reads / writes
                                                ▼
                                      ╔═══════════════ Object Storage ═══════╗
                                      ║ s3://org/ns/{wal,parts,index,meta}   ║
                                      ╚═══════════════╦══════════════════════╝
                                                      │   async index tasks
                                                      ▼
                                  ╔═════════════════════════════════════════╗
                                  ║   elacsym Indexers (auto-scaled Rust)   ║
                                  ║   • ingest WAL → parts                   ║
                                  ║   • build IVF + ERQ, FTS, filters        ║
                                  ║   • compaction / merges                  ║
                                  ╚═════════════════════════════════════════╝
```

* **Only** object storage is stateful; nodes are replaceable.
* **Per-namespace** prefixes; **one WAL entry/sec** target by default (configurable).
* **Cold query**: \~3–4 RTTs to object storage; **Warm**: sub-10ms p50 when hot in cache.

---

## 3) Data Model & Namespaces

* **Namespace**: isolated key-space (tenant, app, dataset).
* **Document**: `{ id, vector?, attributes... }`
* **Schema** (per namespace): attribute types, `filterable`, FTS config, vector type (`[d]f32` or `[d]f16`).
* **Distance**: `cosine_distance` (1 − cos sim) or `euclidean_squared`.

**IDs**: `u64 | uuid | string` (configurable).
**Vectors**: uniform dimensionality; JSON float array or base64 (LE f32/f16).

---

## 4) Storage Layout (Object Keys)

```
s3://{org}/{namespace}/
  wal/                   # append-only batches: WAL-000000000123.parquet.zstd
  parts/                 # immutable data parts: part-UUID/
    segment/rows-*.parq  # columnar rows (vector/attrs)
    fts/…                # per-attr postings, norms
    filters/…            # bitmaps / roaring / ranges
    ivf/
      meta.json          # IVF params, kmeans stats
      centroids.bin      # [nlist x d] float (f16/f32)
      postings/
        list-{cid}.erq1  # x-bit ERQ codes for scan (default 1-bit)
        list-{cid}.map   # docID → (offset, y-code slice)
        list-{cid}.erqY  # y-bit ERQ codes for rerank (default 8-bit)
  index/                 # merged views / routing tables
    router.json          # active part set, sequence numbers
  meta/
    schema.json
    stats.json
```

* **Parts** are append-only; compactions periodically merge small parts.
* **Router** is the atomic pointer to current active set (single small JSON, versioned).

---

## 5) Write Path & Consistency

1. Client calls `POST /v2/namespaces/:ns` with upserts/patches/deletes (+optional conditions).
2. Query node **batches within 1s window** → appends **WAL entry** to `wal/` (write-through local cache).
3. **Strong** consistency: on success, subsequent reads must observe the batch.

   * Query nodes **check** ETag/If-None-Match of `router.json` & newest WAL watermark.
4. Indexers tail WAL, materialize **parts**, update `router.json` with a monotonic **epoch**.
   Unindexed tail is still visible via **exhaustive tail scan** path.

**Atomic Batches**: all rows in a batch become visible atomically.
**Eventually Consistent Reads**: `{ level: "eventual" }` – skips blocking on latest WAL check (≤60s stale cap).

---

## 6) Indexing Pipeline

* **Ingest**: decode WAL → columnar segment(s) per part (Parquet + Zstd).
* **Vector**:

  * **IVF training** on samples; store centroids.
  * Encode vectors with **ERQ-y** (default 8-bit) once; derive **ERQ-x** bits (default 1-bit) for scan.
  * Build postings per centroid list with contiguous **y-code slabs** + **docID map**.
* **FTS**: leverage **Tantivy** to build BM25 postings, norms, and dictionaries per configured attribute; persist Tantivy segments alongside part metadata. `elax-fts` now exposes `LanguagePack` helpers so namespaces can register multiple analyzers (English, French, German, Spanish, Portuguese, Italian, Dutch, Danish, Finnish, Hungarian, Norwegian, Swedish, Russian, Romanian, Turkish, Arabic, Tamil, Greek) with shared defaults (lowercasing, ASCII folding, stemming, stop-word removal). `LanguagePackConfig` provides a serde-friendly representation so deployment configs can reference analyzers by ISO code (`"fr"`) and override tunables (`nostem`, `nostop`, custom token length) before mapping them into schema builders.
* **Filters**: build roaring/range indexes for filterable attributes.
* **Router publish**: write new part, update `router.json` (epoch++), GC old parts eventually.

**Implementation status**: the indexer now materializes part directories in object storage by writing `segment/rows.parquet` slabs (Arrow → Parquet with Zstd), persisting tombstone catalogs, and emitting placeholder IVF metadata so cached nodes can hydrate without rereading the WAL. Compaction replays existing part assets to form merged Parquet outputs before retiring source manifests.

**Compaction**: heuristics on part count/size; re-cluster lists; re-encode (no re-train unless drift).

---

## 7) Vector Algorithm: IVF + Extended RaBitQ

**Extended RaBitQ (ERQ)**:

* Extends RaBitQ to support a **space/accuracy continuum** (x,y bits).
* Retains RaBitQ’s theoretical error bounds; asymptotically optimal trade-off (per abstract).
* We implement ERQ in Rust as a crate `erq` with:

  * **Codebook learning** (offline & online incremental modes).
  * **Encoder/decoder** for x-bit and y-bit code paths.
  * **Distance estimators** for coarse (x-bit) and fine (y-bit) approximation.
  * Optional **residual coding** (list-wise residuals after centroid assignment).
  * SIMD kernels (AVX2/AVX512/NEON) via `std::arch` + runtime feature dispatch.

**Two-stage search**:

1. **Probe**: find top `nprobe` IVF lists using centroid index (cached in RAM).

   * **x-bit ERQ** scan within lists to get **k × rerank\_scale** candidates (default **5×**).
2. **Rerank**: score candidates using **y-bit ERQ** (default 8-bit).

   * Optional final **FP32** rerank when `rerank_mode = "fp32"` (highest accuracy, slower).

**Parameters**

* `nprobe_ratio` (namespace default, overridable per query): fraction of lists to probe (e.g., 0.02).
* `rerank_scale` (default **5**): candidate multiplier before rerank; `0` disables rerank.
* `erq_coarse_bits = x` (default **1**), `erq_rerank_bits = y` (default **8**).
* `alpha` (namespace-level **recall budget** tuning; stable name “**recall\_budget**”)
  influences default `nprobe_ratio` via `nprobe = α·sqrt(N/L)` style heuristic.
* `distance_metric`: `cosine_distance` or `euclidean_squared`.

**Fallbacks**

* **Small parts** (N·dim ≤ \~200K float-ops threshold): bypass IVF → **ERQ-y brute** or FP32 exact.

---

## 8) Query Planner

Given `(rank_by, filters, top_k)`:

* Chooses **vector-first** vs **filter-first** based on selectivity.
* **Recall-aware filtering**: intersect filter bitmaps with IVF candidate sets at list or candidate granularity.
* Minimizes object-store RTTs by grouping ranged reads per list/part and **prefetching y-code slabs**.

Implementation update: the planner now estimates selectivity via filter bitmap cardinality and candidate budgets, opting for filter-first execution when predicates collapse the candidate set or ANN is disabled, and falling back to vector-first when filters are broad. The core request type accepts serialized filter expressions and optional bitmap handles so query nodes can reuse precomputed intersections.

**Cold path RTT budget** (rule of thumb):

1. Router + metadata read
2. Centroid index & filter dictionaries
3. List offsets bitmap(s)
4. Slabs for selected lists (batched)

Warm path: NVMe/RAM hits, 8–15ms p50 achievable on 1M ranges.

---

## 9) Full-Text Search (Tantivy BM25)

We embed the [Tantivy](https://github.com/quickwit-oss/tantivy) search library to power all BM25 execution paths, mirroring the
integration pattern from the upstream [basic search example](https://tantivy-search.github.io/examples/basic_search.html).

* **Schema translation**: the `elax-fts` crate exposes a `SchemaConfig` builder that maps namespace configuration into Tantivy
  `TextOptions`, wiring the identifier field as a stored `STRING` and BM25 fields as tokenized text with configurable boosts and
  tokenizer overrides. Custom analyzers plug in through the built-in tokenizer registry so Phase 4 language packs remain a drop-in
  extension.
* **Index storage**: during part materialization, indexers stream documents into a `tantivy::IndexWriter` backed by an object-store
  directory abstraction. Each part publishes its Tantivy segment files (`*.fast`, `*.fieldnorm`, `*.idx`, `*.store`) under
  `parts/<part_id>/fts/tantivy/`. Query nodes mount the same object-store directory through a read-only Tantivy `Index`, caching
  hot segments on NVMe for low-latency reopen operations. The wrapper guarantees the identifier field is persisted so search hits
  can round-trip back into hybrid planning.
* **Object-store directory**: the `ObjectStoreDirectory` prototype wraps an `object_store::ObjectStore` client and the shared
  `elax-cache` NVMe slab. Reads opportunistically hydrate the cache and subsequent handles serve from memory; deletes tear down
  both the object and any cached blobs. Synchronous flushes upload the full byte slice so Tantivy reloads never observe partial
  segments.
* **Operational considerations**: `atomic_write` broadcasts `meta.json` updates to reload watchers, so query nodes immediately
  reopen after part commits. Cache eviction respects namespace pinning for dedicated FTS tiers, and NVMe warms opportunistically
  as query nodes fetch segment handles. Operators should budget for object-store PUT latency on flush and provision NVMe large
  enough to retain the hottest segment sets plus spillover headroom.
* **Commit & reload**: indexers commit after flushing each part. Query nodes open readers with `ReloadPolicy::OnCommit`, mirroring
  Tantivy's recommended pattern and avoiding bespoke invalidation wiring.
* **Query execution**: `TantivyIndex::search` wraps a `QueryParser`, applies field boosts declared in the schema, and executes the
  query via a shared `IndexSearcher`. Results land as lightweight `{score, doc_id}` pairs that feed either BM25-only responses or
  hybrid candidate sets intersected with vector/filter paths. Ranking relies on Tantivy's built-in BM25 scorer, with hooks to inject
  rerankers if needed.
* **Prefix & fuzzy support**: we reuse Tantivy's `PrefixQuery` and `FuzzyTermQuery` facilities instead of bespoke operators. The
  existing API surface (`last_as_prefix`, `ContainsAllTokens`) maps to these query constructors, maintaining backward compatibility.

Operationally, this approach reduces maintenance of bespoke FTS codecs, while aligning segment lifecycle with the existing
object-storage-first design. Future work may explore Tantivy's fast fields for metadata faceting and aggregated analytics once
the planner requirements solidify.

---

## 10) Filtering

* Operators: `Eq/NotEq/In/NotIn`, ranges (`Lt/Lte/Gt/Gte`), array ops (`Contains`, `ContainsAny`, etc.), glob/regex routed through Tantivy when BM25 is enabled.
* **id** is filterable.
* Bitmaps & range indexes are **zero-copy decoded** from cache or ranged-read.

---

## 11) API (Rust server; JSON over HTTP)

Paths mirror popular patterns for easy client reuse.

### Writes

`POST /v2/namespaces/:ns`

Body supports:

* `upsert_rows | upsert_columns`, `patch_rows | patch_columns`, `deletes`, `delete_by_filter`
* `upsert_condition | patch_condition | delete_condition`
* `distance_metric`, `schema`

**Strong** by default; responds once WAL is durably written to object storage.

### Queries

`POST /v2/namespaces/:ns/query`

Fields:

* `rank_by` (ANN | BM25 | OrderBy), `top_k` (≤1200 default), `filters`, `include_attributes | exclude_attributes`
* `aggregate_by`, `group_by`
* `queries`: multi-query (hybrid FTS + vector)
* `consistency`: `{ "level": "strong" | "eventual" }`

**Vector rank\_by examples**

```json
["vector", "ANN", [0.1, 0.2, ...]]
```

Advanced per-query overrides (optional):

```json
{
  "ann_params": {
    "nprobe_ratio": 0.02,
    "rerank_scale": 5,
    "erq_coarse_bits": 1,
    "erq_rerank_bits": 8,
    "rerank_mode": "erq"   // or "fp32"
  }
}
```

### Metadata

`GET /v1/namespaces/:ns/metadata` → schema, approx sizes, created\_at.

### Namespace Admin

* `GET /v1/namespaces?prefix=...`
* `DELETE /v2/namespaces/:ns`

### Cache Warm Hint

`GET /v1/namespaces/:ns/hint_cache_warm`

### Recall Evaluation

`POST /v1/namespaces/:ns/_debug/recall`

```json
{ "num": 25, "top_k": 10, "queries": null, "filters": null,
  "ann_params": { "rerank_mode": "fp32" } }
```

**Response**: `avg_recall`, `avg_ann_count`, `avg_exhaustive_count`, plus perf counters.

---

## 12) Rust Project Layout

```
elacsym/
  Cargo.toml
  crates/
    elax-api        # HTTP server, JSON types, auth middleware
    elax-core       # query planner, executors, consistency, perf counters
    elax-store      # object-store client, WAL, part I/O, cache, router
    elax-indexer    # async indexer daemon (WAL→parts), compaction
    elax-ivf        # IVF training, assignment, centroid search
    elax-erq        # Extended RaBitQ encode/decode, estimators, SIMD kernels
    elax-fts        # Tantivy integration (schema translation, query builders, directory glue)
    elax-filter     # roaring/range indexes
    elax-cache      # NVMe+RAM cache manager, direct I/O, mmap, prefetch
    elax-metrics    # OpenTelemetry, Prometheus exporters
    elax-cli        # admin tools (compact, verify, export)
```

**Key Traits**

```rust
pub trait VectorEncoder {
    fn train(&mut self, samples: &[f32], dim: usize);
    fn encode(&self, vec: &[f32]) -> Encoded;
    fn dist_estimate(&self, q: &QuerySide, code: &Encoded) -> f32;
}

pub trait Index {
    fn add_part(&mut self, part: PartHandle);
    fn search(&self, q: &AnnQuery, k: usize, params: &AnnParams) -> Vec<Candidate>;
}
```

**SIMD**: feature-gated kernels for x86\_64 (AVX2/AVX512) and aarch64 (NEON).
**Async**: `tokio` runtime; object-store via `aws-sdk-s3` / `cloud-storage` abstractions.

---

## 13) Cache & I/O

* **Centroid index** pinned in RAM (tiny).
* Posting list **offset tables** on NVMe; y-code slabs **mmap** with page-cache hints.
* **Direct I/O** for large sequential fills; **posix\_fadvise** where supported.
* Eviction policy: **temperature-aware LFU**; namespace pinning for VIPs.

---

## 14) Performance Targets (initial)

* **Cold** (1M docs): 300–500ms p50 (3–4 RTTs).
* **Warm** (1M docs): 8–15ms p50, 30–40ms p99.
* Write p50 (500KB batch): \~250–350ms (object store commit bound).
* **Recall\@10** (default): 90–100% with defaults (x=1, y=8, rerank\_scale=5, nprobe\_ratio ≈ 0.02).
* Index throughput: 10K+ vectors/s per indexer on commodity VMs (parallel parts).

(These are reference goals; actuals depend on cloud, dims, filters.)

---

## 15) Configuration (Namespace & Query)

**Namespace (set once, editable with care)**

* `vector: { type: [d]f32|f16, ann: true }`
* `recall_budget` (α): tunes default `nprobe_ratio`.
* `erq: { coarse_bits: x=1, rerank_bits: y=8, residual: true|false }`
* `distance_metric: "cosine_distance" | "euclidean_squared"`
* FTS per-attr options; `filterable` flags.

**Per-Query overrides**: `ann_params` (above), `consistency`, `include/exclude_attributes`.

---

## 16) Failure, Recovery, HA

* **Any node can serve any namespace**; placement only affects warmness.
* WAL is **append-only**; indexers are idempotent; `router.json` update is single-file CAS (ETag).
* If a query node restarts: rebuilds local cache lazily on demand.
* If object storage is unreachable: prefer **consistency over availability** (configurable).

---

## 17) Security & Encryption

* TLS everywhere.
* Pluggable KMS for **CMEK-style** encryption at rest (optional in OSS via envelope keys).
* Per-namespace API keys; read/write scopes.

---

## 18) Limits (initial defaults; configurable)

* Max dims: 8192 (raise with larger codebooks).
* Max top\_k: 1200.
* Max concurrent queries/namespace: 16.
* Max write batch: 256MB.
* Max attribute name length: 128 bytes; names count ≤ 256.
* Max document size: 64 MiB.
* Delete by filter: up to 5M per request (then repeat).

---

## 19) Testing, Benchmarks, and Recall

* **Ground truth**: FP32 exhaustive for sampled queries.
* **Recall endpoint** (`/_debug/recall`) automates sampling and reports `avg_recall`.
* Microbenchmarks for ERQ encode/scan/rerank; dataset harness (SIFT1M, GIST, Deep).

---

## 20) Migration & Interop

* API shapes align with common patterns; clients can be ported with minimal diffs.
* Export/import scripts via `elax-cli export` (ordered by `id` with pagination).

---

## 21) Build, Deploy, Operate

* **Build**: stable Rust, MSRV 1.77+, `RUSTFLAGS="-C target-cpu=native"` for best perf.
* **Container**: distroless + musl possible; AVX/NEON builds per arch.
* **Deploy**: Helm charts; indexers auto-scale on backlog; query nodes scale on QPS.
* **Metrics**: Prometheus + OpenTelemetry spans; SLO panels (p50/p90/p99, cache hit ratio, RTTs).
* **Configuration artifacts**: sample configs live in `configs/` (`query-node.sample.toml`, `namespace.sample.toml`); update them alongside this document when defaults or tunables change.
* **Config loader**: the `elax-config` crate consumes those TOML files (plus `ELAX_*` env overrides) and provisions filesystem, S3, or GCS object-store clients shared by the query node, indexer, and CLI.
* **GC**: tombstone horizon + retain epochs; background purge of old parts/WAL.

---

## 22) Open-Source Plan

* **License**: Apache-2.0.
* **Third-party**: Roaring bitmaps (croaring-rs), Parquet/Arrow, tokenizers.
* **ERQ Implementation**: native Rust crate; optional **FFI bridge** to reference C++ (for validation) gated by feature `erq-ffi`.

  * Reference: VectorDB-NTU/RaBitQ-Library (use to cross-check quality during bring-up).
* **Contrib**: CODEOWNERS/OWNERS per crate; Rustdoc, `clippy -D warnings`, benches in CI.

---

## 23) Pseudocode & Critical Paths

### 23.1 Query (Vector + Filters)

```rust
fn vector_search(ns: &NamespaceCtx, q: &[f32], top_k: usize, p: &AnnParams, filt: &FilterPlan)
 -> Result<Vec<Row>> {
    // Ensure strong consistency if requested
    if p.consistency == Strong { ns.refresh_router_if_stale()?; }

    // 1) Centroid stage (RAM)
    let centroids = ns.cache.centroids.load();
    let lists = centroids.top_nprobe(q, p.nprobe_ratio);

    // 2) Candidate gather (NVMe or ranged GET)
    let mut cands = BinaryHeap::new();
    for l in lists {
        let x_codes = ns.cache.get_x_codes(l)?; // ERQ-x slab, mmap or ranged
        for (doc, xcode) in x_codes.iter() {
            let d_est = erq::dist_estimate_x(q, xcode);
            cands.push((Reverse(d_est), doc));
            if cands.len() > top_k * p.rerank_scale { cands.pop_min(); }
        }
    }

    // 3) Rerank
    let mut reranked = Vec::with_capacity(cands.len());
    if p.rerank_mode == FP32 {
        let dense = ns.fetch_dense_vectors(&cands)?; // only for candidates
        for (doc, v) in dense { reranked.push((l2_or_cos(q, &v), doc)); }
    } else {
        let y_codes = ns.cache.get_y_codes(&cands)?; // batched fetch per list/part
        for (doc, ycode) in y_codes { reranked.push((erq::dist_estimate_y(q, ycode), doc)); }
    }

    // 4) Filters + finalize
    let filtered = filt.apply(&reranked, ns.cache.filters())?;
    Ok(topk(filtered, top_k).into_rows(ns.include_attrs()))
}
```

### 23.2 Indexer (WAL→Part)

```rust
loop {
  let batch = wal.next_batch()?;
  let seg = columnarize(batch);
  let (centroids, assign) = ivf::assign(&seg.vectors, cfg.nlist);
  let erq_y = erq::encode_y(&seg.vectors, cfg.erq.rerank_bits, assign.residuals());
  let erq_x = erq::derive_x(&erq_y, cfg.erq.coarse_bits);
  let part = build_part(seg, centroids, erq_x, erq_y, postings(assign));
  let part_uri = store.upload_part(part)?;
  router.cas_update(|r| r.add(part_uri).epoch_inc())?;
}
```

---

## 24) Tunables & Defaults (Sane Out-of-the-Box)

* `erq.coarse_bits = 1`, `erq.rerank_bits = 8`
* `rerank_scale = 5`
* `recall_budget (α)` picked to target **\~95% recall\@10** at typical dims
  (`nprobe_ratio` derived by heuristic + online continuous recall meter)
* `consistency.level = "strong"` by default; `"eventual"` opt-in
* FTS: `k1=1.2`, `b=0.75`, `language="english"`, `tokenizer="word_v2"`

---

## 25) Trade-offs

* **Writes**: high throughput, higher latency (object store commit).
* **Consistency floor**: \~10–20ms for strong reads due to metadata checks; use eventual to go sub-10ms.
* **Occasional cold queries**: P999 may hit \~100s of ms; warm hints recommended.

---

## 26) Cross-Cutting Workstreams

* **Status: DONE** — Set up CI jobs enforcing `cargo fmt --all`, `cargo clippy --all-targets --all-features -D warnings`, and `cargo test --workspace` via GitHub Actions (`.github/workflows/ci.yml`).
* **Status: DONE** — Add property tests (`proptest`) covering WAL ordering/recovery and ERQ distance estimates vs FP32 ground truth.
* **Status: DONE** — Keep `docs/design.md` and sample configs updated as features land; capture architecture impacts in PR templates (PR template now enforces linking updates to architecture notes).
* **Status: DONE** — Prototype Tantivy object-store directory + NVMe cache layer and document operational considerations.
* **Graph ANN** is avoided to minimize object-store RTTs & write amplification; IVF fits better.

---

## 27) Roadmap

**Bring-up**

* v0: FP32 exact + ERQ-y brute fallback → verify correctness.
* v0.1: IVF + ERQ-x/-y path, SIMD kernels, recall endpoint.
* v0.2: Compaction, router epochs, cache warm hints, metrics.
* v0.3: Regex index (opt-in), grouped aggregates, more languages.

**Performance**

* Direct I/O for cache fills; batched multi-range fetcher.
* Adaptive `nprobe_ratio` per query via learned model of corpus skew.
* Asynchronous prefetching of likely-next lists.

**Ecosystem**

* Official clients: TS, Python, Go, Java, Rust.
* Helm chart, Terraform examples.
* Import tools from common providers.

---

## 28) Appendix A — Schema Examples

**Enable FTS + vector (f16)**

```json
{
  "vector": { "type": "[768]f16", "ann": true },
  "title":  { "type": "string", "full_text_search": true },
  "tags":   { "type": "[]string", "full_text_search": { "stemming": false } },
  "price":  { "type": "float", "filterable": true }
}
```

---

## 29) Appendix B — Example Queries

**Hybrid multi-query**

```json
{
  "queries": [
    { "rank_by": ["vector", "ANN", "base64:..."], "top_k": 50 },
    { "rank_by": ["content", "BM25", "ultralight jacket"], "top_k": 50 }
  ]
}
```

**Vector + filters**

```json
{
  "rank_by": ["vector", "ANN", [0.1,0.2]],
  "filters": ["And", [
    ["price", "Lt", 100.0],
    ["tags", "ContainsAny", ["packable","down"]]
  ]],
  "top_k": 20,
  "ann_params": { "rerank_scale": 5, "rerank_mode": "erq" }
}
```

---

## 30) Appendix C — Operational Runbooks

* **Hot namespace pinning**: mark in `router.json: { pin: true }` → cache avoids eviction.
* **Backfill**: bulk import writes as columnar with `distance_metric` fixed; run indexer in “catch-up” mode.
* **Recall drift**: if `/_debug/recall` drops below SLO, bump `nprobe_ratio` or enable FP32 rerank temporarily.

---

### Notes on Extended RaBitQ Implementation

* Implement ERQ per paper spec with: (1) training for codebooks at multiple bit-budgets; (2) quantization consistent across x,y; (3) distance estimators that share LUTs to minimize per-candidate FLOPs.
* Provide `erq-ffi` feature that optionally links against the reference **C++ RaBitQ Library** for validation tests and A/B kernels; default path is **pure Rust**.
* Ensure runtime dispatch picks the highest available SIMD level while keeping a scalar fallback for portability.

---

## 31) Contribution Workflow

* `.github/pull_request_template.md` records summary, testing evidence, and a dedicated section for architecture/design impacts. Link to updated sections of `docs/design.md` or runbooks when behavior or tunables change.
* Update the sample configuration files in `configs/` whenever defaults shift so new operators can bootstrap clusters without reverse-engineering code-level values.
