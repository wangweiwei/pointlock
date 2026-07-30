//! End-to-end runner tests over the FakeProvider and a temp-dir store:
//! ledger order discipline, verdict folding, in-attempt retry, stop /
//! resume with alignment, offline re-judge, and the crash-window
//! reconcile.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use async_trait::async_trait;
use pointlock_ir::{
    ActionName, ActionOutcome, ActionOutcomeKind, ActionResult, AlignmentClass, AssetRef,
    BindingState, EventCursor, FlowIR, Hash, Observation, PathFrame, ReconcileResult, RunLogEvent,
    RunLogPayload, StepIR, StepId, StepState, VerdictStatus,
};
use pointlock_provider_kit::{
    BoundActionCall, CancellationToken, CapabilityAttestation, EvidenceStream, FakeProvider,
    ObserveRequest, Provider, ProviderError, ProviderSession, ScriptedOutcome, SessionHealth,
    SessionOutcome, UiSnapshotOutcome, VERDICT_SUMMARY_MAX_CHARS, VerdictWrite,
};
use pointlock_runner::{ResumeOptions, RunOptions, RunOutcome, Runner, RunnerError};
use pointlock_store::{NewRun, RunStatus, Store};
use serde_json::{Value, json};

// ─── Fixture helpers ────────────────────────────────────────────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique per-test store directory under the system temp dir, removed on
/// drop (same pattern as the store tests; no tempfile dependency).
struct TempStoreDir(PathBuf);

impl TempStoreDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-runner-test-{tag}-{}-{}",
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
        let base = match step {
            StepIR::Action(s) => &mut s.base,
            StepIR::Assert(s) => &mut s.base,
            StepIR::Call(s) => &mut s.base,
            StepIR::Human(s) => &mut s.base,
            StepIR::If(s) => &mut s.base,
            StepIR::Foreach(s) => &mut s.base,
            StepIR::Let(s) => &mut s.base,
        };
        base.effect_hash = effect;
        base.judge_hash = judge;
    }
    flow.ir_hash = pointlock_ir::ir_hash(flow);
}

fn flow_fixture(lockfile_digest: &Hash, steps: Vec<Value>) -> FlowIR {
    let mut flow: FlowIR = serde_json::from_value(json!({
        "irVersion": 1,
        "flowId": "m0_demo",
        "irHash": h64('e'),
        "provider": { "name": "devicerail", "version": "0.1.0" },
        "requiredFeatures": [],
        "lockfileDigest": lockfile_digest.as_str(),
        "params": [],
        "outputs": [],
        "body": steps,
        "verdictPolicy": "standard",
        "sourceMap": [],
        "subflows": {}
    }))
    .expect("fixture is a valid FlowIR");
    seal(&mut flow);
    flow
}

/// An expr assertion `eq(steps.<sid>.output.ok, true)`.
fn expect_ok(assert_id: &str, sid: &str) -> Value {
    json!({
        "assertId": assert_id,
        "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
            { "ref": format!("steps.{sid}.output.ok") },
            { "lit": true }
        ] } },
        "verifyVia": [],
        "onMissingInput": "unknown"
    })
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

fn succeeded_with(output: Value) -> ScriptedOutcome {
    ScriptedOutcome::Terminal(ActionOutcome::Succeeded {
        result: Box::new(ActionResult {
            call_id: String::new(),
            started_at_ms: 0,
            finished_at_ms: 0,
            output,
            before: None,
            after: None,
            evidence: Vec::new(),
            execution: None,
        }),
    })
}

async fn open(provider: &FakeProvider) -> Box<dyn ProviderSession> {
    provider
        .open_session(provider.default_open_options())
        .await
        .expect("open_session")
}

fn event_types(store: &Store, run_id: &str) -> Vec<&'static str> {
    store
        .events(run_id)
        .expect("events")
        .iter()
        .map(|event| event.payload.event_type())
        .collect()
}

fn run_opts(run_id: &str) -> RunOptions {
    let mut opts = RunOptions::new("fake-device-1");
    opts.run_id = Some(run_id.to_owned());
    opts
}

fn step_of(event: &RunLogEvent) -> Option<&str> {
    event.run_path.iter().rev().find_map(|frame| match frame {
        PathFrame::Step { step_id } => Some(step_id.as_str()),
        _ => None,
    })
}

fn attempt_of(event: &RunLogEvent) -> Option<u64> {
    event.run_path.iter().rev().find_map(|frame| match frame {
        PathFrame::Attempt { n } => Some(*n),
        _ => None,
    })
}

// ─── (a) full pass end to end ──────────────────────────────────────────────

#[tokio::test]
async fn full_pass_end_to_end_keeps_ledger_order_and_checkpoint() {
    let dir = TempStoreDir::new("pass");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![
            action_step("s1", vec![expect_ok("a1", "s1")]),
            action_step("s2", vec![expect_ok("a2", "s2")]),
            action_step("s3", vec![expect_ok("a3", "s3")]),
        ],
    );
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-pass"))
        .await
        .expect("run");
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished with a verdict, got {outcome:?}");
    };
    assert_eq!(verdict.status, VerdictStatus::Pass);
    assert!(!verdict.degraded);

    // Event order discipline, verbatim (spine §6.1/§6.2).
    let step_block = [
        "stepEntered",
        "actionIntent",
        "actionSettled",
        "assertionEvaluated",
        "verdictRecorded",
        "stepExited",
    ];
    let mut expected = vec!["runStarted"];
    for _ in 0..3 {
        expected.extend(step_block);
    }
    expected.push("runFinished");
    assert_eq!(event_types(&store, "run-pass"), expected);

    // The WAL intent strictly precedes its terminal, per callId.
    let events = store.events("run-pass").expect("events");
    for event in &events {
        if let RunLogPayload::ActionSettled { call_id, .. } = &event.payload {
            let intent_seq = events
                .iter()
                .find_map(|candidate| match &candidate.payload {
                    RunLogPayload::ActionIntent {
                        call_id: intent_id, ..
                    } if intent_id == call_id => Some(candidate.seq),
                    _ => None,
                })
                .expect("every settle has its intent");
            assert!(intent_seq < event.seq, "intent must precede settle");
        }
    }

    // stepEntered carries the step's dual hashes and the frozen ready
    // snapshot (== the first intent's argsSnapshot); stepExited carries
    // the projected output (spine §6.1 M1 carriers).
    for event in &events {
        match &event.payload {
            RunLogPayload::StepEntered {
                step_id,
                effect_hash,
                judge_hash,
                resolved_inputs,
            } => {
                let step = flow
                    .body
                    .iter()
                    .find(|step| step.step_id() == step_id)
                    .expect("IR step");
                assert_eq!(effect_hash, &step.base().effect_hash);
                assert_eq!(judge_hash, &step.base().judge_hash);
                assert_eq!(
                    resolved_inputs,
                    &json!({"element": {"identifier": step_id.as_str()}})
                );
            }
            RunLogPayload::StepExited { state, output, .. } => {
                assert_eq!(*state, StepState::Judged);
                assert_eq!(output, &Some(json!({"ok": true})));
            }
            _ => {}
        }
    }

    // Materialized == rebuilt (I1 self-check) and the view is closed;
    // the fold harvested the full StepRecord from the carriers.
    let view = store.verify_checkpoint("run-pass").expect("verify");
    assert_eq!(view.completed.len(), 3);
    assert_eq!(
        view.completed[0].effect_hash,
        flow.body[0].base().effect_hash
    );
    assert_eq!(view.completed[0].judge_hash, flow.body[0].base().judge_hash);
    assert_eq!(
        view.completed[0].resolved_inputs,
        json!({"element": {"identifier": "s1"}})
    );
    assert_eq!(view.completed[0].output, Some(json!({"ok": true})));
    assert!(view.frontier.pending_intent.is_none());
    assert_eq!(view.frames[0].next_index, 3);
    assert_eq!(
        store.run_status("run-pass").expect("status"),
        RunStatus::Finished
    );

    assert_eq!(provider.handle().dispatched_call_ids().len(), 3);
    // Three step verdicts + the flow verdict were written back.
    assert_eq!(provider.handle().recorded_verdicts().len(), 4);
}

// ─── (b) assertion fail → step fail → flow fail ────────────────────────────

#[tokio::test]
async fn assertion_fail_fails_flow_and_blocks_downstream() {
    let dir = TempStoreDir::new("fail");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": false }))]));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![
            action_step("s1", vec![expect_ok("a1", "s1")]),
            action_step("s2", vec![expect_ok("a2", "s2")]),
        ],
    );
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-fail"))
        .await
        .expect("run");
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished with a verdict, got {outcome:?}");
    };
    assert_eq!(verdict.status, VerdictStatus::Fail);

    assert_eq!(
        event_types(&store, "run-fail"),
        vec![
            "runStarted",
            "stepEntered",
            "actionIntent",
            "actionSettled",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            // Halt-on-fail: the downstream step is explicitly blocked.
            "stepEntered",
            "stepExited",
            "runFinished",
        ]
    );
    let view = store.verify_checkpoint("run-fail").expect("verify");
    assert_eq!(view.completed.len(), 2);
    assert_eq!(
        view.completed[0]
            .verdict
            .as_ref()
            .map(|verdict| verdict.status),
        Some(VerdictStatus::Fail)
    );
    assert!(view.completed[1].verdict.is_none());
    assert!(view.completed[1].attempts.is_empty());
    // The blocked span never resolved inputs (resolvedInputs: null, no
    // output) but still archived the step's execution-time hashes.
    assert_eq!(view.completed[1].resolved_inputs, Value::Null);
    assert_eq!(view.completed[1].output, None);
    assert_eq!(
        view.completed[1].effect_hash,
        flow.body[1].base().effect_hash
    );
    // The blocked step never dispatched.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 1);
}

// ─── (c) retryable failure → retry with a new callId and a new intent ──────

