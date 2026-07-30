//! `StepDossierView` — the adjudicable dossier of one step instance
//! (spine §9 rule 3, §10.1; 07 §2.3): IR node + YAML span + the full run
//! record (attempts, observations, evidence, assertion outcomes, verdict
//! lineage). This IS the `pointlock locate` JSON output shape — the CLI
//! and every renderer share this one query layer (08 §2.5).
//!
//! Unlike the timeline (bounded), the dossier is complete: attempts are
//! enriched from the `actionSettled` payloads with the full
//! `ErrorInfo`/timing that the discriminant-only `AttemptRecord` omits.

use std::collections::BTreeMap;

use pointlock_ir::{
    ActionOutcome, AssertionOutcomeRecord, ErrorInfo, EvidenceRef, FlowIR, JsonPointer,
    ObservationRecord, PathFrame, RunLogEvent, RunLogPayload, RunPath, SourceMapEntry, StepIR,
    StepRecord, StepState, StepVerdict, Verdict, parse_run_path, render_parsed_run_path,
    render_run_path,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ProjectionVersion;
use crate::error::StoreError;
use crate::store::Store;

/// One attempt, enriched beyond the checkpoint's discriminant-only
/// `AttemptRecord` by joining the `actionIntent`/`actionSettled` events
/// (07 §2.2: dossier attempts carry the `ErrorInfo` verbatim + timing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptView {
    /// Attempt ordinal from the intent path (`#n`), when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u64>,
    /// 1-based `binding.attempts` chain position (item ②, 2026-07-18);
    /// absent on pre-incorporation ledgers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_index: Option<u32>,
    /// The dispatched attempt's locating channel (wire literal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The dispatched attempt's provider-native action name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_name: Option<String>,
    /// The dispatch correlation id (WAL pairing key).
    pub call_id: String,
    /// Terminal discriminant (`succeeded/failed/cancelled/timedOut`);
    /// absent while the attempt is still in the crash window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Closed taxonomy class, as the runner recorded it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    /// Execution mode the daemon reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<String>,
    /// Daemon-side degradation reason, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// Full provider error on non-success terminals (verbatim).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    /// Dispatch argument snapshot (from the WAL intent).
    pub args_snapshot: Value,
    /// Provider-reported start, on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Provider-reported finish, on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    /// Ledger seq of the intent.
    pub intent_seq: u64,
    /// Ledger seq of the terminal, once settled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settled_seq: Option<u64>,
    /// Declared settlement evidence, verbatim from the terminal's
    /// `result.evidence` (item ③ — the provenance join key; the
    /// LOCALIZED copies live on the dossier's evidence gallery).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<pointlock_ir::AssetRef>,
}

/// One recorded verdict in the append-only lineage (re-judgements
/// append with `supersedes`; history is never deleted — 08 §2.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerdictRecordView {
    /// Ledger seq of the `verdictRecorded` event.
    pub seq: u64,
    /// Wall clock of the record.
    pub at_ms: u64,
    /// The full verdict as recorded.
    pub verdict: Verdict,
    /// This judgment's localized settlement/verdict/human evidence
    /// (item ③, 2026-07-18); empty on offline re-judgements and
    /// pre-incorporation records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub localized: Vec<EvidenceRef>,
    /// Typed localization failures of the same judgment (payload
    /// mirror — the seq-bearing aggregate lives on the dossier).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub localization_gaps: Vec<pointlock_ir::EvidenceGap>,
}

/// One aggregated localization gap of the dossier (item ③): the payload
/// gap plus its verdict-event anchor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceGapView {
    /// The declared asset, verbatim.
    pub asset: pointlock_ir::AssetRef,
    /// Why localization failed.
    pub reason: String,
    /// Ledger seq of the recording `verdictRecorded` event.
    pub seq: u64,
}

