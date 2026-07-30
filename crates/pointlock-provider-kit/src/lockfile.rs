//! Capability lockfile and runtime attestation (spine §4.1).
//!
//! `pointlock lock` runs `system.hello` + `device.capabilities` against a
//! real daemon and freezes the result into a [`CapabilityLockfile`] (checked
//! into the repository like a dependency lockfile). At `openSession` the
//! provider replays the handshake and compares the live world against
//! `lockfileDigest`; any mismatch is `capability_drift` — refuse to run,
//! never silently degrade.

use std::collections::{BTreeMap, BTreeSet};

use pointlock_ir::{ActionName, FeatureId, Hash, domain_hash};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::manifest::{ActionDefinitionStatic, PlatformKind};

/// Domain tag of the lockfile digest, following the 02 §12.2 domain-hash
/// construction (`sha256(utf8(tag + "\n" + JCS(content)))`).
///
/// Pending spine incorporation: the spine fixes the digest as "sha256 of the
/// canonical form of the content" without naming the domain tag; this crate
/// pins it to `pointlock-lockfile/1`.
pub const LOCKFILE_DIGEST_DOMAIN_TAG: &str = "pointlock-lockfile/1";

/// Identity of the provider package the lockfile was produced by
/// (spine §4.1 `CapabilityLockfile.provider`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockfileProvider {
    /// Provider name (e.g. `"devicerail"`).
    pub name: String,
    /// Provider package version.
    pub version: String,
}

/// A negotiated protocol version (`{ major, minor }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolVersion {
    /// Major version.
    pub major: u64,
    /// Minor version.
    pub minor: u64,
}

/// Daemon identity (DeviceRail `PeerInfo`, spine A.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerInfo {
    /// Server name.
    pub name: String,
    /// Server version.
    pub version: String,
}

/// The frozen `system.hello` outcome (spine §4.1 `CapabilityLockfile.hello`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockfileHello {
    /// Negotiated protocol version (expected `{ major: 1, minor: 5 }`).
    pub protocol_selected: ProtocolVersion,
    /// `FeatureSelection.enabled`, verbatim.
    pub features_enabled: Vec<FeatureId>,
    /// Daemon `PeerInfo`.
    pub server: PeerInfo,
}

/// The frozen `device.capabilities` outcome (spine §4.1
/// `CapabilityLockfile.device`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockfileDevice {
    /// Platform of the locked device.
    pub platform: PlatformKind,
    /// The device's `ActionDefinition[]`, frozen verbatim.
    pub actions: Vec<ActionDefinitionStatic>,
}

/// The capability lockfile `pointlock lock` freezes after talking to a real
/// daemon (spine §4.1 `CapabilityLockfile`). Its `digest` is embedded into
/// `FlowIR.lockfileDigest` at compile time and re-checked by attestation at
/// every `openSession`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityLockfile {
    /// The provider package that produced this lockfile.
    pub provider: LockfileProvider,
    /// ISO timestamp of the lock run.
    pub attested_at: String,
    /// Frozen `system.hello` outcome.
    pub hello: LockfileHello,
    /// Frozen `device.capabilities` outcome.
    pub device: LockfileDevice,
    /// sha256 of the canonical form of the fields above (see
    /// [`lockfile_digest`]); embedded into `FlowIR.lockfileDigest`.
    pub digest: Hash,
}

impl CapabilityLockfile {
    /// Recomputes the digest from this lockfile's content and compares it to
    /// the stored `digest` field.
    pub fn digest_consistent(&self) -> bool {
        lockfile_digest(self) == self.digest
    }

    /// Overwrites `digest` with the digest recomputed from the content
    /// fields, sealing the lockfile.
    pub fn seal(&mut self) {
        self.digest = lockfile_digest(self);
    }
}

/// Computes the canonical digest of a lockfile's content — every field
/// except `digest` itself and the volatile `attestedAt` timestamp — via
/// [`pointlock_ir::domain_hash`] under [`LOCKFILE_DIGEST_DOMAIN_TAG`].
///
/// `attestedAt` is excluded so that re-locking an unchanged daemon yields
/// a byte-identical digest (04 §10.2 reproducibility): a timestamp must
/// never invalidate capability facts.
pub fn lockfile_digest(lockfile: &CapabilityLockfile) -> Hash {
    let mut content = serde_json::to_value(lockfile).expect("a lockfile serializes to JSON");
    let object = content
        .as_object_mut()
        .expect("a lockfile serializes to a JSON object");
    object.remove("digest");
    object.remove("attestedAt");
    domain_hash(LOCKFILE_DIGEST_DOMAIN_TAG, &content)
}

