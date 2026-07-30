//! Deterministic RunLog → [`CheckpointView`] folding.
//!
//! `Checkpoint = deterministic fold of the RunLog` (spine §6.1); this module
//! is that fold, exposed as the pure function [`fold_checkpoint`] so the
//! rebuild channel (`pointlock inspect --rebuild-checkpoint`, 07 §3.3) and
//! the write-path materialization share one implementation and can be
//! equality-checked against each other.
//!
//! ## Fold inputs
//!
//! The 17-event union does not carry the root flow id or the provider
//! binding, so the fold takes a [`RunMeta`] (the `run` table row written by
//! [`crate::Store::begin_run`]) alongside the ordered events. Both inputs
//! are immutable after `begin_run`, keeping the fold a pure function of
//! durable state.
//!
//! ## Coverage rules (iron rule: explicit, never silent)
//!
//! Every one of the 17 event types is matched explicitly below. Events that
//! do not change the view are handled as *documented no-ops*, not wildcard
//! arms. Every [`StepRecord`] field has an event carrier (spine §6.1 M1
//! note — no placeholders remain):
//!
//! - `StepRecord.effectHash` / `judgeHash` / `resolvedInputs`: harvested
//!   from the `stepEntered` payload.
//! - `StepRecord.output`: harvested from the `stepExited` payload; a call
//!   step whose exit carries no output keeps the callee outputs harvested
//!   from `callFramePopped`.
//! - `CallFrame.nextIndex`: a *body cursor* — advanced only when the
//!   exited step is a direct body child of the innermost frame (nested
//!   container children and iteration instances do not move it; M2).
//! - `CallFrame.iterStack`: reconstructed from the open container spans —
//!   an in-flight span whose successor extends it with an `iteration`
//!   path frame is a live foreach; the `as` name comes from the
//!   container's `stepEntered` snapshot (`{ items, as }`, the runner's
//!   foreach carrier). No carrier ⇒ no IterState (never fabricated).
//! - `CallFrame.vars` stays empty in the fold: `let` products have no
//!   dedicated event carrier (the `stepEntered` snapshot of a let step
//!   *is* the bindings object, but the fold is kind-agnostic); the runner
//!   re-seeds scope from the records on resume (documented divergence,
//!   pending the handler wave).
//! - `binding.sessionLineage` / `binding.eventCursor`: copied verbatim from
//!   [`RunMeta`]; no event advances the cursor or appends a session
//!   generation yet (M1 scope).
//! - `runResumed.alignmentReport` stays log-resident; the fold does not
//!   re-base completed records.

use pointlock_ir::{
    ActChannel, ActionExecution, ActionName, ActionOutcome, ActionOutcomeKind,
    AssertionOutcomeRecord, AttemptRecord, BindingState, CallFrame, CheckpointView, ErrorClass,
    EvidenceRef, ExecutionMode, FlowId, Frontier, Hash, HumanPending, HumanPurpose, IterState,
    ObservationRecord, PathFrame, PendingIntent, RunLogEvent, RunLogPayload, RunPath, StepId,
    StepRecord, StepState, StepVerdict, Verdict, VerdictStatus,
};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::error::FoldError;

/// Immutable per-run metadata (the `run` table row): the fold input the
/// 17-event union does not carry — root flow id, provider binding seed, and
/// the identity fields also present in `runStarted`.
#[derive(Debug, Clone, PartialEq)]
pub struct RunMeta {
    /// The run's id.
    pub run_id: String,
    /// The root flow's id (source of the root [`CallFrame`]'s `flowId`;
    /// `runStarted` does not carry it).
    pub flow_id: FlowId,
    /// Content hash of the executing IR.
    pub ir_hash: Hash,
    /// Digest of the bound capability lockfile.
    pub lockfile_digest: Hash,
    /// The run's input parameters.
    pub params_snapshot: Value,
    /// Provider binding seed (M0: copied into the view verbatim; no event
    /// advances the cursor yet — M0-C).
    pub binding: BindingState,
    /// Run creation timestamp (ms since epoch); informational.
    pub created_at_ms: u64,
}

/// Run lifecycle status — the `run.status` column's closed four-value set
/// (07 §3.3 DDL CHECK constraint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// The run is executing (also the `begin_run` seed value).
    Running,
    /// The run was suspended (`runSuspended`).
    Suspended,
    /// A human interaction is pending (`humanRequested`; covers both step
    /// and supervision purposes — the discriminator lives in
    /// `humanPending.purpose`, not at run level; R13).
    AwaitingHuman,
    /// The run finished (`runFinished`).
    Finished,
}

impl RunStatus {
    /// The stored string form (matches the DDL CHECK constraint verbatim).
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Suspended => "suspended",
            RunStatus::AwaitingHuman => "awaitingHuman",
            RunStatus::Finished => "finished",
        }
    }

    /// Parses the stored string form.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(RunStatus::Running),
            "suspended" => Some(RunStatus::Suspended),
            "awaitingHuman" => Some(RunStatus::AwaitingHuman),
            "finished" => Some(RunStatus::Finished),
            _ => None,
        }
    }
}

/// Result of a fold: the materializable view plus the run status the same
/// event sequence implies (kept together so the write path and the rebuild
/// self-check share one transition function).
#[derive(Debug, Clone, PartialEq)]
pub struct FoldedRun {
    /// The deterministic checkpoint view.
    pub view: CheckpointView,
    /// The run status after the last event.
    pub status: RunStatus,
}

/// Folds an ordered event sequence into a [`CheckpointView`] + run status.
///
/// Pure and deterministic: same `(meta, events)` in, same [`FoldedRun`]
/// out. With zero events it returns the seeded pre-start view (empty frame
/// stack, `pending` frontier at the root flow path). Structural violations
/// return [`FoldError`] — see the module docs for the coverage rules.
pub fn fold_checkpoint(meta: &RunMeta, events: &[RunLogEvent]) -> Result<FoldedRun, FoldError> {
    Ok(fold_state(meta, events)?.finish())
}

