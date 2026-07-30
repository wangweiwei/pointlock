//! Minimal static check entry point: `check(expr, scope_shape)`.
//!
//! Covers exactly three expression-local obligations (spine §8 stage 3 owns
//! the rest — deep type inference, self-ref positional legality per
//! 02 §4.1.1, data-dependency cycles):
//!
//! 1. Reference root/name legality against a declared [`ScopeShape`].
//! 2. [`PureFn`] arity per the 02 §8.2 table.
//! 3. Literal-only argument positions (`jsonPath` path, `regexMatch`
//!    pattern/flags), including parse/compile validation of the literals
//!    (`regexMatch` patterns are compiled with default flags here; the
//!    `regex` crate's linear-time engine discharges the catastrophic
//!    backtracking rejection by construction).
//!
//! The return shape is final: a list of [`CheckDiagnostic`] (empty = clean);
//! the walk never aborts at the first finding.

use pointlock_ir::{Expr, PureFn, RefPath};
use serde_json::Value;
use serde_json_path::JsonPath as JsonPathQuery;

use crate::error::{CheckDiagnostic, JsonType, ScopeRoot};
use crate::eval::{RegexIssue, arity_of, build_regex};
use crate::scope::ScopeShape;

/// Statically checks `expr` against a declared scope shape, returning all
/// diagnostics found (empty list = no findings).
pub fn check(expr: &Expr, scope_shape: &ScopeShape) -> Vec<CheckDiagnostic> {
    let mut diagnostics = Vec::new();
    walk(expr, scope_shape, &mut diagnostics);
    diagnostics
}

/// The static type of an expression, as far as it is knowable without
/// evaluating (02 §8.2: 「类型由 check 强制」— the knowable part).
///
/// `Unknown` is the gradual-typing escape: references whose shape the
/// caller cannot vouch for, `jsonPath` results, mixed `coalesce` joins.
/// The checker reports only DEFINITE contradictions — an `Unknown` in a
/// typed position is legal and resolves (or degrades) at evaluation time,
/// exactly as before this layer existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprType {
    /// A definitely-typed value.
    Known(JsonType),
    /// Not statically knowable.
    Unknown,
}

/// The caller's knowledge of reference types (the flow's params/env/step
/// output declarations live outside this crate).
pub trait RefTypes {
    /// The static type of the value `path` resolves to; `Unknown` when the
    /// caller cannot vouch for it.
    fn type_of(&self, path: &RefPath) -> ExprType;
}

