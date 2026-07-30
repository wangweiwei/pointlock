//! The M2 execution engine: tree-walking sequential execution of the
//! control-flow step vocabulary (action/call/if/foreach/let/assert) with
//! the spine §6.1/§6.2 event-order discipline.
//!
//! Ledger discipline per action step (verbatim order, spine §6.1 M1 note):
//! ready (argument snapshot frozen) → `stepEntered` (carries the step's
//! effect/judge hashes and the resolved-inputs snapshot) → probing
//! (declared `preflight` only: fresh observe material → `preflightProbed`)
//! → `actionIntent` (own transaction, fsynced *before* dispatch) →
//! `provider.execute` → `actionSettled` → error classification (spine §5)
//! → `observationRecorded` (evidence localized first, file-before-row-
//! before-log) → `assertionEvaluated` per assertion → `verdictRecorded`
//! (when a verdict exists) + `ProviderSession::record_verdict` write-back →
//! `stepExited` (carries the projected output, when one exists).
//!
//! Control-flow steps (M2):
//! - `call` (07 §1): call-by-value in both directions — inputs evaluated in
//!   the caller scope, snapshotted and schema-gated inbound; the callee
//!   body runs in a fresh frame (`params` = inputs, `env` read-only
//!   pass-through, the caller's steps/vars invisible — 07 §1.2 verbatim);
//!   declared outputs evaluated in the callee scope and schema-gated
//!   outbound; `callFramePushed`/`callFramePopped` bracket the frame; the
//!   call step's verdict *is* the callee's flow verdict (spine §6.3).
//! - `if`: strict-boolean `cond`; the unselected branch's steps each leave
//!   an `entered(resolvedInputs: null)`/`exited(skipped)` pair (the
//!   blocked precedent — ledger completeness); containers yield no verdict
//!   of their own (R4).
//! - `foreach`: `items` must evaluate to an array; each round runs the body
//!   under an `iteration` path frame (`[i]`) with `iter.<as>` bound; the
//!   `stepEntered` snapshot carries `{ items, as }` (the fold's IterState
//!   carrier and the resume-time position authority — 07 §4.6).
//! - `let`: pure bindings into the frame's `vars.*` (SSA; rebinding is a
//!   compiler-refused shape — the runtime check is a defense line).
//! - `assert`: `observe: "fresh"` captures via `session.observe` and goes
//!   through the same localization as action observations; `fromStep`
//!   replays the archived material of a prior action step — zero device
//!   I/O.
//!
//! Evidence-localization degradation (M2): a failure to localize
//! (`fetch_evidence` unsupported, stream rupture, integrity mismatch,
//! `ui.snapshot.get` failure) never aborts the run — the observation
//! record keeps the affected field absent and the dependent verify channel
//! receives a typed gap, degrading honestly toward `unknown` (principle 4).
//!
//! The stop token is honored at step boundaries at any depth
//! (`runSuspended` → [`RunOutcome::Suspended`]); suspension leaves the
//! open spans and live frames in place, and resume walks back into the
//! exact frame position (07 §4.6) by adopting completed step instances
//! path-by-path.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use futures_util::future::LocalBoxFuture;
use pointlock_expr::Scope;
use pointlock_ir::{
    ActionExecution, ActionOutcome, ActionResult, ActionStepIR, AssertStepIR, AssertionIR,
    AssetRef, BoundAttempt, CallFrame, CallStepIR, EffectClassAction, ErrorClass, ErrorInfo,
    EvidenceRef, ExecutionMode, FlowIR, ForeachStepIR, HandlerAction, HandlerBinding, HandlerHook,
    HumanMode, HumanPending, HumanPurpose, HumanStepIR, IfStepIR, LetStepIR, Observation,
    ObservationRecord, ObservationSource, ParamDecl, PathFrame, Phase, PredicateIR,
    ProviderStateSummary, RetryPolicy, RunLogPayload, RunPath, StepBase, StepIR, StepId,
    StepRecord, StepState, SupervisePolicy, UiSnapshotOmissionReason, Verdict, VerdictStatus,
    VerifyChannel, render_run_path, to_canonical_json,
};
use pointlock_provider_kit::{
    BoundActionCall, CancellationToken, ObserveRequest, ObserveWant, ProviderSession,
    SessionOutcome, UiSnapshotOutcome, VERDICT_EVIDENCE_MAX_ENTRIES, VERDICT_SUMMARY_MAX_CHARS,
    VerdictWrite,
};
use pointlock_store::Store;
use pointlock_vision::VisionVerifier;
use serde_json::{Map, Value};

use crate::error::{BlockedReason, RunnerError};
use crate::judge::{
    FoldedVerdict, eval_expr_assertion, fold_flow_verdict, fold_step_verdict, project_output,
};
use crate::load::{LoadedFlow, MAX_CALL_DEPTH};
use crate::observe_eval::{
    EvaluatedAssertion, ObserveMaterial, eval_observed_assertion, material_from_observation,
};

/// Terminal outcome of `Runner::run` / `Runner::resume`.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    /// The run reached `runFinished`. `verdict` is the folded flow verdict;
    /// absent when no step produced a verdict (all-unverified flows) or
    /// when the run was aborted by a `cancelled` action terminal.
    Finished {
        /// The folded flow verdict, when one exists.
        verdict: Option<Verdict>,
    },
    /// The run reached `runSuspended` (stop token at a step boundary, or a
    /// provider error left an action without a terminal — resume
    /// reconciles it).
    Suspended,
    /// The run cannot proceed without a human decision: a drifted
    /// preflight whose `onResumeDrift` ladder is exhausted (or absent), or
    /// a reconcile adjudication that could not even be requested (defense
    /// line). The store records a `runSuspended` with the reason.
    Blocked {
        /// Why a human is required.
        reason: BlockedReason,
    },
    /// A human interaction (human step or R13 supervision gate) is
    /// pending: `humanRequested` is fsynced and `runSuspended` recorded —
    /// the runner never blocks waiting. The attached-TTY inline experience
    /// lives in the CLI layer (collect a response through the store
    /// arbitration, then resume in the same process); resume settles the
    /// paired response, re-awaits an unanswered one, or lazily settles an
    /// expired deadline to `unknown` (06 §5.3).
    AwaitingHuman {
        /// The pending request (also materialized in
        /// `CheckpointView.humanPending`).
        pending: HumanPending,
    },
}

/// Shared deadline of the two live capture RPCs (`health()` +
/// `currentCursor()`, 07 §2.2): capture failures degrade the affected
/// fields — they never block the suspend/exit path.
const SUMMARY_CAPTURE_BUDGET_MS: u64 = 2_000;

/// Captures the failure/suspension-instant provider profile (07 §2.2,
/// incorporated 2026-07-18). Pure forensics: nothing downstream may
/// consume it as a control input. Every failure mode degrades honestly:
/// a failed `health()` records `{ ok: false, degraded: <class> }`, a
/// failed `currentCursor()` leaves the cursor absent (never a stale
/// bind-time value — principle 4).
pub(crate) async fn capture_provider_state_summary(
    session: &dyn ProviderSession,
    known_lineage: &[String],
    device_id: &str,
    platform: Option<&str>,
) -> ProviderStateSummary {
    let budget = std::time::Duration::from_millis(SUMMARY_CAPTURE_BUDGET_MS);
    let started = std::time::Instant::now();
    let health = match tokio::time::timeout(budget, session.health()).await {
        Ok(Ok(health)) => pointlock_ir::SessionHealthSnapshot {
            ok: health.ok,
            degraded: health.degraded,
        },
        Ok(Err(error)) => pointlock_ir::SessionHealthSnapshot {
            ok: false,
            degraded: Some(
                serde_json::to_value(error.error_class)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".to_owned()),
            ),
        },
        Err(_) => pointlock_ir::SessionHealthSnapshot {
            ok: false,
            degraded: Some("capture_timeout".to_owned()),
        },
    };
    let remaining = budget.saturating_sub(started.elapsed());
    let event_cursor = match tokio::time::timeout(remaining, session.current_cursor()).await {
        Ok(Ok(cursor)) => Some(cursor),
        _ => None,
    };
    let attestation = session.attestation();
    let mut session_lineage = known_lineage.to_vec();
    if let Some(cursor) = &event_cursor
        && session_lineage.last() != Some(&cursor.session_id)
    {
        session_lineage.push(cursor.session_id.clone());
    }
    ProviderStateSummary {
        session_lineage,
        event_cursor,
        attestation: pointlock_ir::AttestationSnapshot {
            lockfile_digest: attestation.lockfile_digest.clone(),
            attested_at: attestation.attested_at.clone(),
        },
        health,
        device_id: device_id.to_owned(),
        platform: platform.map(str::to_owned),
    }
}

/// The manifest of a locally minted human-evidence asset (item ③):
/// localized by construction (put_evidence wrote the bytes before the
/// verdict cites them).
/// The cited/manifest pair of an escalate ruling's verdict (06 §6): the
/// settlement document when the ruling carries one, empty otherwise.
fn escalate_verdict_material(evidence: &Option<AssetRef>) -> (Vec<AssetRef>, EvidenceManifest) {
    match evidence {
        Some(asset) => (vec![asset.clone()], human_manifest(asset)),
        None => (Vec::new(), EvidenceManifest::default()),
    }
}

fn human_manifest(asset: &AssetRef) -> EvidenceManifest {
    EvidenceManifest {
        localized: vec![pointlock_ir::EvidenceRef {
            asset: asset.clone(),
            sha256: asset.sha256.clone().unwrap_or_default(),
            local_path: asset.uri.clone(),
        }],
        gaps: Vec::new(),
    }
}

/// One judgment's settlement-evidence localization outcome (item ③):
/// what landed and what typed-failed. Rides the `verdictRecorded`
/// payload; observation assets are excluded (they ride
/// `observationRecorded`).
#[derive(Debug, Clone, Default)]
pub(crate) struct EvidenceManifest {
    /// Localized copies (evidence table + `localized` payload field).
    pub localized: Vec<pointlock_ir::EvidenceRef>,
    /// Typed failures (`localizationGaps` payload field).
    pub gaps: Vec<pointlock_ir::EvidenceGap>,
}

/// The act-chain re-entry position of a crash-resume (item ②, 07 §1.4:
/// resume lands at the precise position, never restarts the chain): the
/// recorded 1-based `chainIndex` maps to its 0-based enumeration slot;
/// an out-of-range index is a TYPED refusal (never a guessed position —
/// unreachable via the shipped resume rules); a pre-incorporation
/// ledger (no index) falls back to the head — the pre-ruling behavior,
/// honest under principle 4.
pub(crate) fn chain_start(
    chain_index: Option<u32>,
    step: &ActionStepIR,
) -> Result<usize, RunnerError> {
    match chain_index {
        None => Ok(0),
        Some(index) if index >= 1 && ((index - 1) as usize) < step.binding.attempts.len() => {
            Ok((index - 1) as usize)
        }
        Some(index) => Err(RunnerError::M0Unsupported {
            detail: format!(
                "the pending intent's recorded chainIndex {index} does not exist in the \
                 resumed step's binding chain (len {}); refusing to guess a re-entry \
                 position (unreachable through the shipped resume rules — same-IR chains \
                 cannot shrink and effect-dirty repairs never adopt/replay)",
                step.binding.attempts.len()
            ),
        }),
    }
}

/// SPI ingestion quarantine (M3a viewport review): `Viewport.scaleFactor`
/// is the only f64 in the durable event domain, and serde_json writes a
/// non-finite f64 as `null` — a value the ledger would never read back
/// (every later refold/verify/projection of the run fails permanently).
/// A `succeeded` terminal embedding one is a provider contract violation:
/// it is recorded as a *final failure* with a precise code instead — the
/// honest ledger fact ("the provider reported an unpersistable
/// terminal"), taking the ordinary failure path (handlers may escalate)
/// rather than poisoning the ledger or falsifying the observation.
pub(crate) fn quarantine_unpersistable(outcome: ActionOutcome) -> ActionOutcome {
    let poisoned = match &outcome {
        ActionOutcome::Succeeded { result } => result
            .before
            .iter()
            .chain(result.after.iter())
            .find(|observation| !observation.viewport.scale_factor.is_finite()),
        _ => None,
    };
    match poisoned {
        Some(observation) => ActionOutcome::Failed {
            error: ErrorInfo {
                code: "observation_viewport_invalid".to_owned(),
                message: format!(
                    "provider contract violation: observation {} carries a non-finite \
                     viewport scaleFactor, which cannot be persisted",
                    observation.id
                ),
                retryable: false,
                details: None,
            },
        },
        None => outcome,
    }
}

/// Milliseconds since the Unix epoch (informational `atMs` on events).
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// The root run path of a flow (hard rule: flow frames carry the irHash).
pub(crate) fn root_path(flow: &FlowIR) -> RunPath {
    vec![PathFrame::Flow {
        flow_id: flow.flow_id.clone(),
        ir_hash: flow.ir_hash.clone(),
    }]
}

/// Extracts the last attempt number of a run path.
pub(crate) fn attempt_of(path: &RunPath) -> Option<u64> {
    path.iter().rev().find_map(|frame| match frame {
        PathFrame::Attempt { n } => Some(*n),
        _ => None,
    })
}

/// The IR-version-independent identity of a step *instance*: stepId path
/// plus iteration indexes, with hashes and attempt/phase suffixes
/// stripped. Stable across a repair (irHash change), unique within a run
/// (stepIds are flow-unique; iterations disambiguate rounds). Used to key
/// adoption, open spans, and attempt watermarks.
pub(crate) fn instance_key(path: &[PathFrame]) -> String {
    // The per-attempt suffix (attempt/phase/assertion frames) is never
    // part of the instance identity — but only the TRAILING run of such
    // frames is a suffix. Stripping trailing-only (instead of breaking
    // at the first attempt frame) is byte-identical for every path shape
    // the engine produces today, and stops aliasing distinct
    // interior-attempt instances once the ruled attempt-framed call
    // re-entry of 07 §1 lands (interior attempts render as `#n`).
    let trimmed = {
        let mut end = path.len();
        while end > 0
            && matches!(
                path[end - 1],
                PathFrame::Attempt { .. } | PathFrame::Phase { .. } | PathFrame::Assertion { .. }
            )
        {
            end -= 1;
        }
        &path[..end]
    };
    let mut key = String::new();
    for frame in trimmed {
        match frame {
            PathFrame::Flow { .. } => {}
            PathFrame::Step { step_id } => {
                let _ = write!(key, "/{step_id}");
            }
            PathFrame::Call { step_id, .. } => {
                let _ = match step_id {
                    Some(step_id) => write!(key, "/{step_id}"),
                    None => write!(key, "/hook-call"),
                };
            }
            PathFrame::Iteration { index, key: item } => {
                let _ = match item {
                    Some(item) => write!(key, "[{index}:{item}]"),
                    None => write!(key, "[{index}]"),
                };
            }
            PathFrame::Hook { hook, trigger } => {
                let _ = write!(key, "/hook:{hook:?}:{trigger}");
            }
            PathFrame::Attempt { n } => {
                let _ = write!(key, "#{n}");
            }
            PathFrame::Phase { .. } | PathFrame::Assertion { .. } => {}
        }
    }
    key
}

/// The path frame a step contributes below its parent prefix: call steps
/// contribute a `call` frame (one frame, two rendered segments — 07 §2.1),
/// every other kind a plain `step` frame.
pub(crate) fn child_frame(step: &StepIR) -> PathFrame {
    match step {
        StepIR::Call(call) => PathFrame::Call {
            step_id: Some(call.base.step_id.clone()),
            callee_flow_id: call.flow_ref.flow_id.clone(),
            callee_ir_hash: call.flow_ref.ir_hash.clone(),
        },
        other => PathFrame::Step {
            step_id: other.step_id().clone(),
        },
    }
}

/// Mid-flight work for the frontier step of a resume (07 §4.4 decision
/// table). All variants imply the step's `stepEntered` span is already
/// open in the log — the engine must not re-enter it.
pub(crate) enum FrontierWork {
    /// `reconcile → completed`: adopt the archived terminal — append the
    /// `actionSettled` the crash swallowed, then dispose it through the
    /// exact same settled-outcome path as a live execute (§6.7-B).
    Adopt {
        /// The reconciled callId.
        call_id: String,
        /// The run path of the original `actionIntent` (the settle anchors
        /// to the same attempt).
        intent_path: RunPath,
        /// The archived terminal outcome, verbatim.
        outcome: Box<ActionOutcome>,
        /// The archived `argsSnapshot` (never re-evaluated, spine §6.6).
        args: Value,
        /// The intent's recorded 1-based chain position (item ②): the
        /// act chain re-enters HERE, per 07 §1.4's resume-lands-at-the-
        /// precise-position rule. Absent on pre-incorporation ledgers
        /// (falls back to the chain head — the pre-ruling behavior).
        chain_index: Option<u32>,
    },
    /// `reconcile → neverDispatched` (or an authorized uncertain replay):
    /// dispatch again using the archived args snapshot — never
    /// re-evaluated (spine §6.6).
    Replay {
        /// The archived `argsSnapshot` from the pending intent.
        args: Value,
        /// The intent's recorded chain position (see `Adopt.chain_index`):
        /// the replay re-dispatches THIS attempt — a mid-chain crashed
        /// intent's args belong to that attempt, not the chain head.
        chain_index: Option<u32>,
    },
    /// A human `adopt` adjudication of an uncertain reconcile (07 §4.4):
    /// the ruling says the effect stands, so nothing is dispatched; the
    /// step proceeds straight to the observation-confirmation path
    /// ([`ActPhase::Unconfirmed`]) and its assertions verify the ruled
    /// world.
    ConfirmEffect {
        /// The adjudication context, human-readable.
        message: String,
        /// The archived ready snapshot (the span is open; never
        /// re-evaluated).
        args: Value,
    },
    /// A human `abort` adjudication of an uncertain reconcile (07 §4.4):
    /// close the open span `aborted` and abort the run.
    AbortRuled {
        /// The archived ready snapshot.
        args: Value,
    },
}

/// A reconciled frontier terminal whose WAL intent and `actionSettled` are
/// already on record: the act chain consumes it as the first try's settled
/// outcome instead of dispatching — one disposal code path for live and
/// adopted terminals.
struct AdoptedSettle {
    /// The archived terminal outcome.
    outcome: ActionOutcome,
    /// The `seq` of the appended `actionSettled` event (evidence linking).
    settled_seq: u64,
    /// The attempt number of the original intent.
    attempt_n: u64,
}

/// How a step's execution affects the surrounding body walk.
enum Ctl {
    /// Step concluded (pass/unknown/no-verdict); continue with the next.
    Continue,
    /// A fail verdict: halt — the remaining steps of the current body are
    /// recorded `blocked`, and the halt propagates through enclosing
    /// containers and frames (a callee halt folds into the call step's
    /// verdict, spine §6.3).
    HaltFail,
    /// No terminal could be obtained (transport-class provider error) or a
    /// stop was requested: suspend the run; open spans and live frames
    /// stay open for the frame-precise resume (07 §4.6).
    Suspend(String),
    /// A `cancelled` terminal: the step is recorded `aborted` and the run
    /// finishes without a flow verdict (spine §5).
    Abort,
    /// The run cannot proceed without a human (a drifted preflight whose
    /// `onResumeDrift` ladder is exhausted or absent).
    Blocked(BlockedReason),
    /// A human request is pending (human step or supervision gate): the
    /// run suspends (`runSuspended` after the fsynced `humanRequested`)
    /// and surfaces [`RunOutcome::AwaitingHuman`]. Open spans and live
    /// frames stay open, exactly like `Suspend`.
    AwaitHuman(HumanPending),
}

/// What a handler consultation decided for its host step (spine §3: a
/// disposition, never data — R10).
enum Consulted {
    /// No binding matched, or the trigger budget is exhausted: the
    /// natural path stands.
    None,
    /// Re-enter the failing phase under the handler's retry policy
    /// (budget independent of `StepBase.retry` — spine §6.5 mount 2).
    Retry(RetryPolicy),
    /// Record and release: the verdict stands, downstream is not halted.
    Continue,
    /// Abort the run.
    Abort,
    /// An escalate human superseded the host outcome with this status.
    Escalated {
        status: VerdictStatus,
        summary: String,
        /// The canonical settlement evidence document (06 §6) — cited by
        /// the superseding verdict. Absent only on pre-doc paths.
        evidence: Option<AssetRef>,
    },
    /// An escalate human (repairWorld) declared the world repaired:
    /// re-enter the failing phase once.
    Repaired,
    /// An escalate human is pending: suspend awaiting the response.
    Pending(HumanPending),
    /// The repair subflow completed cleanly: re-enter the failing phase.
    RepairDone,
    /// The repair subflow failed: the host's natural path stands (the
    /// repair flow's own verdict records carry the failure detail).
    RepairFailed,
    /// The repair subflow hit a control outcome (suspend/awaiting-human/
    /// blocked): propagate it.
    Propagate(Ctl),
}

