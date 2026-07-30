//! The public entry points: [`Runner::run`] and [`Runner::resume`] /
//! [`Runner::resume_with_subflows`] (spine §6; 07 §4–§5).
//!
//! Since M2 the entry points accept the resolved subflow registry
//! (`Map<irHash, FlowIR>`, provided by the assembly layer; every entry
//! self-verifies at load). Resume comes in two regimes:
//!
//! - **Same-IR resume** (the common suspend/crash continuation): every
//!   completed step *instance* is adopted by its exact run path — the
//!   walk falls back into open call frames and foreach iterations without
//!   restarting any frame (07 §4.6), with the archived control snapshots
//!   (cond / items / inputs) never re-evaluated (I3).
//! - **Cross-IR resume** (repair): the 07 §5.2 *flat* subset — alignment,
//!   offline re-judge and the `requiresConfirmation` gates over top-level
//!   action steps. The nested rules (call down-drill, per-iteration
//!   alignment, order-consistency) land with the repair wave; the
//!   combination is refused with a typed error, never silently guessed.

use std::collections::{BTreeMap, BTreeSet};

use pointlock_ir::{
    ActionStepIR, AlignmentClass, AlignmentEntry, AlignmentReport, BindingState, CheckpointView,
    FlowIR, Hash, PathFrame, ReconcileResult, RequiresConfirmation, RunLogPayload, RunPath,
    SupervisePolicy, ir_hash,
};
use pointlock_provider_kit::{CancellationToken, ProviderSession, SessionOutcome};
use pointlock_store::{NewRun, Store};
use serde_json::{Map, Value};

use crate::align::{Alignment, Harvest, align, harvest, live_frame_pins};
use crate::engine::{
    Adopted, Execution, FrameState, FrontierWork, HumanRequestFact, RunOutcome, gated_mutating,
    instance_key, is_history, now_ms, params_with_defaults, replay_permitted, root_path,
};
use crate::error::{BlockedReason, RunnerError};
use crate::load::{LoadedFlow, check_attestation, load};
use crate::scope::ScopeSeed;

/// Options of [`Runner::run`].
pub struct RunOptions {
    /// Cooperative stop token, honored at step boundaries
    /// (`runSuspended` → [`RunOutcome::Suspended`]).
    pub stop: CancellationToken,
    /// Explicit run id; a UUIDv4 is generated when absent.
    pub run_id: Option<String>,
    /// The bound device (checkpoint hard binding; also `env.deviceId`).
    pub device_id: String,
    /// The device platform for `env.platform`, when known (comes from the
    /// lockfile at the assembly layer; the SPI attestation does not carry
    /// it).
    pub platform: Option<String>,
    /// The vision verifier consulted by `vision` verify-chain tails.
    /// `None` is equivalent to
    /// [`pointlock_vision::StubVisionVerifier`]: the vision channel cannot
    /// complete and reports `"vision verifier not configured"` — the chain
    /// degrades honestly toward `unknown` (principle 4).
    pub vision: Option<std::sync::Arc<dyn pointlock_vision::VisionVerifier>>,
    /// The resolved subflow registry keyed by `irHash` (07 §1.3): every
    /// callee the flow's `subflows` table pins must be present; entries
    /// self-verify at load. Empty for flows without subflows.
    pub subflows: BTreeMap<Hash, FlowIR>,
    /// This segment's supervision policy (R13, spine §6.9): recorded in
    /// `runStarted.supervisePolicy` (explicitly `null` when absent) and
    /// gates action-step dispatch (`mutating` gates mutating steps,
    /// `all` every action step). Per segment, never inherited.
    pub supervise: Option<SupervisePolicy>,
    /// Injectable wall clock for human-deadline computation and lazy
    /// timeout settlement (tests); `None` uses the system clock.
    pub clock: Option<std::sync::Arc<dyn Fn() -> u64 + Send + Sync>>,
}

impl RunOptions {
    /// Options with a fresh stop token, no explicit run id, no vision
    /// verifier (stub-equivalent), an empty subflow registry, no
    /// supervision and the system clock.
    pub fn new(device_id: impl Into<String>) -> Self {
        RunOptions {
            stop: CancellationToken::new(),
            run_id: None,
            device_id: device_id.into(),
            platform: None,
            vision: None,
            subflows: BTreeMap::new(),
            supervise: None,
            clock: None,
        }
    }
}

/// Options of [`Runner::resume`].
#[derive(Default)]
pub struct ResumeOptions {
    /// Cooperative stop token (see [`RunOptions::stop`]).
    pub stop: CancellationToken,
    /// `env.platform`, when known.
    pub platform: Option<String>,
    /// The FlowIR the run originally executed — optional, and worth
    /// supplying. Alignment reads the archived execution-time per-step
    /// hashes from the checkpoint's `StepRecord`s (harvested from
    /// `stepEntered`, spine §6.1 M1 note), so cross-IR resume works
    /// without it; supplying it additionally unlocks the preflight-only
    /// sub-domain comparison (07 §5.3 / 02 §12.3 ruling 6) — a
    /// `judgeDirty` step whose only change is `preflight` adopts its
    /// archived verdict outright instead of re-judging or re-executing.
    /// Verified against the checkpoint's `irHash`
    /// ([`RunnerError::OldIrMismatch`]); a mismatch is a caller error,
    /// surfaced not ignored.
    pub old_flow_ir: Option<FlowIR>,
    /// This segment's supervision policy (R13, spine §6.9): recorded in
    /// `runResumed.supervisePolicy` (explicitly `null` when absent).
    /// Per segment, never inherited — an unset value means this segment
    /// runs unsupervised regardless of previous segments; a supervision
    /// request already pending still settles by its arbitrated response.
    pub supervise: Option<SupervisePolicy>,
    /// The vision verifier of this segment (see [`RunOptions::vision`]);
    /// `None` is stub-equivalent — vision tails degrade to `unknown`.
    pub vision: Option<std::sync::Arc<dyn pointlock_vision::VisionVerifier>>,
    /// Step ids the author FORCES back to execution this segment
    /// (07 §5.3, the CLI's repeatable `--force-reexecute <stepId>`):
    /// each named step classifies `effectDirty` regardless of its hashes,
    /// so the resume point rolls back to the earliest of them and they
    /// re-run against the live world.
    ///
    /// The escape hatch for a re-judge the author rejects — an offline
    /// re-judge that can only reach `unknown` because the archive lacks
    /// the observation channel the new assertion needs (「缺料 →
    /// unknown」), or an adopted result the author no longer trusts. It
    /// upgrades the CLASSIFICATION only: a forced step that is mutating
    /// and already effective still walks the 07 §5.4 gate and needs
    /// `--allow-mutating-reexec` besides — forcing says "run it again",
    /// authorizing says "yes, even though the world holds its effect".
    /// Like the authorization list it covers this resume only, and it is
    /// cross-IR vocabulary: a same-IR resume has no classification to
    /// upgrade.
    pub force_reexecute: Vec<String>,
    /// Step ids the author explicitly authorized for mutating
    /// re-execution this segment (07 §5.4 step 2, the CLI's repeatable
    /// `--allow-mutating-reexec <stepId>`).
    ///
    /// Each id releases exactly one `requiresConfirmation` entry; there is
    /// no wildcard, and the authorization covers **this resume only** —
    /// nothing about it is persisted, so the next resume re-gates from
    /// scratch. An id naming no gated step is refused rather than ignored:
    /// silently accepting it would let an author believe they had cleared
    /// something they had not.
    ///
    /// Releasing the gate does not skip the world check: the step still
    /// enters `probing` and evaluates its `preflight` (§5.4 step 3), which
    /// is what meets the residue of the earlier effect.
    pub allow_mutating_reexec: Vec<String>,
    /// Injectable wall clock (see [`RunOptions::clock`]).
    pub clock: Option<std::sync::Arc<dyn Fn() -> u64 + Send + Sync>>,
}

