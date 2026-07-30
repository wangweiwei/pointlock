//! Durable execution records and the checkpoint materialization view.
//!
//! Definition home adjudicated to `pointlock-ir` (type truth source, R12) so
//! `pointlock-store` and `pointlock-runner` share one shape without violating
//! the dependency direction; pending spine batch incorporation.
//!
//! Shapes follow spine §6.6 (`CheckpointView`/`StepRecord`), §6.7-A
//! (`AlignmentReport`), and 07 §3.2 (`CallFrame` refinement,
//! `humanPending.purpose`).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::primitives::{ActionName, AssertId, FlowId, Hash, StepId};
use crate::run_path::RunPath;
use crate::runtime::{EvidenceRef, StepVerdict, Viewport};
use crate::vocab::{
    ActChannel, AlignmentClass, Channel, CoordinateFallbackReason, ErrorClass, ExecutionMode,
    HumanMode, HumanPurpose, ScreenshotOmissionReason, StepState, UiSnapshotOmissionReason,
    VerdictStatus,
};

/// Discriminant-only projection of an `ActionOutcome` for durable attempt
/// records (the full outcome lives in the RunLog `actionSettled` payload).
/// Pending incorporation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ActionOutcomeKind {
    /// Completed with a result.
    Succeeded,
    /// Failed with a structured error.
    Failed,
    /// Cooperatively cancelled.
    Cancelled,
    /// Action budget elapsed.
    TimedOut,
}

/// One attempt of an action step (spine §6.6): every dispatch, successful
/// or not, leaves one record keyed by its WAL callId.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptRecord {
    /// The caller-generated action id (matches the `actionIntent` WAL entry).
    pub call_id: String,
    /// Four-way terminal discriminant.
    pub outcome: ActionOutcomeKind,
    /// Runner-side error classification, for non-succeeded outcomes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<ErrorClass>,
    /// The execution mode the provider reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<ExecutionMode>,
    /// Fallback reason when `executionMode` is `coordinateFallback`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<CoordinateFallbackReason>,
    /// 1-based `binding.attempts` position of the dispatch (2026-07-18
    /// incorporation, item ② — carried from the `actionIntent` event;
    /// absent on pre-incorporation ledgers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_index: Option<u32>,
    /// The dispatched attempt's locating channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<ActChannel>,
    /// The dispatched attempt's provider-native action name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_name: Option<ActionName>,
}

/// The provider-side session profile captured at a failure or suspension
/// instant (07 §2.2, incorporated 2026-07-18): pure forensic material for
/// the dossier/overview — resume reconcile, the fold's binding provenance
/// and alignment never consume it. Rides `stepExited` (fail/unknown
/// verdict in force at exit) and `runSuspended` as an additive optional
/// payload field; absent on ledgers recorded before incorporation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderStateSummary {
    /// Known session generations, best-effort: the checkpoint-known
    /// lineage plus the live session observed at capture.
    pub session_lineage: Vec<String>,
    /// The capture-instant cursor (`currentCursor()` RPC). Absent when
    /// the call failed — never a stale bind-time value (principle 4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_cursor: Option<EventCursor>,
    /// The attested capability baseline at capture.
    pub attestation: AttestationSnapshot,
    /// `ProviderSession::health()` at capture; a failed call records
    /// `{ ok: false, degraded: "<errorClass>" }`.
    pub health: SessionHealthSnapshot,
    /// The bound device.
    pub device_id: String,
    /// `lockfile.device.platform`; absent when the assembly layer did
    /// not supply it (the SPI attestation does not carry it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

/// The two attestation facts 07 §2.2 pins into the summary (deliberately
/// not the full `CapabilityAttestation`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttestationSnapshot {
    /// Digest of the attested lockfile.
    pub lockfile_digest: Hash,
    /// Attestation timestamp, verbatim.
    pub attested_at: String,
}

/// Wire twin of the provider-kit `SessionHealth` (defined here because
/// provider-kit depends on ir, not vice versa).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionHealthSnapshot {
    /// Whether the session answered healthy.
    pub ok: bool,
    /// Degradation note (an error class when the health call failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
}