/// Folds the full ledger into the terminal [`FoldState`] (not yet
/// collapsed to a view). `Store::append_event` caches it per run so the
/// next append folds exactly one event instead of the whole ledger —
/// the single-writer invariant (I1) makes the carried state exact, and
/// `verify_checkpoint` remains the from-scratch cross-check.
pub(crate) fn fold_state(meta: &RunMeta, events: &[RunLogEvent]) -> Result<FoldState, FoldError> {
    let mut state = FoldState::seed(meta);
    let mut prev_seq: Option<u64> = None;
    for event in events {
        if event.run_id != meta.run_id {
            return Err(FoldError::RunIdMismatch {
                seq: event.seq,
                expected: meta.run_id.clone(),
                actual: event.run_id.clone(),
            });
        }
        if let Some(prev) = prev_seq
            && event.seq <= prev
        {
            return Err(FoldError::NonMonotonicSeq {
                prev,
                seq: event.seq,
            });
        }
        prev_seq = Some(event.seq);
        state.apply(event)?;
    }
    Ok(state)
}

/// Scratch record of the step currently being assembled between
/// `stepEntered` and `stepExited`. Kept as a stack: a `call` step stays
/// in flight while its callee's steps enter and exit above it.
#[derive(Debug, Clone)]
struct InFlightStep {
    run_path: RunPath,
    step_id: StepId,
    effect_hash: Hash,
    judge_hash: Hash,
    resolved_inputs: Value,
    attempts: Vec<AttemptRecord>,
    /// Callee outputs harvested from `callFramePopped` (call steps); the
    /// `stepExited` payload's own output takes precedence when present.
    call_outputs: Option<Value>,
    observations: Vec<ObservationRecord>,
    evidence: Vec<EvidenceRef>,
    assertion_outcomes: Vec<AssertionOutcomeRecord>,
    verdict: Option<StepVerdict>,
}

/// (chainIndex, channel, actionName) of one `actionIntent` (item ②).
type DispatchIdentity = (Option<u32>, Option<ActChannel>, Option<ActionName>);

#[derive(Debug, Clone)]
pub(crate) struct FoldState {
    view: CheckpointView,
    status: RunStatus,
    root_flow_id: FlowId,
    started: bool,
    in_flight: Vec<InFlightStep>,
    /// callId → the intent's dispatch identity (item ②): in-memory
    /// carrier from `actionIntent` to the settling `attemptRecord`;
    /// never persisted (the durable shapes stay unchanged).
    intent_dispatch: BTreeMap<String, DispatchIdentity>,
    /// The run-path prefix of each live frame (parallel to `view.frames`):
    /// the root flow path, then each pushed call frame's event path. The
    /// direct-body-child test for `nextIndex` needs it.
    frame_paths: Vec<RunPath>,
}

impl FoldState {
    fn seed(meta: &RunMeta) -> Self {
        let root_path: RunPath = vec![PathFrame::Flow {
            flow_id: meta.flow_id.clone(),
            ir_hash: meta.ir_hash.clone(),
        }];
        FoldState {
            view: CheckpointView {
                run_id: meta.run_id.clone(),
                ir_hash: meta.ir_hash.clone(),
                lockfile_digest: meta.lockfile_digest.clone(),
                params_snapshot: meta.params_snapshot.clone(),
                binding: meta.binding.clone(),
                completed: Vec::new(),
                frames: Vec::new(),
                frontier: Frontier {
                    run_path: root_path,
                    state: StepState::Pending,
                    pending_intent: None,
                },
                human_pending: None,
            },
            status: RunStatus::Running,
            root_flow_id: meta.flow_id.clone(),
            started: false,
            in_flight: Vec::new(),
            intent_dispatch: BTreeMap::new(),
            frame_paths: Vec::new(),
        }
    }

    pub(crate) fn finish(mut self) -> FoldedRun {
        // Live foreach reconstruction (07 §3.2 iterStack): an in-flight
        // span whose successor's run path extends it with an `iteration`
        // frame is a live foreach round; the `as` name comes from the
        // container's stepEntered snapshot ({ items, as }). No carrier ⇒
        // no IterState (never fabricated).
        for frame in &mut self.view.frames {
            frame.iter_stack.clear();
        }
        for pair in self.in_flight.windows(2) {
            let (parent, child) = (&pair[0], &pair[1]);
            if child.run_path.len() <= parent.run_path.len() {
                continue;
            }
            let extends = parent
                .run_path
                .iter()
                .zip(child.run_path.iter())
                .all(|(a, b)| same_site(a, b));
            let Some(PathFrame::Iteration { index, key }) =
                child.run_path.get(parent.run_path.len())
            else {
                continue;
            };
            let Some(var) = parent
                .resolved_inputs
                .get("as")
                .and_then(Value::as_str)
                .filter(|_| extends)
            else {
                continue;
            };
            // The IterState belongs to the innermost frame whose prefix
            // covers the foreach span.
            let owner = self.frame_paths.iter().rposition(|prefix| {
                parent.run_path.len() >= prefix.len()
                    && prefix
                        .iter()
                        .zip(parent.run_path.iter())
                        .all(|(a, b)| same_site(a, b))
            });
            if let Some(owner) = owner
                && owner < self.view.frames.len()
            {
                self.view.frames[owner].iter_stack.push(IterState {
                    var: var.to_owned(),
                    index: *index,
                    key: key.clone(),
                });
            }
        }
        FoldedRun {
            view: self.view,
            status: self.status,
        }
    }

