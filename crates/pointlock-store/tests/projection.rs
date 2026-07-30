//! Projection-protocol tests (spine §10, R14): a scripted run exercises
//! the five DTO families; golden fixtures under
//! `schema/fixtures/projection/` anchor the wire shapes for the
//! `@pointlock/projection-types` consumers (02 §1.1 pipeline); every
//! generated schema must satisfy the Draft 2020-12 meta-schema and accept
//! its family's fixtures.
//!
//! Re-bless fixtures with `POINTLOCK_BLESS_PROJECTION=1 cargo test -p
//! pointlock-store --test projection` after an intentional additive change.

use std::path::PathBuf;

use pointlock_ir::{
    AssertionOutcomeRecord, AssetRef, BindingState, Channel, EventCursor, EvidenceRef, FlowIR,
    Hash, HumanMode, HumanPurpose, ObservationRecord, PathFrame, RunLogPayload, RunPath, StepState,
    UiSnapshotOmissionReason, Verdict, VerdictStatus, render_run_path,
};
use pointlock_store::projection::{
    self, ProjectionSchemaFamily, RunTimelineFilter, TIMELINE_MAX_PAGE_SIZE,
};
use pointlock_store::{NewRun, Store};
use serde_json::{Value, json};

// ─── Helpers ────────────────────────────────────────────────────────────────

struct TempStore {
    root: PathBuf,
    store: Store,
}

impl TempStore {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pointlock-projection-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(&root).expect("open store");
        TempStore { root, store }
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn hash(digit: char) -> Hash {
    Hash::try_from(format!("sha256:{}", digit.to_string().repeat(64))).expect("hash")
}

fn flow_frame() -> PathFrame {
    PathFrame::Flow {
        flow_id: "demo".try_into().expect("flow id"),
        ir_hash: hash('a'),
    }
}

fn step_path(step_id: &str) -> RunPath {
    vec![
        flow_frame(),
        PathFrame::Step {
            step_id: step_id.try_into().expect("step id"),
        },
    ]
}

fn attempt_path(step_id: &str, n: u64) -> RunPath {
    let mut path = step_path(step_id);
    path.push(PathFrame::Attempt { n });
    path.push(PathFrame::Phase {
        phase: pointlock_ir::Phase::Act,
    });
    path
}

fn binding() -> BindingState {
    BindingState {
        device_id: "fake-device-1".to_owned(),
        session_lineage: vec!["session-1".to_owned()],
        event_cursor: EventCursor {
            session_id: "session-1".to_owned(),
            last_sequence: 0,
        },
    }
}

fn evidence_ref(digit: char) -> EvidenceRef {
    let hex = digit.to_string().repeat(64);
    EvidenceRef {
        asset: AssetRef {
            id: format!("asset-{digit}"),
            media_type: "image/png".to_owned(),
            uri: format!("devicerail://assets/asset-{digit}"),
            sha256: Some(hex.clone()),
        },
        sha256: hex.clone(),
        local_path: format!("evidence/sha256/{}/{}/{hex}", &hex[..2], &hex[2..4]),
    }
}

fn pass_verdict(summary: &str) -> Verdict {
    Verdict {
        status: VerdictStatus::Pass,
        degraded: false,
        summary: summary.to_owned(),
        evidence: Vec::new(),
        supersedes: None,
    }
}

/// Appends the scripted golden run: one succeeded action step with a full
/// evidence/assertion/verdict trail, one judged human step, terminal
/// flow verdict. Deterministic clocks — the fixtures depend on it.
fn scripted_run(store: &mut Store) -> String {
    let run_id = store
        .begin_run(NewRun {
            run_id: Some("run-golden".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({ "ssid": "lab" }),
            binding: binding(),
            created_at_ms: 1_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];

    let mut at = 1_000u64;
    let mut append = |store: &mut Store, path: &RunPath, payload: RunLogPayload| {
        at += 10;
        store
            .append_event(&run_id, at, path, &payload)
            .expect("append event");
    };

    append(
        store,
        &root,
        RunLogPayload::RunStarted {
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({ "ssid": "lab" }),
            supervise_policy: None,
        },
    );

    // step_a: succeeded action with the full trail.
    let a = step_path("step_a");
    append(
        store,
        &a,
        RunLogPayload::StepEntered {
            step_id: "step_a".try_into().expect("step id"),
            effect_hash: hash('c'),
            judge_hash: hash('d'),
            resolved_inputs: json!({ "element": "ssid_field", "value": "lab" }),
        },
    );
    append(
        store,
        &attempt_path("step_a", 1),
        RunLogPayload::ActionIntent {
            call_id: "call-1".to_owned(),
            args_snapshot: json!({ "element": "ssid_field", "value": "lab" }),
            chain_index: None,
            channel: None,
            action_name: None,
        },
    );
    append(
        store,
        &attempt_path("step_a", 1),
        RunLogPayload::ActionSettled {
            call_id: "call-1".to_owned(),
            outcome: pointlock_ir::ActionOutcome::Succeeded {
                result: Box::new(pointlock_ir::ActionResult {
                    call_id: "call-1".to_owned(),
                    started_at_ms: 1_020,
                    finished_at_ms: 1_025,
                    output: json!({ "value": "lab" }),
                    before: None,
                    after: None,
                    evidence: Vec::new(),
                    execution: None,
                }),
            },
        },
    );
    append(
        store,
        &a,
        RunLogPayload::ObservationRecorded {
            observation: ObservationRecord {
                viewport: Some(pointlock_ir::Viewport {
                    width: 1080,
                    height: 2400,
                    scale_factor: 2.0,
                }),
                observation_id: "obs-1".to_owned(),
                captured_at_ms: 1_030,
                screenshot: Some(evidence_ref('e')),
                screenshot_omission: None,
                ui_snapshot: None,
                ui_snapshot_omission: Some(UiSnapshotOmissionReason::DriverUnsupported),
            },
        },
    );
    append(
        store,
        &a,
        RunLogPayload::AssertionEvaluated {
            outcome: AssertionOutcomeRecord {
                assert_id: "ssid_was_typed".try_into().expect("assert id"),
                result: VerdictStatus::Pass,
                channel: Some(Channel::Dom),
                reason: "dom text equals the requested ssid".to_owned(),
            },
        },
    );
    append(
        store,
        &a,
        RunLogPayload::VerdictRecorded {
            verdict: pass_verdict("all assertions pass on the dom channel"),
            localized: Vec::new(),
            localization_gaps: Vec::new(),
            remote_archival_error: None,
        },
    );
    append(
        store,
        &a,
        RunLogPayload::StepExited {
            provider_state_summary: None,
            state: StepState::Judged,
            output: Some(json!({ "value": "lab" })),
            localized: Vec::new(),
            localization_gaps: Vec::new(),
        },
    );

    // step_b: judged human step.
    let b = step_path("step_b");
    append(
        store,
        &b,
        RunLogPayload::StepEntered {
            step_id: "step_b".try_into().expect("step id"),
            effect_hash: hash('e'),
            judge_hash: hash('f'),
            resolved_inputs: Value::Null,
        },
    );
    append(
        store,
        &b,
        RunLogPayload::HumanRequested {
            request_id: "req-1".to_owned(),
            purpose: HumanPurpose::Step,
            mode: Some(HumanMode::Judge),
            prompt: "Does the panel show the connected banner?".to_owned(),
            presents: json!([]),
            decisions: Some(vec!["pass".to_owned(), "fail".to_owned()]),
            output_schema: None,
            deadline_at_ms: Some(600_000),
        },
    );
    append(
        store,
        &b,
        RunLogPayload::HumanResponded {
            request_id: "req-1".to_owned(),
            purpose: HumanPurpose::Step,
            response: json!({ "status": "pass" }),
            actor: "cli:tester@host".to_owned(),
        },
    );
    append(
        store,
        &b,
        RunLogPayload::VerdictRecorded {
            verdict: pass_verdict("human judged pass"),
            localized: Vec::new(),
            localization_gaps: Vec::new(),
            remote_archival_error: None,
        },
    );
    append(
        store,
        &b,
        RunLogPayload::StepExited {
            provider_state_summary: None,
            state: StepState::Judged,
            output: None,
            localized: Vec::new(),
            localization_gaps: Vec::new(),
        },
    );

    append(
        store,
        &root,
        RunLogPayload::RunFinished {
            verdict: Some(pass_verdict("flow verdict pass")),
            remote_archival_error: None,
        },
    );
    run_id
}

fn repo_schema_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema")
}

fn load_fixture_flow(name: &str) -> FlowIR {
    let path = repo_schema_dir().join("fixtures/positive").join(name);
    let raw = std::fs::read_to_string(&path).expect("read positive fixture");
    serde_json::from_str(&raw).expect("fixture parses as FlowIR")
}

/// Golden compare-or-bless (append-only corpus discipline, 02 §1.1).
fn golden(name: &str, value: &impl serde::Serialize) {
    let dir = repo_schema_dir().join("fixtures/projection");
    let path = dir.join(name);
    let mut body = serde_json::to_string_pretty(value).expect("serialize");
    body.push('\n');
    if std::env::var_os("POINTLOCK_BLESS_PROJECTION").is_some() {
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        std::fs::write(&path, &body).expect("write fixture");
        return;
    }
    let stored = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden fixture {}; run with POINTLOCK_BLESS_PROJECTION=1 to create it",
            path.display()
        )
    });
    assert_eq!(
        stored,
        body,
        "golden fixture {} drifted; re-bless deliberately if the change is intended",
        path.display()
    );
}

