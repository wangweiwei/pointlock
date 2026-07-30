//! M1 integration tests: the full `DeviceRailProvider` SPI surface against
//! a real `devicerail-daemon` running the built-in mock driver (`mock-1`),
//! plus the provider-kit conformance suite (04 §8 / §10).
//!
//! Requires the sibling DeviceRail checkout (see `tests/common/mod.rs`).

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{TempDir, build_daemon, device_rail_repo};
use pointlock_ir::{ActionName, ActionOutcome, ErrorClass, ReconcileResult};
use pointlock_provider_devicerail::{
    DeviceRailProvider, INFRA_REQUIRED_FEATURES, SpawnSpec, lock_via_spawn_at,
};
use pointlock_provider_kit::conformance::{CheckStatus, ConformanceOptions, run_conformance};
use pointlock_provider_kit::{
    BoundActionCall, CancellationToken, ObserveRequest, ObserveWant, OpenSessionOptions, Provider,
    ProviderSession, SessionOutcome, UiSnapshotOutcome, VERDICT_SUMMARY_MAX_CHARS, VerdictWrite,
};
use serde_json::json;
use uuid::Uuid;

/// A fixed lock timestamp for deterministic fixtures. The digest domain
/// excludes `attestedAt` (spine §4.1), so this is cosmetic determinism of
/// the artifact, not a digest-stability requirement.
const LOCK_TIMESTAMP: &str = "2026-01-01T00:00:00Z";

fn spawn_spec(daemon: &Path, evidence_dir: &Path) -> SpawnSpec {
    SpawnSpec {
        command: daemon.to_string_lossy().into_owned(),
        args: Vec::new(),
        env: BTreeMap::from([
            (
                "DEVICERAIL_EVIDENCE_DIR".to_owned(),
                evidence_dir.to_string_lossy().into_owned(),
            ),
            // Pin platform discovery off so the run does not depend on
            // local adb state; the mock driver is always registered.
            ("DEVICERAIL_ANDROID".to_owned(), "off".to_owned()),
        ]),
        cwd: Some(evidence_dir.to_string_lossy().into_owned()),
        shutdown_grace_ms: 5_000,
    }
}

fn endpoint(spec: &SpawnSpec) -> serde_json::Value {
    json!({ "spawn": serde_json::to_value(spec).expect("SpawnSpec serializes") })
}

fn tap_call(arguments: serde_json::Value) -> BoundActionCall {
    BoundActionCall {
        call_id: Uuid::new_v4().to_string(),
        action_name: ActionName::new("tap").unwrap(),
        arguments,
        action_timeout_ms: Some(10_000),
        request_timeout_ms: None,
    }
}

