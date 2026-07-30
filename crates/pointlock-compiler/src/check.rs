//! Phase 3 `check`: normalized structure → typed steps + reference
//! legality (spine §8 stage 3, 03 §4.3 group C).
//!
//! - step id uniqueness over the whole tree — nested `if`/`foreach` bodies
//!   and handler-synthesized escalate ids included (02 §14) — and author
//!   grammar (a single [`StepId`] segment; `:` segments are legal only on
//!   compiler-synthesized ids, i.e. macro-expanded steps, 02 §3);
//! - `${{ ... }}` surface text compiled to [`pointlock_ir::Expr`] (grammar
//!   02 §8.1, via [`crate::expr_text`]) — invoke args, `set_value.value`,
//!   `call.inputs`, `if.cond`, `foreach.in`, `let` bindings, `human`
//!   presents, `outputs.from`, and `expr` predicates;
//! - reference resolution over the closed scope: `params.*` / `env.*`
//!   (closed M1 list: `deviceId` `platform` `runId`, 03 §5-8) / `vars.*`
//!   (after their `let`, SSA) / `iter.<as>` (foreach body only) /
//!   `steps.<topologically earlier id>.output.*` / `steps.<id>.verdict`;
//!   the single self-ref exception is `steps.<self>.output.*` inside this
//!   step's `expect` expr predicates (02 §4.1.1) — `preflight` and every
//!   argument position reject self-refs;
//! - branch isolation: `then` and `else` are mutually invisible; both
//!   branches' ids become visible after the `if` (document-order
//!   topology);
//! - `call.inputs` versus the callee's declared `params` (03 §4.3 F3);
//! - the expression-local static obligations delegated to
//!   [`pointlock_expr::check`] (arity table, literal-only positions);
//! - content-derived assert ids for every predicate type (03 §1.5);
//! - the R13 default-escalation lint (03 §4.3 F6, **warning**): a flow
//!   with no `on_unknown` disposition anywhere is flagged, not rejected.

use std::collections::{BTreeSet, HashSet};

use pointlock_expr::{CheckDiagnostic, ScopeShape};
use pointlock_ir::vocab::{
    CanonicalVerb, EffectClassAction, ElementState, ErrorClass, HandlerHook, HumanMode,
    VerdictPolicy,
};
use pointlock_ir::{
    ActionName, AssertId, ElementSelectorIR, Expr, FeatureId, FlowId, FlowRef, Identifier,
    JsonSchemaDocument, MacroOriginFrame, PredicateIR, RetryPolicy, StepId, domain_hash,
};

use crate::diagnostic::{CompileDiagnostic, DiagnosticSpan, codes, has_error};
use crate::expr_text::{ExprTextError, parse_expr_text};
use crate::normalize::{
    NormalizedAssertion, NormalizedChain, NormalizedCoordinate, NormalizedFlow, NormalizedHandler,
    NormalizedHandlerAction, NormalizedHead, NormalizedHuman, NormalizedParam, NormalizedPredicate,
    NormalizedStep, NormalizedStepKind, NormalizedValue,
};
use crate::subflow::SubflowRecord;

/// The closed `env.*` name list of v0.1 (03 §1.9, pending spine
/// incorporation per 03 §5-8).
const ENV_NAMES: &[&str] = &["deviceId", "platform", "runId"];

/// Typed output of the check phase.
#[derive(Debug)]
pub(crate) struct CheckedFlow {
    /// The flow id.
    pub flow_id: FlowId,
    /// The validated provider name.
    pub provider: String,
    /// Verdict folding policy.
    pub verdict_policy: VerdictPolicy,
    /// Declared parameters (passed through from normalize).
    pub params: Vec<NormalizedParam>,
    /// Typed output declarations.
    pub outputs: Vec<CheckedOutput>,
    /// Flow-level handlers (escalate ids synthesized).
    pub handlers: Vec<CheckedHandler>,
    /// Typed steps in body order.
    pub steps: Vec<CheckedStep>,
}

/// One typed flow output.
#[derive(Debug)]
pub(crate) struct CheckedOutput {
    /// Output name.
    pub name: Identifier,
    /// The declared value contract.
    pub schema: JsonSchemaDocument,
    /// The compiled projection expression.
    pub from: Expr,
}

/// One typed step.
#[derive(Debug)]
pub(crate) struct CheckedStep {
    /// Validated step id.
    pub step_id: StepId,
    /// Macro expansion chain, innermost first (into `sourceMap.origin`).
    pub origin: Vec<MacroOriginFrame>,
    /// The kind-specific body.
    pub kind: CheckedStepKind,
    /// Typed preflight probes.
    pub preflight: Vec<CheckedAssertion>,
    /// Act-phase retry policy (action steps only).
    pub retry: Option<RetryPolicy>,
    /// Optional step budget (human steps carry theirs in the kind).
    pub timeout_ms: Option<u64>,
    /// Checkpoint flag (defaults already materialized).
    pub checkpoint: bool,
    /// Step-level handlers (escalate ids synthesized).
    pub handlers: Vec<CheckedHandler>,
    /// Span of the whole step.
    pub span: DiagnosticSpan,
}

/// The kind-specific body of a typed step.
#[derive(Debug)]
pub(crate) enum CheckedStepKind {
    /// → `ActionStepIR`.
    Action(Box<CheckedAction>),
    /// → `AssertStepIR` (`observe: "fresh"`).
    Assert {
        /// The assertions (≥ 1).
        assertions: Vec<CheckedAssertion>,
    },
    /// → `CallStepIR`.
    Call(Box<CheckedCall>),
    /// → `HumanStepIR`.
    Human(Box<CheckedHuman>),
    /// → `IfStepIR`.
    If(Box<CheckedIf>),
    /// → `ForeachStepIR`.
    Foreach(Box<CheckedForeach>),
    /// → `LetStepIR`.
    Let {
        /// The bindings in declaration order.
        bindings: Vec<(Identifier, Expr)>,
    },
}

/// A typed action step body (M1 shape, unchanged).
#[derive(Debug)]
pub(crate) struct CheckedAction {
    /// The typed head form.
    pub head: CheckedHead,
    /// Declared or verb-inferred effect (bind rejects absence on invoke).
    pub effect: Option<EffectClassAction>,
    /// Author-declared idempotence.
    pub idempotent: bool,
    /// Author-declared act-chain, if any (bind derives the default).
    pub locate_via: Option<NormalizedChain>,
    /// Static coordinate key, if any.
    pub coordinate: Option<NormalizedCoordinate>,
    /// Typed assertions.
    pub assertions: Vec<CheckedAssertion>,
    /// The invoke-output narrowing contract (03 §1.3), when declared.
    pub output_schema: Option<pointlock_ir::JsonSchemaDocument>,
}

