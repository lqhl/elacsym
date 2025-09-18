# Turbopuffer-style Search Engine (S3-Only) — Detailed Design

This document specifies a **serverless, S3-only, multi-namespace** search service called elacsym in Rust with **two-stage search**:
**Stage-1:** IVF + RaBitQ(1-bit) candidate generation → **Stage-2:** re-rank (int8 or fp32).
It integrates: per-part indexing, S3 manifest publication, deletes (tombstones), and background compaction.

---

## 0. Terminology & Symbols

* **Namespace (ns):** isolation unit (like a DB).
* **Part:** one batch insert; immutable after publish (except being superseded by compaction).
* **Small-part fallback:** if `N * dim ≤ 200_000`, treat as `K=1` (no IVF); Stage-1 scans RaBitQ codes directly.
* **K (centroids):** trained per **part** at build time:
  `K = clamp(round(cluster_factor * sqrt(N)), K_min, K_max)`.
* **nprobe (per part):** at **search time**, derived from user `probe_fraction`:
  `nprobe = clamp(round(probe_fraction * K), 1, min(K, nprobe_cap))`.
  If small-part fallback: `K=1, nprobe=1`.
* **Two-stage search parameters:**

  * `topk` (required)
  * `probe_fraction` (per-search; default from namespace)
  * `rerank_scale` (per-search; default = **5**) → Stage-1 candidate target = `topk * rerank_scale`
  * `rerank_precision ∈ {"none","int8","fp32"}` (default **"int8"**). If `rerank_scale=0`, Stage-2 is **skipped**.
  * `fp32_rerank_cap` (optional upper bound on fp32 re-rank pool).

---

## 1. External API (Minimal)

> Auth is out of scope. All responses JSON. Errors follow `{code, message, details?}`.

### 1.1 Create/Update Namespace

`POST /v1/namespaces`

```json
{
  "ns": "news-zh",
  "dim": 768,
  "cluster_factor": 1.0,              // controls K at build time
  "k_min": 1,
  "k_max": 65536,
  "nprobe_cap": 8192,
  "defaults": {
    "probe_fraction": 0.10,           // used at search time if not provided
    "rerank_scale": 5,                // default candidate multiplier
    "rerank_precision": "int8"        // "none" | "int8" | "fp32"
  }
}
```

**200** → `{ "ok": true }`

### 1.2 Insert Part (batch only)

`POST /v1/namespaces/{ns}/parts`

* Body: binary or JSON (vectors fp32, texts?, attrs?, ids?).
* Server builds **one part** containing **three representations**: RaBitQ-1bit, int8, fp32; plus IVF (unless small-part fallback).
  **200** → `{"part_id":"p000123","n":230000,"doc_id_range":[1000000,1230000),"epoch":42}`

### 1.3 Delete by IDs (soft delete)

`POST /v1/namespaces/{ns}/deletes/by_ids`

```json
{ "ids": [1000012, 1000311, 1000577] }
```

**200** → `{"del_part_id":"d000077","epoch":43}`

### 1.4 Search (two-stage)

`GET /v1/namespaces/{ns}/search`
Query params:

* `topk` (int, required)
* `probe_fraction` (float, (0,1], optional)
* `rerank_scale` (int, ≥0; default 5; 0 = skip re-rank)
* `rerank_precision` (`none|int8|fp32`; default `"int8"`)
* `fp32_rerank_cap` (int, optional)
* `text`, `filters` (opaque JSON), `consistency=strong|eventual` (default strong)

**200** →

```json
{
  "topk": 10,
  "items": [
    {"id": 123, "score": 12.34, "meta": {"snippet":"..."}},
    ...
  ],
  "debug": {
    "epoch": 52,
    "probe_fraction": 0.1,
    "rerank_scale": 5,
    "rerank_precision": "int8",
    "per_part": [{"part_id":"p000123","K":1517,"nprobe":152,"fallback":false}, ...]
  }
}
```

### 1.5 Read Manifest (debug)

`GET /v1/namespaces/{ns}/manifest` → current epoch JSON (see §3.3).

