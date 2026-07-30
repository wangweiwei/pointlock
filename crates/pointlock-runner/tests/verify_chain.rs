//! Verify-chain evaluation of element/visual predicates over the
//! FakeProvider (spine §6.3): the three chain rules (completed-positive →
//! pass, completed-negative → final fail, cannot-complete → next channel /
//! exhausted → unknown), ambiguity adjudication, degradedVerify marking
//! with the strict fold, the vision tail (stub and injected), and the
//! offline re-judge of element predicates from localized tree bytes.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use async_trait::async_trait;
use pointlock_ir::{
    ActionOutcome, ActionResult, Channel, FlowIR, Observation, RunLogPayload, StepIR,
    UiSnapshotOmissionReason, VerdictStatus, Viewport,
};
use pointlock_provider_kit::{FakeProvider, Provider, ProviderSession, ScriptedOutcome};
use pointlock_runner::{ResumeOptions, RunOptions, RunOutcome, Runner};
use pointlock_store::Store;
use pointlock_vision::{VisionRequest, VisionVerdict, VisionVerifier};
use serde_json::{Value, json};

// ─── Fixture helpers ────────────────────────────────────────────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique per-test store directory under the system temp dir, removed on
/// drop (same pattern as the runner tests).
struct TempStoreDir(PathBuf);

impl TempStoreDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-verify-chain-test-{tag}-{}-{}",
            std::process::id(),
            DIR_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&path).expect("create temp store dir");
        TempStoreDir(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempStoreDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn h64(fill: char) -> String {
    format!("sha256:{}", fill.to_string().repeat(64))
}

/// Recomputes and stores the per-step dual hashes and the flow irHash
/// (a minimal stand-in for the compiler seal phase).
fn seal(flow: &mut FlowIR) {
    for step in &mut flow.body {
        let effect = pointlock_ir::effect_hash(step);
        let judge = pointlock_ir::judge_hash(step);
        let StepIR::Action(action) = step else {
            panic!("fixtures use action steps only");
        };
        action.base.effect_hash = effect;
        action.base.judge_hash = judge;
    }
    flow.ir_hash = pointlock_ir::ir_hash(flow);
}

fn flow_fixture(lockfile_digest: &pointlock_ir::Hash, policy: &str, steps: Vec<Value>) -> FlowIR {
    let mut flow: FlowIR = serde_json::from_value(json!({
        "irVersion": 1,
        "flowId": "verify_chain_demo",
        "irHash": h64('e'),
        "provider": { "name": "devicerail", "version": "0.1.0" },
        "requiredFeatures": [],
        "lockfileDigest": lockfile_digest.as_str(),
        "params": [],
        "outputs": [],
        "body": steps,
        "verdictPolicy": policy,
        "sourceMap": [],
        "subflows": {}
    }))
    .expect("fixture is a valid FlowIR");
    seal(&mut flow);
    flow
}

fn action_step(id: &str, assertions: Vec<Value>) -> Value {
    json!({
        "kind": "action",
        "stepId": id,
        "effectHash": h64('0'),
        "judgeHash": h64('0'),
        "checkpoint": true,
        "effect": "mutating",
        "idempotent": false,
        "binding": { "attempts": [ {
            "channel": "uiTree",
            "actionName": "tapElement",
            "args": { "element": { "lit": { "identifier": id } } },
            "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
            "protection": "standard"
        } ] },
        "assertions": assertions
    })
}

/// A scripted succeeded terminal that carries an after observation.
fn succeeded_with_after(output: Value, after: Observation) -> ScriptedOutcome {
    ScriptedOutcome::Terminal(ActionOutcome::Succeeded {
        result: Box::new(ActionResult {
            call_id: String::new(),
            started_at_ms: 0,
            finished_at_ms: 0,
            output,
            before: None,
            after: Some(after),
            evidence: Vec::new(),
            execution: None,
        }),
    })
}

/// A DeviceRail-shaped UiSnapshot document over the given nodes.
fn tree(nodes: Value) -> Value {
    json!({
        "formatVersion": 1,
        "observationId": "stamped-by-fake",
        "context": { "contextKind": "native", "contextId": "ctx-1", "documentEpoch": "e1" },
        "rootStableNodeIds": ["n1"],
        "nodes": nodes
    })
}

/// An after observation whose uiSnapshot was legitimately omitted and that
/// carries no screenshot (both channels lack material).
fn omitted_observation() -> Observation {
    Observation {
        id: "obs-omitted".to_owned(),
        device_id: "fake-device-1".to_owned(),
        captured_at_ms: 1,
        viewport: Viewport {
            width: 1080,
            height: 2400,
            scale_factor: 2.0,
        },
        screenshot: None,
        screenshot_omission: None,
        ui_snapshot: None,
        ui_snapshot_omission: Some(UiSnapshotOmissionReason::ProtectedAction),
        metadata: BTreeMap::new(),
    }
}

async fn open(provider: &FakeProvider) -> Box<dyn ProviderSession> {
    provider
        .open_session(provider.default_open_options())
        .await
        .expect("open_session")
}

fn run_opts(run_id: &str) -> RunOptions {
    let mut opts = RunOptions::new("fake-device-1");
    opts.run_id = Some(run_id.to_owned());
    opts
}

/// Runs a single-step flow and returns (flow verdict, assertion outcomes,
/// step verdicts) from the ledger.
async fn run_flow(
    tag: &str,
    provider: &FakeProvider,
    flow: &FlowIR,
    opts: RunOptions,
) -> (
    pointlock_ir::Verdict,
    Vec<pointlock_ir::AssertionOutcomeRecord>,
    Vec<pointlock_ir::Verdict>,
) {
    let dir = TempStoreDir::new(tag);
    let mut store = Store::open(dir.path()).expect("open store");
    let run_id = opts.run_id.clone().expect("fixture run id");
    let session = open(provider).await;
    let outcome = Runner::run(flow, json!({}), session, &mut store, opts)
        .await
        .expect("run");
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished with a verdict, got {outcome:?}");
    };
    let events = store.events(&run_id).expect("events");
    let outcomes = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::AssertionEvaluated { outcome } => Some(outcome.clone()),
            _ => None,
        })
        .collect();
    // Step verdicts only: the flow fold travels in `runFinished`.
    let step_verdicts: Vec<pointlock_ir::Verdict> = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded { verdict, .. } => Some(verdict.clone()),
            _ => None,
        })
        .collect();
    (verdict, outcomes, step_verdicts)
}

