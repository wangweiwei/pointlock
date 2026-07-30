//! Typed, serde-serializable error and diagnostic types (thiserror).
//!
//! [`EvalError`] is the evaluation-time failure vocabulary of `eval`
//! (02 §8.2/§8.3); [`CheckDiagnostic`] is the diagnostic vocabulary of the
//! minimal static `check` entry point (spine §8 stage 3). Both serialize
//! with camelCase wire names (internally tagged on `code`), matching the
//! project-wide serde discipline (spine Appendix A).

use std::fmt;

use pointlock_ir::{PureFn, RefPath};
use serde::{Deserialize, Serialize};

/// JSON value type names, used in type-mismatch errors and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JsonType {
    /// JSON `null`.
    Null,
    /// JSON `true` / `false`.
    Boolean,
    /// JSON number.
    Number,
    /// JSON string.
    String,
    /// JSON array.
    Array,
    /// JSON object.
    Object,
}

impl JsonType {
    /// The [`JsonType`] of a `serde_json::Value`.
    pub fn of(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => JsonType::Null,
            serde_json::Value::Bool(_) => JsonType::Boolean,
            serde_json::Value::Number(_) => JsonType::Number,
            serde_json::Value::String(_) => JsonType::String,
            serde_json::Value::Array(_) => JsonType::Array,
            serde_json::Value::Object(_) => JsonType::Object,
        }
    }
}

impl fmt::Display for JsonType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            JsonType::Null => "null",
            JsonType::Boolean => "boolean",
            JsonType::Number => "number",
            JsonType::String => "string",
            JsonType::Array => "array",
            JsonType::Object => "object",
        })
    }
}

/// The five closed scope roots of the reference grammar (spine §7, 02 §8.1).
/// `secrets` is reserved for v0.2 and is not a member — the [`RefPath`]
/// grammar rejects it before evaluation is ever reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScopeRoot {
    /// `params.*` — run inputs, read-only.
    Params,
    /// `env.*` — read-only environment injected by the binding.
    Env,
    /// `vars.*` — `let` products, SSA single assignment.
    Vars,
    /// `iter.<as>` — `foreach` iteration bindings.
    Iter,
    /// `steps.<id>.output.*` / `steps.<id>.verdict` — upstream step results.
    Steps,
}

impl fmt::Display for ScopeRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ScopeRoot::Params => "params",
            ScopeRoot::Env => "env",
            ScopeRoot::Vars => "vars",
            ScopeRoot::Iter => "iter",
            ScopeRoot::Steps => "steps",
        })
    }
}

/// Arity constraint of a [`PureFn`] (the 02 §8.2 table): a closed
/// `min..=max` range, `max` absent for unbounded variadics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arity {
    /// Minimum number of arguments (inclusive).
    pub min: usize,
    /// Maximum number of arguments (inclusive); absent means unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<usize>,
}

impl Arity {
    /// Exactly `n` arguments.
    pub const fn exact(n: usize) -> Self {
        Arity {
            min: n,
            max: Some(n),
        }
    }

    /// At least `n` arguments, unbounded above.
    pub const fn at_least(n: usize) -> Self {
        Arity { min: n, max: None }
    }

    /// Between `min` and `max` arguments, both inclusive.
    pub const fn range(min: usize, max: usize) -> Self {
        Arity {
            min,
            max: Some(max),
        }
    }

    /// Whether `n` arguments satisfy this constraint.
    pub fn admits(&self, n: usize) -> bool {
        n >= self.min && self.max.is_none_or(|max| n <= max)
    }
}

impl fmt::Display for Arity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.max {
            Some(max) if max == self.min => write!(f, "exactly {}", self.min),
            Some(max) => write!(f, "between {} and {}", self.min, max),
            None => write!(f, "at least {}", self.min),
        }
    }
}

/// The wire name (camelCase, per Appendix A.4) of a [`PureFn`].
pub(crate) fn fn_name(f: &PureFn) -> &'static str {
    match f {
        PureFn::Eq => "eq",
        PureFn::Ne => "ne",
        PureFn::Not => "not",
        PureFn::And => "and",
        PureFn::Or => "or",
        PureFn::Concat => "concat",
        PureFn::Len => "len",
        PureFn::Coalesce => "coalesce",
        PureFn::JsonPath => "jsonPath",
        PureFn::RegexMatch => "regexMatch",
    }
}

