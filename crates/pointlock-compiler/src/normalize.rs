//! Phase 2 `normalize`: spanned AST → normalized candidate structure
//! (spine §8 stage 2, 03 §4.2).
//!
//! Head-key dispatch over the full 03 §1 authoring surface, default
//! materialization (`checkpoint` true — false inside macro expansions,
//! `idempotent` false, verb `effect` inference, `text` matcher defaults,
//! `verdict_policy` standard), closed-vocabulary enforcement. Everything
//! the authoring vocabulary recognizes but this milestone does not
//! implement yields an explicit "not in the M2 subset" diagnostic — never
//! a silent skip (fail-closed, 03 §1.0: there is no third kind of key).
//!
//! ## M2 surface (beyond the M1 verb/fallback core)
//!
//! - the structure heads `call` (relative-path subflow reference, linked
//!   and `irHash`-pinned via [`SubflowLinker`]), `if`/`then`/`else`,
//!   `foreach` (`in`/`as`/`steps`), `let` (03 §1.6);
//! - `human` steps (03 §1.6, 02 §4.4) — `on_timeout` is the fixed literal
//!   `unknown`, `provideInput` requires `expect_schema`;
//! - `macros` definitions and user-defined head-key calls with hygienic
//!   expansion (03 §1.7): synthesized `<call id>:<body id>` step ids,
//!   AST-level `${{ macro.<param> }}` substitution, in-body cross-step
//!   references rejected, recursion rejected, `checkpoint` defaulting to
//!   false inside expansions, `sourceMap` origin frames;
//! - flow- and step-level `handlers` with the five Disposition head keys
//!   (03 §1.8), `preflight`, `retry`, top-level `outputs` and
//!   `verdict_policy`.
//!
//! `observe`/`screenshot` are delivered: they lower to their
//! provider-synthetic actions (04 §9.4.3). String interpolation desugars
//! to `concat(...)` and the `value:` assertion sugar to `text` exact
//! (03 §1.5/§1.9), and `expect_schema` narrows invoke outputs (03 §1.3,
//! C7-enforced in check). The step-level `outputs` projection and
//! `${{ }}` scalars inside composite values remain outside the subset.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use pointlock_ir::vocab::{
    CanonicalVerb, Channel, EffectClassAction, ElementState, ErrorClass, HandlerHook, HumanMode,
    TextMatchMode, UiContextKind, VerdictPolicy,
};
use pointlock_ir::{
    ActionName, BackoffMs, BackoffSchedule, ElementSelectorIR, FlowId, Identifier,
    JsonSchemaDocument, MacroOriginFrame, RectIR, RetryPolicy, SelectorContext, TextMatchIR,
};

use crate::diagnostic::{CompileDiagnostic, DiagnosticSpan, codes, has_error};
use crate::parse::{MapEntry, Node, NodeValue};
use crate::subflow::{SubflowLinker, SubflowRecord};

/// Implemented top-level keys (03 §1.1 / spine A.7 — the full closed set).
pub(crate) const TOP_KEYS: &[&str] = &[
    "flow",
    "provider",
    "params",
    "outputs",
    "steps",
    "handlers",
    "macros",
    "verdict_policy",
];
/// v0.2-reserved keywords, rejected wherever they appear as keys (R6).
pub(crate) const RESERVED: &[&str] = &["secrets", "protected"];

/// The element-verb head keys (03 §1.3, spine A.4 `CanonicalVerb`), in
/// vocabulary order.
pub(crate) const VERB_HEADS_M1: &[(&str, CanonicalVerb)] = &[
    ("tap", CanonicalVerb::Tap),
    ("set_value", CanonicalVerb::SetValue),
    ("clear", CanonicalVerb::Clear),
    ("wait_for", CanonicalVerb::WaitFor),
    ("find", CanonicalVerb::Find),
];

/// The observation head keys (03 §1.3). Unlike the element verbs these
/// carry no selector: they name the parts of an observation to capture
/// (`ObserveWant`), which the provider-synthetic actions take as `wants`
/// (04 §9.4.3). `screenshot` is the single-part shortcut form.
pub(crate) const OBSERVE_HEADS: &[(&str, CanonicalVerb)] = &[
    ("observe", CanonicalVerb::Observe),
    ("screenshot", CanonicalVerb::Screenshot),
];

/// The closed `ObserveWant` vocabulary, in wire spelling.
pub(crate) const OBSERVE_WANTS: &[&str] = &["screenshot", "uiSnapshot"];

/// The provider-synthetic action an observation verb lowers to — the verb
/// token itself (04 §9.4.3 declares them under these names).
fn observe_action_name(verb: CanonicalVerb) -> &'static str {
    match verb {
        CanonicalVerb::Screenshot => "screenshot",
        _ => "observe",
    }
}
/// Step common keys of the implemented surface (diagnostic text).
pub(crate) const STEP_COMMON: &[&str] = &[
    "id",
    "preflight",
    "expect",
    "retry",
    "timeout_ms",
    "effect",
    "idempotent",
    "checkpoint",
    "on_fail",
    "on_unknown",
    "on_error",
    "on_resume_drift",
    "locate_via",
    "coordinate",
];
/// Recognized step keys outside the M2 subset (the step-level `outputs`
/// projection is a later milestone; `expect_schema` is delivered — the
/// invoke-output narrowing of 03 §1.3, and the provideInput contract
/// inside `human` blocks).
pub(crate) const STEP_NOT_M2: &[&str] = &["outputs"];
/// The assertion-item key vocabulary (03 §1.5; `value` is the
/// `text`-exact sugar).
pub(crate) const ASSERTION_KEYS: &[&str] = &[
    "expr",
    "element",
    "state",
    "text",
    "value",
    "visual",
    "region",
    "verify_via",
    "assert_id",
];
/// The step-level handler hook keys (03 §1.8, spine A.4).
pub(crate) const HOOK_KEYS: &[(&str, HandlerHook)] = &[
    ("on_fail", HandlerHook::OnFail),
    ("on_unknown", HandlerHook::OnUnknown),
    ("on_error", HandlerHook::OnError),
    ("on_resume_drift", HandlerHook::OnResumeDrift),
];
/// The five Disposition head keys of a handler binding (03 §1.8).
pub(crate) const DISPOSITION_HEADS: &[&str] = &["retry", "continue", "escalate", "abort", "repair"];
/// Maximum macro expansion (call-chain) depth, fail-closed (03 §1.7 rule 3).
const MAX_MACRO_DEPTH: usize = 16;

/// Closed vocabulary a macro name must not shadow (03 §4.3 A5). Union of
/// every keyword that can appear as a step key or a well-known subkey.
const MACRO_SHADOW_FORBIDDEN: &[&str] = &[
    // verbs (A.4)
    "tap",
    "set_value",
    "clear",
    "wait_for",
    "find",
    "observe",
    "screenshot",
    "invoke",
    // structure (A.7)
    "call",
    "inputs",
    "human",
    "if",
    "then",
    "else",
    "foreach",
    "in",
    "as",
    "let",
    "steps",
    // step common
    "id",
    "preflight",
    "expect",
    "retry",
    "timeout_ms",
    "effect",
    "idempotent",
    "checkpoint",
    "on_fail",
    "on_unknown",
    "on_error",
    "on_resume_drift",
    // fallback
    "locate_via",
    "verify_via",
    "coordinate",
    // assertions
    "element",
    "state",
    "text",
    "value",
    "expr",
    "visual",
    "region",
    "expect_schema",
    "assert_id",
    // top level
    "flow",
    "provider",
    "params",
    "outputs",
    "handlers",
    "macros",
    "verdict_policy",
    // human subkeys
    "mode",
    "prompt",
    "presents",
    "decisions",
    "on_timeout",
    // retry subkeys
    "max_attempts",
    "backoff_ms",
    "retry_on",
    // handler subkeys
    "continue",
    "escalate",
    "abort",
    "repair",
    "error_classes",
    "max_triggers",
    // v0.2 reserved
    "secrets",
    "protected",
];

/// The `WaitForElementCondition` value set (spine A.4 / A.8), shared by
/// `wait_for.state` and the `elementState` predicate's `state`.
pub(crate) const ELEMENT_STATES: &[(&str, ElementState)] = &[
    ("present", ElementState::Present),
    ("visible", ElementState::Visible),
    ("enabled", ElementState::Enabled),
    ("absent", ElementState::Absent),
];

/// The channel-token vocabulary of `locate_via` / `verify_via` (spine A.4
/// `Channel` wire literals). Chain-position legality (vision act-side,
/// coordinate verify-side) is bind-phase (E2), so all four parse here.
pub(crate) const CHANNEL_TOKENS: &[(&str, Channel)] = &[
    ("dom", Channel::Dom),
    ("uiTree", Channel::UiTree),
    ("vision", Channel::Vision),
    ("coordinate", Channel::Coordinate),
];

/// The normalized flow: shape-validated, defaults materialized, expressions
/// still as surface text (compiled in `check`).
#[derive(Debug)]
pub(crate) struct NormalizedFlow {
    /// The flow id.
    pub flow_id: FlowId,
    /// The validated provider name (`"devicerail"` in v0.1).
    pub provider: String,
    /// Verdict folding policy (materialized default: standard).
    pub verdict_policy: VerdictPolicy,
    /// Declared parameters.
    pub params: Vec<NormalizedParam>,
    /// Declared outputs.
    pub outputs: Vec<NormalizedOutput>,
    /// Flow-level handler bindings, in declaration order.
    pub handlers: Vec<NormalizedHandler>,
    /// The step body (≥ 1, post macro expansion).
    pub steps: Vec<NormalizedStep>,
    /// Span of the whole document (R13 lint anchor).
    pub span: DiagnosticSpan,
}

/// One declared flow output (03 §1.1: `outputs.<name>.{schema, from}`).
#[derive(Debug)]
pub(crate) struct NormalizedOutput {
    /// Output name.
    pub name: Identifier,
    /// Meta-validated JSON Schema contract.
    pub schema: JsonSchemaDocument,
    /// The projection expression (surface text; compiled in `check`).
    pub from: NormalizedValue,
}

/// One declared parameter.
#[derive(Debug)]
pub(crate) struct NormalizedParam {
    /// Parameter name.
    pub name: Identifier,
    /// Meta-validated JSON Schema.
    pub schema: JsonSchemaDocument,
    /// Whether the run must supply it (materialized default: false).
    pub required: bool,
    /// Optional default value.
    pub default: Option<serde_json::Value>,
}

/// A value in an expression-bearing position: either a full `${{ ... }}`
/// scalar (surface text) or a plain JSON literal.
#[derive(Debug)]
pub(crate) enum NormalizedValue {
    /// The inner text of a full `${{ ... }}` scalar.
    ExprText {
        /// Text between the delimiters.
        inner: String,
        /// Span of the whole scalar.
        span: DiagnosticSpan,
    },
    /// A literal JSON value (deep-checked to contain no `${{`).
    Literal {
        /// The literal value.
        value: serde_json::Value,
        /// Span of the node.
        span: DiagnosticSpan,
    },
}

/// One normalized step: the common envelope plus the kind-specific body.
#[derive(Debug)]
pub(crate) struct NormalizedStep {
    /// The step id: author-written, or `<call id>:<body id>` synthesized by
    /// macro expansion (then `origin` is non-empty).
    pub id: String,
    /// Span of the id value.
    pub id_span: DiagnosticSpan,
    /// Macro expansion chain, innermost first; empty = author-written.
    pub origin: Vec<MacroOriginFrame>,
    /// The kind-specific body.
    pub kind: NormalizedStepKind,
    /// Pre-entry world probes (03 §1.5: same assertion grammar as expect).
    pub preflight: Vec<NormalizedAssertion>,
    /// Act-phase retry policy (action steps only).
    pub retry: Option<RetryPolicy>,
    /// Optional step budget (for human steps: resolved into
    /// `HumanStepIR.timeoutMs` at check).
    pub timeout_ms: Option<u64>,
    /// Checkpoint materialized default: true; false inside macro
    /// expansions (02 §3/§7).
    pub checkpoint: bool,
    /// Step-level handler bindings, in declaration order.
    pub handlers: Vec<NormalizedHandler>,
    /// Span of the whole step mapping.
    pub span: DiagnosticSpan,
}

/// The kind-specific body of a normalized step (02 §4 vocabulary).
#[derive(Debug)]
pub(crate) enum NormalizedStepKind {
    /// Verb or `invoke` head → `ActionStepIR`.
    Action(Box<NormalizedAction>),
    /// No head key + `expect` → `AssertStepIR` with `observe: "fresh"`.
    Assert {
        /// The assertions (≥ 1 by construction).
        expect: Vec<NormalizedAssertion>,
    },
    /// `call` head → `CallStepIR`.
    Call(Box<NormalizedCall>),
    /// `human` head → `HumanStepIR`.
    Human(Box<NormalizedHuman>),
    /// `if`/`then`/`else` → `IfStepIR`.
    If(Box<NormalizedIf>),
    /// `foreach` (`in`/`as`/`steps`) → `ForeachStepIR`.
    Foreach(Box<NormalizedForeach>),
    /// `let` → `LetStepIR`.
    Let {
        /// The bindings (≥ 1).
        bindings: Vec<NormalizedBinding>,
    },
}

/// An action step's normalized material (M1 shape, unchanged).
#[derive(Debug)]
pub(crate) struct NormalizedAction {
    /// The head-key form (`invoke` or one of the element verbs).
    pub head: NormalizedHead,
    /// Declared or verb-inferred effect (`invoke` has no inference;
    /// absence there is a bind error).
    pub effect: Option<EffectClassAction>,
    /// Author-declared idempotence (materialized default: false).
    pub idempotent: bool,
    /// Author-declared act-chain (03 §1.4); absent = platform default.
    pub locate_via: Option<NormalizedChain>,
    /// Static coordinate key (03 §1.4 rule 3).
    pub coordinate: Option<NormalizedCoordinate>,
    /// `expect` assertions.
    pub expect: Vec<NormalizedAssertion>,
    /// The invoke-output narrowing contract (03 §1.3): present only on
    /// `invoke` heads — verbs and observation heads have documented
    /// output shapes and refuse it.
    pub output_schema: Option<JsonSchemaDocument>,
    /// True when the `invoke` head was LOWERED from `observe:` /
    /// `screenshot:` (04 §9.4.3): its output shape is the documented
    /// observation projection, so C7 does not demand a narrowing.
    pub observation_head: bool,
}

/// A `call` step: the linked callee plus caller-scope inputs (03 §1.6).
#[derive(Debug)]
pub(crate) struct NormalizedCall {
    /// The linked, `irHash`-pinned callee.
    pub subflow: SubflowRecord,
    /// Caller-scope input expressions, in declaration order.
    pub inputs: Vec<NormalizedArg>,
    /// Span of the `call` path value.
    pub call_span: DiagnosticSpan,
}

/// A `human` block's normalized subkeys (03 §1.6, shared by human steps
/// and `escalate` handler actions).
#[derive(Debug)]
pub(crate) struct NormalizedHuman {
    /// Interaction mode.
    pub mode: HumanMode,
    /// The question posed to the human (non-empty).
    pub prompt: String,
    /// Evidence/value expressions presented to the human.
    pub presents: Vec<NormalizedValue>,
    /// Enumerated options for judge/confirm/repairWorld modes.
    pub decisions: Option<Vec<String>>,
    /// `expect_schema` → `HumanStepIR.outputSchema` (provideInput only).
    pub output_schema: Option<JsonSchemaDocument>,
    /// In-block `timeout_ms` (03 §1.8 escalate form); a human *step* may
    /// alternatively carry it at step level (03 §1.6) — exactly one.
    pub timeout_ms: Option<u64>,
    /// Span of the whole human block.
    pub span: DiagnosticSpan,
}

/// An `if` step (03 §1.6).
#[derive(Debug)]
pub(crate) struct NormalizedIf {
    /// The branch condition (surface text; compiled in `check`).
    pub cond: NormalizedValue,
    /// The then-branch (≥ 1 step).
    pub then: Vec<NormalizedStep>,
    /// The optional else-branch (≥ 1 step when present).
    pub r#else: Option<Vec<NormalizedStep>>,
}

/// A `foreach` step (03 §1.6: `in` / `as` / `steps`).
#[derive(Debug)]
pub(crate) struct NormalizedForeach {
    /// The collection expression.
    pub items: NormalizedValue,
    /// The iteration variable name (`iter.<as>`).
    pub r#as: Identifier,
    /// Span of the `as` value.
    pub as_span: DiagnosticSpan,
    /// The loop body (≥ 1 step).
    pub body: Vec<NormalizedStep>,
}

/// One `let` binding (03 §1.6: `vars.<name>`, SSA).
#[derive(Debug)]
pub(crate) struct NormalizedBinding {
    /// The var name.
    pub name: Identifier,
    /// Span of the binding key.
    pub name_span: DiagnosticSpan,
    /// The bound expression.
    pub value: NormalizedValue,
}

/// One handler binding (03 §1.8), flow- or step-level.
#[derive(Debug)]
pub(crate) struct NormalizedHandler {
    /// The hook this handler fires on.
    pub hook: HandlerHook,
    /// Error-class filter (legal only on `on_error`).
    pub error_classes: Option<BTreeSet<ErrorClass>>,
    /// The disposition.
    pub action: NormalizedHandlerAction,
    /// Trigger budget (≥ 1).
    pub max_triggers: u32,
    /// Span of the binding map.
    pub span: DiagnosticSpan,
}

/// The disposition head key of a handler binding (03 §1.8, spine A.4
/// `Disposition`).
#[derive(Debug)]
pub(crate) enum NormalizedHandlerAction {
    /// `retry:` — value is the retry subkeys.
    Retry(RetryPolicy),
    /// `continue:` — record and move on.
    Continue,
    /// `escalate:` — value is the human subkeys (in-block timeout
    /// mandatory: there is no step level to fall back to).
    Escalate(NormalizedHuman),
    /// `abort:` — abort the run.
    Abort,
    /// `repair:` — value is a subflow path, linked like `call`.
    Repair {
        /// The linked repair subflow.
        subflow: SubflowRecord,
    },
}

