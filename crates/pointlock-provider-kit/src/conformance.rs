//! Provider conformance suite (04 §8).
//!
//! Every provider implementation (FakeProvider included) must run all
//! green before the CLI assembles it. This is the M0 minimal assertion
//! set: four-way execute terminals, the reconcile fates (`completed` for
//! archived succeeded *and* non-succeeded terminals — adopted verbatim,
//! never demoted — plus `neverDispatched` / `logUnavailable`), fail-closed
//! verdict caps plus a valid write, and cursor monotonicity — plus the
//! callId-discipline and cancellation-discipline checks that need no
//! fixture provisioning.
//!
//! # Fixture contract
//!
//! [`run_conformance`] drives a live [`Provider`], so the subject must be
//! prepared (scripted fake, mock daemon, …) such that, in the session
//! opened with [`ConformanceOptions::open`], its first four `execute`
//! calls settle `succeeded`, `failed`, `cancelled`, `timedOut` — in that
//! order; when [`ConformanceOptions::dispatch_count`] is provided, a
//! FIFTH execute must settle `failed` with `retryable: true` (the
//! no-autonomous-retry group). The `logUnavailable` fate and the legal
//! observation omission need faults the suite cannot cause through the
//! SPI; subjects provide them via the option hooks (each check is
//! recorded as skipped when its hook is absent).

use pointlock_ir::{ActionOutcome, ErrorClass, EventCursor, ReconcileResult, VerdictStatus};
use serde_json::json;
use uuid::Uuid;

use crate::spi::{
    BoundActionCall, CancellationToken, ObserveRequest, ObserveWant, OpenSessionOptions, Provider,
    ProviderSession, VERDICT_EVIDENCE_MAX_ENTRIES, VERDICT_SUMMARY_MAX_CHARS, VerdictWrite,
};

/// Outcome status of one conformance check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// The assertion held.
    Passed,
    /// The assertion failed (details in [`CheckResult::detail`]).
    Failed,
    /// The check could not be exercised (e.g. no fault-injection hook).
    Skipped,
}

/// Result of one conformance check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// Stable check name.
    pub name: &'static str,
    /// Outcome status.
    pub status: CheckStatus,
    /// Failure/skip explanation.
    pub detail: Option<String>,
}

/// Report of a full conformance run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    /// Every executed (or skipped) check, in suite order.
    pub checks: Vec<CheckResult>,
}

impl ConformanceReport {
    /// `true` when no check failed (skips allowed).
    pub fn passed(&self) -> bool {
        self.checks
            .iter()
            .all(|check| check.status != CheckStatus::Failed)
    }

    /// `true` when every check passed — nothing failed *or* skipped.
    pub fn all_green(&self) -> bool {
        self.checks
            .iter()
            .all(|check| check.status == CheckStatus::Passed)
    }

    /// Panics with a readable summary unless [`all_green`](Self::all_green).
    pub fn assert_all_green(&self) {
        if self.all_green() {
            return;
        }
        let mut lines = Vec::new();
        for check in &self.checks {
            let status = match check.status {
                CheckStatus::Passed => "PASS",
                CheckStatus::Failed => "FAIL",
                CheckStatus::Skipped => "SKIP",
            };
            let detail = check.detail.as_deref().unwrap_or("");
            lines.push(format!("  [{status}] {} {detail}", check.name));
        }
        panic!("provider conformance not all green:\n{}", lines.join("\n"));
    }
}

/// Options for [`run_conformance`].
pub struct ConformanceOptions {
    /// Session options the suite opens the subject with.
    pub open: OpenSessionOptions,
    /// Fault hook making the issuing session's event log unreachable, so
    /// the `logUnavailable` reconcile fate becomes observable. `None`
    /// records that check as skipped.
    pub inject_log_unavailable: Option<Box<dyn Fn() + Send + Sync>>,
    /// Wire-request counter of the subject (how many dispatches actually
    /// left the provider), for the 04 §8 no-autonomous-retry group: one
    /// retryable failure must ride EXACTLY one wire request. When
    /// present, the fixture must script a FIFTH execute settling
    /// `failed` with `retryable: true`. `None` records the check as
    /// skipped.
    pub dispatch_count: Option<Box<dyn Fn() -> usize + Send + Sync>>,
    /// Fault hook making the next observation legally omit its
    /// screenshot, for the 04 §8 terminal-fidelity group: an omission is
    /// typed data, never an error. `None` records the check as skipped.
    pub inject_observation_omission: Option<Box<dyn Fn() + Send + Sync>>,
}

