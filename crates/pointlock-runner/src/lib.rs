//! # pointlock-runner
//!
//! The execution face of Pointlock: the step state machine over the
//! control-flow vocabulary, verdict folding, checkpoint-driven resume with
//! repair alignment, and (next wave) the handler engine. Concrete
//! providers are injected by the assembly layer (`pointlock-cli`); the
//! entry points accept only [`FlowIR`], never strings (principles 1/2).
//!
//! Authoritative design documents:
//! - `docs/design/00-architecture-spine.md` §5 (error taxonomy and default
//!   dispositions), §6 (RunLog vocabulary, step state machine, verdict
//!   folding, checkpoint model, resume semantics)
//! - `docs/design/07-subflow-checkpoint-resume-repair.md` §1 (subflow
//!   call-by-value contract and scope isolation), §4 (resume — including
//!   the frame-precise fallback of §4.6) and §5 (alignment / offline
//!   re-judge / the `requiresConfirmation` gate)
//!
//! ## M2 scope (iron rule: shapes final, content narrowed, nothing silent)
//!
//! Executing since M2: `call` steps (call-by-value inbound/outbound schema
//! gates, `callFramePushed`/`Popped` frame events, call-step verdict =
//! callee flow verdict), `if` (strict-boolean cond; unselected branches
//! leave skipped span pairs), `foreach` (positional iteration frames,
//! `iter.<as>` scope), `let` (SSA `vars.*`), `assert` steps
//! (`observe: "fresh"` / `{ fromStep, which }`), and `preflight` probes
//! (probing phase before acting; failure → the step's `onResumeDrift`
//! ladder — repair subflow then re-probe, or escalate to a `repairWorld`
//! human — and only an exhausted ladder blocks
//! ([`RunOutcome::Blocked`])). The subflow registry travels through
//! [`RunOptions::subflows`] / [`Runner::resume_with_subflows`] and
//! self-verifies at load (`maxCallDepth = 8`, 07 §1.3).
//!
//! Since the human wave (M2-W2a): `human` steps execute with the unified
//! wait-as-suspension semantics — `humanRequested` (fsynced) →
//! `runSuspended` → [`RunOutcome::AwaitingHuman`]; the runner never blocks
//! waiting (attached-TTY inline collection is the CLI layer). Responses
//! are arbitrated by the store single writer
//! (`Store::submit_human_response`, first response wins) and settled on
//! resume through the four-mode verdict/output mapping (06 §2.2); expired
//! deadlines settle lazily to `unknown`, a pure function of `deadlineAtMs`
//! and response absence. R13 supervision
//! ([`RunOptions::supervise`]/[`ResumeOptions::supervise`], per segment,
//! never inherited) gates action-step dispatch strictly before the
//! `actionIntent` WAL: `proceed` dispatches, `abort` ends the run without
//! consulting any handler, `suspend` keeps the request pending across
//! segments.
//!
//! Narrowed content, each refusal typed (never skipped silently):
//! - Handlers execute (retry / continue / escalate / abort / repair, flow-
//!   and step-level, trigger-budgeted); the RE-INVOCATION dispositions on
//!   call/human hosts — 07 §1's attempt-framed full re-call and the
//!   fresh-request re-ask — remain typed refusals.
//! - Cross-IR resume classifies the whole 07 §5.2 nested vocabulary
//!   (`if` branch bodies, `foreach` rounds, the call down-drill of case
//!   (a), the case (b) frame teardown, order consistency); what remains
//!   refused is a resume across a LIVE hook frame or a pending handler
//!   escalation — hook-aware frame re-entry. Same-IR resume is
//!   frame-precise at any depth (07 §4.6).
//! - Evidence-localization failures during `observing` (fetch unsupported,
//!   stream rupture, integrity mismatch, `ui.snapshot.get` errors) never
//!   abort the run: the observation record keeps the field absent and the
//!   dependent verify channel receives a typed gap → honest `unknown`
//!   (principle 4).
//!
//! ## Threading
//!
//! The store is a synchronous single writer (`Connection` is `!Sync`);
//! the runner keeps all store use on the calling task. Async exists only
//! because the Provider SPI is async — a current-thread runtime (or
//! `block_on`) is sufficient.

mod align;
mod engine;
mod error;
mod judge;
mod load;
mod observe_eval;
mod runner;
mod scope;

pub use engine::RunOutcome;
pub use error::{BlockedReason, RunnerError};
pub use runner::{ResumeOptions, RunOptions, Runner};

// Re-exported for signature convenience (the entry points consume these).
pub use pointlock_ir::FlowIR;
pub use pointlock_provider_kit::CancellationToken;
