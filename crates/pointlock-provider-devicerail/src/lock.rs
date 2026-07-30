//! Capability-lockfile production and live re-synthesis (spine §4.1;
//! 04 §9.2 attestation, 04 §10.2 `pointlock lock`).
//!
//! One synthesis function serves both sides of the digest comparison:
//! `pointlock lock` freezes the live world into a lockfile, and
//! `openSession` re-synthesizes the canonical lockfile form from the live
//! handshake + capabilities (identity fields — provider, `attestedAt` —
//! taken from the held baseline) and compares digests. Any drift in the
//! negotiated protocol, feature set, server identity, platform, or action
//! catalog changes the digest → `capability_drift`.
//!
//! Digest stability: `lockfile_digest` excludes the volatile `attestedAt`
//! timestamp from its domain (spine §4.1), so two lock runs against an
//! identical daemon produce byte-identical digests — 04 §10.2's
//! "double-run digest equality" criterion holds without pinning.

use std::collections::BTreeSet;

use devicerail_client::protocol::{
    ActionDefinition, DeviceInfo, FeatureOffer, HelloParams, HelloResult, PeerInfo as WirePeerInfo,
    ProtocolOffer, ProtocolRange,
};
use pointlock_ir::{ErrorClass, FeatureId};
use pointlock_provider_kit::lockfile::{
    CapabilityLockfile, LockfileDevice, LockfileHello, LockfileProvider, PeerInfo, ProtocolVersion,
};
use pointlock_provider_kit::manifest::ActionDefinitionStatic;
use pointlock_provider_kit::{ProviderError, RetryableSource};

use crate::convert::{action_definition_from_wire, platform_kind_from_wire};
use crate::manifest::{INFRA_REQUIRED_FEATURES, OFFERED_OPTIONAL_FEATURES, PROVIDER_NAME};

