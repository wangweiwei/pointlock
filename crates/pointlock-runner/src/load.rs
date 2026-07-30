//! Load-time checks (spine §1.2, 02 §14): hash recomputation, capability
//! drift, subflow-registry closure, and the execution-subset gate.
//!
//! Iron rule: nothing outside the subset is silently skipped. Since M2 the
//! control-flow vocabulary executes — call/if/foreach/let/assert steps and
//! preflight probes — with the human wave (M2-W2a) `human` steps, and with
//! the handler wave (M2-W3) flow- and step-level handlers (re-invocation
//! dispositions on call/human hosts stay typed refusals — 07 §1's
//! attempt-framed re-call and fresh-request re-asks are registered).
//! Human steps carry a shape defense line mirroring the compiler check
//! phase (06 §2.2): `confirm` requires exactly two decision labels,
//! `judge` decisions must stay inside the three-valued vocabulary, and
//! `provideInput` requires an `outputSchema`.
//!
//! Subflows (07 §1.3): the caller provides the resolved callee IR set as a
//! `Map<irHash, FlowIR>`. Every entry self-verifies (`irHash` recompute ==
//! key == declared), every `call` step's `flowRef` must agree with the
//! flow's own `subflows` pin table *and* resolve inside the registry, and
//! the static call closure must stay within `maxCallDepth = 8` (compiler
//! parity; the load check is the runtime defense against hand-built IR).

use std::collections::BTreeMap;

use pointlock_ir::{
    ActionStepIR, FlowIR, FlowRef, HandlerAction, HandlerHook, Hash, PathFrame, RunPath, StepIR,
    effect_hash, ir_hash, judge_hash,
};
use pointlock_provider_kit::CapabilityAttestation;

use crate::error::RunnerError;

/// `maxCallDepth` (07 §1.3): frames including the root.
pub(crate) const MAX_CALL_DEPTH: usize = 8;

/// A load-checked flow closure: hash-verified, subset-narrowed, with the
/// subflow registry resolved over the whole link closure.
pub(crate) struct LoadedFlow<'a> {
    /// The root flow.
    pub root: &'a FlowIR,
    /// Resolved callees keyed by their content hash (the `flowRef.irHash`
    /// pins). Every reachable call target is present and load-checked.
    callees: BTreeMap<&'a Hash, &'a FlowIR>,
}

impl<'a> LoadedFlow<'a> {
    /// Resolves a call step's pinned callee. Infallible after `load` — the
    /// registry closure was verified there.
    pub fn callee(&self, flow_ref: &FlowRef) -> &'a FlowIR {
        self.callees
            .get(&flow_ref.ir_hash)
            .expect("load verified that every call target resolves")
    }

    /// Fallible callee lookup, for load-time validation itself.
    pub fn try_callee(&self, flow_ref: &FlowRef) -> Option<&'a FlowIR> {
        self.callees.get(&flow_ref.ir_hash).copied()
    }

    /// Resolves a run path to the [`StepIR`] it addresses, descending
    /// through call frames (via the registry), iteration frames and
    /// container steps. Attempt/phase/assertion suffixes are ignored.
    pub fn resolve_step(&self, path: &RunPath) -> Option<&'a StepIR> {
        let mut bodies: Vec<&'a [StepIR]> = vec![&self.root.body];
        let mut current: Option<&'a StepIR> = None;
        for frame in path {
            match frame {
                PathFrame::Flow { .. } | PathFrame::Iteration { .. } => {}
                PathFrame::Attempt { .. }
                | PathFrame::Phase { .. }
                | PathFrame::Assertion { .. } => break,
                PathFrame::Hook { .. } => return None,
                PathFrame::Step { step_id } => {
                    let step = find_step(&bodies, step_id.as_str())?;
                    bodies = child_bodies(step);
                    current = Some(step);
                }
                PathFrame::Call { step_id, .. } => {
                    let step = find_step(&bodies, step_id.as_ref()?.as_str())?;
                    let StepIR::Call(call) = step else {
                        return None;
                    };
                    bodies = vec![&self.callee(&call.flow_ref).body];
                    current = Some(step);
                }
            }
        }
        current
    }

    /// Resolves a run path to an action step, when it addresses one.
    pub fn resolve_action(&self, path: &RunPath) -> Option<&'a ActionStepIR> {
        match self.resolve_step(path)? {
            StepIR::Action(action) => Some(action),
            _ => None,
        }
    }
}

