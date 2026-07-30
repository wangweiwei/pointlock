//! Field-by-field transcription of DeviceRail wire DTOs into the shared
//! `pointlock-ir` runtime types (spine A.8 passthrough vocabulary; the wire
//! side is `devicerail-protocol`, strict serde per R12).
//!
//! Known narrowings of the IR-side types (both flagged pending spine
//! incorporation on the `pointlock-ir` definitions):
//!
//! - `pointlock_ir::UiSnapshotRef` keeps only the evidence asset; the wire
//!   `formatVersion` / `context` / `nodeCount` / `byteLength` fields are
//!   dropped in transit.
//! - `pointlock_ir::ActionOutcome::TimedOut` has no `timeoutMs` field; the
//!   wire value is dropped (the same budget is already known to the runner
//!   from `BoundActionCall.actionTimeoutMs`).

use std::collections::BTreeMap;

use devicerail_client::protocol as wire;
use pointlock_ir::{
    ActionExecution, ActionOutcome, ActionResult, AssetRef, CoordinateFallbackReason, ErrorClass,
    ErrorInfo, Observation, ScreenshotOmissionReason, UiContextKind, UiContextRef,
    UiSnapshotOmissionReason, UiSnapshotRef, Viewport,
};
use pointlock_provider_kit::manifest::{ActionDefinitionStatic, ActionProtection, PlatformKind};
use pointlock_provider_kit::{ProviderError, RetryableSource};

pub(crate) fn asset_ref_from_wire(asset: &wire::AssetRef) -> AssetRef {
    AssetRef {
        id: asset.id.clone(),
        media_type: asset.media_type.clone(),
        uri: asset.uri.clone(),
        sha256: asset.sha256.clone(),
    }
}

pub(crate) fn asset_ref_to_wire(asset: &AssetRef) -> wire::AssetRef {
    wire::AssetRef {
        id: asset.id.clone(),
        media_type: asset.media_type.clone(),
        uri: asset.uri.clone(),
        sha256: asset.sha256.clone(),
    }
}

pub(crate) fn error_info_from_wire(error: &wire::ErrorInfo) -> ErrorInfo {
    ErrorInfo {
        code: error.code.clone(),
        message: error.message.clone(),
        retryable: error.retryable,
        details: error.details.clone(),
    }
}

fn viewport_from_wire(viewport: &wire::Viewport) -> Viewport {
    Viewport {
        width: u64::from(viewport.width),
        height: u64::from(viewport.height),
        scale_factor: viewport.scale_factor,
    }
}

fn ui_context_kind_from_wire(kind: wire::UiContextKind) -> UiContextKind {
    match kind {
        wire::UiContextKind::Native => UiContextKind::Native,
        wire::UiContextKind::Web => UiContextKind::Web,
    }
}

fn ui_context_ref_from_wire(context: &wire::UiContextRef) -> UiContextRef {
    UiContextRef {
        context_kind: ui_context_kind_from_wire(context.context_kind),
        context_id: context.context_id.clone(),
        document_epoch: context.document_epoch.clone(),
    }
}

fn screenshot_omission_from_wire(
    reason: wire::ScreenshotOmissionReason,
) -> ScreenshotOmissionReason {
    match reason {
        wire::ScreenshotOmissionReason::Policy => ScreenshotOmissionReason::Policy,
        wire::ScreenshotOmissionReason::ProtectedAction => {
            ScreenshotOmissionReason::ProtectedAction
        }
    }
}

pub(crate) fn ui_snapshot_omission_from_wire(
    reason: wire::UiSnapshotOmissionReason,
) -> UiSnapshotOmissionReason {
    match reason {
        wire::UiSnapshotOmissionReason::DriverUnsupported => {
            UiSnapshotOmissionReason::DriverUnsupported
        }
        wire::UiSnapshotOmissionReason::Policy => UiSnapshotOmissionReason::Policy,
        wire::UiSnapshotOmissionReason::ProtectedAction => {
            UiSnapshotOmissionReason::ProtectedAction
        }
    }
}

