//! The Provider SPI (spine §4.2 runtime interface; 04 §1–§7 contract
//! clauses).
//!
//! The authoritative SPI form is this Rust trait pair (in-process, R12); a
//! stdio JSON-RPC sidecar adapter form is reserved for v0.2. Method names
//! are the snake_case renderings of the spine A.5 exhaustive list
//! (`openSession` → `open_session`, etc.); wire-facing DTO fields stay
//! camelCase.
//!
//! Cancellation: the TS signatures pass an optional `AbortSignal` to
//! `execute` / `observe`. The Rust counterpart is an optional
//! [`CancellationToken`] with the same contract (04 §7.1): on cancellation
//! the provider must forward the cancel intent to the substrate rather than
//! merely abandoning the wait; cancellation is a request, not a guarantee —
//! the provider returns whatever terminal actually materializes; a token
//! that is already cancelled at call time makes the method fail immediately
//! with an `action_cancelled`-class [`ProviderError`] without dispatching.

use std::pin::Pin;

use async_trait::async_trait;
use futures_core::Stream;
use pointlock_ir::{
    ActionName, ActionOutcome, AssetRef, ErrorClass, EventCursor, FeatureId, Hash, Observation,
    ReconcileResult, UiSnapshotOmissionReason, VerdictStatus,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use tokio_util::sync::CancellationToken;

use crate::error::{ProviderError, RetryableSource};
use crate::lockfile::CapabilityAttestation;
use crate::manifest::ProviderManifest;

/// Wire hard cap on `recordVerdict` summaries, in characters (spine §4.2).
/// Providers fail closed on oversize input; compaction is the runner's
/// report-assembly job (04 §5).
pub const VERDICT_SUMMARY_MAX_CHARS: usize = 16384;

/// Wire hard cap on `recordVerdict` evidence entries (spine §4.2).
pub const VERDICT_EVIDENCE_MAX_ENTRIES: usize = 64;

/// Options for [`Provider::open_session`] (spine §4.2
/// `OpenSessionOptions`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenSessionOptions {
    /// Provider-defined endpoint shape (`ProviderEndpoint`; devicerail:
    /// `{ spawn: SpawnSpec } | { attach: AttachSpec }`). Kept opaque JSON at
    /// the SPI layer — each provider deserializes its own shape.
    pub endpoint: Value,
    /// Device to bind. `devices.list` / `device.select` are
    /// connection-local: one `ProviderSession` owns one client instance
    /// exclusively, never shared (04 §1 rule 4).
    pub device_id: String,
    /// The IR's `requiredFeatures`, forwarded in full into
    /// `FeatureOffer.required` — protocol semantics fail the handshake when
    /// unmet (free enforcement, spine §4.1).
    pub required_features: Vec<FeatureId>,
    /// Attestation baseline: the digest the live world must match.
    pub lockfile_digest: Hash,
}

/// A bound, ready-to-dispatch action call (spine §4.2 `BoundActionCall`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoundActionCall {
    /// Runner-generated UUID; equals the substrate action id (devicerail:
    /// `device.execute` `params.id`) — the key to effectively-once and to
    /// crash-time [`ProviderSession::reconcile`]. Providers must use it
    /// verbatim, never regenerate or rewrite it (04 §3).
    pub call_id: String,
    /// Provider-native action name.
    pub action_name: ActionName,
    /// Runtime-evaluated arguments, already re-validated by the runner
    /// against the action's `inputSchema`.
    pub arguments: Value,
    /// Action-scoped budget (driver action body only; expiry yields a
    /// *definite* `timedOut` terminal, 04 §7.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_timeout_ms: Option<u64>,
    /// Request-envelope budget, distinct from the action budget (expiry
    /// yields an envelope error — outcome *uncertain* → reconcile, 04 §7.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout_ms: Option<u64>,
}

/// What an explicit observation should include (spine §4.2
/// `observe(req)` — `wants: ("screenshot" | "uiSnapshot")[]`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ObserveWant {
    /// A screenshot asset.
    Screenshot,
    /// A normalized UI-tree snapshot.
    UiSnapshot,
}