    /// Applies one event. All 17 payload variants are matched explicitly
    /// (`callFramePushed` twice — its `rebase` discriminant selects between
    /// opening a frame and re-entering one); no-op arms are documented as
    /// such (M0 iron rule — nothing is silently ignored via a wildcard).
    pub(crate) fn apply(&mut self, event: &RunLogEvent) -> Result<(), FoldError> {
        let seq = event.seq;
        if !self.started && !matches!(event.payload, RunLogPayload::RunStarted { .. }) {
            return Err(FoldError::EventBeforeRunStarted {
                seq,
                event_type: event.payload.event_type(),
            });
        }
        match &event.payload {
            RunLogPayload::RunStarted {
                ir_hash,
                lockfile_digest,
                params_snapshot,
                // Per-segment supervision policy is log-resident audit data
                // (spine §6.9): CheckpointView has no field for it.
                supervise_policy: _,
            } => {
                if self.started {
                    return Err(FoldError::DuplicateRunStarted { seq });
                }
                self.started = true;
                // The log is the truth: adopt the payload's identity fields
                // (begin_run writes the same values into the run row).
                self.view.ir_hash = ir_hash.clone();
                self.view.lockfile_digest = lockfile_digest.clone();
                self.view.params_snapshot = params_snapshot.clone();
                // Root call frame: flowId comes from RunMeta (the payload
                // does not carry it), inputs are the params snapshot
                // (07 §3.2: the root frame references paramsSnapshot).
                self.view.frames.push(CallFrame {
                    flow_id: self.root_flow_id.clone(),
                    ir_hash: ir_hash.clone(),
                    call_step_id: None,
                    inputs_snapshot: params_snapshot.clone(),
                    vars: Default::default(),
                    iter_stack: Vec::new(),
                    next_index: 0,
                });
                self.view.frontier = Frontier {
                    run_path: vec![PathFrame::Flow {
                        flow_id: self.root_flow_id.clone(),
                        ir_hash: ir_hash.clone(),
                    }],
                    state: StepState::Pending,
                    pending_intent: None,
                };
                self.frame_paths.push(vec![PathFrame::Flow {
                    flow_id: self.root_flow_id.clone(),
                    ir_hash: ir_hash.clone(),
                }]);
                self.status = RunStatus::Running;
            }
            RunLogPayload::StepEntered {
                step_id,
                effect_hash,
                judge_hash,
                resolved_inputs,
            } => {
                self.in_flight.push(InFlightStep {
                    run_path: event.run_path.clone(),
                    step_id: step_id.clone(),
                    effect_hash: effect_hash.clone(),
                    judge_hash: judge_hash.clone(),
                    resolved_inputs: resolved_inputs.clone(),
                    attempts: Vec::new(),
                    call_outputs: None,
                    observations: Vec::new(),
                    evidence: Vec::new(),
                    assertion_outcomes: Vec::new(),
                    verdict: None,
                });
                self.view.frontier = Frontier {
                    run_path: event.run_path.clone(),
                    state: StepState::Ready,
                    pending_intent: None,
                };
            }
            RunLogPayload::PreflightProbed { outcomes } => {
                // The probe outcomes stay log-resident (they are probes,
                // not the step's assert-phase outcomes), but the frontier's
                // STATE materializes (spine §6.2 / §6.6): a passed probe
                // set leaves the step `probing` (the act overwrites it with
                // `acting` moments later — the window is only visible when
                // the run stops in it), and a missed one leaves it
                // `drifted`, which is exactly what a checkpoint suspended
                // on drift must say — 「resume probe failed; awaiting
                // onResumeDrift disposition」 was unobservable before this
                // arm wrote it.
                //
                // An EMPTY outcome list is the `unprobed` mark (07 §4.2
                // rule 1), a note that nothing was checked — not a phase
                // transition; the state stays whatever it was.
                if !outcomes.is_empty() {
                    let missed = outcomes
                        .iter()
                        .any(|outcome| outcome.result != VerdictStatus::Pass);
                    self.view.frontier.state = if missed {
                        StepState::Drifted
                    } else {
                        StepState::Probing
                    };
                }
            }
            RunLogPayload::ActionIntent {
                call_id,
                args_snapshot,
                chain_index,
                channel,
                action_name,
            } => {
                // Fold-internal intent→settle carrier (2026-07-18
                // incorporation, item ②): the durable PendingIntent shape
                // stays unchanged (I1 on existing stores); the identity
                // fields ride in memory keyed by callId, deterministic
                // from events (the full-refold fallback reproduces it).
                self.intent_dispatch.insert(
                    call_id.clone(),
                    (*chain_index, *channel, action_name.clone()),
                );
                // The crash-window key (07 §3.1): frontier records the
                // hanging intent until the matching actionSettled.
                self.view.frontier.state = StepState::Acting;
                self.view.frontier.pending_intent = Some(PendingIntent {
                    call_id: call_id.clone(),
                    args_snapshot: args_snapshot.clone(),
                });
            }
            RunLogPayload::ActionSettled { call_id, outcome } => {
                let step =
                    self.in_flight
                        .last_mut()
                        .ok_or_else(|| FoldError::EventOutsideStep {
                            seq,
                            event_type: event.payload.event_type(),
                        })?;
                let dispatch = self.intent_dispatch.remove(call_id).unwrap_or_default();
                step.attempts
                    .push(attempt_record(call_id, outcome, dispatch));
                // Clear the pending intent this terminal settles. A
                // non-matching callId leaves the intent in place (a runner
                // discipline breach worth surfacing at reconcile time, not
                // papering over here).
                if self
                    .view
                    .frontier
                    .pending_intent
                    .as_ref()
                    .is_some_and(|intent| intent.call_id == *call_id)
                {
                    self.view.frontier.pending_intent = None;
                }
                self.view.frontier.state = StepState::Settling;
            }
            RunLogPayload::ObservationRecorded { observation } => {
                let step =
                    self.in_flight
                        .last_mut()
                        .ok_or_else(|| FoldError::EventOutsideStep {
                            seq,
                            event_type: event.payload.event_type(),
                        })?;
                if let Some(screenshot) = &observation.screenshot {
                    step.evidence.push(screenshot.clone());
                }
                if let Some(ui_snapshot) = &observation.ui_snapshot {
                    step.evidence.push(ui_snapshot.clone());
                }
                step.observations.push(observation.clone());
                self.view.frontier.state = StepState::Observing;
            }
            RunLogPayload::AssertionEvaluated { outcome } => {
                let step =
                    self.in_flight
                        .last_mut()
                        .ok_or_else(|| FoldError::EventOutsideStep {
                            seq,
                            event_type: event.payload.event_type(),
                        })?;
                step.assertion_outcomes.push(outcome.clone());
                self.view.frontier.state = StepState::Asserting;
            }
            RunLogPayload::VerdictRecorded {
                verdict,
                localized,
                localization_gaps: _,
                remote_archival_error: _,
            } => {
                // The judgment's localized manifest merges into the
                // record's evidence (item ③, 2026-07-18): dedup key
                // (sha256, asset.id), first occurrence wins — the same
                // rule the dossier applies, so the two surfaces can
                // never diverge on one ledger. Gaps stay log-resident
                // (the dossier reads them from the event; the checkpoint
                // carries successes only).
                //
                // The target is chosen by the event's OWN run path, never by
                // "is anything in flight". A crash-opened span leaves an
                // in-flight step that has nothing to do with an offline
                // re-judgement written against a completed record, and
                // attaching the verdict to it would silently overwrite a
                // different step's judgment — the ledger would then say the
                // crashed step was judged and the re-judged one was not.
                // Every live `verdictRecorded` is appended at its own step's
                // path (the same path its `stepEntered` used), so path
                // equality selects exactly the target `last_mut()` used to
                // select in every live case.
                let in_flight_at_path = self
                    .in_flight
                    .iter()
                    .rposition(|step| same_instance(&step.run_path, &event.run_path));
                if let Some(index) = in_flight_at_path {
                    let step = &mut self.in_flight[index];
                    merge_evidence(&mut step.evidence, localized);
                    step.verdict = Some(project_verdict(verdict));
                    self.view.frontier.state = StepState::Judged;
                } else if let Some(record) = self
                    .view
                    .completed
                    .iter_mut()
                    .rev()
                    .find(|record| same_instance(&record.run_path, &event.run_path))
                {
                    // Offline re-judgement (judgeDirty, spine §6.7-A): the
                    // log gets a *new* verdictRecorded with `supersedes`;
                    // the fold re-projects the completed record. Rejudge
                    // manifests are empty by construction (nothing is
                    // localized offline) — the merge is a no-op there,
                    // and the arm treats the field identically in both
                    // branches so incremental and full refolds agree.
                    merge_evidence(&mut record.evidence, localized);
                    record.verdict = Some(project_verdict(verdict));
                } else {
                    return Err(FoldError::VerdictWithoutTarget { seq });
                }
            }
            RunLogPayload::StepExited {
                state,
                output,
                localized,
                ..
            } => {
                let mut step = self
                    .in_flight
                    .pop()
                    .ok_or(FoldError::StepExitedWithoutEntry { seq })?;
                // An unverified exit's manifest merges here (item ③
                // review fix) — same rule as the verdict-borne one.
                merge_evidence(&mut step.evidence, localized);
                // A terminal exit of the awaiting step settles its pending
                // request without a response — the lazy timeout settlement
                // (verdict unknown) and the aborted disposition both take
                // this path (06 §5.3).
                if self
                    .view
                    .human_pending
                    .as_ref()
                    .is_some_and(|pending| pending.run_path == event.run_path)
                {
                    self.view.human_pending = None;
                }
                // Every terminal exit leaves a record (judged / skipped /
                // blocked / aborted alike): completion order == exit order.
                self.view.completed.push(StepRecord {
                    run_path: step.run_path,
                    step_id: step.step_id,
                    // Harvested from the stepEntered carrier (spine §6.1
                    // M1 note).
                    effect_hash: step.effect_hash,
                    judge_hash: step.judge_hash,
                    attempts: step.attempts,
                    resolved_inputs: step.resolved_inputs,
                    // The exit's projected output wins; a call step whose
                    // exit carries none keeps the callee outputs from
                    // callFramePopped.
                    output: output.clone().or(step.call_outputs),
                    observations: step.observations,
                    evidence: step.evidence,
                    assertion_outcomes: step.assertion_outcomes,
                    verdict: step.verdict,
                });
                // Advance the innermost frame's *body* cursor — only when
                // the exited step is a direct body child of that frame
                // (nested container children and iteration instances do
                // not move it; M2). While a callee runs, the innermost
                // frame *is* the callee frame, so this lands on the right
                // frame for nested exits too. Frame identity is compared
                // site-wise (hash-insensitive) so a cross-IR resume
                // segment keeps advancing the same frame.
                let frame = self
                    .view
                    .frames
                    .last_mut()
                    .ok_or(FoldError::NoActiveFrame { seq })?;
                if self
                    .frame_paths
                    .last()
                    .is_some_and(|prefix| direct_body_child(prefix, &event.run_path))
                {
                    frame.next_index += 1;
                }
                self.view.frontier = Frontier {
                    run_path: event.run_path.clone(),
                    state: *state,
                    pending_intent: None,
                };
            }
            RunLogPayload::CallFramePushed {
                frame,
                rebase: false,
            } => {
                self.view.frames.push(frame.clone());
                self.frame_paths.push(event.run_path.clone());
            }
            RunLogPayload::CallFramePushed {
                frame,
                rebase: true,
            } => {
                // A live-frame re-entry under a repaired callee (07 §5.2
                // case (a)) — NOT a new stack level. The addressed level is
                // the event path's `call` depth, not the innermost frame: a
                // resume walks back in from the root, so an outer frame is
                // re-entered while the inner ones it once opened are still
                // on the stack. Cross-IR safe by construction — the count
                // reads the path's shape, never its hashes.
                let level = event
                    .run_path
                    .iter()
                    .filter(|frame| matches!(frame, PathFrame::Call { .. }))
                    .count();
                let depth = self.view.frames.len();
                let Some(open) = self.view.frames.get_mut(level) else {
                    return Err(FoldError::RebaseWithoutFrame { seq, level, depth });
                };
                // ONLY the callee pin moves. `inputsSnapshot` above all
                // stays put: a live frame's snapshot is never re-evaluated
                // for a new IR (07 §5.2 corollary / §4.6), and the descent
                // was licensed precisely because the `inputs` expressions
                // did not change — so the archived values ARE the ones the
                // new IR would produce. The body cursor, iteration stack
                // and vars describe where the frame *is*, which a repaired
                // callee does not move either.
                open.ir_hash = frame.ir_hash.clone();
                // `frame_paths` is left alone: it is only ever compared
                // through `same_site`, which is hash-insensitive precisely
                // so a cross-IR resume keeps matching the same site.
            }
            RunLogPayload::CallFramePopped { outputs } => {
                if self.view.frames.len() <= 1 {
                    return Err(FoldError::PoppedRootFrame { seq });
                }
                self.view.frames.pop();
                self.frame_paths.pop();
                // The innermost in-flight step is the host call step (its
                // callee's steps have all exited); the callee's outputs
                // are the call step's output. Handler-repair frames have
                // no host call step — nothing in flight, nothing to fill.
                if let Some(step) = self.in_flight.last_mut() {
                    step.call_outputs = outputs.clone();
                }
            }
            RunLogPayload::HandlerTriggered {
                hook: _,
                trigger: _,
                disposition: _,
            } => {
                // Documented no-op: handler firing is audit data. The hook
                // trace materializes through the `hook` frames of
                // subsequent events' run paths, not as a view field.
            }
            RunLogPayload::HumanRequested {
                request_id,
                purpose,
                mode,
                prompt,
                // The presented evidence/values and the response contract
                // stay log-resident; HumanPending does not carry them
                // (spine §6.6, 06 §4.3 reads them back from the event).
                presents: _,
                decisions: _,
                output_schema: _,
                deadline_at_ms,
            } => {
                self.view.human_pending = Some(HumanPending {
                    run_path: event.run_path.clone(),
                    request_id: request_id.clone(),
                    purpose: *purpose,
                    mode: *mode,
                    prompt: prompt.clone(),
                    deadline_at_ms: *deadline_at_ms,
                });
                self.view.frontier.state = StepState::AwaitingHuman;
                self.status = RunStatus::AwaitingHuman;
            }
            RunLogPayload::HumanResponded {
                request_id,
                purpose,
                response,
                actor: _,
            } => {
                // Lazy settlement (spine §6.8): a response must pair the
                // pending request; the arbitration result itself
                // (response/actor) stays log-resident.
                let paired = self
                    .view
                    .human_pending
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == *request_id);
                if !paired {
                    return Err(FoldError::UnpairedHumanResponse {
                        seq,
                        request_id: request_id.clone(),
                    });
                }
                // A supervision `suspend` answer is non-final (spine §6.9):
                // the request stays pending across segments and the run
                // keeps awaiting a proceed/abort ruling.
                let retains = *purpose == HumanPurpose::Supervision
                    && response.get("decision").and_then(Value::as_str) == Some("suspend");
                if retains {
                    self.status = RunStatus::AwaitingHuman;
                } else {
                    self.view.human_pending = None;
                    // The frontier step state stays as-is: the follow-up
                    // event (actionIntent on supervision-proceed,
                    // verdictRecorded / stepExited on a human step) moves
                    // it.
                    self.status = RunStatus::Running;
                }
            }
            RunLogPayload::RunSuspended { reason: _, .. } => {
                // Run-level status only; the frontier keeps its last
                // step-level state so resume knows where the step stood.
                // A suspension while a human request is pending keeps the
                // run self-describing as awaitingHuman (spine §6.8: the
                // wait is a legal suspend point).
                self.status = if self.view.human_pending.is_some() {
                    RunStatus::AwaitingHuman
                } else {
                    RunStatus::Suspended
                };
            }
            RunLogPayload::RunResumed {
                // Log-resident in M0: the fold does not re-base completed
                // records from the alignment report (module docs; M0-C).
                alignment_report: _,
                // Per-segment policy, log-resident (as for runStarted).
                supervise_policy: _,
                event_cursor,
            } => {
                self.status = RunStatus::Running;
                // 07 §4.5 (incorporated 2026-07-18): a cursor-bearing
                // resume extends the lineage and reseeds the watermark.
                // A cursor-less resume (old ledgers) changes nothing —
                // the view names exactly what was recorded, never an
                // invented generation (principle 4). Only new-binary
                // ledgers carry the field, so stored views and refolds
                // agree on every pre-incorporation store (I1).
                if let Some(cursor) = event_cursor {
                    if self.view.binding.session_lineage.last() != Some(&cursor.session_id) {
                        self.view
                            .binding
                            .session_lineage
                            .push(cursor.session_id.clone());
                    }
                    self.view.binding.event_cursor = cursor.clone();
                }
            }
            RunLogPayload::RunFinished {
                verdict: _,
                remote_archival_error: _,
            } => {
                // The folded flow verdict stays log-resident; the view has
                // no field for it (reports read it from the log).
                self.status = RunStatus::Finished;
            }
        }
        Ok(())
    }
}