/// A typed call step body (02 §6).
#[derive(Debug)]
pub(crate) struct CheckedCall {
    /// The content-pinned callee reference.
    pub flow_ref: FlowRef,
    /// The callee's feature union (merged into the caller at bind).
    pub required_features: BTreeSet<FeatureId>,
    /// Caller-scope input expressions.
    pub inputs: Vec<CheckedArg>,
}

/// A typed human step body (02 §4.4).
#[derive(Debug)]
pub(crate) struct CheckedHuman {
    /// Interaction mode.
    pub mode: HumanMode,
    /// The question posed to the human.
    pub prompt: String,
    /// Compiled presents expressions.
    pub presents: Vec<Expr>,
    /// Enumerated options.
    pub decisions: Option<Vec<String>>,
    /// Input contract (`provideInput`).
    pub output_schema: Option<JsonSchemaDocument>,
    /// Mandatory budget (normalize guaranteed exactly one surface).
    pub timeout_ms: u64,
}

/// A typed if step body.
#[derive(Debug)]
pub(crate) struct CheckedIf {
    /// The compiled condition.
    pub cond: Expr,
    /// The then-branch.
    pub then: Vec<CheckedStep>,
    /// The optional else-branch.
    pub r#else: Option<Vec<CheckedStep>>,
}

/// A typed foreach step body.
#[derive(Debug)]
pub(crate) struct CheckedForeach {
    /// The compiled collection expression.
    pub items: Expr,
    /// The iteration variable name.
    pub r#as: Identifier,
    /// The loop body.
    pub body: Vec<CheckedStep>,
}

/// One typed handler binding.
#[derive(Debug)]
pub(crate) struct CheckedHandler {
    /// The hook.
    pub hook: HandlerHook,
    /// Error-class filter (`on_error` only).
    pub error_classes: Option<BTreeSet<ErrorClass>>,
    /// The typed disposition.
    pub action: CheckedHandlerAction,
    /// Trigger budget.
    pub max_triggers: u32,
}

/// A typed handler disposition.
#[derive(Debug)]
pub(crate) enum CheckedHandlerAction {
    /// Whole-step re-entry.
    Retry(RetryPolicy),
    /// Record and move on.
    Continue,
    /// Escalate to the embedded human node.
    Escalate {
        /// The compiler-synthesized step id (`<host>:<hook>:escalate`).
        step_id: StepId,
        /// The typed human material.
        human: Box<CheckedHuman>,
    },
    /// Abort the run.
    Abort,
    /// Run a repair subflow.
    Repair {
        /// The content-pinned repair reference.
        flow_ref: FlowRef,
        /// The repair flow's feature union.
        required_features: BTreeSet<FeatureId>,
    },
}

/// The typed head-key form of an action step.
#[derive(Debug)]
pub(crate) enum CheckedHead {
    /// `invoke` — explicit native action + args.
    Invoke {
        /// The provider-native action name.
        action: ActionName,
        /// Span of the action value (bind diagnostics point here).
        action_span: DiagnosticSpan,
        /// Typed invoke arguments.
        args: Vec<CheckedArg>,
    },
    /// One of the five element verbs (03 §1.3). Boxed to keep the enum
    /// small (the selector dominates the variant size).
    Verb(Box<CheckedVerb>),
}

/// One typed element-verb head.
#[derive(Debug)]
pub(crate) struct CheckedVerb {
    /// The canonical verb.
    pub verb: CanonicalVerb,
    /// Span of the head key (bind diagnostics point here).
    pub head_span: DiagnosticSpan,
    /// The static element selector.
    pub element: ElementSelectorIR,
    /// Span of the `element` value.
    pub element_span: DiagnosticSpan,
    /// `set_value` only: the compiled value expression.
    pub value: Option<CheckedArg>,
    /// `wait_for` only: the awaited element state.
    pub state: Option<ElementState>,
}

/// One typed argument.
#[derive(Debug)]
pub(crate) struct CheckedArg {
    /// Argument name (verb surface name for verbs; native name for invoke;
    /// callee param name for call inputs).
    pub name: Identifier,
    /// Span of the argument key (or value, for verb subkeys).
    pub name_span: DiagnosticSpan,
    /// The compiled expression (literals become `Expr::Lit`).
    pub expr: Expr,
    /// Whether the value contains references or function applications —
    /// such fields skip the static inputSchema shape check and are
    /// re-validated at runtime after evaluation (spine §7).
    pub dynamic: bool,
}

/// One typed assertion: a compiled predicate plus its surface verify-chain
/// material (chain legality is bind-phase, 03 §1.4).
#[derive(Debug)]
pub(crate) struct CheckedAssertion {
    /// Stable assert id (explicit or content-derived).
    pub assert_id: AssertId,
    /// The compiled predicate.
    pub predicate: PredicateIR,
    /// Author-declared verify-chain, if any.
    pub verify_via: Option<NormalizedChain>,
    /// The `visual:` vision-tail prompt of an element predicate, if any.
    pub visual: Option<(String, DiagnosticSpan)>,
    /// Span of the whole assertion item.
    pub span: DiagnosticSpan,
}

/// Reference position: the self-ref and topology rules differ per
/// evaluation site (02 §8.3 / §4.1.1).
#[derive(Clone, Copy, PartialEq)]
enum RefPosition {
    /// Argument snapshot at `ready` — self-ref illegal.
    Args,
    /// `expect` expr predicates — `steps.<self>.output.*` legal.
    Assertion,
    /// `preflight` expr predicates (`probing`) — self-ref illegal.
    Preflight,
    /// Handler material / flow outputs — no topology (fires out of the
    /// normal control flow; outputs evaluate at flow end).
    Deferred,
}

/// The wire literal of a handler hook (spine A.4, camelCase).
pub(crate) fn hook_wire(hook: HandlerHook) -> &'static str {
    match hook {
        HandlerHook::OnFail => "onFail",
        HandlerHook::OnUnknown => "onUnknown",
        HandlerHook::OnError => "onError",
        HandlerHook::OnResumeDrift => "onResumeDrift",
    }
}

