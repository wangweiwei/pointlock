//! Paged scans over one session's append-only event log
//! (`events.list { sessionId, afterSequence, limit }`; 04 §5 / §9.8).
//!
//! `EventSequence` is one-based: the initial watermark ("nothing consumed
//! yet") is expressed by omitting `afterSequence` — `afterSequence: 0` is
//! not representable on the wire (04 §9.8.3 M1 note; mind the off-by-one).
//!
//! The reconcile correlation key is split by payload type (04 §5, schema
//! verified): `actionStarted` matches `payload.call.id == callId` (the
//! payload nests a full `RecordedActionCall`), `actionCompleted` matches
//! `payload.callId == callId` (no call object; the terminal is read from
//! `payload.outcome`).

use devicerail_client::protocol::{
    ActionOutcome as WireActionOutcome, EventSequence, MAX_EVENTS_LIST_PAGE_SIZE, TestEvent,
    TestEventPayload,
};
use uuid::Uuid;

/// Page size for every scan (the wire maximum).
pub(crate) const SCAN_PAGE_LIMIT: u32 = MAX_EVENTS_LIST_PAGE_SIZE;

/// The fate of one callId inside a fully scanned event range.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CallFate {
    /// An `actionCompleted` terminal is on record (the four-way outcome,
    /// verbatim).
    Completed(WireActionOutcome),
    /// `actionStarted` with no terminal (theoretically shielded against).
    StartedNoTerminal,
    /// The complete range shows no trace of the callId.
    NoTrace,
}

/// Scans the issuing session's full event range for `call_id`, following
/// pagination until a short or empty page. The page fetcher receives the
/// `afterSequence` cursor (`None` = from the beginning, one-based note
/// above).
pub(crate) async fn scan_for_call<F, Fut, E>(
    call_id: Uuid,
    floor: Option<EventSequence>,
    mut fetch_page: F,
) -> Result<CallFate, E>
where
    F: FnMut(Option<EventSequence>) -> Fut,
    Fut: Future<Output = Result<Vec<TestEvent>, E>>,
{
    // 04 §5: the scan starts from the issuing credential's watermark —
    // the intent's `actionIntent` necessarily postdates it, so events at
    // or below the floor cannot carry this call.
    let mut after: Option<EventSequence> = floor;
    let mut started = false;
    loop {
        let page = fetch_page(after).await?;
        for event in &page {
            match &event.payload {
                TestEventPayload::ActionStarted { call } if call.id == call_id => {
                    started = true;
                }
                TestEventPayload::ActionCompleted {
                    call_id: completed_id,
                    outcome,
                } if *completed_id == call_id => {
                    return Ok(CallFate::Completed(outcome.clone()));
                }
                _ => {}
            }
        }
        match page.last() {
            None => break,
            Some(last) => after = Some(last.sequence),
        }
        // A short page means the range is exhausted (protocol docs:
        // "advance afterSequence ... until a short or empty page").
        if page.len() < SCAN_PAGE_LIMIT as usize {
            break;
        }
    }
    Ok(if started {
        CallFate::StartedNoTerminal
    } else {
        CallFate::NoTrace
    })
}