#[tokio::test]
async fn retryable_failure_retries_with_new_call_id_and_new_intent() {
    let dir = TempStoreDir::new("retry");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        ScriptedOutcome::failed("device_unavailable", true),
        succeeded_with(json!({ "ok": true })),
    ]));
    let mut step = action_step("s1", vec![expect_ok("a1", "s1")]);
    step["retry"] = json!({
        "maxAttempts": 2,
        "backoffMs": 0,
        "retryOn": ["action_failed_retryable"]
    });
    let flow = flow_fixture(&provider.lockfile().digest, vec![step]);
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-retry"))
        .await
        .expect("run");
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished with a verdict, got {outcome:?}");
    };
    assert_eq!(verdict.status, VerdictStatus::Pass);

    assert_eq!(
        event_types(&store, "run-retry"),
        vec![
            "runStarted",
            "stepEntered",
            "actionIntent",
            "actionSettled",
            "actionIntent",
            "actionSettled",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            "runFinished",
        ]
    );
    // Two distinct callIds, each with its own WAL intent (spine §6.5).
    let call_ids = provider.handle().dispatched_call_ids();
    assert_eq!(call_ids.len(), 2);
    assert_ne!(call_ids[0], call_ids[1]);
    let events = store.events("run-retry").expect("events");
    let intent_ids: Vec<String> = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::ActionIntent { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(intent_ids, call_ids);
    // Attempt frames advance: #1 then #2.
    let attempts: Vec<u64> = events
        .iter()
        .filter(|event| matches!(event.payload, RunLogPayload::ActionIntent { .. }))
        .map(|event| attempt_of(event).expect("attempt frame"))
        .collect();
    assert_eq!(attempts, vec![1, 2]);

    let view = store.verify_checkpoint("run-retry").expect("verify");
    assert_eq!(view.completed[0].attempts.len(), 2);
    assert_eq!(
        view.completed[0].attempts[0].outcome,
        pointlock_ir::ActionOutcomeKind::Failed
    );
    assert_eq!(
        view.completed[0].attempts[1].outcome,
        pointlock_ir::ActionOutcomeKind::Succeeded
    );
}

// ─── (d) stop after step 2 → suspended → resume completes step 3 only ──────

/// Delegating session wrapper that cancels a stop token once `remaining`
/// executes have completed — a deterministic way to hit the step-boundary
/// stop check.
struct StopAfter {
    inner: Box<dyn ProviderSession>,
    remaining: AtomicUsize,
    stop: CancellationToken,
}

#[async_trait]
impl ProviderSession for StopAfter {
    fn attestation(&self) -> &CapabilityAttestation {
        self.inner.attestation()
    }
    async fn execute(
        &self,
        call: BoundActionCall,
        cancel: Option<CancellationToken>,
    ) -> Result<ActionOutcome, ProviderError> {
        let outcome = self.inner.execute(call, cancel).await;
        if self.remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.stop.cancel();
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
        issuing: &pointlock_ir::EventCursor,
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

#[tokio::test]
async fn stop_then_resume_reuses_completed_steps_and_dispatches_only_the_rest() {
    let dir = TempStoreDir::new("resume");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![
            action_step("s1", vec![expect_ok("a1", "s1")]),
            action_step("s2", vec![expect_ok("a2", "s2")]),
            action_step("s3", vec![expect_ok("a3", "s3")]),
        ],
    );

    let stop = CancellationToken::new();
    let session = Box::new(StopAfter {
        inner: open(&provider).await,
        remaining: AtomicUsize::new(2),
        stop: stop.clone(),
    });
    let mut opts = run_opts("run-stop");
    opts.stop = stop;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, opts)
        .await
        .expect("run");
    assert_eq!(outcome, RunOutcome::Suspended);
    assert_eq!(
        store.run_status("run-stop").expect("status"),
        RunStatus::Suspended
    );
    assert_eq!(
        event_types(&store, "run-stop").last(),
        Some(&"runSuspended")
    );
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);

    // Resume with the same IR: fast path, completed steps adopted.
    let session = open(&provider).await;
    let outcome = Runner::resume(
        &flow,
        "run-stop",
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

    // Only step 3 was dispatched by the resume segment.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 3);

    // The resume segment's ledger: runResumed then exactly one step.
    let types = event_types(&store, "run-stop");
    let resumed_at = types
        .iter()
        .position(|event_type| *event_type == "runResumed")
        .expect("runResumed present");
    // 07 §4.5: the segment header carries the new generation's reseeded
    // cursor, and the folded binding lineage reflects it.
    let events_all = store.events("run-stop").expect("events");
    let resumed_cursor = events_all
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::RunResumed { event_cursor, .. } => Some(event_cursor.clone()),
            _ => None,
        })
        .expect("runResumed payload");
    let cursor = resumed_cursor.expect("the resume header carries the reseeded cursor");
    assert_eq!(cursor.session_id, "fake-session-2");
    // And the folded binding lineage names both generations (07 §4.5).
    let view_lineage = store
        .verify_checkpoint("run-stop")
        .expect("exact")
        .binding
        .session_lineage;
    assert_eq!(
        view_lineage,
        vec!["fake-session-1".to_owned(), "fake-session-2".to_owned()]
    );
    assert_eq!(
        &types[resumed_at..],
        &[
            "runResumed",
            "stepEntered",
            // No `preflight` on the re-entry step: the resume records that
            // it verified nothing rather than pretending it did (I3).
            "preflightProbed",
            "actionIntent",
            "actionSettled",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            "runFinished",
        ]
    );

    // The alignment report adopted s1/s2 and re-executes nothing prior.
    let events = store.events("run-stop").expect("events");
    let report = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::RunResumed {
                alignment_report, ..
            } => Some(alignment_report.clone()),
            _ => None,
        })
        .expect("alignment report");
    let classes: Vec<(String, AlignmentClass)> = report
        .entries
        .iter()
        .map(|entry| (entry.step_id.as_str().to_owned(), entry.class))
        .collect();
    assert_eq!(
        classes,
        vec![
            ("s1".to_owned(), AlignmentClass::Reusable),
            ("s2".to_owned(), AlignmentClass::Reusable),
            ("s3".to_owned(), AlignmentClass::New),
        ]
    );
    assert!(report.requires_confirmation.is_empty());
    let view = store.verify_checkpoint("run-stop").expect("verify");
    assert_eq!(view.completed.len(), 3);
    assert_eq!(
        store.run_status("run-stop").expect("status"),
        RunStatus::Finished
    );
}

// ─── (d2) order consistency: reordering is not adoptable ────────────────────

/// 07 §5.2 order-consistency check. Swapping two completed, data-independent
/// steps leaves every id and both hashes untouched, so per-step
/// classification alone calls them all `reusable` and the run would adopt a
/// history that the new IR never produced — the one place where a gap here
/// yields a silent wrong answer rather than a typed refusal. Adoption is
/// therefore conditional on the matched records' old execution order
/// agreeing with the new IR's traversal order.
#[tokio::test]
async fn resume_refuses_to_adopt_a_reordered_history() {
    let dir = TempStoreDir::new("resume-reorder");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    // Assertion ids follow the step, not its position, so that swapping two
    // steps changes the ORDER and nothing else — the whole point of the case.
    let steps = |ids: [&str; 3]| {
        ids.iter()
            .map(|id| action_step(id, vec![expect_ok(&format!("a_{id}"), id)]))
            .collect::<Vec<_>>()
    };
    // The old run completes s1 then s2, and suspends before s3.
    let flow = flow_fixture(&provider.lockfile().digest, steps(["s1", "s2", "s3"]));
    let stop = CancellationToken::new();
    let session = Box::new(StopAfter {
        inner: open(&provider).await,
        remaining: AtomicUsize::new(2),
        stop: stop.clone(),
    });
    let mut opts = run_opts("run-reorder");
    opts.stop = stop;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, opts)
        .await
        .expect("run");
    assert_eq!(outcome, RunOutcome::Suspended);
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);

    // The author swaps the first two steps. Every stepId and both hashes are
    // unchanged — only the order is.
    let reordered = flow_fixture(&provider.lockfile().digest, steps(["s2", "s1", "s3"]));
    let session = open(&provider).await;
    let error = Runner::resume(
        &reordered,
        "run-reorder",
        session,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect_err("a reordered history must not be adopted silently");
    let RunnerError::RequiresConfirmation { report } = error else {
        panic!("expected the 07 §5.4 gate, got {error:?}");
    };

    // s2 precedes the inversion and is still adopted; s1 — which actually ran
    // FIRST — is the inversion point and is demoted, taking s3 with it.
    let classes: Vec<(String, AlignmentClass)> = report
        .entries
        .iter()
        .map(|entry| (entry.step_id.as_str().to_owned(), entry.class))
        .collect();
    assert_eq!(
        classes,
        vec![
            ("s2".to_owned(), AlignmentClass::Reusable),
            ("s1".to_owned(), AlignmentClass::EffectDirty),
            ("s3".to_owned(), AlignmentClass::New),
        ]
    );

    // The report names the inverted pair, so a reviewer can judge whether the
    // two steps are genuinely order-independent.
    let reason = report
        .entries
        .iter()
        .find(|entry| entry.step_id.as_str() == "s1")
        .and_then(|entry| entry.reason.clone())
        .expect("the demoted entry explains itself");
    assert!(
        reason.contains("order invalidated") && reason.contains("'s1'") && reason.contains("'s2'"),
        "the reason must name the inverted pair, got: {reason}"
    );

    // And the gate cites reordering as the cause — not a hash change, since
    // no hash changed.
    let causes: Vec<&str> = report
        .requires_confirmation
        .iter()
        .map(|entry| entry.cause.as_str())
        .collect();
    assert_eq!(causes, vec!["orderInvalidated"]);

    // Nothing was dispatched: the refusal precedes execution.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);
}

