# Security Policy

## Supported versions

Pointlock is at **v0.1** and pre-1.0; security fixes are applied to the latest
`main`. There is not yet a stream of maintained back-releases.

| Version | Supported |
|---|---|
| `main` (v0.1.x) | ✅ |
| older | ❌ |

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately via one of:

- **GitHub Security Advisories** — use *"Report a vulnerability"* under the
  repository's **Security** tab (preferred; keeps the report private and
  coordinated).
- **Email** — `dengfengwong@gmail.com`.

Please include:

- a description of the issue and its impact,
- steps to reproduce (a minimal `*.flow.yaml` / IR / store state if relevant),
- affected component (crate/package) and version/commit.

We aim to acknowledge reports within a few business days and will keep you
updated as we work on a fix. We'll credit you in the advisory unless you prefer
to remain anonymous.

## Security-relevant design

Pointlock's architecture has several properties worth knowing when assessing risk:

- **Capability-bound compilation & attestation.** Flows are bound against a
  provider capability lockfile and sealed with a hash; at run time attestation
  is re-verified and **drift refuses to run**.
- **Secrets handling.** Secret values follow a strict rule — they are not
  written to the RunLog, evidence store, or projections. See
  [design 06](docs/design/06-provider-human-and-secure-handlers.md).
- **Verification handlers.** CAPTCHA / graphical / SMS verification is **not
  bypassed by default** — it is surfaced to a human. See
  [design 06](docs/design/06-provider-human-and-secure-handlers.md).
- **Evidence is content-addressed**; verdicts never optimistically `pass`
  without supporting evidence.

If you find a way to defeat any of these properties (e.g. leak a secret into a
projection, run past attestation drift, or forge an evidence-backed `pass`),
that is in scope and we'd like to hear about it.
