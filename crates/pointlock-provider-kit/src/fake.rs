//! FakeProvider — the deterministic, programmable reference implementation
//! of the SPI (04 §8): an in-memory world model with scripted outcomes and
//! fault injection. It is both the conformance suite's reference subject
//! and the runner's E2E test double.
//!
//! Determinism: timestamps come from a logical millisecond clock,
//! observation/asset ids from counters, and event sequences from a
//! monotonic counter — no wall clock, no randomness.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use pointlock_ir::{
    ActionName, ActionOutcome, ActionResult, AssetRef, ErrorClass, ErrorInfo, EventCursor,
    FeatureId, Hash, JsonSchemaDocument, Observation, ReconcileResult, ScreenshotOmissionReason,
    UiSnapshotOmissionReason, UiSnapshotRef, Viewport,
};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::{ProviderError, RetryableSource};
use crate::lockfile::{
    CapabilityAttestation, CapabilityLockfile, LockfileDevice, LockfileHello, LockfileProvider,
    PeerInfo, ProtocolVersion,
};
use crate::manifest::{
    ActionDefinitionStatic, ActionProtection, ChannelRole, ChannelSupport, FeatureDeclarations,
    PlatformKind, ProtocolRange, ProviderManifest, VerbBinding,
};
use crate::spi::{
    BoundActionCall, CancellationToken, EvidenceStream, ObserveRequest, ObserveWant, Provider,
    ProviderSession, SessionHealth, SessionOutcome, UiSnapshotOutcome,
    VERDICT_EVIDENCE_MAX_ENTRIES, VERDICT_SUMMARY_MAX_CHARS, VerdictWrite,
};

/// One scripted execute behavior ([`crate::spi::ProviderSession::execute`]),
/// consumed front-to-back
/// from the script queue.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptedOutcome {
    /// Return this terminal outcome. The journal records the dispatch and
    /// archives the terminal. For `succeeded`, the fake stamps
    /// `result.callId` and the timestamps from its logical clock so the
    /// post-condition `result.callId == call.callId` holds regardless of
    /// what the script author wrote.
    Terminal(ActionOutcome),
    /// Simulate transport rupture after dispatch reached the daemon but
    /// before any terminal arrived: the journal records a dispatch with no
    /// terminal (reconcile → `startedNoTerminal`) and `execute` fails with
    /// a `transport_lost`-class [`ProviderError`].
    TransportLostAfterDispatch,
    /// Simulate transport rupture before dispatch reached the daemon: no
    /// journal trace (reconcile → `neverDispatched`) and `execute` fails
    /// with a `transport_lost`-class [`ProviderError`].
    TransportLostBeforeDispatch,
}

impl ScriptedOutcome {
    /// A minimal deterministic `succeeded` terminal (callId and timestamps
    /// are stamped by the fake at execute time).
    pub fn succeeded() -> Self {
        ScriptedOutcome::Terminal(ActionOutcome::Succeeded {
            result: Box::new(ActionResult {
                call_id: String::new(),
                started_at_ms: 0,
                finished_at_ms: 0,
                output: json!({}),
                before: None,
                after: None,
                evidence: Vec::new(),
                execution: None,
            }),
        })
    }

    /// A `failed` terminal with the given wire code and retryable flag.
    pub fn failed(code: &str, retryable: bool) -> Self {
        ScriptedOutcome::Terminal(ActionOutcome::Failed {
            error: ErrorInfo {
                code: code.to_owned(),
                message: format!("scripted failure: {code}"),
                retryable,
                details: None,
            },
        })
    }

    /// A `cancelled` terminal (wire code `action_cancelled`).
    pub fn cancelled() -> Self {
        ScriptedOutcome::Terminal(ActionOutcome::Cancelled {
            error: ErrorInfo {
                code: "action_cancelled".to_owned(),
                message: "scripted cancellation".to_owned(),
                retryable: false,
                details: None,
            },
        })
    }

    /// A `timedOut` terminal (wire code `action_timeout`).
    pub fn timed_out() -> Self {
        ScriptedOutcome::Terminal(ActionOutcome::TimedOut {
            error: ErrorInfo {
                code: "action_timeout".to_owned(),
                message: "scripted action budget expiry".to_owned(),
                retryable: true,
                details: None,
            },
        })
    }
}

/// One dispatch-journal entry: the fake's stand-in for the daemon's
/// append-only session event log (`actionStarted` / `actionCompleted`).
#[derive(Debug, Clone, PartialEq)]
struct JournalEntry {
    call_id: String,
    /// The archived terminal; `None` models `actionStarted` without
    /// `actionCompleted`.
    terminal: Option<ActionOutcome>,
}

#[derive(Debug)]
struct FakeWorld {
    script: VecDeque<ScriptedOutcome>,
    journal: Vec<JournalEntry>,
    verdicts: Vec<VerdictWrite>,
    /// Monotonic event sequence (the daemon-log watermark).
    sequence: u64,
    /// Logical millisecond clock.
    clock_ms: u64,
    observation_counter: u64,
    session_counter: u64,
    screenshot_omission: Option<ScreenshotOmissionReason>,
    ui_snapshot_omission: Option<UiSnapshotOmissionReason>,
    /// When set, `reconcile` reports the issuing session's log as
    /// unreachable with this reason.
    log_unavailable: Option<String>,
    /// When set, `fetch_evidence` fails with a typed unsupported error
    /// (the control-plane-without-byte-channel scenario): the runner must
    /// degrade localization to a typed gap, never abort the run.
    fetch_unsupported: Option<String>,
    /// When set, `record_verdict` fails with a typed transport error:
    /// the runner must annotate "remote archival failed" and keep the
    /// local verdict, never abort the run (04 §5).
    record_verdict_error: Option<String>,
    /// Evidence bytes addressable by `AssetRef.id`.
    evidence_store: BTreeMap<String, Vec<u8>>,
    /// Injected default UI snapshot served by `observe` (evidence bytes)
    /// and registered for `ui_snapshot` per observation.
    injected_ui_snapshot: Option<serde_json::Value>,
    /// observation id → the synthetic UiSnapshot `ui_snapshot` serves.
    ui_snapshots: BTreeMap<String, serde_json::Value>,
    ended: Option<(SessionOutcome, Option<String>)>,
}