/// 07 §5.4 step 2: the gate is released by naming the step, and by nothing
/// else. The same resume that refuses without an authorization completes
/// with one — and the authorization is per-invocation, not persisted.
#[tokio::test]
async fn allow_mutating_reexec_releases_exactly_the_named_step() {
    let dir = TempStoreDir::new("resume-allow");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let steps = |ids: [&str; 3]| {
        ids.iter()
            .map(|id| action_step(id, vec![expect_ok(&format!("a_{id}"), id)]))
            .collect::<Vec<_>>()
    };
    let flow = flow_fixture(&provider.lockfile().digest, steps(["s1", "s2", "s3"]));
    let stop = CancellationToken::new();
    let session = Box::new(StopAfter {
        inner: open(&provider).await,
        remaining: AtomicUsize::new(2),
        stop: stop.clone(),
    });
    let mut opts = run_opts("run-allow");
    opts.stop = stop;
    assert_eq!(
        Runner::run(&flow, json!({}), session, &mut store, opts)
            .await
            .expect("run"),
        RunOutcome::Suspended
    );

    // The author retargets s2 — a mutating, non-idempotent step that already
    // passed (the §5.4 scenario verbatim). Sealing recomputes its
    // effectHash, so the old record is no longer adoptable and re-running
    // the step would repeat an effect that is already in the world.
    let mut edited = steps(["s1", "s2", "s3"]);
    edited[1]["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] = json!("s2_fixed");
    let repaired = flow_fixture(&provider.lockfile().digest, edited);

    // Without authorization: refused, and the report names s2 alone.
    let session = open(&provider).await;
    let error = Runner::resume(
        &repaired,
        "run-allow",
        session,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect_err("an already-effective mutating step must not silently re-run");
    let RunnerError::RequiresConfirmation { report } = error else {
        panic!("expected the 07 §5.4 gate, got {error:?}");
    };
    let gated: Vec<(&str, &str)> = report
        .requires_confirmation
        .iter()
        .map(|entry| {
            let step = entry
                .run_path
                .iter()
                .rev()
                .find_map(|frame| match frame {
                    PathFrame::Step { step_id } => Some(step_id.as_str()),
                    _ => None,
                })
                .expect("a gated entry names its step");
            (step, entry.cause.as_str())
        })
        .collect();
    assert_eq!(gated, vec![("s2", "mutatingReexec")]);
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);

    // Naming a step that is not gated releases nothing — the gate holds, so
    // a typo fails closed rather than authorizing something unintended.
    let session = open(&provider).await;
    let error = Runner::resume(
        &repaired,
        "run-allow",
        session,
        &mut store,
        ResumeOptions {
            allow_mutating_reexec: vec!["s1".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect_err("authorizing the wrong step must not release s2");
    assert!(matches!(error, RunnerError::RequiresConfirmation { .. }));
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);

    // Naming s2 releases it: the resume proceeds and re-executes s2 and s3.
    let session = open(&provider).await;
    let outcome = Runner::resume(
        &repaired,
        "run-allow",
        session,
        &mut store,
        ResumeOptions {
            allow_mutating_reexec: vec!["s2".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("the authorized resume runs");
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished, got {outcome:?}");
    };
    assert_eq!(verdict.status, VerdictStatus::Pass);
    assert_eq!(provider.handle().dispatched_call_ids().len(), 4);
}

/// 07 §5.2: a report entry is path-addressed. A step id alone cannot say
/// WHICH instance was classified once a run nests, so the entry carries the
/// archived run path — the same address `requiresConfirmation` uses, so a
/// reader can join the two halves of the report.
#[tokio::test]
async fn alignment_entries_carry_the_instance_path_they_classified() {
    let dir = TempStoreDir::new("resume-paths");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let steps = |ids: [&str; 3]| {
        ids.iter()
            .map(|id| action_step(id, vec![expect_ok(&format!("a_{id}"), id)]))
            .collect::<Vec<_>>()
    };
    let flow = flow_fixture(&provider.lockfile().digest, steps(["s1", "s2", "s3"]));
    let stop = CancellationToken::new();
    let session = Box::new(StopAfter {
        inner: open(&provider).await,
        remaining: AtomicUsize::new(2),
        stop: stop.clone(),
    });
    let mut opts = run_opts("run-paths");
    opts.stop = stop;
    assert_eq!(
        Runner::run(&flow, json!({}), session, &mut store, opts)
            .await
            .expect("run"),
        RunOutcome::Suspended
    );

    // s2 is retargeted, so it gates; s3 was never reached, so it is `new`.
    let mut edited = steps(["s1", "s2", "s3"]);
    edited[1]["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] = json!("s2_fixed");
    let repaired = flow_fixture(&provider.lockfile().digest, edited);
    let session = open(&provider).await;
    let error = Runner::resume(
        &repaired,
        "run-paths",
        session,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect_err("the retargeted mutating step gates");
    let RunnerError::RequiresConfirmation { report } = error else {
        panic!("expected the 07 §5.4 gate, got {error:?}");
    };

    let tail_step = |path: &[PathFrame]| {
        path.iter()
            .rev()
            .find_map(|frame| match frame {
                PathFrame::Step { step_id } => Some(step_id.as_str().to_owned()),
                _ => None,
            })
            .expect("a classified path names its step")
    };
    // Every entry's path ends at the step the entry names — including the
    // `new` one, which has no archived instance and reports its position in
    // the new flow instead.
    for entry in &report.entries {
        assert_eq!(
            tail_step(&entry.run_path),
            entry.step_id.as_str(),
            "entry path and step id disagree: {entry:?}"
        );
    }
    assert_eq!(report.entries.len(), 3);

    // The gate and the classification now address instances the same way,
    // so the two halves of the report join on the path.
    let gated = &report.requires_confirmation[0];
    let classified = report
        .entries
        .iter()
        .find(|entry| entry.run_path == gated.run_path)
        .expect("the gated instance is also classified");
    assert_eq!(classified.step_id.as_str(), "s2");
    assert_eq!(classified.class, AlignmentClass::EffectDirty);
}

// ─── (e) judgeDirty: offline re-judge without dispatch ──────────────────────

#[tokio::test]
async fn judge_dirty_rejudges_offline_without_dispatch() {
    let dir = TempStoreDir::new("rejudge");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let old_flow = flow_fixture(
        &provider.lockfile().digest,
        vec![
            action_step("s1", vec![expect_ok("a1", "s1")]),
            action_step("s2", vec![expect_ok("a2", "s2")]),
            action_step("s3", vec![expect_ok("a3", "s3")]),
        ],
    );
    let session = open(&provider).await;
    let outcome = Runner::run(
        &old_flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-rejudge"),
    )
    .await
    .expect("run");
    assert!(matches!(outcome, RunOutcome::Finished { verdict: Some(_) }));
    assert_eq!(provider.handle().dispatched_call_ids().len(), 3);
    let events_before = store.events("run-rejudge").expect("events").len();

    // Repair: tighten s3's assertion (judge domain only) — still passing.
    let tightened = json!({
        "assertId": "a3",
        "predicate": { "type": "expr", "expr": { "fn": "and", "args": [
            { "fn": "eq", "args": [ { "ref": "steps.s3.output.ok" }, { "lit": true } ] },
            { "lit": true }
        ] } },
        "verifyVia": [],
        "onMissingInput": "unknown"
    });
    let new_flow = flow_fixture(
        &provider.lockfile().digest,
        vec![
            action_step("s1", vec![expect_ok("a1", "s1")]),
            action_step("s2", vec![expect_ok("a2", "s2")]),
            action_step("s3", vec![tightened]),
        ],
    );
    assert_ne!(new_flow.ir_hash, old_flow.ir_hash);
    // Judge-only repair: effect hash unchanged, judge hash moved.
    assert_eq!(
        new_flow.body[2].base().effect_hash,
        old_flow.body[2].base().effect_hash
    );
    assert_ne!(
        new_flow.body[2].base().judge_hash,
        old_flow.body[2].base().judge_hash
    );

    // A supplied old IR that is not the executed one is refused (the
    // optional integrity check).
    let session = open(&provider).await;
    let error = Runner::resume(
        &new_flow,
        "run-rejudge",
        session,
        &mut store,
        ResumeOptions {
            old_flow_ir: Some(new_flow.clone()),
            ..ResumeOptions::default()
        },
    )
    .await
    .expect_err("must refuse a mismatched old IR");
    assert!(matches!(error, RunnerError::OldIrMismatch { .. }));

    // Cross-IR resume *without* the old IR: alignment runs on the
    // archived per-step hashes (stepEntered carriers, spine §6.1 M1).
    let session = open(&provider).await;
    let outcome = Runner::resume(
        &new_flow,
        "run-rejudge",
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
    assert_eq!(provider.handle().dispatched_call_ids().len(), 3);

    // The resume segment: runResumed → superseding verdict → runFinished.
    let events = store.events("run-rejudge").expect("events");
    let segment: Vec<&'static str> = events[events_before..]
        .iter()
        .map(|event| event.payload.event_type())
        .collect();
    assert_eq!(
        segment,
        vec!["runResumed", "verdictRecorded", "runFinished"]
    );
    let (rejudged, rejudge_localized, rejudge_gaps) = events[events_before..]
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded {
                verdict,
                localized,
                localization_gaps,
                ..
            } => Some((
                verdict.clone(),
                localized.clone(),
                localization_gaps.clone(),
            )),
            _ => None,
        })
        .expect("re-judged verdict");
    // Item ③ invariant: nothing is localized offline — the rejudge
    // manifest is empty by construction.
    assert!(rejudge_localized.is_empty() && rejudge_gaps.is_empty());
    assert_eq!(rejudged.status, VerdictStatus::Pass);
    let supersedes = rejudged.supersedes.expect("supersedes lineage");
    assert!(supersedes.starts_with("seq:"), "got {supersedes}");
    assert_eq!(step_of(&events[events_before + 1]), Some("s3"));

    let report = events[events_before..]
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::RunResumed {
                alignment_report, ..
            } => Some(alignment_report.clone()),
            _ => None,
        })
        .expect("alignment report");
    let classes: Vec<AlignmentClass> = report.entries.iter().map(|entry| entry.class).collect();
    assert_eq!(
        classes,
        vec![
            AlignmentClass::Reusable,
            AlignmentClass::Reusable,
            AlignmentClass::JudgeDirty,
        ]
    );

    // The fold re-projected the completed record's verdict.
    let view = store.verify_checkpoint("run-rejudge").expect("verify");
    let s3 = view
        .completed
        .iter()
        .find(|record| record.step_id.as_str() == "s3")
        .expect("s3 record");
    assert_eq!(
        s3.verdict.as_ref().map(|verdict| verdict.status),
        Some(VerdictStatus::Pass)
    );
    assert!(
        s3.verdict
            .as_ref()
            .and_then(|verdict| verdict.supersedes.as_deref())
            .is_some()
    );
}

// ─── (f) crash window: intent without execute → reconcile → replay ─────────

/// Reconstructs the §6.7-B crash window for step `s1` of `flow`:
/// runStarted → stepEntered → actionIntent `call_id` fsynced — and the
/// process dies before any `actionSettled` lands.
fn stage_crash_window(store: &mut Store, flow: &FlowIR, run_id: &str, call_id: &str) {
    store
        .begin_run(NewRun {
            run_id: Some(run_id.to_owned()),
            flow_id: flow.flow_id.clone(),
            ir_hash: flow.ir_hash.clone(),
            lockfile_digest: flow.lockfile_digest.clone(),
            params_snapshot: json!({}),
            binding: BindingState {
                device_id: "fake-device-1".to_owned(),
                session_lineage: vec!["stale-session".to_owned()],
                event_cursor: EventCursor {
                    session_id: "stale-session".to_owned(),
                    last_sequence: 0,
                },
            },
            created_at_ms: 1,
        })
        .expect("begin run");
    let root = vec![PathFrame::Flow {
        flow_id: flow.flow_id.clone(),
        ir_hash: flow.ir_hash.clone(),
    }];
    store
        .append_event(
            run_id,
            1,
            &root,
            &RunLogPayload::RunStarted {
                ir_hash: flow.ir_hash.clone(),
                lockfile_digest: flow.lockfile_digest.clone(),
                params_snapshot: json!({}),
                supervise_policy: None,
            },
        )
        .expect("runStarted");
    let s1 = StepId::new("s1").expect("step id");
    let mut s1_path = root.clone();
    s1_path.push(PathFrame::Step {
        step_id: s1.clone(),
    });
    // The M1 carrier payload: the crashed segment had already frozen the
    // ready snapshot and archived the step's execution-time hashes.
    let s1_base = flow.body[0].base();
    store
        .append_event(
            run_id,
            2,
            &s1_path,
            &RunLogPayload::StepEntered {
                step_id: s1,
                effect_hash: s1_base.effect_hash.clone(),
                judge_hash: s1_base.judge_hash.clone(),
                resolved_inputs: json!({ "element": { "identifier": "s1" } }),
            },
        )
        .expect("stepEntered");
    let mut attempt_path = s1_path.clone();
    attempt_path.push(PathFrame::Attempt { n: 1 });
    store
        .write_action_intent(
            run_id,
            3,
            &attempt_path,
            call_id,
            json!({ "element": { "identifier": "s1" } }),
            None,
        )
        .expect("actionIntent");
}

/// Journals an archived terminal for `call_id` in the fake's daemon-side
/// log: the dispatch reached the daemon (consuming the front scripted
/// outcome), but the runner died before appending `actionSettled`.
async fn seed_archived_terminal(provider: &FakeProvider, call_id: &str) {
    let session = open(provider).await;
    session
        .execute(
            BoundActionCall {
                call_id: call_id.to_owned(),
                action_name: ActionName::new("tapElement").expect("action name"),
                arguments: json!({ "element": { "identifier": "s1" } }),
                action_timeout_ms: None,
                request_timeout_ms: None,
            },
            None,
        )
        .await
        .expect("a scripted terminal is an Ok value");
}

#[tokio::test]
async fn crash_window_reconciles_never_dispatched_and_replays() {
    let dir = TempStoreDir::new("crash");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![
            action_step("s1", vec![expect_ok("a1", "s1")]),
            action_step("s2", vec![expect_ok("a2", "s2")]),
        ],
    );

    // The crash hit before dispatch: no daemon-side trace of the callId.
    let run_id = "run-crash";
    stage_crash_window(&mut store, &flow, run_id, "call-crash");
    let view = store.rebuild_checkpoint(run_id).expect("rebuild");
    assert_eq!(
        view.frontier
            .pending_intent
            .as_ref()
            .map(|intent| intent.call_id.as_str()),
        Some("call-crash")
    );

    // Resume: reconcile finds no trace in the issuing session's log →
    // neverDispatched → safe replay with the archived args snapshot.
    let session = open(&provider).await;
    let outcome = Runner::resume(&flow, run_id, session, &mut store, ResumeOptions::default())
        .await
        .expect("resume");
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished with a verdict, got {outcome:?}");
    };
    assert_eq!(verdict.status, VerdictStatus::Pass);

    // The replay used a fresh callId; the crashed one never dispatched.
    let call_ids = provider.handle().dispatched_call_ids();
    assert_eq!(call_ids.len(), 2);
    assert!(!call_ids.contains(&"call-crash".to_owned()));

    // s1 carries two intents: the crashed #1 and the replay #2; the span
    // was continued, not re-entered.
    let events = store.events(run_id).expect("events");
    let s1_intents: Vec<u64> = events
        .iter()
        .filter(|event| {
            matches!(event.payload, RunLogPayload::ActionIntent { .. })
                && step_of(event) == Some("s1")
        })
        .map(|event| attempt_of(event).expect("attempt frame"))
        .collect();
    assert_eq!(s1_intents, vec![1, 2]);
    let entered_count = events
        .iter()
        .filter(|event| {
            matches!(event.payload, RunLogPayload::StepEntered { .. })
                && step_of(event) == Some("s1")
        })
        .count();
    assert_eq!(entered_count, 1);

    let view = store.verify_checkpoint(run_id).expect("verify");
    assert!(view.frontier.pending_intent.is_none());
    assert_eq!(view.completed.len(), 2);
    assert_eq!(
        store.run_status(run_id).expect("status"),
        RunStatus::Finished
    );
}

// ─── (g) crash window: archived failed terminal → adopted → retryOn retry ──

#[tokio::test]
async fn crash_window_adopts_failed_terminal_and_retries_per_policy() {
    let dir = TempStoreDir::new("adopt-retry");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        ScriptedOutcome::failed("device_unavailable", true),
        succeeded_with(json!({ "ok": true })),
    ]));
    let mut step = action_step("s1", vec![expect_ok("a1", "s1")]);
    step["retry"] = json!({
        "maxAttempts": 2,
        "backoffMs": 0,
        "retryOn": ["action_failed_retryable"]
    });
    let flow = flow_fixture(&provider.lockfile().digest, vec![step]);

    let run_id = "run-adopt-retry";
    stage_crash_window(&mut store, &flow, run_id, "call-crash");
    seed_archived_terminal(&provider, "call-crash").await;

    // Resume: reconcile → completed(failed) → the archived failure is
    // adopted as the settled terminal, then retried per retryOn exactly
    // like a live failure (retryable class, budget not exhausted).
    let session = open(&provider).await;
    let outcome = Runner::resume(&flow, run_id, session, &mut store, ResumeOptions::default())
        .await
        .expect("resume");
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished with a verdict, got {outcome:?}");
    };
    assert_eq!(verdict.status, VerdictStatus::Pass);

    // The retry is a fresh callId; the crashed one was settled from the
    // archive, never re-dispatched.
    let call_ids = provider.handle().dispatched_call_ids();
    assert_eq!(call_ids.len(), 2);
    assert_eq!(call_ids[0], "call-crash");
    assert_ne!(call_ids[1], "call-crash");

    // Resume segment: the adopted settle closes attempt #1, then the
    // retry's intent/settle at attempt #2 — the span is continued, not
    // re-entered, and no special-case events appear.
    let events = store.events(run_id).expect("events");
    let resumed_at = events
        .iter()
        .position(|event| matches!(event.payload, RunLogPayload::RunResumed { .. }))
        .expect("runResumed present");
    let segment: Vec<&'static str> = events[resumed_at..]
        .iter()
        .map(|event| event.payload.event_type())
        .collect();
    assert_eq!(
        segment,
        vec![
            "runResumed",
            "actionSettled",
            "actionIntent",
            "actionSettled",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            "runFinished",
        ]
    );
    let settled: Vec<(String, u64)> = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::ActionSettled { call_id, .. } => {
                Some((call_id.clone(), attempt_of(event).expect("attempt frame")))
            }
            _ => None,
        })
        .collect();
    assert_eq!(settled[0], ("call-crash".to_owned(), 1));
    assert_eq!(settled[1].1, 2);

    let view = store.verify_checkpoint(run_id).expect("verify");
    assert!(view.frontier.pending_intent.is_none());
    let kinds: Vec<ActionOutcomeKind> = view.completed[0]
        .attempts
        .iter()
        .map(|attempt| attempt.outcome)
        .collect();
    assert_eq!(
        kinds,
        vec![ActionOutcomeKind::Failed, ActionOutcomeKind::Succeeded]
    );
    assert_eq!(
        store.run_status(run_id).expect("status"),
        RunStatus::Finished
    );
}

