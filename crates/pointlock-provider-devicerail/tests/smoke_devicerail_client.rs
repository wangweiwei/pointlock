//! M1 smoke test: real consumption of `devicerail-client` against a freshly
//! built `devicerail-daemon` running the built-in mock driver (`mock-1`).
//!
//! This drives the complete openSession wire sequence from design doc
//! 04 §9.3: spawn → system.hello → devices.list → device.select →
//! device.connect → session.start, then device.capabilities →
//! device.execute(tap) → events.list (reconcile correlation-key check) →
//! verdict.record → session.end(completed) → close.
//!
//! It requires a sibling checkout of the DeviceRail repository at
//! `../../../device-rail` relative to this crate (i.e. next to the pointlock
//! repository) and a working `cargo` on PATH.

mod common;

use std::collections::BTreeSet;

use common::{TempDir, build_daemon, device_rail_repo};
use devicerail_client::{
    CallOptions, DeviceRailClient, SpawnConfig, methods,
    protocol::{
        ActionOutcome, DeviceExecuteParams, DeviceId, DeviceSelectParams, EventsListParams,
        FeatureOffer, HelloParams, PeerInfo, ProtocolOffer, ProtocolRange, ProtocolVersion,
        RequestTimeoutMs, SessionEndParams, SessionOutcome, SessionState, TestEventPayload,
        Verdict, VerdictStatus, feature,
    },
};
use uuid::Uuid;

