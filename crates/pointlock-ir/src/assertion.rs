//! Assertions: the "questions" of the IR (02 §5.3, §9.1).
//!
//! An assertion is a pure predicate over observations / action outputs. It
//! yields `pass | fail | unknown` at runtime — the answer (Verdict) is a
//! runtime artifact and deliberately absent from the IR (principle 3).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::expr::Expr;
use crate::primitives::{AssertId, OnMissingInput};
use crate::selector::{ElementSelectorIR, RectIR, TextMatchIR};
use crate::vocab::{ElementState, VerifyChannel};

/// A single assertion with its explicit verify-chain.
///
/// The three baseline `allOf` conditionals are reproduced on the generated
/// schema via `#[schemars(extend)]`:
/// 1. `expr` predicates consume no observation channel (`verifyVia: []`);
///    all other predicates need at least one channel.
/// 2. `visual` predicates are vision-only (`verifyVia == ["vision"]`).
/// 3. For `elementState`/`elementText`, `visionPrompt` is required iff the
///    chain contains `vision`, forbidden otherwise (03 §1.4 rule 5; the
///    compiler never synthesizes vision prompts, principle 6).
///
/// "vision only at the chain tail" is order-sensitive and remains a
/// bind-phase check (not expressible in JSON Schema).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(extend("allOf" = [
    {
        "if": {
            "properties": {
                "predicate": {
                    "type": "object",
                    "properties": { "type": { "const": "expr" } },
                    "required": ["type"]
                }
            },
            "required": ["predicate"]
        },
        "then": { "properties": { "verifyVia": { "maxItems": 0 } } },
        "else": { "properties": { "verifyVia": { "minItems": 1 } } }
    },
    {
        "if": {
            "properties": {
                "predicate": {
                    "type": "object",
                    "properties": { "type": { "const": "visual" } },
                    "required": ["type"]
                }
            },
            "required": ["predicate"]
        },
        "then": { "properties": { "verifyVia": { "const": ["vision"] } } }
    },
    {
        "if": {
            "properties": {
                "predicate": {
                    "type": "object",
                    "properties": { "type": { "enum": ["elementState", "elementText"] } },
                    "required": ["type"]
                },
                "verifyVia": { "type": "array", "contains": { "const": "vision" } }
            },
            "required": ["predicate", "verifyVia"]
        },
        "then": { "required": ["visionPrompt"] },
        "else": { "not": { "required": ["visionPrompt"] } }
    }
]))]
pub struct AssertionIR {
    /// Stable assertion id (unique within the step).
    pub assert_id: AssertId,
    /// The predicate to evaluate.
    pub predicate: PredicateIR,
    /// Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
    /// vision]`. Order carries semantics (degradation order) and participates
    /// verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
    /// the schema (`uniqueItems`), not by this type.
    #[schemars(schema_with = "unique_verify_via_schema")]
    pub verify_via: Vec<VerifyChannel>,
    /// Author-written vision prompt, handed verbatim to the VisionVerifier
    /// when `vision` is the declared degraded tail of an
    /// `elementState`/`elementText` verify-chain (YAML surface key `visual`).
    /// Required iff such a chain contains `vision`, forbidden otherwise.
    /// Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 16384))]
    pub vision_prompt: Option<String>,
    /// Const `"unknown"` (principle 4): a channel that cannot complete
    /// evaluation yields unknown for that channel and the chain advances; an
    /// exhausted chain yields unknown. A completed negative is final
    /// (spine R5).
    pub on_missing_input: OnMissingInput,
}

fn unique_verify_via_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = <Vec<VerifyChannel>>::json_schema(generator);
    schema
        .ensure_object()
        .insert("uniqueItems".to_owned(), serde_json::Value::Bool(true));
    schema
}

/// The four predicate types (closed, spine A.4), internally tagged `type`.
///
/// Note on closedness: `#[schemars(deny_unknown_fields)]` closes each
/// variant object in the generated schema (baseline parity); serde's
/// internally-tagged deserialization is lenient about unknown fields at
/// runtime (a documented serde limitation) — the schema stays authoritative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub enum PredicateIR {
    /// Element state check; values equal DeviceRail
    /// `WaitForElementCondition` verbatim.
    ElementState {
        /// The element to check.
        selector: ElementSelectorIR,
        /// The expected state.
        state: ElementState,
    },
    /// Element text check.
    ElementText {
        /// The element to check.
        selector: ElementSelectorIR,
        /// The text matcher.
        r#match: TextMatchIR,
    },
    /// Pure expression assertion over outputs (consumes no observation
    /// channel; `verifyVia` is empty).
    Expr {
        /// The boolean expression to evaluate.
        expr: Expr,
    },
    /// Visual assertion (vision-only; the prompt lives here, not in
    /// `visionPrompt`).
    Visual {
        /// Author-written vision prompt.
        #[schemars(length(min = 1, max = 16384))]
        prompt: String,
        /// Optional region of interest.
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<RectIR>,
    },
}
