//! `RunTimelineEntry` — the timeline projection of the RunLog (spine
//! §10.1, 08 §4). Pinned decisions, all unchanged: monotonic `seq` order
//! (never timestamps); the five-value filter vocabulary adopted verbatim
//! from the DeviceRail live visualizer; the 50/page hard cap; evidence as
//! pure references (never inlined bytes); bounded rendering with explicit
//! `truncated` flags and a fail-closed placeholder for over-limit events
//! (the full record stays reachable through `StepDossierView`).

use pointlock_ir::{
    ActionOutcome, HandlerHook, RunLogEvent, RunLogPayload, StepState, render_run_path,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ProjectionVersion;
use crate::error::StoreError;
use crate::store::Store;

/// Hard page-size cap (08 §4.4, = `LIVE_TIMELINE_MAX_PAGE_SIZE`).
pub const TIMELINE_MAX_PAGE_SIZE: u32 = 50;
/// Text fields truncate at 4 KiB (08 §4.4).
pub const TIMELINE_TEXT_MAX_BYTES: usize = 4 * 1024;
/// JSON digests truncate at 16 KiB serialized (08 §4.4).
pub const TIMELINE_JSON_MAX_BYTES: usize = 16 * 1024;
/// JSON digests truncate at depth 12 (08 §4.4).
pub const TIMELINE_JSON_MAX_DEPTH: usize = 12;
/// At most 32 evidence references per entry (08 §4.4).
pub const TIMELINE_EVIDENCE_MAX: usize = 32;

/// The five-value filter vocabulary (08 §4.2, verbatim from the
/// DeviceRail `TimelineFilter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RunTimelineFilter {
    /// Every event (structural events are ONLY here).
    All,
    /// `observationRecorded`.
    Observations,
    /// `actionIntent` + `actionSettled` (intent/terminal pairs).
    Actions,
    /// Failed/cancelled/timedOut settlements + `handlerTriggered(onError)`.
    Errors,
    /// `preflightProbed` + `assertionEvaluated` + `verdictRecorded`.
    Verdicts,
}

impl RunTimelineFilter {
    /// The pinned event→filter mapping (08 §4.2 table; UI and API share
    /// this one function).
    pub fn admits(self, payload: &RunLogPayload) -> bool {
        match self {
            RunTimelineFilter::All => true,
            RunTimelineFilter::Observations => {
                matches!(payload, RunLogPayload::ObservationRecorded { .. })
            }
            RunTimelineFilter::Actions => matches!(
                payload,
                RunLogPayload::ActionIntent { .. } | RunLogPayload::ActionSettled { .. }
            ),
            RunTimelineFilter::Errors => match payload {
                RunLogPayload::ActionSettled { outcome, .. } => {
                    !matches!(outcome, ActionOutcome::Succeeded { .. })
                }
                RunLogPayload::HandlerTriggered { hook, .. } => *hook == HandlerHook::OnError,
                _ => false,
            },
            RunTimelineFilter::Verdicts => matches!(
                payload,
                RunLogPayload::PreflightProbed { .. }
                    | RunLogPayload::AssertionEvaluated { .. }
                    | RunLogPayload::VerdictRecorded { .. }
            ),
        }
    }
}

/// One evidence reference on a timeline entry — reference ONLY, never
/// bytes (08 §4.3); dereference goes through the evidence route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimelineEvidenceRef {
    /// Provider-side asset id.
    pub id: String,
    /// Media type of the referenced bytes.
    pub media_type: String,
    /// Content address of the localized bytes (bare lowercase hex).
    pub sha256: String,
}

/// Typed error surface on error-class entries (08 §4.2: ErrorClass +
/// `ErrorInfo{code, message, retryable}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimelineErrorView {
    /// Closed taxonomy class, when the settlement carried one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    /// Provider error code (open set).
    pub code: String,
    /// Provider error message (bounded).
    pub message: String,
    /// Whether the provider marked the error retryable.
    pub retryable: bool,
}