/// Request for [`ProviderSession::observe`]. `wants` is an intent
/// declaration, not a wire parameter (DeviceRail `device.observe` takes no
/// params): it tells the provider whether to chase a `ui.snapshot.get`
/// follow-up and to fill in truthful omission reasons when a wanted part is
/// missing (04 §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObserveRequest {
    /// The parts the caller needs.
    pub wants: Vec<ObserveWant>,
}

/// Result of dereferencing an observation's normalized UI tree (spine §4.2
/// `uiSnapshot`; TS: `{ ok: true, snapshot } | { ok: false, reason }`).
#[derive(Debug, Clone, PartialEq)]
pub enum UiSnapshotOutcome {
    /// `{ ok: true, snapshot }`. M0 narrow: the snapshot is the normalized
    /// UI tree as raw JSON — the typed `UiSnapshot` DTO follows the
    /// `devicerail-client` crate (M1, pending incorporation).
    Available {
        /// The normalized UI tree.
        snapshot: Value,
    },
    /// `{ ok: false, reason }` — a typed omission (data, not an error); it
    /// propagates to assertion `unknown` (04 §4.2).
    Unavailable {
        /// Why the snapshot cannot be read.
        reason: UiSnapshotOmissionReason,
    },
}

/// Verdict write payload for [`ProviderSession::record_verdict`]
/// (spine §4.2 — `{ status, summary, evidence }`; the daemon only validates
/// and persists, it runs no assertions).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerdictWrite {
    /// Three-valued status.
    pub status: VerdictStatus,
    /// Folding summary (wire cap: [`VERDICT_SUMMARY_MAX_CHARS`]).
    pub summary: String,
    /// Evidence citations (wire cap: [`VERDICT_EVIDENCE_MAX_ENTRIES`]).
    pub evidence: Vec<AssetRef>,
}

/// Session health probe result (spine §4.2 `health()`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionHealth {
    /// Whether the session is usable.
    pub ok: bool,
    /// Most recently observed degradation reason, when any (e.g. the wire
    /// `session_degraded` message).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
}

/// Terminal outcome of a session (DeviceRail `SessionOutcome`, spine A.8).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum SessionOutcome {
    /// The run finished normally.
    Completed,
    /// The run failed.
    Failed,
    /// The run was cancelled.
    Cancelled,
    /// Orderly shutdown (e.g. suspend).
    Shutdown,
}

/// Byte stream returned by [`ProviderSession::fetch_evidence`] (the Rust
/// counterpart of the TS `AsyncIterable<Uint8Array>`).
pub type EvidenceStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, ProviderError>> + Send>>;

/// A provider: a static capability declaration plus a session factory
/// (spine §4.2 `Provider`). Providers declare and faithfully execute; they
/// never fold, translate, improvise, retry, or degrade (04 §1).
#[async_trait]
pub trait Provider: Send + Sync {
    /// The package-shipped static capability declaration.
    fn manifest(&self) -> &ProviderManifest;

    /// Opens a session. Atomic (04 §2.1): returning means the connection is
    /// up, the protocol is negotiated with all `requiredFeatures` satisfied,
    /// the device is selected and connected, the substrate session is
    /// started, and attestation matched `lockfileDigest`. On any failure the
    /// provider cleans up partial resources and fails — never a half-open
    /// session. Failure classes: negotiation/capability mismatch →
    /// `capability_drift`; device unavailable → `action_failed_retryable`;
    /// connection failure → `transport_lost`.
    async fn open_session(
        &self,
        opts: OpenSessionOptions,
    ) -> Result<Box<dyn ProviderSession>, ProviderError>;
}

/// An open provider session (spine §4.2 `ProviderSession`). One session
/// owns one underlying client connection exclusively. In the broken state
/// every method except [`health`](Self::health) (which reports
/// `{ ok: false }`) fails with a `transport_lost`-class error (04 §2.1).
#[async_trait]
pub trait ProviderSession: Send + Sync {
    /// The attestation result (already verified inside `open_session`),
    /// exposed for Evidence and reports.
    fn attestation(&self) -> &CapabilityAttestation;