impl FakeWorld {
    fn new(script: VecDeque<ScriptedOutcome>) -> Self {
        FakeWorld {
            script,
            journal: Vec::new(),
            verdicts: Vec::new(),
            sequence: 0,
            clock_ms: 0,
            observation_counter: 0,
            session_counter: 0,
            screenshot_omission: None,
            ui_snapshot_omission: None,
            log_unavailable: None,
            fetch_unsupported: None,
            record_verdict_error: None,
            evidence_store: BTreeMap::new(),
            injected_ui_snapshot: None,
            ui_snapshots: BTreeMap::new(),
            ended: None,
        }
    }

    fn tick_ms(&mut self) -> u64 {
        self.clock_ms += 1;
        self.clock_ms
    }

    fn bump_sequence(&mut self) {
        self.sequence += 1;
    }

    /// Builds a deterministic asset, registering its bytes for
    /// `fetch_evidence`.
    fn make_asset(&mut self, id: String, media_type: &str, bytes: Vec<u8>) -> AssetRef {
        let sha256 = hex_sha256(&bytes);
        self.evidence_store.insert(id.clone(), bytes);
        AssetRef {
            uri: format!("fake://assets/{id}"),
            id,
            media_type: media_type.to_owned(),
            sha256: Some(sha256),
        }
    }

    /// Synthesizes one observation: a fresh id, a registered screenshot
    /// asset, and — when a snapshot is given — a `UiSnapshotRef` whose
    /// evidence bytes are the snapshot's canonical JSON, with the snapshot
    /// registered for `ui_snapshot(observationId)` (both dereference
    /// routes serve the same injected tree).
    fn synthesize_observation(
        &mut self,
        device_id: &str,
        ui_snapshot: Option<serde_json::Value>,
    ) -> Observation {
        self.observation_counter += 1;
        let observation_id = format!("obs-{}", self.observation_counter);
        let captured_at_ms = self.tick_ms();
        let screenshot = {
            let id = format!("{observation_id}-screenshot");
            let bytes = format!("fake-screenshot:{observation_id}").into_bytes();
            self.make_asset(id, "image/png", bytes)
        };
        let ui_snapshot = ui_snapshot.map(|snapshot| {
            let bytes =
                serde_json::to_vec(&snapshot).expect("an injected UiSnapshot value serializes");
            let id = format!("{observation_id}-uitree");
            let evidence = self.make_asset(id, "application/json", bytes);
            self.ui_snapshots.insert(observation_id.clone(), snapshot);
            UiSnapshotRef { evidence }
        });
        Observation {
            id: observation_id,
            device_id: device_id.to_owned(),
            captured_at_ms,
            viewport: Viewport {
                width: 1080,
                height: 2400,
                scale_factor: 2.0,
            },
            screenshot: Some(screenshot),
            screenshot_omission: None,
            ui_snapshot,
            ui_snapshot_omission: None,
            metadata: BTreeMap::new(),
        }
    }
}

/// A cheap, clonable handle into the fake's world for programming and
/// inspection from tests (fault injection, recorded-state assertions).
#[derive(Debug, Clone)]
pub struct FakeHandle {
    world: Arc<Mutex<FakeWorld>>,
}

impl FakeHandle {
    fn world(&self) -> MutexGuard<'_, FakeWorld> {
        self.world.lock().expect("fake world lock poisoned")
    }

    /// Appends a scripted outcome to the back of the script queue.
    pub fn push_script(&self, outcome: ScriptedOutcome) {
        self.world().script.push_back(outcome);
    }

    /// Makes `reconcile` report the issuing session's log as unreachable.
    pub fn set_log_unavailable(&self, reason: impl Into<String>) {
        self.world().log_unavailable = Some(reason.into());
    }

    /// Restores log availability.
    pub fn clear_log_unavailable(&self) {
        self.world().log_unavailable = None;
    }

    /// Makes `fetch_evidence` fail with a typed unsupported error (`None`
    /// restores it). Models a control plane without an asset byte channel
    /// — the runner degrades localization to a typed gap (M2).
    pub fn set_fetch_evidence_unsupported(&self, reason: Option<String>) {
        self.world().fetch_unsupported = reason;
    }

    /// Makes `record_verdict` fail with a typed transport error (`None`
    /// restores it). Models a wire failure of remote verdict archival —
    /// the runner annotates it and never aborts the run (04 §5).
    pub fn set_record_verdict_error(&self, reason: Option<String>) {
        self.world().record_verdict_error = reason;
    }

    /// Injects a screenshot omission for subsequent observations.
    pub fn set_screenshot_omission(&self, reason: Option<ScreenshotOmissionReason>) {
        self.world().screenshot_omission = reason;
    }

    /// Injects a UI-snapshot omission for subsequent observations and
    /// `ui_snapshot` calls.
    pub fn set_ui_snapshot_omission(&self, reason: Option<UiSnapshotOmissionReason>) {
        self.world().ui_snapshot_omission = reason;
    }

    /// Registers evidence bytes addressable via `AssetRef.id`.
    pub fn insert_evidence(&self, asset_id: impl Into<String>, bytes: Vec<u8>) {
        self.world().evidence_store.insert(asset_id.into(), bytes);
    }

    /// Injects the synthetic UiSnapshot that subsequent `observe` calls
    /// serve: the produced `UiSnapshotRef` evidence bytes are the
    /// snapshot's canonical JSON (fetchable via `fetch_evidence`) and
    /// `ui_snapshot(observationId)` returns the injected content. `None`
    /// restores the placeholder bytes.
    pub fn inject_ui_snapshot(&self, snapshot: Option<serde_json::Value>) {
        self.world().injected_ui_snapshot = snapshot;
    }

    /// Synthesizes an [`Observation`] carrying a registered screenshot
    /// asset and — when `ui_snapshot` is given — a `UiSnapshotRef` whose
    /// evidence bytes are the snapshot's canonical JSON, with
    /// `ui_snapshot(observationId)` serving the same injected tree. Embed
    /// it into a scripted `ActionResult` (`before`/`after`) to drive the
    /// runner's uiTree/vision verify channels programmatically.
    pub fn make_observation(&self, ui_snapshot: Option<serde_json::Value>) -> Observation {
        self.make_observation_for("fake-device-1", ui_snapshot)
    }

    /// [`Self::make_observation`] with an explicit device id — the
    /// observation record lands in the ledger, so a harness binding a
    /// non-default device must not journal the wrong identity.
    pub fn make_observation_for(
        &self,
        device_id: &str,
        ui_snapshot: Option<serde_json::Value>,
    ) -> Observation {
        self.world().synthesize_observation(device_id, ui_snapshot)
    }

    /// The verdicts accepted by `record_verdict` so far.
    pub fn recorded_verdicts(&self) -> Vec<VerdictWrite> {
        self.world().verdicts.clone()
    }

    /// The callIds journaled as dispatched, in dispatch order.
    pub fn dispatched_call_ids(&self) -> Vec<String> {
        self.world()
            .journal
            .iter()
            .map(|entry| entry.call_id.clone())
            .collect()
    }

    /// The outcome recorded by the first effective `end` call, if any.
    pub fn ended_outcome(&self) -> Option<(SessionOutcome, Option<String>)> {
        self.world().ended.clone()
    }
}