fn find_step<'a>(bodies: &[&'a [StepIR]], id: &str) -> Option<&'a StepIR> {
    bodies
        .iter()
        .flat_map(|body| body.iter())
        .find(|step| step.step_id().as_str() == id)
}

/// The bodies a path may descend into below a step (if branches / foreach
/// body; leaf kinds have none — call descent goes through the registry).
fn child_bodies<'a>(step: &'a StepIR) -> Vec<&'a [StepIR]> {
    match step {
        StepIR::If(s) => {
            let mut bodies: Vec<&'a [StepIR]> = vec![&s.then];
            if let Some(otherwise) = &s.r#else {
                bodies.push(otherwise);
            }
            bodies
        }
        StepIR::Foreach(s) => vec![&s.body],
        _ => Vec::new(),
    }
}

/// Runs the load checks over the whole link closure.
///
/// Checks, in order: `irHash` recompute-and-compare on the root and on
/// every registry entry (self-checkable artifacts, 02 §12.2; the registry
/// key must equal the recomputed hash), then per flow: per-step
/// `effectHash`/`judgeHash` recompute-and-compare, the subset gate (the
/// re-invocation dispositions on call/human hosts refused), call-target
/// resolution against both the flow's `subflows` pin table and the
/// registry, and the `maxCallDepth` bound over the static call closure.
pub(crate) fn load<'a>(
    flow: &'a FlowIR,
    registry: &'a BTreeMap<Hash, FlowIR>,
) -> Result<LoadedFlow<'a>, RunnerError> {
    let computed = ir_hash(flow);
    if computed != flow.ir_hash {
        return Err(RunnerError::IrHashMismatch {
            declared: flow.ir_hash.clone(),
            computed,
        });
    }
    let mut callees: BTreeMap<&'a Hash, &'a FlowIR> = BTreeMap::new();
    for (key, callee) in registry {
        let computed = ir_hash(callee);
        if computed != callee.ir_hash || &computed != key {
            return Err(RunnerError::SubflowRegistry {
                detail: format!(
                    "registry entry '{}' does not self-verify: key {key}, declared {}, \
                     computed {computed}",
                    callee.flow_id, callee.ir_hash
                ),
            });
        }
        callees.insert(&callee.ir_hash, callee);
    }
    let loaded = LoadedFlow {
        root: flow,
        callees,
    };
    // Depth-first over the static call closure. Cycles are structurally
    // impossible (a cycle needs a content-hash fixpoint and every pin was
    // recompute-verified above), so the recursion terminates; diamond
    // sharing re-checks a callee per call site, which is fine at depth ≤ 8.
    check_flow(&loaded, flow, 1)?;
    Ok(loaded)
}

fn check_flow(loaded: &LoadedFlow<'_>, flow: &FlowIR, depth: usize) -> Result<(), RunnerError> {
    if depth > MAX_CALL_DEPTH {
        return Err(RunnerError::CallDepthExceeded {
            depth,
            max: MAX_CALL_DEPTH,
        });
    }
    if let Some(bindings) = &flow.handlers {
        check_handlers(loaded, flow, None, bindings)?;
    }
    check_body(loaded, flow, &flow.body, depth)
}

