//! Handler-engine tests (M2 W3): the four hooks, the five dispositions,
//! trigger budgets, hook-frame auditing, escalate suspension/resume, and
//! repair subflows — all over the FakeProvider and a temp-dir store.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pointlock_ir::{
    ActionOutcome, ActionResult, ErrorInfo, FlowIR, HandlerHook, Hash, HumanPurpose, PathFrame,
    RunLogPayload, StepIR, VerdictStatus,
};
use pointlock_provider_kit::{FakeProvider, Provider, ProviderSession, ScriptedOutcome};
use pointlock_runner::{ResumeOptions, RunOptions, RunOutcome, Runner, RunnerError};
use pointlock_store::Store;
use serde_json::{Value, json};

// ─── Fixture helpers (same pattern as the sibling suites) ───────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempStoreDir(PathBuf);

impl TempStoreDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-handlers-test-{tag}-{}-{}",
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

/// Recomputes the per-step dual hashes (recursively — a container's body
/// steps carry their own, and the load check recomputes them) and the flow
/// irHash: a minimal stand-in for the compiler's seal phase.
fn seal(flow: &mut FlowIR) {
    fn seal_steps(steps: &mut [StepIR]) {
        for step in steps.iter_mut() {
            match step {
                StepIR::If(s) => {
                    seal_steps(&mut s.then);
                    if let Some(otherwise) = &mut s.r#else {
                        seal_steps(otherwise);
                    }
                }
                StepIR::Foreach(s) => seal_steps(&mut s.body),
                _ => {}
            }
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
    }
    seal_steps(&mut flow.body);
    flow.ir_hash = pointlock_ir::ir_hash(flow);
}

/// A flow fixture with optional flow-level handlers.
fn flow_fixture(lockfile_digest: &Hash, steps: Vec<Value>, handlers: Option<Value>) -> FlowIR {
    let mut doc = json!({
        "irVersion": 1,
        "flowId": "w3_demo",
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
    });
    if let Some(handlers) = handlers {
        doc["handlers"] = handlers;
    }
    let mut flow: FlowIR = serde_json::from_value(doc).expect("fixture is a valid FlowIR");
    seal(&mut flow);
    flow
}

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

fn action_step(id: &str, assertions: Vec<Value>, handlers: Option<Value>) -> Value {
    let mut step = json!({
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
    });
    if let Some(handlers) = handlers {
        step["handlers"] = handlers;
    }
    step
}

fn escalate_judge(hook: &str, max_triggers: u32) -> Value {
    json!([{
        "hook": hook,
        "action": { "kind": "escalate", "human": {
            "kind": "human",
            "stepId": format!("esc-{hook}"),
            "effectHash": h64('0'),
            "judgeHash": h64('0'),
            "checkpoint": true,
            "mode": "judge",
            "prompt": "rule on the host step",
            "presents": [],
            "timeoutMs": 3_600_000u64,
            "onTimeout": "unknown"
        } },
        "maxTriggers": max_triggers
    }])
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

fn failed_final(code: &str) -> ScriptedOutcome {
    ScriptedOutcome::Terminal(ActionOutcome::Failed {
        error: ErrorInfo {
            code: code.to_owned(),
            message: format!("scripted {code}"),
            retryable: false,
            details: None,
        },
    })
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

fn event_types(store: &Store, run_id: &str) -> Vec<&'static str> {
    store
        .events(run_id)
        .expect("events")
        .iter()
        .map(|event| event.payload.event_type())
        .collect()
}

fn verdict_statuses(
    store: &Store,
    run_id: &str,
    step: &str,
) -> Vec<(VerdictStatus, Option<String>)> {
    store
        .events(run_id)
        .expect("events")
        .iter()
        .filter(|event| {
            event.run_path.iter().any(
                |frame| matches!(frame, PathFrame::Step { step_id } if step_id.as_str() == step),
            )
        })
        .filter_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded { verdict, .. } => {
                Some((verdict.status, verdict.supersedes.clone()))
            }
            _ => None,
        })
        .collect()
}

fn dispatched(provider: &FakeProvider) -> usize {
    provider.handle().dispatched_call_ids().len()
}

// ─── onFail retry: re-act supersedes the failed verdict ─────────────────────

#[tokio::test]
async fn on_fail_retry_reacts_and_supersedes() {
    let dir = TempStoreDir::new("retry");
    let mut store = Store::open(dir.path()).expect("open store");
    // First act echoes ok:false (assertion fails), the handler-retry act
    // echoes ok:true (passes).
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": false })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let handlers = json!([{
        "hook": "onFail",
        "action": { "kind": "retry", "policy": {
            "maxAttempts": 1, "backoffMs": 0, "retryOn": ["action_failed_retryable"]
        } },
        "maxTriggers": 1
    }]);
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step(
            "s1",
            vec![expect_ok("a1", "s1")],
            Some(handlers),
        )],
        None,
    );
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-retry"))
        .await
        .expect("run");
    assert!(matches!(
        outcome,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Pass
    ));
    // Two dispatches (two intents), two verdicts with a supersedes chain.
    assert_eq!(dispatched(&provider), 2);
    let verdicts = verdict_statuses(&store, "run-retry", "s1");
    assert_eq!(verdicts.len(), 2);
    assert_eq!(verdicts[0].0, VerdictStatus::Fail);
    assert_eq!(verdicts[1].0, VerdictStatus::Pass);
    assert!(verdicts[1].1.is_some(), "the re-fold supersedes the first");
    // Exactly one handlerTriggered, exactly one entered/exited pair.
    let types = event_types(&store, "run-retry");
    assert_eq!(
        types.iter().filter(|t| **t == "handlerTriggered").count(),
        1
    );
    assert_eq!(types.iter().filter(|t| **t == "stepEntered").count(), 1);
    assert_eq!(types.iter().filter(|t| **t == "stepExited").count(), 1);
}