/// The runner: executes a sealed [`FlowIR`] against an open provider
/// session, journaling every transition into the single-writer store.
/// Entry signatures accept only `FlowIR`, never strings (principles 1/2).
pub struct Runner;

impl Runner {
    /// Runs a flow from the beginning (spine §6.2 pipeline; §6.1 event
    /// vocabulary). This segment's `supervisePolicy` is recorded verbatim
    /// in `runStarted` — explicitly `null` when unsupervised (R13,
    /// per-segment self-describing ledger).
    pub async fn run(
        flow: &FlowIR,
        params: Value,
        session: Box<dyn ProviderSession>,
        store: &mut Store,
        opts: RunOptions,
    ) -> Result<RunOutcome, RunnerError> {
        let RunOptions {
            stop,
            run_id,
            device_id,
            platform,
            vision,
            subflows,
            supervise,
            clock,
        } = opts;
        let loaded = load(flow, &subflows)?;
        check_attestation(&loaded, session.attestation())?;
        let params = params_with_defaults(flow, params)?;

        let cursor = session.current_cursor().await?;
        let initial_lineage = vec![cursor.session_id.clone()];
        let run_id = store.begin_run(NewRun {
            run_id,
            flow_id: flow.flow_id.clone(),
            ir_hash: flow.ir_hash.clone(),
            lockfile_digest: flow.lockfile_digest.clone(),
            params_snapshot: Value::Object(params.clone()),
            binding: BindingState {
                device_id: device_id.clone(),
                session_lineage: vec![cursor.session_id.clone()],
                event_cursor: cursor,
            },
            created_at_ms: now_ms(),
        })?;
        store.append_event(
            &run_id,
            now_ms(),
            &root_path(flow),
            &RunLogPayload::RunStarted {
                ir_hash: flow.ir_hash.clone(),
                lockfile_digest: flow.lockfile_digest.clone(),
                params_snapshot: Value::Object(params.clone()),
                // R13: this segment's real policy — explicitly null when
                // unsupervised (per-segment, self-describing).
                supervise_policy: supervise,
            },
        )?;

        let env = env_bindings(&device_id, platform.as_deref(), &run_id);
        let exec = Execution {
            flows: &loaded,
            session,
            store,
            run_id,
            stop,
            env,
            session_lineage: initial_lineage,
            pending_summaries: Default::default(),
            attempt_base: Default::default(),
            open_spans: Default::default(),
            live_frames: Default::default(),
            // A fresh run never re-touches a world it stopped watching.
            resumed: false,
            authorized: BTreeSet::new(),
            reentry_seen: false,
            adoptable: Default::default(),
            frontier: None,
            vision,
            supervise,
            human: Default::default(),
            settled: Default::default(),
            recorded_verdicts: Default::default(),
            hook_triggers: Default::default(),
            clock,
        };
        let root = FrameState::new(flow, root_path(flow), params, 1);
        exec.run(root, 0).await
    }

    /// Resumes a run (07 §4) without subflows. Legality ⟺ (A) every
    /// completed record is still recognized under the (possibly repaired)
    /// new IR — recorded as `alignmentReport` in `runResumed`; (B) a
    /// pending intent on the frontier has been reconciled
    /// (`ProviderSession::reconcile`); (C) the world passes the resume
    /// probes — the first to-execute step's declared `preflight` runs
    /// before its act; a step without probes resumes honestly `unprobed`
    /// (I3).
    pub async fn resume(
        new_flow: &FlowIR,
        run_id: &str,
        session: Box<dyn ProviderSession>,
        store: &mut Store,
        opts: ResumeOptions,
    ) -> Result<RunOutcome, RunnerError> {
        let subflows = BTreeMap::new();
        Self::resume_with_subflows(new_flow, &subflows, run_id, session, store, opts).await
    }

    /// [`Runner::resume`] with a resolved subflow registry — required when
    /// the (new) flow pins callees; see [`RunOptions::subflows`].
    pub async fn resume_with_subflows(
        new_flow: &FlowIR,
        subflows: &BTreeMap<Hash, FlowIR>,
        run_id: &str,
        session: Box<dyn ProviderSession>,
        store: &mut Store,
        opts: ResumeOptions,
    ) -> Result<RunOutcome, RunnerError> {
        let loaded = load(new_flow, subflows)?;
        check_attestation(&loaded, session.attestation())?;
        let view = store.rebuild_checkpoint(run_id)?;
        let events = store.events(run_id)?;
        let facts = harvest(&events);

        // The optional old-IR integrity check: when the caller supplies
        // one, verify it is the IR the run executed (a mismatched old IR
        // is a caller error, surfaced not ignored).
        if let Some(old) = opts.old_flow_ir.as_ref() {
            let computed = ir_hash(old);
            if computed != view.ir_hash {
                return Err(RunnerError::OldIrMismatch {
                    expected: view.ir_hash.clone(),
                    computed,
                });
            }
        }

        if view.ir_hash == new_flow.ir_hash {
            resume_same_ir(loaded, view, facts, run_id, session, store, opts).await
        } else {
            resume_cross_ir(loaded, view, facts, run_id, session, store, opts).await
        }
    }