// ─── RunOverview ────────────────────────────────────────────────────────────

#[test]
fn overview_projects_the_scripted_run() {
    let mut temp = TempStore::new("overview");
    let run_id = scripted_run(&mut temp.store);
    let overview = projection::run_overview(&temp.store, &run_id).expect("overview");

    assert_eq!(overview.status, "finished");
    assert_eq!(overview.flow_verdict_status.as_deref(), Some("pass"));
    assert_eq!(overview.flow_verdict_degraded, Some(false));
    assert_eq!(overview.revision, 14);
    assert!(!overview.awaiting_human);
    assert_eq!(overview.supervise_policy, None);
    assert_eq!(overview.steps.len(), 2);

    let key = render_run_path(&step_path("step_a"));
    let cell = overview.steps.get(&key).expect("step_a cell");
    assert_eq!(cell.state, StepState::Judged);
    assert_eq!(cell.verdict_status.as_deref(), Some("pass"));
    assert_eq!(cell.degraded, Some(false));

    golden("run-overview.golden.json", &overview);
}

// ─── Timeline ───────────────────────────────────────────────────────────────

#[test]
fn timeline_filters_follow_the_pinned_mapping() {
    let mut temp = TempStore::new("timeline");
    let run_id = scripted_run(&mut temp.store);

    let all = projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::All, 1, 50)
        .expect("all");
    assert_eq!(all.total, 14);
    assert_eq!(all.entries.len(), 14);
    assert_eq!(all.revision, 14);

    let actions =
        projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::Actions, 1, 50)
            .expect("actions");
    assert_eq!(actions.total, 2, "intent + settled");

    let observations =
        projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::Observations, 1, 50)
            .expect("observations");
    assert_eq!(observations.total, 1);
    assert_eq!(observations.entries[0].evidence.len(), 1);

    let verdicts =
        projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::Verdicts, 1, 50)
            .expect("verdicts");
    // assertionEvaluated + two verdictRecorded; structural events stay
    // out of the four special filters (08 §4.2).
    assert_eq!(verdicts.total, 3);

    let errors = projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::Errors, 1, 50)
        .expect("errors");
    assert_eq!(errors.total, 0);

    // Page-size clamp: requests can only shrink the 50 cap.
    let clamped = projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::All, 1, 500)
        .expect("clamped");
    assert_eq!(clamped.page_size, TIMELINE_MAX_PAGE_SIZE);

    golden("run-timeline.golden.json", &all);
}

