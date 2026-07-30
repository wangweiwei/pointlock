//! The provider assembly layer: resolves a `--provider` registration name
//! to a concrete provider assembled under the v0.1 provider name
//! `"devicerail"`.
//!
//! ## Provider registration ruling (M0 + M1)
//!
//! `FlowIR.provider.name` is the const literal `"devicerail"` for all of
//! v0.1 (spine §2 concept 6; 08 §6 version boundary). The assembly layer is
//! therefore the place where a concrete provider is *registered under* that
//! name:
//!
//! - `--provider fake` (M0, kept verbatim) selects the [`FakeProvider`],
//!   whose manifest is re-registered with `name = "devicerail"` before it
//!   reaches the compiler or the lockfile. This mirrors 08 §6.1's demo note
//!   ("装配层以 `--provider fake` 注入 FakeProvider, 同 manifest 形状").
//! - `--provider devicerail` (M1) selects the real
//!   [`DeviceRailProvider`](pointlock_provider_devicerail::DeviceRailProvider)
//!   over a spawned `devicerail-daemon` ([`DeviceRailAssembly`] builds the
//!   spawn endpoint from `--daemon-cmd` / `--daemon-env`).
//!
//! ## Determinism (fake)
//!
//! The mock lockfile is synthesized purely from the manifest with fixed
//! timestamps, so `pointlock lock` today and the run-time re-synthesis
//! tomorrow produce byte-identical content — and therefore the same digest.
//! A FlowIR compiled against a *different* lockfile fails the attestation
//! gate with `capability_drift`, exactly as a real provider would.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use pointlock_ir::{
    ActionOutcome, ActionResult, AssetRef, EventCursor, FeatureId, Hash, Observation,
    ReconcileResult,
};
use pointlock_provider_devicerail::{DEFAULT_DAEMON_COMMAND, DEFAULT_SHUTDOWN_GRACE_MS, SpawnSpec};
use pointlock_provider_kit::lockfile::{
    CapabilityAttestation, CapabilityLockfile, LockfileDevice, LockfileHello, LockfileProvider,
    PeerInfo, ProtocolVersion,
};
use pointlock_provider_kit::manifest::{PlatformKind, ProviderManifest};
use pointlock_provider_kit::{
    BoundActionCall, CancellationToken, EvidenceStream, FakeHandle, FakeProvider, ObserveRequest,
    Provider, ProviderError, ProviderSession, ScriptedOutcome, SessionHealth, SessionOutcome,
    UiSnapshotOutcome, VerdictWrite,
};

/// The only provider name of v0.1 (`FlowIR.provider.name` const).
pub const PROVIDER_NAME: &str = "devicerail";

/// The mock provider registration of M0 (`--provider fake`).
pub const FAKE_REGISTRATION: &str = "fake";

/// The real provider registration of M1 (`--provider devicerail`): the
/// [`DeviceRailProvider`](pointlock_provider_devicerail::DeviceRailProvider)
/// over a spawned `devicerail-daemon`.
pub const DEVICERAIL_REGISTRATION: &str = "devicerail";

/// The device id the fake assembly binds by default.
pub const DEFAULT_DEVICE_ID: &str = "fake-device-1";

/// The device id the devicerail registration binds by default: the daemon's
/// built-in mock driver device, which is always registered (platform
/// discovery is pinned off by default, see [`DeviceRailAssembly`]).
pub const DEVICERAIL_DEFAULT_DEVICE_ID: &str = "mock-1";

/// A resolved `--provider` registration name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    /// The M0 [`FakeProvider`] registration (`--provider fake`).
    Fake,
    /// The M1 real DeviceRail registration (`--provider devicerail`).
    DeviceRail,
}