/// A programmable vision verifier that counts its invocations.
struct ScriptedVision {
    calls: AtomicUsize,
    status: VerdictStatus,
    reason: &'static str,
}

impl ScriptedVision {
    fn new(status: VerdictStatus, reason: &'static str) -> Arc<Self> {
        Arc::new(ScriptedVision {
            calls: AtomicUsize::new(0),
            status,
            reason,
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl VisionVerifier for ScriptedVision {
    async fn verify(&self, _request: VisionRequest<'_>) -> VisionVerdict {
        self.calls.fetch_add(1, Ordering::SeqCst);
        VisionVerdict {
            status: self.status,
            reason: self.reason.to_owned(),
        }
    }
}

// ─── (a) uiTree completes: present pass / absent pass ───────────────────────

#[tokio::test]
async fn element_state_present_passes_on_the_ui_tree_channel() {
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(Some(tree(json!([
        { "stableNodeId": "n1", "role": "switch", "identifier": "wifi_toggle",
          "name": "Wi-Fi", "enabled": true, "hittable": true }
    ]))));
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "s1",
            vec![json!({
                "assertId": "a1",
                "predicate": { "type": "elementState",
                               "selector": { "identifier": "wifi_toggle" },
                               "state": "present" },
                "verifyVia": ["uiTree"],
                "onMissingInput": "unknown"
            })],
        )],
    );
    let (verdict, outcomes, _) =
        run_flow("present", &provider, &flow, run_opts("run-present")).await;
    assert_eq!(verdict.status, VerdictStatus::Pass);
    assert!(!verdict.degraded);
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].result, VerdictStatus::Pass);
    // The completing channel is recorded (spine §6.3 rule 1).
    assert_eq!(outcomes[0].channel, Some(Channel::UiTree));
}

#[tokio::test]
async fn element_state_absent_passes_when_nothing_matches() {
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(Some(tree(json!([
        { "stableNodeId": "n1", "role": "button", "identifier": "close" }
    ]))));
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "s1",
            vec![json!({
                "assertId": "a1",
                "predicate": { "type": "elementState",
                               "selector": { "identifier": "error_banner" },
                               "state": "absent" },
                "verifyVia": ["uiTree"],
                "onMissingInput": "unknown"
            })],
        )],
    );
    let (verdict, outcomes, _) = run_flow("absent", &provider, &flow, run_opts("run-absent")).await;
    // Completed with zero matches: absent *holds* — completed-and-true,
    // not "could not see" (the distinction the task pins).
    assert_eq!(verdict.status, VerdictStatus::Pass);
    assert_eq!(outcomes[0].result, VerdictStatus::Pass);
    assert_eq!(outcomes[0].channel, Some(Channel::UiTree));
}

// ─── (b) null platform state: channel cannot complete → unknown ────────────

#[tokio::test]
async fn enabled_null_exhausts_the_single_channel_chain_to_unknown() {
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(Some(tree(json!([
        { "stableNodeId": "n1", "role": "switch", "identifier": "wifi_toggle",
          "enabled": null }
    ]))));
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "s1",
            vec![json!({
                "assertId": "a1",
                "predicate": { "type": "elementState",
                               "selector": { "identifier": "wifi_toggle" },
                               "state": "enabled" },
                "verifyVia": ["uiTree"],
                "onMissingInput": "unknown"
            })],
        )],
    );
    let (verdict, outcomes, _) = run_flow("null", &provider, &flow, run_opts("run-null")).await;
    // null is unknown, never coerced (DeviceRail contract): the uiTree
    // channel cannot complete, the chain has no next channel → unknown.
    assert_eq!(verdict.status, VerdictStatus::Unknown);
    assert!(!verdict.degraded, "no channel completed: nothing degraded");
    assert_eq!(outcomes[0].result, VerdictStatus::Unknown);
    assert_eq!(outcomes[0].channel, None);
    assert!(
        outcomes[0].reason.contains("enabled=null"),
        "got {}",
        outcomes[0].reason
    );
}

