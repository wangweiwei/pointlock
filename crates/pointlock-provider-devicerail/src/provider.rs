//! `DeviceRailProvider`: the `Provider` implementation — static manifest
//! plus the atomic `openSession` sequence (04 §2.1 / §9.3):
//!
//! ```text
//! DeviceRailClient.spawn → system.hello → devices.list → device.select
//!   → device.connect → device.capabilities (→ attestation) → session.start
//! ```
//!
//! Returning from `open_session` means every step succeeded and the live
//! world matched `lockfileDigest`; on any failure the spawned daemon is
//! torn down before the error propagates — never a half-open session.

use std::collections::BTreeSet;
use std::time::Duration;

use async_trait::async_trait;
use devicerail_client::protocol::{ActionDefinition, DeviceId, DeviceInfo, DeviceSelectParams};
use devicerail_client::{CallOptions, DeviceRailClient, SpawnConfig, methods};
use pointlock_ir::ErrorClass;
use pointlock_provider_kit::lockfile::{CapabilityAttestation, CapabilityLockfile};
use pointlock_provider_kit::manifest::ProviderManifest;
use pointlock_provider_kit::{
    OpenSessionOptions, Provider, ProviderError, ProviderSession, RetryableSource,
};

use crate::budget::{DEFAULT_CALL_BUDGET_MS, bounded, envelope_options};
use crate::convert::now_utc_iso;
use crate::endpoint::{SpawnSpec, parse_spawn_endpoint};
use crate::lock::{hello_params, lockfile_provider_identity, make_lockfile};
use crate::manifest::{PROVIDER_NAME, devicerail_manifest};
use crate::session::DeviceRailSession;

/// The DeviceRail provider. Holds the capability lockfile the flow was
/// compiled against; `open_session` re-attests the live world against it.
#[derive(Debug, Clone)]
pub struct DeviceRailProvider {
    lockfile: CapabilityLockfile,
}

impl DeviceRailProvider {
    /// Creates a provider around the held lockfile, verifying its
    /// self-consistency (a tampered or hand-edited lockfile fails closed).
    pub fn new(lockfile: CapabilityLockfile) -> Result<Self, ProviderError> {
        if lockfile.provider.name != PROVIDER_NAME {
            return Err(ProviderError::new(
                ErrorClass::CapabilityDrift,
                format!(
                    "lockfile was produced by provider `{}`; this provider is `{PROVIDER_NAME}`",
                    lockfile.provider.name
                ),
                RetryableSource::Classifier,
            ));
        }
        if !lockfile.digest_consistent() {
            return Err(ProviderError::new(
                ErrorClass::CapabilityDrift,
                "lockfile digest is inconsistent with its content; re-run `pointlock lock`",
                RetryableSource::Classifier,
            ));
        }
        Ok(DeviceRailProvider { lockfile })
    }

    /// The held lockfile baseline.
    pub fn lockfile(&self) -> &CapabilityLockfile {
        &self.lockfile
    }

    async fn open_inner(
        &self,
        client: &DeviceRailClient,
        opts: &OpenSessionOptions,
    ) -> Result<DeviceRailSession, ProviderError> {
        let hello = client
            .negotiated_hello()
            .map_err(|error| crate::error_map::provider_error_from_client(error, "system.hello"))?;

        let (device, actions) = connect_device(client, &opts.device_id).await?;

        // Attestation (04 §9.2): re-synthesize the canonical lockfile form
        // from the live world (identity fields from the held baseline) and
        // compare digests. Any drift — negotiated protocol, feature set,
        // server identity, platform, action catalog — changes the digest.
        let live = make_lockfile(
            self.lockfile.provider.clone(),
            self.lockfile.attested_at.clone(),
            &hello,
            &device,
            &actions,
        )?;
        if live.digest != opts.lockfile_digest {
            return Err(ProviderError::new(
                ErrorClass::CapabilityDrift,
                format!(
                    "attestation mismatch: the live world's canonical digest {} does not match \
                     the expected lockfileDigest {}; {} — re-run `pointlock lock` and recompile",
                    live.digest,
                    opts.lockfile_digest,
                    describe_drift(&self.lockfile, &live)
                ),
                RetryableSource::Classifier,
            ));
        }

        // session.start (§9.3 ordering: after connect, before any action).
        let session = bounded(
            client.call::<methods::SessionStart>(methods::NoParams, CallOptions::default()),
        )
        .await
        .map_err(|error| error.into_provider_error("session.start"))?;

        let attestation = CapabilityAttestation::from_lockfile(&live, now_utc_iso());
        Ok(DeviceRailSession::new(
            client.clone(),
            attestation,
            session.id,
        ))
    }
}