/// Resolves a `--provider` registration name. Anything outside the two
/// v0.1 registrations is a typed refusal (fail-closed), not a fallback.
pub fn registration(name: &str) -> Result<Registration, String> {
    match name {
        FAKE_REGISTRATION => Ok(Registration::Fake),
        DEVICERAIL_REGISTRATION => Ok(Registration::DeviceRail),
        other => Err(format!(
            "provider registration '{other}' is unknown; v0.1 registers '{FAKE_REGISTRATION}' \
             (the M0 mock) and '{DEVICERAIL_REGISTRATION}' (the real DeviceRail provider), both \
             assembled under the v0.1 provider name '{PROVIDER_NAME}'"
        )),
    }
}

/// Deterministic lock timestamp: the mock provider has no wall clock worth
/// attesting, and a fixed value keeps the lockfile digest reproducible so
/// the run-time re-synthesis matches the `pointlock lock` artifact (M0
/// assembly decision, documented above).
const MOCK_LOCK_ATTESTED_AT: &str = "1970-01-01T00:00:00Z";

/// Deterministic "live attestation" timestamp exposed on open sessions.
const MOCK_SESSION_ATTESTED_AT: &str = "1970-01-01T00:00:01Z";

/// A provider registration assembled for the CLI: the fake provider plus
/// its devicerail-named manifest and the synthesized mock lockfile.
pub struct FakeAssembly {
    provider: FakeProvider,
    manifest: ProviderManifest,
    lockfile: CapabilityLockfile,
}

/// Assembles the M0 fake registration (see the module docs for the
/// registration ruling).
pub fn assemble_fake() -> FakeAssembly {
    // The CLI programs the execute script per call (echo semantics), so the
    // fake starts with an empty queue.
    let provider = FakeProvider::new(VecDeque::new());
    // Provider registration ruling (module docs): the fake's manifest ships
    // as `name: "fake"`, but v0.1 flows bind only against `"devicerail"`.
    let mut manifest = provider.manifest().clone();
    manifest.name = PROVIDER_NAME.to_owned();
    let lockfile = synthesize_lockfile(&manifest);
    FakeAssembly {
        provider,
        manifest,
        lockfile,
    }
}

/// Synthesizes the mock capability lockfile from a manifest: hello freezes
/// protocol 1.5 with `featuresEnabled = manifest.features.guaranteed`; the
/// device freezes `manifest.knownActions` on a mocked platform. Sealed
/// (digest recomputed) before returning.
///
/// Platform note: `PlatformKind` has no `mock` variant (closed enum, spine
/// §4.1); the mock device reports `android`, matching the FakeProvider's
/// own default and keeping `uiTree` the default locating channel.
fn synthesize_lockfile(manifest: &ProviderManifest) -> CapabilityLockfile {
    let mut lockfile = CapabilityLockfile {
        provider: LockfileProvider {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
        },
        attested_at: MOCK_LOCK_ATTESTED_AT.to_owned(),
        hello: LockfileHello {
            protocol_selected: ProtocolVersion { major: 1, minor: 5 },
            features_enabled: manifest.features.guaranteed.clone(),
            server: PeerInfo {
                name: "pointlock-fake-daemon".to_owned(),
                version: manifest.version.clone(),
            },
        },
        device: LockfileDevice {
            platform: PlatformKind::Android,
            // A lockfile attests the DEVICE's driver actions. The
            // provider-synthetic ones (04 §9.4.3) belong to the provider,
            // not the device, so they stay out — bind overlays them, and
            // keeping them out is also what lets a real driver action of
            // the same name shadow them.
            actions: manifest
                .known_actions
                .iter()
                .filter(|action| !action.synthetic)
                .cloned()
                .collect(),
        },
        digest: Hash::new(format!("sha256:{}", "0".repeat(64)))
            .expect("the all-zero placeholder digest is grammatical"),
    };
    lockfile.seal();
    lockfile
}

impl FakeAssembly {
    /// The devicerail-registered manifest (compiler input).
    pub fn manifest(&self) -> &ProviderManifest {
        &self.manifest
    }