// ─── (c) completed negative is final: vision is never consulted (R5) ───────

#[tokio::test]
async fn element_text_fail_is_final_and_never_consults_vision() {
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(Some(tree(json!([
        { "stableNodeId": "n1", "role": "label", "identifier": "status",
          "text": "Offline" }
    ]))));
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "s1",
            vec![json!({
                "assertId": "a1",
                "predicate": { "type": "elementText",
                               "selector": { "identifier": "status" },
                               "match": { "value": "connected", "mode": "contains",
                                          "caseSensitive": true } },
                "verifyVia": ["uiTree", "vision"],
                "visionPrompt": "the status label says Connected",
                "onMissingInput": "unknown"
            })],
        )],
    );
    let vision = ScriptedVision::new(VerdictStatus::Pass, "vision would have said pass");
    let mut opts = run_opts("run-text-fail");
    opts.vision = Some(vision.clone());
    let (verdict, outcomes, _) = run_flow("text-fail", &provider, &flow, opts).await;
    // uiTree completed with the predicate false → fail, terminal. The
    // degradation chain solves "can't see", not "don't like the answer".
    assert_eq!(verdict.status, VerdictStatus::Fail);
    assert_eq!(outcomes[0].result, VerdictStatus::Fail);
    assert_eq!(outcomes[0].channel, Some(Channel::UiTree));
    assert_eq!(vision.calls(), 0, "a completed negative is final (R5)");
}

// ─── (d) ambiguity: completed negative aligned with element_ambiguous ──────

#[tokio::test]
async fn ambiguous_selector_fails_unique_match_predicates() {
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(Some(tree(json!([
        { "stableNodeId": "n1", "role": "switch", "name": "toggle", "enabled": true },
        { "stableNodeId": "n2", "role": "switch", "name": "toggle", "enabled": true }
    ]))));
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "s1",
            vec![json!({
                "assertId": "a1",
                "predicate": { "type": "elementState",
                               "selector": { "role": "switch", "name": "toggle" },
                               "state": "enabled" },
                "verifyVia": ["uiTree"],
                "onMissingInput": "unknown"
            })],
        )],
    );
    let (verdict, outcomes, _) =
        run_flow("ambiguous", &provider, &flow, run_opts("run-ambiguous")).await;
    assert_eq!(verdict.status, VerdictStatus::Fail);
    assert_eq!(outcomes[0].result, VerdictStatus::Fail);
    assert!(
        outcomes[0].reason.contains("ambiguous"),
        "got {}",
        outcomes[0].reason
    );
}

// ─── (e) omission / missing material → unknown ─────────────────────────────

#[tokio::test]
async fn ui_snapshot_omission_yields_unknown() {
    let provider = FakeProvider::new(VecDeque::new());
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), omitted_observation()));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "s1",
            vec![json!({
                "assertId": "a1",
                "predicate": { "type": "elementState",
                               "selector": { "identifier": "wifi_toggle" },
                               "state": "present" },
                "verifyVia": ["uiTree"],
                "onMissingInput": "unknown"
            })],
        )],
    );
    let (verdict, outcomes, _) =
        run_flow("omission", &provider, &flow, run_opts("run-omission")).await;
    assert_eq!(verdict.status, VerdictStatus::Unknown);
    assert_eq!(outcomes[0].result, VerdictStatus::Unknown);
    assert_eq!(outcomes[0].channel, None);
    assert!(
        outcomes[0].reason.contains("omitted"),
        "got {}",
        outcomes[0].reason
    );
}

#[tokio::test]
async fn visual_predicate_with_stub_equivalent_verifier_is_unknown() {
    let provider = FakeProvider::new(VecDeque::new());
    // A screenshot is present; no verifier is injected (None ≡ stub).
    let after = provider.handle().make_observation(None);
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "s1",
            vec![json!({
                "assertId": "a1",
                "predicate": { "type": "visual",
                               "prompt": "the Wi-Fi settings page is open" },
                "verifyVia": ["vision"],
                "onMissingInput": "unknown"
            })],
        )],
    );
    let (verdict, outcomes, _) =
        run_flow("visual-stub", &provider, &flow, run_opts("run-visual")).await;
    assert_eq!(verdict.status, VerdictStatus::Unknown);
    assert_eq!(outcomes[0].result, VerdictStatus::Unknown);
    assert!(
        outcomes[0]
            .reason
            .contains("vision verifier not configured"),
        "got {}",
        outcomes[0].reason
    );
}