/// Hello offer per design doc 04 §9.2: the provider-infrastructure features
/// are required; quality enhancements stay optional.
///
/// `verdict.record.v1` is offered as optional here (not required as it would
/// be for a flow whose IR uses verdicts) so the smoke run degrades gracefully
/// on daemons older than Protocol 1.5; the verdict step below is gated on the
/// negotiated feature set.
fn smoke_hello() -> HelloParams {
    HelloParams {
        client: PeerInfo {
            name: "pointlock-provider-devicerail".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        protocol: ProtocolOffer::new(vec![ProtocolRange::new(1, 0, 5)]),
        features: FeatureOffer {
            required: BTreeSet::from([
                feature::DEVICE_ROUTING_V1.to_owned(),
                feature::EVENTS_SNAPSHOT_V1.to_owned(),
                feature::REQUEST_CONTROL_V1.to_owned(),
            ]),
            optional: BTreeSet::from([feature::VERDICT_RECORD_V1.to_owned()]),
        },
    }
}

fn execute_call_options() -> CallOptions {
    CallOptions {
        // Request-envelope timeout (requires the negotiated
        // `request.control.v1`); generous because this is a smoke test, not a
        // budget test.
        timeout_ms: Some(RequestTimeoutMs::new(15_000).expect("valid timeout")),
    }
}

#[tokio::test]
async fn smoke_full_open_session_sequence_against_mock_daemon() {
    let repo = device_rail_repo();
    let daemon = build_daemon(&repo);
    let evidence_dir = TempDir::new("pointlock-devicerail-smoke-evidence");

    // spawn + system.hello (§9.2 / §9.3). The mock driver `mock-1` is always
    // registered by the daemon; other platform discovery is pinned off so the
    // run does not depend on local adb/hdc/desktop state.
    let config = SpawnConfig::new(&daemon, smoke_hello())
        .cwd(evidence_dir.path())
        .env("DEVICERAIL_EVIDENCE_DIR", evidence_dir.path())
        .env("DEVICERAIL_ANDROID", "off");
    let client = DeviceRailClient::spawn(config)
        .await
        .expect("spawn daemon and negotiate system.hello");

    let hello = client.negotiated_hello().expect("negotiated hello context");
    assert_eq!(hello.protocol.selected, ProtocolVersion::new(1, 5));
    let enabled = client.enabled_features();
    for required in [
        feature::DEVICE_ROUTING_V1,
        feature::EVENTS_SNAPSHOT_V1,
        feature::REQUEST_CONTROL_V1,
    ] {
        assert!(
            enabled.contains(required),
            "required feature {required} missing from the negotiated set"
        );
    }

    // devices.list → device.select → device.connect.
    let devices = client
        .call::<methods::DevicesList>(methods::NoParams, CallOptions::default())
        .await
        .expect("devices.list");
    let mock_id = DeviceId::new("mock-1");
    assert!(
        devices.devices.iter().any(|device| device.id == mock_id),
        "mock driver device `mock-1` not present in devices.list: {devices:?}"
    );

    let selected = client
        .call::<methods::DeviceSelect>(
            DeviceSelectParams {
                device_id: mock_id.clone(),
            },
            CallOptions::default(),
        )
        .await
        .expect("device.select mock-1");
    assert_eq!(selected.device.id, mock_id);

    let connected = client
        .call::<methods::DeviceConnect>(methods::NoParams, execute_call_options())
        .await
        .expect("device.connect");
    assert!(connected.connected, "device should report connected");

    // session.start (§9.3 ordering: after connect, before any action).
    let session = client
        .call::<methods::SessionStart>(methods::NoParams, CallOptions::default())
        .await
        .expect("session.start");
    assert_eq!(session.state, SessionState::Active);
    let session_id = session.id.clone();

    // device.capabilities: the mock driver's standard action catalog.
    let capabilities = client
        .call::<methods::DeviceCapabilities>(methods::NoParams, execute_call_options())
        .await
        .expect("device.capabilities");
    assert!(!capabilities.is_empty(), "action catalog must be non-empty");
    let action_names = capabilities
        .iter()
        .map(|action| action.name.as_str())
        .collect::<BTreeSet<_>>();
    for expected in ["tap", "scroll", "inputText"] {
        assert!(
            action_names.contains(expected),
            "mock driver catalog missing `{expected}`: {action_names:?}"
        );
    }

    // device.execute(tap) with a caller-generated call id: this uuid is the
    // Pointlock reconcile correlation key (design doc 04 §5 / §9.3).
    let call_id = Uuid::new_v4();
    let result = client
        .call::<methods::DeviceExecute>(
            DeviceExecuteParams {
                id: call_id,
                name: "tap".to_owned(),
                arguments: serde_json::json!({ "x": 120, "y": 240 }),
                action_timeout_ms: None,
            },
            execute_call_options(),
        )
        .await
        .expect("device.execute tap");
    assert_eq!(result.call_id, call_id);
    // The mock driver attaches a post-action observation and its screenshot
    // as evidence under the default capture policy.
    assert!(
        result.after.is_some() || !result.evidence.is_empty(),
        "tap result should carry an after-observation or evidence: {result:?}"
    );

    // events.list from the beginning of the session. The wire EventSequence
    // is one-based, so "afterSequence 0" is expressed by omitting the cursor.
    let events = client
        .call::<methods::EventsList>(
            Some(EventsListParams {
                session_id: Some(session_id.clone()),
                after_sequence: None,
                limit: None,
            }),
            CallOptions::default(),
        )
        .await
        .expect("events.list");
    assert!(
        events
            .iter()
            .all(|event| event.session_id == session_id && event.sequence.get() >= 1),
        "every event must belong to our session with a one-based sequence"
    );

    // Reconcile correlation key: actionStarted carries the full recorded call
    // (payload.call.id) and actionCompleted carries payload.callId; both must
    // equal the caller-generated device.execute uuid, verbatim.
    let started = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                TestEventPayload::ActionStarted { call } if call.id == call_id
            )
        })
        .expect("actionStarted event with payload.call.id == our call id");
    let TestEventPayload::ActionStarted { call } = &started.payload else {
        unreachable!("filtered above");
    };
    assert_eq!(call.name, "tap");
    assert!(
        !call.arguments_redacted,
        "standard actions must keep durable arguments"
    );

    let completed = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload,
                TestEventPayload::ActionCompleted { call_id: completed_id, .. }
                    if *completed_id == call_id
            )
        })
        .expect("actionCompleted event with payload.callId == our call id");
    let TestEventPayload::ActionCompleted { outcome, .. } = &completed.payload else {
        unreachable!("filtered above");
    };
    let ActionOutcome::Succeeded {
        result: event_result,
    } = outcome
    else {
        panic!("tap outcome must be `succeeded`, got: {outcome:?}");
    };
    assert_eq!(event_result.call_id, call_id);
    assert!(
        started.sequence < completed.sequence,
        "actionStarted must precede actionCompleted in the event log"
    );

    // verdict.record (gated on the optionally negotiated verdict.record.v1;
    // the 1.5 mock daemon always grants it, older daemons would skip here).
    if enabled.contains(feature::VERDICT_RECORD_V1) {
        let recorded = client
            .call::<methods::VerdictRecord>(
                devicerail_client::protocol::VerdictRecordParams {
                    verdict: Verdict {
                        status: VerdictStatus::Pass,
                        summary: "pointlock M1 smoke: tap executed and reconciled".to_owned(),
                        evidence: Vec::new(),
                    },
                },
                CallOptions::default(),
            )
            .await
            .expect("verdict.record");
        assert!(
            matches!(
                &recorded.event.payload,
                TestEventPayload::VerdictRecorded { verdict }
                    if verdict.status == VerdictStatus::Pass
            ),
            "verdict.record must append a verdictRecorded event: {recorded:?}"
        );
    } else {
        eprintln!("verdict.record.v1 not negotiated; skipping verdict.record step");
    }

    // session.end(completed): the session ended well; the test verdict lives
    // in verdict.record, not in the session outcome (§9.3).
    let ended = client
        .call::<methods::SessionEnd>(
            Some(SessionEndParams {
                outcome: Some(SessionOutcome::Completed),
                reason: None,
            }),
            CallOptions::default(),
        )
        .await
        .expect("session.end completed");
    assert_eq!(ended.id, session_id);
    assert_eq!(ended.state, SessionState::Ended);

    client.close().await.expect("close client and daemon child");
}
