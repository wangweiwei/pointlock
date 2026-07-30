//! The closed evaluation scope (spine §7): `params.*` / `env.*` / `vars.*`
//! / `iter.<as>` / `steps.<id>.output.*` / `steps.<id>.verdict`.
//!
//! The value domain is `serde_json::Value`. `steps.<id>.verdict` resolves
//! to the upstream verdict *status* as a JSON string
//! (`"pass" | "fail" | "unknown"`). Verdict/VerdictStatus definition home
//! adjudicated to pointlock-ir (type truth source, R12); pending spine batch
//! incorporation — until that type lands, this crate stores the status as an
//! untyped JSON value supplied by the runner.
//!
//! Visibility (which upstream steps a given step may see, subflow hard
//! boundaries, self-ref legality per 02 §4.1.1) is *positional* knowledge:
//! the runner/compiler decides what to bind into a [`Scope`]; resolution
//! here is a pure lookup over whatever was bound.

use std::collections::{BTreeMap, BTreeSet};

use pointlock_ir::RefPath;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{EvalError, JsonType, ScopeRoot};

/// Per-step slice of the scope: the (projected) output and the verdict
/// status, either of which may not exist yet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepScope {
    /// `steps.<id>.output` — the projected output (02 §4.1.1: identity
    /// projection when `outputs` is absent). `None` when the step has
    /// produced no output.
    pub output: Option<Value>,
    /// `steps.<id>.verdict` — the verdict status as a JSON string.
    /// `None` when the step has produced no verdict.
    pub verdict: Option<Value>,
}

/// A concrete evaluation scope: the closed set of bindings visible to one
/// expression evaluation (spine §7). Built by the runner per evaluation
/// site; [`Scope::resolve`] answers [`RefPath`] lookups.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scope {
    params: Map<String, Value>,
    env: Map<String, Value>,
    vars: Map<String, Value>,
    iter: Map<String, Value>,
    steps: BTreeMap<String, StepScope>,
}

impl Scope {
    /// Creates an empty scope.
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds a run input (`params.<name>`).
    pub fn set_param(&mut self, name: impl Into<String>, value: Value) -> &mut Self {
        self.params.insert(name.into(), value);
        self
    }

    /// Binds an environment value (`env.<name>`).
    pub fn set_env(&mut self, name: impl Into<String>, value: Value) -> &mut Self {
        self.env.insert(name.into(), value);
        self
    }

    /// Binds a `let` product (`vars.<name>`). SSA single assignment is a
    /// compiler guarantee; the scope itself is last-write-wins.
    pub fn set_var(&mut self, name: impl Into<String>, value: Value) -> &mut Self {
        self.vars.insert(name.into(), value);
        self
    }

    /// Binds a `foreach` iteration value (`iter.<as>`).
    pub fn set_iter(&mut self, name: impl Into<String>, value: Value) -> &mut Self {
        self.iter.insert(name.into(), value);
        self
    }

    /// Binds an upstream step's (projected) output
    /// (`steps.<id>.output[.field]*`).
    pub fn set_step_output(&mut self, step_id: impl Into<String>, output: Value) -> &mut Self {
        self.steps.entry(step_id.into()).or_default().output = Some(output);
        self
    }

    /// Binds an upstream step's verdict status (`steps.<id>.verdict`),
    /// expected to be the status string `"pass" | "fail" | "unknown"`.
    pub fn set_step_verdict(&mut self, step_id: impl Into<String>, status: Value) -> &mut Self {
        self.steps.entry(step_id.into()).or_default().verdict = Some(status);
        self
    }

    /// Resolves a [`RefPath`] against this scope.
    ///
    /// Errors distinguish `missingRoot` (first name under the root is
    /// unbound), `missingPath` (a later segment is absent) and
    /// `notAnObject` (a mid-path value cannot be traversed). `unknownRoot`
    /// is defensive: the [`RefPath`] grammar admits only the five closed
    /// roots.
    pub fn resolve(&self, path: &RefPath) -> Result<&Value, EvalError> {
        let s = path.as_str();
        // The RefPath grammar guarantees at least `root '.' name`.
        let (root, rest) = s.split_once('.').unwrap_or((s, ""));
        match root {
            "params" | "env" | "vars" | "iter" => {
                let (map, scope_root) = match root {
                    "params" => (&self.params, ScopeRoot::Params),
                    "env" => (&self.env, ScopeRoot::Env),
                    "vars" => (&self.vars, ScopeRoot::Vars),
                    _ => (&self.iter, ScopeRoot::Iter),
                };
                let mut segments = rest.split('.');
                let name = segments.next().unwrap_or("");
                let base = map.get(name).ok_or_else(|| EvalError::MissingRoot {
                    path: path.clone(),
                    root: scope_root,
                    name: name.to_string(),
                })?;
                traverse(path, base, segments)
            }
            "steps" => {
                let (step_id, tail) = rest.split_once('.').unwrap_or((rest, ""));
                let entry = self
                    .steps
                    .get(step_id)
                    .ok_or_else(|| EvalError::MissingRoot {
                        path: path.clone(),
                        root: ScopeRoot::Steps,
                        name: step_id.to_string(),
                    })?;
                if tail == "verdict" {
                    return entry
                        .verdict
                        .as_ref()
                        .ok_or_else(|| EvalError::MissingPath {
                            path: path.clone(),
                            segment: "verdict".to_string(),
                        });
                }
                // Grammar guarantees tail is `output` or `output.<fields>`.
                let fields = match tail {
                    "output" => None,
                    _ => tail.strip_prefix("output."),
                };
                let output = entry
                    .output
                    .as_ref()
                    .ok_or_else(|| EvalError::MissingPath {
                        path: path.clone(),
                        segment: "output".to_string(),
                    })?;
                match (tail, fields) {
                    ("output", _) => Ok(output),
                    (_, Some(fields)) => traverse(path, output, fields.split('.')),
                    // Unreachable for a grammar-valid RefPath; kept typed.
                    (other, None) => Err(EvalError::MissingPath {
                        path: path.clone(),
                        segment: other.to_string(),
                    }),
                }
            }
            other => Err(EvalError::UnknownRoot {
                path: path.clone(),
                root: other.to_string(),
            }),
        }
    }