    /// Executes an action (devicerail: `device.execute`). The four-way
    /// terminal outcome is returned unfolded and untranslated — `failed`,
    /// `cancelled` and `timedOut` are `Ok` values, not errors. `Err` means
    /// "no terminal could be obtained" (transport rupture, envelope
    /// timeout, abort undeliverable); the runner records the attempt as
    /// hanging and goes through [`reconcile`](Self::reconcile) (04 §3).
    ///
    /// Preconditions the conformance suite enforces: `call.call_id` is used
    /// verbatim as the substrate action id; a repeated `call_id` is
    /// rejected (a retry is a new `call_id` + new WAL intent); an
    /// unattested `action_name` is rejected with `capability_drift` before
    /// any wire request.
    async fn execute(
        &self,
        call: BoundActionCall,
        cancel: Option<CancellationToken>,
    ) -> Result<ActionOutcome, ProviderError>;

    /// Takes an explicit observation (devicerail: `device.observe`).
    /// Omissions are data, not errors: a wanted-but-missing part comes back
    /// as a typed omission reason on the [`Observation`] (04 §4.1).
    async fn observe(
        &self,
        req: ObserveRequest,
        cancel: Option<CancellationToken>,
    ) -> Result<Observation, ProviderError>;

    /// Dereferences an observation's normalized UI tree (devicerail:
    /// `ui.snapshot.get { observationId }`; feature
    /// `observation.uiSnapshot.v1`). Protocol hard limit: readable only
    /// while the issuing session is active → the runner localizes
    /// immediately during `observing` (04 §4.2).
    async fn ui_snapshot(&self, observation_id: &str) -> Result<UiSnapshotOutcome, ProviderError>;

    /// Effect reconciliation of a pending intent (spine §6.7-B, 04 §5).
    /// `issuing` is the credential of the session generation that
    /// dispatched the intent (2026-07-18 incorporation: explicit
    /// parameter — `pendingIntent` issuing state, falling back to the
    /// checkpoint binding cursor). The implementation must consult the
    /// ISSUING session's log: when `issuing.sessionId` is not this
    /// session and the old log is unreachable, the honest answer is
    /// `logUnavailable` — never a scan of the current session's log
    /// (a reachable-but-wrong log can fabricate `neverDispatched`).
    async fn reconcile(
        &self,
        call_id: &str,
        issuing: &EventCursor,
    ) -> Result<ReconcileResult, ProviderError>;

    /// Fetches evidence bytes by `AssetRef.uri` for the local
    /// content-addressed store. When `asset.sha256` is present the provider
    /// must verify while reading and fail on mismatch — evidence integrity
    /// is non-negotiable (04 §4.3).
    async fn fetch_evidence(&self, asset: &AssetRef) -> Result<EvidenceStream, ProviderError>;

    /// Writes back a Pointlock-computed verdict (devicerail:
    /// `verdict.record`; feature `verdict.record.v1`). Wire hard caps
    /// ([`VERDICT_SUMMARY_MAX_CHARS`], [`VERDICT_EVIDENCE_MAX_ENTRIES`]):
    /// the provider fails closed on oversize input with a
    /// `bind_arguments_invalid`-class error (04 §5).
    async fn record_verdict(&self, verdict: VerdictWrite) -> Result<(), ProviderError>;

    /// The checkpoint event-cursor watermark: the highest sequence the
    /// provider has delivered to the runner (ack-after-persist), not the
    /// highest the daemon has produced (04 §5).
    async fn current_cursor(&self) -> Result<EventCursor, ProviderError>;

    /// Lightweight, side-effect-free health probe. Must not fail on a
    /// broken session — it reports `{ ok: false }` instead (04 §2.1).
    async fn health(&self) -> Result<SessionHealth, ProviderError>;

    /// Ends the session (devicerail: `session.end`). Idempotent: calling it
    /// on an already ended/broken session is a no-op; best-effort — it must
    /// not fail and block the runner's teardown when the transport is
    /// already gone (04 §2.1).
    async fn end(
        &self,
        outcome: SessionOutcome,
        reason: Option<String>,
    ) -> Result<(), ProviderError>;
}