#[async_trait]
impl Provider for DeviceRailProvider {
    fn manifest(&self) -> &ProviderManifest {
        devicerail_manifest()
    }

    async fn open_session(
        &self,
        opts: OpenSessionOptions,
    ) -> Result<Box<dyn ProviderSession>, ProviderError> {
        // The IR must have been compiled against the very lockfile this
        // provider holds; otherwise the baseline for attestation is wrong.
        if opts.lockfile_digest != self.lockfile.digest {
            return Err(ProviderError::new(
                ErrorClass::CapabilityDrift,
                format!(
                    "the IR's lockfileDigest {} does not match the held lockfile digest {}; \
                     recompile against the current lockfile",
                    opts.lockfile_digest, self.lockfile.digest
                ),
                RetryableSource::Classifier,
            ));
        }
        let spawn = parse_spawn_endpoint(&opts.endpoint)?;
        let client = spawn_client(&spawn, &opts.required_features).await?;
        match self.open_inner(&client, &opts).await {
            Ok(session) => Ok(Box::new(session)),
            Err(error) => {
                // Atomicity (04 §2.1): clean up the spawned daemon; never
                // return a half-open session.
                let _ = client.close().await;
                Err(error)
            }
        }
    }
}

/// Spawns the daemon and negotiates `system.hello` (04 §9.1 / §9.2). The
/// required set is the flow's `requiredFeatures` plus the provider
/// infrastructure three; protocol semantics fail the handshake when any
/// required feature is unmet (free enforcement, spine §4.1).
async fn spawn_client(
    spawn: &SpawnSpec,
    required_features: &[pointlock_ir::FeatureId],
) -> Result<DeviceRailClient, ProviderError> {
    let mut config = SpawnConfig::new(&spawn.command, hello_params(required_features));
    config = config.args(spawn.args.iter());
    for (key, value) in &spawn.env {
        config = config.env(key, value);
    }
    if let Some(cwd) = &spawn.cwd {
        config = config.cwd(cwd);
    }
    // Exit-protocol grace (04 §9.1): stdin EOF → wait → kill. The client
    // collapses the SIGTERM step into its kill path (documented divergence).
    config.client.close_grace = Duration::from_millis(spawn.shutdown_grace_ms.max(1));
    DeviceRailClient::spawn(config).await.map_err(|error| {
        crate::error_map::provider_error_from_client(error, "spawn daemon / system.hello")
    })
}

/// The device-binding leg of §9.3: devices.list (verify the requested
/// device exists) → device.select → device.connect → device.capabilities.
/// Explicit selection only — the protocol's single-device lazy routing is
/// deliberately not used (04 §9.2: implicit routing contradicts
/// attestation determinism).
async fn connect_device(
    client: &DeviceRailClient,
    device_id: &str,
) -> Result<(DeviceInfo, Vec<ActionDefinition>), ProviderError> {
    let devices =
        bounded(client.call::<methods::DevicesList>(methods::NoParams, CallOptions::default()))
            .await
            .map_err(|error| error.into_provider_error("devices.list"))?;
    let wanted = DeviceId::new(device_id);
    if !devices.devices.iter().any(|device| device.id == wanted) {
        let known: Vec<String> = devices
            .devices
            .iter()
            .map(|device| device.id.to_string())
            .collect();
        // Device unavailable → retryable (04 §2.1 failure classes).
        return Err(ProviderError::new(
            ErrorClass::ActionFailedRetryable,
            format!("device `{device_id}` is not present in devices.list (known: {known:?})"),
            RetryableSource::Classifier,
        ));
    }
    bounded(client.call::<methods::DeviceSelect>(
        DeviceSelectParams { device_id: wanted },
        CallOptions::default(),
    ))
    .await
    .map_err(|error| error.into_provider_error("device.select"))?;
    let device = bounded(client.call::<methods::DeviceConnect>(
        methods::NoParams,
        envelope_options(DEFAULT_CALL_BUDGET_MS),
    ))
    .await
    .map_err(|error| error.into_provider_error("device.connect"))?;
    let actions = bounded(client.call::<methods::DeviceCapabilities>(
        methods::NoParams,
        envelope_options(DEFAULT_CALL_BUDGET_MS),
    ))
    .await
    .map_err(|error| error.into_provider_error("device.capabilities"))?;
    Ok((device, actions))
}

