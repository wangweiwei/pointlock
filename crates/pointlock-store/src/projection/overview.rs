//! `RunOverview` — the run-level summary projection (spine §10.1):
//! identity + status + flow verdict + `revision` (= the run's max ledger
//! seq — the SSE invalidation currency, 08 §5) + the per-step state map
//! (the graph overlay's data source; keys are canonical RunPath strings,
//! values the minimal `{state, verdictStatus?, degraded?}` set — dossier
//! detail stays in `StepDossierView`).

use std::collections::BTreeMap;

use pointlock_ir::{AlignmentClass, PathFrame, RunLogPayload, StepState, render_run_path};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ProjectionVersion;
use crate::error::StoreError;
use crate::store::Store;

/// Per-class alignment counts of the latest resume (08 §2.4 top bar).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlignmentSummary {
    /// History kept as-is.
    pub reusable: u32,
    /// Offline re-judgement, no device redispatch.
    pub judge_dirty: u32,
    /// Step + downstream invalidated, re-runs.
    pub effect_dirty: u32,
    /// Newly introduced steps.
    pub new: u32,
    /// Steps whose history lost its IR node.
    pub orphaned: u32,
}

/// The minimal per-step overlay cell (2026-07-17 ruling, additive).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepStateSummary {
    /// Last recorded step state (closed vocabulary).
    pub state: StepState,
    /// Verdict status once judged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict_status: Option<String>,
    /// Degraded-verification marker of that verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<bool>,
    /// Act-chain runtime marks of the LATEST acting pass (08 §3.4,
    /// incorporated 2026-07-18): one entry per SETTLED dispatch, keyed
    /// by 1-based chain position — chips beyond the highest marked
    /// index render untried by absence. Absent entirely on
    /// pre-incorporation ledgers and for undispatched steps (the graph
    /// never invents runtime state, principle 4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub act_chain_marks: Option<Vec<ActChainMark>>,
}

/// One settled dispatch of the latest acting pass (08 §3.4). `mark` is
/// the closed chip vocabulary minus `untried` (absence = untried):
/// `succeeded` | `crossed`. `executionMode`/`fallbackReason` ride
/// verbatim so the renderer (which holds the IR) can apply the
/// `acceptExecutionModes` whitelist check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActChainMark {
    /// 1-based `binding.attempts` position.
    pub chain_index: u32,
    /// `succeeded` | `crossed`.
    pub mark: String,
    /// Daemon-reported execution mode, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<String>,
    /// Daemon-side degradation reason, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

/// The run summary (spine §10.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunOverview {
    /// Protocol version (spine §10.3).
    pub projection_version: ProjectionVersion,
    /// The run.
    pub run_id: String,
    /// The run's flow.
    pub flow_id: String,
    /// The executed IR (full `sha256:` form).
    pub ir_hash: String,
    /// The lockfile digest the run bound against (repair guidance: use
    /// the SAME lockfile — 08 §2.7; additive, no version bump).
    pub lockfile_digest: String,
    /// The bound device.
    pub device_id: String,
    /// Session lineage (resume generations — 08 §2.4).
    pub session_lineage: Vec<String>,
    /// Ledger status (`running`/`suspended`/`awaitingHuman`/`finished`).
    pub status: String,
    /// Snapshot revision = max ledger seq (08 §5: the invalidation
    /// currency; SSE pushes only `{revision}`).
    pub revision: u64,
    /// Run creation wall clock.
    pub created_at_ms: u64,
    /// Terminal wall clock, once finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    /// Flow verdict status; absent = unfinished, finished unverified, or
    /// aborted (the ledger's `runFinished.verdict` as-is — no folding).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_verdict_status: Option<String>,
    /// Degraded marker of the flow verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_verdict_degraded: Option<bool>,
    /// Supervision policy of the current segment (explicit `null` when
    /// unsupervised — recorded per segment, never inherited; R13).
    pub supervise_policy: Option<String>,
    /// Alignment counts of the latest resume, when the run resumed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alignment: Option<AlignmentSummary>,
    /// Whether a human request is pending (inbox red dot — 08 §3.5).
    pub awaiting_human: bool,
    /// The most recent suspension's provider profile (07 §2.2,
    /// incorporated 2026-07-18): present only while the run is actually
    /// suspended/awaitingHuman — a superseded suspension profile must
    /// not read as current on a resumed or finished run. Named for its
    /// timing; the step-anchored captures live in the dossier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_suspension_provider_state_summary: Option<pointlock_ir::ProviderStateSummary>,
    /// The per-step overlay map, keyed by canonical RunPath string
    /// (iteration frames included; strip `[i]`/`[i:key]` to join onto
    /// `FlowGraphView` node anchors — 08 §3.2 aggregate rule).
    pub steps: BTreeMap<String, StepStateSummary>,
}

