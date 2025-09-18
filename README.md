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

3. Ensure the workspace compiles:

   ```bash
   cargo check
   ```

The codebase is intentionally skeletal: most functions signal `todo!()`-style
errors. The focus of this commit is to provide structure, documentation, and
clear guidance for future contributors.

## Additional Reading

The full system design – covering APIs, S3 layout, build pipeline, and search
algorithms – lives in [`docs/design.md`](docs/design.md).
