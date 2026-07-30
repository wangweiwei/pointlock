//! M2 control-flow tests over the FakeProvider and a temp-dir store:
//! nested subflow calls with the call-by-value gates, foreach iteration
//! (including halt folding and the positional resume regime), if branch
//! accounting (skipped pairs), assert steps (fresh / fromStep), preflight
//! probing (pass and drifted), frame-precise resume into a callee, and
//! the evidence-localization degradation rules.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use async_trait::async_trait;
use pointlock_ir::{
    ActionOutcome, ActionResult, AssetRef, EventCursor, FlowIR, Hash, IterState, Observation,
    PathFrame, ReconcileResult, RunLogEvent, RunLogPayload, StepIR, StepState, VerdictStatus,
};
use pointlock_provider_kit::{
    BoundActionCall, CancellationToken, CapabilityAttestation, EvidenceStream, FakeProvider,
    ObserveRequest, Provider, ProviderError, ProviderSession, ScriptedOutcome, SessionHealth,
    SessionOutcome, UiSnapshotOutcome, VerdictWrite,
};
use pointlock_runner::{ResumeOptions, RunOptions, RunOutcome, Runner};
use pointlock_store::Store;
use serde_json::{Value, json};

// ─── Fixture helpers ────────────────────────────────────────────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique per-test store directory under the system temp dir, removed on
/// drop (same pattern as the runner tests; no tempfile dependency).
struct TempStoreDir(PathBuf);

impl TempStoreDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-control-flow-test-{tag}-{}-{}",
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

/// Recomputes and stores the per-step dual hashes (recursively — container
/// subtrees carry their own hashes) and the flow irHash: a minimal
/// stand-in for the compiler seal phase.
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

/// A sealed flow from raw JSON parts.
fn build_flow(
    flow_id: &str,
    lockfile_digest: &Hash,
    params: Value,
    outputs: Value,
    body: Vec<Value>,
    subflows: Value,
) -> FlowIR {
    let mut flow: FlowIR = serde_json::from_value(json!({
        "irVersion": 1,
        "flowId": flow_id,
        "irHash": h64('e'),
        "provider": { "name": "devicerail", "version": "0.1.0" },
        "requiredFeatures": [],
        "lockfileDigest": lockfile_digest.as_str(),
        "params": params,
        "outputs": outputs,
        "body": body,
        "verdictPolicy": "standard",
        "sourceMap": [],
        "subflows": subflows
    }))
    .expect("fixture is a valid FlowIR");
    seal(&mut flow);
    flow
}

fn flow_fixture(lockfile_digest: &Hash, body: Vec<Value>) -> FlowIR {
    build_flow(
        "cf_demo",
        lockfile_digest,
        json!([]),
        json!([]),
        body,
        json!({}),
    )
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
        PathFrame::Call {
            step_id: Some(step_id),
            ..
        } => Some(step_id.as_str()),
        _ => None,
    })
}

fn finished_verdict(outcome: RunOutcome) -> pointlock_ir::Verdict {
    let RunOutcome::Finished {
        verdict: Some(verdict),
    } = outcome
    else {
        panic!("expected Finished with a verdict, got {outcome:?}");
    };
    verdict
}

/// Delegating session wrapper that cancels a stop token once `remaining`
/// executes have completed — the deterministic step-boundary stop.
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

// ─── nested subflow fixtures ────────────────────────────────────────────────

/// Builds the two-layer call closure:
/// root(r1, call→mid, r2) → mid(call→inner, m1) → inner(g1), with the
/// label travelling call-by-value down and echoing back up through the
/// declared outputs.
fn nested_fixture(digest: &Hash) -> (FlowIR, BTreeMap<Hash, FlowIR>) {
    let inner = build_flow(
        "inner",
        digest,
        json!([ { "name": "label", "schema": { "type": "string" }, "required": true } ]),
        json!([ { "name": "echo", "schema": { "type": "string" },
                  "from": { "ref": "params.label" } } ]),
        vec![action_step("g1", vec![expect_ok("ga", "g1")])],
        json!({}),
    );
    let mid = build_flow(
        "mid",
        digest,
        json!([ { "name": "label", "schema": { "type": "string" }, "required": true } ]),
        json!([ { "name": "midEcho", "schema": { "type": "string" },
                  "from": { "ref": "steps.callInner.output.echo" } } ]),
        vec![
            json!({
                "kind": "call",
                "stepId": "callInner",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "flowRef": { "flowId": "inner", "irHash": inner.ir_hash.as_str() },
                "inputs": { "label": { "ref": "params.label" } }
            }),
            action_step("m1", vec![expect_ok("ma", "m1")]),
        ],
        json!({ "inner": { "flowId": "inner", "irHash": inner.ir_hash.as_str() } }),
    );
    let root = build_flow(
        "root",
        digest,
        json!([]),
        json!([]),
        vec![
            action_step("r1", vec![expect_ok("ra", "r1")]),
            json!({
                "kind": "call",
                "stepId": "callMid",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "flowRef": { "flowId": "mid", "irHash": mid.ir_hash.as_str() },
                "inputs": { "label": { "lit": "hello" } }
            }),
            json!({
                "kind": "action",
                "stepId": "r2",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "effect": "mutating",
                "idempotent": false,
                "binding": { "attempts": [ {
                    "channel": "uiTree",
                    "actionName": "tapElement",
                    "args": { "element": { "lit": { "identifier": "r2" } } },
                    "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
                    "protection": "standard"
                } ] },
                "assertions": [ {
                    "assertId": "echoBack",
                    "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
                        { "ref": "steps.callMid.output.midEcho" },
                        { "lit": "hello" }
                    ] } },
                    "verifyVia": [],
                    "onMissingInput": "unknown"
                } ]
            }),
        ],
        json!({ "mid": { "flowId": "mid", "irHash": mid.ir_hash.as_str() } }),
    );
    let mut registry = BTreeMap::new();
    registry.insert(inner.ir_hash.clone(), inner);
    registry.insert(mid.ir_hash.clone(), mid);
    (root, registry)
}

// ─── (a) nested subflow end to end ──────────────────────────────────────────

#[tokio::test]
async fn nested_two_layer_call_passes_end_to_end() {
    let dir = TempStoreDir::new("nested");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // r1
        succeeded_with(json!({ "ok": true })), // g1 (inner)
        succeeded_with(json!({ "ok": true })), // m1 (mid)
        succeeded_with(json!({ "ok": true })), // r2
    ]));
    let (root, registry) = nested_fixture(&provider.lockfile().digest);
    let mid_flow = registry
        .values()
        .find(|flow| flow.flow_id.as_str() == "mid")
        .expect("mid registered");
    let inner_flow = registry
        .values()
        .find(|flow| flow.flow_id.as_str() == "inner")
        .expect("inner registered");
    let (mid_hash8, inner_hash8) = (
        mid_flow.ir_hash.hex_prefix8().to_owned(),
        inner_flow.ir_hash.hex_prefix8().to_owned(),
    );
    let mut opts = run_opts("run-nested");
    opts.subflows = registry;
    let session = open(&provider).await;
    let outcome = Runner::run(&root, json!({}), session, &mut store, opts)
        .await
        .expect("run");
    let verdict = finished_verdict(outcome);
    assert_eq!(verdict.status, VerdictStatus::Pass);
    assert!(!verdict.degraded);

    // Ledger order, verbatim: the frame events bracket the callee bodies.
    let action_block = [
        "stepEntered",
        "actionIntent",
        "actionSettled",
        "assertionEvaluated",
        "verdictRecorded",
        "stepExited",
    ];
    let mut expected = vec!["runStarted"];
    expected.extend(action_block); // r1
    expected.extend(["stepEntered", "callFramePushed"]); // callMid
    expected.extend(["stepEntered", "callFramePushed"]); // callInner
    expected.extend(action_block); // g1
    expected.extend(["callFramePopped", "verdictRecorded", "stepExited"]); // callInner
    expected.extend(action_block); // m1
    expected.extend(["callFramePopped", "verdictRecorded", "stepExited"]); // callMid
    expected.extend(action_block); // r2
    expected.push("runFinished");
    assert_eq!(event_types(&store, "run-nested"), expected);

    // Materialized == rebuilt (I1) with nested frames folded away.
    let view = store.verify_checkpoint("run-nested").expect("verify");
    assert_eq!(view.frames.len(), 1);
    assert_eq!(view.frames[0].next_index, 3);

    // The callee-internal record carries the full nested run path.
    let g1 = view
        .completed
        .iter()
        .find(|record| record.step_id.as_str() == "g1")
        .expect("g1 record");
    assert_eq!(
        pointlock_ir::render_run_path(&g1.run_path),
        format!(
            "root@{}/callMid/call→mid@{mid_hash8}/callInner/call→inner@{inner_hash8}/g1",
            root.ir_hash.hex_prefix8(),
        )
    );

    // Call-by-value both ways: the call step's inputs snapshot and the
    // outbound outputs snapshot are archived on the records.
    let call_mid = view
        .completed
        .iter()
        .find(|record| record.step_id.as_str() == "callMid")
        .expect("callMid record");
    assert_eq!(call_mid.resolved_inputs, json!({ "label": "hello" }));
    assert_eq!(call_mid.output, Some(json!({ "midEcho": "hello" })));
    // The call step's verdict is the callee's flow verdict (spine §6.3).
    assert_eq!(
        call_mid.verdict.as_ref().map(|verdict| verdict.status),
        Some(VerdictStatus::Pass)
    );
    assert_eq!(provider.handle().dispatched_call_ids().len(), 4);
}

