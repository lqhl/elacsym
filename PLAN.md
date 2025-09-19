# PLAN

## Task Breakdown
| Step | Description | Status | Notes |
| --- | --- | --- | --- |
| 1 | Capture requirements and lay out work items in PLAN.md | Done | Summarised scope from design doc and repository goals. |
| 2 | Scaffold Rust workspace and crate skeletons aligned with the design | Done | Workspace file plus per-crate Cargo manifests and placeholder modules. |
| 3 | Add repository documentation (README, design doc, AGENTS guidance) | Done | Authored README, captured the design doc verbatim, and wrote repo-wide agent instructions. |
| 4 | Run baseline tooling (`cargo fmt`, `cargo check`) | Done | Verified formatting and compilation of the newly scaffolded workspace. |
| 5 | Implement storage adaptor (`S3Store`) and manifest I/O | Done | Backed the `ObjectStore` trait with real AWS SDK calls, added manifest load/publish helpers, and covered the flows with unit tests. |
| 6 | Build ingest pipeline (quantisation + part builder) | Done | Implemented RaBitQ encoding/decoding, full IVF/int8/RaBitQ part materialisation, and small-part fallback logic for ingest. |
| 7 | Flesh out search stack (candidate gen + rerank) | Done | Implemented IVF probing, RaBitQ scoring, live-set filtering, and int8/fp32 rerank stages with integration tests. |
| 8 | Expose HTTP surface & orchestration | Todo | `api::serve` is unimplemented; define Axum routes for namespace CRUD, ingest, search, and manifest debug endpoints, delegating to storage/manifests/search layers. |
| 9 | Tombstones and compaction workflows | Todo | Implement `bitmap::LiveSet::from_deletes` to materialise roaring sets and `compactor::compact_once` to drive background merging and manifest publication. |

## Progress Log
- Initialised planning document to track tasks and their completion.
- Created workspace structure with placeholder crates covering all subsystems.
- Documented architecture (README, design doc) and added contributor guidance in AGENTS.md.
- Executed `cargo fmt` and `cargo check` to ensure the skeleton builds cleanly.
- Implemented RaBitQ quantisation and part building pipeline, adding validation and tests for ingest artifacts.
- Reconciled the RaBitQ encoder semantics with the upstream C++ reference to match its centroid/bit rules.
- Expanded ingest to train IVF centroids, quantise int8 vectors, materialise inverted lists, and persist the full part layout to disk for testing.
- Built the search stack to probe IVF lists, score RaBitQ candidates, and rerank with int8/fp32 vectors backed by new tests.
- Added CI coverage that runs formatting, compilation, and ingest/search tests, and extended search coverage to span multiple parts.

## Outstanding TODO Summary
- **Search Path:** Wire the completed candidate generation and rerank pipeline into the forthcoming HTTP handlers once Step 8 lands.
- **API Surface:** `api::serve` must wire Axum routes for namespace CRUD, ingest, search, and health, coordinating with manifest, storage, and search layers.
- **Maintenance Jobs:** `bitmap::LiveSet::from_deletes` and `compactor::compact_once` need logic to hydrate roaring bitmaps, apply tombstones, and publish merged parts.

## Next Steps
1. Develop the search and reranking stages (Step 7) using the produced artefacts and tombstone filtering.
2. Expand the HTTP API and orchestration layer (Step 8) once core workflows are functional.
3. Close the loop with tombstone application and compaction automation (Step 9) to maintain namespace health.
