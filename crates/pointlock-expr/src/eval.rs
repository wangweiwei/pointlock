//! The expression evaluator: `eval(expr, scope)` (02 §8.2/§8.3).
//!
//! Evaluation is a pure computation over an already-materialized [`Scope`]
//! — no I/O, no clock, no provider access. This is what makes the
//! `asserting` phase pure and offline re-judging (`judgeDirty` alignment)
//! mathematically sound (spine §6.2, 02 §8.3).
//!
//! Semantics follow the 02 §8.2 table verbatim where it pins behavior.
//! Choices the table leaves open are documented on each function below and
//! summarized in the crate root.

use pointlock_ir::{Expr, FnExpr, PureFn};
use regex::{Regex, RegexBuilder};
use serde_json::{Number, Value};
use serde_json_path::JsonPath as JsonPathQuery;

use crate::error::{Arity, EvalError, JsonType};
use crate::scope::Scope;

/// The arity table of 02 §8.2. Enforced by the wire schema, re-enforced by
/// [`check`](crate::check), and defensively re-checked by [`eval`].
pub fn arity_of(f: PureFn) -> Arity {
    match f {
        PureFn::Eq | PureFn::Ne | PureFn::JsonPath => Arity::exact(2),
        PureFn::Not | PureFn::Len => Arity::exact(1),
        PureFn::And | PureFn::Or | PureFn::Coalesce => Arity::at_least(2),
        PureFn::Concat => Arity::at_least(1),
        PureFn::RegexMatch => Arity::range(2, 3),
    }
}