    /// The READ-ONLY alignment preview (08 §2.7): the resume path's
    /// classification verbatim — same-IR trivial adoption or the flat
    /// cross-IR `align` — but no session, no attestation, no writes, no
    /// commitment. The preview is not a promise: the world can drift
    /// between preview and resume; the resume-time preflight probes stay
    /// the final judge. A confirmation-gated alignment is a preview
    /// RESULT here (the report shows what the real resume would refuse),
    /// not an error.
    #[allow(clippy::too_many_arguments)]
    pub async fn align_preview(
        new_flow: &FlowIR,
        subflows: &BTreeMap<Hash, FlowIR>,
        run_id: &str,
        store: &Store,
        platform: Option<&str>,
        vision: Option<&dyn pointlock_vision::VisionVerifier>,
        forced: &[String],
        old_flow_ir: Option<&FlowIR>,
    ) -> Result<AlignmentReport, RunnerError> {
        let loaded = load(new_flow, subflows)?;
        let view = store.rebuild_checkpoint(run_id)?;
        let events = store.events(run_id)?;
        let facts = harvest(&events);

        // The same old-IR integrity check the real resume applies: a
        // mismatched old IR is a caller error, and rehearsing with it
        // would classify against the wrong sub-domains.
        if let Some(old) = old_flow_ir {
            let computed = ir_hash(old);
            if computed != view.ir_hash {
                return Err(RunnerError::OldIrMismatch {
                    expected: view.ir_hash.clone(),
                    computed,
                });
            }
        }

        // The preview mirrors resume's typed refusals — a clean rehearsal
        // of a resume the runner would categorically refuse is a lie.
        if let Some(live_hook) = facts
            .live_frames
            .iter()
            .find(|path| path.iter().any(|f| matches!(f, PathFrame::Hook { .. })))
        {
            return Err(RunnerError::M0Unsupported {
                detail: format!(
                    "resume across a live handler-repair frame ({}) is not in the M2 subset — \
                     the repair flow suspended mid-flight; hook-aware frame re-entry is \
                     registered for the repair wave",
                    pointlock_ir::render_run_path(live_hook)
                ),
            });
        }

        if view.ir_hash == new_flow.ir_hash {
            return Ok(same_ir_report(new_flow, &view));
        }

        if !is_alignable_path(&view.frontier.run_path) {
            return Err(RunnerError::M0Unsupported {
                detail: "the run's frontier sits inside a handler frame".to_owned(),
            });
        }
        // Mirrors resume_cross_ir's third hook state (an escalate human
        // still awaiting an answer): rehearsing a resume the runner would
        // categorically refuse is a lie.
        if view
            .human_pending
            .as_ref()
            .is_some_and(|pending| !is_alignable_path(&pending.run_path))
        {
            return Err(RunnerError::M0Unsupported {
                detail: "a handler escalation is still awaiting an answer".to_owned(),
            });
        }

        // `env.platform` comes from the caller (the serve endpoint reads
        // it from the SAME lockfile the resume assembly uses); when
        // absent, an expr predicate referencing it re-judges to unknown
        // in the preview (fail-closed) while the real resume would judge
        // it — pass the platform to keep the rehearsal faithful.
        let seed = ScopeSeed::new(
            params_object(&view),
            &view.binding.device_id,
            platform,
            run_id,
        );
        match align(
            &loaded,
            &new_flow.body,
            &view,
            &facts,
            &seed,
            store,
            vision,
            // A preview shows what WOULD gate: it authorizes nothing. The
            // FORCED list and the old IR it does take — the rehearsal must
            // classify exactly as the real resume will.
            &[],
            forced,
            old_flow_ir,
        )
        .await
        {
            Ok(alignment) => Ok(alignment.report),
            Err(RunnerError::RequiresConfirmation { report }) => Ok(*report),
            Err(other) => Err(other),
        }
    }
}

/// `env.*` bindings: `deviceId`, `runId`, and `platform` when known (the
/// platform comes from the assembly layer — the SPI attestation does not
/// carry it). Run-constant, read-only pass-through across frames (07 §1.2).
fn env_bindings(device_id: &str, platform: Option<&str>, run_id: &str) -> Vec<(String, Value)> {
    let mut env = vec![
        ("deviceId".to_owned(), Value::String(device_id.to_owned())),
        ("runId".to_owned(), Value::String(run_id.to_owned())),
    ];
    if let Some(platform) = platform {
        env.push(("platform".to_owned(), Value::String(platform.to_owned())));
    }
    env
}

/// The same-IR alignment report: top-level instances with execution
/// history are trivially reusable (identical hashes by construction);
/// the rest re-execute as `new`. Shared by [`resume_same_ir`] and the
/// read-only [`Runner::align_preview`].
fn same_ir_report(new_flow: &FlowIR, view: &CheckpointView) -> AlignmentReport {
    let completed: BTreeMap<String, &pointlock_ir::StepRecord> = view
        .completed
        .iter()
        .map(|record| (instance_key(&record.run_path), record))
        .collect();
    let mut entries = Vec::new();
    for step in &new_flow.body {
        let mut path = root_path(new_flow);
        path.push(match step {
            pointlock_ir::StepIR::Call(call) => PathFrame::Call {
                step_id: Some(call.base.step_id.clone()),
                callee_flow_id: call.flow_ref.flow_id.clone(),
                callee_ir_hash: call.flow_ref.ir_hash.clone(),
            },
            other => PathFrame::Step {
                step_id: other.step_id().clone(),
            },
        });
        let key = instance_key(&path);
        let adopted = completed.get(&key).is_some_and(|record| is_history(record));
        entries.push(AlignmentEntry {
            run_path: path.clone(),
            step_id: step.step_id().clone(),
            class: if adopted {
                AlignmentClass::Reusable
            } else {
                AlignmentClass::New
            },
            reason: (!adopted).then(|| "no adoptable prior record".to_owned()),
        });
    }
    AlignmentReport {
        entries,
        resume_point: Some(view.frontier.run_path.clone()),
        requires_confirmation: Vec::new(),
    }
}

// ─── same-IR resume: frame-precise adoption (07 §4.6) ───────────────────────

