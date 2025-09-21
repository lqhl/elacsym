# Backfill Runbook

This guide covers reprocessing an existing dataset into elacsym after schema fixes, model refreshes, or migrating from another system. Backfills stream writes through the WAL so indexers can rebuild parts and publish them atomically.

## Preconditions

- Confirm the namespace schema and `distance_metric` match the target dataset; mixing metrics across parts is unsupported.【F:docs/design.md†L120-L174】
- Verify the indexer fleet is healthy and caught up; `router.json.indexed_wal` should trail `wal_highwater` by less than one batch before starting.【F:crates/elax-store/src/lib.rs†L282-L333】
- Ensure sufficient object-store budget for temporary WAL growth and new parts.

## Backfill Workflow

1. **Stage source data.** Produce newline-delimited JSON (or Parquet converted with internal tooling) matching the namespace schema. Each row should contain the document `id`, vector payload (if applicable), and attribute map.【F:crates/elax-store/src/lib.rs†L334-L371】

2. **Replay through the write API.** Use `elax-cli writes ingest` (or a custom batcher) to issue `POST /v2/namespaces/:ns` requests in 1–5 MB chunks. Respect the 256 MB request cap documented in the design doc.【F:docs/design.md†L252-L330】【F:docs/design.md†L368-L380】

3. **Throttle on WAL lag.** Monitor `wal_highwater - indexed_wal`; if lag exceeds operational limits, pause ingestion to let indexers catch up. Large surges risk extended tail-scan fallbacks for queries.【F:crates/elax-store/src/lib.rs†L282-L333】

4. **Validate new parts.** Once indexing completes, confirm fresh part manifests exist under `parts/` and that `router.json.epoch` advanced. Use `elax-cli parts list` to double-check row counts.【F:crates/elax-store/src/lib.rs†L282-L333】

5. **Run smoke queries.** Exercise representative vector and BM25 queries against the namespace. Compare hit distributions against the source system to ensure no regressions in filters or FTS analyzers.【F:docs/design.md†L286-L330】

6. **Evaluate recall.** Call `POST /v1/namespaces/:ns/_debug/recall` with a sample workload. Verify `avg_recall` meets the namespace SLO before declaring success.【F:docs/design.md†L286-L308】

7. **Trim historical WAL.** After verifying the backfill, follow retention policy to age out superseded WAL segments and compact old parts if required.【F:docs/design.md†L100-L148】

## Failure Handling

- **Indexer stalls:** Inspect indexer logs for Parquet or Tantivy errors. You can temporarily disable ingestion and replay the offending batch once the bug is fixed.
- **Router not advancing:** If `router.json` does not reflect new parts, check for CAS conflicts on upload; indexers expect to own the router write and will retry if ETag checks fail.
- **Recall drop after backfill:** Follow the recall drift runbook to adjust `nprobe_ratio` or rerank strategy before re-running the bulk job.

Document the workload size, ingest duration, and any throttling actions in the post-run summary.