/// Deterministic, programmable SPI reference implementation (04 §8).
///
/// Construction injects the execute script; all other behavior is
/// programmable through [`FakeProvider::handle`]. Sessions share the
/// provider's world, so a journal written through one session generation
/// stays readable by `reconcile` from a later one (resume tests).
#[derive(Debug)]
pub struct FakeProvider {
    manifest: ProviderManifest,
    lockfile: CapabilityLockfile,
    world: Arc<Mutex<FakeWorld>>,
}

impl FakeProvider {
    /// Creates a fake provider that will serve `script` from its
    /// `execute` implementation, front to back.
    pub fn new(script: VecDeque<ScriptedOutcome>) -> Self {
        let manifest = Self::default_manifest();
        let lockfile = Self::default_lockfile(&manifest);
        FakeProvider {
            manifest,
            lockfile,
            world: Arc::new(Mutex::new(FakeWorld::new(script))),
        }
    }

    /// A programming/inspection handle sharing this provider's world.
    pub fn handle(&self) -> FakeHandle {
        FakeHandle {
            world: Arc::clone(&self.world),
        }
    }

    /// The lockfile this fake attests against (its digest goes into
    /// [`crate::spi::OpenSessionOptions::lockfile_digest`]).
    pub fn lockfile(&self) -> &CapabilityLockfile {
        &self.lockfile
    }

    /// Session options that open successfully against this fake.
    pub fn default_open_options(&self) -> crate::spi::OpenSessionOptions {
        crate::spi::OpenSessionOptions {
            endpoint: json!({ "fake": true }),
            device_id: "fake-device-1".to_owned(),
            required_features: self.lockfile.hello.features_enabled.clone(),
            lockfile_digest: self.lockfile.digest.clone(),
        }
    }

    fn feature(id: &str) -> FeatureId {
        FeatureId::new(id).expect("built-in feature ids are grammatical")
    }

    fn action_name(name: &str) -> ActionName {
        ActionName::new(name).expect("built-in action names are grammatical")
    }

    fn semantic_action(name: &str) -> ActionDefinitionStatic {
        ActionDefinitionStatic {
            name: Self::action_name(name),
            input_schema: JsonSchemaDocument::new(json!({ "type": "object" }))
                .expect("an object schema is a valid schema document"),
            output_schema: None,
            protection: ActionProtection::Standard,
            synthetic: false,
        }
    }