/// Resumes under the identical IR: every completed step instance is
/// adopted by its exact run path; open spans and live call frames are
/// re-entered without re-appending their events; the walk lands on the
/// frontier position inside any depth of nesting — no frame restarts, no
/// snapshot re-evaluation.
async fn resume_same_ir(
    loaded: LoadedFlow<'_>,
    view: CheckpointView,
    facts: Harvest,
    run_id: &str,
    mut session: Box<dyn ProviderSession>,
    store: &mut Store,
    opts: ResumeOptions,
) -> Result<RunOutcome, RunnerError> {
    let new_flow = loaded.root;
    // The bind-time binding cursor (run-row meta, written once at
    // begin_run, never rewritten): the issuing credential of intents
    // dispatched before any resume (07 §4.5).
    let bind_cursor = store.run_meta(run_id)?.binding.event_cursor;
    // Adoption set: completed instances keyed by their instance path.
    let mut adoptable: BTreeMap<String, Adopted> = BTreeMap::new();
    for record in &view.completed {
        let key = instance_key(&record.run_path);
        adoptable.insert(
            key.clone(),
            Adopted {
                record: record.clone(),
                before_id: facts.before_observation.get(&key).cloned(),
                after_id: facts.after_observation.get(&key).cloned(),
            },
        );
    }
    let open_spans: BTreeMap<String, Value> = facts
        .open_spans
        .iter()
        .map(|path| {
            let key = instance_key(path);
            let inputs = facts
                .entered_inputs
                .get(&key)
                .cloned()
                .unwrap_or(Value::Null);
            (key, inputs)
        })
        .collect();
    // A live hook-launched repair frame (a suspension *inside* a repair
    // subflow) needs hook-aware frame re-entry — a typed M2 refusal, never
    // a guess (the repair's own records stay archived and honest).
    if let Some(live_hook) = facts
        .live_frames
        .iter()
        .find(|path| path.iter().any(|f| matches!(f, PathFrame::Hook { .. })))
    {
        return Err(RunnerError::M0Unsupported {
            detail: format!(
                "resume across a live handler-repair frame ({}) is not in the M2 subset — \
                 the repair flow suspended mid-flight; hook-aware frame re-entry is \
                 registered for the repair wave",
                pointlock_ir::render_run_path(live_hook)
            ),
        });
    }
    let live_frames = live_frame_pins(&facts);

    // The alignment report of a same-IR resume (shared with the
    // read-only preview — one classification truth source).
    let mut report = same_ir_report(new_flow, &view);

    // (B) unconditional reconcile of a pending intent (07 §4.1/§4.4). The
    // frontier step is where the walk will land (everything before it is
    // adopted), so `at_resume` holds by construction.
    let mut frontier_work = None;
    let mut deferred_settle = None;
    let mut pending_adjudication: Option<Box<Adjudication>> = None;
    let mut blocked = None;
    if let Some(intent) = &view.frontier.pending_intent {
        let frontier_key = instance_key(&view.frontier.run_path);
        let new_step = loaded.resolve_action(&view.frontier.run_path);
        // Same-IR: the archived entered hash matches the resolved step's
        // by construction; a missing carrier fails closed (dirty).
        let effect_dirty = match (new_step, facts.entered_effect_hash.get(&frontier_key)) {
            (Some(step), Some(archived)) => *archived != step.base.effect_hash,
            _ => true,
        };
        let decision = match reconcile_frontier(
            &mut session,
            new_step,
            true,
            effect_dirty,
            &view,
            &facts,
            &bind_cursor,
            &mut report,
            intent,
        )
        .await
        {
            Ok(decision) => decision,
            Err(error) => {
                let _ = session.end(SessionOutcome::Shutdown, None).await;
                return Err(error);
            }
        };
        match decision {
            FrontierDecision::Work(work) => frontier_work = Some((frontier_key, work)),
            FrontierDecision::DeferredSettle(settle) => deferred_settle = Some(settle),
            FrontierDecision::Adjudicate(adjudication) => pending_adjudication = Some(adjudication),
            FrontierDecision::Blocked(reason) => blocked = Some(reason),
            FrontierDecision::Nothing => {}
        }
    }

    // The segment header: runResumed carries the alignment report, this
    // segment's supervisePolicy (explicitly null when unsupervised —
    // R13), and the new generation's reseeded cursor (07 §4.5: taken
    // after the reconcile decisions, before this append; absent when the
    // RPC fails — honest, never stale).
    let resumed_cursor = session.current_cursor().await.ok();
    store.append_event(
        run_id,
        now_ms(),
        &root_path(new_flow),
        &RunLogPayload::RunResumed {
            alignment_report: report.clone(),
            supervise_policy: opts.supervise,
            event_cursor: resumed_cursor,
        },
    )?;

    // A reconciled completed terminal that cannot be adopted at the
    // resume point is still recorded — the ledger closes the intent and
    // keeps the world fact as evidence (07 §4.1).
    if let Some((path, call_id, outcome)) = deferred_settle {
        store.append_event(
            run_id,
            now_ms(),
            &path,
            &RunLogPayload::ActionSettled {
                call_id,
                outcome: crate::engine::quarantine_unpersistable(*outcome),
            },
        )?;
    }

    if let Some(adjudication) = pending_adjudication {
        // Phase 1 of the 07 §4.4 default escalation: the request (fresh or
        // re-awaited) is the segment's outcome — the run suspends
        // `awaitingHuman` and the answer arrives through the ordinary
        // arbitration channel, durable for the next resume to consume.
        let Adjudication {
            run_path,
            request,
            pending,
        } = *adjudication;
        if let Some((request_id, prompt, presents)) = request {
            store.append_event(
                run_id,
                now_ms(),
                &run_path,
                &RunLogPayload::HumanRequested {
                    request_id,
                    purpose: pointlock_ir::HumanPurpose::Step,
                    mode: Some(pointlock_ir::vocab::HumanMode::RepairWorld),
                    prompt,
                    presents,
                    decisions: Some(vec![
                        "adopt".to_owned(),
                        "redo".to_owned(),
                        "abort".to_owned(),
                    ]),
                    output_schema: None,
                    deadline_at_ms: None,
                },
            )?;
        }
        let summary = crate::engine::capture_provider_state_summary(
            session.as_ref(),
            &view.binding.session_lineage,
            &view.binding.device_id,
            opts.platform.as_deref(),
        )
        .await;
        store.append_event(
            run_id,
            now_ms(),
            &root_path(new_flow),
            &RunLogPayload::RunSuspended {
                provider_state_summary: Some(summary),
                reason: Some(format!(
                    "awaiting human adjudication (requestId {})",
                    pending.request_id
                )),
            },
        )?;
        let _ = session.end(SessionOutcome::Shutdown, None).await;
        return Ok(RunOutcome::AwaitingHuman { pending });
    }

    if let Some(reason) = blocked {
        // Suspension-instant profile (07 §2.2): the session is still
        // live at this pre-Execution blocked refusal.
        let summary = crate::engine::capture_provider_state_summary(
            session.as_ref(),
            &view.binding.session_lineage,
            &view.binding.device_id,
            opts.platform.as_deref(),
        )
        .await;
        store.append_event(
            run_id,
            now_ms(),
            &root_path(new_flow),
            &RunLogPayload::RunSuspended {
                provider_state_summary: Some(summary),
                reason: Some(reason.to_string()),
            },
        )?;
        let _ = session.end(SessionOutcome::Shutdown, None).await;
        return Ok(RunOutcome::Blocked { reason });
    }

    let params = params_object(&view);
    let env = env_bindings(&view.binding.device_id, opts.platform.as_deref(), run_id);
    let exec = Execution {
        flows: &loaded,
        session,
        store,
        run_id: run_id.to_owned(),
        stop: opts.stop,
        env,
        attempt_base: facts.max_attempt.clone(),
        open_spans,
        live_frames,
        resumed: true,
        authorized: opts.allow_mutating_reexec.iter().cloned().collect(),
        reentry_seen: false,
        adoptable,
        frontier: frontier_work,
        session_lineage: view.binding.session_lineage.clone(),
        pending_summaries: BTreeMap::new(),
        vision: opts.vision.clone(),
        supervise: opts.supervise,
        human: facts.human_requests.clone(),
        settled: facts.settled.clone(),
        recorded_verdicts: facts.recorded_verdicts.clone(),
        hook_triggers: facts.hook_triggers.clone(),
        clock: opts.clock,
    };
    let root = FrameState::new(new_flow, root_path(new_flow), params, 1);
    exec.run(root, 0).await
}

// ─── cross-IR resume: the flat alignment subset (07 §5.2) ────────────────────

