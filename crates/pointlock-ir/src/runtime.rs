//! Runtime wire types shared between the runner and providers.
//!
//! Definition home adjudicated to `pointlock-ir` (type truth source, R12) so
//! that `pointlock-store` (which depends only on `ir`) can persist them
//! without violating the spine §1.2 dependency direction; pending spine
//! batch incorporation.
//!
//! Shapes follow spine §4.2 (Provider SPI signatures) and Appendix A.8
//! (DeviceRail passthrough vocabulary). DeviceRail-shaped types here are the
//! Pointlock-side projections the provider adapter maps wire payloads onto;
//! exact field-level alignment with the DeviceRail protocol schemas is
//! re-verified when `devicerail-client` lands (M1).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::vocab::{
    CoordinateFallbackReason, ScreenshotOmissionReason, UiContextKind, UiSnapshotOmissionReason,
    VerdictStatus,
};

/// Content-addressed evidence reference (DeviceRail `AssetRef`, spine A.8).
/// Never inline bytes; `sha256` is the bare hex digest when present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetRef {
    /// Provider-scoped asset id (e.g. `sha256:<digest>` or an opaque id).
    pub id: String,
    /// Media type, e.g. `image/png`.
    pub media_type: String,
    /// Provider URI, e.g. `devicerail://assets/sha256/<digest>`.
    pub uri: String,
    /// Bare lowercase hex sha256 digest of the content, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// A typed failed-localization record (2026-07-18 incorporation, item ③):
/// the declared asset plus why its bytes could not be localized. Gaps are
/// data, never silent omissions (principle 4/R4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceGap {
    /// The declared asset, verbatim.
    pub asset: AssetRef,
    /// Why localization failed.
    pub reason: String,
}

/// A localized evidence entry: the provider [`AssetRef`] plus the local
/// content-addressed copy (spine §6.6 — evidence is localized during
/// `observing` because provider-side retention is not guaranteed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceRef {
    /// The provider-side reference this entry was localized from.
    pub asset: AssetRef,
    /// Bare lowercase hex sha256 digest of the localized bytes.
    pub sha256: String,
    /// Path inside the store's content-addressed evidence area.
    pub local_path: String,
}

/// Viewport of an observation (DeviceRail `Viewport`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Viewport {
    /// Width in device-independent pixels.
    pub width: u64,
    /// Height in device-independent pixels.
    pub height: u64,
    /// Device scale factor.
    pub scale_factor: f64,
}

/// Full identity of one UI context (DeviceRail `UiContextRef`, spine A.8).
/// `documentEpoch` changes on navigation/reconnect and invalidates prior
/// node references.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiContextRef {
    /// Native accessibility or web document context.
    pub context_kind: UiContextKind,
    /// Provider-scoped context id.
    pub context_id: String,
    /// Epoch token; a mismatch means prior `UiNodeRef`s are stale.
    pub document_epoch: String,
}

/// Reference to one node of a captured UI snapshot (DeviceRail `UiNodeRef`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiNodeRef {
    /// The observation this node was captured in.
    pub observation_id: String,
    /// The UI context (carries the `documentEpoch` staleness token).
    pub context: UiContextRef,
    /// Stable node id inside the snapshot.
    pub stable_node_id: String,
}

/// Reference to the UI-tree evidence of an observation. Minimal shape
/// (the evidence asset); exact DeviceRail `UiSnapshotRef` field alignment is
/// re-verified in M1 — pending incorporation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSnapshotRef {
    /// The normalized UI-tree evidence asset.
    pub evidence: AssetRef,
}

/// A judgement-free point-in-time world snapshot (spine §2 concept 9,
/// DeviceRail `Observation` shape). Omissions are typed data, not errors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Observation {
    /// Provider-scoped observation id.
    pub id: String,
    /// Device the observation was captured on.
    pub device_id: String,
    /// Capture timestamp (ms since epoch).
    pub captured_at_ms: u64,
    /// Viewport at capture time.
    pub viewport: Viewport,
    /// Screenshot evidence, when captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<AssetRef>,
    /// Why the screenshot was legitimately omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_omission: Option<ScreenshotOmissionReason>,
    /// UI-tree evidence, when captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_snapshot: Option<UiSnapshotRef>,
    /// Why the UI snapshot was legitimately omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_snapshot_omission: Option<UiSnapshotOmissionReason>,
    /// Provider metadata passthrough.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

