# Agent Guidelines for `elacsym`

## Scope
This file applies to the entire repository.

## Development Workflow
- Keep `PLAN.md` up to date. Add new tasks when you scope work and mark them as
  progress is made.
- Follow the workspace layout that mirrors the design document. New runtime
  features should live in an existing crate when possible; create additional
  crates only if the design explicitly calls for a separation.

## Coding Standards
- Use Rust 2021 edition idioms. Prefer explicit error types over panics.
- For placeholders or unimplemented sections, bubble up an error with
  `anyhow::bail!` or the shared `common::Error` rather than calling `todo!()`.
- Document public functions with brief `///` comments explaining their role.
- When touching code that interacts with S3 paths or manifest schemas, cross
  check against `docs/design.md` to keep the repository consistent with the
  design.

## Required Tooling
Run the following before submitting changes:

```bash
cargo fmt
cargo check
```

Add additional checks (e.g., `cargo clippy`, tests, or integration harnesses)
whenever the touched code would benefit from them.

## Documentation
- Keep `README.md` aligned with the current state of the project.
- Update `docs/design.md` if the high-level architecture diverges from the
  captured design.