// ─── (h) crash window: archived failed terminal outside retryOn → step fail ─

#[tokio::test]
async fn crash_window_adopts_failed_terminal_and_fails_step_without_retry() {
    let dir = TempStoreDir::new("adopt-fail");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([ScriptedOutcome::failed(
        "device_unavailable",
        true,
    )]));
    let mut step = action_step("s1", vec![expect_ok("a1", "s1")]);
    // A policy exists but does not list the adopted failure's class:
    // retryOn decides, and here it says no.
    step["retry"] = json!({
        "maxAttempts": 3,
        "backoffMs": 0,
        "retryOn": ["action_timed_out"]
    });
    let flow = flow_fixture(&provider.lockfile().digest, vec![step]);

    let run_id = "run-adopt-fail";
    stage_crash_window(&mut store, &flow, run_id, "call-crash");
    seed_archived_terminal(&provider, "call-crash").await;

    // Resume: reconcile → completed(failed) → the archived failure is
    // adopted (a certain fate, not an uncertain branch) and the step
    // fails through the live error-classification path.
    let session = open(&provider).await;
    let outcome = Runner::resume(&flow, run_id, session, &mut store, ResumeOptions::default())
        .await
        .expect("resume");
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished with a verdict, got {outcome:?}");
    };
    assert_eq!(verdict.status, VerdictStatus::Fail);

    // No new dispatch: the adopted failure resolves the step by itself.
    assert_eq!(
        provider.handle().dispatched_call_ids(),
        vec!["call-crash".to_owned()]
    );

    let events = store.events(run_id).expect("events");
    let resumed_at = events
        .iter()
        .position(|event| matches!(event.payload, RunLogPayload::RunResumed { .. }))
        .expect("runResumed present");
    let segment: Vec<&'static str> = events[resumed_at..]
        .iter()
        .map(|event| event.payload.event_type())
        .collect();
    assert_eq!(
        segment,
        vec![
            "runResumed",
            "actionSettled",
            "verdictRecorded",
            "stepExited",
            "runFinished",
        ]
    );

    // The step verdict is a definite fail folded from the adopted
    // terminal, on the same path as a live act-phase failure.
    let step_verdict = events[resumed_at..]
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded { verdict, .. } if step_of(event) == Some("s1") => {
                Some(verdict.clone())
            }
            _ => None,
        })
        .expect("step verdict");
    assert_eq!(step_verdict.status, VerdictStatus::Fail);
    assert!(
        step_verdict.summary.contains("act phase failed"),
        "got {}",
        step_verdict.summary
    );

    let view = store.verify_checkpoint(run_id).expect("verify");
    assert!(view.frontier.pending_intent.is_none());
    assert_eq!(
        view.completed[0]
            .attempts
            .iter()
            .map(|attempt| attempt.outcome)
            .collect::<Vec<ActionOutcomeKind>>(),
        vec![ActionOutcomeKind::Failed]
    );
    assert_eq!(
        store.run_status(run_id).expect("status"),
        RunStatus::Finished
    );
}

// ─── load gates: M0 subset and capability drift ─────────────────────────────