#[tokio::test]
async fn chain_exhaustion_reports_every_channel_gap() {
    let provider = FakeProvider::new(VecDeque::new());
    // First channel: uiSnapshot omitted. Tail: a screenshot exists but the
    // vision verifier answers unknown (an injected always-unknown verifier
    // — the stub's behavior, made observable). Neither channel completes.
    let mut after = provider.handle().make_observation(None);
    after.ui_snapshot_omission = Some(UiSnapshotOmissionReason::ProtectedAction);
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "s1",
            vec![json!({
                "assertId": "a1",
                "predicate": { "type": "elementState",
                               "selector": { "identifier": "wifi_toggle" },
                               "state": "visible" },
                "verifyVia": ["uiTree", "vision"],
                "visionPrompt": "the Wi-Fi toggle is visible on screen",
                "onMissingInput": "unknown"
            })],
        )],
    );
    let vision = ScriptedVision::new(VerdictStatus::Unknown, "cannot tell from this frame");
    let mut opts = run_opts("run-exhausted");
    opts.vision = Some(vision.clone());
    let (verdict, outcomes, _) = run_flow("exhausted", &provider, &flow, opts).await;
    // The vision tail *was* consulted (the screenshot is available) and
    // answered unknown: cannot-complete, chain exhausted.
    assert_eq!(vision.calls(), 1);
    assert_eq!(verdict.status, VerdictStatus::Unknown);
    assert_eq!(outcomes[0].result, VerdictStatus::Unknown);
    assert_eq!(outcomes[0].channel, None);
    // The reason aggregates each channel's incompleteness.
    assert!(
        outcomes[0].reason.contains("uiTree:"),
        "got {}",
        outcomes[0].reason
    );
    assert!(
        outcomes[0]
            .reason
            .contains("vision: cannot tell from this frame"),
        "got {}",
        outcomes[0].reason
    );
}

// ─── (f) degradedVerify: non-preferred channel answers; strict folds ───────

#[tokio::test]
async fn non_preferred_channel_pass_marks_degraded_verify() {
    let provider = FakeProvider::new(VecDeque::new());
    // dom cannot complete in M1; uiTree (second in the chain) answers.
    let after = provider.handle().make_observation(Some(tree(json!([
        { "stableNodeId": "n1", "role": "switch", "identifier": "wifi_toggle" }
    ]))));
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "s1",
            vec![json!({
                "assertId": "a1",
                "predicate": { "type": "elementState",
                               "selector": { "identifier": "wifi_toggle" },
                               "state": "present" },
                "verifyVia": ["dom", "uiTree"],
                "onMissingInput": "unknown"
            })],
        )],
    );
    let (verdict, outcomes, step_verdicts) =
        run_flow("degraded", &provider, &flow, run_opts("run-degraded")).await;
    assert_eq!(outcomes[0].result, VerdictStatus::Pass);
    assert_eq!(outcomes[0].channel, Some(Channel::UiTree));
    assert!(
        outcomes[0].reason.contains("degradedVerify"),
        "got {}",
        outcomes[0].reason
    );
    // standard policy: the pass survives, flagged degraded (spine §6.3).
    assert_eq!(step_verdicts[0].status, VerdictStatus::Pass);
    assert!(step_verdicts[0].degraded);
    assert_eq!(verdict.status, VerdictStatus::Pass);
    assert!(verdict.degraded);
}

#[tokio::test]
async fn strict_policy_folds_a_degraded_verify_pass_to_unknown() {
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(Some(tree(json!([
        { "stableNodeId": "n1", "role": "switch", "identifier": "wifi_toggle" }
    ]))));
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "strict",
        vec![action_step(
            "s1",
            vec![json!({
                "assertId": "a1",
                "predicate": { "type": "elementState",
                               "selector": { "identifier": "wifi_toggle" },
                               "state": "present" },
                "verifyVia": ["dom", "uiTree"],
                "onMissingInput": "unknown"
            })],
        )],
    );
    let (verdict, outcomes, step_verdicts) =
        run_flow("strict", &provider, &flow, run_opts("run-strict")).await;
    // The assertion itself passed (on the degraded channel)…
    assert_eq!(outcomes[0].result, VerdictStatus::Pass);
    // …but strict folds the degraded step pass to unknown (spine §6.3).
    assert_eq!(step_verdicts[0].status, VerdictStatus::Unknown);
    assert!(step_verdicts[0].degraded);
    assert_eq!(verdict.status, VerdictStatus::Unknown);
}

#[tokio::test]
async fn vision_tail_pass_through_injected_verifier_is_a_degraded_pass() {
    let provider = FakeProvider::new(VecDeque::new());
    // uiTree omitted; the vision tail answers pass via the injected
    // verifier (RunOptions::vision) over the localized screenshot bytes.
    let mut after = provider.handle().make_observation(None);
    after.ui_snapshot_omission = Some(UiSnapshotOmissionReason::DriverUnsupported);
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "s1",
            vec![json!({
                "assertId": "a1",
                "predicate": { "type": "elementState",
                               "selector": { "identifier": "wifi_toggle" },
                               "state": "present" },
                "verifyVia": ["uiTree", "vision"],
                "visionPrompt": "the Wi-Fi toggle is present on screen",
                "onMissingInput": "unknown"
            })],
        )],
    );
    let vision = ScriptedVision::new(VerdictStatus::Pass, "toggle clearly visible");
    let mut opts = run_opts("run-vision-pass");
    opts.vision = Some(vision.clone());
    let (verdict, outcomes, step_verdicts) = run_flow("vision-pass", &provider, &flow, opts).await;
    assert_eq!(vision.calls(), 1);
    assert_eq!(outcomes[0].result, VerdictStatus::Pass);
    assert_eq!(outcomes[0].channel, Some(Channel::Vision));
    assert_eq!(step_verdicts[0].status, VerdictStatus::Pass);
    assert!(
        step_verdicts[0].degraded,
        "vision answered as the chain tail"
    );
    assert_eq!(verdict.status, VerdictStatus::Pass);
    assert!(verdict.degraded);
}