    /// Projects the declared-name shape of this scope, for the static
    /// [`check`](crate::check) entry point.
    pub fn shape(&self) -> ScopeShape {
        ScopeShape {
            params: self.params.keys().cloned().collect(),
            env: self.env.keys().cloned().collect(),
            vars: self.vars.keys().cloned().collect(),
            iter: self.iter.keys().cloned().collect(),
            steps: self.steps.keys().cloned().collect(),
        }
    }
}

/// Walks the remaining dotted identifier segments into a JSON value.
fn traverse<'a, 'b>(
    path: &RefPath,
    mut value: &'a Value,
    segments: impl Iterator<Item = &'b str>,
) -> Result<&'a Value, EvalError> {
    for segment in segments {
        match value {
            Value::Object(map) => {
                value = map.get(segment).ok_or_else(|| EvalError::MissingPath {
                    path: path.clone(),
                    segment: segment.to_string(),
                })?;
            }
            other => {
                return Err(EvalError::NotAnObject {
                    path: path.clone(),
                    segment: segment.to_string(),
                    actual: JsonType::of(other),
                });
            }
        }
    }
    Ok(value)
}

/// Declared-name shape of a scope: which first-level names exist under each
/// closed root, plus which step ids are visible. This is the input of the
/// static [`check`](crate::check) entry point (root/name legality only;
/// deep type inference belongs to the compiler `check` stage).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScopeShape {
    /// Declared param names.
    pub params: BTreeSet<String>,
    /// Declared environment names.
    pub env: BTreeSet<String>,
    /// Declared `let` product names.
    pub vars: BTreeSet<String>,
    /// Declared `foreach` iteration names (`as` bindings).
    pub iter: BTreeSet<String>,
    /// Visible step ids (author-level; synthesized `:` ids are never
    /// referenceable by grammar).
    pub steps: BTreeSet<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn path(s: &str) -> RefPath {
        RefPath::new(s).unwrap()
    }

    #[test]
    fn resolves_every_scope_root() {
        let scope = scope();
        assert_eq!(
            scope.resolve(&path("params.ssid")).unwrap(),
            &json!("lab-5g")
        );
        assert_eq!(
            scope.resolve(&path("env.deviceId")).unwrap(),
            &json!("device-1")
        );
        assert_eq!(scope.resolve(&path("vars.label")).unwrap(), &json!("run-1"));
        assert_eq!(scope.resolve(&path("iter.item")).unwrap(), &json!(2));
        assert_eq!(
            scope
                .resolve(&path("steps.wait_connected.verdict"))
                .unwrap(),
            &json!("pass")
        );
        assert_eq!(
            scope.resolve(&path("steps.wait_connected.output")).unwrap(),
            &json!({ "matched": true, "element": { "stableNodeId": "n1" } })
        );
        assert_eq!(
            scope
                .resolve(&path("steps.wait_connected.output.element.stableNodeId"))
                .unwrap(),
            &json!("n1")
        );
    }

    #[test]
    fn missing_root_vs_missing_path_vs_not_an_object() {
        let scope = scope();
        assert_eq!(
            scope.resolve(&path("params.nope")),
            Err(EvalError::MissingRoot {
                path: path("params.nope"),
                root: ScopeRoot::Params,
                name: "nope".to_string(),
            })
        );
        assert_eq!(
            scope.resolve(&path("steps.nope.output")),
            Err(EvalError::MissingRoot {
                path: path("steps.nope.output"),
                root: ScopeRoot::Steps,
                name: "nope".to_string(),
            })
        );
        assert_eq!(
            scope.resolve(&path("params.nested.a.zz")),
            Err(EvalError::MissingPath {
                path: path("params.nested.a.zz"),
                segment: "zz".to_string(),
            })
        );
        assert_eq!(
            scope.resolve(&path("params.ssid.x")),
            Err(EvalError::NotAnObject {
                path: path("params.ssid.x"),
                segment: "x".to_string(),
                actual: JsonType::String,
            })
        );
    }

    #[test]
    fn absent_output_and_verdict_are_missing_path() {
        let mut scope = Scope::new();
        scope.set_step_verdict("only_verdict", json!("unknown"));
        assert_eq!(
            scope.resolve(&path("steps.only_verdict.output")),
            Err(EvalError::MissingPath {
                path: path("steps.only_verdict.output"),
                segment: "output".to_string(),
            })
        );
        scope.set_step_output("only_output", json!({}));
        assert_eq!(
            scope.resolve(&path("steps.only_output.verdict")),
            Err(EvalError::MissingPath {
                path: path("steps.only_output.verdict"),
                segment: "verdict".to_string(),
            })
        );
    }

    #[test]
    fn shape_reflects_bindings() {
        let shape = scope().shape();
        assert!(shape.params.contains("ssid"));
        assert!(shape.params.contains("nested"));
        assert!(shape.env.contains("deviceId"));
        assert!(shape.vars.contains("label"));
        assert!(shape.iter.contains("item"));
        assert!(shape.steps.contains("wait_connected"));
    }
}
