//! The RunLog event vocabulary: the append-only single source of truth of a
//! run (spine §6.1, closed 17-event union).
//!
//! Definition home adjudicated to `pointlock-ir` (type truth source, R12);
//! pending spine batch incorporation. Payload fields not pinned verbatim by
//! the spine carry a minimal reasonable shape and are marked pending
//! incorporation in their doc comments.
//!
//! R13 additions: `runStarted`/`runResumed` carry the segment's
//! `supervisePolicy` (explicitly `null` when unsupervised — per-segment,
//! never inherited), and `humanRequested`/`humanResponded` carry the
//! `purpose` discriminator.
//!
//! M1 incorporation (spine §6.1, StepRecord event carriers): `stepEntered`
//! carries `{ stepId, effectHash, judgeHash, resolvedInputs }` and is
//! appended after the ready-phase input snapshot is frozen, before any
//! preflight/`actionIntent`; `stepExited` carries `{ state, output? }`
//! (`output` present when the output projection completed). The checkpoint
//! fold harvests `StepRecord`'s hash/input/output fields from these events
//! — no placeholders remain.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::primitives::{ActionName, Hash, JsonSchemaDocument, StepId};
use crate::record::{
    AlignmentReport, AssertionOutcomeRecord, CallFrame, EventCursor, ObservationRecord,
    ProviderStateSummary,
};
use crate::run_path::RunPath;
use crate::runtime::{ActionOutcome, EvidenceGap, EvidenceRef, Verdict};
use crate::vocab::{ActChannel, HandlerHook, HumanMode, HumanPurpose, StepState, SupervisePolicy};

/// The envelope of one RunLog event (07 §3.3: `seq` is allocated inside the
/// appending transaction and is monotonically increasing per run).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunLogEvent {
    /// The run this event belongs to.
    pub run_id: String,
    /// One-based, per-run monotonic sequence — the authority on order.
    pub seq: u64,
    /// Wall-clock timestamp (ms since epoch); informational only.
    pub at_ms: u64,
    /// The run path the event is anchored to.
    pub run_path: RunPath,
    /// The typed payload.
    pub payload: RunLogPayload,
}