    /// The synthesized mock lockfile (what `pointlock lock` writes and what
    /// run-time attestation is checked against).
    pub fn lockfile(&self) -> &CapabilityLockfile {
        &self.lockfile
    }

    /// The `env.platform` string of the mock device.
    pub fn platform(&self) -> String {
        serde_json::to_value(self.lockfile.device.platform)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .expect("PlatformKind serializes to a string literal")
    }

    /// Opens a session against the fake and wraps it with the CLI's echo
    /// semantics and (optionally) a stop-after plan.
    ///
    /// The inner fake attests its own private lockfile; the wrapper exposes
    /// the devicerail attestation built from [`Self::lockfile`], so the
    /// runner's `capability_drift` gate compares the FlowIR against the
    /// artifact `pointlock lock` actually wrote.
    pub async fn open_session(
        &self,
        device_id: &str,
        required_features: Vec<FeatureId>,
        stop_after: Option<StopAfterPlan>,
    ) -> Result<Box<dyn ProviderSession>, ProviderError> {
        let mut opts = self.provider.default_open_options();
        opts.device_id = device_id.to_owned();
        opts.required_features = required_features;
        let inner = self.provider.open_session(opts).await?;
        let attestation =
            CapabilityAttestation::from_lockfile(&self.lockfile, MOCK_SESSION_ATTESTED_AT);
        Ok(Box::new(EchoSession {
            inner,
            attestation,
            handle: self.provider.handle(),
            device_id: device_id.to_owned(),
            stop_after,
        }))
    }
}

/// Cancels a stop token after a fixed number of completed dispatches — the
/// deterministic carrier of `pointlock run --stop-after <step-id>`.
///
/// M0 determinism argument: the M0 IR subset admits neither retry policies
/// nor fallback attempt chains (single bound attempt per step), and the
/// echo script makes every dispatch succeed, so the execute sequence is
/// exactly the body order — dispatch N belongs to body step N. The engine
/// honors the token at the next step boundary, i.e. *after* the named step
/// fully completes (judged + stepExited).
pub struct StopAfterPlan {
    /// The run's stop token (shared with `RunOptions::stop`).
    pub token: CancellationToken,
    /// Dispatches remaining before the token is cancelled.
    pub remaining: AtomicUsize,
}

/// Delegating session wrapper adding the CLI's M0 default behavior:
///
/// - **echo semantics**: before each dispatch it programs the fake with a
///   `succeeded` terminal whose `output` echoes the evaluated call
///   arguments verbatim. Scripting *before* delegation (instead of patching
///   the return value) keeps the fake's journal consistent, so a
///   resume-time `reconcile → completed` adopts the same echoed output.
/// - **stop-after**: see [`StopAfterPlan`].
struct EchoSession {
    inner: Box<dyn ProviderSession>,
    attestation: CapabilityAttestation,
    handle: FakeHandle,
    /// The bound device (journaled on synthesized observations).
    device_id: String,
    stop_after: Option<StopAfterPlan>,
}

impl EchoSession {
    /// A `succeeded` terminal echoing `output`; the fake stamps `callId`
    /// and the logical timestamps at execute time. Every echo carries a
    /// synthesized *after* observation with a registered screenshot —
    /// the fake's manifest declares the vision verify channel, so the
    /// echo path must feed it (no UI snapshot: the echo world has no
    /// tree to declare, and inventing one would not be honest).
    fn echo_outcome(&self, output: serde_json::Value) -> ScriptedOutcome {
        ScriptedOutcome::Terminal(ActionOutcome::Succeeded {
            result: Box::new(ActionResult {
                call_id: String::new(),
                started_at_ms: 0,
                finished_at_ms: 0,
                output,
                before: None,
                after: Some(self.handle.make_observation_for(&self.device_id, None)),
                evidence: Vec::new(),
                execution: None,
            }),
        })
    }
}

