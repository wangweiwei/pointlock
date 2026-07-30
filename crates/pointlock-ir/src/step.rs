//! Steps: the closed 7-kind vocabulary (02 §4) plus the act-chain binding
//! types (02 §5).
//!
//! ## Wire shape vs. Rust shape
//!
//! On the wire, `StepIR` is a `kind`-discriminated union whose variants
//! compose `StepBase` (baseline schema: `allOf` + `unevaluatedProperties:
//! false`). In Rust, each variant struct carries an explicit const `kind`
//! marker field and `#[serde(flatten)]`s [`StepBase`]; [`StepIR`] itself is
//! `#[serde(untagged)]`. This produces the exact same wire bytes as serde's
//! internal tagging *and* lets a variant struct (e.g. [`HumanStepIR`] inside
//! `HandlerAction::escalate`) serialize standalone with its `kind` field, as
//! the baseline requires.
//!
//! ## Closedness caveat (documented divergence)
//!
//! serde's `deny_unknown_fields` cannot be combined with `flatten`, so the
//! step variant structs are *serde-lenient* about unknown fields at runtime.
//! The schema stays authoritative: `#[schemars(deny_unknown_fields)]`
//! (schema-only, no serde effect) closes each variant in the generated
//! schema, mirroring the baseline's `unevaluatedProperties: false`. Golden
//! fixture behavioral equivalence is judged schema-vs-schema (02 §1.1), so
//! runtime leniency of the DTO loader is an implementation note, not a
//! contract change.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::assertion::AssertionIR;
use crate::expr::{Expr, ExprMap};
use crate::flow::FlowRef;
use crate::handler::{HandlerBinding, RetryPolicy};
use crate::primitives::{
    ActionName, FeatureId, Hash, JsonSchemaDocument, OnTimeout, Protection, StepId, literal_marker,
};
use crate::vocab::{
    ActChannel, CanonicalVerb, EffectClassAction, ExecutionMode, HumanMode, ObservationWhich,
};

/// Common step envelope (02 §3).
///
/// Deliberately NOT closed (baseline exemption class 3): each `StepIR`
/// variant composes it and closes itself. `stepId` is identity, the two
/// hashes are content — the pivot of the resume-alignment mechanism;
/// `stepId` deliberately participates in neither hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StepBase {
    /// Author-provided, flow-unique, stable step identity.
    pub step_id: StepId,
    /// Canonical hash of "what this step does to the world" (02 §12.3).
    pub effect_hash: Hash,
    /// Canonical hash of "how this step is judged" (02 §12.3).
    pub judge_hash: Hash,
    /// Pre-entry world probes; double as resume drift detection
    /// (spine §6.7-C). Distinct from post-hoc `assertions` (`expect`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub preflight: Option<Vec<AssertionIR>>,
    /// Retry policy — applies to the act phase only (spine §6.5 mount 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryPolicy>,
    /// Step budget in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub timeout_ms: Option<u64>,
    /// Step-level handler hooks; override flow-level ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub handlers: Option<Vec<HandlerBinding>>,
    /// Whether to materialize a checkpoint at this step boundary. Required
    /// because sealed IR materializes all defaulted fields
    /// (single-representation rule; default true, false inside macro
    /// expansions).
    pub checkpoint: bool,
}

// ─── kind markers (const tags) ──────────────────────────────────────────────

literal_marker! {
    /// `kind: "action"`.
    ActionKind => "action"
}
literal_marker! {
    /// `kind: "assert"`.
    AssertKind => "assert"
}
literal_marker! {
    /// `kind: "call"`.
    CallKind => "call"
}
literal_marker! {
    /// `kind: "human"`.
    HumanKind => "human"
}
literal_marker! {
    /// `kind: "if"`.
    IfKind => "if"
}
literal_marker! {
    /// `kind: "foreach"`.
    ForeachKind => "foreach"
}
literal_marker! {
    /// `kind: "let"`.
    LetKind => "let"
}

/// The closed step union (7 kinds, spine A.4), discriminated by `kind` on
/// the wire. See the module docs for why this is `untagged` in serde while
/// remaining a `kind`-tagged union on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum StepIR {
    /// `kind: "action"` — fixed pipeline `preflight? → act → observe → assert`.
    Action(ActionStepIR),
    /// `kind: "assert"` — side-effect-free observation and judgment.
    Assert(AssertStepIR),
    /// `kind: "call"` — subflow invocation, pinned by content hash.
    Call(CallStepIR),
    /// `kind: "human"` — human collaboration node (principle 8).
    Human(HumanStepIR),
    /// `kind: "if"` — conditional container.
    If(IfStepIR),
    /// `kind: "foreach"` — iteration container.
    Foreach(ForeachStepIR),
    /// `kind: "let"` — pure bindings into `vars.*` (SSA).
    Let(LetStepIR),
}