struct Recorder {
    checks: Vec<CheckResult>,
}

impl Recorder {
    fn record(&mut self, name: &'static str, result: Result<(), String>) {
        self.checks.push(match result {
            Ok(()) => CheckResult {
                name,
                status: CheckStatus::Passed,
                detail: None,
            },
            Err(detail) => CheckResult {
                name,
                status: CheckStatus::Failed,
                detail: Some(detail),
            },
        });
    }

    fn skip(&mut self, name: &'static str, reason: &str) {
        self.checks.push(CheckResult {
            name,
            status: CheckStatus::Skipped,
            detail: Some(reason.to_owned()),
        });
    }
}

fn fresh_call(session: &dyn ProviderSession) -> BoundActionCall {
    let action_name = session
        .attestation()
        .actions
        .keys()
        .next()
        .expect("an attested session declares at least one action")
        .clone();
    BoundActionCall {
        call_id: Uuid::new_v4().to_string(),
        action_name,
        arguments: json!({}),
        action_timeout_ms: None,
        request_timeout_ms: None,
    }
}

/// Runs the conformance suite against `provider` (see the module docs for
/// the fixture contract) and returns the per-check report.
pub async fn run_conformance(
    provider: &dyn Provider,
    options: ConformanceOptions,
) -> ConformanceReport {
    let mut recorder = Recorder { checks: Vec::new() };

    // ── openSession is the prerequisite of everything else ────────────────
    let session = match provider.open_session(options.open).await {
        Ok(session) => {
            recorder.record("openSession.opens", Ok(()));
            session
        }
        Err(error) => {
            recorder.record(
                "openSession.opens",
                Err(format!("open_session failed: {error}")),
            );
            return ConformanceReport {
                checks: recorder.checks,
            };
        }
    };
    let session: &dyn ProviderSession = session.as_ref();

    let cursor_before = session.current_cursor().await;
    // The issuing credential of every dispatch the suite performs below:
    // captured BEFORE those dispatches (04 §5 — the checkpoint binding
    // cursor predates the intent; an after-the-fact cursor is not a
    // credential). A cursor failure records failed checks downstream
    // instead of panicking the harness.
    let issuing_owned = cursor_before.clone().ok();
    let issuing = match &issuing_owned {
        Some(cursor) => cursor,
        None => &EventCursor {
            session_id: String::new(),
            last_sequence: 0,
        },
    };

    // ── execute: the four-way terminal set is reachable, unfolded ─────────
    let mut terminal_call_ids = TerminalCallIds::default();
    let result = execute_terminals(session, &mut terminal_call_ids).await;
    recorder.record("execute.fourTerminalsReachable", result);
    let succeeded_call_id = terminal_call_ids.succeeded;
    let failed_call_id = terminal_call_ids.failed;

    let cursor_after_executes = session.current_cursor().await;

    // ── callId discipline: a repeated callId is rejected ───────────────────
    if let Some(call_id) = &succeeded_call_id {
        let mut replay = fresh_call(session);
        replay.call_id = call_id.clone();
        let result = match session.execute(replay, None).await {
            Err(_) => Ok(()),
            Ok(outcome) => Err(format!(
                "a repeated callId must be rejected (a retry is a new callId + new WAL \
                 intent); got terminal {}",
                outcome.kind()
            )),
        };
        recorder.record("execute.rejectsDuplicateCallId", result);
    } else {
        recorder.skip(
            "execute.rejectsDuplicateCallId",
            "no succeeded callId available (terminals check failed)",
        );
    }

    // ── fail-closed: an unattested actionName never dispatches ────────────
    {
        let before = options.dispatch_count.as_ref().map(|counter| counter());
        let mut rogue = fresh_call(session);
        rogue.action_name = pointlock_ir::ActionName::new("conformanceRogueAction")
            .expect("grammatical action name");
        let result = match session.execute(rogue, None).await {
            Err(_) => match (before, options.dispatch_count.as_ref()) {
                (Some(before), Some(counter)) if counter() != before => Err(
                    "the refusal must happen BEFORE any wire request left the provider".to_owned(),
                ),
                _ => Ok(()),
            },
            Ok(outcome) => Err(format!(
                "an unattested actionName must be refused fail-closed (04 §8), got \
                 terminal {}",
                outcome.kind()
            )),
        };
        recorder.record("execute.rejectsUnattestedAction", result);
    }

    // ── no autonomous retry: one retryable failure = one wire request ─────
    match &options.dispatch_count {
        Some(counter) => {
            let before = counter();
            let call = fresh_call(session);
            let result = match session.execute(call, None).await {
                Ok(ActionOutcome::Failed { error }) if error.retryable => {
                    let sent = counter().saturating_sub(before);
                    if sent == 1 {
                        Ok(())
                    } else {
                        Err(format!(
                            "a retryable failure must ride exactly one wire request — \
                             retries are the runner's, never the provider's (04 §3); \
                             counted {sent}"
                        ))
                    }
                }
                Ok(other) => Err(format!(
                    "fixture contract: the fifth scripted execute must settle failed \
                     with retryable: true, got {}",
                    other.kind()
                )),
                Err(error) => Err(format!(
                    "the retryable failure must arrive as a terminal, got \
                     ProviderError: {error}"
                )),
            };
            recorder.record("execute.noAutonomousRetry", result);
        }
        None => recorder.skip(
            "execute.noAutonomousRetry",
            "no dispatch_count hook provided",
        ),
    }

    // ── terminal fidelity: a legal omission is data, never an error ───────
    match &options.inject_observation_omission {
        Some(inject) => {
            inject();
            let result = match session
                .observe(
                    ObserveRequest {
                        wants: vec![ObserveWant::Screenshot],
                    },
                    None,
                )
                .await
            {
                Ok(observation) => {
                    if observation.screenshot.is_none() && observation.screenshot_omission.is_some()
                    {
                        Ok(())
                    } else {
                        Err(format!(
                            "the injected omission must surface as typed omission data \
                             (screenshot absent + reason recorded); got screenshot={} \
                             omission={:?}",
                            observation.screenshot.is_some(),
                            observation.screenshot_omission
                        ))
                    }
                }
                Err(error) => Err(format!(
                    "a legal omission is typed data, never an error (04 §8): {error}"
                )),
            };
            recorder.record("observe.omissionIsNotAnError", result);
        }
        None => recorder.skip(
            "observe.omissionIsNotAnError",
            "no inject_observation_omission hook provided",
        ),
    }

    // ── reconcile: foreign issuing credential never fabricates ────────────
    // `neverDispatched` (04 §5, 2026-07-18): with a credential naming a
    // session that is NOT this one, for a callId this session's log DOES
    // contain, the only sound answers are `completed` (the provider has
    // cross-generation retrieval and genuinely read the history) or
    // `logUnavailable` (it does not). `neverDispatched` would mean the
    // provider scanned the wrong log and licensed an auto-replay.
    if let Some(call_id) = &succeeded_call_id {
        let foreign = EventCursor {
            session_id: "conformance-foreign-generation".to_owned(),
            last_sequence: 0,
        };
        let result = match session.reconcile(call_id, &foreign).await {
            Ok(ReconcileResult::NeverDispatched) => Err(
                "a foreign issuing credential answered neverDispatched for a call this \
                 session dispatched — the provider scanned the wrong log"
                    .to_owned(),
            ),
            Ok(_) => Ok(()),
            Err(error) => Err(format!("reconcile failed: {error}")),
        };
        recorder.record("reconcile.foreignCredentialNeverFabricates", result);
    } else {
        recorder.skip(
            "reconcile.foreignCredentialNeverFabricates",
            "no succeeded callId available (terminals check failed)",
        );
    }

    // ── cancellation discipline: pre-cancelled ⇒ no dispatch ──────────────
    let result = pre_cancelled_execute(session, issuing).await;
    recorder.record("execute.preCancelledSendsNothing", result);

    // ── reconcile: completed (archived succeeded terminal) ────────────────
    if let Some(call_id) = &succeeded_call_id {
        let result = match session.reconcile(call_id, issuing).await {
            Ok(ReconcileResult::Completed { outcome }) => match outcome.as_ref() {
                ActionOutcome::Succeeded { result } if result.call_id == *call_id => Ok(()),
                ActionOutcome::Succeeded { result } => Err(format!(
                    "completed fate carries callId {} instead of {call_id}",
                    result.call_id
                )),
                other => Err(format!(
                    "the archived terminal was succeeded; completed fate must adopt it \
                     verbatim, got {}",
                    other.kind()
                )),
            },
            Ok(other) => Err(format!("expected fate completed, got {other:?}")),
            Err(error) => Err(format!("reconcile failed: {error}")),
        };
        recorder.record("reconcile.completed", result);
    } else {
        recorder.skip(
            "reconcile.completed",
            "no succeeded callId available (terminals check failed)",
        );
    }

    // ── reconcile: completed (archived non-succeeded terminal) ────────────
    // A recorded failed/cancelled/timedOut is a *certain* fate: it must
    // come back as completed with the terminal verbatim, never demoted to
    // an uncertain branch (logUnavailable/startedNoTerminal).
    if let Some(call_id) = &failed_call_id {
        let result = match session.reconcile(call_id, issuing).await {
            Ok(ReconcileResult::Completed { outcome }) if outcome.kind() == "failed" => Ok(()),
            Ok(ReconcileResult::Completed { outcome }) => Err(format!(
                "the archived terminal was failed; completed fate must adopt it verbatim, \
                 got {}",
                outcome.kind()
            )),
            Ok(other) => Err(format!(
                "an archived failed terminal is a certain fate and must reconcile as \
                 completed, got {other:?}"
            )),
            Err(error) => Err(format!("reconcile failed: {error}")),
        };
        recorder.record("reconcile.completedNonSuccess", result);
    } else {
        recorder.skip(
            "reconcile.completedNonSuccess",
            "no failed callId available (terminals check failed)",
        );
    }

    // ── reconcile: neverDispatched ─────────────────────────────────────────
    let result = match session
        .reconcile(
            &Uuid::new_v4().to_string(),
            &session.current_cursor().await.expect("cursor"),
        )
        .await
    {
        Ok(ReconcileResult::NeverDispatched) => Ok(()),
        Ok(other) => Err(format!("expected fate neverDispatched, got {other:?}")),
        Err(error) => Err(format!("reconcile failed: {error}")),
    };
    recorder.record("reconcile.neverDispatched", result);

    // ── recordVerdict: fail-closed caps, then a valid write ───────────────
    let oversized_summary = VerdictWrite {
        status: VerdictStatus::Pass,
        summary: "x".repeat(VERDICT_SUMMARY_MAX_CHARS + 1),
        evidence: Vec::new(),
    };
    let result = expect_bind_arguments_invalid(
        session.record_verdict(oversized_summary).await,
        "an oversized summary",
    );
    recorder.record("recordVerdict.rejectsOversizedSummary", result);

    let oversized_evidence = VerdictWrite {
        status: VerdictStatus::Pass,
        summary: "capped".to_owned(),
        evidence: vec![sample_evidence(session).await; VERDICT_EVIDENCE_MAX_ENTRIES + 1],
    };
    let result = expect_bind_arguments_invalid(
        session.record_verdict(oversized_evidence).await,
        "an oversized evidence list",
    );
    recorder.record("recordVerdict.rejectsOversizedEvidence", result);

    let valid = VerdictWrite {
        status: VerdictStatus::Unknown,
        summary: "conformance write".to_owned(),
        evidence: Vec::new(),
    };
    let result = session
        .record_verdict(valid)
        .await
        .map_err(|error| format!("a within-caps verdict must be accepted: {error}"));
    recorder.record("recordVerdict.acceptsValid", result);

    // ── cursor monotonicity ────────────────────────────────────────────────
    let cursor_after_verdicts = session.current_cursor().await;
    let result = cursor_monotonic(cursor_before, cursor_after_executes, cursor_after_verdicts);
    recorder.record("currentCursor.monotonic", result);

    // ── reconcile: logUnavailable (needs fault injection; last — it may
    //    poison subsequent log reads) ────────────────────────────────────────
    match &options.inject_log_unavailable {
        Some(inject) => {
            inject();
            let probe = succeeded_call_id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let result = match session.reconcile(&probe, issuing).await {
                Ok(ReconcileResult::LogUnavailable { .. }) => Ok(()),
                Ok(other) => Err(format!(
                    "an unreachable issuing-session log must always be logUnavailable, \
                     got {other:?}"
                )),
                Err(error) => Err(format!("reconcile failed: {error}")),
            };
            recorder.record("reconcile.logUnavailable", result);
        }
        None => recorder.skip(
            "reconcile.logUnavailable",
            "no inject_log_unavailable hook provided",
        ),
    }

    // ── teardown ───────────────────────────────────────────────────────────
    let result = session
        .end(crate::spi::SessionOutcome::Completed, None)
        .await
        .map_err(|error| format!("end must be best-effort and not fail teardown: {error}"));
    recorder.record("end.completes", result);

    ConformanceReport {
        checks: recorder.checks,
    }
}

