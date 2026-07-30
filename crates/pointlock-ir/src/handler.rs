//! Handlers and retry policies (02 §10).
//!
//! Handlers are explicit strategies mounted on state-machine hooks; they
//! yield dispositions, never data-flow outputs (spine R10). Retry mounts at
//! exactly two places: `StepBase.retry` (act phase, same attempt, new
//! callId) and `HandlerAction::Retry` (whole-step re-entry from `acting`,
//! independent budget) — there is structurally no third place.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::flow::FlowRef;
use crate::step::HumanStepIR;
use crate::vocab::{ErrorClass, HandlerHook};

/// A handler mounted on a hook.
///
/// The `errorClasses` filter is legal only on `onError` — the baseline
/// `if`/`else` conditional is reproduced via `#[schemars(extend)]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(extend(
    "if" = { "properties": { "hook": { "const": "onError" } }, "required": ["hook"] },
    "else" = { "not": { "required": ["errorClasses"] } }
))]
pub struct HandlerBinding {
    /// The hook this handler fires on.
    pub hook: HandlerHook,
    /// Error-class filter — only meaningful (and only legal) on `onError`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub error_classes: Option<BTreeSet<ErrorClass>>,
    /// What to do when the hook fires.
    pub action: HandlerAction,
    /// Trigger budget (loop guard).
    #[schemars(range(min = 1))]
    pub max_triggers: u32,
}

/// The disposition produced by a handler; `kind` values are the closed
/// `Disposition` enum: `retry | continue | escalate | abort | repair`
/// (spine A.4). No variant carries outputs — error paths cannot enter the
/// data flow (spine R10).
/// Note on closedness: `#[schemars(deny_unknown_fields)]` closes each
/// variant object in the generated schema (baseline parity); serde's
/// internally-tagged deserialization is lenient about unknown fields at
/// runtime — the schema stays authoritative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub enum HandlerAction {
    /// Re-enter the step from `acting` with an independent retry budget.
    Retry {
        /// The retry policy for the re-entry.
        policy: RetryPolicy,
    },
    /// Record and move on (verdict unchanged).
    Continue,
    /// Escalate to a human node (compiler-synthesized stepId, 02 §3).
    Escalate {
        /// The embedded human step (boxed: much larger than sibling variants).
        human: Box<HumanStepIR>,
    },
    /// Abort the run.
    Abort,
    /// Run a repair subflow — no data outputs; afterwards re-probe
    /// (`onResumeDrift`) or re-enter (`onFail`).
    #[serde(rename_all = "camelCase")]
    Repair {
        /// The repair subflow, pinned like a `call`.
        flow_ref: FlowRef,
    },
}

/// Retry policy. Applies to the act phase only (spine §6.5 mount point 1);
/// each retry mints a new `callId` and a new `actionIntent` WAL record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetryPolicy {
    /// Attempt budget (≥ 1).
    #[schemars(range(min = 1))]
    pub max_attempts: u32,
    /// Backoff: fixed milliseconds or an exponential schedule.
    pub backoff_ms: BackoffMs,
    /// Which error classes are retryable here. Semantically meaningful:
    /// `action_failed_retryable`, `target_stale` (forces re-observe), and —
    /// for idempotent steps — `action_timed_out`; `check` warns on the rest.
    /// Set semantics — serialized in [`ErrorClass`] declaration order.
    #[schemars(length(min = 1))]
    pub retry_on: BTreeSet<ErrorClass>,
}

/// Backoff declaration: a plain number of milliseconds, or an exponential
/// schedule. Numbers use `serde_json::Number` so integers round-trip
/// canonically (02 §12.1 rule 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum BackoffMs {
    /// Fixed backoff in milliseconds (≥ 0).
    Fixed(#[schemars(range(min = 0))] serde_json::Number),
    /// Exponential schedule.
    Schedule(BackoffSchedule),
}

/// Exponential backoff schedule (inline object in the baseline).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct BackoffSchedule {
    /// Initial delay in milliseconds (≥ 0).
    #[schemars(range(min = 0))]
    pub initial: serde_json::Number,
    /// Multiplication factor (≥ 1).
    #[schemars(range(min = 1))]
    pub factor: serde_json::Number,
    /// Delay ceiling in milliseconds (≥ 0).
    #[schemars(range(min = 0))]
    pub max: serde_json::Number,
}
