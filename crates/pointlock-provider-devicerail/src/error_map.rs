//! Error normalization: DeviceRail wire facts → Pointlock `ErrorClass`
//! (design doc 04 §6 / §9.6).
//!
//! Three error surfaces feed this module (04 §6.1):
//!
//! 1. `ErrorInfo` inside an action terminal — data, not an exception; the
//!    runner classifies it in `settling` via [`classify_wire_code`].
//! 2. JSON-RPC envelope errors (`ClientError::RemoteRpc`) — classified by
//!    [`classify_remote_rpc`], with the capability rows taking priority
//!    (04 §6.2 rules 1–2 sit above the open-set fallback).
//! 3. Client/transport errors (`devicerail-client` `ClientError`) — mapped
//!    by [`provider_error_from_client`]; the class-name column of the 04
//!    §9.6 table is replaced by the Rust client's variants per the R12 note
//!    (semantics, not spellings, drive the mapping).
//!
//! `retryableSource` discipline (04 §6.3): rows whose class is pinned by the
//! code identity itself record `classifier` (the retryability judgement is
//! Pointlock's table, whatever the daemon's flag says); rows decided by the
//! daemon's `ErrorInfo.retryable` declaration record `daemon`.

use devicerail_client::ClientError;
use devicerail_client::protocol::RpcError;
use pointlock_ir::ErrorClass;
use pointlock_provider_kit::{ProviderError, RetryableSource};

use crate::convert::error_info_from_wire;

/// The daemon's numeric JSON-RPC code for driver-layer failures of
/// `device.execute` (DeviceRail daemon `DRIVER_ERROR`). A driver failure is
/// appended to the session log as an `actionCompleted` `failed` terminal
/// before the RPC error is returned (durable-terminal shield), so this code
/// is the wire discriminator between "the action reached the driver and
/// failed" (a definite `failed` terminal) and "the request never entered
/// action execution" (a ProviderError).
pub const DRIVER_ERROR_RPC_CODE: i32 = -32000;

/// Maps one action-layer wire error code to the closed `ErrorClass`
/// (04 §6.2 / §9.6; the wire code set is open — unknown codes fall through
/// on the daemon's declared `retryable` bit).
pub fn classify_wire_code(code: &str, retryable: bool) -> (ErrorClass, RetryableSource) {
    match code {
        // Locate misses are final for the current attempt: the element is
        // genuinely not there (the act-chain advances or the step fails).
        "element_not_found" | "element_ambiguous" => {
            (ErrorClass::ActionFailedFinal, RetryableSource::Classifier)
        }
        // Staleness (documentEpoch expiry family): re-observe before retry.
        "element_stale" | "ui_context_changed" => {
            (ErrorClass::TargetStale, RetryableSource::Classifier)
        }
        // The daemon declares this retryable; the table mirrors the flag.
        "device_unavailable" => (ErrorClass::ActionFailedRetryable, RetryableSource::Daemon),
        "session_degraded" => (ErrorClass::SessionDegraded, RetryableSource::Classifier),
        // Local inputSchema validation should have caught this offline — a
        // compiler/expression bug signal, never retried.
        "invalid_arguments" => (
            ErrorClass::BindArgumentsInvalid,
            RetryableSource::Classifier,
        ),
        // `action_timeout` is the archived-terminal spelling; the daemon's
        // RPC envelope spells the same fact `action_timed_out` (action
        // scope) or `request_timed_out` (request scope) — one class.
        "action_timeout" | "action_timed_out" | "request_timed_out" => {
            (ErrorClass::ActionTimedOut, RetryableSource::Classifier)
        }
        // `action_cancelled` is the archived-terminal spelling;
        // `request_cancelled` is the RPC envelope spelling of the same fact.
        "action_cancelled" | "request_cancelled" => {
            (ErrorClass::ActionCancelled, RetryableSource::Classifier)
        }
        // Open set: trust the daemon's declaration for classification;
        // whether to actually retry stays the runner's RetryPolicy call.
        _ if retryable => (ErrorClass::ActionFailedRetryable, RetryableSource::Daemon),
        _ => (ErrorClass::ActionFailedFinal, RetryableSource::Daemon),
    }
}

/// Classifies a JSON-RPC envelope error (04 §6.2 priority rows 1–2 first,
/// then the action-layer table).
pub fn classify_remote_rpc(error: &RpcError) -> (ErrorClass, RetryableSource) {
    match error.data.code.as_str() {
        // Capability rows outrank everything (04 §6.2 rule 1): a feature
        // the handshake did not grant, or the daemon's own
        // semanticActions ⇒ uiSnapshot coupling check (04 §9.2).
        "feature_not_negotiated"
        | "required_feature_unsupported"
        | "semantic_snapshot_dependency_unsatisfied" => {
            (ErrorClass::CapabilityDrift, RetryableSource::Classifier)
        }
        // Request decode failures (daemon `invalid_params`) are the envelope
        // spelling of 04 §6.2 rule 2.
        "invalid_params" => (
            ErrorClass::BindArgumentsInvalid,
            RetryableSource::Classifier,
        ),
        code => classify_wire_code(code, error.data.retryable),
    }
}