/// Checks the normalized flow. On success the second component carries the
/// warnings (03 §4.3 F6); on failure every diagnostic (errors and
/// warnings) is returned.
pub(crate) fn check(
    flow: NormalizedFlow,
) -> Result<(CheckedFlow, Vec<CompileDiagnostic>), Vec<CompileDiagnostic>> {
    let mut diags = Vec::new();

    // Pass 1: id grammar and whole-tree uniqueness (02 §14: nested bodies
    // and handler-synthesized escalate ids included).
    let mut seen: HashSet<String> = HashSet::new();
    collect_handler_ids("flow", &flow.handlers, &mut seen, flow.span, &mut diags);
    collect_step_ids(&flow.steps, &mut seen, &mut diags);
    let all_ids = seen;

    let shape = ScopeShape {
        params: flow
            .params
            .iter()
            .map(|param| param.name.to_string())
            .collect(),
        env: ENV_NAMES.iter().map(ToString::to_string).collect(),
        vars: BTreeSet::new(),
        iter: BTreeSet::new(),
        steps: all_ids.iter().cloned().collect(),
    };

    // The invoke-output contract tables (03 §1.3 / C7): narrowed
    // invokes type their fields; un-narrowed ones refuse field access.
    let mut invoke_outputs = std::collections::BTreeMap::new();
    let mut unnarrowed_invokes = HashSet::new();
    collect_invoke_outputs(&flow.steps, &mut invoke_outputs, &mut unnarrowed_invokes);

    let ref_types = FlowRefTypes {
        params: flow
            .params
            .iter()
            .filter_map(|param| {
                schema_type(param.schema.as_value()).map(|kind| (param.name.to_string(), kind))
            })
            .collect(),
        invoke_outputs,
    };
    let mut checker = Checker {
        all_ids,
        shape,
        ref_types,
        unnarrowed_invokes,
        saw_on_unknown: false,
        diags,
    };

    checker.saw_on_unknown |= flow
        .handlers
        .iter()
        .any(|handler| handler.hook == HandlerHook::OnUnknown);
    let handlers = checker.check_handlers("flow", flow.handlers);

    let mut earlier: HashSet<String> = HashSet::new();
    let steps = checker.check_step_list(flow.steps, &mut earlier);

    // F2: outputs.from resolve over the closed scope; outputs evaluate at
    // flow end, so every step is visible and there is no self.
    let mut outputs = Vec::new();
    for output in flow.outputs {
        if let Some(arg) = checker.check_value(
            output.name.clone(),
            value_span(&output.from),
            &output.from,
            RefPosition::Deferred,
            "",
            &earlier,
        ) {
            outputs.push(CheckedOutput {
                name: output.name,
                schema: output.schema,
                from: arg.expr,
            });
        }
    }

    // R13 lint (03 §1.8 / §4.3 F6): silence on `unknown` must be a choice.
    if !checker.saw_on_unknown {
        checker.diags.push(CompileDiagnostic::warning(
            codes::LINT_NO_UNKNOWN_DISPOSITION,
            "the flow has no disposition for `unknown` (no flow-level or step-level \
             `on_unknown` handler); the default posture is escalate-to-human (R13) — \
             delete deliberately, not silently",
            Some(flow.span),
        ));
    }

    let Checker { diags, .. } = checker;
    if has_error(&diags) {
        Err(diags)
    } else {
        Ok((
            CheckedFlow {
                flow_id: flow.flow_id,
                provider: flow.provider,
                verdict_policy: flow.verdict_policy,
                params: flow.params,
                outputs,
                handlers,
                steps,
            },
            diags,
        ))
    }
}

/// The span carried by a normalized value.
fn value_span(value: &NormalizedValue) -> DiagnosticSpan {
    match value {
        NormalizedValue::Literal { span, .. } | NormalizedValue::ExprText { span, .. } => *span,
    }
}

/// Registers one id, diagnosing duplicates.
fn insert_unique(
    id: &str,
    span: DiagnosticSpan,
    seen: &mut HashSet<String>,
    diags: &mut Vec<CompileDiagnostic>,
) {
    if !seen.insert(id.to_owned()) {
        diags.push(CompileDiagnostic::error(
            codes::CHECK_DUPLICATE_STEP_ID,
            format!("duplicate step id '{id}'"),
            Some(span),
        ));
    }
}

/// Collects the synthesized escalate ids of a handler list (02 §3:
/// `<host>:<hook>:escalate`).
fn collect_handler_ids(
    host: &str,
    handlers: &[NormalizedHandler],
    seen: &mut HashSet<String>,
    span: DiagnosticSpan,
    diags: &mut Vec<CompileDiagnostic>,
) {
    for handler in handlers {
        if matches!(handler.action, NormalizedHandlerAction::Escalate(_)) {
            let id = format!("{host}:{}:escalate", hook_wire(handler.hook));
            insert_unique(&id, handler.span, seen, diags);
            let _ = span;
        }
    }
}

/// Collects and grammar-checks every step id in the tree.
fn collect_step_ids(
    steps: &[NormalizedStep],
    seen: &mut HashSet<String>,
    diags: &mut Vec<CompileDiagnostic>,
) {
    for step in steps {
        match StepId::new(step.id.clone()) {
            // Author-written ids are a single segment; `:` segments are
            // legal only on macro-synthesized ids (origin non-empty).
            Ok(step_id) if step_id.is_synthesized() && step.origin.is_empty() => {
                diags.push(CompileDiagnostic::error(
                    codes::CHECK_BAD_STEP_ID,
                    format!(
                        "step id '{}' must not contain ':' (reserved for compiler-synthesized \
                         ids)",
                        step.id
                    ),
                    Some(step.id_span),
                ));
            }
            Ok(_) => {}
            Err(err) => diags.push(CompileDiagnostic::error(
                codes::CHECK_BAD_STEP_ID,
                format!("invalid step id '{}': {}", step.id, err.reason),
                Some(step.id_span),
            )),
        }
        insert_unique(&step.id, step.id_span, seen, diags);
        collect_handler_ids(&step.id, &step.handlers, seen, step.span, diags);
        match &step.kind {
            NormalizedStepKind::If(if_step) => {
                collect_step_ids(&if_step.then, seen, diags);
                if let Some(else_steps) = &if_step.r#else {
                    collect_step_ids(else_steps, seen, diags);
                }
            }
            NormalizedStepKind::Foreach(foreach) => {
                collect_step_ids(&foreach.body, seen, diags);
            }
            _ => {}
        }
    }
}