/// The callIds of the scripted terminals the reconcile checks probe later.
#[derive(Default)]
struct TerminalCallIds {
    succeeded: Option<String>,
    failed: Option<String>,
}

async fn execute_terminals(
    session: &dyn ProviderSession,
    call_ids: &mut TerminalCallIds,
) -> Result<(), String> {
    let expected = ["succeeded", "failed", "cancelled", "timedOut"];
    for expected_kind in expected {
        let call = fresh_call(session);
        let call_id = call.call_id.clone();
        match session.execute(call, None).await {
            Ok(outcome) => {
                if outcome.kind() != expected_kind {
                    return Err(format!(
                        "fixture contract: expected terminal {expected_kind}, got {} \
                         (terminals must arrive unfolded and untranslated)",
                        outcome.kind()
                    ));
                }
                match &outcome {
                    ActionOutcome::Succeeded { result } => {
                        if result.call_id != call_id {
                            return Err(format!(
                                "succeeded result must echo the dispatched callId verbatim: \
                                 got {} instead of {call_id}",
                                result.call_id
                            ));
                        }
                        call_ids.succeeded = Some(call_id);
                    }
                    ActionOutcome::Failed { .. } => call_ids.failed = Some(call_id),
                    _ => {}
                }
            }
            Err(error) => {
                return Err(format!(
                    "expected terminal {expected_kind}, got ProviderError: {error}"
                ));
            }
        }
    }
    Ok(())
}

