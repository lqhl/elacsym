# Repository Guidelines

## Project Structure & Module Organization

The repository currently tracks architecture plans in `docs/design.md`. Rust crate sources belong in `src/` at the root; split reusable components into `crates/` and binaries into `bin/` as they appear. Place integration fixtures under `tests/`, and keep heavy assets out of git—prefer mocks or generated data documented in `docs/assets/`.

## Build, Test, and Development Commands

- `cargo build --all-targets` compiles the full workspace; run before pushing.
- `cargo fmt --all` enforces formatting; configure editors to run on save.
- `cargo clippy --all-targets --all-features -D warnings` gates on lint cleanliness.
- `cargo test --workspace` executes unit, integration, and property suites.
Use `RUST_LOG=debug cargo run --bin query-node` during local iteration; store sample configs in `configs/*.toml`.

## Coding Style & Naming Conventions

Follow rustfmt defaults with 4-space indentation. Modules and files use `snake_case`; types and traits use `CamelCase`; constants use `SCREAMING_SNAKE_CASE`. Prefer `anyhow::Result<T>` for fallible APIs and document public interfaces with Rustdoc. Keep binary entrypoints thin—push logic into library crates for reuse and easier testing.

## Testing Guidelines

Add unit tests inline under `mod tests` blocks. Integration and API smoke tests live in `tests/` and should exercise ERQ x-bit and y-bit paths plus BM25 filters. When touching storage, add property tests (e.g., `proptest`) that verify WAL ordering and recovery. Note any uncovered scenarios in the PR description.

## Commit & Pull Request Guidelines

Write imperative, present-tense commit subjects under 72 characters (see `Add design doc`). Squash incidental fixups locally. Pull requests must include a summary, ERQ/architecture impact assessment, testing log (`cargo test`, `clippy`), and links to related issues or design sections. Attach logs or screenshots when user-facing behavior changes.

## Documentation Expectations

Update `docs/design.md` when architecture or tunables evolve. Record operational runbooks in `docs/runbooks/` and keep sample configs synchronized with code defaults so new agents can bootstrap clusters quickly.

---

## Implementation Plan

Legend: `TODO` = not started, `DOING` = in progress, `DONE` = complete.

### Phase 0 — Workspace Bring-Up
- Status: DONE — Create Cargo workspace scaffolding, top-level `Cargo.toml`, and initial crate directories matching `docs/design.md` (`crates/`, `bin/`, `tests/`).
- Status: DONE — Document build/test commands in `README.md` and ensure `cargo fmt`, `clippy`, and `test` scripts run locally.
- Status: DONE — Add developer onboarding notes (toolchain, style) to `docs/design.md` or new `docs/runbooks/` entry.

### Phase 1 — v0 Baseline (FP32 exact + ERQ-y brute fallback)
- Status: DONE — Implement `elax-store` with WAL append, router management, and pluggable object-store clients (local + S3-compatible).
- Status: DONE — Implement `elax-core` FP32 query path with strong consistency checks against router/WAL watermarks.
- Status: DONE — Expose `elax-api` HTTP endpoints for `POST /v2/namespaces/:ns` writes and `POST /v2/namespaces/:ns/query` with FP32 execution.
- Status: DONE — Provide integration tests covering strong consistency and mixed write/read batches (see Testing Guidelines).

### Phase 2 — v0.1 IVF + ERQ Bring-Up
- Status: DONE — Build `elax-ivf` crate for centroid training, list assignment, and nprobe selection heuristics (k-means++ trainer, assignment API, and nprobe heuristic validated with core integration tests).
- Status: DONE — Build `elax-erq` crate implementing Extended RaBitQ encode/decode (x-bit/y-bit) plus SIMD feature gates.
- Status: DONE — Integrate IVF + ERQ search into `elax-core` query planner with configurable `ann_params` defaults, ERQ encoding, and FP32 rerank support.
- Status: DONE — Implement recall evaluation endpoint (`/_debug/recall`) exercising FP32 vs ERQ paths with test fixtures.

### Phase 3 — v0.2 Cache, Index Maintenance, Metrics
- Status: DONE — Implement `elax-cache` for NVMe+RAM slab management, prefetch, and eviction aligned with design.
- Status: DONE — Extend `elax-indexer` to materialize parts, manage compaction heuristics, and publish router epochs atomically.
- Status: DONE — Integrate `elax-metrics` (Prometheus/OpenTelemetry) into query nodes and indexers with baseline dashboards.
- Status: DONE — Ship `elax-cli` admin tooling for compaction, verification, and export workflows.

### Phase 4 — v0.3 Feature Expansion
- Status: TODO — Add regex index opt-in within `elax-fts` and grouped aggregation support in query planner.
- Status: TODO — Expand multi-language FTS configurations and runbooks documenting tokenizer/stemming options.
- Status: TODO — Publish operational runbooks in `docs/runbooks/` for cache pinning, backfill, and recall drift remediation.

### Cross-Cutting Workstreams
- Status: TODO — Set up CI jobs enforcing `cargo fmt --all`, `cargo clippy --all-targets --all-features -D warnings`, and `cargo test --workspace`.
- Status: TODO — Add property tests (e.g., `proptest`) for WAL ordering/recovery and ERQ distance estimates vs FP32 ground truth.
- Status: TODO — Keep `docs/design.md` and sample configs updated as features land; capture architecture impacts in PR templates.

## Progress Log (Phase 1)

- 2025-09-20 — Bootstrapped Cargo workspace with crate layout matching `docs/design.md`; README and developer onboarding runbook added.
- 2025-09-20 — Implemented Phase 1 filesystem-backed WAL (`elax-store`), in-memory FP32 search with strong consistency (`elax-core`), and HTTP API routes (`elax-api`); added integration test `crates/elax-api/tests/phase1_flow.rs`.
- 2025-09-20 — Query-node binary launches API server using env-configurable data root/bind address.

## Progress Log (Phase 2)

- 2025-09-21 — Added `elax-ivf` k-means++ trainer with centroid assignment, probe ordering, and `nprobe` heuristic plus unit tests; remaining work tracks integration into `elax-core` planner.
- 2025-09-22 — Wired IVF candidate search into `elax-core` with `ann_params`, automatic retraining on writes, and deterministic coverage test ensuring IVF prioritizes nearest cluster.
- 2025-09-23 — Added ERQ quantization crate, integrated IVF+ERQ ANN planner with FP32 fallback, and exposed recall debug endpoint with coverage tests.

### Outstanding Issues

- `cargo build` currently fails locally on user machine due to remaining compile errors after the `ObjectStore` removal; further diagnostics required once full crate graph compiles.
- Local environment for the agent lacks network access to crates.io, blocking dependency fetches (`anyhow`, `tokio`, etc.); prevents the agent from running `cargo build/test` internally.
- Workspace metadata commands (`cargo fmt`, `cargo test`, etc.) fail until the referenced `bin/query-node` crate is restored in the repository.