/// The hook audit frame (`/hook:<name>:<n>`, 07 §2.1).
fn hook_frame(hook: HandlerHook, trigger: u64) -> PathFrame {
    PathFrame::Hook { hook, trigger }
}

/// The run path of an escalate hook human: host + hook frame + step frame.
fn hook_child_path(
    step_path: &RunPath,
    hook: HandlerHook,
    trigger: u64,
    human: &HumanStepIR,
) -> RunPath {
    let mut path = step_path.clone();
    path.push(hook_frame(hook, trigger));
    path.push(PathFrame::Step {
        step_id: human.base.step_id.clone(),
    });
    path
}

/// Maps an escalate human's arbitrated response to a consultation outcome
/// (the four-mode table of 06 §2.2, narrowed to the escalate context).
fn map_escalate_response(human: &HumanStepIR, response: &Value) -> Consulted {
    let decision = response
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match human.mode {
        HumanMode::RepairWorld => match decision {
            // 06 §2.1's closed repairWorld vocabulary. In the escalate
            // ladder `done` re-enters the disposition (re-probe / re-act);
            // `cannotRepair` is the human's explicit "the world cannot be
            // brought back" — the run aborts rather than looping on a
            // declared impossibility. The catch-all is defense: the store
            // arbitrates the vocabulary before anything reaches here.
            "done" => Consulted::Repaired,
            _ => Consulted::Abort,
        },
        HumanMode::Confirm => {
            let first = human
                .decisions
                .as_ref()
                .and_then(|labels| labels.first())
                .map(String::as_str);
            let status = if first == Some(decision) {
                VerdictStatus::Pass
            } else {
                VerdictStatus::Fail
            };
            Consulted::Escalated {
                status,
                summary: format!("escalate confirm decision '{decision}' (position-mapped)"),
                evidence: None,
            }
        }
        // Judge (provideInput escalates are refused at load).
        _ => {
            let status = match response.get("status").and_then(Value::as_str) {
                Some("pass") => VerdictStatus::Pass,
                Some("fail") => VerdictStatus::Fail,
                _ => VerdictStatus::Unknown,
            };
            Consulted::Escalated {
                status,
                evidence: None,
                summary: format!(
                    "escalate judge ruling: {}",
                    response
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                ),
            }
        }
    }
}

/// The facts of one `humanRequested` on the ledger, harvested for resume
/// settlement (the log is the truth, I1). A supervision `suspend` answer
/// is non-final and never fills `final_response` (spine §6.9).
#[derive(Debug, Clone)]
pub(crate) struct HumanRequestFact {
    /// The request id a response must pair with.
    pub request_id: String,
    /// The request's anchor path (the awaiting/gated step).
    pub run_path: RunPath,
    /// Step vs supervision gate.
    pub purpose: HumanPurpose,
    /// Interaction mode (`purpose="step"` only).
    pub mode: Option<HumanMode>,
    /// The prompt shown to the human.
    pub prompt: String,
    /// The materialized presents snapshot (cited by the evidence doc).
    pub presents: Value,
    /// Absolute deadline; absent for supervision requests.
    pub deadline_at_ms: Option<u64>,
    /// The paired final response payload, when one was arbitrated.
    pub final_response: Option<Value>,
    /// Who gave the final response.
    pub final_actor: Option<String>,
}

/// Outcome of the act phase (attempt chain + in-attempt retry).
enum ActPhase {
    /// A `succeeded` terminal.
    Succeeded {
        result: Box<ActionResult>,
        /// Whether the provider reported an execution mode outside the
        /// attempt's whitelist (§6.4 R-degrade).
        degraded: bool,
        /// The `seq` of the `actionSettled` event (evidence linking).
        settled_seq: u64,
        /// The attempt number the terminal settled on.
        attempt_n: u64,
    },
    /// Step fails (final failure, exhausted retries, invalid arguments).
    StepFail { class: ErrorClass, message: String },
    /// Step folds to unknown (timeout without idempotence/retry; session
    /// degradation).
    StepUnknown {
        message: String,
        /// The error class that produced the unknown, when the class still
        /// governs handler selection. `session_degraded` is the one the
        /// spine §5 error table routes to a flow-level `onError` while the
        /// step itself folds to unknown — dropping the class here would
        /// silently send it to `onUnknown` instead, so a declared
        /// `on_error: { error_classes: [session_degraded] }` would never
        /// fire. A non-idempotent timeout keeps `None`: its row prescribes
        /// the unknown path and no error hook.
        error_class: Option<ErrorClass>,
    },
    /// The act's fate is sealed or ruled but its EFFECT is unproven, and
    /// the step declares assertions that can ask the world. Two producers:
    /// a `timedOut` terminal (spine §5 / 07 §4.3 — a recorded timeout is
    /// certain and reconcile never upgrades it; the observe half of 「先
    /// reconcile/observe 确认」 is what remains), and a human `adopt`
    /// adjudication of an uncertain reconcile (07 §4.4 — the ruling says
    /// the effect stands; the step's own assertions verify). The
    /// settlement loop captures a fresh observation and evaluates the
    /// assertions over it: decisive ones conclude pass/fail, indecisive
    /// ones fold to unknown — exactly the path the bare uncertainty took
    /// before.
    Unconfirmed {
        /// What made the effect unprovable, human-readable (prefixes the
        /// verdict summary).
        message: String,
    },
    /// A `cancelled` terminal.
    Aborted,
    /// No terminal (transport-class provider error) — suspend.
    Suspend(String),
}

/// The localized before/after observation records of an executed action
/// step — the material source of `observe: { fromStep }` assert steps.
pub(crate) struct StepObs {
    /// The localized records, in capture order.
    pub observations: Vec<ObservationRecord>,
    /// The before observation's id, when one was captured.
    pub before_id: Option<String>,
    /// The after observation's id, when one was captured.
    pub after_id: Option<String>,
}

/// A completed step instance carried over by resume (adoption by exact
/// instance path — 07 §4.6: completed steps enter as records, never
/// re-execute).
pub(crate) struct Adopted {
    /// The archived record (fold output).
    pub record: StepRecord,
    /// The before observation's id (harvested from `actionSettled`).
    pub before_id: Option<String>,
    /// The after observation's id (harvested from `actionSettled`).
    pub after_id: Option<String>,
}

/// Whether a record is execution history (vs a blocked/skipped accounting
/// pair, which concluded nothing and seeds nothing).
pub(crate) fn is_history(record: &StepRecord) -> bool {
    !record.attempts.is_empty()
        || record.verdict.is_some()
        || record.output.is_some()
        || !record.resolved_inputs.is_null()
}

/// One live execution frame: the root flow or a callee (07 §1.2 — a
/// frame's full execution semantics are determined by
/// `(calleeIrHash, inputsSnapshot, env)`). Scope contents never cross the
/// frame boundary except read-only `env.*`.
pub(crate) struct FrameState<'a> {
    /// The flow executing in this frame.
    pub flow: &'a FlowIR,
    /// The frame's root path (`[flow]` for the root frame; up to and
    /// including the `call` frame for a callee).
    pub base_path: RunPath,
    /// `params.*`: the run params (root) or the gated inputs snapshot.
    pub params: Map<String, Value>,
    /// `vars.*` accumulated by `let` steps (SSA).
    pub vars: BTreeMap<String, Value>,
    /// Live `iter.<as>` bindings, innermost last.
    pub iters: Vec<(String, Value)>,
    /// `steps.<id>.output` of concluded steps in this frame.
    pub outputs: BTreeMap<String, Value>,
    /// `steps.<id>.verdict` of concluded steps in this frame.
    pub verdicts: BTreeMap<String, (VerdictStatus, bool)>,
    /// Every step-instance verdict produced in this frame, in execution
    /// order — the flow-verdict fold input (iteration instances count
    /// individually; callee-internal verdicts fold through their call
    /// step, never leak here).
    pub fold: Vec<(VerdictStatus, bool)>,
    /// Localized observations per executed action step (assert `fromStep`).
    pub observed: BTreeMap<String, StepObs>,
    /// Call depth (root = 1).
    pub depth: usize,
}

impl<'a> FrameState<'a> {
    /// A fresh frame over `flow`.
    pub fn new(
        flow: &'a FlowIR,
        base_path: RunPath,
        params: Map<String, Value>,
        depth: usize,
    ) -> Self {
        FrameState {
            flow,
            base_path,
            params,
            vars: BTreeMap::new(),
            iters: Vec::new(),
            outputs: BTreeMap::new(),
            verdicts: BTreeMap::new(),
            fold: Vec::new(),
            observed: BTreeMap::new(),
            depth,
        }
    }

    /// Materializes the closed evaluation scope of this frame (spine §7):
    /// `params.* / env.* / vars.* / iter.<as> / steps.<id>.*`, plus an
    /// optional self-output binding (raw output for projection, projected
    /// output for assertions — 02 §4.1.1).
    pub fn scope(&self, env: &[(String, Value)], self_binding: Option<(&str, &Value)>) -> Scope {
        let mut scope = Scope::new();
        for (name, value) in &self.params {
            scope.set_param(name.clone(), value.clone());
        }
        for (name, value) in env {
            scope.set_env(name.clone(), value.clone());
        }
        for (name, value) in &self.vars {
            scope.set_var(name.clone(), value.clone());
        }
        for (name, value) in &self.iters {
            scope.set_iter(name.clone(), value.clone());
        }
        for (step_id, output) in &self.outputs {
            scope.set_step_output(step_id.clone(), output.clone());
        }
        for (step_id, (status, _degraded)) in &self.verdicts {
            let status = serde_json::to_value(status).expect("VerdictStatus serializes");
            scope.set_step_verdict(step_id.clone(), status);
        }
        if let Some((step_id, value)) = self_binding {
            scope.set_step_output(step_id.to_owned(), value.clone());
        }
        scope
    }

    fn seed_verdict(&mut self, step_id: &StepId, status: VerdictStatus, degraded: bool) {
        self.verdicts
            .insert(step_id.as_str().to_owned(), (status, degraded));
        self.fold.push((status, degraded));
    }

    /// Replaces the most recently seeded verdict (an escalate handler's
    /// superseding judgment for the step it was consulted on — the host
    /// verdict is by construction the last seeded entry at consultation
    /// time).
    fn reseed_last(&mut self, step_id: &StepId, status: VerdictStatus, degraded: bool) {
        self.verdicts
            .insert(step_id.as_str().to_owned(), (status, degraded));
        self.fold.pop();
        self.fold.push((status, degraded));
    }
}

/// The single-run execution engine. Owns the provider session; borrows the
/// single-writer store (the runner keeps store use single-threaded — async
/// exists only because the SPI is async).
pub(crate) struct Execution<'a> {
    pub flows: &'a LoadedFlow<'a>,
    pub session: Box<dyn ProviderSession>,
    pub store: &'a mut Store,
    pub run_id: String,
    pub stop: CancellationToken,
    /// `env.*` bindings (deviceId / runId / platform): run-constant,
    /// read-only pass-through across every frame (07 §1.2).
    pub env: Vec<(String, Value)>,
    /// Highest attempt number already used per step instance (resume
    /// continues the numbering; empty on a fresh run).
    pub attempt_base: BTreeMap<String, u64>,
    /// Step spans left open by a crash/suspension: instance key → the
    /// archived ready-phase snapshot. Execution re-enters these spans
    /// without a second `stepEntered`, and containers reuse the archived
    /// snapshot instead of re-evaluating (spine §6.6).
    pub open_spans: BTreeMap<String, Value>,
    /// Call frames already pushed (and not popped) by a previous segment:
    /// instance key → the callee `irHash` the open frame currently claims.
    /// Resume must not push them again; when the pin moved under a
    /// down-drill it re-enters them instead (07 §5.2 case (a)).
    pub live_frames: BTreeMap<String, pointlock_ir::Hash>,
    /// Completed step instances to adopt instead of executing, keyed by
    /// instance path.
    pub adoptable: BTreeMap<String, Adopted>,
    /// Reconciled mid-flight work for the frontier step instance.
    pub frontier: Option<(String, FrontierWork)>,
    /// Whether this segment is a RESUME. It decides where the honest
    /// `unprobed` mark belongs (07 §4.2 rule 1): a fresh run never
    /// re-touches a world it stopped watching, so nothing in it is
    /// unprobed.
    pub resumed: bool,
    /// Step ids released through the 07 §5.4 gate this segment. Step 3 of
    /// that rule extends the preflight guard to every one of them, so each
    /// is an `unprobed` site of its own when it declares no probes.
    pub authorized: BTreeSet<String>,
    /// Latch: the segment's re-entry step has been reached. Set the first
    /// time a step gets as far as probing — adopted steps short-circuit
    /// long before, so the first one that arrives here IS 07 §4.2's
    /// 「resume 的首个待执行 step」.
    pub reentry_seen: bool,
    /// The vision verifier for `vision` verify-chain tails. `None` is
    /// equivalent to the stub: the vision channel cannot complete and
    /// reports `"vision verifier not configured"`.
    pub vision: Option<Arc<dyn VisionVerifier>>,
    /// Known session generations (checkpoint lineage; a fresh run seeds
    /// the bind-time session). Best-effort input of the failure-instant
    /// provider profile (07 §2.2).
    pub session_lineage: Vec<String>,
    /// Failure-instant provider profiles captured at verdict time, keyed
    /// by step-instance key; attached to the span's `stepExited` by
    /// `append` (intensional gate by construction) and discarded on a
    /// superseding pass, an aborted follow-up exit, or span re-entry.
    pub pending_summaries: BTreeMap<String, ProviderStateSummary>,
    /// This segment's supervision policy (R13, spine §6.9): per segment,
    /// never inherited. `None` — unsupervised.
    pub supervise: Option<SupervisePolicy>,
    /// Human requests on the ledger, keyed by step-instance key (resume
    /// settlement input; empty on a fresh run).
    pub human: BTreeMap<String, HumanRequestFact>,
    /// Settled terminals on the ledger, keyed by step-instance key: the
    /// re-entry material for open action spans whose act already settled
    /// before a handler-wave suspension (never re-dispatch, I2).
    pub settled: BTreeMap<String, crate::align::SettledFact>,
    /// Recorded verdicts on the ledger, keyed by step-instance key: the
    /// handler-consultation re-entry point on resume.
    pub recorded_verdicts: BTreeMap<String, (VerdictStatus, bool)>,
    /// Handler trigger watermarks ("{instance}|{hook}" → highest trigger
    /// on the ledger): `maxTriggers` counts across segments, never resets.
    pub hook_triggers: BTreeMap<String, u64>,
    /// Injectable wall clock for deadline computation and lazy timeout
    /// settlement; `None` uses the system clock. The settlement *result*
    /// is a pure function of `deadlineAtMs` and response presence — never
    /// of the settlement instant (06 §5.3).
    pub clock: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
}

impl<'a> Execution<'a> {
    fn append(&mut self, path: &RunPath, payload: &RunLogPayload) -> Result<u64, RunnerError> {
        // The 07 §2.2 attach point: a fail/unknown-verdict span exiting
        // (any exit site — the gate is intensional, not an enumerated
        // list) carries the verdict-instant provider profile. Aborted
        // follow-up exits make no semantic claim and discard it; span
        // re-entry invalidates a stale capture.
        let enriched;
        let payload = match payload {
            RunLogPayload::StepEntered { .. } => {
                self.pending_summaries.remove(&instance_key(path));
                payload
            }
            RunLogPayload::StepExited {
                state,
                output,
                provider_state_summary: None,
                localized,
                localization_gaps,
            } => match (state, self.pending_summaries.remove(&instance_key(path))) {
                (StepState::Aborted, _) | (_, None) => payload,
                (_, Some(summary)) => {
                    enriched = RunLogPayload::StepExited {
                        state: *state,
                        output: output.clone(),
                        provider_state_summary: Some(summary),
                        localized: localized.clone(),
                        localization_gaps: localization_gaps.clone(),
                    };
                    &enriched
                }
            },
            _ => payload,
        };
        Ok(self
            .store
            .append_event(&self.run_id, now_ms(), path, payload)?)
    }

    /// Pre-stashes resume-generation profiles for crash-opened spans a
    /// sync `record_pairs` cascade is about to close: a span whose ledger
    /// verdict is fail/unknown must not exit summary-less just because
    /// its verdict was recorded by a previous segment (07 §2.2 note 3).
    async fn stash_open_span_summaries(&mut self) {
        let keys: Vec<String> = self
            .open_spans
            .keys()
            .filter(|key| {
                matches!(
                    self.recorded_verdicts.get(*key),
                    Some((VerdictStatus::Fail | VerdictStatus::Unknown, _))
                ) && !self.pending_summaries.contains_key(*key)
            })
            .cloned()
            .collect();
        if keys.is_empty() {
            return;
        }
        let summary = self.capture_summary().await;
        for key in keys {
            self.pending_summaries.insert(key, summary.clone());
        }
    }

    /// Captures the provider profile with this run's identity bindings.
    async fn capture_summary(&self) -> ProviderStateSummary {
        let env_str = |key: &str| {
            self.env
                .iter()
                .find(|(name, _)| name == key)
                .and_then(|(_, value)| value.as_str().map(str::to_owned))
        };
        capture_provider_state_summary(
            self.session.as_ref(),
            &self.session_lineage,
            &env_str("deviceId").unwrap_or_default(),
            env_str("platform").as_deref(),
        )
        .await
    }

    /// The wall clock the human-deadline machinery reads (injectable for
    /// tests; event `atMs` stamps stay on the system clock — they are
    /// informational, deadlines are semantics).
    fn now(&self) -> u64 {
        match &self.clock {
            Some(clock) => clock(),
            None => now_ms(),
        }
    }

    /// Runs the root body from `start` and settles the run terminal.
    pub async fn run(
        mut self,
        mut root: FrameState<'a>,
        start: usize,
    ) -> Result<RunOutcome, RunnerError> {
        let flows = self.flows;
        let body: &'a [StepIR] = &flows.root.body;
        let prefix = root.base_path.clone();
        let ctl = self
            .exec_body(&mut root, prefix.clone(), body, start)
            .await?;
        match ctl {
            Ctl::Continue | Ctl::HaltFail => self.finish(false, &root).await,
            Ctl::Abort => self.finish(true, &root).await,
            Ctl::Suspend(reason) => {
                // Suspension-instant profile (07 §2.2): captured while
                // the session is still live, before teardown.
                let summary = self.capture_summary().await;
                self.append(
                    &prefix,
                    &RunLogPayload::RunSuspended {
                        provider_state_summary: Some(summary),
                        reason: Some(reason),
                    },
                )?;
                self.end_session(SessionOutcome::Shutdown).await;
                Ok(RunOutcome::Suspended)
            }
            Ctl::Blocked(reason) => {
                let summary = self.capture_summary().await;
                self.append(
                    &prefix,
                    &RunLogPayload::RunSuspended {
                        provider_state_summary: Some(summary),
                        reason: Some(reason.to_string()),
                    },
                )?;
                self.end_session(SessionOutcome::Shutdown).await;
                Ok(RunOutcome::Blocked { reason })
            }
            Ctl::AwaitHuman(pending) => {
                // The unified wait semantics: `humanRequested` is already
                // fsynced (its append committed); the segment suspends and
                // the process may exit — notification and collection are
                // the CLI layer's job (spine §6.8, 06 §5.1/§5.2). The
                // session is released while waiting; resume opens a new
                // one (session lineage).
                let summary = self.capture_summary().await;
                self.append(
                    &prefix,
                    &RunLogPayload::RunSuspended {
                        provider_state_summary: Some(summary),
                        reason: Some(format!(
                            "awaiting human response (requestId {})",
                            pending.request_id
                        )),
                    },
                )?;
                self.end_session(SessionOutcome::Shutdown).await;
                Ok(RunOutcome::AwaitingHuman { pending })
            }
        }
    }

    /// Folds the root flow verdict, appends `runFinished`, ends the
    /// session.
    async fn finish(
        mut self,
        aborted: bool,
        root: &FrameState<'a>,
    ) -> Result<RunOutcome, RunnerError> {
        let prefix = root.base_path.clone();
        let verdict = if aborted {
            // An aborted run makes no flow-level semantic claim.
            None
        } else {
            fold_flow_verdict(&root.fold, root.flow.verdict_policy).map(|folded| Verdict {
                status: folded.status,
                degraded: folded.degraded,
                summary: folded.summary,
                evidence: Vec::new(),
                supersedes: None,
            })
        };
        let remote_archival_error = match &verdict {
            // Judgment authority is Pointlock's; the daemon only persists
            // (spine §6.3 write-back). Failure is annotation material,
            // never a run error (04 §5).
            Some(verdict) => self.try_verdict_writeback(verdict).await,
            None => None,
        };
        self.append(
            &prefix,
            &RunLogPayload::RunFinished {
                verdict: verdict.clone(),
                remote_archival_error,
            },
        )?;
        let session_outcome = if aborted {
            SessionOutcome::Cancelled
        } else if verdict
            .as_ref()
            .is_some_and(|verdict| verdict.status == VerdictStatus::Fail)
        {
            SessionOutcome::Failed
        } else {
            SessionOutcome::Completed
        };
        self.end_session(session_outcome).await;
        Ok(RunOutcome::Finished { verdict })
    }

