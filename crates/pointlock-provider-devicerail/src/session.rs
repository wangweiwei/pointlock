//! `DeviceRailSession`: the `ProviderSession` implementation over one
//! exclusively-owned `DeviceRailClient` connection (04 §1 rule 4 — never
//! shared, never pooled).

use std::collections::HashSet;
use std::sync::Mutex;

use async_trait::async_trait;
use devicerail_client::protocol::{
    DeviceExecuteParams, EventSequence, EventsListParams, SessionEndParams,
    SessionId as WireSessionId, SessionOutcome as WireSessionOutcome, TestEvent,
    UiSnapshotGetParams, Verdict as WireVerdict, VerdictRecordParams,
    VerdictStatus as WireVerdictStatus, feature,
};
use devicerail_client::{CallOptions, ClientError, DeviceRailClient, RequestHandle, methods};
use pointlock_ir::{
    ActionOutcome, ActionResult, ErrorClass, EventCursor, Observation, ReconcileResult,
    ScreenshotOmissionReason, UiSnapshotOmissionReason, VerdictStatus,
};
use pointlock_provider_kit::lockfile::CapabilityAttestation;
use pointlock_provider_kit::{
    BoundActionCall, CancellationToken, EvidenceStream, ObserveRequest, ObserveWant, ProviderError,
    ProviderSession, RetryableSource, SessionHealth, SessionOutcome, UiSnapshotOutcome,
    VERDICT_EVIDENCE_MAX_ENTRIES, VERDICT_SUMMARY_MAX_CHARS, VerdictWrite, now_ms,
    observation_projection, synthetic_observation_wants,
};
use uuid::Uuid;

use crate::budget::{
    BoundedError, DEFAULT_CALL_BUDGET_MS, ENVELOPE_MARGIN_MS, bounded, clamp_timeout,
    envelope_options,
};
use crate::convert::{
    action_outcome_from_wire, action_result_from_wire, asset_ref_to_wire, observation_from_wire,
    ui_snapshot_omission_from_wire,
};
use crate::error_map::{
    cancelled_before_dispatch, execute_terminal_from_rpc, provider_error_from_client, session_gone,
};
use crate::scan::{CallFate, SCAN_PAGE_LIMIT, latest_sequence, scan_for_call};

#[derive(Debug, Default)]
struct SessionState {
    /// Set by [`DeviceRailSession::end`]; every method except `health` and
    /// `reconcile` fails afterwards (04 §2.1).
    ended: bool,
    /// Most recently observed degradation reason (wire `session_degraded`
    /// message), surfaced by `health` (04 §2.1).
    degraded: Option<String>,
    /// callIds already dispatched through this session — a repeated callId
    /// is rejected; a retry is a new callId with a new WAL intent (04 §3).
    dispatched: HashSet<Uuid>,
    /// Highest event sequence this provider has delivered to the runner
    /// through `currentCursor` (ack-after-persist watermark, 04 §9.8.3).
    watermark: Option<EventSequence>,
}

/// One open DeviceRail session (see the module docs).
pub struct DeviceRailSession {
    client: DeviceRailClient,
    attestation: CapabilityAttestation,
    session_id: WireSessionId,
    state: Mutex<SessionState>,
}