    /// A provider-synthetic readonly action (04 §9.4.3): the fake serves
    /// `observe`/`screenshot` from its own `observe()`, so no driver
    /// declares them and they must not appear in a lockfile.
    fn synthetic_action(name: &str) -> ActionDefinitionStatic {
        // Unlike the fake's driver actions this carries a real schema: the
        // `wants` vocabulary is closed, and compiling a flow that asks for
        // a part nobody can capture should fail at compile time.
        let input_schema = if name == "observe" {
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["wants"],
                "properties": {
                    "wants": {
                        "type": "array",
                        "minItems": 1,
                        "uniqueItems": true,
                        "items": { "enum": ["screenshot", "uiSnapshot"] }
                    }
                }
            })
        } else {
            json!({ "type": "object", "additionalProperties": false, "properties": {} })
        };
        ActionDefinitionStatic {
            name: Self::action_name(name),
            input_schema: JsonSchemaDocument::new(input_schema)
                .expect("an object schema is a valid schema document"),
            output_schema: None,
            protection: ActionProtection::Standard,
            synthetic: true,
        }
    }

    fn default_manifest() -> ProviderManifest {
        use pointlock_ir::CanonicalVerb;
        let semantic = Self::feature("device.semanticActions.v1");
        let verb_binding = |verb: CanonicalVerb, action: &str, arg: &str| VerbBinding {
            verb,
            action_name: Self::action_name(action),
            requires_feature: Some(semantic.clone()),
            arg_map: BTreeMap::from([(arg.to_owned(), arg.to_owned())]),
        };
        ProviderManifest {
            name: "fake".to_owned(),
            version: "0.1.0".to_owned(),
            protocol: ProtocolRange {
                major: 1,
                min_minor: 5,
                max_minor: 5,
            },
            features: FeatureDeclarations {
                guaranteed: vec![
                    Self::feature("device.semanticActions.v1"),
                    Self::feature("observation.uiSnapshot.v1"),
                    Self::feature("verdict.record.v1"),
                    Self::feature("events.snapshot.v1"),
                    Self::feature("request.control.v1"),
                ],
                conditional: Vec::new(),
            },
            verb_bindings: vec![
                verb_binding(CanonicalVerb::Tap, "tapElement", "element"),
                verb_binding(CanonicalVerb::SetValue, "setElementValue", "element"),
                verb_binding(CanonicalVerb::Clear, "clearElement", "element"),
                verb_binding(CanonicalVerb::WaitFor, "waitForElement", "element"),
                verb_binding(CanonicalVerb::Find, "findElement", "element"),
                verb_binding(CanonicalVerb::Observe, "observe", "wants"),
                // The shortcut form fixes `wants` inside the action and
                // takes no arguments (its schema forbids any), so its
                // binding maps none — same shape as the devicerail
                // manifest.
                VerbBinding {
                    verb: CanonicalVerb::Screenshot,
                    action_name: Self::action_name("screenshot"),
                    requires_feature: None,
                    arg_map: BTreeMap::new(),
                },
            ],
            channels: vec![
                ChannelSupport {
                    channel: pointlock_ir::Channel::UiTree,
                    role: ChannelRole::Both,
                    requires_feature: Some(Self::feature("observation.uiSnapshot.v1")),
                    requires_platform: None,
                },
                // vision: verify-only (principle 7) — the fake synthesizes
                // a screenshot on every observation, so declaring the
                // channel is honest; the verification itself is
                // Pointlock-side (pointlock-vision).
                ChannelSupport {
                    channel: pointlock_ir::Channel::Vision,
                    role: ChannelRole::Verify,
                    requires_feature: None,
                    requires_platform: None,
                },
            ],
            known_actions: vec![
                Self::semantic_action("findElement"),
                Self::semantic_action("tapElement"),
                Self::semantic_action("clearElement"),
                Self::semantic_action("setElementValue"),
                Self::semantic_action("waitForElement"),
                Self::synthetic_action("observe"),
                Self::synthetic_action("screenshot"),
            ],
        }
    }

    fn default_lockfile(manifest: &ProviderManifest) -> CapabilityLockfile {
        let mut lockfile = CapabilityLockfile {
            provider: LockfileProvider {
                name: manifest.name.clone(),
                version: manifest.version.clone(),
            },
            attested_at: "1970-01-01T00:00:00Z".to_owned(),
            hello: LockfileHello {
                protocol_selected: ProtocolVersion { major: 1, minor: 5 },
                features_enabled: manifest.features.guaranteed.clone(),
                server: PeerInfo {
                    name: "fake-daemon".to_owned(),
                    version: manifest.version.clone(),
                },
            },
            device: LockfileDevice {
                platform: PlatformKind::Android,
                // A lockfile lists DRIVER actions; the synthetic ones are
                // the provider's own and are overlaid at bind time.
                actions: manifest
                    .known_actions
                    .iter()
                    .filter(|action| !action.synthetic)
                    .cloned()
                    .collect(),
            },
            digest: Hash::new(format!("sha256:{}", "0".repeat(64)))
                .expect("the placeholder digest is grammatical"),
        };
        lockfile.seal();
        lockfile
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn manifest(&self) -> &ProviderManifest {
        &self.manifest
    }

    async fn open_session(
        &self,
        opts: crate::spi::OpenSessionOptions,
    ) -> Result<Box<dyn ProviderSession>, ProviderError> {
        if opts.lockfile_digest != self.lockfile.digest {
            return Err(ProviderError::new(
                ErrorClass::CapabilityDrift,
                format!(
                    "attestation mismatch: live world digest {} != expected lockfileDigest {}",
                    self.lockfile.digest, opts.lockfile_digest
                ),
                RetryableSource::Classifier,
            ));
        }
        if let Some(missing) = opts
            .required_features
            .iter()
            .find(|feature| !self.lockfile.hello.features_enabled.contains(feature))
        {
            return Err(ProviderError::new(
                ErrorClass::CapabilityDrift,
                format!("required feature not negotiated: {missing}"),
                RetryableSource::Classifier,
            ));
        }
        let session_id = {
            let mut world = self.world.lock().expect("fake world lock poisoned");
            world.session_counter += 1;
            // A new session generation reopens the world.
            world.ended = None;
            format!("fake-session-{}", world.session_counter)
        };
        let attestation = CapabilityAttestation::from_lockfile(
            &self.lockfile,
            // Deterministic "live" attestation timestamp.
            "1970-01-01T00:00:01Z",
        );
        Ok(Box::new(FakeProviderSession {
            session_id,
            device_id: opts.device_id,
            attestation,
            world: Arc::clone(&self.world),
        }))
    }
}

/// A session over the fake world. See [`FakeProvider`].
#[derive(Debug)]
pub struct FakeProviderSession {
    session_id: String,
    device_id: String,
    attestation: CapabilityAttestation,
    world: Arc<Mutex<FakeWorld>>,
}