pub(crate) fn observation_from_wire(observation: &wire::Observation) -> Observation {
    Observation {
        id: observation.id.to_string(),
        device_id: observation.device_id.to_string(),
        captured_at_ms: observation.captured_at_ms,
        viewport: viewport_from_wire(&observation.viewport),
        screenshot: observation.screenshot.as_ref().map(asset_ref_from_wire),
        screenshot_omission: observation
            .screenshot_omission
            .map(screenshot_omission_from_wire),
        ui_snapshot: observation
            .ui_snapshot
            .as_ref()
            .map(|snapshot| UiSnapshotRef {
                evidence: asset_ref_from_wire(&snapshot.evidence),
            }),
        ui_snapshot_omission: observation
            .ui_snapshot_omission
            .map(ui_snapshot_omission_from_wire),
        metadata: observation
            .metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn execution_from_wire(execution: &wire::ActionExecution) -> ActionExecution {
    match execution {
        wire::ActionExecution::NativeSemantic { context } => ActionExecution::NativeSemantic {
            context: ui_context_ref_from_wire(context),
        },
        wire::ActionExecution::WebSemantic { context } => ActionExecution::WebSemantic {
            context: ui_context_ref_from_wire(context),
        },
        wire::ActionExecution::CoordinateFallback {
            context,
            fallback_reason,
        } => ActionExecution::CoordinateFallback {
            context: ui_context_ref_from_wire(context),
            fallback_reason: match fallback_reason {
                wire::CoordinateFallbackReason::SemanticInteractionUnavailable => {
                    CoordinateFallbackReason::SemanticInteractionUnavailable
                }
                wire::CoordinateFallbackReason::PlatformLimitation => {
                    CoordinateFallbackReason::PlatformLimitation
                }
            },
        },
    }
}

pub(crate) fn action_result_from_wire(result: &wire::ActionResult) -> ActionResult {
    ActionResult {
        call_id: result.call_id.to_string(),
        started_at_ms: result.started_at_ms,
        finished_at_ms: result.finished_at_ms,
        output: result.output.clone(),
        before: result.before.as_ref().map(observation_from_wire),
        after: result.after.as_ref().map(observation_from_wire),
        evidence: result.evidence.iter().map(asset_ref_from_wire).collect(),
        execution: result.execution.as_ref().map(execution_from_wire),
    }
}

pub(crate) fn action_outcome_from_wire(outcome: &wire::ActionOutcome) -> ActionOutcome {
    match outcome {
        wire::ActionOutcome::Succeeded { result } => ActionOutcome::Succeeded {
            result: Box::new(action_result_from_wire(result)),
        },
        wire::ActionOutcome::Failed { error } => ActionOutcome::Failed {
            error: error_info_from_wire(error),
        },
        wire::ActionOutcome::Cancelled { error } => ActionOutcome::Cancelled {
            error: error_info_from_wire(error),
        },
        // The IR-side TimedOut variant has no timeoutMs field (module docs).
        wire::ActionOutcome::TimedOut { error, .. } => ActionOutcome::TimedOut {
            error: error_info_from_wire(error),
        },
    }
}

/// Maps the wire `Platform` discriminator onto the spine's closed
/// `PlatformKind`.
///
/// Type gap (reported, pending spine incorporation): `PlatformKind` has no
/// counterpart for the DeviceRail `mock` platform (and no open `other`
/// escape). Until the spine extends the enum, the mock platform maps
/// deterministically to `linux` — the same mapping is used by both
/// `pointlock lock` and openSession attestation, so lockfile digests stay
/// consistent; the substitution is confined to the digest domain and the
/// lockfile's informational `platform` field.
pub(crate) fn platform_kind_from_wire(
    platform: &wire::Platform,
) -> Result<PlatformKind, ProviderError> {
    match platform {
        wire::Platform::Web => Ok(PlatformKind::Web),
        wire::Platform::Android => Ok(PlatformKind::Android),
        wire::Platform::Ios => Ok(PlatformKind::Ios),
        wire::Platform::HarmonyOs => Ok(PlatformKind::HarmonyOs),
        wire::Platform::MacOs => Ok(PlatformKind::MacOs),
        wire::Platform::Windows => Ok(PlatformKind::Windows),
        wire::Platform::Linux => Ok(PlatformKind::Linux),
        wire::Platform::Rdp => Ok(PlatformKind::Rdp),
        // Provisional mapping; see the function docs.
        wire::Platform::Mock => Ok(PlatformKind::Linux),
        wire::Platform::Other(name) => Err(ProviderError::new(
            ErrorClass::CapabilityDrift,
            format!(
                "device platform `{name}` has no PlatformKind counterpart; \
                 the capability lockfile cannot represent it"
            ),
            RetryableSource::Classifier,
        )),
    }
}

/// Transcribes one wire `ActionDefinition` into the manifest/lockfile form.
///
/// Narrowing note: `ActionDefinitionStatic` has no `description` field, so
/// the wire description is dropped — two daemons differing only in action
/// descriptions produce the same lockfile digest (reported as a type gap).
pub(crate) fn action_definition_from_wire(
    action: &wire::ActionDefinition,
) -> Result<ActionDefinitionStatic, ProviderError> {
    let name = pointlock_ir::ActionName::new(action.name.clone()).map_err(|error| {
        ProviderError::new(
            ErrorClass::CapabilityDrift,
            format!("device action name is outside the ActionName grammar: {error}"),
            RetryableSource::Classifier,
        )
    })?;
    let input_schema =
        pointlock_ir::JsonSchemaDocument::new(action.input_schema.clone()).map_err(|error| {
            ProviderError::new(
                ErrorClass::CapabilityDrift,
                format!(
                    "device action `{}` carries a non-document inputSchema: {error}",
                    action.name
                ),
                RetryableSource::Classifier,
            )
        })?;
    Ok(ActionDefinitionStatic {
        name,
        input_schema,
        output_schema: None,
        protection: match action.protection {
            wire::ActionProtection::Standard => ActionProtection::Standard,
            wire::ActionProtection::Protected => ActionProtection::Protected,
        },
        // A driver action reported by `device.capabilities` is by
        // definition not provider-synthetic.
        synthetic: false,
    })
}

/// Current UTC time as an RFC 3339 second-resolution timestamp
/// (`YYYY-MM-DDThh:mm:ssZ`), without a date-time dependency: civil-date
/// conversion via the standard days-from-epoch algorithm.
pub(crate) fn now_utc_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    epoch_seconds_to_iso(secs)
}

fn epoch_seconds_to_iso(secs: u64) -> String {
    let days = secs / 86_400;
    let second_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        (second_of_day % 3600) / 60,
        second_of_day % 60,
    )
}