// ─── onFail continue: verdict stands, downstream released ───────────────────

#[tokio::test]
async fn on_fail_continue_releases_downstream() {
    let dir = TempStoreDir::new("continue");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": false })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let handlers =
        json!([{ "hook": "onFail", "action": { "kind": "continue" }, "maxTriggers": 1 }]);
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![
            action_step("s1", vec![expect_ok("a1", "s1")], Some(handlers)),
            action_step("s2", vec![expect_ok("a2", "s2")], None),
        ],
        None,
    );
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-cont"))
        .await
        .expect("run");
    // s1's fail verdict stands, s2 ran and passed; the flow verdict folds
    // fail (the release is control-flow, never verdict laundering).
    assert!(matches!(
        outcome,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Fail
    ));
    assert_eq!(dispatched(&provider), 2, "downstream step was dispatched");
    let types = event_types(&store, "run-cont");
    assert!(!types.contains(&"runSuspended"));
}

// ─── onError retry: an error-class negative walks onError, not onFail ──────

#[tokio::test]
async fn on_error_retry_with_class_filter() {
    let dir = TempStoreDir::new("onerror");
    let mut store = Store::open(dir.path()).expect("open store");
    // element_not_found classifies action_failed_final (04 §6 table).
    let provider = FakeProvider::new(VecDeque::from([
        failed_final("element_not_found"),
        succeeded_with(json!({ "ok": true })),
    ]));
    let handlers = json!([{
        "hook": "onError",
        "errorClasses": ["action_failed_final"],
        "action": { "kind": "retry", "policy": {
            "maxAttempts": 1, "backoffMs": 0, "retryOn": []
        } },
        "maxTriggers": 2
    }]);
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step(
            "s1",
            vec![expect_ok("a1", "s1")],
            Some(handlers),
        )],
        None,
    );
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-err"))
        .await
        .expect("run");
    assert!(matches!(
        outcome,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Pass
    ));
    assert_eq!(dispatched(&provider), 2);

    // A non-matching class filter never consults the binding.
    let dir2 = TempStoreDir::new("onerror-miss");
    let mut store2 = Store::open(dir2.path()).expect("open store");
    let provider2 = FakeProvider::new(VecDeque::from([failed_final("element_not_found")]));
    let handlers2 = json!([{
        "hook": "onError",
        "errorClasses": ["transport_lost"],
        "action": { "kind": "retry", "policy": {
            "maxAttempts": 1, "backoffMs": 0, "retryOn": []
        } },
        "maxTriggers": 2
    }]);
    let flow2 = flow_fixture(
        &provider2.lockfile().digest,
        vec![action_step(
            "s1",
            vec![expect_ok("a1", "s1")],
            Some(handlers2),
        )],
        None,
    );
    let session2 = open(&provider2).await;
    let outcome2 = Runner::run(
        &flow2,
        json!({}),
        session2,
        &mut store2,
        run_opts("run-err-miss"),
    )
    .await
    .expect("run");
    assert!(matches!(
        outcome2,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Fail
    ));
    assert_eq!(dispatched(&provider2), 1, "no retry without a class match");
    assert!(!event_types(&store2, "run-err-miss").contains(&"handlerTriggered"));
}

// ─── maxTriggers: the budget is per instance and final ──────────────────────

#[tokio::test]
async fn max_triggers_bounds_the_ladder() {
    let dir = TempStoreDir::new("budget");
    let mut store = Store::open(dir.path()).expect("open store");
    // Always-failing assertions; one retry trigger allowed → exactly two
    // dispatches, then the fail stands.
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": false })),
        succeeded_with(json!({ "ok": false })),
        succeeded_with(json!({ "ok": false })),
    ]));
    let handlers = json!([{
        "hook": "onFail",
        "action": { "kind": "retry", "policy": {
            "maxAttempts": 1, "backoffMs": 0, "retryOn": []
        } },
        "maxTriggers": 1
    }]);
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step(
            "s1",
            vec![expect_ok("a1", "s1")],
            Some(handlers),
        )],
        None,
    );
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-budget"),
    )
    .await
    .expect("run");
    assert!(matches!(
        outcome,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Fail
    ));
    assert_eq!(dispatched(&provider), 2, "one trigger = one re-entry");
    let types = event_types(&store, "run-budget");
    assert_eq!(
        types.iter().filter(|t| **t == "handlerTriggered").count(),
        1
    );
}

// ─── escalate: suspend, human judges pass, resume supersedes ────────────────