/// Evaluates `expr` against `scope`.
///
/// - `lit` returns the literal verbatim.
/// - `ref` resolves the dotted path via [`Scope::resolve`] (missing
///   bindings and mid-path failures are typed errors, never null).
/// - `fn` applies the whitelisted pure function per the 02 §8.2 table.
pub fn eval(expr: &Expr, scope: &Scope) -> Result<Value, EvalError> {
    match expr {
        Expr::Lit(lit) => Ok(lit.lit.clone()),
        Expr::Ref(reference) => scope.resolve(&reference.r#ref).cloned(),
        Expr::Fn(fx) => eval_fn(fx, scope),
    }
}

fn eval_fn(fx: &FnExpr, scope: &Scope) -> Result<Value, EvalError> {
    let f = fx.r#fn;
    let args = &fx.args;
    let expected = arity_of(f);
    if !expected.admits(args.len()) {
        return Err(EvalError::ArityMismatch {
            function: f,
            expected,
            actual: args.len(),
        });
    }
    match f {
        // `eq` / `ne`: deep structural equality. The table pins the
        // signature `(T, T) → boolean`; the equality relation on numbers is
        // an unpinned detail — this implementation compares numbers by
        // mathematical value (1 == 1.0), consistent with the JSON data
        // model where a number has no intrinsic width.
        PureFn::Eq => Ok(Value::Bool(json_eq(
            &eval(&args[0], scope)?,
            &eval(&args[1], scope)?,
        ))),
        PureFn::Ne => Ok(Value::Bool(!json_eq(
            &eval(&args[0], scope)?,
            &eval(&args[1], scope)?,
        ))),
        // `not` / `and` / `or`: strictly boolean — any non-boolean operand
        // is a type error, never coerced (no truthiness).
        PureFn::Not => {
            let b = expect_bool(f, 0, eval(&args[0], scope)?)?;
            Ok(Value::Bool(!b))
        }
        // The table notes short-circuiting has no *observable* difference
        // for pure expressions; this implementation does short-circuit,
        // which additionally means evaluation errors in unreached
        // arguments are not surfaced (unpinned choice, documented).
        PureFn::And => {
            for (index, arg) in args.iter().enumerate() {
                if !expect_bool(f, index, eval(arg, scope)?)? {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        PureFn::Or => {
            for (index, arg) in args.iter().enumerate() {
                if expect_bool(f, index, eval(arg, scope)?)? {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }
        // `concat`: string concatenation; any non-string operand is a type
        // error (no implicit stringification).
        PureFn::Concat => {
            let mut out = String::new();
            for (index, arg) in args.iter().enumerate() {
                out.push_str(&expect_string(f, index, eval(arg, scope)?)?);
            }
            Ok(Value::String(out))
        }
        // `len`: character count for strings (Unicode scalar values — an
        // unpinned choice; byte length would leak encoding), element count
        // for arrays; anything else is a type error.
        PureFn::Len => match eval(&args[0], scope)? {
            Value::String(s) => Ok(Value::from(s.chars().count() as u64)),
            Value::Array(items) => Ok(Value::from(items.len() as u64)),
            other => Err(EvalError::TypeMismatch {
                function: f,
                arg_index: 0,
                expected: vec![JsonType::String, JsonType::Array],
                actual: JsonType::of(&other),
            }),
        },
        PureFn::Coalesce => eval_coalesce(args, scope),
        PureFn::JsonPath => eval_json_path(f, args, scope),
        PureFn::RegexMatch => eval_regex_match(f, args, scope),
    }
}

/// `coalesce` — first non-absent value (02 §8.2, `(T?, …, T) → T`).
///
/// Unpinned details (documented choices):
/// - Absence at eval time is (a) a JSON `null` result, or (b) a **direct
///   `ref` argument** whose resolution fails with a missing binding
///   (`missingRoot` / `missingPath`) — the eval-time face of
///   absence-by-omission (02 §2.4). Structural failures (`notAnObject`)
///   and errors inside nested `fn` arguments still propagate.
/// - Arguments after the first non-absent value are not evaluated.
/// - If every argument is absent, the result is JSON `null`.
fn eval_coalesce(args: &[Expr], scope: &Scope) -> Result<Value, EvalError> {
    for arg in args {
        match eval(arg, scope) {
            Ok(Value::Null) => continue,
            Ok(value) => return Ok(value),
            Err(EvalError::MissingRoot { .. } | EvalError::MissingPath { .. })
                if matches!(arg, Expr::Ref(_)) =>
            {
                continue;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(Value::Null)
}

/// `jsonPath` — RFC 9535 query (`serde_json_path`); `args[1]` must be a
/// literal string (02 §8.2, path statically inspectable).
///
/// Result-shape choice (the table pins only `→ any` / `unknown` type):
/// the query's nodelist collapses as empty → `null`, exactly one node →
/// that node, more than one node → array of nodes. Downstream consumption
/// must narrow via `expect_schema` anyway (02 §8.2).
fn eval_json_path(f: PureFn, args: &[Expr], scope: &Scope) -> Result<Value, EvalError> {
    let path_lit = lit_string_arg(f, args, 1)?;
    let query = JsonPathQuery::parse(&path_lit).map_err(|err| EvalError::InvalidJsonPath {
        path: path_lit.clone(),
        message: err.to_string(),
    })?;
    let root = eval(&args[0], scope)?;
    let nodes = query.query(&root).all();
    Ok(match nodes.len() {
        0 => Value::Null,
        1 => nodes[0].clone(),
        _ => Value::Array(nodes.into_iter().cloned().collect()),
    })
}

/// `regexMatch` — `(string, string[, string]) → boolean`; pattern and flags
/// must be literals (02 §8.2). The `regex` crate guarantees linear-time
/// matching, which discharges the "reject catastrophic backtracking"
/// constraint by construction.
///
/// Unpinned details (documented choices):
/// - Flags vocabulary: `i` (case-insensitive), `m` (multi-line),
///   `s` (dot matches newline), `x` (ignore whitespace); anything else is
///   `invalidRegexFlags`.
/// - Match semantics: unanchored search (`Regex::is_match`); authors anchor
///   explicitly with `^` / `$`.
fn eval_regex_match(f: PureFn, args: &[Expr], scope: &Scope) -> Result<Value, EvalError> {
    let pattern = lit_string_arg(f, args, 1)?;
    let flags = if args.len() == 3 {
        Some(lit_string_arg(f, args, 2)?)
    } else {
        None
    };
    let regex = build_regex(&pattern, flags.as_deref()).map_err(|issue| match issue {
        RegexIssue::BadFlags(flags) => EvalError::InvalidRegexFlags { flags },
        RegexIssue::BadPattern(message) => EvalError::InvalidRegex {
            pattern: pattern.clone(),
            message,
        },
    })?;
    let subject = expect_string(f, 0, eval(&args[0], scope)?)?;
    Ok(Value::Bool(regex.is_match(&subject)))
}

/// Why a regex could not be built from its literals.
pub(crate) enum RegexIssue {
    /// The flags string contains a flag outside `i` / `m` / `s` / `x`.
    BadFlags(String),
    /// The pattern does not compile.
    BadPattern(String),
}

/// Compiles a `regexMatch` pattern with the optional flags literal.
/// Shared by [`eval`] and [`check`](crate::check).
pub(crate) fn build_regex(pattern: &str, flags: Option<&str>) -> Result<Regex, RegexIssue> {
    let mut builder = RegexBuilder::new(pattern);
    if let Some(flags) = flags {
        for flag in flags.chars() {
            match flag {
                'i' => builder.case_insensitive(true),
                'm' => builder.multi_line(true),
                's' => builder.dot_matches_new_line(true),
                'x' => builder.ignore_whitespace(true),
                _ => return Err(RegexIssue::BadFlags(flags.to_string())),
            };
        }
    }
    builder
        .build()
        .map_err(|err| RegexIssue::BadPattern(err.to_string()))
}

/// Extracts a literal-only string argument (`jsonPath` path, `regexMatch`
/// pattern/flags): non-literal → `litRequired`; literal non-string →
/// `typeMismatch`.
fn lit_string_arg(f: PureFn, args: &[Expr], index: usize) -> Result<String, EvalError> {
    match &args[index] {
        Expr::Lit(lit) => match &lit.lit {
            Value::String(s) => Ok(s.clone()),
            other => Err(EvalError::TypeMismatch {
                function: f,
                arg_index: index,
                expected: vec![JsonType::String],
                actual: JsonType::of(other),
            }),
        },
        _ => Err(EvalError::LitRequired {
            function: f,
            arg_index: index,
        }),
    }
}

fn expect_bool(f: PureFn, index: usize, value: Value) -> Result<bool, EvalError> {
    match value {
        Value::Bool(b) => Ok(b),
        other => Err(EvalError::TypeMismatch {
            function: f,
            arg_index: index,
            expected: vec![JsonType::Boolean],
            actual: JsonType::of(&other),
        }),
    }
}

fn expect_string(f: PureFn, index: usize, value: Value) -> Result<String, EvalError> {
    match value {
        Value::String(s) => Ok(s),
        other => Err(EvalError::TypeMismatch {
            function: f,
            arg_index: index,
            expected: vec![JsonType::String],
            actual: JsonType::of(&other),
        }),
    }
}

/// Deep structural equality with numbers compared by mathematical value.
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => number_eq(x, y),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(va, vb)| json_eq(va, vb))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, va)| y.get(key).is_some_and(|vb| json_eq(va, vb)))
        }
        _ => a == b,
    }
}

/// Numeric equality by mathematical value: exact within a shared integer
/// representation, `f64` comparison across representations (documented
/// approximation for integers beyond 2^53).
fn number_eq(x: &Number, y: &Number) -> bool {
    if let (Some(a), Some(b)) = (x.as_i64(), y.as_i64()) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (x.as_u64(), y.as_u64()) {
        return a == b;
    }
    match (x.as_f64(), y.as_f64()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pointlock_ir::RefPath;
    use serde_json::json;

    fn scope() -> Scope {
        let mut scope = Scope::new();
        scope
            .set_param("ssid", json!("lab-5g"))
            .set_param("nested", json!({ "a": { "b": [1, 2, 3] } }))
            .set_env("deviceId", json!("device-1"))
            .set_var("label", json!("run-1"))
            .set_iter("item", json!(2))
            .set_step_output(
                "wait_connected",
                json!({ "matched": true, "element": { "stableNodeId": "n1" } }),
            )
            .set_step_verdict("wait_connected", json!("pass"));
        scope
    }

    fn r(path: &str) -> Expr {
        Expr::reference(RefPath::new(path).unwrap())
    }

    fn call(f: PureFn, args: Vec<Expr>) -> Expr {
        Expr::call(f, args)
    }

    #[test]
    fn lit_returns_verbatim() {
        let value = json!({ "a": [1, null, "x"] });
        assert_eq!(eval(&Expr::lit(value.clone()), &scope()).unwrap(), value);
    }

    #[test]
    fn ref_resolves_deep_paths() {
        assert_eq!(
            eval(
                &r("steps.wait_connected.output.element.stableNodeId"),
                &scope()
            )
            .unwrap(),
            json!("n1")
        );
        assert_eq!(
            eval(&r("steps.wait_connected.verdict"), &scope()).unwrap(),
            json!("pass")
        );
        assert!(matches!(
            eval(&r("params.nested.a.zz"), &scope()),
            Err(EvalError::MissingPath { .. })
        ));
        assert!(matches!(
            eval(&r("params.nope"), &scope()),
            Err(EvalError::MissingRoot { .. })
        ));
    }

    #[test]
    fn eq_ne_deep_equality() {
        let a = Expr::lit(json!({ "x": [1, { "y": "z" }], "n": 1 }));
        let b = Expr::lit(json!({ "n": 1, "x": [1, { "y": "z" }] }));
        assert_eq!(
            eval(&call(PureFn::Eq, vec![a.clone(), b.clone()]), &scope()).unwrap(),
            json!(true)
        );
        assert_eq!(
            eval(&call(PureFn::Ne, vec![a, b]), &scope()).unwrap(),
            json!(false)
        );
        // Numbers compare by mathematical value (documented choice).
        assert_eq!(
            eval(
                &call(PureFn::Eq, vec![Expr::lit(json!(1)), Expr::lit(json!(1.0))]),
                &scope()
            )
            .unwrap(),
            json!(true)
        );
        // No cross-type coercion: "1" != 1.
        assert_eq!(
            eval(
                &call(PureFn::Eq, vec![Expr::lit(json!("1")), Expr::lit(json!(1))]),
                &scope()
            )
            .unwrap(),
            json!(false)
        );
    }

    #[test]
    fn not_is_strictly_boolean() {
        assert_eq!(
            eval(&call(PureFn::Not, vec![Expr::lit(json!(true))]), &scope()).unwrap(),
            json!(false)
        );
        assert_eq!(
            eval(&call(PureFn::Not, vec![Expr::lit(json!(1))]), &scope()),
            Err(EvalError::TypeMismatch {
                function: PureFn::Not,
                arg_index: 0,
                expected: vec![JsonType::Boolean],
                actual: JsonType::Number,
            })
        );
    }

    #[test]
    fn and_or_short_circuit_and_stay_strict() {
        // Short-circuit: the missing ref after `false` is never evaluated.
        assert_eq!(
            eval(
                &call(PureFn::And, vec![Expr::lit(json!(false)), r("params.nope")]),
                &scope()
            )
            .unwrap(),
            json!(false)
        );
        // Short-circuit: the would-be type error after `true` is never hit.
        assert_eq!(
            eval(
                &call(
                    PureFn::Or,
                    vec![
                        Expr::lit(json!(true)),
                        call(PureFn::Not, vec![Expr::lit(json!(1))]),
                    ],
                ),
                &scope()
            )
            .unwrap(),
            json!(true)
        );
        assert_eq!(
            eval(
                &call(
                    PureFn::And,
                    vec![Expr::lit(json!(true)), Expr::lit(json!(true))]
                ),
                &scope()
            )
            .unwrap(),
            json!(true)
        );
        assert_eq!(
            eval(
                &call(
                    PureFn::Or,
                    vec![Expr::lit(json!(false)), Expr::lit(json!(false))]
                ),
                &scope()
            )
            .unwrap(),
            json!(false)
        );
        // Strict booleans: non-boolean in a reached position is an error.
        assert_eq!(
            eval(
                &call(
                    PureFn::And,
                    vec![Expr::lit(json!(true)), Expr::lit(json!("x"))]
                ),
                &scope()
            ),
            Err(EvalError::TypeMismatch {
                function: PureFn::And,
                arg_index: 1,
                expected: vec![JsonType::Boolean],
                actual: JsonType::String,
            })
        );
    }

    #[test]
    fn arity_is_enforced() {
        assert_eq!(
            eval(&call(PureFn::And, vec![Expr::lit(json!(true))]), &scope()),
            Err(EvalError::ArityMismatch {
                function: PureFn::And,
                expected: Arity::at_least(2),
                actual: 1,
            })
        );
        assert_eq!(
            eval(&call(PureFn::Len, vec![]), &scope()),
            Err(EvalError::ArityMismatch {
                function: PureFn::Len,
                expected: Arity::exact(1),
                actual: 0,
            })
        );
        assert_eq!(
            eval(
                &call(
                    PureFn::RegexMatch,
                    vec![
                        Expr::lit(json!("a")),
                        Expr::lit(json!("a")),
                        Expr::lit(json!("i")),
                        Expr::lit(json!("m")),
                    ],
                ),
                &scope()
            ),
            Err(EvalError::ArityMismatch {
                function: PureFn::RegexMatch,
                expected: Arity::range(2, 3),
                actual: 4,
            })
        );
    }

    #[test]
    fn concat_joins_strings_only() {
        assert_eq!(
            eval(
                &call(
                    PureFn::Concat,
                    vec![Expr::lit(json!("net: ")), r("params.ssid")]
                ),
                &scope()
            )
            .unwrap(),
            json!("net: lab-5g")
        );
        assert_eq!(
            eval(
                &call(
                    PureFn::Concat,
                    vec![Expr::lit(json!("a")), Expr::lit(json!(1))]
                ),
                &scope()
            ),
            Err(EvalError::TypeMismatch {
                function: PureFn::Concat,
                arg_index: 1,
                expected: vec![JsonType::String],
                actual: JsonType::Number,
            })
        );
    }

    #[test]
    fn len_counts_chars_and_elements() {
        assert_eq!(
            eval(
                &call(PureFn::Len, vec![Expr::lit(json!("héllo"))]),
                &scope()
            )
            .unwrap(),
            json!(5)
        );
        assert_eq!(
            eval(&call(PureFn::Len, vec![Expr::lit(json!("汉字"))]), &scope()).unwrap(),
            json!(2)
        );
        assert_eq!(
            eval(
                &call(PureFn::Len, vec![Expr::lit(json!([1, 2, 3]))]),
                &scope()
            )
            .unwrap(),
            json!(3)
        );
        assert_eq!(
            eval(&call(PureFn::Len, vec![Expr::lit(json!(5))]), &scope()),
            Err(EvalError::TypeMismatch {
                function: PureFn::Len,
                arg_index: 0,
                expected: vec![JsonType::String, JsonType::Array],
                actual: JsonType::Number,
            })
        );
    }

    #[test]
    fn coalesce_returns_first_non_absent() {
        assert_eq!(
            eval(
                &call(
                    PureFn::Coalesce,
                    vec![Expr::lit(json!(null)), Expr::lit(json!("x"))]
                ),
                &scope()
            )
            .unwrap(),
            json!("x")
        );
        // A missing direct ref is absence, not an error.
        assert_eq!(
            eval(
                &call(
                    PureFn::Coalesce,
                    vec![r("params.nope"), Expr::lit(json!("d"))]
                ),
                &scope()
            )
            .unwrap(),
            json!("d")
        );
        assert_eq!(
            eval(
                &call(
                    PureFn::Coalesce,
                    vec![r("params.nested.a.zz"), Expr::lit(json!("d"))],
                ),
                &scope()
            )
            .unwrap(),
            json!("d")
        );
        // All absent → null.
        assert_eq!(
            eval(
                &call(
                    PureFn::Coalesce,
                    vec![Expr::lit(json!(null)), r("params.nope")]
                ),
                &scope()
            )
            .unwrap(),
            json!(null)
        );
        // Arguments after the first non-absent value are not evaluated.
        assert_eq!(
            eval(
                &call(
                    PureFn::Coalesce,
                    vec![
                        Expr::lit(json!("v")),
                        call(PureFn::Not, vec![Expr::lit(json!(1))]),
                    ],
                ),
                &scope()
            )
            .unwrap(),
            json!("v")
        );
        // Errors inside nested fn arguments propagate.
        assert!(matches!(
            eval(
                &call(
                    PureFn::Coalesce,
                    vec![
                        call(PureFn::Not, vec![Expr::lit(json!(1))]),
                        Expr::lit(json!("d")),
                    ],
                ),
                &scope()
            ),
            Err(EvalError::TypeMismatch { .. })
        ));
        // Structural failures are errors, not absence.
        assert!(matches!(
            eval(
                &call(
                    PureFn::Coalesce,
                    vec![r("params.ssid.x"), Expr::lit(json!("d"))]
                ),
                &scope()
            ),
            Err(EvalError::NotAnObject { .. })
        ));
    }

    #[test]
    fn json_path_queries_and_collapses_nodelists() {
        // Exactly one node → that node.
        assert_eq!(
            eval(
                &call(
                    PureFn::JsonPath,
                    vec![r("params.nested"), Expr::lit(json!("$.a.b[1]"))],
                ),
                &scope()
            )
            .unwrap(),
            json!(2)
        );
        // Multiple nodes → array of nodes.
        assert_eq!(
            eval(
                &call(
                    PureFn::JsonPath,
                    vec![r("params.nested"), Expr::lit(json!("$.a.b[*]"))],
                ),
                &scope()
            )
            .unwrap(),
            json!([1, 2, 3])
        );
        // Empty nodelist → null.
        assert_eq!(
            eval(
                &call(
                    PureFn::JsonPath,
                    vec![r("params.nested"), Expr::lit(json!("$.zz"))],
                ),
                &scope()
            )
            .unwrap(),
            json!(null)
        );
    }

    #[test]
    fn json_path_enforces_literal_path() {
        assert_eq!(
            eval(
                &call(PureFn::JsonPath, vec![r("params.nested"), r("params.ssid")]),
                &scope()
            ),
            Err(EvalError::LitRequired {
                function: PureFn::JsonPath,
                arg_index: 1,
            })
        );
        assert_eq!(
            eval(
                &call(
                    PureFn::JsonPath,
                    vec![r("params.nested"), Expr::lit(json!(5))]
                ),
                &scope()
            ),
            Err(EvalError::TypeMismatch {
                function: PureFn::JsonPath,
                arg_index: 1,
                expected: vec![JsonType::String],
                actual: JsonType::Number,
            })
        );
        assert!(matches!(
            eval(
                &call(
                    PureFn::JsonPath,
                    vec![r("params.nested"), Expr::lit(json!("$["))],
                ),
                &scope()
            ),
            Err(EvalError::InvalidJsonPath { .. })
        ));
    }

    #[test]
    fn regex_match_matches_and_respects_flags() {
        assert_eq!(
            eval(
                &call(
                    PureFn::RegexMatch,
                    vec![r("params.ssid"), Expr::lit(json!("^lab-\\d+g$"))],
                ),
                &scope()
            )
            .unwrap(),
            json!(true)
        );
        assert_eq!(
            eval(
                &call(
                    PureFn::RegexMatch,
                    vec![Expr::lit(json!("other")), Expr::lit(json!("^lab"))],
                ),
                &scope()
            )
            .unwrap(),
            json!(false)
        );
        assert_eq!(
            eval(
                &call(
                    PureFn::RegexMatch,
                    vec![
                        Expr::lit(json!("LAB-5G")),
                        Expr::lit(json!("^lab")),
                        Expr::lit(json!("i")),
                    ],
                ),
                &scope()
            )
            .unwrap(),
            json!(true)
        );
    }

    #[test]
    fn regex_match_enforces_literals_and_vocabulary() {
        assert_eq!(
            eval(
                &call(PureFn::RegexMatch, vec![r("params.ssid"), r("params.ssid")]),
                &scope()
            ),
            Err(EvalError::LitRequired {
                function: PureFn::RegexMatch,
                arg_index: 1,
            })
        );
        assert_eq!(
            eval(
                &call(
                    PureFn::RegexMatch,
                    vec![
                        Expr::lit(json!("a")),
                        Expr::lit(json!("a")),
                        r("params.ssid"),
                    ],
                ),
                &scope()
            ),
            Err(EvalError::LitRequired {
                function: PureFn::RegexMatch,
                arg_index: 2,
            })
        );
        assert!(matches!(
            eval(
                &call(
                    PureFn::RegexMatch,
                    vec![Expr::lit(json!("a")), Expr::lit(json!("("))],
                ),
                &scope()
            ),
            Err(EvalError::InvalidRegex { .. })
        ));
        assert_eq!(
            eval(
                &call(
                    PureFn::RegexMatch,
                    vec![
                        Expr::lit(json!("a")),
                        Expr::lit(json!("a")),
                        Expr::lit(json!("z")),
                    ],
                ),
                &scope()
            ),
            Err(EvalError::InvalidRegexFlags {
                flags: "z".to_string(),
            })
        );
        assert_eq!(
            eval(
                &call(
                    PureFn::RegexMatch,
                    vec![Expr::lit(json!(1)), Expr::lit(json!("a"))]
                ),
                &scope()
            ),
            Err(EvalError::TypeMismatch {
                function: PureFn::RegexMatch,
                arg_index: 0,
                expected: vec![JsonType::String],
                actual: JsonType::Number,
            })
        );
    }
}