async fn pre_cancelled_execute(
    session: &dyn ProviderSession,
    issuing: &EventCursor,
) -> Result<(), String> {
    let token = CancellationToken::new();
    token.cancel();
    let call = fresh_call(session);
    let call_id = call.call_id.clone();
    match session.execute(call, Some(token)).await {
        Err(error) if error.error_class == ErrorClass::ActionCancelled => {}
        Err(error) => {
            return Err(format!(
                "a pre-cancelled token must fail with class action_cancelled, got {}",
                error.error_class as u8
            ));
        }
        Ok(outcome) => {
            return Err(format!(
                "a pre-cancelled token must not dispatch; got terminal {}",
                outcome.kind()
            ));
        }
    }
    match session.reconcile(&call_id, issuing).await {
        Ok(ReconcileResult::NeverDispatched) => Ok(()),
        Ok(other) => Err(format!(
            "a pre-cancelled execute must leave no wire trace; reconcile found {other:?}"
        )),
        Err(error) => Err(format!("reconcile failed: {error}")),
    }
}

fn expect_bind_arguments_invalid(
    result: Result<(), crate::error::ProviderError>,
    what: &str,
) -> Result<(), String> {
    match result {
        Err(error) if error.error_class == ErrorClass::BindArgumentsInvalid => Ok(()),
        Err(error) => Err(format!(
            "{what} must be rejected with class bind_arguments_invalid, got: {error}"
        )),
        Ok(()) => Err(format!(
            "{what} must be rejected fail-closed, but was accepted"
        )),
    }
}