// ─── provider-synthetic observation actions (04 §9.4.3) ─────────────────────
/// Wall clock in milliseconds since the epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The parts a provider-synthetic observation action asks for, or `None`
/// when the call is not one of them (04 §9.4.3).
///
/// `observe` declares its parts in `wants`; `screenshot` is the shortcut
/// whose parts are fixed here, which is why its action takes no arguments.
/// A malformed `wants` is a bind-argument error rather than a guess — the
/// runner already re-validated it against the advertised schema, so
/// reaching this is drift.
pub fn synthetic_observation_wants(
    call: &BoundActionCall,
) -> Result<Option<Vec<ObserveWant>>, ProviderError> {
    let invalid = |detail: String| {
        ProviderError::new(
            ErrorClass::BindArgumentsInvalid,
            detail,
            RetryableSource::Classifier,
        )
    };
    match call.action_name.as_str() {
        "screenshot" => Ok(Some(vec![ObserveWant::Screenshot])),
        "observe" => {
            let raw = call
                .arguments
                .get("wants")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| invalid("`observe` requires a `wants` array".to_owned()))?;
            let mut wants = Vec::with_capacity(raw.len());
            for part in raw {
                match part.as_str() {
                    Some("screenshot") => wants.push(ObserveWant::Screenshot),
                    Some("uiSnapshot") => wants.push(ObserveWant::UiSnapshot),
                    other => {
                        return Err(invalid(format!(
                            "`observe` wants an entry outside the closed vocabulary: {other:?}"
                        )));
                    }
                }
            }
            if wants.is_empty() {
                return Err(invalid("`observe` needs at least one part".to_owned()));
            }
            Ok(Some(wants))
        }
        _ => Ok(None),
    }
}

/// The output projection of an observation action (04 §9.4.3): the
/// observation's identity plus the parts that were captured, each with its
/// truthful omission reason when a wanted part is legitimately absent.
/// Omission is data, never an error (04 §4.1).
pub fn observation_projection(
    observation: &Observation,
    wants: &[ObserveWant],
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "observationId".to_owned(),
        serde_json::Value::String(observation.id.clone()),
    );
    if wants.contains(&ObserveWant::Screenshot) {
        if let Some(asset) = &observation.screenshot {
            out.insert(
                "screenshot".to_owned(),
                serde_json::to_value(asset).unwrap_or(serde_json::Value::Null),
            );
        }
        if let Some(reason) = observation.screenshot_omission {
            out.insert(
                "screenshotOmission".to_owned(),
                serde_json::to_value(reason).unwrap_or(serde_json::Value::Null),
            );
        }
    }
    if wants.contains(&ObserveWant::UiSnapshot) {
        if let Some(snapshot) = &observation.ui_snapshot {
            out.insert(
                "uiSnapshot".to_owned(),
                serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null),
            );
        }
        if let Some(reason) = observation.ui_snapshot_omission {
            out.insert(
                "uiSnapshotOmission".to_owned(),
                serde_json::to_value(reason).unwrap_or(serde_json::Value::Null),
            );
        }
    }
    serde_json::Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bound_action_call_wire_shape() {
        let call = BoundActionCall {
            call_id: "4a1f2c9e-0000-4000-8000-000000000001".to_owned(),
            action_name: ActionName::new("tapElement").unwrap(),
            arguments: json!({ "element": { "byText": "OK" } }),
            action_timeout_ms: Some(5000),
            request_timeout_ms: None,
        };
        let wire = serde_json::to_value(&call).expect("serialize");
        assert_eq!(wire["callId"], "4a1f2c9e-0000-4000-8000-000000000001");
        assert_eq!(wire["actionName"], "tapElement");
        assert_eq!(wire["actionTimeoutMs"], 5000);
        assert!(wire.get("requestTimeoutMs").is_none());
        let back: BoundActionCall = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, call);
    }

    #[test]
    fn observe_want_wire_literals() {
        assert_eq!(
            serde_json::to_value(ObserveWant::UiSnapshot).unwrap(),
            json!("uiSnapshot")
        );
        assert_eq!(
            serde_json::to_value(ObserveWant::Screenshot).unwrap(),
            json!("screenshot")
        );
    }

    #[test]
    fn session_outcome_wire_literals() {
        for (outcome, literal) in [
            (SessionOutcome::Completed, "completed"),
            (SessionOutcome::Failed, "failed"),
            (SessionOutcome::Cancelled, "cancelled"),
            (SessionOutcome::Shutdown, "shutdown"),
        ] {
            assert_eq!(serde_json::to_value(outcome).unwrap(), json!(literal));
        }
    }
}