/// The `system.hello` request this provider always sends (04 §9.2):
/// protocol range pinned 1.5–1.5 (rationale at the offer site below);
/// `required` = the flow's `requiredFeatures` ∪
/// the provider-infrastructure three (★ rows); `optional` = the rest of
/// the known feature universe, so the negotiated `enabled` set — and with
/// it the lockfile digest — depends only on the daemon.
pub(crate) fn hello_params(extra_required: &[FeatureId]) -> HelloParams {
    let mut required: BTreeSet<String> = INFRA_REQUIRED_FEATURES
        .iter()
        .map(|feature| (*feature).to_owned())
        .collect();
    required.extend(
        extra_required
            .iter()
            .map(|feature| feature.as_str().to_owned()),
    );
    let optional: BTreeSet<String> = OFFERED_OPTIONAL_FEATURES
        .iter()
        .map(|feature| (*feature).to_owned())
        .filter(|feature| !required.contains(feature))
        .collect();
    HelloParams {
        client: WirePeerInfo {
            name: "pointlock-provider-devicerail".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        // 04 §9.2 pins the offer at minMinor 5: the semantic-action /
        // verdict / uiSnapshot contracts Pointlock depends on were
        // introduced in Protocol 1.5, so a lower minor MUST fail the
        // handshake here — offering 1.0 lets `pointlock lock` succeed
        // against a 1.2 daemon and freeze a lockfile that only fails
        // later, at compile bind (D2), far from the cause.
        protocol: ProtocolOffer::new(vec![ProtocolRange::new(1, 5, 5)]),
        features: FeatureOffer { required, optional },
    }
}

/// Transcribes the live `system.hello` outcome into the lockfile `hello`
/// block (`featuresEnabled` verbatim, in the wire set's sorted order).
fn lockfile_hello_from_live(hello: &HelloResult) -> Result<LockfileHello, ProviderError> {
    let features_enabled = hello
        .features
        .enabled
        .iter()
        .map(|feature| {
            FeatureId::new(feature.clone()).map_err(|error| {
                ProviderError::new(
                    ErrorClass::CapabilityDrift,
                    format!("negotiated feature id is outside the FeatureId grammar: {error}"),
                    RetryableSource::Classifier,
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(LockfileHello {
        protocol_selected: ProtocolVersion {
            major: u64::from(hello.protocol.selected.major),
            minor: u64::from(hello.protocol.selected.minor),
        },
        features_enabled,
        server: PeerInfo {
            name: hello.server.name.clone(),
            version: hello.server.version.clone(),
        },
    })
}

/// Builds a sealed [`CapabilityLockfile`] from one live handshake +
/// device-capabilities outcome. This is the `pointlock lock` freeze path
/// and — with `provider`/`attested_at` taken from the held baseline — the
/// attestation re-synthesis path.
pub fn make_lockfile(
    provider: LockfileProvider,
    attested_at: impl Into<String>,
    hello: &HelloResult,
    device: &DeviceInfo,
    actions: &[ActionDefinition],
) -> Result<CapabilityLockfile, ProviderError> {
    let actions = actions
        .iter()
        .map(action_definition_from_wire)
        .collect::<Result<Vec<ActionDefinitionStatic>, _>>()?;
    let mut lockfile = CapabilityLockfile {
        provider,
        attested_at: attested_at.into(),
        hello: lockfile_hello_from_live(hello)?,
        device: LockfileDevice {
            platform: platform_kind_from_wire(&device.platform)?,
            actions,
        },
        // Placeholder; sealed below.
        digest: pointlock_ir::Hash::new(format!("sha256:{}", "0".repeat(64)))
            .expect("placeholder digest is grammatical"),
    };
    lockfile.seal();
    Ok(lockfile)
}

/// The provider identity this crate stamps into fresh lockfiles.
pub fn lockfile_provider_identity() -> LockfileProvider {
    LockfileProvider {
        name: PROVIDER_NAME.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use devicerail_client::protocol::{
        ActionProtection as WireActionProtection, DeviceId, FeatureSelection, Platform,
        ProtocolSelection, ProtocolVersion as WireProtocolVersion, TransportInfo,
    };
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    fn live_hello(enabled: &[&str]) -> HelloResult {
        HelloResult {
            connection_id: Uuid::nil(),
            protocol: ProtocolSelection {
                selected: WireProtocolVersion::new(1, 5),
            },
            server: WirePeerInfo {
                name: "devicerail-daemon".to_owned(),
                version: "0.9.0".to_owned(),
            },
            transport: TransportInfo {
                kind: "stdio".to_owned(),
                framing: "ndjson".to_owned(),
            },
            features: FeatureSelection {
                enabled: enabled
                    .iter()
                    .map(|feature| (*feature).to_owned())
                    .collect(),
            },
        }
    }

    fn mock_device() -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::new("mock-1"),
            name: "DeviceRail Mock Device".to_owned(),
            platform: Platform::Mock,
            os_version: Some("0.1".to_owned()),
            connected: true,
        }
    }

    fn tap_action() -> ActionDefinition {
        ActionDefinition {
            name: "tap".to_owned(),
            description: "Tap a point".to_owned(),
            input_schema: json!({ "type": "object" }),
            protection: WireActionProtection::Standard,
        }
    }

    #[test]
    fn hello_offer_bundles_infra_required_and_the_feature_universe() {
        let params = hello_params(&[FeatureId::new("device.semanticActions.v1").unwrap()]);
        // The 04 §9.2 pin: 1.5 exactly — the contracts Pointlock needs
        // exist from 1.5, so an older daemon must fail the handshake
        // rather than freeze a lockfile that dies later at bind D2.
        assert_eq!(params.protocol.ranges, vec![ProtocolRange::new(1, 5, 5)]);
        for infra in INFRA_REQUIRED_FEATURES {
            assert!(params.features.required.contains(infra), "missing {infra}");
        }
        assert!(
            params
                .features
                .required
                .contains("device.semanticActions.v1")
        );
        // A feature moved into required leaves optional; the rest of the
        // universe stays offered so `enabled` is flow-independent.
        assert!(
            !params
                .features
                .optional
                .contains("device.semanticActions.v1")
        );
        assert!(
            params
                .features
                .optional
                .contains("observation.uiSnapshot.v1")
        );
        assert!(!params.features.optional.contains("action.protected.v1"));
    }

    #[test]
    fn make_lockfile_is_deterministic_for_the_same_live_world() {
        let hello = live_hello(&[
            "device.routing.v1",
            "events.snapshot.v1",
            "request.control.v1",
            "verdict.record.v1",
        ]);
        let first = make_lockfile(
            lockfile_provider_identity(),
            "2026-01-01T00:00:00Z",
            &hello,
            &mock_device(),
            &[tap_action()],
        )
        .expect("lockfile");
        let second = make_lockfile(
            lockfile_provider_identity(),
            "2026-01-01T00:00:00Z",
            &hello,
            &mock_device(),
            &[tap_action()],
        )
        .expect("lockfile");
        assert!(first.digest_consistent());
        assert_eq!(first.digest, second.digest);
        assert_eq!(
            first.hello.protocol_selected,
            ProtocolVersion { major: 1, minor: 5 }
        );
        // featuresEnabled verbatim, in the wire set's sorted order.
        assert_eq!(
            first
                .hello
                .features_enabled
                .iter()
                .map(FeatureId::as_str)
                .collect::<Vec<_>>(),
            [
                "device.routing.v1",
                "events.snapshot.v1",
                "request.control.v1",
                "verdict.record.v1"
            ]
        );
    }

    #[test]
    fn any_live_drift_changes_the_digest() {
        let baseline = make_lockfile(
            lockfile_provider_identity(),
            "2026-01-01T00:00:00Z",
            &live_hello(&["events.snapshot.v1"]),
            &mock_device(),
            &[tap_action()],
        )
        .expect("lockfile");

        // Feature drift.
        let drifted = make_lockfile(
            lockfile_provider_identity(),
            "2026-01-01T00:00:00Z",
            &live_hello(&["events.snapshot.v1", "media.stream.v1"]),
            &mock_device(),
            &[tap_action()],
        )
        .expect("lockfile");
        assert_ne!(baseline.digest, drifted.digest);

        // Action-schema drift.
        let mut changed_action = tap_action();
        changed_action.input_schema = json!({ "type": "object", "required": ["x"] });
        let drifted = make_lockfile(
            lockfile_provider_identity(),
            "2026-01-01T00:00:00Z",
            &live_hello(&["events.snapshot.v1"]),
            &mock_device(),
            &[changed_action],
        )
        .expect("lockfile");
        assert_ne!(baseline.digest, drifted.digest);
    }
}
