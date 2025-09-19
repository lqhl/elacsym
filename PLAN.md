# PLAN

## Task Breakdown
| Step | Description | Status | Notes |
| --- | --- | --- | --- |
| 1 | Capture requirements and lay out work items in PLAN.md | Done | Summarised scope from design doc and repository goals. |
| 2 | Scaffold Rust workspace and crate skeletons aligned with the design | Done | Workspace file plus per-crate Cargo manifests and placeholder modules. |
| 3 | Add repository documentation (README, design doc, AGENTS guidance) | Done | Authored README, captured the design doc verbatim, and wrote repo-wide agent instructions. |
| 4 | Run baseline tooling (`cargo fmt`, `cargo check`) | Done | Verified formatting and compilation of the newly scaffolded workspace. |
| 5 | Implement storage adaptor (`S3Store`) and manifest I/O | Done | Backed the `ObjectStore` trait with real AWS SDK calls, added manifest load/publish helpers, and covered the flows with unit tests. |
| 6 | Build ingest pipeline (quantisation + part builder) | Todo | `quant::{encode_rabitq,score_with_rabitq}` and `part_builder::build_part` need real implementations to generate RaBitQ metadata, write artifacts, and emit statistics per the design doc. |
| 7 | Flesh out search stack (candidate gen + rerank) | Todo | `index::search_namespace` and `rerank::{rerank_int8,rerank_fp32}` remain placeholders; plan to integrate IVF probing, tombstone handling, and rerank fallbacks. |
| 8 | Expose HTTP surface & orchestration | Todo | `api::serve` is unimplemented; define Axum routes for namespace CRUD, ingest, search, and manifest debug endpoints, delegating to storage/manifests/search layers. |
| 9 | Tombstones and compaction workflows | Todo | Implement `bitmap::LiveSet::from_deletes` to materialise roaring sets and `compactor::compact_once` to drive background merging and manifest publication. |

## Progress Log
- Initialised planning document to track tasks and their completion.
- Created workspace structure with placeholder crates covering all subsystems.
- Documented architecture (README, design doc) and added contributor guidance in AGENTS.md.
- Executed `cargo fmt` and `cargo check` to ensure the skeleton builds cleanly.

## Outstanding TODO Summary
- **Ingest Pipeline:** `quant::encode_rabitq`, `quant::score_with_rabitq`, and `part_builder::build_part` are skeletons. Implement RaBitQ encoding, scoring heuristics, IVF training, part artefact assembly, and upload staging per `docs/design.md`.
- **Search Path:** `index::search_namespace` and the rerank functions (`rerank::rerank_int8`, `rerank::rerank_fp32`) are placeholders; build candidate generation, live-set filtering, and reranking strategies aligned with namespace defaults.
- **API Surface:** `api::serve` must wire Axum routes for namespace CRUD, ingest, search, and health, coordinating with manifest, storage, and search layers.
- **Maintenance Jobs:** `bitmap::LiveSet::from_deletes` and `compactor::compact_once` need logic to hydrate roaring bitmaps, apply tombstones, and publish merged parts.

## Next Steps
1. Implement the ingest pipeline (Step 6), leveraging quantisation kernels to emit parts and manifest entries.
2. Develop the search and reranking stages (Step 7) using the produced artefacts and tombstone filtering.
3. Expand the HTTP API and orchestration layer (Step 8) once core workflows are functional.
4. Close the loop with tombstone application and compaction automation (Step 9) to maintain namespace health.