#[async_trait]
impl ProviderSession for EchoSession {
    fn attestation(&self) -> &CapabilityAttestation {
        &self.attestation
    }

    async fn execute(
        &self,
        call: BoundActionCall,
        cancel: Option<CancellationToken>,
    ) -> Result<ActionOutcome, ProviderError> {
        self.handle
            .push_script(self.echo_outcome(call.arguments.clone()));
        let outcome = self.inner.execute(call, cancel).await;
        if let Some(plan) = &self.stop_after
            && plan.remaining.fetch_sub(1, Ordering::SeqCst) == 1
        {
            plan.token.cancel();
        }
        outcome
    }

    async fn observe(
        &self,
        req: ObserveRequest,
        cancel: Option<CancellationToken>,
    ) -> Result<Observation, ProviderError> {
        self.inner.observe(req, cancel).await
    }

    async fn ui_snapshot(&self, observation_id: &str) -> Result<UiSnapshotOutcome, ProviderError> {
        self.inner.ui_snapshot(observation_id).await
    }

    async fn reconcile(
        &self,
        call_id: &str,
        issuing: &EventCursor,
    ) -> Result<ReconcileResult, ProviderError> {
        self.inner.reconcile(call_id, issuing).await
    }

    async fn fetch_evidence(&self, asset: &AssetRef) -> Result<EvidenceStream, ProviderError> {
        self.inner.fetch_evidence(asset).await
    }

    async fn record_verdict(&self, verdict: VerdictWrite) -> Result<(), ProviderError> {
        self.inner.record_verdict(verdict).await
    }

    async fn current_cursor(&self) -> Result<EventCursor, ProviderError> {
        self.inner.current_cursor().await
    }

    async fn health(&self) -> Result<SessionHealth, ProviderError> {
        self.inner.health().await
    }

    async fn end(
        &self,
        outcome: SessionOutcome,
        reason: Option<String>,
    ) -> Result<(), ProviderError> {
        self.inner.end(outcome, reason).await
    }
}

// ─── The M1 devicerail registration ─────────────────────────────────────────

/// Counter distinguishing evidence tempdirs within one process.
static EVIDENCE_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// The M1 devicerail spawn assembly: the [`SpawnSpec`] that `--daemon-cmd`
/// / `--daemon-env` resolve to, with the smoke-test defaults applied first
/// (a fresh evidence tempdir as `DEVICERAIL_EVIDENCE_DIR`, and
/// `DEVICERAIL_ANDROID=off` so the run does not depend on local adb state —
/// the daemon's built-in mock driver is always registered). User-provided
/// `--daemon-env` entries override the defaults key by key.
pub struct DeviceRailAssembly {
    spawn: SpawnSpec,
}