#[tokio::test]
async fn escalate_judge_suspends_and_resumes_without_redispatch() {
    let dir = TempStoreDir::new("escalate");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": false })),
        succeeded_with(json!({ "ok": true })),
    ]));
    // Flow-level onFail escalate (precedence test comes separately).
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![
            action_step("s1", vec![expect_ok("a1", "s1")], None),
            action_step("s2", vec![expect_ok("a2", "s2")], None),
        ],
        Some(escalate_judge("onFail", 1)),
    );
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-esc"))
        .await
        .expect("run");
    let RunOutcome::AwaitingHuman { pending } = outcome else {
        panic!("expected AwaitingHuman, got {outcome:?}");
    };
    assert_eq!(pending.purpose, HumanPurpose::Step);
    // The hook human anchors under the host's hook frame.
    assert!(
        pending
            .run_path
            .iter()
            .any(|frame| matches!(frame, PathFrame::Hook { hook, trigger }
                if *hook == HandlerHook::OnFail && *trigger == 1)),
        "escalate human path carries the hook frame: {:?}",
        pending.run_path
    );
    assert_eq!(dispatched(&provider), 1, "suspended before any re-dispatch");

    // The human rules pass; resume settles without re-dispatching s1 and
    // releases s2.
    store
        .submit_human_response(
            "run-esc",
            &pending.request_id,
            "cli:tester",
            1,
            json!({ "status": "pass", "note": "acceptable" }),
        )
        .expect("arbitrate");
    let session = open(&provider).await;
    let outcome = Runner::resume(
        &flow,
        "run-esc",
        session,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("resume");
    assert!(matches!(
        outcome,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Pass
    ));
    // The audit event names the declared disposition (03 §1.8).
    let events = store.events("run-esc").expect("events");
    assert!(
        events.iter().any(|event| matches!(
            &event.payload,
            RunLogPayload::HandlerTriggered { disposition, .. }
                if disposition.as_deref() == Some("escalate")
        )),
        "handlerTriggered must carry the declared disposition"
    );
    // s1: fail verdict then the superseding escalate pass.
    let verdicts = verdict_statuses(&store, "run-esc", "s1");
    assert_eq!(verdicts.len(), 2);
    assert_eq!(verdicts[0].0, VerdictStatus::Fail);
    assert_eq!(verdicts[1].0, VerdictStatus::Pass);
    // The superseding ruling cites the canonical settlement evidence
    // document (06 §6) — a human overrule never rests on empty evidence.
    let events = store.events("run-esc").expect("events");
    let rulings: Vec<(Vec<pointlock_ir::AssetRef>, usize)> = events
        .iter()
        .filter(|event| {
            event.run_path.iter().any(
                |frame| matches!(frame, PathFrame::Step { step_id } if step_id.as_str() == "s1"),
            )
        })
        .filter_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded {
                verdict, localized, ..
            } => Some((verdict.evidence.clone(), localized.len())),
            _ => None,
        })
        .collect();
    let (ruling_evidence, ruling_localized) = &rulings[1];
    assert_eq!(
        ruling_evidence.len(),
        1,
        "the escalate ruling must cite the settlement doc"
    );
    assert!(ruling_evidence[0].id.starts_with("humanResponse:"));
    assert_eq!(*ruling_localized, 1, "the doc is localized evidence");
    // s1 was never re-dispatched: exactly two dispatches total (s1 + s2).
    assert_eq!(dispatched(&provider), 2);
    // Exactly one entered/exited pair for s1 across both segments.
    let events = store.events("run-esc").expect("events");
    let s1_spans = events
        .iter()
        .filter(|event| {
            matches!(&event.payload, RunLogPayload::StepEntered { .. })
                && event.run_path.iter().any(
                    |frame| matches!(frame, PathFrame::Step { step_id } if step_id.as_str() == "s1"),
                )
        })
        .count();
    assert_eq!(s1_spans, 1);
}

// ─── step-level precedence over flow-level ──────────────────────────────────

#[tokio::test]
async fn step_level_handlers_take_precedence() {
    let dir = TempStoreDir::new("precedence");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": false })),
        succeeded_with(json!({ "ok": true })),
    ]));
    // Flow-level would escalate; the step-level retry wins.
    let step_handlers = json!([{
        "hook": "onFail",
        "action": { "kind": "retry", "policy": {
            "maxAttempts": 1, "backoffMs": 0, "retryOn": []
        } },
        "maxTriggers": 1
    }]);
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step(
            "s1",
            vec![expect_ok("a1", "s1")],
            Some(step_handlers),
        )],
        Some(escalate_judge("onFail", 1)),
    );
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-prec"))
        .await
        .expect("run");
    assert!(matches!(
        outcome,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Pass
    ));
    assert_eq!(dispatched(&provider), 2, "step-level retry re-acted");
    assert!(
        !event_types(&store, "run-prec").contains(&"humanRequested"),
        "the flow-level escalate never fired"
    );
}

// ─── repair: a hook-framed subflow runs, then the host re-acts ──────────────