/// Builds the unified [`ProviderError`] carrier from a `devicerail-client`
/// failure (04 §9.6 "client" rows; class names follow the Rust client crate
/// per the R12 note).
pub fn provider_error_from_client(error: ClientError, context: &str) -> ProviderError {
    let message = format!("{context}: {error}");
    match error {
        ClientError::RemoteRpc { error, .. } => {
            let (class, source) = classify_remote_rpc(&error);
            ProviderError::new(class, message, source)
                .with_wire(error_info_from_wire(&error.data))
                .with_client_code("remote_rpc_error")
        }
        ClientError::FeatureNotNegotiated { .. } => ProviderError::new(
            ErrorClass::CapabilityDrift,
            message,
            RetryableSource::Classifier,
        )
        .with_client_code("feature_not_negotiated"),
        // Connection-layer failures: the transport is gone or the protocol
        // stream is poisoned — either way the connection is untrustworthy.
        ClientError::Transport(_) | ClientError::Closed => ProviderError::new(
            ErrorClass::TransportLost,
            message,
            RetryableSource::Classifier,
        )
        .with_client_code("transport_closed"),
        ClientError::Framing(_) => ProviderError::new(
            ErrorClass::TransportLost,
            message,
            RetryableSource::Classifier,
        )
        .with_client_code("ndjson_frame_error"),
        ClientError::ProtocolViolation(_) => ProviderError::new(
            ErrorClass::TransportLost,
            message,
            RetryableSource::Classifier,
        )
        .with_client_code("protocol_violation"),
        // A response that fails strict deserialization means the protocol
        // stream can no longer be trusted (R12 serde discipline).
        ClientError::Serialization(_) => ProviderError::new(
            ErrorClass::TransportLost,
            message,
            RetryableSource::Classifier,
        )
        .with_client_code("serialization"),
        // Client-side backpressure: 04 §9.6 keeps this internal and only
        // promotes persistent overflow to transport_lost. M1 has no
        // provider-internal queueing layer, so overflow surfaces directly
        // as transport_lost (divergence documented in the crate docs).
        ClientError::PendingRequestLimit(_)
        | ClientError::AbandonedRequestLimit(_)
        | ClientError::WriteQueueFull { .. } => ProviderError::new(
            ErrorClass::TransportLost,
            message,
            RetryableSource::Classifier,
        )
        .with_client_code("write_queue_overflow"),
        ClientError::WriteFrameTooLarge { .. } => ProviderError::new(
            ErrorClass::TransportLost,
            message,
            RetryableSource::Classifier,
        )
        .with_client_code("write_frame_too_large"),
        // The client's terminal phases mean the connection is gone; any
        // other phase reported here is a provider sequencing bug (04 §9.6
        // `handshake_state` row): unrecoverable, never retried — the
        // conformance suite exists to extinguish these.
        ClientError::HandshakeState(state @ ("failed" | "closed" | "closing")) => {
            ProviderError::new(
                ErrorClass::TransportLost,
                message,
                RetryableSource::Classifier,
            )
            .with_client_code(format!("handshake_state:{state}"))
        }
        ClientError::HandshakeState(_) => ProviderError::new(
            ErrorClass::ActionFailedFinal,
            message,
            RetryableSource::Classifier,
        )
        .with_client_code("handshake_state"),
        ClientError::RuntimeUnavailable | ClientError::Internal(_) => ProviderError::new(
            ErrorClass::ActionFailedFinal,
            message,
            RetryableSource::Classifier,
        )
        .with_client_code("internal"),
    }
}