#[test]
fn timeline_bounds_oversized_text() {
    let mut temp = TempStore::new("bounds");
    let run_id = temp
        .store
        .begin_run(NewRun {
            run_id: Some("run-bounds".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 1_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];
    temp.store
        .append_event(
            &run_id,
            1_010,
            &root,
            &RunLogPayload::RunStarted {
                ir_hash: hash('a'),
                lockfile_digest: hash('b'),
                params_snapshot: json!({}),
                supervise_policy: None,
            },
        )
        .expect("start");
    let a = step_path("step_a");
    temp.store
        .append_event(
            &run_id,
            1_020,
            &a,
            &RunLogPayload::StepEntered {
                step_id: "step_a".try_into().expect("step id"),
                effect_hash: hash('c'),
                judge_hash: hash('d'),
                resolved_inputs: Value::Null,
            },
        )
        .expect("enter");
    temp.store
        .append_event(
            &run_id,
            1_030,
            &a,
            &RunLogPayload::AssertionEvaluated {
                outcome: AssertionOutcomeRecord {
                    assert_id: "big".try_into().expect("assert id"),
                    result: VerdictStatus::Unknown,
                    channel: None,
                    reason: "x".repeat(10_000),
                },
            },
        )
        .expect("assert");

    let page = projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::Verdicts, 1, 50)
        .expect("page");
    let entry = &page.entries[0];
    assert!(entry.truncated, "4 KiB text cap must flag truncation");
    match &entry.detail {
        projection::TimelineDetail::AssertionEvaluated { reason, .. } => {
            assert!(reason.len() <= 4 * 1024);
        }
        other => panic!("unexpected detail {other:?}"),
    }
}

// ─── StepDossierView + locate ───────────────────────────────────────────────

#[test]
fn dossier_joins_the_full_attempt_record() {
    let mut temp = TempStore::new("dossier");
    let run_id = scripted_run(&mut temp.store);

    let path = projection::locate_step(&temp.store, &run_id, "step_a").expect("locate bare id");
    let dossier = projection::step_dossier(&temp.store, &run_id, &path, &[]).expect("dossier");

    assert_eq!(dossier.step_id, "step_a");
    assert_eq!(dossier.run_path, render_run_path(&step_path("step_a")));
    assert_eq!(dossier.attempts.len(), 1);
    let attempt = &dossier.attempts[0];
    assert_eq!(attempt.n, Some(1));
    assert_eq!(attempt.outcome.as_deref(), Some("succeeded"));
    assert_eq!(attempt.started_at_ms, Some(1_020));
    assert_eq!(attempt.finished_at_ms, Some(1_025));
    assert_eq!(dossier.observations.len(), 1);
    assert_eq!(dossier.assertion_outcomes.len(), 1);
    assert_eq!(dossier.evidence.len(), 1);
    assert_eq!(dossier.verdict_history.len(), 1);
    assert_eq!(
        dossier.verdict.as_ref().map(|v| v.status),
        Some(VerdictStatus::Pass)
    );
    assert_eq!(dossier.state, Some(StepState::Judged));
    assert!(dossier.ir_node.is_none(), "no artifact supplied");

    // Canonical-string locate resolves to the same instance (spine §9
    // round-trip guarantee).
    let by_path =
        projection::locate_step(&temp.store, &run_id, &dossier.run_path).expect("locate by path");
    assert_eq!(render_run_path(&by_path), dossier.run_path);

    golden("step-dossier-view.golden.json", &dossier);
}

#[test]
fn locate_rejects_unknown_and_bad_paths() {
    let mut temp = TempStore::new("locate-errors");
    let run_id = scripted_run(&mut temp.store);

    let unknown = projection::locate_step(&temp.store, &run_id, "no_such_step");
    assert!(matches!(
        unknown,
        Err(pointlock_store::StoreError::UnknownStepInstance { .. })
    ));

    let bad = projection::locate_step(&temp.store, &run_id, "demo@zz/step");
    assert!(matches!(
        bad,
        Err(pointlock_store::StoreError::BadRunPath { .. })
    ));
}

#[test]
fn dossier_resolves_ir_node_and_source_span() {
    let mut temp = TempStore::new("dossier-ir");
    let flow = load_fixture_flow("wifi_toggle.json");

    // Script a minimal run of the fixture flow so the governing hash of
    // the instance path matches the fixture's irHash.
    let run_id = temp
        .store
        .begin_run(NewRun {
            run_id: Some("run-ir".to_owned()),
            flow_id: flow.flow_id.clone(),
            ir_hash: flow.ir_hash.clone(),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 1_000,
        })
        .expect("begin run");
    let root = vec![PathFrame::Flow {
        flow_id: flow.flow_id.clone(),
        ir_hash: flow.ir_hash.clone(),
    }];
    temp.store
        .append_event(
            &run_id,
            1_010,
            &root,
            &RunLogPayload::RunStarted {
                ir_hash: flow.ir_hash.clone(),
                lockfile_digest: hash('b'),
                params_snapshot: json!({}),
                supervise_policy: None,
            },
        )
        .expect("start");
    let first_step = flow.body[0].step_id().clone();
    let mut path = root.clone();
    path.push(PathFrame::Step {
        step_id: first_step.clone(),
    });
    temp.store
        .append_event(
            &run_id,
            1_020,
            &path,
            &RunLogPayload::StepEntered {
                step_id: first_step.clone(),
                effect_hash: hash('c'),
                judge_hash: hash('d'),
                resolved_inputs: Value::Null,
            },
        )
        .expect("enter");

    let dossier =
        projection::step_dossier(&temp.store, &run_id, &path, std::slice::from_ref(&flow))
            .expect("dossier");
    let node = dossier.ir_node.expect("irNode present with the artifact");
    assert_eq!(node.step_id(), &first_step);
    let source = dossier.source.expect("source present with the artifact");
    assert_eq!(source.ir_path.as_ref(), "/body/0");
}

// ─── HumanInboxEntry ────────────────────────────────────────────────────────

#[test]
fn inbox_pairs_and_keeps_supervision_suspend_pending() {
    let mut temp = TempStore::new("inbox");
    // Run 1: the scripted golden run — its request is answered.
    let golden_run = scripted_run(&mut temp.store);
    assert_eq!(
        projection::run_inbox(&temp.store, &golden_run)
            .expect("run inbox")
            .len(),
        0
    );

    // Run 2: a pending step request + a supervision gate whose only
    // answer is a (non-final) suspend.
    let run_id = temp
        .store
        .begin_run(NewRun {
            run_id: Some("run-pending".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 2_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];
    temp.store
        .append_event(
            &run_id,
            2_010,
            &root,
            &RunLogPayload::RunStarted {
                ir_hash: hash('a'),
                lockfile_digest: hash('b'),
                params_snapshot: json!({}),
                supervise_policy: Some(pointlock_ir::SupervisePolicy::Mutating),
            },
        )
        .expect("start");
    let gate = step_path("guarded");
    temp.store
        .append_event(
            &run_id,
            2_020,
            &gate,
            &RunLogPayload::StepEntered {
                step_id: "guarded".try_into().expect("step id"),
                effect_hash: hash('c'),
                judge_hash: hash('d'),
                resolved_inputs: Value::Null,
            },
        )
        .expect("enter");
    temp.store
        .append_event(
            &run_id,
            2_030,
            &gate,
            &RunLogPayload::HumanRequested {
                request_id: "req-gate".to_owned(),
                purpose: HumanPurpose::Supervision,
                mode: None,
                prompt: "about to tap a mutating control".to_owned(),
                presents: json!([]),
                decisions: None,
                output_schema: None,
                deadline_at_ms: None,
            },
        )
        .expect("request");
    temp.store
        .append_event(
            &run_id,
            2_040,
            &gate,
            &RunLogPayload::HumanResponded {
                request_id: "req-gate".to_owned(),
                purpose: HumanPurpose::Supervision,
                response: json!({ "decision": "suspend" }),
                actor: "cli:tester@host".to_owned(),
            },
        )
        .expect("suspend answer");

    let inbox = projection::human_inbox(&temp.store).expect("cross-run inbox");
    assert_eq!(
        inbox.len(),
        1,
        "suspend is non-final: the gate stays pending"
    );
    let entry = &inbox[0];
    assert_eq!(entry.run_id, run_id);
    assert_eq!(entry.purpose, "supervision");
    assert_eq!(entry.mode, None);
    assert_eq!(entry.deadline_at_ms, None);

    golden("human-inbox-entry.golden.json", entry);
}

// ─── FlowGraphView ──────────────────────────────────────────────────────────

#[test]
fn graph_projects_the_wifi_toggle_fixture() {
    let flow = load_fixture_flow("wifi_toggle.json");
    let graph = projection::flow_graph_view(&flow);

    assert_eq!(graph.flow_id, "wifi_toggle");
    // 7 top-level steps + 1 nested then-step.
    assert_eq!(graph.nodes.len(), 8);
    let seq = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == projection::GraphEdgeKind::Seq)
        .count();
    assert_eq!(seq, 6, "6 sibling links between 7 top-level steps");
    let branch: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == projection::GraphEdgeKind::Branch)
        .collect();
    assert_eq!(branch.len(), 1);
    assert_eq!(branch[0].label.as_deref(), Some("then"));
    assert_eq!(graph.flow_hooks.len(), 1);
    assert_eq!(graph.flow_hooks[0].hook, "onUnknown");
    assert_eq!(graph.flow_hooks[0].disposition, "escalate");

    let nested = graph
        .nodes
        .iter()
        .find(|node| node.id == "wait_connected")
        .expect("nested node");
    assert_eq!(nested.parent_id.as_deref(), Some("wait_if_passed"));
    assert_eq!(nested.region, Some(projection::NodeRegion::Then));
    assert!(nested.run_path.contains("wait_if_passed/wait_connected"));

    let call = graph
        .nodes
        .iter()
        .find(|node| node.id == "ensure_session")
        .expect("call node");
    match &call.body {
        projection::GraphNodeBody::Call { callee_flow_id, .. } => {
            assert_eq!(callee_flow_id, "ensure_logged_in");
        }
        other => panic!("expected call body, got {other:?}"),
    }
    // The call anchor mirrors the runner's Call frame, so overlay keys
    // and locate deep links join (07 §2.1 rendering).
    assert!(
        call.run_path
            .contains("/ensure_session/call→ensure_logged_in@"),
        "call anchor: {}",
        call.run_path
    );

    golden("flow-graph-view.golden.json", &graph);
}

#[test]
fn graph_projects_foreach_and_step_hooks() {
    let foreach = projection::flow_graph_view(&load_fixture_flow("minimal_foreach.json"));
    let body_child = foreach
        .nodes
        .iter()
        .find(|node| node.region == Some(projection::NodeRegion::Body))
        .expect("foreach body child");
    assert!(body_child.parent_id.is_some());

    let hooks = projection::flow_graph_view(&load_fixture_flow("step_level_handlers.json"));
    let hook_edges: Vec<_> = hooks
        .edges
        .iter()
        .filter(|edge| edge.kind == projection::GraphEdgeKind::Hook)
        .collect();
    assert!(
        !hook_edges.is_empty(),
        "step-level handlers become hook edges"
    );
    for edge in hook_edges {
        assert!(edge.to.is_none(), "hook edges anchor at the host only");
        assert!(edge.hook.is_some(), "hook edges carry the badge payload");
    }
}

// ─── Schemas ────────────────────────────────────────────────────────────────

#[test]
fn projection_schemas_satisfy_the_meta_schema_and_accept_fixtures() {
    for (family, schema) in projection::projection_schemas() {
        jsonschema::meta::validate(&schema)
            .unwrap_or_else(|err| panic!("{} schema invalid: {err}", family.stem()));
    }

    // Each golden fixture validates against its family schema and
    // round-trips through the Rust DTO to the identical Value.
    let dir = repo_schema_dir().join("fixtures/projection");
    let cases: [(ProjectionSchemaFamily, &str); 5] = [
        (
            ProjectionSchemaFamily::FlowGraph,
            "flow-graph-view.golden.json",
        ),
        (
            ProjectionSchemaFamily::RunTimeline,
            "run-timeline.golden.json",
        ),
        (
            ProjectionSchemaFamily::StepDossier,
            "step-dossier-view.golden.json",
        ),
        (
            ProjectionSchemaFamily::HumanInbox,
            "human-inbox-entry.golden.json",
        ),
        (
            ProjectionSchemaFamily::RunOverview,
            "run-overview.golden.json",
        ),
    ];
    for (family, fixture) in cases {
        let path = dir.join(fixture);
        let raw = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "missing {}; run with POINTLOCK_BLESS_PROJECTION=1 first",
                path.display()
            )
        });
        let value: Value = serde_json::from_str(&raw).expect("fixture parses");
        let schema = projection::projection_schema(family);
        let compiled = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(&schema)
            .expect("schema compiles");
        assert!(
            compiled.is_valid(&value),
            "{fixture} does not validate against the {} schema",
            family.stem()
        );

        let round_trip: Value = match family {
            ProjectionSchemaFamily::FlowGraph => {
                let typed: projection::FlowGraphView =
                    serde_json::from_value(value.clone()).expect("typed parse");
                serde_json::to_value(&typed).expect("re-serialize")
            }
            ProjectionSchemaFamily::RunTimeline => {
                let typed: projection::TimelinePage =
                    serde_json::from_value(value.clone()).expect("typed parse");
                serde_json::to_value(&typed).expect("re-serialize")
            }
            ProjectionSchemaFamily::StepDossier => {
                let typed: projection::StepDossierView =
                    serde_json::from_value(value.clone()).expect("typed parse");
                serde_json::to_value(&typed).expect("re-serialize")
            }
            ProjectionSchemaFamily::HumanInbox => {
                let typed: projection::HumanInboxEntry =
                    serde_json::from_value(value.clone()).expect("typed parse");
                serde_json::to_value(&typed).expect("re-serialize")
            }
            ProjectionSchemaFamily::RunOverview => {
                let typed: projection::RunOverview =
                    serde_json::from_value(value.clone()).expect("typed parse");
                serde_json::to_value(&typed).expect("re-serialize")
            }
        };
        assert_eq!(round_trip, value, "{fixture} round-trip identity");
    }
}

