//! The unified error carrier thrown by provider methods (04 §1, pending
//! spine incorporation).
//!
//! `execute()`'s four-way terminal outcome is **not** expressed through
//! [`ProviderError`] — `failed | cancelled | timedOut` are ordinary return
//! values (a definite thing happened in the world). `ProviderError` only
//! covers "the call itself could not obtain a terminal outcome": transport
//! rupture, handshake failure, protocol violation, envelope timeout. This
//! split is the watershed between the runner's `settling` and `onError`
//! paths.

use std::fmt;

use pointlock_ir::{ErrorClass, ErrorInfo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Where a `retryable` judgement came from (04 §6.3 audit requirement):
/// the daemon's own declaration, or the Pointlock classifier's fallback.
/// Recorded so a classifier's conservative guess is never mistaken for a
/// substrate fact.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum RetryableSource {
    /// The daemon declared retryability (`ErrorInfo.retryable`).
    Daemon,
    /// The Pointlock-side classifier assigned retryability as a fallback.
    Classifier,
}

/// Unified provider-method error carrier: wire fact and normalized
/// conclusion side by side, both recorded in the RunLog (04 §1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderError {
    /// Normalized conclusion (spine §5 closed enum).
    pub error_class: ErrorClass,
    /// Human-readable message.
    pub message: String,
    /// The daemon's original `{ code, message, retryable, details? }`, when
    /// the failure carried one (boxed: much larger than the other fields).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire: Option<Box<ErrorInfo>>,
    /// Client-side error code (e.g. `"transport_closed"`; TS reference
    /// implementation `@devicerail/client` `ClientErrorCode` — the Rust-side
    /// naming follows the `devicerail-client` crate when it lands, M1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_code: Option<String>,
    /// Where the retryable judgement came from.
    pub retryable_source: RetryableSource,
}

impl ProviderError {
    /// Constructs a carrier with neither wire error nor client code.
    pub fn new(
        error_class: ErrorClass,
        message: impl Into<String>,
        retryable_source: RetryableSource,
    ) -> Self {
        ProviderError {
            error_class,
            message: message.into(),
            wire: None,
            client_code: None,
            retryable_source,
        }
    }

    /// Attaches the daemon's original wire error.
    pub fn with_wire(mut self, wire: ErrorInfo) -> Self {
        self.wire = Some(Box::new(wire));
        self
    }

    /// Attaches a client-side error code.
    pub fn with_client_code(mut self, client_code: impl Into<String>) -> Self {
        self.client_code = Some(client_code.into());
        self
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Render the class in its wire (snake_case) spelling.
        let class = serde_json::to_value(self.error_class).expect("ErrorClass serializes");
        let class = class.as_str().expect("ErrorClass serializes to a string");
        write!(f, "provider error [{class}]: {}", self.message)
    }
}

impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_error_wire_shape() {
        let error = ProviderError::new(
            ErrorClass::TransportLost,
            "daemon exited",
            RetryableSource::Classifier,
        )
        .with_wire(ErrorInfo {
            code: "session_degraded".to_owned(),
            message: "adb bridge lost".to_owned(),
            retryable: true,
            details: None,
        })
        .with_client_code("transport_closed");

        let wire = serde_json::to_value(&error).expect("serialize");
        assert_eq!(wire["errorClass"], "transport_lost");
        assert_eq!(wire["retryableSource"], "classifier");
        assert_eq!(wire["clientCode"], "transport_closed");
        assert_eq!(wire["wire"]["code"], "session_degraded");
        let back: ProviderError = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, error);
    }

    #[test]
    fn provider_error_optional_fields_absent_when_none() {
        let error = ProviderError::new(
            ErrorClass::CapabilityDrift,
            "digest mismatch",
            RetryableSource::Classifier,
        );
        let wire = serde_json::to_value(&error).expect("serialize");
        assert_eq!(
            wire,
            json!({
                "errorClass": "capability_drift",
                "message": "digest mismatch",
                "retryableSource": "classifier",
            })
        );
    }

    #[test]
    fn provider_error_display_uses_wire_class_spelling() {
        let error = ProviderError::new(
            ErrorClass::ActionTimedOut,
            "budget elapsed",
            RetryableSource::Daemon,
        );
        assert_eq!(
            error.to_string(),
            "provider error [action_timed_out]: budget elapsed"
        );
    }
}
