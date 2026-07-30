# Pointlock Documentation

This directory holds the authoritative design specification for Pointlock. The
documents are written to be read in order, but each is self-contained.

New here? Start with the [project README](../README.md) for the what and why,
then read **design 00** below.

## Reading order

1. **[Architecture spine (00)](design/00-architecture-spine.md)** — required
   reading. The highest-authority document: layering, the 13 core concepts, the
   Typed IR core types, the Provider SPI, the error taxonomy, the runner state
   machine, dual-hash resume alignment, the canonical vocabulary, and the
   requirements-traceability matrix.
2. Then, by role:
   - **Writing flows?** → [Authoring & compilation (03)](design/03-authoring-and-compilation.md)
   - **Building a provider?** → [Provider SPI & DeviceRail (04)](design/04-provider-interface-and-devicerail.md) and [Playwright / HTTP / CLI (05)](design/05-providers-playwright-http-cli.md)
   - **Care about reliability semantics?** → [Subflow / checkpoint / resume / repair (07)](design/07-subflow-checkpoint-resume-repair.md)
   - **Implementing / consuming the UI?** → [UI & MVP (08)](design/08-ui-and-mvp.md)

## Index

| # | Document | Topic |
|---|---|---|
| — | [requirements.md](requirements.md) | 13 deliverables · 10 design principles · deviation register |
| 00 | [Architecture spine](design/00-architecture-spine.md) | **Highest authority.** Layering, core concepts, Typed IR, Provider SPI, error taxonomy, runner state machine, dual-hash resume, canonical vocabulary, requirements matrix |
| 01 | [Positioning & concepts](design/01-positioning-and-concepts.md) | Project positioning, naming evaluation, the 13 concepts' lifecycles, comparisons with LangGraph / Prefect / n8n / Temporal |
| 02 | [Typed IR](design/02-typed-ir.md) | IR v0.1 design: step structure, dual-hash domains, expression grammar, closed vocabulary, versioning. Normative schema: `../schema/flow-ir.v0.1.schema.json` |
| 03 | [Authoring & compilation](design/03-authoring-and-compilation.md) | Authoring format, Android/Web examples, the NL → YAML → IR → Runner pipeline, diagnostic codes |
| 04 | [Provider SPI & DeviceRail](design/04-provider-interface-and-devicerail.md) | Provider interface contract; the full DeviceRail mapping (negotiation, action translation, error codes, timeout budgets, reconcile) |
| 05 | [Playwright / HTTP / CLI providers](design/05-providers-playwright-http-cli.md) | Three extension providers + the two paths to the Web (v0.2+) |
| 06 | [Human & secure handlers](design/06-provider-human-and-secure-handlers.md) | Human-step modes, notification channels, the secret rule, the `handle_interactive_verification` handler family |
| 07 | [Subflow / checkpoint / resume / repair](design/07-subflow-checkpoint-resume-repair.md) | Subflow calls, RunPath failure location, the checkpoint model (SQLite/WAL), resume correctness conditions, the localized-repair alignment algorithm |
| 08 | [UI & MVP](design/08-ui-and-mvp.md) | `pointlock-ui` information architecture, the M0–M3 milestones, the v0.1 non-goals |

## Related

- [Runnable examples](../examples/) — `*.flow.yaml` demos.
- [`schema/`](../schema/) — generated JSON Schema and golden fixtures.
- [CONTRIBUTING](../CONTRIBUTING.md) — the code-generation and fixture workflow.