// ─── Call-step instances (runner anchors calls at the Call frame) ───────────

/// The runner enters a call step at `PathFrame::Call { step_id: Some }`
/// with no separate `Step` frame (07 §2.1). Scripts: suspend/resume,
/// preflight, a hook trigger, a failed attempt with a taxonomy-coded
/// error, a call step with one inner callee step, and a re-judgement.
fn scripted_call_run(store: &mut Store) -> String {
    let run_id = store
        .begin_run(NewRun {
            run_id: Some("run-call".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 3_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];

    let mut at = 3_000u64;
    let mut append = |store: &mut Store, path: &RunPath, payload: RunLogPayload| {
        at += 10;
        store
            .append_event(&run_id, at, path, &payload)
            .expect("append event");
    };

    append(
        store,
        &root,
        RunLogPayload::RunStarted {
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            supervise_policy: None,
        },
    );

    // step_x: preflight + a failed attempt (taxonomy-coded) + a hook
    // trigger + first verdict, later superseded by a re-judgement.
    let x = step_path("step_x");
    append(
        store,
        &x,
        RunLogPayload::StepEntered {
            step_id: "step_x".try_into().expect("step id"),
            effect_hash: hash('c'),
            judge_hash: hash('d'),
            resolved_inputs: Value::Null,
        },
    );
    append(
        store,
        &x,
        RunLogPayload::PreflightProbed {
            outcomes: vec![AssertionOutcomeRecord {
                assert_id: "world_intact".try_into().expect("assert id"),
                result: VerdictStatus::Pass,
                channel: Some(Channel::Dom),
                reason: "probe pass".to_owned(),
            }],
        },
    );
    append(
        store,
        &attempt_path("step_x", 1),
        RunLogPayload::ActionIntent {
            call_id: "call-x1".to_owned(),
            args_snapshot: json!({}),
            chain_index: None,
            channel: None,
            action_name: None,
        },
    );
    append(
        store,
        &attempt_path("step_x", 1),
        RunLogPayload::ActionSettled {
            call_id: "call-x1".to_owned(),
            outcome: pointlock_ir::ActionOutcome::Failed {
                error: pointlock_ir::ErrorInfo {
                    code: "action_failed_final".to_owned(),
                    message: "element vanished".to_owned(),
                    retryable: false,
                    details: None,
                },
            },
        },
    );
    append(
        store,
        &x,
        RunLogPayload::HandlerTriggered {
            hook: pointlock_ir::HandlerHook::OnFail,
            trigger: 1,
            disposition: Some("escalate".to_owned()),
        },
    );
    append(
        store,
        &x,
        RunLogPayload::VerdictRecorded {
            verdict: Verdict {
                status: VerdictStatus::Fail,
                degraded: false,
                summary: "attempt failed".to_owned(),
                evidence: Vec::new(),
                supersedes: None,
            },
            localized: Vec::new(),
            localization_gaps: Vec::new(),
            remote_archival_error: None,
        },
    );
    append(
        store,
        &x,
        RunLogPayload::StepExited {
            provider_state_summary: None,
            state: StepState::Judged,
            output: None,
            localized: Vec::new(),
            localization_gaps: Vec::new(),
        },
    );

    // Suspension divider + resume with an empty alignment report.
    append(
        store,
        &root,
        RunLogPayload::RunSuspended {
            provider_state_summary: None,
            reason: None,
        },
    );
    append(
        store,
        &root,
        RunLogPayload::RunResumed {
            alignment_report: pointlock_ir::AlignmentReport {
                entries: vec![pointlock_ir::AlignmentEntry {
                    run_path: root.clone(),
                    step_id: "step_x".try_into().expect("step id"),
                    class: pointlock_ir::AlignmentClass::Reusable,
                    reason: None,
                }],
                resume_point: None,
                requires_confirmation: Vec::new(),
            },
            supervise_policy: None,
            event_cursor: None,
        },
    );

    // Re-judgement of step_x AFTER its exit (offline re-judgement path).
    append(
        store,
        &x,
        RunLogPayload::VerdictRecorded {
            verdict: Verdict {
                status: VerdictStatus::Pass,
                degraded: false,
                summary: "re-judged offline".to_owned(),
                evidence: Vec::new(),
                supersedes: Some("verdict-1".to_owned()),
            },
            localized: Vec::new(),
            localization_gaps: Vec::new(),
            remote_archival_error: None,
        },
    );

    // ensure: a call step anchored at the Call frame, one inner step.
    let call = vec![
        flow_frame(),
        PathFrame::Call {
            step_id: Some("ensure".try_into().expect("step id")),
            callee_flow_id: "callee".try_into().expect("flow id"),
            callee_ir_hash: hash('9'),
        },
    ];
    append(
        store,
        &call,
        RunLogPayload::StepEntered {
            step_id: "ensure".try_into().expect("step id"),
            effect_hash: hash('c'),
            judge_hash: hash('d'),
            resolved_inputs: json!({ "user": "amy" }),
        },
    );
    append(
        store,
        &call,
        RunLogPayload::CallFramePushed {
            frame: pointlock_ir::CallFrame {
                flow_id: "callee".try_into().expect("flow id"),
                ir_hash: hash('9'),
                call_step_id: Some("ensure".try_into().expect("step id")),
                inputs_snapshot: json!({ "user": "amy" }),
                vars: std::collections::BTreeMap::new(),
                iter_stack: Vec::new(),
                next_index: 0,
            },
            rebase: false,
        },
    );
    let mut inner = call.clone();
    inner.push(PathFrame::Step {
        step_id: "inner".try_into().expect("step id"),
    });
    append(
        store,
        &inner,
        RunLogPayload::StepEntered {
            step_id: "inner".try_into().expect("step id"),
            effect_hash: hash('e'),
            judge_hash: hash('f'),
            resolved_inputs: Value::Null,
        },
    );
    append(
        store,
        &inner,
        RunLogPayload::StepExited {
            provider_state_summary: None,
            state: StepState::Judged,
            output: None,
            localized: Vec::new(),
            localization_gaps: Vec::new(),
        },
    );
    append(
        store,
        &call,
        RunLogPayload::CallFramePopped {
            outputs: Some(json!({ "ok": true })),
        },
    );
    append(
        store,
        &call,
        RunLogPayload::VerdictRecorded {
            verdict: pass_verdict("callee flow verdict pass"),
            localized: Vec::new(),
            localization_gaps: Vec::new(),
            remote_archival_error: None,
        },
    );
    append(
        store,
        &call,
        RunLogPayload::StepExited {
            provider_state_summary: None,
            state: StepState::Judged,
            output: Some(json!({ "ok": true })),
            localized: Vec::new(),
            localization_gaps: Vec::new(),
        },
    );

    append(
        store,
        &root,
        RunLogPayload::RunFinished {
            verdict: Some(pass_verdict("flow verdict pass")),
            remote_archival_error: None,
        },
    );
    run_id
}

#[test]
fn call_steps_locate_and_dossier_at_the_call_frame() {
    let mut temp = TempStore::new("call");
    let run_id = scripted_call_run(&mut temp.store);

    // Bare step id resolves a call-step instance.
    let path = projection::locate_step(&temp.store, &run_id, "ensure").expect("locate call step");
    let dossier = projection::step_dossier(&temp.store, &run_id, &path, &[]).expect("dossier");
    assert_eq!(dossier.step_id, "ensure");
    assert!(dossier.run_path.contains("/ensure/call→callee@"));
    assert_eq!(
        dossier.verdict.as_ref().map(|v| v.status),
        Some(VerdictStatus::Pass),
        "call verdict = callee flow verdict"
    );
    assert_eq!(dossier.output, Some(json!({ "ok": true })));
    // The call step's environment is its HOST frame (identity-walked).
    let frame = dossier.frame.expect("host frame environment");
    assert_eq!(frame.inputs_snapshot, json!({}));

    // Canonical-string round trip through the call segment.
    let by_path =
        projection::locate_step(&temp.store, &run_id, &dossier.run_path).expect("by path");
    assert_eq!(render_run_path(&by_path), dossier.run_path);

    // JSON PathFrame[] input (07 §2.3 structured authority).
    let frames_json = serde_json::to_string(&dossier.run_path_frames).expect("serialize frames");
    let by_frames =
        projection::locate_step(&temp.store, &run_id, &frames_json).expect("by JSON frames");
    assert_eq!(render_run_path(&by_frames), dossier.run_path);

    // The inner callee step: locatable, and its frame environment is
    // ABSENT after the call popped (fail-closed identity walk — the live
    // stack no longer carries the callee frame).
    let inner = projection::locate_step(&temp.store, &run_id, "inner").expect("locate inner");
    let inner_dossier =
        projection::step_dossier(&temp.store, &run_id, &inner, &[]).expect("inner dossier");
    assert_eq!(inner_dossier.step_id, "inner");
    assert!(
        inner_dossier.frame.is_none(),
        "popped frame never mis-joins"
    );
}

#[test]
fn call_run_projects_the_remaining_event_kinds() {
    let mut temp = TempStore::new("call-timeline");
    let run_id = scripted_call_run(&mut temp.store);

    let all = projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::All, 1, 50)
        .expect("all");
    let kinds: Vec<&str> = all
        .entries
        .iter()
        .map(|entry| match &entry.detail {
            projection::TimelineDetail::PreflightProbed { .. } => "preflightProbed",
            projection::TimelineDetail::HandlerTriggered { .. } => "handlerTriggered",
            projection::TimelineDetail::RunSuspended { .. } => "runSuspended",
            projection::TimelineDetail::RunResumed { .. } => "runResumed",
            projection::TimelineDetail::CallFramePushed { .. } => "callFramePushed",
            projection::TimelineDetail::CallFramePopped { .. } => "callFramePopped",
            _ => "",
        })
        .filter(|kind| !kind.is_empty())
        .collect();
    for expected in [
        "preflightProbed",
        "handlerTriggered",
        "runSuspended",
        "runResumed",
        "callFramePushed",
        "callFramePopped",
    ] {
        assert!(kinds.contains(&expected), "missing {expected} in {kinds:?}");
    }

    // The failed settlement carries the joined ErrorClass (08 §4.2) and
    // enters the errors filter; the onFail handler trigger does NOT
    // (only onError does).
    let errors = projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::Errors, 1, 50)
        .expect("errors");
    assert_eq!(errors.total, 1);
    match &errors.entries[0].detail {
        projection::TimelineDetail::ActionSettled { error, .. } => {
            let error = error.as_ref().expect("error view");
            assert_eq!(error.error_class.as_deref(), Some("action_failed_final"));
            assert_eq!(error.code, "action_failed_final");
            assert!(!error.retryable);
        }
        other => panic!("unexpected errors entry {other:?}"),
    }

    // Re-judgement lineage: two verdicts for step_x, the later carrying
    // supersedes; the dossier exposes the full history.
    let x = projection::locate_step(&temp.store, &run_id, "step_x").expect("locate step_x");
    let dossier = projection::step_dossier(&temp.store, &run_id, &x, &[]).expect("dossier");
    assert_eq!(dossier.verdict_history.len(), 2);
    assert_eq!(
        dossier.verdict.as_ref().and_then(|v| v.supersedes.clone()),
        Some("verdict-1".to_owned())
    );
    assert_eq!(
        dossier.verdict.as_ref().map(|v| v.status),
        Some(VerdictStatus::Pass)
    );
    assert_eq!(dossier.preflight.len(), 1);
    assert_eq!(dossier.handler_triggers.len(), 1);
    assert_eq!(
        dossier.attempts[0].error.as_ref().map(|e| e.code.as_str()),
        Some("action_failed_final")
    );
    assert_eq!(
        dossier.attempts[0].error_class.as_deref(),
        Some("action_failed_final")
    );

    // Overview: the overlay reflects the re-judged verdict and the graph
    // key carries the call segment (anchor join, 08 §3.2).
    let overview = projection::run_overview(&temp.store, &run_id).expect("overview");
    let x_key = render_run_path(&step_path("step_x"));
    assert_eq!(
        overview.steps[&x_key].verdict_status.as_deref(),
        Some("pass"),
        "overlay shows the superseding verdict"
    );
    assert!(
        overview
            .steps
            .keys()
            .any(|key| key.contains("/ensure/call→callee@")),
        "call instance key carries the call segment: {:?}",
        overview.steps.keys().collect::<Vec<_>>()
    );
    let alignment = overview.alignment.expect("alignment summary");
    assert_eq!(alignment.reusable, 1);
}