/// Infers `expr`'s static type, pushing a diagnostic for every DEFINITE
/// argument-type contradiction of the 02 §8.2 signature table (C5). The
/// walk never aborts: every argument is inferred and checked regardless of
/// earlier findings, and arity violations (already reported by [`check`])
/// simply skip the positions that do not exist.
pub fn infer(expr: &Expr, refs: &dyn RefTypes, diagnostics: &mut Vec<CheckDiagnostic>) -> ExprType {
    match expr {
        Expr::Lit(lit) => ExprType::Known(JsonType::of(&lit.lit)),
        Expr::Ref(reference) => refs.type_of(&reference.r#ref),
        Expr::Fn(fx) => {
            let f = fx.r#fn;
            let args: Vec<ExprType> = fx
                .args
                .iter()
                .map(|arg| infer(arg, refs, diagnostics))
                .collect();
            let expect = |diagnostics: &mut Vec<CheckDiagnostic>,
                          position: usize,
                          actual: ExprType,
                          admitted: &[JsonType]| {
                if let ExprType::Known(concrete) = actual
                    && !admitted.contains(&concrete)
                {
                    diagnostics.push(CheckDiagnostic::ArgumentType {
                        function: f,
                        position: position + 1,
                        expected: admitted.to_vec(),
                        actual: concrete,
                    });
                }
            };
            match f {
                PureFn::Eq | PureFn::Ne => {
                    // (T, T) → boolean: two DEFINITE, DIFFERENT types can
                    // never compare equal-typed; either side Unknown is
                    // legal (02 §8.2 「两参类型可比」).
                    if let (Some(ExprType::Known(left)), Some(ExprType::Known(right))) =
                        (args.first().copied(), args.get(1).copied())
                        && left != right
                    {
                        diagnostics.push(CheckDiagnostic::NotComparable {
                            function: f,
                            left,
                            right,
                        });
                    }
                    ExprType::Known(JsonType::Boolean)
                }
                PureFn::Not | PureFn::And | PureFn::Or => {
                    for (position, actual) in args.iter().copied().enumerate() {
                        expect(diagnostics, position, actual, &[JsonType::Boolean]);
                    }
                    ExprType::Known(JsonType::Boolean)
                }
                PureFn::Concat => {
                    for (position, actual) in args.iter().copied().enumerate() {
                        expect(diagnostics, position, actual, &[JsonType::String]);
                    }
                    ExprType::Known(JsonType::String)
                }
                PureFn::Len => {
                    if let Some(actual) = args.first().copied() {
                        expect(diagnostics, 0, actual, &[JsonType::String, JsonType::Array]);
                    }
                    ExprType::Known(JsonType::Number)
                }
                PureFn::Coalesce => {
                    // (T?, …, T) → T: the join of the argument types —
                    // concrete when they all agree, Unknown otherwise.
                    let mut joined: Option<ExprType> = None;
                    for actual in args.iter().copied() {
                        joined = Some(match joined {
                            None => actual,
                            Some(seen) if seen == actual => seen,
                            Some(_) => ExprType::Unknown,
                        });
                    }
                    joined.unwrap_or(ExprType::Unknown)
                }
                PureFn::JsonPath => ExprType::Unknown,
                PureFn::RegexMatch => {
                    if let Some(actual) = args.first().copied() {
                        expect(diagnostics, 0, actual, &[JsonType::String]);
                    }
                    ExprType::Known(JsonType::Boolean)
                }
            }
        }
    }
}

fn walk(expr: &Expr, shape: &ScopeShape, diagnostics: &mut Vec<CheckDiagnostic>) {
    match expr {
        Expr::Lit(_) => {}
        Expr::Ref(reference) => check_ref(&reference.r#ref, shape, diagnostics),
        Expr::Fn(fx) => {
            let f = fx.r#fn;
            let expected = arity_of(f);
            if !expected.admits(fx.args.len()) {
                diagnostics.push(CheckDiagnostic::ArityMismatch {
                    function: f,
                    expected,
                    actual: fx.args.len(),
                });
            }
            match f {
                PureFn::JsonPath => {
                    check_lit_string(f, &fx.args, 1, LitPosition::JsonPathQuery, diagnostics);
                }
                PureFn::RegexMatch => {
                    check_lit_string(f, &fx.args, 1, LitPosition::RegexPattern, diagnostics);
                    if fx.args.len() >= 3 {
                        check_lit_string(f, &fx.args, 2, LitPosition::RegexFlags, diagnostics);
                    }
                }
                _ => {}
            }
            for arg in &fx.args {
                walk(arg, shape, diagnostics);
            }
        }
    }
}

fn check_ref(path: &RefPath, shape: &ScopeShape, diagnostics: &mut Vec<CheckDiagnostic>) {
    let s = path.as_str();
    // The RefPath grammar guarantees `root '.' name…`.
    let Some((root, rest)) = s.split_once('.') else {
        return;
    };
    match root {
        "steps" => {
            let step_id = rest.split('.').next().unwrap_or(rest);
            if !shape.steps.contains(step_id) {
                diagnostics.push(CheckDiagnostic::UndeclaredStep {
                    path: path.clone(),
                    step_id: step_id.to_string(),
                });
            }
        }
        "params" | "env" | "vars" | "iter" => {
            let (declared, scope_root) = match root {
                "params" => (&shape.params, ScopeRoot::Params),
                "env" => (&shape.env, ScopeRoot::Env),
                "vars" => (&shape.vars, ScopeRoot::Vars),
                _ => (&shape.iter, ScopeRoot::Iter),
            };
            let name = rest.split('.').next().unwrap_or(rest);
            if !declared.contains(name) {
                diagnostics.push(CheckDiagnostic::UndeclaredName {
                    path: path.clone(),
                    root: scope_root,
                    name: name.to_string(),
                });
            }
        }
        // Unreachable for a grammar-valid RefPath (the grammar rejects
        // out-of-scope roots, including reserved `secrets.*`).
        _ => {}
    }
}

/// Which literal-only position is being validated (drives the content
/// validation applied to the literal).
enum LitPosition {
    JsonPathQuery,
    RegexPattern,
    RegexFlags,
}

fn check_lit_string(
    f: PureFn,
    args: &[Expr],
    index: usize,
    position: LitPosition,
    diagnostics: &mut Vec<CheckDiagnostic>,
) {
    // Position absent entirely → already covered by the arity diagnostic.
    let Some(arg) = args.get(index) else {
        return;
    };
    let literal = match arg {
        Expr::Lit(lit) => &lit.lit,
        _ => {
            diagnostics.push(CheckDiagnostic::LitRequired {
                function: f,
                arg_index: index,
            });
            return;
        }
    };
    let Value::String(text) = literal else {
        diagnostics.push(CheckDiagnostic::LitNotString {
            function: f,
            arg_index: index,
            actual: JsonType::of(literal),
        });
        return;
    };
    match position {
        LitPosition::JsonPathQuery => {
            if let Err(err) = JsonPathQuery::parse(text) {
                diagnostics.push(CheckDiagnostic::InvalidJsonPathLiteral {
                    path: text.clone(),
                    message: err.to_string(),
                });
            }
        }
        LitPosition::RegexPattern => {
            if let Err(RegexIssue::BadPattern(message)) = build_regex(text, None) {
                diagnostics.push(CheckDiagnostic::InvalidRegexLiteral {
                    pattern: text.clone(),
                    message,
                });
            }
        }
        LitPosition::RegexFlags => {
            if !text
                .chars()
                .all(|flag| matches!(flag, 'i' | 'm' | 's' | 'x'))
            {
                diagnostics.push(CheckDiagnostic::InvalidRegexFlags {
                    flags: text.clone(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Arity;
    use serde_json::json;

    fn shape() -> ScopeShape {
        ScopeShape {
            params: ["ssid", "nested"].iter().map(ToString::to_string).collect(),
            env: ["deviceId"].iter().map(ToString::to_string).collect(),
            vars: ["label"].iter().map(ToString::to_string).collect(),
            iter: ["item"].iter().map(ToString::to_string).collect(),
            steps: ["wait_connected"].iter().map(ToString::to_string).collect(),
        }
    }

    fn r(path: &str) -> Expr {
        Expr::reference(RefPath::new(path).unwrap())
    }

    #[test]
    fn clean_expression_yields_no_diagnostics() {
        let expr = Expr::call(
            PureFn::And,
            vec![
                Expr::call(
                    PureFn::Eq,
                    vec![r("steps.wait_connected.verdict"), Expr::lit(json!("pass"))],
                ),
                Expr::call(
                    PureFn::RegexMatch,
                    vec![
                        r("params.ssid"),
                        Expr::lit(json!("^lab")),
                        Expr::lit(json!("i")),
                    ],
                ),
            ],
        );
        assert_eq!(check(&expr, &shape()), vec![]);
    }

    #[test]
    fn undeclared_names_and_steps_are_reported() {
        assert_eq!(
            check(&r("params.nope"), &shape()),
            vec![CheckDiagnostic::UndeclaredName {
                path: RefPath::new("params.nope").unwrap(),
                root: ScopeRoot::Params,
                name: "nope".to_string(),
            }]
        );
        assert_eq!(
            check(&r("steps.zz.output.matched"), &shape()),
            vec![CheckDiagnostic::UndeclaredStep {
                path: RefPath::new("steps.zz.output.matched").unwrap(),
                step_id: "zz".to_string(),
            }]
        );
        // Deep params path: only the first-level name is checked here.
        assert_eq!(check(&r("params.nested.a.b.c"), &shape()), vec![]);
    }

    #[test]
    fn arity_is_checked_and_walk_still_recurses() {
        let expr = Expr::call(PureFn::And, vec![r("vars.nope")]);
        assert_eq!(
            check(&expr, &shape()),
            vec![
                CheckDiagnostic::ArityMismatch {
                    function: PureFn::And,
                    expected: Arity::at_least(2),
                    actual: 1,
                },
                CheckDiagnostic::UndeclaredName {
                    path: RefPath::new("vars.nope").unwrap(),
                    root: ScopeRoot::Vars,
                    name: "nope".to_string(),
                },
            ]
        );
    }

    #[test]
    fn lit_only_positions_are_enforced() {
        let expr = Expr::call(PureFn::JsonPath, vec![r("params.nested"), r("params.ssid")]);
        assert_eq!(
            check(&expr, &shape()),
            vec![CheckDiagnostic::LitRequired {
                function: PureFn::JsonPath,
                arg_index: 1,
            }]
        );
        let expr = Expr::call(
            PureFn::RegexMatch,
            vec![
                Expr::lit(json!("a")),
                Expr::call(PureFn::Concat, vec![Expr::lit(json!("^a"))]),
            ],
        );
        assert_eq!(
            check(&expr, &shape()),
            vec![CheckDiagnostic::LitRequired {
                function: PureFn::RegexMatch,
                arg_index: 1,
            }]
        );
        let expr = Expr::call(
            PureFn::JsonPath,
            vec![r("params.nested"), Expr::lit(json!(5))],
        );
        assert_eq!(
            check(&expr, &shape()),
            vec![CheckDiagnostic::LitNotString {
                function: PureFn::JsonPath,
                arg_index: 1,
                actual: JsonType::Number,
            }]
        );
    }

    #[test]
    fn invalid_literals_are_reported() {
        let expr = Expr::call(
            PureFn::JsonPath,
            vec![r("params.nested"), Expr::lit(json!("$["))],
        );
        assert!(matches!(
            check(&expr, &shape()).as_slice(),
            [CheckDiagnostic::InvalidJsonPathLiteral { .. }]
        ));
        let expr = Expr::call(
            PureFn::RegexMatch,
            vec![Expr::lit(json!("a")), Expr::lit(json!("("))],
        );
        assert!(matches!(
            check(&expr, &shape()).as_slice(),
            [CheckDiagnostic::InvalidRegexLiteral { .. }]
        ));
        let expr = Expr::call(
            PureFn::RegexMatch,
            vec![
                Expr::lit(json!("a")),
                Expr::lit(json!("a")),
                Expr::lit(json!("gz")),
            ],
        );
        assert_eq!(
            check(&expr, &shape()),
            vec![CheckDiagnostic::InvalidRegexFlags {
                flags: "gz".to_string(),
            }]
        );
    }

    #[test]
    fn diagnostics_from_nested_arguments_are_collected() {
        let expr = Expr::call(
            PureFn::Concat,
            vec![
                Expr::lit(json!("a")),
                Expr::call(PureFn::Concat, vec![r("iter.nope")]),
            ],
        );
        assert_eq!(
            check(&expr, &shape()),
            vec![CheckDiagnostic::UndeclaredName {
                path: RefPath::new("iter.nope").unwrap(),
                root: ScopeRoot::Iter,
                name: "nope".to_string(),
            }]
        );
    }
}