impl DeviceRailAssembly {
    /// Builds the spawn assembly. `daemon_env` entries are already-split
    /// `KEY=VALUE` pairs; later entries win over the injected defaults.
    /// The effective evidence directory is created (the daemon expects it
    /// to exist and it doubles as the child's working directory, matching
    /// the M1 smoke harness).
    pub fn new(
        daemon_cmd: Option<&Path>,
        daemon_env: Vec<(String, String)>,
    ) -> Result<Self, String> {
        let mut env: BTreeMap<String, String> =
            BTreeMap::from([("DEVICERAIL_ANDROID".to_owned(), "off".to_owned())]);
        for (key, value) in daemon_env {
            env.insert(key, value);
        }
        let evidence_dir = match env.get("DEVICERAIL_EVIDENCE_DIR") {
            Some(pinned) => PathBuf::from(pinned),
            None => {
                let dir = std::env::temp_dir().join(format!(
                    "pointlock-devicerail-evidence-{}-{}",
                    std::process::id(),
                    EVIDENCE_DIR_COUNTER.fetch_add(1, Ordering::Relaxed),
                ));
                env.insert(
                    "DEVICERAIL_EVIDENCE_DIR".to_owned(),
                    dir.to_string_lossy().into_owned(),
                );
                dir
            }
        };
        std::fs::create_dir_all(&evidence_dir).map_err(|err| {
            format!(
                "cannot create the daemon evidence directory {}: {err}",
                evidence_dir.display()
            )
        })?;
        Ok(DeviceRailAssembly {
            spawn: SpawnSpec {
                command: daemon_cmd
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|| DEFAULT_DAEMON_COMMAND.to_owned()),
                args: Vec::new(),
                env,
                cwd: Some(evidence_dir.to_string_lossy().into_owned()),
                shutdown_grace_ms: DEFAULT_SHUTDOWN_GRACE_MS,
            },
        })
    }

    /// The spawn spec (the `pointlock lock` freeze path consumes it
    /// directly via `lock_via_spawn_at`).
    pub fn spawn_spec(&self) -> &SpawnSpec {
        &self.spawn
    }

    /// The `OpenSessionOptions.endpoint` value in its wire shape (the
    /// spawn form of the endpoint union, 04 §9.1).
    pub fn endpoint(&self) -> serde_json::Value {
        serde_json::json!({ "spawn": self.spawn })
    }
}

/// Delegating session wrapper carrying the CLI's `--stop-after` plan over
/// an arbitrary provider session (M1: the DeviceRail session; the fake
/// path keeps its own [`EchoSession`] wrapper, which scripts echoes too).
///
/// The determinism argument of [`StopAfterPlan`] applies unchanged: the
/// executable IR subset admits a single bound attempt per step with no
/// retry policy, so as long as every dispatch succeeds (the M1 demo flow
/// over the daemon's mock driver does), dispatch N belongs to body step N.
pub struct StopAfterSession {
    inner: Box<dyn ProviderSession>,
    plan: StopAfterPlan,
}

impl StopAfterSession {
    /// Wraps `inner` with the plan.
    pub fn new(inner: Box<dyn ProviderSession>, plan: StopAfterPlan) -> Self {
        StopAfterSession { inner, plan }
    }
}

#[async_trait]
impl ProviderSession for StopAfterSession {
    fn attestation(&self) -> &CapabilityAttestation {
        self.inner.attestation()
    }

    async fn execute(
        &self,
        call: BoundActionCall,
        cancel: Option<CancellationToken>,
    ) -> Result<ActionOutcome, ProviderError> {
        let outcome = self.inner.execute(call, cancel).await;
        if self.plan.remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.plan.token.cancel();
        }
        outcome
    }

    async fn observe(
        &self,
        req: ObserveRequest,
        cancel: Option<CancellationToken>,
    ) -> Result<Observation, ProviderError> {
        self.inner.observe(req, cancel).await
    }

    async fn ui_snapshot(&self, observation_id: &str) -> Result<UiSnapshotOutcome, ProviderError> {
        self.inner.ui_snapshot(observation_id).await
    }

    async fn reconcile(
        &self,
        call_id: &str,
        issuing: &EventCursor,
    ) -> Result<ReconcileResult, ProviderError> {
        self.inner.reconcile(call_id, issuing).await
    }

    async fn fetch_evidence(&self, asset: &AssetRef) -> Result<EvidenceStream, ProviderError> {
        self.inner.fetch_evidence(asset).await
    }

    async fn record_verdict(&self, verdict: VerdictWrite) -> Result<(), ProviderError> {
        self.inner.record_verdict(verdict).await
    }

    async fn current_cursor(&self) -> Result<EventCursor, ProviderError> {
        self.inner.current_cursor().await
    }

    async fn health(&self) -> Result<SessionHealth, ProviderError> {
        self.inner.health().await
    }

    async fn end(
        &self,
        outcome: SessionOutcome,
        reason: Option<String>,
    ) -> Result<(), ProviderError> {
        self.inner.end(outcome, reason).await
    }
}
