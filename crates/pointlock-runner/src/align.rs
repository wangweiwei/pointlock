//! Resume alignment (spine §6.7-A; 07 §5.2 subset) and the offline
//! re-judge of `judgeDirty` steps (07 §5.3).
//!
//! ## Scope of this module (M2)
//!
//! Cross-IR alignment classifies the whole 07 §5.2 nested vocabulary: `if`
//! branch bodies and `foreach` rounds are walked step by step, and a `call`
//! is descended into under the case-(a) down-drill rule ([`drillable`]).
//! Order consistency ([`order_inversion`]) rides along at every depth —
//! unlike the nested rules its absence would not refuse but silently adopt
//! a differently-ordered history. Same-IR resume — including frame-precise
//! resume into callees and foreach iterations — does not come through here
//! at all: it adopts completed instances by exact run path (see
//! `runner.rs`).
//!
//! ## Classification inputs
//!
//! The fold harvests every completed step's execution-time `effectHash` /
//! `judgeHash` from the `stepEntered` carrier (spine §6.1 M1 note), so
//! alignment compares the archived `StepRecord` hashes against the new IR
//! directly — cross-IR resume never REQUIRES the old FlowIR. Supplying
//! `ResumeOptions::old_flow_ir` unlocks the preflight-only sub-domain
//! comparison ([`preflight_only_change`], 07 §5.3): a `judgeDirty` whose
//! delta is preflight alone adopts outright. The implemented subset:
//! stepId matching, `reusable`
//! adoption, `judgeDirty` offline re-judge over the archived raw output
//! (harvested from `actionSettled` payloads — the log is the truth, I1)
//! against the *new* step's assertions, `effectDirty` rollback to the
//! earliest invalidated step (position-based, sequential flows), and
//! `new`/`orphaned` classification.

use std::collections::{BTreeMap, BTreeSet};

use pointlock_ir::{
    ActionOutcomeKind, ActionStepIR, AlignmentClass, AlignmentEntry, AlignmentReport, CallStepIR,
    CheckpointView, FlowIR, HandlerHook, Hash, PathFrame, PredicateIR, RequiresConfirmation,
    RunLogEvent, RunLogPayload, RunPath, StepIR, StepRecord, Verdict, VerdictPolicy, VerdictStatus,
    effect_hash,
};
use pointlock_store::Store;
use serde_json::Value;

use crate::engine::{
    Adopted, HumanRequestFact, attempt_of, child_frame, gated_mutating, instance_key, is_history,
    root_path,
};
use crate::error::RunnerError;
use crate::judge::{eval_expr_assertion, fold_step_verdict, project_output};
use crate::observe_eval::{
    EvaluatedAssertion, ObserveMaterial, eval_observed_assertion, material_from_observation,
};
use crate::scope::ScopeSeed;

/// The open call frames of a checkpoint: instance key → the callee `irHash`
/// the frame currently claims, read off its trailing `call` path frame
/// (07 §2.1: a call step's path frame IS the frame it opens).
pub(crate) fn live_frame_pins(facts: &Harvest) -> BTreeMap<String, Hash> {
    facts
        .live_frames
        .iter()
        .filter_map(|path| match path.last() {
            Some(PathFrame::Call { callee_ir_hash, .. }) => {
                Some((instance_key(path), callee_ir_hash.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Whether a `call`'s effect domain differs ONLY in the callee pin
/// (07 §5.2 "先辨因，后下钻" — establish the cause, then descend).
///
/// A call's `effectHash` fuses `{kind, flowRef, inputs}` into one digest
/// (02 §12.3) and `flowRef` carries the callee's `irHash`, so a digest that
/// moved cannot say by itself whether the callee moved, the arguments
/// moved, or both. Substituting the pin the old run actually entered and
/// recomputing separates them: equality proves `{kind, flowRef.flowId,
/// inputs}` are byte-identical and the entire difference is the callee's
/// content. The `flowId` is deliberately NOT substituted: re-pointing the
/// call at a different flow must move the digest, and it does.
///
/// The spec words this as comparing the old and new `CallStepIR.inputs`
/// canonical forms, which would need the old FlowIR. Substitution answers
/// the same question from the archive alone, and answers it slightly
/// stronger (a changed `flowId` fails it too), so cross-IR alignment keeps
/// its property of never requiring `--old-ir`.
///
/// The distinction is load-bearing, not cosmetic. A callee step's own
/// hashes cover the callee-side `params.*` expressions, never the caller's
/// argument source: change which value the caller passes and every hash
/// inside the callee stays byte-identical, so a descent would report
/// "nothing changed" about a body that now runs on different values. And
/// the live frame's `inputsSnapshot` is never re-evaluated for a new IR
/// (07 §5.2 corollary), so the remaining steps would keep using the old
/// values regardless. Both failure modes are silent; this test is what
/// makes the descent honest.
fn call_head_unchanged(call: &CallStepIR, archived_pin: &Hash, archived_effect: &Hash) -> bool {
    let mut probe = call.clone();
    probe.flow_ref.ir_hash = archived_pin.clone();
    // The effect domain reads `{kind, flowRef, inputs}` off the wire form;
    // the stored hash fields on the step are not part of it, so the probe
    // needs no resealing.
    effect_hash(&StepIR::Call(probe)) == *archived_effect
}

/// The callee body a `call` node may be descended into (07 §5.2 case (a)),
/// or `None` when this call is classified whole.
///
/// The archive is read from the LIVE FRAME STACK, not from a `StepRecord`,
/// because for the case this rule exists to serve there is no record to
/// read. The fold appends a `StepRecord` only on `stepExited`, and
/// `exec_call` returns without one whenever the callee suspends or blocks
/// ("suspension/blocking leaves the frame live"). So an unfinished call —
/// precisely 07 §5.2's 「该 call step 在旧 run 中尚未完成（崩在 callee
/// 内部）」 — contributes an open frame and a `stepEntered`, and nothing
/// else. Both conditions therefore come from [`Harvest`]:
/// - the frame is still open, which IS the "not completed" precondition.
///   A concluded call is case (c) and is re-called whole whatever the
///   cause; a frame that was popped without a `stepExited` (a crash in
///   that window) is not open either, and is not descended into;
/// - its `stepEntered` archived the execution-time `effectHash`
///   (`exec_call` enters the step *before* it pushes the frame, so a live
///   frame always has one), which [`call_head_unchanged`] tests against.
///
/// A run can hold BOTH a record and an open frame for one instance — an
/// earlier segment completed the call, a later one re-executed it and
/// suspended inside. The open frame is then the truth and the record is
/// the abandoned execution's; liveness wins, and the stale record is never
/// adopted (see the `descended` arms in [`align`]).
fn drillable<'a>(
    call: &CallStepIR,
    key: &str,
    live_pins: &BTreeMap<String, Hash>,
    facts: &Harvest,
    loaded: &crate::load::LoadedFlow<'a>,
) -> Option<&'a FlowIR> {
    let archived_pin = live_pins.get(key)?;
    let archived_effect = facts.entered_effect_hash.get(key)?;
    if !call_head_unchanged(call, archived_pin, archived_effect) {
        return None;
    }
    loaded.try_callee(&call.flow_ref)
}

/// Resolves the OLD IR's step at an archived instance path — root body,
/// `if` branches and `foreach` rounds only. A `call` (or hook) frame stops
/// the walk: the old CALLEE registry is not supplied, and guessing at a
/// callee's old shape is exactly what this returns `None` instead of.
fn old_step_at<'a>(old_root: &'a FlowIR, path: &RunPath) -> Option<&'a StepIR> {
    let mut bodies: Vec<&'a [StepIR]> = vec![&old_root.body];
    let mut current: Option<&'a StepIR> = None;
    for frame in path {
        match frame {
            PathFrame::Flow { .. } | PathFrame::Iteration { .. } => {}
            PathFrame::Attempt { .. } | PathFrame::Phase { .. } | PathFrame::Assertion { .. } => {
                break;
            }
            PathFrame::Hook { .. } | PathFrame::Call { .. } => return None,
            PathFrame::Step { step_id } => {
                let step = bodies
                    .iter()
                    .flat_map(|body| body.iter())
                    .find(|step| step.step_id() == step_id)?;
                bodies = match step {
                    StepIR::If(branch) => {
                        let mut inner: Vec<&'a [StepIR]> = vec![&branch.then];
                        if let Some(otherwise) = &branch.r#else {
                            inner.push(otherwise);
                        }
                        inner
                    }
                    StepIR::Foreach(each) => vec![&each.body],
                    _ => Vec::new(),
                };
                current = Some(step);
            }
        }
    }
    current
}

/// 07 §5.3 / 02 §12.3 ruling 6: whether a `judgeDirty` step's judge-domain
/// delta is PREFLIGHT-ONLY — the archived verdict then judged exactly the
/// question the new IR asks, and adoption is the prescribed disposition
/// (「该步按 reusable 采认……reason 标注 preflightChanged」).
///
/// The fused `judgeHash` cannot say this by itself; the OLD step supplies
/// the other half. Two integrity gates before believing it:
/// - the old step at this path must HASH to the archived `judgeHash` — it
///   is then provably the step this record executed (the whole-IR
///   `OldIrMismatch` check upstream covers the root, this covers the walk);
/// - kinds must match (a kind change is never preflight-only).
fn preflight_only_change(old_root: Option<&FlowIR>, record: &StepRecord, step: &StepIR) -> bool {
    let Some(old_step) = old_root.and_then(|old| old_step_at(old, &record.run_path)) else {
        return false;
    };
    old_step.kind() == step.kind()
        && pointlock_ir::judge_hash(old_step) == record.judge_hash
        && pointlock_ir::judge_subdomain_sans_preflight(old_step)
            == pointlock_ir::judge_subdomain_sans_preflight(step)
}

/// The callee pin a `call` step names, for reporting whether a down-drilled
/// frame was rebased or merely re-entered. Never called on another kind.
fn call_pin(step: &StepIR) -> Hash {
    match step {
        StepIR::Call(call) => call.flow_ref.ir_hash.clone(),
        other => unreachable!("only a call node is ever drilled, got {}", other.kind()),
    }
}

/// Whether the instance keyed `inner` sits strictly inside the one keyed
/// `outer`. Instance keys, not paths: a record archived under the old IR
/// and a node of the new one describe the same site with different flow
/// hashes, and [`instance_key`] is exactly the rendering that drops them.
pub(crate) fn is_instance_descendant(outer: &str, inner: &str) -> bool {
    inner.len() > outer.len()
        && inner.starts_with(outer)
        // A key segment starts with `/` (step or call) or `[` (iteration),
        // so the boundary check keeps `/loginTwice` out of `/login`.
        && matches!(inner.as_bytes()[outer.len()], b'/' | b'[')
}

/// One node of the flattened new-IR walk: the instance path it would take
/// and the step there, containers before their children.
pub(crate) struct Node<'a> {
    /// The instance path in the NEW flow.
    pub path: RunPath,
    /// The step at that position.
    pub step: &'a StepIR,
}