/// Follows the log to its end from `from` and returns the highest sequence
/// seen (or `from` when no newer events exist). Used by `currentCursor`
/// (04 §5: v0.1 infers the watermark from the latest `events.list` page).
pub(crate) async fn latest_sequence<F, Fut, E>(
    from: Option<EventSequence>,
    mut fetch_page: F,
) -> Result<Option<EventSequence>, E>
where
    F: FnMut(Option<EventSequence>) -> Fut,
    Fut: Future<Output = Result<Vec<TestEvent>, E>>,
{
    let mut after = from;
    loop {
        let page = fetch_page(after).await?;
        let Some(last) = page.last() else {
            return Ok(after);
        };
        after = Some(last.sequence);
        if page.len() < SCAN_PAGE_LIMIT as usize {
            return Ok(after);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use devicerail_client::ClientError;
    use devicerail_client::protocol::{ErrorInfo, EventId, RecordedActionCall, SessionId};
    use serde_json::json;

    use super::*;

    fn event(sequence: u64, payload: TestEventPayload) -> TestEvent {
        TestEvent {
            event_id: EventId::new(),
            session_id: SessionId::from(Uuid::nil()),
            sequence: EventSequence::new(sequence).expect("one-based sequence"),
            request_id: None,
            device_id: None,
            at_ms: sequence,
            payload,
        }
    }

    fn started(sequence: u64, call_id: Uuid) -> TestEvent {
        event(
            sequence,
            TestEventPayload::ActionStarted {
                call: RecordedActionCall {
                    id: call_id,
                    name: "tap".to_owned(),
                    arguments: json!({ "x": 1, "y": 2 }),
                    arguments_redacted: false,
                },
            },
        )
    }

    fn completed(sequence: u64, call_id: Uuid) -> TestEvent {
        event(
            sequence,
            TestEventPayload::ActionCompleted {
                call_id,
                outcome: WireActionOutcome::Failed {
                    error: ErrorInfo {
                        code: "element_not_found".to_owned(),
                        message: "not found".to_owned(),
                        retryable: false,
                        details: None,
                    },
                },
            },
        )
    }

    /// One scripted page fetch: an immediately-ready `events.list` result.
    type ReadyPage = std::future::Ready<Result<Vec<TestEvent>, ClientError>>;

    /// The `afterSequence` cursors a scripted pager was asked for.
    type CursorLog = Arc<Mutex<Vec<Option<u64>>>>;

    /// A scripted pager: serves `pages` in order, records the cursors it
    /// was asked for, then keeps serving empty pages.
    fn pager(
        pages: Vec<Vec<TestEvent>>,
    ) -> (impl FnMut(Option<EventSequence>) -> ReadyPage, CursorLog) {
        let cursors = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&cursors);
        let mut pages = pages.into_iter();
        (
            move |after: Option<EventSequence>| {
                observed
                    .lock()
                    .expect("cursor log")
                    .push(after.map(EventSequence::get));
                std::future::ready(Ok(pages.next().unwrap_or_default()))
            },
            cursors,
        )
    }

    /// A full page of `SCAN_PAGE_LIMIT` filler events starting at `base`.
    fn full_filler_page(base: u64) -> Vec<TestEvent> {
        (0..u64::from(SCAN_PAGE_LIMIT))
            .map(|offset| event(base + offset, TestEventPayload::SessionStarted))
            .collect()
    }

    #[tokio::test]
    async fn scan_follows_pagination_and_advances_the_one_based_cursor() {
        let call_id = Uuid::new_v4();
        let mut second_page = full_filler_page(1001);
        second_page.push(started(2001, call_id));
        let third_page = vec![completed(2002, call_id)];
        let (fetch, cursors) = pager(vec![full_filler_page(1), second_page, third_page]);

        let fate = scan_for_call(call_id, None, fetch).await.expect("scan");
        assert!(matches!(fate, CallFate::Completed(_)));
        // Initial watermark is None (afterSequence 0 is unrepresentable);
        // subsequent cursors are the last sequence of each full page.
        assert_eq!(*cursors.lock().unwrap(), vec![None, Some(1000), Some(2001)]);
    }

    #[tokio::test]
    async fn completed_terminal_is_adopted_verbatim() {
        let call_id = Uuid::new_v4();
        let (fetch, _) = pager(vec![vec![started(1, call_id), completed(2, call_id)]]);
        let fate = scan_for_call(call_id, None, fetch).await.expect("scan");
        let CallFate::Completed(outcome) = fate else {
            panic!("expected completed, got {fate:?}");
        };
        let WireActionOutcome::Failed { error } = outcome else {
            panic!("archived failed terminal must come back unfolded");
        };
        assert_eq!(error.code, "element_not_found");
    }

    #[tokio::test]
    async fn started_without_terminal_and_no_trace_are_distinguished() {
        let call_id = Uuid::new_v4();
        let (fetch, _) = pager(vec![vec![started(1, call_id)]]);
        assert_eq!(
            scan_for_call(call_id, None, fetch).await.expect("scan"),
            CallFate::StartedNoTerminal
        );

        let (fetch, _) = pager(vec![vec![
            started(1, Uuid::new_v4()),
            completed(2, Uuid::new_v4()),
        ]]);
        assert_eq!(
            scan_for_call(call_id, None, fetch).await.expect("scan"),
            CallFate::NoTrace
        );
    }

    #[tokio::test]
    async fn scan_errors_propagate() {
        let call_id = Uuid::new_v4();
        let failing = |_: Option<EventSequence>| {
            std::future::ready(Err::<Vec<TestEvent>, _>(ClientError::Closed))
        };
        assert!(scan_for_call(call_id, None, failing).await.is_err());
    }

    #[tokio::test]
    async fn latest_sequence_follows_to_the_end() {
        let (fetch, cursors) = pager(vec![
            full_filler_page(1),
            vec![event(1001, TestEventPayload::SessionStarted)],
        ]);
        let latest = latest_sequence(None, fetch).await.expect("scan");
        assert_eq!(latest.map(EventSequence::get), Some(1001));
        assert_eq!(*cursors.lock().unwrap(), vec![None, Some(1000)]);

        // No newer events: the input watermark is preserved.
        let (fetch, _) = pager(vec![]);
        let unchanged = latest_sequence(EventSequence::new(7), fetch)
            .await
            .expect("scan");
        assert_eq!(unchanged.map(EventSequence::get), Some(7));
    }
}