/// The step's head-key dispatch result (03 §1.2 classes 1 and the
/// `invoke` escape hatch).
#[derive(Debug)]
pub(crate) enum NormalizedHead {
    /// `invoke: { action, args }` — the escape hatch (spine A.6).
    Invoke {
        /// The provider-native action name.
        action: ActionName,
        /// Span of the action value.
        action_span: DiagnosticSpan,
        /// Ordered invoke arguments.
        args: Vec<NormalizedArg>,
    },
    /// One of the five element verbs (03 §1.3). Boxed to keep the enum
    /// small (the selector dominates the variant size).
    Verb(Box<NormalizedVerb>),
}

/// One normalized element-verb head (03 §1.3).
#[derive(Debug)]
pub(crate) struct NormalizedVerb {
    /// The canonical verb.
    pub verb: CanonicalVerb,
    /// Span of the head key (bind diagnostics point here).
    pub head_span: DiagnosticSpan,
    /// The static element selector (field-isomorphic passthrough).
    pub element: ElementSelectorIR,
    /// Span of the `element` value.
    pub element_span: DiagnosticSpan,
    /// `set_value` only: the value to set (Expr allowed).
    pub value: Option<NormalizedValue>,
    /// `wait_for` only: the awaited element state.
    pub state: Option<ElementState>,
}

/// A declared channel chain (`locate_via` / per-assertion `verify_via`).
#[derive(Debug, Clone)]
pub(crate) struct NormalizedChain {
    /// The channels in author order (subsequence legality is bind-phase).
    pub channels: Vec<Channel>,
    /// Span of the chain value.
    pub span: DiagnosticSpan,
}

/// The static `coordinate: { x, y }` key (03 §1.4 rule 3).
#[derive(Debug, Clone)]
pub(crate) struct NormalizedCoordinate {
    /// X coordinate (static literal).
    pub x: serde_json::Number,
    /// Y coordinate (static literal).
    pub y: serde_json::Number,
    /// Span of the coordinate value.
    pub span: DiagnosticSpan,
}

/// One `expect` item, dispatched to its predicate form (03 §1.5).
#[derive(Debug)]
pub(crate) struct NormalizedAssertion {
    /// Explicit assert id, if written (otherwise derived in `check`).
    pub assert_id: Option<(String, DiagnosticSpan)>,
    /// The predicate form.
    pub predicate: NormalizedPredicate,
    /// Author-declared verify-chain; absent = predicate-specific default.
    pub verify_via: Option<NormalizedChain>,
    /// The `visual:` prompt of an element predicate whose chain degrades to
    /// vision (03 §1.4 rule 5); for `visual` predicates the prompt lives in
    /// the predicate itself.
    pub visual: Option<(String, DiagnosticSpan)>,
    /// Span of the whole assertion item.
    pub span: DiagnosticSpan,
}

/// The four predicate surfaces (spine A.4); all fields except the `expr`
/// text are static literals (predicate staticness, 03 §1.5).
#[derive(Debug)]
pub(crate) enum NormalizedPredicate {
    /// `- expr: ${{ ... }}`.
    Expr {
        /// Inner text of the expression scalar.
        inner: String,
        /// Span of the expr scalar.
        span: DiagnosticSpan,
    },
    /// `- element: {...}` + `state:`.
    ElementState {
        /// The element to check.
        selector: ElementSelectorIR,
        /// The expected state.
        state: ElementState,
    },
    /// `- element: {...}` + `text: {...}`.
    ElementText {
        /// The element to check.
        selector: ElementSelectorIR,
        /// The text matcher (defaults materialized).
        matcher: TextMatchIR,
    },
    /// `- visual: "<prompt>"` (+ optional `region`).
    Visual {
        /// Author-written vision prompt.
        prompt: String,
        /// Optional region of interest.
        region: Option<RectIR>,
    },
}

/// Where a static selector appears — the diagnostics differ (a verb element
/// could one day be a dynamic `UiNodeRef` target; a predicate never can).
#[derive(Clone, Copy, PartialEq)]
enum SelectorPosition {
    /// A verb's `element` subkey (03 §1.3).
    VerbElement,
    /// An assertion predicate's `element` subkey (03 §1.5).
    Predicate,
}

impl SelectorPosition {
    /// Diagnostic for a `${{ }}` in a position this selector requires to be
    /// static.
    fn expr_diagnostic(self, span: DiagnosticSpan) -> CompileDiagnostic {
        match self {
            SelectorPosition::VerbElement => CompileDiagnostic::error(
                codes::NORM_BAD_SELECTOR_SHAPE,
                "element selector fields must be static literals; dynamic element targets \
                 (UiNodeRef chaining, 04 §9.5) are not in the M2 subset",
                Some(span),
            ),
            SelectorPosition::Predicate => CompileDiagnostic::error(
                codes::NORM_PREDICATE_NOT_STATIC,
                "predicate fields must be static literals (03 §1.5); for a dynamic comparison \
                 rewrite the assertion as an `expr` predicate over an upstream step output",
                Some(span),
            ),
        }
    }
}

/// Classification of a string scalar with respect to `${{ }}`.
enum ExprClass<'a> {
    /// The whole scalar is one `${{ ... }}` expression.
    Full(&'a str),
    /// `${{` appears but the scalar is not a single full expression
    /// (string interpolation — not in M1).
    Embedded,
    /// No `${{` anywhere.
    Plain,
}

fn classify_expr_text(s: &str) -> ExprClass<'_> {
    let trimmed = s.trim();
    let Some(start) = trimmed.find("${{") else {
        return ExprClass::Plain;
    };
    if start == 0 && trimmed.ends_with("}}") && trimmed.len() >= 5 && !trimmed[3..].contains("${{")
    {
        return ExprClass::Full(&trimmed[3..trimmed.len() - 2]);
    }
    ExprClass::Embedded
}

/// 03 §1.9's interpolation sugar: an embedded-`${{ }}` string desugars to
/// `concat(...)` expression text — literal runs become double-quoted
/// JSON-escaped string literals (the expr-text grammar), each island
/// keeps its inner text verbatim (parsed and validated at `check` like
/// any full scalar). A `}}` inside an island's own string literal is a
/// known mis-split: the desugared text then fails expr parsing with a
/// clear syntax diagnostic — fail-closed, never silent.
fn desugar_interpolation(s: &str) -> Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    let mut rest = s;
    loop {
        match rest.find("${{") {
            Some(start) => {
                if start > 0 {
                    parts.push(quote_expr_string(&rest[..start]));
                }
                let after = &rest[start + 3..];
                let Some(end) = after.find("}}") else {
                    return Err("unterminated `${{` island".to_owned());
                };
                let inner = after[..end].trim();
                if inner.is_empty() {
                    return Err("empty `${{ }}` island".to_owned());
                }
                parts.push(inner.to_owned());
                rest = &after[end + 2..];
            }
            None => {
                if !rest.is_empty() {
                    parts.push(quote_expr_string(rest));
                }
                break;
            }
        }
    }
    Ok(format!("concat({})", parts.join(", ")))
}

/// A double-quoted, JSON-escaped string literal of the expr-text grammar.
fn quote_expr_string(text: &str) -> String {
    serde_json::to_string(text).expect("a string always serializes")
}

/// Deep-scans a literal-position JSON node for `${{` occurrences and emits
/// the M1 rejection diagnostics (RF2020 embedded, RF2021 nested full).
fn scan_literal_for_expr(node: &Node, diags: &mut Vec<CompileDiagnostic>) {
    match &node.value {
        NodeValue::String(s) => match classify_expr_text(s) {
            ExprClass::Plain => {}
            ExprClass::Full(_) => diags.push(CompileDiagnostic::error(
                codes::NORM_NESTED_EXPR_NOT_IMPLEMENTED,
                "not in the M2 subset: a ${{ ... }} expression nested inside a composite value \
                 (argument values are either one full expression scalar or a pure literal)",
                Some(node.span),
            )),
            ExprClass::Embedded => diags.push(CompileDiagnostic::error(
                codes::NORM_INTERPOLATION_INVALID,
                "not in the M2 subset: string interpolation inside a composite literal member \
                 (top-level string arguments desugar to concat(...); composite members stay \
                 pure literals)",
                Some(node.span),
            )),
        },
        NodeValue::Sequence(items) => {
            for item in items {
                scan_literal_for_expr(item, diags);
            }
        }
        NodeValue::Mapping(entries) => {
            for entry in entries {
                scan_literal_for_expr(&entry.value, diags);
            }
        }
        _ => {}
    }
}

/// Classifies an expression-bearing value node (invoke args,
/// `set_value.value`).
fn normalize_value(node: &Node, diags: &mut Vec<CompileDiagnostic>) -> Option<NormalizedValue> {
    if let NodeValue::String(s) = &node.value {
        match classify_expr_text(s) {
            ExprClass::Full(inner) => {
                return Some(NormalizedValue::ExprText {
                    inner: inner.to_owned(),
                    span: node.span,
                });
            }
            ExprClass::Embedded => {
                // 03 §1.9's interpolation sugar: the embedded string
                // desugars to `concat(...)` expression text; the result
                // is an ordinary full expression scalar (and always a
                // string — concat's contract).
                return match desugar_interpolation(s) {
                    Ok(inner) => Some(NormalizedValue::ExprText {
                        inner,
                        span: node.span,
                    }),
                    Err(reason) => {
                        diags.push(CompileDiagnostic::error(
                            codes::NORM_INTERPOLATION_INVALID,
                            format!("malformed string interpolation: {reason}"),
                            Some(node.span),
                        ));
                        None
                    }
                };
            }
            ExprClass::Plain => {}
        }
    }
    let before = diags.len();
    scan_literal_for_expr(node, diags);
    if diags.len() > before {
        return None;
    }
    Some(NormalizedValue::Literal {
        value: node.to_json(),
        span: node.span,
    })
}

fn expect_mapping<'a>(
    node: &'a Node,
    what: &str,
    code: &'static str,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<&'a [MapEntry]> {
    match &node.value {
        NodeValue::Mapping(entries) => Some(entries),
        other => {
            diags.push(CompileDiagnostic::error(
                code,
                format!("{what} must be a mapping, got {}", type_name_of(other)),
                Some(node.span),
            ));
            None
        }
    }
}

fn type_name_of(value: &NodeValue) -> &'static str {
    match value {
        NodeValue::Null => "null",
        NodeValue::Bool(_) => "boolean",
        NodeValue::Number(_) => "number",
        NodeValue::String(_) => "string",
        NodeValue::Sequence(_) => "sequence",
        NodeValue::Mapping(_) => "mapping",
    }
}

/// Normalizes the parsed document, accumulating every diagnostic it can
/// attribute before failing. `file` is the current source file (subflow
/// paths resolve against it); the linker is shared across the closure.
pub(crate) fn normalize(
    root: &Node,
    file: &str,
    linker: &mut SubflowLinker<'_>,
) -> Result<NormalizedFlow, Vec<CompileDiagnostic>> {
    let mut diags = Vec::new();
    let NodeValue::Mapping(entries) = &root.value else {
        unreachable!("parse guarantees a mapping root");
    };

    let mut flow_entry = None;
    let mut provider_entry = None;
    let mut verdict_entry = None;
    let mut params_entry = None;
    let mut outputs_entry = None;
    let mut handlers_entry = None;
    let mut macros_entry = None;
    let mut steps_entry = None;

    for entry in entries {
        match entry.key.as_str() {
            "flow" => flow_entry = Some(entry),
            "provider" => provider_entry = Some(entry),
            "verdict_policy" => verdict_entry = Some(entry),
            "params" => params_entry = Some(entry),
            "outputs" => outputs_entry = Some(entry),
            "handlers" => handlers_entry = Some(entry),
            "macros" => macros_entry = Some(entry),
            "steps" => steps_entry = Some(entry),
            key if RESERVED.contains(&key) => diags.push(CompileDiagnostic::error(
                codes::NORM_RESERVED_KEYWORD,
                format!("`{key}` is reserved for v0.2 (spine R6)"),
                Some(entry.key_span),
            )),
            key => diags.push(CompileDiagnostic::error(
                codes::NORM_UNKNOWN_TOP_KEY,
                format!(
                    "unknown top-level key `{key}` (closed vocabulary: {})",
                    TOP_KEYS.join(", ")
                ),
                Some(entry.key_span),
            )),
        }
    }

    let flow_id = normalize_flow_id(flow_entry, root.span, &mut diags);
    let provider = normalize_provider(provider_entry, root.span, &mut diags);
    let verdict_policy = verdict_entry
        .and_then(|entry| normalize_verdict_policy(&entry.value, &mut diags))
        // Materialized default (03 §1.1).
        .unwrap_or(VerdictPolicy::Standard);
    let params = params_entry
        .map(|entry| normalize_params(&entry.value, &mut diags))
        .unwrap_or_default();
    let outputs = outputs_entry
        .map(|entry| normalize_outputs(&entry.value, &mut diags))
        .unwrap_or_default();

    let mut normalizer = Normalizer {
        file,
        linker,
        macros: BTreeMap::new(),
        origin_stack: Vec::new(),
        macro_stack: Vec::new(),
        diags,
    };
    if let Some(entry) = macros_entry {
        normalizer.normalize_macros(&entry.value);
    }
    let handlers = handlers_entry
        .map(|entry| normalizer.normalize_handler_block(&entry.value))
        .unwrap_or_default();
    let steps = normalizer.normalize_steps(steps_entry, root.span);
    let diags = normalizer.diags;

    match (flow_id, provider, !has_error(&diags)) {
        (Some(flow_id), Some(provider), true) => {
            debug_assert!(diags.is_empty(), "normalize emits no warnings today");
            Ok(NormalizedFlow {
                flow_id,
                provider,
                verdict_policy,
                params,
                outputs,
                handlers,
                steps,
                span: root.span,
            })
        }
        _ => Err(diags),
    }
}

/// Parses `verdict_policy` (03 §1.1: `standard | strict`).
fn normalize_verdict_policy(
    node: &Node,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<VerdictPolicy> {
    match &node.value {
        NodeValue::String(s) if s == "standard" => Some(VerdictPolicy::Standard),
        NodeValue::String(s) if s == "strict" => Some(VerdictPolicy::Strict),
        other => {
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_VERDICT_POLICY,
                format!(
                    "`verdict_policy` must be `standard` or `strict`, got {}",
                    match other {
                        NodeValue::String(s) => format!("'{s}'"),
                        other => type_name_of(other).to_owned(),
                    }
                ),
                Some(node.span),
            ));
            None
        }
    }
}

/// Parses the top-level `outputs` block (03 §1.1).
fn normalize_outputs(node: &Node, diags: &mut Vec<CompileDiagnostic>) -> Vec<NormalizedOutput> {
    let Some(entries) = expect_mapping(node, "`outputs`", codes::NORM_BAD_OUTPUTS_SHAPE, diags)
    else {
        return Vec::new();
    };
    let mut outputs = Vec::new();
    for entry in entries {
        let Ok(name) = Identifier::new(entry.key.clone()) else {
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_OUTPUTS_SHAPE,
                format!(
                    "output name '{}' must match [A-Za-z_][A-Za-z0-9_]*",
                    entry.key
                ),
                Some(entry.key_span),
            ));
            continue;
        };
        let Some(fields) = expect_mapping(
            &entry.value,
            &format!("output `{name}`"),
            codes::NORM_BAD_OUTPUTS_SHAPE,
            diags,
        ) else {
            continue;
        };
        let mut schema = None;
        let mut from = None;
        let mut ok = true;
        for field in fields {
            match field.key.as_str() {
                "schema" => {
                    schema = normalize_schema_document(
                        &format!("output `{name}`"),
                        &field.value,
                        codes::NORM_BAD_OUTPUTS_SHAPE,
                        diags,
                    );
                    ok &= schema.is_some();
                }
                "from" => {
                    from = normalize_value(&field.value, diags);
                    ok &= from.is_some();
                }
                key => {
                    ok = false;
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_OUTPUTS_SHAPE,
                        format!("output `{name}`: unknown subkey `{key}` (closed: schema, from)"),
                        Some(field.key_span),
                    ));
                }
            }
        }
        if ok && (schema.is_none() || from.is_none()) {
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_OUTPUTS_SHAPE,
                format!("output `{name}` must carry both `schema` and `from`"),
                Some(entry.value.span),
            ));
            ok = false;
        }
        if ok {
            outputs.push(NormalizedOutput {
                name,
                schema: schema.expect("checked above"),
                from: from.expect("checked above"),
            });
        }
    }
    outputs
}

fn normalize_flow_id(
    entry: Option<&MapEntry>,
    root_span: DiagnosticSpan,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<FlowId> {
    let Some(entry) = entry else {
        diags.push(CompileDiagnostic::error(
            codes::NORM_MISSING_TOP_KEY,
            "required top-level key `flow` is missing",
            Some(root_span),
        ));
        return None;
    };
    let NodeValue::String(raw) = &entry.value.value else {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_FLOW_ID,
            format!(
                "`flow` must be a string flow id, got {}",
                entry.value.type_name()
            ),
            Some(entry.value.span),
        ));
        return None;
    };
    match FlowId::new(raw.clone()) {
        Ok(flow_id) => Some(flow_id),
        Err(err) => {
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_FLOW_ID,
                format!("invalid flow id '{raw}': {}", err.reason),
                Some(entry.value.span),
            ));
            None
        }
    }
}