impl std::fmt::Debug for DeviceRailSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceRailSession")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl DeviceRailSession {
    pub(crate) fn new(
        client: DeviceRailClient,
        attestation: CapabilityAttestation,
        session_id: WireSessionId,
    ) -> Self {
        DeviceRailSession {
            client,
            attestation,
            session_id,
            state: Mutex::new(SessionState::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SessionState> {
        self.state.lock().expect("session state lock poisoned")
    }

    fn ensure_active(&self, method: &str) -> Result<(), ProviderError> {
        if self.state().ended {
            return Err(session_gone(method));
        }
        Ok(())
    }

    fn feature_enabled(&self, feature: &str) -> bool {
        self.attestation
            .features_enabled
            .iter()
            .any(|enabled| enabled.as_str() == feature)
    }

    fn pre_cancelled(cancel: &Option<CancellationToken>) -> bool {
        cancel.as_ref().is_some_and(CancellationToken::is_cancelled)
    }

    /// Records a wire-reported degradation for later `health` probes.
    fn note_degradation(&self, error: &ClientError) {
        if let ClientError::RemoteRpc { error, .. } = error
            && error.data.code == "session_degraded"
        {
            self.state().degraded = Some(error.data.message.clone());
        }
    }

    /// Awaits an in-flight request, forwarding a cancellation intent to the
    /// daemon when the token fires (04 §7.1): cancellation is a request,
    /// not a guarantee — after `request.cancel` the call keeps waiting for
    /// whatever terminal actually materializes.
    async fn await_with_cancel<T: Send + 'static>(
        &self,
        handle: RequestHandle<T>,
        cancel: Option<CancellationToken>,
    ) -> Result<T, ClientError> {
        let request_id = handle.id().clone();
        let result = handle.result();
        let Some(token) = cancel else {
            return result.await;
        };
        let mut result = std::pin::pin!(result);
        tokio::select! {
            outcome = &mut result => outcome,
            () = token.cancelled() => {
                // Best-effort: the daemon may already be finalizing.
                let _ = self.client.cancel(request_id).await;
                result.await
            }
        }
    }

    /// One `events.list` page of the issuing session's log (04 §5 query
    /// discipline: only ever this session's log in the M1 single-session
    /// scenario), under the provider-local budget.
    async fn events_page(
        &self,
        after: Option<EventSequence>,
    ) -> Result<Vec<TestEvent>, BoundedError> {
        let params = EventsListParams {
            session_id: Some(self.session_id.clone()),
            after_sequence: after,
            limit: Some(SCAN_PAGE_LIMIT),
        };
        bounded(
            self.client
                .call::<methods::EventsList>(Some(params), CallOptions::default()),
        )
        .await
    }

    /// The 04 §9.2 pin: compile time guarantees `verdict.record.v1`
    /// rides every IR into `requiredFeatures`, so an un-negotiated
    /// session can never legitimately reach `recordVerdict` — reaching
    /// it IS capability drift (fail-closed, never a silent no-op). The
    /// runner turns the failure into the 04 §5 "remote archival failed"
    /// annotation; the local verdict is untouched either way.
    fn verdict_record_gate(negotiated: bool) -> Result<(), ProviderError> {
        if negotiated {
            return Ok(());
        }
        Err(ProviderError::new(
            ErrorClass::CapabilityDrift,
            "verdict.record.v1 was not negotiated in this session; the compiler \
             guarantees every IR requires it (04 §9.2) — reaching recordVerdict \
             without it is capability drift",
            RetryableSource::Classifier,
        ))
    }

    /// Derives the request-envelope budget for one `device.execute`
    /// (04 §9.7): an explicit `requestTimeoutMs` wins (after checking
    /// invariant 1); otherwise the envelope is the action budget plus the
    /// margin, so timeouts converge on the definite `timedOut` side.
    fn execute_budgets(
        call: &BoundActionCall,
    ) -> Result<(Option<u64>, Option<u64>), ProviderError> {
        match (call.action_timeout_ms, call.request_timeout_ms) {
            (Some(action), Some(envelope)) if action >= envelope => Err(ProviderError::new(
                ErrorClass::BindArgumentsInvalid,
                format!(
                    "budget invariant violated: actionTimeoutMs ({action}) must be strictly \
                     below the request envelope timeoutMs ({envelope}) (04 §9.7 invariant 1)"
                ),
                RetryableSource::Classifier,
            )),
            (action, Some(envelope)) => Ok((action, Some(envelope))),
            (Some(action), None) => Ok((
                Some(action),
                Some(action.saturating_add(ENVELOPE_MARGIN_MS)),
            )),
            (None, None) => Ok((None, None)),
        }
    }

    /// Serves a provider-synthetic observation action (04 §9.4.3).
    ///
    /// The runner's contract is unchanged — it called `execute` and gets an
    /// `ActionOutcome` back — but the wire path is `device.observe`, not
    /// `device.execute`. The action is readonly, so there is no WAL
    /// concern: a dangling readonly intent is always safe to replay, which
    /// is why these actions are exempt from reconcile by construction
    /// (spine §6.7-B).
    async fn execute_observation(
        &self,
        call: BoundActionCall,
        wants: Vec<ObserveWant>,
        cancel: Option<CancellationToken>,
    ) -> Result<ActionOutcome, ProviderError> {
        let started_at_ms = now_ms();
        let observation = self
            .observe(
                ObserveRequest {
                    wants: wants.clone(),
                },
                cancel,
            )
            .await?;
        let output = observation_projection(&observation, &wants);
        Ok(ActionOutcome::Succeeded {
            result: Box::new(ActionResult {
                call_id: call.call_id,
                started_at_ms,
                finished_at_ms: now_ms(),
                output,
                before: None,
                // The observation IS the act: recording it as the after
                // snapshot lets a following assertion verify against it
                // through the ordinary verify chain.
                after: Some(observation),
                evidence: Vec::new(),
                execution: None,
            }),
        })
    }
}

#[async_trait]
impl ProviderSession for DeviceRailSession {
    fn attestation(&self) -> &CapabilityAttestation {
        &self.attestation
    }

    async fn execute(
        &self,
        call: BoundActionCall,
        cancel: Option<CancellationToken>,
    ) -> Result<ActionOutcome, ProviderError> {
        self.ensure_active("execute")?;
        if Self::pre_cancelled(&cancel) {
            // The WAL intent is already written; reconcile will find
            // neverDispatched — safe (04 §7.1).
            return Err(cancelled_before_dispatch());
        }
        // Provider-synthetic observation actions (04 §9.4.3) route to
        // `device.observe` instead of `device.execute`. They are the
        // provider's own, so no driver declares them and they are absent
        // from the attestation — which is also the shadowing rule at run
        // time: a driver action of the same name IS attested and therefore
        // takes the normal path below, exactly as bind resolved it.
        if !self.attestation.actions.contains_key(&call.action_name)
            && let Some(wants) = synthetic_observation_wants(&call)?
        {
            return self.execute_observation(call, wants, cancel).await;
        }
        // Fail-closed on unattested actions, before any wire request (04 §3).
        if !self.attestation.actions.contains_key(&call.action_name) {
            return Err(ProviderError::new(
                ErrorClass::CapabilityDrift,
                format!(
                    "actionName `{}` is not in the attested action set; refusing to dispatch",
                    call.action_name
                ),
                RetryableSource::Classifier,
            ));
        }
        // The runner-generated callId is used verbatim as the substrate
        // action id (`device.execute` params.id) — never regenerated (04 §3).
        let call_id = Uuid::parse_str(&call.call_id).map_err(|error| {
            ProviderError::new(
                ErrorClass::BindArgumentsInvalid,
                format!("callId must be the runner-generated UUID: {error}"),
                RetryableSource::Classifier,
            )
        })?;
        let (action_ms, envelope_ms) = Self::execute_budgets(&call)?;
        if !self.state().dispatched.insert(call_id) {
            return Err(ProviderError::new(
                ErrorClass::BindArgumentsInvalid,
                format!(
                    "duplicate callId {call_id}: a retry is a new callId with a new WAL intent \
                     (04 §3)"
                ),
                RetryableSource::Classifier,
            ));
        }

        let params = DeviceExecuteParams {
            id: call_id,
            name: call.action_name.as_str().to_owned(),
            arguments: call.arguments,
            action_timeout_ms: action_ms.map(clamp_timeout),
        };
        let options = CallOptions {
            timeout_ms: envelope_ms.map(clamp_timeout),
        };
        let handle = self
            .client
            .begin_call::<methods::DeviceExecute>(params, options)
            .map_err(|error| provider_error_from_client(error, "device.execute"))?;

        match self.await_with_cancel(handle, cancel).await {
            // RPC success only ever carries the succeeded terminal.
            Ok(result) => Ok(ActionOutcome::Succeeded {
                result: Box::new(action_result_from_wire(&result)),
            }),
            Err(error) => {
                // Non-succeeded terminals arrive as RPC errors with the
                // durable-terminal shield behind them; return them
                // unfolded and untranslated (04 §3).
                if let ClientError::RemoteRpc { error: rpc, .. } = &error
                    && let Some(outcome) = execute_terminal_from_rpc(rpc)
                {
                    return Ok(outcome);
                }
                // No terminal could be obtained: the runner records the
                // attempt as hanging and goes through reconcile (04 §3).
                self.note_degradation(&error);
                Err(provider_error_from_client(error, "device.execute"))
            }
        }
    }

    async fn observe(
        &self,
        req: ObserveRequest,
        cancel: Option<CancellationToken>,
    ) -> Result<Observation, ProviderError> {
        self.ensure_active("observe")?;
        if Self::pre_cancelled(&cancel) {
            return Err(cancelled_before_dispatch());
        }
        // `device.observe` takes no params; `wants` is an intent
        // declaration, not a wire parameter (04 §4.1).
        let handle = self
            .client
            .begin_call::<methods::DeviceObserve>(
                methods::NoParams,
                envelope_options(DEFAULT_CALL_BUDGET_MS),
            )
            .map_err(|error| provider_error_from_client(error, "device.observe"))?;
        let wire_observation = self
            .await_with_cancel(handle, cancel)
            .await
            .map_err(|error| {
                self.note_degradation(&error);
                provider_error_from_client(error, "device.observe")
            })?;
        let mut observation = observation_from_wire(&wire_observation);

        // Truthful omission back-fill (04 §4.1): when a wanted part is
        // missing and the daemon made no claim, record why it can
        // legitimately be absent — omission is data, not an error.
        if req.wants.contains(&ObserveWant::Screenshot)
            && observation.screenshot.is_none()
            && observation.screenshot_omission.is_none()
        {
            // The daemon's capture policy decided not to produce one.
            observation.screenshot_omission = Some(ScreenshotOmissionReason::Policy);
        }
        if req.wants.contains(&ObserveWant::UiSnapshot)
            && observation.ui_snapshot.is_none()
            && observation.ui_snapshot_omission.is_none()
        {
            // No snapshot and no claim: the driver has no semantic UI
            // channel for this observation.
            observation.ui_snapshot_omission = Some(UiSnapshotOmissionReason::DriverUnsupported);
        }
        Ok(observation)
    }

    async fn ui_snapshot(&self, observation_id: &str) -> Result<UiSnapshotOutcome, ProviderError> {
        self.ensure_active("uiSnapshot")?;
        // Fail-closed feature gate (04 §9.2 adjudication): compile-time
        // promises make an un-negotiated `observation.uiSnapshot.v1`
        // unreachable — reaching it is drift, not a soft omission.
        if !self.feature_enabled(feature::OBSERVATION_UI_SNAPSHOT_V1) {
            return Err(ProviderError::new(
                ErrorClass::CapabilityDrift,
                "ui.snapshot.get requires observation.uiSnapshot.v1, which this session did \
                 not negotiate (04 §9.2: reaching this is capability drift)",
                RetryableSource::Classifier,
            ));
        }
        let observation_id = Uuid::parse_str(observation_id).map_err(|error| {
            ProviderError::new(
                ErrorClass::BindArgumentsInvalid,
                format!("observationId must be the provider-issued UUID: {error}"),
                RetryableSource::Classifier,
            )
        })?;
        let call = self.client.call::<methods::UiSnapshotGet>(
            UiSnapshotGetParams { observation_id },
            CallOptions::default(),
        );
        match bounded(call).await {
            Ok(snapshot) => Ok(UiSnapshotOutcome::Available {
                // M0-narrow SPI shape: the normalized tree as raw JSON.
                snapshot: serde_json::to_value(snapshot).map_err(|error| {
                    ProviderError::new(
                        ErrorClass::TransportLost,
                        format!("ui.snapshot.get result failed to re-serialize: {error}"),
                        RetryableSource::Classifier,
                    )
                })?,
            }),
            // The daemon's typed "no snapshot on this observation" reply
            // carries the observation's omission reason — a typed omission,
            // not an error (04 §4.2).
            Err(BoundedError::Client(ClientError::RemoteRpc { error, .. }))
                if error.data.code == "ui_snapshot_unavailable" =>
            {
                let reason = error
                    .data
                    .details
                    .as_ref()
                    .and_then(|details| details.get("omissionReason"))
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                    .map(ui_snapshot_omission_from_wire)
                    .unwrap_or(UiSnapshotOmissionReason::DriverUnsupported);
                Ok(UiSnapshotOutcome::Unavailable { reason })
            }
            Err(error) => Err(error.into_provider_error("ui.snapshot.get")),
        }
    }

    async fn reconcile(
        &self,
        call_id: &str,
        issuing: &EventCursor,
    ) -> Result<ReconcileResult, ProviderError> {
        // Cross-generation guard (04 §5, 2026-07-18 incorporation): the
        // scan below reads THIS session's log. A foreign issuing
        // credential must answer logUnavailable — scanning the current
        // session's (reachable but wrong) log would fabricate
        // `neverDispatched` and license an auto-replay. True old-log
        // retrieval (sessions.list + events over a dead session) is
        // provider-milestone work (v0.2).
        if issuing.session_id != self.session_id.to_string() {
            return Ok(ReconcileResult::LogUnavailable {
                reason: format!(
                    "the issuing session {} is not this session ({}); cross-generation \
                     log retrieval is not implemented in v0.1",
                    issuing.session_id, self.session_id
                ),
            });
        }
        // An ended/broken session cannot reach the issuing session's log
        // through this connection any more: always the uncertain branch,
        // never a guess (04 §5).
        if self.state().ended {
            return Ok(ReconcileResult::LogUnavailable {
                reason: "the provider session has ended; the issuing session's event log is \
                         no longer reachable through this connection"
                    .to_owned(),
            });
        }
        // The issuing session must still be indexed — `neverDispatched`
        // requires having read its *complete* event range; a deleted or
        // cleared log (events.clear, daemon restart) is always
        // logUnavailable (04 §5).
        let sessions = match bounded(
            self.client
                .call::<methods::SessionsList>(methods::NoParams, CallOptions::default()),
        )
        .await
        {
            Ok(sessions) => sessions,
            Err(error) => {
                return Ok(ReconcileResult::LogUnavailable {
                    reason: format!("the daemon's session index is unreadable: {error}"),
                });
            }
        };
        if !sessions.iter().any(|session| session.id == self.session_id) {
            return Ok(ReconcileResult::LogUnavailable {
                reason: format!(
                    "issuing session {} is absent from the daemon's session index (event log \
                     cleared or deleted)",
                    self.session_id
                ),
            });
        }
        // A non-UUID callId can never have been dispatched (`device.execute`
        // ids are UUIDs); the log is readable, so no-trace is definite.
        let Ok(call_uuid) = Uuid::parse_str(call_id) else {
            return Ok(ReconcileResult::NeverDispatched);
        };
        let floor = EventSequence::new(issuing.last_sequence);
        match scan_for_call(call_uuid, floor, |after| self.events_page(after)).await {
            // A recorded terminal is a certain fate whatever its four-way
            // discriminant: adopted verbatim, never demoted (04 §5).
            Ok(CallFate::Completed(outcome)) => Ok(ReconcileResult::Completed {
                outcome: Box::new(action_outcome_from_wire(&outcome)),
            }),
            Ok(CallFate::StartedNoTerminal) => Ok(ReconcileResult::StartedNoTerminal),
            Ok(CallFate::NoTrace) => Ok(ReconcileResult::NeverDispatched),
            Err(error) => Ok(ReconcileResult::LogUnavailable {
                reason: format!("event log scan failed mid-range: {error}"),
            }),
        }
    }

    async fn fetch_evidence(
        &self,
        asset: &pointlock_ir::AssetRef,
    ) -> Result<EvidenceStream, ProviderError> {
        self.ensure_active("fetchEvidence")?;
        // Honest unsupported (04 §4.3 intent vs wire reality): DeviceRail
        // asset URIs use the `devicerail://assets/sha256/<digest>` scheme,
        // the NDJSON control plane carries no asset bytes, and
        // `devicerail-client` exposes no fetch API (`media.stream.capture`
        // creates new frames; it cannot dereference an existing AssetRef).
        // Reported as a wire/client gap — evidence localization needs an
        // asset byte channel.
        let _ = asset_ref_to_wire(asset);
        Err(ProviderError::new(
            ErrorClass::ActionFailedFinal,
            format!(
                "fetchEvidence is unsupported by the DeviceRail wire surface: no byte channel \
                 exists for asset URI `{}` (devicerail:// assets are daemon-internal; \
                 devicerail-client has no fetch API)",
                asset.uri
            ),
            RetryableSource::Classifier,
        ))
    }

    async fn record_verdict(&self, verdict: VerdictWrite) -> Result<(), ProviderError> {
        self.ensure_active("recordVerdict")?;
        // Wire hard caps, fail-closed before any request — compaction is
        // the runner's report-assembly job (04 §5).
        let summary_chars = verdict.summary.chars().count();
        if summary_chars > VERDICT_SUMMARY_MAX_CHARS {
            return Err(ProviderError::new(
                ErrorClass::BindArgumentsInvalid,
                format!(
                    "verdict summary is {summary_chars} chars; the wire cap is \
                     {VERDICT_SUMMARY_MAX_CHARS} (fail-closed, 04 §5)"
                ),
                RetryableSource::Classifier,
            ));
        }
        if verdict.evidence.len() > VERDICT_EVIDENCE_MAX_ENTRIES {
            return Err(ProviderError::new(
                ErrorClass::BindArgumentsInvalid,
                format!(
                    "verdict cites {} evidence entries; the wire cap is \
                     {VERDICT_EVIDENCE_MAX_ENTRIES} (fail-closed, 04 §5)",
                    verdict.evidence.len()
                ),
                RetryableSource::Classifier,
            ));
        }
        Self::verdict_record_gate(self.feature_enabled(feature::VERDICT_RECORD_V1))?;
        let params = VerdictRecordParams {
            verdict: WireVerdict {
                status: match verdict.status {
                    VerdictStatus::Pass => WireVerdictStatus::Pass,
                    VerdictStatus::Fail => WireVerdictStatus::Fail,
                    VerdictStatus::Unknown => WireVerdictStatus::Unknown,
                },
                summary: verdict.summary,
                evidence: verdict.evidence.iter().map(asset_ref_to_wire).collect(),
            },
        };
        bounded(
            self.client
                .call::<methods::VerdictRecord>(params, CallOptions::default()),
        )
        .await
        .map(|_receipt| ())
        .map_err(|error| error.into_provider_error("verdict.record"))
    }

    async fn current_cursor(&self) -> Result<EventCursor, ProviderError> {
        self.ensure_active("currentCursor")?;
        // v0.1 watermark inference: follow `events.list` to the log's end
        // (04 §5; no event stream in v0.1, §9.8 scope note). What is
        // returned here is by construction what has been delivered to the
        // runner — ack-after-persist (04 §9.8.3).
        let from = self.state().watermark;
        let latest = latest_sequence(from, |after| self.events_page(after))
            .await
            .map_err(|error| error.into_provider_error("events.list"))?;
        if let Some(sequence) = latest {
            self.state().watermark = Some(sequence);
        }
        Ok(EventCursor {
            session_id: self.session_id.to_string(),
            last_sequence: latest.map(EventSequence::get).unwrap_or(0),
        })
    }

    async fn health(&self) -> Result<SessionHealth, ProviderError> {
        // Must not fail on a broken session (04 §2.1).
        let (ended, degraded) = {
            let state = self.state();
            (state.ended, state.degraded.clone())
        };
        if ended {
            return Ok(SessionHealth {
                ok: false,
                degraded,
            });
        }
        let probe = bounded(
            self.client
                .call::<methods::SessionCurrent>(methods::NoParams, CallOptions::default()),
        )
        .await;
        Ok(match probe {
            Ok(current) if current.session_id == self.session_id => {
                SessionHealth { ok: true, degraded }
            }
            Ok(current) => SessionHealth {
                ok: false,
                degraded: Some(format!(
                    "daemon's active session {} is not the bound session {}",
                    current.session_id, self.session_id
                )),
            },
            Err(error) => SessionHealth {
                ok: false,
                degraded: Some(error.to_string()),
            },
        })
    }

    async fn end(
        &self,
        outcome: SessionOutcome,
        reason: Option<String>,
    ) -> Result<(), ProviderError> {
        // Idempotent: ending an ended/broken session is a no-op (04 §2.1).
        {
            let mut state = self.state();
            if state.ended {
                return Ok(());
            }
            state.ended = true;
        }
        // Best-effort exit protocol (04 §9.1, spawn form): session.end →
        // device.disconnect → close (stdin EOF → grace → kill). Transport
        // failures must not block the runner's teardown.
        let params = SessionEndParams {
            outcome: Some(match outcome {
                SessionOutcome::Completed => WireSessionOutcome::Completed,
                SessionOutcome::Failed => WireSessionOutcome::Failed,
                SessionOutcome::Cancelled => WireSessionOutcome::Cancelled,
                SessionOutcome::Shutdown => WireSessionOutcome::Shutdown,
            }),
            reason,
        };
        let _ = bounded(
            self.client
                .call::<methods::SessionEnd>(Some(params), CallOptions::default()),
        )
        .await;
        let _ = bounded(self.client.call::<methods::DeviceDisconnect>(
            methods::NoParams,
            envelope_options(DEFAULT_CALL_BUDGET_MS),
        ))
        .await;
        let _ = self.client.close().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pointlock_ir::ActionName;
    use serde_json::json;

    fn call(action_ms: Option<u64>, request_ms: Option<u64>) -> BoundActionCall {
        BoundActionCall {
            call_id: Uuid::new_v4().to_string(),
            action_name: ActionName::new("tap").unwrap(),
            arguments: json!({}),
            action_timeout_ms: action_ms,
            request_timeout_ms: request_ms,
        }
    }

    #[test]
    fn record_verdict_without_the_feature_is_capability_drift() {
        // 04 §9.2: never a silent no-op — the compile-time guarantee
        // makes this arm unreachable, so reaching it is drift.
        let error = DeviceRailSession::verdict_record_gate(false).expect_err("drift");
        assert_eq!(error.error_class, ErrorClass::CapabilityDrift);
        assert!(error.message.contains("verdict.record.v1"));
        // Negative control: a negotiated session passes the gate.
        DeviceRailSession::verdict_record_gate(true).expect("negotiated");
    }

    #[test]
    fn envelope_budget_is_derived_from_the_action_budget() {
        // 04 §9.7: envelope = action + margin, keeping expiry on the
        // definite-terminal side.
        let (action, envelope) =
            DeviceRailSession::execute_budgets(&call(Some(15_000), None)).unwrap();
        assert_eq!(action, Some(15_000));
        assert_eq!(envelope, Some(20_000));

        // Explicit envelope wins.
        let (action, envelope) =
            DeviceRailSession::execute_budgets(&call(Some(1_000), Some(30_000))).unwrap();
        assert_eq!(action, Some(1_000));
        assert_eq!(envelope, Some(30_000));

        // No budgets: nothing is sent (the step watchdog still covers).
        let (action, envelope) = DeviceRailSession::execute_budgets(&call(None, None)).unwrap();
        assert_eq!(action, None);
        assert_eq!(envelope, None);
    }

    #[test]
    fn budget_invariant_one_fails_closed() {
        // actionTimeoutMs must be strictly below the envelope.
        for (action, envelope) in [(5_000, 5_000), (6_000, 5_000)] {
            let error = DeviceRailSession::execute_budgets(&call(Some(action), Some(envelope)))
                .expect_err("invariant 1");
            assert_eq!(error.error_class, ErrorClass::BindArgumentsInvalid);
        }
    }
}
