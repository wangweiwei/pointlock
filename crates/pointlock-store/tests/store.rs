//! Integration tests: append → rebuild round-trip, crash-window recovery,
//! seq allocation, evidence idempotence, run status transitions, and the
//! verify_checkpoint self-check.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pointlock_ir::{
    ActionExecution, ActionOutcome, ActionResult, AlignmentReport, AssertId,
    AssertionOutcomeRecord, AssetRef, BindingState, CallFrame, Channel, ErrorClass, ErrorInfo,
    EventCursor, EvidenceRef, ExecutionMode, FlowId, Hash, HumanMode, HumanPurpose,
    JsonSchemaDocument, ObservationRecord, PathFrame, RunLogPayload, RunPath, StepId, StepState,
    SupervisePolicy, UiContextKind, UiContextRef, Verdict, VerdictStatus,
};
use pointlock_store::{FoldError, HumanResponseRejection, NewRun, RunStatus, Store, StoreError};
use serde_json::{Value, json};

// ─── Fixture helpers ────────────────────────────────────────────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique per-test store directory under the system temp dir, removed on
/// drop (no tempfile dependency needed).
struct TempStoreDir(PathBuf);

impl TempStoreDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-store-test-{tag}-{}-{}",
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

fn hash(fill: char) -> Hash {
    Hash::new(format!("sha256:{}", fill.to_string().repeat(64))).expect("valid hash")
}

fn flow_id() -> FlowId {
    FlowId::new("checkout").expect("valid flow id")
}

fn step_id(name: &str) -> StepId {
    StepId::new(name).expect("valid step id")
}

fn binding() -> BindingState {
    BindingState {
        device_id: "dev-1".to_owned(),
        session_lineage: vec!["s-1".to_owned()],
        event_cursor: EventCursor {
            session_id: "s-1".to_owned(),
            last_sequence: 0,
        },
    }
}

fn new_run(run_id: &str) -> NewRun {
    NewRun {
        run_id: Some(run_id.to_owned()),
        flow_id: flow_id(),
        ir_hash: hash('a'),
        lockfile_digest: hash('b'),
        params_snapshot: json!({"user": "alice"}),
        binding: binding(),
        created_at_ms: 1_000,
    }
}

fn root_path() -> RunPath {
    vec![PathFrame::Flow {
        flow_id: flow_id(),
        ir_hash: hash('a'),
    }]
}

fn step_path(name: &str) -> RunPath {
    let mut path = root_path();
    path.push(PathFrame::Step {
        step_id: step_id(name),
    });
    path
}

fn run_started() -> RunLogPayload {
    RunLogPayload::RunStarted {
        ir_hash: hash('a'),
        lockfile_digest: hash('b'),
        params_snapshot: json!({"user": "alice"}),
        supervise_policy: Some(SupervisePolicy::Mutating),
    }
}

fn succeeded_outcome(call_id: &str) -> ActionOutcome {
    ActionOutcome::Succeeded {
        result: Box::new(ActionResult {
            call_id: call_id.to_owned(),
            started_at_ms: 1,
            finished_at_ms: 2,
            output: json!({"tapped": true}),
            before: None,
            after: None,
            evidence: vec![],
            execution: Some(ActionExecution::NativeSemantic {
                context: UiContextRef {
                    context_kind: UiContextKind::Native,
                    context_id: "ctx-1".to_owned(),
                    document_epoch: "epoch-1".to_owned(),
                },
            }),
        }),
    }
}

fn failed_outcome() -> ActionOutcome {
    ActionOutcome::Failed {
        error: ErrorInfo {
            code: "action_failed_final".to_owned(),
            message: "element vanished".to_owned(),
            retryable: false,
            details: None,
        },
    }
}

fn observation(evidence_digest: char) -> ObservationRecord {
    ObservationRecord {
        viewport: None,
        observation_id: "obs-1".to_owned(),
        captured_at_ms: 5,
        screenshot: Some(EvidenceRef {
            asset: AssetRef {
                id: "asset-1".to_owned(),
                media_type: "image/png".to_owned(),
                uri: "devicerail://assets/sha256/feed".to_owned(),
                sha256: None,
            },
            sha256: evidence_digest.to_string().repeat(64),
            local_path: "evidence/sha256/cc/cc/cafe".to_owned(),
        }),
        screenshot_omission: None,
        ui_snapshot: None,
        ui_snapshot_omission: None,
    }
}

fn assertion_pass() -> AssertionOutcomeRecord {
    AssertionOutcomeRecord {
        assert_id: AssertId::new("a1").expect("valid assert id"),
        result: VerdictStatus::Pass,
        channel: Some(Channel::UiTree),
        reason: "text matched".to_owned(),
    }
}

fn verdict_pass() -> Verdict {
    Verdict {
        status: VerdictStatus::Pass,
        degraded: false,
        summary: "all assertions passed".to_owned(),
        evidence: vec![],
        supersedes: None,
    }
}

fn callee_frame() -> CallFrame {
    CallFrame {
        flow_id: FlowId::new("pay_flow").expect("valid flow id"),
        ir_hash: hash('c'),
        call_step_id: Some(step_id("do_checkout")),
        inputs_snapshot: json!({"amount": 42}),
        vars: BTreeMap::new(),
        iter_stack: vec![],
        next_index: 0,
    }
}