/// Flattens a body into traversal order, descending into containers whose
/// control decision the archive still vouches for (07 §5.2).
///
/// A container's `effectHash` covers only its HEAD — an `if`'s `cond`, a
/// `foreach`'s `{items, as}` — never its body. So equal hashes prove the
/// same branch/rounds will be chosen, and the body must be walked to see
/// whether anything inside changed; that walk is the whole reason nested
/// alignment exists. Descent stops when the archive cannot vouch for the
/// decision:
/// - no record → the container never ran, so nothing inside it did either;
/// - `effectHash` differs → the head changed, so the old decision says
///   nothing about the new one and the whole subtree re-executes.
///
/// Which branch an `if` took is read from its archived `resolvedInputs`
/// (`{"cond": bool}`) — the decision as it actually happened, never
/// re-derived (I3).
///
/// A `call` descends under its own rule ([`drillable`]) and records the
/// fact in `descended`: its head is `{flowId, inputs}` and its body is the
/// callee, so the pin has to be held constant before the head can be
/// compared at all.
#[allow(clippy::too_many_arguments)]
fn flatten<'a>(
    loaded: &crate::load::LoadedFlow<'a>,
    facts: &Harvest,
    live_pins: &BTreeMap<String, Hash>,
    prefix: &RunPath,
    body: &'a [StepIR],
    completed: &BTreeMap<String, &StepRecord>,
    descended: &mut BTreeSet<String>,
    out: &mut Vec<Node<'a>>,
) {
    for step in body {
        let mut path = prefix.clone();
        path.push(child_frame(step));
        let key = instance_key(&path);
        let record = completed.get(&key).copied();
        out.push(Node {
            path: path.clone(),
            step,
        });
        if let StepIR::Call(call) = step {
            // A `call` reads its archive from the live frame stack, not
            // from a record — an unfinished call has none ([`drillable`]).
            // A plain `effectHash` comparison would be the wrong gate here
            // anyway: it fuses the callee pin with the arguments, so it
            // refuses the one case that may descend and has nothing to say
            // about the ones that may not.
            if let Some(callee) = drillable(call, &key, live_pins, facts, loaded) {
                descended.insert(key);
                flatten(
                    loaded,
                    facts,
                    live_pins,
                    &path,
                    &callee.body,
                    completed,
                    descended,
                    out,
                );
            }
            continue;
        }
        // Descent is only licensed when the archive still vouches for the
        // container's own decision: no record means nothing inside it ran,
        // and a changed head means the old decision says nothing about the
        // new one.
        let Some(record) = record else { continue };
        match step {
            StepIR::If(branch) => {
                if record.effect_hash != step.base().effect_hash {
                    continue;
                }
                // Which branch ran is read from the archived decision, never
                // re-derived (I3).
                let Some(taken) = record.resolved_inputs.get("cond").and_then(Value::as_bool)
                else {
                    continue;
                };
                let selected: &'a [StepIR] = if taken {
                    &branch.then
                } else {
                    branch.r#else.as_deref().unwrap_or(&[])
                };
                descended.insert(key);
                flatten(
                    loaded, facts, live_pins, &path, selected, completed, descended, out,
                );
            }
            StepIR::Foreach(each) => {
                if record.effect_hash != step.base().effect_hash {
                    continue;
                }
                // Rounds align by index (v0.1 positional; `key` reserved).
                // The round COUNT comes from the archived items snapshot —
                // the same source the engine adopts from — never from
                // re-evaluating the `in` expression. A snapshot that is not
                // an array is not a round count of zero: it is a snapshot
                // this walk cannot read, so the foreach is left un-descended
                // and answers for its body as a whole (`gated_effect`)
                // rather than silently exempting it.
                let Some(rounds) = record
                    .resolved_inputs
                    .get("items")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                else {
                    continue;
                };
                descended.insert(key);
                for index in 0..rounds {
                    let mut round = path.clone();
                    round.push(PathFrame::Iteration {
                        index: index as u64,
                        key: None,
                    });
                    flatten(
                        loaded, facts, live_pins, &round, &each.body, completed, descended, out,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Whether re-running this step could repeat a non-idempotent effect
/// (07 §5.4 side-effect criterion).
///
/// An action answers for itself. A CONTAINER — `if`, `foreach`, `call` —
/// answers for its body exactly when the walk did not go inside it:
///
/// - descended: its body steps are nodes of their own and gate one by one,
///   which is both more precise (only the steps that actually re-execute
///   are named) and complete;
/// - not descended: the archive could not vouch for the container's own
///   decision — its head changed — so the walk stopped at it and the body
///   is INVISIBLE to the classifier. Re-executing the container re-runs
///   whatever ran in there, and gating the container as a whole is then the
///   only statement that covers those steps. Without this the §5.4 gate has
///   a hole exactly where the author edited something: change an `if`'s
///   `cond` and its branch replays every already-effective mutating step
///   with no confirmation asked (07 §5.2 位置失效 / I2 (ii)).
///
/// A callee the registry cannot resolve is treated as mutating: fail-closed
/// beats guessing that an unknown flow is harmless.
fn gated_effect(step: &StepIR, descended: bool, loaded: &crate::load::LoadedFlow<'_>) -> bool {
    fn body_mutates(body: &[StepIR], loaded: &crate::load::LoadedFlow<'_>, depth: usize) -> bool {
        if depth > 32 {
            return true; // pathological nesting: refuse to conclude "safe"
        }
        body.iter().any(|step| match step {
            StepIR::Action(action) => gated_mutating(action),
            StepIR::If(branch) => {
                body_mutates(&branch.then, loaded, depth + 1)
                    || branch
                        .r#else
                        .as_deref()
                        .is_some_and(|otherwise| body_mutates(otherwise, loaded, depth + 1))
            }
            StepIR::Foreach(each) => body_mutates(&each.body, loaded, depth + 1),
            StepIR::Call(call) => match loaded.try_callee(&call.flow_ref) {
                Some(callee) => body_mutates(&callee.body, loaded, depth + 1),
                None => true,
            },
            _ => false,
        })
    }
    match step {
        StepIR::Action(action) => gated_mutating(action),
        StepIR::If(_) | StepIR::Foreach(_) | StepIR::Call(_) if descended => false,
        StepIR::If(branch) => {
            body_mutates(&branch.then, loaded, 0)
                || branch
                    .r#else
                    .as_deref()
                    .is_some_and(|otherwise| body_mutates(otherwise, loaded, 0))
        }
        StepIR::Foreach(each) => body_mutates(&each.body, loaded, 0),
        StepIR::Call(call) => match loaded.try_callee(&call.flow_ref) {
            Some(callee) => body_mutates(&callee.body, loaded, 0),
            None => true,
        },
        _ => false,
    }
}

/// Whether this step encloses a body the classifier may or may not have
/// walked into — the three kinds [`gated_effect`] answers for as a whole
/// when the walk stopped at them.
fn is_container(step: &StepIR) -> bool {
    matches!(step, StepIR::If(_) | StepIR::Foreach(_) | StepIR::Call(_))
}

/// Whether `inner` sits strictly inside `outer`.
fn is_descendant(outer: &RunPath, inner: &RunPath) -> bool {
    inner.len() > outer.len() && inner.starts_with(outer)
}

/// The trigger-counter key of one hook on one step instance. The
/// `handlerTriggered` event anchors at the *host* step path (the hook
/// frame is reserved for the disposition's child work), so the counter
/// key is host-instance + wire hook name.
pub(crate) fn hook_trigger_key(instance: &str, hook: HandlerHook) -> String {
    let wire = serde_json::to_value(hook).expect("HandlerHook serializes");
    format!(
        "{instance}|{}",
        wire.as_str().expect("HandlerHook is a string literal")
    )
}

/// Facts harvested from the raw RunLog (the log is the truth, I1). Keys
/// are step-*instance* keys ([`instance_key`]) — IR-version-independent,
/// unique per instance (iteration rounds and callee steps included).
/// The issuing-generation state of a pending intent (07 §4.5,
/// incorporated 2026-07-18): which session generation dispatched it.
/// Computed per intent while scanning — exact, not sticky: an intent
/// dispatched after a cursor-BEARING resume is credentialed by that
/// resume's cursor regardless of earlier cursor-less generations (the
/// segment that dispatched it is self-identifying). Only intents from a
/// cursor-less (pre-incorporation) generation are `Unknown`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum IssuingCursor {
    /// No resume preceded the intent: the bind-time binding cursor is
    /// the credential (single-generation by observation).
    FromBinding,
    /// The last preceding cursor-bearing resume's cursor.
    Known(pointlock_ir::EventCursor),
    /// A cursor-less resume preceded the intent: the issuing session is
    /// unknowable — never fabricated (principle 4).
    Unknown,
}

pub(crate) struct Harvest {
    /// callId → the issuing-generation state at dispatch time.
    pub intent_issuing: BTreeMap<String, IssuingCursor>,
    /// callId → the intent's recorded 1-based chain position (item ②);
    /// absent for pre-incorporation intents.
    pub intent_chain_index: BTreeMap<String, u32>,
    /// instance → the last succeeded `ActionResult.output` (raw).
    pub raw_output: BTreeMap<String, Value>,
    /// instance → the last succeeded terminal's after-observation id.
    pub after_observation: BTreeMap<String, String>,
    /// instance → the last succeeded terminal's before-observation id.
    pub before_observation: BTreeMap<String, String>,
    /// instance → highest attempt number seen on an `actionIntent` path.
    pub max_attempt: BTreeMap<String, u64>,
    /// instance → seq of the last `verdictRecorded` event.
    pub verdict_seq: BTreeMap<String, u64>,
    /// callId → the `actionIntent` event's run path.
    pub intent_path: BTreeMap<String, RunPath>,
    /// instance → the last `stepEntered`'s execution-time `effectHash`
    /// (frontier effect-dirty check, spine §6.1 M1 carrier).
    pub entered_effect_hash: BTreeMap<String, Hash>,
    /// instance → the last `stepEntered`'s frozen ready snapshot (the
    /// control-value carrier for containers on a frame-precise resume:
    /// cond / items / call inputs are never re-evaluated — I3).
    pub entered_inputs: BTreeMap<String, Value>,
    /// instance → the seq of its `stepEntered` event: the old run's
    /// *execution order*, which the 07 §5.2 order-consistency check
    /// compares against the new IR's traversal order.
    pub entered_seq: BTreeMap<String, u64>,
    /// Step spans left open by a crash/suspension (stepEntered without
    /// stepExited), outermost first.
    pub open_spans: Vec<RunPath>,
    /// Call frames pushed and not popped (live across the suspension):
    /// resume must not push them again.
    pub live_frames: Vec<RunPath>,
    /// instance → the human request anchored there (human step waits and
    /// R13 supervision gates), with its arbitrated final response when
    /// one is on the ledger. A supervision `suspend` answer is non-final
    /// and never fills `final_response` (spine §6.9).
    pub human_requests: BTreeMap<String, HumanRequestFact>,
    /// instance → the last recorded terminal for that step instance,
    /// whatever its four-way discriminant. Lets a resumed segment re-enter
    /// an open action span whose act already settled (a handler-wave
    /// suspension) without ever re-dispatching (I2).
    pub settled: BTreeMap<String, SettledFact>,
    /// instance → the last recorded verdict's (status, degraded) — the
    /// re-entry point for handler consultation on resume.
    pub recorded_verdicts: BTreeMap<String, (VerdictStatus, bool)>,
    /// "{instance}|{hook}" → highest handler trigger count on the ledger
    /// (maxTriggers continues across segments, never resets).
    pub hook_triggers: BTreeMap<String, u64>,
}

/// The harvested fact of one settled action terminal (any discriminant).
/// (Only the terminal itself is consumed today; the attempt number and
/// event seq are recoverable from the ledger when a consumer needs them.)
#[derive(Debug, Clone)]
pub(crate) struct SettledFact {
    /// The full four-way terminal.
    pub outcome: pointlock_ir::ActionOutcome,
}

/// Walks the ordered event log once and extracts the resume facts.
pub(crate) fn harvest(events: &[RunLogEvent]) -> Harvest {
    let mut facts = Harvest {
        intent_issuing: BTreeMap::new(),
        intent_chain_index: BTreeMap::new(),
        raw_output: BTreeMap::new(),
        after_observation: BTreeMap::new(),
        before_observation: BTreeMap::new(),
        max_attempt: BTreeMap::new(),
        verdict_seq: BTreeMap::new(),
        intent_path: BTreeMap::new(),
        entered_effect_hash: BTreeMap::new(),
        entered_inputs: BTreeMap::new(),
        entered_seq: BTreeMap::new(),
        open_spans: Vec::new(),
        live_frames: Vec::new(),
        human_requests: BTreeMap::new(),
        settled: BTreeMap::new(),
        recorded_verdicts: BTreeMap::new(),
        hook_triggers: BTreeMap::new(),
    };
    // requestId → instance key, for pairing responses to their request.
    let mut request_index: BTreeMap<String, String> = BTreeMap::new();
    // The running issuing-generation state (07 §4.5): FromBinding until a
    // resume intervenes; a cursor-bearing resume names its generation, a
    // cursor-less one makes subsequent intents Unknown.
    let mut issuing = IssuingCursor::FromBinding;
    for event in events {
        match &event.payload {
            RunLogPayload::StepEntered {
                effect_hash,
                resolved_inputs,
                ..
            } => {
                let key = instance_key(&event.run_path);
                facts.open_spans.push(event.run_path.clone());
                facts
                    .entered_effect_hash
                    .insert(key.clone(), effect_hash.clone());
                facts
                    .entered_inputs
                    .insert(key.clone(), resolved_inputs.clone());
                // First entry wins: a re-entered span (handler wave) must
                // not restamp the step's position in the original order.
                facts.entered_seq.entry(key).or_insert(event.seq);
            }
            RunLogPayload::StepExited { .. } => {
                facts.open_spans.pop();
            }
            RunLogPayload::CallFramePushed { rebase, .. } => {
                // A rebase RE-ENTERS an already-open frame (07 §5.2 case
                // (a)); it opens none, so the live-frame stack is unchanged.
                if !rebase {
                    facts.live_frames.push(event.run_path.clone());
                }
            }
            RunLogPayload::CallFramePopped { .. } => {
                facts.live_frames.pop();
            }
            RunLogPayload::RunResumed { event_cursor, .. } => {
                issuing = match event_cursor {
                    Some(cursor) => IssuingCursor::Known(cursor.clone()),
                    None => IssuingCursor::Unknown,
                };
            }
            RunLogPayload::ActionIntent {
                call_id,
                chain_index,
                ..
            } => {
                if let Some(index) = chain_index {
                    facts.intent_chain_index.insert(call_id.clone(), *index);
                }
                facts
                    .intent_issuing
                    .insert(call_id.clone(), issuing.clone());
                facts
                    .intent_path
                    .insert(call_id.clone(), event.run_path.clone());
                let key = instance_key(&event.run_path);
                let n = attempt_of(&event.run_path).unwrap_or(1);
                let entry = facts.max_attempt.entry(key).or_insert(0);
                *entry = (*entry).max(n);
            }
            RunLogPayload::ActionSettled { outcome, .. } => {
                let key = instance_key(&event.run_path);
                facts.settled.insert(
                    key.clone(),
                    SettledFact {
                        outcome: outcome.clone(),
                    },
                );
                let pointlock_ir::ActionOutcome::Succeeded { result } = outcome else {
                    continue;
                };
                facts.raw_output.insert(key.clone(), result.output.clone());
                match &result.after {
                    Some(after) => {
                        facts
                            .after_observation
                            .insert(key.clone(), after.id.clone());
                    }
                    None => {
                        facts.after_observation.remove(&key);
                    }
                }
                match &result.before {
                    Some(before) => {
                        facts.before_observation.insert(key, before.id.clone());
                    }
                    None => {
                        facts.before_observation.remove(&key);
                    }
                }
            }
            RunLogPayload::VerdictRecorded { verdict, .. } => {
                let key = instance_key(&event.run_path);
                facts.verdict_seq.insert(key.clone(), event.seq);
                facts
                    .recorded_verdicts
                    .insert(key, (verdict.status, verdict.degraded));
            }
            RunLogPayload::HandlerTriggered { hook, trigger, .. } => {
                let key = hook_trigger_key(&instance_key(&event.run_path), *hook);
                let entry = facts.hook_triggers.entry(key).or_insert(0);
                *entry = (*entry).max(*trigger);
            }
            RunLogPayload::HumanRequested {
                request_id,
                purpose,
                mode,
                prompt,
                presents,
                decisions: _,
                output_schema: _,
                deadline_at_ms,
            } => {
                let key = instance_key(&event.run_path);
                request_index.insert(request_id.clone(), key.clone());
                facts.human_requests.insert(
                    key,
                    HumanRequestFact {
                        request_id: request_id.clone(),
                        run_path: event.run_path.clone(),
                        purpose: *purpose,
                        mode: *mode,
                        prompt: prompt.clone(),
                        presents: presents.clone(),
                        deadline_at_ms: *deadline_at_ms,
                        final_response: None,
                        final_actor: None,
                    },
                );
            }
            RunLogPayload::HumanResponded {
                request_id,
                purpose,
                response,
                actor,
            } => {
                // A supervision `suspend` ruling is non-final: the
                // request stays open for a later proceed/abort
                // (spine §6.9); only final responses settle.
                let is_final = !(*purpose == pointlock_ir::HumanPurpose::Supervision
                    && response.get("decision").and_then(Value::as_str) == Some("suspend"));
                if is_final
                    && let Some(key) = request_index.get(request_id)
                    && let Some(fact) = facts.human_requests.get_mut(key)
                    && fact.request_id == *request_id
                {
                    fact.final_response = Some(response.clone());
                    fact.final_actor = Some(actor.clone());
                }
            }
            _ => {}
        }
    }
    facts
}

/// A `judgeDirty` step re-judged offline: the new verdict (with
/// `supersedes` lineage) to be appended at the *old* record's run path so
/// the fold re-projects the completed record.
pub(crate) struct Rejudged {
    /// The old record's run path (anchors the `verdictRecorded` event).
    pub run_path: RunPath,
    /// The re-folded verdict, `supersedes` pointing at the superseded
    /// verdict event (`seq:<n>` — verdicts carry no id of their own;
    /// pending incorporation).
    pub verdict: Verdict,
}

/// The alignment result: the report (goes into `runResumed`), the resume
/// point, the offline re-judgements to record, and the adoption set.
pub(crate) struct Alignment {
    /// The report recorded in `runResumed` (spine §6.7-A).
    pub report: AlignmentReport,
    /// Instance key of the first step that must re-execute, when one
    /// exists — the resume point. A key rather than a body index: a body
    /// index cannot name a position inside a callee or an iteration, and
    /// the engine has nowhere to put one (`exec_call`/`exec_if`/
    /// `exec_foreach` all re-enter their body at 0).
    pub resume_key: Option<String>,
    /// Offline re-judgements to append (`verdictRecorded` with
    /// `supersedes`).
    pub rejudged: Vec<Rejudged>,
    /// The instances this resume adopts, keyed by [`instance_key`] — the
    /// same shape same-IR resume builds, and the mechanism that skips
    /// already-concluded work at ANY depth. Execution always restarts at
    /// the top of the body and the adoption set does the skipping; nothing
    /// after the resume point is in here.
    ///
    /// A successfully re-judged step carries its NEW verdict on the
    /// adopted record (07 §5.3), so the engine seeds the superseding
    /// judgment rather than the one the old run recorded.
    pub adoptable: BTreeMap<String, Adopted>,
    /// Instance key of the live call frame this resume TEARS DOWN before
    /// executing (07 §5.2 case (b)): the caller's arguments changed while
    /// the callee frame was open, so the frame — its open spans and its
    /// stack level — is closed on the ledger (mirroring `exec_call`'s
    /// abort unwind) and the call re-executes as a fresh instance with
    /// `inputs` re-evaluated under the new IR. `None` on every resume
    /// that dismantles nothing.
    pub teardown: Option<String>,
}

/// Aligns the new IR against the checkpoint (07 §5.2 flat subset).
///
/// Classification compares the archived execution-time hashes on each
/// [`StepRecord`] against the new IR's per-step hashes. Fails closed with
/// [`RunnerError::RequiresConfirmation`] when re-execution from the resume
/// point would re-run an already-effective non-idempotent mutating step
/// (07 §5.4 unified gate; I2 (i)/(ii)).
///
/// `store` provides read access to the localized evidence area: the
/// offline re-judge of element predicates replays the verify chain over
/// the archived tree bytes — zero device I/O (async only because the
/// chain-evaluation entry point is; no session is ever touched here).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn align(
    loaded: &crate::load::LoadedFlow<'_>,
    body: &[StepIR],
    view: &CheckpointView,
    facts: &Harvest,
    seed: &ScopeSeed,
    store: &Store,
    vision: Option<&dyn pointlock_vision::VisionVerifier>,
    authorized: &[String],
    forced: &[String],
    old_root: Option<&FlowIR>,
) -> Result<Alignment, RunnerError> {
    let new_flow = loaded.root;
    let policy = new_flow.verdict_policy;
    let completed = completed_by_instance(view);
    // The new IR in traversal order, containers before their children.
    let mut nodes = Vec::new();
    let mut descended = BTreeSet::new();
    let live_pins = live_frame_pins(facts);
    flatten(
        loaded,
        facts,
        &live_pins,
        &root_path(new_flow),
        body,
        &completed,
        &mut descended,
        &mut nodes,
    );
    // Order consistency gates adoption itself (07 §5.2), so it is settled
    // before a single record is adopted.
    let inversion = order_inversion(&nodes, &completed, facts);

    let mut entries = Vec::new();
    let mut rejudged = Vec::new();
    let mut outputs: BTreeMap<String, Value> = BTreeMap::new();
    let mut verdicts: BTreeMap<String, (VerdictStatus, bool)> = BTreeMap::new();
    let mut adoptable: BTreeMap<String, Adopted> = BTreeMap::new();
    let mut first_dirty: Option<usize> = None;
    // The one live call frame this resume dismantles (07 §5.2 case (b)),
    // when alignment finds one. At most one exists: open frames form the
    // suspension chain, and the walk stops at the OUTERMOST call it cannot
    // descend — nothing below it is a node.
    let mut teardown: Option<String> = None;

    for (index, node) in nodes.iter().enumerate() {
        let step = node.step;
        let step_id = step.step_id().clone();
        let key = instance_key(&node.path);
        if matches!(step, StepIR::Call(_))
            && live_pins.contains_key(&key)
            && !descended.contains(&key)
        {
            // 07 §5.2 case (b): the frame is still open but the walk could
            // not descend — the fused head moved in a way the pin
            // substitution does not explain, i.e. the ARGUMENTS (or the
            // flow target) changed. The only honest disposition is the
            // spec's: tear the stale frame down and re-call the callee
            // under the new IR, with `inputs` re-evaluated and re-
            // snapshotted — the archived `inputsSnapshot` holds the OLD
            // values and is never silently reused. Nothing under the old
            // frame is adopted; the resume point is the call itself
            // (「resume 点不得晚于该 call step」).
            entries.push(AlignmentEntry {
                run_path: node.path.clone(),
                step_id,
                class: AlignmentClass::EffectDirty,
                reason: Some(
                    "call arguments changed while the callee frame was still open; the stale \
                     frame is torn down and the callee re-called with freshly evaluated \
                     inputs (07 §5.2 case (b))"
                        .to_owned(),
                ),
            });
            first_dirty.get_or_insert(index);
            teardown = Some(key);
            continue;
        }
        if matches!(step, StepIR::Call(_)) && descended.contains(&key) {
            // A down-drilled `call` is a CONTAINER, not a leaf. Its frame is
            // still open ([`drillable`] requires it), so it is RE-ENTERED,
            // never adopted — adopting it would make the engine skip the
            // very frame the repair lives in. It does not fix the resume
            // point either: 07 §5.2 puts that inside the callee ("resume 点
            // 可以落在 callee 内部"), at the first step in there the archive
            // cannot vouch for. Nothing in the caller's body after the call
            // can be adopted past it regardless — a call that never
            // completed is a call nothing after it ever ran.
            //
            // An unfinished call usually has NO record at all; when it does
            // (an earlier segment completed it, a later one re-executed it
            // and suspended inside), that record belongs to the abandoned
            // execution and is deliberately left un-adopted.
            let pin_moved = live_pins.get(&key) != Some(&call_pin(step));
            entries.push(AlignmentEntry {
                run_path: node.path.clone(),
                step_id,
                class: AlignmentClass::EffectDirty,
                reason: Some(if pin_moved {
                    "callee content changed, arguments untouched; the live callee frame is \
                     re-entered and aligned step by step (07 §5.2 case (a) down-drill)"
                        .to_owned()
                } else {
                    "unchanged call, frame still open; re-entered and aligned step by step"
                        .to_owned()
                }),
            });
            continue;
        }
        let Some(record) = completed.get(&key) else {
            // Not executed in the old run (spine §6.7-A `new`; also the
            // "new-relative" case of 07 §5.5).
            entries.push(AlignmentEntry {
                run_path: node.path.clone(),
                step_id,
                class: AlignmentClass::New,
                reason: Some("no adoptable prior record".to_owned()),
            });
            first_dirty.get_or_insert(index);
            continue;
        };
        // The record archives its execution-time hashes (harvested from
        // the stepEntered carrier); compare them against the new IR
        // directly. Same-irHash resumes trivially compare equal.
        let class = {
            let effect_same = record.effect_hash == step.base().effect_hash;
            let judge_same = record.judge_hash == step.base().judge_hash;
            match (effect_same, judge_same) {
                (true, true) => AlignmentClass::Reusable,
                (true, false) => AlignmentClass::JudgeDirty,
                (false, _) => AlignmentClass::EffectDirty,
            }
        };
        // From the inversion onward the hashes agreeing proves nothing:
        // those records belong to a differently-ordered history. Demote to
        // `effectDirty` so they re-execute (07 §5.2).
        let reordered = inversion.is_some_and(|(from, _, _)| index >= from);
        // 07 §5.3: a step the author names in `--force-reexecute` upgrades
        // to `effectDirty` whatever its hashes say — the escape hatch for a
        // re-judge stuck at `unknown` on missing archive material, or an
        // adopted result the author no longer trusts. Classification only:
        // the §5.4 gate below still applies in full.
        let forced_hit = forced.iter().any(|name| name == step_id.as_str());
        let class = if reordered || forced_hit {
            AlignmentClass::EffectDirty
        } else {
            class
        };
        match class {
            AlignmentClass::Reusable => {
                let reason = if first_dirty.is_some() {
                    // Positionally invalidated: adopted record discarded,
                    // step re-executes after the resume point (07 §5.2).
                    Some("positionally invalidated: after the resume point".to_owned())
                } else {
                    adopt(
                        step,
                        &key,
                        record,
                        facts,
                        seed,
                        &mut outputs,
                        &mut verdicts,
                        &mut adoptable,
                        None,
                    );
                    None
                };
                entries.push(AlignmentEntry {
                    run_path: record.run_path.clone(),
                    step_id,
                    class,
                    reason,
                });
            }
            AlignmentClass::JudgeDirty => {
                if first_dirty.is_some() {
                    entries.push(AlignmentEntry {
                        run_path: record.run_path.clone(),
                        step_id,
                        class,
                        reason: Some(
                            "judgeHash changed; re-executes after the resume point".to_owned(),
                        ),
                    });
                    continue;
                }
                // 07 §5.3's sub-domain split, when the old IR is at hand:
                // a preflight-only judge change adopts outright — probes
                // run BEFORE the act, so the archived verdict judged the
                // very question the new IR asks. This subsumes the guards
                // below for that case: an assert step or a foreach-round
                // action whose only change is preflight no longer
                // re-executes for want of a re-judge surface.
                if preflight_only_change(old_root, record, step) {
                    adopt(
                        step,
                        &key,
                        record,
                        facts,
                        seed,
                        &mut outputs,
                        &mut verdicts,
                        &mut adoptable,
                        None,
                    );
                    entries.push(AlignmentEntry {
                        run_path: record.run_path.clone(),
                        step_id,
                        class,
                        reason: Some("preflightChanged".to_owned()),
                    });
                    continue;
                }
                // Offline re-judge (07 §5.3): new assertions over the
                // archived raw output and the localized observation
                // material — pure, zero device I/O.
                // An offline re-judge inside a `foreach` round would run
                // against the WRONG scope: `ScopeSeed` binds params / env /
                // steps only, while the live engine also binds `iter.<as>`
                // and `vars.*`. A verdict computed without those would be
                // wrong, and it would `supersede` a real one — so inside an
                // iteration the step re-executes instead. Re-judging there
                // needs a frame-local scope first.
                if node
                    .path
                    .iter()
                    .any(|frame| matches!(frame, PathFrame::Iteration { .. }))
                {
                    entries.push(AlignmentEntry {
                        run_path: record.run_path.clone(),
                        step_id,
                        class: AlignmentClass::EffectDirty,
                        reason: Some(
                            "judgeHash changed inside a foreach round; re-executed rather than \
                             re-judged (an offline re-judge there has no `iter` binding)"
                                .to_owned(),
                        ),
                    });
                    first_dirty.get_or_insert(index);
                    continue;
                }
                // An `assert` step's judge domain is `preflight` + `observe`
                // + `assertions` (02 §12.3), so a judgeDirty there is NOT
                // necessarily preflight-only — the author may have changed
                // the assertion itself. It re-executes: an assert step is
                // readonly, so re-observing is safe and strictly more
                // truthful than judging archived material. (Offline
                // re-judge for assert steps needs `rejudge` generalized
                // beyond `ActionStepIR` first.)
                if matches!(step, StepIR::Assert(_)) {
                    entries.push(AlignmentEntry {
                        run_path: record.run_path.clone(),
                        step_id,
                        class: AlignmentClass::EffectDirty,
                        reason: Some(
                            "judgeHash changed on an assert step; re-observed rather than \
                             re-judged"
                                .to_owned(),
                        ),
                    });
                    first_dirty.get_or_insert(index);
                    continue;
                }
                let StepIR::Action(action) = step else {
                    // 02 §12.3 ruling 6: for `human` / `let` / `if` /
                    // `foreach` / `call` the judge domain is `preflight`
                    // ALONE, so a judgeDirty there can only be a preflight
                    // change — adopt, never re-judge (no assertions exist to
                    // replay, and a settled human answer stays settled: the
                    // whole question domain is effect-side, so a changed
                    // prompt would already have been effectDirty).
                    adopt(
                        step,
                        &key,
                        record,
                        facts,
                        seed,
                        &mut outputs,
                        &mut verdicts,
                        &mut adoptable,
                        None,
                    );
                    entries.push(AlignmentEntry {
                        run_path: record.run_path.clone(),
                        step_id,
                        class,
                        reason: Some("preflightChanged".to_owned()),
                    });
                    continue;
                };
                let verdict = rejudge(
                    action, &key, record, facts, seed, &outputs, &verdicts, policy, store, vision,
                )
                .await;
                let failed = verdict.status == VerdictStatus::Fail;
                rejudged.push(Rejudged {
                    run_path: record.run_path.clone(),
                    verdict: verdict.clone(),
                });
                if failed {
                    // The re-judge honestly overturns history; the resume
                    // point rolls back to this step (07 §5.3).
                    first_dirty = Some(index);
                    entries.push(AlignmentEntry {
                        run_path: record.run_path.clone(),
                        step_id,
                        class,
                        reason: Some("offline re-judge failed; step re-executes".to_owned()),
                    });
                } else {
                    adopt(
                        step,
                        &key,
                        record,
                        facts,
                        seed,
                        &mut outputs,
                        &mut verdicts,
                        &mut adoptable,
                        Some((verdict.status, verdict.degraded)),
                    );
                    entries.push(AlignmentEntry {
                        run_path: record.run_path.clone(),
                        step_id,
                        class,
                        reason: Some("judgeHash changed; re-judged offline".to_owned()),
                    });
                }
            }
            AlignmentClass::EffectDirty => {
                let reason = match inversion {
                    // The report must name the inverted pair, so a reviewer
                    // can judge whether the two are truly order-independent.
                    Some((_, earlier_id, later_id)) if reordered => Some(format!(
                        "order invalidated: '{later_id}' ran before '{earlier_id}' in the old \
                         run, reversing their order in the new IR; no record is adopted from \
                         there on"
                    )),
                    _ if forced_hit => {
                        Some("forced re-execution (--force-reexecute, 07 §5.3)".to_owned())
                    }
                    _ => Some("effectHash changed".to_owned()),
                };
                entries.push(AlignmentEntry {
                    run_path: record.run_path.clone(),
                    step_id,
                    class,
                    reason,
                });
                first_dirty.get_or_insert(index);
            }
            AlignmentClass::New | AlignmentClass::Orphaned => {
                unreachable!("classified above")
            }
        }
    }

    // A container may only be adopted when its whole subtree was: adopting
    // it makes the engine skip the container outright (`adopt_step` seeds
    // the children and never re-enters), so one dirty step inside would be
    // skipped along with it. Dropping just the container leaves its adopted
    // children in place: the engine re-enters, re-derives the same branch,
    // adopts what it can and re-runs only what is dirty.
    //
    // Innermost first (reverse pre-order), so an un-adopted inner container
    // propagates outward.
    for node in nodes.iter().rev() {
        if !matches!(node.step, StepIR::If(_) | StepIR::Foreach(_)) {
            continue;
        }
        let key = instance_key(&node.path);
        if !adoptable.contains_key(&key) {
            continue;
        }
        let subtree_intact = nodes
            .iter()
            .filter(|other| is_descendant(&node.path, &other.path))
            .all(|child| adoptable.contains_key(&instance_key(&child.path)));
        if !subtree_intact {
            adoptable.remove(&key);
        }
    }

    // Old records whose instance the new IR no longer reaches: archived,
    // never adopted.
    //
    // A record from inside a callee frame counts only when the walk went in
    // there. Where a `call` was classified whole (cases (b)/(c)), its
    // callee-internal records were never candidates and calling them
    // "absent from the new IR" would be a false statement about the new
    // flow — they are simply archived with their frame. Where the walk DID
    // descend, a record with no node is genuinely a step the repaired
    // callee dropped, and the report says so.
    let node_keys: BTreeSet<String> = nodes.iter().map(|node| instance_key(&node.path)).collect();
    for record in completed.values() {
        // 07 §5.2: 「hook 帧下的记录（handler 审计痕）……旧 hook 记录一律归档」.
        // A handler's audit trace was never a step of the flow body, so
        // "absent from the new IR" would be a false statement about it —
        // and alignment cannot even address these paths (`resolve_step`
        // refuses a hook frame outright). Stated as its own rule: the
        // records the engine produces today happen to sit under the
        // handler-launched `call` frame and would be skipped by the
        // descent check below anyway, but that is an accident of the
        // current path shapes, not a reason.
        if record
            .run_path
            .iter()
            .any(|frame| matches!(frame, PathFrame::Hook { .. }))
        {
            continue;
        }
        let key = instance_key(&record.run_path);
        if node_keys.contains(&key) {
            continue;
        }
        // Only PROPER ancestors gate the report: a record whose own trailing
        // frame is the call (the call step itself, 07 §2.1) is a caller-level
        // instance and is orphaned like any other when the new IR drops it.
        let enclosing = record.run_path.len().saturating_sub(1);
        let inside_undrilled_frame = record.run_path[..enclosing]
            .iter()
            .enumerate()
            .filter(|(_, frame)| matches!(frame, PathFrame::Call { .. }))
            .any(|(index, _)| !descended.contains(&instance_key(&record.run_path[..=index])));
        if inside_undrilled_frame {
            continue;
        }
        entries.push(AlignmentEntry {
            run_path: record.run_path.clone(),
            step_id: record.step_id.clone(),
            class: AlignmentClass::Orphaned,
            reason: Some("absent from the new IR".to_owned()),
        });
    }

    // The resume point: the first non-reusable / non-successfully-
    // re-judged position; with nothing dirty, the frontier cursor of the
    // (sequential) root frame.
    //
    // The cursor is a ROOT-BODY index and `nodes` is the flattened
    // pre-order walk, so the cursor must be translated, not used raw: with
    // a container before the cursor, `nodes[cursor]` would land INSIDE it
    // (children directly follow their container) and the report would name
    // an interior step as the resume point. The cursor-th top-level node —
    // path depth 2, `[flow, frame]` — is the position the cursor means; a
    // cursor past the body maps past the nodes (no resume point).
    let resume_idx = first_dirty.unwrap_or_else(|| {
        let cursor = view
            .frames
            .first()
            .map(|frame| frame.next_index as usize)
            .unwrap_or(0);
        nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.path.len() == 2)
            .nth(cursor)
            .map(|(index, _)| index)
            .unwrap_or(nodes.len())
    });

    // 07 §5.4 unified confirmation gate over everything that re-executes.
    // An authorization naming no gated step releases nothing and is not an
    // error: the gate stays fail-closed either way, so a mistyped id simply
    // leaves its step gated and the resume refuses again — while a segment
    // that legitimately no longer gates a previously authorized step (the
    // human-wave continuation of one resume) must not be failed for it.
    let mut requires_confirmation = Vec::new();
    for (index, node) in nodes.iter().enumerate().skip(resume_idx) {
        let sid = node.step.step_id().as_str();
        let key = instance_key(&node.path);
        // The torn-down call (07 §5.2 case (b)) has no record of its own —
        // the frame never concluded — but the spec gates it on the FRAME's
        // work: 「旧帧内若存在 succeeded/timedOut 的非幂等 mutating attempt，
        // 该 call step 进 requiresConfirmation」. Its evidence therefore
        // comes from the callee's completed records alone (the frontier
        // step's still-open attempt is the reconcile pass's, which gates it
        // as `frontierUnknown` separately). Handler audit work stays
        // excluded, as everywhere.
        if teardown.as_deref() == Some(key.as_str()) {
            let effective = gated_effect(node.step, false, loaded)
                && completed.iter().any(|(other, inner)| {
                    is_instance_descendant(&key, other)
                        && !inner
                            .run_path
                            .iter()
                            .any(|frame| matches!(frame, PathFrame::Hook { .. }))
                        && prior_effect_possible(inner)
                });
            if effective && !authorized.iter().any(|allowed| allowed == sid) {
                let run_path = facts
                    .live_frames
                    .iter()
                    .find(|path| instance_key(path) == key)
                    .cloned()
                    .unwrap_or_else(|| node.path.clone());
                requires_confirmation.push(RequiresConfirmation {
                    run_path,
                    step_id: Some(node.step.step_id().clone()),
                    cause: "mutatingReexec".to_owned(),
                    reason: format!(
                        "re-calling '{sid}' (traversal index {index}) tears down a frame whose \
                         completed steps include a mutating, non-idempotent action that took \
                         effect; the re-call needs explicit authorization (07 §5.2 case (b))"
                    ),
                });
            }
            continue;
        }
        let Some(record) = completed.get(&key) else {
            continue;
        };
        // An action answers for itself; a container answers for its body
        // exactly when the walk did not go inside it (07 §5.4, see
        // [`gated_effect`]). A descended container gates through the steps
        // in there, which are nodes of their own.
        if !gated_effect(node.step, descended.contains(&key), loaded) {
            continue;
        }
        // A container answers for its BODY, not just for its own record. Its
        // record carries no attempts — those live on the action records
        // inside it — and an `if`'s or `foreach`'s record carries no verdict
        // either, so every container would sail through this check while
        // re-execution replays whatever ran in there. That is exactly what
        // 07 §5.2 (b) sends to `requiresConfirmation` for a call frame
        // ("旧帧内若存在 succeeded/timedOut 的非幂等 mutating attempt"), and
        // what §5.2 位置失效 says for a re-executed branch.
        //
        // The scan is deliberately coarse: `gated_effect` has already
        // established that the body's closure contains a gated mutating
        // step, and pairing that with "something in there took effect"
        // over-gates at worst — which the author releases by name.
        let effective = prior_effect_possible(record)
            || (is_container(node.step)
                && completed.iter().any(|(other, inner)| {
                    is_instance_descendant(&key, other)
                        // Handler work is NOT body work. `gated_effect`
                        // derives "this container's body mutates" from the
                        // body closure alone — `body_mutates` never walks
                        // `handlers` — so pairing that declaration with
                        // effect evidence harvested from a handler would
                        // gate a container whose body did nothing, and say
                        // so in a `reason` that is plainly false. Re-running
                        // the container replays its body, not the repair
                        // that once fixed the world for it (07 §5.2: a
                        // handler audit trace is archived, not reused).
                        //
                        // Filtered on the PATH, not the key: `hook-call` is
                        // a spellable step id, while a `hook` path frame is
                        // unforgeable.
                        && !inner
                            .run_path
                            .iter()
                            .any(|frame| matches!(frame, PathFrame::Hook { .. }))
                        && prior_effect_possible(inner)
                }));
        if !effective {
            // priorVerdict=fail with no succeeded attempt: the old action
            // never took effect — re-execution is the repair (no gate).
            continue;
        }
        if authorized.iter().any(|allowed| allowed == sid) {
            // Explicitly authorized for this resume (07 §5.4 step 2). The
            // step still probes its `preflight` when it runs (step 3) —
            // authorization releases the gate, not the world check.
            continue;
        }
        // The closed cause set of 07 §5.4 (`RequiresConfirmation::cause`):
        // reordering is its own cause and outranks the hash-derived ones,
        // because such a step's hashes are unchanged — its record was
        // rejected for belonging to a different execution order.
        let cause = if inversion.is_some_and(|(from, _, _)| index >= from) {
            "orderInvalidated"
        } else {
            // By instance, not by step id: ids are unique per flow, so a
            // caller and its callee may legitimately share one, and a
            // `foreach` body step has an entry per round.
            match entries
                .iter()
                .find(|entry| instance_key(&entry.run_path) == key)
                .map(|entry| entry.class)
            {
                Some(AlignmentClass::EffectDirty) => "mutatingReexec",
                _ => "positionalReplay",
            }
        };
        requires_confirmation.push(RequiresConfirmation {
            run_path: record.run_path.clone(),
            step_id: Some(node.step.step_id().clone()),
            cause: cause.to_owned(),
            reason: if is_container(node.step) {
                format!(
                    "re-executing '{sid}' (traversal index {index}) replays steps inside it \
                     that are mutating, not idempotent, and whose prior effect may be in the \
                     world; it needs explicit authorization"
                )
            } else {
                format!(
                    "step '{sid}' (traversal index {index}) is mutating, not idempotent, and \
                     its prior effect may be in the world; re-execution needs explicit \
                     authorization"
                )
            },
        });
    }

    // Defense line, not a rule: every live non-hook frame must be either
    // descended (case (a) re-entry) or the teardown target (case (b)).
    // The walk guarantees it — open frames form one suspension chain and
    // the walk stops at the outermost call it cannot descend — so a frame
    // outside both sets means the walker and the ledger disagree about
    // the run's shape, and executing on that disagreement would re-enter
    // a frame nothing classified.
    if let Some(stranded) = facts.live_frames.iter().find(|path| {
        let key = instance_key(path);
        !path
            .iter()
            .any(|frame| matches!(frame, PathFrame::Hook { .. }))
            && !descended.contains(&key)
            && teardown.as_deref() != Some(key.as_str())
    }) {
        return Err(RunnerError::M0Unsupported {
            detail: format!(
                "live frame {} is neither re-entered nor torn down — the new IR's walk never \
                 reached its call site (a container above it changed shape); resume cannot \
                 address it",
                pointlock_ir::render_run_path(stranded)
            ),
        });
    }

    let resume_point = nodes.get(resume_idx).map(|node| node.path.clone());
    let report = AlignmentReport {
        entries,
        resume_point,
        requires_confirmation,
    };
    if !report.requires_confirmation.is_empty() {
        return Err(RunnerError::RequiresConfirmation {
            report: Box::new(report),
        });
    }
    Ok(Alignment {
        report,
        resume_key: nodes.get(resume_idx).map(|node| instance_key(&node.path)),
        rejudged,
        adoptable,
        teardown,
    })
}

/// The first order inversion, as `(new-IR index, ran-later, ran-earlier)`.
///
/// 07 §5.2 order consistency: matching by `stepId` is blind to reordering.
/// Swap two completed, data-independent steps and their ids and both
/// hashes are untouched — every step classifies `reusable` — yet the world
/// was produced in the old order, so the execution sequence the new IR
/// claims never happened. Order-sensitive effects (which of two taps came
/// first, when a `fresh` observation was taken) make that a different
/// history wearing this IR's name.
///
/// So: adoption is conditional on order. Walk the matched completed
/// records in new-IR traversal order and require their old execution seq
/// to increase strictly; the first position that does not is the
/// inversion, and nothing from there on may be adopted.
fn order_inversion<'a>(
    nodes: &'a [Node<'a>],
    completed: &BTreeMap<String, &StepRecord>,
    facts: &Harvest,
) -> Option<(usize, &'a str, &'a str)> {
    let mut previous: Option<(&'a str, u64)> = None;
    for (index, node) in nodes.iter().enumerate() {
        let sid = node.step.step_id().as_str();
        let key = instance_key(&node.path);
        if !completed.contains_key(&key) {
            continue;
        }
        // A record whose entry seq was never harvested carries no order
        // evidence; it is not counted as an inversion (principle 4: never
        // fabricate). It also does not update the cursor, so a genuine
        // inversion straddling it is still caught.
        let Some(&seq) = facts.entered_seq.get(&key) else {
            continue;
        };
        if let Some((earlier_id, earlier_seq)) = previous
            && seq <= earlier_seq
        {
            return Some((index, earlier_id, sid));
        }
        previous = Some((sid, seq));
    }
    None
}

/// The last adoptable record per step *instance*.
///
/// Keyed by [`instance_key`], not by step id: a step id is unique only in
/// a flat body, while an instance key addresses the exact occurrence —
/// iteration round, callee frame and all. On a flat run the two are
/// byte-identical (`/{stepId}`), so this is the same index the flat
/// subset always used, addressed in the vocabulary nesting will need.
///
/// Two rules ride along:
/// - only execution history is indexed ([`is_history`], the same predicate
///   the engine's adoption short-circuit uses). A skipped branch step or a
///   blocked tail is an accounting pair that concluded nothing: the engine
///   writes those with `resolvedInputs: null` and no attempt, so all four
///   clauses are false and they are excluded. A *container* record — an
///   `if`'s archived `{cond}`, a `foreach`'s `{items, as}` — has no
///   attempt, verdict or output either, and its control snapshot IS its
///   whole history; only this predicate keeps it.
/// - a repeated instance keeps the LAST record. The fold appends a record
///   per exit, so a step that re-executed after a resume has several; the
///   newest is the one this resume may adopt.
fn completed_by_instance(view: &CheckpointView) -> BTreeMap<String, &StepRecord> {
    let mut map = BTreeMap::new();
    for record in &view.completed {
        if !is_history(record) {
            continue;
        }
        map.insert(instance_key(&record.run_path), record);
    }
    map
}

/// Adopts a completed record.
///
/// Two things happen, for two different consumers:
/// - the alignment-local `outputs`/`verdicts` advance, because every later
///   step's offline re-judge and output projection evaluates against the
///   scope the adopted prefix produces;
/// - the record enters the adoption set, which is what the engine actually
///   consumes. `override_verdict` (a successful offline re-judge) is
///   written onto the adopted record, so the engine seeds the superseding
///   judgment rather than the one the old run recorded.
#[allow(clippy::too_many_arguments)]
fn adopt(
    step: &StepIR,
    key: &str,
    record: &StepRecord,
    facts: &Harvest,
    seed: &ScopeSeed,
    outputs: &mut BTreeMap<String, Value>,
    verdicts: &mut BTreeMap<String, (VerdictStatus, bool)>,
    adoptable: &mut BTreeMap<String, Adopted>,
    override_verdict: Option<(VerdictStatus, bool)>,
) {
    let sid = step.step_id().as_str();
    // Only an action projects an output; a container's archived control
    // snapshot is consumed by the engine, not by this scope.
    if let (StepIR::Action(action), Some(raw)) = (step, facts.raw_output.get(key)) {
        let scope = seed.scope(outputs, verdicts, Some((sid, raw)));
        if let Ok(projected) = project_output(action, raw, &scope) {
            outputs.insert(sid.to_owned(), projected);
        }
    }
    if let Some((status, degraded)) = override_verdict {
        verdicts.insert(sid.to_owned(), (status, degraded));
    } else if let Some(verdict) = &record.verdict {
        verdicts.insert(sid.to_owned(), (verdict.status, verdict.degraded));
    }

    let mut adopted_record = record.clone();
    if let Some((status, degraded)) = override_verdict
        && let Some(verdict) = &mut adopted_record.verdict
    {
        verdict.status = status;
        verdict.degraded = degraded;
    }
    adoptable.insert(
        key.to_owned(),
        Adopted {
            record: adopted_record,
            before_id: facts.before_observation.get(key).cloned(),
            after_id: facts.after_observation.get(key).cloned(),
        },
    );
}

/// Offline re-judge (07 §5.3): evaluate the new assertions against the
/// archived output and the localized observation material; missing archive
/// material yields unknown, never a guess (principle 4). Element
/// predicates replay their verify chain over the *localized* tree bytes —
/// the session is never touched. The vision tail consults this segment's
/// verifier (`ResumeOptions::vision`, M3a-W4) against the *archived*
/// screenshot bytes — still zero device I/O; without a verifier it
/// degrades honestly to unknown. The execution-degradation flag of the
/// old verdict carries over (re-judging cannot un-degrade an execution).
///
/// `key` is the step INSTANCE key, passed in rather than synthesized from
/// the step id: the archived material (`raw_output`, the observation ids)
/// is indexed by instance, and a step inside an `if` branch or a callee
/// frame keys as `/host/step`, not `/step`. Synthesizing it would look up
/// nothing, produce `unknown`, and supersede a real verdict with it.
#[allow(clippy::too_many_arguments)]
async fn rejudge(
    step: &ActionStepIR,
    key: &str,
    record: &StepRecord,
    facts: &Harvest,
    seed: &ScopeSeed,
    outputs: &BTreeMap<String, Value>,
    verdicts: &BTreeMap<String, (VerdictStatus, bool)>,
    policy: VerdictPolicy,
    store: &Store,
    vision: Option<&dyn pointlock_vision::VisionVerifier>,
) -> Verdict {
    let sid = step.base.step_id.as_str();
    let degraded = record
        .verdict
        .as_ref()
        .map(|verdict| verdict.degraded)
        .unwrap_or(false);
    let supersedes = facts.verdict_seq.get(key).map(|seq| format!("seq:{seq}"));
    let folded = match facts.raw_output.get(key) {
        None => crate::judge::FoldedVerdict {
            status: VerdictStatus::Unknown,
            degraded,
            summary: "offline re-judge: no archived output for this step (missing input ⇒ \
                      unknown)"
                .to_owned(),
        },
        Some(raw) => {
            let raw_scope = seed.scope(outputs, verdicts, Some((sid, raw)));
            match project_output(step, raw, &raw_scope) {
                Err(error) => crate::judge::FoldedVerdict {
                    status: VerdictStatus::Unknown,
                    degraded,
                    summary: format!("offline re-judge: output projection failed: {error}"),
                },
                Ok(projected) => {
                    let scope = seed.scope(outputs, verdicts, Some((sid, &projected)));
                    let material = archived_material(
                        record,
                        facts.after_observation.get(key).map(String::as_str),
                        store,
                    );
                    let mut assertion_outcomes = Vec::with_capacity(step.assertions.len());
                    let mut degraded_verify = false;
                    for assertion in &step.assertions {
                        let evaluated = match &assertion.predicate {
                            PredicateIR::Expr { expr } => EvaluatedAssertion {
                                record: eval_expr_assertion(assertion, expr, &scope),
                                degraded_verify: false,
                            },
                            _ => eval_observed_assertion(assertion, &material, vision).await,
                        };
                        degraded_verify |= evaluated.degraded_verify;
                        assertion_outcomes.push(evaluated.record);
                    }
                    if assertion_outcomes.is_empty() {
                        crate::judge::FoldedVerdict {
                            status: VerdictStatus::Unknown,
                            degraded,
                            summary: "offline re-judge: the new step declares no assertions"
                                .to_owned(),
                        }
                    } else {
                        let mut folded = fold_step_verdict(
                            &assertion_outcomes,
                            degraded,
                            degraded_verify,
                            policy,
                        );
                        folded.summary =
                            format!("offline re-judge over archived output: {}", folded.summary);
                        folded
                    }
                }
            }
        }
    };
    Verdict {
        status: folded.status,
        degraded: folded.degraded,
        summary: folded.summary,
        evidence: Vec::new(),
        supersedes,
    }
}

/// Rebuilds the verify-chain material of a completed step from its
/// archived after-observation record (shared reader:
/// [`material_from_observation`]).
fn archived_material(
    record: &StepRecord,
    after_id: Option<&str>,
    store: &Store,
) -> ObserveMaterial {
    let Some(after_id) = after_id else {
        return ObserveMaterial::absent(
            "offline re-judge: the archived run recorded no after observation",
        );
    };
    let Some(observation) = record
        .observations
        .iter()
        .find(|observation| observation.observation_id == after_id)
    else {
        return ObserveMaterial::absent(
            "offline re-judge: the after observation was never localized",
        );
    };
    material_from_observation(store, observation)
}

/// The 07 §5.4 side-effect criterion (2026-07-28 ruling): an EFFECTIVE
/// ATTEMPT — a `succeeded` or `timedOut` terminal — is what puts the
/// effect in the world, and it alone decides the gate.
///
/// The verdict is deliberately not consulted. It judges the world AFTER
/// the act (the assertion layer); whether the act landed is the attempt
/// terminal's testimony, and the two disagree in both directions:
/// - `fail` WITH a succeeded attempt (the act landed, the assertion
///   refused it) GATES — re-execution is a second effect regardless of
///   what the assertions thought of the first;
/// - `unknown` with only `failed`/`cancelled` attempts (e.g. a
///   `session_degraded` terminal folding the step to unknown) does NOT —
///   the terminal is the daemon's word that the act never took effect,
///   and re-execution is the repair.
///
/// A `pass`/`unknown` that DOES rest on an effective attempt still gates
/// through that attempt, so nothing the old verdict-based reading gated
/// correctly is lost. Container records carry no attempts and answer
/// through the descendant scan at the gate site instead.
fn prior_effect_possible(record: &StepRecord) -> bool {
    record.attempts.iter().any(|attempt| {
        matches!(
            attempt.outcome,
            ActionOutcomeKind::Succeeded | ActionOutcomeKind::TimedOut
        )
    })
}