/// Resumes under a repaired IR. M2 subset: the old records and the new
/// body must both be flat top-level action steps; anything nested is a
/// typed refusal (the 07 §5.2 nested alignment rules land with the repair
/// wave).
async fn resume_cross_ir(
    loaded: LoadedFlow<'_>,
    view: CheckpointView,
    facts: Harvest,
    run_id: &str,
    mut session: Box<dyn ProviderSession>,
    store: &mut Store,
    opts: ResumeOptions,
) -> Result<RunOutcome, RunnerError> {
    let new_flow = loaded.root;
    // Bind-time credential, as in resume_same_ir (07 §4.5).
    let bind_cursor = store.run_meta(run_id)?.binding.event_cursor;
    // The FRONTIER may not sit inside a handler frame: resolving it means
    // walking a path `resolve_step` refuses by construction, and the
    // reconcile below would then have no step to reconcile against.
    // Completed hook-framed records are a different matter — see
    // [`is_alignable_path`].
    if !is_alignable_path(&view.frontier.run_path) {
        let _ = session.end(SessionOutcome::Shutdown, None).await;
        return Err(RunnerError::M0Unsupported {
            detail: format!(
                "the run's frontier sits inside a handler frame ({}); hook-aware frame \
                 re-entry is registered for the repair wave",
                pointlock_ir::render_run_path(&view.frontier.run_path)
            ),
        });
    }

    // Unfinished handler work in ANY of its three shapes is a categorical
    // refusal, and it is settled BEFORE alignment runs — `align` can return
    // `RequiresConfirmation`, and letting that mask a resume the runner
    // would refuse outright would tell the operator to authorize step ids
    // for something that can never proceed. It is also the order
    // `align_preview` uses, and the preview promises to mirror resume's
    // typed refusals.
    //
    // (i) a repair subflow suspended mid-flight — its call frame is still
    // open.
    if let Some(live_hook) = facts
        .live_frames
        .iter()
        .find(|path| path.iter().any(|f| matches!(f, PathFrame::Hook { .. })))
    {
        let _ = session.end(SessionOutcome::Shutdown, None).await;
        return Err(RunnerError::M0Unsupported {
            detail: format!(
                "resume across a live handler-repair frame ({}) is not in the M2 subset — \
                 the repair flow suspended mid-flight; hook-aware frame re-entry is \
                 registered for the repair wave",
                pointlock_ir::render_run_path(live_hook)
            ),
        });
    }
    // (ii) an escalate hook human still awaiting an answer. It leaves NO
    // other trace: it opens no span and pushes no frame (「hook humans are
    // not body steps」), so `live_frames`, `frontier` and `completed` are
    // all blind to it — `humanPending` is the only carrier. Cross-IR it is
    // genuinely unsafe: the continuation is looked up by an instance key
    // rebuilt from the NEW host path, so renaming the host mints a SECOND
    // request and strands the first unanswerable, and deleting the host
    // strands it forever. Same-IR rebuilds the same key and settles
    // correctly, which is why this refusal lives here and not there.
    if let Some(pending) = view
        .human_pending
        .as_ref()
        .filter(|pending| !is_alignable_path(&pending.run_path))
    {
        let _ = session.end(SessionOutcome::Shutdown, None).await;
        return Err(RunnerError::M0Unsupported {
            detail: format!(
                "a handler escalation is still awaiting an answer ({}); resuming it under a \
                 repaired IR needs hook-aware re-entry, which is registered for the repair \
                 wave — answer or let it time out first",
                pointlock_ir::render_run_path(&pending.run_path)
            ),
        });
    }

    let seed = ScopeSeed::new(
        params_object(&view),
        &view.binding.device_id,
        opts.platform.as_deref(),
        run_id,
    );

    // (A) alignment first (07 §4.1). Classification runs on the archived
    // per-step hashes the fold harvested from `stepEntered` (spine §6.1
    // M1 note) — the old FlowIR is not required.
    let mut alignment = match align(
        &loaded,
        &new_flow.body,
        &view,
        &facts,
        &seed,
        store,
        opts.vision.as_deref(),
        &opts.allow_mutating_reexec,
        &opts.force_reexecute,
        opts.old_flow_ir.as_ref(),
    )
    .await
    {
        Ok(alignment) => alignment,
        Err(error) => {
            // A pre-header refusal must not leak the opened session
            // (best-effort teardown, 04 §2.1).
            let _ = session.end(SessionOutcome::Shutdown, None).await;
            return Err(error);
        }
    };

    // (B) unconditional reconcile of a pending intent (07 §4.1/§4.4).
    let mut frontier_work = None;
    let mut deferred_settle = None;
    let mut pending_adjudication: Option<Box<Adjudication>> = None;
    let mut blocked = None;
    if let Some(intent) = &view.frontier.pending_intent {
        let frontier_key = instance_key(&view.frontier.run_path);
        // Resolved by PATH, not by a flat id scan: the frontier can sit
        // inside a branch, and same-IR resume already resolves it this way.
        let new_step = loaded.resolve_action(&view.frontier.run_path);
        // The frontier IS the resume point when its instance is the one
        // alignment named. Instance keys, not body indices: the comparison
        // has to keep working once the resume point can sit inside a
        // callee or an iteration.
        let at_resume = alignment.resume_key.as_deref() == Some(frontier_key.as_str());
        // §4.1 cross semantics: an effect-dirty frontier step's old result
        // is never adopted — it is the product of the old binding. A
        // missing hash or a frontier step absent from the new IR fails
        // closed (dirty).
        let effect_dirty = match (new_step, facts.entered_effect_hash.get(&frontier_key)) {
            (Some(step), Some(archived)) => *archived != step.base.effect_hash,
            _ => true,
        };
        let decision = match reconcile_frontier(
            &mut session,
            new_step,
            at_resume,
            effect_dirty,
            &view,
            &facts,
            &bind_cursor,
            &mut alignment.report,
            intent,
        )
        .await
        {
            Ok(decision) => decision,
            Err(error) => {
                let _ = session.end(SessionOutcome::Shutdown, None).await;
                return Err(error);
            }
        };
        match decision {
            FrontierDecision::Work(work) => frontier_work = Some((frontier_key, work)),
            FrontierDecision::DeferredSettle(settle) => deferred_settle = Some(settle),
            FrontierDecision::Adjudicate(adjudication) => pending_adjudication = Some(adjudication),
            FrontierDecision::Blocked(reason) => blocked = Some(reason),
            FrontierDecision::Nothing => {}
        }
    }

    // The segment header (see the same-IR site for the cursor semantics).
    let resumed_cursor = session.current_cursor().await.ok();
    store.append_event(
        run_id,
        now_ms(),
        &root_path(new_flow),
        &RunLogPayload::RunResumed {
            alignment_report: alignment.report.clone(),
            supervise_policy: opts.supervise,
            event_cursor: resumed_cursor,
        },
    )?;

    // Offline re-judgements: new verdicts with `supersedes` lineage,
    // anchored at the old records' run paths (the fold re-projects the
    // completed records); written back via the *current* session
    // (07 §5.3 — cross-session write-back is sound, the daemon only
    // persists).
    let rejudged = std::mem::take(&mut alignment.rejudged);
    for rejudge in rejudged {
        // Remote archival first so its outcome rides the event; a
        // failure is annotation material, never a resume error (04 §5 —
        // the RunLog is the sole truth). Wire caps applied here like on
        // every other write-back: compaction is the runner's job (04 §5).
        let remote_archival_error = session
            .record_verdict(pointlock_provider_kit::VerdictWrite {
                status: rejudge.verdict.status,
                summary: crate::engine::cap_wire_summary(&rejudge.verdict),
                evidence: rejudge
                    .verdict
                    .evidence
                    .iter()
                    .take(pointlock_provider_kit::VERDICT_EVIDENCE_MAX_ENTRIES)
                    .cloned()
                    .collect(),
            })
            .await
            .err()
            .map(|error| format!("remote archival failed: {error}"));
        store.append_event(
            run_id,
            now_ms(),
            &rejudge.run_path,
            &RunLogPayload::VerdictRecorded {
                verdict: rejudge.verdict.clone(),
                localized: Vec::new(),
                localization_gaps: Vec::new(),
                remote_archival_error,
            },
        )?;
    }

    // A reconciled completed terminal that cannot be adopted at the
    // resume point is still recorded — the ledger closes the intent
    // and keeps the world fact as evidence (07 §4.1).
    if let Some((path, call_id, outcome)) = deferred_settle {
        store.append_event(
            run_id,
            now_ms(),
            &path,
            &RunLogPayload::ActionSettled {
                call_id,
                outcome: crate::engine::quarantine_unpersistable(*outcome),
            },
        )?;
    }

    if let Some(adjudication) = pending_adjudication {
        // Phase 1 of the 07 §4.4 default escalation: the request (fresh or
        // re-awaited) is the segment's outcome — the run suspends
        // `awaitingHuman` and the answer arrives through the ordinary
        // arbitration channel, durable for the next resume to consume.
        let Adjudication {
            run_path,
            request,
            pending,
        } = *adjudication;
        if let Some((request_id, prompt, presents)) = request {
            store.append_event(
                run_id,
                now_ms(),
                &run_path,
                &RunLogPayload::HumanRequested {
                    request_id,
                    purpose: pointlock_ir::HumanPurpose::Step,
                    mode: Some(pointlock_ir::vocab::HumanMode::RepairWorld),
                    prompt,
                    presents,
                    decisions: Some(vec![
                        "adopt".to_owned(),
                        "redo".to_owned(),
                        "abort".to_owned(),
                    ]),
                    output_schema: None,
                    deadline_at_ms: None,
                },
            )?;
        }
        let summary = crate::engine::capture_provider_state_summary(
            session.as_ref(),
            &view.binding.session_lineage,
            &view.binding.device_id,
            opts.platform.as_deref(),
        )
        .await;
        store.append_event(
            run_id,
            now_ms(),
            &root_path(new_flow),
            &RunLogPayload::RunSuspended {
                provider_state_summary: Some(summary),
                reason: Some(format!(
                    "awaiting human adjudication (requestId {})",
                    pending.request_id
                )),
            },
        )?;
        let _ = session.end(SessionOutcome::Shutdown, None).await;
        return Ok(RunOutcome::AwaitingHuman { pending });
    }

    if let Some(reason) = blocked {
        // Suspension-instant profile (07 §2.2): the session is still
        // live at this pre-Execution blocked refusal.
        let summary = crate::engine::capture_provider_state_summary(
            session.as_ref(),
            &view.binding.session_lineage,
            &view.binding.device_id,
            opts.platform.as_deref(),
        )
        .await;
        store.append_event(
            run_id,
            now_ms(),
            &root_path(new_flow),
            &RunLogPayload::RunSuspended {
                provider_state_summary: Some(summary),
                reason: Some(reason.to_string()),
            },
        )?;
        let _ = session.end(SessionOutcome::Shutdown, None).await;
        return Ok(RunOutcome::Blocked { reason });
    }

    let Alignment {
        adoptable,
        teardown,
        ..
    } = alignment;
    // 07 §5.2 case (b): dismantle the stale frame ON THE LEDGER before
    // execution starts, mirroring `exec_call`'s abort unwind exactly —
    // close the open spans innermost-first (the fold's exit pairing is
    // LIFO), pop each live frame right before its own call span closes,
    // and let the call step exit `aborted` (an aborted execution makes no
    // semantic claim; nothing here is adoptable history). Emitted AFTER
    // the deferred settle above, so a terminal the reconcile closed lands
    // on the still-open frontier span and is archived with it.
    //
    // The suspension chain is one nested sequence, so "the torn-down
    // subtree" is precisely the open spans at or under the call's own key.
    let torn = |key: &str| -> bool {
        teardown
            .as_deref()
            .is_some_and(|call| key == call || crate::align::is_instance_descendant(call, key))
    };
    if teardown.is_some() {
        let live_keys: BTreeSet<String> = facts
            .live_frames
            .iter()
            .map(|path| instance_key(path))
            .collect();
        for span in facts.open_spans.iter().rev() {
            let key = instance_key(span);
            if !torn(&key) {
                continue;
            }
            if live_keys.contains(&key) {
                // The span belongs to a call step whose frame is open: the
                // frame pops first, the span closes second — the exact
                // unwind order of a live abort.
                store.append_event(
                    run_id,
                    now_ms(),
                    span,
                    &RunLogPayload::CallFramePopped { outputs: None },
                )?;
            }
            store.append_event(
                run_id,
                now_ms(),
                span,
                &RunLogPayload::StepExited {
                    provider_state_summary: None,
                    state: pointlock_ir::StepState::Aborted,
                    output: None,
                    localized: Vec::new(),
                    localization_gaps: Vec::new(),
                },
            )?;
        }
    }
    let open_spans: BTreeMap<String, Value> = facts
        .open_spans
        .iter()
        .filter(|path| !torn(&instance_key(path)))
        .map(|path| {
            let key = instance_key(path);
            let inputs = facts
                .entered_inputs
                .get(&key)
                .cloned()
                .unwrap_or(Value::Null);
            (key, inputs)
        })
        .collect();
    // The torn-down frame is gone from the ledger; handing its pin to the
    // engine would make `exec_call` skip the push for a frame that no
    // longer exists.
    let live_frames: BTreeMap<String, pointlock_ir::Hash> = live_frame_pins(&facts)
        .into_iter()
        .filter(|(key, _)| !torn(key))
        .collect();
    let params = params_object(&view);
    let env = env_bindings(&view.binding.device_id, opts.platform.as_deref(), run_id);
    let exec = Execution {
        flows: &loaded,
        session,
        store,
        run_id: run_id.to_owned(),
        stop: opts.stop,
        env,
        attempt_base: facts.max_attempt.clone(),
        open_spans,
        // Live call frames must not be pushed again on resume (07 §4.6);
        // the pin lets `exec_call` tell a plain re-entry from one that has
        // to rebase the frame onto a repaired callee (07 §5.2 case (a)).
        // The torn-down frame (case (b)) is filtered out above.
        live_frames,
        resumed: true,
        authorized: opts.allow_mutating_reexec.iter().cloned().collect(),
        reentry_seen: false,
        adoptable,
        frontier: frontier_work,
        session_lineage: view.binding.session_lineage.clone(),
        pending_summaries: BTreeMap::new(),
        vision: opts.vision.clone(),
        supervise: opts.supervise,
        human: facts.human_requests.clone(),
        settled: facts.settled.clone(),
        recorded_verdicts: facts.recorded_verdicts.clone(),
        hook_triggers: facts.hook_triggers.clone(),
        clock: opts.clock,
    };
    // Execution restarts at the top of the body; the adoption set does the
    // skipping, seeding each adopted step's output/verdict into its OWN
    // frame as it is reached. That is the same mechanism same-IR resume
    // uses, and the only one that can express a resume point at depth.
    let root = FrameState::new(new_flow, root_path(new_flow), params, 1);
    exec.run(root, 0).await
}

