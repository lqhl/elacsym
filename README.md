# elacsym

Prototype workspace for the elacsym search engine described in `docs/design.md`.

## Workspace Layout

- `crates/`: reusable library crates (`elax-*`).
- `bin/`: binary crates such as the `query-node` service.
- `tests/`: integration and property test harnesses (placeholder).
- `docs/`: design documents, runbooks, and architecture notes.

## Prerequisites

- Rust toolchain `1.77` (or newer) via `rustup`.
- `cargo` with network access to crates.io (initial build will fetch dependencies).

## Build & Test Commands

All commands are run from the repository root.

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -D warnings
cargo build --all-targets
cargo test --workspace
RUST_LOG=debug cargo run --bin query-node
```

CI should mirror the sequence above to guarantee consistent formatting, linting, compilation, and tests.