/// One localized gallery item with its provenance (08 §2.5: the
/// evidence tile says WHICH observation/judgment produced it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceItemView {
    /// The localized reference (asset + content address + local path).
    #[serde(flatten)]
    pub reference: EvidenceRef,
    /// Provenance label: `observation:<id>/screenshot`,
    /// `observation:<id>/uiSnapshot`, `verdict@seq:<n>` (that judgment's
    /// settlement/human evidence), or `exit@seq:<n>` (an unverified
    /// exit's manifest).
    pub source: String,
}

/// One handler trigger against this instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HandlerTriggerView {
    /// The hook.
    pub hook: String,
    /// Trigger ordinal.
    pub trigger: u64,
    /// The consulted binding's declared disposition head (03 §1.8
    /// closed set; 08 §2.5 handler row). Absent on pre-incorporation
    /// ledgers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    /// Ledger seq.
    pub seq: u64,
    /// Wall clock.
    pub at_ms: u64,
}

/// YAML source location resolved through the sourceMap (07 §2.3 part 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceLocation {
    /// JSON Pointer of the IR node inside its `FlowIR`.
    pub ir_path: JsonPointer,
    /// The sourceMap entry: file + span + macro origin trace.
    pub entry: SourceMapEntry,
}

/// The enclosing frame environment (07 §2.3 part 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrameEnvironment {
    /// Call-by-value inputs snapshot of the enclosing frame.
    pub inputs_snapshot: Value,
    /// `let` bindings visible in the frame.
    pub vars: BTreeMap<String, Value>,
    /// The failure-instant provider profile of this step's exit
    /// (07 §2.2/§2.3 part 4, incorporated 2026-07-18): present only for
    /// fail/unknown-verdict exits on post-incorporation ledgers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_state_summary: Option<pointlock_ir::ProviderStateSummary>,
}

/// The adjudicable dossier (spine §10.1; = `pointlock locate` JSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepDossierView {
    /// Protocol version (spine §10.3).
    pub projection_version: ProjectionVersion,
    /// The queried run.
    pub run_id: String,
    /// Canonical string of the instance path.
    pub run_path: String,
    /// The structured path (frames are the authority — spine §9).
    pub run_path_frames: RunPath,
    /// The step id.
    pub step_id: String,
    /// Effect-identity hash at entry.
    pub effect_hash: String,
    /// Judgement-identity hash at entry.
    pub judge_hash: String,
    /// The IR node in full, when the governing FlowIR artifact was
    /// supplied (07 §2.3 part 1; the ledger alone cannot resolve IR).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ir_node: Option<StepIR>,
    /// YAML source location, when the artifact was supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,
    /// Inputs frozen at `ready`.
    pub resolved_inputs: Value,
    /// Resume-drift probe outcomes (07 §5), in ledger order.
    pub preflight: Vec<AssertionOutcomeRecord>,
    /// Enriched attempts, in intent order.
    pub attempts: Vec<AttemptView>,
    /// Step output, once exited with one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// Recorded observations (omissions shown as-is).
    pub observations: Vec<ObservationRecord>,
    /// Assertion outcomes, in evaluation order.
    pub assertion_outcomes: Vec<AssertionOutcomeRecord>,
    /// The current verdict projection (last recorded), when judged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<StepVerdict>,
    /// The full append-only verdict lineage (supersedes chain).
    pub verdict_history: Vec<VerdictRecordView>,
    /// Handler triggers anchored at this instance.
    pub handler_triggers: Vec<HandlerTriggerView>,
    /// Localized evidence references (localPath included — 07 §2.3):
    /// observation assets plus every judgment's localized manifest,
    /// deduped (sha256, asset.id) first-wins, each with its provenance.
    pub evidence: Vec<EvidenceItemView>,
    /// Typed localization failures across the judgments (item ③): the
    /// honest-gap gallery — a cited-but-unlocalizable asset is a gap
    /// tile, never a silent omission.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_gaps: Vec<EvidenceGapView>,
    /// The failure-instant provider profile of this instance's exit
    /// (07 §2.2/§2.3 part 4): step-anchored — it rides this instance's
    /// own `stepExited`, so it surfaces here even when the live-frame
    /// environment join fails closed (popped call frames, later
    /// siblings — exactly the post-mortem cases the dossier serves).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_state_summary: Option<pointlock_ir::ProviderStateSummary>,
    /// Enclosing frame environment, when reconstructable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<FrameEnvironment>,
    /// Final exit state, or the frontier state while in flight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<StepState>,
}