impl StepIR {
    /// The step's identity, independent of kind.
    pub fn step_id(&self) -> &StepId {
        &self.base().step_id
    }

    /// The shared step envelope, independent of kind.
    pub fn base(&self) -> &StepBase {
        match self {
            StepIR::Action(s) => &s.base,
            StepIR::Assert(s) => &s.base,
            StepIR::Call(s) => &s.base,
            StepIR::Human(s) => &s.base,
            StepIR::If(s) => &s.base,
            StepIR::Foreach(s) => &s.base,
            StepIR::Let(s) => &s.base,
        }
    }

    /// The wire value of the `kind` discriminator.
    pub fn kind(&self) -> &'static str {
        match self {
            StepIR::Action(_) => "action",
            StepIR::Assert(_) => "assert",
            StepIR::Call(_) => "call",
            StepIR::Human(_) => "human",
            StepIR::If(_) => "if",
            StepIR::Foreach(_) => "foreach",
            StepIR::Let(_) => "let",
        }
    }
}

/// Action step (02 §4.1): fixed pipeline `preflight? → act → observe → assert`.
///
/// `assertions` MAY be empty: a mutating action step without assertions
/// yields no verdict (report annotation `unverified`, spine R4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct ActionStepIR {
    /// Const `"action"`.
    pub kind: ActionKind,
    /// Common step envelope (flattened on the wire).
    #[serde(flatten)]
    pub base: StepBase,
    /// Canonical verb — pure metadata for reports; the runner has no verb
    /// switch (spine R7).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verb: Option<CanonicalVerb>,
    /// `mutating | readonly` (`pure` is excluded — it belongs to `let`).
    pub effect: EffectClassAction,
    /// Author-declared idempotence (materialized default: false). Governs
    /// timed-out auto-retry and reconcile-uncertain replay permission.
    pub idempotent: bool,
    /// The compile-time fully bound act-chain.
    pub binding: ActionBinding,
    /// Post-hoc assertions. Empty array ⇒ this step yields no verdict.
    pub assertions: Vec<AssertionIR>,
    /// Output projection: `Record<name, Expr>` over `ActionResult.output` /
    /// observation metadata. Self-refs inside refer to the *raw* output
    /// (02 §4.1.1). Absent ⇒ identity projection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<ExprMap>,
    /// Data contract of the projected output, for downstream static checks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonSchemaDocument>,
}

/// Ordered, closed act-chain. No declared fallback ⇒ exactly one attempt
/// (principle 6: the runner never improvises a downgrade).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionBinding {
    /// The attempts, in declared order. A subsequent attempt is tried only
    /// after `action_failed_final` of the previous one (spine §6.2).
    #[schemars(length(min = 1))]
    pub attempts: Vec<BoundAttempt>,
}

/// One fully bound attempt of the act-chain (02 §5.1).
///
/// `protection` is const `"standard"` in v0.1: bind rejects protected
/// actions (spine R6). `coordinate` attempts must carry literal static
/// coordinates in `args` (bind-phase check).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoundAttempt {
    /// Locating channel — [`ActChannel`], so `vision` is structurally
    /// impossible here (principle 7).
    pub channel: ActChannel,
    /// Provider-native action name (per lockfile.device.actions).
    pub action_name: ActionName,
    /// Arguments as expressions; shape-checked against the action's
    /// `inputSchema` at bind time and re-checked after evaluation at runtime.
    pub args: ExprMap,
    /// Feature this attempt depends on (e.g. `device.semanticActions.v1`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_feature: Option<FeatureId>,
    /// Whitelist of daemon-internal execution modes (spine §6.4 R-degrade).
    /// Derived per attempt from its own `channel` only; semantic attempts
    /// never include `coordinateFallback`. Set semantics — serialized in
    /// declaration order of [`ExecutionMode`], deduplicated on load.
    #[schemars(length(min = 1))]
    pub accept_execution_modes: std::collections::BTreeSet<ExecutionMode>,
    /// Const `"standard"` in v0.1 (spine R6).
    pub protection: Protection,
}

/// Assert step (02 §4.2): side-effect-free observation and judgment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct AssertStepIR {
    /// Const `"assert"`.
    pub kind: AssertKind,
    /// Common step envelope (flattened on the wire).
    #[serde(flatten)]
    pub base: StepBase,
    /// Observation source: fresh capture or reuse of an action step's
    /// before/after observation (offline re-judgeable).
    pub observe: ObservationSource,
    /// At least one assertion (an assertion-free assert step is meaningless).
    #[schemars(length(min = 1))]
    pub assertions: Vec<AssertionIR>,
}

literal_marker! {
    /// The `"fresh"` literal of [`ObservationSource`].
    FreshMarker => "fresh"
}

/// Where an assert step's observation comes from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ObservationSource {
    /// `"fresh"` — trigger one `ProviderSession.observe` (readonly,
    /// replay-safe).
    Fresh(FreshMarker),
    /// Reuse the referenced action step's archived observation — no device
    /// contact, purely offline re-judgeable.
    FromStep(ObservationFromStep),
}