/// Whether cross-IR alignment can ADDRESS this path.
///
/// Exactly a hook guard, and says so rather than re-listing the seven
/// frames it accepts: the walker descends `if` branch bodies, `foreach`
/// rounds, and — under the case (a) down-drill — callee bodies, addressing
/// every one of them by instance key, so `flow`/`step`/`call`/`iteration`
/// (and the attempt/phase/assertion suffixes) are all classifiable. `hook`
/// is the one frame shape nothing addresses.
///
/// Applied to the FRONTIER only. Completed hook-framed records are not
/// refused — 07 §5.2's last bullet rules 「hook 帧下的记录（handler 审计
/// 痕）不参与对齐复用……旧 hook 记录一律归档」: archive them, do not refuse
/// the resume. Refusing cost a real case — a run whose `onFail` repair
/// subflow completed could never be repaired cross-IR afterwards — and
/// archival is already structural rather than a promise:
/// - they are never ADOPTED: adoption is keyed by instance, and node keys
///   come from `child_frame`, which emits only `step`/`call`/`iteration`
///   segments. `instance_key` renders a hook frame as `/hook:<Hook>:<n>`,
///   which no `StepId` can spell, so no node key can ever collide;
/// - they are never ORPHAN-reported: the only hook-framed `StepRecord`s
///   come from a repair subflow's body, whose path always carries the
///   handler-launched `call` frame, and the orphan pass skips records
///   under a call frame the walk did not descend into. An escalate human
///   writes `humanRequested` and no step span at all, so it contributes
///   no record to misreport.
///
/// A LIVE hook frame is still refused, separately and before this: a
/// repair subflow suspended mid-flight needs hook-aware frame re-entry,
/// which is the repair wave's.
fn is_alignable_path(path: &RunPath) -> bool {
    !path
        .iter()
        .any(|frame| matches!(frame, PathFrame::Hook { .. }))
}

