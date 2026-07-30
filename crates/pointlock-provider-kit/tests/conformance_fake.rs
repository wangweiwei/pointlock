//! The conformance suite must be all green against FakeProvider (04 §8:
//! FakeProvider is the suite's reference subject).

use std::collections::VecDeque;

use pointlock_ir::ScreenshotOmissionReason;
use pointlock_provider_kit::conformance::{CheckStatus, ConformanceOptions, run_conformance};
use pointlock_provider_kit::fake::{FakeProvider, ScriptedOutcome};

#[tokio::test]
async fn conformance_suite_is_all_green_on_fake_provider() {
    // Fixture contract: the first four executes settle succeeded, failed,
    // cancelled, timedOut — in that order; the fifth (dispatch_count
    // provided) is the no-autonomous-retry probe: retryable failure.
    let provider = FakeProvider::new(VecDeque::from([
        ScriptedOutcome::succeeded(),
        ScriptedOutcome::failed("device_unavailable", true),
        ScriptedOutcome::cancelled(),
        ScriptedOutcome::timed_out(),
        ScriptedOutcome::failed("transient_glitch", true),
    ]));
    let handle = provider.handle();
    let log_handle = provider.handle();
    let count_handle = provider.handle();
    let omission_handle = provider.handle();

    let report = run_conformance(
        &provider,
        ConformanceOptions {
            open: provider.default_open_options(),
            inject_log_unavailable: Some(Box::new(move || {
                log_handle
                    .set_log_unavailable("session log deleted by conformance fault injection");
            })),
            dispatch_count: Some(Box::new(move || count_handle.dispatched_call_ids().len())),
            inject_observation_omission: Some(Box::new(move || {
                omission_handle.set_screenshot_omission(Some(ScreenshotOmissionReason::Policy));
            })),
        },
    )
    .await;
    let _ = handle;

    report.assert_all_green();
    // Belt and braces: nothing was skipped either.
    assert!(
        report
            .checks
            .iter()
            .all(|check| check.status == CheckStatus::Passed)
    );
}

#[tokio::test]
async fn conformance_without_fault_hook_skips_only_log_unavailable() {
    let provider = FakeProvider::new(VecDeque::from([
        ScriptedOutcome::succeeded(),
        ScriptedOutcome::failed("device_unavailable", false),
        ScriptedOutcome::cancelled(),
        ScriptedOutcome::timed_out(),
    ]));

    let report = run_conformance(
        &provider,
        ConformanceOptions {
            open: provider.default_open_options(),
            inject_log_unavailable: None,
            dispatch_count: None,
            inject_observation_omission: None,
        },
    )
    .await;

    assert!(report.passed(), "no check may fail: {:#?}", report.checks);
    let skipped: Vec<&str> = report
        .checks
        .iter()
        .filter(|check| check.status == CheckStatus::Skipped)
        .map(|check| check.name)
        .collect();
    assert_eq!(
        skipped,
        vec![
            "execute.noAutonomousRetry",
            "observe.omissionIsNotAnError",
            "reconcile.logUnavailable",
        ]
    );
}
