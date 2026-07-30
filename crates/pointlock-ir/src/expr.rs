//! The non-Turing-complete expression AST (spine §7, 02 §8).
//!
//! IR expressions are data (a JSON AST), never strings: the YAML surface
//! `${{ ... }}` is compiled away in `parse`/`normalize`, and the runner has
//! no parser and no eval. Purity (no loops, no user functions, no I/O, no
//! clock) is what makes offline re-judging (`judgeDirty` alignment)
//! mathematically sound.

use std::borrow::Cow;
use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::primitives::{Identifier, RefPath};

/// Expression node: exactly one of `lit` / `ref` / `fn` (02 §8.1).
///
/// Wire shape is the baseline schema's `oneOf` of three closed single-key
/// objects; the variants are mutually exclusive by their required keys, so
/// serde's untagged representation is deterministic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Expr {
    /// Literal JSON value.
    Lit(LitExpr),
    /// Reference into the closed scope grammar.
    Ref(RefExpr),
    /// Whitelisted pure-function application.
    Fn(FnExpr),
}

impl Expr {
    /// Builds a literal expression.
    pub fn lit(value: impl Into<serde_json::Value>) -> Self {
        Expr::Lit(LitExpr { lit: value.into() })
    }

    /// Builds a reference expression.
    pub fn reference(path: RefPath) -> Self {
        Expr::Ref(RefExpr { r#ref: path })
    }

    /// Builds a pure-function application. Arity/type constraints are
    /// enforced by the schema and by the compiler `check` phase, not here.
    pub fn call(f: PureFn, args: Vec<Expr>) -> Self {
        Expr::Fn(FnExpr { r#fn: f, args })
    }
}

/// Literal JSON value (any JSON type, including null).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LitExpr {
    /// The literal value, verbatim.
    pub lit: serde_json::Value,
}

/// Reference expression: a [`RefPath`] into the closed scope grammar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefExpr {
    /// The dotted reference path.
    pub r#ref: RefPath,
}

/// Whitelisted pure-function application.
///
/// Arity is enforced by the schema (the `allOf` conditionals below mirror the
/// baseline); argument types plus the literal-only constraints (`jsonPath`
/// path, `regexMatch` pattern/flags) are enforced in the compiler `check`
/// phase (02 §8.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("allOf" = [
    { "if": { "properties": { "fn": { "enum": ["eq", "ne", "jsonPath"] } } },
      "then": { "properties": { "args": { "minItems": 2, "maxItems": 2 } } } },
    { "if": { "properties": { "fn": { "enum": ["not", "len"] } } },
      "then": { "properties": { "args": { "minItems": 1, "maxItems": 1 } } } },
    { "if": { "properties": { "fn": { "enum": ["and", "or", "coalesce"] } } },
      "then": { "properties": { "args": { "minItems": 2 } } } },
    { "if": { "properties": { "fn": { "const": "concat" } } },
      "then": { "properties": { "args": { "minItems": 1 } } } },
    { "if": { "properties": { "fn": { "const": "regexMatch" } } },
      "then": { "properties": { "args": { "minItems": 2, "maxItems": 3 } } } }
]))]
pub struct FnExpr {
    /// The pure function to apply.
    pub r#fn: PureFn,
    /// Ordered argument expressions.
    pub args: Vec<Expr>,
}

/// The closed pure-function whitelist (spine §7 / A.4, 02 §8.2).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum PureFn {
    /// `(T, T) → boolean`
    Eq,
    /// `(T, T) → boolean`
    Ne,
    /// `(boolean) → boolean`
    Not,
    /// `(boolean…) → boolean` (arity ≥ 2)
    And,
    /// `(boolean…) → boolean` (arity ≥ 2)
    Or,
    /// `(string…) → string` (arity ≥ 1)
    Concat,
    /// `(string | array) → number`
    Len,
    /// `(T?, …, T) → T` — first non-absent value (arity ≥ 2)
    Coalesce,
    /// `(any, string) → any` — path must be a literal string
    JsonPath,
    /// `(string, string[, string]) → boolean` — pattern/flags must be literals
    RegexMatch,
}

/// Identifier-keyed map of expressions (baseline exemption class 2: keys are
/// data, constrained by `propertyNames`; values are strongly typed).
///
/// Backed by a `BTreeMap` so serialization order is deterministic — a
/// prerequisite of the canonical form (02 §12.1), where map member order is
/// JCS-sorted and carries no semantics.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExprMap(pub BTreeMap<Identifier, Expr>);

impl ExprMap {
    /// Creates an empty map.
    pub fn new() -> Self {
        Self::default()
    }
}

impl std::ops::Deref for ExprMap {
    type Target = BTreeMap<Identifier, Expr>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ExprMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<BTreeMap<Identifier, Expr>> for ExprMap {
    fn from(map: BTreeMap<Identifier, Expr>) -> Self {
        Self(map)
    }
}

impl FromIterator<(Identifier, Expr)> for ExprMap {
    fn from_iter<I: IntoIterator<Item = (Identifier, Expr)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl JsonSchema for ExprMap {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ExprMap")
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("pointlock_ir::ExprMap")
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "propertyNames": { "pattern": "^[A-Za-z_][A-Za-z0-9_]*$" },
            "additionalProperties": generator.subschema_for::<Expr>(),
            "description": "Identifier-keyed map of expressions (exemption class 2: keys are data, constrained by propertyNames)."
        })
    }
}