// ─── (g) offline re-judge: element predicates over localized tree bytes ────

#[tokio::test]
async fn judge_dirty_element_rejudge_replays_local_tree_bytes_without_a_session() {
    let dir = TempStoreDir::new("rejudge-element");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(Some(tree(json!([
        { "stableNodeId": "n1", "role": "label", "identifier": "status",
          "text": "Wi-Fi enabled" }
    ]))));
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));

    let old_assert = json!({
        "assertId": "a1",
        "predicate": { "type": "elementState",
                       "selector": { "identifier": "status" },
                       "state": "present" },
        "verifyVia": ["uiTree"],
        "onMissingInput": "unknown"
    });
    let old_flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step("s1", vec![old_assert])],
    );
    let session = open(&provider).await;
    let outcome = Runner::run(
        &old_flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-rejudge-element"),
    )
    .await
    .expect("run");
    assert!(matches!(outcome, RunOutcome::Finished { verdict: Some(_) }));
    assert_eq!(provider.handle().dispatched_call_ids().len(), 1);
    let events_before = store.events("run-rejudge-element").expect("events").len();

    // Judge-domain repair: tighten to an elementText check that holds
    // against the archived tree. effectHash unchanged ⇒ judgeDirty.
    let new_assert = json!({
        "assertId": "a1",
        "predicate": { "type": "elementText",
                       "selector": { "identifier": "status" },
                       "match": { "value": "wi-fi", "mode": "contains",
                                  "caseSensitive": false } },
        "verifyVia": ["uiTree"],
        "onMissingInput": "unknown"
    });
    let new_flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step("s1", vec![new_assert])],
    );
    assert_eq!(
        new_flow.body[0].base().effect_hash,
        old_flow.body[0].base().effect_hash
    );
    assert_ne!(
        new_flow.body[0].base().judge_hash,
        old_flow.body[0].base().judge_hash
    );

    // Sever the session route: if the re-judge touched `ui_snapshot`, it
    // would see a typed omission and fold to unknown. A pass can only
    // come from the localized bytes — pure function over local evidence.
    provider
        .handle()
        .set_ui_snapshot_omission(Some(UiSnapshotOmissionReason::DriverUnsupported));

    let session = open(&provider).await;
    let outcome = Runner::resume(
        &new_flow,
        "run-rejudge-element",
        session,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("resume");
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished with a verdict, got {outcome:?}");
    };
    assert_eq!(verdict.status, VerdictStatus::Pass);

    // Zero new dispatches: the re-judge is offline (07 §5.3).
    assert_eq!(provider.handle().dispatched_call_ids().len(), 1);

    // Resume segment: runResumed → superseding verdict → runFinished.
    let events = store.events("run-rejudge-element").expect("events");
    let segment: Vec<&'static str> = events[events_before..]
        .iter()
        .map(|event| event.payload.event_type())
        .collect();
    assert_eq!(
        segment,
        vec!["runResumed", "verdictRecorded", "runFinished"]
    );
    let rejudged = events[events_before..]
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded { verdict, .. } => Some(verdict.clone()),
            _ => None,
        })
        .expect("re-judged verdict");
    assert_eq!(rejudged.status, VerdictStatus::Pass);
    assert!(rejudged.supersedes.is_some());
    assert!(
        rejudged.summary.contains("offline re-judge"),
        "got {}",
        rejudged.summary
    );
}

// ─── (h) offline re-judge: the vision tail consults this segment's verifier ─

/// M3a-W4 regression: `ResumeOptions::vision` must reach the offline
/// re-judge leg (align/rejudge), not just live execution — a configured
/// verifier judging the *archived* screenshot bytes, still zero device
/// I/O. Before the fix the rejudge hardcoded `None` and a judgeDirty
/// visual assertion silently degraded to "vision verifier not
/// configured" even under `--vision anthropic`.
#[tokio::test]
async fn judge_dirty_visual_rejudge_consults_the_segment_verifier() {
    let dir = TempStoreDir::new("rejudge-vision");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    // A screenshot travels with the after observation and is localized
    // during the original run's observing phase.
    let after = provider.handle().make_observation(None);
    provider
        .handle()
        .push_script(succeeded_with_after(json!({ "ok": true }), after));

    let expr_ok = json!({
        "assertId": "a1",
        "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
            { "ref": "steps.s1.output.ok" }, { "lit": true } ] } },
        "verifyVia": [],
        "onMissingInput": "unknown"
    });
    let old_flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step("s1", vec![expr_ok.clone()])],
    );
    let session = open(&provider).await;
    let outcome = Runner::run(
        &old_flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-rejudge-vision"),
    )
    .await
    .expect("run");
    assert!(matches!(outcome, RunOutcome::Finished { verdict: Some(_) }));
    assert_eq!(provider.handle().dispatched_call_ids().len(), 1);
    let events_before = store.events("run-rejudge-vision").expect("events").len();

    // Judge-domain repair: add a visual assertion. effectHash unchanged
    // ⇒ judgeDirty, offline re-judge.
    let visual = json!({
        "assertId": "a2",
        "predicate": { "type": "visual",
                       "prompt": "the Wi-Fi settings page is visibly open" },
        "verifyVia": ["vision"],
        "onMissingInput": "unknown"
    });
    let new_flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step("s1", vec![expr_ok, visual])],
    );
    assert_eq!(
        new_flow.body[0].base().effect_hash,
        old_flow.body[0].base().effect_hash
    );
    assert_ne!(
        new_flow.body[0].base().judge_hash,
        old_flow.body[0].base().judge_hash
    );

    let vision = ScriptedVision::new(VerdictStatus::Pass, "the page is visibly open");
    let session = open(&provider).await;
    let outcome = Runner::resume(
        &new_flow,
        "run-rejudge-vision",
        session,
        &mut store,
        ResumeOptions {
            vision: Some(vision.clone()),
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("resume");
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished with a verdict, got {outcome:?}");
    };
    assert_eq!(verdict.status, VerdictStatus::Pass);

    // The verifier WAS consulted, and offline: zero new dispatches.
    assert_eq!(vision.calls(), 1);
    assert_eq!(provider.handle().dispatched_call_ids().len(), 1);

    let events = store.events("run-rejudge-vision").expect("events");
    let rejudged = events[events_before..]
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded { verdict, .. } => Some(verdict.clone()),
            _ => None,
        })
        .expect("re-judged verdict");
    assert_eq!(rejudged.status, VerdictStatus::Pass);
    assert!(rejudged.supersedes.is_some());
}