/// Strips attempt/phase/assertion frames — the instance identity
/// (these three only ever suffix a step segment, spine §9).
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

fn attempt_of(path: &[PathFrame]) -> Option<u64> {
    path.iter().rev().find_map(|frame| match frame {
        PathFrame::Attempt { n } => Some(*n),
        _ => None,
    })
}

/// Serializes a unit-enum value to its wire literal.
fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Enumerates every entered step instance of a run, in first-entry order
/// (canonical string → structured path).
fn entered_instances(events: &[RunLogEvent]) -> Vec<(String, RunPath)> {
    let mut seen = Vec::new();
    let mut keys = std::collections::BTreeSet::new();
    for event in events {
        if matches!(event.payload, RunLogPayload::StepEntered { .. }) {
            let instance = instance_path(&event.run_path);
            let key = render_run_path(&instance);
            if keys.insert(key.clone()) {
                seen.push((key, instance));
            }
        }
    }
    seen
}

/// Resolves a locate `--step` argument to one instance path: a canonical
/// path string (spine §9 grammar, matched by rendered equality against
/// recorded instances — parse yields hash prefixes only), a JSON
/// `PathFrame[]` document (07 §2.3: the structured form is the
/// authority), or a bare step id (unique-match convenience; ambiguity is
/// a typed error).
pub fn locate_step(store: &Store, run_id: &str, step: &str) -> Result<RunPath, StoreError> {
    let events = store.events(run_id)?;
    let instances = entered_instances(&events);

    // JSON PathFrame[] input (07 §2.3): full hashes, exact identity.
    if step.trim_start().starts_with('[') {
        let frames: RunPath = serde_json::from_str(step).map_err(|err| StoreError::BadRunPath {
            input: step.to_owned(),
            message: format!("not a JSON PathFrame[] document: {err}"),
        })?;
        let wanted = render_run_path(&instance_path(&frames));
        return instances
            .into_iter()
            .find(|(key, _)| *key == wanted)
            .map(|(_, path)| path)
            .ok_or_else(|| StoreError::UnknownStepInstance {
                run_id: run_id.to_owned(),
                path: wanted,
            });
    }

    if step.contains('/') || step.contains('@') {
        let parsed = parse_run_path(step).map_err(|err| StoreError::BadRunPath {
            input: step.to_owned(),
            message: format!("{} at offset {}", err.message, err.offset),
        })?;
        // `render_run_path` abbreviates hashes to the same 8-hex prefix
        // the parser yields, so rendered equality is exact instance
        // identity (spine §9 round-trip guarantee).
        let wanted = render_parsed_run_path(&instance_parsed(&parsed));
        return instances
            .into_iter()
            .find(|(key, _)| *key == wanted)
            .map(|(_, path)| path)
            .ok_or_else(|| StoreError::UnknownStepInstance {
                run_id: run_id.to_owned(),
                path: step.to_owned(),
            });
    }

    let matches: Vec<(String, RunPath)> = instances
        .into_iter()
        .filter(|(_, path)| {
            instance_step_id(path)
                .map(|step_id| step_id.as_ref() == step)
                .unwrap_or(false)
        })
        .collect();
    match matches.len() {
        0 => Err(StoreError::UnknownStepInstance {
            run_id: run_id.to_owned(),
            path: step.to_owned(),
        }),
        1 => Ok(matches.into_iter().next().expect("len checked").1),
        _ => Err(StoreError::AmbiguousStep {
            run_id: run_id.to_owned(),
            step: step.to_owned(),
            candidates: matches.into_iter().map(|(key, _)| key).collect(),
        }),
    }
}