impl FakeProviderSession {
    fn world(&self) -> MutexGuard<'_, FakeWorld> {
        self.world.lock().expect("fake world lock poisoned")
    }

    fn transport_lost(context: &str) -> ProviderError {
        ProviderError::new(
            ErrorClass::TransportLost,
            format!("fake transport lost: {context}"),
            RetryableSource::Classifier,
        )
        .with_client_code("transport_closed")
    }

    fn ensure_active(world: &FakeWorld, method: &str) -> Result<(), ProviderError> {
        match world.ended {
            Some(_) => Err(Self::transport_lost(&format!(
                "session already ended; {method} unavailable"
            ))),
            None => Ok(()),
        }
    }

    fn pre_cancelled(cancel: &Option<CancellationToken>) -> bool {
        cancel.as_ref().is_some_and(CancellationToken::is_cancelled)
    }

    fn cancelled_before_dispatch() -> ProviderError {
        ProviderError::new(
            ErrorClass::ActionCancelled,
            "cancellation token was already cancelled; no wire request was sent",
            RetryableSource::Classifier,
        )
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[async_trait]
impl ProviderSession for FakeProviderSession {
    fn attestation(&self) -> &CapabilityAttestation {
        &self.attestation
    }

    async fn execute(
        &self,
        call: BoundActionCall,
        cancel: Option<CancellationToken>,
    ) -> Result<ActionOutcome, ProviderError> {
        // Provider-synthetic observation actions (04 §9.4.3) are served
        // from `observe()` rather than the journal. Routed before the world
        // lock is taken, since `observe` takes it too. Absence from the
        // attestation is the shadowing rule at run time: a driver action of
        // the same name is attested and takes the normal path.
        if !self.attestation.actions.contains_key(&call.action_name)
            && let Some(wants) = crate::synthetic_observation_wants(&call)?
        {
            {
                let world = self.world();
                Self::ensure_active(&world, "execute")?;
            }
            if Self::pre_cancelled(&cancel) {
                return Err(Self::cancelled_before_dispatch());
            }
            let started_at_ms = crate::now_ms();
            let observation = self
                .observe(
                    ObserveRequest {
                        wants: wants.clone(),
                    },
                    cancel,
                )
                .await?;
            return Ok(ActionOutcome::Succeeded {
                result: Box::new(pointlock_ir::ActionResult {
                    call_id: call.call_id,
                    started_at_ms,
                    finished_at_ms: crate::now_ms(),
                    output: crate::observation_projection(&observation, &wants),
                    before: None,
                    after: Some(observation),
                    evidence: Vec::new(),
                    execution: None,
                }),
            });
        }
        let mut world = self.world();
        Self::ensure_active(&world, "execute")?;
        if Self::pre_cancelled(&cancel) {
            return Err(Self::cancelled_before_dispatch());
        }
        if !self.attestation.actions.contains_key(&call.action_name) {
            return Err(ProviderError::new(
                ErrorClass::CapabilityDrift,
                format!(
                    "actionName {} is not attested; refusing to dispatch",
                    call.action_name
                ),
                RetryableSource::Classifier,
            ));
        }
        if world
            .journal
            .iter()
            .any(|entry| entry.call_id == call.call_id)
        {
            return Err(ProviderError::new(
                ErrorClass::BindArgumentsInvalid,
                format!(
                    "duplicate callId {}: a retry must be a new callId with a new WAL intent",
                    call.call_id
                ),
                RetryableSource::Classifier,
            ));
        }

        let scripted = world.script.pop_front().unwrap_or_else(|| {
            // Script exhaustion is a harness misconfiguration; surface it as
            // a deterministic failed terminal rather than a transport error.
            ScriptedOutcome::Terminal(ActionOutcome::Failed {
                error: ErrorInfo {
                    code: "fake_script_exhausted".to_owned(),
                    message: "FakeProvider script exhausted; push more ScriptedOutcomes".to_owned(),
                    retryable: false,
                    details: None,
                },
            })
        });

        match scripted {
            ScriptedOutcome::Terminal(mut outcome) => {
                let started_at_ms = world.tick_ms();
                let finished_at_ms = world.tick_ms();
                if let ActionOutcome::Succeeded { result } = &mut outcome {
                    result.call_id = call.call_id.clone();
                    result.started_at_ms = started_at_ms;
                    result.finished_at_ms = finished_at_ms;
                }
                // actionStarted + actionCompleted.
                world.bump_sequence();
                world.bump_sequence();
                world.journal.push(JournalEntry {
                    call_id: call.call_id,
                    terminal: Some(outcome.clone()),
                });
                Ok(outcome)
            }
            ScriptedOutcome::TransportLostAfterDispatch => {
                // actionStarted only.
                world.bump_sequence();
                world.journal.push(JournalEntry {
                    call_id: call.call_id,
                    terminal: None,
                });
                Err(Self::transport_lost("connection dropped after dispatch"))
            }
            ScriptedOutcome::TransportLostBeforeDispatch => {
                Err(Self::transport_lost("connection dropped before dispatch"))
            }
        }
    }

    async fn observe(
        &self,
        req: ObserveRequest,
        cancel: Option<CancellationToken>,
    ) -> Result<Observation, ProviderError> {
        let mut world = self.world();
        Self::ensure_active(&world, "observe")?;
        if Self::pre_cancelled(&cancel) {
            return Err(Self::cancelled_before_dispatch());
        }
        world.observation_counter += 1;
        let observation_id = format!("obs-{}", world.observation_counter);
        let captured_at_ms = world.tick_ms();

        let wants_screenshot = req.wants.contains(&ObserveWant::Screenshot);
        let wants_ui_snapshot = req.wants.contains(&ObserveWant::UiSnapshot);

        let screenshot_omission = world.screenshot_omission;
        let screenshot = (wants_screenshot && screenshot_omission.is_none()).then(|| {
            let id = format!("{observation_id}-screenshot");
            let bytes = format!("fake-screenshot:{observation_id}").into_bytes();
            world.make_asset(id, "image/png", bytes)
        });

        let ui_snapshot_omission = world.ui_snapshot_omission;
        let ui_snapshot = (wants_ui_snapshot && ui_snapshot_omission.is_none()).then(|| {
            let id = format!("{observation_id}-uitree");
            // An injected snapshot is served on both dereference routes:
            // its canonical JSON becomes the evidence bytes and the value
            // is registered for `ui_snapshot(observationId)`.
            let bytes = match world.injected_ui_snapshot.clone() {
                Some(snapshot) => {
                    let bytes = serde_json::to_vec(&snapshot)
                        .expect("an injected UiSnapshot value serializes");
                    world.ui_snapshots.insert(observation_id.clone(), snapshot);
                    bytes
                }
                None => format!("fake-uitree:{observation_id}").into_bytes(),
            };
            UiSnapshotRef {
                evidence: world.make_asset(id, "application/json", bytes),
            }
        });

        // observationCaptured.
        world.bump_sequence();
        Ok(Observation {
            id: observation_id,
            device_id: self.device_id.clone(),
            captured_at_ms,
            viewport: Viewport {
                width: 1080,
                height: 2400,
                scale_factor: 2.0,
            },
            screenshot,
            screenshot_omission: wants_screenshot.then_some(screenshot_omission).flatten(),
            ui_snapshot,
            ui_snapshot_omission: wants_ui_snapshot.then_some(ui_snapshot_omission).flatten(),
            metadata: BTreeMap::new(),
        })
    }

    async fn ui_snapshot(&self, observation_id: &str) -> Result<UiSnapshotOutcome, ProviderError> {
        let world = self.world();
        Self::ensure_active(&world, "ui_snapshot")?;
        if let Some(reason) = world.ui_snapshot_omission {
            return Ok(UiSnapshotOutcome::Unavailable { reason });
        }
        if let Some(snapshot) = world.ui_snapshots.get(observation_id) {
            return Ok(UiSnapshotOutcome::Available {
                snapshot: snapshot.clone(),
            });
        }
        Ok(UiSnapshotOutcome::Available {
            snapshot: json!({
                "observationId": observation_id,
                "contexts": [],
                "nodes": [],
            }),
        })
    }

    async fn reconcile(
        &self,
        call_id: &str,
        issuing: &EventCursor,
    ) -> Result<ReconcileResult, ProviderError> {
        // The fake's journal is WORLD-scoped: it survives session
        // generations, so it genuinely contains the issuing session's
        // events whatever the credential — the fake trivially has the
        // cross-generation log retrieval a real provider only gains in
        // v0.2 (04 §5). Answering `neverDispatched` from it is therefore
        // sound for any credential; `_issuing` documents the contract.
        let _ = issuing;
        let world = self.world();
        if let Some(reason) = &world.log_unavailable {
            return Ok(ReconcileResult::LogUnavailable {
                reason: reason.clone(),
            });
        }
        let Some(entry) = world.journal.iter().find(|entry| entry.call_id == call_id) else {
            // The complete event range of the issuing session shows no
            // trace: safe to replay.
            return Ok(ReconcileResult::NeverDispatched);
        };
        match &entry.terminal {
            // An archived terminal is a certain fate whatever its four-way
            // discriminant: adopt it verbatim through the completed fate.
            Some(outcome) => Ok(ReconcileResult::Completed {
                outcome: Box::new(outcome.clone()),
            }),
            None => Ok(ReconcileResult::StartedNoTerminal),
        }
    }

    async fn fetch_evidence(&self, asset: &AssetRef) -> Result<EvidenceStream, ProviderError> {
        let world = self.world();
        Self::ensure_active(&world, "fetch_evidence")?;
        if let Some(reason) = &world.fetch_unsupported {
            return Err(ProviderError::new(
                ErrorClass::ActionFailedFinal,
                format!("fetch_evidence unsupported: {reason}"),
                RetryableSource::Classifier,
            )
            .with_client_code("asset_fetch_unsupported"));
        }
        let Some(bytes) = world.evidence_store.get(&asset.id).cloned() else {
            return Err(ProviderError::new(
                ErrorClass::ActionFailedFinal,
                format!("unknown evidence asset: {}", asset.id),
                RetryableSource::Classifier,
            ));
        };
        if let Some(expected) = &asset.sha256 {
            let actual = hex_sha256(&bytes);
            if &actual != expected {
                return Err(ProviderError::new(
                    ErrorClass::ActionFailedFinal,
                    format!(
                        "evidence integrity failure for {}: sha256 {actual} != declared {expected}",
                        asset.id
                    ),
                    RetryableSource::Classifier,
                ));
            }
        }
        Ok(Box::pin(futures_util::stream::iter([Ok(bytes)])))
    }

    async fn record_verdict(&self, verdict: VerdictWrite) -> Result<(), ProviderError> {
        let mut world = self.world();
        Self::ensure_active(&world, "record_verdict")?;
        if let Some(reason) = &world.record_verdict_error {
            return Err(ProviderError::new(
                ErrorClass::TransportLost,
                format!("scripted verdict.record failure: {reason}"),
                RetryableSource::Classifier,
            ));
        }
        let summary_chars = verdict.summary.chars().count();
        if summary_chars > VERDICT_SUMMARY_MAX_CHARS {
            return Err(ProviderError::new(
                ErrorClass::BindArgumentsInvalid,
                format!(
                    "verdict summary is {summary_chars} chars; wire cap is \
                     {VERDICT_SUMMARY_MAX_CHARS} (fail-closed; compaction is the runner's job)"
                ),
                RetryableSource::Classifier,
            ));
        }
        if verdict.evidence.len() > VERDICT_EVIDENCE_MAX_ENTRIES {
            return Err(ProviderError::new(
                ErrorClass::BindArgumentsInvalid,
                format!(
                    "verdict cites {} evidence entries; wire cap is {VERDICT_EVIDENCE_MAX_ENTRIES}",
                    verdict.evidence.len()
                ),
                RetryableSource::Classifier,
            ));
        }
        world.verdicts.push(verdict);
        // verdictRecorded.
        world.bump_sequence();
        Ok(())
    }

    async fn current_cursor(&self) -> Result<EventCursor, ProviderError> {
        let world = self.world();
        Ok(EventCursor {
            session_id: self.session_id.clone(),
            last_sequence: world.sequence,
        })
    }

    async fn health(&self) -> Result<SessionHealth, ProviderError> {
        let world = self.world();
        Ok(SessionHealth {
            ok: world.ended.is_none(),
            degraded: None,
        })
    }

    async fn end(
        &self,
        outcome: SessionOutcome,
        reason: Option<String>,
    ) -> Result<(), ProviderError> {
        let mut world = self.world();
        if world.ended.is_some() {
            // Idempotent: ending an ended session is a no-op (04 §2.1).
            return Ok(());
        }
        world.ended = Some((outcome, reason));
        // sessionEnded.
        world.bump_sequence();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use pointlock_ir::VerdictStatus;

    fn call(call_id: &str) -> BoundActionCall {
        BoundActionCall {
            call_id: call_id.to_owned(),
            action_name: ActionName::new("tapElement").unwrap(),
            arguments: json!({}),
            action_timeout_ms: None,
            request_timeout_ms: None,
        }
    }

    async fn open(provider: &FakeProvider) -> Box<dyn ProviderSession> {
        provider
            .open_session(provider.default_open_options())
            .await
            .expect("open_session")
    }

    #[tokio::test]
    async fn execute_stamps_call_id_and_journals_terminal() {
        let provider = FakeProvider::new(VecDeque::from([ScriptedOutcome::succeeded()]));
        let session = open(&provider).await;
        let outcome = session
            .execute(call("call-1"), None)
            .await
            .expect("execute");
        let ActionOutcome::Succeeded { result } = outcome else {
            panic!("expected succeeded, got {outcome:?}");
        };
        assert_eq!(result.call_id, "call-1");
        assert!(result.finished_at_ms > result.started_at_ms);
        assert_eq!(provider.handle().dispatched_call_ids(), vec!["call-1"]);

        let fate = session
            .reconcile("call-1", &session.current_cursor().await.expect("cursor"))
            .await
            .expect("reconcile");
        let ReconcileResult::Completed { outcome } = fate else {
            panic!("expected completed, got {fate:?}");
        };
        let ActionOutcome::Succeeded { result } = *outcome else {
            panic!("expected a succeeded terminal, got {outcome:?}");
        };
        assert_eq!(result.call_id, "call-1");
    }

    #[tokio::test]
    async fn a_foreign_issuing_credential_reads_the_world_journal() {
        // The fake's world journal is the union of every generation's
        // log (in-process cross-generation retrieval): a foreign
        // credential still reads the TRUE complete history, so
        // `neverDispatched` for an unseen callId is sound — unlike a
        // per-session log, which must answer logUnavailable (the
        // devicerail guard).
        let provider = FakeProvider::new(VecDeque::new());
        let session = open(&provider).await;
        let foreign = EventCursor {
            session_id: "some-earlier-generation".to_owned(),
            last_sequence: 3,
        };
        let fate = session
            .reconcile("never-seen", &foreign)
            .await
            .expect("reconcile");
        assert!(
            matches!(fate, ReconcileResult::NeverDispatched),
            "got {fate:?}"
        );
    }

    #[tokio::test]
    async fn reconcile_adopts_archived_non_succeeded_terminals_verbatim() {
        let provider = FakeProvider::new(VecDeque::from([
            ScriptedOutcome::failed("device_unavailable", true),
            ScriptedOutcome::timed_out(),
        ]));
        let session = open(&provider).await;
        // Non-succeeded terminals are Ok values, not errors (04 §3).
        session
            .execute(call("call-f"), None)
            .await
            .expect("failed terminal");
        session
            .execute(call("call-t"), None)
            .await
            .expect("timedOut terminal");

        for (call_id, kind) in [("call-f", "failed"), ("call-t", "timedOut")] {
            let fate = session
                .reconcile(call_id, &session.current_cursor().await.expect("cursor"))
                .await
                .expect("reconcile");
            let ReconcileResult::Completed { outcome } = fate else {
                panic!("expected completed for {call_id}, got {fate:?}");
            };
            assert_eq!(outcome.kind(), kind);
        }
    }

    #[tokio::test]
    async fn transport_loss_variants_map_to_reconcile_fates() {
        let provider = FakeProvider::new(VecDeque::from([
            ScriptedOutcome::TransportLostAfterDispatch,
            ScriptedOutcome::TransportLostBeforeDispatch,
        ]));
        let session = open(&provider).await;

        let error = session.execute(call("hung"), None).await.unwrap_err();
        assert_eq!(error.error_class, ErrorClass::TransportLost);
        assert_eq!(
            session
                .reconcile("hung", &session.current_cursor().await.expect("cursor"))
                .await
                .expect("reconcile"),
            ReconcileResult::StartedNoTerminal
        );

        let error = session.execute(call("lost"), None).await.unwrap_err();
        assert_eq!(error.error_class, ErrorClass::TransportLost);
        assert_eq!(
            session
                .reconcile("lost", &session.current_cursor().await.expect("cursor"))
                .await
                .expect("reconcile"),
            ReconcileResult::NeverDispatched
        );
    }

    #[tokio::test]
    async fn duplicate_call_id_is_rejected_without_consuming_script() {
        let provider = FakeProvider::new(VecDeque::from([
            ScriptedOutcome::succeeded(),
            ScriptedOutcome::failed("device_unavailable", true),
        ]));
        let session = open(&provider).await;
        session.execute(call("dup"), None).await.expect("first");
        let error = session.execute(call("dup"), None).await.unwrap_err();
        assert_eq!(error.error_class, ErrorClass::BindArgumentsInvalid);
        // The scripted `failed` terminal is still available for a fresh id.
        let outcome = session.execute(call("fresh"), None).await.expect("second");
        assert_eq!(outcome.kind(), "failed");
    }

    #[tokio::test]
    async fn pre_cancelled_token_prevents_dispatch() {
        let provider = FakeProvider::new(VecDeque::from([ScriptedOutcome::succeeded()]));
        let session = open(&provider).await;
        let token = CancellationToken::new();
        token.cancel();
        let error = session
            .execute(call("never-sent"), Some(token))
            .await
            .unwrap_err();
        assert_eq!(error.error_class, ErrorClass::ActionCancelled);
        assert_eq!(
            session
                .reconcile(
                    "never-sent",
                    &session.current_cursor().await.expect("cursor")
                )
                .await
                .expect("reconcile"),
            ReconcileResult::NeverDispatched
        );
    }

    #[tokio::test]
    async fn unattested_action_is_refused_fail_closed() {
        let provider = FakeProvider::new(VecDeque::from([ScriptedOutcome::succeeded()]));
        let session = open(&provider).await;
        let mut bogus = call("bogus");
        bogus.action_name = ActionName::new("launchMissiles").unwrap();
        let error = session.execute(bogus, None).await.unwrap_err();
        assert_eq!(error.error_class, ErrorClass::CapabilityDrift);
        assert!(provider.handle().dispatched_call_ids().is_empty());
    }

    #[tokio::test]
    async fn observe_honors_wants_and_injected_omissions() {
        let provider = FakeProvider::new(VecDeque::new());
        let session = open(&provider).await;
        let observation = session
            .observe(
                ObserveRequest {
                    wants: vec![ObserveWant::Screenshot, ObserveWant::UiSnapshot],
                },
                None,
            )
            .await
            .expect("observe");
        assert!(observation.screenshot.is_some());
        assert!(observation.ui_snapshot.is_some());
        assert!(observation.screenshot_omission.is_none());

        provider
            .handle()
            .set_screenshot_omission(Some(ScreenshotOmissionReason::ProtectedAction));
        let observation = session
            .observe(
                ObserveRequest {
                    wants: vec![ObserveWant::Screenshot],
                },
                None,
            )
            .await
            .expect("observe");
        assert!(observation.screenshot.is_none());
        assert_eq!(
            observation.screenshot_omission,
            Some(ScreenshotOmissionReason::ProtectedAction)
        );
        // An unwanted part is simply absent, with no omission claim.
        assert!(observation.ui_snapshot.is_none());
        assert!(observation.ui_snapshot_omission.is_none());
    }

    #[tokio::test]
    async fn ui_snapshot_omission_yields_typed_unavailable() {
        let provider = FakeProvider::new(VecDeque::new());
        let session = open(&provider).await;
        provider
            .handle()
            .set_ui_snapshot_omission(Some(UiSnapshotOmissionReason::DriverUnsupported));
        let outcome = session.ui_snapshot("obs-1").await.expect("ui_snapshot");
        assert_eq!(
            outcome,
            UiSnapshotOutcome::Unavailable {
                reason: UiSnapshotOmissionReason::DriverUnsupported
            }
        );
    }

    #[tokio::test]
    async fn injected_ui_snapshot_is_served_on_both_dereference_routes() {
        let provider = FakeProvider::new(VecDeque::new());
        let session = open(&provider).await;
        let tree = json!({
            "formatVersion": 1,
            "observationId": "ignored",
            "context": { "contextKind": "native", "contextId": "ctx-1", "documentEpoch": "e1" },
            "rootStableNodeIds": ["n1"],
            "nodes": [ { "stableNodeId": "n1", "role": "switch", "identifier": "wifi" } ],
        });

        // Route 1: observe() serves the injected tree.
        provider.handle().inject_ui_snapshot(Some(tree.clone()));
        let observation = session
            .observe(
                ObserveRequest {
                    wants: vec![ObserveWant::UiSnapshot],
                },
                None,
            )
            .await
            .expect("observe");
        let snapshot_ref = observation.ui_snapshot.expect("ui snapshot ref");
        let outcome = session
            .ui_snapshot(&observation.id)
            .await
            .expect("ui_snapshot");
        assert_eq!(
            outcome,
            UiSnapshotOutcome::Available {
                snapshot: tree.clone()
            }
        );
        let mut stream = session
            .fetch_evidence(&snapshot_ref.evidence)
            .await
            .expect("stream");
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            bytes.extend(chunk.expect("chunk"));
        }
        assert_eq!(bytes, serde_json::to_vec(&tree).expect("serialize"));

        // Route 2: a handle-synthesized observation carries the same
        // contract for embedding into scripted ActionResults.
        let synthesized = provider.handle().make_observation(Some(tree.clone()));
        assert!(synthesized.screenshot.is_some());
        let outcome = session
            .ui_snapshot(&synthesized.id)
            .await
            .expect("ui_snapshot");
        assert_eq!(outcome, UiSnapshotOutcome::Available { snapshot: tree });
    }

    #[tokio::test]
    async fn fetch_evidence_streams_bytes_and_verifies_sha256() {
        let provider = FakeProvider::new(VecDeque::new());
        let session = open(&provider).await;
        let observation = session
            .observe(
                ObserveRequest {
                    wants: vec![ObserveWant::Screenshot],
                },
                None,
            )
            .await
            .expect("observe");
        let asset = observation.screenshot.expect("screenshot asset");

        let mut stream = session.fetch_evidence(&asset).await.expect("stream");
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            bytes.extend(chunk.expect("chunk"));
        }
        assert_eq!(Some(hex_sha256(&bytes)), asset.sha256);

        let mut tampered = asset.clone();
        tampered.sha256 = Some("0".repeat(64));
        let error = session
            .fetch_evidence(&tampered)
            .await
            .err()
            .expect("integrity failure");
        assert_eq!(error.error_class, ErrorClass::ActionFailedFinal);
    }

    #[tokio::test]
    async fn record_verdict_caps_fail_closed() {
        let provider = FakeProvider::new(VecDeque::new());
        let session = open(&provider).await;

        let oversized = VerdictWrite {
            status: VerdictStatus::Pass,
            summary: "x".repeat(VERDICT_SUMMARY_MAX_CHARS + 1),
            evidence: Vec::new(),
        };
        let error = session.record_verdict(oversized).await.unwrap_err();
        assert_eq!(error.error_class, ErrorClass::BindArgumentsInvalid);

        let valid = VerdictWrite {
            status: VerdictStatus::Unknown,
            summary: "verify chain degraded".to_owned(),
            evidence: Vec::new(),
        };
        session.record_verdict(valid.clone()).await.expect("write");
        assert_eq!(provider.handle().recorded_verdicts(), vec![valid]);
    }

    #[tokio::test]
    async fn end_is_idempotent_and_breaks_other_methods() {
        let provider = FakeProvider::new(VecDeque::from([ScriptedOutcome::succeeded()]));
        let session = open(&provider).await;
        session
            .end(SessionOutcome::Completed, Some("done".to_owned()))
            .await
            .expect("end");
        session
            .end(SessionOutcome::Failed, None)
            .await
            .expect("idempotent end");
        assert_eq!(
            provider.handle().ended_outcome(),
            Some((SessionOutcome::Completed, Some("done".to_owned())))
        );
        let health = session.health().await.expect("health");
        assert!(!health.ok);
        let error = session.execute(call("late"), None).await.unwrap_err();
        assert_eq!(error.error_class, ErrorClass::TransportLost);
    }

    #[tokio::test]
    async fn open_session_fails_closed_on_digest_or_feature_drift() {
        let provider = FakeProvider::new(VecDeque::new());

        let mut drifted = provider.default_open_options();
        drifted.lockfile_digest = Hash::new(format!("sha256:{}", "f".repeat(64))).unwrap();
        let error = provider.open_session(drifted).await.err().expect("drift");
        assert_eq!(error.error_class, ErrorClass::CapabilityDrift);

        let mut unsatisfiable = provider.default_open_options();
        unsatisfiable
            .required_features
            .push(FeatureId::new("media.stream.v1").unwrap());
        let error = provider
            .open_session(unsatisfiable)
            .await
            .err()
            .expect("missing feature");
        assert_eq!(error.error_class, ErrorClass::CapabilityDrift);
    }
}