async fn sample_evidence(session: &dyn ProviderSession) -> pointlock_ir::AssetRef {
    // Prefer a genuine asset from an observation; fall back to a synthetic
    // reference (the cap check must fire before any dereference).
    if let Ok(observation) = session
        .observe(
            ObserveRequest {
                wants: vec![ObserveWant::Screenshot],
            },
            None,
        )
        .await
        && let Some(asset) = observation.screenshot
    {
        return asset;
    }
    pointlock_ir::AssetRef {
        id: "conformance-synthetic-asset".to_owned(),
        media_type: "image/png".to_owned(),
        uri: "conformance://assets/synthetic".to_owned(),
        sha256: None,
    }
}

fn cursor_monotonic(
    before: Result<EventCursor, crate::error::ProviderError>,
    after_executes: Result<EventCursor, crate::error::ProviderError>,
    after_verdicts: Result<EventCursor, crate::error::ProviderError>,
) -> Result<(), String> {
    let before = before.map_err(|error| format!("currentCursor failed: {error}"))?;
    let after_executes =
        after_executes.map_err(|error| format!("currentCursor failed: {error}"))?;
    let after_verdicts =
        after_verdicts.map_err(|error| format!("currentCursor failed: {error}"))?;

    if after_executes.session_id != before.session_id
        || after_verdicts.session_id != before.session_id
    {
        return Err(format!(
            "cursor sessionId drifted within one session: {} / {} / {}",
            before.session_id, after_executes.session_id, after_verdicts.session_id
        ));
    }
    if after_executes.last_sequence <= before.last_sequence {
        return Err(format!(
            "cursor must strictly advance across executed actions: {} -> {}",
            before.last_sequence, after_executes.last_sequence
        ));
    }
    if after_verdicts.last_sequence < after_executes.last_sequence {
        return Err(format!(
            "cursor must never regress: {} -> {}",
            after_executes.last_sequence, after_verdicts.last_sequence
        ));
    }
    Ok(())
}