/// The step id an instance path names: its last `Step` frame, or the
/// call step of its last `Call` frame (the runner anchors call steps at
/// the `Call` frame itself, with no separate `Step` frame — 07 §2.1).
/// `None` for a handler-launched repair flow's own frame
/// (`Call { step_id: None }` — it has no host step to name).
fn instance_step_id(path: &[PathFrame]) -> Option<&pointlock_ir::StepId> {
    path.iter().rev().find_map(|frame| match frame {
        PathFrame::Step { step_id } => Some(step_id),
        PathFrame::Call {
            step_id: Some(step_id),
            ..
        } => Some(step_id),
        _ => None,
    })
}

/// Strips attempt/phase/assertion frames from a parsed path.
fn instance_parsed(path: &[pointlock_ir::ParsedPathFrame]) -> Vec<pointlock_ir::ParsedPathFrame> {
    use pointlock_ir::ParsedPathFrame as P;
    path.iter()
        .filter(|frame| {
            !matches!(
                frame,
                P::Attempt { .. } | P::Phase { .. } | P::Assertion { .. }
            )
        })
        .cloned()
        .collect()
}

/// Builds the dossier of one instance. `artifacts` may carry the run's
/// FlowIR and its subflow closure; when the governing IR is absent the
/// dossier still delivers the complete run record (irNode/source absent).
pub fn step_dossier(
    store: &Store,
    run_id: &str,
    path: &[PathFrame],
    artifacts: &[FlowIR],
) -> Result<StepDossierView, StoreError> {
    let events = store.events(run_id)?;
    let instance = instance_path(path);
    let key = render_run_path(&instance);

    let step_id =
        instance_step_id(&instance)
            .cloned()
            .ok_or_else(|| StoreError::UnknownStepInstance {
                run_id: run_id.to_owned(),
                path: key.clone(),
            })?;

    // ── Event fold for this instance ──────────────────────────────────
    let mut entered: Option<(String, String, Value)> = None; // effect, judge, inputs
    let mut preflight = Vec::new();
    let mut attempts: Vec<AttemptView> = Vec::new();
    let mut by_call: BTreeMap<String, usize> = BTreeMap::new();
    let mut observations = Vec::new();
    let mut assertion_outcomes = Vec::new();
    let mut verdict_history = Vec::new();
    let mut handler_triggers = Vec::new();
    let mut evidence: Vec<EvidenceItemView> = Vec::new();
    let mut evidence_gaps: Vec<EvidenceGapView> = Vec::new();
    let mut output = None;
    let mut state: Option<StepState> = None;
    let mut step_summary: Option<pointlock_ir::ProviderStateSummary> = None;
    let mut touched = false;

    for event in &events {
        if render_run_path(&instance_path(&event.run_path)) != key {
            continue;
        }
        touched = true;
        match &event.payload {
            RunLogPayload::StepEntered {
                effect_hash,
                judge_hash,
                resolved_inputs,
                ..
            } => {
                entered = Some((
                    effect_hash.to_string(),
                    judge_hash.to_string(),
                    resolved_inputs.clone(),
                ));
            }
            RunLogPayload::PreflightProbed { outcomes } => {
                preflight.extend(outcomes.iter().cloned());
            }
            RunLogPayload::ActionIntent {
                call_id,
                args_snapshot,
                chain_index,
                channel,
                action_name,
            } => {
                by_call.insert(call_id.clone(), attempts.len());
                attempts.push(AttemptView {
                    n: attempt_of(&event.run_path),
                    chain_index: *chain_index,
                    channel: channel.as_ref().map(wire),
                    action_name: action_name.as_ref().map(|name| name.as_str().to_owned()),
                    call_id: call_id.clone(),
                    outcome: None,
                    error_class: None,
                    execution_mode: None,
                    fallback_reason: None,
                    error: None,
                    args_snapshot: args_snapshot.clone(),
                    started_at_ms: None,
                    finished_at_ms: None,
                    intent_seq: event.seq,
                    settled_seq: None,
                    evidence: Vec::new(),
                });
            }
            RunLogPayload::ActionSettled { call_id, outcome } => {
                if let Some(&index) = by_call.get(call_id) {
                    let attempt = &mut attempts[index];
                    attempt.outcome = Some(outcome.kind().to_owned());
                    if let ActionOutcome::Succeeded { result } = outcome {
                        attempt.evidence = result.evidence.clone();
                    }
                    attempt.settled_seq = Some(event.seq);
                    match outcome {
                        ActionOutcome::Succeeded { result } => {
                            attempt.started_at_ms = Some(result.started_at_ms);
                            attempt.finished_at_ms = Some(result.finished_at_ms);
                            attempt.execution_mode =
                                result.execution.as_ref().map(|execution| match execution {
                                    pointlock_ir::ActionExecution::NativeSemantic { .. } => {
                                        "nativeSemantic".to_owned()
                                    }
                                    pointlock_ir::ActionExecution::WebSemantic { .. } => {
                                        "webSemantic".to_owned()
                                    }
                                    pointlock_ir::ActionExecution::CoordinateFallback {
                                        fallback_reason,
                                        ..
                                    } => {
                                        attempt.fallback_reason = Some(wire(fallback_reason));
                                        "coordinateFallback".to_owned()
                                    }
                                });
                        }
                        ActionOutcome::Failed { error }
                        | ActionOutcome::Cancelled { error }
                        | ActionOutcome::TimedOut { error } => {
                            attempt.error = Some(error.clone());
                        }
                    }
                }
            }
            RunLogPayload::ObservationRecorded { observation } => {
                if let Some(evidence_ref) = &observation.screenshot {
                    evidence.push(EvidenceItemView {
                        reference: evidence_ref.clone(),
                        source: format!("observation:{}/screenshot", observation.observation_id),
                    });
                }
                if let Some(evidence_ref) = &observation.ui_snapshot {
                    evidence.push(EvidenceItemView {
                        reference: evidence_ref.clone(),
                        source: format!("observation:{}/uiSnapshot", observation.observation_id),
                    });
                }
                observations.push(observation.clone());
            }
            RunLogPayload::AssertionEvaluated { outcome } => {
                assertion_outcomes.push(outcome.clone());
            }
            RunLogPayload::VerdictRecorded {
                verdict,
                localized,
                localization_gaps,
                remote_archival_error: _,
            } => {
                // The judgment's manifest merges into the gallery with
                // the same (sha256, asset.id) first-wins rule the fold
                // applies — the two surfaces cannot diverge.
                for entry in localized {
                    let duplicate = evidence.iter().any(|existing| {
                        existing.reference.sha256 == entry.sha256
                            && existing.reference.asset.id == entry.asset.id
                    });
                    if !duplicate {
                        evidence.push(EvidenceItemView {
                            reference: entry.clone(),
                            source: format!("verdict@seq:{}", event.seq),
                        });
                    }
                }
                for gap in localization_gaps {
                    evidence_gaps.push(EvidenceGapView {
                        asset: gap.asset.clone(),
                        reason: gap.reason.clone(),
                        seq: event.seq,
                    });
                }
                verdict_history.push(VerdictRecordView {
                    seq: event.seq,
                    at_ms: event.at_ms,
                    verdict: verdict.clone(),
                    localized: localized.clone(),
                    localization_gaps: localization_gaps.clone(),
                });
            }
            RunLogPayload::HandlerTriggered {
                hook,
                trigger,
                disposition,
            } => {
                handler_triggers.push(HandlerTriggerView {
                    hook: wire(hook),
                    trigger: *trigger,
                    disposition: disposition.clone(),
                    seq: event.seq,
                    at_ms: event.at_ms,
                });
            }
            RunLogPayload::StepExited {
                provider_state_summary,
                state: exit_state,
                output: exit_output,
                localized,
                localization_gaps,
            } => {
                state = Some(*exit_state);
                if exit_output.is_some() {
                    output = exit_output.clone();
                }
                if provider_state_summary.is_some() {
                    step_summary = provider_state_summary.clone();
                }
                // An UNVERIFIED exit's manifest (item ③ review fix):
                // same merge rule as the verdict-borne one.
                for entry in localized {
                    let duplicate = evidence.iter().any(|existing| {
                        existing.reference.sha256 == entry.sha256
                            && existing.reference.asset.id == entry.asset.id
                    });
                    if !duplicate {
                        evidence.push(EvidenceItemView {
                            reference: entry.clone(),
                            source: format!("exit@seq:{}", event.seq),
                        });
                    }
                }
                for gap in localization_gaps {
                    evidence_gaps.push(EvidenceGapView {
                        asset: gap.asset.clone(),
                        reason: gap.reason.clone(),
                        seq: event.seq,
                    });
                }
            }
            _ => {}
        }
    }

    if !touched || entered.is_none() {
        return Err(StoreError::UnknownStepInstance {
            run_id: run_id.to_owned(),
            path: key.clone(),
        });
    }
    let (effect_hash, judge_hash, resolved_inputs) = entered.expect("checked above");

    // ── Checkpoint overlay: recorded error classes + frontier state ───
    let checkpoint = store.materialized_checkpoint(run_id)?;
    if let Some((_, view)) = &checkpoint {
        if let Some(record) = view
            .completed
            .iter()
            .find(|record: &&StepRecord| render_run_path(&record.run_path) == key)
        {
            for recorded in &record.attempts {
                if let Some(&index) = by_call.get(&recorded.call_id) {
                    let attempt = &mut attempts[index];
                    attempt.error_class = recorded.error_class.as_ref().map(wire);
                    if attempt.execution_mode.is_none() {
                        attempt.execution_mode = recorded.execution_mode.as_ref().map(wire);
                    }
                    if attempt.fallback_reason.is_none() {
                        attempt.fallback_reason = recorded.fallback_reason.as_ref().map(wire);
                    }
                }
            }
        }
        if state.is_none() && render_run_path(&instance_path(&view.frontier.run_path)) == key {
            state = Some(view.frontier.state);
        }
    }

    // Current verdict = last recorded projection (append-only lineage).
    let verdict = verdict_history.last().map(|record| StepVerdict {
        status: record.verdict.status,
        degraded: record.verdict.degraded,
        supersedes: record.verdict.supersedes.clone(),
    });

    // ── IR resolution, when the governing artifact is supplied ────────
    // The governing flow is the one whose body DECLARES the step: for a
    // call step (instance ends with the Call frame itself) that is the
    // HOST flow, not the callee — so the terminal Call frame is skipped
    // before scanning for the innermost Flow/Call authority.
    let scan = match instance.last() {
        Some(PathFrame::Call { .. }) => &instance[..instance.len() - 1],
        _ => instance.as_slice(),
    };
    let governing_hash = scan.iter().rev().find_map(|frame| match frame {
        PathFrame::Flow { ir_hash, .. } => Some(ir_hash.clone()),
        PathFrame::Call { callee_ir_hash, .. } => Some(callee_ir_hash.clone()),
        _ => None,
    });
    let mut ir_node = None;
    let mut source = None;
    if let Some(hash) = governing_hash
        && let Some(flow) = artifacts.iter().find(|flow| flow.ir_hash == hash)
        && let Some((node, pointer)) = find_step(&flow.body, "/body", step_id.as_ref())
    {
        source = flow
            .source_map
            .iter()
            .find(|entry| entry.ir_path.as_ref() == pointer)
            .map(|entry| SourceLocation {
                ir_path: entry.ir_path.clone(),
                entry: entry.clone(),
            });
        ir_node = Some(node.clone());
    }

    // ── Frame environment from the checkpoint frame stack ─────────────
    // `CheckpointView.frames` is the LIVE stack; it matches this instance
    // only while the instance's own call chain is still open. Every
    // frame of the chain is identity-checked (root by irHash, each Call
    // by call step + callee identity) — on any mismatch the environment
    // fails closed to absent rather than reporting another call's state.
    let frame = checkpoint
        .as_ref()
        .and_then(|(_, view)| frame_environment(&instance, &view.frames))
        .map(|mut environment| {
            environment.provider_state_summary = step_summary.clone();
            environment
        });

    Ok(StepDossierView {
        projection_version: ProjectionVersion,
        run_id: run_id.to_owned(),
        run_path: key,
        run_path_frames: instance,
        step_id: step_id.to_string(),
        effect_hash,
        judge_hash,
        ir_node,
        source,
        resolved_inputs,
        preflight,
        attempts,
        output,
        observations,
        assertion_outcomes,
        verdict,
        verdict_history,
        handler_triggers,
        evidence,
        evidence_gaps,
        provider_state_summary: step_summary,
        frame,
        state,
    })
}