#[tokio::test]
async fn call_inbound_gate_refuses_schema_invalid_inputs() {
    let dir = TempStoreDir::new("gate-in");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let digest = provider.lockfile().digest.clone();
    let callee = build_flow(
        "callee",
        &digest,
        json!([ { "name": "label", "schema": { "type": "string" }, "required": true } ]),
        json!([]),
        vec![action_step("c1", vec![])],
        json!({}),
    );
    let root = build_flow(
        "root",
        &digest,
        json!([]),
        json!([]),
        vec![
            json!({
                "kind": "call",
                "stepId": "callBad",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "flowRef": { "flowId": "callee", "irHash": callee.ir_hash.as_str() },
                // A number against {"type":"string"}: the inbound gate
                // must fail bind-class, before any frame is pushed.
                "inputs": { "label": { "lit": 42 } }
            }),
            action_step("after", vec![]),
        ],
        json!({ "callee": { "flowId": "callee", "irHash": callee.ir_hash.as_str() } }),
    );
    let mut registry = BTreeMap::new();
    registry.insert(callee.ir_hash.clone(), callee);
    let mut opts = run_opts("run-gate-in");
    opts.subflows = registry;
    let session = open(&provider).await;
    let outcome = Runner::run(&root, json!({}), session, &mut store, opts)
        .await
        .expect("run");
    let verdict = finished_verdict(outcome);
    assert_eq!(verdict.status, VerdictStatus::Fail);

    // No frame was pushed and nothing dispatched: the gate is pre-frame.
    let types = event_types(&store, "run-gate-in");
    assert!(!types.contains(&"callFramePushed"));
    assert!(provider.handle().dispatched_call_ids().is_empty());

    let events = store.events("run-gate-in").expect("events");
    let call_verdict = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded { verdict, .. } if step_of(event) == Some("callBad") => {
                Some(verdict.clone())
            }
            _ => None,
        })
        .expect("call step verdict");
    assert_eq!(call_verdict.status, VerdictStatus::Fail);
    assert!(
        call_verdict.summary.contains("bind_arguments_invalid"),
        "got {}",
        call_verdict.summary
    );
    // Halt-on-fail blocked the downstream step.
    let view = store.verify_checkpoint("run-gate-in").expect("verify");
    let after = view
        .completed
        .iter()
        .find(|record| record.step_id.as_str() == "after")
        .expect("after record");
    assert!(after.attempts.is_empty() && after.verdict.is_none());
}

#[tokio::test]
async fn call_outbound_gate_fails_on_schema_invalid_outputs() {
    let dir = TempStoreDir::new("gate-out");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": true }))]));
    let digest = provider.lockfile().digest.clone();
    let callee = build_flow(
        "callee",
        &digest,
        json!([]),
        // The declared output evaluates to a boolean against
        // {"type":"string"}: the outbound gate must fail after the body.
        json!([ { "name": "flag", "schema": { "type": "string" },
                  "from": { "ref": "steps.c1.output.ok" } } ]),
        vec![action_step("c1", vec![expect_ok("ca", "c1")])],
        json!({}),
    );
    let root = build_flow(
        "root",
        &digest,
        json!([]),
        json!([]),
        vec![json!({
            "kind": "call",
            "stepId": "callBad",
            "effectHash": h64('0'),
            "judgeHash": h64('0'),
            "checkpoint": true,
            "flowRef": { "flowId": "callee", "irHash": callee.ir_hash.as_str() },
            "inputs": {}
        })],
        json!({ "callee": { "flowId": "callee", "irHash": callee.ir_hash.as_str() } }),
    );
    let mut registry = BTreeMap::new();
    registry.insert(callee.ir_hash.clone(), callee);
    let mut opts = run_opts("run-gate-out");
    opts.subflows = registry;
    let session = open(&provider).await;
    let outcome = Runner::run(&root, json!({}), session, &mut store, opts)
        .await
        .expect("run");
    let verdict = finished_verdict(outcome);
    assert_eq!(verdict.status, VerdictStatus::Fail);

    // The callee body ran (one dispatch); the frame was pushed and popped
    // without outputs; the call step failed on the outbound gate.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 1);
    let events = store.events("run-gate-out").expect("events");
    let popped = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::CallFramePopped { outputs } => Some(outputs.clone()),
            _ => None,
        })
        .expect("frame popped");
    assert_eq!(popped, None);
    let call_verdict = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::VerdictRecorded { verdict, .. } if step_of(event) == Some("callBad") => {
                Some(verdict.clone())
            }
            _ => None,
        })
        .expect("call step verdict");
    assert!(
        call_verdict.summary.contains("outbound gate"),
        "got {}",
        call_verdict.summary
    );
}

// ─── (b) foreach ────────────────────────────────────────────────────────────

fn foreach_flow(digest: &Hash, tail_step: bool) -> FlowIR {
    let mut body = vec![json!({
        "kind": "foreach",
        "stepId": "eachItem",
        "effectHash": h64('0'),
        "judgeHash": h64('0'),
        "checkpoint": true,
        "items": { "lit": ["a", "b", "c"] },
        "as": "item",
        "body": [ {
            "kind": "action",
            "stepId": "perItem",
            "effectHash": h64('0'),
            "judgeHash": h64('0'),
            "checkpoint": true,
            "effect": "mutating",
            "idempotent": true,
            "binding": { "attempts": [ {
                "channel": "uiTree",
                "actionName": "tapElement",
                "args": { "value": { "ref": "iter.item" } },
                "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
                "protection": "standard"
            } ] },
            "assertions": [ expect_ok("pa", "perItem") ]
        } ]
    })];
    if tail_step {
        body.push(action_step("tail", vec![]));
    }
    build_flow("fe_flow", digest, json!([]), json!([]), body, json!({}))
}

#[tokio::test]
async fn foreach_runs_three_iterations_with_positional_frames() {
    let dir = TempStoreDir::new("foreach");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let flow = foreach_flow(&provider.lockfile().digest, false);
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-fe"))
        .await
        .expect("run");
    let verdict = finished_verdict(outcome);
    assert_eq!(verdict.status, VerdictStatus::Pass);
    // Three iteration instances fold into the flow verdict.
    assert!(
        verdict.summary.contains("3 judged step(s)"),
        "got {}",
        verdict.summary
    );

    let events = store.events("run-fe").expect("events");
    // The container snapshot carries { items, as } — the positional
    // resume authority and the fold's IterState carrier.
    let fe_entered = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::StepEntered {
                step_id,
                resolved_inputs,
                ..
            } if step_id.as_str() == "eachItem" => Some(resolved_inputs.clone()),
            _ => None,
        })
        .expect("foreach entered");
    assert_eq!(
        fe_entered,
        json!({ "items": ["a", "b", "c"], "as": "item" })
    );
    // Each round dispatched the iteration item, under its `[i]` frame.
    let intents: Vec<(u64, Value)> = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::ActionIntent { args_snapshot, .. } => {
                let index = event.run_path.iter().find_map(|frame| match frame {
                    PathFrame::Iteration { index, .. } => Some(*index),
                    _ => None,
                })?;
                Some((index, args_snapshot.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        intents,
        vec![
            (0, json!({ "value": "a" })),
            (1, json!({ "value": "b" })),
            (2, json!({ "value": "c" })),
        ]
    );
    // One record per iteration instance plus the container.
    let view = store.verify_checkpoint("run-fe").expect("verify");
    assert_eq!(view.completed.len(), 4);
    assert_eq!(view.frames[0].next_index, 1);
}

#[tokio::test]
async fn foreach_halts_on_a_failing_iteration_and_folds_fail() {
    let dir = TempStoreDir::new("foreach-fail");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": false })), // iteration [1] fails
    ]));
    let flow = foreach_flow(&provider.lockfile().digest, true);
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-fe-fail"),
    )
    .await
    .expect("run");
    let verdict = finished_verdict(outcome);
    assert_eq!(verdict.status, VerdictStatus::Fail);

    // Iteration [2] never materialized; the trailing top-level step is
    // explicitly blocked; the container closed its span.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);
    let action_block = [
        "stepEntered",
        "actionIntent",
        "actionSettled",
        "assertionEvaluated",
        "verdictRecorded",
        "stepExited",
    ];
    let mut expected = vec!["runStarted", "stepEntered"]; // run + foreach
    expected.extend(action_block); // [0] perItem
    expected.extend(action_block); // [1] perItem (fail)
    expected.push("stepExited"); // foreach container
    expected.extend(["stepEntered", "stepExited"]); // tail blocked
    expected.push("runFinished");
    assert_eq!(event_types(&store, "run-fe-fail"), expected);
    store.verify_checkpoint("run-fe-fail").expect("verify");
}

// ─── (c) if branches ────────────────────────────────────────────────────────

fn if_flow(digest: &Hash) -> FlowIR {
    build_flow(
        "if_flow",
        digest,
        json!([ { "name": "mode", "schema": { "type": "string" }, "required": true } ]),
        json!([]),
        vec![json!({
            "kind": "if",
            "stepId": "branch",
            "effectHash": h64('0'),
            "judgeHash": h64('0'),
            "checkpoint": true,
            "cond": { "fn": "eq", "args": [ { "ref": "params.mode" }, { "lit": "yes" } ] },
            "then": [ action_step("t1", vec![expect_ok("ta", "t1")]) ],
            "else": [ action_step("e1", vec![expect_ok("ea", "e1")]) ]
        })],
        json!({}),
    )
}

#[tokio::test]
async fn if_selects_then_and_accounts_the_else_branch_as_skipped() {
    let dir = TempStoreDir::new("if-then");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": true }))]));
    let flow = if_flow(&provider.lockfile().digest);
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({ "mode": "yes" }),
        session,
        &mut store,
        run_opts("run-if-then"),
    )
    .await
    .expect("run");
    assert_eq!(finished_verdict(outcome).status, VerdictStatus::Pass);

    // Ledger: the unselected branch's pair lands right after the
    // container enters, before the selected branch runs.
    assert_eq!(
        event_types(&store, "run-if-then"),
        vec![
            "runStarted",
            "stepEntered", // branch (cond snapshot)
            "stepEntered", // e1 skipped pair
            "stepExited",
            "stepEntered", // t1
            "actionIntent",
            "actionSettled",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            "stepExited", // branch container
            "runFinished",
        ]
    );
    let events = store.events("run-if-then").expect("events");
    // The container snapshot carries the strict-boolean decision.
    let cond = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::StepEntered {
                step_id,
                resolved_inputs,
                ..
            } if step_id.as_str() == "branch" => Some(resolved_inputs.clone()),
            _ => None,
        })
        .expect("branch entered");
    assert_eq!(cond, json!({ "cond": true }));
    // The skipped pair: resolvedInputs null, exit state skipped.
    let skipped_exit = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::StepExited { state, .. } if step_of(event) == Some("e1") => Some(*state),
            _ => None,
        })
        .expect("e1 exit");
    assert_eq!(skipped_exit, StepState::Skipped);
    let view = store.verify_checkpoint("run-if-then").expect("verify");
    let e1 = view
        .completed
        .iter()
        .find(|record| record.step_id.as_str() == "e1")
        .expect("e1 record");
    assert_eq!(e1.resolved_inputs, Value::Null);
    assert!(e1.attempts.is_empty() && e1.verdict.is_none() && e1.output.is_none());
    assert_eq!(provider.handle().dispatched_call_ids().len(), 1);
}