/// Structured, stable-coded error (DeviceRail `ErrorInfo`, spine A.8).
/// `retryable` is advisory metadata for the runner — providers never retry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorInfo {
    /// Stable error code (open string set on the wire; mapped to the closed
    /// `ErrorClass` by the provider adapter, spine §5).
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Whether the originator considers the failure retryable.
    pub retryable: bool,
    /// Sanitized structured details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// The execution mode the provider actually used (spine §4.2) — the key
/// input for unauthorized-degradation auditing (§6.4 R-degrade).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
pub enum ActionExecution {
    /// Native semantic channel.
    #[serde(rename_all = "camelCase")]
    NativeSemantic {
        /// The UI context the action ran in.
        context: UiContextRef,
    },
    /// Web semantic channel.
    #[serde(rename_all = "camelCase")]
    WebSemantic {
        /// The UI context the action ran in.
        context: UiContextRef,
    },
    /// Daemon-internal coordinate fallback (must be whitelisted by
    /// `acceptExecutionModes`, otherwise the verdict degrades).
    #[serde(rename_all = "camelCase")]
    CoordinateFallback {
        /// The UI context the action ran in.
        context: UiContextRef,
        /// Why the daemon fell back.
        fallback_reason: CoordinateFallbackReason,
    },
}

/// Result of a succeeded action (spine §4.2). Every action returns state
/// deltas: `before`/`after` observations plus evidence references.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionResult {
    /// The caller-generated action id (`device.execute` params.id).
    pub call_id: String,
    /// Dispatch timestamp (ms since epoch).
    pub started_at_ms: u64,
    /// Terminal timestamp (ms since epoch).
    pub finished_at_ms: u64,
    /// Action output (e.g. `findElement` → `{ element: UiNodeRef }`).
    pub output: Value,
    /// World snapshot before the action, when captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Observation>,
    /// World snapshot after the action, when captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Observation>,
    /// Evidence produced by the action.
    pub evidence: Vec<AssetRef>,
    /// The execution mode the provider reports having used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ActionExecution>,
}

/// Four-way terminal outcome of an action (spine §4.2; never folded,
/// never translated — `cancelled` and `timedOut` are recorded terminals).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "camelCase", deny_unknown_fields)]
pub enum ActionOutcome {
    /// The action completed and reported a result.
    #[serde(rename_all = "camelCase")]
    Succeeded {
        /// The action result (boxed: much larger than the error variants).
        result: Box<ActionResult>,
    },
    /// The action failed with a structured error.
    #[serde(rename_all = "camelCase")]
    Failed {
        /// The structured failure.
        error: ErrorInfo,
    },
    /// The action was cooperatively cancelled.
    #[serde(rename_all = "camelCase")]
    Cancelled {
        /// The structured cancellation record.
        error: ErrorInfo,
    },
    /// The action-scoped budget elapsed.
    #[serde(rename_all = "camelCase")]
    TimedOut {
        /// The structured timeout record.
        error: ErrorInfo,
    },
}

impl ActionOutcome {
    /// The wire discriminant of this outcome.
    pub fn kind(&self) -> &'static str {
        match self {
            ActionOutcome::Succeeded { .. } => "succeeded",
            ActionOutcome::Failed { .. } => "failed",
            ActionOutcome::Cancelled { .. } => "cancelled",
            ActionOutcome::TimedOut { .. } => "timedOut",
        }
    }
}

/// Fate of a hanging action intent after a crash (spine §4.2/§6.7-B).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "fate", rename_all = "camelCase", deny_unknown_fields)]
pub enum ReconcileResult {
    /// A terminal event for the callId is on record: adopt it. The archived
    /// terminal is the full four-way [`ActionOutcome`] — a recorded
    /// `failed`/`cancelled`/`timedOut` is a *certain* fate and comes back
    /// verbatim, never demoted to an uncertain branch.
    #[serde(rename_all = "camelCase")]
    Completed {
        /// The recorded terminal outcome (boxed: much larger than the
        /// other variants).
        outcome: Box<ActionOutcome>,
    },
    /// The issuing session's full event range shows no trace: safe replay.
    NeverDispatched,
    /// A start without a terminal (theoretically shielded against);
    /// treated as the uncertain branch.
    StartedNoTerminal,
    /// The issuing session's log is unreachable: the uncertain branch.
    #[serde(rename_all = "camelCase")]
    LogUnavailable {
        /// Why the log could not be read.
        reason: String,
    },
}