// ─── viewport quarantine (M3a viewport close review) ────────────────────────

/// A non-finite `scaleFactor` must never reach the ledger: serde_json
/// writes it as `null`, which would poison every subsequent read of the
/// run (permanent refold failure). The SPI ingestion quarantine records
/// the provider's "success" as a final failure with a precise code — the
/// ledger stays fully readable and exact, and no observation is recorded.
#[tokio::test]
async fn a_non_finite_viewport_scale_is_quarantined_not_persisted() {
    let dir = TempStoreDir::new("viewport-nan");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let mut after = provider.handle().make_observation(None);
    after.viewport.scale_factor = f64::NAN;
    provider
        .handle()
        .push_script(succeeded_with_after(json!({ "ok": true }), after));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step("s1", vec![])],
    );
    let session = open(&provider).await;
    let _ = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-viewport-nan"),
    )
    .await
    .expect("run must not error");

    // The ledger reads back whole and exact (I1).
    let view = store
        .verify_checkpoint("run-viewport-nan")
        .expect("ledger stays readable and exact");

    // The settled terminal is the quarantine failure, not the poisoned
    // success; no observation was recorded at all.
    let events = store.events("run-viewport-nan").expect("events");
    let settled_code = events.iter().find_map(|event| match &event.payload {
        RunLogPayload::ActionSettled {
            outcome: pointlock_ir::ActionOutcome::Failed { error },
            ..
        } => Some(error.code.clone()),
        _ => None,
    });
    assert_eq!(
        settled_code.as_deref(),
        Some("observation_viewport_invalid")
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, RunLogPayload::ObservationRecorded { .. })),
        "no observation may be recorded from a quarantined terminal"
    );
    assert!(
        view.completed
            .iter()
            .all(|record| record.observations.is_empty())
    );
}

// ─── providerStateSummary capture (07 §2.2, incorporated 2026-07-18) ────────

/// A fail-verdict exit carries the failure-instant provider profile;
/// the passing sibling carries none. The gate is intensional (verdict
/// in force at exit), attached centrally at append time.
#[tokio::test]
async fn a_fail_verdict_exit_carries_the_provider_state_summary() {
    let dir = TempStoreDir::new("summary-fail");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let after_one = provider.handle().make_observation(None);
    let after_two = provider.handle().make_observation(None);
    provider
        .handle()
        .push_script(succeeded_with_after(json!({ "ok": true }), after_one));
    provider
        .handle()
        .push_script(succeeded_with_after(json!({ "ok": false }), after_two));
    let pass_assert = json!({
        "assertId": "a1",
        "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
            { "ref": "steps.s1.output.ok" }, { "lit": true } ] } },
        "verifyVia": [],
        "onMissingInput": "unknown"
    });
    let fail_assert = json!({
        "assertId": "a2",
        "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
            { "ref": "steps.s2.output.ok" }, { "lit": true } ] } },
        "verifyVia": [],
        "onMissingInput": "unknown"
    });
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![
            action_step("s1", vec![pass_assert]),
            action_step("s2", vec![fail_assert]),
        ],
    );
    let session = open(&provider).await;
    let _ = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-summary"),
    )
    .await
    .expect("run");

    let events = store.events("run-summary").expect("events");
    let mut summaries = Vec::new();
    for event in &events {
        if let RunLogPayload::StepExited {
            provider_state_summary,
            ..
        } = &event.payload
        {
            summaries.push(provider_state_summary.clone());
        }
    }
    assert_eq!(summaries.len(), 2);
    assert!(summaries[0].is_none(), "pass exit must carry no summary");
    let summary = summaries[1].as_ref().expect("fail exit carries one");
    assert_eq!(summary.device_id, "fake-device-1");
    assert!(summary.health.ok, "the fake session answers healthy");
    assert!(!summary.session_lineage.is_empty());
    assert!(summary.event_cursor.is_some());

    // The ledger with the new field still folds exactly (I1).
    store.verify_checkpoint("run-summary").expect("exact");
}