impl ObservationSource {
    /// The `"fresh"` source.
    pub fn fresh() -> Self {
        ObservationSource::Fresh(FreshMarker::Value)
    }

    /// A `fromStep` source.
    pub fn from_step(from_step: StepId, which: ObservationWhich) -> Self {
        ObservationSource::FromStep(ObservationFromStep { from_step, which })
    }
}

/// The object branch of [`ObservationSource`] (inline in the baseline).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
pub struct ObservationFromStep {
    /// The action step whose observation is reused.
    pub from_step: StepId,
    /// Which observation (`after` | `before`).
    pub which: ObservationWhich,
}

/// Call step (02 §6): subflow invocation. Callee pinned by content hash
/// (`flowRef.irHash` must appear in `FlowIR.subflows`). Call-by-value:
/// `inputs` are evaluated in the caller scope and snapshotted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct CallStepIR {
    /// Const `"call"`.
    pub kind: CallKind,
    /// Common step envelope (flattened on the wire).
    #[serde(flatten)]
    pub base: StepBase,
    /// The callee, pinned by `flowId` + `irHash`.
    pub flow_ref: FlowRef,
    /// Caller-scope input expressions (call-by-value snapshot).
    pub inputs: ExprMap,
}

/// Human step (02 §4.4): human collaboration is a formal node (principle 8).
///
/// `onTimeout` is const `"unknown"`: a human step that times out never
/// defaults to pass or fail (principles 4/8). `timeoutMs` is required
/// (unbounded waits are `runSuspended`, not silent hangs). The
/// `provideInput` ⇒ `outputSchema` requirement is a schema conditional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(
    deny_unknown_fields,
    extend(
        "if" = { "properties": { "mode": { "const": "provideInput" } }, "required": ["mode"] },
        "then" = { "required": ["outputSchema"] }
    )
)]
pub struct HumanStepIR {
    /// Const `"human"`.
    pub kind: HumanKind,
    /// Common step envelope (flattened on the wire).
    #[serde(flatten)]
    pub base: StepBase,
    /// Interaction mode.
    pub mode: HumanMode,
    /// The question posed to the human.
    #[schemars(length(min = 1, max = 16384))]
    pub prompt: String,
    /// Evidence/values presented to the human (expressions).
    pub presents: Vec<Expr>,
    /// Enumerated options for judge/confirm modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1), inner(length(min = 1, max = 256)))]
    pub decisions: Option<Vec<String>>,
    /// Input contract for `provideInput` mode (required there, schema-enforced).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonSchemaDocument>,
    /// Required budget; expiry yields verdict `unknown`.
    #[schemars(range(min = 1))]
    pub timeout_ms: u64,
    /// Const `"unknown"` (principle 4).
    pub on_timeout: OnTimeout,
}

/// If step (02 §4.5): conditional container. Container hashes exclude the
/// subtree — child steps carry their own identity and hashes (02 §12.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct IfStepIR {
    /// Const `"if"`.
    pub kind: IfKind,
    /// Common step envelope (flattened on the wire).
    #[serde(flatten)]
    pub base: StepBase,
    /// The branch condition.
    pub cond: Expr,
    /// Steps executed when the condition holds (≥ 1).
    #[schemars(length(min = 1))]
    pub then: Vec<StepIR>,
    /// Steps executed otherwise (≥ 1 when present). Unselected branch steps
    /// are `skipped`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub r#else: Option<Vec<StepIR>>,
}

/// Foreach step (02 §4.6): iteration container. The iteration variable is
/// referenced via `iter.<as>`; `RunPath` disambiguates rounds with
/// `{ kind: "iteration", index }` frames, so body stepIds do not (and must
/// not) vary per round.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct ForeachStepIR {
    /// Const `"foreach"`.
    pub kind: ForeachKind,
    /// Common step envelope (flattened on the wire).
    #[serde(flatten)]
    pub base: StepBase,
    /// The collection expression.
    pub items: Expr,
    /// Iteration variable name (scoped as `iter.<as>`).
    pub r#as: crate::primitives::Identifier,
    /// Loop body (≥ 1 step).
    #[schemars(length(min = 1))]
    pub body: Vec<StepIR>,
}

/// Let step (02 §4.7): pure bindings into the `vars.*` scope, SSA single
/// assignment (rebinding an existing var name is a check-phase error).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct LetStepIR {
    /// Const `"let"`.
    pub kind: LetKind,
    /// Common step envelope (flattened on the wire).
    #[serde(flatten)]
    pub base: StepBase,
    /// The bindings (≥ 1 entry).
    #[schemars(schema_with = "let_bindings_schema")]
    pub bindings: ExprMap,
}

fn let_bindings_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "allOf": [generator.subschema_for::<ExprMap>()],
        "type": "object",
        "minProperties": 1
    })
}