fn normalize_provider(
    entry: Option<&MapEntry>,
    root_span: DiagnosticSpan,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<String> {
    let Some(entry) = entry else {
        diags.push(CompileDiagnostic::error(
            codes::NORM_MISSING_TOP_KEY,
            "required top-level key `provider` is missing",
            Some(root_span),
        ));
        return None;
    };
    match &entry.value.value {
        NodeValue::String(name) if name == "devicerail" => Some(name.clone()),
        NodeValue::String(name) => {
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_PROVIDER,
                format!("provider '{name}' is not supported; v0.1 accepts only 'devicerail'"),
                Some(entry.value.span),
            ));
            None
        }
        other => {
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_PROVIDER,
                format!("`provider` must be a string, got {}", type_name_of(other)),
                Some(entry.value.span),
            ));
            None
        }
    }
}

fn normalize_params(node: &Node, diags: &mut Vec<CompileDiagnostic>) -> Vec<NormalizedParam> {
    let Some(entries) = expect_mapping(node, "`params`", codes::NORM_BAD_PARAMS_SHAPE, diags)
    else {
        return Vec::new();
    };
    let mut params = Vec::new();
    for entry in entries {
        let Ok(name) = Identifier::new(entry.key.clone()) else {
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_PARAMS_SHAPE,
                format!(
                    "param name '{}' must match [A-Za-z_][A-Za-z0-9_]*",
                    entry.key
                ),
                Some(entry.key_span),
            ));
            continue;
        };
        if let Some(param) = normalize_param(name, &entry.value, diags) {
            params.push(param);
        }
    }
    params
}

fn normalize_param(
    name: Identifier,
    node: &Node,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<NormalizedParam> {
    let entries = expect_mapping(
        node,
        &format!("param `{name}`"),
        codes::NORM_BAD_PARAMS_SHAPE,
        diags,
    )?;
    let mut schema = None;
    let mut required = false;
    let mut default = None;
    let mut ok = true;
    for entry in entries {
        match entry.key.as_str() {
            "schema" => {
                schema = normalize_param_schema(&name, &entry.value, diags);
                ok &= schema.is_some();
            }
            "required" => match &entry.value.value {
                NodeValue::Bool(b) => required = *b,
                other => {
                    ok = false;
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_PARAM_VALUE,
                        format!(
                            "param `{name}`: `required` must be a boolean, got {}",
                            type_name_of(other)
                        ),
                        Some(entry.value.span),
                    ));
                }
            },
            "default" => {
                let before = diags.len();
                scan_literal_for_expr(&entry.value, diags);
                if diags.len() > before {
                    ok = false;
                } else {
                    default = Some(entry.value.to_json());
                }
            }
            key => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_UNKNOWN_PARAM_KEY,
                    format!(
                        "param `{name}`: unknown subkey `{key}` (closed: schema, required, default)"
                    ),
                    Some(entry.key_span),
                ));
            }
        }
    }
    if schema.is_none() && ok {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_PARAM_VALUE,
            format!("param `{name}`: required subkey `schema` is missing"),
            Some(node.span),
        ));
        ok = false;
    }
    if !ok {
        return None;
    }
    Some(NormalizedParam {
        name,
        schema: schema.expect("checked above"),
        required,
        default,
    })
}