---

## 2. S3 Layout (Namespace-scoped)

```
/namespaces/{ns}/
  manifest/current                     # {"epoch": 42, "etag": "..."}
  manifest/{epoch}.json                # complete read view

  parts/{part_id}/
    meta.json                          # schema, dim, stats
    stats.json
    ivf/centroids.bin                  # f32[K][dim], 64B-aligned
    ivf/lists/{list_id}.ilist          # inverted list blocks (see §6.2)
    rabitq/meta.json                   # transform thresholds, seeds
    rabitq/codes-1bit.bin              # packed codes (if not in ilist)
    vectors/int8/vecpage-00000.bin     # int8 pages (see §6.3)
    vectors/int8/scales.bin            # per-dim or per-chunk scales
    vectors/fp32/vecpage-00000.bin     # raw float32 pages
    text/tantivy/...                   # optional
    attr/bitmaps/...                   # optional

  deletes/{del_part_id}/
    tombstone.bitmap.roaring           # preferred
    tombstone.ids.bin                  # for tiny sets
    meta.json

  tmp/{uuid}/...                       # build staging (gc on publish)
```

**Consistency:** publish by writing `manifest/{new}.json`, then atomically switch `manifest/current` with **If-Match** on old ETag. Readers see **old OR new** atomically.

---

## 3. Manifest

### 3.1 Namespace Section

```json
{
  "namespace": {
    "dim": 768,
    "cluster_factor": 1.0,
    "k_min": 1,
    "k_max": 65536,
    "nprobe_cap": 8192,
    "defaults": {
      "probe_fraction": 0.10,
      "rerank_scale": 5,
      "rerank_precision": "int8"
    }
  }
}
```

### 3.2 Parts Entry

```json
{
  "part_id": "p000123",
  "n": 2300000,
  "dim": 768,
  "k_trained": 1517,                   // 1 if small-part fallback
  "small_part_fallback": false,
  "doc_id_range": [100000, 2400000],
  "paths": {
    "centroids": "parts/p000123/ivf/centroids.bin",
    "ilist_dir": "parts/p000123/ivf/lists/",
    "rabitq_meta": "parts/p000123/rabitq/meta.json",
    "rabitq_codes": "parts/p000123/rabitq/codes-1bit.bin",
    "vec_int8_dir": "parts/p000123/vectors/int8/",
    "vec_fp32_dir": "parts/p000123/vectors/fp32/"
  },
  "stats": {
    "created_at": "2025-09-18T12:34:56Z",
    "mean_norm": 1.0
  }
}
```

### 3.3 Deletes

```json
"delete_parts": [
  {
    "del_part_id": "d000077",
    "type": "bitmap",
    "paths": {"bitmap": "deletes/d000077/tombstone.bitmap.roaring"},
    "created_at": "2025-09-18T13:00:00Z"
  }
]
```

---

## 4. Build Pipeline (Insert Part)

Input: `N` vectors (fp32), texts?, attrs?, ids? (optional).
Algorithm:

1. **Small-part check:** if `N * dim ≤ 200_000` → `K=1` (skip KMeans).
2. Else **train IVF**: `K = clamp(round(cluster_factor * sqrt(N)), K_min, K_max)`. K-Means++ on sample or full.
3. **Assign lists**: nearest centroid for each doc.
4. **Generate RaBitQ(1-bit) codes**: compute transform (randomized rotation/projection and thresholds) → pack bits (see §6.1).
5. **Quantize int8**:

   * Per-dim (or per 32-dim chunk) symmetric scale: `scale[d] = max(|v_i[d]|)`, `int8 = round(v / scale * 127)`.
   * Persist `scales.bin` and `vecpage-*.bin`.
6. **Persist fp32 pages** for exact re-rank/export.
7. **Write ilist** (if `K>1`): per list, contiguous blocks of `(docΔ, 1-bit code slice [, optional payload])`.
8. Upload to `tmp/{uuid}` → verify → promote to `parts/{part_id}` → write `manifest/{epoch+1}.json` → **atomic switch** `manifest/current`.