/// Howard Hinnant's `civil_from_days`: proleptic-Gregorian date from a day
/// count relative to 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u64, u64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn wire_observation() -> wire::Observation {
        wire::Observation {
            id: Uuid::nil(),
            device_id: wire::DeviceId::new("mock-1"),
            captured_at_ms: 42,
            viewport: wire::Viewport {
                width: 1280,
                height: 720,
                scale_factor: 2.0,
            },
            screenshot: Some(wire::AssetRef {
                id: "asset-1".to_owned(),
                media_type: "image/png".to_owned(),
                uri: "devicerail://assets/sha256/abc".to_owned(),
                sha256: Some("abc".to_owned()),
            }),
            screenshot_omission: None,
            ui_snapshot: None,
            ui_snapshot_omission: Some(wire::UiSnapshotOmissionReason::DriverUnsupported),
            metadata: serde_json::Map::from_iter([("frameSequence".to_owned(), json!(7))]),
        }
    }

    #[test]
    fn observation_transcribes_field_by_field() {
        let converted = observation_from_wire(&wire_observation());
        assert_eq!(converted.id, Uuid::nil().to_string());
        assert_eq!(converted.device_id, "mock-1");
        assert_eq!(converted.captured_at_ms, 42);
        assert_eq!(converted.viewport.width, 1280);
        assert_eq!(converted.viewport.scale_factor, 2.0);
        let screenshot = converted.screenshot.expect("screenshot mapped");
        assert_eq!(screenshot.uri, "devicerail://assets/sha256/abc");
        assert_eq!(screenshot.sha256.as_deref(), Some("abc"));
        assert_eq!(
            converted.ui_snapshot_omission,
            Some(UiSnapshotOmissionReason::DriverUnsupported)
        );
        assert_eq!(converted.metadata["frameSequence"], json!(7));
    }

    #[test]
    fn action_result_round_trips_through_the_ir_wire_shape() {
        let call_id = Uuid::new_v4();
        let context = wire::UiContextRef {
            context_kind: wire::UiContextKind::Native,
            context_id: "ctx-1".to_owned(),
            document_epoch: "epoch-1".to_owned(),
        };
        let result = wire::ActionResult {
            call_id,
            started_at_ms: 1,
            finished_at_ms: 2,
            output: json!({ "accepted": true }),
            before: None,
            after: Some(wire_observation()),
            evidence: vec![wire::AssetRef {
                id: "asset-2".to_owned(),
                media_type: "image/png".to_owned(),
                uri: "devicerail://assets/sha256/def".to_owned(),
                sha256: None,
            }],
            execution: Some(wire::ActionExecution::CoordinateFallback {
                context,
                fallback_reason: wire::CoordinateFallbackReason::PlatformLimitation,
            }),
        };
        let converted = action_result_from_wire(&result);
        assert_eq!(converted.call_id, call_id.to_string());
        assert_eq!(converted.output, json!({ "accepted": true }));
        assert_eq!(converted.evidence.len(), 1);
        assert!(converted.after.is_some());
        // Execution mode passes through untranslated (04 §3 post-condition;
        // R-degrade audit input).
        let wire_value = serde_json::to_value(converted.execution.expect("execution")).unwrap();
        assert_eq!(wire_value["mode"], "coordinateFallback");
        assert_eq!(wire_value["fallbackReason"], "platformLimitation");
        assert_eq!(wire_value["context"]["documentEpoch"], "epoch-1");
    }

    #[test]
    fn four_way_outcomes_transcribe_verbatim() {
        let error = wire::ErrorInfo {
            code: "action_timeout".to_owned(),
            message: "budget elapsed".to_owned(),
            retryable: true,
            details: Some(json!({ "scope": "action" })),
        };
        let timed_out = wire::ActionOutcome::TimedOut {
            error: error.clone(),
            timeout_ms: 10,
        };
        let converted = action_outcome_from_wire(&timed_out);
        assert_eq!(converted.kind(), "timedOut");
        let ActionOutcome::TimedOut { error: converted } = converted else {
            unreachable!();
        };
        assert_eq!(converted.code, "action_timeout");
        assert_eq!(converted.details, Some(json!({ "scope": "action" })));

        let cancelled = wire::ActionOutcome::Cancelled { error };
        assert_eq!(action_outcome_from_wire(&cancelled).kind(), "cancelled");
    }

    #[test]
    fn platform_mapping_covers_the_closed_kinds_and_flags_other() {
        assert_eq!(
            platform_kind_from_wire(&wire::Platform::Android).unwrap(),
            PlatformKind::Android
        );
        assert_eq!(
            platform_kind_from_wire(&wire::Platform::HarmonyOs).unwrap(),
            PlatformKind::HarmonyOs
        );
        // Provisional mock mapping (type gap; see function docs).
        assert_eq!(
            platform_kind_from_wire(&wire::Platform::Mock).unwrap(),
            PlatformKind::Linux
        );
        let error = platform_kind_from_wire(&wire::Platform::Other("fridge".to_owned()))
            .expect_err("open platform");
        assert_eq!(error.error_class, ErrorClass::CapabilityDrift);
    }

    #[test]
    fn action_definition_transcription_drops_description_only() {
        let action = wire::ActionDefinition {
            name: "tap".to_owned(),
            description: "Tap a point".to_owned(),
            input_schema: json!({ "type": "object" }),
            protection: wire::ActionProtection::Standard,
        };
        let converted = action_definition_from_wire(&action).expect("transcribes");
        assert_eq!(converted.name.as_str(), "tap");
        assert_eq!(
            serde_json::Value::from(converted.input_schema.clone()),
            json!({ "type": "object" })
        );
        assert_eq!(converted.protection, ActionProtection::Standard);
    }

    #[test]
    fn iso_timestamp_formatting_matches_known_vectors() {
        assert_eq!(epoch_seconds_to_iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(epoch_seconds_to_iso(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(epoch_seconds_to_iso(1_767_225_599), "2025-12-31T23:59:59Z");
    }
}
