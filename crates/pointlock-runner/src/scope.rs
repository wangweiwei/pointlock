//! Scope assembly for expression evaluation (spine §7 closed scope).
//!
//! The runner is the positional authority on what is bound into a
//! [`Scope`]: `params.*` from the run snapshot, `env.*` from the binding
//! (deviceId / platform / runId), `steps.<id>.output` / `.verdict` from
//! completed upstream steps, plus the self-output binding used by output
//! projection and assertion evaluation (02 §4.1.1). Shared by the live
//! engine and the offline re-judge (alignment) so both evaluate over the
//! same closed scope.

use std::collections::BTreeMap;

use pointlock_expr::Scope;
use pointlock_ir::VerdictStatus;
use serde_json::{Map, Value};

/// The run-constant part of every evaluation scope: params and env.
pub(crate) struct ScopeSeed {
    params: Map<String, Value>,
    env: Vec<(String, Value)>,
}

impl ScopeSeed {
    /// Builds the seed. `env` binds `deviceId`, `runId`, and `platform`
    /// when known (the platform comes from the assembly layer — the SPI
    /// attestation does not carry it).
    pub fn new(
        params: Map<String, Value>,
        device_id: &str,
        platform: Option<&str>,
        run_id: &str,
    ) -> Self {
        let mut env = vec![
            ("deviceId".to_owned(), Value::String(device_id.to_owned())),
            ("runId".to_owned(), Value::String(run_id.to_owned())),
        ];
        if let Some(platform) = platform {
            env.push(("platform".to_owned(), Value::String(platform.to_owned())));
        }
        ScopeSeed { params, env }
    }

    /// Materializes a [`Scope`]: seed + upstream step outputs/verdicts +
    /// an optional self-output binding (raw output for projection,
    /// projected output for assertions — 02 §4.1.1).
    pub fn scope(
        &self,
        outputs: &BTreeMap<String, Value>,
        verdicts: &BTreeMap<String, (VerdictStatus, bool)>,
        self_binding: Option<(&str, &Value)>,
    ) -> Scope {
        let mut scope = Scope::new();
        for (name, value) in &self.params {
            scope.set_param(name.clone(), value.clone());
        }
        for (name, value) in &self.env {
            scope.set_env(name.clone(), value.clone());
        }
        for (step_id, output) in outputs {
            scope.set_step_output(step_id.clone(), output.clone());
        }
        for (step_id, (status, _degraded)) in verdicts {
            let status = serde_json::to_value(status).expect("VerdictStatus serializes");
            scope.set_step_verdict(step_id.clone(), status);
        }
        if let Some((step_id, value)) = self_binding {
            scope.set_step_output(step_id.to_owned(), value.clone());
        }
        scope
    }
}