#[tokio::test]
async fn provider_full_lifecycle_against_mock_daemon() {
    let repo = device_rail_repo();
    let daemon = build_daemon(&repo);
    let evidence_dir = TempDir::new("pointlock-devicerail-provider-evidence");
    let spec = spawn_spec(&daemon, evidence_dir.path());

    // ── pointlock lock: freeze the live world; double-run digest stability
    //    (04 §10.2) with a pinned attestedAt ────────────────────────────────
    let lockfile = lock_via_spawn_at(&spec, "mock-1", LOCK_TIMESTAMP)
        .await
        .expect("lock run against the mock daemon");
    assert!(lockfile.digest_consistent());
    let second = lock_via_spawn_at(&spec, "mock-1", LOCK_TIMESTAMP)
        .await
        .expect("second lock run");
    assert_eq!(
        lockfile.digest, second.digest,
        "the same daemon must freeze to the same digest (attestedAt pinned)"
    );
    let enabled: Vec<&str> = lockfile
        .hello
        .features_enabled
        .iter()
        .map(|feature| feature.as_str())
        .collect();
    for infra in INFRA_REQUIRED_FEATURES {
        assert!(enabled.contains(&infra), "infra feature {infra} missing");
    }
    assert!(enabled.contains(&"verdict.record.v1"));
    let action_names: Vec<&str> = lockfile
        .device
        .actions
        .iter()
        .map(|action| action.name.as_str())
        .collect();
    for expected in ["tap", "inputText", "scroll"] {
        assert!(action_names.contains(&expected), "{expected} missing");
    }

    // ── openSession: full §9.3 sequence + attestation pass ────────────────
    let provider = DeviceRailProvider::new(lockfile.clone()).expect("provider construction");
    let session = provider
        .open_session(OpenSessionOptions {
            endpoint: endpoint(&spec),
            device_id: "mock-1".to_owned(),
            required_features: Vec::new(),
            lockfile_digest: lockfile.digest.clone(),
        })
        .await
        .expect("openSession against the mock daemon");
    // The issuing credential predates every dispatch below (04 §5).
    let issuing = session
        .current_cursor()
        .await
        .expect("cursor before dispatches");
    let session: &dyn ProviderSession = session.as_ref();

    let attestation = session.attestation();
    assert_eq!(attestation.provider_id, "devicerail");
    assert_eq!(attestation.protocol_selected.major, 1);
    assert_eq!(attestation.protocol_selected.minor, 5);
    assert_eq!(attestation.lockfile_digest, lockfile.digest);
    assert!(
        attestation
            .actions
            .contains_key(&ActionName::new("tap").unwrap())
    );

    // ── execute(tap): the succeeded terminal through the SPI ──────────────
    let call = tap_call(json!({ "x": 120, "y": 240 }));
    let call_id = call.call_id.clone();
    let outcome = session.execute(call, None).await.expect("execute tap");
    let ActionOutcome::Succeeded { result } = outcome else {
        panic!("tap must settle succeeded, got {}", outcome.kind());
    };
    assert_eq!(result.call_id, call_id, "callId echoed verbatim (04 §3)");
    assert!(
        result.after.is_some() || !result.evidence.is_empty(),
        "the mock driver attaches an after-observation/evidence"
    );

    // ── execute(tap, bad args): a definite failed terminal, unfolded ──────
    let failed_call = tap_call(json!({ "x": -1, "y": 5 }));
    let failed_call_id = failed_call.call_id.clone();
    let outcome = session
        .execute(failed_call, None)
        .await
        .expect("a failed terminal is an Ok value, not an error (04 §3)");
    let ActionOutcome::Failed { error } = &outcome else {
        panic!("negative coordinates must fail, got {}", outcome.kind());
    };
    assert_eq!(error.code, "invalid_arguments");

    // ── callId discipline ──────────────────────────────────────────────────
    let mut duplicate = tap_call(json!({ "x": 1, "y": 1 }));
    duplicate.call_id = call_id.clone();
    let error = session
        .execute(duplicate, None)
        .await
        .expect_err("a repeated callId is rejected");
    assert_eq!(error.error_class, ErrorClass::BindArgumentsInvalid);

    // ── cancellation discipline: pre-cancelled ⇒ nothing on the wire ──────
    let token = CancellationToken::new();
    token.cancel();
    let never_sent = tap_call(json!({ "x": 2, "y": 2 }));
    let never_sent_id = never_sent.call_id.clone();
    let error = session
        .execute(never_sent, Some(token))
        .await
        .expect_err("pre-cancelled token must not dispatch");
    assert_eq!(error.error_class, ErrorClass::ActionCancelled);

    // ── reconcile on the real event log: both certain fates + no-trace ────
    let fate = session
        .reconcile(&call_id, &issuing)
        .await
        .expect("reconcile");
    let ReconcileResult::Completed { outcome } = fate else {
        panic!("expected completed, got {fate:?}");
    };
    let ActionOutcome::Succeeded { result } = outcome.as_ref() else {
        panic!("archived succeeded terminal adopted verbatim");
    };
    assert_eq!(result.call_id, call_id);

    let fate = session
        .reconcile(&failed_call_id, &issuing)
        .await
        .expect("reconcile");
    let ReconcileResult::Completed { outcome } = fate else {
        panic!("an archived failed terminal is a certain fate, got {fate:?}");
    };
    assert_eq!(outcome.kind(), "failed");

    assert_eq!(
        session
            .reconcile(&never_sent_id, &issuing)
            .await
            .expect("reconcile"),
        ReconcileResult::NeverDispatched,
        "a pre-cancelled execute leaves no wire trace"
    );

    // ── cross-generation guard (04 §5, 2026-07-18): a foreign issuing ─────
    // credential must answer logUnavailable — never a scan of THIS
    // session's (reachable but wrong) log, which would fabricate
    // `neverDispatched` for the dispatched call above.
    let foreign = pointlock_ir::EventCursor {
        session_id: "an-earlier-generation".to_owned(),
        last_sequence: 0,
    };
    let fate = session
        .reconcile(&call_id, &foreign)
        .await
        .expect("reconcile");
    assert!(
        matches!(fate, ReconcileResult::LogUnavailable { .. }),
        "a foreign credential must not be answered from this session's log, got {fate:?}"
    );
    assert_eq!(
        session
            .reconcile(&Uuid::new_v4().to_string(), &issuing,)
            .await
            .expect("reconcile"),
        ReconcileResult::NeverDispatched
    );

    // ── observe: omissions are data (04 §4.1) ──────────────────────────────
    let observation = session
        .observe(
            ObserveRequest {
                wants: vec![ObserveWant::Screenshot, ObserveWant::UiSnapshot],
            },
            None,
        )
        .await
        .expect("observe");
    assert!(
        observation.screenshot.is_some(),
        "mock daemon captures screenshots under the default policy"
    );
    // The mock driver has no semantic UI channel: a wanted-but-missing
    // snapshot comes back as a typed omission, not an error.
    assert!(observation.ui_snapshot.is_none());
    assert!(observation.ui_snapshot_omission.is_some());

    // ── uiSnapshot deref: typed unavailability from the wire reply ────────
    let snapshot = session
        .ui_snapshot(&observation.id)
        .await
        .expect("ui.snapshot.get");
    assert!(
        matches!(snapshot, UiSnapshotOutcome::Unavailable { .. }),
        "the mock observation carries no snapshot: typed omission, got {snapshot:?}"
    );

    // ── fetchEvidence: honestly unsupported on this wire surface ──────────
    let asset = observation.screenshot.clone().expect("screenshot asset");
    let error = session
        .fetch_evidence(&asset)
        .await
        .err()
        .expect("no asset byte channel exists");
    assert_eq!(error.error_class, ErrorClass::ActionFailedFinal);

    // ── recordVerdict: fail-closed caps + a real wire write ───────────────
    let error = session
        .record_verdict(VerdictWrite {
            status: pointlock_ir::VerdictStatus::Pass,
            summary: "x".repeat(VERDICT_SUMMARY_MAX_CHARS + 1),
            evidence: Vec::new(),
        })
        .await
        .expect_err("oversized summary is rejected fail-closed");
    assert_eq!(error.error_class, ErrorClass::BindArgumentsInvalid);
    session
        .record_verdict(VerdictWrite {
            status: pointlock_ir::VerdictStatus::Pass,
            summary: "pointlock M1 integration: tap executed and reconciled".to_owned(),
            evidence: Vec::new(),
        })
        .await
        .expect("verdict.record");

    // ── currentCursor: v0.1 events.list inference, monotonic ──────────────
    let cursor = session.current_cursor().await.expect("currentCursor");
    assert!(cursor.last_sequence >= 1, "sessionStarted is sequence 1");
    let later = session.current_cursor().await.expect("currentCursor");
    assert_eq!(later.session_id, cursor.session_id);
    assert!(later.last_sequence >= cursor.last_sequence);

    // ── health ─────────────────────────────────────────────────────────────
    let health = session.health().await.expect("health");
    assert!(health.ok, "live session probes healthy: {health:?}");

    // ── end: idempotent, breaks other methods, reconcile → logUnavailable ──
    session
        .end(SessionOutcome::Completed, None)
        .await
        .expect("end");
    session
        .end(SessionOutcome::Failed, Some("ignored".to_owned()))
        .await
        .expect("idempotent end");
    let health = session.health().await.expect("health after end");
    assert!(!health.ok);
    let error = session
        .execute(tap_call(json!({ "x": 3, "y": 3 })), None)
        .await
        .expect_err("execute after end");
    assert_eq!(error.error_class, ErrorClass::TransportLost);
    // The credential is this session's own (saved before end — the RPC
    // is dead now); the ended-session gate must still refuse.
    let fate = session
        .reconcile(&call_id, &issuing)
        .await
        .expect("reconcile");
    assert!(
        matches!(fate, ReconcileResult::LogUnavailable { .. }),
        "an ended session's log is unreachable through this connection: {fate:?}"
    );
}