/// Extracts a definite four-way terminal from a `device.execute` RPC
/// failure, when the wire fact is one (04 §9.6).
///
/// The daemon returns non-`succeeded` terminals as RPC errors — the
/// success response only ever carries an `ActionResult` — while its
/// durable-terminal shield archives the matching `actionCompleted` event:
///
/// - `request_cancelled` / `action_cancelled` → a recorded `cancelled`
///   terminal;
/// - `request_timed_out` / `action_timed_out` / `action_timeout` → a
///   recorded `timedOut` terminal (both scopes finalize durably);
/// - any other error with the daemon's `DRIVER_ERROR` numeric code
///   ([`DRIVER_ERROR_RPC_CODE`]) → a recorded `failed` terminal, with the
///   driver's `ErrorInfo` verbatim.
///
/// `None` means the request never entered action execution (pre-dispatch
/// rejection: `session_required`, `invalid_params`,
/// `semantic_channel_unavailable`, …) — the caller falls back to
/// ProviderError classification.
pub fn execute_terminal_from_rpc(error: &RpcError) -> Option<pointlock_ir::ActionOutcome> {
    use pointlock_ir::ActionOutcome;
    let info = error_info_from_wire(&error.data);
    match error.data.code.as_str() {
        "request_cancelled" | "action_cancelled" => Some(ActionOutcome::Cancelled { error: info }),
        "request_timed_out" | "action_timed_out" | "action_timeout" => {
            Some(ActionOutcome::TimedOut { error: info })
        }
        // Raised before `actionStarted` is appended — not a terminal.
        "semantic_channel_unavailable" => None,
        _ if error.code == DRIVER_ERROR_RPC_CODE => Some(ActionOutcome::Failed { error: info }),
        _ => None,
    }
}

/// The immediate error for a call made with an already-cancelled token:
/// nothing was sent on the wire (04 §7.1).
pub(crate) fn cancelled_before_dispatch() -> ProviderError {
    ProviderError::new(
        ErrorClass::ActionCancelled,
        "cancellation token was already cancelled; no wire request was sent",
        RetryableSource::Classifier,
    )
}