#[tokio::test]
async fn if_selects_else_and_accounts_the_then_branch_as_skipped() {
    let dir = TempStoreDir::new("if-else");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": true }))]));
    let flow = if_flow(&provider.lockfile().digest);
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({ "mode": "no" }),
        session,
        &mut store,
        run_opts("run-if-else"),
    )
    .await
    .expect("run");
    assert_eq!(finished_verdict(outcome).status, VerdictStatus::Pass);
    let events = store.events("run-if-else").expect("events");
    let skipped: Vec<&str> = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::StepExited { state, .. } if *state == StepState::Skipped => {
                step_of(event)
            }
            _ => None,
        })
        .collect();
    assert_eq!(skipped, vec!["t1"]);
    let executed: Vec<&str> = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::ActionIntent { .. } => step_of(event),
            _ => None,
        })
        .collect();
    assert_eq!(executed, vec!["e1"]);
}

// ─── (d) let steps feed vars.* ──────────────────────────────────────────────

#[tokio::test]
async fn let_bindings_enter_vars_and_downstream_scope() {
    let dir = TempStoreDir::new("let");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": true }))]));
    let flow = build_flow(
        "let_flow",
        &provider.lockfile().digest,
        json!([]),
        json!([]),
        vec![
            json!({
                "kind": "let",
                "stepId": "bind",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "bindings": { "label": { "fn": "concat", "args": [
                    { "lit": "run-" }, { "lit": "42" }
                ] } }
            }),
            json!({
                "kind": "action",
                "stepId": "uses",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "effect": "mutating",
                "idempotent": false,
                "binding": { "attempts": [ {
                    "channel": "uiTree",
                    "actionName": "tapElement",
                    "args": { "value": { "ref": "vars.label" } },
                    "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
                    "protection": "standard"
                } ] },
                "assertions": []
            }),
        ],
        json!({}),
    );
    let session = open(&provider).await;
    let outcome = Runner::run(&flow, json!({}), session, &mut store, run_opts("run-let"))
        .await
        .expect("run");
    assert!(matches!(outcome, RunOutcome::Finished { verdict: None }));
    let events = store.events("run-let").expect("events");
    // The let snapshot carries the evaluated bindings (the resume-time
    // vars carrier); the downstream step consumed vars.label.
    let bind_entered = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::StepEntered {
                step_id,
                resolved_inputs,
                ..
            } if step_id.as_str() == "bind" => Some(resolved_inputs.clone()),
            _ => None,
        })
        .expect("bind entered");
    assert_eq!(bind_entered, json!({ "label": "run-42" }));
    let args = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::ActionIntent { args_snapshot, .. } => Some(args_snapshot.clone()),
            _ => None,
        })
        .expect("intent");
    assert_eq!(args, json!({ "value": "run-42" }));
}

// ─── (e) assert steps: fresh and fromStep ───────────────────────────────────

fn element_present(assert_id: &str, identifier: &str) -> Value {
    json!({
        "assertId": assert_id,
        "predicate": { "type": "elementState",
                       "selector": { "identifier": identifier },
                       "state": "present" },
        "verifyVia": ["uiTree"],
        "onMissingInput": "unknown"
    })
}

#[tokio::test]
async fn assert_step_fresh_observes_and_judges() {
    let dir = TempStoreDir::new("assert-fresh");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": true }))]));
    provider.handle().inject_ui_snapshot(Some(tree(json!([
        { "stableNodeId": "n1", "role": "banner", "identifier": "welcome" }
    ]))));
    let flow = build_flow(
        "assert_fresh",
        &provider.lockfile().digest,
        json!([]),
        json!([]),
        vec![
            action_step("a1", vec![]),
            json!({
                "kind": "assert",
                "stepId": "checkFresh",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "observe": "fresh",
                "assertions": [ element_present("fa", "welcome") ]
            }),
        ],
        json!({}),
    );
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-assert-fresh"),
    )
    .await
    .expect("run");
    assert_eq!(finished_verdict(outcome).status, VerdictStatus::Pass);
    assert_eq!(
        event_types(&store, "run-assert-fresh"),
        vec![
            "runStarted",
            "stepEntered", // a1 (unasserted)
            "actionIntent",
            "actionSettled",
            "stepExited",
            "stepEntered", // checkFresh
            "observationRecorded",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            "runFinished",
        ]
    );
    // The fresh observation localized the tree for the uiTree channel.
    let events = store.events("run-assert-fresh").expect("events");
    let outcome_record = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::AssertionEvaluated { outcome } => Some(outcome.clone()),
            _ => None,
        })
        .expect("assertion outcome");
    assert_eq!(outcome_record.result, VerdictStatus::Pass);
    assert_eq!(outcome_record.channel, Some(pointlock_ir::Channel::UiTree));
}

#[tokio::test]
async fn assert_step_from_step_reuses_archived_material_without_observing() {
    let dir = TempStoreDir::new("assert-from");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(Some(tree(json!([
        { "stableNodeId": "n1", "role": "label", "identifier": "status",
          "text": "Connected" }
    ]))));
    provider
        .handle()
        .push_script(succeeded_with_after(json!({ "ok": true }), after));
    let flow = build_flow(
        "assert_from",
        &provider.lockfile().digest,
        json!([]),
        json!([]),
        vec![
            action_step("a1", vec![element_present("aa", "status")]),
            json!({
                "kind": "assert",
                "stepId": "checkAgain",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "observe": { "fromStep": "a1", "which": "after" },
                "assertions": [ {
                    "assertId": "fb",
                    "predicate": { "type": "elementText",
                                   "selector": { "identifier": "status" },
                                   "match": { "value": "connected", "mode": "contains",
                                              "caseSensitive": false } },
                    "verifyVia": ["uiTree"],
                    "onMissingInput": "unknown"
                } ]
            }),
        ],
        json!({}),
    );
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-assert-from"),
    )
    .await
    .expect("run");
    assert_eq!(finished_verdict(outcome).status, VerdictStatus::Pass);

    // No second observation was captured: the assert step replays a1's
    // archived material (zero device I/O for the observe source).
    let events = store.events("run-assert-from").expect("events");
    let observation_events: Vec<&str> = events
        .iter()
        .filter(|event| matches!(event.payload, RunLogPayload::ObservationRecorded { .. }))
        .map(|event| step_of(event).unwrap_or("?"))
        .collect();
    assert_eq!(observation_events, vec!["a1"]);
    let from_step_outcome = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::AssertionEvaluated { outcome } if outcome.assert_id.as_str() == "fb" => {
                Some(outcome.clone())
            }
            _ => None,
        })
        .next()
        .expect("fromStep assertion");
    assert_eq!(from_step_outcome.result, VerdictStatus::Pass);
}

// ─── (f) preflight: pass and drifted (+ forced re-probe on resume) ──────────

fn preflight_flow(digest: &Hash, probe_identifier: &str) -> FlowIR {
    let mut step = action_step("probed", vec![expect_ok("pa", "probed")]);
    step["preflight"] = json!([element_present("pf", probe_identifier)]);
    build_flow(
        "pf_flow",
        digest,
        json!([]),
        json!([]),
        vec![step],
        json!({}),
    )
}

#[tokio::test]
async fn preflight_pass_probes_then_acts() {
    let dir = TempStoreDir::new("pf-pass");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": true }))]));
    provider.handle().inject_ui_snapshot(Some(tree(json!([
        { "stableNodeId": "n1", "role": "page", "identifier": "loginPage" }
    ]))));
    let flow = preflight_flow(&provider.lockfile().digest, "loginPage");
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-pf-pass"),
    )
    .await
    .expect("run");
    assert_eq!(finished_verdict(outcome).status, VerdictStatus::Pass);
    assert_eq!(
        event_types(&store, "run-pf-pass"),
        vec![
            "runStarted",
            "stepEntered",
            "observationRecorded", // probe material
            "preflightProbed",
            "actionIntent",
            "actionSettled",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            "runFinished",
        ]
    );
}

#[tokio::test]
async fn preflight_drift_blocks_and_resume_reprobes() {
    let dir = TempStoreDir::new("pf-drift");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([succeeded_with(json!({ "ok": true }))]));
    // The world does not satisfy the probe: a different page is up.
    provider.handle().inject_ui_snapshot(Some(tree(json!([
        { "stableNodeId": "n1", "role": "page", "identifier": "homePage" }
    ]))));
    let flow = preflight_flow(&provider.lockfile().digest, "loginPage");
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-pf-drift"),
    )
    .await
    .expect("run");
    let RunOutcome::Blocked { reason } = outcome else {
        panic!("expected Blocked, got {outcome:?}");
    };
    assert!(reason.to_string().contains("drifted"), "got {reason}");
    // drifted → runSuspended; nothing was dispatched; the span stays open
    // for the resume to land on.
    assert_eq!(
        event_types(&store, "run-pf-drift"),
        vec![
            "runStarted",
            "stepEntered",
            "observationRecorded",
            "preflightProbed",
            "runSuspended",
        ]
    );
    assert!(provider.handle().dispatched_call_ids().is_empty());

    // Repair the world, resume: the first to-execute step re-probes
    // (condition C, forced), passes, and the act finally leaves.
    provider.handle().inject_ui_snapshot(Some(tree(json!([
        { "stableNodeId": "n1", "role": "page", "identifier": "loginPage" }
    ]))));
    let session = open(&provider).await;
    let events_before = store.events("run-pf-drift").expect("events").len();
    let outcome = Runner::resume(
        &flow,
        "run-pf-drift",
        session,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("resume");
    assert_eq!(finished_verdict(outcome).status, VerdictStatus::Pass);
    let events = store.events("run-pf-drift").expect("events");
    let segment: Vec<&'static str> = events[events_before..]
        .iter()
        .map(|event| event.payload.event_type())
        .collect();
    assert_eq!(
        segment,
        vec![
            "runResumed",
            "observationRecorded", // forced re-probe (the span is open — no re-enter)
            "preflightProbed",
            "actionIntent",
            "actionSettled",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            "runFinished",
        ]
    );
    // The span was continued, not re-entered.
    let entered_count = events
        .iter()
        .filter(|event| matches!(event.payload, RunLogPayload::StepEntered { .. }))
        .count();
    assert_eq!(entered_count, 1);
}

