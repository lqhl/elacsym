# elacsym

elacsym is a serverless, S3-backed vector search engine inspired by the Turbopuffer
architecture. The project targets a two-stage retrieval pipeline (IVF + RaBitQ for
candidate generation followed by int8/fp32 re-ranking) while operating entirely on
immutable parts stored in object storage.

## Repository Layout

- `Cargo.toml`: Rust workspace definition with dedicated crates per subsystem.
- `crates/`: Individual library crates that map to architectural components
  such as the HTTP API, storage abstraction, manifest management, quantisation,
  and background compaction.
- `docs/design.md`: Canonical design document captured from the initial product
  specification.
- `PLAN.md`: Rolling plan that tracks major steps completed during bootstrap.
- `AGENTS.md`: Developer instructions, coding conventions, and required checks.

## Getting Started

1. Install Rust (1.75 or newer recommended).
2. Fetch workspace dependencies:

   ```bash
   cargo fetch
   ```

3. Validate the workspace builds and the core ingest/search workflows pass their tests:

   ```bash
   cargo check
   cargo test --workspace
   ```

The ingest pipeline can materialise IVF/RaBitQ/int8 artefacts on disk, and the
search crate executes candidate generation plus reranking over those artefacts.

## Testing

Continuous integration runs formatting, compilation, and the full test suite.
To exercise the most important flows locally:

- Verify the ingest pipeline writes the expected artefacts:

  ```bash
  cargo test --package part_builder build_part_small_batch
  ```

- Check multi-part search, including candidate merging, using the IVF/RaBitQ stack:

  ```bash
  cargo test --package index search_merges_candidates_across_parts
  ```

- Confirm the reranking stage respects fp32 precision caps:

  ```bash
  cargo test --package index fp32_rerank_matches_exact_dot_product
  ```

## Additional Reading

The full system design – covering APIs, S3 layout, build pipeline, and search
algorithms – lives in [`docs/design.md`](docs/design.md).