/// The closed 17-variant payload union (spine §6.1/A.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum RunLogPayload {
    /// A run segment started.
    #[serde(rename_all = "camelCase")]
    RunStarted {
        /// Content hash of the executing IR.
        ir_hash: Hash,
        /// Digest of the bound capability lockfile.
        lockfile_digest: Hash,
        /// The run's input parameters.
        params_snapshot: Value,
        /// The segment's supervision policy; explicitly `null` when
        /// unsupervised (R13 — recorded per segment, never inherited).
        supervise_policy: Option<SupervisePolicy>,
    },
    /// A step was scheduled and its inputs are frozen (appended after the
    /// ready-phase input snapshot completes, before preflight /
    /// `actionIntent` — spine §6.1 M1 note).
    #[serde(rename_all = "camelCase")]
    StepEntered {
        /// The entered step.
        step_id: StepId,
        /// Effect-domain hash of the step at execution time (alignment
        /// input; the fold copies it into `StepRecord.effectHash`).
        effect_hash: Hash,
        /// Judge-domain hash of the step at execution time (alignment
        /// input; the fold copies it into `StepRecord.judgeHash`).
        judge_hash: Hash,
        /// The ready-phase input snapshot: input expressions evaluated
        /// once and frozen, never re-evaluated on resume. Explicitly
        /// `null` for spans whose inputs were never resolved
        /// (blocked/skipped steps, or a failed argument evaluation).
        resolved_inputs: Value,
    },
    /// Preflight probes were evaluated (pending incorporation of the
    /// payload shape).
    #[serde(rename_all = "camelCase")]
    PreflightProbed {
        /// One outcome per probe assertion, in declaration order.
        outcomes: Vec<AssertionOutcomeRecord>,
    },
    /// The WAL entry written and fsynced *before* dispatching an action
    /// (spine §6.2 — the crash-safety anchor).
    #[serde(rename_all = "camelCase")]
    ActionIntent {
        /// The caller-generated action id.
        call_id: String,
        /// The evaluated arguments as they will be dispatched.
        args_snapshot: Value,
        /// 1-based position in `binding.attempts` (2026-07-18
        /// incorporation, item ②): the dispatch-identity discriminant —
        /// the act-chain overlay and the crash-resume chain re-entry
        /// both key on it. Absent on pre-incorporation ledgers.
        #[serde(skip_serializing_if = "Option::is_none")]
        chain_index: Option<u32>,
        /// The bound attempt's locating channel, verbatim.
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<ActChannel>,
        /// The bound attempt's provider-native action name, verbatim.
        #[serde(skip_serializing_if = "Option::is_none")]
        action_name: Option<ActionName>,
    },
    /// An action reached its four-way terminal.
    #[serde(rename_all = "camelCase")]
    ActionSettled {
        /// The action id this terminal belongs to.
        call_id: String,
        /// The terminal outcome (never folded).
        outcome: ActionOutcome,
    },
    /// An observation was captured and localized.
    #[serde(rename_all = "camelCase")]
    ObservationRecorded {
        /// The localized observation record.
        observation: ObservationRecord,
    },
    /// One assertion finished evaluating along its verify chain.
    #[serde(rename_all = "camelCase")]
    AssertionEvaluated {
        /// The evaluation outcome.
        outcome: AssertionOutcomeRecord,
    },
    /// A verdict was folded and recorded.
    #[serde(rename_all = "camelCase")]
    VerdictRecorded {
        /// The folded verdict.
        verdict: Verdict,
        /// Localized settlement/verdict/human-class evidence of THIS
        /// judgment (item ③, 2026-07-18; observation refs excluded —
        /// they ride `observationRecorded`). Empty on offline
        /// re-judgements (nothing is newly localized offline) and on
        /// pre-incorporation ledgers.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        localized: Vec<EvidenceRef>,
        /// Typed localization failures of the same judgment — the
        /// honest-gap record (principle 4/R4).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        localization_gaps: Vec<EvidenceGap>,
        /// The provider's `verdict.record` write-back failure, when the
        /// remote archival attempt failed (04 §5: the failure never
        /// changes the local verdict — the RunLog is the sole truth —
        /// and is annotated in the report as "remote archival failed").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote_archival_error: Option<String>,
    },
    /// A step reached a terminal lifecycle state.
    #[serde(rename_all = "camelCase")]
    StepExited {
        /// The terminal state.
        state: StepState,
        /// The projected step output, carried when the output projection
        /// completed (spine §6.1 M1 note); absent for exits without an
        /// output (blocked/skipped/aborted, error verdicts).
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<Value>,
        /// The failure-instant session profile (07 §2.2, incorporated
        /// 2026-07-18): present when a fail/unknown verdict is in force
        /// at exit; absent otherwise, on aborted follow-up exits (an
        /// aborted terminal makes no semantic claim), and on ledgers
        /// recorded before incorporation. One-directional additive
        /// (R12): pre-field readers reject ledgers carrying it.
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_state_summary: Option<ProviderStateSummary>,
        /// The settlement-evidence manifest of an UNVERIFIED exit
        /// (item ③ review fix): an assertion-less step records no
        /// verdict (R4), so its judgment manifest rides the exit
        /// instead — same merge rule, same honesty. Empty on verdict-
        /// bearing exits (the manifest rode `verdictRecorded`) and on
        /// pre-incorporation ledgers.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        localized: Vec<EvidenceRef>,
        /// Typed localization failures of the same unverified exit.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        localization_gaps: Vec<EvidenceGap>,
    },
    /// A subflow call frame was pushed.
    #[serde(rename_all = "camelCase")]
    CallFramePushed {
        /// The pushed frame.
        frame: CallFrame,
        /// Live-frame RE-ENTRY under a repaired callee, not a new stack
        /// level (07 §5.2 call down-drill, case (a)): the resume descended
        /// back into a frame that was still open and the callee's `irHash`
        /// moved, so `frames` must name the callee actually being executed
        /// ("frames 中该帧的 irHash 更新为新 callee irHash"). The fold
        /// updates that frame's `irHash` in place and keeps everything
        /// else — above all its `inputsSnapshot`, which a new IR never
        /// re-evaluates (§5.2 corollary / §4.6).
        ///
        /// Additive optional field (spine §6.1, the 2026-07-18 payload
        /// batch): absent on every pre-incorporation ledger, so a refold
        /// of an old run is byte-identical to what it always was.
        #[serde(default, skip_serializing_if = "core::ops::Not::not")]
        rebase: bool,
    },
    /// A subflow call frame was popped (pending incorporation of the
    /// payload shape).
    #[serde(rename_all = "camelCase")]
    CallFramePopped {
        /// The callee's declared outputs, when it completed.
        outputs: Option<Value>,
    },
    /// A handler hook fired.
    #[serde(rename_all = "camelCase")]
    HandlerTriggered {
        /// Which hook fired.
        hook: HandlerHook,
        /// One-based trigger count toward `maxTriggers`.
        trigger: u64,
        /// The consulted binding's declared disposition head (closed:
        /// `retry|continue|escalate|abort|repair`, 03 §1.8) — what the
        /// hook resolved TO, known at emission. Absent on
        /// pre-incorporation ledgers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disposition: Option<String>,
    },
    /// A human interaction was requested (fsynced *before* notifying any
    /// channel, spine §6.8/§6.9). M2 incorporation of the 06 §2.1 request
    /// shape: `mode`, `decisions`, `outputSchema` and the absolute
    /// `deadlineAtMs` watermark are carried by the event itself, so the
    /// store arbitration and the lazy timeout settlement need no source
    /// other than the ledger.
    #[serde(rename_all = "camelCase")]
    HumanRequested {
        /// The request id a response must pair with.
        request_id: String,
        /// Step vs supervision gate (R13).
        purpose: HumanPurpose,
        /// Interaction mode. Required semantics when `purpose` is `step`;
        /// absent for supervision gates, which carry no mode (06 §2.1).
        #[serde(skip_serializing_if = "Option::is_none")]
        mode: Option<HumanMode>,
        /// The prompt shown to the human (auto-generated gate description
        /// for supervision requests).
        prompt: String,
        /// The evidence/values presented, materialized once at ready —
        /// the `resolvedInputs` snapshot discipline (06 §2.3).
        presents: Value,
        /// Enumerated options: `confirm` carries exactly two labels
        /// (position-mapped to pass/fail); `judge` a subset of the
        /// three-valued vocabulary (06 §2.2).
        #[serde(skip_serializing_if = "Option::is_none")]
        decisions: Option<Vec<String>>,
        /// Input contract for `provideInput` responses; the store
        /// arbitration validates against it (06 §4.3 rule 3).
        #[serde(skip_serializing_if = "Option::is_none")]
        output_schema: Option<JsonSchemaDocument>,
        /// Absolute response deadline (ms since epoch), converted from
        /// `timeoutMs` at request creation — the lazy-settlement watermark
        /// (06 §5.3). Absent for supervision requests (no deadline,
        /// spine §6.9).
        #[serde(skip_serializing_if = "Option::is_none")]
        deadline_at_ms: Option<u64>,
    },
    /// A human response was arbitrated and recorded (pending incorporation
    /// of the payload shape).
    #[serde(rename_all = "camelCase")]
    HumanResponded {
        /// The paired request id.
        request_id: String,
        /// Step vs supervision gate (R13).
        purpose: HumanPurpose,
        /// The response payload (mode/decision-shaped, arbitrated by the
        /// store single writer).
        response: Value,
        /// Who responded.
        actor: String,
    },
    /// The run segment was suspended.
    #[serde(rename_all = "camelCase")]
    RunSuspended {
        /// Optional human-readable reason.
        reason: Option<String>,
        /// The suspension-instant session profile (07 §2.2): captured
        /// whenever a live session exists at the write site; same
        /// compat posture as on `stepExited`.
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_state_summary: Option<ProviderStateSummary>,
    },
    /// A run segment resumed from a checkpoint.
    #[serde(rename_all = "camelCase")]
    RunResumed {
        /// The alignment report of this resume (spine §6.7-A).
        alignment_report: AlignmentReport,
        /// The segment's supervision policy; explicitly `null` when
        /// unsupervised (R13 — per segment, never inherited).
        supervise_policy: Option<SupervisePolicy>,
        /// The new generation's reseeded cursor (07 §4.5, incorporated
        /// 2026-07-18): `sessionId` is the lineage extension, taken via
        /// `currentCursor()` after the reconcile decisions and before
        /// this append. Absent when the RPC failed at capture — and on
        /// ledgers recorded before incorporation (one-directional
        /// additive, R12).
        #[serde(skip_serializing_if = "Option::is_none")]
        event_cursor: Option<EventCursor>,
    },
    /// The run finished.
    #[serde(rename_all = "camelCase")]
    RunFinished {
        /// The folded flow verdict, when one was produced.
        verdict: Option<Verdict>,
        /// The flow verdict's `verdict.record` write-back failure
        /// (04 §5 — see [`RunLogPayload::VerdictRecorded`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote_archival_error: Option<String>,
    },
}