/// Runs the `pointlock lock` wire sequence against a freshly spawned daemon
/// and freezes the outcome into a sealed [`CapabilityLockfile`]
/// (04 §10.2; CLI wiring lands with `pointlock lock`).
pub async fn lock_via_spawn(
    spawn: &SpawnSpec,
    device_id: &str,
) -> Result<CapabilityLockfile, ProviderError> {
    lock_via_spawn_at(spawn, device_id, now_utc_iso()).await
}

/// [`lock_via_spawn`] with a caller-pinned `attestedAt`. The digest domain
/// already excludes `attestedAt` (spine §4.1, 04 §10.2), so pinning is
/// never needed for digest stability — it remains useful only for
/// byte-identical lockfile artifacts / pinned provenance in fixtures.
pub async fn lock_via_spawn_at(
    spawn: &SpawnSpec,
    device_id: &str,
    attested_at: impl Into<String>,
) -> Result<CapabilityLockfile, ProviderError> {
    let client = spawn_client(spawn, &[]).await?;
    let result = async {
        let hello = client
            .negotiated_hello()
            .map_err(|error| crate::error_map::provider_error_from_client(error, "system.hello"))?;
        let (device, actions) = connect_device(&client, device_id).await?;
        make_lockfile(
            lockfile_provider_identity(),
            attested_at,
            &hello,
            &device,
            &actions,
        )
    }
    .await;
    let _ = client.close().await;
    result
}