// ─── (g) frame-precise resume into a callee (07 §4.6) ───────────────────────

fn two_step_callee_fixture(digest: &Hash) -> (FlowIR, BTreeMap<Hash, FlowIR>) {
    let callee = build_flow(
        "twostep",
        digest,
        json!([]),
        json!([]),
        vec![
            action_step("c1", vec![expect_ok("c1a", "c1")]),
            action_step("c2", vec![expect_ok("c2a", "c2")]),
        ],
        json!({}),
    );
    let root = build_flow(
        "outer",
        digest,
        json!([]),
        json!([]),
        vec![
            action_step("r1", vec![expect_ok("r1a", "r1")]),
            json!({
                "kind": "call",
                "stepId": "callOnce",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "flowRef": { "flowId": "twostep", "irHash": callee.ir_hash.as_str() },
                "inputs": {}
            }),
            action_step("r2", vec![expect_ok("r2a", "r2")]),
        ],
        json!({ "twostep": { "flowId": "twostep", "irHash": callee.ir_hash.as_str() } }),
    );
    let mut registry = BTreeMap::new();
    registry.insert(callee.ir_hash.clone(), callee);
    (root, registry)
}

#[tokio::test]
async fn suspend_inside_callee_resumes_at_the_exact_frame_position() {
    let dir = TempStoreDir::new("frame-resume");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // r1
        succeeded_with(json!({ "ok": true })), // c1
        succeeded_with(json!({ "ok": true })), // c2 (after resume)
        succeeded_with(json!({ "ok": true })), // r2 (after resume)
    ]));
    let (root, registry) = two_step_callee_fixture(&provider.lockfile().digest);

    // Stop lands at the step boundary before c2: the callee frame stays
    // live, the call span stays open.
    let stop = CancellationToken::new();
    let session = Box::new(StopAfter {
        inner: open(&provider).await,
        remaining: AtomicUsize::new(2),
        stop: stop.clone(),
    });
    let mut opts = run_opts("run-frame");
    opts.stop = stop;
    opts.subflows = registry.clone();
    let outcome = Runner::run(&root, json!({}), session, &mut store, opts)
        .await
        .expect("run");
    assert_eq!(outcome, RunOutcome::Suspended);
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);
    let events_before = store.events("run-frame").expect("events").len();

    // The suspended checkpoint keeps the live callee frame.
    let view = store.verify_checkpoint("run-frame").expect("verify");
    assert_eq!(view.frames.len(), 2);
    assert_eq!(view.frames[1].flow_id.as_str(), "twostep");
    assert_eq!(view.frames[1].next_index, 1);

    // Resume: r1 and c1 adopt by instance path; the walk falls back into
    // the live frame and continues at c2 — the caller never re-runs and
    // the frame is never re-pushed (07 §4.6).
    let session = open(&provider).await;
    let outcome = Runner::resume_with_subflows(
        &root,
        &registry,
        "run-frame",
        session,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("resume");
    assert_eq!(finished_verdict(outcome).status, VerdictStatus::Pass);
    assert_eq!(provider.handle().dispatched_call_ids().len(), 4);

    let events = store.events("run-frame").expect("events");
    let segment: Vec<&'static str> = events[events_before..]
        .iter()
        .map(|event| event.payload.event_type())
        .collect();
    assert_eq!(
        segment,
        vec![
            "runResumed",
            // The live call frame is re-entered first, and `callOnce`
            // declares no `preflight`: the resume says so (07 §4.2 rule 1).
            "preflightProbed",
            "stepEntered", // c2 — the exact frame position
            "actionIntent",
            "actionSettled",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            "callFramePopped",
            "verdictRecorded", // callOnce = callee flow verdict
            "stepExited",
            "stepEntered", // r2
            "actionIntent",
            "actionSettled",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            "runFinished",
        ]
    );
    // One frame push in the whole ledger; c1 entered exactly once.
    let pushes = events
        .iter()
        .filter(|event| matches!(event.payload, RunLogPayload::CallFramePushed { .. }))
        .count();
    assert_eq!(pushes, 1);
    let c1_entries = events
        .iter()
        .filter(|event| {
            matches!(event.payload, RunLogPayload::StepEntered { .. })
                && step_of(event) == Some("c1")
        })
        .count();
    assert_eq!(c1_entries, 1);
    let view = store.verify_checkpoint("run-frame").expect("verify");
    assert_eq!(view.frames.len(), 1);
    assert_eq!(view.frames[0].next_index, 3);
}

// ─── (h) foreach mid-iteration crash resume (positional regime) ─────────────

#[tokio::test]
async fn foreach_crash_mid_iteration_resumes_at_the_same_index() {
    let dir = TempStoreDir::new("fe-resume");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),       // [0]
        ScriptedOutcome::TransportLostAfterDispatch, // [1] — no terminal
    ]));
    let flow = foreach_flow(&provider.lockfile().digest, false);
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-fe-crash"),
    )
    .await
    .expect("run");
    assert_eq!(outcome, RunOutcome::Suspended);

    // The suspended checkpoint: the pending intent hangs at iteration [1]
    // and the fold reconstructed the live IterState from the carriers.
    let view = store.verify_checkpoint("run-fe-crash").expect("verify");
    assert!(view.frontier.pending_intent.is_some());
    assert_eq!(
        view.frames[0].iter_stack,
        vec![IterState {
            var: "item".to_owned(),
            index: 1,
            key: None,
        }]
    );
    let events_before = store.events("run-fe-crash").expect("events").len();

    // Resume: [0] adopts; reconcile finds startedNoTerminal — the step is
    // declared idempotent, so the replay is authorized (07 §4.4) with the
    // archived args; [1] replays under the same positional index, then
    // [2] runs fresh.
    provider
        .handle()
        .push_script(succeeded_with(json!({ "ok": true }))); // [1] replay
    provider
        .handle()
        .push_script(succeeded_with(json!({ "ok": true }))); // [2]
    let session = open(&provider).await;
    let outcome = Runner::resume(
        &flow,
        "run-fe-crash",
        session,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("resume");
    let verdict = finished_verdict(outcome);
    assert_eq!(verdict.status, VerdictStatus::Pass);
    assert!(
        verdict.summary.contains("3 judged step(s)"),
        "got {}",
        verdict.summary
    );

    let events = store.events("run-fe-crash").expect("events");
    let segment: Vec<&'static str> = events[events_before..]
        .iter()
        .map(|event| event.payload.event_type())
        .collect();
    assert_eq!(
        segment,
        vec![
            "runResumed",
            // The segment's re-entry step declares no `preflight`, so the
            // resume records that it checked nothing (07 §4.2 rule 1 /
            // I3) instead of staying silent about it.
            "preflightProbed",
            "actionIntent", // [1] replay — the span is open, no re-enter
            "actionSettled",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            "stepEntered", // [2]
            "actionIntent",
            "actionSettled",
            "assertionEvaluated",
            "verdictRecorded",
            "stepExited",
            "stepExited", // foreach container
            "runFinished",
        ]
    );
    // Positional regime: the replayed intent still anchors at [1], as
    // attempt #2 of the same instance; iteration [0] never re-dispatched.
    let intents: Vec<(u64, u64)> = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::ActionIntent { .. } => {
                let iteration = event.run_path.iter().find_map(|frame| match frame {
                    PathFrame::Iteration { index, .. } => Some(*index),
                    _ => None,
                })?;
                let attempt = event.run_path.iter().find_map(|frame| match frame {
                    PathFrame::Attempt { n } => Some(*n),
                    _ => None,
                })?;
                Some((iteration, attempt))
            }
            _ => None,
        })
        .collect();
    assert_eq!(intents, vec![(0, 1), (1, 1), (1, 2), (2, 1)]);
    store.verify_checkpoint("run-fe-crash").expect("verify");
}

// ─── (i) evidence-localization degradation (M2 rule) ────────────────────────

fn visual_assert_flow(digest: &Hash) -> FlowIR {
    flow_fixture(
        digest,
        vec![{
            let mut step = action_step("shot", vec![]);
            step["assertions"] = json!([ {
                "assertId": "va",
                "predicate": { "type": "visual", "prompt": "the page looks right" },
                "verifyVia": ["vision"],
                "onMissingInput": "unknown"
            } ]);
            step
        }],
    )
}

#[tokio::test]
async fn fetch_unsupported_degrades_to_unknown_instead_of_aborting() {
    let dir = TempStoreDir::new("degrade-fetch");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(None); // screenshot only
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    provider
        .handle()
        .set_fetch_evidence_unsupported(Some("control plane has no asset byte channel".into()));
    let flow = visual_assert_flow(&provider.lockfile().digest);
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-degrade-fetch"),
    )
    .await
    .expect("the run must not abort on a localization failure");
    let verdict = finished_verdict(outcome);
    assert_eq!(verdict.status, VerdictStatus::Unknown);

    let events = store.events("run-degrade-fetch").expect("events");
    let outcome_record = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::AssertionEvaluated { outcome } => Some(outcome.clone()),
            _ => None,
        })
        .expect("assertion outcome");
    assert_eq!(outcome_record.result, VerdictStatus::Unknown);
    assert!(
        outcome_record.reason.contains("evidence fetch failed"),
        "got {}",
        outcome_record.reason
    );
    // The observation record keeps the field absent — a typed gap, not a
    // fabricated citation.
    let view = store
        .verify_checkpoint("run-degrade-fetch")
        .expect("verify");
    let shot = &view.completed[0];
    assert_eq!(shot.observations.len(), 1);
    assert!(shot.observations[0].screenshot.is_none());
}

#[tokio::test]
async fn evidence_integrity_mismatch_degrades_to_unknown() {
    let dir = TempStoreDir::new("degrade-integrity");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(None);
    let asset_id = after
        .screenshot
        .as_ref()
        .expect("screenshot asset")
        .id
        .clone();
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    // Corrupt the provider-side bytes after the digest was declared: the
    // fetch fails integrity — a typed gap, never a run abort.
    provider
        .handle()
        .insert_evidence(asset_id, b"tampered bytes".to_vec());
    let flow = visual_assert_flow(&provider.lockfile().digest);
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-degrade-integrity"),
    )
    .await
    .expect("the run must not abort on an integrity failure");
    let verdict = finished_verdict(outcome);
    assert_eq!(verdict.status, VerdictStatus::Unknown);
    let events = store.events("run-degrade-integrity").expect("events");
    let outcome_record = events
        .iter()
        .find_map(|event| match &event.payload {
            RunLogPayload::AssertionEvaluated { outcome } => Some(outcome.clone()),
            _ => None,
        })
        .expect("assertion outcome");
    assert!(
        outcome_record.reason.contains("integrity"),
        "got {}",
        outcome_record.reason
    );
}