fn normalize_param_schema(
    name: &Identifier,
    node: &Node,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<JsonSchemaDocument> {
    normalize_schema_document(
        &format!("param `{name}`"),
        node,
        codes::NORM_BAD_PARAM_VALUE,
        diags,
    )
}

/// Parses and meta-validates an embedded JSON Schema document (03 §4.3 F1;
/// shared by `params.*.schema`, `outputs.*.schema` and `expect_schema`).
fn normalize_schema_document(
    what: &str,
    node: &Node,
    code: &'static str,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<JsonSchemaDocument> {
    let value = node.to_json();
    let document = match JsonSchemaDocument::new(value) {
        Ok(document) => document,
        Err(err) => {
            diags.push(CompileDiagnostic::error(
                code,
                format!("{what}: `schema` {}", err.reason),
                Some(node.span),
            ));
            return None;
        }
    };
    // Meta-validation against the JSON Schema meta-schema (03 §4.3 F1).
    if let Err(err) = jsonschema::meta::validate(document.as_value()) {
        diags.push(CompileDiagnostic::error(
            code,
            format!("{what}: `schema` is not a valid JSON Schema: {err}"),
            Some(node.span),
        ));
        return None;
    }
    Some(document)
}

/// The verb's inferred effect (03 §1.3, no third value).
fn inferred_effect(verb: CanonicalVerb) -> EffectClassAction {
    match verb {
        CanonicalVerb::Tap | CanonicalVerb::SetValue | CanonicalVerb::Clear => {
            EffectClassAction::Mutating
        }
        _ => EffectClassAction::Readonly,
    }
}

/// One macro definition (03 §1.7): compile-time params over a raw step-node
/// body, hygienically expanded at every call site.
#[derive(Debug)]
struct MacroDef {
    /// Declared compile-time parameter names, in declaration order.
    params: Vec<String>,
    /// The raw body step nodes (substitution happens on clones).
    body: Vec<Node>,
}

/// Whether a string is a legal author-written step id *segment*
/// (`[A-Za-z_][A-Za-z0-9_-]*` — no `:` and no `.`, 03 §1.2 / 02 §3).
fn is_author_step_id(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Whether a string is a plain identifier (`[A-Za-z_][A-Za-z0-9_]*`).
fn is_plain_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Extracts every `steps.<ident>` token from a scalar (conservative
/// textual scan for the macro cross-step-reference ban, 03 §1.7 rule 2).
fn referenced_step_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(pos) = rest.find("steps.") {
        let after = &rest[pos + "steps.".len()..];
        let ident: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if !ident.is_empty() {
            out.push(ident);
        }
        rest = &rest[pos + "steps.".len()..];
    }
    out
}

/// Collects the step ids declared inside a macro body (any nesting level —
/// `then`/`else` branches and `foreach.steps` included).
fn collect_body_step_ids(nodes: &[Node], ids: &mut HashSet<String>) {
    for node in nodes {
        let NodeValue::Mapping(entries) = &node.value else {
            continue;
        };
        for entry in entries {
            match entry.key.as_str() {
                "id" => {
                    if let NodeValue::String(s) = &entry.value.value {
                        ids.insert(s.clone());
                    }
                }
                "then" | "else" => {
                    if let NodeValue::Sequence(items) = &entry.value.value {
                        collect_body_step_ids(items, ids);
                    }
                }
                "foreach" => {
                    if let NodeValue::Mapping(sub) = &entry.value.value {
                        for sub_entry in sub {
                            if sub_entry.key == "steps"
                                && let NodeValue::Sequence(items) = &sub_entry.value.value
                            {
                                collect_body_step_ids(items, ids);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// The human-block parsing position: a step's `human:` head may take its
/// timeout from step level, an `escalate:` block must carry it in-block.
#[derive(Clone, Copy, PartialEq)]
enum HumanContext {
    /// `human:` head key of a step (03 §1.6).
    Step,
    /// `escalate:` disposition of a handler binding (03 §1.8).
    Escalate,
}

/// Flow-local normalization state: the macro table, the expansion stacks
/// (hygiene + recursion detection), the closure-shared subflow linker and
/// the diagnostic sink.
struct Normalizer<'a, 'l> {
    /// The current source file (subflow resolution base, origin frames).
    file: &'a str,
    /// The closure-shared subflow linker.
    linker: &'a mut SubflowLinker<'l>,
    /// Flow-local macro definitions.
    macros: BTreeMap<String, MacroDef>,
    /// Live expansion chain, outermost first (origin = reversed).
    origin_stack: Vec<MacroOriginFrame>,
    /// Macro names on the live expansion chain (recursion detection).
    macro_stack: Vec<String>,
    /// Accumulated diagnostics.
    diags: Vec<CompileDiagnostic>,
}

impl Normalizer<'_, '_> {
    fn error(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Option<DiagnosticSpan>,
    ) {
        self.diags
            .push(CompileDiagnostic::error(code, message, span));
    }

    /// The origin chain of the step currently being produced (innermost
    /// expansion first, 02 §7); empty for author-written steps.
    fn current_origin(&self) -> Vec<MacroOriginFrame> {
        self.origin_stack.iter().rev().cloned().collect()
    }

    // ── macros ──────────────────────────────────────────────────────────

    /// Parses the `macros:` definition block (03 §1.7).
    fn normalize_macros(&mut self, node: &Node) {
        let Some(entries) =
            expect_mapping(node, "`macros`", codes::NORM_BAD_MACRO_DEF, &mut self.diags)
        else {
            return;
        };
        for entry in entries {
            let name = entry.key.as_str();
            if MACRO_SHADOW_FORBIDDEN.contains(&name) {
                self.error(
                    codes::NORM_BAD_MACRO_DEF,
                    format!(
                        "macro name `{name}` shadows the closed authoring vocabulary (03 §4.3 A5)"
                    ),
                    Some(entry.key_span),
                );
                continue;
            }
            if !is_plain_identifier(name) {
                self.error(
                    codes::NORM_BAD_MACRO_DEF,
                    format!("macro name '{name}' must match [A-Za-z_][A-Za-z0-9_]*"),
                    Some(entry.key_span),
                );
                continue;
            }
            let Some(def_entries) = expect_mapping(
                &entry.value,
                &format!("macro `{name}`"),
                codes::NORM_BAD_MACRO_DEF,
                &mut self.diags,
            ) else {
                continue;
            };
            let mut params: Vec<String> = Vec::new();
            let mut body: Option<Vec<Node>> = None;
            let mut ok = true;
            for def_entry in def_entries {
                match def_entry.key.as_str() {
                    "params" => match &def_entry.value.value {
                        NodeValue::Sequence(items) => {
                            for item in items {
                                match &item.value {
                                    NodeValue::String(param) if is_plain_identifier(param) => {
                                        if params.contains(param) {
                                            ok = false;
                                            self.error(
                                                codes::NORM_BAD_MACRO_DEF,
                                                format!(
                                                    "macro `{name}`: duplicate param '{param}'"
                                                ),
                                                Some(item.span),
                                            );
                                        } else {
                                            params.push(param.clone());
                                        }
                                    }
                                    other => {
                                        ok = false;
                                        self.error(
                                            codes::NORM_BAD_MACRO_DEF,
                                            format!(
                                                "macro `{name}`: params must be identifier \
                                                 strings, got {}",
                                                type_name_of(other)
                                            ),
                                            Some(item.span),
                                        );
                                    }
                                }
                            }
                        }
                        other => {
                            ok = false;
                            self.error(
                                codes::NORM_BAD_MACRO_DEF,
                                format!(
                                    "macro `{name}`: `params` must be a sequence, got {}",
                                    type_name_of(other)
                                ),
                                Some(def_entry.value.span),
                            );
                        }
                    },
                    "steps" => match &def_entry.value.value {
                        NodeValue::Sequence(items) if !items.is_empty() => {
                            body = Some(items.clone());
                        }
                        other => {
                            ok = false;
                            self.error(
                                codes::NORM_BAD_MACRO_DEF,
                                format!(
                                    "macro `{name}`: `steps` must be a non-empty sequence, got {}",
                                    match other {
                                        NodeValue::Sequence(_) => "an empty sequence".to_owned(),
                                        other => type_name_of(other).to_owned(),
                                    }
                                ),
                                Some(def_entry.value.span),
                            );
                        }
                    },
                    key => {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_MACRO_DEF,
                            format!(
                                "macro `{name}`: unknown subkey `{key}` (closed: params, steps)"
                            ),
                            Some(def_entry.key_span),
                        );
                    }
                }
            }
            let Some(body) = body else {
                if ok {
                    self.error(
                        codes::NORM_BAD_MACRO_DEF,
                        format!("macro `{name}` is missing the required `steps` body"),
                        Some(entry.value.span),
                    );
                }
                continue;
            };
            if !ok {
                continue;
            }
            // 03 §1.7 rule 2 (§4.3 B5): the body must not reference its own
            // step ids — synthesized ids have no ref surface. Checked at
            // definition time over the raw body (pre-substitution, so
            // call-site arguments legally referencing outer steps of the
            // same name are not caught by this scan).
            let mut body_ids = HashSet::new();
            collect_body_step_ids(&body, &mut body_ids);
            for step_node in &body {
                self.scan_cross_step_refs(name, step_node, &body_ids);
            }
            self.macros
                .insert(name.to_owned(), MacroDef { params, body });
        }
    }

    /// Rejects `steps.<body id>.*` references inside a macro body
    /// (conservative textual scan over `${{ }}`-bearing scalars).
    fn scan_cross_step_refs(&mut self, name: &str, node: &Node, body_ids: &HashSet<String>) {
        match &node.value {
            NodeValue::String(s) if s.contains("${{") => {
                for token in referenced_step_tokens(s) {
                    if body_ids.contains(&token) {
                        self.error(
                            codes::NORM_MACRO_CROSS_STEP_REF,
                            format!(
                                "macro `{name}`: `steps.{token}` references a step defined \
                                 inside the macro body — synthesized ids have no ref surface; \
                                 for in-body data flow use a subflow (03 §1.7 rule 2)"
                            ),
                            Some(node.span),
                        );
                    }
                }
            }
            NodeValue::Sequence(items) => {
                for item in items {
                    self.scan_cross_step_refs(name, item, body_ids);
                }
            }
            NodeValue::Mapping(entries) => {
                for entry in entries {
                    self.scan_cross_step_refs(name, &entry.value, body_ids);
                }
            }
            _ => {}
        }
    }

    /// AST-level `${{ macro.<param> }}` substitution over a cloned body
    /// node (03 §1.7 rule 1). Only the full-scalar form is a parameter
    /// reference; any other `macro.` use inside an expression is rejected.
    fn substitute(
        &mut self,
        node: &Node,
        macro_name: &str,
        args: &BTreeMap<String, Node>,
    ) -> Option<Node> {
        match &node.value {
            NodeValue::String(s) => match classify_expr_text(s) {
                ExprClass::Full(inner) => {
                    let inner = inner.trim();
                    if let Some(param) = inner.strip_prefix("macro.") {
                        if !is_plain_identifier(param) {
                            self.error(
                                codes::NORM_BAD_MACRO_PARAM,
                                format!(
                                    "macro `{macro_name}`: '{inner}' is not a grammatical \
                                     `macro.<param>` reference"
                                ),
                                Some(node.span),
                            );
                            return None;
                        }
                        match args.get(param) {
                            Some(arg) => Some(arg.clone()),
                            None => {
                                self.error(
                                    codes::NORM_BAD_MACRO_PARAM,
                                    format!(
                                        "macro `{macro_name}` has no param '{param}' \
                                         (declared: {})",
                                        args.keys().cloned().collect::<Vec<_>>().join(", ")
                                    ),
                                    Some(node.span),
                                );
                                None
                            }
                        }
                    } else if inner.contains("macro.") {
                        self.error(
                            codes::NORM_BAD_MACRO_PARAM,
                            "a `macro.<param>` reference must be one full \
                             `${{ macro.<param> }}` scalar (03 §1.7 rule 1)",
                            Some(node.span),
                        );
                        None
                    } else {
                        Some(node.clone())
                    }
                }
                ExprClass::Embedded if s.contains("macro.") => {
                    self.error(
                        codes::NORM_BAD_MACRO_PARAM,
                        "a `macro.<param>` reference must be one full \
                         `${{ macro.<param> }}` scalar (03 §1.7 rule 1)",
                        Some(node.span),
                    );
                    None
                }
                _ => Some(node.clone()),
            },
            NodeValue::Sequence(items) => {
                let mut out = Vec::with_capacity(items.len());
                let mut ok = true;
                for item in items {
                    match self.substitute(item, macro_name, args) {
                        Some(node) => out.push(node),
                        None => ok = false,
                    }
                }
                ok.then_some(Node {
                    value: NodeValue::Sequence(out),
                    span: node.span,
                })
            }
            NodeValue::Mapping(entries) => {
                let mut out = Vec::with_capacity(entries.len());
                let mut ok = true;
                for entry in entries {
                    match self.substitute(&entry.value, macro_name, args) {
                        Some(value) => out.push(MapEntry {
                            key: entry.key.clone(),
                            key_span: entry.key_span,
                            value,
                        }),
                        None => ok = false,
                    }
                }
                ok.then_some(Node {
                    value: NodeValue::Mapping(out),
                    span: node.span,
                })
            }
            _ => Some(node.clone()),
        }
    }

    /// Hygiene prefixing (03 §1.7 rule 2): every step id in the expanded
    /// body becomes `<call id>:<body id>` — the `:` segment is the
    /// compiler-reserved StepId section (02 §3). Recurses into nested step
    /// lists; nested macro call sites are prefixed here and extend the
    /// chain again at their own expansion.
    fn prefix_step_ids(&mut self, node: &mut Node, macro_name: &str, call_id: &str) {
        let NodeValue::Mapping(entries) = &mut node.value else {
            return;
        };
        for entry in entries.iter_mut() {
            match entry.key.as_str() {
                "id" => {
                    if let NodeValue::String(id) = &mut entry.value.value {
                        if is_author_step_id(id) {
                            *id = format!("{call_id}:{id}");
                        } else {
                            self.diags.push(CompileDiagnostic::error(
                                codes::NORM_BAD_MACRO_DEF,
                                format!(
                                    "macro `{macro_name}`: body step id '{id}' must be a \
                                     single author segment [A-Za-z_][A-Za-z0-9_-]* (no ':' \
                                     or '.')"
                                ),
                                Some(entry.value.span),
                            ));
                        }
                    }
                    // A non-string id falls through to the step normalizer's
                    // own diagnostic.
                }
                "then" | "else" => {
                    if let NodeValue::Sequence(items) = &mut entry.value.value {
                        for item in items {
                            self.prefix_step_ids(item, macro_name, call_id);
                        }
                    }
                }
                "foreach" => {
                    if let NodeValue::Mapping(sub) = &mut entry.value.value {
                        for sub_entry in sub.iter_mut() {
                            if sub_entry.key == "steps"
                                && let NodeValue::Sequence(items) = &mut sub_entry.value.value
                            {
                                for item in items {
                                    self.prefix_step_ids(item, macro_name, call_id);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Expands one macro call site into its normalized step sequence
    /// (03 §1.7): argument binding, AST substitution, hygiene prefixing,
    /// recursive normalization under the pushed origin frame.
    fn expand_macro(
        &mut self,
        name: &str,
        args_node: &Node,
        call_id: &str,
        call_site_span: DiagnosticSpan,
        preflight: Vec<NormalizedAssertion>,
    ) -> Vec<NormalizedStep> {
        if self.macro_stack.iter().any(|frame| frame == name) {
            self.error(
                codes::NORM_MACRO_RECURSION,
                format!(
                    "macro `{name}` expands itself (directly or mutually) — recursion is \
                     rejected (03 §1.7 rule 3)"
                ),
                Some(call_site_span),
            );
            return Vec::new();
        }
        if self.macro_stack.len() >= MAX_MACRO_DEPTH {
            self.error(
                codes::NORM_MACRO_RECURSION,
                format!("macro expansion depth exceeds the limit of {MAX_MACRO_DEPTH}"),
                Some(call_site_span),
            );
            return Vec::new();
        }
        let def = self.macros.get(name).expect("dispatch checked membership");
        let declared: Vec<String> = def.params.clone();
        let body: Vec<Node> = def.body.clone();

        // Bind call-site arguments: exactly the declared params, each once.
        let mut args: BTreeMap<String, Node> = BTreeMap::new();
        let mut ok = true;
        match &args_node.value {
            NodeValue::Null => {}
            NodeValue::Mapping(entries) => {
                for entry in entries {
                    if declared.contains(&entry.key) {
                        args.insert(entry.key.clone(), entry.value.clone());
                    } else {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_MACRO_PARAM,
                            format!(
                                "macro `{name}` has no param '{}' (declared: {})",
                                entry.key,
                                declared.join(", ")
                            ),
                            Some(entry.key_span),
                        );
                    }
                }
            }
            other => {
                self.error(
                    codes::NORM_BAD_MACRO_PARAM,
                    format!(
                        "a macro call takes a mapping of its params, got {}",
                        type_name_of(other)
                    ),
                    Some(args_node.span),
                );
                return Vec::new();
            }
        }
        for param in &declared {
            if !args.contains_key(param) {
                ok = false;
                self.error(
                    codes::NORM_BAD_MACRO_PARAM,
                    format!("macro `{name}`: required param '{param}' is missing"),
                    Some(args_node.span),
                );
            }
        }
        if !ok {
            return Vec::new();
        }

        // Substitute and prefix on clones of the body.
        let mut expanded_nodes = Vec::new();
        let mut expansion_ok = true;
        for step_node in &body {
            match self.substitute(step_node, name, &args) {
                Some(mut node) => {
                    self.prefix_step_ids(&mut node, name, call_id);
                    expanded_nodes.push(node);
                }
                None => expansion_ok = false,
            }
        }
        if !expansion_ok {
            return Vec::new();
        }

        // Normalize the expansion under its origin frame (02 §7): the
        // frame's span is the call site the author actually wrote.
        self.origin_stack.push(MacroOriginFrame {
            r#macro: Identifier::new(name).expect("macro names are identifiers by construction"),
            file: self.file.to_owned(),
            span: call_site_span.to_ir(),
        });
        self.macro_stack.push(name.to_owned());
        let mut steps = Vec::new();
        for node in &expanded_nodes {
            steps.extend(self.normalize_step(node));
        }
        self.macro_stack.pop();
        self.origin_stack.pop();

        // A call-site `preflight` probes the world before the first
        // expanded step runs (03 §2.2 precedent).
        if !preflight.is_empty()
            && let Some(first) = steps.first_mut()
        {
            let mut merged = preflight;
            merged.append(&mut first.preflight);
            first.preflight = merged;
        }
        steps
    }

    // ── steps ───────────────────────────────────────────────────────────

    /// Normalizes the top-level `steps` entry.
    fn normalize_steps(
        &mut self,
        entry: Option<&MapEntry>,
        root_span: DiagnosticSpan,
    ) -> Vec<NormalizedStep> {
        let Some(entry) = entry else {
            self.error(
                codes::NORM_MISSING_TOP_KEY,
                "required top-level key `steps` is missing",
                Some(root_span),
            );
            return Vec::new();
        };
        let NodeValue::Sequence(items) = &entry.value.value else {
            self.error(
                codes::NORM_BAD_STEPS_SHAPE,
                format!(
                    "`steps` must be a sequence, got {}",
                    entry.value.type_name()
                ),
                Some(entry.value.span),
            );
            return Vec::new();
        };
        if items.is_empty() {
            self.error(
                codes::NORM_BAD_STEPS_SHAPE,
                "`steps` must contain at least one step",
                Some(entry.value.span),
            );
            return Vec::new();
        }
        self.normalize_step_list(items)
    }

    /// Normalizes a nested step list (`then`/`else`/`foreach.steps`),
    /// splicing macro expansions in place.
    fn normalize_step_list(&mut self, items: &[Node]) -> Vec<NormalizedStep> {
        let mut steps = Vec::new();
        for item in items {
            steps.extend(self.normalize_step(item));
        }
        steps
    }

    /// Normalizes one step mapping. Head-key dispatch order is closed
    /// (03 §1.2): verbs → structure keys → expect-only assert step →
    /// flow-local macro names. A macro call site expands to several steps;
    /// every error path yields the empty sequence (diagnostics recorded).
    #[allow(clippy::too_many_lines)]
    fn normalize_step(&mut self, node: &Node) -> Vec<NormalizedStep> {
        let Some(entries) =
            expect_mapping(node, "a step", codes::NORM_BAD_STEPS_SHAPE, &mut self.diags)
        else {
            return Vec::new();
        };

        let mut id: Option<(String, DiagnosticSpan)> = None;
        let mut head: Option<PendingHead<'_>> = None;
        let mut head_keys: Vec<(String, DiagnosticSpan)> = Vec::new();
        let mut declared_effect: Option<(EffectClassAction, DiagnosticSpan)> = None;
        let mut idempotent: Option<(bool, DiagnosticSpan)> = None;
        let mut timeout_ms: Option<(u64, DiagnosticSpan)> = None;
        let mut checkpoint: Option<(bool, DiagnosticSpan)> = None;
        let mut expect_schema: Option<(JsonSchemaDocument, DiagnosticSpan)> = None;
        let mut expect: Option<(Vec<NormalizedAssertion>, DiagnosticSpan)> = None;
        let mut preflight: Vec<NormalizedAssertion> = Vec::new();
        let mut retry: Option<(RetryPolicy, DiagnosticSpan)> = None;
        let mut locate_via: Option<NormalizedChain> = None;
        let mut coordinate: Option<NormalizedCoordinate> = None;
        let mut inputs: Option<(&Node, DiagnosticSpan)> = None;
        let mut then_entry: Option<&MapEntry> = None;
        let mut else_entry: Option<&MapEntry> = None;
        let mut handlers: Vec<NormalizedHandler> = Vec::new();
        let mut handler_keys: Vec<(String, DiagnosticSpan)> = Vec::new();
        let mut ok = true;

        for entry in entries {
            let key = entry.key.as_str();
            if let Some((_, verb)) = VERB_HEADS_M1.iter().find(|(name, _)| *name == key) {
                head_keys.push((key.to_owned(), entry.key_span));
                match normalize_verb(*verb, key, entry.key_span, &entry.value, &mut self.diags) {
                    Some(verb_head) => head = Some(PendingHead::Verb(Box::new(verb_head))),
                    None => ok = false,
                }
                continue;
            }
            if let Some((_, verb)) = OBSERVE_HEADS.iter().find(|(name, _)| *name == key) {
                head_keys.push((key.to_owned(), entry.key_span));
                match self.normalize_observe(*verb, key, &entry.value) {
                    Some(wants) => {
                        head = Some(PendingHead::Observe {
                            verb: *verb,
                            head_span: entry.key_span,
                            wants,
                        });
                    }
                    None => ok = false,
                }
                continue;
            }
            if let Some((_, hook)) = HOOK_KEYS.iter().find(|(name, _)| *name == key) {
                handler_keys.push((key.to_owned(), entry.key_span));
                handlers.extend(self.normalize_handler_bindings(*hook, &entry.value));
                continue;
            }
            match key {
                "id" => match &entry.value.value {
                    NodeValue::String(raw) => id = Some((raw.clone(), entry.value.span)),
                    other => {
                        ok = false;
                        self.error(
                            codes::NORM_MISSING_STEP_ID,
                            format!("step `id` must be a string, got {}", type_name_of(other)),
                            Some(entry.value.span),
                        );
                    }
                },
                "invoke" => {
                    head_keys.push((key.to_owned(), entry.key_span));
                    match normalize_invoke(&entry.value, &mut self.diags) {
                        Some((action, action_span, args)) => {
                            head = Some(PendingHead::Invoke {
                                action,
                                action_span,
                                args,
                            });
                        }
                        None => ok = false,
                    }
                }
                "call" => {
                    head_keys.push((key.to_owned(), entry.key_span));
                    head = Some(PendingHead::Call { node: &entry.value });
                }
                "human" => {
                    head_keys.push((key.to_owned(), entry.key_span));
                    head = Some(PendingHead::Human(&entry.value));
                }
                "if" => {
                    head_keys.push((key.to_owned(), entry.key_span));
                    head = Some(PendingHead::If(&entry.value));
                }
                "foreach" => {
                    head_keys.push((key.to_owned(), entry.key_span));
                    head = Some(PendingHead::Foreach(&entry.value));
                }
                "let" => {
                    head_keys.push((key.to_owned(), entry.key_span));
                    head = Some(PendingHead::Let(&entry.value));
                }
                "then" => then_entry = Some(entry),
                "else" => else_entry = Some(entry),
                "inputs" => inputs = Some((&entry.value, entry.key_span)),
                "effect" => match &entry.value.value {
                    NodeValue::String(s) if s == "mutating" => {
                        declared_effect = Some((EffectClassAction::Mutating, entry.value.span));
                    }
                    NodeValue::String(s) if s == "readonly" => {
                        declared_effect = Some((EffectClassAction::Readonly, entry.value.span));
                    }
                    other => {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_EFFECT_VALUE,
                            format!(
                                "`effect` must be `mutating` or `readonly`, got {}",
                                match other {
                                    NodeValue::String(s) => format!("'{s}'"),
                                    other => type_name_of(other).to_owned(),
                                }
                            ),
                            Some(entry.value.span),
                        );
                    }
                },
                "idempotent" => match &entry.value.value {
                    NodeValue::Bool(b) => idempotent = Some((*b, entry.key_span)),
                    other => {
                        ok = false;
                        self.diags
                            .push(bad_step_value(key, "a boolean", other, entry.value.span));
                    }
                },
                "checkpoint" => match &entry.value.value {
                    NodeValue::Bool(b) => checkpoint = Some((*b, entry.key_span)),
                    other => {
                        ok = false;
                        self.diags
                            .push(bad_step_value(key, "a boolean", other, entry.value.span));
                    }
                },
                "timeout_ms" => match &entry.value.value {
                    NodeValue::Number(n) if n.as_u64().is_some_and(|v| v >= 1) => {
                        timeout_ms = n.as_u64().map(|v| (v, entry.key_span));
                    }
                    other => {
                        ok = false;
                        self.diags.push(bad_step_value(
                            key,
                            "a positive integer",
                            other,
                            entry.value.span,
                        ));
                    }
                },
                "expect" => {
                    expect = Some((
                        normalize_expect(&entry.value, &mut self.diags),
                        entry.key_span,
                    ));
                }
                "preflight" => {
                    preflight = normalize_expect(&entry.value, &mut self.diags);
                }
                "retry" => match normalize_retry(&entry.value, &mut self.diags) {
                    Some(policy) => retry = Some((policy, entry.key_span)),
                    None => ok = false,
                },
                "locate_via" => {
                    match normalize_chain(&entry.value, "locate_via", &mut self.diags) {
                        Some(chain) => locate_via = Some(chain),
                        None => ok = false,
                    }
                }
                "coordinate" => match normalize_coordinate(&entry.value, &mut self.diags) {
                    Some(value) => coordinate = Some(value),
                    None => ok = false,
                },
                "verify_via" => {
                    ok = false;
                    self.error(
                        codes::NORM_BAD_FALLBACK_SHAPE,
                        "`verify_via` is declared per assertion item inside `expect`/`preflight`, \
                         not at step level (03 §1.4)",
                        Some(entry.key_span),
                    );
                }
                "expect_schema" => {
                    // 03 §1.3: the invoke-output narrowing contract.
                    // Collected here; placement is validated once the
                    // head form is known (invoke only — verbs and
                    // observation heads have documented output shapes).
                    expect_schema = normalize_schema_document(
                        "`expect_schema`",
                        &entry.value,
                        codes::NORM_BAD_STEP_VALUE,
                        &mut self.diags,
                    )
                    .map(|schema| (schema, entry.key_span));
                    ok &= expect_schema.is_some();
                }
                key if STEP_NOT_M2.contains(&key) => {
                    ok = false;
                    self.error(
                        codes::NORM_STEP_KEY_NOT_IMPLEMENTED,
                        format!("not in the M2 subset: step key `{key}` is not implemented yet"),
                        Some(entry.key_span),
                    );
                }
                key if RESERVED.contains(&key) => {
                    ok = false;
                    self.error(
                        codes::NORM_RESERVED_KEYWORD,
                        format!("`{key}` is reserved for v0.2 (spine R6)"),
                        Some(entry.key_span),
                    );
                }
                key if self.macros.contains_key(key) => {
                    head_keys.push((key.to_owned(), entry.key_span));
                    head = Some(PendingHead::Macro {
                        name: key.to_owned(),
                        args: &entry.value,
                    });
                }
                key => {
                    ok = false;
                    self.error(
                        codes::NORM_UNKNOWN_STEP_KEY,
                        format!(
                            "unknown step key `{key}` (closed vocabulary: {}; head keys: \
                             invoke, tap, set_value, clear, wait_for, find, call, human, if, \
                             foreach, let, or a flow-local macro name)",
                            STEP_COMMON.join(", ")
                        ),
                        Some(entry.key_span),
                    );
                }
            }
        }

        if id.is_none() {
            self.error(
                codes::NORM_MISSING_STEP_ID,
                "step is missing the required `id` key",
                Some(node.span),
            );
            ok = false;
        }
        if head_keys.len() > 1 {
            let list = head_keys
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            self.error(
                codes::NORM_MULTIPLE_HEAD_KEYS,
                format!("a step must have exactly one head key, found: {list}"),
                Some(head_keys[1].1),
            );
            ok = false;
        }
        if head_keys.is_empty() && expect.is_none() {
            self.error(
                codes::NORM_MISSING_HEAD_KEY,
                "step has no head key and no `expect` (head keys: invoke, tap, set_value, \
                 clear, wait_for, find, call, human, if, foreach, let, or a flow-local macro \
                 name; an expect-only step is an assert step)",
                Some(node.span),
            );
            ok = false;
        }
        if !ok {
            return Vec::new();
        }
        let (id, id_span) = id.expect("checked above");

        // Macro call site: the step evaporates into the expansion; only
        // `id` and `preflight` travel with it (03 §1.7 / §2.2).
        if let Some(PendingHead::Macro { name, args }) = &head {
            let mut misplaced: Vec<(&str, DiagnosticSpan)> = Vec::new();
            if let Some((_, span)) = declared_effect {
                misplaced.push(("effect", span));
            }
            if let Some((_, span)) = idempotent {
                misplaced.push(("idempotent", span));
            }
            if let Some((_, span)) = timeout_ms {
                misplaced.push(("timeout_ms", span));
            }
            if let Some((_, span)) = checkpoint {
                misplaced.push(("checkpoint", span));
            }
            if let Some((_, span)) = &expect {
                misplaced.push(("expect", *span));
            }
            if let Some((_, span)) = &retry {
                misplaced.push(("retry", *span));
            }
            if let Some(chain) = &locate_via {
                misplaced.push(("locate_via", chain.span));
            }
            if let Some(value) = &coordinate {
                misplaced.push(("coordinate", value.span));
            }
            if let Some((_, span)) = inputs {
                misplaced.push(("inputs", span));
            }
            if let Some(entry) = then_entry {
                misplaced.push(("then", entry.key_span));
            }
            if let Some(entry) = else_entry {
                misplaced.push(("else", entry.key_span));
            }
            for (key, span) in &handler_keys {
                misplaced.push((key.as_str(), *span));
            }
            if !misplaced.is_empty() {
                for (key, span) in misplaced {
                    self.error(
                        codes::NORM_UNKNOWN_STEP_KEY,
                        format!(
                            "step key `{key}` is not allowed on a macro invocation \
                             (allowed: id, preflight — the call site evaporates, 03 §1.7)"
                        ),
                        Some(span),
                    );
                }
                return Vec::new();
            }
            let name = name.clone();
            let args: &Node = args;
            return self.expand_macro(&name, args, &id, node.span, preflight);
        }

        // Non-if steps must not carry `then`/`else`; non-call steps must
        // not carry `inputs`.
        if !matches!(head, Some(PendingHead::If(_))) {
            let mut misplaced = false;
            for entry in [then_entry, else_entry].into_iter().flatten() {
                self.error(
                    codes::NORM_BAD_IF_SHAPE,
                    format!("`{}` requires an `if` head key (03 §1.6)", entry.key),
                    Some(entry.key_span),
                );
                misplaced = true;
            }
            if misplaced {
                return Vec::new();
            }
        }
        if !matches!(head, Some(PendingHead::Call { .. }))
            && let Some((_, span)) = inputs
        {
            self.error(
                codes::NORM_BAD_CALL_SHAPE,
                "`inputs` requires a `call` head key (`invoke` takes `args` inside its own \
                 block, 03 §1.6)",
                Some(span),
            );
            return Vec::new();
        }

        // Action-only keys on non-action kinds.
        let is_action = matches!(
            head,
            Some(PendingHead::Verb(_))
                | Some(PendingHead::Invoke { .. })
                | Some(PendingHead::Observe { .. })
        );
        if !is_action {
            let mut misplaced: Vec<(&str, DiagnosticSpan)> = Vec::new();
            if let Some((_, span)) = declared_effect {
                misplaced.push(("effect", span));
            }
            if let Some((_, span)) = idempotent {
                misplaced.push(("idempotent", span));
            }
            if let Some(chain) = &locate_via {
                misplaced.push(("locate_via", chain.span));
            }
            if let Some(value) = &coordinate {
                misplaced.push(("coordinate", value.span));
            }
            if let Some((_, span)) = &retry {
                misplaced.push(("retry", *span));
            }
            if let Some((_, span)) = &expect_schema {
                // 03 §1.3: the invoke-output narrowing; inside a `human`
                // block the same key is the provideInput contract — at
                // step level on a structural head it is misplaced.
                misplaced.push(("expect_schema", *span));
            }
            if !misplaced.is_empty() {
                for (key, span) in misplaced {
                    let code = if key == "retry" {
                        codes::NORM_BAD_RETRY_SHAPE
                    } else {
                        codes::NORM_UNKNOWN_STEP_KEY
                    };
                    self.error(
                        code,
                        format!(
                            "step '{id}': `{key}` applies to action steps only (this step \
                             has no act phase)"
                        ),
                        Some(span),
                    );
                }
                return Vec::new();
            }
        }
        // `expect` is legal on action steps and (as the sole surface) on
        // assert steps.
        if !is_action
            && !head_keys.is_empty()
            && let Some((_, span)) = &expect
        {
            self.error(
                codes::NORM_BAD_EXPECT_SHAPE,
                format!(
                    "step '{id}': `expect` is legal on action steps and expect-only assert \
                     steps; structural steps carry no assertions (02 §4)"
                ),
                Some(*span),
            );
            return Vec::new();
        }

        let origin = self.current_origin();
        // Materialized default: true, but false inside macro expansions
        // (02 §3/§7); an explicit declaration wins either way.
        let checkpoint = checkpoint
            .map(|(value, _)| value)
            .unwrap_or_else(|| origin.is_empty());
        let idempotent_value = idempotent.map(|(value, _)| value).unwrap_or(false);
        let mut step_timeout = timeout_ms.map(|(value, _)| value);

        let kind = match head {
            None => {
                let (assertions, expect_span) = expect.expect("assert dispatch requires expect");
                if assertions.is_empty() {
                    self.error(
                        codes::NORM_BAD_EXPECT_SHAPE,
                        format!("step '{id}': an assert step needs at least one assertion"),
                        Some(expect_span),
                    );
                    return Vec::new();
                }
                NormalizedStepKind::Assert { expect: assertions }
            }
            Some(PendingHead::Verb(verb_head)) => {
                // Effect: verbs infer; an explicit declaration must agree
                // (03 §1.3 — neither side is silently trusted).
                let inferred = inferred_effect(verb_head.verb);
                if let Some((declared, effect_span)) = declared_effect
                    && declared != inferred
                {
                    self.error(
                        codes::NORM_EFFECT_CONFLICT,
                        format!(
                            "step '{id}': explicit `effect` contradicts the inferred effect of \
                             the verb (03 §1.3); drop the declaration or fix it"
                        ),
                        Some(effect_span),
                    );
                    return Vec::new();
                }
                if let Some((_, schema_span)) = &expect_schema {
                    self.error(
                        codes::NORM_BAD_STEP_VALUE,
                        "`expect_schema` narrows an `invoke` output (03 §1.3); element verbs \
                         have documented output shapes and take no narrowing",
                        Some(*schema_span),
                    );
                    return Vec::new();
                }
                NormalizedStepKind::Action(Box::new(NormalizedAction {
                    head: NormalizedHead::Verb(verb_head),
                    effect: Some(inferred),
                    idempotent: idempotent_value,
                    locate_via,
                    coordinate,
                    expect: expect.map(|(list, _)| list).unwrap_or_default(),
                    output_schema: None,
                    observation_head: false,
                }))
            }
            Some(PendingHead::Observe {
                verb,
                head_span,
                wants,
            }) => {
                // Readonly by construction, like the other verbs: an
                // explicit declaration must agree rather than be trusted
                // (03 §1.3).
                let inferred = inferred_effect(verb);
                if let Some((declared, effect_span)) = declared_effect
                    && declared != inferred
                {
                    self.error(
                        codes::NORM_EFFECT_CONFLICT,
                        format!(
                            "step '{id}': explicit `effect` contradicts the inferred effect of \
                             the verb (03 §1.3); drop the declaration or fix it"
                        ),
                        Some(effect_span),
                    );
                    return Vec::new();
                }
                // The act-chain surface is meaningless without a selector.
                let mut invalid = false;
                for (label, span) in [
                    locate_via.as_ref().map(|chain| ("locate_via", chain.span)),
                    coordinate.as_ref().map(|value| ("coordinate", value.span)),
                ]
                .into_iter()
                .flatten()
                {
                    self.error(
                        codes::NORM_BAD_FALLBACK_SHAPE,
                        format!(
                            "step '{id}': `{label}` requires an element verb; \
                             `{}` locates nothing",
                            observe_action_name(verb)
                        ),
                        Some(span),
                    );
                    invalid = true;
                }
                if invalid {
                    return Vec::new();
                }
                // Lower to the provider-synthetic action of the same name
                // (04 §9.4.3): the IR carries only native action names and
                // the runner keeps its single `execute` contract.
                let args = match wants {
                    Some(parts) => vec![NormalizedArg {
                        name: Identifier::new("wants").expect("`wants` is a valid identifier"),
                        name_span: head_span,
                        value: NormalizedValue::Literal {
                            value: serde_json::Value::Array(
                                parts.into_iter().map(serde_json::Value::String).collect(),
                            ),
                            span: head_span,
                        },
                    }],
                    None => Vec::new(),
                };
                if let Some((_, schema_span)) = &expect_schema {
                    self.error(
                        codes::NORM_BAD_STEP_VALUE,
                        "`expect_schema` narrows an `invoke` output (03 §1.3); observation \
                         heads have the documented observation output shape",
                        Some(*schema_span),
                    );
                    return Vec::new();
                }
                NormalizedStepKind::Action(Box::new(NormalizedAction {
                    head: NormalizedHead::Invoke {
                        action: ActionName::new(observe_action_name(verb))
                            .expect("the synthetic action names are valid"),
                        action_span: head_span,
                        args,
                    },
                    effect: Some(inferred),
                    idempotent: idempotent_value,
                    locate_via: None,
                    coordinate: None,
                    expect: expect.map(|(list, _)| list).unwrap_or_default(),
                    output_schema: None,
                    observation_head: true,
                }))
            }
            Some(PendingHead::Invoke {
                action,
                action_span,
                args,
            }) => {
                // The fallback surface belongs to verb steps (03 §1.4).
                let mut invalid = false;
                if let Some(chain) = &locate_via {
                    self.error(
                        codes::NORM_BAD_FALLBACK_SHAPE,
                        format!("step '{id}': `locate_via` requires a verb head key, not `invoke`"),
                        Some(chain.span),
                    );
                    invalid = true;
                }
                if let Some(value) = &coordinate {
                    self.error(
                        codes::NORM_BAD_FALLBACK_SHAPE,
                        format!("step '{id}': `coordinate` requires a verb head key, not `invoke`"),
                        Some(value.span),
                    );
                    invalid = true;
                }
                if invalid {
                    return Vec::new();
                }
                NormalizedStepKind::Action(Box::new(NormalizedAction {
                    head: NormalizedHead::Invoke {
                        action,
                        action_span,
                        args,
                    },
                    effect: declared_effect.map(|(effect, _)| effect),
                    idempotent: idempotent_value,
                    locate_via: None,
                    coordinate: None,
                    expect: expect.map(|(list, _)| list).unwrap_or_default(),
                    output_schema: expect_schema.map(|(schema, _)| schema),
                    observation_head: false,
                }))
            }
            Some(PendingHead::Call { node: path_node }) => {
                let subflow = self.link_subflow(path_node, codes::NORM_BAD_CALL_SHAPE, "call");
                let inputs = match inputs {
                    Some((node, _)) => self.normalize_call_inputs(node),
                    None => Some(Vec::new()),
                };
                let (Some(subflow), Some(inputs)) = (subflow, inputs) else {
                    return Vec::new();
                };
                NormalizedStepKind::Call(Box::new(NormalizedCall {
                    subflow,
                    inputs,
                    call_span: path_node.span,
                }))
            }
            Some(PendingHead::Human(human_node)) => {
                let Some(mut human) = self.normalize_human(human_node, HumanContext::Step) else {
                    return Vec::new();
                };
                // Exactly one timeout surface: in-block or step level
                // (03 §1.6/§1.8; single representation).
                match (human.timeout_ms, step_timeout) {
                    (Some(_), Some(_)) => {
                        self.error(
                            codes::NORM_BAD_HUMAN_SHAPE,
                            format!(
                                "step '{id}': `timeout_ms` is declared both inside the `human` \
                                 block and at step level — write it once"
                            ),
                            Some(human.span),
                        );
                        return Vec::new();
                    }
                    (None, None) => {
                        self.error(
                            codes::NORM_BAD_HUMAN_SHAPE,
                            format!(
                                "step '{id}': a human step requires `timeout_ms` (expiry yields \
                                 verdict unknown; unbounded waits are runSuspended, 02 §4.4)"
                            ),
                            Some(node.span),
                        );
                        return Vec::new();
                    }
                    (None, Some(value)) => human.timeout_ms = Some(value),
                    (Some(_), None) => {}
                }
                // The budget lives in HumanStepIR.timeoutMs; StepBase's
                // optional slot stays empty (single representation).
                step_timeout = None;
                NormalizedStepKind::Human(Box::new(human))
            }
            Some(PendingHead::If(cond_node)) => {
                let cond = normalize_value(cond_node, &mut self.diags);
                let then_steps = match then_entry {
                    Some(entry) => match &entry.value.value {
                        NodeValue::Sequence(items) if !items.is_empty() => {
                            Some(self.normalize_step_list(items))
                        }
                        other => {
                            self.error(
                                codes::NORM_BAD_IF_SHAPE,
                                format!(
                                    "`then` must be a non-empty step sequence, got {}",
                                    match other {
                                        NodeValue::Sequence(_) => "an empty sequence".to_owned(),
                                        other => type_name_of(other).to_owned(),
                                    }
                                ),
                                Some(entry.value.span),
                            );
                            None
                        }
                    },
                    None => {
                        self.error(
                            codes::NORM_BAD_IF_SHAPE,
                            format!("step '{id}': `if` requires a `then` branch (03 §1.6)"),
                            Some(node.span),
                        );
                        None
                    }
                };
                let else_steps = match else_entry {
                    Some(entry) => match &entry.value.value {
                        NodeValue::Sequence(items) if !items.is_empty() => {
                            Some(Some(self.normalize_step_list(items)))
                        }
                        other => {
                            self.error(
                                codes::NORM_BAD_IF_SHAPE,
                                format!(
                                    "`else` must be a non-empty step sequence, got {}",
                                    match other {
                                        NodeValue::Sequence(_) => "an empty sequence".to_owned(),
                                        other => type_name_of(other).to_owned(),
                                    }
                                ),
                                Some(entry.value.span),
                            );
                            None
                        }
                    },
                    None => Some(None),
                };
                let (Some(cond), Some(then), Some(r#else)) = (cond, then_steps, else_steps) else {
                    return Vec::new();
                };
                NormalizedStepKind::If(Box::new(NormalizedIf { cond, then, r#else }))
            }
            Some(PendingHead::Foreach(body_node)) => {
                let Some(foreach) = self.normalize_foreach(&id, body_node) else {
                    return Vec::new();
                };
                NormalizedStepKind::Foreach(Box::new(foreach))
            }
            Some(PendingHead::Let(bindings_node)) => {
                let Some(bindings) = self.normalize_let(&id, bindings_node) else {
                    return Vec::new();
                };
                NormalizedStepKind::Let { bindings }
            }
            Some(PendingHead::Macro { .. }) => unreachable!("expanded above"),
        };

        vec![NormalizedStep {
            id,
            id_span,
            origin,
            kind,
            preflight,
            retry: retry.map(|(policy, _)| policy),
            timeout_ms: step_timeout,
            checkpoint,
            handlers,
            span: node.span,
        }]
    }

    /// Parses a `foreach` block (`in` / `as` / `steps`, 03 §1.6).
    fn normalize_foreach(&mut self, id: &str, node: &Node) -> Option<NormalizedForeach> {
        let entries = expect_mapping(
            node,
            "`foreach`",
            codes::NORM_BAD_FOREACH_SHAPE,
            &mut self.diags,
        )?;
        let mut items_value = None;
        let mut as_binding: Option<(Identifier, DiagnosticSpan)> = None;
        let mut body = None;
        let mut ok = true;
        for entry in entries {
            match entry.key.as_str() {
                "in" => {
                    items_value = normalize_value(&entry.value, &mut self.diags);
                    ok &= items_value.is_some();
                }
                "as" => match &entry.value.value {
                    NodeValue::String(raw) => match Identifier::new(raw.clone()) {
                        Ok(name) => as_binding = Some((name, entry.value.span)),
                        Err(err) => {
                            ok = false;
                            self.error(
                                codes::NORM_BAD_FOREACH_SHAPE,
                                format!("invalid `as` name '{raw}': {}", err.reason),
                                Some(entry.value.span),
                            );
                        }
                    },
                    other => {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_FOREACH_SHAPE,
                            format!(
                                "`foreach.as` must be an identifier string, got {}",
                                type_name_of(other)
                            ),
                            Some(entry.value.span),
                        );
                    }
                },
                "steps" => match &entry.value.value {
                    NodeValue::Sequence(nodes) if !nodes.is_empty() => {
                        body = Some(self.normalize_step_list(nodes));
                    }
                    other => {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_FOREACH_SHAPE,
                            format!(
                                "`foreach.steps` must be a non-empty step sequence, got {}",
                                match other {
                                    NodeValue::Sequence(_) => "an empty sequence".to_owned(),
                                    other => type_name_of(other).to_owned(),
                                }
                            ),
                            Some(entry.value.span),
                        );
                    }
                },
                key => {
                    ok = false;
                    self.error(
                        codes::NORM_BAD_FOREACH_SHAPE,
                        format!("unknown `foreach` subkey `{key}` (closed: in, as, steps)"),
                        Some(entry.key_span),
                    );
                }
            }
        }
        if ok && (items_value.is_none() || as_binding.is_none() || body.is_none()) {
            self.error(
                codes::NORM_BAD_FOREACH_SHAPE,
                format!("step '{id}': `foreach` requires `in`, `as` and `steps` (03 §1.6)"),
                Some(node.span),
            );
            ok = false;
        }
        if !ok {
            return None;
        }
        let (r#as, as_span) = as_binding.expect("checked above");
        Some(NormalizedForeach {
            items: items_value.expect("checked above"),
            r#as,
            as_span,
            body: body.expect("checked above"),
        })
    }

    /// Parses a `let` block (03 §1.6: `vars.<name>` bindings, ≥ 1).
    fn normalize_let(&mut self, id: &str, node: &Node) -> Option<Vec<NormalizedBinding>> {
        let entries = expect_mapping(node, "`let`", codes::NORM_BAD_LET_SHAPE, &mut self.diags)?;
        if entries.is_empty() {
            self.error(
                codes::NORM_BAD_LET_SHAPE,
                format!("step '{id}': `let` must carry at least one binding"),
                Some(node.span),
            );
            return None;
        }
        let mut bindings = Vec::new();
        let mut ok = true;
        for entry in entries {
            let Ok(name) = Identifier::new(entry.key.clone()) else {
                ok = false;
                self.error(
                    codes::NORM_BAD_LET_SHAPE,
                    format!(
                        "binding name '{}' must match [A-Za-z_][A-Za-z0-9_]*",
                        entry.key
                    ),
                    Some(entry.key_span),
                );
                continue;
            };
            match normalize_value(&entry.value, &mut self.diags) {
                Some(value) => bindings.push(NormalizedBinding {
                    name,
                    name_span: entry.key_span,
                    value,
                }),
                None => ok = false,
            }
        }
        ok.then_some(bindings)
    }

    /// Parses `call.inputs` (03 §1.6: caller-scope expressions by name).
    fn normalize_call_inputs(&mut self, node: &Node) -> Option<Vec<NormalizedArg>> {
        let entries = expect_mapping(
            node,
            "`inputs`",
            codes::NORM_BAD_CALL_SHAPE,
            &mut self.diags,
        )?;
        let mut inputs = Vec::new();
        let mut ok = true;
        for entry in entries {
            let Ok(name) = Identifier::new(entry.key.clone()) else {
                ok = false;
                self.error(
                    codes::NORM_BAD_CALL_SHAPE,
                    format!(
                        "input name '{}' must match [A-Za-z_][A-Za-z0-9_]*",
                        entry.key
                    ),
                    Some(entry.key_span),
                );
                continue;
            };
            match normalize_value(&entry.value, &mut self.diags) {
                Some(value) => inputs.push(NormalizedArg {
                    name,
                    name_span: entry.key_span,
                    value,
                }),
                None => ok = false,
            }
        }
        ok.then_some(inputs)
    }

    /// Validates a subflow path value and links it (`call` / `repair`).
    fn link_subflow(
        &mut self,
        path_node: &Node,
        code: &'static str,
        key: &str,
    ) -> Option<SubflowRecord> {
        let NodeValue::String(path) = &path_node.value else {
            self.error(
                code,
                format!(
                    "`{key}` must be a relative subflow path string, got {}",
                    path_node.type_name()
                ),
                Some(path_node.span),
            );
            return None;
        };
        if path.is_empty() {
            self.error(
                code,
                format!("`{key}` must be a non-empty relative path"),
                Some(path_node.span),
            );
            return None;
        }
        if std::path::Path::new(path).is_absolute() {
            self.error(
                code,
                format!(
                    "`{key}` must be a relative path, resolved against the current file \
                     (03 §1.1)"
                ),
                Some(path_node.span),
            );
            return None;
        }
        self.linker
            .link(self.file, path, path_node.span, &mut self.diags)
    }

    // ── human blocks ────────────────────────────────────────────────────

    /// Parses a `human` block (03 §1.6) — shared by human steps and
    /// `escalate` handler actions (03 §1.8).
    fn normalize_human(&mut self, node: &Node, context: HumanContext) -> Option<NormalizedHuman> {
        let what = match context {
            HumanContext::Step => "a `human` block",
            HumanContext::Escalate => "an `escalate` block",
        };
        let entries = expect_mapping(node, what, codes::NORM_BAD_HUMAN_SHAPE, &mut self.diags)?;
        let mut mode: Option<(HumanMode, DiagnosticSpan)> = None;
        let mut prompt = None;
        let mut presents = Vec::new();
        let mut decisions = None;
        let mut output_schema: Option<(JsonSchemaDocument, DiagnosticSpan)> = None;
        let mut timeout_ms = None;
        let mut ok = true;
        for entry in entries {
            match entry.key.as_str() {
                "mode" => {
                    let parsed = match &entry.value.value {
                        NodeValue::String(s) => match s.as_str() {
                            "confirm" => Some(HumanMode::Confirm),
                            "judge" => Some(HumanMode::Judge),
                            "provideInput" => Some(HumanMode::ProvideInput),
                            "repairWorld" => Some(HumanMode::RepairWorld),
                            _ => None,
                        },
                        _ => None,
                    };
                    match parsed {
                        Some(value) => mode = Some((value, entry.value.span)),
                        None => {
                            ok = false;
                            self.error(
                                codes::NORM_BAD_HUMAN_SHAPE,
                                format!(
                                    "`mode` must be one of confirm, judge, provideInput, \
                                     repairWorld, got {}",
                                    match &entry.value.value {
                                        NodeValue::String(s) => format!("'{s}'"),
                                        other => type_name_of(other).to_owned(),
                                    }
                                ),
                                Some(entry.value.span),
                            );
                        }
                    }
                }
                "prompt" => match &entry.value.value {
                    NodeValue::String(s) if !s.is_empty() && s.chars().count() <= 16384 => {
                        prompt = Some(s.clone());
                    }
                    other => {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_HUMAN_SHAPE,
                            format!(
                                "`prompt` must be a non-empty string of at most 16384 \
                                 characters, got {}",
                                type_name_of(other)
                            ),
                            Some(entry.value.span),
                        );
                    }
                },
                "presents" => match &entry.value.value {
                    NodeValue::Sequence(items) => {
                        for item in items {
                            match normalize_value(item, &mut self.diags) {
                                Some(value) => presents.push(value),
                                None => ok = false,
                            }
                        }
                    }
                    other => {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_HUMAN_SHAPE,
                            format!(
                                "`presents` must be a sequence of expressions, got {}",
                                type_name_of(other)
                            ),
                            Some(entry.value.span),
                        );
                    }
                },
                "decisions" => match &entry.value.value {
                    NodeValue::Sequence(items) if !items.is_empty() => {
                        let mut options = Vec::new();
                        for item in items {
                            match &item.value {
                                NodeValue::String(s)
                                    if !s.is_empty() && s.chars().count() <= 256 =>
                                {
                                    options.push(s.clone());
                                }
                                other => {
                                    ok = false;
                                    self.error(
                                        codes::NORM_BAD_HUMAN_SHAPE,
                                        format!(
                                            "`decisions` entries must be non-empty strings of \
                                             at most 256 characters, got {}",
                                            type_name_of(other)
                                        ),
                                        Some(item.span),
                                    );
                                }
                            }
                        }
                        decisions = Some(options);
                    }
                    other => {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_HUMAN_SHAPE,
                            format!(
                                "`decisions` must be a non-empty sequence of strings, got {}",
                                match other {
                                    NodeValue::Sequence(_) => "an empty sequence".to_owned(),
                                    other => type_name_of(other).to_owned(),
                                }
                            ),
                            Some(entry.value.span),
                        );
                    }
                },
                "expect_schema" => {
                    output_schema = normalize_schema_document(
                        "`expect_schema`",
                        &entry.value,
                        codes::NORM_BAD_HUMAN_SHAPE,
                        &mut self.diags,
                    )
                    .map(|schema| (schema, entry.value.span));
                    ok &= output_schema.is_some();
                }
                "timeout_ms" => match &entry.value.value {
                    NodeValue::Number(n) if n.as_u64().is_some_and(|v| v >= 1) => {
                        timeout_ms = n.as_u64();
                    }
                    other => {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_HUMAN_SHAPE,
                            format!(
                                "`timeout_ms` must be a positive integer, got {}",
                                type_name_of(other)
                            ),
                            Some(entry.value.span),
                        );
                    }
                },
                "on_timeout" => match &entry.value.value {
                    NodeValue::String(s) if s == "unknown" => {}
                    other => {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_HUMAN_SHAPE,
                            format!(
                                "`on_timeout` is the fixed literal `unknown` (principle 4); \
                                 got {}",
                                match other {
                                    NodeValue::String(s) => format!("'{s}'"),
                                    other => type_name_of(other).to_owned(),
                                }
                            ),
                            Some(entry.value.span),
                        );
                    }
                },
                key => {
                    ok = false;
                    self.error(
                        codes::NORM_BAD_HUMAN_SHAPE,
                        format!(
                            "unknown human subkey `{key}` (closed: mode, prompt, presents, \
                             decisions, expect_schema, timeout_ms, on_timeout)"
                        ),
                        Some(entry.key_span),
                    );
                }
            }
        }
        if mode.is_none() && ok {
            self.error(
                codes::NORM_BAD_HUMAN_SHAPE,
                "a human block is missing the required `mode`",
                Some(node.span),
            );
            ok = false;
        }
        if prompt.is_none() && ok {
            self.error(
                codes::NORM_BAD_HUMAN_SHAPE,
                "a human block is missing the required `prompt`",
                Some(node.span),
            );
            ok = false;
        }
        if !ok {
            return None;
        }
        let (mode, _) = mode.expect("checked above");
        // `expect_schema` ⟺ provideInput (03 §1.6; the IR conditional
        // requires it there and it has no meaning elsewhere).
        if mode == HumanMode::ProvideInput && output_schema.is_none() {
            self.error(
                codes::NORM_BAD_HUMAN_SHAPE,
                "`mode: provideInput` requires `expect_schema` (the input contract, 02 §4.4)",
                Some(node.span),
            );
            return None;
        }
        if mode != HumanMode::ProvideInput
            && let Some((_, span)) = &output_schema
        {
            self.error(
                codes::NORM_BAD_HUMAN_SHAPE,
                "`expect_schema` is only meaningful with `mode: provideInput` (03 §1.6)",
                Some(*span),
            );
            return None;
        }
        if context == HumanContext::Escalate && timeout_ms.is_none() {
            self.error(
                codes::NORM_BAD_HUMAN_SHAPE,
                "an `escalate` block requires `timeout_ms` (02 §4.4: human steps carry a \
                 mandatory budget)",
                Some(node.span),
            );
            return None;
        }
        Some(NormalizedHuman {
            mode,
            prompt: prompt.expect("checked above"),
            presents,
            decisions,
            output_schema: output_schema.map(|(schema, _)| schema),
            timeout_ms,
            span: node.span,
        })
    }

    // ── handlers ────────────────────────────────────────────────────────

    /// Parses the flow-level `handlers:` block (mapping hook → binding or
    /// binding list, 03 §1.8).
    fn normalize_handler_block(&mut self, node: &Node) -> Vec<NormalizedHandler> {
        let Some(entries) = expect_mapping(
            node,
            "`handlers`",
            codes::NORM_BAD_HANDLER_SHAPE,
            &mut self.diags,
        ) else {
            return Vec::new();
        };
        let mut handlers = Vec::new();
        for entry in entries {
            let Some((_, hook)) = HOOK_KEYS.iter().find(|(key, _)| *key == entry.key) else {
                self.error(
                    codes::NORM_BAD_HANDLER_SHAPE,
                    format!(
                        "unknown handler hook `{}` (closed: on_fail, on_unknown, on_error, \
                         on_resume_drift)",
                        entry.key
                    ),
                    Some(entry.key_span),
                );
                continue;
            };
            handlers.extend(self.normalize_handler_bindings(*hook, &entry.value));
        }
        handlers
    }

    /// Parses one hook's value: a binding map or a sequence of binding
    /// maps (03 §1.8).
    /// `observe: [screenshot, uiSnapshot]` / `screenshot: {}` (03 §1.3).
    ///
    /// Returns the requested parts for `observe`, or `None`-inside-`Some`
    /// for the `screenshot` shortcut, whose synthetic action fixes
    /// `wants` itself and therefore takes no arguments. A parse failure is
    /// the outer `None`.
    fn normalize_observe(
        &mut self,
        verb: CanonicalVerb,
        verb_key: &str,
        node: &Node,
    ) -> Option<Option<Vec<String>>> {
        if verb == CanonicalVerb::Screenshot {
            // The shortcut form takes an empty mapping: it is the
            // single-part spelling of `observe: [screenshot]`, so there is
            // nothing left to say.
            return match &node.value {
                NodeValue::Mapping(entries) if entries.is_empty() => Some(None),
                NodeValue::Mapping(entries) => {
                    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
                    self.error(
                        codes::NORM_BAD_VERB_SHAPE,
                        format!(
                            "step verb `screenshot` takes an empty mapping `{{}}` (it is the \
                             single-part form of `observe`); unexpected subkey(s): {}",
                            keys.join(", ")
                        ),
                        Some(node.span),
                    );
                    None
                }
                other => {
                    self.error(
                        codes::NORM_BAD_VERB_SHAPE,
                        format!(
                            "step verb `screenshot` takes an empty mapping `{{}}`, got {}",
                            type_name_of(other)
                        ),
                        Some(node.span),
                    );
                    None
                }
            };
        }

        let NodeValue::Sequence(items) = &node.value else {
            self.error(
                codes::NORM_BAD_VERB_SHAPE,
                format!(
                    "step verb `{verb_key}` takes a list of observation parts (closed: {}), got {}",
                    OBSERVE_WANTS.join(", "),
                    type_name_of(&node.value)
                ),
                Some(node.span),
            );
            return None;
        };
        if items.is_empty() {
            self.error(
                codes::NORM_BAD_VERB_SHAPE,
                format!(
                    "step verb `{verb_key}` needs at least one observation part (closed: {})",
                    OBSERVE_WANTS.join(", ")
                ),
                Some(node.span),
            );
            return None;
        }
        let mut wants: Vec<String> = Vec::new();
        let mut ok = true;
        for item in items {
            let NodeValue::String(raw) = &item.value else {
                ok = false;
                self.error(
                    codes::NORM_BAD_VERB_SHAPE,
                    format!(
                        "step verb `{verb_key}`: each observation part is a string (closed: {}), \
                         got {}",
                        OBSERVE_WANTS.join(", "),
                        type_name_of(&item.value)
                    ),
                    Some(item.span),
                );
                continue;
            };
            if !OBSERVE_WANTS.contains(&raw.as_str()) {
                ok = false;
                self.error(
                    codes::NORM_BAD_VERB_SHAPE,
                    format!(
                        "step verb `{verb_key}`: unknown observation part `{raw}` (closed: {})",
                        OBSERVE_WANTS.join(", ")
                    ),
                    Some(item.span),
                );
                continue;
            }
            if wants.iter().any(|seen| seen == raw) {
                ok = false;
                self.error(
                    codes::NORM_BAD_VERB_SHAPE,
                    format!("step verb `{verb_key}`: observation part `{raw}` is repeated"),
                    Some(item.span),
                );
                continue;
            }
            wants.push(raw.clone());
        }
        ok.then_some(Some(wants))
    }

    fn normalize_handler_bindings(
        &mut self,
        hook: HandlerHook,
        node: &Node,
    ) -> Vec<NormalizedHandler> {
        match &node.value {
            NodeValue::Sequence(items) => items
                .iter()
                .filter_map(|item| self.normalize_handler_binding(hook, item))
                .collect(),
            NodeValue::Mapping(_) => self
                .normalize_handler_binding(hook, node)
                .into_iter()
                .collect(),
            other => {
                self.error(
                    codes::NORM_BAD_HANDLER_SHAPE,
                    format!(
                        "a handler hook takes a binding map or a sequence of binding maps, \
                         got {}",
                        type_name_of(other)
                    ),
                    Some(node.span),
                );
                Vec::new()
            }
        }
    }

    /// Parses one handler binding: exactly one Disposition head key plus
    /// `max_triggers` (and `error_classes` on `on_error` only, 03 §1.8).
    fn normalize_handler_binding(
        &mut self,
        hook: HandlerHook,
        node: &Node,
    ) -> Option<NormalizedHandler> {
        let entries = expect_mapping(
            node,
            "a handler binding",
            codes::NORM_BAD_HANDLER_SHAPE,
            &mut self.diags,
        )?;
        let mut error_classes: Option<BTreeSet<ErrorClass>> = None;
        let mut max_triggers = None;
        let mut action: Option<NormalizedHandlerAction> = None;
        let mut dispositions: Vec<(String, DiagnosticSpan)> = Vec::new();
        let mut ok = true;
        for entry in entries {
            let key = entry.key.as_str();
            if DISPOSITION_HEADS.contains(&key) {
                dispositions.push((key.to_owned(), entry.key_span));
                match key {
                    "retry" => {
                        action = normalize_retry(&entry.value, &mut self.diags)
                            .map(NormalizedHandlerAction::Retry);
                        ok &= action.is_some();
                    }
                    "continue" | "abort" => {
                        let empty = match &entry.value.value {
                            NodeValue::Null => true,
                            NodeValue::Mapping(entries) => entries.is_empty(),
                            _ => false,
                        };
                        if empty {
                            action = Some(if key == "continue" {
                                NormalizedHandlerAction::Continue
                            } else {
                                NormalizedHandlerAction::Abort
                            });
                        } else {
                            ok = false;
                            self.error(
                                codes::NORM_BAD_HANDLER_SHAPE,
                                format!("`{key}` takes an empty value (`{{}}`), 03 §1.8"),
                                Some(entry.value.span),
                            );
                        }
                    }
                    "escalate" => {
                        action = self
                            .normalize_human(&entry.value, HumanContext::Escalate)
                            .map(NormalizedHandlerAction::Escalate);
                        ok &= action.is_some();
                    }
                    "repair" => {
                        action = self
                            .link_subflow(&entry.value, codes::NORM_BAD_HANDLER_SHAPE, "repair")
                            .map(|subflow| NormalizedHandlerAction::Repair { subflow });
                        ok &= action.is_some();
                    }
                    _ => unreachable!("DISPOSITION_HEADS is closed"),
                }
                continue;
            }
            match key {
                "error_classes" => {
                    if hook != HandlerHook::OnError {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_HANDLER_SHAPE,
                            "`error_classes` is legal only on `on_error` (03 §1.8)",
                            Some(entry.key_span),
                        );
                        continue;
                    }
                    match parse_error_classes(
                        &entry.value,
                        codes::NORM_BAD_HANDLER_SHAPE,
                        &mut self.diags,
                    ) {
                        Some(classes) => error_classes = Some(classes),
                        None => ok = false,
                    }
                }
                "max_triggers" => match &entry.value.value {
                    NodeValue::Number(n)
                        if n.as_u64()
                            .is_some_and(|v| v >= 1 && v <= u64::from(u32::MAX)) =>
                    {
                        max_triggers = n.as_u64().map(|v| v as u32);
                    }
                    other => {
                        ok = false;
                        self.error(
                            codes::NORM_BAD_HANDLER_SHAPE,
                            format!(
                                "`max_triggers` must be a positive integer, got {}",
                                type_name_of(other)
                            ),
                            Some(entry.value.span),
                        );
                    }
                },
                key => {
                    ok = false;
                    self.error(
                        codes::NORM_BAD_HANDLER_SHAPE,
                        format!(
                            "unknown handler subkey `{key}` (closed: retry, continue, \
                             escalate, abort, repair, error_classes, max_triggers)"
                        ),
                        Some(entry.key_span),
                    );
                }
            }
        }
        if dispositions.len() != 1 {
            let span = dispositions
                .get(1)
                .map(|(_, span)| *span)
                .unwrap_or(node.span);
            self.error(
                codes::NORM_BAD_HANDLER_SHAPE,
                format!(
                    "a handler binding carries exactly one disposition head key (retry | \
                     continue | escalate | abort | repair), found {}",
                    dispositions.len()
                ),
                Some(span),
            );
            ok = false;
        }
        if max_triggers.is_none() && ok {
            self.error(
                codes::NORM_BAD_HANDLER_SHAPE,
                "a handler binding is missing the required `max_triggers` (loop guard)",
                Some(node.span),
            );
            ok = false;
        }
        if !ok {
            return None;
        }
        Some(NormalizedHandler {
            hook,
            error_classes,
            action: action.expect("exactly one disposition parsed"),
            max_triggers: max_triggers.expect("checked above"),
            span: node.span,
        })
    }
}

/// The head-key dispatch intermediate of one step (03 §1.2).
enum PendingHead<'n> {
    /// One of the five element verbs, already normalized.
    Verb(Box<NormalizedVerb>),
    /// `observe:` / `screenshot:` — the observation verbs (04 §9.4.3).
    /// They lower to the provider-synthetic action of the same name; the
    /// captured parts travel as the `wants` argument.
    Observe {
        /// Which observation verb was written (drives the effect and the
        /// synthetic action name).
        verb: CanonicalVerb,
        /// Span of the head key.
        head_span: DiagnosticSpan,
        /// The requested parts, in vocabulary order; `None` for the
        /// `screenshot` shortcut, whose action takes no arguments.
        wants: Option<Vec<String>>,
    },
    /// The `invoke` escape hatch, already normalized.
    Invoke {
        /// The provider-native action name.
        action: ActionName,
        /// Span of the action value.
        action_span: DiagnosticSpan,
        /// Ordered invoke arguments.
        args: Vec<NormalizedArg>,
    },
    /// `call:` — the raw path node (linked during assembly).
    Call {
        /// The path value node.
        node: &'n Node,
    },
    /// `human:` — the raw block node.
    Human(&'n Node),
    /// `if:` — the raw condition node.
    If(&'n Node),
    /// `foreach:` — the raw block node.
    Foreach(&'n Node),
    /// `let:` — the raw bindings node.
    Let(&'n Node),
    /// A flow-local macro call (03 §1.2 dispatch class 4).
    Macro {
        /// The macro name.
        name: String,
        /// The raw call-site argument node.
        args: &'n Node,
    },
}

/// Parses a `retry` policy (03 §1.2: `max_attempts` / `backoff_ms` /
/// `retry_on`) into the IR [`RetryPolicy`] — every field is static.
fn normalize_retry(node: &Node, diags: &mut Vec<CompileDiagnostic>) -> Option<RetryPolicy> {
    let entries = expect_mapping(node, "`retry`", codes::NORM_BAD_RETRY_SHAPE, diags)?;
    let mut max_attempts = None;
    let mut backoff_ms = None;
    let mut retry_on = None;
    let mut ok = true;
    for entry in entries {
        match entry.key.as_str() {
            "max_attempts" => match &entry.value.value {
                NodeValue::Number(n)
                    if n.as_u64()
                        .is_some_and(|v| v >= 1 && v <= u64::from(u32::MAX)) =>
                {
                    max_attempts = n.as_u64().map(|v| v as u32);
                }
                other => {
                    ok = false;
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_RETRY_SHAPE,
                        format!(
                            "`max_attempts` must be a positive integer, got {}",
                            type_name_of(other)
                        ),
                        Some(entry.value.span),
                    ));
                }
            },
            "backoff_ms" => {
                backoff_ms = normalize_backoff(&entry.value, diags);
                ok &= backoff_ms.is_some();
            }
            "retry_on" => {
                match parse_error_classes(&entry.value, codes::NORM_BAD_RETRY_SHAPE, diags) {
                    Some(classes) => retry_on = Some(classes),
                    None => ok = false,
                }
            }
            key => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_RETRY_SHAPE,
                    format!(
                        "unknown `retry` subkey `{key}` (closed: max_attempts, backoff_ms, \
                         retry_on)"
                    ),
                    Some(entry.key_span),
                ));
            }
        }
    }
    if ok && (max_attempts.is_none() || backoff_ms.is_none() || retry_on.is_none()) {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_RETRY_SHAPE,
            "`retry` requires all of `max_attempts`, `backoff_ms` and `retry_on` (03 §1.2)",
            Some(node.span),
        ));
        ok = false;
    }
    if !ok {
        return None;
    }
    Some(RetryPolicy {
        max_attempts: max_attempts.expect("checked above"),
        backoff_ms: backoff_ms.expect("checked above"),
        retry_on: retry_on.expect("checked above"),
    })
}

/// Parses `backoff_ms`: a fixed number of milliseconds or an exponential
/// `{ initial, factor, max }` schedule (02 §10).
fn normalize_backoff(node: &Node, diags: &mut Vec<CompileDiagnostic>) -> Option<BackoffMs> {
    let non_negative = |n: &serde_json::Number| n.as_f64().is_some_and(|v| v >= 0.0);
    match &node.value {
        NodeValue::Number(n) if non_negative(n) => Some(BackoffMs::Fixed(n.clone())),
        NodeValue::Mapping(entries) => {
            let mut initial = None;
            let mut factor = None;
            let mut max = None;
            let mut ok = true;
            for entry in entries {
                let (slot, minimum): (&mut Option<serde_json::Number>, f64) =
                    match entry.key.as_str() {
                        "initial" => (&mut initial, 0.0),
                        "factor" => (&mut factor, 1.0),
                        "max" => (&mut max, 0.0),
                        key => {
                            ok = false;
                            diags.push(CompileDiagnostic::error(
                                codes::NORM_BAD_RETRY_SHAPE,
                                format!(
                                    "unknown `backoff_ms` field `{key}` (closed: initial, \
                                     factor, max)"
                                ),
                                Some(entry.key_span),
                            ));
                            continue;
                        }
                    };
                match &entry.value.value {
                    NodeValue::Number(n) if n.as_f64().is_some_and(|v| v >= minimum) => {
                        *slot = Some(n.clone());
                    }
                    other => {
                        ok = false;
                        diags.push(CompileDiagnostic::error(
                            codes::NORM_BAD_RETRY_SHAPE,
                            format!(
                                "`backoff_ms.{}` must be a number ≥ {minimum}, got {}",
                                entry.key,
                                type_name_of(other)
                            ),
                            Some(entry.value.span),
                        ));
                    }
                }
            }
            if ok && (initial.is_none() || factor.is_none() || max.is_none()) {
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_RETRY_SHAPE,
                    "a `backoff_ms` schedule requires all of initial, factor, max",
                    Some(node.span),
                ));
                ok = false;
            }
            if !ok {
                return None;
            }
            Some(BackoffMs::Schedule(BackoffSchedule {
                initial: initial.expect("checked above"),
                factor: factor.expect("checked above"),
                max: max.expect("checked above"),
            }))
        }
        other => {
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_RETRY_SHAPE,
                format!(
                    "`backoff_ms` must be a non-negative number or an `{{initial, factor, \
                     max}}` schedule, got {}",
                    type_name_of(other)
                ),
                Some(node.span),
            ));
            None
        }
    }
}

/// Parses a non-empty sequence of [`ErrorClass`] wire literals
/// (`retry_on` / `error_classes`), attributing errors to `code`.
fn parse_error_classes(
    node: &Node,
    code: &'static str,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<BTreeSet<ErrorClass>> {
    let NodeValue::Sequence(items) = &node.value else {
        diags.push(CompileDiagnostic::error(
            code,
            format!(
                "an error-class list must be a non-empty sequence, got {}",
                node.type_name()
            ),
            Some(node.span),
        ));
        return None;
    };
    if items.is_empty() {
        diags.push(CompileDiagnostic::error(
            code,
            "an error-class list must not be empty",
            Some(node.span),
        ));
        return None;
    }
    let mut classes = BTreeSet::new();
    let mut ok = true;
    for item in items {
        let parsed = match &item.value {
            NodeValue::String(s) => {
                serde_json::from_value::<ErrorClass>(serde_json::Value::String(s.clone())).ok()
            }
            _ => None,
        };
        match parsed {
            Some(class) => {
                classes.insert(class);
            }
            None => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    code,
                    format!(
                        "unknown error class {} (closed ErrorClass vocabulary, spine A.4)",
                        match &item.value {
                            NodeValue::String(s) => format!("'{s}'"),
                            other => type_name_of(other).to_owned(),
                        }
                    ),
                    Some(item.span),
                ));
            }
        }
    }
    ok.then_some(classes)
}

fn bad_step_value(
    key: &str,
    expected: &str,
    got: &NodeValue,
    span: DiagnosticSpan,
) -> CompileDiagnostic {
    CompileDiagnostic::error(
        codes::NORM_BAD_STEP_VALUE,
        format!("`{key}` must be {expected}, got {}", type_name_of(got)),
        Some(span),
    )
}

type NormalizedInvoke = (ActionName, DiagnosticSpan, Vec<NormalizedArg>);

/// One invoke argument.
#[derive(Debug)]
pub(crate) struct NormalizedArg {
    /// Argument name (native action argument).
    pub name: Identifier,
    /// Span of the argument key.
    pub name_span: DiagnosticSpan,
    /// The argument value.
    pub value: NormalizedValue,
}

fn normalize_invoke(node: &Node, diags: &mut Vec<CompileDiagnostic>) -> Option<NormalizedInvoke> {
    let entries = expect_mapping(node, "`invoke`", codes::NORM_BAD_INVOKE_SHAPE, diags)?;
    let mut action = None;
    let mut args = Vec::new();
    let mut ok = true;
    for entry in entries {
        match entry.key.as_str() {
            "action" => match &entry.value.value {
                NodeValue::String(raw) => match ActionName::new(raw.clone()) {
                    Ok(name) => action = Some((name, entry.value.span)),
                    Err(err) => {
                        ok = false;
                        diags.push(CompileDiagnostic::error(
                            codes::NORM_BAD_INVOKE_SHAPE,
                            format!("invalid action name '{raw}': {}", err.reason),
                            Some(entry.value.span),
                        ));
                    }
                },
                other => {
                    ok = false;
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_INVOKE_SHAPE,
                        format!(
                            "`invoke.action` must be a string, got {}",
                            type_name_of(other)
                        ),
                        Some(entry.value.span),
                    ));
                }
            },
            "args" => {
                let Some(arg_entries) = expect_mapping(
                    &entry.value,
                    "`invoke.args`",
                    codes::NORM_BAD_INVOKE_SHAPE,
                    diags,
                ) else {
                    ok = false;
                    continue;
                };
                for arg in arg_entries {
                    let Ok(name) = Identifier::new(arg.key.clone()) else {
                        ok = false;
                        diags.push(CompileDiagnostic::error(
                            codes::NORM_BAD_INVOKE_SHAPE,
                            format!(
                                "argument name '{}' must match [A-Za-z_][A-Za-z0-9_]*",
                                arg.key
                            ),
                            Some(arg.key_span),
                        ));
                        continue;
                    };
                    match normalize_value(&arg.value, diags) {
                        Some(value) => args.push(NormalizedArg {
                            name,
                            name_span: arg.key_span,
                            value,
                        }),
                        None => ok = false,
                    }
                }
            }
            "inputs" => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_INVOKE_SHAPE,
                    "`invoke.inputs` is not in the M2 subset: `invoke` takes `args` \
                     (the 03 §5-4 `inputs` incorporation is pending)",
                    Some(entry.key_span),
                ));
            }
            key => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_INVOKE_SHAPE,
                    format!("unknown `invoke` subkey `{key}` (closed: action, args)"),
                    Some(entry.key_span),
                ));
            }
        }
    }
    if action.is_none() && ok {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_INVOKE_SHAPE,
            "`invoke` is missing the required `action` subkey",
            Some(node.span),
        ));
        ok = false;
    }
    if !ok {
        return None;
    }
    let (action, action_span) = action.expect("checked above");
    Some((action, action_span, args))
}