#[tokio::test]
async fn on_fail_repair_runs_subflow_then_reacts() {
    let dir = TempStoreDir::new("repair");
    let mut store = Store::open(dir.path()).expect("open store");
    // Dispatch order: s1 (fail-assert), repair step, s1 re-act (pass).
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": false })),
        succeeded_with(json!({ "fixed": true })),
        succeeded_with(json!({ "ok": true })),
    ]));

    // The repair callee: one readonly action, no assertions (unverified).
    let mut repair: FlowIR = serde_json::from_value(json!({
        "irVersion": 1,
        "flowId": "fix_world",
        "irHash": h64('a'),
        "provider": { "name": "devicerail", "version": "0.1.0" },
        "requiredFeatures": [],
        "lockfileDigest": provider.lockfile().digest.as_str(),
        "params": [],
        "outputs": [],
        "body": [ {
            "kind": "action",
            "stepId": "fix",
            "effectHash": h64('0'),
            "judgeHash": h64('0'),
            "checkpoint": true,
            "effect": "readonly",
            "idempotent": true,
            "binding": { "attempts": [ {
                "channel": "uiTree",
                "actionName": "tapElement",
                "args": { "element": { "lit": { "identifier": "fix" } } },
                "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
                "protection": "standard"
            } ] },
            "assertions": []
        } ],
        "verdictPolicy": "standard",
        "sourceMap": [],
        "subflows": {}
    }))
    .expect("repair flow");
    seal(&mut repair);

    let handlers = json!([{
        "hook": "onFail",
        "action": { "kind": "repair", "flowRef": {
            "flowId": "fix_world", "irHash": repair.ir_hash.as_str()
        } },
        "maxTriggers": 1
    }]);
    let mut flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step(
            "s1",
            vec![expect_ok("a1", "s1")],
            Some(handlers),
        )],
        None,
    );
    flow.subflows.insert(
        serde_json::from_value(json!("fix_world")).expect("flow id"),
        serde_json::from_value(json!({
            "flowId": "fix_world", "irHash": repair.ir_hash.as_str()
        }))
        .expect("flow ref"),
    );
    seal(&mut flow);

    let mut subflows = BTreeMap::new();
    subflows.insert(repair.ir_hash.clone(), repair);
    let session = open(&provider).await;
    let mut opts = run_opts("run-repair");
    opts.subflows = subflows;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, opts)
        .await
        .expect("run");
    assert!(matches!(
        outcome,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Pass
    ));
    assert_eq!(dispatched(&provider), 3, "host, repair step, host re-act");
    let types = event_types(&store, "run-repair");
    assert_eq!(
        types.iter().filter(|t| **t == "handlerTriggered").count(),
        1
    );
    assert_eq!(
        types.iter().filter(|t| **t == "callFramePushed").count(),
        1,
        "the repair ran in a hook call frame"
    );
    // The repair step's events carry the hook frame in their path.
    let events = store.events("run-repair").expect("events");
    let fix_entered = events
        .iter()
        .find(|event| {
            matches!(&event.payload, RunLogPayload::StepEntered { .. })
                && event.run_path.iter().any(
                    |frame| matches!(frame, PathFrame::Step { step_id } if step_id.as_str() == "fix"),
                )
        })
        .expect("repair step entered");
    assert!(
        fix_entered
            .run_path
            .iter()
            .any(|frame| matches!(frame, PathFrame::Hook { .. })),
        "repair work is hook-framed: {:?}",
        fix_entered.run_path
    );
}

// ─── onResumeDrift: escalate repairWorld → repaired → re-probe ──────────────

#[tokio::test]
async fn drift_escalate_repair_world_reprobes() {
    let dir = TempStoreDir::new("drift");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": true }))]));
    // A preflight expr probe that always fails: eq(1, 2).
    let probe = json!({
        "assertId": "world_ready",
        "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
            { "lit": 1 }, { "lit": 2 }
        ] } },
        "verifyVia": [],
        "onMissingInput": "unknown"
    });
    let mut step = action_step("s1", vec![expect_ok("a1", "s1")], None);
    step["preflight"] = json!([probe]);
    let escalate = json!([{
        "hook": "onResumeDrift",
        "action": { "kind": "escalate", "human": {
            "kind": "human",
            "stepId": "esc-drift",
            "effectHash": h64('0'),
            "judgeHash": h64('0'),
            "checkpoint": true,
            "mode": "repairWorld",
            "prompt": "fix the world",
            "presents": [],
            "timeoutMs": 3_600_000u64,
            "onTimeout": "unknown"
        } },
        "maxTriggers": 1
    }]);
    let flow = flow_fixture(&provider.lockfile().digest, vec![step], Some(escalate));
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-drift"))
        .await
        .expect("run");
    let RunOutcome::AwaitingHuman { pending } = outcome else {
        panic!("expected AwaitingHuman, got {outcome:?}");
    };

    // "done" re-probes; the probe is a pure lit-expr and still fails →
    // budget exhausted → Blocked{Drifted}. (The honest ladder end: the
    // probe, not the declaration, readmits — principle 4.)
    store
        .submit_human_response(
            "run-drift",
            &pending.request_id,
            "cli:tester",
            1,
            json!({ "decision": "done" }),
        )
        .expect("arbitrate");
    let session = open(&provider).await;
    let outcome = Runner::resume(
        &flow,
        "run-drift",
        session,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("resume");
    // The checkpoint SAYS the frontier drifted (spine §6.2 / §6.6): the
    // re-probe missed, the escalate budget is exhausted, and `drifted` is
    // what the missed probe materialized — a run suspended on drift was
    // indistinguishable from a merely-pending one before this arm existed.
    assert_eq!(
        store
            .verify_checkpoint("run-drift")
            .expect("verify")
            .frontier
            .state,
        pointlock_ir::StepState::Drifted
    );
    assert!(
        matches!(outcome, RunOutcome::Blocked { .. }),
        "still-failing probe after repair blocks: {outcome:?}"
    );
    assert_eq!(dispatched(&provider), 0, "the act never ran");
    // Two probe rounds on the ledger (initial + post-repair).
    let probes = event_types(&store, "run-drift")
        .iter()
        .filter(|t| **t == "preflightProbed")
        .count();
    assert!(probes >= 2, "re-probe after the repaired ruling");
}