---

## 5. Deletes & Compaction

* **Deletes** are append-only **delete parts** (bitmap or ids).
* At **query time**, build **live mask** per part:
  `live_mask(part) = NOT( union(all tombstones intersecting part range) )` (cached).
* **Compaction (background)**: when many small parts accumulate or size thresholds trip:

  1. Read chosen parts + apply tombstones → live stream.
  2. Rebuild a **new large part** (retrain IVF; re-encode RaBitQ/int8; copy fp32).
  3. Publish manifest: add new part; drop fully covered old parts and redundant delete parts (optionally after safety window).
  4. GC old data after delay.

**Coordination:** single-ns compactor uses S3 lock: `/_control/merge.lock` with **If-None-Match** lease semantics.

---

## 6. On-Disk Formats (Key Files)

### 6.1 RaBitQ (1-bit) Codes

* `rabitq/meta.json`

  * random transform seeds, per-dim thresholds (or per-block), scaling corrections for unbiased distance estimate.
* `codes-1bit.bin` (if not co-located in ilist):

  * Packed bits for all docs in doc\_id order. Bit packing: row-major per doc, or blocked (e.g., 256 docs × dim bits) to align with cache lines.
  * Suggested block: 256 docs × dim bits → byte-aligned rows; include a small block header `{first_doc_id, count}`.

**Distance estimate:** XOR query bitstring with doc bitstring, POPCNT, linear correction → approximate L2/cosine proxy.

### 6.2 IVF ilist (`ivf/lists/{list_id}.ilist`)

```
Header { magic="ILST", version=1, list_id, count, block_size }
Block[i]:
  doc_id_delta_vbyte[]          // monotonically increasing
  code_bitmap[]                 // 1-bit codes aligned with docs (slice or indices)
  opt_payload...                // optional residual/PQ (not used now)
  Footer {first_doc_id, prefix_sums}
```

* Block size: 4–16 KB.
* Prefer storing the 1-bit code slice **inline** to reduce S3 range-GETs on cold reads.

### 6.3 Vector Pages

**int8:**

* `vectors/int8/vecpage-00000.bin`: fixed page (e.g., 8MB), contiguous doc rows (row = `dim` int8).
* `vectors/int8/scales.bin`: either per-dim scales (`dim` f32) or per-block (e.g., per 32 dims).

  * Recommended: **per-block** table for better cache: `blocks = dim/32`, `scales[blocks]`.

**fp32:**

* `vectors/fp32/vecpage-*.bin`: same paging, row = `dim` f32.

**Doc→page mapping:** doc IDs assigned sequentially within part; derive `(page_idx, offset)` arithmetically (no extra index). Keep a small header with `first_doc_id` per page.

---

## 7. Search Pipeline (Two-Stage)

### 7.1 Inputs

* Required: `topk`
* Optional: `probe_fraction` (default ns), `rerank_scale` (default 5), `rerank_precision` (`"int8"` default), `fp32_rerank_cap`
* Filters/text, consistency

### 7.2 Per-Part Planning

```rust
fn plan_for_part(part: &PartMeta, ns: &NsCfg, probe_fraction: f32) -> Plan {
    if part.small_part_fallback { return Plan{K:1, nprobe:1, fallback:true}; }
    let K = part.k_trained.max(1);
    let raw = (probe_fraction * K as f32).round() as usize;
    let nprobe = raw.clamp(1, K.min(ns.nprobe_cap.max(1)));
    Plan{K, nprobe, fallback:false}
}
```

### 7.3 Execution

1. **Read manifest/current** (strong: refresh each request; eventual: cached).

2. **For each visible part**:

   * If **fallback**: scan all 1-bit codes (blocked) → RaBitQ estimate for **all or batched docs**, keep up to `L_part = topk * rerank_scale` (or higher if filtering reduces pool).
   * Else: load centroids (mmap/SSD) → pick `nprobe` lists → for each list, scan ilist block(s), compute 1-bit estimate, keep `L_per_list` (e.g., `≈2×topk`) → merge to `L_part` (≈`topk * rerank_scale`).
   * Apply **text/attr filters** and **live mask** (deletes). If candidates < `topk`, consider doubling `nprobe` **within cap** (one backoff round).

