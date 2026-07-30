# Contributing to Pointlock

Thanks for your interest in Pointlock! This guide covers how to set up a dev
environment, the code-generation workflow, and how to propose changes.

By participating you agree to abide by our [Code of Conduct](CODE_OF_CONDUCT.md).

## Ways to contribute

- 🐛 **Report a bug** — open a [bug report](.github/ISSUE_TEMPLATE/bug_report.yml).
- 💡 **Propose a feature** — open a [feature request](.github/ISSUE_TEMPLATE/feature_request.yml).
  For anything touching the Typed IR, the Provider SPI, or the projection
  protocol, please open a discussion/issue **before** a PR — these surfaces are
  design-governed (see below).
- 📖 **Improve docs** — corrections and clarifications are always welcome.
- 🔧 **Send a pull request** — see the workflow below.

## Prerequisites

| Tool | Version | Purpose |
|---|---|---|
| [Rust](https://rustup.rs) | stable, 2024 edition | core crates |
| [Node.js](https://nodejs.org) | ≥ 22 | TypeScript UI & generated types |
| [pnpm](https://pnpm.io) | 10.x | JS workspace package manager |

> **DeviceRail provider.** `pointlock-provider-devicerail` depends on
> `devicerail-client`, referenced today as a sibling path checkout
> (`../device-rail/crates/client`) until it is published to crates.io.
> A full-workspace build needs the DeviceRail repo checked out alongside this
> one. The provider-agnostic core (`pointlock-ir`, `pointlock-expr`,
> `pointlock-compiler`, `pointlock-store`, `pointlock-runner`) is where most
> contributions land.

## Build & test

```bash
# Rust core
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check

# TypeScript workspace
pnpm install
pnpm -r build
pnpm -r test
```

## Code generation

The **source of type truth is the `pointlock-ir` Rust DTOs** (serde + schemars).
JSON Schema, the `@pointlock/projection-types` TypeScript types, and golden
fixtures are all generated from Rust — never hand-edit generated files. After
changing a DTO or a projection shape, regenerate and commit the artifacts:

```bash
# 1. Regenerate JSON Schema from the Rust DTOs
cargo run -p pointlock-ir    --bin pointlock-ir-schema-gen
cargo run -p pointlock-store --bin pointlock-projection-schema-gen

# 2. Regenerate the TypeScript types from the schemas
pnpm --filter @pointlock/ir-types generate
pnpm --filter @pointlock/projection-types generate

# 3. Re-bless golden projection fixtures (only when the change is intended)
POINTLOCK_BLESS_PROJECTION=1 cargo test -p pointlock-store
```

CI verifies that committed generated artifacts match what the generators
produce, so run the steps above before pushing.

## Design discipline

Pointlock follows a **design-before-implementation** discipline. The
[design documents](docs/README.md) are the authoritative specification, with
[`docs/design/00-architecture-spine.md`](docs/design/00-architecture-spine.md)
as the highest authority. Changes to the Typed IR, the Provider SPI, the event
vocabulary, or the projection protocol should be reflected in the relevant
design doc in the same PR (or a preceding one). Deviations are either fixed or
registered in the deviation table — never left silent.

## Pull request workflow

1. Fork and create a branch off `main` (e.g. `feat/http-provider`, `fix/resume-cursor`).
2. Make your change with tests. Keep the working tree formatted and clippy-clean.
3. Use [Conventional Commits](https://www.conventionalcommits.org/) for messages,
   matching the existing history — e.g.
   `feat(runner): …`, `fix(store): …`, `docs: …`, `test(compiler): …`.
4. Ensure `cargo test --workspace`, `cargo clippy`, `cargo fmt --check`, and
   `pnpm -r test` all pass, and regenerated artifacts are committed.
5. Open a PR using the [template](.github/PULL_REQUEST_TEMPLATE.md); describe
   the change, link any issue, and note any design-doc updates.

## License

By contributing, you agree that your contributions will be licensed under the
[Apache License 2.0](LICENSE).