/// Normalizes one element-verb head value (03 §1.3): `element` wraps the
/// selector; `set_value` adds `value` (Expr allowed); `wait_for` adds
/// `state`.
fn normalize_verb(
    verb: CanonicalVerb,
    verb_key: &str,
    head_span: DiagnosticSpan,
    node: &Node,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<NormalizedVerb> {
    let entries = expect_mapping(
        node,
        &format!("`{verb_key}`"),
        codes::NORM_BAD_VERB_SHAPE,
        diags,
    )?;
    let takes_value = verb == CanonicalVerb::SetValue;
    let takes_state = verb == CanonicalVerb::WaitFor;
    let closed_keys = match (takes_value, takes_state) {
        (true, _) => "element, value",
        (_, true) => "element, state",
        _ => "element",
    };

    let mut element = None;
    let mut value = None;
    let mut state = None;
    let mut ok = true;
    for entry in entries {
        match entry.key.as_str() {
            "element" => {
                element = normalize_selector(&entry.value, SelectorPosition::VerbElement, diags)
                    .map(|selector| (selector, entry.value.span));
                ok &= element.is_some();
            }
            "value" if takes_value => {
                value = normalize_value(&entry.value, diags);
                ok &= value.is_some();
            }
            "state" if takes_state => {
                state = normalize_element_state(&entry.value, codes::NORM_BAD_VERB_SHAPE, diags);
                ok &= state.is_some();
            }
            key => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_VERB_SHAPE,
                    format!("unknown `{verb_key}` subkey `{key}` (closed: {closed_keys})"),
                    Some(entry.key_span),
                ));
            }
        }
    }
    if element.is_none() && ok {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_VERB_SHAPE,
            format!("`{verb_key}` is missing the required `element` subkey"),
            Some(node.span),
        ));
        ok = false;
    }
    if takes_value && value.is_none() && ok {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_VERB_SHAPE,
            "`set_value` is missing the required `value` subkey",
            Some(node.span),
        ));
        ok = false;
    }
    if takes_state && state.is_none() && ok {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_VERB_SHAPE,
            "`wait_for` is missing the required `state` subkey (present|visible|enabled|absent)",
            Some(node.span),
        ));
        ok = false;
    }
    if !ok {
        return None;
    }
    let (element, element_span) = element.expect("checked above");
    Some(NormalizedVerb {
        verb,
        head_span,
        element,
        element_span,
        value,
        state,
    })
}