fn check_body(
    loaded: &LoadedFlow<'_>,
    flow: &FlowIR,
    steps: &[StepIR],
    depth: usize,
) -> Result<(), RunnerError> {
    for step in steps {
        verify_step_hashes(step)?;
        let step_id = step.step_id().clone();
        if let Some(bindings) = &step.base().handlers {
            check_handlers(loaded, flow, Some(&step_id), bindings)?;
            // Re-invocation dispositions need machinery specific kinds
            // lack: call re-invocation (07 §1 attempt-framed re-call) and
            // human re-asking are typed W3 refusals at consultation time;
            // step-level bindings that could only ever hit them are
            // refused here already.
            if matches!(step, StepIR::Call(_) | StepIR::Human(_)) {
                for binding in bindings {
                    if matches!(
                        binding.action,
                        HandlerAction::Retry { .. } | HandlerAction::Repair { .. }
                    ) && !matches!(binding.hook, HandlerHook::OnResumeDrift)
                    {
                        return Err(RunnerError::NotInM0Subset {
                            step_id: Some(step_id.clone()),
                            construct: format!(
                                "re-invocation disposition on a {} step's handlers \
                                 (escalate/continue/abort are supported)",
                                if matches!(step, StepIR::Call(_)) {
                                    "call"
                                } else {
                                    "human"
                                }
                            ),
                        });
                    }
                }
            }
        }
        match step {
            StepIR::Human(human) => check_human_step(human)?,
            StepIR::Action(_) | StepIR::Assert(_) | StepIR::Let(_) => {}
            StepIR::If(s) => {
                check_body(loaded, flow, &s.then, depth)?;
                if let Some(otherwise) = &s.r#else {
                    check_body(loaded, flow, otherwise, depth)?;
                }
            }
            StepIR::Foreach(s) => check_body(loaded, flow, &s.body, depth)?,
            StepIR::Call(call) => {
                // The pin must be registered in the flow's own subflows
                // table (link closure: the caller's irHash covers it).
                match flow.subflows.get(&call.flow_ref.flow_id) {
                    Some(pin) if *pin == call.flow_ref => {}
                    _ => {
                        return Err(RunnerError::SubflowRegistry {
                            detail: format!(
                                "call step '{}' pins {}@{} but the flow's subflows table \
                                 does not register that reference",
                                step_id, call.flow_ref.flow_id, call.flow_ref.ir_hash
                            ),
                        });
                    }
                }
                let Some(callee) = loaded.callees.get(&call.flow_ref.ir_hash).copied() else {
                    return Err(RunnerError::SubflowRegistry {
                        detail: format!(
                            "call step '{}' pins {}@{} but the provided registry has no \
                             such entry",
                            step_id, call.flow_ref.flow_id, call.flow_ref.ir_hash
                        ),
                    });
                };
                if callee.flow_id != call.flow_ref.flow_id {
                    return Err(RunnerError::SubflowRegistry {
                        detail: format!(
                            "call step '{}': the registry entry for {} declares flowId '{}'",
                            step_id, call.flow_ref.ir_hash, callee.flow_id
                        ),
                    });
                }
                check_flow(loaded, callee, depth + 1)?;
            }
        }
    }
    Ok(())
}

/// The runtime shape defense of a human step (06 §2.2; the compiler check
/// phase refuses these first, this guards hand-built IR).
fn check_human_step(human: &pointlock_ir::HumanStepIR) -> Result<(), RunnerError> {
    let invalid = |reason: &str| RunnerError::InvalidHumanStep {
        step_id: human.base.step_id.clone(),
        reason: reason.to_owned(),
    };
    match human.mode {
        pointlock_ir::HumanMode::Confirm => {
            // Exactly two labels, position-mapped to pass/fail.
            if human.decisions.as_ref().is_none_or(|d| d.len() != 2) {
                return Err(invalid(
                    "confirm mode requires exactly two decision labels \
                     (first maps to pass, second to fail)",
                ));
            }
        }
        pointlock_ir::HumanMode::Judge => {
            // The three-valued vocabulary admits no aliases.
            if let Some(decisions) = &human.decisions
                && decisions
                    .iter()
                    .any(|d| !matches!(d.as_str(), "pass" | "fail" | "unknown"))
            {
                return Err(invalid(
                    "judge decisions must be a subset of pass|fail|unknown",
                ));
            }
        }
        pointlock_ir::HumanMode::ProvideInput => {
            if human.output_schema.is_none() {
                return Err(invalid("provideInput mode requires an outputSchema"));
            }
        }
        pointlock_ir::HumanMode::RepairWorld => {}
    }
    Ok(())
}