/// Renders a type expectation list as `"string | array"`.
pub(crate) fn format_types(types: &[JsonType]) -> String {
    types
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Evaluation-time failure (typed, serializable).
///
/// Reference failures distinguish `missingRoot` (the first name looked up
/// under a scope root is unbound — the eval-time face of
/// absence-by-omission, 02 §2.4) from `missingPath` (a later segment is
/// absent) and `notAnObject` (a mid-path value cannot be traversed).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(
    tag = "code",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EvalError {
    /// The path's root is outside the closed scope grammar. Defensive only:
    /// the [`RefPath`] grammar admits exactly the five [`ScopeRoot`]s, so a
    /// validated path can never produce this.
    #[error("'{path}': unknown scope root '{root}'")]
    UnknownRoot {
        /// The offending reference path.
        path: RefPath,
        /// The out-of-grammar root segment.
        root: String,
    },

    /// The first name looked up under the scope root is not bound
    /// (e.g. `params.ssid` with no `ssid` param, `steps.x.output` with no
    /// step `x` in scope).
    #[error("'{path}': no binding named '{name}' under scope root '{root}'")]
    MissingRoot {
        /// The offending reference path.
        path: RefPath,
        /// The scope root the lookup went through.
        root: ScopeRoot,
        /// The unbound first-level name.
        name: String,
    },

    /// The root binding exists but a later segment is absent
    /// (e.g. `params.a.b` where `params.a` exists but has no key `b`;
    /// `steps.x.verdict` where step `x` has produced no verdict yet).
    #[error("'{path}': segment '{segment}' is absent")]
    MissingPath {
        /// The offending reference path.
        path: RefPath,
        /// The first absent segment.
        segment: String,
    },

    /// A mid-path value is not a JSON object and cannot be traversed
    /// further (structural mismatch — never treated as absence).
    #[error("'{path}': cannot traverse segment '{segment}': value is {actual}, not an object")]
    NotAnObject {
        /// The offending reference path.
        path: RefPath,
        /// The segment where traversal stopped.
        segment: String,
        /// The actual JSON type encountered.
        actual: JsonType,
    },

    /// Argument count violates the 02 §8.2 arity table (defensive re-check;
    /// the wire schema enforces the same table).
    #[error("fn '{}' expects {expected} argument(s), got {actual}", fn_name(.function))]
    ArityMismatch {
        /// The applied pure function.
        #[serde(rename = "fn")]
        function: PureFn,
        /// The admissible arity.
        expected: Arity,
        /// The actual argument count.
        actual: usize,
    },

    /// An argument evaluated to a JSON type outside the function's
    /// signature (strict typing: no coercion anywhere, 02 §8.2).
    #[error("fn '{}' argument {arg_index}: expected {}, got {actual}", fn_name(.function), format_types(.expected))]
    TypeMismatch {
        /// The applied pure function.
        #[serde(rename = "fn")]
        function: PureFn,
        /// Zero-based argument position.
        arg_index: usize,
        /// The admissible JSON types at this position.
        expected: Vec<JsonType>,
        /// The actual JSON type encountered.
        actual: JsonType,
    },

    /// A literal-only argument position (`jsonPath` path, `regexMatch`
    /// pattern/flags, 02 §8.2) received a non-literal expression.
    #[error("fn '{}' argument {arg_index} must be a literal", fn_name(.function))]
    LitRequired {
        /// The applied pure function.
        #[serde(rename = "fn")]
        function: PureFn,
        /// Zero-based argument position.
        arg_index: usize,
    },

    /// The `jsonPath` path literal is not a valid RFC 9535 query.
    #[error("invalid jsonPath query '{path}': {message}")]
    InvalidJsonPath {
        /// The offending path literal.
        path: String,
        /// Parser message.
        message: String,
    },

    /// The `regexMatch` pattern literal does not compile.
    #[error("invalid regexMatch pattern '{pattern}': {message}")]
    InvalidRegex {
        /// The offending pattern literal.
        pattern: String,
        /// Compiler message.
        message: String,
    },

    /// The `regexMatch` flags literal contains an unsupported flag
    /// (supported: `i`, `m`, `s`, `x`).
    #[error("invalid regexMatch flags '{flags}': supported flags are i, m, s, x")]
    InvalidRegexFlags {
        /// The offending flags literal.
        flags: String,
    },
}

/// Diagnostic produced by the minimal static `check` entry point.
///
/// `check` returns a *list* of diagnostics (never aborts at the first);
/// deep type inference over the whole flow is the compiler `check` stage's
/// job (spine §8 stage 3) and is deliberately not attempted here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(
    tag = "code",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CheckDiagnostic {
    /// The first name under a non-`steps` scope root is not declared in the
    /// scope shape.
    #[error("'{path}': name '{name}' is not declared under scope root '{root}'")]
    UndeclaredName {
        /// The offending reference path.
        path: RefPath,
        /// The scope root the reference goes through.
        root: ScopeRoot,
        /// The undeclared first-level name.
        name: String,
    },

    /// A `steps.<id>.*` reference names a step id absent from the scope
    /// shape.
    #[error("'{path}': step '{step_id}' is not declared in scope")]
    UndeclaredStep {
        /// The offending reference path.
        path: RefPath,
        /// The undeclared step id.
        step_id: String,
    },

    /// Argument count violates the 02 §8.2 arity table.
    #[error("fn '{}' expects {expected} argument(s), got {actual}", fn_name(.function))]
    ArityMismatch {
        /// The applied pure function.
        #[serde(rename = "fn")]
        function: PureFn,
        /// The admissible arity.
        expected: Arity,
        /// The actual argument count.
        actual: usize,
    },

    /// A literal-only argument position (`jsonPath` path, `regexMatch`
    /// pattern/flags) holds a non-literal expression.
    #[error("fn '{}' argument {arg_index} must be a literal", fn_name(.function))]
    LitRequired {
        /// The applied pure function.
        #[serde(rename = "fn")]
        function: PureFn,
        /// Zero-based argument position.
        arg_index: usize,
    },

    /// A literal-only argument position holds a literal that is not a
    /// string.
    #[error("fn '{}' argument {arg_index}: literal must be a string, got {actual}", fn_name(.function))]
    LitNotString {
        /// The applied pure function.
        #[serde(rename = "fn")]
        function: PureFn,
        /// Zero-based argument position.
        arg_index: usize,
        /// The actual JSON type of the literal.
        actual: JsonType,
    },

    /// The `jsonPath` path literal is not a valid RFC 9535 query.
    #[error("invalid jsonPath query '{path}': {message}")]
    InvalidJsonPathLiteral {
        /// The offending path literal.
        path: String,
        /// Parser message.
        message: String,
    },

    /// The `regexMatch` pattern literal does not compile.
    #[error("invalid regexMatch pattern '{pattern}': {message}")]
    InvalidRegexLiteral {
        /// The offending pattern literal.
        pattern: String,
        /// Compiler message.
        message: String,
    },

    /// The `regexMatch` flags literal contains an unsupported flag
    /// (supported: `i`, `m`, `s`, `x`).
    #[error("invalid regexMatch flags '{flags}': supported flags are i, m, s, x")]
    InvalidRegexFlags {
        /// The offending flags literal.
        flags: String,
    },
    /// A function argument's DEFINITE type contradicts the 02 §8.2
    /// signature (C5). `Unknown`-typed arguments never raise this.
    #[error(
        "{function:?}: argument {position} is {actual:?}, the signature admits {expected:?} (02 §8.2)"
    )]
    ArgumentType {
        /// The function whose signature is violated.
        function: PureFn,
        /// 1-based argument position.
        position: usize,
        /// The types the signature admits at that position.
        expected: Vec<JsonType>,
        /// The argument's inferred definite type.
        actual: JsonType,
    },

    /// `eq`/`ne` over two DEFINITE, different types — the comparison can
    /// never hold type-equal operands (02 §8.2 「两参类型可比」).
    #[error("{function:?}: {left:?} and {right:?} are never comparable (02 §8.2)")]
    NotComparable {
        /// `eq` or `ne`.
        function: PureFn,
        /// The left operand's definite type.
        left: JsonType,
        /// The right operand's definite type.
        right: JsonType,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn arity_admits() {
        assert!(Arity::exact(2).admits(2));
        assert!(!Arity::exact(2).admits(3));
        assert!(Arity::at_least(2).admits(9));
        assert!(!Arity::at_least(2).admits(1));
        assert!(Arity::range(2, 3).admits(3));
        assert!(!Arity::range(2, 3).admits(4));
    }

    #[test]
    fn eval_error_wire_shape_is_camel_case() {
        let err = EvalError::ArityMismatch {
            function: PureFn::And,
            expected: Arity::at_least(2),
            actual: 1,
        };
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({ "code": "arityMismatch", "fn": "and", "expected": { "min": 2 }, "actual": 1 })
        );

        let err = EvalError::TypeMismatch {
            function: PureFn::Not,
            arg_index: 0,
            expected: vec![JsonType::Boolean],
            actual: JsonType::Number,
        };
        let value = serde_json::to_value(&err).unwrap();
        assert_eq!(value["code"], "typeMismatch");
        assert_eq!(value["argIndex"], 0);
        assert_eq!(value["expected"], json!(["boolean"]));
        assert_eq!(value["actual"], "number");

        let back: EvalError = serde_json::from_value(value).unwrap();
        assert_eq!(back, err);
    }

    #[test]
    fn check_diagnostic_wire_shape_is_camel_case() {
        let path = pointlock_ir::RefPath::new("steps.zz.output").unwrap();
        let diag = CheckDiagnostic::UndeclaredStep {
            path: path.clone(),
            step_id: "zz".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&diag).unwrap(),
            json!({ "code": "undeclaredStep", "path": "steps.zz.output", "stepId": "zz" })
        );
    }

    #[test]
    fn display_messages_are_stable() {
        let err = EvalError::MissingRoot {
            path: pointlock_ir::RefPath::new("params.ssid").unwrap(),
            root: ScopeRoot::Params,
            name: "ssid".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "'params.ssid': no binding named 'ssid' under scope root 'params'"
        );
    }
}