#[tokio::test]
async fn malformed_human_steps_are_refused_with_a_typed_error() {
    // Since the human wave (M2-W2a) `human` steps execute, but the load
    // shape defense mirrors the compiler check phase (06 §2.2): a
    // `confirm` step without exactly two decision labels is refused.
    let dir = TempStoreDir::new("subset");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let human_step = json!({
        "kind": "human",
        "stepId": "approve",
        "effectHash": h64('0'),
        "judgeHash": h64('0'),
        "checkpoint": true,
        "mode": "confirm",
        "prompt": "Approve?",
        "presents": [],
        "timeoutMs": 1000,
        "onTimeout": "unknown"
    });
    let flow = flow_fixture(&provider.lockfile().digest, vec![human_step]);
    let session = open(&provider).await;
    let error = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-subset"),
    )
    .await
    .expect_err("must refuse");
    let RunnerError::InvalidHumanStep { step_id, reason } = error else {
        panic!("expected InvalidHumanStep, got {error:?}");
    };
    assert_eq!(step_id.as_str(), "approve");
    assert!(reason.contains("exactly two decision labels"));
}

#[tokio::test]
async fn capability_drift_refuses_to_run() {
    let dir = TempStoreDir::new("drift");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let other_digest = Hash::new(h64('f')).expect("hash");
    let flow = flow_fixture(&other_digest, vec![action_step("s1", vec![])]);
    let session = open(&provider).await;
    let error = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-drift"))
        .await
        .expect_err("must refuse");
    assert!(matches!(error, RunnerError::CapabilityDrift { .. }));
}

// ─── Unknown issuing generation: uncertain branch WITHOUT an RPC ────────────

/// A cursor-less (pre-incorporation) resume before the pending intent
/// makes the issuing session unknowable: the runner must take the
/// uncertain branch WITHOUT calling `reconcile` — a fabricated
/// credential could fabricate `neverDispatched` (07 §4.5).
#[tokio::test]
async fn an_unknowable_issuing_generation_reconciles_nothing() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use pointlock_ir::AlignmentReport;

    struct CountingSession {
        inner: Box<dyn ProviderSession>,
        reconciles: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ProviderSession for CountingSession {
        fn attestation(&self) -> &pointlock_provider_kit::lockfile::CapabilityAttestation {
            self.inner.attestation()
        }
        async fn execute(
            &self,
            call: BoundActionCall,
            cancel: Option<CancellationToken>,
        ) -> Result<pointlock_ir::ActionOutcome, ProviderError> {
            self.inner.execute(call, cancel).await
        }
        async fn observe(
            &self,
            req: ObserveRequest,
            cancel: Option<CancellationToken>,
        ) -> Result<Observation, ProviderError> {
            self.inner.observe(req, cancel).await
        }
        async fn ui_snapshot(
            &self,
            observation_id: &str,
        ) -> Result<UiSnapshotOutcome, ProviderError> {
            self.inner.ui_snapshot(observation_id).await
        }
        async fn reconcile(
            &self,
            call_id: &str,
            issuing: &pointlock_ir::EventCursor,
        ) -> Result<ReconcileResult, ProviderError> {
            self.reconciles.fetch_add(1, Ordering::SeqCst);
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
        async fn health(&self) -> Result<pointlock_provider_kit::SessionHealth, ProviderError> {
            self.inner.health().await
        }
        async fn end(
            &self,
            outcome: pointlock_provider_kit::SessionOutcome,
            reason: Option<String>,
        ) -> Result<(), ProviderError> {
            self.inner.end(outcome, reason).await
        }
    }

    let dir = TempStoreDir::new("unknowable");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step("s1", vec![expect_ok("a1", "s1")])],
    );
    stage_crash_window(&mut store, &flow, "run-unknowable", "call-unknowable");
    // Interpose a cursor-less resume BEFORE the intent: rebuild the
    // ledger shape by appending it between runStarted and the intent is
    // not possible append-only — instead stage a fresh ledger with the
    // resume before the intent.
    let events = store.events("run-unknowable").expect("events");
    assert_eq!(events.len(), 3, "runStarted + stepEntered + actionIntent");

    // Build the actual fixture: a second run whose ledger carries a
    // cursor-less runResumed between runStarted and the crash window.
    let run_id = "run-unknowable-2";
    store
        .begin_run(NewRun {
            run_id: Some(run_id.to_owned()),
            flow_id: flow.flow_id.clone(),
            ir_hash: flow.ir_hash.clone(),
            lockfile_digest: flow.lockfile_digest.clone(),
            params_snapshot: json!({}),
            binding: BindingState {
                device_id: "fake-device-1".to_owned(),
                session_lineage: vec!["stale-session".to_owned()],
                event_cursor: EventCursor {
                    session_id: "stale-session".to_owned(),
                    last_sequence: 0,
                },
            },
            created_at_ms: 1,
        })
        .expect("begin run");
    let root = vec![PathFrame::Flow {
        flow_id: flow.flow_id.clone(),
        ir_hash: flow.ir_hash.clone(),
    }];
    store
        .append_event(
            run_id,
            1,
            &root,
            &RunLogPayload::RunStarted {
                ir_hash: flow.ir_hash.clone(),
                lockfile_digest: flow.lockfile_digest.clone(),
                params_snapshot: json!({}),
                supervise_policy: None,
            },
        )
        .expect("runStarted");
    store
        .append_event(
            run_id,
            2,
            &root,
            &RunLogPayload::RunResumed {
                alignment_report: AlignmentReport {
                    entries: vec![],
                    resume_point: None,
                    requires_confirmation: vec![],
                },
                supervise_policy: None,
                event_cursor: None,
            },
        )
        .expect("cursor-less resume");
    let s1 = StepId::new("s1").expect("step id");
    let mut s1_path = root.clone();
    s1_path.push(PathFrame::Step {
        step_id: s1.clone(),
    });
    let s1_base = flow.body[0].base();
    store
        .append_event(
            run_id,
            3,
            &s1_path,
            &RunLogPayload::StepEntered {
                step_id: s1,
                effect_hash: s1_base.effect_hash.clone(),
                judge_hash: s1_base.judge_hash.clone(),
                resolved_inputs: json!({ "element": { "identifier": "s1" } }),
            },
        )
        .expect("stepEntered");
    let mut attempt_path = s1_path.clone();
    attempt_path.push(PathFrame::Attempt { n: 1 });
    store
        .write_action_intent(
            run_id,
            4,
            &attempt_path,
            "call-unknowable-2",
            json!({ "element": { "identifier": "s1" } }),
            None,
        )
        .expect("actionIntent");

    let reconciles = Arc::new(AtomicUsize::new(0));
    let session = CountingSession {
        inner: open(&provider).await,
        reconciles: Arc::clone(&reconciles),
    };
    let outcome = Runner::resume(
        &flow,
        run_id,
        Box::new(session),
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("resume");

    // The uncertain branch asks a human instead of dead-ending — and
    // reconcile was NEVER called (the credential would be fabricated).
    let RunOutcome::AwaitingHuman { pending } = outcome else {
        panic!("expected the adjudication wait, got {outcome:?}");
    };
    assert_eq!(reconciles.load(Ordering::SeqCst), 0);
    assert_eq!(
        pending.mode,
        Some(pointlock_ir::vocab::HumanMode::RepairWorld)
    );
    assert!(
        pending.prompt.contains("unknowable"),
        "the prompt names the uncertainty: {}",
        pending.prompt
    );
}

// ─── act-chain dispatch identity + crash re-entry (item ②, 2026-07-18) ──────

/// A two-attempt step json: tapElement → setElementValue, both attested.
fn two_attempt_step(id: &str, idempotent: bool) -> Value {
    json!({
        "kind": "action",
        "stepId": id,
        "effectHash": h64('0'),
        "judgeHash": h64('0'),
        "checkpoint": true,
        "effect": "mutating",
        "idempotent": idempotent,
        "binding": { "attempts": [ {
            "channel": "uiTree",
            "actionName": "tapElement",
            "args": { "element": { "lit": { "identifier": id } } },
            "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
            "protection": "standard"
        }, {
            "channel": "uiTree",
            "actionName": "setElementValue",
            "args": { "element": { "lit": { "identifier": id } }, "value": { "lit": "x" } },
            "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
            "protection": "standard"
        } ] },
        "assertions": []
    })
}

/// A final failure advances the chain; both dispatches journal their
/// chain identity, and the overview's latest-pass marks read
/// crossed@1 / succeeded@2.
#[tokio::test]
async fn chain_advance_journals_dispatch_identity_and_marks() {
    let dir = TempStoreDir::new("chain-identity");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        ScriptedOutcome::failed("action_failed_final", false),
        succeeded_with(json!({ "ok": true })),
    ]));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![two_attempt_step("s1", false)],
    );
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-chain"))
        .await
        .expect("run");
    assert!(matches!(outcome, RunOutcome::Finished { .. }));

    let view = store.verify_checkpoint("run-chain").expect("exact");
    let record = &view.completed[0];
    assert_eq!(record.attempts.len(), 2);
    assert_eq!(record.attempts[0].chain_index, Some(1));
    assert_eq!(
        record.attempts[0].action_name.as_ref().map(|n| n.as_str()),
        Some("tapElement")
    );
    assert_eq!(record.attempts[1].chain_index, Some(2));
    assert_eq!(
        record.attempts[1].action_name.as_ref().map(|n| n.as_str()),
        Some("setElementValue")
    );

    let overview =
        pointlock_store::projection::run_overview(&store, "run-chain").expect("overview");
    let cell = overview
        .steps
        .values()
        .find(|cell| cell.act_chain_marks.is_some())
        .expect("a marked cell");
    let marks = cell.act_chain_marks.as_ref().expect("marks");
    assert_eq!(marks.len(), 2);
    assert_eq!(
        (marks[0].chain_index, marks[0].mark.as_str()),
        (1, "crossed")
    );
    assert_eq!(
        (marks[1].chain_index, marks[1].mark.as_str()),
        (2, "succeeded")
    );
}