// ─── call-step re-invocation dispositions are typed refusals ────────────────

#[tokio::test]
async fn call_step_retry_disposition_is_a_typed_refusal() {
    let provider = FakeProvider::new(VecDeque::new());
    let mut callee: FlowIR = serde_json::from_value(json!({
        "irVersion": 1,
        "flowId": "callee",
        "irHash": h64('a'),
        "provider": { "name": "devicerail", "version": "0.1.0" },
        "requiredFeatures": [],
        "lockfileDigest": provider.lockfile().digest.as_str(),
        "params": [],
        "outputs": [],
        "body": [],
        "verdictPolicy": "standard",
        "sourceMap": [],
        "subflows": {}
    }))
    .expect("callee");
    seal(&mut callee);

    let call_step = json!({
        "kind": "call",
        "stepId": "c1",
        "effectHash": h64('0'),
        "judgeHash": h64('0'),
        "checkpoint": true,
        "flowRef": { "flowId": "callee", "irHash": callee.ir_hash.as_str() },
        "inputs": {},
        "handlers": [{
            "hook": "onFail",
            "action": { "kind": "retry", "policy": {
                "maxAttempts": 1, "backoffMs": 0, "retryOn": []
            } },
            "maxTriggers": 1
        }]
    });
    let mut flow = flow_fixture(&provider.lockfile().digest, vec![call_step], None);
    flow.subflows.insert(
        serde_json::from_value(json!("callee")).expect("flow id"),
        serde_json::from_value(json!({
            "flowId": "callee", "irHash": callee.ir_hash.as_str()
        }))
        .expect("flow ref"),
    );
    seal(&mut flow);

    let mut subflows = BTreeMap::new();
    subflows.insert(callee.ir_hash.clone(), callee);
    let dir = TempStoreDir::new("call-refusal");
    let mut store = Store::open(dir.path()).expect("open store");
    let session = open(&provider).await;
    let mut opts = run_opts("run-call-refusal");
    opts.subflows = subflows;
    let result = Runner::run(&flow, json!({}), session, &mut store, opts).await;
    assert!(
        matches!(result, Err(RunnerError::NotInM0Subset { .. })),
        "load refuses re-invocation dispositions on call steps: {result:?}"
    );
}

// ─── providerStateSummary discard on aborted follow-up exits (07 §2.2) ──────

/// An `abort` disposition after a fail verdict exits the step `aborted`
/// — that exit makes no semantic claim and must NOT carry the captured
/// profile (the fail verdict itself stays on the ledger regardless).
#[tokio::test]
async fn an_aborted_exit_discards_the_captured_summary() {
    let dir = TempStoreDir::new("summary-abort");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": false }))]));
    let handlers = json!([{
        "hook": "onFail",
        "action": { "kind": "abort" },
        "maxTriggers": 1
    }]);
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step(
            "doomed",
            vec![expect_ok("a1", "doomed")],
            Some(handlers),
        )],
        None,
    );
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-summary-abort"),
    )
    .await
    .expect("run");
    assert!(matches!(outcome, RunOutcome::Finished { .. }));

    let events = store.events("run-summary-abort").expect("events");
    for event in &events {
        if let RunLogPayload::StepExited {
            state,
            provider_state_summary,
            ..
        } = &event.payload
        {
            assert_eq!(*state, pointlock_ir::StepState::Aborted);
            assert!(
                provider_state_summary.is_none(),
                "an aborted exit must not carry the profile"
            );
        }
    }
    store.verify_checkpoint("run-summary-abort").expect("exact");
}

// ─── session_degraded: unknown step AND a flow-level onError (spine §5) ──────