/// A folded step verdict (spine §2 concept 12). Append-only history:
/// re-judgement produces a *new* verdict with `supersedes` set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Verdict {
    /// Three-valued status (unknown never folds to pass).
    pub status: VerdictStatus,
    /// Whether the pass rests on a degraded verify channel or an
    /// unauthorized provider-internal fallback (spine §6.3/§6.4).
    pub degraded: bool,
    /// Human-readable folding summary (provider write path caps this at
    /// 16384 chars, spine §4.2).
    pub summary: String,
    /// Evidence citations (provider write path caps at 64, spine §4.2).
    pub evidence: Vec<AssetRef>,
    /// Id of the verdict this one supersedes, for re-judgement lineage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

/// The durable per-step verdict projection stored in `StepRecord`
/// (spine §6.6 — the folded summary/evidence live in the RunLog's
/// `verdictRecorded` payload; the record keeps the alignment-relevant core).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepVerdict {
    /// Three-valued status.
    pub status: VerdictStatus,
    /// Degradation flag (see [`Verdict::degraded`]).
    pub degraded: bool,
    /// Id of the superseded verdict, for re-judgement lineage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn action_outcome_wire_discriminants_round_trip() {
        let error = ErrorInfo {
            code: "action_timeout".to_owned(),
            message: "budget elapsed".to_owned(),
            retryable: true,
            details: None,
        };
        let timed_out = ActionOutcome::TimedOut {
            error: error.clone(),
        };
        let wire = serde_json::to_value(&timed_out).expect("serialize");
        assert_eq!(wire["outcome"], "timedOut");
        assert_eq!(wire["error"]["retryable"], true);
        let back: ActionOutcome = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, timed_out);
        assert_eq!(back.kind(), "timedOut");

        let cancelled = ActionOutcome::Cancelled { error };
        assert_eq!(
            serde_json::to_value(&cancelled).expect("serialize")["outcome"],
            "cancelled"
        );
    }

    #[test]
    fn reconcile_fates_round_trip() {
        let unavailable = ReconcileResult::LogUnavailable {
            reason: "session deleted".to_owned(),
        };
        let wire = serde_json::to_value(&unavailable).expect("serialize");
        assert_eq!(
            wire,
            json!({"fate": "logUnavailable", "reason": "session deleted"})
        );
        let never: ReconcileResult =
            serde_json::from_value(json!({"fate": "neverDispatched"})).expect("deserialize");
        assert_eq!(never, ReconcileResult::NeverDispatched);
    }

    #[test]
    fn reconcile_completed_carries_the_four_way_outcome_verbatim() {
        let completed = ReconcileResult::Completed {
            outcome: Box::new(ActionOutcome::Failed {
                error: ErrorInfo {
                    code: "device_unavailable".to_owned(),
                    message: "device went away".to_owned(),
                    retryable: true,
                    details: None,
                },
            }),
        };
        let wire = serde_json::to_value(&completed).expect("serialize");
        assert_eq!(wire["fate"], "completed");
        assert_eq!(wire["outcome"]["outcome"], "failed");
        assert_eq!(wire["outcome"]["error"]["retryable"], true);
        let back: ReconcileResult = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, completed);
    }

    #[test]
    fn observation_omissions_are_camel_case_and_absent_when_none() {
        let observation = Observation {
            id: "obs-1".to_owned(),
            device_id: "dev-1".to_owned(),
            captured_at_ms: 1,
            viewport: Viewport {
                width: 1080,
                height: 2400,
                scale_factor: 2.0,
            },
            screenshot: None,
            screenshot_omission: Some(ScreenshotOmissionReason::ProtectedAction),
            ui_snapshot: None,
            ui_snapshot_omission: Some(UiSnapshotOmissionReason::DriverUnsupported),
            metadata: BTreeMap::new(),
        };
        let wire = serde_json::to_value(&observation).expect("serialize");
        assert_eq!(wire["screenshotOmission"], "protectedAction");
        assert_eq!(wire["uiSnapshotOmission"], "driverUnsupported");
        assert!(wire.get("screenshot").is_none());
        assert!(wire.get("metadata").is_none());
        let back: Observation = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, observation);
    }

    #[test]
    fn execution_mode_fallback_reason_round_trips() {
        let execution = ActionExecution::CoordinateFallback {
            context: UiContextRef {
                context_kind: UiContextKind::Native,
                context_id: "ctx-1".to_owned(),
                document_epoch: "epoch-1".to_owned(),
            },
            fallback_reason: CoordinateFallbackReason::SemanticInteractionUnavailable,
        };
        let wire = serde_json::to_value(&execution).expect("serialize");
        assert_eq!(wire["mode"], "coordinateFallback");
        assert_eq!(wire["fallbackReason"], "semanticInteractionUnavailable");
        let back: ActionExecution = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, execution);
    }
}