/// The error every non-`health` method returns once the session has ended
/// or broken (04 §2.1).
pub(crate) fn session_gone(method: &str) -> ProviderError {
    ProviderError::new(
        ErrorClass::TransportLost,
        format!("provider session has ended; {method} is unavailable"),
        RetryableSource::Classifier,
    )
    .with_client_code("transport_closed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use devicerail_client::protocol::ErrorInfo as WireErrorInfo;

    fn wire_error(code: &str, numeric: i32, retryable: bool) -> RpcError {
        RpcError {
            code: numeric,
            message: format!("test {code}"),
            data: WireErrorInfo {
                code: code.to_owned(),
                message: format!("test {code}"),
                retryable,
                details: None,
            },
        }
    }

    #[test]
    fn action_layer_table_rows_map_exactly() {
        // Each row of the 04 §6 table, verbatim.
        let rows = [
            ("element_not_found", false, ErrorClass::ActionFailedFinal),
            ("element_ambiguous", false, ErrorClass::ActionFailedFinal),
            ("element_stale", true, ErrorClass::TargetStale),
            ("ui_context_changed", true, ErrorClass::TargetStale),
            (
                "device_unavailable",
                true,
                ErrorClass::ActionFailedRetryable,
            ),
            ("session_degraded", true, ErrorClass::SessionDegraded),
            ("invalid_arguments", false, ErrorClass::BindArgumentsInvalid),
            ("action_timeout", true, ErrorClass::ActionTimedOut),
            ("action_timed_out", true, ErrorClass::ActionTimedOut),
            ("request_timed_out", true, ErrorClass::ActionTimedOut),
            ("action_cancelled", false, ErrorClass::ActionCancelled),
            ("request_cancelled", true, ErrorClass::ActionCancelled),
        ];
        for (code, retryable, expected) in rows {
            let (class, _) = classify_wire_code(code, retryable);
            assert_eq!(class, expected, "row {code}");
        }
    }

    #[test]
    fn table_rows_pin_the_class_regardless_of_the_wire_flag() {
        // A daemon claiming element_not_found is retryable must not promote
        // the class: the code identity decides, and the judgement source is
        // recorded as classifier.
        let (class, source) = classify_wire_code("element_not_found", true);
        assert_eq!(class, ErrorClass::ActionFailedFinal);
        assert_eq!(source, RetryableSource::Classifier);
    }

    #[test]
    fn open_set_codes_follow_the_daemon_retryable_bit() {
        let (class, source) = classify_wire_code("device_not_connected", true);
        assert_eq!(class, ErrorClass::ActionFailedRetryable);
        assert_eq!(source, RetryableSource::Daemon);

        let (class, source) = classify_wire_code("unknown_action", false);
        assert_eq!(class, ErrorClass::ActionFailedFinal);
        assert_eq!(source, RetryableSource::Daemon);
    }

    #[test]
    fn capability_rows_outrank_the_open_set_in_envelope_errors() {
        for code in [
            "feature_not_negotiated",
            "required_feature_unsupported",
            "semantic_snapshot_dependency_unsatisfied",
        ] {
            let (class, _) = classify_remote_rpc(&wire_error(code, -32004, true));
            assert_eq!(class, ErrorClass::CapabilityDrift, "row {code}");
        }
        let (class, _) = classify_remote_rpc(&wire_error("invalid_params", -32602, false));
        assert_eq!(class, ErrorClass::BindArgumentsInvalid);
        let (class, _) = classify_remote_rpc(&wire_error("session_degraded", -32006, true));
        assert_eq!(class, ErrorClass::SessionDegraded);
    }

    #[test]
    fn remote_rpc_carrier_keeps_the_wire_error_and_client_code() {
        let error = ClientError::RemoteRpc {
            request_id: devicerail_client::protocol::RpcId::Number(1),
            error: Box::new(wire_error("device_unavailable", -32000, true)),
        };
        let carrier = provider_error_from_client(error, "device.execute");
        assert_eq!(carrier.error_class, ErrorClass::ActionFailedRetryable);
        assert_eq!(carrier.retryable_source, RetryableSource::Daemon);
        assert_eq!(carrier.client_code.as_deref(), Some("remote_rpc_error"));
        let wire = carrier.wire.expect("wire error attached");
        assert_eq!(wire.code, "device_unavailable");
        assert!(wire.retryable);
    }

    #[test]
    fn transport_family_maps_to_transport_lost() {
        for (error, code) in [
            (
                ClientError::Transport("pipe closed".to_owned()),
                "transport_closed",
            ),
            (ClientError::Closed, "transport_closed"),
            (
                ClientError::ProtocolViolation("bad frame".to_owned()),
                "protocol_violation",
            ),
            (
                ClientError::Framing(devicerail_client::FramingError::InvalidUtf8),
                "ndjson_frame_error",
            ),
            (
                ClientError::Serialization("strict decode failed".to_owned()),
                "serialization",
            ),
        ] {
            let carrier = provider_error_from_client(error, "ctx");
            assert_eq!(carrier.error_class, ErrorClass::TransportLost);
            assert_eq!(carrier.client_code.as_deref(), Some(code));
        }
    }

    #[test]
    fn feature_not_negotiated_is_capability_drift() {
        let error = ClientError::FeatureNotNegotiated {
            method: "ui.snapshot.get",
            feature: "observation.uiSnapshot.v1",
        };
        let carrier = provider_error_from_client(error, "ui.snapshot.get");
        assert_eq!(carrier.error_class, ErrorClass::CapabilityDrift);
        assert_eq!(
            carrier.client_code.as_deref(),
            Some("feature_not_negotiated")
        );
    }

    #[test]
    fn client_bug_family_is_final_but_terminal_phases_are_transport() {
        // closed/failed/closing phases mean the connection is gone.
        for phase in ["closed", "failed", "closing"] {
            let carrier = provider_error_from_client(ClientError::HandshakeState(phase), "ctx");
            assert_eq!(carrier.error_class, ErrorClass::TransportLost, "{phase}");
        }
        // Any other phase is a provider sequencing bug.
        let carrier = provider_error_from_client(ClientError::HandshakeState("ready"), "ctx");
        assert_eq!(carrier.error_class, ErrorClass::ActionFailedFinal);
        let carrier = provider_error_from_client(ClientError::Internal("bug".to_owned()), "ctx");
        assert_eq!(carrier.error_class, ErrorClass::ActionFailedFinal);
    }

    #[test]
    fn execute_rpc_failures_extract_definite_terminals() {
        use pointlock_ir::ActionOutcome;

        // Cancellation: both the envelope and the archived spelling.
        for code in ["request_cancelled", "action_cancelled"] {
            let outcome = execute_terminal_from_rpc(&wire_error(code, -32007, true))
                .expect("cancelled terminal");
            assert_eq!(outcome.kind(), "cancelled");
        }
        // Timeouts: request scope and action scope both finalize durably.
        for code in ["request_timed_out", "action_timed_out", "action_timeout"] {
            let outcome = execute_terminal_from_rpc(&wire_error(code, -32008, true))
                .expect("timedOut terminal");
            assert_eq!(outcome.kind(), "timedOut");
        }
        // Driver-layer failure (numeric DRIVER_ERROR): a recorded failed
        // terminal carrying the driver ErrorInfo verbatim.
        let outcome = execute_terminal_from_rpc(&wire_error(
            "element_not_found",
            DRIVER_ERROR_RPC_CODE,
            false,
        ))
        .expect("failed terminal");
        let ActionOutcome::Failed { error } = outcome else {
            panic!("expected failed terminal");
        };
        assert_eq!(error.code, "element_not_found");

        // Pre-dispatch rejections yield no terminal.
        assert!(execute_terminal_from_rpc(&wire_error("session_required", -32005, true)).is_none());
        assert!(
            execute_terminal_from_rpc(&wire_error(
                "semantic_channel_unavailable",
                DRIVER_ERROR_RPC_CODE,
                false
            ))
            .is_none()
        );
        assert!(execute_terminal_from_rpc(&wire_error("invalid_params", -32602, false)).is_none());
    }
}