/// The spine §5 error table gives `session_degraded` two effects at once:
/// 「当前 step → unknown，触发 flow 级 onError handler」. The step folding to
/// unknown is the easy half; routing it to `onError` is the half that was
/// missing, because the unknown carried no error class and the hook
/// selector could only reach `onUnknown` — so a flow that declared
/// `on_error: { errorClasses: [session_degraded] }` silently never fired.
#[tokio::test]
async fn session_degraded_folds_unknown_and_fires_the_flow_level_on_error() {
    let dir = TempStoreDir::new("degraded");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        failed_final("session_degraded"),
        succeeded_with(json!({ "ok": true })),
    ]));
    // FLOW level, not step level: the whole point of the row.
    let handlers = json!([{
        "hook": "onError",
        "errorClasses": ["session_degraded"],
        "action": { "kind": "retry", "policy": {
            "maxAttempts": 1, "backoffMs": 0, "retryOn": []
        } },
        "maxTriggers": 2
    }]);
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step("s1", vec![expect_ok("a1", "s1")], None)],
        Some(handlers),
    );
    let outcome = Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-degraded"),
    )
    .await
    .expect("run");

    assert!(
        event_types(&store, "run-degraded").contains(&"handlerTriggered"),
        "the flow-level onError must fire: {:?}",
        event_types(&store, "run-degraded")
    );
    assert_eq!(
        dispatched(&provider),
        2,
        "the handler's retry re-dispatched"
    );
    assert!(
        matches!(
            outcome,
            RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Pass
        ),
        "{outcome:?}"
    );

    // Control: the same degradation with an `onUnknown` binding must NOT
    // fire it — `session_degraded` is an ERROR-path unknown, and routing it
    // to onUnknown would be the old behaviour wearing a new name.
    let dir2 = TempStoreDir::new("degraded-unknown");
    let mut store2 = Store::open(dir2.path()).expect("open store");
    let provider2 = FakeProvider::new(VecDeque::from([failed_final("session_degraded")]));
    let handlers2 = json!([{
        "hook": "onUnknown",
        "action": { "kind": "continue" },
        "maxTriggers": 1
    }]);
    let flow2 = flow_fixture(
        &provider2.lockfile().digest,
        vec![action_step("s1", vec![expect_ok("a1", "s1")], None)],
        Some(handlers2),
    );
    Runner::run(
        &flow2,
        json!({}),
        open(&provider2).await,
        &mut store2,
        run_opts("run-degraded-u"),
    )
    .await
    .expect("run");
    assert!(
        !event_types(&store2, "run-degraded-u").contains(&"handlerTriggered"),
        "an onUnknown binding must not catch an error-path degradation"
    );
}