3. **Stage-2 re-rank** based on `rerank_scale` and `rerank_precision`:

   * If `rerank_scale == 0` **or** `rerank_precision == "none"` → **skip** re-rank; return top-`topk` by 1-bit scores.
   * If `"int8"` (default): gather pages for candidate doc IDs → compute int8 distance (SIMD dot or dequantized L2) → push to global heap.
   * If `"fp32"`: optionally **first** do `"int8"` down-select to `M = min(fp32_rerank_cap, L_global)`; then fetch fp32 pages → compute exact distance (SIMD) → final heap.

4. **Merge across parts** and return global top-`topk`.

**Notes**

* Candidate size **per part** targets `topk * rerank_scale`. You may clamp locally by min/max to avoid skew (e.g., `[topk, 10*topk]`).
* Maintain a **global soft cap** for candidate pool to keep memory bounded; drop lowest if exceeded.
* Pre-aggregate **vector page fetches** (NVMe/S3 range-GET) based on candidate IDs (page set).

---

## 8. Parameter Defaults & Bounds

* `cluster_factor` (ns): **1.0** (0.5–2.0 typical)
* `K_min=1`, `K_max=65536`
* `probe_fraction` (search): **0.10** (0.02–0.5 typical)
* `nprobe_cap` (ns): default `8192`
* `rerank_scale` (search): **5** (integer ≥0; if 0 → no re-rank)
* `rerank_precision` (search): **"int8"**
* `fp32_rerank_cap` (search): default unset; recommended `5×topk` if used
* Small-part fallback threshold: `N * dim ≤ 200_000`

---

## 9. Rust Crate Layout

```
crates/
  api/            # axum handlers, validation, error types
  storage/        # S3 (aws-sdk-s3 or opendal), put_if_match, range-GET
  manifest/       # read/write epochs, atomic switch, caching
  part_builder/   # KMeans, RaBitQ encode, int8 quant, fp32 paging, ilist writer
  index/          # IVF select, ilist scan, 1-bit scoring, merges
  quant/          # RaBitQ transforms & POPCNT paths; int8 kernels; SIMD
  rerank/         # int8/fp32 distance kernels, page planner
  bitmap/         # roaring integration, live mask cache
  text/           # tantivy adapter (optional)
  compactor/      # part selection, apply deletes, rebuild, publish, GC
  common/         # ids, schema, metrics(tracing+OTLP), config, errors
```

---

## 10. Core Pseudocode

### 10.1 Search Handler

```rust
async fn search(req: SearchReq) -> Result<SearchResp> {
    let ns = manifest::load_ns_cfg(&req.ns).await?;
    let epoch = manifest::load_current(&req.ns, req.consistency).await?;
    let view  = manifest::load_epoch(&req.ns, epoch).await?;

    let pf   = req.probe_fraction.unwrap_or(ns.defaults.probe_fraction);
    let rs   = req.rerank_scale.unwrap_or(ns.defaults.rerank_scale);
    let prec = req.rerank_precision.unwrap_or(ns.defaults.rerank_precision);

    let q_tr = quant::rabitq::transform_query(&req.q_vec, &view.rabitq_global_meta);

    let mut global_cand = CandidateHeap::with_soft_cap(req.topk * rs * 4);

    for part in &view.parts {
        let plan = plan_for_part(part, &ns, pf);
        let mut cand = if plan.fallback {
            index::scan_all_1bit(part, &q_tr, req.topk * rs * 2).await?
        } else {
            index::ivf_probe_1bit(part, &q_tr, plan.nprobe, req.topk * rs).await?
        };
        filters::apply_text_attr(&mut cand, &req, part).await?;
        bitmap::apply_deletes(&mut cand, &view.delete_parts, part).await?;
        global_cand.extend(cand);
    }

    let out = match (rs, prec.as_str()) {
        (0, _) | (_, "none") => global_cand.topk(req.topk), // 1-bit only
        (_, "int8") => {
            rerank::int8_topk(&req.ns, &view, global_cand.into_vec(), req.topk).await?
        }
        (_, "fp32") => {
            let cap = req.fp32_rerank_cap.unwrap_or(req.topk * 5);
            let mid = rerank::int8_take(&req.ns, &view, global_cand.into_vec(), cap).await?;
            rerank::fp32_topk(&req.ns, &view, mid, req.topk).await?
        }
        _ => bail!("invalid rerank_precision"),
    };

    Ok(SearchResp { topk: req.topk, items: out, debug: Some(...)} )
}
```

