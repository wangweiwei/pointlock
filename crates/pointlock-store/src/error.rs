//! Error types of the store: fold-level (pure, structural), store-level
//! (SQLite / IO / serialization plus the fold errors they wrap), and the
//! typed rejections of the human-response arbitration (06 §4.3).

use std::fmt;

/// A structural violation detected while folding a RunLog into a
/// [`pointlock_ir::CheckpointView`].
///
/// The fold is deliberately *not* lenient about impossible sequences
/// (M0 iron rule: events the fold does not understand or cannot anchor are
/// surfaced, never silently ignored) — a fold error inside
/// [`crate::Store::append_event`] rolls the whole append back, so a log that
/// cannot fold is never persisted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FoldError {
    /// An event in the fold input belongs to a different run.
    #[error("event at seq {seq} belongs to run {actual}, fold input is for run {expected}")]
    RunIdMismatch {
        /// Sequence number of the offending event.
        seq: u64,
        /// The run the fold input is for.
        expected: String,
        /// The run the event claims.
        actual: String,
    },
    /// Event sequence numbers must be strictly increasing.
    #[error("event seq {seq} does not increase monotonically (previous {prev})")]
    NonMonotonicSeq {
        /// The previous sequence number.
        prev: u64,
        /// The offending sequence number.
        seq: u64,
    },
    /// Any event other than `runStarted` arrived before `runStarted`.
    #[error("event {event_type} at seq {seq} precedes runStarted")]
    EventBeforeRunStarted {
        /// Sequence number of the offending event.
        seq: u64,
        /// Wire discriminant of the offending event.
        event_type: &'static str,
    },
    /// A second `runStarted` was appended (later segments start with
    /// `runResumed`).
    #[error("duplicate runStarted at seq {seq} (later segments start with runResumed)")]
    DuplicateRunStarted {
        /// Sequence number of the offending event.
        seq: u64,
    },
    /// A step-scoped event arrived with no in-flight step to anchor to.
    #[error("{event_type} at seq {seq} has no in-flight step (missing stepEntered)")]
    EventOutsideStep {
        /// Sequence number of the offending event.
        seq: u64,
        /// Wire discriminant of the offending event.
        event_type: &'static str,
    },
    /// `stepExited` arrived without a matching `stepEntered`.
    #[error("stepExited at seq {seq} without a matching stepEntered")]
    StepExitedWithoutEntry {
        /// Sequence number of the offending event.
        seq: u64,
    },
    /// `verdictRecorded` targets neither the in-flight step nor any
    /// completed record at its run path.
    #[error("verdictRecorded at seq {seq} targets neither the in-flight step nor a completed step")]
    VerdictWithoutTarget {
        /// Sequence number of the offending event.
        seq: u64,
    },
    /// `callFramePopped` would pop the root flow frame (or an empty stack).
    #[error("callFramePopped at seq {seq} would pop the root frame")]
    PoppedRootFrame {
        /// Sequence number of the offending event.
        seq: u64,
    },
    /// `stepExited` with no active call frame to advance (structurally
    /// impossible after a well-formed `runStarted`).
    #[error("stepExited at seq {seq} with no active call frame")]
    NoActiveFrame {
        /// Sequence number of the offending event.
        seq: u64,
    },
    /// A `callFramePushed` marked `rebase` names a stack level that is not
    /// open (07 §5.2: a rebase re-enters an already-open frame; it never
    /// creates one).
    #[error(
        "callFramePushed(rebase) at seq {seq} targets frame level {level}, stack depth is {depth}"
    )]
    RebaseWithoutFrame {
        /// Sequence number of the offending event.
        seq: u64,
        /// The stack level the event's run path addresses.
        level: usize,
        /// The number of frames currently open.
        depth: usize,
    },
    /// `humanResponded` pairs no pending request.
    #[error("humanResponded at seq {seq} pairs no pending request (requestId {request_id})")]
    UnpairedHumanResponse {
        /// Sequence number of the offending event.
        seq: u64,
        /// The unpaired request id.
        request_id: String,
    },
}