    /// Executes one body level sequentially. The stop token is honored
    /// before every step (step boundaries, any depth); a fail halts the
    /// level and records the remaining steps `blocked`.
    async fn exec_body(
        &mut self,
        frame: &mut FrameState<'a>,
        prefix: RunPath,
        body: &'a [StepIR],
        start: usize,
    ) -> Result<Ctl, RunnerError> {
        for (index, step) in body.iter().enumerate().skip(start) {
            if self.stop.is_cancelled() {
                return Ok(Ctl::Suspend("stop requested".to_owned()));
            }
            match self.exec_step(frame, &prefix, step).await? {
                Ctl::Continue => {}
                Ctl::HaltFail => {
                    // Halt-on-fail: remaining steps of this level are
                    // explicitly recorded blocked (never silently dropped
                    // from the ledger).
                    self.stash_open_span_summaries().await;
                    self.record_pairs(&prefix, &body[index + 1..], StepState::Blocked)?;
                    return Ok(Ctl::HaltFail);
                }
                other => return Ok(other),
            }
        }
        Ok(Ctl::Continue)
    }

    /// Executes (or adopts) one step instance. Boxed: the recursion point
    /// of the tree walk (containers and calls re-enter `exec_body`).
    fn exec_step<'s>(
        &'s mut self,
        frame: &'s mut FrameState<'a>,
        prefix: &'s RunPath,
        step: &'a StepIR,
    ) -> LocalBoxFuture<'s, Result<Ctl, RunnerError>>
    where
        'a: 's,
    {
        Box::pin(async move {
            let mut path = prefix.clone();
            path.push(child_frame(step));
            let key = instance_key(&path);
            // Resume adoption (07 §4.6/I2): a concluded instance enters as
            // its record and never re-executes.
            if self
                .adoptable
                .get(&key)
                .is_some_and(|adopted| is_history(&adopted.record))
            {
                let adopted = self.adoptable.remove(&key).expect("checked present");
                self.adopt_step(frame, step, adopted);
                return Ok(Ctl::Continue);
            }
            match step {
                StepIR::Action(s) => self.exec_action(frame, path, s).await,
                StepIR::Call(s) => self.exec_call(frame, path, s).await,
                StepIR::If(s) => self.exec_if(frame, path, s).await,
                StepIR::Foreach(s) => self.exec_foreach(frame, path, s).await,
                StepIR::Let(s) => self.exec_let(frame, path, s).await,
                StepIR::Assert(s) => self.exec_assert(frame, path, s).await,
                StepIR::Human(s) => self.exec_human(frame, path, s).await,
            }
        })
    }

    /// Seeds a frame with an adopted record's effects (outputs / verdicts /
    /// vars / observation material); containers recursively consume their
    /// children's records using the archived control snapshots — never a
    /// re-evaluation (I3).
    fn adopt_step(&mut self, frame: &mut FrameState<'a>, step: &'a StepIR, adopted: Adopted) {
        let id = step.step_id().as_str().to_owned();
        let record = adopted.record;
        match step {
            StepIR::Action(_) => {
                if let Some(verdict) = &record.verdict {
                    frame.seed_verdict(step.step_id(), verdict.status, verdict.degraded);
                }
                if let Some(output) = record.output.clone() {
                    frame.outputs.insert(id.clone(), output);
                }
                frame.observed.insert(
                    id,
                    StepObs {
                        observations: record.observations,
                        before_id: adopted.before_id,
                        after_id: adopted.after_id,
                    },
                );
            }
            StepIR::Assert(_) | StepIR::Call(_) | StepIR::Human(_) => {
                // A settled human step re-enters as its verdict/output
                // (the response was already arbitrated and folded into the
                // record) — never re-asked.
                if let Some(verdict) = &record.verdict {
                    frame.seed_verdict(step.step_id(), verdict.status, verdict.degraded);
                }
                if let Some(output) = record.output.clone() {
                    frame.outputs.insert(id, output);
                }
            }
            StepIR::Let(_) => {
                // The archived ready snapshot *is* the bindings product.
                if let Value::Object(bindings) = record.resolved_inputs {
                    for (name, value) in bindings {
                        frame.vars.insert(name, value);
                    }
                }
            }
            StepIR::If(s) => {
                // Consume both branches: the selected branch's records seed
                // effects, the unselected branch's skipped pairs seed
                // nothing — both leave the adoption set.
                self.adopt_children(frame, &record.run_path, &s.then);
                if let Some(otherwise) = &s.r#else {
                    self.adopt_children(frame, &record.run_path, otherwise);
                }
            }
            StepIR::Foreach(s) => {
                let rounds = record
                    .resolved_inputs
                    .get("items")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
                for index in 0..rounds {
                    let mut prefix = record.run_path.clone();
                    prefix.push(PathFrame::Iteration {
                        index: index as u64,
                        key: None,
                    });
                    self.adopt_children(frame, &prefix, &s.body);
                }
            }
        }
    }

    fn adopt_children(
        &mut self,
        frame: &mut FrameState<'a>,
        prefix: &RunPath,
        steps: &'a [StepIR],
    ) {
        for step in steps {
            let mut path = prefix.clone();
            path.push(child_frame(step));
            let key = instance_key(&path);
            if let Some(adopted) = self.adoptable.remove(&key) {
                self.adopt_step(frame, step, adopted);
            }
        }
    }

    /// Records `entered(resolvedInputs: null)`/`exited(state)` pairs for a
    /// subtree that will not execute (skipped branches, blocked tails) —
    /// ledger completeness per the blocked precedent. Children are handled
    /// before their container so that crash-opened spans close innermost
    /// first (the fold's exit pairing is positional). Instances already on
    /// the ledger from a previous segment are kept, not re-emitted.
    fn record_pairs(
        &mut self,
        prefix: &RunPath,
        steps: &'a [StepIR],
        state: StepState,
    ) -> Result<(), RunnerError> {
        for step in steps {
            let mut path = prefix.clone();
            path.push(child_frame(step));
            match step {
                StepIR::If(s) => {
                    self.record_pairs(&path, &s.then, state)?;
                    if let Some(otherwise) = &s.r#else {
                        self.record_pairs(&path, otherwise, state)?;
                    }
                }
                StepIR::Foreach(s) => self.record_pairs(&path, &s.body, state)?,
                _ => {}
            }
            let key = instance_key(&path);
            if self.adoptable.remove(&key).is_some() {
                continue;
            }
            if self.open_spans.remove(&key).is_some() {
                // A previous segment opened this span; close it with the
                // terminal state instead of double-entering.
                self.append(
                    &path,
                    &RunLogPayload::StepExited {
                        provider_state_summary: None,
                        state,
                        output: None,
                        localized: Vec::new(),
                        localization_gaps: Vec::new(),
                    },
                )?;
                continue;
            }
            self.append(
                &path,
                &RunLogPayload::StepEntered {
                    step_id: step.step_id().clone(),
                    effect_hash: step.base().effect_hash.clone(),
                    judge_hash: step.base().judge_hash.clone(),
                    resolved_inputs: Value::Null,
                },
            )?;
            self.append(
                &path,
                &RunLogPayload::StepExited {
                    provider_state_summary: None,
                    state,
                    output: None,
                    localized: Vec::new(),
                    localization_gaps: Vec::new(),
                },
            )?;
        }
        Ok(())
    }

    /// The archived ready snapshot of a crash/suspension-opened span, when
    /// this instance has one (peek — `enter_step` consumes it).
    fn open_span_inputs(&self, key: &str) -> Option<Value> {
        self.open_spans.get(key).cloned()
    }

    /// Appends `stepEntered` unless the instance's span is already open
    /// (resume: the log has an unmatched `stepEntered`). The payload
    /// carries the step's dual hashes and the frozen ready-phase input
    /// snapshot (spine §6.1 M1 note).
    fn enter_step(
        &mut self,
        path: &RunPath,
        base: &StepBase,
        resolved_inputs: Value,
    ) -> Result<(), RunnerError> {
        let key = instance_key(path);
        if self.open_spans.remove(&key).is_some() {
            return Ok(());
        }
        self.append(
            path,
            &RunLogPayload::StepEntered {
                step_id: base.step_id.clone(),
                effect_hash: base.effect_hash.clone(),
                judge_hash: base.judge_hash.clone(),
                resolved_inputs,
            },
        )?;
        Ok(())
    }

    /// Evaluates one bound attempt's argument expressions against the
    /// frame scope (the ready-phase resolution).
    fn resolve_args(
        &self,
        frame: &FrameState<'a>,
        attempt: &BoundAttempt,
    ) -> Result<Value, String> {
        let scope = frame.scope(&self.env, None);
        let mut evaluated = serde_json::Map::new();
        for (name, expr) in attempt.args.iter() {
            match pointlock_expr::eval(expr, &scope) {
                Ok(value) => {
                    evaluated.insert(name.as_str().to_owned(), value);
                }
                Err(error) => return Err(format!("argument evaluation failed: {error}")),
            }
        }
        Ok(Value::Object(evaluated))
    }

    // ─── probing (spine §6.2; 07 §4.2) ──────────────────────────────────────

    /// Runs a step's declared `preflight`, or records that there was none
    /// to run (07 §4.2 rule 1 / I3).
    ///
    /// 「该步无声明则跳过并在报告标 `unprobed`（诚实优先于安慰）」. The
    /// carrier is a `preflightProbed` with an EMPTY outcome list, which is
    /// unambiguous rather than clever: `preflight` is `minItems: 1` in the
    /// schema, so a declared probe list can never evaluate to zero
    /// outcomes. No new event type, no new payload field, and old ledgers
    /// — which never emitted it — refold byte-identically.
    ///
    /// It is written at exactly the two places the spec names, and nowhere
    /// else. A step in the middle of a continuously-executing run is not
    /// re-touching a world anyone stopped watching, and marking it would
    /// turn an honest signal into noise:
    /// - the segment's re-entry step, when the segment is a resume
    ///   (§4.2 rule 1);
    /// - every step released through the §5.4 gate (step 3: 「本条
    ///   preflight 守护对 `positionalReplay`/`orderInvalidated`/
    ///   `frontierUnknown` 的步同样强制适用」) — those re-execute onto a
    ///   world that carries the earlier effect, which is the whole reason
    ///   they had to be authorized by name.
    async fn probe_or_note(
        &mut self,
        frame: &mut FrameState<'a>,
        path: &RunPath,
        base: &'a StepBase,
    ) -> Result<Option<Ctl>, RunnerError> {
        let reentry = self.resumed && !self.reentry_seen;
        self.reentry_seen = true;
        if let Some(probes) = &base.preflight {
            return self.probe_preflight(frame, path, base, probes).await;
        }
        if reentry || self.authorized.contains(base.step_id.as_str()) {
            let mut probe_path = path.clone();
            probe_path.push(PathFrame::Phase {
                phase: Phase::Preflight,
            });
            self.append(
                &probe_path,
                &RunLogPayload::PreflightProbed {
                    outcomes: Vec::new(),
                },
            )?;
        }
        Ok(None)
    }

    /// Evaluates a step's declared `preflight` probes over fresh observe
    /// material (spine §6.7-C operationalized). A probe that does not hold
    /// — or cannot be evaluated (exhausted chain) — is drift: the step's
    /// `onResumeDrift` ladder is consulted (repair → re-probe, escalate →
    /// `repairWorld`); with none left the run blocks (`drifted` →
    /// `runSuspended`).
    async fn probe_preflight(
        &mut self,
        frame: &mut FrameState<'a>,
        path: &RunPath,
        base: &'a StepBase,
        probes: &'a [AssertionIR],
    ) -> Result<Option<Ctl>, RunnerError> {
        let step_id = &base.step_id;
        let mut probe_path = path.clone();
        probe_path.push(PathFrame::Phase {
            phase: Phase::Preflight,
        });
        let needs = VerifyNeeds::of(probes);
        let mut active_retry: Option<(RetryPolicy, u32)> = None;
        loop {
            let material = self.fresh_material(&needs, &probe_path).await?;
            let scope = frame.scope(&self.env, None);
            let mut outcomes = Vec::with_capacity(probes.len());
            for probe in probes {
                let evaluated = match &probe.predicate {
                    PredicateIR::Expr { expr } => EvaluatedAssertion {
                        record: eval_expr_assertion(probe, expr, &scope),
                        degraded_verify: false,
                    },
                    _ => eval_observed_assertion(probe, &material, self.vision.as_deref()).await,
                };
                outcomes.push(evaluated.record);
            }
            self.append(
                &probe_path,
                &RunLogPayload::PreflightProbed {
                    outcomes: outcomes.clone(),
                },
            )?;
            let Some(missed) = outcomes
                .iter()
                .find(|outcome| outcome.result != VerdictStatus::Pass)
            else {
                return Ok(None);
            };
            // Unable-to-confirm is drift too (07 §4.2 rule 2: not being
            // able to see the world is not the world being fine —
            // principle 4).
            let what = match missed.result {
                VerdictStatus::Fail => "did not hold",
                _ => "could not be evaluated (treated as drift)",
            };
            let detail = format!("probe '{}' {what}: {}", missed.assert_id, missed.reason);

            // In-force drift-handler retry budget: re-probe (readonly).
            if let Some((policy, used)) = active_retry.take()
                && used < policy.max_attempts
            {
                self.backoff_policy(&policy, used).await;
                active_retry = Some((policy, used + 1));
                continue;
            }

            // A failed probe consults `onResumeDrift` (spine §6.2/§6.7-C:
            // probing → drifted → the drift handler; exhausted budgets
            // block awaiting a human decision).
            match self
                .consult_hook(
                    frame,
                    path,
                    base.handlers.as_deref(),
                    HandlerHook::OnResumeDrift,
                    None,
                )
                .await?
            {
                Consulted::None | Consulted::RepairFailed => {
                    return Ok(Some(Ctl::Blocked(BlockedReason::Drifted {
                        step_id: step_id.as_str().to_owned(),
                        detail,
                    })));
                }
                Consulted::Continue => {
                    // The author accepts the drifted world: proceed.
                    return Ok(None);
                }
                Consulted::Abort => return Ok(Some(Ctl::Abort)),
                Consulted::Escalated { status, .. } => match status {
                    // A human judged the world acceptable: proceed.
                    VerdictStatus::Pass => return Ok(None),
                    _ => {
                        return Ok(Some(Ctl::Blocked(BlockedReason::Drifted {
                            step_id: step_id.as_str().to_owned(),
                            detail: format!("{detail}; escalate ruling: not acceptable"),
                        })));
                    }
                },
                Consulted::Retry(policy) => {
                    self.backoff_policy(&policy, 0).await;
                    active_retry = Some((policy, 1));
                }
                // A repaired world (declared or via the repair flow):
                // re-probe — the probe, not the declaration, readmits.
                Consulted::Repaired | Consulted::RepairDone => {}
                Consulted::Pending(pending) => return Ok(Some(Ctl::AwaitHuman(pending))),
                Consulted::Propagate(ctl) => return Ok(Some(ctl)),
            }
        }
    }

    /// Captures a fresh observation (`session.observe`) sized to the
    /// declared verify needs, localizes it (`observationRecorded`), and
    /// returns the verify-chain material. An observe failure is a typed
    /// material gap, never a run abort — the dependent assertions degrade
    /// toward unknown.
    async fn fresh_material(
        &mut self,
        needs: &VerifyNeeds,
        anchor: &RunPath,
    ) -> Result<ObserveMaterial, RunnerError> {
        let mut wants = Vec::new();
        if needs.ui_tree {
            wants.push(ObserveWant::UiSnapshot);
        }
        if needs.vision {
            wants.push(ObserveWant::Screenshot);
        }
        if wants.is_empty() {
            // Expr-only consumers need no observation channel.
            return Ok(ObserveMaterial::default());
        }
        let observation = match self.session.observe(ObserveRequest { wants }, None).await {
            Ok(observation) => observation,
            Err(error) => {
                return Ok(ObserveMaterial::absent(&format!(
                    "fresh observation failed: {error}"
                )));
            }
        };
        let mut cited = Vec::new();
        let mut material = ObserveMaterial::default();
        let record = self
            .localize_observation(&observation, &mut cited, Some((needs, &mut material)))
            .await?;
        self.append(
            anchor,
            &RunLogPayload::ObservationRecorded {
                observation: record,
            },
        )?;
        Ok(material)
    }

    // ─── action steps ───────────────────────────────────────────────────────

    async fn exec_action(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: RunPath,
        step: &'a ActionStepIR,
    ) -> Result<Ctl, RunnerError> {
        let step_id = step.base.step_id.clone();
        let key = instance_key(&step_path);
        let work = match &self.frontier {
            Some((frontier_key, _)) if *frontier_key == key => {
                self.frontier.take().map(|(_, work)| work)
            }
            _ => None,
        };

        // Ready precedes entered (spine §6.1 M1 note): the first bound
        // attempt's argument expressions are resolved once and frozen;
        // `stepEntered` carries the snapshot and lands before any
        // preflight probe or `actionIntent`. Frontier work reuses the
        // archived snapshot verbatim — never re-evaluated (spine §6.6).
        let resolved = match &work {
            Some(FrontierWork::Adopt { args, .. })
            | Some(FrontierWork::Replay { args, .. })
            | Some(FrontierWork::ConfirmEffect { args, .. })
            | Some(FrontierWork::AbortRuled { args }) => Ok(args.clone()),
            None => {
                let attempt = step
                    .binding
                    .attempts
                    .first()
                    .expect("sealed action steps carry at least one bound attempt");
                self.resolve_args(frame, attempt)
            }
        };
        let resolved_inputs = match resolved {
            Ok(args) => args,
            Err(message) => {
                // Inputs never resolved: the span still opens and closes
                // (one entered/exited pair per step), with
                // `resolvedInputs: null` — a failing argument evaluation
                // is a compiler/expression bug signal
                // (bind_arguments_invalid discipline): step fails, no
                // retry.
                self.enter_step(&step_path, &step.base, Value::Null)?;
                return self
                    .settle_error(
                        frame,
                        &step_path,
                        &step_id,
                        VerdictStatus::Fail,
                        format!("act phase failed [bind_arguments_invalid]: {message}"),
                    )
                    .await;
            }
        };
        let had_open_span = self.open_span_inputs(&key).is_some();
        self.enter_step(&step_path, &step.base, resolved_inputs.clone())?;

        // Handler-wave resume re-entry (I2): an open span whose act
        // already settled and whose verdict is on the ledger means a
        // previous segment suspended mid-handler (a pending escalate).
        // Never re-dispatch — enter the disposition loop directly from
        // the recorded ruling.
        let resume_ruling = if work.is_none() && had_open_span {
            match (self.settled.get(&key), self.recorded_verdicts.get(&key)) {
                (Some(_), Some(ruling)) => Some(*ruling),
                _ => None,
            }
        } else {
            None
        };

        // Probing (§6.2): declared preflight evaluates between entered and
        // acting. An adopted frontier terminal (or a handler-wave
        // re-entry) means the act already left in a previous life —
        // probing "is the world ready for the act" after the act is
        // meaningless, so it is skipped there.
        if !matches!(work, Some(FrontierWork::Adopt { .. }))
            && resume_ruling.is_none()
            && let Some(ctl) = self.probe_or_note(frame, &step_path, &step.base).await?
        {
            return Ok(ctl);
        }

        // R13 supervision gate (spine §6.9): sits wholly before the
        // `actionIntent` WAL — a refused act never had an intent on the
        // ledger. Adopt/Replay frontier work is never re-gated: the act
        // already happened, or its intent was WAL-authorized by a
        // previous segment.
        if work.is_none()
            && resume_ruling.is_none()
            && let Some(ctl) = self.gate_supervision(&step_path, step, &resolved_inputs)?
        {
            return Ok(ctl);
        }

        let mut acted = if resume_ruling.is_some() {
            None
        } else {
            Some(match work {
                Some(FrontierWork::Adopt {
                    call_id,
                    intent_path,
                    outcome,
                    args,
                    chain_index,
                }) => {
                    // Record the terminal the crash swallowed; this settles
                    // the pending intent in the checkpoint fold.
                    let attempt_n = attempt_of(&intent_path).unwrap_or(1);
                    let outcome = quarantine_unpersistable(*outcome);
                    let settled_seq = self.append(
                        &intent_path,
                        &RunLogPayload::ActionSettled {
                            call_id,
                            outcome: outcome.clone(),
                        },
                    )?;
                    // From here the adopted terminal takes the exact same
                    // settled-outcome path as a live one.
                    let adopted = AdoptedSettle {
                        outcome,
                        settled_seq,
                        attempt_n,
                    };
                    self.act_chain(
                        frame,
                        step,
                        &step_path,
                        Some(args),
                        Some(adopted),
                        chain_start(chain_index, step)?,
                    )
                    .await?
                }
                Some(FrontierWork::Replay { args, chain_index }) => {
                    self.act_chain(
                        frame,
                        step,
                        &step_path,
                        Some(args),
                        None,
                        chain_start(chain_index, step)?,
                    )
                    .await?
                }
                Some(FrontierWork::ConfirmEffect { message, .. }) => {
                    // No dispatch: the adjudication already ruled on the
                    // act; only the world's testimony is still owed.
                    ActPhase::Unconfirmed { message }
                }
                Some(FrontierWork::AbortRuled { .. }) => {
                    // The ruled abort mirrors a `cancelled` terminal's
                    // unwind: the open span closes `aborted` and the run
                    // makes no further semantic claim.
                    ActPhase::Aborted
                }
                // The fresh path hands the ready snapshot to the first
                // chain attempt — resolved exactly once, above.
                None => {
                    self.act_chain(
                        frame,
                        step,
                        &step_path,
                        Some(resolved_inputs.clone()),
                        None,
                        0,
                    )
                    .await?
                }
            })
        };

        // ── the settlement/disposition loop (M2 W3) ─────────────────────
        //
        // One round = one settled act (or the resumed recorded ruling)
        // judged and, on fail/unknown, consulted against the handlers.
        // Retry-class dispositions re-enter the act with the frozen
        // snapshot (new callId, new WAL intent); every re-fold records a
        // new verdict superseding the previous one (spine §2 concept 12).
        let mut last_verdict_seq: Option<u64> = None;
        let mut seeded = false;
        let mut active_retry: Option<(RetryPolicy, u32)> = None;
        let mut ruling = resume_ruling;
        loop {
            // What this round established: (status, degraded, summary,
            // cited evidence, projected output, error-path class).
            let (status, degraded, summary, cited, projected, error_class): (
                Option<VerdictStatus>,
                bool,
                String,
                Vec<AssetRef>,
                Option<Value>,
                Option<ErrorClass>,
            );
            let mut round_manifest = EvidenceManifest::default();
            let mut ruled_from_ledger = false;
            match (acted.take(), ruling.take()) {
                (None, Some((recorded_status, recorded_degraded))) => {
                    // Resumed at the recorded verdict: derive the output
                    // projection from the archived succeeded terminal so
                    // downstream refs keep working (pure re-projection).
                    ruled_from_ledger = true;
                    let recovered = match self.settled.get(&key).map(|fact| &fact.outcome) {
                        Some(ActionOutcome::Succeeded { result }) => {
                            let raw_scope =
                                frame.scope(&self.env, Some((step_id.as_str(), &result.output)));
                            project_output(step, &result.output, &raw_scope).ok()
                        }
                        _ => None,
                    };
                    status = Some(recorded_status);
                    degraded = recorded_degraded;
                    summary = "resumed at the recorded verdict (handler re-entry)".to_owned();
                    cited = Vec::new();
                    projected = recovered;
                    error_class = None;
                    // Cross-segment gate (07 §2.2 note 3): the previous
                    // segment's verdict is in force but its stash died
                    // with the process. Capture the RESUME-generation
                    // profile so the eventual exit still carries one —
                    // self-describing via its own sessionLineage/cursor;
                    // the failure-instant profile rides the prior
                    // segment's runSuspended.
                    if matches!(
                        recorded_status,
                        VerdictStatus::Fail | VerdictStatus::Unknown
                    ) {
                        let captured = self.capture_summary().await;
                        self.pending_summaries.insert(key.clone(), captured);
                    }
                }
                (Some(phase), _) => match phase {
                    ActPhase::Succeeded {
                        result,
                        degraded: degraded_execution,
                        settled_seq,
                        attempt_n,
                    } => {
                        let (round_cited, material, observed, manifest) = self
                            .observing(step, &step_path, attempt_n, &result, settled_seq)
                            .await?;
                        round_manifest = manifest;
                        frame.observed.insert(step_id.as_str().to_owned(), observed);
                        // Output projection (self-refs see the raw output).
                        let raw_scope =
                            frame.scope(&self.env, Some((step_id.as_str(), &result.output)));
                        let round_projected = match project_output(step, &result.output, &raw_scope)
                        {
                            Ok(value) => value,
                            Err(error) => {
                                // A failing output projection is a
                                // compiler/expression bug signal: step
                                // fails, no retry, no consultation
                                // (bind_arguments_invalid discipline).
                                return self
                                    .settle_error(
                                        frame,
                                        &step_path,
                                        &step_id,
                                        VerdictStatus::Fail,
                                        format!("output projection failed: {error}"),
                                    )
                                    .await;
                            }
                        };
                        // Asserting: pure computation over materialized
                        // values (the vision tail is the one declared
                        // exception to purity).
                        let assert_scope =
                            frame.scope(&self.env, Some((step_id.as_str(), &round_projected)));
                        let mut outcomes = Vec::with_capacity(step.assertions.len());
                        let mut degraded_verify = false;
                        for assertion in &step.assertions {
                            let evaluated = match &assertion.predicate {
                                PredicateIR::Expr { expr } => EvaluatedAssertion {
                                    record: eval_expr_assertion(assertion, expr, &assert_scope),
                                    degraded_verify: false,
                                },
                                _ => {
                                    eval_observed_assertion(
                                        assertion,
                                        &material,
                                        self.vision.as_deref(),
                                    )
                                    .await
                                }
                            };
                            degraded_verify |= evaluated.degraded_verify;
                            outcomes.push(evaluated.record);
                        }
                        for outcome in &outcomes {
                            let mut path = step_path.clone();
                            path.push(PathFrame::Phase {
                                phase: Phase::Assert,
                            });
                            path.push(PathFrame::Assertion {
                                assert_id: outcome.assert_id.clone(),
                            });
                            self.append(
                                &path,
                                &RunLogPayload::AssertionEvaluated {
                                    outcome: outcome.clone(),
                                },
                            )?;
                        }
                        if outcomes.is_empty() {
                            // No assertions ⇒ no verdict (spine R4):
                            // execution status only (`unverified`).
                            status = None;
                            degraded = false;
                            summary = String::new();
                        } else {
                            let folded = fold_step_verdict(
                                &outcomes,
                                degraded_execution,
                                degraded_verify,
                                frame.flow.verdict_policy,
                            );
                            status = Some(folded.status);
                            degraded = folded.degraded;
                            summary = folded.summary;
                        }
                        cited = round_cited;
                        projected = Some(round_projected);
                        error_class = None;
                    }
                    ActPhase::Unconfirmed { message } => {
                        // The observe half of 「先 reconcile/observe 确认」:
                        // a fresh observation, the step's own assertions
                        // over it — the same pure evaluation an assert step
                        // runs. Expr assertions reference an output the
                        // timeout never produced and resolve unknown
                        // (`onMissingInput`, principle 4); element and
                        // visual predicates read the world and can be
                        // decisive in both directions. The fold is the
                        // confirmation verdict: pass — the effect is
                        // visibly there; fail — visibly not; unknown —
                        // unconfirmable, exactly the path the bare timeout
                        // always took.
                        let needs = VerifyNeeds::of(&step.assertions);
                        let material = self.fresh_material(&needs, &step_path).await?;
                        let assert_scope = frame.scope(&self.env, None);
                        let mut outcomes = Vec::with_capacity(step.assertions.len());
                        let mut degraded_verify = false;
                        for assertion in &step.assertions {
                            let evaluated = match &assertion.predicate {
                                PredicateIR::Expr { expr } => EvaluatedAssertion {
                                    record: eval_expr_assertion(assertion, expr, &assert_scope),
                                    degraded_verify: false,
                                },
                                _ => {
                                    eval_observed_assertion(
                                        assertion,
                                        &material,
                                        self.vision.as_deref(),
                                    )
                                    .await
                                }
                            };
                            degraded_verify |= evaluated.degraded_verify;
                            outcomes.push(evaluated.record);
                        }
                        for outcome in &outcomes {
                            let mut path = step_path.clone();
                            path.push(PathFrame::Phase {
                                phase: Phase::Assert,
                            });
                            path.push(PathFrame::Assertion {
                                assert_id: outcome.assert_id.clone(),
                            });
                            self.append(
                                &path,
                                &RunLogPayload::AssertionEvaluated {
                                    outcome: outcome.clone(),
                                },
                            )?;
                        }
                        let folded = fold_step_verdict(
                            &outcomes,
                            false,
                            degraded_verify,
                            frame.flow.verdict_policy,
                        );
                        status = Some(folded.status);
                        degraded = folded.degraded;
                        summary = format!(
                            "{message}; assertions over a fresh observation: {}",
                            folded.summary
                        );
                        cited = Vec::new();
                        projected = None;
                        error_class = None;
                    }
                    ActPhase::StepFail { class, message } => {
                        let wire = serde_json::to_value(class).expect("ErrorClass serializes");
                        let wire = wire.as_str().expect("ErrorClass is a string literal");
                        status = Some(VerdictStatus::Fail);
                        degraded = false;
                        summary = format!("act phase failed [{wire}]: {message}");
                        cited = Vec::new();
                        projected = None;
                        error_class = Some(class);
                    }
                    ActPhase::StepUnknown {
                        message,
                        error_class: class,
                    } => {
                        status = Some(VerdictStatus::Unknown);
                        degraded = false;
                        summary = message;
                        cited = Vec::new();
                        projected = None;
                        // Usually none — an unknown has no error to route.
                        // `session_degraded` is the exception the spine §5
                        // table names, and it keeps its class so the hook
                        // selector below reaches `onError`.
                        error_class = class;
                    }
                    ActPhase::Aborted => {
                        self.append(
                            &step_path,
                            &RunLogPayload::StepExited {
                                provider_state_summary: None,
                                state: StepState::Aborted,
                                output: None,
                                localized: Vec::new(),
                                localization_gaps: Vec::new(),
                            },
                        )?;
                        return Ok(Ctl::Abort);
                    }
                    ActPhase::Suspend(reason) => return Ok(Ctl::Suspend(reason)),
                },
                (None, None) => unreachable!("every round has an act result or a ruling"),
            }

            // Record this round's verdict (unless it came off the ledger)
            // and seed/reseed the frame fold.
            let round_status = status;
            if let Some(current) = round_status {
                if !ruled_from_ledger {
                    let folded = FoldedVerdict {
                        status: current,
                        degraded,
                        summary: summary.clone(),
                    };
                    let supersedes = last_verdict_seq.map(|seq| format!("seq:{seq}"));
                    let seq = self
                        .record_step_verdict(
                            &step_path,
                            &folded,
                            cited,
                            supersedes,
                            std::mem::take(&mut round_manifest),
                        )
                        .await?;
                    last_verdict_seq = Some(seq);
                }
                if seeded {
                    frame.reseed_last(&step_id, current, degraded);
                } else {
                    frame.seed_verdict(&step_id, current, degraded);
                    seeded = true;
                }
            }

            // Pass / unverified: exit judged and release.
            if round_status.is_none() || round_status == Some(VerdictStatus::Pass) {
                // An UNVERIFIED exit (R4: no assertions ⇒ no verdict ⇒
                // no verdictRecorded carrier) must not drop its
                // settlement-evidence manifest — it rides the exit
                // instead (item ③ review fix). A pass-verdict exit's
                // manifest already rode its verdictRecorded.
                let exit_manifest = if round_status.is_none() {
                    std::mem::take(&mut round_manifest)
                } else {
                    EvidenceManifest::default()
                };
                self.append(
                    &step_path,
                    &RunLogPayload::StepExited {
                        provider_state_summary: None,
                        state: StepState::Judged,
                        output: projected.clone(),
                        localized: exit_manifest.localized,
                        localization_gaps: exit_manifest.gaps,
                    },
                )?;
                if let Some(projected) = projected {
                    frame.outputs.insert(step_id.as_str().to_owned(), projected);
                }
                return Ok(Ctl::Continue);
            }
            let current = round_status.expect("checked above");

            // In-force handler retry budget (one consultation grants
            // `max_attempts` re-entries with its backoff schedule).
            if let Some((policy, used)) = active_retry.take()
                && used < policy.max_attempts
            {
                self.backoff_policy(&policy, used).await;
                active_retry = Some((policy, used + 1));
                acted = Some(
                    // In-force retry policy: re-enter from the chain head
                    // (item ② ruling — handler retry restarts the chain).
                    self.act_chain(
                        frame,
                        step,
                        &step_path,
                        Some(resolved_inputs.clone()),
                        None,
                        0,
                    )
                    .await?,
                );
                continue;
            }

            // Consult the hook: assertion negatives walk onFail/onUnknown,
            // error-path negatives walk onError, error-path unknowns walk
            // onUnknown (AssertionFailure is a verdict, not an error —
            // spine §5).
            let hook = if error_class.is_some() {
                HandlerHook::OnError
            } else if current == VerdictStatus::Fail {
                HandlerHook::OnFail
            } else {
                HandlerHook::OnUnknown
            };
            match self
                .consult_hook(
                    frame,
                    &step_path,
                    step.base.handlers.as_deref(),
                    hook,
                    error_class,
                )
                .await?
            {
                Consulted::None | Consulted::RepairFailed => {
                    self.append(
                        &step_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Judged,
                            output: projected.clone(),
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    if let Some(projected) = projected {
                        frame.outputs.insert(step_id.as_str().to_owned(), projected);
                    }
                    return Ok(if current == VerdictStatus::Fail {
                        Ctl::HaltFail
                    } else {
                        Ctl::Continue
                    });
                }
                Consulted::Continue => {
                    // Record-and-release: the verdict stands, downstream
                    // is not halted (spine §3 disposition table).
                    self.append(
                        &step_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Judged,
                            output: projected.clone(),
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    if let Some(projected) = projected {
                        frame.outputs.insert(step_id.as_str().to_owned(), projected);
                    }
                    return Ok(Ctl::Continue);
                }
                Consulted::Abort => {
                    self.append(
                        &step_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Aborted,
                            output: None,
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    return Ok(Ctl::Abort);
                }
                Consulted::Escalated {
                    status: ruled,
                    summary: ruled_summary,
                    evidence: ruling_evidence,
                } => {
                    let folded = FoldedVerdict {
                        status: ruled,
                        degraded: false,
                        summary: ruled_summary,
                    };
                    let supersedes = last_verdict_seq.map(|seq| format!("seq:{seq}"));
                    let (cited, manifest) = escalate_verdict_material(&ruling_evidence);
                    let ruled_seq = self
                        .record_step_verdict(&step_path, &folded, cited, supersedes, manifest)
                        .await?;
                    self.link_ruling_evidence(ruled_seq, &ruling_evidence)?;
                    if seeded {
                        frame.reseed_last(&step_id, ruled, false);
                    } else {
                        frame.seed_verdict(&step_id, ruled, false);
                    }
                    self.append(
                        &step_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Judged,
                            output: projected.clone(),
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    if let Some(projected) = projected {
                        frame.outputs.insert(step_id.as_str().to_owned(), projected);
                    }
                    return Ok(if ruled == VerdictStatus::Fail {
                        Ctl::HaltFail
                    } else {
                        Ctl::Continue
                    });
                }
                Consulted::Retry(policy) => {
                    self.backoff_policy(&policy, 0).await;
                    active_retry = Some((policy, 1));
                    acted = Some(
                        self.act_chain(
                            frame,
                            step,
                            &step_path,
                            Some(resolved_inputs.clone()),
                            None,
                            0,
                        )
                        .await?,
                    );
                }
                Consulted::Repaired | Consulted::RepairDone => {
                    // The world was (declared) fixed: one re-entry; a
                    // further negative re-consults (maxTriggers bounds
                    // the total).
                    acted = Some(
                        self.act_chain(
                            frame,
                            step,
                            &step_path,
                            Some(resolved_inputs.clone()),
                            None,
                            0,
                        )
                        .await?,
                    );
                }
                Consulted::Pending(pending) => {
                    // The verdict of this round is on the ledger (recorded
                    // above): the resume segment re-enters through the
                    // recorded-ruling jump. Span stays open.
                    return Ok(Ctl::AwaitHuman(pending));
                }
                Consulted::Propagate(ctl) => return Ok(ctl),
            }
        }
    }

    /// Settles a step whose execution ended in a definite negative (fail)
    /// or an unconfirmable state (unknown): verdict, write-back, exit.
    async fn settle_error(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: &RunPath,
        step_id: &StepId,
        status: VerdictStatus,
        summary: String,
    ) -> Result<Ctl, RunnerError> {
        let folded = FoldedVerdict {
            status,
            degraded: false,
            summary,
        };
        self.record_step_verdict(
            step_path,
            &folded,
            Vec::new(),
            None,
            EvidenceManifest::default(),
        )
        .await?;
        frame.seed_verdict(step_id, status, false);
        self.append(
            step_path,
            &RunLogPayload::StepExited {
                provider_state_summary: None,
                state: StepState::Judged,
                // No output projection completed on the error path.
                output: None,
                localized: Vec::new(),
                localization_gaps: Vec::new(),
            },
        )?;
        if status == VerdictStatus::Fail {
            Ok(Ctl::HaltFail)
        } else {
            Ok(Ctl::Continue)
        }
    }

    /// The act phase: the bound attempt chain with in-attempt retry
    /// (spine §6.5 mount point 1 — every retry is a new callId and a new
    /// WAL intent; the chain advances only on `action_failed_final`).
    async fn act_chain(
        &mut self,
        frame: &FrameState<'a>,
        step: &'a ActionStepIR,
        step_path: &RunPath,
        args_override: Option<Value>,
        mut adopted: Option<AdoptedSettle>,
        start_position: usize,
    ) -> Result<ActPhase, RunnerError> {
        let key = instance_key(step_path);
        let chain_len = step.binding.attempts.len();
        for (position, attempt) in step
            .binding
            .attempts
            .iter()
            .enumerate()
            .skip(start_position)
        {
            // The entry attempt consumes the override snapshot (the ready
            // snapshot on a fresh run, the archived argsSnapshot on a
            // crash re-entry — anchored at the recorded chain position,
            // 07 §1.4); a chain advance re-resolves the next attempt's
            // own argument expressions.
            let args = if position == start_position && args_override.is_some() {
                args_override.clone().expect("checked is_some")
            } else {
                match self.resolve_args(frame, attempt) {
                    Ok(args) => args,
                    Err(message) => {
                        return Ok(ActPhase::StepFail {
                            class: ErrorClass::BindArgumentsInvalid,
                            message,
                        });
                    }
                }
            };

            let mut tries: u32 = 0;
            loop {
                tries += 1;
                let (outcome, settled_seq, attempt_n) = match adopted.take() {
                    // The reconciled terminal is this try's settled
                    // outcome; its WAL intent and `actionSettled` are
                    // already on record — no dispatch.
                    Some(settle) => (settle.outcome, settle.settled_seq, settle.attempt_n),
                    None => {
                        let attempt_n = self.next_attempt_n(&key);
                        let call_id = uuid::Uuid::new_v4().to_string();
                        let mut attempt_path = step_path.clone();
                        attempt_path.push(PathFrame::Attempt { n: attempt_n });
                        // WAL discipline (spine §6.2): the intent commit
                        // *is* the fsync; only after it returns may the
                        // dispatch leave.
                        self.store.write_action_intent(
                            &self.run_id,
                            now_ms(),
                            &attempt_path,
                            &call_id,
                            args.clone(),
                            Some(pointlock_store::IntentDispatch {
                                chain_index: (position + 1) as u32,
                                channel: attempt.channel,
                                action_name: attempt.action_name.clone(),
                            }),
                        )?;
                        let call = BoundActionCall {
                            call_id: call_id.clone(),
                            action_name: attempt.action_name.clone(),
                            arguments: args.clone(),
                            action_timeout_ms: step.base.timeout_ms,
                            request_timeout_ms: None,
                        };
                        let outcome = match self.session.execute(call, None).await {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                // No terminal could be obtained: the intent
                                // stays pending; suspend and reconcile on
                                // resume.
                                return Ok(ActPhase::Suspend(format!(
                                    "no terminal for callId {call_id}: {error}"
                                )));
                            }
                        };
                        let outcome = quarantine_unpersistable(outcome);
                        let settled_seq = self.append(
                            &attempt_path,
                            &RunLogPayload::ActionSettled {
                                call_id: call_id.clone(),
                                outcome: outcome.clone(),
                            },
                        )?;
                        (outcome, settled_seq, attempt_n)
                    }
                };
                match outcome {
                    ActionOutcome::Succeeded { result } => {
                        let degraded = !execution_accepted(attempt, &result.execution);
                        return Ok(ActPhase::Succeeded {
                            result,
                            degraded,
                            settled_seq,
                            attempt_n,
                        });
                    }
                    other => {
                        let class = classify(&other);
                        let message = terminal_message(&other);
                        if class == ErrorClass::ActionCancelled {
                            return Ok(ActPhase::Aborted);
                        }
                        if retry_allowed(step, class, tries) {
                            self.backoff(step, tries).await;
                            continue;
                        }
                        match class {
                            // Chain advance: only a final (possibly
                            // degradable) failure tries the next attempt.
                            ErrorClass::ActionFailedFinal if position + 1 < chain_len => break,
                            ErrorClass::ActionTimedOut => {
                                // A recorded `timedOut` is a certain fate —
                                // reconcile returns it verbatim and adds
                                // nothing — so the only confirmation channel
                                // left is OBSERVATION (spine §5 / 07 §4.3).
                                // With assertions declared, the settlement
                                // loop asks the world; without any there is
                                // nothing that could confirm, and the step
                                // folds to the honest unknown directly.
                                if !step.assertions.is_empty() {
                                    return Ok(ActPhase::Unconfirmed {
                                        message: format!("action timed out ({message})"),
                                    });
                                }
                                return Ok(ActPhase::StepUnknown {
                                    message: format!(
                                        "action timed out and the outcome could not be \
                                         confirmed: {message}"
                                    ),
                                    error_class: None,
                                });
                            }
                            ErrorClass::SessionDegraded => {
                                // spine §5: "当前 step → unknown，触发 flow 级
                                // onError handler" — both halves, so the
                                // class rides along to the hook selector.
                                return Ok(ActPhase::StepUnknown {
                                    message: format!("session degraded: {message}"),
                                    error_class: Some(ErrorClass::SessionDegraded),
                                });
                            }
                            _ => {
                                return Ok(ActPhase::StepFail { class, message });
                            }
                        }
                    }
                }
            }
        }
        unreachable!("the act chain always returns from within its last attempt")
    }

    /// Allocates the next attempt number for a step instance (monotonic
    /// across resume segments — the base is harvested from the log).
    fn next_attempt_n(&mut self, key: &str) -> u64 {
        let counter = self.attempt_base.entry(key.to_owned()).or_insert(0);
        *counter += 1;
        *counter
    }

    /// Waits out the retry backoff (spine §6.5 mount 1).
    async fn backoff(&self, step: &ActionStepIR, tries: u32) {
        let Some(policy) = &step.base.retry else {
            return;
        };
        self.backoff_policy(policy, tries).await;
    }

    /// Sleeps one backoff period of an explicit policy (the handler-retry
    /// disposition carries its own policy, independent of `StepBase.retry`
    /// — spine §6.5 mount 2).
    async fn backoff_policy(&self, policy: &RetryPolicy, tries: u32) {
        let ms = backoff_ms(policy, tries);
        if ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
    }

    // ─── human steps & supervision gate (06 §2/§5; spine §6.8/§6.9) ─────────

    /// The R13 supervision gate of one action step, consulted between the
    /// preflight probes and the act chain (i.e. strictly before any
    /// `actionIntent`). Returns `None` to let the dispatch proceed.
    ///
    /// A pending gate request from a previous segment resolves first —
    /// regardless of *this* segment's policy (the request survives across
    /// segments, spine §6.9): `proceed` falls through to the intent,
    /// `abort` exits the step `aborted` and aborts the run without
    /// consulting any handler, anything else (unanswered, or the non-final
    /// `suspend` ruling) re-awaits. Fresh gating follows this segment's
    /// policy: `mutating` gates mutating action steps only, `all` gates
    /// every action step.
    fn gate_supervision(
        &mut self,
        step_path: &RunPath,
        step: &'a ActionStepIR,
        resolved_inputs: &Value,
    ) -> Result<Option<Ctl>, RunnerError> {
        let key = instance_key(step_path);
        if let Some(fact) = self
            .human
            .get(&key)
            .filter(|fact| fact.purpose == HumanPurpose::Supervision)
        {
            let decision = fact
                .final_response
                .as_ref()
                .and_then(|response| response.get("decision"))
                .and_then(Value::as_str);
            return match decision {
                // humanResponded(proceed) is on the ledger before the
                // intent this clears the way for (§6.9 WAL order).
                Some("proceed") => Ok(None),
                Some("abort") => {
                    // The human ruling is final: no handler is consulted
                    // (§6.9 R13); the step exits `aborted` and the run
                    // takes the existing aborted terminal.
                    self.append(
                        step_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Aborted,
                            output: None,
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    Ok(Some(Ctl::Abort))
                }
                _ => Ok(Some(Ctl::AwaitHuman(HumanPending {
                    run_path: fact.run_path.clone(),
                    request_id: fact.request_id.clone(),
                    purpose: HumanPurpose::Supervision,
                    mode: None,
                    prompt: fact.prompt.clone(),
                    deadline_at_ms: None,
                }))),
            };
        }
        let Some(policy) = self.supervise else {
            return Ok(None);
        };
        let gated = match policy {
            SupervisePolicy::All => true,
            SupervisePolicy::Mutating => step.effect == EffectClassAction::Mutating,
        };
        if !gated {
            return Ok(None);
        }
        // Fresh gate: auto-generated description over runPath /
        // actionName / resolvedInputs (§6.9). No mode, no decisions
        // contract, no deadline — the decision vocabulary is the closed
        // proceed | abort | suspend, arbitrated by the store.
        let attempt = step
            .binding
            .attempts
            .first()
            .expect("sealed action steps carry at least one bound attempt");
        let action_name = attempt.action_name.as_str().to_owned();
        let rendered = render_run_path(step_path);
        let request_id = uuid::Uuid::new_v4().to_string();
        let prompt = format!(
            "Supervision gate: approve dispatching action '{action_name}' at {rendered}? \
             The resolved inputs are presented."
        );
        let presents = serde_json::json!([
            { "kind": "value", "label": "runPath", "value": rendered },
            { "kind": "value", "label": "actionName", "value": action_name },
            { "kind": "value", "label": "resolvedInputs", "value": resolved_inputs },
        ]);
        // fsync-before-notify (spine §6.9): the append commit *is* the
        // fsync; the runner then suspends — any notification happens in
        // the CLI layer, strictly after this returns.
        self.append(
            step_path,
            &RunLogPayload::HumanRequested {
                request_id: request_id.clone(),
                purpose: HumanPurpose::Supervision,
                mode: None,
                prompt: prompt.clone(),
                presents,
                decisions: None,
                output_schema: None,
                deadline_at_ms: None,
            },
        )?;
        Ok(Some(Ctl::AwaitHuman(HumanPending {
            run_path: step_path.clone(),
            request_id,
            purpose: HumanPurpose::Supervision,
            mode: None,
            prompt,
            deadline_at_ms: None,
        })))
    }

    /// Executes one human step (06 §5.1 pinned order): ready (presents
    /// materialized once and frozen) → `stepEntered` → declared preflight
    /// → `humanRequested` (fsynced by its commit) → suspend /
    /// [`RunOutcome::AwaitingHuman`]. Resume settles a paired response
    /// through the four-mode mapping, re-awaits an unanswered request
    /// inside its deadline, and lazily settles an expired one to `unknown`
    /// — the settlement result depends only on `deadlineAtMs` and response
    /// presence, never on the settlement instant (06 §5.3).
    async fn exec_human(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: RunPath,
        step: &'a HumanStepIR,
    ) -> Result<Ctl, RunnerError> {
        let step_id = step.base.step_id.clone();
        let key = instance_key(&step_path);

        // A request already on the ledger for this instance: settle or
        // keep waiting — never a second `humanRequested` while one is
        // pending.
        if let Some(fact) = self
            .human
            .get(&key)
            .filter(|fact| fact.purpose == HumanPurpose::Step)
        {
            let fact = fact.clone();
            if let Some(response) = fact.final_response.clone() {
                // The span is open by construction (settlement closes it
                // for good); enter_step only consumes it.
                self.enter_step(&step_path, &step.base, Value::Null)?;
                return self
                    .settle_human_response(frame, &step_path, step, &fact, response)
                    .await;
            }
            match fact.deadline_at_ms {
                Some(deadline) if self.now() > deadline => {
                    self.enter_step(&step_path, &step.base, Value::Null)?;
                    return self
                        .settle_human_timeout(frame, &step_path, step, &fact)
                        .await;
                }
                // Unanswered and not expired: re-await the same request
                // (no new requestId, no re-notify obligation here).
                _ => {
                    return Ok(Ctl::AwaitHuman(HumanPending {
                        run_path: fact.run_path.clone(),
                        request_id: fact.request_id.clone(),
                        purpose: HumanPurpose::Step,
                        mode: Some(step.mode),
                        prompt: fact.prompt.clone(),
                        deadline_at_ms: fact.deadline_at_ms,
                    }));
                }
            }
        }

        // Ready: materialize `presents` once and freeze — the
        // `resolvedInputs` snapshot discipline (06 §2.3). A
        // crash/suspension-opened span reuses the archived snapshot.
        let snapshot = match self.open_span_inputs(&key) {
            Some(archived) => archived,
            None => {
                let scope = frame.scope(&self.env, None);
                let mut items = Vec::with_capacity(step.presents.len());
                let mut error = None;
                for (index, expr) in step.presents.iter().enumerate() {
                    match pointlock_expr::eval(expr, &scope) {
                        Ok(value) => items.push(value),
                        Err(eval_error) => {
                            error =
                                Some(format!("present #{index} evaluation failed: {eval_error}"));
                            break;
                        }
                    }
                }
                if let Some(message) = error {
                    // A failing presents evaluation is a compiler /
                    // expression bug signal (bind_arguments_invalid
                    // discipline): step fails, nothing is asked.
                    self.enter_step(&step_path, &step.base, Value::Null)?;
                    return self
                        .settle_error(
                            frame,
                            &step_path,
                            &step_id,
                            VerdictStatus::Fail,
                            format!("human presents failed [bind_arguments_invalid]: {message}"),
                        )
                        .await;
                }
                serde_json::json!({ "presents": items })
            }
        };
        self.enter_step(&step_path, &step.base, snapshot.clone())?;
        if let Some(ctl) = self.probe_or_note(frame, &step_path, &step.base).await? {
            return Ok(ctl);
        }
        let presents = snapshot
            .get("presents")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        let request_id = uuid::Uuid::new_v4().to_string();
        // `timeoutMs` converts to the absolute deadline watermark at
        // request creation (06 §5.3) — the sole lazy-settlement input.
        let deadline_at_ms = self.now().saturating_add(step.timeout_ms);
        // fsync-before-notify (spine §6.8, 06 §5.1): the append commit
        // *is* the fsync (WAL + synchronous=FULL); the runner suspends
        // and never notifies — channels are the CLI layer's job.
        self.append(
            &step_path,
            &RunLogPayload::HumanRequested {
                request_id: request_id.clone(),
                purpose: HumanPurpose::Step,
                mode: Some(step.mode),
                prompt: step.prompt.clone(),
                presents,
                decisions: step.decisions.clone(),
                output_schema: step.output_schema.clone(),
                deadline_at_ms: Some(deadline_at_ms),
            },
        )?;
        Ok(Ctl::AwaitHuman(HumanPending {
            run_path: step_path,
            request_id,
            purpose: HumanPurpose::Step,
            mode: Some(step.mode),
            prompt: step.prompt.clone(),
            deadline_at_ms: Some(deadline_at_ms),
        }))
    }

    /// Settles a human step from its arbitrated final response — the
    /// four-mode verdict/output mapping (06 §2.2 as adjudicated):
    ///
    /// | mode | response | verdict | step output |
    /// |---|---|---|---|
    /// | `confirm` | `decision` = first label | pass | the response object |
    /// | `confirm` | `decision` = second label | fail | the response object |
    /// | `judge` | `status` pass/fail/unknown | verbatim | the response object |
    /// | `provideInput` | `input` (schema-checked by the store) | pass | the input value |
    /// | `repairWorld` | `decision: "done"` | pass (testimony, not observation; never degraded) | the response object |
    /// | `repairWorld` | `decision: "cannotRepair"` | fail (06 §2.2 — a verdict, catchable by `onFail`; never a run abort) | the response object |
    ///
    /// Every settlement materializes the response as a canonical JSON
    /// evidence document and cites it from the verdict (06 §6).
    async fn settle_human_response(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: &RunPath,
        step: &'a HumanStepIR,
        fact: &HumanRequestFact,
        response: Value,
    ) -> Result<Ctl, RunnerError> {
        let step_id = step.base.step_id.clone();
        let decision = response
            .get("decision")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let (status, output, summary) = match step.mode {
            HumanMode::Confirm => {
                let labels = step
                    .decisions
                    .as_ref()
                    .expect("load validated confirm decisions");
                // Position-mapped double label: first → pass, second →
                // fail (membership was the store arbitration's check).
                let status = if labels.first().map(String::as_str) == Some(decision.as_str()) {
                    VerdictStatus::Pass
                } else {
                    VerdictStatus::Fail
                };
                let position = if status == VerdictStatus::Pass {
                    "first"
                } else {
                    "second"
                };
                (
                    status,
                    Some(response.clone()),
                    format!(
                        "human confirm decision '{decision}' is the {position} label \
                         (position-mapped verdict)"
                    ),
                )
            }
            HumanMode::Judge => {
                // The human ruling *is* the verdict (spine §6.3).
                let status = match response.get("status").and_then(Value::as_str) {
                    Some("pass") => VerdictStatus::Pass,
                    Some("fail") => VerdictStatus::Fail,
                    _ => VerdictStatus::Unknown,
                };
                let label = response
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                (
                    status,
                    Some(response.clone()),
                    format!("human judge ruling: {label}"),
                )
            }
            HumanMode::ProvideInput => {
                // The store validated `input` against `outputSchema`; the
                // established fact is "a human provided schema-valid
                // input" — pass, and the input *is* the step output.
                let input = response.get("input").cloned().unwrap_or(Value::Null);
                (
                    VerdictStatus::Pass,
                    Some(input),
                    "human provided input validated against the outputSchema".to_owned(),
                )
            }
            HumanMode::RepairWorld => {
                // 06 §2.2's closed mapping: `done` → pass, `cannotRepair`
                // → fail. Testimony, not observation — pass carries no
                // degraded flag (machine re-checks belong to follow-up
                // assert/preflight steps), and a fail is a verdict the
                // step's own `onFail` may catch, never a unilateral run
                // abort.
                let status = if decision == "done" {
                    VerdictStatus::Pass
                } else {
                    VerdictStatus::Fail
                };
                let summary = if status == VerdictStatus::Pass {
                    "human declared the world repaired (testimony; follow-up machine \
                     re-check advised)"
                        .to_owned()
                } else {
                    "human declared the world unrepairable (cannotRepair)".to_owned()
                };
                (status, Some(response.clone()), summary)
            }
        };
        let asset = self.put_human_evidence(step_path, fact, Some(&response), "response")?;
        let folded = FoldedVerdict {
            status,
            degraded: false,
            summary,
        };
        let verdict_seq = self
            .record_step_verdict(
                step_path,
                &folded,
                vec![asset.clone()],
                None,
                human_manifest(&asset),
            )
            .await?;
        if let Some(sha256) = &asset.sha256 {
            self.store
                .link_evidence(&self.run_id, verdict_seq, &asset.id, sha256)?;
        }
        frame.seed_verdict(&step_id, status, false);
        self.append(
            step_path,
            &RunLogPayload::StepExited {
                provider_state_summary: None,
                state: StepState::Judged,
                output: output.clone(),
                localized: Vec::new(),
                localization_gaps: Vec::new(),
            },
        )?;
        if let Some(output) = output {
            frame.outputs.insert(step_id.as_str().to_owned(), output);
        }
        if status == VerdictStatus::Fail {
            Ok(Ctl::HaltFail)
        } else {
            Ok(Ctl::Continue)
        }
    }

    /// Lazy timeout settlement (06 §5.3): the deadline passed with no
    /// arbitrated response — verdict `unknown` (`onTimeout` is fixed),
    /// no output (downstream consumers of it block). The judgment inputs
    /// are `deadlineAtMs` and response absence only; the settlement
    /// instant leaves no trace in the verdict ("no one came" is itself a
    /// recorded historical fact, 06 §6).
    async fn settle_human_timeout(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: &RunPath,
        step: &'a HumanStepIR,
        fact: &HumanRequestFact,
    ) -> Result<Ctl, RunnerError> {
        let step_id = &step.base.step_id;
        let deadline = fact.deadline_at_ms.unwrap_or_default();
        let asset = self.put_human_evidence(step_path, fact, None, "timeout")?;
        let folded = FoldedVerdict {
            status: VerdictStatus::Unknown,
            degraded: false,
            summary: format!(
                "human response deadline (deadlineAtMs {deadline}) passed without a \
                 response; onTimeout is fixed to unknown"
            ),
        };
        let verdict_seq = self
            .record_step_verdict(
                step_path,
                &folded,
                vec![asset.clone()],
                None,
                human_manifest(&asset),
            )
            .await?;
        if let Some(sha256) = &asset.sha256 {
            self.store
                .link_evidence(&self.run_id, verdict_seq, &asset.id, sha256)?;
        }
        frame.seed_verdict(step_id, VerdictStatus::Unknown, false);

        // A timed-out human step is an unknown verdict: the onUnknown
        // ladder applies (06's escalation pattern). Re-ask dispositions
        // (retry/repair) need fresh-request machinery — typed M2 refusal.
        match self
            .consult_hook(
                frame,
                step_path,
                step.base.handlers.as_deref(),
                HandlerHook::OnUnknown,
                None,
            )
            .await?
        {
            Consulted::None | Consulted::RepairFailed | Consulted::Continue => {}
            Consulted::Abort => {
                self.append(
                    step_path,
                    &RunLogPayload::StepExited {
                        provider_state_summary: None,
                        state: StepState::Aborted,
                        output: None,
                        localized: Vec::new(),
                        localization_gaps: Vec::new(),
                    },
                )?;
                return Ok(Ctl::Abort);
            }
            Consulted::Escalated {
                status: ruled,
                summary: ruled_summary,
                evidence: ruling_evidence,
            } => {
                let folded = FoldedVerdict {
                    status: ruled,
                    degraded: false,
                    summary: ruled_summary,
                };
                let (cited, manifest) = escalate_verdict_material(&ruling_evidence);
                let ruled_seq = self
                    .record_step_verdict(
                        step_path,
                        &folded,
                        cited,
                        Some(format!("seq:{verdict_seq}")),
                        manifest,
                    )
                    .await?;
                self.link_ruling_evidence(ruled_seq, &ruling_evidence)?;
                frame.reseed_last(step_id, ruled, false);
                self.append(
                    step_path,
                    &RunLogPayload::StepExited {
                        provider_state_summary: None,
                        state: StepState::Judged,
                        output: None,
                        localized: Vec::new(),
                        localization_gaps: Vec::new(),
                    },
                )?;
                return Ok(if ruled == VerdictStatus::Fail {
                    Ctl::HaltFail
                } else {
                    Ctl::Continue
                });
            }
            Consulted::Retry(_) | Consulted::Repaired | Consulted::RepairDone => {
                return Err(RunnerError::M0Unsupported {
                    detail: format!(
                        "re-ask dispositions on the timed-out human step '{step_id}' need \
                         fresh-request machinery — not in the M2 subset \
                         (escalate/continue/abort are supported)"
                    ),
                });
            }
            Consulted::Pending(pending) => return Ok(Ctl::AwaitHuman(pending)),
            Consulted::Propagate(ctl) => return Ok(ctl),
        }

        self.append(
            step_path,
            &RunLogPayload::StepExited {
                provider_state_summary: None,
                state: StepState::Judged,
                output: None,
                localized: Vec::new(),
                localization_gaps: Vec::new(),
            },
        )?;
        Ok(Ctl::Continue)
    }

    /// Materializes a human settlement as a canonical JSON evidence
    /// document in the content-addressed store and mints its citable
    /// [`AssetRef`] (06 §6: who / when-relative-to-deadline / what was
    /// decided / what was presented). Deliberately clock-free: the
    /// document is a pure function of the request and the arbitrated
    /// response (or its absence).
    /// Links an escalate ruling's settlement document to its superseding
    /// verdict row (same file-before-row-before-log join as body humans).
    fn link_ruling_evidence(
        &mut self,
        verdict_seq: u64,
        evidence: &Option<AssetRef>,
    ) -> Result<(), RunnerError> {
        if let Some(asset) = evidence
            && let Some(sha256) = &asset.sha256
        {
            self.store
                .link_evidence(&self.run_id, verdict_seq, &asset.id, sha256)?;
        }
        Ok(())
    }

    fn put_human_evidence(
        &mut self,
        step_path: &RunPath,
        fact: &HumanRequestFact,
        response: Option<&Value>,
        settled_as: &str,
    ) -> Result<AssetRef, RunnerError> {
        let document = serde_json::json!({
            "pointlockEvidence": "humanResponse/1",
            "requestId": fact.request_id,
            "runId": self.run_id,
            "runPath": render_run_path(step_path),
            "purpose": fact.purpose,
            "mode": fact.mode,
            "prompt": fact.prompt,
            "presented": fact.presents,
            "response": response.cloned().unwrap_or(Value::Null),
            "actor": match (settled_as, &fact.final_actor) {
                // Timeout settlements have no actor (06 §6).
                ("timeout", _) => Value::Null,
                (_, Some(actor)) => Value::String(actor.clone()),
                (_, None) => Value::Null,
            },
            "deadlineAtMs": fact.deadline_at_ms,
            "settledAs": settled_as,
        });
        let bytes = to_canonical_json(&document).into_bytes();
        // file-before-row-before-log: the bytes are durable before the
        // verdict event cites them.
        let put = self.store.put_evidence(&bytes, "application/json")?;
        Ok(AssetRef {
            id: format!("humanResponse:{}", fact.request_id),
            media_type: "application/json".to_owned(),
            // Locally minted evidence: the URI is the content-addressed
            // library path (06 §6).
            uri: put.local_path,
            sha256: Some(put.sha256),
        })
    }

    // ─── handler engine (spine §3/§6.5 mount 2; M2 W3) ──────────────────────
    //
    // Handlers are explicit policies on four hooks; they yield a
    // disposition, never data (R10). Consultation happens *inside* the
    // host step's open span, before `stepExited`, so a retry disposition
    // re-enters the failing phase within the same entered/exited pair.
    // The `handlerTriggered` audit event anchors at the host step path;
    // the hook frame (`/hook:<name>:<n>`) anchors the disposition's own
    // work (escalate humans, repair frames).

    /// Resolves the binding a hook consults: the step-level list first
    /// (first match wins), else the flow-level list (spine §3
    /// `StepBase.handlers` overrides `FlowIR.handlers`). `onError`
    /// bindings additionally filter on `errorClasses`.
    fn resolve_binding(
        &self,
        frame: &FrameState<'a>,
        step_handlers: Option<&'a [HandlerBinding]>,
        hook: HandlerHook,
        error_class: Option<ErrorClass>,
    ) -> Option<&'a HandlerBinding> {
        let matches = |binding: &&'a HandlerBinding| {
            binding.hook == hook
                && (hook != HandlerHook::OnError
                    || match (&binding.error_classes, error_class) {
                        (None, _) => true,
                        (Some(filter), Some(class)) => filter.contains(&class),
                        (Some(_), None) => false,
                    })
        };
        step_handlers
            .and_then(|bindings| bindings.iter().find(matches))
            .or_else(|| {
                frame
                    .flow
                    .handlers
                    .as_deref()
                    .and_then(|bindings| bindings.iter().find(matches))
            })
    }

    /// Consults the matching handler binding for `hook` on the host step.
    ///
    /// Trigger counting is per host instance per hook and continues
    /// across segments (harvested from the ledger); an exhausted budget
    /// returns [`Consulted::None`] — the natural path stands. A pending
    /// escalate continuation (a previous segment's unanswered hook human)
    /// is settled or re-awaited *without* consuming a new trigger.
    async fn consult_hook(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: &RunPath,
        step_handlers: Option<&'a [HandlerBinding]>,
        hook: HandlerHook,
        error_class: Option<ErrorClass>,
    ) -> Result<Consulted, RunnerError> {
        let Some(binding) = self.resolve_binding(frame, step_handlers, hook, error_class) else {
            return Ok(Consulted::None);
        };
        let counter_key = crate::align::hook_trigger_key(&instance_key(step_path), hook);
        let current = self.hook_triggers.get(&counter_key).copied().unwrap_or(0);

        // Escalate continuation: trigger N already on the ledger and its
        // hook human still governs — settle or re-await, no new trigger.
        if current >= 1
            && let HandlerAction::Escalate { human } = &binding.action
        {
            let human_path = hook_child_path(step_path, hook, current, human);
            if self.human.contains_key(&instance_key(&human_path)) {
                // A request exists for the current trigger: it fully
                // governs (settle its response, its timeout, or re-await).
                return self
                    .run_hook_human(frame, step_path, hook, current, human)
                    .await;
            }
        }

        let trigger = current + 1;
        if trigger > u64::from(binding.max_triggers) {
            return Ok(Consulted::None);
        }
        self.hook_triggers.insert(counter_key, trigger);
        let disposition = match &binding.action {
            HandlerAction::Retry { .. } => "retry",
            HandlerAction::Continue => "continue",
            HandlerAction::Abort => "abort",
            HandlerAction::Escalate { .. } => "escalate",
            HandlerAction::Repair { .. } => "repair",
        };
        self.append(
            step_path,
            &RunLogPayload::HandlerTriggered {
                hook,
                trigger,
                disposition: Some(disposition.to_owned()),
            },
        )?;

        match &binding.action {
            HandlerAction::Retry { policy } => Ok(Consulted::Retry(policy.clone())),
            HandlerAction::Continue => Ok(Consulted::Continue),
            HandlerAction::Abort => Ok(Consulted::Abort),
            HandlerAction::Escalate { human } => {
                self.run_hook_human(frame, step_path, hook, trigger, human)
                    .await
            }
            HandlerAction::Repair { flow_ref } => {
                self.run_repair(frame, step_path, hook, trigger, flow_ref)
                    .await
            }
        }
    }

    /// Runs (or settles) an escalate hook human. Hook humans are not body
    /// steps: they open no span and seed no frame verdict — their ruling
    /// is returned to the consultation site, which supersedes the host
    /// verdict and cites the canonical settlement evidence document
    /// minted here (06 §6; the request/response ledger pair is the join).
    async fn run_hook_human(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: &RunPath,
        hook: HandlerHook,
        trigger: u64,
        human: &'a HumanStepIR,
    ) -> Result<Consulted, RunnerError> {
        let human_path = hook_child_path(step_path, hook, trigger, human);
        let key = instance_key(&human_path);
        if let Some(fact) = self.human.get(&key) {
            let fact = fact.clone();
            if let Some(response) = fact.final_response.clone() {
                // Consume the settlement: a ruling governs exactly once —
                // a later consult on the same host walks a *new* trigger
                // (or exhausts the budget), never re-reads this answer.
                self.human.remove(&key);
                // Every settlement materializes the canonical evidence
                // document (06 §6); the Escalated superseding verdict
                // cites it below. Non-verdict dispositions (Repaired/
                // Abort) keep it durable in the library with the
                // request/response pair as the join.
                let asset =
                    self.put_human_evidence(&human_path, &fact, Some(&response), "response")?;
                return Ok(match map_escalate_response(human, &response) {
                    Consulted::Escalated {
                        status, summary, ..
                    } => Consulted::Escalated {
                        status,
                        summary,
                        evidence: Some(asset),
                    },
                    other => other,
                });
            }
            match fact.deadline_at_ms {
                Some(deadline) if self.now() > deadline => {
                    // Lazy timeout settlement: unknown, fixed (onTimeout);
                    // consumed like any settlement — with the canonical
                    // evidence document (06 §6, actor null on timeout).
                    self.human.remove(&key);
                    let asset = self.put_human_evidence(&human_path, &fact, None, "timeout")?;
                    return Ok(Consulted::Escalated {
                        status: VerdictStatus::Unknown,
                        summary: format!(
                            "escalate human '{}' timed out (deadline watermark passed): unknown",
                            human.base.step_id
                        ),
                        evidence: Some(asset),
                    });
                }
                _ => {
                    return Ok(Consulted::Pending(HumanPending {
                        run_path: fact.run_path.clone(),
                        request_id: fact.request_id.clone(),
                        purpose: HumanPurpose::Step,
                        mode: Some(human.mode),
                        prompt: fact.prompt.clone(),
                        deadline_at_ms: fact.deadline_at_ms,
                    }));
                }
            }
        }

        // First encounter: materialize presents in the host frame's scope
        // and freeze; fsync-before-notify discipline as everywhere.
        let scope = frame.scope(&self.env, None);
        let mut items = Vec::with_capacity(human.presents.len());
        for expr in &human.presents {
            match pointlock_expr::eval(expr, &scope) {
                Ok(value) => items.push(value),
                // A failing presents expression degrades to an empty
                // exhibit — the request still goes out (the human can
                // rule without exhibits; principle 8 over strictness).
                Err(_) => items.push(Value::Null),
            }
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        let deadline_at_ms = self.now().saturating_add(human.timeout_ms);
        self.append(
            &human_path,
            &RunLogPayload::HumanRequested {
                request_id: request_id.clone(),
                purpose: HumanPurpose::Step,
                mode: Some(human.mode),
                prompt: human.prompt.clone(),
                presents: Value::Array(items),
                decisions: human.decisions.clone(),
                output_schema: human.output_schema.clone(),
                deadline_at_ms: Some(deadline_at_ms),
            },
        )?;
        Ok(Consulted::Pending(HumanPending {
            run_path: human_path,
            request_id,
            purpose: HumanPurpose::Step,
            mode: Some(human.mode),
            prompt: human.prompt.clone(),
            deadline_at_ms: Some(deadline_at_ms),
        }))
    }

    /// Runs a repair subflow under the hook frame (a call frame without a
    /// host call step — spine §9). Repair flows take no caller inputs
    /// (their params materialize from declared defaults, 06 §7.5 binding-
    /// flow pattern); they yield no data (R10) — only their flow verdict
    /// comes back as the disposition signal.
    async fn run_repair(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: &RunPath,
        hook: HandlerHook,
        trigger: u64,
        flow_ref: &'a pointlock_ir::FlowRef,
    ) -> Result<Consulted, RunnerError> {
        let callee = self.flows.callee(flow_ref);
        let mut repair_path = step_path.clone();
        repair_path.push(hook_frame(hook, trigger));
        repair_path.push(PathFrame::Call {
            step_id: None,
            callee_flow_id: callee.flow_id.clone(),
            callee_ir_hash: callee.ir_hash.clone(),
        });
        // Defaults-only inbound materialization.
        let params = match call_inputs_gate(callee, Map::new()) {
            Ok(params) => params,
            // A repair flow whose params cannot materialize from defaults
            // is a repair failure; its detail is compiler-diagnosable.
            Err(_) => return Ok(Consulted::RepairFailed),
        };
        self.append(
            &repair_path,
            &RunLogPayload::CallFramePushed {
                frame: CallFrame {
                    flow_id: callee.flow_id.clone(),
                    ir_hash: callee.ir_hash.clone(),
                    call_step_id: None,
                    inputs_snapshot: Value::Object(params.clone()),
                    vars: BTreeMap::new(),
                    iter_stack: Vec::new(),
                    next_index: 0,
                },
                rebase: false,
            },
        )?;
        let mut repair_frame =
            FrameState::new(callee, repair_path.clone(), params, frame.depth + 1);
        let ctl = self
            .exec_body(&mut repair_frame, repair_path.clone(), &callee.body, 0)
            .await?;
        match ctl {
            Ctl::Continue | Ctl::HaltFail => {}
            other => {
                // Suspension inside a repair leaves its frame live; the
                // resume path refuses live hook frames with a typed error
                // (registered M2 limitation) — the ledger stays honest.
                return Ok(Consulted::Propagate(other));
            }
        }
        self.append(
            &repair_path,
            &RunLogPayload::CallFramePopped { outputs: None },
        )?;
        let verdict = fold_flow_verdict(&repair_frame.fold, callee.verdict_policy);
        match (ctl, verdict) {
            (Ctl::HaltFail, _) => Ok(Consulted::RepairFailed),
            (_, Some(folded)) if folded.status != VerdictStatus::Pass => {
                Ok(Consulted::RepairFailed)
            }
            _ => Ok(Consulted::RepairDone),
        }
    }

    // ─── call steps (07 §1) ─────────────────────────────────────────────────

    async fn exec_call(
        &mut self,
        frame: &mut FrameState<'a>,
        call_path: RunPath,
        step: &'a CallStepIR,
    ) -> Result<Ctl, RunnerError> {
        let step_id = step.base.step_id.clone();
        let key = instance_key(&call_path);
        let callee: &'a FlowIR = self.flows.callee(&step.flow_ref);
        // Runtime defense line for maxCallDepth (the load check already
        // bounds the static closure; this guards the walk itself).
        if frame.depth + 1 > MAX_CALL_DEPTH {
            return Err(RunnerError::CallDepthExceeded {
                depth: frame.depth + 1,
                max: MAX_CALL_DEPTH,
            });
        }

        // Ready: call-by-value inputs snapshot. A suspension-opened span
        // reuses the archived snapshot (already gated) — never
        // re-evaluated (spine §6.6, 07 §5.2 corollary).
        let archived = self.open_span_inputs(&key);
        let gated = match archived {
            Some(Value::Object(map)) => map,
            Some(other) => {
                // A non-object archived snapshot is a ledger anomaly; the
                // honest disposition is a bind-class failure.
                self.enter_step(&call_path, &step.base, other)?;
                return self
                    .settle_error(
                        frame,
                        &call_path,
                        &step_id,
                        VerdictStatus::Fail,
                        "archived call inputs snapshot is not an object".to_owned(),
                    )
                    .await;
            }
            None => {
                // Evaluate each input expression in the *caller* scope.
                let scope = frame.scope(&self.env, None);
                let mut inputs = Map::new();
                let mut eval_error = None;
                for (name, expr) in step.inputs.iter() {
                    match pointlock_expr::eval(expr, &scope) {
                        Ok(value) => {
                            inputs.insert(name.as_str().to_owned(), value);
                        }
                        Err(error) => {
                            eval_error = Some(format!("input '{name}' evaluation failed: {error}"));
                            break;
                        }
                    }
                }
                if let Some(message) = eval_error {
                    self.enter_step(&call_path, &step.base, Value::Null)?;
                    return self
                        .settle_error(
                            frame,
                            &call_path,
                            &step_id,
                            VerdictStatus::Fail,
                            format!("call inputs failed [bind_arguments_invalid]: {message}"),
                        )
                        .await;
                }
                // Inbound gate: defaults + per-param schema validation
                // (07 §1.1 — runtime re-check; failure classifies
                // bind_arguments_invalid, no retry).
                match call_inputs_gate(callee, inputs.clone()) {
                    Ok(gated) => gated,
                    Err(message) => {
                        self.enter_step(&call_path, &step.base, Value::Object(inputs))?;
                        return self
                            .settle_error(
                                frame,
                                &call_path,
                                &step_id,
                                VerdictStatus::Fail,
                                format!("call inputs failed [bind_arguments_invalid]: {message}"),
                            )
                            .await;
                    }
                }
            }
        };
        self.enter_step(&call_path, &step.base, Value::Object(gated.clone()))?;

        if let Some(ctl) = self.probe_or_note(frame, &call_path, &step.base).await? {
            return Ok(ctl);
        }

        // Frame push (07 §3.1 frame-transfer materialization point) —
        // unless a previous segment already pushed it and we are resuming
        // back into the live frame. Re-entering one whose callee pin moved
        // is announced as a `rebase`, so `frames` names the callee actually
        // executing rather than the one the crashed segment entered
        // (07 §5.2 case (a)); the fold updates that frame's `irHash` in
        // place and touches nothing else.
        let open_pin = self.live_frames.remove(&key);
        let rebase = match &open_pin {
            None => false,
            Some(pin) => *pin != callee.ir_hash,
        };
        if open_pin.is_none() || rebase {
            self.append(
                &call_path,
                &RunLogPayload::CallFramePushed {
                    frame: CallFrame {
                        flow_id: callee.flow_id.clone(),
                        ir_hash: callee.ir_hash.clone(),
                        call_step_id: Some(step_id.clone()),
                        inputs_snapshot: Value::Object(gated.clone()),
                        vars: BTreeMap::new(),
                        iter_stack: Vec::new(),
                        next_index: 0,
                    },
                    rebase,
                },
            )?;
        }

        // The callee body runs in a fresh scope: params = inputs, env
        // passes through read-only, the caller's steps/vars are invisible
        // (07 §1.2 — hard boundary).
        let mut callee_frame = FrameState::new(callee, call_path.clone(), gated, frame.depth + 1);
        let ctl = self
            .exec_body(&mut callee_frame, call_path.clone(), &callee.body, 0)
            .await?;
        match ctl {
            Ctl::Continue | Ctl::HaltFail => {}
            Ctl::Abort => {
                // Unwind the frame so the ledger stays balanced; an
                // aborted run makes no semantic claim.
                self.append(
                    &call_path,
                    &RunLogPayload::CallFramePopped { outputs: None },
                )?;
                self.append(
                    &call_path,
                    &RunLogPayload::StepExited {
                        provider_state_summary: None,
                        state: StepState::Aborted,
                        output: None,
                        localized: Vec::new(),
                        localization_gaps: Vec::new(),
                    },
                )?;
                return Ok(Ctl::Abort);
            }
            // Suspension/blocking leaves the frame live: resume falls back
            // into the exact position (07 §4.6), never restarts the frame.
            other => return Ok(other),
        }

        // Outbound gate: declared outputs evaluated in the *callee* scope,
        // schema-validated, snapshotted (07 §1.1). Only a completed body
        // has outputs; a halted callee pops without them.
        let outputs = if matches!(ctl, Ctl::Continue) {
            match self.call_outputs_gate(callee, &callee_frame) {
                Ok(outputs) => Some(outputs),
                Err(message) => {
                    self.append(
                        &call_path,
                        &RunLogPayload::CallFramePopped { outputs: None },
                    )?;
                    return self
                        .settle_error(
                            frame,
                            &call_path,
                            &step_id,
                            VerdictStatus::Fail,
                            format!("callee outputs failed the outbound gate: {message}"),
                        )
                        .await;
                }
            }
        } else {
            None
        };
        self.append(
            &call_path,
            &RunLogPayload::CallFramePopped {
                outputs: outputs.clone(),
            },
        )?;

        // The call step's verdict *is* the callee's flow verdict
        // (spine §6.3); `degraded` propagates verbatim and participates in
        // the caller's fold.
        let callee_verdict = fold_flow_verdict(&callee_frame.fold, callee.verdict_policy);
        let mut status = None;
        let mut verdict_seq = None;
        if let Some(folded) = callee_verdict {
            let folded = FoldedVerdict {
                status: folded.status,
                degraded: folded.degraded,
                summary: format!(
                    "callee '{}' flow verdict: {}",
                    callee.flow_id, folded.summary
                ),
            };
            let seq = self
                .record_step_verdict(
                    &call_path,
                    &folded,
                    Vec::new(),
                    None,
                    EvidenceManifest::default(),
                )
                .await?;
            verdict_seq = Some(seq);
            frame.seed_verdict(&step_id, folded.status, folded.degraded);
            status = Some(folded.status);
        }

        // Handler consultation on the call step's own verdict (the callee
        // handled its internal failures itself; this hook is the caller's
        // policy about the aggregate). Re-invocation dispositions (retry /
        // repair-then-re-call) need the 07 §1 attempt-framed full re-call
        // — a typed M2 refusal, registered.
        if matches!(
            status,
            Some(VerdictStatus::Fail) | Some(VerdictStatus::Unknown)
        ) {
            let hook = if status == Some(VerdictStatus::Fail) {
                HandlerHook::OnFail
            } else {
                HandlerHook::OnUnknown
            };
            match self
                .consult_hook(frame, &call_path, step.base.handlers.as_deref(), hook, None)
                .await?
            {
                Consulted::None | Consulted::RepairFailed => {}
                Consulted::Continue => {
                    self.append(
                        &call_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Judged,
                            output: outputs.clone(),
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    if let Some(outputs) = outputs {
                        frame.outputs.insert(step_id.as_str().to_owned(), outputs);
                    }
                    return Ok(Ctl::Continue);
                }
                Consulted::Abort => {
                    self.append(
                        &call_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Aborted,
                            output: None,
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    return Ok(Ctl::Abort);
                }
                Consulted::Escalated {
                    status: ruled,
                    summary: ruled_summary,
                    evidence: ruling_evidence,
                } => {
                    let folded = FoldedVerdict {
                        status: ruled,
                        degraded: false,
                        summary: ruled_summary,
                    };
                    let supersedes = verdict_seq.map(|seq| format!("seq:{seq}"));
                    let (cited, manifest) = escalate_verdict_material(&ruling_evidence);
                    let ruled_seq = self
                        .record_step_verdict(&call_path, &folded, cited, supersedes, manifest)
                        .await?;
                    self.link_ruling_evidence(ruled_seq, &ruling_evidence)?;
                    frame.reseed_last(&step_id, ruled, false);
                    self.append(
                        &call_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Judged,
                            output: outputs.clone(),
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    if let Some(outputs) = outputs {
                        frame.outputs.insert(step_id.as_str().to_owned(), outputs);
                    }
                    return Ok(if ruled == VerdictStatus::Fail {
                        Ctl::HaltFail
                    } else {
                        Ctl::Continue
                    });
                }
                Consulted::Retry(_) | Consulted::Repaired | Consulted::RepairDone => {
                    return Err(RunnerError::M0Unsupported {
                        detail: format!(
                            "handler re-invocation dispositions on call step '{step_id}' need the 07 §1 \
                             attempt-framed full re-call — not in the M2 subset \
                             (escalate/continue/abort are supported)"
                        ),
                    });
                }
                Consulted::Pending(pending) => {
                    return Ok(Ctl::AwaitHuman(pending));
                }
                Consulted::Propagate(inner) => return Ok(inner),
            }
        }

        self.append(
            &call_path,
            &RunLogPayload::StepExited {
                provider_state_summary: None,
                state: StepState::Judged,
                output: outputs.clone(),
                localized: Vec::new(),
                localization_gaps: Vec::new(),
            },
        )?;
        if let Some(outputs) = outputs {
            frame.outputs.insert(step_id.as_str().to_owned(), outputs);
        }
        if matches!(ctl, Ctl::HaltFail) || status == Some(VerdictStatus::Fail) {
            Ok(Ctl::HaltFail)
        } else {
            Ok(Ctl::Continue)
        }
    }

    /// The outbound gate: callee `outputs` declarations evaluated over the
    /// callee frame's scope and validated against their schemas.
    fn call_outputs_gate(
        &self,
        callee: &FlowIR,
        callee_frame: &FrameState<'a>,
    ) -> Result<Value, String> {
        let scope = callee_frame.scope(&self.env, None);
        let mut outputs = Map::new();
        for decl in &callee.outputs {
            let value = pointlock_expr::eval(&decl.from, &scope)
                .map_err(|error| format!("output '{}' evaluation failed: {error}", decl.name))?;
            jsonschema::validate(decl.schema.as_value(), &value)
                .map_err(|error| format!("output '{}' failed its schema: {error}", decl.name))?;
            outputs.insert(decl.name.as_str().to_owned(), value);
        }
        Ok(Value::Object(outputs))
    }

    // ─── if steps ───────────────────────────────────────────────────────────

    async fn exec_if(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: RunPath,
        step: &'a IfStepIR,
    ) -> Result<Ctl, RunnerError> {
        let step_id = step.base.step_id.clone();
        let key = instance_key(&step_path);
        // A suspension-opened span reuses the archived branch decision —
        // the snapshot rule (spine §6.6) applies to control values too.
        let cond_value = match self.open_span_inputs(&key) {
            Some(archived) => archived.get("cond").cloned().unwrap_or(Value::Null),
            None => {
                let scope = frame.scope(&self.env, None);
                match pointlock_expr::eval(&step.cond, &scope) {
                    Ok(value) => value,
                    Err(error) => {
                        self.enter_step(&step_path, &step.base, Value::Null)?;
                        return self
                            .settle_error(
                                frame,
                                &step_path,
                                &step_id,
                                VerdictStatus::Fail,
                                format!("if cond evaluation failed: {error}"),
                            )
                            .await;
                    }
                }
            }
        };
        // Strict boolean (02 §4.5): anything else is a compiler/expression
        // bug signal — step fail, no branch is taken.
        let Some(cond) = cond_value.as_bool() else {
            self.enter_step(
                &step_path,
                &step.base,
                serde_json::json!({ "cond": cond_value }),
            )?;
            return self
                .settle_error(
                    frame,
                    &step_path,
                    &step_id,
                    VerdictStatus::Fail,
                    format!("if cond evaluated to a non-boolean value: {cond_value}"),
                )
                .await;
        };
        self.enter_step(&step_path, &step.base, serde_json::json!({ "cond": cond }))?;

        if let Some(ctl) = self.probe_or_note(frame, &step_path, &step.base).await? {
            return Ok(ctl);
        }

        let empty: &'a [StepIR] = &[];
        let (selected, unselected): (&'a [StepIR], &'a [StepIR]) = if cond {
            (&step.then, step.r#else.as_deref().unwrap_or(empty))
        } else {
            (step.r#else.as_deref().unwrap_or(empty), &step.then)
        };
        // The unselected branch's steps each leave an
        // entered(null)/exited(skipped) pair — ledger completeness (the
        // blocked precedent). Recorded before the selected branch runs so
        // a mid-branch suspension leaves a complete account.
        self.stash_open_span_summaries().await;
        self.record_pairs(&step_path, unselected, StepState::Skipped)?;

        let ctl = self
            .exec_body(frame, step_path.clone(), selected, 0)
            .await?;
        match ctl {
            Ctl::Continue | Ctl::HaltFail => {
                // Containers yield no verdict of their own (R4): the exit
                // closes the span; child verdicts already folded.
                self.append(
                    &step_path,
                    &RunLogPayload::StepExited {
                        provider_state_summary: None,
                        state: StepState::Judged,
                        output: None,
                        localized: Vec::new(),
                        localization_gaps: Vec::new(),
                    },
                )?;
                Ok(ctl)
            }
            Ctl::Abort => {
                self.append(
                    &step_path,
                    &RunLogPayload::StepExited {
                        provider_state_summary: None,
                        state: StepState::Aborted,
                        output: None,
                        localized: Vec::new(),
                        localization_gaps: Vec::new(),
                    },
                )?;
                Ok(Ctl::Abort)
            }
            other => Ok(other),
        }
    }

    // ─── foreach steps ──────────────────────────────────────────────────────

    async fn exec_foreach(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: RunPath,
        step: &'a ForeachStepIR,
    ) -> Result<Ctl, RunnerError> {
        let step_id = step.base.step_id.clone();
        let key = instance_key(&step_path);
        let items_value = match self.open_span_inputs(&key) {
            Some(archived) => archived.get("items").cloned().unwrap_or(Value::Null),
            None => {
                let scope = frame.scope(&self.env, None);
                match pointlock_expr::eval(&step.items, &scope) {
                    Ok(value) => value,
                    Err(error) => {
                        self.enter_step(&step_path, &step.base, Value::Null)?;
                        return self
                            .settle_error(
                                frame,
                                &step_path,
                                &step_id,
                                VerdictStatus::Fail,
                                format!("foreach items evaluation failed: {error}"),
                            )
                            .await;
                    }
                }
            }
        };
        let Some(items) = items_value.as_array().cloned() else {
            self.enter_step(
                &step_path,
                &step.base,
                serde_json::json!({ "items": items_value, "as": step.r#as.as_str() }),
            )?;
            return self
                .settle_error(
                    frame,
                    &step_path,
                    &step_id,
                    VerdictStatus::Fail,
                    format!("foreach items evaluated to a non-array value: {items_value}"),
                )
                .await;
        };
        // The snapshot carries `{ items, as }`: the position authority for
        // the positional (index-keyed) resume regime and the fold's
        // IterState carrier.
        self.enter_step(
            &step_path,
            &step.base,
            serde_json::json!({ "items": items, "as": step.r#as.as_str() }),
        )?;

        if let Some(ctl) = self.probe_or_note(frame, &step_path, &step.base).await? {
            return Ok(ctl);
        }

        for (index, item) in items.iter().enumerate() {
            frame
                .iters
                .push((step.r#as.as_str().to_owned(), item.clone()));
            let mut iter_prefix = step_path.clone();
            iter_prefix.push(PathFrame::Iteration {
                index: index as u64,
                key: None,
            });
            let ctl = self.exec_body(frame, iter_prefix, &step.body, 0).await?;
            frame.iters.pop();
            match ctl {
                Ctl::Continue => {}
                Ctl::HaltFail => {
                    // The failing iteration already blocked its own tail;
                    // later iterations never materialize as instances.
                    self.append(
                        &step_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Judged,
                            output: None,
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    return Ok(Ctl::HaltFail);
                }
                Ctl::Abort => {
                    self.append(
                        &step_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Aborted,
                            output: None,
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    return Ok(Ctl::Abort);
                }
                other => return Ok(other),
            }
        }
        self.append(
            &step_path,
            &RunLogPayload::StepExited {
                provider_state_summary: None,
                state: StepState::Judged,
                output: None,
                localized: Vec::new(),
                localization_gaps: Vec::new(),
            },
        )?;
        Ok(Ctl::Continue)
    }

    // ─── let steps ──────────────────────────────────────────────────────────

    async fn exec_let(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: RunPath,
        step: &'a LetStepIR,
    ) -> Result<Ctl, RunnerError> {
        let step_id = step.base.step_id.clone();
        let key = instance_key(&step_path);
        let evaluated = match self.open_span_inputs(&key) {
            // The archived snapshot *is* the bindings product (pure,
            // deterministic) — never re-evaluated on resume.
            Some(Value::Object(map)) => map,
            Some(_) | None => {
                let scope = frame.scope(&self.env, None);
                let mut evaluated = Map::new();
                let mut error = None;
                for (name, expr) in step.bindings.iter() {
                    // SSA single assignment: rebinding is refused by the
                    // compiler check phase; this is the runtime defense
                    // line against hand-built IR.
                    if frame.vars.contains_key(name.as_str()) {
                        error = Some(format!(
                            "binding '{name}' rebinds an existing var (SSA single assignment)"
                        ));
                        break;
                    }
                    match pointlock_expr::eval(expr, &scope) {
                        Ok(value) => {
                            evaluated.insert(name.as_str().to_owned(), value);
                        }
                        Err(eval_error) => {
                            error =
                                Some(format!("binding '{name}' evaluation failed: {eval_error}"));
                            break;
                        }
                    }
                }
                if let Some(message) = error {
                    self.enter_step(&step_path, &step.base, Value::Null)?;
                    return self
                        .settle_error(
                            frame,
                            &step_path,
                            &step_id,
                            VerdictStatus::Fail,
                            format!("let bindings failed: {message}"),
                        )
                        .await;
                }
                evaluated
            }
        };
        // The ready snapshot carries the evaluated bindings — the resume
        // walk re-seeds `vars.*` from exactly this carrier.
        self.enter_step(&step_path, &step.base, Value::Object(evaluated.clone()))?;
        if let Some(ctl) = self.probe_or_note(frame, &step_path, &step.base).await? {
            return Ok(ctl);
        }
        self.append(
            &step_path,
            &RunLogPayload::StepExited {
                provider_state_summary: None,
                state: StepState::Judged,
                output: None,
                localized: Vec::new(),
                localization_gaps: Vec::new(),
            },
        )?;
        for (name, value) in evaluated {
            frame.vars.insert(name, value);
        }
        Ok(Ctl::Continue)
    }

    // ─── assert steps ───────────────────────────────────────────────────────

    async fn exec_assert(
        &mut self,
        frame: &mut FrameState<'a>,
        step_path: RunPath,
        step: &'a AssertStepIR,
    ) -> Result<Ctl, RunnerError> {
        let step_id = step.base.step_id.clone();
        // An assert step resolves no input expressions; the span still
        // opens with an explicitly-null snapshot.
        self.enter_step(&step_path, &step.base, Value::Null)?;
        if let Some(ctl) = self.probe_or_note(frame, &step_path, &step.base).await? {
            return Ok(ctl);
        }
        let needs = VerifyNeeds::of(&step.assertions);
        let mut last_verdict_seq: Option<u64> = None;
        let mut seeded = false;
        let mut active_retry: Option<(RetryPolicy, u32)> = None;
        loop {
            let material = match &step.observe {
                // Fresh capture: one `session.observe`, localized through the
                // same observing pipeline as action observations.
                ObservationSource::Fresh(_) => {
                    let mut anchor = step_path.clone();
                    anchor.push(PathFrame::Phase {
                        phase: Phase::Observe,
                    });
                    self.fresh_material(&needs, &anchor).await?
                }
                // Archive reuse: the referenced action step's localized
                // observation material — zero device I/O, offline
                // re-judgeable by construction.
                ObservationSource::FromStep(from) => {
                    let which = from.which;
                    match frame.observed.get(from.from_step.as_str()) {
                        None => ObserveMaterial::absent(&format!(
                            "step '{}' has no archived observation in this frame",
                            from.from_step
                        )),
                        Some(observed) => {
                            let wanted = match which {
                                pointlock_ir::ObservationWhich::After => &observed.after_id,
                                pointlock_ir::ObservationWhich::Before => &observed.before_id,
                            };
                            match wanted {
                                None => ObserveMaterial::absent(&format!(
                                    "step '{}' recorded no {:?} observation",
                                    from.from_step, which
                                )),
                                Some(observation_id) => {
                                    match observed
                                        .observations
                                        .iter()
                                        .find(|record| record.observation_id == *observation_id)
                                    {
                                        None => ObserveMaterial::absent(
                                            "the referenced observation was never localized",
                                        ),
                                        Some(record) => {
                                            material_from_observation(self.store, record)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            };
            let scope = frame.scope(&self.env, None);
            let mut outcomes = Vec::with_capacity(step.assertions.len());
            let mut degraded_verify = false;
            for assertion in &step.assertions {
                let evaluated = match &assertion.predicate {
                    PredicateIR::Expr { expr } => EvaluatedAssertion {
                        record: eval_expr_assertion(assertion, expr, &scope),
                        degraded_verify: false,
                    },
                    _ => {
                        eval_observed_assertion(assertion, &material, self.vision.as_deref()).await
                    }
                };
                degraded_verify |= evaluated.degraded_verify;
                outcomes.push(evaluated.record);
            }
            for outcome in &outcomes {
                let mut path = step_path.clone();
                path.push(PathFrame::Phase {
                    phase: Phase::Assert,
                });
                path.push(PathFrame::Assertion {
                    assert_id: outcome.assert_id.clone(),
                });
                self.append(
                    &path,
                    &RunLogPayload::AssertionEvaluated {
                        outcome: outcome.clone(),
                    },
                )?;
            }
            // Assert steps declare ≥ 1 assertion, so a verdict always folds.
            let folded =
                fold_step_verdict(&outcomes, false, degraded_verify, frame.flow.verdict_policy);
            let supersedes = last_verdict_seq.map(|seq| format!("seq:{seq}"));
            let seq = self
                .record_step_verdict(
                    &step_path,
                    &folded,
                    Vec::new(),
                    supersedes,
                    EvidenceManifest::default(),
                )
                .await?;
            last_verdict_seq = Some(seq);
            if seeded {
                frame.reseed_last(&step_id, folded.status, folded.degraded);
            } else {
                frame.seed_verdict(&step_id, folded.status, folded.degraded);
                seeded = true;
            }
            if folded.status == VerdictStatus::Pass {
                self.append(
                    &step_path,
                    &RunLogPayload::StepExited {
                        provider_state_summary: None,
                        state: StepState::Judged,
                        output: None,
                        localized: Vec::new(),
                        localization_gaps: Vec::new(),
                    },
                )?;
                return Ok(Ctl::Continue);
            }

            // In-force handler retry budget (observe + assert re-entry is
            // readonly by construction — always replay-safe).
            if let Some((policy, used)) = active_retry.take()
                && used < policy.max_attempts
            {
                self.backoff_policy(&policy, used).await;
                active_retry = Some((policy, used + 1));
                continue;
            }

            let hook = if folded.status == VerdictStatus::Fail {
                HandlerHook::OnFail
            } else {
                HandlerHook::OnUnknown
            };
            match self
                .consult_hook(frame, &step_path, step.base.handlers.as_deref(), hook, None)
                .await?
            {
                Consulted::None | Consulted::RepairFailed => {
                    self.append(
                        &step_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Judged,
                            output: None,
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    return Ok(if folded.status == VerdictStatus::Fail {
                        Ctl::HaltFail
                    } else {
                        Ctl::Continue
                    });
                }
                Consulted::Continue => {
                    self.append(
                        &step_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Judged,
                            output: None,
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    return Ok(Ctl::Continue);
                }
                Consulted::Abort => {
                    self.append(
                        &step_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Aborted,
                            output: None,
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    return Ok(Ctl::Abort);
                }
                Consulted::Escalated {
                    status: ruled,
                    summary: ruled_summary,
                    evidence: ruling_evidence,
                } => {
                    let folded = FoldedVerdict {
                        status: ruled,
                        degraded: false,
                        summary: ruled_summary,
                    };
                    let supersedes = last_verdict_seq.map(|seq| format!("seq:{seq}"));
                    let (cited, manifest) = escalate_verdict_material(&ruling_evidence);
                    let ruled_seq = self
                        .record_step_verdict(&step_path, &folded, cited, supersedes, manifest)
                        .await?;
                    self.link_ruling_evidence(ruled_seq, &ruling_evidence)?;
                    frame.reseed_last(&step_id, ruled, false);
                    self.append(
                        &step_path,
                        &RunLogPayload::StepExited {
                            provider_state_summary: None,
                            state: StepState::Judged,
                            output: None,
                            localized: Vec::new(),
                            localization_gaps: Vec::new(),
                        },
                    )?;
                    return Ok(if ruled == VerdictStatus::Fail {
                        Ctl::HaltFail
                    } else {
                        Ctl::Continue
                    });
                }
                Consulted::Retry(policy) => {
                    self.backoff_policy(&policy, 0).await;
                    active_retry = Some((policy, 1));
                }
                Consulted::Repaired | Consulted::RepairDone => {}
                Consulted::Pending(pending) => {
                    return Ok(Ctl::AwaitHuman(pending));
                }
                Consulted::Propagate(ctl) => return Ok(ctl),
            }
            // Loop: re-observe and re-evaluate (fresh material each round;
            // fromStep archives are stable, so a re-round only makes sense
            // after a repair — both are honest re-judgements on new/declared
            // world state).
        }
    }

    // ─── observing / localization ───────────────────────────────────────────

    /// The observing phase: localize evidence (spine §6.6 — provider-side
    /// retention is not guaranteed) and record the before/after
    /// observations. Returns the provider asset refs cited by the verdict,
    /// the [`ObserveMaterial`] of the *after* observation, and the
    /// localized records (the `fromStep` archive of this step).
    async fn observing(
        &mut self,
        step: &'a ActionStepIR,
        step_path: &RunPath,
        attempt_n: u64,
        result: &ActionResult,
        settled_seq: u64,
    ) -> Result<(Vec<AssetRef>, ObserveMaterial, StepObs, EvidenceManifest), RunnerError> {
        let mut observe_path = step_path.clone();
        observe_path.push(PathFrame::Attempt { n: attempt_n });
        observe_path.push(PathFrame::Phase {
            phase: Phase::Observe,
        });
        let needs = VerifyNeeds::of(&step.assertions);
        let mut cited = Vec::new();
        let mut material =
            ObserveMaterial::absent("the action result carries no after observation");
        let mut observed = StepObs {
            observations: Vec::new(),
            before_id: None,
            after_id: None,
        };
        if let Some(observation) = &result.before {
            let record = self
                .localize_observation(observation, &mut cited, None)
                .await?;
            // file-before-row-before-log: the bytes are on disk and indexed
            // before this event references them.
            self.append(
                &observe_path,
                &RunLogPayload::ObservationRecorded {
                    observation: record.clone(),
                },
            )?;
            observed.before_id = Some(record.observation_id.clone());
            observed.observations.push(record);
        }
        if let Some(observation) = &result.after {
            material = ObserveMaterial::default();
            let record = self
                .localize_observation(observation, &mut cited, Some((&needs, &mut material)))
                .await?;
            self.append(
                &observe_path,
                &RunLogPayload::ObservationRecorded {
                    observation: record.clone(),
                },
            )?;
            observed.after_id = Some(record.observation_id.clone());
            observed.observations.push(record);
        }
        let mut manifest = EvidenceManifest::default();
        for asset in &result.evidence {
            // Bounded manifest (the cited-list cap's sibling): entries
            // beyond the cap are recorded as typed gaps — bounded DTOs
            // must not silently truncate (spine §10 bounded-render
            // discipline).
            if manifest.localized.len() >= VERDICT_EVIDENCE_MAX_ENTRIES {
                manifest.gaps.push(pointlock_ir::EvidenceGap {
                    asset: asset.clone(),
                    reason: format!(
                        "evidence cap exceeded ({VERDICT_EVIDENCE_MAX_ENTRIES} max per judgment)"
                    ),
                });
                continue;
            }
            // Auxiliary settlement evidence (item ③, 2026-07-18): a
            // success joins this judgment's localized manifest (and the
            // evidence_ref index); a failure is a TYPED gap on the
            // verdict record — never a silent omission (principle 4/R4).
            match self.try_localize(asset).await? {
                Ok((evidence, _bytes)) => {
                    self.store.link_evidence(
                        &self.run_id,
                        settled_seq,
                        &asset.id,
                        &evidence.sha256,
                    )?;
                    cited.push(asset.clone());
                    manifest.localized.push(evidence);
                }
                Err(reason) => {
                    manifest.gaps.push(pointlock_ir::EvidenceGap {
                        asset: asset.clone(),
                        reason,
                    });
                }
            }
        }
        Ok((cited, material, observed, manifest))
    }

    /// Localizes one observation's evidence and builds its durable record.
    /// Omissions are typed data and pass through verbatim; a localization
    /// failure leaves the affected field absent and feeds the dependent
    /// verify channel a typed gap — the run is never aborted over it (M2
    /// degradation rule; principle 4 routes it to `unknown`).
    async fn localize_observation(
        &mut self,
        observation: &Observation,
        cited: &mut Vec<AssetRef>,
        mut material: Option<(&VerifyNeeds, &mut ObserveMaterial)>,
    ) -> Result<ObservationRecord, RunnerError> {
        let screenshot = match &observation.screenshot {
            Some(asset) => match self.try_localize(asset).await? {
                Ok((evidence, bytes)) => {
                    cited.push(asset.clone());
                    if let Some((needs, material)) = material.as_mut()
                        && needs.vision
                    {
                        material.screenshot = Some((bytes, asset.media_type.clone()));
                    }
                    Some(evidence)
                }
                Err(gap) => {
                    if let Some((needs, material)) = material.as_mut()
                        && needs.vision
                    {
                        material.screenshot_gap = Some(gap);
                    }
                    None
                }
            },
            None => {
                if let Some((needs, material)) = material.as_mut()
                    && needs.vision
                {
                    material.screenshot_gap = Some(match observation.screenshot_omission {
                        Some(reason) => {
                            format!("screenshot omitted by the provider ({reason:?})")
                        }
                        None => "the observation carries no screenshot".to_owned(),
                    });
                }
                None
            }
        };
        let mut ui_snapshot_omission = observation.ui_snapshot_omission;
        let ui_snapshot = match &observation.ui_snapshot {
            Some(snapshot) => {
                let wants_tree = matches!(&material, Some((needs, _)) if needs.ui_tree);
                let localized = if wants_tree {
                    self.localize_ui_tree(
                        observation,
                        snapshot,
                        &mut material,
                        &mut ui_snapshot_omission,
                    )
                    .await?
                } else {
                    // No assertion consumes the tree: localize the
                    // provider-side evidence object as before (retention is
                    // not guaranteed), without the `ui.snapshot.get` pull.
                    // A failure is a gap, not an abort.
                    self.try_localize(&snapshot.evidence)
                        .await?
                        .ok()
                        .map(|(evidence, _bytes)| evidence)
                };
                // Cite only what actually landed (item ③ review fix): a
                // citation the local library cannot serve would be a
                // silent gallery omission. Citation granularity is the
                // ASSET (the pointer); the byte-exact truth (sha256 +
                // localPath of what was actually stored — the canonical
                // tree on the wants_tree path) lives on the record's
                // EvidenceRef, which is what consumers resolve.
                if localized.is_some() {
                    cited.push(snapshot.evidence.clone());
                }
                localized
            }
            None => {
                if let Some((needs, material)) = material.as_mut()
                    && needs.ui_tree
                {
                    material.ui_tree_gap = Some(match observation.ui_snapshot_omission {
                        Some(reason) => {
                            format!("uiSnapshot omitted by the provider ({reason:?})")
                        }
                        None => "the observation carries no uiSnapshot".to_owned(),
                    });
                }
                None
            }
        };
        // Finiteness guard: `scaleFactor` is the only f64 in the durable
        // record domain, and serde_json writes a non-finite f64 as `null`
        // — which round-trips into a permanent ledger read failure (every
        // refold/verify/projection of the run errors). A provider that
        // reports a non-finite viewport gets the field honestly absent
        // rather than a poisoned ledger (clamping would falsify evidence;
        // aborting the run over a cosmetic field would violate the M2
        // degradation rule).
        let viewport = observation
            .viewport
            .scale_factor
            .is_finite()
            .then(|| observation.viewport.clone());
        Ok(ObservationRecord {
            observation_id: observation.id.clone(),
            captured_at_ms: observation.captured_at_ms,
            viewport,
            screenshot,
            screenshot_omission: observation.screenshot_omission,
            ui_snapshot,
            ui_snapshot_omission,
        })
    }

    /// Pulls the observation's normalized UI tree (`ui.snapshot.get`),
    /// localizes the canonical bytes, and feeds the verify-chain material.
    /// A typed `Unavailable` — or a provider error on the dereference — is
    /// data, not a run abort: it becomes the uiTree channel's gap (unknown
    /// propagation), never a fabricated tree.
    async fn localize_ui_tree(
        &mut self,
        observation: &Observation,
        snapshot: &pointlock_ir::UiSnapshotRef,
        material: &mut Option<(&VerifyNeeds, &mut ObserveMaterial)>,
        ui_snapshot_omission: &mut Option<UiSnapshotOmissionReason>,
    ) -> Result<Option<EvidenceRef>, RunnerError> {
        match self.session.ui_snapshot(&observation.id).await {
            Ok(UiSnapshotOutcome::Available { snapshot: tree }) => {
                let bytes =
                    serde_json::to_vec(&tree).expect("a serde_json::Value always serializes");
                let put = self.store.put_evidence(&bytes, "application/json")?;
                if let Some((_, material)) = material.as_mut() {
                    material.ui_tree = Some(bytes);
                }
                Ok(Some(EvidenceRef {
                    asset: snapshot.evidence.clone(),
                    sha256: put.sha256,
                    local_path: put.local_path,
                }))
            }
            Ok(UiSnapshotOutcome::Unavailable { reason }) => {
                *ui_snapshot_omission = Some(reason);
                if let Some((_, material)) = material.as_mut() {
                    material.ui_tree_gap =
                        Some(format!("uiSnapshot dereference unavailable ({reason:?})"));
                }
                Ok(None)
            }
            Err(error) => {
                if let Some((_, material)) = material.as_mut() {
                    material.ui_tree_gap = Some(format!("uiSnapshot dereference failed: {error}"));
                }
                Ok(None)
            }
        }
    }

    /// Fetches an asset's bytes into the content-addressed evidence area.
    /// The outer `Result` is infrastructure (store I/O — still fatal); the
    /// inner one is the typed localization gap (fetch unsupported/ruptured,
    /// integrity mismatch — the run degrades, never aborts; 04 §4.3 is
    /// honored by *not using* mismatched bytes, with the reason on record).
    async fn try_localize(
        &mut self,
        asset: &AssetRef,
    ) -> Result<Result<(EvidenceRef, Vec<u8>), String>, RunnerError> {
        let mut stream = match self.session.fetch_evidence(asset).await {
            Ok(stream) => stream,
            Err(error) => {
                return Ok(Err(format!(
                    "evidence fetch failed for asset {}: {error}",
                    asset.id
                )));
            }
        };
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(part) => bytes.extend(part),
                Err(error) => {
                    return Ok(Err(format!(
                        "evidence stream failed for asset {}: {error}",
                        asset.id
                    )));
                }
            }
        }
        let put = self.store.put_evidence(&bytes, &asset.media_type)?;
        if let Some(expected) = &asset.sha256
            && expected != &put.sha256
        {
            return Ok(Err(format!(
                "evidence integrity failure for asset {}: sha256 {} != declared {expected}",
                asset.id, put.sha256
            )));
        }
        let evidence = EvidenceRef {
            asset: asset.clone(),
            sha256: put.sha256,
            local_path: put.local_path,
        };
        Ok(Ok((evidence, bytes)))
    }

    /// Appends `verdictRecorded` and writes the verdict back through the
    /// provider (`verdict.record` — the daemon only validates and
    /// persists). Returns the `verdictRecorded` seq (evidence linking).
    async fn record_step_verdict(
        &mut self,
        path: &RunPath,
        folded: &FoldedVerdict,
        cited: Vec<AssetRef>,
        supersedes: Option<String>,
        manifest: EvidenceManifest,
    ) -> Result<u64, RunnerError> {
        let verdict = Verdict {
            status: folded.status,
            degraded: folded.degraded,
            // The local ledger keeps the FULL summary — the 16384-char
            // cap is a wire hard limit, applied at write-back only
            // (04 §5).
            summary: folded.summary.clone(),
            evidence: cited
                .into_iter()
                .take(VERDICT_EVIDENCE_MAX_ENTRIES)
                .collect(),
            supersedes,
        };
        // Failure-instant capture (07 §2.2): the verdict instant IS the
        // failure instant, so the capture runs FIRST — before the
        // write-back RPC below can delay it on a degraded daemon. The
        // profile rides the span's exit through the `append` attach; a
        // superseding pass discards it. Deliberate cost: a retry round
        // that will be superseded still pays a capture (bounded by the
        // 2s budget) — skipping it would need the consult outcome, which
        // is only known after the handler runs, and a wrong skip would
        // ship a summary-less fail exit. Correctness over the bounded
        // RPC.
        let key = instance_key(path);
        match verdict.status {
            VerdictStatus::Fail | VerdictStatus::Unknown => {
                let summary = self.capture_summary().await;
                self.pending_summaries.insert(key, summary);
            }
            VerdictStatus::Pass => {
                self.pending_summaries.remove(&key);
            }
        }
        // Remote archival before the append so its outcome can ride the
        // event; it is archival of an already-derived verdict, not a
        // world effect, so the actionIntent WAL discipline does not
        // apply. A failure never changes the local verdict and never
        // aborts the run — it is annotated here and surfaced by the
        // report (04 §5). Deliberate cost: on a hung daemon the
        // `verdictRecorded` `at_ms` trails the fold instant by the
        // bounded write-back budget — the price of carrying the outcome
        // on the event under the closed §6.1 vocabulary.
        let remote_archival_error = self.try_verdict_writeback(&verdict).await;
        let seq = self.append(
            path,
            &RunLogPayload::VerdictRecorded {
                verdict: verdict.clone(),
                localized: manifest.localized,
                localization_gaps: manifest.gaps,
                remote_archival_error,
            },
        )?;
        Ok(seq)
    }

    /// `ProviderSession::record_verdict` write-back with the wire caps
    /// applied on the runner side (compaction is the runner's job,
    /// 04 §5). Returns the failure rendered for the ledger annotation —
    /// never an error: remote archival failure must not change the local
    /// verdict or abort the run (04 §5, the RunLog is the sole truth).
    async fn try_verdict_writeback(&mut self, verdict: &Verdict) -> Option<String> {
        self.session
            .record_verdict(VerdictWrite {
                status: verdict.status,
                summary: cap_wire_summary(verdict),
                evidence: verdict
                    .evidence
                    .iter()
                    .take(VERDICT_EVIDENCE_MAX_ENTRIES)
                    .cloned()
                    .collect(),
            })
            .await
            .err()
            .map(|error| format!("remote archival failed: {error}"))
    }

    /// Best-effort session teardown (04 §2.1: `end` must not block the
    /// runner's teardown when the transport is already gone).
    async fn end_session(&mut self, outcome: SessionOutcome) {
        let _ = self.session.end(outcome, None).await;
    }
}

/// The inbound gate of a call step (07 §1.1): apply the callee's declared
/// param defaults, refuse undeclared inputs and missing required params,
/// and validate every present value against its `ParamDecl.schema`.
fn call_inputs_gate(
    callee: &FlowIR,
    inputs: Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    for key in inputs.keys() {
        if !callee
            .params
            .iter()
            .any(|decl: &ParamDecl| decl.name.as_str() == key)
        {
            return Err(format!(
                "input '{key}' is not a declared param of callee '{}'",
                callee.flow_id
            ));
        }
    }
    let gated =
        params_with_defaults(callee, Value::Object(inputs)).map_err(|error| error.to_string())?;
    for decl in &callee.params {
        if let Some(value) = gated.get(decl.name.as_str()) {
            jsonschema::validate(decl.schema.as_value(), value)
                .map_err(|error| format!("param '{}' failed its schema: {error}", decl.name))?;
        }
    }
    Ok(gated)
}

/// Applies declared param defaults over the supplied params/inputs;
/// missing required params without defaults are refused. Shared by the run
/// entry (run params) and the call step's inbound gate (07 §1.1).
pub(crate) fn params_with_defaults(
    flow: &FlowIR,
    params: Value,
) -> Result<Map<String, Value>, RunnerError> {
    let mut map = match params {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => {
            return Err(RunnerError::InvalidParams {
                reason: format!("params must be a JSON object or null, got {other}"),
            });
        }
    };
    for decl in &flow.params {
        if map.contains_key(decl.name.as_str()) {
            continue;
        }
        if let Some(default) = &decl.default {
            map.insert(decl.name.as_str().to_owned(), default.clone());
        } else if decl.required {
            return Err(RunnerError::InvalidParams {
                reason: format!(
                    "required param '{}' is missing and has no default",
                    decl.name
                ),
            });
        }
    }
    Ok(map)
}

/// Which observation channels a set of assertions' verify chains consume —
/// decides what the observing phase must localize into
/// [`ObserveMaterial`] and what a fresh observe must want.
pub(crate) struct VerifyNeeds {
    /// Some assertion's chain contains `uiTree`.
    pub ui_tree: bool,
    /// Some assertion's chain contains `vision`.
    pub vision: bool,
}

impl VerifyNeeds {
    /// Scans the assertions (expr predicates consume no channel).
    pub fn of(assertions: &[AssertionIR]) -> Self {
        let mut needs = VerifyNeeds {
            ui_tree: false,
            vision: false,
        };
        for assertion in assertions {
            if matches!(assertion.predicate, PredicateIR::Expr { .. }) {
                continue;
            }
            for channel in &assertion.verify_via {
                match channel {
                    VerifyChannel::UiTree => needs.ui_tree = true,
                    VerifyChannel::Vision => needs.vision = true,
                    VerifyChannel::Dom => {}
                }
            }
        }
        needs
    }
}

/// Truncates a verdict summary to the provider wire cap (char-aware),
/// appending a content-hash pointer to the local full verdict when it
/// cuts (04 §5: the RunLog keeps the complete summary; the wire copy
/// points back at it).
pub(crate) fn cap_wire_summary(verdict: &Verdict) -> String {
    if verdict.summary.chars().count() <= VERDICT_SUMMARY_MAX_CHARS {
        return verdict.summary.clone();
    }
    let pointer = format!(
        " …[truncated; full local verdict {}]",
        pointlock_ir::domain_hash(
            "pointlock-runner/1/local-verdict",
            &serde_json::to_value(verdict).expect("a Verdict always serializes"),
        )
    );
    let keep = VERDICT_SUMMARY_MAX_CHARS.saturating_sub(pointer.chars().count());
    let mut capped: String = verdict.summary.chars().take(keep).collect();
    capped.push_str(&pointer);
    capped
}

/// The backoff delay before retry number `tries + 1` (spine §3
/// `RetryPolicy.backoffMs`).
fn backoff_ms(policy: &pointlock_ir::RetryPolicy, tries: u32) -> u64 {
    match &policy.backoff_ms {
        pointlock_ir::BackoffMs::Fixed(number) => number.as_f64().unwrap_or(0.0) as u64,
        pointlock_ir::BackoffMs::Schedule(schedule) => {
            let initial = schedule.initial.as_f64().unwrap_or(0.0);
            let factor = schedule.factor.as_f64().unwrap_or(1.0);
            let max = schedule.max.as_f64().unwrap_or(f64::MAX);
            let exponent = tries.saturating_sub(1);
            (initial * factor.powi(exponent as i32)).min(max) as u64
        }
    }
}

/// Whether an in-attempt retry is allowed (spine §6.5 mount point 1,
/// closed): only `action_failed_retryable`, `target_stale`, and — for
/// idempotent steps — `action_timed_out`, and only when the policy lists
/// the class and the budget is not exhausted.
fn retry_allowed(step: &ActionStepIR, class: ErrorClass, tries: u32) -> bool {
    let Some(policy) = &step.base.retry else {
        return false;
    };
    if tries >= policy.max_attempts {
        return false;
    }
    if !policy.retry_on.contains(&class) {
        return false;
    }
    match class {
        ErrorClass::ActionFailedRetryable | ErrorClass::TargetStale => true,
        ErrorClass::ActionTimedOut => step.idempotent,
        _ => false,
    }
}

/// Maps a non-succeeded terminal onto the closed `ErrorClass` taxonomy
/// (spine §5). When the wire code spells a class verbatim it is adopted
/// (mirrors the store fold's best-effort rule); otherwise `failed` maps by
/// the daemon-declared `retryable` flag.
pub(crate) fn classify(outcome: &ActionOutcome) -> ErrorClass {
    match outcome {
        ActionOutcome::Succeeded { .. } => {
            unreachable!("classify is only called on non-succeeded terminals")
        }
        ActionOutcome::Failed { error } => code_spelled_class(&error.code).unwrap_or({
            if error.retryable {
                ErrorClass::ActionFailedRetryable
            } else {
                ErrorClass::ActionFailedFinal
            }
        }),
        ActionOutcome::TimedOut { .. } => ErrorClass::ActionTimedOut,
        ActionOutcome::Cancelled { .. } => ErrorClass::ActionCancelled,
    }
}

fn code_spelled_class(code: &str) -> Option<ErrorClass> {
    serde_json::from_value(Value::String(code.to_owned())).ok()
}

fn terminal_message(outcome: &ActionOutcome) -> String {
    let error: &ErrorInfo = match outcome {
        ActionOutcome::Failed { error }
        | ActionOutcome::Cancelled { error }
        | ActionOutcome::TimedOut { error } => error,
        ActionOutcome::Succeeded { .. } => {
            unreachable!("terminal_message is only called on non-succeeded terminals")
        }
    };
    format!("{} ({})", error.message, error.code)
}

/// Whether the provider-reported execution mode is inside the attempt's
/// whitelist (§6.4 R-degrade). An absent execution report cannot be
/// audited and is accepted (the DeviceRail adapter always reports it).
fn execution_accepted(attempt: &BoundAttempt, execution: &Option<ActionExecution>) -> bool {
    match execution {
        None => true,
        Some(execution) => {
            let mode = match execution {
                ActionExecution::NativeSemantic { .. } => ExecutionMode::NativeSemantic,
                ActionExecution::WebSemantic { .. } => ExecutionMode::WebSemantic,
                ActionExecution::CoordinateFallback { .. } => ExecutionMode::CoordinateFallback,
            };
            attempt.accept_execution_modes.contains(&mode)
        }
    }
}

/// Whether an action step is effectively mutating for the I2 replay gates
/// (mutating and not declared idempotent).
pub(crate) fn gated_mutating(step: &ActionStepIR) -> bool {
    step.effect == EffectClassAction::Mutating && !step.idempotent
}

/// Whether an uncertain reconcile branch may replay the step
/// (07 §4.4: `idempotent: true` or `effect: "readonly"`).
pub(crate) fn replay_permitted(step: &ActionStepIR) -> bool {
    step.effect == EffectClassAction::Readonly || step.idempotent
}

#[cfg(test)]
mod chain_start_tests {
    use super::*;

    fn two_attempt_step() -> ActionStepIR {
        serde_json::from_value(serde_json::json!({
            "kind": "action",
            "stepId": "s1",
            "effectHash": format!("sha256:{}", "0".repeat(64)),
            "judgeHash": format!("sha256:{}", "0".repeat(64)),
            "checkpoint": true,
            "effect": "mutating",
            "idempotent": true,
            "binding": { "attempts": [ {
                "channel": "uiTree",
                "actionName": "tapElement",
                "args": {},
                "acceptExecutionModes": ["nativeSemantic"],
                "protection": "standard"
            }, {
                "channel": "uiTree",
                "actionName": "setElementValue",
                "args": {},
                "acceptExecutionModes": ["nativeSemantic"],
                "protection": "standard"
            } ] },
            "assertions": []
        }))
        .expect("fixture step")
    }

    #[test]
    fn maps_recorded_positions_and_refuses_out_of_range() {
        let step = two_attempt_step();
        assert_eq!(chain_start(None, &step).expect("head"), 0);
        assert_eq!(chain_start(Some(1), &step).expect("first"), 0);
        assert_eq!(chain_start(Some(2), &step).expect("second"), 1);
        assert!(chain_start(Some(3), &step).is_err());
        assert!(chain_start(Some(0), &step).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_summary_truncation_appends_the_local_verdict_pointer() {
        let verdict = Verdict {
            status: VerdictStatus::Fail,
            degraded: false,
            summary: "x".repeat(VERDICT_SUMMARY_MAX_CHARS + 100),
            evidence: Vec::new(),
            supersedes: None,
        };
        let capped = cap_wire_summary(&verdict);
        // Exactly at the wire cap — the fake/devicerail providers reject
        // anything above it fail-closed.
        assert_eq!(capped.chars().count(), VERDICT_SUMMARY_MAX_CHARS);
        // The 04 §5 pointer to the full local verdict rides the tail.
        assert!(
            capped.ends_with(']') && capped.contains("full local verdict sha256:"),
            "pointer missing: …{}",
            &capped[capped.len().saturating_sub(90)..]
        );
        assert!(capped.starts_with("xxx"));

        // Negative control: an in-cap summary passes through verbatim,
        // pointer-free.
        let short = Verdict {
            summary: "all assertions passed".to_owned(),
            ..verdict
        };
        assert_eq!(cap_wire_summary(&short), "all assertions passed");
    }
}
