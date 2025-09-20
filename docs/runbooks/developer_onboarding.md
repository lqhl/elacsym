# Developer Onboarding

This runbook covers the minimal steps required to bootstrap a local elacsym development environment.

## Prerequisites

- Install the Rust toolchain via `rustup` (MSRV: 1.77).
- Ensure `cargo` can reach crates.io to download workspace dependencies.
- Install `clang`/`llvm` if local SIMD builds will target AVX/NEON (optional for the placeholder workspace).

## Initial Setup

1. Clone the repository and change into the working directory.
2. Run `cargo fmt --all` to verify rustfmt is available.
3. Run `cargo build --all-targets` to compile every crate (first run fetches dependencies).
   - If sandboxing or offline operation blocks crates.io, prime a local registry mirror or request network access before continuing.
4. Run `cargo clippy --all-targets --all-features -D warnings` to confirm lint cleanliness.
5. Execute `cargo test --workspace` to ensure unit/integration suites pass.

## Local Iteration

- Launch the placeholder query node with `RUST_LOG=debug cargo run --bin query-node`.
- Use `cargo watch -x fmt -x check` during development for fast feedback (optional, requires `cargo-watch`).
- Update `AGENTS.md` and relevant docs when you add new features or adjust architecture assumptions.

## Troubleshooting

- **Dependency fetch failure**: confirm network egress to `https://index.crates.io`. In restricted environments, request a crates mirror or vendor dependencies.
- **Toolchain mismatch**: run `rustup override set 1.77.0` inside the workspace to lock the compiler version.
- **Clippy failures**: fix lint warnings locally; CI treats warnings as errors.

Keep this document synchronized with changes to build infrastructure, testing expectations, or onboarding gotchas.