/// A crashed mid-chain intent replays AT its recorded position with the
/// archived args (07 §1.4: resume lands at the precise position) — the
/// pre-fix behavior re-entered at the chain head and dispatched attempt
/// 1's action with attempt 2's args.
#[tokio::test]
async fn a_mid_chain_crashed_intent_replays_at_its_position() {
    let dir = TempStoreDir::new("mid-chain-replay");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": true }))]));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![two_attempt_step("s1", true)],
    );

    // Stage the crash window with the intent at chain position 2.
    store
        .begin_run(NewRun {
            run_id: Some("run-mid".to_owned()),
            flow_id: flow.flow_id.clone(),
            ir_hash: flow.ir_hash.clone(),
            lockfile_digest: flow.lockfile_digest.clone(),
            params_snapshot: json!({}),
            binding: BindingState {
                device_id: "fake-device-1".to_owned(),
                session_lineage: vec!["stale-session".to_owned()],
                event_cursor: EventCursor {
                    session_id: "stale-session".to_owned(),
                    last_sequence: 0,
                },
            },
            created_at_ms: 1,
        })
        .expect("begin run");
    let root = vec![PathFrame::Flow {
        flow_id: flow.flow_id.clone(),
        ir_hash: flow.ir_hash.clone(),
    }];
    store
        .append_event(
            "run-mid",
            1,
            &root,
            &RunLogPayload::RunStarted {
                ir_hash: flow.ir_hash.clone(),
                lockfile_digest: flow.lockfile_digest.clone(),
                params_snapshot: json!({}),
                supervise_policy: None,
            },
        )
        .expect("runStarted");
    let s1 = StepId::new("s1").expect("step id");
    let mut s1_path = root.clone();
    s1_path.push(PathFrame::Step {
        step_id: s1.clone(),
    });
    let s1_base = flow.body[0].base();
    store
        .append_event(
            "run-mid",
            2,
            &s1_path,
            &RunLogPayload::StepEntered {
                step_id: s1,
                effect_hash: s1_base.effect_hash.clone(),
                judge_hash: s1_base.judge_hash.clone(),
                resolved_inputs: json!({ "element": { "identifier": "s1" } }),
            },
        )
        .expect("stepEntered");
    let mut attempt_path = s1_path.clone();
    attempt_path.push(PathFrame::Attempt { n: 1 });
    store
        .write_action_intent(
            "run-mid",
            3,
            &attempt_path,
            "call-mid",
            json!({ "element": { "identifier": "s1" }, "value": "x" }),
            Some(pointlock_store::IntentDispatch {
                chain_index: 2,
                channel: pointlock_ir::ActChannel::UiTree,
                action_name: pointlock_ir::ActionName::new("setElementValue").expect("action name"),
            }),
        )
        .expect("actionIntent");

    let session = open(&provider).await;
    let outcome = Runner::resume(
        &flow,
        "run-mid",
        session,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("resume");
    assert!(
        matches!(outcome, RunOutcome::Finished { .. }),
        "{outcome:?}"
    );

    // The replay's fresh intent carries chain position 2 and the SECOND
    // attempt's action — never attempt 1 with attempt 2's args.
    let events = store.events("run-mid").expect("events");
    let replay_intent = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::ActionIntent {
                call_id,
                chain_index,
                action_name,
                ..
            } if call_id != "call-mid" => Some((*chain_index, action_name.clone())),
            _ => None,
        })
        .next_back()
        .expect("a replay intent");
    assert_eq!(replay_intent.0, Some(2));
    assert_eq!(
        replay_intent.1.as_ref().map(|name| name.as_str()),
        Some("setElementValue")
    );
    store.verify_checkpoint("run-mid").expect("exact");
}

/// A crash leaves a step span open. Repairing an EARLIER step's assertion
/// and resuming must re-judge it offline — the combination used to be a
/// typed refusal because the fold picked its `verdictRecorded` target by
/// "is anything in flight" rather than by the event's own path, and would
/// have written the re-judgement onto the crashed step instead. Dispatching
/// by path makes the two independent, and this is the commonest repair
/// shape there is: something broke, and while you are in there you also
/// tighten a judgement further up.
#[tokio::test]
async fn an_offline_rejudge_lands_beside_a_crash_opened_span() {
    let dir = TempStoreDir::new("rejudge-open");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // s1 completes and passes
        pointlock_provider_kit::ScriptedOutcome::TransportLostAfterDispatch, // s2 crashes
    ]));
    let old_flow = flow_fixture(
        &provider.lockfile().digest,
        vec![
            action_step("s1", vec![expect_ok("a1", "s1")]),
            action_step("s2", vec![expect_ok("a2", "s2")]),
        ],
    );
    Runner::run(
        &old_flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-open"),
    )
    .await
    .expect("run suspends on the transport rupture");

    // The crash left s2's span open: `stepEntered` with no `stepExited`.
    let types = event_types(&store, "run-open");
    assert!(
        types.iter().filter(|t| **t == "stepEntered").count() == 2
            && types.iter().filter(|t| **t == "stepExited").count() == 1,
        "expected exactly one open span, got {types:?}"
    );
    let before = store.events("run-open").expect("events").len();

    // Tighten s1's assertion — judge domain only, still passing.
    let tightened = json!({
        "assertId": "a1",
        "predicate": { "type": "expr", "expr": { "fn": "and", "args": [
            { "fn": "eq", "args": [ { "ref": "steps.s1.output.ok" }, { "lit": true } ] },
            { "lit": true }
        ] } },
        "verifyVia": [],
        "onMissingInput": "unknown"
    });
    let new_flow = flow_fixture(
        &provider.lockfile().digest,
        vec![
            action_step("s1", vec![tightened]),
            action_step("s2", vec![expect_ok("a2", "s2")]),
        ],
    );
    assert_eq!(
        new_flow.body[0].base().effect_hash,
        old_flow.body[0].base().effect_hash
    );
    assert_ne!(
        new_flow.body[0].base().judge_hash,
        old_flow.body[0].base().judge_hash
    );

    // The resume is no longer refused. (It goes on to block on the
    // unresolved non-idempotent intent — that is a different rule, and the
    // point here is that it got past alignment at all.)
    let outcome = Runner::resume(
        &new_flow,
        "run-open",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await;
    match &outcome {
        Ok(_) => {}
        Err(err) => assert!(
            !err.to_string().contains("step span is open"),
            "the crash-frontier refusal must be gone, got: {err}"
        ),
    }

    // The re-judgement landed on s1's record — NOT on the crashed s2.
    let view = store.verify_checkpoint("run-open").expect("verify");
    let s1 = view
        .completed
        .iter()
        .rev()
        .find(|record| record.step_id.as_str() == "s1")
        .expect("s1 has a record");
    let verdict = s1.verdict.as_ref().expect("s1 keeps a verdict");
    assert_eq!(verdict.status, VerdictStatus::Pass);
    assert!(
        verdict.supersedes.is_some(),
        "s1's verdict must be the superseding re-judgement, got {verdict:?}"
    );
    assert!(
        !view
            .completed
            .iter()
            .any(|record| record.step_id.as_str() == "s2"),
        "the crashed step's span is still open; it has no record to be judged"
    );
    let rejudges = store.events("run-open").expect("events")[before..]
        .iter()
        .filter(|event| matches!(event.payload, RunLogPayload::VerdictRecorded { .. }))
        .count();
    assert_eq!(rejudges, 1, "exactly one offline re-judgement was written");
}

