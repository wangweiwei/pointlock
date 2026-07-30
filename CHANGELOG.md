# Changelog

All notable changes to Pointlock are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

_Work toward v0.2: public Provider SPI (sidecar), Playwright / HTTP / CLI
providers, repair-write UI, checkpoint materialization cadence._

## [0.1.4] — 2026-07-30

## [0.1.3] — 2026-07-30

## [0.1.2] — 2026-07-30

## [0.1.1] — 2026-07-30

## [0.1.0] — 2026-07-19

The first end-to-end line: author → compile → run → resume → repair → inspect,
with DeviceRail as the first provider. Milestones **M0 through M3a**. 437 tests
passing.

### Added

**Core (M0–M1)**
- `pointlock-ir` Typed IR with dual-hash identity and a versioned, content-addressed flow shape; JSON Schema and TypeScript types generated from the Rust DTOs.
- `pointlock-expr` expression language shared by compiler and runner.
- `pointlock-compiler`: the five-stage pipeline (parse → normalize → check → bind → seal), dual chains, element verbs, static predicates, capability-bound binding, and sealed artifacts.
- `pointlock-store`: event-sourced RunLog, SQLite/WAL checkpoints, an evidence store, and the `verify_checkpoint` invariant.
- `pointlock-runner`: the execution state machine, verdict fold, WAL-before-act intent, `callId` reconcile, and dual-hash resume alignment.
- `pointlock-provider-kit` Provider SPI and `pointlock-provider-devicerail`, the full DeviceRail SPI implementation with real-daemon E2E, attestation-drift refusal, and offline re-judging.
- `pointlock-cli`: `lock`, `compile`, `run`, `resume`, `inspect`.

**Human, control flow & authoring (M2)**
- Control flow: frame-stack interpreter, subflows as first-class citizens, and `RunPath` failure location.
- Human subsystem and supervised runs (`run --supervise <mutating|all>`), with `pointlock-human-cli` as the interaction channel.
- Handler engine — four hooks, five dispositions — including the verification-handler family.
- Compiler full authoring surface; bundle artifacts (sealed IR + binding closure).
- `@pointlock/nl-drafter` — natural-language → YAML drafting with elicitation.

**Read side, repair & vision (M3a)**
- Projection protocol: five read-only DTO families (`FlowGraphView`, `RunTimelineEntry`, `StepDossierView`, `HumanInboxEntry`, `RunOverview`), independently versioned, with a query layer and `locate`.
- `pointlock inspect --serve` projection host + `@pointlock/projection-types`.
- `@pointlock/ui` — the v0.1 read-only projection console (React Flow graph, timeline, adjudicable dossier).
- Repair closure: alignment preview, repair endpoints, and UI.
- `pointlock-vision` — the Anthropic-backed visual verifier (downgrade-only).
- `pointlock report` — the run report command surface.
- Observation viewport; a settlement-evidence manifest; per-attempt chain-position join; `providerStateSummary`; `sessionLineage` per segment.

**Post-audit hardening (2026-07-28, pre-release)**
- Authoring surface completed: step-level `expect_schema` narrowing of invoke outputs with C7 enforcement (`RF3020` — narrow before you dereference), the `value:` assertion sugar, string-interpolation desugaring to `concat(...)`, and `pointlock compile --emit-authoring-schema` (the closed-keyword-table vocabulary document the NL drafter consumes).
- Human subsystem completed: the webhook notify-only channel (`--webhook-url` + `POINTLOCK_WEBHOOK_SECRET`, HMAC-signed `pointlockWebhook: 1` envelopes), OS-identity actor attribution (`cli:os:<user>@<host>`), canonical settlement-evidence documents for hook escalations, and declared-first `repairWorld` vocabulary in `pointlock-human-cli`.
- R13 repair-proposal loop closed: `@pointlock/nl-drafter` `proposeRepair` (dossier → minimal revision + unified diff) and the CLI approval gate `pointlock resume --preview` (read-only alignment rehearsal; approve by rerunning without the flag).
- `verdict.record.v1` chain: compiled into every IR's `requiredFeatures`, capability-drift on un-negotiated sessions, and write-back failures annotated (`remote archival failed`) instead of aborting the run; ledger keeps full verdict summaries with a content-hash pointer on the truncated wire copy.
- Resume honesty markers end to end: `unprobed`, frame `rebase`, `handlerTriggered.disposition`, evidence-gallery provenance, act-chain runtime marks, `degradedVerify`, provider-state forensics, and the flow list's current-lockfile staleness flag (`inspect --serve --lockfile`).
- Subflow expansion in the UI (lazy callee graphs by irHash, composed runtime anchors) and the bundle-pool graph route.
- Provider conformance suite completed to the 04 §8 assertion groups (no-autonomous-retry, unattested-action refusal, omission-is-data).
- Milestone acceptance demos: `examples/m0-hello/`, `examples/m1-login/`, `examples/m2-checkout/`.
- `@pointlock/ir-types` generation pipeline (FlowIR schema → TypeScript, fixture-anchored) and `@pointlock/walk-drafter` registered into the spine A.1 roster (grounded drafting; unpublished in v0.1).

### Notes

- Each milestone was hardened with an adversarial review-and-fix loop; the
  incorporation-review batch landed four additive design rulings and closed
  three pre-existing defects.
- `pointlock-provider-devicerail` consumes `devicerail-client` from crates.io
  when published; local development resolves it from the sibling checkout.

[Unreleased]: https://github.com/wangweiwei/pointlock/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/wangweiwei/pointlock/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/wangweiwei/pointlock/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/wangweiwei/pointlock/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/wangweiwei/pointlock/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/wangweiwei/pointlock/releases/tag/v0.1.0