/// The typed per-event detail of one entry. A closed enum mirroring the
/// 17-variant RunLog union, carrying only bounded summaries — full
/// payloads stay in the dossier (08 §4.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TimelineDetail {
    /// Run opened.
    #[serde(rename_all = "camelCase")]
    RunStarted {
        /// Supervision policy, explicit `null` when unsupervised (R13).
        supervise_policy: Option<String>,
    },
    /// Step span opened.
    #[serde(rename_all = "camelCase")]
    StepEntered {
        /// The entered step.
        step_id: String,
    },
    /// Resume-drift probes evaluated.
    #[serde(rename_all = "camelCase")]
    PreflightProbed {
        /// Probe outcome counts as `pass/fail/unknown`.
        pass: u32,
        /// Failed probes.
        fail: u32,
        /// Unknown probes.
        unknown: u32,
        /// The step declared NO probes and this resume re-touched the world
        /// without checking it (07 §4.2 rule 1 / I3: 「没有探针的 resume 在
        /// 报告中标 `unprobed`，而不是假装校验过」). Stated as a flag, not
        /// left to be inferred from three zeroes — the whole point of the
        /// rule is that a reader sees it said.
        #[serde(default, skip_serializing_if = "core::ops::Not::not")]
        unprobed: bool,
    },
    /// WAL intent (dispatch about to happen).
    #[serde(rename_all = "camelCase")]
    ActionIntent {
        /// The dispatch correlation id.
        call_id: String,
        /// Bounded argument snapshot digest.
        args: BoundedValue,
    },
    /// Action reached a terminal (four-way, never folded — 08 §2.5).
    #[serde(rename_all = "camelCase")]
    ActionSettled {
        /// The dispatch correlation id.
        call_id: String,
        /// Terminal discriminant (`succeeded/failed/cancelled/timedOut`).
        outcome: String,
        /// Execution mode when reported (`coordinateFallback` highlighted
        /// by renderers).
        #[serde(skip_serializing_if = "Option::is_none")]
        execution_mode: Option<String>,
        /// Daemon-side degradation reason, when any.
        #[serde(skip_serializing_if = "Option::is_none")]
        fallback_reason: Option<String>,
        /// Error surface on non-success terminals.
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<TimelineErrorView>,
    },
    /// Observation captured (legal omissions shown as-is).
    #[serde(rename_all = "camelCase")]
    ObservationRecorded {
        /// Observation id.
        observation_id: String,
        /// Capture wall-clock.
        captured_at_ms: u64,
        /// Screenshot omission reason, when omitted.
        #[serde(skip_serializing_if = "Option::is_none")]
        screenshot_omission: Option<String>,
        /// uiSnapshot omission reason, when omitted.
        #[serde(skip_serializing_if = "Option::is_none")]
        ui_snapshot_omission: Option<String>,
    },
    /// One assertion evaluated.
    #[serde(rename_all = "camelCase")]
    AssertionEvaluated {
        /// The assertion.
        assert_id: String,
        /// Three-valued result.
        result: String,
        /// Channel that completed evaluation, when one did.
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
        /// Bounded reason text.
        reason: String,
    },
    /// Step verdict recorded (re-judgements append, never replace).
    #[serde(rename_all = "camelCase")]
    VerdictRecorded {
        /// Three-valued status.
        status: String,
        /// Degraded-verification marker.
        degraded: bool,
        /// Superseded verdict id, on re-judgement.
        #[serde(skip_serializing_if = "Option::is_none")]
        supersedes: Option<String>,
        /// Bounded verdict summary.
        summary: String,
        /// Bounded `verdict.record` write-back failure — remote archival
        /// failed; the local verdict is untouched (04 §5).
        #[serde(skip_serializing_if = "Option::is_none")]
        remote_archival_error: Option<String>,
    },
    /// Step span closed.
    #[serde(rename_all = "camelCase")]
    StepExited {
        /// Exit state (closed vocabulary).
        state: StepState,
    },
    /// Call frame pushed (subflow entered).
    #[serde(rename_all = "camelCase")]
    CallFramePushed {
        /// Callee identity `flowId@sha256:…`.
        callee: String,
        /// The frame was RE-ENTERED under a repaired callee rather than
        /// opened (07 §5.2 case (a) down-drill). Absent on every ordinary
        /// push and on every pre-incorporation ledger.
        #[serde(default, skip_serializing_if = "core::ops::Not::not")]
        rebase: bool,
    },
    /// Call frame popped (subflow returned).
    #[serde(rename_all = "camelCase")]
    CallFramePopped {
        /// Whether the callee returned outputs.
        has_outputs: bool,
    },
    /// Handler consulted (hook fired).
    #[serde(rename_all = "camelCase")]
    HandlerTriggered {
        /// The hook.
        hook: String,
        /// Trigger ordinal for the instance.
        trigger: u64,
        /// Declared disposition head (03 §1.8), when recorded.
        #[serde(skip_serializing_if = "Option::is_none")]
        disposition: Option<String>,
    },
    /// Human request opened.
    #[serde(rename_all = "camelCase")]
    HumanRequested {
        /// Pairing id.
        request_id: String,
        /// `step` vs `supervision` (R13).
        purpose: String,
        /// Interaction mode on step requests.
        #[serde(skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        /// Bounded prompt text.
        prompt: String,
    },
    /// Human response recorded through the arbitration door.
    #[serde(rename_all = "camelCase")]
    HumanResponded {
        /// Pairing id.
        request_id: String,
        /// `step` vs `supervision`.
        purpose: String,
        /// Responding actor.
        actor: String,
        /// Bounded response digest.
        response: BoundedValue,
    },
    /// Run suspended (timeline shows the lineage divider — 08 §4.1).
    #[serde(rename_all = "camelCase")]
    RunSuspended {
        /// Suspension reason, explicit `null` when unstated.
        reason: Option<String>,
    },
    /// Run resumed with an alignment report.
    #[serde(rename_all = "camelCase")]
    RunResumed {
        /// Per-class alignment counts, in class order.
        alignment: BoundedValue,
        /// Supervision policy of the new segment (explicit `null` when
        /// unsupervised — never inherited, R13).
        supervise_policy: Option<String>,
    },
    /// Run reached its terminal.
    #[serde(rename_all = "camelCase")]
    RunFinished {
        /// Flow verdict status; absent = finished unverified/aborted.
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        /// Degraded marker of the flow verdict.
        #[serde(skip_serializing_if = "Option::is_none")]
        degraded: Option<bool>,
        /// Bounded `verdict.record` write-back failure of the flow
        /// verdict (04 §5 — see `verdictRecorded`).
        #[serde(skip_serializing_if = "Option::is_none")]
        remote_archival_error: Option<String>,
    },
    /// Fail-closed placeholder: the event exceeded the bounded-rendering
    /// limits even after truncation; the full record is in the dossier
    /// (08 §4.4 — no half entries).
    #[serde(rename_all = "camelCase")]
    OverLimit {
        /// The original event discriminant.
        event_type: String,
    },
}