/// Store-level error: SQLite / filesystem / serialization failures, fold
/// errors surfaced through the write path, and the self-check verdicts of
/// [`crate::Store::verify_checkpoint`].
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem error (evidence area, store directory).
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization error.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    /// The referenced run does not exist.
    #[error("unknown run {0}")]
    UnknownRun(String),
    /// `begin_run` was called with a run id that already exists.
    #[error("run {0} already exists")]
    DuplicateRun(String),
    /// The run has no materialized checkpoint row yet (no events appended).
    #[error("run {0} has no materialized checkpoint")]
    NoCheckpoint(String),
    /// A fold error (see [`FoldError`] for the rollback semantics on the
    /// write path).
    #[error(transparent)]
    Fold(#[from] FoldError),
    /// The checkpoint row lags the log head — rule 2 of 07 §3.3 (same-
    /// transaction materialization) was violated; a store-layer bug.
    #[error(
        "checkpoint for run {run_id} is stale: materialized at seq \
         {materialized_seq}, log head is {log_seq}"
    )]
    StaleCheckpoint {
        /// The run whose checkpoint is stale.
        run_id: String,
        /// `checkpoint.log_seq` as stored.
        materialized_seq: u64,
        /// The actual `MAX(seq)` of the run's log.
        log_seq: u64,
    },
    /// The materialized view differs from the full-log refold — I1's
    /// runtime self-check tripped; a store-layer bug (07 §3.3).
    #[error(
        "materialized checkpoint for run {run_id} (log_seq {log_seq}) differs from the rebuilt fold"
    )]
    CheckpointMismatch {
        /// The run whose checkpoint mismatches.
        run_id: String,
        /// The `log_seq` the stored view claims.
        log_seq: u64,
        /// Canonical JSON of the stored view.
        materialized: String,
        /// Canonical JSON of the rebuilt view.
        rebuilt: String,
    },
    /// The `run.status` column differs from the folded status.
    #[error("run {run_id} status '{stored}' differs from folded status '{folded}'")]
    StatusMismatch {
        /// The run whose status mismatches.
        run_id: String,
        /// `run.status` as stored.
        stored: String,
        /// Status produced by the fold.
        folded: String,
    },
    /// A stored row failed to parse back into its typed shape.
    #[error("corrupt stored data for run {run_id}: {reason}")]
    Corrupt {
        /// The run whose stored data is corrupt.
        run_id: String,
        /// What failed to parse.
        reason: String,
    },
    /// [`crate::Store::submit_human_response`] refused the response.
    /// Typed and side-effect free: a rejected response never becomes a
    /// `humanResponded` event (06 §4.3 — bad data does not enter the
    /// ledger).
    #[error("human response for request {request_id} of run {run_id} rejected: {reason}")]
    HumanResponseRejected {
        /// The run the response targeted.
        run_id: String,
        /// The request the response tried to pair with.
        request_id: String,
        /// Why the arbitration refused it.
        reason: HumanResponseRejection,
    },
    /// A locate/dossier query referenced a step instance the ledger never
    /// entered (spine §9: locate resolves recorded instances only).
    #[error("run {run_id} has no step instance at '{path}'")]
    UnknownStepInstance {
        /// The queried run.
        run_id: String,
        /// The canonical path (or bare step id) that failed to resolve.
        path: String,
    },
    /// A bare step id matched several instances (iterations/hook entries);
    /// the caller must pick one canonical path.
    #[error("step '{step}' of run {run_id} is ambiguous; candidates: {}", candidates.join(", "))]
    AmbiguousStep {
        /// The queried run.
        run_id: String,
        /// The bare step id.
        step: String,
        /// Canonical strings of every matching instance.
        candidates: Vec<String>,
    },
    /// A canonical run-path string failed to parse (spine §9 grammar).
    #[error("run path '{input}' does not parse: {message}")]
    BadRunPath {
        /// The offending input.
        input: String,
        /// Parser message with offset context.
        message: String,
    },
}

/// The closed rejection vocabulary of the human-response arbitration
/// (06 §4.3: `unknownRequest | alreadyResponded | deadlineExceeded |
/// schemaViolation`, plus the lazily-settled leftover).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HumanResponseRejection {
    /// No `humanRequested` event carries this request id.
    UnknownRequest,
    /// A final response is already paired (first response wins; a
    /// supervision `suspend` answer is non-final and does not pair).
    AlreadyResponded,
    /// The response arrived after the request's absolute deadline, judged
    /// by the store-receipt clock — the only timeout judge (06 §4.3 rule
    /// 2). Lazy settlement of the expired request stays the runner's job;
    /// the arbitration only refuses the late response.
    DeadlineExpired {
        /// The request's absolute deadline (ms since epoch).
        deadline_at_ms: u64,
        /// When the store received the response (ms since epoch).
        received_at_ms: u64,
    },
    /// The request is no longer pending (its step was already settled).
    Settled,
    /// The response payload does not match the shape the request's
    /// purpose/mode demands (includes `outputSchema` violations of
    /// `provideInput` and out-of-vocabulary decisions).
    InvalidShape {
        /// What exactly is wrong, human-readable.
        reason: String,
    },
}

impl fmt::Display for HumanResponseRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HumanResponseRejection::UnknownRequest => write!(f, "unknown request"),
            HumanResponseRejection::AlreadyResponded => {
                write!(f, "already responded (first response wins)")
            }
            HumanResponseRejection::DeadlineExpired {
                deadline_at_ms,
                received_at_ms,
            } => write!(
                f,
                "deadline expired (deadlineAtMs {deadline_at_ms}, received at {received_at_ms})"
            ),
            HumanResponseRejection::Settled => {
                write!(f, "the request is no longer pending (already settled)")
            }
            HumanResponseRejection::InvalidShape { reason } => {
                write!(f, "invalid response shape: {reason}")
            }
        }
    }
}
