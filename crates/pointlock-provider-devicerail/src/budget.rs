//! The three-level timeout budget, provider-owned parts (04 §9.7).
//!
//! - Request-envelope `timeoutMs` (wire, `request.control.v1`-gated): only
//!   `device.connect/disconnect/capabilities/observe/execute` and
//!   `media.stream.capture` accept it. For `execute` the provider derives
//!   it from the action budget (`actionTimeoutMs + margin`), converging
//!   timeouts onto the *definite* `timedOut` terminal side.
//! - Every other method has no envelope level; the fixed 15 000 ms default
//!   is a provider-local timer — on expiry the provider stops waiting
//!   locally (the response may still be in flight; mutating semantics are
//!   handled per method contract, 04 §9.7 last paragraph).

use std::time::Duration;

use devicerail_client::protocol::RequestTimeoutMs;
use devicerail_client::{CallOptions, ClientError};
use pointlock_ir::ErrorClass;
use pointlock_provider_kit::{ProviderError, RetryableSource};

use crate::error_map::provider_error_from_client;

/// Envelope margin E (04 §9.7): absorbs daemon scheduling and durable
/// terminal finalization, keeping `actionTimeoutMs < envelope timeoutMs`.
pub(crate) const ENVELOPE_MARGIN_MS: u64 = 5_000;

/// Fixed default budget for device operations without a caller-supplied
/// budget and for envelope-less methods (04 §9.7).
pub(crate) const DEFAULT_CALL_BUDGET_MS: u64 = 15_000;

/// Clamps a derived budget into the wire `RequestTimeoutMs` domain
/// (positive integer; 04 §9.7 invariant 3: floor and clamp).
pub(crate) fn clamp_timeout(ms: u64) -> RequestTimeoutMs {
    RequestTimeoutMs::new(ms.clamp(RequestTimeoutMs::MIN, RequestTimeoutMs::MAX))
        .expect("clamped value is inside the RequestTimeoutMs domain")
}

/// `CallOptions` carrying a request-envelope timeout.
pub(crate) fn envelope_options(ms: u64) -> CallOptions {
    CallOptions {
        timeout_ms: Some(clamp_timeout(ms)),
    }
}

/// Failure of a locally-budgeted call.
#[derive(Debug)]
pub(crate) enum BoundedError {
    /// The client call itself failed.
    Client(ClientError),
    /// The provider-local budget elapsed; the response may still arrive.
    Elapsed {
        /// The elapsed budget.
        budget_ms: u64,
    },
}

impl std::fmt::Display for BoundedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoundedError::Client(error) => error.fmt(f),
            BoundedError::Elapsed { budget_ms } => write!(
                f,
                "provider-local {budget_ms}ms budget elapsed while waiting for the response"
            ),
        }
    }
}

impl BoundedError {
    /// Normalizes into the unified carrier (04 §6). A local budget expiry
    /// is an uncertain outcome — the response may still be in flight — and
    /// is classed with the timeout family.
    pub(crate) fn into_provider_error(self, context: &str) -> ProviderError {
        match self {
            BoundedError::Client(error) => provider_error_from_client(error, context),
            BoundedError::Elapsed { budget_ms } => ProviderError::new(
                ErrorClass::ActionTimedOut,
                format!(
                    "{context}: provider-local {budget_ms}ms budget elapsed; the response may \
                     still be in flight (04 §9.7)"
                ),
                RetryableSource::Classifier,
            ),
        }
    }
}

/// Awaits `future` under the provider-local default budget.
pub(crate) async fn bounded<T>(
    future: impl Future<Output = Result<T, ClientError>>,
) -> Result<T, BoundedError> {
    match tokio::time::timeout(Duration::from_millis(DEFAULT_CALL_BUDGET_MS), future).await {
        Ok(result) => result.map_err(BoundedError::Client),
        Err(_) => Err(BoundedError::Elapsed {
            budget_ms: DEFAULT_CALL_BUDGET_MS,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_respects_the_wire_domain() {
        assert_eq!(clamp_timeout(0).get(), 1);
        assert_eq!(clamp_timeout(15_000).get(), 15_000);
        assert_eq!(clamp_timeout(u64::MAX).get(), RequestTimeoutMs::MAX);
    }

    #[tokio::test]
    async fn local_budget_expiry_is_classified_as_timed_out() {
        let hung = std::future::pending::<Result<(), ClientError>>();
        let short = tokio::time::timeout(Duration::from_millis(10), bounded(hung));
        // The outer 10ms timer fires first; emulate the elapsed branch
        // directly instead of waiting the full default budget.
        assert!(short.await.is_err());
        let error = BoundedError::Elapsed { budget_ms: 15_000 }.into_provider_error("events.list");
        assert_eq!(error.error_class, ErrorClass::ActionTimedOut);
        assert_eq!(error.retryable_source, RetryableSource::Classifier);
    }
}