// ─── Timed-out settlement clears pending state without a response ───────────

#[test]
fn terminal_step_exit_settles_pending_requests() {
    let mut temp = TempStore::new("timeout-settle");
    let run_id = temp
        .store
        .begin_run(NewRun {
            run_id: Some("run-timeout".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 4_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];
    let gate = step_path("ask");
    let events: Vec<(RunPath, RunLogPayload)> = vec![
        (
            root.clone(),
            RunLogPayload::RunStarted {
                ir_hash: hash('a'),
                lockfile_digest: hash('b'),
                params_snapshot: json!({}),
                supervise_policy: None,
            },
        ),
        (
            gate.clone(),
            RunLogPayload::StepEntered {
                step_id: "ask".try_into().expect("step id"),
                effect_hash: hash('c'),
                judge_hash: hash('d'),
                resolved_inputs: Value::Null,
            },
        ),
        (
            gate.clone(),
            RunLogPayload::HumanRequested {
                request_id: "req-t".to_owned(),
                purpose: HumanPurpose::Step,
                mode: Some(HumanMode::Confirm),
                prompt: "confirm?".to_owned(),
                presents: json!([]),
                decisions: Some(vec!["yes".to_owned(), "no".to_owned()]),
                output_schema: None,
                deadline_at_ms: Some(4_050),
            },
        ),
        // Lazy timeout settlement: NO humanResponded — verdict unknown +
        // terminal exit settle the request (06 §5.3).
        (
            gate.clone(),
            RunLogPayload::VerdictRecorded {
                verdict: Verdict {
                    status: VerdictStatus::Unknown,
                    degraded: false,
                    summary: "timed out".to_owned(),
                    evidence: Vec::new(),
                    supersedes: None,
                },
                localized: Vec::new(),
                localization_gaps: Vec::new(),
                remote_archival_error: None,
            },
        ),
        (
            gate.clone(),
            RunLogPayload::StepExited {
                provider_state_summary: None,
                state: StepState::Judged,
                output: None,
                localized: Vec::new(),
                localization_gaps: Vec::new(),
            },
        ),
        (
            root.clone(),
            RunLogPayload::RunFinished {
                verdict: None,
                remote_archival_error: None,
            },
        ),
    ];
    let mut at = 4_000u64;
    for (path, payload) in &events {
        at += 10;
        temp.store
            .append_event(&run_id, at, path, payload)
            .expect("append");
    }

    assert_eq!(
        projection::run_inbox(&temp.store, &run_id)
            .expect("inbox")
            .len(),
        0,
        "terminal exit settles the request without a response"
    );
    let overview = projection::run_overview(&temp.store, &run_id).expect("overview");
    assert!(!overview.awaiting_human);
    assert_eq!(
        overview.steps[&render_run_path(&gate)]
            .verdict_status
            .as_deref(),
        Some("unknown")
    );
}

// ─── Bounded rendering: depth, over-limit placeholder, evidence cap ─────────

#[test]
fn timeline_bounds_depth_overlimit_and_evidence() {
    let mut temp = TempStore::new("hard-bounds");
    let run_id = temp
        .store
        .begin_run(NewRun {
            run_id: Some("run-hard-bounds".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 5_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];
    temp.store
        .append_event(
            &run_id,
            5_010,
            &root,
            &RunLogPayload::RunStarted {
                ir_hash: hash('a'),
                lockfile_digest: hash('b'),
                params_snapshot: json!({}),
                supervise_policy: None,
            },
        )
        .expect("start");
    let a = step_path("step_a");
    temp.store
        .append_event(
            &run_id,
            5_020,
            &a,
            &RunLogPayload::StepEntered {
                step_id: "step_a".try_into().expect("step id"),
                effect_hash: hash('c'),
                judge_hash: hash('d'),
                resolved_inputs: Value::Null,
            },
        )
        .expect("enter");

    // Depth 20 nesting → depth-12 marker, truncated flag.
    let mut deep = json!("leaf");
    for _ in 0..20 {
        deep = json!({ "next": deep });
    }
    temp.store
        .append_event(
            &run_id,
            5_030,
            &attempt_path("step_a", 1),
            &RunLogPayload::ActionIntent {
                call_id: "call-deep".to_owned(),
                args_snapshot: deep,
                chain_index: None,
                channel: None,
                action_name: None,
            },
        )
        .expect("intent");

    // An observation id far over the JSON cap → the fail-closed
    // OverLimit placeholder (no half entries, 08 §4.4).
    temp.store
        .append_event(
            &run_id,
            5_040,
            &a,
            &RunLogPayload::ObservationRecorded {
                observation: ObservationRecord {
                    viewport: None,
                    observation_id: "x".repeat(20_000),
                    captured_at_ms: 5_040,
                    screenshot: None,
                    screenshot_omission: None,
                    ui_snapshot: None,
                    ui_snapshot_omission: None,
                },
            },
        )
        .expect("observation");

    // 40 verdict evidence refs → 32 kept + 8 omitted.
    let assets: Vec<AssetRef> = (0..40)
        .map(|i| AssetRef {
            id: format!("asset-{i}"),
            media_type: "image/png".to_owned(),
            uri: format!("devicerail://assets/{i}"),
            sha256: Some("a".repeat(64)),
        })
        .collect();
    temp.store
        .append_event(
            &run_id,
            5_050,
            &a,
            &RunLogPayload::VerdictRecorded {
                verdict: Verdict {
                    status: VerdictStatus::Pass,
                    degraded: false,
                    summary: "evidence heavy".to_owned(),
                    evidence: assets,
                    supersedes: None,
                },
                localized: Vec::new(),
                localization_gaps: Vec::new(),
                remote_archival_error: None,
            },
        )
        .expect("verdict");

    let all = projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::All, 1, 50)
        .expect("page");

    let intent = all
        .entries
        .iter()
        .find(|entry| {
            matches!(
                entry.detail,
                projection::TimelineDetail::ActionIntent { .. }
            )
        })
        .expect("intent entry");
    assert!(intent.truncated, "depth bound flags truncation");
    match &intent.detail {
        projection::TimelineDetail::ActionIntent { args, .. } => {
            assert!(args.truncated);
            assert!(
                serde_json::to_string(&args.value)
                    .expect("serialize")
                    .contains("depth truncated")
            );
        }
        other => panic!("unexpected {other:?}"),
    }

    let observation = all
        .entries
        .iter()
        .find(|entry| entry.seq == 4)
        .expect("observation entry");
    match &observation.detail {
        projection::TimelineDetail::OverLimit { event_type } => {
            assert_eq!(event_type, "observationRecorded");
        }
        other => panic!("expected the OverLimit placeholder, got {other:?}"),
    }
    assert!(observation.truncated);

    let verdict = all
        .entries
        .iter()
        .find(|entry| {
            matches!(
                entry.detail,
                projection::TimelineDetail::VerdictRecorded { .. }
            )
        })
        .expect("verdict entry");
    assert_eq!(verdict.evidence.len(), 32, "hard evidence cap");
    assert_eq!(verdict.evidence_omitted, 8);
}

// ─── Pagination beyond page one ─────────────────────────────────────────────

#[test]
fn timeline_pagination_offsets_and_normalizes() {
    let mut temp = TempStore::new("pages");
    let run_id = scripted_run(&mut temp.store);

    let second = projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::All, 2, 5)
        .expect("page 2");
    assert_eq!(second.entries.len(), 5);
    assert_eq!(second.entries[0].seq, 6, "page 2 starts after 5 entries");
    assert_eq!(second.total, 14);

    let past = projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::All, 9, 5)
        .expect("page past end");
    assert!(past.entries.is_empty());
    assert_eq!(past.total, 14);

    let zero = projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::All, 0, 5)
        .expect("page 0");
    assert_eq!(zero.page, 1, "page 0 normalizes to 1");
    assert_eq!(zero.entries[0].seq, 1);
}