/// Per-flow checking state. `shape.vars` and `shape.iter` are mutated
/// along the walk (SSA products and live iteration bindings).
/// The flow's reference-type knowledge (C5/C6 gradual typing): `params.*`
/// from the declared param schemas' `type`, `env.*` are strings by
/// construction (every binding the runner seeds is one), everything else —
/// step outputs, vars, iteration items — is `Unknown` until those carry
/// static types of their own. `Unknown` never errors, so the layer only
/// ever REFUSES definite contradictions.
struct FlowRefTypes {
    /// param name → declared JSON `type`, when the schema pins one.
    params: std::collections::BTreeMap<String, pointlock_expr::JsonType>,
    /// invoke step id → its `expect_schema` document (03 §1.3): the
    /// narrowing that makes `steps.<id>.output.<field>` a typed
    /// position for C5/C6.
    invoke_outputs: std::collections::BTreeMap<String, serde_json::Value>,
}

impl pointlock_expr::RefTypes for FlowRefTypes {
    fn type_of(&self, path: &pointlock_ir::RefPath) -> pointlock_expr::ExprType {
        let mut segments = path.as_str().split('.');
        let root = segments.next();
        let name = segments.next();
        match (root, name) {
            (Some("params"), Some(name)) => {
                if segments.next().is_some() {
                    // A deeper path digs INTO the value; its type is not
                    // the param's own.
                    pointlock_expr::ExprType::Unknown
                } else {
                    self.params
                        .get(name)
                        .copied()
                        .map(pointlock_expr::ExprType::Known)
                        .unwrap_or(pointlock_expr::ExprType::Unknown)
                }
            }
            (Some("env"), Some(_)) if segments.clone().next().is_none() => {
                pointlock_expr::ExprType::Known(pointlock_expr::JsonType::String)
            }
            // steps.<id>.output.<field...>: dig through the declared
            // narrowing schema's `properties`, field by field.
            (Some("steps"), Some(step_id)) => {
                if segments.next() != Some("output") {
                    return pointlock_expr::ExprType::Unknown;
                }
                let Some(schema) = self.invoke_outputs.get(step_id) else {
                    return pointlock_expr::ExprType::Unknown;
                };
                let mut cursor = schema;
                for field in segments {
                    match cursor.get("properties").and_then(|p| p.get(field)) {
                        Some(next) => cursor = next,
                        None => return pointlock_expr::ExprType::Unknown,
                    }
                }
                schema_type(cursor)
                    .map(pointlock_expr::ExprType::Known)
                    .unwrap_or(pointlock_expr::ExprType::Unknown)
            }
            _ => pointlock_expr::ExprType::Unknown,
        }
    }
}

/// Walks the normalized tree collecting the invoke-output contract
/// tables (03 §1.3): authored `invoke` steps land either in the
/// narrowed map (with their `expect_schema` document) or the
/// un-narrowed C7 set. Observation-lowered invokes are exempt — their
/// output shape is documented (04 §9.4.3). `call` is a hard boundary;
/// handlers have no outputs (C4).
fn collect_invoke_outputs(
    steps: &[crate::normalize::NormalizedStep],
    narrowed: &mut std::collections::BTreeMap<String, serde_json::Value>,
    unnarrowed: &mut HashSet<String>,
) {
    use crate::normalize::{NormalizedHead, NormalizedStepKind};
    for step in steps {
        match &step.kind {
            NormalizedStepKind::Action(action) => {
                if matches!(action.head, NormalizedHead::Invoke { .. }) && !action.observation_head
                {
                    match &action.output_schema {
                        Some(schema) => {
                            narrowed.insert(step.id.clone(), schema.as_value().clone());
                        }
                        None => {
                            unnarrowed.insert(step.id.clone());
                        }
                    }
                }
            }
            NormalizedStepKind::If(if_step) => {
                collect_invoke_outputs(&if_step.then, narrowed, unnarrowed);
                if let Some(otherwise) = &if_step.r#else {
                    collect_invoke_outputs(otherwise, narrowed, unnarrowed);
                }
            }
            NormalizedStepKind::Foreach(foreach) => {
                collect_invoke_outputs(&foreach.body, narrowed, unnarrowed);
            }
            _ => {}
        }
    }
}

/// Maps a JSON Schema `type` keyword onto the expression type lattice.
fn schema_type(schema: &serde_json::Value) -> Option<pointlock_expr::JsonType> {
    match schema.get("type").and_then(serde_json::Value::as_str) {
        Some("boolean") => Some(pointlock_expr::JsonType::Boolean),
        Some("number") | Some("integer") => Some(pointlock_expr::JsonType::Number),
        Some("string") => Some(pointlock_expr::JsonType::String),
        Some("array") => Some(pointlock_expr::JsonType::Array),
        Some("object") => Some(pointlock_expr::JsonType::Object),
        Some("null") => Some(pointlock_expr::JsonType::Null),
        _ => None,
    }
}

struct Checker {
    /// Every step id in the tree (forward-ref vs undefined distinction).
    all_ids: HashSet<String>,
    /// Authored `invoke` steps with NO `expect_schema` (C7: taking a
    /// field from their output is a rejection, 03 §1.3).
    unnarrowed_invokes: HashSet<String>,
    /// The declared-name shape for the expression-local check.
    shape: ScopeShape,
    /// The reference-type knowledge for the C5/C6 inference.
    ref_types: FlowRefTypes,
    /// Whether any `on_unknown` disposition exists (R13 lint).
    saw_on_unknown: bool,
    /// Accumulated diagnostics (errors and warnings).
    diags: Vec<CompileDiagnostic>,
}