/// A bounded JSON digest (08 §4.4): truncation is always explicit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoundedValue {
    /// The digest value (depth- and size-bounded).
    pub value: Value,
    /// Whether anything was cut while bounding.
    pub truncated: bool,
}

/// One timeline entry (spine §10.1 `RunTimelineEntry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunTimelineEntry {
    /// Ledger sequence — the ordering authority (08 §4.1).
    pub seq: u64,
    /// Wall clock, informational only.
    pub at_ms: u64,
    /// Canonical run-path string of the event.
    pub run_path: String,
    /// Bounded typed detail (discriminant = RunLog event type).
    pub detail: TimelineDetail,
    /// Evidence references (≤ 32; pure references — 08 §4.3).
    pub evidence: Vec<TimelineEvidenceRef>,
    /// How many evidence references were dropped by the cap.
    pub evidence_omitted: u32,
    /// Whether any field of this entry was truncated while bounding.
    pub truncated: bool,
}

/// One synchronous timeline page (08 §4.4: pagination never moves seen
/// entries; `revision` tells the client when to re-pull).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimelinePage {
    /// Protocol version (spine §10.3).
    pub projection_version: ProjectionVersion,
    /// The projected run.
    pub run_id: String,
    /// Filter applied server-side.
    pub filter: RunTimelineFilter,
    /// 1-based page number.
    pub page: u32,
    /// Effective page size (≤ 50; requests can only shrink it).
    pub page_size: u32,
    /// Snapshot revision (= max ledger seq at query time).
    pub revision: u64,
    /// Total entries admitted by the filter.
    pub total: u64,
    /// The page.
    pub entries: Vec<RunTimelineEntry>,
}