/// Whether `path` addresses a direct body child of the frame rooted at
/// `prefix`: exactly one extra frame, and that frame is a step or a call
/// (iteration instances and nested container children are not body
/// children).
fn direct_body_child(prefix: &RunPath, path: &RunPath) -> bool {
    path.len() == prefix.len() + 1
        && prefix.iter().zip(path.iter()).all(|(a, b)| same_site(a, b))
        && matches!(
            path.last(),
            Some(PathFrame::Step { .. }) | Some(PathFrame::Call { .. })
        )
}

/// Whether two run paths address the SAME step instance.
///
/// Hash-insensitive by way of [`same_site`], because a cross-IR resume
/// rewrites the flow and callee hashes of a path whose sites are unchanged:
/// a step whose span was opened by the crashed segment carries the OLD
/// flow's hashes, while the events the resume appends carry the new ones.
/// Both branches of the `verdictRecorded` arm use this one notion — if they
/// disagreed, a path could match neither and a legitimate verdict would
/// fold to `VerdictWithoutTarget`.
fn same_instance(a: &[PathFrame], b: &[PathFrame]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| same_site(x, y))
}

/// Site-wise path-frame identity: hash-insensitive (a cross-IR resume
/// changes the flow/callee hashes of the same site), position-sensitive.
fn same_site(a: &PathFrame, b: &PathFrame) -> bool {
    match (a, b) {
        (PathFrame::Flow { flow_id: a, .. }, PathFrame::Flow { flow_id: b, .. }) => a == b,
        (PathFrame::Step { step_id: a }, PathFrame::Step { step_id: b }) => a == b,
        (
            PathFrame::Call {
                step_id: a,
                callee_flow_id: af,
                ..
            },
            PathFrame::Call {
                step_id: b,
                callee_flow_id: bf,
                ..
            },
        ) => a == b && af == bf,
        (
            PathFrame::Iteration { index: a, key: ak },
            PathFrame::Iteration { index: b, key: bk },
        ) => a == b && ak == bk,
        (a, b) => a == b,
    }
}