#[tokio::test]
async fn ui_tree_channel_still_works_when_the_screenshot_cannot_localize() {
    let dir = TempStoreDir::new("degrade-tree");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::new());
    let after = provider.handle().make_observation(Some(tree(json!([
        { "stableNodeId": "n1", "role": "switch", "identifier": "wifi_toggle" }
    ]))));
    provider
        .handle()
        .push_script(succeeded_with_after(json!({}), after));
    // The screenshot byte channel is gone; the tree travels through
    // `ui.snapshot.get` and must keep the uiTree channel evaluable.
    provider
        .handle()
        .set_fetch_evidence_unsupported(Some("no byte channel".into()));
    let flow = flow_fixture(
        &provider.lockfile().digest,
        vec![action_step(
            "s1",
            vec![element_present("ea", "wifi_toggle")],
        )],
    );
    let session = open(&provider).await;
    let outcome = Runner::run(
        &flow,
        json!({}),
        session,
        &mut store,
        run_opts("run-degrade-tree"),
    )
    .await
    .expect("run");
    let verdict = finished_verdict(outcome);
    assert_eq!(verdict.status, VerdictStatus::Pass);
    let view = store.verify_checkpoint("run-degrade-tree").expect("verify");
    let record = &view.completed[0];
    assert_eq!(record.observations.len(), 1);
    // The screenshot is honestly absent; the tree localized locally.
    assert!(record.observations[0].screenshot.is_none());
    assert!(record.observations[0].ui_snapshot.is_some());
}

// ─── (c2) cross-IR alignment inside an if branch ────────────────────────────