// ─── foreach iterations: instance keys + ambiguity ──────────────────────────

#[test]
fn foreach_iterations_key_instances_and_flag_ambiguity() {
    let mut temp = TempStore::new("foreach");
    let run_id = temp
        .store
        .begin_run(NewRun {
            run_id: Some("run-foreach".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 6_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];
    let mut at = 6_000u64;
    let mut append = |store: &mut Store, path: &RunPath, payload: RunLogPayload| {
        at += 10;
        store
            .append_event(&run_id, at, path, &payload)
            .expect("append event");
    };
    append(
        &mut temp.store,
        &root,
        RunLogPayload::RunStarted {
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            supervise_policy: None,
        },
    );
    // Two iterations of `each[i]/tap_it`.
    for index in 0..2u64 {
        let mut path = step_path("each");
        path.push(PathFrame::Iteration { index, key: None });
        path.push(PathFrame::Step {
            step_id: "tap_it".try_into().expect("step id"),
        });
        append(
            &mut temp.store,
            &path,
            RunLogPayload::StepEntered {
                step_id: "tap_it".try_into().expect("step id"),
                effect_hash: hash('c'),
                judge_hash: hash('d'),
                resolved_inputs: json!({ "round": index }),
            },
        );
        append(
            &mut temp.store,
            &path,
            RunLogPayload::VerdictRecorded {
                verdict: if index == 0 {
                    pass_verdict("round 0 pass")
                } else {
                    Verdict {
                        status: VerdictStatus::Fail,
                        degraded: false,
                        summary: "round 1 fail".to_owned(),
                        evidence: Vec::new(),
                        supersedes: None,
                    }
                },
                localized: Vec::new(),
                localization_gaps: Vec::new(),
                remote_archival_error: None,
            },
        );
        append(
            &mut temp.store,
            &path,
            RunLogPayload::StepExited {
                provider_state_summary: None,
                state: StepState::Judged,
                output: None,
                localized: Vec::new(),
                localization_gaps: Vec::new(),
            },
        );
    }

    // Bare id over two iteration instances → typed ambiguity with both
    // canonical candidates.
    let ambiguous = projection::locate_step(&temp.store, &run_id, "tap_it");
    match ambiguous {
        Err(pointlock_store::StoreError::AmbiguousStep { candidates, .. }) => {
            assert_eq!(candidates.len(), 2);
            assert!(candidates[0].contains("each[0]/tap_it"), "{candidates:?}");
            assert!(candidates[1].contains("each[1]/tap_it"), "{candidates:?}");
        }
        other => panic!("expected AmbiguousStep, got {other:?}"),
    }

    // A canonical iteration path resolves exactly one instance.
    let round1 = projection::locate_step(
        &temp.store,
        &run_id,
        &format!("demo@{}/each[1]/tap_it", "a".repeat(8)),
    )
    .expect("locate round 1");
    let dossier = projection::step_dossier(&temp.store, &run_id, &round1, &[]).expect("dossier");
    assert_eq!(dossier.resolved_inputs, json!({ "round": 1 }));
    assert_eq!(
        dossier.verdict.as_ref().map(|v| v.status),
        Some(VerdictStatus::Fail)
    );

    // The overlay keys the two instances separately (aggregation is the
    // renderer's join, 08 §3.2).
    let overview = projection::run_overview(&temp.store, &run_id).expect("overview");
    let keys: Vec<&String> = overview.steps.keys().collect();
    assert!(keys.iter().any(|key| key.contains("each[0]/tap_it")));
    assert!(keys.iter().any(|key| key.contains("each[1]/tap_it")));
}

// ─── if/else graph projection ───────────────────────────────────────────────

#[test]
fn graph_projects_both_if_branches() {
    let graph = projection::flow_graph_view(&load_fixture_flow("minimal_if.json"));
    let branch_labels: Vec<&str> = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == projection::GraphEdgeKind::Branch)
        .filter_map(|edge| edge.label.as_deref())
        .collect();
    assert!(branch_labels.contains(&"then"), "{branch_labels:?}");
    assert!(branch_labels.contains(&"else"), "{branch_labels:?}");
    assert!(
        graph
            .nodes
            .iter()
            .any(|node| node.region == Some(projection::NodeRegion::Else)),
        "else-region node present"
    );
}

// ─── list_runs ──────────────────────────────────────────────────────────────

#[test]
fn list_runs_orders_by_creation() {
    let mut temp = TempStore::new("list-runs");
    let first = scripted_run(&mut temp.store);
    let second = temp
        .store
        .begin_run(NewRun {
            run_id: Some("run-later".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 9_000,
        })
        .expect("begin run");
    let runs = temp.store.list_runs().expect("list");
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].run_id, first);
    assert_eq!(runs[1].run_id, second);
    assert_eq!(runs[0].status, pointlock_store::RunStatus::Finished);
    assert_eq!(runs[1].status, pointlock_store::RunStatus::Running);
}