/// Recomputes and compares one step's stored dual hashes.
fn verify_step_hashes(step: &StepIR) -> Result<(), RunnerError> {
    let computed_effect = effect_hash(step);
    if computed_effect != step.base().effect_hash {
        return Err(RunnerError::StepHashMismatch {
            step_id: step.step_id().clone(),
            domain: "effect",
            declared: step.base().effect_hash.clone(),
            computed: computed_effect,
        });
    }
    let computed_judge = judge_hash(step);
    if computed_judge != step.base().judge_hash {
        return Err(RunnerError::StepHashMismatch {
            step_id: step.step_id().clone(),
            domain: "judge",
            declared: step.base().judge_hash.clone(),
            computed: computed_judge,
        });
    }
    Ok(())
}

/// Attestation gate (spine §4.1): the live session must attest exactly the
/// lockfile digest the IR was bound against — anything else is
/// `capability_drift`, refuse to run. The whole link closure is checked:
/// callees were bound against the same lockfile (the compiler unions their
/// feature needs into the caller). Feature satisfaction was already
/// enforced inside `open_session` (free enforcement via
/// `FeatureOffer.required`), so only digests are compared here.
pub(crate) fn check_attestation(
    loaded: &LoadedFlow<'_>,
    attestation: &CapabilityAttestation,
) -> Result<(), RunnerError> {
    for flow in std::iter::once(loaded.root).chain(loaded.callees.values().copied()) {
        if attestation.lockfile_digest != flow.lockfile_digest {
            return Err(RunnerError::CapabilityDrift {
                expected: flow.lockfile_digest.clone(),
                attested: attestation.lockfile_digest.clone(),
            });
        }
    }
    Ok(())
}

/// Load-time validation of handler bindings (W3): escalate humans must be
/// verdict-capable (provideInput has no verdict semantics on a hook), and
/// repair targets must resolve inside the linked closure like call pins.
fn check_handlers(
    loaded: &LoadedFlow<'_>,
    flow: &FlowIR,
    host: Option<&pointlock_ir::StepId>,
    bindings: &[pointlock_ir::HandlerBinding],
) -> Result<(), RunnerError> {
    for binding in bindings {
        match &binding.action {
            HandlerAction::Escalate { human } => {
                if human.mode == pointlock_ir::HumanMode::ProvideInput {
                    return Err(RunnerError::NotInM0Subset {
                        step_id: host.cloned(),
                        construct: "provideInput escalate handlers (no verdict semantics \
                                    on a hook; use judge/confirm/repairWorld)"
                            .to_owned(),
                    });
                }
                check_human_step(human)?;
            }
            HandlerAction::Repair { flow_ref } => {
                match flow.subflows.get(&flow_ref.flow_id) {
                    Some(pin) if *pin == *flow_ref => {}
                    _ => {
                        return Err(RunnerError::SubflowRegistry {
                            detail: format!(
                                "repair handler pins {}@{} but the flow's subflows table \
                                 does not register that reference",
                                flow_ref.flow_id,
                                flow_ref.ir_hash.hex_prefix8()
                            ),
                        });
                    }
                }
                if loaded.try_callee(flow_ref).is_none() {
                    return Err(RunnerError::SubflowRegistry {
                        detail: format!(
                            "repair handler target {}@{} is not in the loaded registry",
                            flow_ref.flow_id,
                            flow_ref.ir_hash.hex_prefix8()
                        ),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}