/// Parses a `WaitForElementCondition` value (shared by `wait_for.state` and
/// the `elementState` predicate).
fn normalize_element_state(
    node: &Node,
    code: &'static str,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<ElementState> {
    if let NodeValue::String(raw) = &node.value {
        if matches!(classify_expr_text(raw), ExprClass::Plain) {
            if let Some((_, state)) = ELEMENT_STATES.iter().find(|(name, _)| name == raw) {
                return Some(*state);
            }
        } else {
            diags.push(SelectorPosition::Predicate.expr_diagnostic(node.span));
            return None;
        }
    }
    diags.push(CompileDiagnostic::error(
        code,
        format!(
            "`state` must be one of present, visible, enabled, absent, got {}",
            match &node.value {
                NodeValue::String(s) => format!("'{s}'"),
                other => type_name_of(other).to_owned(),
            }
        ),
        Some(node.span),
    ));
    None
}

/// Extracts a static string field of a selector/predicate, rejecting
/// `${{ }}` per the position's staticness rule.
fn static_string(
    node: &Node,
    what: &str,
    min_len_1: bool,
    position: SelectorPosition,
    shape_code: &'static str,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<String> {
    match &node.value {
        NodeValue::String(s) => match classify_expr_text(s) {
            ExprClass::Plain => {
                if min_len_1 && s.is_empty() {
                    diags.push(CompileDiagnostic::error(
                        shape_code,
                        format!("{what} must be a non-empty string"),
                        Some(node.span),
                    ));
                    return None;
                }
                Some(s.clone())
            }
            _ => {
                diags.push(position.expr_diagnostic(node.span));
                None
            }
        },
        other => {
            diags.push(CompileDiagnostic::error(
                shape_code,
                format!("{what} must be a string, got {}", type_name_of(other)),
                Some(node.span),
            ));
            None
        }
    }
}