/// A localized observation record (spine §6.6; omissions are typed data).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservationRecord {
    /// Provider-scoped observation id.
    pub observation_id: String,
    /// Capture timestamp (ms since epoch).
    pub captured_at_ms: u64,
    /// The capture-time viewport, verbatim from the provider observation
    /// (M3a additive close of the registered W1 gap; absent on ledgers
    /// recorded before the field existed, or when the provider reported
    /// a non-finite scale factor — the runner refuses to persist a value
    /// serde_json would write as `null` and never read back).
    ///
    /// Compat note: the additive claim is one-directional — readers
    /// built before this field existed reject ledgers containing it
    /// (`deny_unknown_fields`, fail-closed per R12); binary downgrade
    /// against a store written by a newer runner is unsupported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<Viewport>,
    /// Localized screenshot evidence, when captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<EvidenceRef>,
    /// Why the screenshot was legitimately omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_omission: Option<ScreenshotOmissionReason>,
    /// Localized UI-tree evidence, when captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_snapshot: Option<EvidenceRef>,
    /// Why the UI snapshot was legitimately omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_snapshot_omission: Option<UiSnapshotOmissionReason>,
}

/// Outcome of one assertion evaluation along its verify chain (spine §6.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssertionOutcomeRecord {
    /// The assertion's id.
    pub assert_id: AssertId,
    /// Three-valued result (a chain that ran dry is `unknown`, never `fail`).
    pub result: VerdictStatus,
    /// The channel that completed the evaluation, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
    /// Human-readable evaluation reason.
    pub reason: String,
}

/// The durable record of one completed step (spine §6.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepRecord {
    /// Full run path of this step instance.
    pub run_path: RunPath,
    /// The step's stable id.
    pub step_id: StepId,
    /// Effect-domain hash at execution time (alignment input).
    pub effect_hash: Hash,
    /// Judge-domain hash at execution time (alignment input).
    pub judge_hash: Hash,
    /// Every dispatch attempt, in order.
    pub attempts: Vec<AttemptRecord>,
    /// Input expressions evaluated once at `ready` and snapshotted;
    /// never re-evaluated on resume.
    pub resolved_inputs: Value,
    /// Projected step output, when the step declares outputs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// Localized observations.
    pub observations: Vec<ObservationRecord>,
    /// Localized evidence (content-addressed).
    pub evidence: Vec<EvidenceRef>,
    /// Assertion outcomes, in declaration order.
    pub assertion_outcomes: Vec<AssertionOutcomeRecord>,
    /// The folded verdict projection, absent for unasserted mutations (R4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<StepVerdict>,
}

/// One live `foreach` iteration inside a call frame (07 §3.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IterState {
    /// The iteration variable name (`foreach ... as`).
    #[serde(rename = "as")]
    pub var: String,
    /// Zero-based index of the current item.
    pub index: u64,
    /// Optional stable item key (keyed foreach is reserved, v0.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// One frame of the live call stack (07 §3.2 refinement; `frames[0]` is the
/// root flow frame). Pending spine incorporation of the refined fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallFrame {
    /// The flow executing in this frame.
    pub flow_id: FlowId,
    /// Content hash of that flow's IR.
    pub ir_hash: Hash,
    /// The host call step's id; absent for the root frame and for
    /// handler-repair launched frames (spine §9).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_step_id: Option<StepId>,
    /// Call-by-value input snapshot taken when the frame was pushed;
    /// never updated from a newer IR (07 §5.2).
    pub inputs_snapshot: Value,
    /// `let` bindings materialized in this frame (SSA, single assignment).
    pub vars: BTreeMap<String, Value>,
    /// Live `foreach` iterations, innermost last.
    pub iter_stack: Vec<IterState>,
    /// Index of the next body step to schedule in this frame.
    pub next_index: u64,
}

/// The WAL'd intent of a dispatched-but-unsettled action (spine §6.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingIntent {
    /// The caller-generated action id to reconcile against.
    pub call_id: String,
    /// The evaluated arguments as dispatched.
    pub args_snapshot: Value,
}