/// Merges a judgment's localized manifest into a record's evidence:
/// dedup key (sha256, asset.id), first occurrence wins (item ③ — one
/// rule for checkpoint and dossier).
fn merge_evidence(evidence: &mut Vec<EvidenceRef>, localized: &[EvidenceRef]) {
    for entry in localized {
        let duplicate = evidence
            .iter()
            .any(|existing| existing.sha256 == entry.sha256 && existing.asset.id == entry.asset.id);
        if !duplicate {
            evidence.push(entry.clone());
        }
    }
}

/// Projects a four-way terminal into the durable [`AttemptRecord`]
/// (spine §6.6): discriminant + best-effort classification. The full
/// outcome stays in the `actionSettled` payload.
fn attempt_record(
    call_id: &str,
    outcome: &ActionOutcome,
    dispatch: DispatchIdentity,
) -> AttemptRecord {
    let kind = match outcome {
        ActionOutcome::Succeeded { .. } => ActionOutcomeKind::Succeeded,
        ActionOutcome::Failed { .. } => ActionOutcomeKind::Failed,
        ActionOutcome::Cancelled { .. } => ActionOutcomeKind::Cancelled,
        ActionOutcome::TimedOut { .. } => ActionOutcomeKind::TimedOut,
    };
    // Best-effort M0 classification: ErrorInfo.code is an open string the
    // provider adapter maps onto the closed ErrorClass; when the code
    // already spells a class verbatim we adopt it, otherwise None (a
    // dedicated carrier is M0-C).
    let error_class = match outcome {
        ActionOutcome::Succeeded { .. } => None,
        ActionOutcome::Failed { error }
        | ActionOutcome::Cancelled { error }
        | ActionOutcome::TimedOut { error } => parse_error_class(&error.code),
    };
    let (execution_mode, fallback_reason) = match outcome {
        ActionOutcome::Succeeded { result } => match &result.execution {
            Some(ActionExecution::NativeSemantic { .. }) => {
                (Some(ExecutionMode::NativeSemantic), None)
            }
            Some(ActionExecution::WebSemantic { .. }) => (Some(ExecutionMode::WebSemantic), None),
            Some(ActionExecution::CoordinateFallback {
                fallback_reason, ..
            }) => (
                Some(ExecutionMode::CoordinateFallback),
                Some(*fallback_reason),
            ),
            None => (None, None),
        },
        _ => (None, None),
    };
    let (chain_index, channel, action_name) = dispatch;
    AttemptRecord {
        call_id: call_id.to_owned(),
        outcome: kind,
        error_class,
        execution_mode,
        fallback_reason,
        chain_index,
        channel,
        action_name,
    }
}