impl Checker {
    fn check_step_list(
        &mut self,
        steps: Vec<NormalizedStep>,
        earlier: &mut HashSet<String>,
    ) -> Vec<CheckedStep> {
        steps
            .into_iter()
            .filter_map(|step| self.check_step(step, earlier))
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    fn check_step(
        &mut self,
        step: NormalizedStep,
        earlier: &mut HashSet<String>,
    ) -> Option<CheckedStep> {
        let id = step.id.clone();
        self.saw_on_unknown |= step
            .handlers
            .iter()
            .any(|handler| handler.hook == HandlerHook::OnUnknown);
        let handlers = self.check_handlers(&id, step.handlers);

        // Preflight probes evaluate at `probing`, before the step acts —
        // self-refs are illegal there (02 §4.1.1).
        let preflight = self.check_assertions(step.preflight, RefPosition::Preflight, &id, earlier);

        let kind = match step.kind {
            NormalizedStepKind::Action(action) => {
                let head = match action.head {
                    NormalizedHead::Invoke {
                        action: action_name,
                        action_span,
                        args,
                    } => {
                        let mut checked_args = Vec::new();
                        for arg in args {
                            if let Some(checked) = self.check_value(
                                arg.name,
                                arg.name_span,
                                &arg.value,
                                RefPosition::Args,
                                &id,
                                earlier,
                            ) {
                                checked_args.push(checked);
                            }
                        }
                        CheckedHead::Invoke {
                            action: action_name,
                            action_span,
                            args: checked_args,
                        }
                    }
                    NormalizedHead::Verb(verb) => {
                        let value = verb.value.as_ref().and_then(|value| {
                            self.check_value(
                                Identifier::new("value")
                                    .expect("'value' is a grammatical identifier"),
                                value_span(value),
                                value,
                                RefPosition::Args,
                                &id,
                                earlier,
                            )
                        });
                        CheckedHead::Verb(Box::new(CheckedVerb {
                            verb: verb.verb,
                            head_span: verb.head_span,
                            element: verb.element,
                            element_span: verb.element_span,
                            value,
                            state: verb.state,
                        }))
                    }
                };
                let assertions =
                    self.check_assertions(action.expect, RefPosition::Assertion, &id, earlier);
                CheckedStepKind::Action(Box::new(CheckedAction {
                    head,
                    effect: action.effect,
                    idempotent: action.idempotent,
                    locate_via: action.locate_via,
                    coordinate: action.coordinate,
                    assertions,
                    output_schema: action.output_schema,
                }))
            }
            NormalizedStepKind::Assert { expect } => {
                let assertions =
                    self.check_assertions(expect, RefPosition::Assertion, &id, earlier);
                CheckedStepKind::Assert { assertions }
            }
            NormalizedStepKind::Call(call) => {
                let mut inputs = Vec::new();
                for arg in &call.inputs {
                    if let Some(checked) = self.check_value(
                        arg.name.clone(),
                        arg.name_span,
                        &arg.value,
                        RefPosition::Args,
                        &id,
                        earlier,
                    ) {
                        inputs.push(checked);
                    }
                }
                self.check_call_contract(&id, &call.subflow, &inputs, call.call_span);
                CheckedStepKind::Call(Box::new(CheckedCall {
                    flow_ref: call.subflow.flow_ref.clone(),
                    required_features: call.subflow.required_features.clone(),
                    inputs,
                }))
            }
            NormalizedStepKind::Human(human) => {
                let human = self.check_human(*human, RefPosition::Args, &id, earlier)?;
                CheckedStepKind::Human(Box::new(human))
            }
            NormalizedStepKind::If(if_step) => {
                let cond_span = value_span(&if_step.cond);
                let cond = self
                    .check_value(
                        Identifier::new("cond").expect("'cond' is a grammatical identifier"),
                        cond_span,
                        &if_step.cond,
                        RefPosition::Args,
                        &id,
                        earlier,
                    )
                    .map(|arg| arg.expr);
                if let Some(cond) = &cond {
                    let mut sink = Vec::new();
                    let inferred = pointlock_expr::infer(cond, &self.ref_types, &mut sink);
                    // check_value already reported the C5 findings.
                    self.check_predicate_type(inferred, "`cond`", cond_span);
                }
                // Branch isolation: `then` and `else` are mutually
                // exclusive at runtime, so neither sees the other; after
                // the if, both branches' products are document-order
                // visible.
                let earlier_before = earlier.clone();
                let vars_before = self.shape.vars.clone();
                let then = self.check_step_list(if_step.then, earlier);
                let then_earlier = std::mem::replace(earlier, earlier_before);
                let then_vars = std::mem::replace(&mut self.shape.vars, vars_before);
                let r#else = if_step
                    .r#else
                    .map(|steps| self.check_step_list(steps, earlier));
                earlier.extend(then_earlier);
                self.shape.vars.extend(then_vars);
                CheckedStepKind::If(Box::new(CheckedIf {
                    cond: cond?,
                    then,
                    r#else,
                }))
            }
            NormalizedStepKind::Foreach(foreach) => {
                let items = self
                    .check_value(
                        Identifier::new("items").expect("'items' is a grammatical identifier"),
                        value_span(&foreach.items),
                        &foreach.items,
                        RefPosition::Args,
                        &id,
                        earlier,
                    )
                    .map(|arg| arg.expr);
                let as_name = foreach.r#as.to_string();
                if !self.shape.iter.insert(as_name.clone()) {
                    self.diags.push(CompileDiagnostic::error(
                        codes::CHECK_SSA_REBIND,
                        format!(
                            "iteration variable '{as_name}' shadows an enclosing `foreach` \
                             binding"
                        ),
                        Some(foreach.as_span),
                    ));
                }
                let body = self.check_step_list(foreach.body, earlier);
                self.shape.iter.remove(&as_name);
                CheckedStepKind::Foreach(Box::new(CheckedForeach {
                    items: items?,
                    r#as: foreach.r#as,
                    body,
                }))
            }
            NormalizedStepKind::Let { bindings } => {
                let mut checked = Vec::new();
                for binding in bindings {
                    let expr = self
                        .check_value(
                            binding.name.clone(),
                            binding.name_span,
                            &binding.value,
                            RefPosition::Args,
                            &id,
                            earlier,
                        )
                        .map(|arg| arg.expr);
                    // SSA single assignment (02 §4.7): a name binds once
                    // per flow; later bindings in the same `let` may read
                    // earlier ones.
                    if !self.shape.vars.insert(binding.name.to_string()) {
                        self.diags.push(CompileDiagnostic::error(
                            codes::CHECK_SSA_REBIND,
                            format!(
                                "`vars.{}` is already bound — SSA single assignment (02 §4.7)",
                                binding.name
                            ),
                            Some(binding.name_span),
                        ));
                        continue;
                    }
                    if let Some(expr) = expr {
                        checked.push((binding.name, expr));
                    }
                }
                CheckedStepKind::Let { bindings: checked }
            }
        };

        // RF3019 (warning): `retry_on` entries the retry machinery can
        // never fire on are dead weight — only `action_failed_retryable`
        // and `target_stale` retry unconditionally, and `action_timed_out`
        // only on an idempotent step (spine §5 / 07 §4.3). The flow still
        // compiles; the author just wrote a promise nothing keeps.
        if let Some(policy) = &step.retry {
            let idempotent = matches!(
                &kind,
                CheckedStepKind::Action(action) if action.idempotent
            );
            for class in &policy.retry_on {
                let live = matches!(
                    class,
                    pointlock_ir::ErrorClass::ActionFailedRetryable
                        | pointlock_ir::ErrorClass::TargetStale
                ) || (*class == pointlock_ir::ErrorClass::ActionTimedOut && idempotent);
                if !live {
                    let wire = serde_json::to_value(class).expect("ErrorClass serializes");
                    self.diags.push(CompileDiagnostic::warning(
                        codes::LINT_DEAD_RETRY_ON,
                        format!(
                            "step '{}': `retry_on` entry `{}` never retries — only \
                             `action_failed_retryable` and `target_stale` do, plus \
                             `action_timed_out` on an `idempotent: true` step (spine §5); \
                             the entry is dead",
                            step.id,
                            wire.as_str().unwrap_or("?")
                        ),
                        Some(step.span),
                    ));
                }
            }
        }

        earlier.insert(id);
        let step_id = StepId::new(step.id).ok()?;
        Some(CheckedStep {
            step_id,
            origin: step.origin,
            kind,
            preflight,
            retry: step.retry,
            timeout_ms: step.timeout_ms,
            checkpoint: step.checkpoint,
            handlers,
            span: step.span,
        })
    }

    /// Checks a human block's expressions and materializes the typed form.
    fn check_human(
        &mut self,
        human: NormalizedHuman,
        position: RefPosition,
        self_id: &str,
        earlier: &HashSet<String>,
    ) -> Option<CheckedHuman> {
        let mut presents = Vec::new();
        for value in &human.presents {
            if let Some(arg) = self.check_value(
                Identifier::new("presents").expect("'presents' is a grammatical identifier"),
                value_span(value),
                value,
                position,
                self_id,
                earlier,
            ) {
                presents.push(arg.expr);
            }
        }
        Some(CheckedHuman {
            mode: human.mode,
            prompt: human.prompt,
            presents,
            decisions: human.decisions,
            output_schema: human.output_schema,
            timeout_ms: human
                .timeout_ms
                .expect("normalize guaranteed exactly one timeout surface"),
        })
    }

    /// Checks one handler list, synthesizing escalate step ids
    /// (`<host>:<hook>:escalate`, 02 §3). Handlers fire outside the normal
    /// control flow, so their expressions skip step topology.
    fn check_handlers(
        &mut self,
        host: &str,
        handlers: Vec<NormalizedHandler>,
    ) -> Vec<CheckedHandler> {
        let earlier: HashSet<String> = self.all_ids.clone();
        let mut checked = Vec::new();
        for handler in handlers {
            let action = match handler.action {
                NormalizedHandlerAction::Retry(policy) => CheckedHandlerAction::Retry(policy),
                NormalizedHandlerAction::Continue => CheckedHandlerAction::Continue,
                NormalizedHandlerAction::Abort => CheckedHandlerAction::Abort,
                NormalizedHandlerAction::Repair { subflow } => {
                    let SubflowRecord {
                        flow_ref,
                        required_features,
                        ..
                    } = subflow;
                    CheckedHandlerAction::Repair {
                        flow_ref,
                        required_features,
                    }
                }
                NormalizedHandlerAction::Escalate(human) => {
                    let raw = format!("{host}:{}:escalate", hook_wire(handler.hook));
                    let step_id = match StepId::new(raw.clone()) {
                        Ok(step_id) => step_id,
                        Err(err) => {
                            self.diags.push(CompileDiagnostic::error(
                                codes::CHECK_BAD_STEP_ID,
                                format!("synthesized handler step id '{raw}': {}", err.reason),
                                Some(handler.span),
                            ));
                            continue;
                        }
                    };
                    let Some(human) =
                        self.check_human(human, RefPosition::Deferred, host, &earlier)
                    else {
                        continue;
                    };
                    CheckedHandlerAction::Escalate {
                        step_id,
                        human: Box::new(human),
                    }
                }
            };
            checked.push(CheckedHandler {
                hook: handler.hook,
                error_classes: handler.error_classes,
                action,
                max_triggers: handler.max_triggers,
            });
        }
        checked
    }

    /// F3 (03 §4.3): `call.inputs` versus the callee's declared params —
    /// unknown inputs, missing required inputs, and literal inputs that
    /// fail the param schema are compile errors; expression inputs defer
    /// to the runtime contract check.
    fn check_call_contract(
        &mut self,
        step_id: &str,
        subflow: &SubflowRecord,
        inputs: &[CheckedArg],
        call_span: DiagnosticSpan,
    ) {
        for param in &subflow.params {
            if param.required
                && !param.has_default
                && !inputs.iter().any(|input| input.name.as_str() == param.name)
            {
                self.diags.push(CompileDiagnostic::error(
                    codes::CHECK_CALL_INPUT_CONTRACT,
                    format!(
                        "step '{step_id}': required input '{}' of subflow '{}' is missing",
                        param.name, subflow.flow_ref.flow_id
                    ),
                    Some(call_span),
                ));
            }
        }
        for input in inputs {
            let Some(param) = subflow
                .params
                .iter()
                .find(|param| param.name == input.name.as_str())
            else {
                self.diags.push(CompileDiagnostic::error(
                    codes::CHECK_CALL_INPUT_CONTRACT,
                    format!(
                        "step '{step_id}': subflow '{}' declares no param '{}' (declared: {})",
                        subflow.flow_ref.flow_id,
                        input.name,
                        subflow
                            .params
                            .iter()
                            .map(|param| param.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Some(input.name_span),
                ));
                continue;
            };
            if input.dynamic {
                continue; // Runtime re-validation covers expression inputs.
            }
            let Expr::Lit(literal) = &input.expr else {
                unreachable!("non-dynamic args are literals by construction");
            };
            if let Err(err) = jsonschema::validate(param.schema.as_value(), &literal.lit) {
                self.diags.push(CompileDiagnostic::error(
                    codes::CHECK_CALL_INPUT_CONTRACT,
                    format!(
                        "step '{step_id}': input '{}' fails the param schema of subflow \
                         '{}': {err}",
                        input.name, subflow.flow_ref.flow_id
                    ),
                    Some(input.name_span),
                ));
            }
        }
    }

    /// Checks one assertion list (expect or preflight), deriving stable
    /// assert ids and rejecting duplicates within the list.
    fn check_assertions(
        &mut self,
        assertions: Vec<NormalizedAssertion>,
        position: RefPosition,
        self_id: &str,
        earlier: &HashSet<String>,
    ) -> Vec<CheckedAssertion> {
        let mut checked: Vec<CheckedAssertion> = Vec::new();
        let mut assert_ids: HashSet<String> = HashSet::new();
        for assertion in assertions {
            let predicate = match assertion.predicate {
                NormalizedPredicate::Expr { inner, span } => {
                    let Some(expr) = self.compile_expr(&inner, span) else {
                        continue;
                    };
                    let inferred = self.check_expr(&expr, span, position, self_id, earlier);
                    self.check_predicate_type(inferred, "the `expr` predicate", span);
                    PredicateIR::Expr { expr }
                }
                NormalizedPredicate::ElementState { selector, state } => {
                    PredicateIR::ElementState { selector, state }
                }
                NormalizedPredicate::ElementText { selector, matcher } => {
                    PredicateIR::ElementText {
                        selector,
                        r#match: matcher,
                    }
                }
                NormalizedPredicate::Visual { prompt, region } => {
                    PredicateIR::Visual { prompt, region }
                }
            };
            let (assert_id, id_span) = match assertion.assert_id {
                Some((raw, span)) => match AssertId::new(raw.clone()) {
                    Ok(assert_id) => (assert_id, span),
                    Err(err) => {
                        self.diags.push(CompileDiagnostic::error(
                            codes::NORM_BAD_EXPECT_SHAPE,
                            format!("invalid assert_id '{raw}': {}", err.reason),
                            Some(span),
                        ));
                        continue;
                    }
                },
                None => (derive_assert_id(&predicate), assertion.span),
            };
            if !assert_ids.insert(assert_id.to_string()) {
                self.diags.push(CompileDiagnostic::error(
                    codes::CHECK_DUPLICATE_ASSERT_ID,
                    format!("duplicate assert id '{assert_id}' within step '{self_id}'"),
                    Some(id_span),
                ));
                continue;
            }
            checked.push(CheckedAssertion {
                assert_id,
                predicate,
                verify_via: assertion.verify_via,
                visual: assertion.visual,
                span: assertion.span,
            });
        }
        checked
    }

    /// Compiles one expression-bearing argument value and runs the
    /// reference topology + expression-local checks over it.
    fn check_value(
        &mut self,
        name: Identifier,
        name_span: DiagnosticSpan,
        value: &NormalizedValue,
        position: RefPosition,
        self_id: &str,
        earlier: &HashSet<String>,
    ) -> Option<CheckedArg> {
        let (expr, span) = match value {
            NormalizedValue::Literal { value, span } => (Expr::lit(value.clone()), *span),
            NormalizedValue::ExprText { inner, span } => (self.compile_expr(inner, *span)?, *span),
        };
        self.check_expr(&expr, span, position, self_id, earlier);
        Some(CheckedArg {
            name,
            name_span,
            dynamic: is_dynamic(&expr),
            expr,
        })
    }

    /// Compiles `${{ ... }}` inner text, translating failures to RF3xxx.
    fn compile_expr(&mut self, inner: &str, span: DiagnosticSpan) -> Option<Expr> {
        match parse_expr_text(inner) {
            Ok(expr) => Some(expr),
            Err(ExprTextError::Syntax { message }) => {
                self.diags.push(CompileDiagnostic::error(
                    codes::CHECK_EXPR_SYNTAX,
                    format!("expression syntax error: {message}"),
                    Some(span),
                ));
                None
            }
            Err(ExprTextError::UnknownFunction { name }) => {
                self.diags.push(CompileDiagnostic::error(
                    codes::CHECK_UNKNOWN_FUNCTION,
                    format!(
                        "unknown function '{name}' (whitelist: eq, ne, not, and, or, concat, \
                         len, coalesce, jsonPath, regexMatch)"
                    ),
                    Some(span),
                ));
                None
            }
            Err(ExprTextError::BadRef { path, reason }) => {
                self.diags.push(CompileDiagnostic::error(
                    codes::CHECK_BAD_REF_PATH,
                    format!("invalid reference '{path}': {reason}"),
                    Some(span),
                ));
                None
            }
        }
    }

    /// Runs reference topology plus the expression-local static check, and
    /// returns the inferred static type (C5: definite signature
    /// contradictions become diagnostics on the way).
    fn check_expr(
        &mut self,
        expr: &Expr,
        span: DiagnosticSpan,
        position: RefPosition,
        self_id: &str,
        earlier: &HashSet<String>,
    ) -> pointlock_expr::ExprType {
        self.check_step_refs(expr, span, position, self_id, earlier);
        for finding in pointlock_expr::check(expr, &self.shape) {
            self.diags.push(translate_expr_diagnostic(finding, span));
        }
        let mut type_findings = Vec::new();
        let inferred = pointlock_expr::infer(expr, &self.ref_types, &mut type_findings);
        for finding in type_findings {
            self.diags.push(translate_expr_diagnostic(finding, span));
        }
        inferred
    }

    /// C6 (03 §4.3): a predicate position must be boolean. `Unknown` stays
    /// legal — it resolves (or degrades to unknown) at evaluation; only a
    /// DEFINITE non-boolean is refused, because it can never hold.
    fn check_predicate_type(
        &mut self,
        inferred: pointlock_expr::ExprType,
        what: &str,
        span: DiagnosticSpan,
    ) {
        if let pointlock_expr::ExprType::Known(concrete) = inferred
            && concrete != pointlock_expr::JsonType::Boolean
        {
            self.diags.push(CompileDiagnostic::error(
                codes::CHECK_PREDICATE_NOT_BOOLEAN,
                format!(
                    "{what} is statically {concrete:?}, never boolean — a predicate position \
                     must evaluate to a boolean (03 §4.3 C6)"
                ),
                Some(span),
            ));
        }
    }

    /// Step-reference topology: only topologically earlier steps are
    /// C7 (03 §1.3/§4.3): a field taken from an un-narrowed `invoke`
    /// output is a rejection — unknown-typed data must be narrowed via
    /// `expect_schema` before dereferencing. The WHOLE output
    /// (`steps.<id>.output` with no field) stays legal: passing an
    /// opaque value on is not dereferencing it.
    fn check_narrowed_output(&mut self, path: &str, step_id: &str, span: DiagnosticSpan) {
        if !self.unnarrowed_invokes.contains(step_id) {
            return;
        }
        let field_access = path
            .strip_prefix(&format!("steps.{step_id}.output."))
            .is_some_and(|rest| !rest.is_empty());
        if field_access {
            self.diags.push(
                CompileDiagnostic::error(
                    codes::CHECK_UNNARROWED_INVOKE_OUTPUT,
                    format!(
                        "'{path}': step '{step_id}' is an `invoke` with an unknown output — \
                         a field cannot be taken from it before narrowing (C7, 03 §1.3)"
                    ),
                    Some(span),
                )
                .with_hint(format!(
                    "declare `expect_schema` on step '{step_id}' to narrow its output \
                     contract"
                )),
            );
        }
    }

    /// visible; the single self-ref exception is `steps.<self>.output.*`
    /// in assertion position (02 §4.1.1). `Deferred` positions skip
    /// topology (handlers fire out of band; outputs evaluate at flow end).
    fn check_step_refs(
        &mut self,
        expr: &Expr,
        span: DiagnosticSpan,
        position: RefPosition,
        self_id: &str,
        earlier: &HashSet<String>,
    ) {
        match expr {
            Expr::Lit(_) => {}
            Expr::Ref(reference) => {
                let Some(step_id) = reference.r#ref.referenced_step() else {
                    return;
                };
                let path = reference.r#ref.as_str();
                self.check_narrowed_output(path, step_id, span);
                if position == RefPosition::Deferred {
                    if !self.all_ids.contains(step_id) {
                        self.diags.push(CompileDiagnostic::error(
                            codes::CHECK_UNDEFINED_STEP,
                            format!("'{path}': step '{step_id}' does not exist in this flow"),
                            Some(span),
                        ));
                    }
                    return;
                }
                if step_id == self_id {
                    let output_self_ref = path.starts_with(&format!("steps.{step_id}.output"));
                    if position == RefPosition::Assertion && output_self_ref {
                        return; // The one legal self-ref surface (02 §4.1.1).
                    }
                    self.diags.push(CompileDiagnostic::error(
                        codes::CHECK_ILLEGAL_SELF_REF,
                        format!(
                            "'{path}': illegal self-reference (only `expect` expr predicates \
                             may read steps.{self_id}.output.*, 02 §4.1.1)"
                        ),
                        Some(span),
                    ));
                } else if earlier.contains(step_id) {
                    // Legal: topologically earlier.
                } else if self.all_ids.contains(step_id) {
                    self.diags.push(CompileDiagnostic::error(
                        codes::CHECK_FORWARD_REF,
                        format!(
                            "'{path}': forward reference — step '{step_id}' is not \
                             topologically earlier than '{self_id}' (unentered branches and \
                             later steps are not visible)"
                        ),
                        Some(span),
                    ));
                } else {
                    self.diags.push(CompileDiagnostic::error(
                        codes::CHECK_UNDEFINED_STEP,
                        format!("'{path}': step '{step_id}' does not exist in this flow"),
                        Some(span),
                    ));
                }
            }
            Expr::Fn(call) => {
                for arg in &call.args {
                    self.check_step_refs(arg, span, position, self_id, earlier);
                }
            }
        }
    }
}

/// Whether the expression is anything beyond a plain literal. Function
/// applications count as dynamic even over literal arguments — no constant
/// folding, so their value exists only after evaluation.
fn is_dynamic(expr: &Expr) -> bool {
    !matches!(expr, Expr::Lit(_))
}

/// Maps `pointlock_expr` findings onto the RF3xxx code table.
fn translate_expr_diagnostic(finding: CheckDiagnostic, span: DiagnosticSpan) -> CompileDiagnostic {
    let (code, message) = match &finding {
        CheckDiagnostic::UndeclaredName { .. } => {
            (codes::CHECK_UNDECLARED_NAME, finding.to_string())
        }
        CheckDiagnostic::UndeclaredStep { .. } => {
            (codes::CHECK_UNDEFINED_STEP, finding.to_string())
        }
        CheckDiagnostic::ArityMismatch { .. } => (codes::CHECK_ARITY, finding.to_string()),
        CheckDiagnostic::ArgumentType { .. } | CheckDiagnostic::NotComparable { .. } => {
            (codes::CHECK_TYPE_MISMATCH, finding.to_string())
        }
        CheckDiagnostic::LitRequired { .. } | CheckDiagnostic::LitNotString { .. } => {
            (codes::CHECK_LIT_ONLY, finding.to_string())
        }
        CheckDiagnostic::InvalidJsonPathLiteral { .. }
        | CheckDiagnostic::InvalidRegexLiteral { .. }
        | CheckDiagnostic::InvalidRegexFlags { .. } => {
            (codes::CHECK_BAD_FN_LITERAL, finding.to_string())
        }
    };
    CompileDiagnostic::error(code, message, Some(span))
}

/// Derives a stable, content-addressed assert id (03 §1.5: the default id
/// is derived from the assertion content).
///
/// `expr` predicates keep the M0 derivation (a hash over the expression
/// value alone) so recompiling an M0 flow yields byte-identical ids; the
/// static predicate types hash their full predicate value, prefixed by the
/// wire `type` literal.
fn derive_assert_id(predicate: &PredicateIR) -> AssertId {
    let (prefix, value) = match predicate {
        PredicateIR::Expr { expr } => (
            "expr",
            serde_json::to_value(expr).expect("an Expr always serializes"),
        ),
        PredicateIR::ElementState { .. } => (
            "elementState",
            serde_json::to_value(predicate).expect("a predicate always serializes"),
        ),
        PredicateIR::ElementText { .. } => (
            "elementText",
            serde_json::to_value(predicate).expect("a predicate always serializes"),
        ),
        PredicateIR::Visual { .. } => (
            "visual",
            serde_json::to_value(predicate).expect("a predicate always serializes"),
        ),
    };
    let hash = domain_hash("pointlock-compiler/1/assertId", &value);
    AssertId::new(format!("{prefix}-{}", hash.hex_prefix8()))
        .expect("'<type>-' + 8 hex chars is always grammatical")
}