/// Projects one timeline page. `page` is 1-based; `page_size` is clamped
/// to the hard cap (08 §4.4).
pub fn timeline_page(
    store: &Store,
    run_id: &str,
    filter: RunTimelineFilter,
    page: u32,
    page_size: u32,
) -> Result<TimelinePage, StoreError> {
    let events = store.events(run_id)?;
    let revision = events.last().map(|event| event.seq).unwrap_or(0);

    // The runner-recorded ErrorClass lives on the checkpoint's attempt
    // records, keyed by callId (08 §4.2: error entries carry ErrorClass +
    // ErrorInfo); joined here so the timeline never re-derives taxonomy.
    let mut error_classes: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    if let Some((_, view)) = store.materialized_checkpoint(run_id)? {
        for record in &view.completed {
            for attempt in &record.attempts {
                if let Some(class) = &attempt.error_class {
                    error_classes.insert(attempt.call_id.clone(), wire(class));
                }
            }
        }
    }

    let admitted: Vec<&RunLogEvent> = events
        .iter()
        .filter(|event| filter.admits(&event.payload))
        .collect();
    let page = page.max(1);
    let page_size = page_size.clamp(1, TIMELINE_MAX_PAGE_SIZE);
    let start = (page as usize - 1).saturating_mul(page_size as usize);
    let entries = admitted
        .iter()
        .skip(start)
        .take(page_size as usize)
        .map(|event| entry_of(event, &error_classes))
        .collect();
    Ok(TimelinePage {
        projection_version: ProjectionVersion,
        run_id: run_id.to_owned(),
        filter,
        page,
        page_size,
        revision,
        total: admitted.len() as u64,
        entries,
    })
}

/// Serializes a unit-enum value to its wire literal.
fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Truncates a string at the byte cap on a char boundary; explicit flag.
fn bound_text(text: &str) -> (String, bool) {
    if text.len() <= TIMELINE_TEXT_MAX_BYTES {
        return (text.to_owned(), false);
    }
    let mut cut = TIMELINE_TEXT_MAX_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (text[..cut].to_owned(), true)
}

/// Depth-bounds a JSON value; deeper structure collapses to a marker
/// string (explicitly visible, never silent).
fn bound_depth(value: &Value, depth: usize, truncated: &mut bool) -> Value {
    if depth == 0 {
        *truncated = true;
        return Value::String("…depth truncated".to_owned());
    }
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), bound_depth(v, depth - 1, truncated)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| bound_depth(v, depth - 1, truncated))
                .collect(),
        ),
        Value::String(text) => {
            let (bounded, cut) = bound_text(text);
            if cut {
                *truncated = true;
            }
            Value::String(bounded)
        }
        other => other.clone(),
    }
}

/// Bounds a JSON value by depth then by serialized size (08 §4.4). When
/// even the depth-bounded form exceeds the byte cap, the whole value
/// fails closed to a marker.
fn bound_value(value: &Value) -> BoundedValue {
    let mut truncated = false;
    let bounded = bound_depth(value, TIMELINE_JSON_MAX_DEPTH, &mut truncated);
    let size = serde_json::to_string(&bounded)
        .map(|s| s.len())
        .unwrap_or(0);
    if size > TIMELINE_JSON_MAX_BYTES {
        return BoundedValue {
            value: Value::String("…over 16 KiB, see the step dossier".to_owned()),
            truncated: true,
        };
    }
    BoundedValue {
        value: bounded,
        truncated,
    }
}