/// Appends `payload` with an auto-incrementing timestamp and asserts the
/// returned seq.
fn append(store: &mut Store, run_id: &str, path: RunPath, payload: RunLogPayload) -> u64 {
    store
        .append_event(run_id, 1_000, &path, &payload)
        .expect("append event")
}

// ─── Tests ──────────────────────────────────────────────────────────────────

/// Round-trip over a 19-event log spanning a plain action step, a subflow
/// call (nested step, frame push/pop), a human interaction, and the run
/// terminal: the materialized view must equal the full-log refold, and the
/// fold must have assembled the records this task pinned down.
#[test]
fn append_then_rebuild_round_trips() {
    let dir = TempStoreDir::new("roundtrip");
    let mut store = Store::open(dir.path()).expect("open store");
    let run_id = store.begin_run(new_run("run-rt")).expect("begin run");

    append(&mut store, &run_id, root_path(), run_started());
    // Step 1: login — full act/observe/assert/verdict pipeline.
    append(
        &mut store,
        &run_id,
        step_path("login"),
        RunLogPayload::StepEntered {
            step_id: step_id("login"),
            effect_hash: hash('e'),
            judge_hash: hash('f'),
            resolved_inputs: json!({"target": "loginButton"}),
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("login"),
        RunLogPayload::ActionIntent {
            call_id: "c-1".to_owned(),
            args_snapshot: json!({"target": "loginButton"}),
            chain_index: None,
            channel: None,
            action_name: None,
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("login"),
        RunLogPayload::ActionSettled {
            call_id: "c-1".to_owned(),
            outcome: succeeded_outcome("c-1"),
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("login"),
        RunLogPayload::ObservationRecorded {
            observation: observation('d'),
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("login"),
        RunLogPayload::AssertionEvaluated {
            outcome: assertion_pass(),
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("login"),
        RunLogPayload::VerdictRecorded {
            verdict: verdict_pass(),
            localized: Vec::new(),
            localization_gaps: Vec::new(),
            remote_archival_error: None,
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("login"),
        RunLogPayload::StepExited {
            provider_state_summary: None,
            state: StepState::Judged,
            output: Some(json!({"tapped": true})),
            localized: Vec::new(),
            localization_gaps: Vec::new(),
        },
    );
    // Step 2: do_checkout — a call step; its callee runs one failing step.
    append(
        &mut store,
        &run_id,
        step_path("do_checkout"),
        RunLogPayload::StepEntered {
            step_id: step_id("do_checkout"),
            effect_hash: hash('3'),
            judge_hash: hash('4'),
            resolved_inputs: json!({"amount": 42}),
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("do_checkout"),
        RunLogPayload::CallFramePushed {
            frame: callee_frame(),
            rebase: false,
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("pay"),
        RunLogPayload::StepEntered {
            step_id: step_id("pay"),
            effect_hash: hash('1'),
            judge_hash: hash('2'),
            resolved_inputs: json!({"target": "payButton"}),
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("pay"),
        RunLogPayload::ActionIntent {
            call_id: "c-2".to_owned(),
            args_snapshot: json!({"target": "payButton"}),
            chain_index: None,
            channel: None,
            action_name: None,
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("pay"),
        RunLogPayload::ActionSettled {
            call_id: "c-2".to_owned(),
            outcome: failed_outcome(),
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("pay"),
        RunLogPayload::StepExited {
            provider_state_summary: None,
            state: StepState::Judged,
            output: None,
            localized: Vec::new(),
            localization_gaps: Vec::new(),
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("do_checkout"),
        RunLogPayload::CallFramePopped {
            outputs: Some(json!({"receipt": "r-77"})),
        },
    );
    // The call step's exit carries no output of its own: the fold keeps
    // the callee outputs harvested from callFramePopped.
    append(
        &mut store,
        &run_id,
        step_path("do_checkout"),
        RunLogPayload::StepExited {
            provider_state_summary: None,
            state: StepState::Judged,
            output: None,
            localized: Vec::new(),
            localization_gaps: Vec::new(),
        },
    );
    // A human interaction, then the run terminal.
    append(
        &mut store,
        &run_id,
        step_path("confirm"),
        RunLogPayload::HumanRequested {
            request_id: "req-1".to_owned(),
            purpose: HumanPurpose::Step,
            mode: Some(HumanMode::Confirm),
            prompt: "Confirm the receipt".to_owned(),
            presents: json!([]),
            decisions: Some(vec!["confirm".to_owned(), "reject".to_owned()]),
            output_schema: None,
            deadline_at_ms: Some(600_000),
        },
    );
    append(
        &mut store,
        &run_id,
        step_path("confirm"),
        RunLogPayload::HumanResponded {
            request_id: "req-1".to_owned(),
            purpose: HumanPurpose::Step,
            response: json!({"decision": "confirm"}),
            actor: "cli:tester".to_owned(),
        },
    );
    let last_seq = append(
        &mut store,
        &run_id,
        root_path(),
        RunLogPayload::RunFinished {
            verdict: Some(verdict_pass()),
            remote_archival_error: None,
        },
    );
    assert_eq!(last_seq, 19);

    // Materialized == rebuilt (the invariant under test), and the
    // self-check agrees.
    let (log_seq, materialized) = store
        .materialized_checkpoint(&run_id)
        .expect("read checkpoint")
        .expect("checkpoint exists");
    let rebuilt = store.rebuild_checkpoint(&run_id).expect("rebuild");
    assert_eq!(log_seq, 19);
    assert_eq!(materialized, rebuilt);
    let verified = store.verify_checkpoint(&run_id).expect("verify");
    assert_eq!(verified, rebuilt);

    // The fold assembled the full StepRecord from the event carriers.
    assert_eq!(rebuilt.completed.len(), 3);
    let login = &rebuilt.completed[0];
    assert_eq!(login.step_id.as_str(), "login");
    assert_eq!(login.attempts.len(), 1);
    assert_eq!(
        login.attempts[0].execution_mode,
        Some(ExecutionMode::NativeSemantic)
    );
    // Hashes and inputs from stepEntered, output from stepExited
    // (spine §6.1 M1 carriers — no placeholders).
    assert_eq!(login.effect_hash, hash('e'));
    assert_eq!(login.judge_hash, hash('f'));
    assert_eq!(login.resolved_inputs, json!({"target": "loginButton"}));
    assert_eq!(login.output, Some(json!({"tapped": true})));
    assert_eq!(login.observations.len(), 1);
    assert_eq!(login.evidence.len(), 1);
    assert_eq!(login.assertion_outcomes.len(), 1);
    assert_eq!(
        login.verdict.as_ref().expect("login verdict").status,
        VerdictStatus::Pass
    );
    // Callee step completes before its host call step; the error code
    // spelled a closed ErrorClass verbatim and was adopted.
    let pay = &rebuilt.completed[1];
    assert_eq!(pay.step_id.as_str(), "pay");
    assert_eq!(
        pay.attempts[0].error_class,
        Some(ErrorClass::ActionFailedFinal)
    );
    // A failed step exits without an output.
    assert_eq!(pay.output, None);
    let checkout = &rebuilt.completed[2];
    assert_eq!(checkout.step_id.as_str(), "do_checkout");
    assert_eq!(checkout.resolved_inputs, json!({"amount": 42}));
    // The call step's output is the callee outputs from callFramePopped
    // (its own exit carried none).
    assert_eq!(checkout.output, Some(json!({"receipt": "r-77"})));
    // Frame stack folded back to the root, whose cursor advanced by the
    // two root-body steps plus the confirm-step-free human exchange (none).
    assert_eq!(rebuilt.frames.len(), 1);
    assert_eq!(rebuilt.frames[0].next_index, 2);
    assert!(rebuilt.human_pending.is_none());
    assert_eq!(
        store.run_status(&run_id).expect("status"),
        RunStatus::Finished
    );
}

/// Crash window: actionIntent committed, process dies before (or during)
/// dispatch. Reopening the store must surface the hanging intent for
/// reconcile (07 §3.1 — "the key to the crash window").
#[test]
fn crash_after_action_intent_survives_reopen() {
    let dir = TempStoreDir::new("crash");
    let run_id;
    {
        let mut store = Store::open(dir.path()).expect("open store");
        run_id = store.begin_run(new_run("run-crash")).expect("begin run");
        append(&mut store, &run_id, root_path(), run_started());
        append(
            &mut store,
            &run_id,
            step_path("pay"),
            RunLogPayload::StepEntered {
                step_id: step_id("pay"),
                effect_hash: hash('1'),
                judge_hash: hash('2'),
                resolved_inputs: json!({"target": "payButton"}),
            },
        );
        let seq = store
            .write_action_intent(
                &run_id,
                2_000,
                &step_path("pay"),
                "c-crash",
                json!({"target": "payButton"}),
                None,
            )
            .expect("write intent");
        assert_eq!(seq, 3);
        // Simulated crash: the Store (and its connection) drops here,
        // after commit, before any actionSettled.
    }
    let store = Store::open(dir.path()).expect("reopen store");
    let rebuilt = store.rebuild_checkpoint(&run_id).expect("rebuild");
    assert_eq!(rebuilt.frontier.state, StepState::Acting);
    let intent = rebuilt
        .frontier
        .pending_intent
        .expect("hanging intent on record");
    assert_eq!(intent.call_id, "c-crash");
    assert_eq!(intent.args_snapshot, json!({"target": "payButton"}));
    // The materialized row committed in the same transaction survives too.
    store
        .verify_checkpoint(&run_id)
        .expect("verify after crash");
}

/// seq starts at 1 and increases monotonically, independently per run.
#[test]
fn seq_is_monotonic_from_one_and_per_run() {
    let dir = TempStoreDir::new("seq");
    let mut store = Store::open(dir.path()).expect("open store");
    let run_a = store.begin_run(new_run("run-a")).expect("begin run a");
    let run_b = store.begin_run(new_run("run-b")).expect("begin run b");

    assert_eq!(append(&mut store, &run_a, root_path(), run_started()), 1);
    assert_eq!(
        append(
            &mut store,
            &run_a,
            root_path(),
            RunLogPayload::RunSuspended {
                provider_state_summary: None,
                reason: None
            },
        ),
        2
    );
    // A second run allocates its own sequence, also from 1.
    assert_eq!(append(&mut store, &run_b, root_path(), run_started()), 1);

    let events = store.events(&run_a).expect("events");
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);
}

/// An event whose fold is structurally invalid is refused atomically: no
/// run_log row is left behind and the checkpoint stays at the prior seq.
#[test]
fn unfoldable_event_is_rejected_and_rolled_back() {
    let dir = TempStoreDir::new("rollback");
    let mut store = Store::open(dir.path()).expect("open store");
    let run_id = store.begin_run(new_run("run-rb")).expect("begin run");
    append(&mut store, &run_id, root_path(), run_started());

    let err = store
        .append_event(
            &run_id,
            2_000,
            &step_path("ghost"),
            &RunLogPayload::StepExited {
                provider_state_summary: None,
                state: StepState::Judged,
                output: None,
                localized: Vec::new(),
                localization_gaps: Vec::new(),
            },
        )
        .expect_err("must refuse");
    assert!(matches!(
        err,
        StoreError::Fold(FoldError::StepExitedWithoutEntry { seq: 2 })
    ));
    assert_eq!(store.events(&run_id).expect("events").len(), 1);
    let (log_seq, _) = store
        .materialized_checkpoint(&run_id)
        .expect("read checkpoint")
        .expect("checkpoint exists");
    assert_eq!(log_seq, 1);
    store.verify_checkpoint(&run_id).expect("still consistent");
}

/// Evidence localization is content-addressed and idempotent:
/// two-level fanout layout, dedup on re-put, one index row.
#[test]
fn evidence_put_is_idempotent_and_content_addressed() {
    let dir = TempStoreDir::new("evidence");
    let mut store = Store::open(dir.path()).expect("open store");

    let bytes = b"png-bytes-of-a-screenshot";
    let first = store.put_evidence(bytes, "image/png").expect("first put");
    assert!(!first.deduplicated);
    assert_eq!(first.sha256.len(), 64);
    assert_eq!(
        first.local_path,
        format!(
            "evidence/sha256/{}/{}/{}",
            &first.sha256[0..2],
            &first.sha256[2..4],
            first.sha256
        )
    );
    assert!(first.abs_path.is_file());
    assert_eq!(
        std::fs::read(&first.abs_path).expect("read back"),
        bytes.to_vec()
    );

    let second = store.put_evidence(bytes, "image/png").expect("second put");
    assert!(second.deduplicated);
    assert_eq!(second.sha256, first.sha256);
    assert_eq!(second.local_path, first.local_path);

    let other = store
        .put_evidence(b"different", "image/png")
        .expect("other");
    assert_ne!(other.sha256, first.sha256);

    // Exactly one index row per digest.
    let raw = rusqlite::Connection::open(dir.path().join("pointlock.db")).expect("raw conn");
    let count: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM evidence WHERE sha256 = ?1",
            [&first.sha256],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(count, 1);
}

/// Run status follows the folded transitions: awaitingHuman on request,
/// running on response, suspended/running across suspend/resume, finished
/// at the terminal.
#[test]
fn run_status_transitions_follow_the_log() {
    let dir = TempStoreDir::new("status");
    let mut store = Store::open(dir.path()).expect("open store");
    let run_id = store.begin_run(new_run("run-st")).expect("begin run");
    assert_eq!(
        store.run_status(&run_id).expect("status"),
        RunStatus::Running
    );

    append(&mut store, &run_id, root_path(), run_started());
    append(
        &mut store,
        &run_id,
        step_path("confirm"),
        RunLogPayload::HumanRequested {
            request_id: "req-9".to_owned(),
            purpose: HumanPurpose::Supervision,
            mode: None,
            prompt: "Approve mutating dispatch".to_owned(),
            presents: json!([]),
            decisions: None,
            output_schema: None,
            deadline_at_ms: None,
        },
    );
    assert_eq!(
        store.run_status(&run_id).expect("status"),
        RunStatus::AwaitingHuman
    );
    let pending = store
        .rebuild_checkpoint(&run_id)
        .expect("rebuild")
        .human_pending
        .expect("pending request");
    assert_eq!(pending.purpose, HumanPurpose::Supervision);

    append(
        &mut store,
        &run_id,
        step_path("confirm"),
        RunLogPayload::HumanResponded {
            request_id: "req-9".to_owned(),
            purpose: HumanPurpose::Supervision,
            response: json!({"decision": "proceed"}),
            actor: "cli:tester".to_owned(),
        },
    );
    assert_eq!(
        store.run_status(&run_id).expect("status"),
        RunStatus::Running
    );

    append(
        &mut store,
        &run_id,
        root_path(),
        RunLogPayload::RunSuspended {
            provider_state_summary: None,
            reason: Some("operator pause".to_owned()),
        },
    );
    assert_eq!(
        store.run_status(&run_id).expect("status"),
        RunStatus::Suspended
    );

    append(
        &mut store,
        &run_id,
        root_path(),
        RunLogPayload::RunResumed {
            alignment_report: AlignmentReport {
                entries: vec![],
                resume_point: None,
                requires_confirmation: vec![],
            },
            supervise_policy: None,
            event_cursor: None,
        },
    );
    assert_eq!(
        store.run_status(&run_id).expect("status"),
        RunStatus::Running
    );

    append(
        &mut store,
        &run_id,
        root_path(),
        RunLogPayload::RunFinished {
            verdict: None,
            remote_archival_error: None,
        },
    );
    assert_eq!(
        store.run_status(&run_id).expect("status"),
        RunStatus::Finished
    );
    store.verify_checkpoint(&run_id).expect("consistent at end");
}

/// verify_checkpoint detects a tampered materialized row (I1's runtime
/// self-check): the mismatch is a typed error, not a silent pass.
#[test]
fn verify_checkpoint_detects_a_corrupted_materialization() {
    let dir = TempStoreDir::new("verify");
    let mut store = Store::open(dir.path()).expect("open store");
    let run_id = store.begin_run(new_run("run-vf")).expect("begin run");
    append(&mut store, &run_id, root_path(), run_started());
    store.verify_checkpoint(&run_id).expect("consistent");

    // Corrupt the stored view out-of-band (something no Store API allows).
    let raw = rusqlite::Connection::open(dir.path().join("pointlock.db")).expect("raw conn");
    let mut view: Value = raw
        .query_row(
            "SELECT view FROM checkpoint WHERE run_id = ?1",
            [&run_id],
            |row| row.get::<_, String>(0),
        )
        .map(|s| serde_json::from_str(&s).expect("stored view parses"))
        .expect("stored view");
    view["paramsSnapshot"] = json!({"user": "mallory"});
    raw.execute(
        "UPDATE checkpoint SET view = ?2 WHERE run_id = ?1",
        rusqlite::params![run_id, view.to_string()],
    )
    .expect("tamper");
    drop(raw);

    let err = store.verify_checkpoint(&run_id).expect_err("must detect");
    assert!(matches!(err, StoreError::CheckpointMismatch { .. }));
}

/// begin_run refuses duplicate run ids; unknown runs surface typed errors.
#[test]
fn duplicate_and_unknown_runs_are_typed_errors() {
    let dir = TempStoreDir::new("ids");
    let mut store = Store::open(dir.path()).expect("open store");
    store.begin_run(new_run("run-dup")).expect("first begin");
    let err = store
        .begin_run(new_run("run-dup"))
        .expect_err("must refuse");
    assert!(matches!(err, StoreError::DuplicateRun(id) if id == "run-dup"));

    let err = store
        .append_event(
            "run-ghost",
            1,
            &root_path(),
            &RunLogPayload::RunSuspended {
                provider_state_summary: None,
                reason: None,
            },
        )
        .expect_err("must refuse");
    assert!(matches!(err, StoreError::UnknownRun(id) if id == "run-ghost"));
    let err = store.events("run-ghost").expect_err("must refuse");
    assert!(matches!(err, StoreError::UnknownRun(_)));
    let err = store
        .verify_checkpoint("run-dup")
        .expect_err("no events yet");
    assert!(matches!(err, StoreError::NoCheckpoint(_)));
}

// ─── Human response arbitration (06 §4.3; R13) ──────────────────────────────

/// Stages a run awaiting one human request and returns the store.
fn staged_human_run(dir: &TempStoreDir, run_id: &str, payload: RunLogPayload) -> (Store, String) {
    let mut store = Store::open(dir.path()).expect("open store");
    let run_id = store.begin_run(new_run(run_id)).expect("begin run");
    append(&mut store, &run_id, root_path(), run_started());
    append(&mut store, &run_id, step_path("ask"), payload);
    (store, run_id)
}

fn confirm_request(request_id: &str, deadline_at_ms: u64) -> RunLogPayload {
    RunLogPayload::HumanRequested {
        request_id: request_id.to_owned(),
        purpose: HumanPurpose::Step,
        mode: Some(HumanMode::Confirm),
        prompt: "Approve the transfer?".to_owned(),
        presents: json!([]),
        decisions: Some(vec!["approve".to_owned(), "reject".to_owned()]),
        output_schema: None,
        deadline_at_ms: Some(deadline_at_ms),
    }
}

fn rejection(err: StoreError) -> HumanResponseRejection {
    match err {
        StoreError::HumanResponseRejected { reason, .. } => reason,
        other => panic!("expected HumanResponseRejected, got {other}"),
    }
}

/// A valid response is appended as `humanResponded` and clears the
/// pending request; a second final response is refused (first response
/// wins).
#[test]
fn arbitration_accepts_once_and_refuses_the_second_response() {
    let dir = TempStoreDir::new("arb-first-wins");
    let (mut store, run_id) = staged_human_run(&dir, "run-arb", confirm_request("req-1", 5_000));
    assert_eq!(
        store.run_status(&run_id).expect("status"),
        RunStatus::AwaitingHuman
    );

    let seq = store
        .submit_human_response(
            &run_id,
            "req-1",
            "cli:os:alice@host",
            4_000,
            json!({"decision": "approve", "note": "looks right"}),
        )
        .expect("first response accepted");
    let events = store.events(&run_id).expect("events");
    assert_eq!(events.last().expect("last").seq, seq);
    assert_eq!(
        events.last().expect("last").payload.event_type(),
        "humanResponded"
    );
    let view = store.rebuild_checkpoint(&run_id).expect("rebuild");
    assert!(view.human_pending.is_none());

    // First response wins: the racing second answer is refused, and the
    // ledger is untouched.
    let err = store
        .submit_human_response(
            &run_id,
            "req-1",
            "cli:os:bob@host",
            4_500,
            json!({"decision": "reject"}),
        )
        .expect_err("second response must be refused");
    assert_eq!(rejection(err), HumanResponseRejection::AlreadyResponded);
    assert_eq!(store.events(&run_id).expect("events").len(), events.len());
}

/// Unknown request ids are typed rejections.
#[test]
fn arbitration_refuses_unknown_requests() {
    let dir = TempStoreDir::new("arb-unknown");
    let (mut store, run_id) = staged_human_run(&dir, "run-arb", confirm_request("req-1", 5_000));
    let err = store
        .submit_human_response(
            &run_id,
            "req-ghost",
            "cli:x",
            1,
            json!({"decision": "approve"}),
        )
        .expect_err("must refuse");
    assert_eq!(rejection(err), HumanResponseRejection::UnknownRequest);
}

/// The store-receipt clock is the only timeout judge: a response received
/// after `deadlineAtMs` is refused without writing any event — settlement
/// of the expired request stays the runner's job (lazy, 06 §5.3).
#[test]
fn arbitration_refuses_late_responses_without_writing_events() {
    let dir = TempStoreDir::new("arb-deadline");
    let (mut store, run_id) = staged_human_run(&dir, "run-arb", confirm_request("req-1", 5_000));
    let before = store.events(&run_id).expect("events").len();

    let err = store
        .submit_human_response(
            &run_id,
            "req-1",
            "cli:x",
            5_001,
            json!({"decision": "approve"}),
        )
        .expect_err("late response must be refused");
    assert_eq!(
        rejection(err),
        HumanResponseRejection::DeadlineExpired {
            deadline_at_ms: 5_000,
            received_at_ms: 5_001,
        }
    );
    assert_eq!(store.events(&run_id).expect("events").len(), before);
    // The request stays pending: the arbitration never settles timeouts.
    assert!(
        store
            .rebuild_checkpoint(&run_id)
            .expect("rebuild")
            .human_pending
            .is_some()
    );

    // The boundary is inclusive: at == deadline is still in time.
    store
        .submit_human_response(
            &run_id,
            "req-1",
            "cli:x",
            5_000,
            json!({"decision": "approve"}),
        )
        .expect("on-deadline response accepted");
}

/// Shape validation per purpose/mode: out-of-vocabulary decisions and
/// schema-violating provideInput payloads never enter the ledger.
#[test]
fn arbitration_validates_response_shapes() {
    // confirm: membership in the request's double label.
    let dir = TempStoreDir::new("arb-shape-confirm");
    let (mut store, run_id) = staged_human_run(&dir, "run-arb", confirm_request("req-1", 5_000));
    let err = store
        .submit_human_response(&run_id, "req-1", "cli:x", 1, json!({"decision": "maybe"}))
        .expect_err("must refuse an out-of-label decision");
    assert!(matches!(
        rejection(err),
        HumanResponseRejection::InvalidShape { .. }
    ));
    let err = store
        .submit_human_response(
            &run_id,
            "req-1",
            "cli:x",
            1,
            json!({"decision": "approve", "extra": 1}),
        )
        .expect_err("must refuse unexpected fields");
    assert!(matches!(
        rejection(err),
        HumanResponseRejection::InvalidShape { .. }
    ));

    // judge: the three-valued vocabulary admits no aliases.
    let dir = TempStoreDir::new("arb-shape-judge");
    let (mut store, run_id) = staged_human_run(
        &dir,
        "run-arb",
        RunLogPayload::HumanRequested {
            request_id: "req-j".to_owned(),
            purpose: HumanPurpose::Step,
            mode: Some(HumanMode::Judge),
            prompt: "Final ruling".to_owned(),
            presents: json!([]),
            decisions: None,
            output_schema: None,
            deadline_at_ms: Some(5_000),
        },
    );
    let err = store
        .submit_human_response(&run_id, "req-j", "cli:x", 1, json!({"status": "approve"}))
        .expect_err("must refuse an aliased status");
    assert!(matches!(
        rejection(err),
        HumanResponseRejection::InvalidShape { .. }
    ));
    store
        .submit_human_response(&run_id, "req-j", "cli:x", 1, json!({"status": "unknown"}))
        .expect("three-valued status accepted");

    // provideInput: the input must satisfy the request's outputSchema.
    let dir = TempStoreDir::new("arb-shape-input");
    let schema = JsonSchemaDocument::new(json!({
        "type": "object",
        "properties": { "code": { "type": "string", "pattern": "^[0-9]{4}$" } },
        "required": ["code"],
        "additionalProperties": false
    }))
    .expect("valid schema");
    let (mut store, run_id) = staged_human_run(
        &dir,
        "run-arb",
        RunLogPayload::HumanRequested {
            request_id: "req-i".to_owned(),
            purpose: HumanPurpose::Step,
            mode: Some(HumanMode::ProvideInput),
            prompt: "Enter the code".to_owned(),
            presents: json!([]),
            decisions: None,
            output_schema: Some(schema),
            deadline_at_ms: Some(5_000),
        },
    );
    let before = store.events(&run_id).expect("events").len();
    let err = store
        .submit_human_response(
            &run_id,
            "req-i",
            "cli:x",
            1,
            json!({"input": {"code": 1234}}),
        )
        .expect_err("must refuse a schema violation");
    assert!(matches!(
        rejection(err),
        HumanResponseRejection::InvalidShape { .. }
    ));
    assert_eq!(store.events(&run_id).expect("events").len(), before);
    store
        .submit_human_response(
            &run_id,
            "req-i",
            "cli:x",
            1,
            json!({"input": {"code": "1234"}}),
        )
        .expect("valid input accepted");
}

/// Supervision decisions are the closed three: proceed | abort | suspend
/// (no skip); a suspend answer is recorded but keeps the request pending,
/// so a later final ruling still pairs.
#[test]
fn arbitration_supervision_suspend_is_non_final() {
    let dir = TempStoreDir::new("arb-supervision");
    let (mut store, run_id) = staged_human_run(
        &dir,
        "run-arb",
        RunLogPayload::HumanRequested {
            request_id: "req-s".to_owned(),
            purpose: HumanPurpose::Supervision,
            mode: None,
            prompt: "Approve dispatch of tapPay".to_owned(),
            presents: json!([]),
            decisions: None,
            output_schema: None,
            deadline_at_ms: None,
        },
    );
    let err = store
        .submit_human_response(&run_id, "req-s", "cli:x", 1, json!({"decision": "skip"}))
        .expect_err("skip is deliberately not a decision");
    assert!(matches!(
        rejection(err),
        HumanResponseRejection::InvalidShape { .. }
    ));

    // suspend: recorded, request stays pending, run stays awaitingHuman.
    store
        .submit_human_response(&run_id, "req-s", "cli:x", 2, json!({"decision": "suspend"}))
        .expect("suspend accepted");
    let view = store.rebuild_checkpoint(&run_id).expect("rebuild");
    assert_eq!(
        view.human_pending.expect("still pending").request_id,
        "req-s"
    );
    assert_eq!(
        store.run_status(&run_id).expect("status"),
        RunStatus::AwaitingHuman
    );

    // Supervision requests have no deadline: a very late final ruling
    // still pairs (received long "after" any plausible clock).
    store
        .submit_human_response(
            &run_id,
            "req-s",
            "cli:x",
            9_999_999,
            json!({"decision": "proceed"}),
        )
        .expect("proceed accepted after suspend");
    let view = store.rebuild_checkpoint(&run_id).expect("rebuild");
    assert!(view.human_pending.is_none());

    // The final ruling closed the request for good.
    let err = store
        .submit_human_response(
            &run_id,
            "req-s",
            "cli:x",
            10_000_000,
            json!({"decision": "abort"}),
        )
        .expect_err("must refuse after a final ruling");
    assert_eq!(rejection(err), HumanResponseRejection::AlreadyResponded);
}

// ─── The single-writer incremental fold cache (M3a-W4) ──────────────────────

/// One full step's event sequence appended through `append_event`.
fn append_step(store: &mut Store, run_id: &str, name: &str, call_id: &str) {
    append(
        store,
        run_id,
        step_path(name),
        RunLogPayload::StepEntered {
            step_id: step_id(name),
            effect_hash: hash('e'),
            judge_hash: hash('f'),
            resolved_inputs: json!({ "target": name }),
        },
    );
    append(
        store,
        run_id,
        step_path(name),
        RunLogPayload::ActionIntent {
            call_id: call_id.to_owned(),
            args_snapshot: json!({ "target": name }),
            chain_index: None,
            channel: None,
            action_name: None,
        },
    );
    append(
        store,
        run_id,
        step_path(name),
        RunLogPayload::ActionSettled {
            call_id: call_id.to_owned(),
            outcome: succeeded_outcome(call_id),
        },
    );
    append(
        store,
        run_id,
        step_path(name),
        RunLogPayload::StepExited {
            provider_state_summary: None,
            state: StepState::Judged,
            output: Some(json!({ "tapped": true })),
            localized: Vec::new(),
            localization_gaps: Vec::new(),
        },
    );
}

/// The incremental path must materialize exactly what the from-scratch
/// refold does — `verify_checkpoint` (I1) is asserted after every single
/// append across a multi-step run driven through one handle (all cache
/// hits after the first).
#[test]
fn incremental_fold_cache_matches_the_full_refold_at_every_append() {
    let dir = TempStoreDir::new("fold-cache");
    let mut store = Store::open(dir.path()).expect("open store");
    let run_id = store.begin_run(new_run("run-cache")).expect("begin run");
    append(&mut store, &run_id, root_path(), run_started());
    store.verify_checkpoint(&run_id).expect("after runStarted");
    for (index, name) in ["s1", "s2", "s3", "s4"].iter().enumerate() {
        let call_id = format!("c-{index}");
        // Append the step's four events one at a time, verifying the
        // materialized view against the full refold after each.
        for step_event in 0..4u8 {
            let payload = match step_event {
                0 => RunLogPayload::StepEntered {
                    step_id: step_id(name),
                    effect_hash: hash('e'),
                    judge_hash: hash('f'),
                    resolved_inputs: json!({ "target": name }),
                },
                1 => RunLogPayload::ActionIntent {
                    call_id: call_id.clone(),
                    args_snapshot: json!({ "target": name }),
                    chain_index: None,
                    channel: None,
                    action_name: None,
                },
                2 => RunLogPayload::ActionSettled {
                    call_id: call_id.clone(),
                    outcome: succeeded_outcome(&call_id),
                },
                _ => RunLogPayload::StepExited {
                    provider_state_summary: None,
                    state: StepState::Judged,
                    output: Some(json!({ "tapped": true })),
                    localized: Vec::new(),
                    localization_gaps: Vec::new(),
                },
            };
            append(&mut store, &run_id, step_path(name), payload);
            store
                .verify_checkpoint(&run_id)
                .unwrap_or_else(|err| panic!("verify after {name} event {step_event}: {err}"));
        }
    }
    let view = store.verify_checkpoint(&run_id).expect("final verify");
    assert_eq!(view.completed.len(), 4);
}

/// A second handle interleaving appends creates a seq discontinuity for
/// the first handle's cache — the fallback full refold must keep both
/// handles' materializations exact (single-writer is the invariant per
/// *run segment*, but stale handles must never corrupt the view).
#[test]
fn a_stale_handle_falls_back_to_the_full_refold() {
    let dir = TempStoreDir::new("fold-stale");
    let mut writer_a = Store::open(dir.path()).expect("open A");
    let run_id = writer_a.begin_run(new_run("run-stale")).expect("begin run");
    append(&mut writer_a, &run_id, root_path(), run_started());
    append_step(&mut writer_a, &run_id, "s1", "c-1");

    // Writer B appends from a cold cache (full-refold path), advancing
    // the log behind A's back.
    let mut writer_b = Store::open(dir.path()).expect("open B");
    append_step(&mut writer_b, &run_id, "s2", "c-2");
    writer_b.verify_checkpoint(&run_id).expect("B verify");

    // A's cache is now stale (its seq watermark lags the log head): its
    // next append must detect the discontinuity and refold fully.
    append_step(&mut writer_a, &run_id, "s3", "c-3");
    let view = writer_a.verify_checkpoint(&run_id).expect("A verify");
    assert_eq!(view.completed.len(), 3);
}

/// A refused append (fold error) must not poison the cache: subsequent
/// valid appends keep materializing exactly.
#[test]
fn a_refused_append_does_not_poison_the_cache() {
    let dir = TempStoreDir::new("fold-poison");
    let mut store = Store::open(dir.path()).expect("open store");
    let run_id = store.begin_run(new_run("run-poison")).expect("begin run");
    append(&mut store, &run_id, root_path(), run_started());
    append_step(&mut store, &run_id, "s1", "c-1");

    // A stepExited with no step in flight is a fold error — the append
    // is refused and rolled back.
    let error = store
        .append_event(
            &run_id,
            1_000,
            &step_path("ghost"),
            &RunLogPayload::StepExited {
                provider_state_summary: None,
                state: StepState::Judged,
                output: None,
                localized: Vec::new(),
                localization_gaps: Vec::new(),
            },
        )
        .expect_err("a spanless stepExited must be refused");
    assert!(matches!(
        error,
        StoreError::Fold(FoldError::StepExitedWithoutEntry { .. })
    ));

    // The ledger did not grow, the view still verifies, and the next
    // valid append folds exactly.
    store
        .verify_checkpoint(&run_id)
        .expect("verify after refusal");
    append_step(&mut store, &run_id, "s2", "c-2");
    let view = store
        .verify_checkpoint(&run_id)
        .expect("verify after recovery");
    assert_eq!(view.completed.len(), 2);
}

// ─── sessionLineage per segment (07 §4.5, incorporated 2026-07-18) ──────────

/// A cursor-bearing `runResumed` extends the folded binding lineage and
/// reseeds the watermark; a cursor-less one changes nothing (old-ledger
/// honesty). verify_checkpoint stays exact throughout.
#[test]
fn a_cursor_bearing_resume_extends_the_folded_lineage() {
    let dir = TempStoreDir::new("lineage");
    let mut store = Store::open(dir.path()).expect("open store");
    let run_id = store.begin_run(new_run("run-lineage")).expect("begin");
    append(&mut store, &run_id, root_path(), run_started());
    append(
        &mut store,
        &run_id,
        root_path(),
        RunLogPayload::RunSuspended {
            provider_state_summary: None,
            reason: Some("stop".to_owned()),
        },
    );
    // Old-style resume (no cursor): lineage unchanged.
    append(
        &mut store,
        &run_id,
        root_path(),
        RunLogPayload::RunResumed {
            alignment_report: AlignmentReport {
                entries: vec![],
                resume_point: None,
                requires_confirmation: vec![],
            },
            supervise_policy: None,
            event_cursor: None,
        },
    );
    let view = store.verify_checkpoint(&run_id).expect("exact");
    assert_eq!(view.binding.session_lineage, vec!["s-1".to_owned()]);
    assert_eq!(view.binding.event_cursor.session_id, "s-1");

    // New-style resume: lineage extends, cursor reseeds.
    append(
        &mut store,
        &run_id,
        root_path(),
        RunLogPayload::RunSuspended {
            provider_state_summary: None,
            reason: Some("stop".to_owned()),
        },
    );
    append(
        &mut store,
        &run_id,
        root_path(),
        RunLogPayload::RunResumed {
            alignment_report: AlignmentReport {
                entries: vec![],
                resume_point: None,
                requires_confirmation: vec![],
            },
            supervise_policy: None,
            event_cursor: Some(EventCursor {
                session_id: "s-2".to_owned(),
                last_sequence: 0,
            }),
        },
    );
    let view = store.verify_checkpoint(&run_id).expect("exact");
    assert_eq!(
        view.binding.session_lineage,
        vec!["s-1".to_owned(), "s-2".to_owned()]
    );
    assert_eq!(view.binding.event_cursor.session_id, "s-2");
}