/// The execution frontier: the single in-flight step (spine §6.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Frontier {
    /// Run path of the in-flight step.
    pub run_path: RunPath,
    /// Its lifecycle state at materialization time.
    pub state: StepState,
    /// The hanging action intent, when crash-vulnerable work was in flight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_intent: Option<PendingIntent>,
}

/// A pending human interaction (spine §6.6 + R13 purpose discriminator;
/// 06 §8-Q4 adopted — `mode`/`deadlineAtMs` are carried so lazy timeout
/// settlement needs no event-stream re-read).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HumanPending {
    /// Run path of the awaiting step (or gated step for supervision).
    pub run_path: RunPath,
    /// The request id the response must pair with.
    pub request_id: String,
    /// Why the interaction exists (step vs supervision gate).
    pub purpose: HumanPurpose,
    /// Interaction mode of a `purpose="step"` request; absent for
    /// supervision gates (06 §2.1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<HumanMode>,
    /// The prompt shown to the human (auto-generated gate description for
    /// supervision requests).
    pub prompt: String,
    /// Absolute response deadline (ms since epoch); absent for supervision
    /// requests, which have no deadline (spine §6.9).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<u64>,
}

/// Event-log cursor of the bound provider session (spine §6.6; advanced
/// ack-after-persist, never across session generations).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventCursor {
    /// The provider session that issued the recorded events.
    pub session_id: String,
    /// Last sequence durably consumed from that session.
    pub last_sequence: u64,
}

/// The run's provider binding state (spine §6.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingState {
    /// The bound device.
    pub device_id: String,
    /// Every provider session generation of this run, oldest first.
    pub session_lineage: Vec<String>,
    /// Cursor into the issuing session's event log.
    pub event_cursor: EventCursor,
}

/// The deterministic materialized view of a RunLog at a safe point
/// (spine §6.6); always reconstructible by folding the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckpointView {
    /// The run this view materializes.
    pub run_id: String,
    /// Content hash of the executing IR.
    pub ir_hash: Hash,
    /// Digest of the capability lockfile the IR was bound against.
    pub lockfile_digest: Hash,
    /// The run's input parameters, snapshotted at start.
    pub params_snapshot: Value,
    /// Provider binding state.
    pub binding: BindingState,
    /// Records of all completed steps, in completion order.
    pub completed: Vec<StepRecord>,
    /// The live call stack; `frames[0]` is the root flow frame.
    pub frames: Vec<CallFrame>,
    /// The single in-flight step.
    pub frontier: Frontier,
    /// The pending human interaction, when suspended on one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_pending: Option<HumanPending>,
}

/// One aligned step in a resume alignment report (spine §6.7-A).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlignmentEntry {
    /// Which instance was classified (07 §5.2: the report entry is
    /// path-addressed).
    ///
    /// A step id alone is ambiguous the moment a run nests: the same id
    /// recurs once per `foreach` round and once per call of a callee, so
    /// two entries could name the same step and mean different instances.
    /// The path says which one — and it is the same address
    /// `requiresConfirmation` already uses, so the two halves of the
    /// report finally speak one vocabulary.
    pub run_path: RunPath,
    /// The step being classified. Kept alongside the path: it is what a
    /// reader recognizes, and what `--allow-mutating-reexec` names.
    pub step_id: StepId,
    /// Its alignment class.
    pub class: AlignmentClass,
    /// Sub-domain annotation, e.g. `preflightChanged` (02 §12.3 ruling 6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A step whose re-execution needs explicit human authorization
/// (07 §5.2/§5.4 unified gate).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequiresConfirmation {
    /// Run path of the gated step.
    pub run_path: RunPath,
    /// The step id `--allow-mutating-reexec` takes for this gate (07 §5.4
    /// step 2: authorization names steps one by one, never a wildcard).
    ///
    /// Without it a reader has to re-derive the id from the path — which is
    /// exactly what the CLI did, and what every other consumer would have
    /// had to reimplement. Additive optional field (spine §6.1 R12): absent
    /// on pre-incorporation ledgers, where consumers fall back to the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<StepId>,
    /// Gate cause. Known values: `mutatingReexec`, `positionalReplay`,
    /// `orderInvalidated`, `frontierUnknown` (07 §5.4); closed-set
    /// incorporation pending.
    pub cause: String,
    /// Human-readable explanation.
    pub reason: String,
}