/// Serializes a unit-enum value to its wire literal.
fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Strips attempt/phase/assertion frames — the instance identity.
fn instance_path(path: &[PathFrame]) -> Vec<PathFrame> {
    path.iter()
        .filter(|frame| {
            !matches!(
                frame,
                PathFrame::Attempt { .. } | PathFrame::Phase { .. } | PathFrame::Assertion { .. }
            )
        })
        .cloned()
        .collect()
}

/// Projects one run's overview from its ledger + metadata.
pub fn run_overview(store: &Store, run_id: &str) -> Result<RunOverview, StoreError> {
    let meta = store.run_meta(run_id)?;
    let status = store.run_status(run_id)?;
    let events = store.events(run_id)?;
    let revision = events.last().map(|event| event.seq).unwrap_or(0);

    let mut steps: BTreeMap<String, StepStateSummary> = BTreeMap::new();
    let mut finished_at_ms = None;
    let mut flow_verdict: Option<(String, bool)> = None;
    let mut supervise_policy: Option<String> = None;
    let mut alignment = None;
    let mut awaiting: Option<(String, pointlock_ir::RunPath)> = None;
    let mut last_suspension_summary: Option<pointlock_ir::ProviderStateSummary> = None;
    let mut session_lineage = meta.binding.session_lineage.clone();
    let mut chain_marks: BTreeMap<String, Vec<ActChainMark>> = BTreeMap::new();
    let mut intent_index: BTreeMap<String, (String, u32)> = BTreeMap::new();
    let mut boundary_pending: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    for event in &events {
        let key = || render_run_path(&instance_path(&event.run_path));
        match &event.payload {
            RunLogPayload::RunStarted {
                supervise_policy: policy,
                ..
            } => {
                supervise_policy = policy.as_ref().map(wire);
            }
            RunLogPayload::RunResumed {
                alignment_report,
                supervise_policy: policy,
                event_cursor,
            } => {
                // 07 §4.5: a cursor-bearing resume is a generation — the
                // display lineage extends here (the run row keeps the
                // bind-time binding verbatim).
                if let Some(cursor) = event_cursor
                    && session_lineage.last() != Some(&cursor.session_id)
                {
                    session_lineage.push(cursor.session_id.clone());
                }
                // Per-segment recording, never inherited (R13); a resume
                // also supersedes the prior suspension profile.
                supervise_policy = policy.as_ref().map(wire);
                last_suspension_summary = None;
                let count = |class: AlignmentClass| {
                    alignment_report
                        .entries
                        .iter()
                        .filter(|entry| entry.class == class)
                        .count() as u32
                };
                alignment = Some(AlignmentSummary {
                    reusable: count(AlignmentClass::Reusable),
                    judge_dirty: count(AlignmentClass::JudgeDirty),
                    effect_dirty: count(AlignmentClass::EffectDirty),
                    new: count(AlignmentClass::New),
                    orphaned: count(AlignmentClass::Orphaned),
                });
            }
            RunLogPayload::StepEntered { .. } => {
                // A fresh span invalidates any previous pass's marks.
                chain_marks.remove(&key());
                steps.insert(
                    key(),
                    StepStateSummary {
                        state: StepState::Ready,
                        verdict_status: None,
                        degraded: None,
                        act_chain_marks: None,
                    },
                );
            }
            RunLogPayload::ActionIntent {
                call_id,
                chain_index: Some(index),
                ..
            } => {
                let step_key = key();
                if boundary_pending.remove(&step_key) {
                    // The deferred pass boundary: the new pass starts.
                    chain_marks.remove(&step_key);
                } else if let Some(marks) = chain_marks.get_mut(&step_key) {
                    let max_marked = marks.iter().map(|mark| mark.chain_index).max();
                    if max_marked.is_some_and(|max| *index < max) {
                        // A restart below the pass's frontier (fresh
                        // re-execution after an effect-dirty resume):
                        // the crashed pass's marks are superseded.
                        marks.clear();
                    } else {
                        // A same-position re-dispatch (in-attempt retry):
                        // the latest settle owns the chip.
                        marks.retain(|mark| mark.chain_index != *index);
                    }
                }
                intent_index.insert(call_id.clone(), (step_key, *index));
            }
            RunLogPayload::ActionSettled { call_id, outcome } => {
                if let Some((step_key, index)) = intent_index.remove(call_id) {
                    let (mark, execution_mode, fallback_reason) = match outcome {
                        pointlock_ir::ActionOutcome::Succeeded { result } => {
                            let (mode, reason) = match &result.execution {
                                Some(pointlock_ir::ActionExecution::NativeSemantic { .. }) => {
                                    (Some("nativeSemantic".to_owned()), None)
                                }
                                Some(pointlock_ir::ActionExecution::WebSemantic { .. }) => {
                                    (Some("webSemantic".to_owned()), None)
                                }
                                Some(pointlock_ir::ActionExecution::CoordinateFallback {
                                    fallback_reason,
                                    ..
                                }) => (
                                    Some("coordinateFallback".to_owned()),
                                    Some(wire(fallback_reason)),
                                ),
                                None => (None, None),
                            };
                            ("succeeded", mode, reason)
                        }
                        _ => ("crossed", None, None),
                    };
                    chain_marks.entry(step_key).or_default().push(ActChainMark {
                        chain_index: index,
                        mark: mark.to_owned(),
                        execution_mode,
                        fallback_reason,
                    });
                }
            }
            RunLogPayload::HandlerTriggered { hook, .. } => {
                // A hook firing delimits the acting pass (item ② ruling)
                // — but LAZILY: the old pass's marks are invalidated only
                // when a new pass actually STARTS (its first intent). A
                // continue/abort/escalate disposition never starts one,
                // and the settled marks it leaves ARE the latest pass
                // (Wave D review). onResumeDrift/onTimeout triggers do
                // not delimit: a crash-resume continuation is the same
                // pass (07 §1.4).
                if matches!(
                    hook,
                    pointlock_ir::HandlerHook::OnFail
                        | pointlock_ir::HandlerHook::OnError
                        | pointlock_ir::HandlerHook::OnUnknown
                ) {
                    boundary_pending.insert(key());
                }
            }
            RunLogPayload::StepExited { state, .. } => {
                if let Some(cell) = steps.get_mut(&key()) {
                    cell.state = *state;
                }
                // A terminal exit of the awaiting step settles its
                // request without a response (lazy timeout settlement /
                // aborted disposition — mirrors the checkpoint fold).
                if awaiting
                    .as_ref()
                    .is_some_and(|(_, path)| *path == event.run_path)
                {
                    awaiting = None;
                }
            }
            RunLogPayload::VerdictRecorded { verdict, .. } => {
                if let Some(cell) = steps.get_mut(&key()) {
                    cell.verdict_status = Some(wire(&verdict.status));
                    cell.degraded = Some(verdict.degraded);
                }
            }
            RunLogPayload::HumanRequested { request_id, .. } => {
                awaiting = Some((request_id.clone(), event.run_path.clone()));
                if let Some(cell) = steps.get_mut(&key()) {
                    cell.state = StepState::AwaitingHuman;
                }
            }
            RunLogPayload::HumanResponded {
                request_id,
                purpose,
                response,
                ..
            } => {
                let non_final = *purpose == pointlock_ir::HumanPurpose::Supervision
                    && response.get("decision").and_then(Value::as_str) == Some("suspend");
                if !non_final && awaiting.as_ref().is_some_and(|(id, _)| id == request_id) {
                    awaiting = None;
                }
            }
            RunLogPayload::RunSuspended {
                provider_state_summary,
                ..
            } => {
                last_suspension_summary = provider_state_summary.clone();
            }
            RunLogPayload::RunFinished {
                verdict,
                remote_archival_error: _,
            } => {
                finished_at_ms = Some(event.at_ms);
                flow_verdict = verdict
                    .as_ref()
                    .map(|verdict| (wire(&verdict.status), verdict.degraded));
            }
            _ => {}
        }
    }

    // Attach the latest-pass marks to their cells.
    for (step_key, marks) in chain_marks {
        if let Some(cell) = steps.get_mut(&step_key) {
            cell.act_chain_marks = Some(marks);
        }
    }

    // The live frontier state overrides the seed for the in-flight step.
    if let Some((_, view)) = store.materialized_checkpoint(run_id)? {
        let key = render_run_path(&instance_path(&view.frontier.run_path));
        if let Some(cell) = steps.get_mut(&key) {
            cell.state = view.frontier.state;
        }
    }

    let (flow_verdict_status, flow_verdict_degraded) = match flow_verdict {
        Some((status, degraded)) => (Some(status), Some(degraded)),
        None => (None, None),
    };

    Ok(RunOverview {
        projection_version: ProjectionVersion,
        run_id: run_id.to_owned(),
        flow_id: meta.flow_id.to_string(),
        ir_hash: meta.ir_hash.to_string(),
        lockfile_digest: meta.lockfile_digest.to_string(),
        device_id: meta.binding.device_id.clone(),
        session_lineage,
        status: status.as_str().to_owned(),
        revision,
        created_at_ms: meta.created_at_ms,
        finished_at_ms,
        flow_verdict_status,
        flow_verdict_degraded,
        supervise_policy,
        alignment,
        awaiting_human: awaiting.is_some(),
        last_suspension_provider_state_summary: match status {
            crate::RunStatus::Suspended | crate::RunStatus::AwaitingHuman => {
                last_suspension_summary
            }
            _ => None,
        },
        steps,
    })
}