#[tokio::test]
async fn attestation_rejects_a_drifted_lockfile() {
    let repo = device_rail_repo();
    let daemon = build_daemon(&repo);
    let evidence_dir = TempDir::new("pointlock-devicerail-drift-evidence");
    let spec = spawn_spec(&daemon, evidence_dir.path());

    let mut lockfile = lock_via_spawn_at(&spec, "mock-1", LOCK_TIMESTAMP)
        .await
        .expect("lock run");
    // Simulate a world that was locked with one more negotiated feature
    // than the live daemon grants: drop one enabled feature and re-seal.
    lockfile
        .hello
        .features_enabled
        .retain(|feature| feature.as_str() != "verdict.record.v1");
    lockfile.seal();

    let provider = DeviceRailProvider::new(lockfile.clone()).expect("self-consistent lockfile");
    let error = match provider
        .open_session(OpenSessionOptions {
            endpoint: endpoint(&spec),
            device_id: "mock-1".to_owned(),
            required_features: Vec::new(),
            lockfile_digest: lockfile.digest.clone(),
        })
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("attestation must refuse a drifted lockfile"),
    };
    assert_eq!(error.error_class, ErrorClass::CapabilityDrift);
    assert!(
        error.message.contains("attestation mismatch"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("verdict.record.v1"),
        "the drift report names the diff: {}",
        error.message
    );
}