/// 07 §5.2's hook ruling: 「hook 帧下的记录（handler 审计痕）不参与对齐复用
/// ……旧 hook 记录一律归档」 — archive them, do NOT refuse the resume. This
/// used to be a hard `M0Unsupported`, which cost a real and unremarkable
/// case: a run whose `onFail` repair subflow completed could never be
/// repaired cross-IR afterwards. Archival is structural, and the test reads
/// all three halves of it: the resume proceeds, the repair's record is not
/// adopted, and it is not misreported as absent from the new IR either.
#[tokio::test]
async fn a_completed_repair_frame_is_archived_not_a_refusal() {
    let dir = TempStoreDir::new("repair-crossir");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": false })), // s1 fails its assertion
        succeeded_with(json!({ "fixed": true })), // the repair step
        succeeded_with(json!({ "ok": true })),  // s1 re-acts and passes
        succeeded_with(json!({ "ok": true })),  // s2
        succeeded_with(json!({ "ok": true })),  // s2 replayed after the resume
    ]));
    let mut repair: FlowIR = serde_json::from_value(json!({
        "irVersion": 1,
        "flowId": "fix_world",
        "irHash": h64('a'),
        "provider": { "name": "devicerail", "version": "0.1.0" },
        "requiredFeatures": [],
        "lockfileDigest": provider.lockfile().digest.as_str(),
        "params": [],
        "outputs": [],
        "body": [ {
            "kind": "action", "stepId": "fix",
            "effectHash": h64('0'), "judgeHash": h64('0'), "checkpoint": true,
            "effect": "readonly", "idempotent": true,
            "binding": { "attempts": [ {
                "channel": "uiTree", "actionName": "tapElement",
                "args": { "element": { "lit": { "identifier": "fix" } } },
                "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
                "protection": "standard"
            } ] },
            "assertions": []
        } ],
        "verdictPolicy": "standard", "sourceMap": [], "subflows": {}
    }))
    .expect("repair flow");
    seal(&mut repair);
    let handlers = json!([{
        "hook": "onFail",
        "action": { "kind": "repair", "flowRef": {
            "flowId": "fix_world", "irHash": repair.ir_hash.as_str()
        } },
        "maxTriggers": 1
    }]);

    let build = |s2_target: &str| {
        let mut s2 = action_step("s2", vec![expect_ok("a2", "s2")], None);
        s2["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] = json!(s2_target);
        let mut flow = flow_fixture(
            &provider.lockfile().digest,
            vec![
                action_step("s1", vec![expect_ok("a1", "s1")], Some(handlers.clone())),
                s2,
            ],
            None,
        );
        flow.subflows.insert(
            serde_json::from_value(json!("fix_world")).expect("flow id"),
            serde_json::from_value(json!({
                "flowId": "fix_world", "irHash": repair.ir_hash.as_str()
            }))
            .expect("flow ref"),
        );
        seal(&mut flow);
        flow
    };
    let mut subflows = BTreeMap::new();
    subflows.insert(repair.ir_hash.clone(), repair.clone());

    let flow = build("s2_v1");
    let mut opts = run_opts("run-hook-x");
    opts.subflows = subflows.clone();
    Runner::run(&flow, json!({}), open(&provider).await, &mut store, opts)
        .await
        .expect("run");

    // The precondition: a hook-framed record on the ledger, frame popped.
    let view = store.verify_checkpoint("run-hook-x").expect("verify");
    let hook_records: Vec<_> = view
        .completed
        .iter()
        .filter(|record| {
            record
                .run_path
                .iter()
                .any(|frame| matches!(frame, PathFrame::Hook { .. }))
        })
        .collect();
    assert_eq!(hook_records.len(), 1, "the repair step left a record");
    assert_eq!(hook_records[0].step_id.as_str(), "fix");
    assert_eq!(view.frames.len(), 1, "the hook call frame was popped");

    // Retarget s2 — a cross-IR resume that used to be refused outright.
    let repaired = build("s2_v2");
    assert_ne!(repaired.ir_hash, flow.ir_hash);
    let outcome = Runner::resume_with_subflows(
        &repaired,
        &subflows,
        "run-hook-x",
        open(&provider).await,
        &mut store,
        ResumeOptions {
            // s2 is mutating, not idempotent, and already passed.
            allow_mutating_reexec: vec!["s2".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("a completed repair frame must not refuse the resume");
    assert!(matches!(
        outcome,
        RunOutcome::Finished { verdict: Some(ref v) } if v.status == VerdictStatus::Pass
    ));
    // s1 (and its repair) were adopted; only s2 ran again.
    assert_eq!(dispatched(&provider), 5);

    // The repair record is neither adopted nor called absent from the new
    // IR: it is simply archived with its frame.
    let report = store
        .events("run-hook-x")
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
    assert!(
        !report
            .entries
            .iter()
            .any(|entry| entry.step_id.as_str() == "fix"),
        "a handler audit trace is not a step of the flow body: {:?}",
        report.entries
    );
}

// ─── the §5.4 container gate must read BODY effect, not handler audit ───────

/// Builds `if branch { t1 }` where `t1` is mutating, non-idempotent, and
/// carries an `onFail` repair. `cond_flipped` moves the if's `effectHash`
/// without changing which branch runs, so cross-IR alignment refuses to
/// descend and the `if` is judged as a whole.
fn repair_inside_branch(digest: &Hash, repair: &FlowIR, cond_flipped: bool, hook: &str) -> FlowIR {
    let handlers = json!([{
        "hook": hook,
        "action": { "kind": "repair", "flowRef": {
            "flowId": "fix_world", "irHash": repair.ir_hash.as_str()
        } },
        "maxTriggers": 1
    }]);
    let cond = if cond_flipped {
        json!({ "fn": "eq", "args": [ { "lit": "yes" }, { "ref": "params.mode" } ] })
    } else {
        json!({ "fn": "eq", "args": [ { "ref": "params.mode" }, { "lit": "yes" } ] })
    };
    let mut flow: FlowIR = serde_json::from_value(json!({
        "irVersion": 1,
        "flowId": "w3_demo",
        "irHash": h64('e'),
        "provider": { "name": "devicerail", "version": "0.1.0" },
        "requiredFeatures": [],
        "lockfileDigest": digest.as_str(),
        "params": [ { "name": "mode", "schema": { "type": "string" }, "required": true } ],
        "outputs": [],
        "body": [ {
            "kind": "if",
            "stepId": "branch",
            "effectHash": h64('0'),
            "judgeHash": h64('0'),
            "checkpoint": true,
            "cond": cond,
            "then": [ action_step("t1", vec![expect_ok("a1", "t1")], Some(handlers)) ],
            "else": []
        } ],
        "verdictPolicy": "standard",
        "sourceMap": [],
        "subflows": { "fix_world": {
            "flowId": "fix_world", "irHash": repair.ir_hash.as_str()
        } }
    }))
    .expect("fixture is a valid FlowIR");
    seal(&mut flow);
    flow
}

fn repair_flow(digest: &Hash) -> FlowIR {
    let mut repair: FlowIR = serde_json::from_value(json!({
        "irVersion": 1, "flowId": "fix_world", "irHash": h64('a'),
        "provider": { "name": "devicerail", "version": "0.1.0" },
        "requiredFeatures": [], "lockfileDigest": digest.as_str(),
        "params": [], "outputs": [],
        "body": [ {
            "kind": "action", "stepId": "fix",
            "effectHash": h64('0'), "judgeHash": h64('0'), "checkpoint": true,
            "effect": "readonly", "idempotent": true,
            "binding": { "attempts": [ {
                "channel": "uiTree", "actionName": "tapElement",
                "args": { "element": { "lit": { "identifier": "fix" } } },
                "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
                "protection": "standard"
            } ] },
            "assertions": []
        } ],
        "verdictPolicy": "standard", "sourceMap": [], "subflows": {}
    }))
    .expect("repair flow");
    seal(&mut repair);
    repair
}

async fn run_branch_then_resume(
    tag: &str,
    hook: &str,
    outcomes: VecDeque<ScriptedOutcome>,
) -> Result<RunOutcome, RunnerError> {
    let dir = TempStoreDir::new(tag);
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(outcomes);
    let digest = provider.lockfile().digest.clone();
    let repair = repair_flow(&digest);
    let subflows = BTreeMap::from([(repair.ir_hash.clone(), repair.clone())]);

    let flow = repair_inside_branch(&digest, &repair, false, hook);
    let mut opts = run_opts(tag);
    opts.subflows = subflows.clone();
    Runner::run(
        &flow,
        json!({ "mode": "yes" }),
        open(&provider).await,
        &mut store,
        opts,
    )
    .await
    .expect("run");

    // Precondition: the repair left a hook-framed record inside the branch.
    let view = store.verify_checkpoint(tag).expect("verify");
    assert!(
        view.completed.iter().any(|record| record
            .run_path
            .iter()
            .any(|frame| matches!(frame, PathFrame::Hook { .. }))),
        "the repair must have left a hook-framed record"
    );

    let flipped = repair_inside_branch(&digest, &repair, true, hook);
    Runner::resume_with_subflows(
        &flipped,
        &subflows,
        tag,
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
}

/// A handler's audit trace is not body work. `gated_effect` derives "this
/// container's body mutates" from the body closure alone — it never walks
/// `handlers` — so counting a completed repair as effect evidence for the
/// container gates a body that did nothing, with a `reason` string that is
/// plainly false. Here `t1`'s every attempt FAILED, so nothing in the
/// branch took effect; only the repair did.
#[tokio::test]
async fn a_completed_repair_is_not_body_effect_for_its_container() {
    let outcome = run_branch_then_resume(
        "gate-hookonly",
        // An action-level failure is an ERROR-path negative (spine §5), so
        // the repair hangs off `onError`; `onFail` is for a completed
        // assertion negative, which would mean the act DID take effect.
        "onError",
        VecDeque::from([
            failed_final("element_not_found"),     // t1 — never takes effect
            succeeded_with(json!({ "ok": true })), // the repair
            failed_final("element_not_found"),     // t1 again, still no effect
            failed_final("element_not_found"),     // t1 replayed after the resume
            succeeded_with(json!({ "ok": true })), // its repair again
            failed_final("element_not_found"),
        ]),
    )
    .await;
    match outcome {
        Ok(_) => {}
        Err(RunnerError::RequiresConfirmation { report }) => panic!(
            "nothing in the branch body took effect; the gate read the handler's audit \
             trace as body effect: {:?}",
            report.requires_confirmation
        ),
        Err(other) => panic!("unexpected: {other}"),
    }
}

/// The anti-over-filter control for the same rule: with the SAME hook
/// record present, a branch whose body step actually took effect must still
/// gate. The fix removes handler evidence, never body evidence.
#[tokio::test]
async fn a_container_whose_body_took_effect_still_gates() {
    let outcome = run_branch_then_resume(
        "gate-bodyeffect",
        "onFail",
        VecDeque::from([
            succeeded_with(json!({ "ok": false })), // t1 acts, assertion refuses
            succeeded_with(json!({ "ok": true })),  // the repair
            succeeded_with(json!({ "ok": true })),  // t1 re-acts and passes
        ]),
    )
    .await;
    let Err(RunnerError::RequiresConfirmation { report }) = outcome else {
        panic!("a branch whose body step passed must gate, got {outcome:?}");
    };
    assert!(
        report.requires_confirmation.iter().any(|gate| gate
            .step_id
            .as_ref()
            .is_some_and(|id| id.as_str() == "branch")),
        "the container must be the gated entry: {:?}",
        report.requires_confirmation
    );
}

/// An escalate hook human still awaiting an answer is unfinished handler
/// work that leaves NO other trace: it opens no step span and pushes no
/// call frame («hook humans are not body steps»), so `live_frames`,
/// `frontier` and `completed` are all blind to it — `humanPending` is the
/// only carrier. Cross-IR that is genuinely unsafe: the continuation is
/// looked up by an instance key rebuilt from the NEW host path, so renaming
/// the host mints a SECOND request and strands the first unanswerable.
/// Same-IR rebuilds the same key and settles correctly, which is why the
/// refusal is cross-IR only (the neighbouring escalate test pins that).
#[tokio::test]
async fn a_pending_hook_escalation_refuses_a_cross_ir_resume() {
    let dir = TempStoreDir::new("escalate-crossir");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": false })), // s1's assertion refuses
    ]));
    let build = |s2_target: &str| {
        let mut s2 = action_step("s2", vec![expect_ok("a2", "s2")], None);
        s2["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] = json!(s2_target);
        flow_fixture(
            &provider.lockfile().digest,
            vec![action_step("s1", vec![expect_ok("a1", "s1")], None), s2],
            Some(escalate_judge("onFail", 1)),
        )
    };
    let flow = build("s2_v1");
    let outcome = Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-esc-x"),
    )
    .await
    .expect("run");
    assert!(matches!(outcome, RunOutcome::AwaitingHuman { .. }));

    // The precondition, stated: nothing except `humanPending` records it.
    let view = store.verify_checkpoint("run-esc-x").expect("verify");
    let pending = view.human_pending.as_ref().expect("a pending escalation");
    assert!(
        pending
            .run_path
            .iter()
            .any(|frame| matches!(frame, PathFrame::Hook { .. }))
    );
    assert_eq!(view.frames.len(), 1, "a hook human pushes no call frame");
    assert!(
        !view.completed.iter().any(|record| record
            .run_path
            .iter()
            .any(|frame| matches!(frame, PathFrame::Hook { .. }))),
        "a hook human opens no span, so it leaves no record"
    );

    let error = Runner::resume(
        &build("s2_v2"),
        "run-esc-x",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect_err("a pending handler escalation must not resume under a repaired IR");
    let text = error.to_string();
    assert!(
        text.contains("handler escalation is still awaiting an answer"),
        "expected the pending-escalation refusal, got: {text}"
    );
    assert_eq!(dispatched(&provider), 1, "the refusal is pre-execution");
}