fn parse_error_class(code: &str) -> Option<ErrorClass> {
    serde_json::from_value(Value::String(code.to_owned())).ok()
}

/// Projects a folded [`Verdict`] onto the durable per-step [`StepVerdict`]
/// (spine §6.6: summary/evidence stay in the `verdictRecorded` payload).
fn project_verdict(verdict: &Verdict) -> StepVerdict {
    StepVerdict {
        status: verdict.status,
        degraded: verdict.degraded,
        supersedes: verdict.supersedes.clone(),
    }
}

#[cfg(test)]
mod tests {
    use pointlock_ir::{BindingState, EventCursor, SupervisePolicy};
    use serde_json::json;

    use super::*;

    fn hash(fill: char) -> Hash {
        Hash::new(format!("sha256:{}", fill.to_string().repeat(64))).expect("valid hash")
    }

    fn meta() -> RunMeta {
        RunMeta {
            run_id: "run-1".to_owned(),
            flow_id: FlowId::new("checkout").expect("valid flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({"user": "alice"}),
            binding: BindingState {
                device_id: "dev-1".to_owned(),
                session_lineage: vec!["s-1".to_owned()],
                event_cursor: EventCursor {
                    session_id: "s-1".to_owned(),
                    last_sequence: 0,
                },
            },
            created_at_ms: 1,
        }
    }

    fn event(seq: u64, run_path: RunPath, payload: RunLogPayload) -> RunLogEvent {
        RunLogEvent {
            run_id: "run-1".to_owned(),
            seq,
            at_ms: 1_000 + seq,
            run_path,
            payload,
        }
    }

    fn run_started() -> RunLogPayload {
        RunLogPayload::RunStarted {
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({"user": "alice"}),
            supervise_policy: Some(SupervisePolicy::Mutating),
        }
    }

    #[test]
    fn zero_events_fold_to_the_seeded_pre_start_view() {
        let folded = fold_checkpoint(&meta(), &[]).expect("fold");
        assert!(folded.view.frames.is_empty());
        assert_eq!(folded.view.frontier.state, StepState::Pending);
        assert_eq!(folded.status, RunStatus::Running);
    }

    #[test]
    fn run_started_initializes_the_root_frame_from_meta_flow_id() {
        let folded = fold_checkpoint(&meta(), &[event(1, vec![], run_started())]).expect("fold");
        assert_eq!(folded.view.frames.len(), 1);
        assert_eq!(folded.view.frames[0].flow_id.as_str(), "checkout");
        assert_eq!(
            folded.view.frames[0].inputs_snapshot,
            json!({"user": "alice"})
        );
        assert_eq!(folded.view.frames[0].next_index, 0);
    }

    #[test]
    fn events_before_run_started_are_rejected() {
        let err = fold_checkpoint(
            &meta(),
            &[event(
                1,
                vec![],
                RunLogPayload::StepEntered {
                    step_id: StepId::new("login").expect("valid step id"),
                    effect_hash: hash('c'),
                    judge_hash: hash('d'),
                    resolved_inputs: json!({}),
                },
            )],
        )
        .expect_err("must reject");
        assert_eq!(
            err,
            FoldError::EventBeforeRunStarted {
                seq: 1,
                event_type: "stepEntered"
            }
        );
    }

    #[test]
    fn duplicate_run_started_is_rejected() {
        let err = fold_checkpoint(
            &meta(),
            &[
                event(1, vec![], run_started()),
                event(2, vec![], run_started()),
            ],
        )
        .expect_err("must reject");
        assert_eq!(err, FoldError::DuplicateRunStarted { seq: 2 });
    }

    #[test]
    fn non_monotonic_seq_is_rejected() {
        let err = fold_checkpoint(
            &meta(),
            &[
                event(1, vec![], run_started()),
                event(
                    1,
                    vec![],
                    RunLogPayload::RunSuspended {
                        provider_state_summary: None,
                        reason: None,
                    },
                ),
            ],
        )
        .expect_err("must reject");
        assert_eq!(err, FoldError::NonMonotonicSeq { prev: 1, seq: 1 });
    }

    #[test]
    fn step_exited_without_entry_is_rejected() {
        let err = fold_checkpoint(
            &meta(),
            &[
                event(1, vec![], run_started()),
                event(
                    2,
                    vec![],
                    RunLogPayload::StepExited {
                        provider_state_summary: None,
                        state: StepState::Judged,
                        output: None,
                        localized: Vec::new(),
                        localization_gaps: Vec::new(),
                    },
                ),
            ],
        )
        .expect_err("must reject");
        assert_eq!(err, FoldError::StepExitedWithoutEntry { seq: 2 });
    }

    #[test]
    fn step_record_fields_are_harvested_from_the_carrier_events() {
        let step_path: RunPath = vec![PathFrame::Step {
            step_id: StepId::new("login").expect("valid step id"),
        }];
        let folded = fold_checkpoint(
            &meta(),
            &[
                event(1, vec![], run_started()),
                event(
                    2,
                    step_path.clone(),
                    RunLogPayload::StepEntered {
                        step_id: StepId::new("login").expect("valid step id"),
                        effect_hash: hash('c'),
                        judge_hash: hash('d'),
                        resolved_inputs: json!({"element": {"identifier": "loginButton"}}),
                    },
                ),
                event(
                    3,
                    step_path.clone(),
                    RunLogPayload::StepExited {
                        provider_state_summary: None,
                        state: StepState::Judged,
                        output: Some(json!({"ok": true})),
                        localized: Vec::new(),
                        localization_gaps: Vec::new(),
                    },
                ),
            ],
        )
        .expect("fold");
        let record = &folded.view.completed[0];
        assert_eq!(record.effect_hash, hash('c'));
        assert_eq!(record.judge_hash, hash('d'));
        assert_eq!(
            record.resolved_inputs,
            json!({"element": {"identifier": "loginButton"}})
        );
        assert_eq!(record.output, Some(json!({"ok": true})));
    }

    #[test]
    fn popping_the_root_frame_is_rejected() {
        let err = fold_checkpoint(
            &meta(),
            &[
                event(1, vec![], run_started()),
                event(2, vec![], RunLogPayload::CallFramePopped { outputs: None }),
            ],
        )
        .expect_err("must reject");
        assert_eq!(err, FoldError::PoppedRootFrame { seq: 2 });
    }

    #[test]
    fn unpaired_human_response_is_rejected() {
        let err = fold_checkpoint(
            &meta(),
            &[
                event(1, vec![], run_started()),
                event(
                    2,
                    vec![],
                    RunLogPayload::HumanResponded {
                        request_id: "req-ghost".to_owned(),
                        purpose: pointlock_ir::HumanPurpose::Step,
                        response: json!({}),
                        actor: "cli:tester".to_owned(),
                    },
                ),
            ],
        )
        .expect_err("must reject");
        assert_eq!(
            err,
            FoldError::UnpairedHumanResponse {
                seq: 2,
                request_id: "req-ghost".to_owned()
            }
        );
    }

    fn human_requested(request_id: &str, purpose: HumanPurpose) -> RunLogPayload {
        RunLogPayload::HumanRequested {
            request_id: request_id.to_owned(),
            purpose,
            mode: match purpose {
                HumanPurpose::Step => Some(pointlock_ir::HumanMode::Confirm),
                HumanPurpose::Supervision => None,
            },
            prompt: "Decide".to_owned(),
            presents: json!([]),
            decisions: None,
            output_schema: None,
            deadline_at_ms: match purpose {
                HumanPurpose::Step => Some(9_000),
                HumanPurpose::Supervision => None,
            },
        }
    }

    #[test]
    fn supervision_suspend_answer_keeps_the_request_pending() {
        let step_path: RunPath = vec![PathFrame::Step {
            step_id: StepId::new("pay").expect("valid step id"),
        }];
        let folded = fold_checkpoint(
            &meta(),
            &[
                event(1, vec![], run_started()),
                event(
                    2,
                    step_path.clone(),
                    human_requested("req-1", HumanPurpose::Supervision),
                ),
                event(
                    3,
                    step_path.clone(),
                    RunLogPayload::HumanResponded {
                        request_id: "req-1".to_owned(),
                        purpose: HumanPurpose::Supervision,
                        response: json!({"decision": "suspend"}),
                        actor: "cli:tester".to_owned(),
                    },
                ),
                // The suspend ruling parks the run; the request survives.
                event(
                    4,
                    vec![],
                    RunLogPayload::RunSuspended {
                        provider_state_summary: None,
                        reason: None,
                    },
                ),
            ],
        )
        .expect("fold");
        let pending = folded.view.human_pending.expect("request stays pending");
        assert_eq!(pending.request_id, "req-1");
        assert_eq!(folded.status, RunStatus::AwaitingHuman);

        // A later final ruling still pairs and settles the wait.
        let folded = fold_checkpoint(
            &meta(),
            &[
                event(1, vec![], run_started()),
                event(
                    2,
                    step_path.clone(),
                    human_requested("req-1", HumanPurpose::Supervision),
                ),
                event(
                    3,
                    step_path.clone(),
                    RunLogPayload::HumanResponded {
                        request_id: "req-1".to_owned(),
                        purpose: HumanPurpose::Supervision,
                        response: json!({"decision": "suspend"}),
                        actor: "cli:tester".to_owned(),
                    },
                ),
                event(
                    4,
                    step_path,
                    RunLogPayload::HumanResponded {
                        request_id: "req-1".to_owned(),
                        purpose: HumanPurpose::Supervision,
                        response: json!({"decision": "proceed"}),
                        actor: "cli:tester".to_owned(),
                    },
                ),
            ],
        )
        .expect("fold");
        assert!(folded.view.human_pending.is_none());
        assert_eq!(folded.status, RunStatus::Running);
    }

    #[test]
    fn run_suspended_while_a_request_is_pending_stays_awaiting_human() {
        let step_path: RunPath = vec![PathFrame::Step {
            step_id: StepId::new("ask").expect("valid step id"),
        }];
        let folded = fold_checkpoint(
            &meta(),
            &[
                event(1, vec![], run_started()),
                event(2, step_path, human_requested("req-2", HumanPurpose::Step)),
                event(
                    3,
                    vec![],
                    RunLogPayload::RunSuspended {
                        provider_state_summary: None,
                        reason: None,
                    },
                ),
            ],
        )
        .expect("fold");
        assert_eq!(folded.status, RunStatus::AwaitingHuman);
        let pending = folded.view.human_pending.expect("pending");
        assert_eq!(pending.deadline_at_ms, Some(9_000));
        assert_eq!(pending.mode, Some(pointlock_ir::HumanMode::Confirm));
    }

    #[test]
    fn step_exit_settles_the_pending_request_without_a_response() {
        // The lazy timeout settlement shape: the awaiting step exits
        // (verdict unknown) with no humanResponded on the ledger.
        let step_path: RunPath = vec![PathFrame::Step {
            step_id: StepId::new("ask").expect("valid step id"),
        }];
        let folded = fold_checkpoint(
            &meta(),
            &[
                event(1, vec![], run_started()),
                event(
                    2,
                    step_path.clone(),
                    RunLogPayload::StepEntered {
                        step_id: StepId::new("ask").expect("valid step id"),
                        effect_hash: hash('c'),
                        judge_hash: hash('d'),
                        resolved_inputs: json!({"presents": []}),
                    },
                ),
                event(
                    3,
                    step_path.clone(),
                    human_requested("req-3", HumanPurpose::Step),
                ),
                event(
                    4,
                    vec![],
                    RunLogPayload::RunSuspended {
                        provider_state_summary: None,
                        reason: None,
                    },
                ),
                event(
                    5,
                    vec![],
                    RunLogPayload::RunResumed {
                        alignment_report: pointlock_ir::AlignmentReport {
                            entries: vec![],
                            resume_point: None,
                            requires_confirmation: vec![],
                        },
                        supervise_policy: None,
                        event_cursor: None,
                    },
                ),
                event(
                    6,
                    step_path,
                    RunLogPayload::StepExited {
                        provider_state_summary: None,
                        state: StepState::Judged,
                        output: None,
                        localized: Vec::new(),
                        localization_gaps: Vec::new(),
                    },
                ),
            ],
        )
        .expect("fold");
        assert!(folded.view.human_pending.is_none());
        assert_eq!(folded.status, RunStatus::Running);
    }

    #[test]
    fn error_class_is_adopted_only_when_the_code_spells_a_class() {
        assert_eq!(
            parse_error_class("action_failed_final"),
            Some(ErrorClass::ActionFailedFinal)
        );
        assert_eq!(parse_error_class("SOME_DAEMON_CODE"), None);
    }
}
