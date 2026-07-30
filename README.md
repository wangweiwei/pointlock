<div align="center">

<img src="assets/logo.svg" alt="Pointlock logo" width="150">

<h1 align="center">Pointlock</h1>

<p align="center"><strong>Compile natural-language tasks into a typed, capability-bound, crash-safe flow IR — and run them reliably on real devices, browsers, and APIs.</strong></p>

[![License: Apache 2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024_edition-orange.svg)](rust-toolchain.toml)
[![Node](https://img.shields.io/badge/node-%3E%3D22-3c873a.svg)](package.json)
[![Status](https://img.shields.io/badge/status-v0.1_M0--M3a-success.svg)](#roadmap)
[![Tests](https://img.shields.io/badge/tests-437%20passing-success.svg)](#testing)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](CONTRIBUTING.md)

**English** · [简体中文](README.zh-CN.md) · [Documentation](docs/README.md) · [Roadmap](#roadmap)

</div>

---

## What is Pointlock?

**Pointlock is a lightweight, open-source flow-orchestration engine that compiles a natural-language task into a strongly-typed, capability-bound intermediate representation (Typed IR) and executes it reliably** — with crash-safe resumption, localized repair, and evidence-backed `pass` / `fail` / `unknown` verdicts — across real devices, browsers, and APIs.

Most agent and RPA stacks decide *at runtime* whether a step is even possible, optimistically assume success when they can't tell, and lose their place when a process dies mid-action. Pointlock moves those decisions earlier and makes them durable: capabilities are checked **at compile time**, every mutating action is **write-ahead-logged before it fires**, and a verdict is never `pass` unless content-addressed evidence supports it.

> Pointlock is **not** part of **DeviceRail** — but DeviceRail is its first execution provider. The engine is provider-agnostic; DeviceRail (real Android/device automation) is simply the first backend wired in.

## Why Pointlock — three guarantees

| Guarantee | What it means | How it's enforced |
|---|---|---|
| 🧭 **Capability-bound compilation** | A missing capability is a **compile error**, not a runtime surprise. Compilation consumes a provider's capability **lockfile** — no device needs to be online. | `pointlock lock` freezes a provider's real capability catalog into a versioned lockfile; the compiler binds every action against it and *seals* the result (`irHash`, `lockfileDigest`). At run time, attestation is re-checked and **drift refuses to run**. |
| 🛟 **Crash-safe execution** | At *any* instant of a crash, restart can determine **"did that action actually happen or not?"** | Event-sourced **RunLog** + **write-ahead intent** before each act + `callId` **reconcile** on restart. No double-fire, no lost outcome. |
| 🔬 **Evidence-backed tri-state verdicts** | `pass` / `fail` / `unknown` — never an optimistic `pass` when the truth can't be confirmed. | Actions, assertions, and verdicts are separated; every verdict cites **content-addressed evidence**; visual checks may only *downgrade* confidence, never fabricate a pass. |

## Quick start

> **Prerequisites:** [Rust](https://rustup.rs) (2024 edition, stable) and [Node ≥ 22](https://nodejs.org) + [pnpm](https://pnpm.io) (for the UI). See [Building from source](#building-from-source) for the DeviceRail provider note.

```bash
# 1. Freeze a provider's capabilities into a lockfile (offline; uses the built-in fake here)
cargo run -p pointlock-cli -- lock --provider fake --out fake.lock.json

# 2. Compile a YAML task into a sealed, capability-bound FlowIR
cargo run -p pointlock-cli -- compile examples/wifi-demo.flow.yaml \
  --lockfile fake.lock.json --out demo.ir.json

# 3. Run it — stop early to simulate a crash/suspension (exit code 3 = suspended)
cargo run -p pointlock-cli -- run demo.ir.json --store .runs --stop-after set_ssid

# 4. Resume from checkpoint — alignment replays only what wasn't durably completed
cargo run -p pointlock-cli -- resume demo.ir.json --store .runs --run <run-id>

# 5. Inspect the ledger, checkpoint, and verdict dossier
cargo run -p pointlock-cli -- inspect --store .runs --run <run-id> --rebuild-checkpoint

# …or open the read-only projection console in a browser
cargo run -p pointlock-cli -- inspect --store .runs --serve
```

## How it works

YAML is the **authoring surface**, not the execution protocol. The runner only ever executes Typed IR; the compiler and runner never depend on each other.

```
  YAML (authoring surface)
    │  pointlock-compiler:  parse → normalize → check → bind → seal
    ▼
  FlowIR  (irHash · dual-hash domains · lockfileDigest)         ← the durable contract
    │
    ▼
  pointlock-runner ── state machine · verdict fold · resume alignment · supervision gate
    ├── pointlock-store             RunLog / Checkpoint / Evidence   (SQLite + WAL)
    └── pointlock-provider-kit      Provider SPI (Rust trait)
          └── pointlock-provider-devicerail → devicerail-client → daemon
  shared kernel: pointlock-ir + pointlock-expr
  read side:     projection protocol → @pointlock/projection-types → @pointlock/ui (first renderer)
```

1. **Author** a task as `*.flow.yaml` (or draft it from natural language with `@pointlock/nl-drafter`).
2. **Lock** a provider's capabilities offline (`pointlock lock`).
3. **Compile** — capabilities are bound and the IR is sealed with a hash. Missing capability ⇒ compile error.
4. **Run** — every mutating step is WAL-logged before dispatch; verdicts fold into a checkpoint.
5. **Resume / repair** — on crash or partial failure, alignment reconciles the ledger and replays only what is unproven.
6. **Inspect** — a renderer-agnostic read-only **projection protocol** feeds a React-based console, a `report`, or your own UI.

## Features

- **Typed IR with dual-hash identity** — content-addressed flows; deterministic, versioned, diff-able.
- **Capability lockfiles** — reproducible, offline compilation against a provider's real catalog.
- **Event-sourced RunLog + SQLite/WAL checkpoints** — a `verify_checkpoint` invariant proves the materialized view equals a from-scratch refold.
- **`callId` reconcile** — exactly-once reasoning about in-flight actions across crashes.
- **Tri-state, evidence-backed verdicts** — `pass` / `fail` / `unknown`, content-addressed evidence, offline re-judging.
- **Localized repair** — re-run a single failed step or sub-chain without redoing proven work.
- **Human-in-the-loop as a first-class node** — supervised runs (`run --supervise <mutating|all>`) gate each mutating step through the same durable channel as human steps.
- **Subflows as first-class citizens** — `RunPath` locates failures precisely inside nested calls.
- **Renderer-agnostic projection protocol** — five read-only DTO families (`FlowGraphView` / `RunTimelineEntry` / `StepDossierView` / `HumanInboxEntry` / `RunOverview`), independently versioned; HTTP+JSON canonical, SSE optional, in-process equivalent.
- **Single static binary** — the CLI ships as one self-contained executable.

## How Pointlock compares

| | **Pointlock** | LangGraph | Temporal | n8n / Zapier | Prefect / Airflow |
|---|:---:|:---:|:---:|:---:|:---:|
| Primary domain | Real device / browser / API task automation | LLM agent graphs | Durable microservice workflows | SaaS app integrations | Data pipelines |
| Capability checking | **Compile-time (lockfile-bound)** | Runtime | Runtime | Runtime | Runtime |
| Crash recovery | **Event-sourced + WAL intent + reconcile** | App-defined | Event-sourced (server) | Limited | Task-level retries |
| Verdict model | **Tri-state + content-addressed evidence** | Free-form | App-defined | Success/fail | Success/fail |
| Localized repair | **Yes — single step / sub-chain** | Re-run graph | Replay | Re-run | Re-run task |
| Footprint | **Single binary + SQLite** | Library | Server cluster | Hosted/Server | Server + DB |
| Authoring | YAML → Typed IR (or NL draft) | Python | Code (Go/Java/…) | Visual editor | Python |

Pointlock is deliberately **lightweight** (single process, SQLite — no Temporal cluster) and optimized for *distribution surface and type rigidity*, not raw throughput (the runner is I/O-bound).

## FAQ

**Is Pointlock production-ready?**
Pointlock is at **v0.1** — the M0–M3a milestones are complete (compiler, runner, store, DeviceRail provider, human/supervision subsystem, vision verifier, projection protocol, read-only UI, and the `report`/`inspect` command surface), with 437 tests passing. The public SPI and multi-provider surface (Playwright / HTTP / CLI providers) are designed but land in v0.2. Treat it as an early, well-tested foundation rather than a 1.0.

**How is it different from an LLM agent framework?**
Agent frameworks decide what's possible while they run and often assume success. Pointlock *compiles* a plan against a provider's frozen capabilities before anything executes, refuses to run on capability drift, and never records `pass` without evidence. Natural language is an authoring convenience (`@pointlock/nl-drafter`), not the execution substrate.

**Do I need a device connected to compile a flow?**
No. Compilation binds against a **capability lockfile** produced offline by `pointlock lock`. Devices are only needed at run time — where attestation is re-verified.

**What happens if the process crashes mid-action?**
Every mutating action writes its intent ahead of dispatch. On restart, `callId` reconcile determines whether the action actually happened, so the run neither double-fires nor silently loses an outcome — it resumes from a consistent checkpoint.

**Which providers are supported today?**
**DeviceRail** (real device/Android automation) is the first and only wired provider in v0.1. The Provider SPI is a stable Rust trait; Playwright, HTTP, and CLI providers are specified in the [design docs](docs/design/05-providers-playwright-http-cli.md) for v0.2.

**Why Rust + TypeScript?**
The type-truth source is the `pointlock-ir` Rust DTOs (serde + schemars); the JSON Schema, the `@pointlock/projection-types` TypeScript types, and golden fixtures are all generated from them. Rust is chosen for distribution surface and type rigidity; the TypeScript UI is the first renderer over the projection protocol.

## Project layout

```
pointlock/
├── crates/                     # Rust core (Cargo workspace, 10 crates)
│   ├── pointlock-ir/            #   Typed IR — the source of type truth (serde + schemars)
│   ├── pointlock-expr/          #   expression language shared by compiler & runner
│   ├── pointlock-compiler/      #   YAML → normalize → check → bind → seal
│   ├── pointlock-provider-kit/  #   Provider SPI (Rust trait)
│   ├── pointlock-store/         #   RunLog / Checkpoint / Evidence + projections (rusqlite)
│   ├── pointlock-runner/        #   state machine, verdict fold, resume alignment
│   ├── pointlock-provider-devicerail/  # first provider (→ devicerail-client → daemon)
│   ├── pointlock-vision/        #   Anthropic-backed visual verifier (downgrade-only)
│   ├── pointlock-human-cli/     #   human-step / supervision interaction channel
│   └── pointlock-cli/           #   the single static binary
├── packages/                   # TypeScript workspace (pnpm)
│   ├── ir-types/               #   IR types (type-only package)
│   ├── projection-types/       #   generated projection DTOs (the UI's only contract)
│   ├── nl-drafter/             #   natural-language → YAML drafting
│   ├── walk-drafter/           #   grounded drafting: drive live pages → *.flow.yaml drafts (unpublished)
│   └── ui/                     #   @pointlock/ui — the v0.1 read-only projection console
├── schema/                     # generated JSON Schema + golden fixtures
├── docs/                       # design documents (see docs/README.md)
├── examples/                   # runnable *.flow.yaml demos
└── assets/                     # brand assets (logo, icon)
```

## Documentation

Start with the **[documentation index](docs/README.md)**. The design series is the authoritative specification:

| # | Document | Topic |
|---|---|---|
| — | [Requirements](docs/requirements.md) | 13 deliverables · 10 design principles · deviation register |
| 00 | [Architecture spine](docs/design/00-architecture-spine.md) | **Highest authority** — layering, 13 core concepts, Typed IR, Provider SPI, error taxonomy, runner state machine, dual-hash resume, canonical vocabulary |
| 01 | [Positioning & concepts](docs/design/01-positioning-and-concepts.md) | project positioning, naming, concept lifecycles, comparisons |
| 02 | [Typed IR](docs/design/02-typed-ir.md) | IR v0.1 design; the normative schema is [`schema/flow-ir.v0.1.schema.json`](schema/) |
| 03 | [Authoring & compilation](docs/design/03-authoring-and-compilation.md) | YAML surface, examples, the five-stage compile pipeline, diagnostics |
| 04 | [Provider SPI & DeviceRail](docs/design/04-provider-interface-and-devicerail.md) | provider contract + the full DeviceRail mapping |
| 05 | [Playwright / HTTP / CLI providers](docs/design/05-providers-playwright-http-cli.md) | three extension providers (v0.2+) |
| 06 | [Human & secure handlers](docs/design/06-provider-human-and-secure-handlers.md) | human steps, notification channels, the secret rule, verification handlers |
| 07 | [Subflow / checkpoint / resume / repair](docs/design/07-subflow-checkpoint-resume-repair.md) | execution semantics, checkpoint model, resume correctness, localized repair |
| 08 | [UI & MVP](docs/design/08-ui-and-mvp.md) | projection console, M0–M3 milestones, v0.1 non-goals |

## Roadmap

- ✅ **v0.1 (M0 → M3a)** — Typed IR, capability-bound compiler, crash-safe runner + store, DeviceRail provider, human/supervision subsystem, vision verifier, projection protocol, read-only UI, `report` / `inspect` / `locate`.
- 🔜 **v0.2** — public Provider SPI (sidecar), Playwright / HTTP / CLI providers, repair-write UI, checkpoint materialization cadence, `pointlock report` extensions.
- 🧭 **Beyond** — multi-provider flows, secrets subsystem, web-UI authoring.

See the deviation register in [requirements](docs/requirements.md) and the milestone definitions in [design 08](docs/design/08-ui-and-mvp.md).

## Building from source

```bash
# Rust core
cargo build --workspace
cargo test  --workspace

# TypeScript UI + generated types
pnpm install
pnpm -r build
pnpm -r test
```

> **DeviceRail provider note.** `pointlock-provider-devicerail` depends on `devicerail-client`, currently referenced as a **sibling path checkout** (`../device-rail/crates/client`) until it is published to crates.io. To build the full workspace you need the DeviceRail repository checked out alongside this one. This is tracked in [`Cargo.toml`](Cargo.toml) and the [design docs](docs/design/04-provider-interface-and-devicerail.md).

## Testing

The Rust workspace ships **437 passing tests** (unit, integration, and conformance), including the `verify_checkpoint` invariant (materialized view ≡ from-scratch refold) and a provider conformance suite. Generated artifacts (JSON Schema, `@pointlock/projection-types`, golden fixtures) are regenerated and verified in CI. See [CONTRIBUTING](CONTRIBUTING.md) for the regeneration and fixture-blessing workflow.

## Contributing

Contributions are welcome. Please read:

- [**CONTRIBUTING.md**](CONTRIBUTING.md) — dev setup, toolchain, code-generation & fixture workflow, PR flow
- [**CODE_OF_CONDUCT.md**](CODE_OF_CONDUCT.md) — our community standards (Contributor Covenant)
- [**SECURITY.md**](SECURITY.md) — how to report vulnerabilities
- [**CHANGELOG.md**](CHANGELOG.md) — release history

## License

Licensed under the [Apache License 2.0](LICENSE).

## Acknowledgements

Pointlock's design was produced through a multi-agent review process (competing skeletons → a synthesized ruling → parallel chapter refinement → adversarial cross-checks with all critical/major findings fixed), and hardened milestone-by-milestone with an adversarial review-and-fix loop after every increment.

<div align="center">
<sub>Built with Rust and TypeScript · <a href="docs/README.md">Docs</a> · <a href="CONTRIBUTING.md">Contribute</a> · <a href="README.zh-CN.md">中文</a></sub>
</div>