/// The runtime attestation result exposed on an open session (spine §4.2
/// `CapabilityAttestation`). `openSession` has already compared it against
/// the expected `lockfileDigest`; it is surfaced for Evidence and reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityAttestation {
    /// Provider identity.
    pub provider_id: String,
    /// Protocol version selected by the live handshake.
    pub protocol_selected: ProtocolVersion,
    /// Features enabled by the live handshake.
    pub features_enabled: BTreeSet<FeatureId>,
    /// Attested actions, keyed by action name.
    pub actions: BTreeMap<ActionName, ActionDefinitionStatic>,
    /// The lockfile digest the live world was verified against.
    pub lockfile_digest: Hash,
    /// ISO timestamp of the attestation.
    pub attested_at: String,
}

impl CapabilityAttestation {
    /// Builds the attestation view of a lockfile, as a provider does after a
    /// successful `openSession` comparison (`attested_at` is the live
    /// attestation time, not the lock time).
    pub fn from_lockfile(lockfile: &CapabilityLockfile, attested_at: impl Into<String>) -> Self {
        CapabilityAttestation {
            provider_id: lockfile.provider.name.clone(),
            protocol_selected: lockfile.hello.protocol_selected,
            features_enabled: lockfile.hello.features_enabled.iter().cloned().collect(),
            actions: lockfile
                .device
                .actions
                .iter()
                .map(|action| (action.name.clone(), action.clone()))
                .collect(),
            lockfile_digest: lockfile.digest.clone(),
            attested_at: attested_at.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ActionProtection;
    use pointlock_ir::JsonSchemaDocument;
    use serde_json::json;

    fn placeholder_hash() -> Hash {
        Hash::new(format!("sha256:{}", "0".repeat(64))).unwrap()
    }

    fn sample_lockfile() -> CapabilityLockfile {
        let mut lockfile = CapabilityLockfile {
            provider: LockfileProvider {
                name: "devicerail".to_owned(),
                version: "0.1.0".to_owned(),
            },
            attested_at: "2026-01-01T00:00:00Z".to_owned(),
            hello: LockfileHello {
                protocol_selected: ProtocolVersion { major: 1, minor: 5 },
                features_enabled: vec![
                    FeatureId::new("device.semanticActions.v1").unwrap(),
                    FeatureId::new("verdict.record.v1").unwrap(),
                ],
                server: PeerInfo {
                    name: "devicerail-daemon".to_owned(),
                    version: "1.5.0".to_owned(),
                },
            },
            device: LockfileDevice {
                platform: PlatformKind::Android,
                actions: vec![ActionDefinitionStatic {
                    name: ActionName::new("tapElement").unwrap(),
                    input_schema: JsonSchemaDocument::new(json!({ "type": "object" })).unwrap(),
                    output_schema: None,
                    protection: ActionProtection::Standard,
                    synthetic: false,
                }],
            },
            digest: placeholder_hash(),
        };
        lockfile.seal();
        lockfile
    }

    #[test]
    fn lockfile_digest_is_deterministic_and_excludes_digest_field() {
        let lockfile = sample_lockfile();
        assert!(lockfile.digest_consistent());
        assert_eq!(lockfile_digest(&lockfile), lockfile.digest);

        let mut tampered = lockfile.clone();
        tampered.hello.features_enabled.pop();
        assert!(!tampered.digest_consistent());

        // Changing only the digest field does not change the recomputed
        // digest (the digest hashes the content, not itself).
        let mut redigested = lockfile.clone();
        redigested.digest = placeholder_hash();
        assert_eq!(lockfile_digest(&redigested), lockfile.digest);

        // The volatile attestedAt timestamp is outside the digest domain:
        // re-locking an unchanged daemon is byte-identical (04 §10.2).
        let mut relocked = lockfile.clone();
        relocked.attested_at = "2027-01-01T00:00:00Z".to_owned();
        assert_eq!(lockfile_digest(&relocked), lockfile.digest);
    }

    #[test]
    fn lockfile_wire_shape_round_trips() {
        let lockfile = sample_lockfile();
        let wire = serde_json::to_value(&lockfile).expect("serialize");
        assert_eq!(wire["hello"]["protocolSelected"]["minor"], 5);
        assert_eq!(
            wire["hello"]["featuresEnabled"][0],
            "device.semanticActions.v1"
        );
        assert_eq!(wire["device"]["platform"], "android");
        assert_eq!(wire["attestedAt"], "2026-01-01T00:00:00Z");
        let back: CapabilityLockfile = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, lockfile);
    }

    #[test]
    fn attestation_projects_lockfile_and_round_trips() {
        let lockfile = sample_lockfile();
        let attestation = CapabilityAttestation::from_lockfile(&lockfile, "2026-01-02T00:00:00Z");
        assert_eq!(attestation.provider_id, "devicerail");
        assert_eq!(attestation.lockfile_digest, lockfile.digest);
        assert!(
            attestation
                .actions
                .contains_key(&ActionName::new("tapElement").unwrap())
        );
        let wire = serde_json::to_value(&attestation).expect("serialize");
        assert_eq!(wire["providerId"], "devicerail");
        assert_eq!(wire["actions"]["tapElement"]["protection"], "standard");
        let back: CapabilityAttestation = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, attestation);
    }
}