// ─── RunOverview last-suspension provider profile (07 §2.2) ─────────────────

fn sample_summary() -> pointlock_ir::ProviderStateSummary {
    pointlock_ir::ProviderStateSummary {
        session_lineage: vec!["session-1".to_owned()],
        event_cursor: Some(EventCursor {
            session_id: "session-1".to_owned(),
            last_sequence: 7,
        }),
        attestation: pointlock_ir::AttestationSnapshot {
            lockfile_digest: hash('b'),
            attested_at: "1970-01-01T00:00:01Z".to_owned(),
        },
        health: pointlock_ir::SessionHealthSnapshot {
            ok: true,
            degraded: None,
        },
        device_id: "fake-device-1".to_owned(),
        platform: Some("android".to_owned()),
    }
}

/// The overview surfaces the most recent suspension's profile only while
/// the run is actually suspended; a resume supersedes it.
#[test]
fn overview_gates_the_suspension_summary_on_the_live_status() {
    let mut fixture = TempStore::new("overview-summary");
    let run_id = fixture
        .store
        .begin_run(NewRun {
            run_id: Some("run-osum".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 1_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];
    fixture
        .store
        .append_event(
            &run_id,
            1_010,
            &root,
            &RunLogPayload::RunStarted {
                ir_hash: hash('a'),
                lockfile_digest: hash('b'),
                params_snapshot: json!({}),
                supervise_policy: None,
            },
        )
        .expect("runStarted");
    fixture
        .store
        .append_event(
            &run_id,
            1_020,
            &root,
            &RunLogPayload::RunSuspended {
                provider_state_summary: Some(sample_summary()),
                reason: Some("stop token".to_owned()),
            },
        )
        .expect("runSuspended");

    let overview =
        pointlock_store::projection::run_overview(&fixture.store, &run_id).expect("overview");
    assert_eq!(overview.status, "suspended");
    let summary = overview
        .last_suspension_provider_state_summary
        .as_ref()
        .expect("suspended run surfaces the profile");
    assert_eq!(summary.device_id, "fake-device-1");

    fixture
        .store
        .append_event(
            &run_id,
            1_030,
            &root,
            &RunLogPayload::RunResumed {
                alignment_report: pointlock_ir::AlignmentReport {
                    entries: vec![],
                    resume_point: None,
                    requires_confirmation: vec![],
                },
                supervise_policy: None,
                event_cursor: None,
            },
        )
        .expect("runResumed");
    let overview =
        pointlock_store::projection::run_overview(&fixture.store, &run_id).expect("overview");
    assert!(
        overview.last_suspension_provider_state_summary.is_none(),
        "a resumed run must not read a stale suspension profile"
    );
}

/// The awaitingHuman arm of the status gate also surfaces the profile,
/// and a second suspension supersedes the first (last wins).
#[test]
fn overview_summary_awaiting_human_and_last_suspension_wins() {
    let mut fixture = TempStore::new("overview-summary-2");
    let run_id = fixture
        .store
        .begin_run(NewRun {
            run_id: Some("run-osum2".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 1_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];
    fixture
        .store
        .append_event(
            &run_id,
            1_010,
            &root,
            &RunLogPayload::RunStarted {
                ir_hash: hash('a'),
                lockfile_digest: hash('b'),
                params_snapshot: json!({}),
                supervise_policy: None,
            },
        )
        .expect("runStarted");
    fixture
        .store
        .append_event(
            &run_id,
            1_020,
            &step_path("confirm"),
            &RunLogPayload::HumanRequested {
                request_id: "req-1".to_owned(),
                purpose: pointlock_ir::HumanPurpose::Step,
                mode: Some(pointlock_ir::HumanMode::Confirm),
                prompt: "confirm".to_owned(),
                presents: json!([]),
                decisions: Some(vec!["confirmed".to_owned()]),
                output_schema: None,
                deadline_at_ms: None,
            },
        )
        .expect("humanRequested");
    let mut first = sample_summary();
    first.device_id = "dev-first".to_owned();
    fixture
        .store
        .append_event(
            &run_id,
            1_030,
            &root,
            &RunLogPayload::RunSuspended {
                provider_state_summary: Some(first),
                reason: Some("awaiting human".to_owned()),
            },
        )
        .expect("first suspension");
    let overview =
        pointlock_store::projection::run_overview(&fixture.store, &run_id).expect("overview");
    assert_eq!(overview.status, "awaitingHuman");
    assert_eq!(
        overview
            .last_suspension_provider_state_summary
            .as_ref()
            .expect("awaitingHuman surfaces the profile")
            .device_id,
        "dev-first"
    );
}

/// Display lineage extends only on cursor-bearing resumes; cursor-less
/// (pre-incorporation) resumes extend nothing (07 §4.5).
#[test]
fn overview_lineage_extends_only_on_cursor_bearing_resumes() {
    let mut fixture = TempStore::new("overview-lineage");
    let run_id = fixture
        .store
        .begin_run(NewRun {
            run_id: Some("run-olin".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 1_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];
    let resumed = |cursor: Option<pointlock_ir::EventCursor>| RunLogPayload::RunResumed {
        alignment_report: pointlock_ir::AlignmentReport {
            entries: vec![],
            resume_point: None,
            requires_confirmation: vec![],
        },
        supervise_policy: None,
        event_cursor: cursor,
    };
    let mut at = 1_000u64;
    let mut append = |store: &mut pointlock_store::Store, payload: RunLogPayload| {
        at += 10;
        store
            .append_event(&run_id, at, &root, &payload)
            .expect("append");
    };
    append(
        &mut fixture.store,
        RunLogPayload::RunStarted {
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            supervise_policy: None,
        },
    );
    append(&mut fixture.store, resumed(None));
    append(
        &mut fixture.store,
        resumed(Some(pointlock_ir::EventCursor {
            session_id: "session-2".to_owned(),
            last_sequence: 0,
        })),
    );
    append(&mut fixture.store, resumed(None));
    let overview =
        pointlock_store::projection::run_overview(&fixture.store, &run_id).expect("overview");
    assert_eq!(
        overview.session_lineage,
        vec!["session-1".to_owned(), "session-2".to_owned()]
    );
}

/// The act-chain pass boundary is LAZY (Wave D review): a hook firing
/// alone never erases the latest pass's marks — only the next pass's
/// first intent does. A continue-style disposition therefore keeps its
/// settled marks; a retry replaces them with the new pass's.
#[test]
fn act_chain_marks_survive_hooks_until_a_new_pass_starts() {
    let mut fixture = TempStore::new("marks-boundary");
    let run_id = fixture
        .store
        .begin_run(NewRun {
            run_id: Some("run-marks".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 1_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];
    let mut at = 1_000u64;
    let mut append =
        |store: &mut pointlock_store::Store, path: &RunPath, payload: RunLogPayload| {
            at += 10;
            store
                .append_event(&run_id, at, path, &payload)
                .expect("append");
        };
    let intent = |call: &str, index: u32| RunLogPayload::ActionIntent {
        call_id: call.to_owned(),
        args_snapshot: json!({}),
        chain_index: Some(index),
        channel: Some(pointlock_ir::ActChannel::UiTree),
        action_name: Some("tapElement".try_into().expect("action name")),
    };
    let settled_failed = |call: &str| RunLogPayload::ActionSettled {
        call_id: call.to_owned(),
        outcome: pointlock_ir::ActionOutcome::Failed {
            error: pointlock_ir::ErrorInfo {
                code: "action_failed_final".to_owned(),
                message: "nope".to_owned(),
                retryable: false,
                details: None,
            },
        },
    };

    append(
        &mut fixture.store,
        &root,
        RunLogPayload::RunStarted {
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            supervise_policy: None,
        },
    );
    let step = step_path("step_a");
    append(
        &mut fixture.store,
        &step,
        RunLogPayload::StepEntered {
            step_id: "step_a".try_into().expect("step id"),
            effect_hash: hash('e'),
            judge_hash: hash('f'),
            resolved_inputs: json!({}),
        },
    );
    append(
        &mut fixture.store,
        &attempt_path("step_a", 1),
        intent("c-1", 1),
    );
    append(
        &mut fixture.store,
        &attempt_path("step_a", 1),
        settled_failed("c-1"),
    );
    append(
        &mut fixture.store,
        &step,
        RunLogPayload::HandlerTriggered {
            hook: pointlock_ir::HandlerHook::OnFail,
            trigger: 1,
            disposition: Some("escalate".to_owned()),
        },
    );

    // Continue-style: no new pass starts — the crossed@1 mark survives.
    let overview =
        pointlock_store::projection::run_overview(&fixture.store, &run_id).expect("overview");
    let marks = overview
        .steps
        .values()
        .find_map(|cell| cell.act_chain_marks.as_ref())
        .expect("marks survive a hook with no new pass");
    assert_eq!(
        (marks[0].chain_index, marks[0].mark.as_str()),
        (1, "crossed")
    );

    // A retry pass starts: the deferred boundary erases the old marks
    // and the new pass's settle owns the chip.
    append(
        &mut fixture.store,
        &attempt_path("step_a", 2),
        intent("c-2", 1),
    );
    append(
        &mut fixture.store,
        &attempt_path("step_a", 2),
        RunLogPayload::ActionSettled {
            call_id: "c-2".to_owned(),
            outcome: pointlock_ir::ActionOutcome::Succeeded {
                result: Box::new(pointlock_ir::ActionResult {
                    call_id: "c-2".to_owned(),
                    started_at_ms: 1,
                    finished_at_ms: 2,
                    output: json!({}),
                    before: None,
                    after: None,
                    evidence: Vec::new(),
                    execution: None,
                }),
            },
        },
    );
    let overview =
        pointlock_store::projection::run_overview(&fixture.store, &run_id).expect("overview");
    let marks = overview
        .steps
        .values()
        .find_map(|cell| cell.act_chain_marks.as_ref())
        .expect("the new pass's marks");
    assert_eq!(marks.len(), 1);
    assert_eq!(
        (marks[0].chain_index, marks[0].mark.as_str()),
        (1, "succeeded")
    );
}

#[test]
fn timeline_carries_the_remote_archival_annotation() {
    // 04 §5: a failed verdict write-back is annotated on the event; the
    // timeline mirrors it so the surface can say so.
    let mut temp = TempStore::new("archival");
    let run_id = temp
        .store
        .begin_run(NewRun {
            run_id: Some("run-archival".to_owned()),
            flow_id: "demo".try_into().expect("flow id"),
            ir_hash: hash('a'),
            lockfile_digest: hash('b'),
            params_snapshot: json!({}),
            binding: binding(),
            created_at_ms: 1_000,
        })
        .expect("begin run");
    let root = vec![flow_frame()];
    temp.store
        .append_event(
            &run_id,
            1_010,
            &root,
            &RunLogPayload::RunStarted {
                ir_hash: hash('a'),
                lockfile_digest: hash('b'),
                params_snapshot: json!({}),
                supervise_policy: None,
            },
        )
        .expect("start");
    let a = step_path("step_a");
    temp.store
        .append_event(
            &run_id,
            1_015,
            &a,
            &RunLogPayload::StepEntered {
                step_id: "step_a".try_into().expect("step id"),
                effect_hash: hash('c'),
                judge_hash: hash('d'),
                resolved_inputs: Value::Null,
            },
        )
        .expect("enter");
    temp.store
        .append_event(
            &run_id,
            1_020,
            &a,
            &RunLogPayload::VerdictRecorded {
                verdict: pass_verdict("pass, remotely unarchived"),
                localized: Vec::new(),
                localization_gaps: Vec::new(),
                remote_archival_error: Some("remote archival failed: daemon gone".to_owned()),
            },
        )
        .expect("verdict");
    temp.store
        .append_event(
            &run_id,
            1_030,
            &root,
            &RunLogPayload::RunFinished {
                verdict: Some(pass_verdict("flow pass")),
                remote_archival_error: Some("remote archival failed: daemon gone".to_owned()),
            },
        )
        .expect("finish");

    let page = projection::timeline_page(&temp.store, &run_id, RunTimelineFilter::All, 1, 50)
        .expect("page");
    let mut verdict_seen = false;
    let mut finish_seen = false;
    for entry in &page.entries {
        match &entry.detail {
            projection::TimelineDetail::VerdictRecorded {
                remote_archival_error,
                ..
            } => {
                assert_eq!(
                    remote_archival_error.as_deref(),
                    Some("remote archival failed: daemon gone")
                );
                verdict_seen = true;
            }
            projection::TimelineDetail::RunFinished {
                remote_archival_error,
                ..
            } => {
                assert_eq!(
                    remote_archival_error.as_deref(),
                    Some("remote archival failed: daemon gone")
                );
                finish_seen = true;
            }
            _ => {}
        }
    }
    assert!(verdict_seen && finish_seen);
}