/// A pending human adjudication of an uncertain reconcile (07 §4.4): the
/// run suspends `awaitingHuman` on a synthesized `repairWorld` request
/// whose vocabulary is `adopt | redo | abort` (00 §6.7-B). Paired to its
/// intent BY CALL ID (carried in `presents`), so an answer ruled for one
/// dispatch can never be replayed onto a later one.
struct Adjudication {
    /// The hook-framed anchor (`<frontier>/hook:OnResumeDrift:1/adjudicate`).
    run_path: RunPath,
    /// A fresh request to append — `(requestId, prompt, presents)`; `None`
    /// when an unanswered request for this callId is already on the ledger
    /// and the segment simply re-awaits it.
    request: Option<(String, String, Value)>,
    /// What the segment reports as the pending interaction.
    pending: pointlock_ir::HumanPending,
}

/// What the frontier reconcile decided.
enum FrontierDecision {
    /// Mid-flight work for the resume step.
    Work(FrontierWork),
    /// Close the intent in the ledger with the archived terminal; the step
    /// re-executes fresh.
    DeferredSettle(
        (
            pointlock_ir::RunPath,
            String,
            Box<pointlock_ir::ActionOutcome>,
        ),
    ),
    /// Human adjudication required: suspend `awaitingHuman` on the
    /// adjudication request (fresh or re-awaited).
    Adjudicate(Box<Adjudication>),
    /// Human adjudication impossible to even request (defense line).
    Blocked(BlockedReason),
    /// Nothing to carry over (e.g. neverDispatched off the resume point).
    Nothing,
}

/// Applies the 07 §4.4 decision table to a pending intent. `new_step` is
/// the frontier step as resolved in the new IR (nested paths supported);
/// `at_resume` states whether execution will land exactly on it;
/// `effect_dirty` is the §4.1 cross-semantics discriminator.
#[allow(clippy::too_many_arguments)]
async fn reconcile_frontier(
    session: &mut Box<dyn ProviderSession>,
    new_step: Option<&ActionStepIR>,
    at_resume: bool,
    effect_dirty: bool,
    view: &CheckpointView,
    facts: &Harvest,
    bind_cursor: &pointlock_ir::EventCursor,
    report: &mut AlignmentReport,
    intent: &pointlock_ir::PendingIntent,
) -> Result<FrontierDecision, RunnerError> {
    // The issuing credential (07 §4.5): per-intent exact state from the
    // ledger scan. `FromBinding` (no resume preceded the intent) resolves
    // to the BIND-TIME cursor — the run-row binding written once at
    // begin_run — NOT the folded view's cursor, which every
    // cursor-bearing resume reseeds to the newest generation (a
    // generation that never issued this intent). A missing harvest entry
    // means the ledger cannot attest the issuing generation at all:
    // fail-closed to Unknown, never a fabricated credential. Unknown is
    // answered with the uncertain branch WITHOUT an RPC.
    let issuing = facts
        .intent_issuing
        .get(&intent.call_id)
        .cloned()
        .unwrap_or(crate::align::IssuingCursor::Unknown);
    let fate = match &issuing {
        crate::align::IssuingCursor::FromBinding => {
            session.reconcile(&intent.call_id, bind_cursor).await?
        }
        crate::align::IssuingCursor::Known(cursor) => {
            session.reconcile(&intent.call_id, cursor).await?
        }
        crate::align::IssuingCursor::Unknown => ReconcileResult::LogUnavailable {
            reason: "the issuing session is unknowable (a resume predating the \
                     eventCursor carrier intervened); refusing to reconcile with \
                     a fabricated credential"
                .to_owned(),
        },
    };
    let intent_path = facts
        .intent_path
        .get(&intent.call_id)
        .cloned()
        .unwrap_or_else(|| view.frontier.run_path.clone());
    let mutating_gated = new_step.map(gated_mutating).unwrap_or(true);

    match fate {
        ReconcileResult::Completed { outcome } => {
            if !effect_dirty && at_resume {
                // The archived terminal — whatever its four-way
                // discriminant — is adopted and disposed through the same
                // settled-outcome path as a live execute (§6.7-B).
                return Ok(FrontierDecision::Work(FrontierWork::Adopt {
                    call_id: intent.call_id.clone(),
                    intent_path,
                    outcome,
                    args: intent.args_snapshot.clone(),
                    chain_index: facts.intent_chain_index.get(&intent.call_id).copied(),
                }));
            }
            // Not adoptable at the resume point. Whether re-execution
            // risks a second effect follows the 07 §5.4 criterion: only a
            // succeeded or timedOut terminal can have mutated the world;
            // an archived failed/cancelled left no effect to double.
            let effect_possible = matches!(
                outcome.as_ref(),
                pointlock_ir::ActionOutcome::Succeeded { .. }
                    | pointlock_ir::ActionOutcome::TimedOut { .. }
            );
            if effect_possible && mutating_gated {
                // The old action (possibly) took effect but its terminal
                // cannot be adopted (effect-dirty or positionally
                // invalidated): re-execution is a second effect —
                // 07 §5.4 `frontierUnknown`, fail-closed.
                report.requires_confirmation.push(RequiresConfirmation {
                    run_path: view.frontier.run_path.clone(),
                    step_id: new_step.map(|step| step.base.step_id.clone()),
                    cause: "frontierUnknown".to_owned(),
                    reason: format!(
                        "callId {} reached a recorded {} terminal on the device but \
                         it is not adoptable; re-execution of the mutating step \
                         needs explicit authorization",
                        intent.call_id,
                        outcome.kind()
                    ),
                });
                return Err(RunnerError::RequiresConfirmation {
                    report: Box::new(report.clone()),
                });
            }
            Ok(FrontierDecision::DeferredSettle((
                intent_path,
                intent.call_id.clone(),
                outcome,
            )))
        }
        ReconcileResult::NeverDispatched => {
            if !effect_dirty && at_resume {
                // Safe replay: archived args, new callId, new WAL intent.
                Ok(FrontierDecision::Work(FrontierWork::Replay {
                    chain_index: facts.intent_chain_index.get(&intent.call_id).copied(),
                    args: intent.args_snapshot.clone(),
                }))
            } else {
                // The step re-executes fresh from ready (nothing happened
                // in the world).
                Ok(FrontierDecision::Nothing)
            }
        }
        ReconcileResult::StartedNoTerminal => uncertain_branch(
            new_step,
            intent,
            &view.frontier.run_path,
            facts,
            report,
            at_resume,
            effect_dirty,
            "startedNoTerminal",
            facts.intent_chain_index.get(&intent.call_id).copied(),
        ),
        ReconcileResult::LogUnavailable { reason } => uncertain_branch(
            new_step,
            intent,
            &view.frontier.run_path,
            facts,
            report,
            at_resume,
            effect_dirty,
            &format!("logUnavailable: {reason}"),
            facts.intent_chain_index.get(&intent.call_id).copied(),
        ),
    }
}