/// Builds an `if` flow whose taken branch has two steps, so a repair can
/// touch the second one alone.
fn if_flow_two_step(digest: &Hash, second_target: &str) -> FlowIR {
    let mut second = action_step("t2", vec![expect_ok("tb", "t2")]);
    second["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] = json!(second_target);
    build_flow(
        "if_flow",
        digest,
        json!([ { "name": "mode", "schema": { "type": "string" }, "required": true } ]),
        json!([]),
        vec![json!({
            "kind": "if",
            "stepId": "branch",
            "effectHash": h64('0'),
            "judgeHash": h64('0'),
            "checkpoint": true,
            "cond": { "fn": "eq", "args": [ { "ref": "params.mode" }, { "lit": "yes" } ] },
            "then": [ action_step("t1", vec![expect_ok("ta", "t1")]), second ],
            "else": [ action_step("e1", vec![expect_ok("ea", "e1")]) ]
        })],
        json!({}),
    )
}

/// 07 §5.2: an `if`'s effectHash covers its `cond`, never its body. So an
/// unchanged cond means the same branch runs and the branch must be walked
/// step by step — a flat classifier would call the whole `if` reusable and
/// adopt a body it never looked at.
#[tokio::test]
async fn cross_ir_resume_classifies_inside_the_taken_branch() {
    let dir = TempStoreDir::new("if-cross-ir");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let flow = if_flow_two_step(&provider.lockfile().digest, "t2_original");
    let outcome = Runner::run(
        &flow,
        json!({ "mode": "yes" }),
        open(&provider).await,
        &mut store,
        run_opts("run-if-x"),
    )
    .await
    .expect("run");
    assert!(matches!(outcome, RunOutcome::Finished { .. }));
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);

    // Retarget the SECOND branch step. The `if` and `t1` are untouched, so
    // only `t2` may re-execute.
    let repaired = if_flow_two_step(&provider.lockfile().digest, "t2_repaired");
    let outcome = Runner::resume(
        &repaired,
        "run-if-x",
        open(&provider).await,
        &mut store,
        ResumeOptions {
            allow_mutating_reexec: vec!["t2".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("the branch-internal repair resumes");
    assert!(matches!(outcome, RunOutcome::Finished { .. }));
    // Exactly one more dispatch: t1 was adopted from inside the branch.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 3);
}

/// A change confined to the branch that was NOT taken leaves the taken
/// branch's records untouched — nothing re-executes.
#[tokio::test]
async fn cross_ir_resume_ignores_a_change_in_the_untaken_branch() {
    let dir = TempStoreDir::new("if-untaken");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let flow = if_flow(&provider.lockfile().digest);
    Runner::run(
        &flow,
        json!({ "mode": "yes" }),
        open(&provider).await,
        &mut store,
        run_opts("run-if-u"),
    )
    .await
    .expect("run");
    assert_eq!(provider.handle().dispatched_call_ids().len(), 1);

    // Edit the `else` branch, which this run never entered.
    let mut edited = if_flow(&provider.lockfile().digest);
    let pointlock_ir::StepIR::If(branch) = &mut edited.body[0] else {
        panic!("if step");
    };
    let pointlock_ir::StepIR::Action(otherwise) =
        &mut branch.r#else.as_mut().expect("else branch")[0]
    else {
        panic!("action step");
    };
    otherwise.base.effect_hash = Hash::new(h64('9')).expect("hash");
    seal(&mut edited);

    Runner::resume(
        &edited,
        "run-if-u",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("an untaken-branch edit does not invalidate the taken branch");
    // Nothing re-executed.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 1);
}

// ─── (b2) cross-IR alignment inside foreach rounds ──────────────────────────

/// The three-round foreach plus a tail action whose target is given, so a
/// repair can touch the tail alone.
fn foreach_flow_with_tail(digest: &Hash, tail_target: &str) -> FlowIR {
    let mut tail = action_step("tail", vec![]);
    tail["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] = json!(tail_target);
    foreach_flow_parts(digest, json!(["a", "b", "c"]), Some(tail))
}

/// The foreach with a different iteration space and no tail.
fn foreach_flow_items(digest: &Hash, items: Value) -> FlowIR {
    foreach_flow_parts(digest, items, None)
}

fn foreach_flow_parts(digest: &Hash, items: Value, tail: Option<Value>) -> FlowIR {
    let mut body = vec![json!({
        "kind": "foreach",
        "stepId": "eachItem",
        "effectHash": h64('0'),
        "judgeHash": h64('0'),
        "checkpoint": true,
        "items": { "lit": items },
        "as": "item",
        "body": [ {
            "kind": "action",
            "stepId": "perItem",
            "effectHash": h64('0'),
            "judgeHash": h64('0'),
            "checkpoint": true,
            "effect": "mutating",
            "idempotent": true,
            "binding": { "attempts": [ {
                "channel": "uiTree",
                "actionName": "tapElement",
                "args": { "value": { "ref": "iter.item" } },
                "acceptExecutionModes": ["nativeSemantic", "webSemantic"],
                "protection": "standard"
            } ] },
            "assertions": [ expect_ok("pa", "perItem") ]
        } ]
    })];
    if let Some(tail) = tail {
        body.push(tail);
    }
    build_flow("fe_flow", digest, json!([]), json!([]), body, json!({}))
}

/// 07 §5.2: a `foreach`'s effectHash covers `{items, as}` — its head, never
/// its body. An unchanged head means the same rounds ran, so each completed
/// round is classified internally; a changed head invalidates the whole
/// step. Rounds align by index (v0.1 positional).
#[tokio::test]
async fn cross_ir_resume_adopts_completed_foreach_rounds() {
    let dir = TempStoreDir::new("fe-cross-ir");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    // Three rounds plus a tail step; the tail is what the repair touches.
    let flow = foreach_flow(&provider.lockfile().digest, true);
    Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-fe-x"),
    )
    .await
    .expect("run");
    assert_eq!(provider.handle().dispatched_call_ids().len(), 4);

    // Retarget only the tail. Every round is untouched, so all three are
    // adopted from inside the foreach and only the tail re-executes.
    let repaired = foreach_flow_with_tail(&provider.lockfile().digest, "tail_repaired");

    Runner::resume(
        &repaired,
        "run-fe-x",
        open(&provider).await,
        &mut store,
        // The tail is mutating and already passed, so re-running it needs
        // the 07 §5.4 authorization — the gate is doing its job here.
        ResumeOptions {
            allow_mutating_reexec: vec!["tail".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("the rounds are adopted; only the tail re-runs");
    assert_eq!(
        provider.handle().dispatched_call_ids().len(),
        5,
        "exactly one more dispatch: the three rounds were adopted"
    );
}

/// A changed foreach head (`items`) invalidates the step as a whole — the
/// old rounds say nothing about the new ones.
#[tokio::test]
async fn cross_ir_resume_invalidates_a_foreach_whose_head_changed() {
    let dir = TempStoreDir::new("fe-head");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let flow = foreach_flow(&provider.lockfile().digest, false);
    Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-fe-h"),
    )
    .await
    .expect("run");
    assert_eq!(provider.handle().dispatched_call_ids().len(), 3);

    // Two items instead of three: a different iteration space entirely.
    let repaired = foreach_flow_items(&provider.lockfile().digest, json!(["a", "b"]));

    Runner::resume(
        &repaired,
        "run-fe-h",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("a changed head re-runs the whole foreach");
    // Both new rounds ran: nothing from the old iteration space was adopted.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 5);
}

// ─── (d) leaf kinds under cross-IR alignment ────────────────────────────────

fn assert_flow(digest: &Hash, matcher: &str) -> FlowIR {
    build_flow(
        "assert_x",
        digest,
        json!([]),
        json!([]),
        vec![
            action_step("a1", vec![]),
            json!({
                "kind": "assert",
                "stepId": "checkFresh",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "observe": "fresh",
                "assertions": [ element_present("fa", matcher) ]
            }),
        ],
        json!({}),
    )
}

/// 02 §12.3: an `assert` step's judge domain is `preflight` + `observe` +
/// `assertions`, so a judgeDirty there can be a changed ASSERTION — it must
/// not be waved through as a preflight-only change. The step re-observes.
#[tokio::test]
async fn a_changed_assertion_on_an_assert_step_re_observes() {
    let dir = TempStoreDir::new("assert-cross-ir");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let flow = assert_flow(&provider.lockfile().digest, "welcome");
    Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-as-x"),
    )
    .await
    .expect("run");

    // The assertion now looks for a different element: the archived verdict
    // says nothing about it.
    let repaired = assert_flow(&provider.lockfile().digest, "farewell");
    Runner::resume(
        &repaired,
        "run-as-x",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("the assert step re-executes");

    // The report must show the assert step invalidated — never adopted as a
    // preflight-only change, which would keep a verdict that judged a
    // DIFFERENT assertion.
    let report = store
        .events("run-as-x")
        .expect("events")
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            pointlock_ir::RunLogPayload::RunResumed {
                alignment_report, ..
            } => Some(alignment_report.clone()),
            _ => None,
        })
        .expect("the resume recorded its alignment report");
    let entry = report
        .entries
        .iter()
        .find(|entry| entry.step_id.as_str() == "checkFresh")
        .expect("the assert step is classified");
    assert_eq!(entry.class, pointlock_ir::AlignmentClass::EffectDirty);
    assert_ne!(
        entry.reason.as_deref(),
        Some("preflightChanged"),
        "a changed assertion is not a preflight-only change"
    );
}

// ─── (e) call steps under cross-IR alignment ────────────────────────────────

/// A caller with one `call` and one following action, over a callee whose
/// single step targets `callee_target`.
fn call_pair(
    digest: &Hash,
    callee_target: &str,
    tail_target: &str,
) -> (FlowIR, BTreeMap<Hash, FlowIR>) {
    let mut inner_step = action_step("g1", vec![expect_ok("ga", "g1")]);
    inner_step["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] =
        json!(callee_target);
    let inner = build_flow(
        "inner",
        digest,
        json!([]),
        json!([]),
        vec![inner_step],
        json!({}),
    );

    let mut tail = action_step("after", vec![expect_ok("aa", "after")]);
    tail["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] = json!(tail_target);
    let root = build_flow(
        "caller",
        digest,
        json!([]),
        json!([]),
        vec![
            json!({
                "kind": "call",
                "stepId": "callInner",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "flowRef": { "flowId": "inner", "irHash": inner.ir_hash.as_str() },
                "inputs": {}
            }),
            tail,
        ],
        json!({ "inner": { "flowId": "inner", "irHash": inner.ir_hash.as_str() } }),
    );
    let registry = BTreeMap::from([(inner.ir_hash.clone(), inner)]);
    (root, registry)
}

/// 07 §5.2: a `call` is classified as a whole. An untouched call adopts —
/// its callee is not re-entered — and only the changed step after it runs.
#[tokio::test]
async fn cross_ir_resume_adopts_an_untouched_call_without_re_entering_the_callee() {
    let dir = TempStoreDir::new("call-adopt");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let (root, registry) = call_pair(&provider.lockfile().digest, "g1_target", "after_v1");
    let mut opts = run_opts("run-call-a");
    opts.subflows = registry.clone();
    Runner::run(&root, json!({}), open(&provider).await, &mut store, opts)
        .await
        .expect("run");
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);

    // Only the trailing action changes; the call and its callee are byte-identical.
    let (repaired, registry) = call_pair(&provider.lockfile().digest, "g1_target", "after_v2");
    Runner::resume_with_subflows(
        &repaired,
        &registry,
        "run-call-a",
        open(&provider).await,
        &mut store,
        ResumeOptions {
            allow_mutating_reexec: vec!["after".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("the call adopts; only the tail re-runs");
    assert_eq!(
        provider.handle().dispatched_call_ids().len(),
        3,
        "the callee must not be re-entered"
    );
}

/// 07 §5.2 case (c): the call COMPLETED in the old run, so a changed callee
/// invalidates it as a whole whatever the cause — there is no position
/// inside a closed frame to continue from. The callee is re-called rather
/// than descended into (contrast the case (a) down-drill below, which needs
/// the frame still open).
#[tokio::test]
async fn cross_ir_resume_re_calls_a_callee_that_changed() {
    let dir = TempStoreDir::new("call-dirty");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let (root, registry) = call_pair(&provider.lockfile().digest, "g1_v1", "after_v1");
    let mut opts = run_opts("run-call-d");
    opts.subflows = registry.clone();
    Runner::run(&root, json!({}), open(&provider).await, &mut store, opts)
        .await
        .expect("run");
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);

    // Retarget the step INSIDE the callee: the callee's irHash changes, so
    // the call step's effectHash changes with it.
    let (repaired, registry) = call_pair(&provider.lockfile().digest, "g1_v2", "after_v1");
    Runner::resume_with_subflows(
        &repaired,
        &registry,
        "run-call-d",
        open(&provider).await,
        &mut store,
        ResumeOptions {
            // The call gates because its callee's closure contains a
            // mutating step (07 §5.4); `after` follows the resume point.
            allow_mutating_reexec: vec!["callInner".to_owned(), "after".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("the changed call is re-called");
    // Both the callee step and the tail ran again.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 4);
}

// ─── (h) call-frame down-drill (07 §5.2 case (a)) ───────────────────────────

/// `outer(r1, call→twostep(label), r2)` over a two-step callee. `c2_target`
/// moves the CALLEE's content (and so its irHash) while leaving the
/// caller's argument expression alone; `label` moves the caller's argument
/// while leaving the callee byte-identical. The two knobs are exactly the
/// two halves the fused call `effectHash` cannot tell apart on its own.
fn drilldown_pair(digest: &Hash, c2_target: &str, label: &str) -> (FlowIR, BTreeMap<Hash, FlowIR>) {
    let mut c1 = action_step("c1", vec![expect_ok("c1a", "c1")]);
    // The callee reads its parameter, so "the argument changed" is a real
    // semantic change inside the callee — and still moves no hash in here.
    c1["binding"]["attempts"][0]["args"]["element"] = json!({ "ref": "params.label" });
    let mut c2 = action_step("c2", vec![expect_ok("c2a", "c2")]);
    c2["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] = json!(c2_target);
    let callee = build_flow(
        "twostep",
        digest,
        json!([ { "name": "label", "schema": {}, "required": true } ]),
        json!([]),
        vec![c1, c2],
        json!({}),
    );
    let root = build_flow(
        "outer",
        digest,
        json!([]),
        json!([]),
        vec![
            action_step("r1", vec![expect_ok("r1a", "r1")]),
            json!({
                "kind": "call",
                "stepId": "callOnce",
                "effectHash": h64('0'),
                "judgeHash": h64('0'),
                "checkpoint": true,
                "flowRef": { "flowId": "twostep", "irHash": callee.ir_hash.as_str() },
                "inputs": { "label": { "lit": { "identifier": label } } }
            }),
            action_step("r2", vec![expect_ok("r2a", "r2")]),
        ],
        json!({ "twostep": { "flowId": "twostep", "irHash": callee.ir_hash.as_str() } }),
    );
    let registry = BTreeMap::from([(callee.ir_hash.clone(), callee)]);
    (root, registry)
}

fn last_alignment(store: &Store, run_id: &str) -> pointlock_ir::AlignmentReport {
    store
        .events(run_id)
        .expect("events")
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            RunLogPayload::RunResumed {
                alignment_report, ..
            } => Some(alignment_report.clone()),
            _ => None,
        })
        .expect("the resume recorded its alignment report")
}

/// Suspends inside the callee, repairs the CALLEE, resumes: the already
/// concluded callee step is adopted from inside the live frame and only the
/// repaired one runs. This is the whole point of the down-drill — without
/// it the call would classify `new` (an unfinished call has no StepRecord)
/// and the callee would replay from its first step.
#[tokio::test]
async fn down_drill_adopts_callee_steps_before_the_repair() {
    let dir = TempStoreDir::new("drill-ok");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // r1
        succeeded_with(json!({ "ok": true })), // c1
        succeeded_with(json!({ "ok": true })), // c2 (after the repair)
        succeeded_with(json!({ "ok": true })), // r2
    ]));
    let digest = provider.lockfile().digest.clone();
    let (root, registry) = drilldown_pair(&digest, "c2_v1", "same");

    let stop = CancellationToken::new();
    let session = Box::new(StopAfter {
        inner: open(&provider).await,
        remaining: AtomicUsize::new(2), // r1, c1 — the stop lands before c2
        stop: stop.clone(),
    });
    let mut opts = run_opts("run-drill");
    opts.stop = stop;
    opts.subflows = registry.clone();
    assert_eq!(
        Runner::run(&root, json!({}), session, &mut store, opts)
            .await
            .expect("run"),
        RunOutcome::Suspended
    );
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);

    // The suspended checkpoint: the callee frame is live and there is NO
    // record for the call step — the precondition the down-drill reads.
    let view = store.verify_checkpoint("run-drill").expect("verify");
    assert_eq!(view.frames.len(), 2);
    let old_callee_pin = view.frames[1].ir_hash.clone();
    assert!(
        !view
            .completed
            .iter()
            .any(|record| record.step_id.as_str() == "callOnce"),
        "an unfinished call contributes no StepRecord"
    );

    // Repair the callee's second step. The caller's `inputs` are untouched,
    // so only the callee pin moves — 07 §5.2 case (a).
    let (repaired, new_registry) = drilldown_pair(&digest, "c2_v2", "same");
    assert_ne!(repaired.ir_hash, root.ir_hash);

    Runner::resume_with_subflows(
        &repaired,
        &new_registry,
        "run-drill",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("the down-drill resumes inside the callee");

    // c1 was adopted, not re-dispatched: 2 before + c2 + r2 = 4.
    assert_eq!(
        provider.handle().dispatched_call_ids().len(),
        4,
        "the concluded callee step must not run again"
    );

    let report = last_alignment(&store, "run-drill");
    let call_entry = report
        .entries
        .iter()
        .find(|entry| entry.step_id.as_str() == "callOnce")
        .expect("the call step is classified");
    assert_eq!(call_entry.class, pointlock_ir::AlignmentClass::EffectDirty);
    assert!(
        call_entry
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("down-drill")),
        "the report must name the down-drill, got {:?}",
        call_entry.reason
    );
    // The resume point is INSIDE the callee, not at the call step.
    assert_eq!(
        report
            .resume_point
            .as_deref()
            .map(pointlock_ir::render_run_path),
        Some(pointlock_ir::render_run_path(
            &report
                .entries
                .iter()
                .find(|entry| entry.step_id.as_str() == "c2")
                .expect("c2 is classified")
                .run_path
        ))
    );
    let c1_entry = report
        .entries
        .iter()
        .find(|entry| entry.step_id.as_str() == "c1")
        .expect("the callee step is classified");
    assert_eq!(c1_entry.class, pointlock_ir::AlignmentClass::Reusable);

    // The frame was RE-ENTERED, not re-pushed, and its pin was rebased onto
    // the repaired callee (07 §5.2: "frames 中该帧的 irHash 更新为新 callee
    // irHash").
    let events = store.events("run-drill").expect("events");
    let rebases: Vec<bool> = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::CallFramePushed { rebase, .. } => Some(*rebase),
            _ => None,
        })
        .collect();
    assert_eq!(rebases, vec![false, true], "one open, one re-entry");
    let mid = store
        .rebuild_checkpoint("run-drill")
        .expect("rebuild")
        .frames
        .len();
    assert_eq!(mid, 1, "the frame stack is balanced again after the pop");
    let new_pin = new_registry
        .keys()
        .next()
        .expect("one callee in the registry")
        .clone();
    assert_ne!(new_pin, old_callee_pin);
    let pins: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::CallFramePushed {
                frame,
                rebase: true,
            } => Some(frame.ir_hash.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(pins, vec![new_pin]);
}

/// 07 §5.2 case (b): the caller swaps the argument it passes while the
/// callee frame is still open. The callee is byte-identical, so every hash
/// inside it is unchanged and a hash-matching descent would report
/// "nothing changed" about a body that now runs on a different value — the
/// down-drill must refuse. What happens instead is the spec's teardown:
/// the stale frame is closed on the ledger (aborted, adopting nothing) and
/// the callee is RE-CALLED with `inputs` freshly evaluated under the new
/// IR. Because the frame's completed steps include a mutating action that
/// took effect, the re-call must first be authorized by name (§5.4).
#[tokio::test]
async fn changed_arguments_tear_down_the_live_frame_and_re_call() {
    let dir = TempStoreDir::new("drill-args");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // r1
        succeeded_with(json!({ "ok": true })), // c1
        succeeded_with(json!({ "ok": true })), // c1 again, on the re-call
        succeeded_with(json!({ "ok": true })), // c2
        succeeded_with(json!({ "ok": true })), // r2
    ]));
    let digest = provider.lockfile().digest.clone();
    let (root, registry) = drilldown_pair(&digest, "c2_v1", "before");
    let (repaired, new_registry) = drilldown_pair(&digest, "c2_v1", "after");

    // The premise, stated as an assertion: the callee did not move at all,
    // so no hash inside it can betray the swapped argument.
    let old_callee = registry.values().next().expect("callee");
    let new_callee = new_registry.values().next().expect("callee");
    assert_eq!(old_callee.ir_hash, new_callee.ir_hash);
    assert_eq!(
        old_callee.body[0].base().effect_hash,
        new_callee.body[0].base().effect_hash
    );
    assert_ne!(repaired.ir_hash, root.ir_hash);

    let stop = CancellationToken::new();
    let session = Box::new(StopAfter {
        inner: open(&provider).await,
        remaining: AtomicUsize::new(2),
        stop: stop.clone(),
    });
    let mut opts = run_opts("run-args");
    opts.stop = stop;
    opts.subflows = registry.clone();
    assert_eq!(
        Runner::run(&root, json!({}), session, &mut store, opts)
            .await
            .expect("run"),
        RunOutcome::Suspended
    );

    // Unauthorized: the frame holds an effective mutating step (c1), so the
    // teardown re-call gates on the CALL, by name.
    let error = Runner::resume_with_subflows(
        &repaired,
        &new_registry,
        "run-args",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect_err("re-calling over an effective mutating step needs authorization");
    let pointlock_runner::RunnerError::RequiresConfirmation { report } = error else {
        panic!("expected the 07 §5.4 gate, got {error}");
    };
    assert_eq!(report.requires_confirmation.len(), 1, "{report:?}");
    assert_eq!(
        report.requires_confirmation[0]
            .step_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("callOnce")
    );
    assert_eq!(
        provider.handle().dispatched_call_ids().len(),
        2,
        "pre-execution refusal"
    );

    // Authorized: the stale frame is torn down and the callee re-called
    // with the NEW argument.
    let outcome = Runner::resume_with_subflows(
        &repaired,
        &new_registry,
        "run-args",
        open(&provider).await,
        &mut store,
        ResumeOptions {
            allow_mutating_reexec: vec!["callOnce".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("the authorized teardown re-call proceeds");
    assert_eq!(finished_verdict(outcome).status, VerdictStatus::Pass);
    // r1 adopted; c1 re-ran (new argument), c2, r2: 2 + 3 dispatches.
    assert_eq!(provider.handle().dispatched_call_ids().len(), 5);

    // The ledger is balanced — the teardown popped the stale frame before
    // the re-call pushed its own — and the checkpoint stack is closed.
    let view = store.verify_checkpoint("run-args").expect("verify");
    assert_eq!(view.frames.len(), 1, "no stranded frame survives");
    let events = store.events("run-args").expect("events");
    let pushes = events
        .iter()
        .filter(|event| matches!(event.payload, RunLogPayload::CallFramePushed { .. }))
        .count();
    let pops = events
        .iter()
        .filter(|event| matches!(event.payload, RunLogPayload::CallFramePopped { .. }))
        .count();
    assert_eq!(
        (pushes, pops),
        (2, 2),
        "old frame + torn down, new frame + popped"
    );

    // The RE-CALL's frame snapshot carries the freshly evaluated argument —
    // the whole point of the teardown: the old snapshot is never reused.
    let last_push = events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            RunLogPayload::CallFramePushed { frame, rebase } => Some((frame.clone(), *rebase)),
            _ => None,
        })
        .expect("the re-call pushed a frame");
    assert!(
        !last_push.1,
        "a teardown re-call is a fresh push, not a rebase"
    );
    assert_eq!(
        last_push.0.inputs_snapshot["label"]["identifier"],
        json!("after"),
        "inputs were re-evaluated under the new IR: {:?}",
        last_push.0.inputs_snapshot
    );

    // The stale frame's own exit is an honest abort: no verdict, adopting
    // nothing — followed later by the re-call's judged record.
    let call_records: Vec<_> = view
        .completed
        .iter()
        .filter(|record| record.step_id.as_str() == "callOnce")
        .collect();
    assert_eq!(call_records.len(), 2, "aborted teardown + judged re-call");
    assert!(
        call_records[0].verdict.is_none(),
        "the abort claims nothing"
    );
    assert!(call_records[1].verdict.is_some(), "the re-call concluded");
}

/// The no-gate half of case (b): a callee whose completed work is all
/// readonly re-calls WITHOUT authorization — nothing effective is in the
/// world, so re-execution is plain repair.
#[tokio::test]
async fn a_readonly_frame_tears_down_without_authorization() {
    let dir = TempStoreDir::new("drill-args-ro");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // r1
        succeeded_with(json!({ "ok": true })), // c1
        succeeded_with(json!({ "ok": true })), // c1 on the re-call
        succeeded_with(json!({ "ok": true })), // c2
        succeeded_with(json!({ "ok": true })), // r2
    ]));
    let digest = provider.lockfile().digest.clone();
    let readonly_pair = |label: &str| {
        let (mut root, mut registry) = drilldown_pair(&digest, "c2_v1", label);
        let mut callee = registry.values().next().expect("callee").clone();
        for step in &mut callee.body {
            if let pointlock_ir::StepIR::Action(action) = step {
                action.effect = pointlock_ir::vocab::EffectClassAction::Readonly;
                action.idempotent = true;
            }
        }
        seal(&mut callee);
        let pointlock_ir::StepIR::Call(call) = &mut root.body[1] else {
            panic!("body[1] is the call");
        };
        call.flow_ref.ir_hash = callee.ir_hash.clone();
        root.subflows.insert(
            serde_json::from_value(json!("twostep")).expect("flow id"),
            serde_json::from_value(json!({
                "flowId": "twostep", "irHash": callee.ir_hash.as_str()
            }))
            .expect("flow ref"),
        );
        seal(&mut root);
        registry.clear();
        registry.insert(callee.ir_hash.clone(), callee);
        (root, registry)
    };
    let (root, registry) = readonly_pair("before");
    let (repaired, new_registry) = readonly_pair("after");
    assert_ne!(repaired.ir_hash, root.ir_hash);

    let stop = CancellationToken::new();
    let session = Box::new(StopAfter {
        inner: open(&provider).await,
        remaining: AtomicUsize::new(2),
        stop: stop.clone(),
    });
    let mut opts = run_opts("run-args-ro");
    opts.stop = stop;
    opts.subflows = registry.clone();
    Runner::run(&root, json!({}), session, &mut store, opts)
        .await
        .expect("run");

    let outcome = Runner::resume_with_subflows(
        &repaired,
        &new_registry,
        "run-args-ro",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("a readonly frame needs no authorization to re-call");
    assert_eq!(finished_verdict(outcome).status, VerdictStatus::Pass);
    assert_eq!(provider.handle().dispatched_call_ids().len(), 5);
    let view = store.verify_checkpoint("run-args-ro").expect("verify");
    assert_eq!(view.frames.len(), 1);
}

/// 07 §5.2 case (b)/(c) gate: a call answers for its FRAME. The call's own
/// record carries no attempts — those live on the callee's action records —
/// so a frame whose aggregate verdict is `fail` while a mutating step
/// inside it succeeded and passed would otherwise sail through the §5.4
/// gate and silently replay that step on the re-call.
#[tokio::test]
async fn re_calling_a_failed_frame_gates_on_its_effective_callee_step() {
    let dir = TempStoreDir::new("frame-gate");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })),  // r1
        succeeded_with(json!({ "ok": true })),  // c1 — succeeds AND passes
        succeeded_with(json!({ "ok": false })), // c2 — the tap lands, the assertion refuses
    ]));
    let digest = provider.lockfile().digest.clone();
    let (root, registry) = drilldown_pair(&digest, "c2_v1", "same");
    let mut opts = run_opts("run-gate");
    opts.subflows = registry.clone();
    let outcome = Runner::run(&root, json!({}), open(&provider).await, &mut store, opts)
        .await
        .expect("run");
    assert_eq!(finished_verdict(outcome).status, VerdictStatus::Fail);

    // The frame concluded (case (c)): a record with a `fail` verdict and no
    // live frame. c1 nonetheless passed — its effect is in the world.
    let view = store.verify_checkpoint("run-gate").expect("verify");
    assert_eq!(view.frames.len(), 1, "the callee frame was popped");
    let call_record = view
        .completed
        .iter()
        .find(|record| record.step_id.as_str() == "callOnce")
        .expect("the concluded call has a record");
    assert_eq!(
        call_record
            .verdict
            .as_ref()
            .expect("aggregate verdict")
            .status,
        VerdictStatus::Fail
    );

    let (repaired, new_registry) = drilldown_pair(&digest, "c2_v2", "same");
    let error = Runner::resume_with_subflows(
        &repaired,
        &new_registry,
        "run-gate",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect_err("re-calling the frame replays c1 and must be confirmed");
    let pointlock_runner::RunnerError::RequiresConfirmation { report } = error else {
        panic!("expected the 07 §5.4 gate, got {error}");
    };
    let gated: Vec<String> = report
        .requires_confirmation
        .iter()
        .map(|entry| pointlock_ir::render_run_path(&entry.run_path))
        .collect();
    assert!(
        report
            .requires_confirmation
            .iter()
            .any(|entry| pointlock_ir::render_run_path(&entry.run_path).contains("callOnce")),
        "the call step must be gated for its frame's effective step, got {gated:?}"
    );
}