/// Normalizes an element selector map: field-isomorphic passthrough to
/// [`ElementSelectorIR`] (03 §1.3 — field names verbatim, camelCase over
/// the YAML snake_case rule), fully static, at least one field.
fn normalize_selector(
    node: &Node,
    position: SelectorPosition,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<ElementSelectorIR> {
    if let NodeValue::String(s) = &node.value
        && !matches!(classify_expr_text(s), ExprClass::Plain)
    {
        diags.push(position.expr_diagnostic(node.span));
        return None;
    }
    let entries = expect_mapping(
        node,
        "an element selector",
        codes::NORM_BAD_SELECTOR_SHAPE,
        diags,
    )?;
    let mut selector = ElementSelectorIR::default();
    let mut ok = true;
    for entry in entries {
        match entry.key.as_str() {
            "context" => {
                selector.context = normalize_selector_context(&entry.value, position, diags);
                ok &= selector.context.is_some();
            }
            "role" => {
                selector.role = static_string(
                    &entry.value,
                    "selector `role`",
                    true,
                    position,
                    codes::NORM_BAD_SELECTOR_SHAPE,
                    diags,
                );
                ok &= selector.role.is_some();
            }
            "name" => {
                selector.name = static_string(
                    &entry.value,
                    "selector `name`",
                    true,
                    position,
                    codes::NORM_BAD_SELECTOR_SHAPE,
                    diags,
                );
                ok &= selector.name.is_some();
            }
            "identifier" => {
                selector.identifier = static_string(
                    &entry.value,
                    "selector `identifier`",
                    true,
                    position,
                    codes::NORM_BAD_SELECTOR_SHAPE,
                    diags,
                );
                ok &= selector.identifier.is_some();
            }
            "text" => {
                selector.text = normalize_text_match(&entry.value, position, diags);
                ok &= selector.text.is_some();
            }
            "value" => {
                selector.value = static_string(
                    &entry.value,
                    "selector `value`",
                    false,
                    position,
                    codes::NORM_BAD_SELECTOR_SHAPE,
                    diags,
                );
                ok &= selector.value.is_some();
            }
            "css" => {
                selector.css = static_string(
                    &entry.value,
                    "selector `css`",
                    true,
                    position,
                    codes::NORM_BAD_SELECTOR_SHAPE,
                    diags,
                );
                ok &= selector.css.is_some();
            }
            key => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_SELECTOR_SHAPE,
                    format!(
                        "unknown selector field `{key}` (closed, ElementSelector-isomorphic: \
                         context, role, name, identifier, text, value, css)"
                    ),
                    Some(entry.key_span),
                ));
            }
        }
    }
    if ok && entries.is_empty() {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_SELECTOR_SHAPE,
            "an element selector must carry at least one field (minProperties: 1)",
            Some(node.span),
        ));
        ok = false;
    }
    ok.then_some(selector)
}