/// The identity-verified frame-environment join (07 §2.3 part 4): walks
/// the instance path's Flow/Call chain against the live frame stack and
/// returns the enclosing frame only when EVERY level matches — the root
/// by `irHash`, each call level by call step id + callee identity. A
/// popped or re-pushed stack (completed calls, later siblings) fails the
/// walk and the environment is reported absent, never mis-attributed.
fn frame_environment(
    instance: &[PathFrame],
    frames: &[pointlock_ir::CallFrame],
) -> Option<FrameEnvironment> {
    let mut level = 0usize;
    for (index, path_frame) in instance.iter().enumerate() {
        match path_frame {
            PathFrame::Flow { ir_hash, flow_id } => {
                let frame = frames.first()?;
                if frame.ir_hash != *ir_hash
                    || frame.flow_id != *flow_id
                    || frame.call_step_id.is_some()
                {
                    return None;
                }
            }
            PathFrame::Call {
                step_id,
                callee_flow_id,
                callee_ir_hash,
            } => {
                // The terminal Call frame IS the dossier target (the call
                // step itself); its environment is the host frame already
                // verified, so the callee frame is not required.
                if index + 1 == instance.len() {
                    break;
                }
                level += 1;
                let frame = frames.get(level)?;
                if frame.call_step_id != *step_id
                    || frame.flow_id != *callee_flow_id
                    || frame.ir_hash != *callee_ir_hash
                {
                    return None;
                }
            }
            _ => {}
        }
    }
    frames.get(level).map(|frame| FrameEnvironment {
        inputs_snapshot: frame.inputs_snapshot.clone(),
        vars: frame.vars.clone(),
        provider_state_summary: None,
    })
}

/// Depth-first search for a step id in a body; returns the node and its
/// JSON Pointer inside the FlowIR document.
fn find_step<'a>(body: &'a [StepIR], prefix: &str, step_id: &str) -> Option<(&'a StepIR, String)> {
    for (index, step) in body.iter().enumerate() {
        let pointer = format!("{prefix}/{index}");
        if step.step_id().as_ref() == step_id {
            return Some((step, pointer));
        }
        match step {
            StepIR::If(nested) => {
                if let Some(found) = find_step(&nested.then, &format!("{pointer}/then"), step_id) {
                    return Some(found);
                }
                if let Some(else_body) = nested.r#else.as_deref()
                    && let Some(found) = find_step(else_body, &format!("{pointer}/else"), step_id)
                {
                    return Some(found);
                }
            }
            StepIR::Foreach(nested) => {
                if let Some(found) = find_step(&nested.body, &format!("{pointer}/body"), step_id) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}