/// The alignment report a resume records in `runResumed` (spine §6.7-A).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlignmentReport {
    /// Per-step classifications, in new-IR order.
    pub entries: Vec<AlignmentEntry>,
    /// The computed resume point, absent when the run cannot resume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_point: Option<RunPath>,
    /// Steps requiring explicit re-execution authorization.
    pub requires_confirmation: Vec<RequiresConfirmation>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn iter_state_uses_the_as_keyword_on_the_wire() {
        let iter = IterState {
            var: "item".to_owned(),
            index: 2,
            key: None,
        };
        let wire = serde_json::to_value(&iter).expect("serialize");
        assert_eq!(wire, json!({"as": "item", "index": 2}));
    }

    #[test]
    fn checkpoint_view_round_trips_with_absent_optionals() {
        let view = CheckpointView {
            run_id: "run-1".to_owned(),
            ir_hash: serde_json::from_value(json!(format!("sha256:{}", "a".repeat(64))))
                .expect("hash"),
            lockfile_digest: serde_json::from_value(json!(format!("sha256:{}", "b".repeat(64))))
                .expect("hash"),
            params_snapshot: json!({"user": "alice"}),
            binding: BindingState {
                device_id: "dev-1".to_owned(),
                session_lineage: vec!["s-1".to_owned()],
                event_cursor: EventCursor {
                    session_id: "s-1".to_owned(),
                    last_sequence: 42,
                },
            },
            completed: vec![],
            frames: vec![CallFrame {
                flow_id: serde_json::from_value(json!("checkout")).expect("flow id"),
                ir_hash: serde_json::from_value(json!(format!("sha256:{}", "a".repeat(64))))
                    .expect("hash"),
                call_step_id: None,
                inputs_snapshot: json!({}),
                vars: BTreeMap::new(),
                iter_stack: vec![],
                next_index: 3,
            }],
            frontier: Frontier {
                run_path: vec![],
                state: StepState::Acting,
                pending_intent: Some(PendingIntent {
                    call_id: "c-1".to_owned(),
                    args_snapshot: json!({"x": 1}),
                }),
            },
            human_pending: None,
        };
        let wire = serde_json::to_value(&view).expect("serialize");
        assert_eq!(wire["frontier"]["state"], "acting");
        assert_eq!(wire["binding"]["eventCursor"]["lastSequence"], 42);
        assert!(wire.get("humanPending").is_none());
        assert!(wire["frames"][0].get("callStepId").is_none());
        let back: CheckpointView = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, view);
    }

    #[test]
    fn human_pending_purpose_discriminates_supervision() {
        // Supervision requests carry no mode and no deadline (spine §6.9):
        // both optionals are absent on the wire, never null.
        let pending = HumanPending {
            run_path: vec![],
            request_id: "req-1".to_owned(),
            purpose: HumanPurpose::Supervision,
            mode: None,
            prompt: "Approve mutating dispatch of tapPay".to_owned(),
            deadline_at_ms: None,
        };
        let wire = serde_json::to_value(&pending).expect("serialize");
        assert_eq!(wire["purpose"], "supervision");
        let object = wire.as_object().expect("object");
        assert!(!object.contains_key("mode"));
        assert!(!object.contains_key("deadlineAtMs"));
        let back: HumanPending = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, pending);
    }

    #[test]
    fn human_pending_step_purpose_carries_mode_and_deadline() {
        // 06 §8-Q4 adopted: a step-purpose pending request carries its
        // mode and absolute deadline for lazy settlement.
        let pending = HumanPending {
            run_path: vec![],
            request_id: "req-2".to_owned(),
            purpose: HumanPurpose::Step,
            mode: Some(crate::vocab::HumanMode::Confirm),
            prompt: "Confirm the transfer".to_owned(),
            deadline_at_ms: Some(1_700_000_600_000),
        };
        let wire = serde_json::to_value(&pending).expect("serialize");
        assert_eq!(wire["mode"], "confirm");
        assert_eq!(
            wire["deadlineAtMs"],
            serde_json::json!(1_700_000_600_000_u64)
        );
        let back: HumanPending = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, pending);
    }
}
