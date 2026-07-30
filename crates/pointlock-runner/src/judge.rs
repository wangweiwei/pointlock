//! Pure judgment: output projection, expr-assertion evaluation, and the
//! deterministic verdict folding rules (spine §6.3).
//!
//! Everything here is pure computation over already-materialized values —
//! no I/O, no clock, no provider access. That purity is what makes the
//! offline re-judge of the `judgeDirty` alignment class (07 §5.3)
//! mathematically identical to the online `asserting` phase.

use pointlock_expr::{EvalError, Scope, eval};
use pointlock_ir::{
    ActionStepIR, AssertionIR, AssertionOutcomeRecord, Expr, VerdictPolicy, VerdictStatus,
};
use serde_json::Value;

/// Applies the step's declared output projection over the raw
/// `ActionResult.output`. Absent `outputs` map ⇒ identity projection
/// (02 §4.1.1). `scope_with_self_raw` must bind the *raw* output under the
/// step's own id (self-refs in the projection see the raw output).
pub(crate) fn project_output(
    step: &ActionStepIR,
    raw: &Value,
    scope_with_self_raw: &Scope,
) -> Result<Value, EvalError> {
    match &step.outputs {
        None => Ok(raw.clone()),
        Some(map) => {
            let mut projected = serde_json::Map::new();
            for (name, expr) in map.iter() {
                projected.insert(name.as_str().to_owned(), eval(expr, scope_with_self_raw)?);
            }
            Ok(Value::Object(projected))
        }
    }
}

/// Evaluates one expr assertion over the given scope (the scope must bind
/// the *projected* output under the step's own id). Element/visual
/// predicates take the verify-chain path in [`crate::observe_eval`]
/// instead — expr predicates consume no observation channel (`verifyVia:
/// []`), so `channel` is always absent here.
///
/// Principle 4 typed into control flow: an evaluation error or a missing
/// input yields `unknown` for that assertion — never a panic, never a
/// guessed pass/fail.
pub(crate) fn eval_expr_assertion(
    assertion: &AssertionIR,
    expr: &Expr,
    scope: &Scope,
) -> AssertionOutcomeRecord {
    let (result, reason) = match eval(expr, scope) {
        Ok(Value::Bool(true)) => (
            VerdictStatus::Pass,
            "expr predicate evaluated to true".to_owned(),
        ),
        Ok(Value::Bool(false)) => (
            VerdictStatus::Fail,
            "expr predicate evaluated to false".to_owned(),
        ),
        Ok(other) => (
            VerdictStatus::Unknown,
            format!("expr predicate evaluated to a non-boolean value: {other}"),
        ),
        Err(error) => (
            VerdictStatus::Unknown,
            format!("expr predicate could not be evaluated: {error}"),
        ),
    };
    AssertionOutcomeRecord {
        assert_id: assertion.assert_id.clone(),
        result,
        channel: None,
        reason,
    }
}

/// A folded verdict core (status + degradation + human-readable summary).
pub(crate) struct FoldedVerdict {
    /// The three-valued status.
    pub status: VerdictStatus,
    /// Whether the verdict rests on a degraded execution/verification.
    pub degraded: bool,
    /// The folding summary.
    pub summary: String,
}

/// Folds assertion outcomes into a step verdict (spine §6.3, verbatim):
/// any fail → fail; else any unknown → unknown; else pass. A pass that
/// rests on a degraded execution (unauthorized provider-internal mode,
/// §6.4 R-degrade) or a degraded verification (an assertion answered by a
/// non-preferred verify channel, §6.3 rule 1) keeps `degraded=true`; under
/// `verdictPolicy: strict` that pass folds to unknown.
pub(crate) fn fold_step_verdict(
    outcomes: &[AssertionOutcomeRecord],
    degraded_execution: bool,
    degraded_verify: bool,
    policy: VerdictPolicy,
) -> FoldedVerdict {
    let (mut status, mut summary) = if let Some(fail) = outcomes
        .iter()
        .find(|outcome| outcome.result == VerdictStatus::Fail)
    {
        (
            VerdictStatus::Fail,
            format!("assertion '{}' failed: {}", fail.assert_id, fail.reason),
        )
    } else if let Some(unknown) = outcomes
        .iter()
        .find(|outcome| outcome.result == VerdictStatus::Unknown)
    {
        (
            VerdictStatus::Unknown,
            format!(
                "assertion '{}' unknown: {}",
                unknown.assert_id, unknown.reason
            ),
        )
    } else {
        (
            VerdictStatus::Pass,
            format!("all {} assertion(s) passed", outcomes.len()),
        )
    };
    if degraded_execution {
        summary.push_str("; execution was degraded by the provider (unauthorized mode)");
    }
    if degraded_verify {
        summary.push_str("; verification was degraded to a non-preferred channel");
    }
    let degraded = degraded_execution || degraded_verify;
    if degraded && status == VerdictStatus::Pass && policy == VerdictPolicy::Strict {
        status = VerdictStatus::Unknown;
        summary.push_str("; strict verdict policy folds a degraded pass to unknown");
    }
    FoldedVerdict {
        status,
        degraded,
        summary,
    }
}

/// Folds step verdicts into the flow verdict (spine §6.3: same rule over
/// the steps that *have* verdicts; steps without verdicts — unasserted
/// mutations, blocked/aborted steps — do not participate). Returns `None`
/// when no step produced a verdict.
pub(crate) fn fold_flow_verdict(
    step_verdicts: &[(VerdictStatus, bool)],
    policy: VerdictPolicy,
) -> Option<FoldedVerdict> {
    if step_verdicts.is_empty() {
        return None;
    }
    let fails = count(step_verdicts, VerdictStatus::Fail);
    let unknowns = count(step_verdicts, VerdictStatus::Unknown);
    let passes = count(step_verdicts, VerdictStatus::Pass);
    let degraded = step_verdicts.iter().any(|(_, degraded)| *degraded);
    let mut status = if fails > 0 {
        VerdictStatus::Fail
    } else if unknowns > 0 {
        VerdictStatus::Unknown
    } else {
        VerdictStatus::Pass
    };
    let mut summary = format!(
        "flow verdict over {} judged step(s): {passes} pass, {fails} fail, {unknowns} unknown",
        step_verdicts.len()
    );
    if degraded {
        summary.push_str("; at least one verdict is degraded");
        if status == VerdictStatus::Pass && policy == VerdictPolicy::Strict {
            status = VerdictStatus::Unknown;
            summary.push_str("; strict verdict policy folds a degraded pass to unknown");
        }
    }
    Some(FoldedVerdict {
        status,
        degraded,
        summary,
    })
}

fn count(verdicts: &[(VerdictStatus, bool)], status: VerdictStatus) -> usize {
    verdicts.iter().filter(|(s, _)| *s == status).count()
}