/// The 07 §5.4 criterion is the EFFECTIVE ATTEMPT, not the verdict
/// (2026-07-28 ruling). Both directions in one test:
/// - s1's act LANDED (`succeeded` terminal) and its assertion refused it —
///   verdict `fail`. Editing s1 and resuming re-executes an act whose
///   effect is in the world: it must gate, though the verdict says fail.
/// - control: the same shape where s1's act NEVER landed (every attempt a
///   `failed` terminal) must not gate — the terminal is the daemon's word
///   that there is nothing in the world to double.
#[tokio::test]
async fn the_gate_reads_the_attempt_terminal_not_the_verdict() {
    // Direction 1: fail verdict, succeeded attempt — gates.
    let dir = TempStoreDir::new("gate-attempt");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": false })), // act lands, assertion refuses
    ]));
    let build = |target: &str| {
        let mut s1 = action_step("s1", vec![expect_ok("a1", "s1")]);
        s1["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] = json!(target);
        flow_fixture(&provider.lockfile().digest, vec![s1])
    };
    let flow = build("s1_v1");
    Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-ga"),
    )
    .await
    .expect("run");
    let record = store
        .verify_checkpoint("run-ga")
        .expect("verify")
        .completed
        .first()
        .cloned()
        .expect("s1 has a record");
    assert_eq!(
        record.verdict.as_ref().expect("judged").status,
        VerdictStatus::Fail
    );
    assert!(
        record
            .attempts
            .iter()
            .any(|attempt| attempt.outcome == pointlock_ir::ActionOutcomeKind::Succeeded),
        "the premise: the act landed"
    );

    let error = Runner::resume(
        &build("s1_v2"),
        "run-ga",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect_err("a landed act gates its re-execution, whatever the verdict said");
    let RunnerError::RequiresConfirmation { report } = error else {
        panic!("expected the 07 §5.4 gate, got {error}");
    };
    assert_eq!(report.requires_confirmation.len(), 1);

    // Direction 2: fail verdict, every attempt a failed terminal — no gate.
    let dir2 = TempStoreDir::new("gate-noattempt");
    let mut store2 = Store::open(dir2.path()).expect("open store");
    let provider2 = FakeProvider::new(VecDeque::from([
        ScriptedOutcome::failed("element_not_found", false), // the act never lands
        succeeded_with(json!({ "ok": true })),               // the repaired step, after resume
    ]));
    let build2 = |target: &str| {
        let mut s1 = action_step("s1", vec![expect_ok("a1", "s1")]);
        s1["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] = json!(target);
        flow_fixture(&provider2.lockfile().digest, vec![s1])
    };
    Runner::run(
        &build2("s1_v1"),
        json!({}),
        open(&provider2).await,
        &mut store2,
        run_opts("run-gn"),
    )
    .await
    .expect("run");
    Runner::resume(
        &build2("s1_v2"),
        "run-gn",
        open(&provider2).await,
        &mut store2,
        ResumeOptions::default(),
    )
    .await
    .expect("an act that never landed needs no authorization to repair");
    assert_eq!(provider2.handle().dispatched_call_ids().len(), 2);
}

/// 07 §5.3's escape hatch: `--force-reexecute <stepId>` upgrades the named
/// step to `effectDirty` whatever its hashes say. Here s2's repair touched
/// only its assertion (judge domain) and the offline re-judge would adopt
/// it — the author disagrees and forces it back onto the device.
/// Classification only: s2 is mutating and already effective, so the §5.4
/// gate still demands `--allow-mutating-reexec` on top — forcing says "run
/// it again", authorizing says "yes, even though the world holds its
/// effect".
#[tokio::test]
async fn force_reexecute_upgrades_the_classification_but_not_the_gate() {
    let dir = TempStoreDir::new("force");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // s1
        succeeded_with(json!({ "ok": true })), // s2
        succeeded_with(json!({ "ok": true })), // s2 forced re-run
    ]));
    let build = |assert_tail: bool| {
        let a2 = if assert_tail {
            json!({
                "assertId": "a2",
                "predicate": { "type": "expr", "expr": { "fn": "and", "args": [
                    { "fn": "eq", "args": [ { "ref": "steps.s2.output.ok" }, { "lit": true } ] },
                    { "lit": true }
                ] } },
                "verifyVia": [],
                "onMissingInput": "unknown"
            })
        } else {
            expect_ok("a2", "s2")
        };
        flow_fixture(
            &provider.lockfile().digest,
            vec![
                action_step("s1", vec![expect_ok("a1", "s1")]),
                action_step("s2", vec![a2]),
            ],
        )
    };
    let flow = build(false);
    Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-force"),
    )
    .await
    .expect("run");
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);

    // Judge-only repair: without forcing, the re-judge adopts s2 and
    // nothing re-runs (pinned by the neighbouring judge_dirty test).
    let repaired = build(true);
    assert_eq!(
        repaired.body[1].base().effect_hash,
        flow.body[1].base().effect_hash
    );
    assert_ne!(
        repaired.body[1].base().judge_hash,
        flow.body[1].base().judge_hash
    );

    // Forced but unauthorized: the upgrade lands in the §5.4 gate.
    let error = Runner::resume(
        &repaired,
        "run-force",
        open(&provider).await,
        &mut store,
        ResumeOptions {
            force_reexecute: vec!["s2".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect_err("a forced mutating effective step still gates");
    let RunnerError::RequiresConfirmation { report } = error else {
        panic!("expected the 07 §5.4 gate, got {error}");
    };
    assert_eq!(report.requires_confirmation.len(), 1);
    assert_eq!(
        report.requires_confirmation[0]
            .step_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("s2")
    );

    // Forced and authorized: s2 re-runs on the device; s1 stays adopted.
    let outcome = Runner::resume(
        &repaired,
        "run-force",
        open(&provider).await,
        &mut store,
        ResumeOptions {
            force_reexecute: vec!["s2".to_owned()],
            allow_mutating_reexec: vec!["s2".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("the authorized forced re-run proceeds");
    assert!(matches!(
        outcome,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Pass
    ));
    assert_eq!(
        provider.handle().dispatched_call_ids().len(),
        3,
        "exactly s2 re-ran"
    );
    let report = store
        .events("run-force")
        .expect("events")
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            RunLogPayload::RunResumed {
                alignment_report, ..
            } => Some(alignment_report.clone()),
            _ => None,
        })
        .expect("the resume recorded its report");
    let entry = report
        .entries
        .iter()
        .find(|entry| entry.step_id.as_str() == "s2")
        .expect("s2 classified");
    assert_eq!(entry.class, AlignmentClass::EffectDirty);
    assert!(
        entry
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("force-reexecute")),
        "the report names the forcing: {:?}",
        entry.reason
    );
}

// ─── uncertain reconcile → adjudication (00 §6.7-B / 07 §4.4) ───────────────

/// A UiSnapshot tree in the fake's wire shape.
fn tree(nodes: Value) -> Value {
    json!({
        "formatVersion": 1,
        "observationId": "stamped-by-fake",
        "context": { "contextKind": "native", "contextId": "ctx-1", "documentEpoch": "e1" },
        "rootStableNodeIds": ["n1"],
        "nodes": nodes
    })
}

/// One uncertain non-idempotent mutating intent, then the full adjudication
/// cycle. The uncertain branch used to dead-end (`Blocked`, with no channel
/// to ever answer); now it is the DEFAULT `onResumeDrift` escalation — a
/// synthesized `repairWorld` request ruling `adopt | redo | abort`:
/// - first resume: `awaitingHuman`, the prompt names the callId;
/// - unanswered re-resume: the SAME request re-awaited, no duplicate ask;
/// - `redo`: the human's ruling IS the replay license (I2 (iv)) — the act
///   dispatches again and the run concludes.
#[tokio::test]
async fn an_uncertain_intent_is_adjudicated_redo() {
    let dir = TempStoreDir::new("adj-redo");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        pointlock_provider_kit::ScriptedOutcome::TransportLostAfterDispatch,
        succeeded_with(json!({ "ok": true })), // the redone dispatch
    ]));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step("s1", vec![expect_ok("a1", "s1")])],
    );
    let outcome = Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-adj-redo"),
    )
    .await
    .expect("run suspends on the rupture");
    assert_eq!(outcome, RunOutcome::Suspended);

    // First resume: reconcile says startedNoTerminal → adjudication asked.
    let outcome = Runner::resume(
        &flow,
        "run-adj-redo",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("resume asks");
    let RunOutcome::AwaitingHuman { pending } = outcome else {
        panic!("expected the adjudication wait, got {outcome:?}");
    };
    assert_eq!(
        pending.mode,
        Some(pointlock_ir::vocab::HumanMode::RepairWorld)
    );

    // Unanswered re-resume: the same request re-awaits — exactly ONE ask
    // on the ledger.
    let outcome = Runner::resume(
        &flow,
        "run-adj-redo",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("re-await");
    let RunOutcome::AwaitingHuman { pending: again } = outcome else {
        panic!("expected the re-await, got {outcome:?}");
    };
    assert_eq!(again.request_id, pending.request_id);
    let asks = store
        .events("run-adj-redo")
        .expect("events")
        .iter()
        .filter(|event| matches!(event.payload, RunLogPayload::HumanRequested { .. }))
        .count();
    assert_eq!(asks, 1, "no duplicate adjudication request");

    // `redo` — the ruling licenses the replay; nothing else does (I2).
    store
        .submit_human_response(
            "run-adj-redo",
            &pending.request_id,
            "cli:tester",
            1,
            json!({ "decision": "redo" }),
        )
        .expect("arbitrate");
    let outcome = Runner::resume(
        &flow,
        "run-adj-redo",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("the redone resume runs");
    assert!(matches!(
        outcome,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Pass
    ));
    // rupture dispatch + redo dispatch.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);
}

/// `adopt`: the ruling says the effect stands — nothing re-dispatches; the
/// step's own assertions verify the ruled world over a fresh observation
/// (the same confirmation path a timed-out act takes) and the verdict is
/// theirs. `abort` is the third ruling: the open span closes `aborted` and
/// the run concludes without a semantic claim.
#[tokio::test]
async fn an_uncertain_intent_is_adjudicated_adopt_and_abort() {
    // adopt —
    let dir = TempStoreDir::new("adj-adopt");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        pointlock_provider_kit::ScriptedOutcome::TransportLostAfterDispatch,
    ]));
    provider.handle().inject_ui_snapshot(Some(tree(json!([
        { "stableNodeId": "n1", "role": "text", "identifier": "order_done",
          "name": "订单已提交", "hittable": true }
    ]))));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step(
            "s1",
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
    Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-adj-adopt"),
    )
    .await
    .expect("run");
    let outcome = Runner::resume(
        &flow,
        "run-adj-adopt",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("resume asks");
    let RunOutcome::AwaitingHuman { pending } = outcome else {
        panic!("expected the adjudication wait, got {outcome:?}");
    };
    store
        .submit_human_response(
            "run-adj-adopt",
            &pending.request_id,
            "cli:tester",
            1,
            json!({ "decision": "adopt" }),
        )
        .expect("arbitrate");
    let outcome = Runner::resume(
        &flow,
        "run-adj-adopt",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("the adopted resume confirms");
    assert!(
        matches!(
            outcome,
            RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Pass
        ),
        "{outcome:?}"
    );
    // NOTHING was re-dispatched: the one rupture dispatch stands.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 1);
    let record = store
        .verify_checkpoint("run-adj-adopt")
        .expect("verify")
        .completed
        .first()
        .cloned()
        .expect("s1 concluded");
    assert!(
        record
            .verdict
            .as_ref()
            .is_some_and(|verdict| verdict.status == VerdictStatus::Pass),
        "the assertions verified the ruled world: {record:?}"
    );

    // abort —
    let dir2 = TempStoreDir::new("adj-abort");
    let mut store2 = Store::open(dir2.path()).expect("open store");
    let provider2 = FakeProvider::new(VecDeque::from([
        pointlock_provider_kit::ScriptedOutcome::TransportLostAfterDispatch,
    ]));
    let flow2 = flow_fixture(
        &provider2.lockfile().digest,
        vec![action_step("s1", vec![expect_ok("a1", "s1")])],
    );
    Runner::run(
        &flow2,
        json!({}),
        open(&provider2).await,
        &mut store2,
        run_opts("run-adj-abort"),
    )
    .await
    .expect("run");
    let outcome = Runner::resume(
        &flow2,
        "run-adj-abort",
        open(&provider2).await,
        &mut store2,
        ResumeOptions::default(),
    )
    .await
    .expect("resume asks");
    let RunOutcome::AwaitingHuman { pending } = outcome else {
        panic!("expected the adjudication wait, got {outcome:?}");
    };
    store2
        .submit_human_response(
            "run-adj-abort",
            &pending.request_id,
            "cli:tester",
            1,
            json!({ "decision": "abort" }),
        )
        .expect("arbitrate");
    let outcome = Runner::resume(
        &flow2,
        "run-adj-abort",
        open(&provider2).await,
        &mut store2,
        ResumeOptions::default(),
    )
    .await
    .expect("the aborted resume concludes");
    assert!(
        matches!(outcome, RunOutcome::Finished { verdict: None }),
        "an aborted run makes no semantic claim: {outcome:?}"
    );
    assert_eq!(provider2.handle().dispatched_call_ids().len(), 1);
    let view = store2.verify_checkpoint("run-adj-abort").expect("verify");
    assert!(
        view.completed
            .iter()
            .any(|record| record.step_id.as_str() == "s1" && record.verdict.is_none()),
        "the span closed aborted, claiming nothing"
    );
}

/// 07 §5.3 / 02 §12.3 ruling 6: with the OLD IR supplied, a `judgeDirty`
/// whose judge delta is PREFLIGHT-ONLY adopts its archived verdict — the
/// probes run before the act, so the verdict judged exactly the question
/// the new IR asks. The case that pays: an assert step, which without the
/// old IR must re-execute (its fused judgeHash cannot say the assertions
/// are untouched). Both halves pinned: without `--old-ir` it re-runs;
/// with it, nothing does.
#[tokio::test]
async fn a_preflight_only_change_adopts_with_the_old_ir() {
    let build = |digest: &Hash, with_probe: bool| {
        let mut assert_step = json!({
            "kind": "assert",
            "stepId": "check",
            "effectHash": h64('0'),
            "judgeHash": h64('0'),
            "checkpoint": true,
            "observe": "fresh",
            "assertions": [ {
                "assertId": "a1",
                "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
                    { "ref": "steps.probe.output.ok" }, { "lit": true }
                ] } },
                "verifyVia": [],
                "onMissingInput": "unknown"
            } ]
        });
        if with_probe {
            assert_step["preflight"] = json!([{
                "assertId": "pf",
                "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
                    { "lit": 1 }, { "lit": 1 }
                ] } },
                "verifyVia": [],
                "onMissingInput": "unknown"
            }]);
        }
        flow_fixture(
            digest,
            vec![
                action_step("probe", vec![expect_ok("pa", "probe")]),
                assert_step,
                action_step("tail", vec![expect_ok("ta", "tail")]),
            ],
        )
    };

    // WITHOUT the old IR: the assert step re-executes (re-observes).
    let dir = TempStoreDir::new("pfo-bare");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // probe
        succeeded_with(json!({ "ok": true })), // tail
        succeeded_with(json!({ "ok": true })), // tail re-runs (positional)
    ]));
    let digest = provider.lockfile().digest.clone();
    let flow = build(&digest, false);
    let repaired = build(&digest, true);
    assert_eq!(
        repaired.body[1].base().effect_hash,
        flow.body[1].base().effect_hash,
        "adding a probe is judge-domain only"
    );
    assert_ne!(
        repaired.body[1].base().judge_hash,
        flow.body[1].base().judge_hash
    );
    Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-pfo-bare"),
    )
    .await
    .expect("run");
    Runner::resume(
        &repaired,
        "run-pfo-bare",
        open(&provider).await,
        &mut store,
        ResumeOptions {
            allow_mutating_reexec: vec!["tail".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("bare resume re-observes");
    // probe adopted; check re-observed (no dispatch — assert steps observe,
    // not act); tail positionally replayed: 2 + 1 dispatches.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 3);

    // WITH the old IR: everything adopts; nothing re-runs at all.
    let dir2 = TempStoreDir::new("pfo-old");
    let mut store2 = Store::open(dir2.path()).expect("open store");
    let provider2 = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // probe
        succeeded_with(json!({ "ok": true })), // tail
    ]));
    let digest2 = provider2.lockfile().digest.clone();
    let flow2 = build(&digest2, false);
    let repaired2 = build(&digest2, true);
    Runner::run(
        &flow2,
        json!({}),
        open(&provider2).await,
        &mut store2,
        run_opts("run-pfo-old"),
    )
    .await
    .expect("run");
    let outcome = Runner::resume(
        &repaired2,
        "run-pfo-old",
        open(&provider2).await,
        &mut store2,
        ResumeOptions {
            old_flow_ir: Some(flow2.clone()),
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("the old-IR resume adopts");
    assert!(matches!(
        outcome,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Pass
    ));
    assert_eq!(
        provider2.handle().dispatched_call_ids().len(),
        2,
        "nothing re-ran: the preflight-only change adopted"
    );
    let report = store2
        .events("run-pfo-old")
        .expect("events")
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            RunLogPayload::RunResumed {
                alignment_report, ..
            } => Some(alignment_report.clone()),
            _ => None,
        })
        .expect("report recorded");
    let entry = report
        .entries
        .iter()
        .find(|entry| entry.step_id.as_str() == "check")
        .expect("the assert step is classified");
    assert_eq!(entry.class, AlignmentClass::JudgeDirty);
    assert_eq!(entry.reason.as_deref(), Some("preflightChanged"));
}