/// An unknown-verdict exit carries the profile too (fail and unknown are
/// both "the machine could not confirm" instants), and a retried-then-
/// passing span discards the earlier round's capture: the final pass
/// exit carries none.
#[tokio::test]
async fn unknown_exits_carry_and_pass_exits_discard_the_summary() {
    let dir = TempStoreDir::new("summary-unknown");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(None);
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    // Missing ref ⇒ onMissingInput: unknown.
    let unknown_assert = json!({
        "assertId": "a1",
        "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
            { "ref": "steps.s1.output.missing" }, { "lit": true } ] } },
        "verifyVia": [],
        "onMissingInput": "unknown"
    });
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step("s1", vec![unknown_assert])],
    );
    let session = open(&provider).await;
    let _ = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-summary-unknown"),
    )
    .await
    .expect("run");
    let events = store.events("run-summary-unknown").expect("events");
    let exit_summary = events.iter().find_map(|event| match &event.payload {
        RunLogPayload::StepExited {
            provider_state_summary,
            ..
        } => Some(provider_state_summary.clone()),
        _ => None,
    });
    assert!(
        exit_summary.expect("one exit").is_some(),
        "unknown-verdict exit must carry the profile"
    );
    store
        .verify_checkpoint("run-summary-unknown")
        .expect("exact");
}

// ─── settlement-evidence manifest (item ③, 2026-07-18) ──────────────────────

/// Auxiliary settlement evidence rides the judgment: a localizable asset
/// lands in `verdictRecorded.localized` and merges into the checkpoint
/// record's evidence; an unlocalizable one becomes a TYPED gap on the
/// same payload — never a silent omission. I1 exact throughout.
#[tokio::test]
async fn settlement_evidence_manifests_and_gaps_ride_the_verdict() {
    let expr_ok = json!({
        "assertId": "a1",
        "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
            { "ref": "steps.s1.output.ok" }, { "lit": true } ] } },
        "verifyVia": [],
        "onMissingInput": "unknown"
    });

    let aux_outcome = |provider: &FakeProvider| {
        // A registered screenshot asset doubles as the auxiliary
        // settlement evidence (the fake serves registered assets).
        let observation = provider.handle().make_observation(None);
        let aux = observation.screenshot.clone().expect("registered asset");
        ScriptedOutcome::Terminal(pointlock_ir::ActionOutcome::Succeeded {
            result: Box::new(pointlock_ir::ActionResult {
                call_id: String::new(),
                started_at_ms: 0,
                finished_at_ms: 0,
                output: json!({ "ok": true }),
                before: None,
                after: None,
                evidence: vec![aux],
                execution: None,
            }),
        })
    };

    // Run A: the asset localizes — manifest non-empty, record merged.
    let dir = TempStoreDir::new("manifest-happy");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let outcome_script = aux_outcome(&provider);
    provider.handle().push_script(outcome_script);
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step("s1", vec![expr_ok.clone()])],
    );
    let session = open(&provider).await;
    let _ = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-mani"))
        .await
        .expect("run");
    let events = store.events("run-mani").expect("events");
    let (localized, gaps) = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded {
                localized,
                localization_gaps,
                ..
            } => Some((localized.clone(), localization_gaps.clone())),
            _ => None,
        })
        .expect("verdict recorded");
    assert_eq!(localized.len(), 1, "the aux asset localized");
    assert!(gaps.is_empty());
    let view = store.verify_checkpoint("run-mani").expect("exact");
    assert!(
        view.completed[0]
            .evidence
            .iter()
            .any(|entry| entry.sha256 == localized[0].sha256),
        "the manifest merged into the record's evidence"
    );

    // Run B: fetch unsupported — a typed gap, nothing merged.
    let dir_b = TempStoreDir::new("manifest-gap");
    let mut store_b = Store::open(dir_b.path()).expect("open store");
    let provider_b = FakeProvider::new(VecDeque::new());
    let outcome_script_b = aux_outcome(&provider_b);
    provider_b.handle().push_script(outcome_script_b);
    provider_b
        .handle()
        .set_fetch_evidence_unsupported(Some("no byte channel".into()));
    let flow_b = flow_fixture(
        &provider_b.lockfile().digest,
        "standard",
        vec![action_step("s1", vec![expr_ok])],
    );
    let session_b = open(&provider_b).await;
    let _ = Runner::run(
        &flow_b,
        json!({}),
        session_b,
        &mut store_b,
        run_opts("run-mani-gap"),
    )
    .await
    .expect("run");
    let events_b = store_b.events("run-mani-gap").expect("events");
    let (localized_b, gaps_b) = events_b
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded {
                localized,
                localization_gaps,
                ..
            } => Some((localized.clone(), localization_gaps.clone())),
            _ => None,
        })
        .expect("verdict recorded");
    assert!(localized_b.is_empty());
    assert_eq!(gaps_b.len(), 1, "the failure is a typed gap");
    assert!(!gaps_b[0].reason.is_empty());
    let view_b = store_b.verify_checkpoint("run-mani-gap").expect("exact");
    assert!(
        view_b.completed[0].evidence.is_empty(),
        "nothing merges on a gap"
    );
}