/// The uncertain reconcile branch (07 §4.4): replay only with the explicit
/// author permission (`idempotent` / `readonly`); otherwise the DEFAULT
/// `onResumeDrift` escalation — a synthesized `repairWorld` human rules
/// `adopt | redo | abort` over the presented callId (00 §6.7-B). The
/// request and its answer live on the ordinary human ledger
/// (`humanRequested`/`humanResponded`), so the operator answers through
/// the same channels as any other wait and the ruling is durable: a crash
/// after the answer re-derives the same disposition.
///
/// A DECLARED `onResumeDrift` binding keeps serving the probe-drift ladder
/// it was written for; routing the reconcile adjudication through custom
/// bindings is registered for the repair wave.
#[allow(clippy::too_many_arguments)]
fn uncertain_branch(
    new_step: Option<&ActionStepIR>,
    intent: &pointlock_ir::PendingIntent,
    frontier_path: &RunPath,
    facts: &Harvest,
    report: &mut AlignmentReport,
    at_resume: bool,
    effect_dirty: bool,
    fate: &str,
    chain_index: Option<u32>,
) -> Result<FrontierDecision, RunnerError> {
    let permitted = new_step.map(replay_permitted).unwrap_or(false);
    if permitted {
        if at_resume && !effect_dirty {
            return Ok(FrontierDecision::Work(FrontierWork::Replay {
                chain_index,
                args: intent.args_snapshot.clone(),
            }));
        }
        // Fresh re-execution is equally safe for readonly/idempotent.
        return Ok(FrontierDecision::Nothing);
    }

    // The adjudication anchor: one hook-framed instance under the frontier
    // step. The leaf id is fixed — identity per INTENT comes from the
    // callId carried in `presents`, checked below, so an answer ruled for
    // an earlier dispatch is never replayed onto this one.
    let mut hook_path = frontier_path.clone();
    hook_path.push(PathFrame::Hook {
        hook: pointlock_ir::HandlerHook::OnResumeDrift,
        trigger: 1,
    });
    hook_path.push(PathFrame::Step {
        step_id: "adjudicate".try_into().expect("a fixed valid step id"),
    });
    let key = instance_key(&hook_path);

    if let Some(fact) = facts.human_requests.get(&key)
        && fact.presents.get("callId").and_then(Value::as_str) == Some(intent.call_id.as_str())
    {
        match &fact.final_response {
            None => {
                // Asked and unanswered: re-await the same request, no
                // duplicate append.
                return Ok(FrontierDecision::Adjudicate(Box::new(Adjudication {
                    run_path: hook_path.clone(),
                    request: None,
                    pending: pending_of(fact, &hook_path),
                })));
            }
            Some(response) => {
                let ruling = response.get("decision").and_then(Value::as_str);
                match ruling {
                    Some("adopt") => {
                        if at_resume && !effect_dirty {
                            // The ruled effect stands; the step's own
                            // assertions verify it over a fresh
                            // observation ([`FrontierWork::ConfirmEffect`]).
                            return Ok(FrontierDecision::Work(FrontierWork::ConfirmEffect {
                                message: format!(
                                    "uncertain fate ({fate}) of callId {} adjudicated \
                                     `adopt`",
                                    intent.call_id
                                ),
                                args: intent.args_snapshot.clone(),
                            }));
                        }
                        // Adopted effect on a step that must nonetheless
                        // re-execute (effect-dirty / positionally
                        // invalidated): a second effect — the 07 §5.4
                        // frontierUnknown gate, same as an unadoptable
                        // recorded terminal.
                        report.requires_confirmation.push(RequiresConfirmation {
                            run_path: frontier_path.clone(),
                            step_id: new_step.map(|step| step.base.step_id.clone()),
                            cause: "frontierUnknown".to_owned(),
                            reason: format!(
                                "callId {} was adjudicated `adopt` (the effect stands) but \
                                 the step is not adoptable here; re-execution of the \
                                 mutating step needs explicit authorization",
                                intent.call_id
                            ),
                        });
                        return Err(RunnerError::RequiresConfirmation {
                            report: Box::new(report.clone()),
                        });
                    }
                    Some("redo") => {
                        // I2 source (iv): the human's redo IS the license.
                        if at_resume && !effect_dirty {
                            return Ok(FrontierDecision::Work(FrontierWork::Replay {
                                chain_index,
                                args: intent.args_snapshot.clone(),
                            }));
                        }
                        return Ok(FrontierDecision::Nothing);
                    }
                    Some("abort") => {
                        return Ok(FrontierDecision::Work(FrontierWork::AbortRuled {
                            args: intent.args_snapshot.clone(),
                        }));
                    }
                    other => {
                        // The store arbitrates against the declared
                        // vocabulary, so this is a ledger anomaly — the
                        // defense line blocks rather than guesses.
                        return Ok(FrontierDecision::Blocked(BlockedReason::RequiresHuman {
                            call_id: intent.call_id.clone(),
                            detail: format!(
                                "adjudication response carries an unusable decision \
                                 {other:?}; refusing to guess"
                            ),
                        }));
                    }
                }
            }
        }
    }

    // No adjudication asked yet (or the one on the ledger belongs to an
    // earlier dispatch): mint the request.
    let request_id = uuid::Uuid::new_v4().to_string();
    let prompt = format!(
        "the fate of callId {} is uncertain ({fate}) and the step is mutating and \
         not idempotent — automatic replay is forbidden (I2). Inspect the device, \
         then rule: `adopt` (the effect happened; verify and continue), `redo` \
         (the effect did not happen or you undid it; dispatch again), or `abort` \
         (stop the run)",
        intent.call_id
    );
    let presents = serde_json::json!({
        "callId": intent.call_id,
        "fate": fate,
        "argsSnapshot": intent.args_snapshot,
    });
    let pending = pointlock_ir::HumanPending {
        run_path: hook_path.clone(),
        request_id: request_id.clone(),
        purpose: pointlock_ir::HumanPurpose::Step,
        mode: Some(pointlock_ir::vocab::HumanMode::RepairWorld),
        prompt: prompt.clone(),
        deadline_at_ms: None,
    };
    Ok(FrontierDecision::Adjudicate(Box::new(Adjudication {
        run_path: hook_path,
        request: Some((request_id, prompt, presents)),
        pending,
    })))
}

/// The pending descriptor of an already-asked adjudication.
fn pending_of(fact: &HumanRequestFact, hook_path: &RunPath) -> pointlock_ir::HumanPending {
    pointlock_ir::HumanPending {
        run_path: hook_path.clone(),
        request_id: fact.request_id.clone(),
        purpose: fact.purpose,
        mode: fact.mode,
        prompt: fact.prompt.clone(),
        deadline_at_ms: fact.deadline_at_ms,
    }
}

/// The params snapshot of a checkpoint as an object map (it was written by
/// `Runner::run` as an object; anything else folds to empty).
fn params_object(view: &CheckpointView) -> Map<String, Value> {
    match &view.params_snapshot {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    }
}