/// Normalizes the selector `context` submap (`{ contextKind, contextId? }`,
/// DeviceRail passthrough).
fn normalize_selector_context(
    node: &Node,
    position: SelectorPosition,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<SelectorContext> {
    let entries = expect_mapping(
        node,
        "selector `context`",
        codes::NORM_BAD_SELECTOR_SHAPE,
        diags,
    )?;
    let mut context_kind = None;
    let mut context_id = None;
    let mut ok = true;
    for entry in entries {
        match entry.key.as_str() {
            "contextKind" => match &entry.value.value {
                NodeValue::String(s) if s == "native" => context_kind = Some(UiContextKind::Native),
                NodeValue::String(s) if s == "web" => context_kind = Some(UiContextKind::Web),
                other => {
                    ok = false;
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_SELECTOR_SHAPE,
                        format!(
                            "`contextKind` must be `native` or `web`, got {}",
                            match other {
                                NodeValue::String(s) => format!("'{s}'"),
                                other => type_name_of(other).to_owned(),
                            }
                        ),
                        Some(entry.value.span),
                    ));
                }
            },
            "contextId" => {
                context_id = static_string(
                    &entry.value,
                    "`contextId`",
                    true,
                    position,
                    codes::NORM_BAD_SELECTOR_SHAPE,
                    diags,
                );
                ok &= context_id.is_some();
            }
            key => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_SELECTOR_SHAPE,
                    format!("unknown `context` field `{key}` (closed: contextKind, contextId)"),
                    Some(entry.key_span),
                ));
            }
        }
    }
    if context_kind.is_none() && ok {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_SELECTOR_SHAPE,
            "selector `context` is missing the required `contextKind`",
            Some(node.span),
        ));
        ok = false;
    }
    if !ok {
        return None;
    }
    Some(SelectorContext {
        context_kind: context_kind.expect("checked above"),
        context_id,
    })
}

/// Normalizes a `text` matcher, materializing the defaults `mode: exact`
/// and `caseSensitive: false` (single-representation rule, 02 §2.4).
fn normalize_text_match(
    node: &Node,
    position: SelectorPosition,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<TextMatchIR> {
    let entries = expect_mapping(
        node,
        "a `text` matcher",
        codes::NORM_BAD_SELECTOR_SHAPE,
        diags,
    )?;
    let mut value = None;
    let mut mode = TextMatchMode::Exact;
    let mut case_sensitive = false;
    let mut ok = true;
    for entry in entries {
        match entry.key.as_str() {
            "value" => {
                value = static_string(
                    &entry.value,
                    "text `value`",
                    true,
                    position,
                    codes::NORM_BAD_SELECTOR_SHAPE,
                    diags,
                );
                ok &= value.is_some();
            }
            "mode" => match &entry.value.value {
                NodeValue::String(s) if s == "exact" => mode = TextMatchMode::Exact,
                NodeValue::String(s) if s == "contains" => mode = TextMatchMode::Contains,
                other => {
                    ok = false;
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_SELECTOR_SHAPE,
                        format!(
                            "text `mode` must be `exact` or `contains`, got {}",
                            match other {
                                NodeValue::String(s) => format!("'{s}'"),
                                other => type_name_of(other).to_owned(),
                            }
                        ),
                        Some(entry.value.span),
                    ));
                }
            },
            "caseSensitive" => match &entry.value.value {
                NodeValue::Bool(b) => case_sensitive = *b,
                other => {
                    ok = false;
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_SELECTOR_SHAPE,
                        format!(
                            "text `caseSensitive` must be a boolean, got {}",
                            type_name_of(other)
                        ),
                        Some(entry.value.span),
                    ));
                }
            },
            key => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_SELECTOR_SHAPE,
                    format!("unknown `text` field `{key}` (closed: value, mode, caseSensitive)"),
                    Some(entry.key_span),
                ));
            }
        }
    }
    if value.is_none() && ok {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_SELECTOR_SHAPE,
            "a `text` matcher is missing the required `value`",
            Some(node.span),
        ));
        ok = false;
    }
    if !ok {
        return None;
    }
    Some(TextMatchIR {
        value: value.expect("checked above"),
        mode,
        case_sensitive,
    })
}

/// Parses a channel chain value (`locate_via` / assertion `verify_via`):
/// a non-empty sequence over the closed channel-token vocabulary. Order,
/// duplicates and chain-position legality are bind-phase (E1/E2).
fn normalize_chain(
    node: &Node,
    key_name: &str,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<NormalizedChain> {
    let NodeValue::Sequence(items) = &node.value else {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_FALLBACK_SHAPE,
            format!(
                "`{key_name}` must be a sequence of channel names, got {}",
                node.type_name()
            ),
            Some(node.span),
        ));
        return None;
    };
    if items.is_empty() {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_FALLBACK_SHAPE,
            format!("`{key_name}` must not be empty (omit the key for the platform default)"),
            Some(node.span),
        ));
        return None;
    }
    let mut channels = Vec::new();
    let mut ok = true;
    for item in items {
        match &item.value {
            NodeValue::String(raw) => match CHANNEL_TOKENS.iter().find(|(name, _)| name == raw) {
                Some((_, channel)) => channels.push(*channel),
                None => {
                    ok = false;
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_FALLBACK_SHAPE,
                        format!(
                            "unknown channel '{raw}' in `{key_name}` (closed vocabulary: \
                                 dom, uiTree, vision, coordinate)"
                        ),
                        Some(item.span),
                    ));
                }
            },
            other => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_FALLBACK_SHAPE,
                    format!(
                        "`{key_name}` entries must be channel-name strings, got {}",
                        type_name_of(other)
                    ),
                    Some(item.span),
                ));
            }
        }
    }
    ok.then_some(NormalizedChain {
        channels,
        span: node.span,
    })
}

/// Parses the static `coordinate: { x, y }` key (03 §1.4 rule 3).
fn normalize_coordinate(
    node: &Node,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<NormalizedCoordinate> {
    let entries = expect_mapping(node, "`coordinate`", codes::NORM_BAD_FALLBACK_SHAPE, diags)?;
    let mut x = None;
    let mut y = None;
    let mut ok = true;
    for entry in entries {
        let slot = match entry.key.as_str() {
            "x" => &mut x,
            "y" => &mut y,
            key => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_FALLBACK_SHAPE,
                    format!("unknown `coordinate` field `{key}` (closed: x, y)"),
                    Some(entry.key_span),
                ));
                continue;
            }
        };
        match &entry.value.value {
            NodeValue::Number(n) => *slot = Some(n.clone()),
            other => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_FALLBACK_SHAPE,
                    format!(
                        "`coordinate.{}` must be a static number, got {}",
                        entry.key,
                        type_name_of(other)
                    ),
                    Some(entry.value.span),
                ));
            }
        }
    }
    if ok && (x.is_none() || y.is_none()) {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_FALLBACK_SHAPE,
            "`coordinate` must carry both static `x` and `y` (03 §1.4 rule 3)",
            Some(node.span),
        ));
        ok = false;
    }
    if !ok {
        return None;
    }
    Some(NormalizedCoordinate {
        x: x.expect("checked above"),
        y: y.expect("checked above"),
        span: node.span,
    })
}

/// Parses the region of a `visual` predicate into a [`RectIR`].
fn normalize_region(node: &Node, diags: &mut Vec<CompileDiagnostic>) -> Option<RectIR> {
    let entries = expect_mapping(node, "`region`", codes::NORM_BAD_EXPECT_SHAPE, diags)?;
    let mut fields: [Option<serde_json::Number>; 4] = [None, None, None, None];
    const NAMES: [&str; 4] = ["x", "y", "width", "height"];
    let mut ok = true;
    for entry in entries {
        let Some(index) = NAMES.iter().position(|name| *name == entry.key) else {
            ok = false;
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_EXPECT_SHAPE,
                format!(
                    "unknown `region` field `{}` (closed: x, y, width, height)",
                    entry.key
                ),
                Some(entry.key_span),
            ));
            continue;
        };
        match &entry.value.value {
            NodeValue::Number(n) => {
                if index >= 2 && n.as_f64().is_some_and(|v| v < 0.0) {
                    ok = false;
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_EXPECT_SHAPE,
                        format!("`region.{}` must be ≥ 0", entry.key),
                        Some(entry.value.span),
                    ));
                } else {
                    fields[index] = Some(n.clone());
                }
            }
            NodeValue::String(s) if !matches!(classify_expr_text(s), ExprClass::Plain) => {
                ok = false;
                diags.push(SelectorPosition::Predicate.expr_diagnostic(entry.value.span));
            }
            other => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_EXPECT_SHAPE,
                    format!(
                        "`region.{}` must be a static number, got {}",
                        entry.key,
                        type_name_of(other)
                    ),
                    Some(entry.value.span),
                ));
            }
        }
    }
    if ok && fields.iter().any(Option::is_none) {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_EXPECT_SHAPE,
            "`region` must carry all of x, y, width, height",
            Some(node.span),
        ));
        ok = false;
    }
    if !ok {
        return None;
    }
    let [x, y, width, height] = fields;
    Some(RectIR {
        x: x.expect("checked above"),
        y: y.expect("checked above"),
        width: width.expect("checked above"),
        height: height.expect("checked above"),
    })
}

/// Normalizes the `expect` list into predicate forms (03 §1.5): `expr`,
/// `element`+`state`, `element`+`text`, `visual`(+`region`). Every other
/// key combination is diagnosed — never skipped.
fn normalize_expect(node: &Node, diags: &mut Vec<CompileDiagnostic>) -> Vec<NormalizedAssertion> {
    let NodeValue::Sequence(items) = &node.value else {
        diags.push(CompileDiagnostic::error(
            codes::NORM_BAD_EXPECT_SHAPE,
            format!(
                "`expect` must be a sequence of assertions, got {}",
                node.type_name()
            ),
            Some(node.span),
        ));
        return Vec::new();
    };
    let mut assertions = Vec::new();
    for item in items {
        if let Some(assertion) = normalize_expect_item(item, diags) {
            assertions.push(assertion);
        }
    }
    assertions
}

fn normalize_expect_item(
    item: &Node,
    diags: &mut Vec<CompileDiagnostic>,
) -> Option<NormalizedAssertion> {
    let entries = expect_mapping(
        item,
        "an `expect` item",
        codes::NORM_BAD_EXPECT_SHAPE,
        diags,
    )?;

    let mut assert_id = None;
    let mut expr = None;
    let mut element = None;
    let mut state = None;
    let mut text = None;
    let mut sugar_value: Option<(String, DiagnosticSpan)> = None;
    let mut visual = None;
    let mut region = None;
    let mut verify_via = None;
    let mut ok = true;

    for entry in entries {
        match entry.key.as_str() {
            "expr" => {
                let inner = match &entry.value.value {
                    NodeValue::String(raw) => match classify_expr_text(raw) {
                        ExprClass::Full(inner) => Some(inner.to_owned()),
                        _ => None,
                    },
                    _ => None,
                };
                match inner {
                    Some(inner) => expr = Some((inner, entry.value.span)),
                    None => {
                        ok = false;
                        diags.push(CompileDiagnostic::error(
                            codes::NORM_BAD_EXPECT_SHAPE,
                            "`expr` must be a single ${{ ... }} expression scalar",
                            Some(entry.value.span),
                        ));
                    }
                }
            }
            "assert_id" => match &entry.value.value {
                NodeValue::String(raw) => {
                    assert_id = Some((raw.clone(), entry.value.span));
                }
                other => {
                    ok = false;
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_EXPECT_SHAPE,
                        format!("`assert_id` must be a string, got {}", type_name_of(other)),
                        Some(entry.value.span),
                    ));
                }
            },
            "element" => {
                element = normalize_selector(&entry.value, SelectorPosition::Predicate, diags)
                    .map(|selector| (selector, entry.value.span));
                ok &= element.is_some();
            }
            "state" => {
                state = normalize_element_state(&entry.value, codes::NORM_BAD_EXPECT_SHAPE, diags)
                    .map(|state| (state, entry.value.span));
                ok &= state.is_some();
            }
            "text" => {
                text = normalize_text_match(&entry.value, SelectorPosition::Predicate, diags)
                    .map(|matcher| (matcher, entry.value.span));
                ok &= text.is_some();
            }
            "visual" => {
                visual = static_string(
                    &entry.value,
                    "`visual`",
                    true,
                    SelectorPosition::Predicate,
                    codes::NORM_BAD_EXPECT_SHAPE,
                    diags,
                )
                .map(|prompt| (prompt, entry.value.span));
                ok &= visual.is_some();
            }
            "region" => {
                region = normalize_region(&entry.value, diags).map(|rect| (rect, entry.value.span));
                ok &= region.is_some();
            }
            "verify_via" => match normalize_chain(&entry.value, "verify_via", diags) {
                Some(chain) => verify_via = Some(chain),
                None => ok = false,
            },
            "value" => {
                // 03 §1.5 sugar: `value: <s>` = `text: { value: <s>,
                // mode: exact }` — static non-empty literal only (the
                // predicate staticness rule; a post-expansion expression
                // is rejected exactly like text `value`). Merged after
                // the loop so a co-present `text` conflicts explicitly.
                sugar_value = static_string(
                    &entry.value,
                    "`value`",
                    true,
                    SelectorPosition::Predicate,
                    codes::NORM_BAD_EXPECT_SHAPE,
                    diags,
                )
                .map(|raw| (raw, entry.value.span));
                ok &= sugar_value.is_some();
            }
            key => {
                ok = false;
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_EXPECT_SHAPE,
                    format!(
                        "unknown assertion key `{key}` (closed: {})",
                        ASSERTION_KEYS.join(", ")
                    ),
                    Some(entry.key_span),
                ));
            }
        }
    }
    if let Some((raw, span)) = sugar_value {
        if let Some((_, text_span)) = &text {
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_EXPECT_SHAPE,
                "`value` is the sugar for `text` (03 §1.5) — give one or the other, not both",
                Some(*text_span),
            ));
            return None;
        }
        text = Some((
            TextMatchIR {
                value: raw,
                mode: TextMatchMode::Exact,
                case_sensitive: false,
            },
            span,
        ));
    }
    if !ok {
        return None;
    }

    // Form dispatch (03 §1.5): exactly one predicate surface per item. The
    // `visual` key is legal both as the visual-predicate head and as the
    // vision-tail prompt of an element predicate (03 §1.4 rule 5).
    let (predicate, visual_prompt) = match (expr, element, visual) {
        (Some((inner, span)), None, None) => {
            let mut form_ok = true;
            if let Some(chain) = &verify_via {
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_EXPECT_SHAPE,
                    "an `expr` predicate consumes no observation channel; `verify_via` is not \
                     allowed on it (02 §5.3)",
                    Some(chain.span),
                ));
                form_ok = false;
            }
            for (key, span) in [
                ("state", state.as_ref().map(|(_, span)| *span)),
                ("text", text.as_ref().map(|(_, span)| *span)),
                ("region", region.as_ref().map(|(_, span)| *span)),
            ] {
                if let Some(span) = span {
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_EXPECT_SHAPE,
                        format!("`{key}` is not allowed on an `expr` assertion"),
                        Some(span),
                    ));
                    form_ok = false;
                }
            }
            if !form_ok {
                return None;
            }
            (NormalizedPredicate::Expr { inner, span }, None)
        }
        (None, Some((selector, element_span)), visual) => {
            if let Some((_, span)) = &region {
                diags.push(CompileDiagnostic::error(
                    codes::NORM_BAD_EXPECT_SHAPE,
                    "`region` is only allowed on a `visual` predicate",
                    Some(*span),
                ));
                return None;
            }
            let predicate = match (state, text) {
                (Some((state, _)), None) => NormalizedPredicate::ElementState { selector, state },
                (None, Some((matcher, _))) => {
                    NormalizedPredicate::ElementText { selector, matcher }
                }
                (Some(_), Some((_, span))) => {
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_EXPECT_SHAPE,
                        "an `element` assertion carries exactly one of `state` or `text`",
                        Some(span),
                    ));
                    return None;
                }
                (None, None) => {
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_EXPECT_SHAPE,
                        "an `element` assertion needs `state` (elementState) or `text` \
                         (elementText)",
                        Some(element_span),
                    ));
                    return None;
                }
            };
            (predicate, visual)
        }
        (None, None, Some((prompt, _))) => {
            let mut form_ok = true;
            for (key, span) in [
                ("state", state.as_ref().map(|(_, span)| *span)),
                ("text", text.as_ref().map(|(_, span)| *span)),
            ] {
                if let Some(span) = span {
                    diags.push(CompileDiagnostic::error(
                        codes::NORM_BAD_EXPECT_SHAPE,
                        format!("`{key}` requires an `element` selector"),
                        Some(span),
                    ));
                    form_ok = false;
                }
            }
            if !form_ok {
                return None;
            }
            (
                NormalizedPredicate::Visual {
                    prompt,
                    region: region.map(|(rect, _)| rect),
                },
                None,
            )
        }
        (None, None, None) => {
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_EXPECT_SHAPE,
                "an `expect` item must carry a predicate: `expr`, `element` + `state`|`text`, \
                 or `visual`",
                Some(item.span),
            ));
            return None;
        }
        _ => {
            diags.push(CompileDiagnostic::error(
                codes::NORM_BAD_EXPECT_SHAPE,
                "an `expect` item carries exactly one predicate surface (`expr`, `element`, or \
                 `visual`)",
                Some(item.span),
            ));
            return None;
        }
    };

    Some(NormalizedAssertion {
        assert_id,
        predicate,
        verify_via,
        visual: visual_prompt,
        span: item.span,
    })
}