/// 07 §5.4 over a container the walk STOPPED at. A changed `cond` makes the
/// whole branch re-execute, and the steps in there already ran: they are
/// mutating, not idempotent, and passed. The walk does not descend into a
/// container whose head moved — the archive cannot vouch for the old
/// decision — so those steps are not nodes and cannot gate individually.
/// The `if` must therefore gate as a whole; without that the confirmation
/// channel has a hole exactly where the author edited something.
#[tokio::test]
async fn a_changed_cond_gates_the_branch_it_replays() {
    let dir = TempStoreDir::new("cond-gate");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // t1
        succeeded_with(json!({ "ok": true })), // t2
        succeeded_with(json!({ "ok": true })),
        succeeded_with(json!({ "ok": true })),
    ]));
    let flow = if_flow_two_step(&provider.lockfile().digest, "t2_original");
    Runner::run(
        &flow,
        json!({ "mode": "yes" }),
        open(&provider).await,
        &mut store,
        run_opts("run-cond"),
    )
    .await
    .expect("run");
    assert_eq!(provider.handle().dispatched_call_ids().len(), 2);

    // Same branch taken, different `cond` EXPRESSION: `eq(a, b)` becomes
    // `eq(b, a)`. The if's effectHash moves; nothing else does.
    let mut edited = if_flow_two_step(&provider.lockfile().digest, "t2_original");
    let pointlock_ir::StepIR::If(branch) = &mut edited.body[0] else {
        panic!("body[0] is the if");
    };
    branch.cond = serde_json::from_value(json!({
        "fn": "eq", "args": [ { "lit": "yes" }, { "ref": "params.mode" } ]
    }))
    .expect("cond");
    seal(&mut edited);

    let error = Runner::resume(
        &edited,
        "run-cond",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect_err("replaying the branch needs confirmation");
    let pointlock_runner::RunnerError::RequiresConfirmation { report } = error else {
        panic!("expected the 07 §5.4 gate, got {error}");
    };
    let gated: Vec<&str> = report
        .requires_confirmation
        .iter()
        .map(|entry| entry.cause.as_str())
        .collect();
    assert_eq!(gated, vec!["mutatingReexec"]);
    assert_eq!(
        pointlock_ir::render_run_path(&report.requires_confirmation[0].run_path),
        pointlock_ir::render_run_path(
            &report
                .entries
                .iter()
                .find(|entry| entry.step_id.as_str() == "branch")
                .expect("the if is classified")
                .run_path
        ),
        "the gate names the container, which is what --allow-mutating-reexec takes"
    );

    // Naming it releases the gate; the branch then replays.
    Runner::resume(
        &edited,
        "run-cond",
        open(&provider).await,
        &mut store,
        ResumeOptions {
            allow_mutating_reexec: vec!["branch".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("authorized replay");
    assert_eq!(provider.handle().dispatched_call_ids().len(), 4);
}

// ─── unprobed: an honest resume says what it did NOT check (I3) ──────────────

fn probe_count(store: &Store, run_id: &str) -> Vec<usize> {
    store
        .events(run_id)
        .expect("events")
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::PreflightProbed { outcomes } => Some(outcomes.len()),
            _ => None,
        })
        .collect()
}

/// 07 §4.2 rule 1 / I3: 「该步无声明则跳过并在报告标 `unprobed`（诚实优先于
/// 安慰）」. The mark is a `preflightProbed` with an EMPTY outcome list —
/// unambiguous because `preflight` is `minItems: 1` in the schema, so a
/// declared probe list never evaluates to zero outcomes.
///
/// The test pins where it must NOT appear as hard as where it must: a fresh
/// run verifies nothing because it never stopped watching, and a step in the
/// middle of a continuously-executing segment is not re-touching an
/// unwatched world either. Marking those would turn an honest signal into
/// noise.
#[tokio::test]
async fn a_resume_without_probes_records_that_it_checked_nothing() {
    let dir = TempStoreDir::new("unprobed");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // r1
        succeeded_with(json!({ "ok": true })), // c1
        succeeded_with(json!({ "ok": true })), // c2 after the resume
        succeeded_with(json!({ "ok": true })), // r2 after the resume
    ]));
    let digest = provider.lockfile().digest.clone();
    let (root, registry) = drilldown_pair(&digest, "c2_v1", "same");

    let stop = CancellationToken::new();
    let session = Box::new(StopAfter {
        inner: open(&provider).await,
        remaining: AtomicUsize::new(2),
        stop: stop.clone(),
    });
    let mut opts = run_opts("run-unprobed");
    opts.stop = stop;
    opts.subflows = registry.clone();
    Runner::run(&root, json!({}), session, &mut store, opts)
        .await
        .expect("run");

    // A fresh run checks nothing because it never stopped watching: no
    // mark anywhere, not even on the three steps without `preflight`.
    assert_eq!(
        probe_count(&store, "run-unprobed"),
        Vec::<usize>::new(),
        "a fresh run has no unwatched world to re-touch"
    );
    let before = store.events("run-unprobed").expect("events").len();

    Runner::resume_with_subflows(
        &root,
        &registry,
        "run-unprobed",
        open(&provider).await,
        &mut store,
        ResumeOptions::default(),
    )
    .await
    .expect("resume");

    // Exactly ONE mark in the resumed segment — the re-entry step. c2 and
    // r2 execute after it inside one continuous segment and are not marks
    // of their own.
    let segment: Vec<usize> = store.events("run-unprobed").expect("events")[before..]
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::PreflightProbed { outcomes } => Some(outcomes.len()),
            _ => None,
        })
        .collect();
    assert_eq!(
        segment,
        vec![0],
        "one unprobed mark at the re-entry step, and nothing after it"
    );
}