/// Runs the provider-kit conformance suite against the real daemon and
/// asserts the honestly-reachable shape (04 §8).
///
/// The suite's fixture contract requires the first four executes to settle
/// `succeeded`/`failed`/`cancelled`/`timedOut` in scripted order — a real
/// daemon with the mock driver is not programmable that way, so
/// `execute.fourTerminalsReachable` records Failed and its dependents are
/// Skipped; `reconcile.logUnavailable` is Skipped because the stdio spawn
/// transport admits exactly one connection, leaving no side channel for
/// the fault-injection hook. Every other check must pass.
#[tokio::test]
async fn conformance_suite_reports_the_expected_shape_on_the_real_daemon() {
    let repo = device_rail_repo();
    let daemon = build_daemon(&repo);
    let evidence_dir = TempDir::new("pointlock-devicerail-conformance-evidence");
    let spec = spawn_spec(&daemon, evidence_dir.path());

    let lockfile = lock_via_spawn_at(&spec, "mock-1", LOCK_TIMESTAMP)
        .await
        .expect("lock run");
    let provider = DeviceRailProvider::new(lockfile.clone()).expect("provider");

    let report = run_conformance(
        &provider,
        ConformanceOptions {
            open: OpenSessionOptions {
                endpoint: endpoint(&spec),
                device_id: "mock-1".to_owned(),
                required_features: Vec::new(),
                lockfile_digest: lockfile.digest.clone(),
            },
            inject_log_unavailable: None,
            dispatch_count: None,
            inject_observation_omission: None,
        },
    )
    .await;

    let expected: BTreeMap<&str, CheckStatus> = BTreeMap::from([
        ("openSession.opens", CheckStatus::Passed),
        // Fixture contract not satisfiable on a non-programmable daemon.
        ("execute.fourTerminalsReachable", CheckStatus::Failed),
        ("execute.rejectsDuplicateCallId", CheckStatus::Skipped),
        // Fail-closed before any wire request (session.rs attestation
        // check) — passes even against the non-programmable daemon.
        ("execute.rejectsUnattestedAction", CheckStatus::Passed),
        ("execute.noAutonomousRetry", CheckStatus::Skipped),
        ("observe.omissionIsNotAnError", CheckStatus::Skipped),
        ("execute.preCancelledSendsNothing", CheckStatus::Passed),
        ("reconcile.completed", CheckStatus::Skipped),
        ("reconcile.completedNonSuccess", CheckStatus::Skipped),
        ("reconcile.neverDispatched", CheckStatus::Passed),
        ("recordVerdict.rejectsOversizedSummary", CheckStatus::Passed),
        (
            "recordVerdict.rejectsOversizedEvidence",
            CheckStatus::Passed,
        ),
        ("recordVerdict.acceptsValid", CheckStatus::Passed),
        ("currentCursor.monotonic", CheckStatus::Passed),
        (
            "reconcile.foreignCredentialNeverFabricates",
            CheckStatus::Skipped,
        ),
        ("reconcile.logUnavailable", CheckStatus::Skipped),
        ("end.completes", CheckStatus::Passed),
    ]);
    let mut seen = BTreeMap::new();
    for check in &report.checks {
        seen.insert(check.name, check.status);
        let Some(expected_status) = expected.get(check.name) else {
            panic!("unexpected conformance check {}", check.name);
        };
        assert_eq!(
            check.status, *expected_status,
            "check {} diverged (detail: {:?})",
            check.name, check.detail
        );
    }
    for name in expected.keys() {
        assert!(seen.contains_key(name), "check {name} missing from report");
    }
}
