//! `OpenSessionOptions.endpoint` shapes for this provider (04 §9.1).
//!
//! The SPI keeps the endpoint as opaque JSON; this provider deserializes
//! the `{ spawn: SpawnSpec } | { attach: AttachSpec }` union. M1 implements
//! the spawn form only — Pointlock owns the daemon lifecycle; the attach
//! form (debug/shared-device scenarios) is rejected with a typed error
//! until the client-transport surface for it is wired up.

use std::collections::BTreeMap;

use pointlock_ir::ErrorClass;
use pointlock_provider_kit::{ProviderError, RetryableSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default daemon command, resolved through `PATH` (04 §9.1).
pub const DEFAULT_DAEMON_COMMAND: &str = "devicerail-daemon";

/// Default shutdown grace before the daemon child is killed (04 §9.1).
pub const DEFAULT_SHUTDOWN_GRACE_MS: u64 = 5_000;

/// The spawn endpoint form: Pointlock owns the daemon process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpawnSpec {
    /// Daemon command (default `devicerail-daemon`, `PATH`-resolved).
    #[serde(default = "default_command")]
    pub command: String,
    /// Daemon arguments (default empty: the daemon serves stdio NDJSON
    /// when started without arguments).
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment for the child.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory for the child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Grace period of the exit protocol (stdin EOF → wait → kill).
    #[serde(default = "default_shutdown_grace_ms")]
    pub shutdown_grace_ms: u64,
}

fn default_command() -> String {
    DEFAULT_DAEMON_COMMAND.to_owned()
}

fn default_shutdown_grace_ms() -> u64 {
    DEFAULT_SHUTDOWN_GRACE_MS
}

/// The endpoint union in its wire shape.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EndpointWire {
    #[serde(default)]
    spawn: Option<SpawnSpec>,
    #[serde(default)]
    attach: Option<Value>,
}

/// Parses the opaque SPI endpoint value into the M1-supported spawn form.
pub(crate) fn parse_spawn_endpoint(endpoint: &Value) -> Result<SpawnSpec, ProviderError> {
    let invalid = |detail: String| {
        ProviderError::new(
            ErrorClass::BindArgumentsInvalid,
            format!("invalid DeviceRail endpoint: {detail}"),
            RetryableSource::Classifier,
        )
    };
    let wire: EndpointWire =
        serde_json::from_value(endpoint.clone()).map_err(|error| invalid(error.to_string()))?;
    match (wire.spawn, wire.attach) {
        (Some(spawn), None) => Ok(spawn),
        (None, Some(_)) => Err(invalid(
            "the attach endpoint form is reserved for a later milestone; \
             M1 implements spawn only (04 §9.1)"
                .to_owned(),
        )),
        (Some(_), Some(_)) => Err(invalid(
            "endpoint must be exactly one of { spawn } | { attach }".to_owned(),
        )),
        (None, None) => Err(invalid(
            "endpoint must carry a { spawn: SpawnSpec } object".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spawn_endpoint_defaults_apply() {
        let spec = parse_spawn_endpoint(&json!({ "spawn": {} })).expect("parse");
        assert_eq!(spec.command, "devicerail-daemon");
        assert!(spec.args.is_empty());
        assert_eq!(spec.shutdown_grace_ms, 5_000);
    }

    #[test]
    fn spawn_endpoint_round_trips_explicit_fields() {
        let spec = parse_spawn_endpoint(&json!({
            "spawn": {
                "command": "/opt/devicerail/bin/devicerail-daemon",
                "args": ["--verbose"],
                "env": { "DEVICERAIL_ANDROID": "off" },
                "cwd": "/tmp/run",
                "shutdownGraceMs": 250
            }
        }))
        .expect("parse");
        assert_eq!(spec.command, "/opt/devicerail/bin/devicerail-daemon");
        assert_eq!(spec.args, ["--verbose"]);
        assert_eq!(spec.env["DEVICERAIL_ANDROID"], "off");
        assert_eq!(spec.cwd.as_deref(), Some("/tmp/run"));
        assert_eq!(spec.shutdown_grace_ms, 250);
    }

    #[test]
    fn attach_and_malformed_endpoints_are_rejected_typed() {
        for endpoint in [
            json!({ "attach": { "transport": "socket" } }),
            json!({}),
            json!({ "spawn": {}, "attach": {} }),
            json!({ "spwan": {} }),
        ] {
            let error = parse_spawn_endpoint(&endpoint).expect_err("rejected");
            assert_eq!(error.error_class, ErrorClass::BindArgumentsInvalid);
        }
    }
}