/// Human-readable drift summary for the attestation failure report
/// (04 §9.2: list what appeared, what vanished, what changed).
fn describe_drift(expected: &CapabilityLockfile, live: &CapabilityLockfile) -> String {
    let mut notes: Vec<String> = Vec::new();
    if expected.hello.protocol_selected != live.hello.protocol_selected {
        notes.push(format!(
            "protocol {}.{} -> {}.{}",
            expected.hello.protocol_selected.major,
            expected.hello.protocol_selected.minor,
            live.hello.protocol_selected.major,
            live.hello.protocol_selected.minor,
        ));
    }
    if expected.hello.server != live.hello.server {
        notes.push(format!(
            "server {}@{} -> {}@{}",
            expected.hello.server.name,
            expected.hello.server.version,
            live.hello.server.name,
            live.hello.server.version,
        ));
    }
    let expected_features: BTreeSet<&str> = expected
        .hello
        .features_enabled
        .iter()
        .map(|feature| feature.as_str())
        .collect();
    let live_features: BTreeSet<&str> = live
        .hello
        .features_enabled
        .iter()
        .map(|feature| feature.as_str())
        .collect();
    let missing: Vec<&&str> = expected_features.difference(&live_features).collect();
    if !missing.is_empty() {
        notes.push(format!("features lost: {missing:?}"));
    }
    let gained: Vec<&&str> = live_features.difference(&expected_features).collect();
    if !gained.is_empty() {
        notes.push(format!("features gained: {gained:?}"));
    }
    if expected.device.platform != live.device.platform {
        notes.push(format!(
            "platform {:?} -> {:?}",
            expected.device.platform, live.device.platform
        ));
    }
    let expected_actions: BTreeSet<&str> = expected
        .device
        .actions
        .iter()
        .map(|action| action.name.as_str())
        .collect();
    let live_actions: BTreeSet<&str> = live
        .device
        .actions
        .iter()
        .map(|action| action.name.as_str())
        .collect();
    let vanished: Vec<&&str> = expected_actions.difference(&live_actions).collect();
    if !vanished.is_empty() {
        notes.push(format!("actions vanished: {vanished:?}"));
    }
    let appeared: Vec<&&str> = live_actions.difference(&expected_actions).collect();
    if !appeared.is_empty() {
        notes.push(format!("actions appeared: {appeared:?}"));
    }
    let changed: Vec<&str> = expected
        .device
        .actions
        .iter()
        .filter_map(|action| {
            live.device
                .actions
                .iter()
                .find(|live_action| live_action.name == action.name)
                .filter(|live_action| *live_action != action)
                .map(|_| action.name.as_str())
        })
        .collect();
    if !changed.is_empty() {
        notes.push(format!("action definitions changed: {changed:?}"));
    }
    if notes.is_empty() {
        // The content diff is invisible at this granularity (e.g. only the
        // action order changed); the digests still disagree.
        "no field-level diff detected at summary granularity".to_owned()
    } else {
        format!("drift: {}", notes.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use pointlock_ir::{FeatureId, Hash};
    use pointlock_provider_kit::lockfile::{
        LockfileDevice, LockfileHello, LockfileProvider, PeerInfo, ProtocolVersion,
    };
    use pointlock_provider_kit::manifest::PlatformKind;

    use super::*;

    fn sample_lockfile(name: &str) -> CapabilityLockfile {
        let mut lockfile = CapabilityLockfile {
            provider: LockfileProvider {
                name: name.to_owned(),
                version: "0.1.0".to_owned(),
            },
            attested_at: "2026-01-01T00:00:00Z".to_owned(),
            hello: LockfileHello {
                protocol_selected: ProtocolVersion { major: 1, minor: 5 },
                features_enabled: vec![FeatureId::new("events.snapshot.v1").unwrap()],
                server: PeerInfo {
                    name: "devicerail-daemon".to_owned(),
                    version: "0.9.0".to_owned(),
                },
            },
            device: LockfileDevice {
                platform: PlatformKind::Linux,
                actions: Vec::new(),
            },
            digest: Hash::new(format!("sha256:{}", "0".repeat(64))).unwrap(),
        };
        lockfile.seal();
        lockfile
    }

    #[test]
    fn provider_construction_fails_closed_on_bad_lockfiles() {
        // Wrong provider name.
        let error = DeviceRailProvider::new(sample_lockfile("fake")).expect_err("name check");
        assert_eq!(error.error_class, ErrorClass::CapabilityDrift);

        // Tampered digest.
        let mut tampered = sample_lockfile(PROVIDER_NAME);
        tampered.digest = Hash::new(format!("sha256:{}", "f".repeat(64))).unwrap();
        let error = DeviceRailProvider::new(tampered).expect_err("digest check");
        assert_eq!(error.error_class, ErrorClass::CapabilityDrift);

        // A consistent devicerail lockfile constructs.
        assert!(DeviceRailProvider::new(sample_lockfile(PROVIDER_NAME)).is_ok());
    }

    #[tokio::test]
    async fn open_session_rejects_a_foreign_lockfile_digest_before_spawning() {
        let provider = DeviceRailProvider::new(sample_lockfile(PROVIDER_NAME)).unwrap();
        let opts = OpenSessionOptions {
            // A command that must never run: the digest gate fires first.
            endpoint: serde_json::json!({ "spawn": { "command": "/nonexistent/devicerail" } }),
            device_id: "mock-1".to_owned(),
            required_features: Vec::new(),
            lockfile_digest: Hash::new(format!("sha256:{}", "e".repeat(64))).unwrap(),
        };
        let error = match provider.open_session(opts).await {
            Err(error) => error,
            Ok(_) => panic!("the digest gate must fire before spawning"),
        };
        assert_eq!(error.error_class, ErrorClass::CapabilityDrift);
        assert!(error.message.contains("recompile"));
    }

    #[test]
    fn drift_description_names_the_differences() {
        let expected = sample_lockfile(PROVIDER_NAME);
        let mut live = expected.clone();
        live.hello.features_enabled = vec![
            FeatureId::new("events.snapshot.v1").unwrap(),
            FeatureId::new("media.stream.v1").unwrap(),
        ];
        live.hello.server.version = "1.0.0".to_owned();
        live.seal();
        let summary = describe_drift(&expected, &live);
        assert!(summary.contains("features gained"), "{summary}");
        assert!(summary.contains("media.stream.v1"), "{summary}");
        assert!(summary.contains("server"), "{summary}");
    }
}