/// The repairWorld STEP settle mapping, per 06 §2.2's closed table:
/// `done` → pass; `cannotRepair` → **fail** — a verdict the flow folds
/// like any other, never a unilateral run abort. (The retired as-built
/// vocabulary `repaired|abort` matched neither 06 nor the adjudication
/// and aborted the whole run on `abort`; 2026-07-28 unification.)
#[tokio::test]
async fn a_repair_world_step_settles_done_pass_cannot_repair_fail() {
    let build_flow = |digest: &Hash| {
        flow_fixture(
            digest,
            vec![json!({
                "kind": "human",
                "stepId": "fix",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "mode": "repairWorld",
                "prompt": "put the device back",
                "presents": [],
                "timeoutMs": 3_600_000u64,
                "onTimeout": "unknown"
            })],
        )
    };
    for (decision, expected) in [
        ("done", VerdictStatus::Pass),
        ("cannotRepair", VerdictStatus::Fail),
    ] {
        let dir = TempStoreDir::new(&format!("rw-{decision}"));
        let mut store = Store::open(dir.path()).expect("open store");
        let provider = FakeProvider::new(VecDeque::new());
        let flow = build_flow(&provider.lockfile().digest);
        let run_id = format!("run-rw-{decision}");
        let outcome = Runner::run(
            &flow,
            json!({}),
            open(&provider).await,
            &mut store,
            run_opts(&run_id),
        )
        .await
        .expect("run");
        let RunOutcome::AwaitingHuman { pending } = outcome else {
            panic!("expected the wait, got {outcome:?}");
        };
        store
            .submit_human_response(
                &run_id,
                &pending.request_id,
                "cli:tester",
                1,
                json!({ "decision": decision }),
            )
            .expect("the 06 §2.1 vocabulary arbitrates");
        let outcome = Runner::resume(
            &flow,
            &run_id,
            open(&provider).await,
            &mut store,
            ResumeOptions::default(),
        )
        .await
        .expect("settle");
        let RunOutcome::Finished {
            verdict: Some(verdict),
        } = outcome
        else {
            panic!("a ruled repairWorld step FOLDS, never aborts: {outcome:?}");
        };
        assert_eq!(verdict.status, expected, "decision {decision}");
    }

    // The retired vocabulary is refused at arbitration.
    let dir = TempStoreDir::new("rw-retired");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let flow = build_flow(&provider.lockfile().digest);
    let outcome = Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-rw-x"),
    )
    .await
    .expect("run");
    let RunOutcome::AwaitingHuman { pending } = outcome else {
        panic!("expected the wait");
    };
    let error = store
        .submit_human_response(
            "run-rw-x",
            &pending.request_id,
            "cli:tester",
            1,
            json!({ "decision": "repaired" }),
        )
        .expect_err("the retired token must not arbitrate");
    assert!(
        error.to_string().contains("done|cannotRepair"),
        "got {error}"
    );
}

#[tokio::test]
async fn verdict_writeback_failure_is_annotated_never_fatal() {
    // 04 §5: remote archival failure never changes the local verdict and
    // never aborts the run — it rides the ledger as an annotation.
    let dir = TempStoreDir::new("archival");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": true }))]));
    provider
        .handle()
        .set_record_verdict_error(Some("daemon unreachable".to_owned()));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step("s1", vec![expect_ok("a1", "s1")])],
    );
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-arch"))
        .await
        .expect("the run must not abort over archival");
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished with a verdict, got {outcome:?}");
    };
    // The local verdict is untouched by the archival failure.
    assert_eq!(verdict.status, VerdictStatus::Pass);

    let events = store.events("run-arch").expect("events");
    let step_annotations: Vec<&Option<String>> = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded {
                remote_archival_error,
                ..
            } => Some(remote_archival_error),
            _ => None,
        })
        .collect();
    assert_eq!(step_annotations.len(), 1);
    let annotation = step_annotations[0]
        .as_deref()
        .expect("the failed write-back must be annotated");
    assert!(
        annotation.contains("remote archival failed"),
        "{annotation}"
    );
    let finish_annotation = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::RunFinished {
                remote_archival_error,
                ..
            } => Some(remote_archival_error),
            _ => None,
        })
        .expect("runFinished present");
    assert!(
        finish_annotation
            .as_deref()
            .is_some_and(|error| error.contains("remote archival failed")),
        "the flow verdict's write-back failure must be annotated too: {finish_annotation:?}"
    );
}

#[tokio::test]
async fn verdict_writeback_success_leaves_no_annotation() {
    // Negative control: without the injected failure the annotation is
    // absent everywhere — proof the previous test observes the
    // mechanism, not a default.
    let dir = TempStoreDir::new("archival-ok");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": true }))]));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step("s1", vec![expect_ok("a1", "s1")])],
    );
    let session = open(&provider).await;
    Runner::run(&flow, json!({}), session, &mut store, run_opts("run-ok"))
        .await
        .expect("run");
    let events = store.events("run-ok").expect("events");
    for event in &events {
        match &event.payload {
            RunLogPayload::VerdictRecorded {
                remote_archival_error,
                ..
            }
            | RunLogPayload::RunFinished {
                remote_archival_error,
                ..
            } => assert!(remote_archival_error.is_none()),
            _ => {}
        }
    }
    // And the write-back really happened (the fake archived it).
    assert_eq!(provider.handle().recorded_verdicts().len(), 2);
}

#[tokio::test]
async fn ledger_keeps_the_full_summary_while_the_wire_copy_is_capped() {
    // 04 §5: the 16384-char cap is a WIRE hard limit — the local ledger
    // keeps the complete summary and the wire copy carries a
    // content-hash pointer back to it. Driven end-to-end: a non-boolean
    // expr predicate embeds the oversized observed value in its reason,
    // which the fold carries into the verdict summary verbatim.
    let dir = TempStoreDir::new("fullsum");
    let mut store = Store::open(dir.path()).expect("open store");
    let big = "y".repeat(VERDICT_SUMMARY_MAX_CHARS + 64);
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "big": big }))]));
    let assertion = json!({
        "assertId": "a1",
        "predicate": { "type": "expr", "expr": { "ref": "steps.s1.output.big" } },
        "verifyVia": [],
        "onMissingInput": "unknown"
    });
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step("s1", vec![assertion])],
    );
    let session = open(&provider).await;
    Runner::run(&flow, json!({}), session, &mut store, run_opts("run-full"))
        .await
        .expect("run");

    // The LEDGER copy is uncapped.
    let events = store.events("run-full").expect("events");
    let (ledger_summary, annotation) = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded {
                verdict,
                remote_archival_error,
                ..
            } => Some((verdict.summary.clone(), remote_archival_error.clone())),
            _ => None,
        })
        .expect("step verdict recorded");
    assert!(
        ledger_summary.chars().count() > VERDICT_SUMMARY_MAX_CHARS,
        "the ledger must keep the FULL summary ({} chars)",
        ledger_summary.chars().count()
    );

    // The WIRE copy was capped with the pointer — and accepted by the
    // fail-closed provider (no annotation), proof the compaction is the
    // runner's and it did its job.
    assert_eq!(annotation, None);
    let wire = provider.handle().recorded_verdicts();
    let step_wire = &wire[0].summary;
    assert_eq!(step_wire.chars().count(), VERDICT_SUMMARY_MAX_CHARS);
    assert!(
        step_wire.contains("full local verdict sha256:"),
        "the truncation pointer must ride the wire tail"
    );
}