### 10.2 Build Part (Insert)

```rust
async fn build_part(ns: &NsCfg, batch: Vec<Vec<f32>>) -> Result<PartArtifacts> {
    let n = batch.len(); let d = ns.dim;
    let small = n * d <= 200_000;

    let (K, cents) = if small {
        (1, None)
    } else {
        let K = clamp(round(ns.cluster_factor * sqrt(n)), ns.k_min, ns.k_max);
        let cents = kmeans_pp(batch.sample(...), K);
        (K, Some(cents))
    };

    // Assign lists (or all to list 0)
    let assigns = match cents { Some(c) => assign_to_centroids(&batch, &c), None => zeros(n) };

    // RaBitQ encode
    let (rq_meta, rq_codes) = quant::rabitq::encode_1bit(&batch);

    // int8 & fp32 pages
    let (int8_pages, scales) = quant::int8::encode(&batch);
    let fp32_pages = paging::fp32_pages(&batch);

    // Persist centroids, ilists, codes, int8, fp32
    // -> upload tmp/{uuid} -> promote -> write manifest -> switch current
    ...
}
```

---

## 11. Performance & Safety Notes

* **I/O locality:** always batch candidate doc IDs to **page-level** prefetch (NVMe or S3 range-GET).
* **Candidate sizing:** per-part target = `topk * rerank_scale`; clamp to `[topk, topk*10]`. Maintain global soft cap.
* **1-bit scoring:** vectorized XOR+POPCNT; pack bits in cache-friendly tiles (e.g., 256 docs × dim bits).
* **int8 kernels:** dot-product with per-block scales; AVX2/NEON paths; fall back to scalar.
* **fp32 kernels:** contiguous rows; AVX2/AVX-512; unroll by 8/16.
* **Consistency:** strong reads refresh `manifest/current`; eventual uses cached epoch.
* **Compaction:** ensure only one compactor per ns (S3 lease); do not block searches.

---

## 12. Testing & Bench

* **Unit:**

  * RaBitQ transform/encode/decode parity
  * int8 quantization accuracy vs fp32 (cosine/L2)
  * ilist block reader (boundary, vbyte delta, alignment)
  * manifest atomic switch (simulate concurrent writes)

* **Integration:**

  * Small-part fallback (N=64/128/256, dim=384/768) correctness & latency
  * Large parts (N=1M/5M), `probe_fraction∈{0.05,0.1,0.2}`, `rerank_scale∈{0,5,10}`, `rerank_precision∈{"none","int8","fp32"}`
  * Deletes correctness (bitmap vs ids), live-mask cache
  * Compaction publish and GC (old parts not visible after switch)

* **KPIs:**

  * P50/P95 split (Stage-1, Stage-2, S3 I/O counts/bytes)
  * Recall proxy (optional offline) under typical filters
  * NVMe cache hit ratio; S3 range-GET avg size

---

## 13. Defaults Summary (single place)

* Namespace:

  * `cluster_factor=1.0`, `k_min=1`, `k_max=65536`, `nprobe_cap=8192`, `defaults.probe_fraction=0.10`, `defaults.rerank_scale=5`, `defaults.rerank_precision="int8"`.
* Search:

  * `probe_fraction` omitted → namespace default
  * `rerank_scale` omitted → 5
  * `rerank_precision` omitted → "int8"
  * `fp32_rerank_cap` omitted → unset (or `5×topk` recommended when using "fp32").