/// An UNVERIFIED step (no assertions ⇒ no verdict, R4) must not drop
/// its settlement-evidence manifest: it rides the `stepExited` instead,
/// gaps included, and merges into the record under the same rule.
#[tokio::test]
async fn an_unverified_exit_carries_the_settlement_manifest() {
    let dir = TempStoreDir::new("manifest-unverified");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let observation = provider.handle().make_observation(None);
    let aux = observation.screenshot.clone().expect("registered asset");
    provider.handle().push_script(ScriptedOutcome::Terminal(
        pointlock_ir::ActionOutcome::Succeeded {
            result: Box::new(pointlock_ir::ActionResult {
                call_id: String::new(),
                started_at_ms: 0,
                finished_at_ms: 0,
                output: json!({ "ok": true }),
                before: None,
                after: None,
                evidence: vec![aux],
                execution: None,
            }),
        },
    ));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step("s1", vec![])],
    );
    let session = open(&provider).await;
    let _ = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-mani-unverified"),
    )
    .await
    .expect("run");

    let events = store.events("run-mani-unverified").expect("events");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, RunLogPayload::VerdictRecorded { .. })),
        "an unverified step records no verdict"
    );
    let exit_localized = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::StepExited { localized, .. } if !localized.is_empty() => {
                Some(localized.clone())
            }
            _ => None,
        })
        .expect("the exit carries the manifest");
    assert_eq!(exit_localized.len(), 1);
    let view = store
        .verify_checkpoint("run-mani-unverified")
        .expect("exact");
    assert!(
        view.completed[0]
            .evidence
            .iter()
            .any(|entry| entry.sha256 == exit_localized[0].sha256),
        "the exit-borne manifest merged into the record"
    );
}

// ─── timeout confirmation by observation (spine §5 / 07 §4.3) ───────────────

/// 「先 reconcile/observe 确认，确认不了 → unknown 路径」. A recorded
/// `timedOut` is a certain terminal — reconcile returns it verbatim — so
/// the observe half is the confirmation channel: a fresh observation, the
/// step's own assertions over it. All three verdicts in one place:
/// - the element the assertion looks for IS there → the effect landed →
///   pass, with the summary naming the timeout;
/// - the world is VISIBLY without it → fail;
/// - no observable material (assertion needs an output the timeout never
///   produced) → unknown, the path every timeout took before.
#[tokio::test]
async fn a_timed_out_act_is_confirmed_by_a_fresh_observation() {
    // Confirmed: the world shows the submitted marker.
    let provider = FakeProvider::new(VecDeque::from([ScriptedOutcome::timed_out()]));
    provider.handle().inject_ui_snapshot(Some(tree(json!([
        { "stableNodeId": "n1", "role": "text", "identifier": "order_done",
          "name": "订单已提交", "hittable": true }
    ]))));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "submit",
            vec![json!({
                "assertId": "confirmed",
                "predicate": { "type": "elementState",
                               "selector": { "identifier": "order_done" },
                               "state": "present" },
                "verifyVia": ["uiTree"],
                "onMissingInput": "unknown"
            })],
        )],
    );
    let (verdict, outcomes, _) =
        run_flow("timeout-pass", &provider, &flow, run_opts("run-top")).await;
    assert_eq!(verdict.status, VerdictStatus::Pass, "{verdict:?}");
    assert_eq!(outcomes[0].result, VerdictStatus::Pass);

    // Refuted: the world is visibly without the marker.
    let provider = FakeProvider::new(VecDeque::from([ScriptedOutcome::timed_out()]));
    provider.handle().inject_ui_snapshot(Some(tree(json!([
        { "stableNodeId": "n1", "role": "text", "identifier": "error_banner",
          "name": "提交失败", "hittable": true }
    ]))));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "submit",
            vec![json!({
                "assertId": "confirmed",
                "predicate": { "type": "elementState",
                               "selector": { "identifier": "order_done" },
                               "state": "present" },
                "verifyVia": ["uiTree"],
                "onMissingInput": "unknown"
            })],
        )],
    );
    let (verdict, outcomes, _) =
        run_flow("timeout-fail", &provider, &flow, run_opts("run-tof")).await;
    assert_eq!(verdict.status, VerdictStatus::Fail, "{verdict:?}");
    assert_eq!(outcomes[0].result, VerdictStatus::Fail);

    // Unconfirmable: the only assertion reads the output the timeout never
    // produced — unknown, exactly the old path.
    let provider = FakeProvider::new(VecDeque::from([ScriptedOutcome::timed_out()]));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        "standard",
        vec![action_step(
            "submit",
            vec![json!({
                "assertId": "confirmed",
                "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
                    { "ref": "steps.submit.output.ok" }, { "lit": true }
                ] } },
                "verifyVia": [],
                "onMissingInput": "unknown"
            })],
        )],
    );
    let (verdict, outcomes, _) =
        run_flow("timeout-unknown", &provider, &flow, run_opts("run-tou")).await;
    assert_eq!(verdict.status, VerdictStatus::Unknown, "{verdict:?}");
    assert_eq!(outcomes[0].result, VerdictStatus::Unknown);
}
