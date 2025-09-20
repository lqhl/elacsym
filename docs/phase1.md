# Phase 1 Design Notes

This document captures the concrete interface decisions for Phase 1 (v0 baseline) implementation. The goal is to deliver a functional end-to-end path for strong-consistency writes and FP32 query execution backed by a local object-storage surrogate.

## elax-store

### Namespaces & Layout

- Root path configured per process (defaults to `./.elacsym` during local development).
- Per-namespace directories under `namespaces/{namespace}/` with the following structure:
  - `wal/` — append-only batches stored as line-delimited JSON (`WAL-{sequence}.jsonl`).
  - `router.json` — authoritative pointer to latest materialized epoch and WAL high-watermark.

### Types

```rust
pub struct NamespacePath {
    pub namespace: String,
    pub root: PathBuf,
}

pub struct WalWriter {
    ns: NamespacePath,
}

pub struct RouterState {
    pub namespace: String,
    pub epoch: u64,
    pub wal_highwater: u64,
}
```

- `WalBatch` represents a strongly consistent batch. Encoded as `serde_json::Value` for now; later revisions will use columnar formats.
- `WalPointer` contains the namespace, sequence number, and byte range within the WAL file.

### Operations

- `WalWriter::append(batch) -> Result<WalPointer>`
  - Sequence number is monotonic per namespace; stored in `router.json` to survive restarts.
  - Batch is fsync’d before returning to guarantee durability.
- `Router::load(ns) -> Result<RouterState>` reads `router.json` or returns defaults.
- `Router::update(ns, new_state)` performs compare-and-swap using epochs to guard concurrent writers.

Local storage uses `tokio::fs` for async operations. We gate fsync behind a feature flag to ease testing.

## elax-core

### Namespace Registry

- `NamespaceRegistry` maintains active namespaces, retrieving router/WAL state from `elax-store` on demand.
- `NamespaceHandle` bundles the `WalWriter`, `RouterState`, and an in-memory row store (`Vec<Row>`).
- `Row` stores an `id`, raw vector (`Vec<f32>`), and JSON attributes.

### Query Execution

- `execute_query(request)` workflow:
  1. Ensure strong consistency by checking namespace router high-watermark against `WalPointer` returned from the most recent write (for Phase 1 we simply reload rows from WAL synchronously).
  2. Perform FP32 exhaustive search using cosine distance or L2 based on namespace schema.
  3. Apply optional filters (Phase 1 supports identity filter only).

### Writes

- `apply_write(batch)` persists to WAL via `WalWriter::append` and immediately applies updates to the in-memory row store so subsequent reads observe them.

## elax-api

### Framework

- Use `axum` + `tokio` to expose HTTP endpoints.
- `POST /v2/namespaces/:ns` accepts a `WriteBatch` JSON body with `upserts`/`deletes` arrays.
- `POST /v2/namespaces/:ns/query` accepts a `QueryRequest` with `rank_by`, `top_k`, and optional `vector` payload.

### Request/Response Types

- `WriteBatch` maps to `elax-core::WriteBatch`.
- `QueryRequest` triggers `execute_query` and returns `QueryResponse` containing rows sorted by score.

### Strong Consistency Handling

- After processing a write, the handler returns the WAL sequence number. Queries include an optional `min_wal_sequence`; server blocks (Phase 1: synchronous reload) until router state indicates the sequence is durable.

## Testing Strategy

- Unit tests in `elax-store` for WAL append/sequence ordering and router CAS behavior.
- Unit tests in `elax-core` verifying FP32 scoring and strong-consistency by issuing write + immediate query.
- Integration test in `tests/phase1_flow.rs` that drives the HTTP API: create namespace, upsert rows, query, delete.

## Limitations & Follow-Ups

- No background indexer yet; router updates are synchronous with writes.
- Filters are placeholders; future phases will add real planning.
- Object storage is simulated via local filesystem; swap with S3 client when available.