impl RunLogPayload {
    /// The wire discriminant (`type`) of this payload.
    pub fn event_type(&self) -> &'static str {
        match self {
            RunLogPayload::RunStarted { .. } => "runStarted",
            RunLogPayload::StepEntered { .. } => "stepEntered",
            RunLogPayload::PreflightProbed { .. } => "preflightProbed",
            RunLogPayload::ActionIntent { .. } => "actionIntent",
            RunLogPayload::ActionSettled { .. } => "actionSettled",
            RunLogPayload::ObservationRecorded { .. } => "observationRecorded",
            RunLogPayload::AssertionEvaluated { .. } => "assertionEvaluated",
            RunLogPayload::VerdictRecorded { .. } => "verdictRecorded",
            RunLogPayload::StepExited { .. } => "stepExited",
            RunLogPayload::CallFramePushed { .. } => "callFramePushed",
            RunLogPayload::CallFramePopped { .. } => "callFramePopped",
            RunLogPayload::HandlerTriggered { .. } => "handlerTriggered",
            RunLogPayload::HumanRequested { .. } => "humanRequested",
            RunLogPayload::HumanResponded { .. } => "humanResponded",
            RunLogPayload::RunSuspended { .. } => "runSuspended",
            RunLogPayload::RunResumed { .. } => "runResumed",
            RunLogPayload::RunFinished { .. } => "runFinished",
        }
    }

    /// All seventeen wire discriminants (spine §6.1 closed set).
    pub const EVENT_TYPES: [&'static str; 17] = [
        "runStarted",
        "stepEntered",
        "preflightProbed",
        "actionIntent",
        "actionSettled",
        "observationRecorded",
        "assertionEvaluated",
        "verdictRecorded",
        "stepExited",
        "callFramePushed",
        "callFramePopped",
        "handlerTriggered",
        "humanRequested",
        "humanResponded",
        "runSuspended",
        "runResumed",
        "runFinished",
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hash(fill: char) -> Hash {
        serde_json::from_value(json!(format!("sha256:{}", fill.to_string().repeat(64))))
            .expect("valid hash literal")
    }

    #[test]
    fn run_started_serializes_explicit_null_supervise_policy() {
        let payload = RunLogPayload::RunStarted {
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            supervise_policy: None,
        };
        let wire = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(wire["type"], "runStarted");
        // R13: explicitly null, not absent — the ledger is per-segment
        // self-describing about supervision.
        assert!(
            wire.as_object()
                .expect("object")
                .contains_key("supervisePolicy")
        );
        assert_eq!(wire["supervisePolicy"], Value::Null);

        let supervised = RunLogPayload::RunStarted {
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            supervise_policy: Some(SupervisePolicy::Mutating),
        };
        let wire = serde_json::to_value(&supervised).expect("serialize");
        assert_eq!(wire["supervisePolicy"], "mutating");
    }

    #[test]
    fn step_entered_carries_hashes_and_the_resolved_inputs_snapshot() {
        let payload = RunLogPayload::StepEntered {
            step_id: serde_json::from_value(json!("login")).expect("step id"),
            effect_hash: hash('c'),
            judge_hash: hash('d'),
            resolved_inputs: json!({"element": {"identifier": "loginButton"}}),
        };
        let wire = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(wire["type"], "stepEntered");
        assert_eq!(
            wire["effectHash"],
            json!(format!("sha256:{}", "c".repeat(64)))
        );
        assert_eq!(
            wire["judgeHash"],
            json!(format!("sha256:{}", "d".repeat(64)))
        );
        assert_eq!(
            wire["resolvedInputs"],
            json!({"element": {"identifier": "loginButton"}})
        );

        // Blocked/skipped spans never resolve inputs: explicitly null,
        // never absent (the ledger is self-describing).
        let unresolved = RunLogPayload::StepEntered {
            step_id: serde_json::from_value(json!("blocked_step")).expect("step id"),
            effect_hash: hash('c'),
            judge_hash: hash('d'),
            resolved_inputs: Value::Null,
        };
        let wire = serde_json::to_value(&unresolved).expect("serialize");
        assert!(
            wire.as_object()
                .expect("object")
                .contains_key("resolvedInputs")
        );
        assert_eq!(wire["resolvedInputs"], Value::Null);
    }

    #[test]
    fn step_exited_output_is_present_only_when_projected() {
        let with_output = RunLogPayload::StepExited {
            provider_state_summary: None,
            state: StepState::Judged,
            output: Some(json!({"ok": true})),
            localized: Vec::new(),
            localization_gaps: Vec::new(),
        };
        let wire = serde_json::to_value(&with_output).expect("serialize");
        assert_eq!(wire["type"], "stepExited");
        assert_eq!(wire["output"], json!({"ok": true}));

        let without = RunLogPayload::StepExited {
            provider_state_summary: None,
            state: StepState::Blocked,
            output: None,
            localized: Vec::new(),
            localization_gaps: Vec::new(),
        };
        let wire = serde_json::to_value(&without).expect("serialize");
        assert!(wire.get("output").is_none());
        let back: RunLogPayload = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, without);
    }

    #[test]
    fn human_events_carry_the_purpose_discriminator() {
        // Supervision requests carry no mode/decisions/schema/deadline:
        // the optionals are absent on the wire, never null.
        let requested = RunLogPayload::HumanRequested {
            request_id: "req-1".to_owned(),
            purpose: HumanPurpose::Supervision,
            mode: None,
            prompt: "Approve dispatch".to_owned(),
            presents: json!([]),
            decisions: None,
            output_schema: None,
            deadline_at_ms: None,
        };
        let wire = serde_json::to_value(&requested).expect("serialize");
        assert_eq!(wire["type"], "humanRequested");
        assert_eq!(wire["purpose"], "supervision");
        let object = wire.as_object().expect("object");
        assert!(!object.contains_key("mode"));
        assert!(!object.contains_key("decisions"));
        assert!(!object.contains_key("outputSchema"));
        assert!(!object.contains_key("deadlineAtMs"));
        let back: RunLogPayload = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, requested);

        let responded: RunLogPayload = serde_json::from_value(json!({
            "type": "humanResponded",
            "requestId": "req-1",
            "purpose": "supervision",
            "response": {"decision": "proceed"},
            "actor": "cli:dengfengwang",
        }))
        .expect("deserialize");
        assert_eq!(responded.event_type(), "humanResponded");
    }

    #[test]
    fn human_requested_step_purpose_carries_the_full_request_shape() {
        let schema = crate::primitives::JsonSchemaDocument::new(json!({
            "type": "object",
            "properties": { "code": { "type": "string" } },
            "required": ["code"]
        }))
        .expect("valid schema document");
        let requested = RunLogPayload::HumanRequested {
            request_id: "req-2".to_owned(),
            purpose: HumanPurpose::Step,
            mode: Some(HumanMode::ProvideInput),
            prompt: "Enter the code".to_owned(),
            presents: json!([{"kind": "value", "value": 1}]),
            decisions: Some(vec!["approve".to_owned(), "reject".to_owned()]),
            output_schema: Some(schema),
            deadline_at_ms: Some(1_700_000_600_000),
        };
        let wire = serde_json::to_value(&requested).expect("serialize");
        assert_eq!(wire["mode"], "provideInput");
        assert_eq!(wire["decisions"], json!(["approve", "reject"]));
        assert_eq!(wire["outputSchema"]["required"], json!(["code"]));
        assert_eq!(wire["deadlineAtMs"], json!(1_700_000_600_000_u64));
        let back: RunLogPayload = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, requested);
    }

    #[test]
    fn all_seventeen_discriminants_are_distinct_and_stable() {
        let mut seen = std::collections::BTreeSet::new();
        for name in RunLogPayload::EVENT_TYPES {
            assert!(seen.insert(name), "duplicate event type {name}");
        }
        assert_eq!(seen.len(), 17);
    }

    #[test]
    fn envelope_round_trips() {
        let event = RunLogEvent {
            run_id: "run-1".to_owned(),
            seq: 7,
            at_ms: 1_700_000_000_000,
            run_path: vec![],
            payload: RunLogPayload::ActionIntent {
                call_id: "c-1".to_owned(),
                args_snapshot: json!({"x": 1}),
                chain_index: None,
                channel: None,
                action_name: None,
            },
        };
        let wire = serde_json::to_value(&event).expect("serialize");
        assert_eq!(wire["payload"]["type"], "actionIntent");
        assert_eq!(wire["seq"], 7);
        let back: RunLogEvent = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, event);
    }
}