/// Builds one bounded entry from one ledger event. `error_classes` is
/// the checkpoint's callId → ErrorClass join (08 §4.2).
fn entry_of(
    event: &RunLogEvent,
    error_classes: &std::collections::BTreeMap<String, String>,
) -> RunTimelineEntry {
    let mut truncated = false;
    let mut evidence: Vec<TimelineEvidenceRef> = Vec::new();
    let mut evidence_omitted = 0u32;

    let detail = match &event.payload {
        RunLogPayload::RunStarted {
            supervise_policy, ..
        } => TimelineDetail::RunStarted {
            supervise_policy: supervise_policy.as_ref().map(wire),
        },
        RunLogPayload::StepEntered { step_id, .. } => TimelineDetail::StepEntered {
            step_id: step_id.to_string(),
        },
        RunLogPayload::PreflightProbed { outcomes } => {
            let count = |status: pointlock_ir::VerdictStatus| {
                outcomes.iter().filter(|o| o.result == status).count() as u32
            };
            TimelineDetail::PreflightProbed {
                pass: count(pointlock_ir::VerdictStatus::Pass),
                fail: count(pointlock_ir::VerdictStatus::Fail),
                unknown: count(pointlock_ir::VerdictStatus::Unknown),
                // `preflight` is `minItems: 1` in the schema, so a declared
                // probe list never evaluates to zero outcomes — an empty one
                // can only mean the step declared none.
                unprobed: outcomes.is_empty(),
            }
        }
        RunLogPayload::ActionIntent {
            call_id,
            args_snapshot,
            ..
        } => {
            let args = bound_value(args_snapshot);
            truncated |= args.truncated;
            TimelineDetail::ActionIntent {
                call_id: call_id.clone(),
                args,
            }
        }
        RunLogPayload::ActionSettled { call_id, outcome } => {
            let (execution_mode, fallback_reason) = match outcome {
                ActionOutcome::Succeeded { result } => {
                    for asset in &result.evidence {
                        push_evidence(asset, &mut evidence, &mut evidence_omitted);
                    }
                    match result.execution.as_ref() {
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
                    }
                }
                _ => (None, None),
            };
            let error = match outcome {
                ActionOutcome::Succeeded { .. } => None,
                ActionOutcome::Failed { error }
                | ActionOutcome::Cancelled { error }
                | ActionOutcome::TimedOut { error } => {
                    let (message, cut) = bound_text(&error.message);
                    truncated |= cut;
                    Some(TimelineErrorView {
                        error_class: error_classes.get(call_id).cloned(),
                        code: error.code.clone(),
                        message,
                        retryable: error.retryable,
                    })
                }
            };
            TimelineDetail::ActionSettled {
                call_id: call_id.clone(),
                outcome: outcome.kind().to_owned(),
                execution_mode,
                fallback_reason,
                error,
            }
        }
        RunLogPayload::ObservationRecorded { observation } => {
            if let Some(evidence_ref) = &observation.screenshot {
                push_evidence(&evidence_ref.asset, &mut evidence, &mut evidence_omitted);
            }
            if let Some(evidence_ref) = &observation.ui_snapshot {
                push_evidence(&evidence_ref.asset, &mut evidence, &mut evidence_omitted);
            }
            TimelineDetail::ObservationRecorded {
                observation_id: observation.observation_id.clone(),
                captured_at_ms: observation.captured_at_ms,
                screenshot_omission: observation.screenshot_omission.as_ref().map(wire),
                ui_snapshot_omission: observation.ui_snapshot_omission.as_ref().map(wire),
            }
        }
        RunLogPayload::AssertionEvaluated { outcome } => {
            let (reason, cut) = bound_text(&outcome.reason);
            truncated |= cut;
            TimelineDetail::AssertionEvaluated {
                assert_id: outcome.assert_id.to_string(),
                result: wire(&outcome.result),
                channel: outcome.channel.as_ref().map(wire),
                reason,
            }
        }
        RunLogPayload::VerdictRecorded {
            verdict,
            remote_archival_error,
            ..
        } => {
            let (summary, cut) = bound_text(&verdict.summary);
            truncated |= cut;
            for asset in &verdict.evidence {
                push_evidence(asset, &mut evidence, &mut evidence_omitted);
            }
            let remote_archival_error = remote_archival_error.as_deref().map(|error| {
                let (bounded, cut) = bound_text(error);
                truncated |= cut;
                bounded
            });
            TimelineDetail::VerdictRecorded {
                status: wire(&verdict.status),
                degraded: verdict.degraded,
                supersedes: verdict.supersedes.clone(),
                summary,
                remote_archival_error,
            }
        }
        RunLogPayload::StepExited { state, .. } => TimelineDetail::StepExited { state: *state },
        RunLogPayload::CallFramePushed { frame, rebase } => TimelineDetail::CallFramePushed {
            callee: format!("{}@{}", frame.flow_id, frame.ir_hash),
            rebase: *rebase,
        },
        RunLogPayload::CallFramePopped { outputs } => TimelineDetail::CallFramePopped {
            has_outputs: outputs.is_some(),
        },
        RunLogPayload::HandlerTriggered {
            hook,
            trigger,
            disposition,
        } => TimelineDetail::HandlerTriggered {
            hook: wire(hook),
            trigger: *trigger,
            disposition: disposition.clone(),
        },
        RunLogPayload::HumanRequested {
            request_id,
            purpose,
            mode,
            prompt,
            ..
        } => {
            let (prompt, cut) = bound_text(prompt);
            truncated |= cut;
            TimelineDetail::HumanRequested {
                request_id: request_id.clone(),
                purpose: wire(purpose),
                mode: mode.as_ref().map(wire),
                prompt,
            }
        }
        RunLogPayload::HumanResponded {
            request_id,
            purpose,
            response,
            actor,
        } => {
            let response = bound_value(response);
            truncated |= response.truncated;
            TimelineDetail::HumanResponded {
                request_id: request_id.clone(),
                purpose: wire(purpose),
                actor: actor.clone(),
                response,
            }
        }
        RunLogPayload::RunSuspended { reason, .. } => TimelineDetail::RunSuspended {
            reason: reason.clone(),
        },
        RunLogPayload::RunResumed {
            alignment_report,
            supervise_policy,
            ..
        } => {
            let counts = serde_json::json!({
                "reusable": count_class(alignment_report, pointlock_ir::AlignmentClass::Reusable),
                "judgeDirty": count_class(alignment_report, pointlock_ir::AlignmentClass::JudgeDirty),
                "effectDirty": count_class(alignment_report, pointlock_ir::AlignmentClass::EffectDirty),
                "new": count_class(alignment_report, pointlock_ir::AlignmentClass::New),
                "orphaned": count_class(alignment_report, pointlock_ir::AlignmentClass::Orphaned),
            });
            TimelineDetail::RunResumed {
                alignment: bound_value(&counts),
                supervise_policy: supervise_policy.as_ref().map(wire),
            }
        }
        RunLogPayload::RunFinished {
            verdict,
            remote_archival_error,
        } => {
            let remote_archival_error = remote_archival_error.as_deref().map(|error| {
                let (bounded, cut) = bound_text(error);
                truncated |= cut;
                bounded
            });
            TimelineDetail::RunFinished {
                status: verdict.as_ref().map(|v| wire(&v.status)),
                degraded: verdict.as_ref().map(|v| v.degraded),
                remote_archival_error,
            }
        }
    };

    // Fail-closed over-limit gate: a bounded entry that still serializes
    // over the JSON cap collapses to a placeholder (08 §4.4).
    let serialized = serde_json::to_string(&detail).map(|s| s.len()).unwrap_or(0);
    let detail = if serialized > TIMELINE_JSON_MAX_BYTES {
        truncated = true;
        TimelineDetail::OverLimit {
            event_type: event.payload.event_type().to_owned(),
        }
    } else {
        detail
    };

    RunTimelineEntry {
        seq: event.seq,
        at_ms: event.at_ms,
        run_path: render_run_path(&event.run_path),
        detail,
        evidence,
        evidence_omitted,
        truncated,
    }
}

fn count_class(
    report: &pointlock_ir::AlignmentReport,
    class: pointlock_ir::AlignmentClass,
) -> usize {
    report
        .entries
        .iter()
        .filter(|entry| entry.class == class)
        .count()
}

fn push_evidence(
    asset: &pointlock_ir::AssetRef,
    evidence: &mut Vec<TimelineEvidenceRef>,
    omitted: &mut u32,
) {
    if evidence.len() >= TIMELINE_EVIDENCE_MAX {
        *omitted += 1;
        return;
    }
    evidence.push(TimelineEvidenceRef {
        id: asset.id.clone(),
        media_type: asset.media_type.clone(),
        sha256: asset.sha256.clone().unwrap_or_default(),
    });
}