/// An always-true `expr` preflight: it probes for real (a non-empty outcome
/// list) without ever drifting.
fn trivial_preflight() -> Value {
    json!([{
        "assertId": "pf",
        "predicate": { "type": "expr", "expr": { "fn": "eq", "args": [
            { "lit": 1 }, { "lit": 1 }
        ] } },
        "verifyVia": [],
        "onMissingInput": "unknown"
    }])
}

/// 07 §5.4 step 3: the preflight guard covers every step released through
/// the gate, not just the resume's re-entry step — 「本条 preflight 守护对
/// `positionalReplay`/`orderInvalidated`/`frontierUnknown` 的步同样强制适用」.
/// A released step that declares no probes is therefore its own `unprobed`
/// mark: it re-executes onto a world carrying its earlier effect, which is
/// the whole reason it had to be authorized by name.
#[tokio::test]
async fn a_gate_released_step_is_its_own_unprobed_mark() {
    let dir = TempStoreDir::new("unprobed-gate");
    let mut store = Store::open(dir.path()).expect("open store");
    let provider = FakeProvider::new(VecDeque::from([
        succeeded_with(json!({ "ok": true })), // s1
        succeeded_with(json!({ "ok": true })), // s2
        succeeded_with(json!({ "ok": true })), // s3
        succeeded_with(json!({ "ok": true })), // s2 replayed
        succeeded_with(json!({ "ok": true })), // s3 replayed
    ]));
    let digest = provider.lockfile().digest.clone();
    let body = |s2_target: &str| {
        let mut s2 = action_step("s2", vec![expect_ok("a2", "s2")]);
        s2["binding"]["attempts"][0]["args"]["element"]["lit"]["identifier"] = json!(s2_target);
        // s2 carries probes, s3 does not — the difference the test reads.
        s2["preflight"] = trivial_preflight();
        vec![
            action_step("s1", vec![expect_ok("a1", "s1")]),
            s2,
            action_step("s3", vec![expect_ok("a3", "s3")]),
        ]
    };
    let flow = flow_fixture(&digest, body("s2_v1"));
    Runner::run(
        &flow,
        json!({}),
        open(&provider).await,
        &mut store,
        run_opts("run-gate-probe"),
    )
    .await
    .expect("run");
    let before = store.events("run-gate-probe").expect("events").len();

    // Retarget s2: it goes effectDirty (the re-entry step) and s3 follows
    // it as `positionalReplay`. Both are mutating and already effective, so
    // both must be named.
    let repaired = flow_fixture(&digest, body("s2_v2"));
    Runner::resume(
        &repaired,
        "run-gate-probe",
        open(&provider).await,
        &mut store,
        ResumeOptions {
            allow_mutating_reexec: vec!["s2".to_owned(), "s3".to_owned()],
            ..ResumeOptions::default()
        },
    )
    .await
    .expect("authorized replay");

    let segment: Vec<usize> = store.events("run-gate-probe").expect("events")[before..]
        .iter()
        .filter_map(|event| match &event.payload {
            RunLogPayload::PreflightProbed { outcomes } => Some(outcomes.len()),
            _ => None,
        })
        .collect();
    assert_eq!(
        segment,
        vec![1, 0],
        "s2 probed for real (1 outcome); s3 was released by name with no probes \
         declared, so it is an unprobed mark of its own"
    );
}
