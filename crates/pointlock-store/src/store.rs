//! The single-writer SQLite store: RunLog append path, same-transaction
//! checkpoint materialization, and the content-addressed evidence area.
//!
//! ## Durability & consistency rules (07 §3.3, verbatim discipline)
//!
//! 1. **actionIntent WAL**: [`Store::write_action_intent`] appends in its
//!    own transaction; under `journal_mode=WAL` + `synchronous=FULL` a
//!    returned commit *is* the fsync — callers MUST call it (and see it
//!    return) before `provider.execute` dispatch.
//! 2. **Same-transaction materialization**: [`Store::append_event`] updates
//!    the `checkpoint` row in the same transaction as the `run_log` insert;
//!    a read checkpoint is always consistent with an exact `log_seq`.
//! 3. **file-before-row-before-log**: [`Store::put_evidence`] writes and
//!    fsyncs the evidence bytes before inserting the `evidence` row; the
//!    RunLog event referencing the evidence is appended by the caller
//!    afterwards. A crash leaves at most an orphan file (GC-able), never a
//!    log that references missing evidence.
//! 4. **Append-only**: there is no API that UPDATEs or DELETEs `run_log`
//!    rows — the guarantee is structural, not procedural.
//!
//! ## Single writer
//!
//! `Store` owns its [`rusqlite::Connection`] (which is `!Sync`); exactly one
//! `Store` instance must perform writes for a given store directory. WAL
//! mode lets concurrently opened read-only inspectors (e.g.
//! `pointlock inspect`) read without blocking the writer.

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use pointlock_ir::{
    BindingState, CheckpointView, FlowId, Hash, HumanMode, HumanPurpose, JsonSchemaDocument,
    RunLogEvent, RunLogPayload, RunPath, to_canonical_json,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;

use crate::error::{HumanResponseRejection, StoreError};
use crate::fold::{FoldState, FoldedRun, RunMeta, RunStatus, fold_checkpoint, fold_state};

/// Schema DDL. Follows 07 §3.3 verbatim, with one documented divergence:
/// the `run` table carries a `binding` column (canonical `BindingState`
/// JSON) — spine §6.1 defines `Run = (irHash, paramsSnapshot, bindingSpec,
/// runId)` and the 17-event union carries no binding, so the fold needs it
/// as input alongside the log.
const DDL: &str = "
CREATE TABLE IF NOT EXISTS run (
  run_id           TEXT PRIMARY KEY,
  flow_id          TEXT NOT NULL,
  ir_hash          TEXT NOT NULL,
  lockfile_digest  TEXT NOT NULL,
  params_snapshot  TEXT NOT NULL,
  binding          TEXT NOT NULL,
  status           TEXT NOT NULL CHECK (status IN
                     ('running','suspended','awaitingHuman','finished')),
  created_at_ms    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS run_log (
  run_id   TEXT    NOT NULL REFERENCES run(run_id),
  seq      INTEGER NOT NULL,
  type     TEXT    NOT NULL,
  at_ms    INTEGER NOT NULL,
  run_path TEXT    NOT NULL,
  payload  TEXT    NOT NULL,
  PRIMARY KEY (run_id, seq)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS checkpoint (
  run_id  TEXT PRIMARY KEY REFERENCES run(run_id),
  log_seq INTEGER NOT NULL,
  view    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS evidence (
  sha256     TEXT PRIMARY KEY,
  media_type TEXT NOT NULL,
  byte_size  INTEGER NOT NULL,
  local_path TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS evidence_ref (
  run_id   TEXT NOT NULL,
  seq      INTEGER NOT NULL,
  asset_id TEXT NOT NULL,
  sha256   TEXT NOT NULL REFERENCES evidence(sha256),
  PRIMARY KEY (run_id, seq, asset_id)
);
";

/// The dispatch identity of an `actionIntent` (2026-07-18 incorporation,
/// item ②): 1-based chain position plus the bound attempt's channel and
/// native action name, verbatim.
#[derive(Debug, Clone)]
pub struct IntentDispatch {
    /// 1-based position in `binding.attempts`.
    pub chain_index: u32,
    /// The attempt's locating channel.
    pub channel: pointlock_ir::ActChannel,
    /// The attempt's provider-native action name.
    pub action_name: pointlock_ir::ActionName,
}

/// Parameters of [`Store::begin_run`]. The `runStarted` event is *not*
/// written here — the caller appends it (07 §3.1: the log is the truth;
/// `begin_run` only creates the run row the fold takes as [`RunMeta`]).
#[derive(Debug, Clone)]
pub struct NewRun {
    /// Explicit run id; a UUIDv4 is generated when absent.
    pub run_id: Option<String>,
    /// The root flow's id.
    pub flow_id: FlowId,
    /// Content hash of the executing IR.
    pub ir_hash: Hash,
    /// Digest of the bound capability lockfile.
    pub lockfile_digest: Hash,
    /// The run's input parameters.
    pub params_snapshot: Value,
    /// Provider binding seed (device, initial session lineage, cursor).
    pub binding: BindingState,
    /// Run creation timestamp (ms since epoch).
    pub created_at_ms: u64,
}

/// One row of [`Store::list_runs`] — the run-index projection source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunListEntry {
    /// The run id.
    pub run_id: String,
    /// The run's flow.
    pub flow_id: String,
    /// The executed IR hash (canonical `sha256:` form).
    pub ir_hash: String,
    /// Current lifecycle status (folded column).
    pub status: RunStatus,
    /// Run creation wall clock.
    pub created_at_ms: u64,
}

/// One resolved evidence entry ([`Store::evidence_meta`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceMeta {
    /// Media type of the stored bytes.
    pub media_type: String,
    /// Store-relative path (`evidence/sha256/<2>/<2>/<digest>`).
    pub local_path: String,
    /// Absolute path under the store root.
    pub abs_path: PathBuf,
}

/// Result of [`Store::put_evidence`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidencePut {
    /// Bare lowercase hex sha256 digest of the bytes.
    pub sha256: String,
    /// Path relative to the store root
    /// (`evidence/sha256/<2>/<2>/<digest>`) — the form stored in
    /// `evidence.local_path` and suitable for `EvidenceRef.localPath`.
    pub local_path: String,
    /// Absolute filesystem path of the localized bytes.
    pub abs_path: PathBuf,
    /// Byte size of the content.
    pub byte_size: u64,
    /// `true` when the content was already present (idempotent dedup).
    pub deduplicated: bool,
}

/// The single-writer store handle (see the module docs for the durability
/// rules and the single-writer discipline).
pub struct Store {
    conn: Connection,
    root: PathBuf,
    /// Per-run terminal fold state of the last append (single-writer
    /// append cache): the next `append_event` folds exactly one event
    /// instead of the whole ledger. Any seq discontinuity (fresh handle,
    /// failed commit) falls back to the full refold; the persisted view
    /// is identical either way, and `verify_checkpoint` remains the
    /// from-scratch cross-check (I1).
    fold_cache: std::collections::HashMap<String, (u64, FoldState)>,
}

impl Store {
    /// Opens (creating if needed) the store rooted at `root`: the SQLite
    /// database at `<root>/pointlock.db` and the evidence area at
    /// `<root>/evidence/`. Sets `journal_mode=WAL`, `synchronous=FULL`
    /// (the actionIntent fsync semantics depend on FULL) and
    /// `foreign_keys=ON`, and applies the DDL.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join("evidence"))?;
        let conn = Connection::open(root.join("pointlock.db"))?;
        // `PRAGMA journal_mode` returns the resulting mode as a row.
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        debug_assert_eq!(mode.to_ascii_lowercase(), "wal");
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(DDL)?;
        Ok(Store {
            conn,
            root,
            fold_cache: std::collections::HashMap::new(),
        })
    }

    /// The store's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates the run row (status `running`) and returns the run id.
    ///
    /// The caller is expected to append the `runStarted` event next — the
    /// row only seeds the fold input ([`RunMeta`]); it is not a log entry.
    pub fn begin_run(&mut self, new_run: NewRun) -> Result<String, StoreError> {
        let run_id = new_run
            .run_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM run WHERE run_id = ?1)",
            [&run_id],
            |row| row.get(0),
        )?;
        if exists {
            return Err(StoreError::DuplicateRun(run_id));
        }
        let params_json = to_canonical_json(&new_run.params_snapshot);
        let binding_json = to_canonical_json(&serde_json::to_value(&new_run.binding)?);
        self.conn.execute(
            "INSERT INTO run (run_id, flow_id, ir_hash, lockfile_digest, params_snapshot, \
             binding, status, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run_id,
                new_run.flow_id.as_str(),
                new_run.ir_hash.as_str(),
                new_run.lockfile_digest.as_str(),
                params_json,
                binding_json,
                RunStatus::Running.as_str(),
                new_run.created_at_ms as i64,
            ],
        )?;
        Ok(run_id)
    }

    /// Appends one event and returns its allocated `seq`.
    ///
    /// One `IMMEDIATE` transaction covers: `seq := MAX(seq)+1` allocation,
    /// the `run_log` insert, the checkpoint re-materialization, and the
    /// `run.status` transition. The materialization folds incrementally
    /// through the per-run single-writer fold cache;
    /// any seq discontinuity falls back to the full refold, and the
    /// persisted view is identical either way. Atomicity means an event
    /// whose fold fails is *refused* — the log can never outrun the
    /// materialized view.
    pub fn append_event(
        &mut self,
        run_id: &str,
        at_ms: u64,
        run_path: &RunPath,
        payload: &RunLogPayload,
    ) -> Result<u64, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let meta = read_meta(&tx, run_id)?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM run_log WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )?;
        let run_path_json = to_canonical_json(&serde_json::to_value(run_path)?);
        let payload_json = to_canonical_json(&serde_json::to_value(payload)?);
        tx.execute(
            "INSERT INTO run_log (run_id, seq, type, at_ms, run_path, payload) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run_id,
                seq,
                payload.event_type(),
                at_ms as i64,
                run_path_json,
                payload_json,
            ],
        )?;
        let event = RunLogEvent {
            run_id: run_id.to_owned(),
            seq: seq as u64,
            at_ms,
            run_path: run_path.clone(),
            payload: payload.clone(),
        };
        let folded = match self.fold_cache.get_mut(run_id) {
            Some((cached_seq, state)) if *cached_seq + 1 == seq as u64 => {
                if let Err(err) = state.apply(&event) {
                    // A failed apply leaves the state half-mutated.
                    self.fold_cache.remove(run_id);
                    return Err(err.into());
                }
                *cached_seq = seq as u64;
                state.clone().finish()
            }
            _ => {
                let events = read_events(&tx, run_id)?;
                let state = fold_state(&meta, &events)?;
                self.fold_cache
                    .insert(run_id.to_owned(), (seq as u64, state.clone()));
                state.finish()
            }
        };
        let view_json = to_canonical_json(&serde_json::to_value(&folded.view)?);
        tx.execute(
            "INSERT INTO checkpoint (run_id, log_seq, view) VALUES (?1, ?2, ?3) \
             ON CONFLICT(run_id) DO UPDATE SET log_seq = excluded.log_seq, view = excluded.view",
            params![run_id, seq, view_json],
        )?;
        tx.execute(
            "UPDATE run SET status = ?2 WHERE run_id = ?1",
            params![run_id, folded.status.as_str()],
        )?;
        tx.commit()?;
        Ok(seq as u64)
    }

    /// Appends the `actionIntent` WAL entry in its own transaction and
    /// returns its `seq`.
    ///
    /// **Dispatch discipline (07 §3.3 rule 1)**: under WAL +
    /// `synchronous=FULL`, this method returning `Ok` means the intent is
    /// durably on disk — that *is* the "fsync before dispatch" of spine
    /// §6.2. Callers MUST invoke this and observe the `Ok` before calling
    /// `provider.execute`; on crash, `reconcile(callId)` finds the intent
    /// regardless of whether the dispatch left the process.
    pub fn write_action_intent(
        &mut self,
        run_id: &str,
        at_ms: u64,
        run_path: &RunPath,
        call_id: &str,
        args_snapshot: Value,
        dispatch: Option<IntentDispatch>,
    ) -> Result<u64, StoreError> {
        let (chain_index, channel, action_name) = match dispatch {
            Some(dispatch) => (
                Some(dispatch.chain_index),
                Some(dispatch.channel),
                Some(dispatch.action_name),
            ),
            None => (None, None, None),
        };
        self.append_event(
            run_id,
            at_ms,
            run_path,
            &RunLogPayload::ActionIntent {
                call_id: call_id.to_owned(),
                args_snapshot,
                chain_index,
                channel,
                action_name,
            },
        )
    }

    /// The single-writer arbitration of a human response (06 §4.3; R13):
    /// validates the response against the pending request read back from
    /// the ledger and — only when every rule passes — appends the
    /// `humanResponded` event, returning its `seq`.
    ///
    /// `at_ms` is the store-receipt clock, **the only timeout judge**
    /// (06 §4.3 rule 2): a response received after the request's
    /// `deadlineAtMs` is refused with
    /// [`HumanResponseRejection::DeadlineExpired`] and *no event is
    /// written* — the lazy settlement of the expired request itself stays
    /// the runner's job on resume (06 §5.3).
    ///
    /// Arbitration rules, in order (all rejections are typed
    /// [`StoreError::HumanResponseRejected`] and side-effect free — bad
    /// data never enters the ledger):
    ///
    /// 1. The request must exist (`humanRequested` with this id).
    /// 2. **First response wins**: a request with a paired final response
    ///    is closed. A supervision `suspend` answer is non-final
    ///    (spine §6.9) — it is recorded but keeps the request open for a
    ///    later proceed/abort ruling.
    /// 3. `at_ms` must not exceed the request's `deadlineAtMs`
    ///    (supervision requests carry none and never expire).
    /// 4. The request must still be pending (a lazily-settled step no
    ///    longer accepts responses).
    /// 5. The payload must match the shape the request's purpose/mode
    ///    demands (06 §2.1 union as adjudicated): `confirm`
    ///    `{decision ∈ decisions, note?}`, `judge`
    ///    `{status ∈ pass|fail|unknown, note?}`, `provideInput`
    ///    `{input (validated against outputSchema), note?}`, `repairWorld`
    ///    `{decision ∈ the request's declared decisions, else
    ///    done|cannotRepair (06 §2.1), note?}`, supervision
    ///    `{decision ∈ proceed|abort|suspend, note?}`.
    ///
    /// Single-writer discipline makes check-then-append race-free: this
    /// `Store` owns the only write connection.
    pub fn submit_human_response(
        &mut self,
        run_id: &str,
        request_id: &str,
        actor: &str,
        at_ms: u64,
        response: Value,
    ) -> Result<u64, StoreError> {
        let reject = |reason: HumanResponseRejection| StoreError::HumanResponseRejected {
            run_id: run_id.to_owned(),
            request_id: request_id.to_owned(),
            reason,
        };
        let events = self.events(run_id)?;
        // Rule 1: the request must exist on the ledger.
        let request = events
            .iter()
            .find_map(|event| match &event.payload {
                RunLogPayload::HumanRequested {
                    request_id: rid,
                    purpose,
                    mode,
                    decisions,
                    output_schema,
                    deadline_at_ms,
                    ..
                } if rid == request_id => Some((
                    event.run_path.clone(),
                    *purpose,
                    *mode,
                    decisions.clone(),
                    output_schema.clone(),
                    *deadline_at_ms,
                )),
                _ => None,
            })
            .ok_or_else(|| reject(HumanResponseRejection::UnknownRequest))?;
        let (run_path, purpose, mode, decisions, output_schema, deadline_at_ms) = request;
        // Rule 2: first response wins — any paired *final* response closes
        // the request (supervision suspend answers are non-final).
        let finally_responded = events.iter().any(|event| match &event.payload {
            RunLogPayload::HumanResponded {
                request_id: rid,
                purpose,
                response,
                ..
            } if rid == request_id => {
                !(*purpose == HumanPurpose::Supervision
                    && response.get("decision").and_then(Value::as_str) == Some("suspend"))
            }
            _ => false,
        });
        if finally_responded {
            return Err(reject(HumanResponseRejection::AlreadyResponded));
        }
        // Rule 3: the store-receipt clock is the only timeout judge.
        if let Some(deadline) = deadline_at_ms
            && at_ms > deadline
        {
            return Err(reject(HumanResponseRejection::DeadlineExpired {
                deadline_at_ms: deadline,
                received_at_ms: at_ms,
            }));
        }
        // Rule 4: the request must still be pending (the lazy timeout
        // settlement closes it via the step exit, without a response).
        let pending = self
            .materialized_checkpoint(run_id)?
            .and_then(|(_, view)| view.human_pending)
            .is_some_and(|pending| pending.request_id == request_id);
        if !pending {
            return Err(reject(HumanResponseRejection::Settled));
        }
        // Rule 5: shape validation per purpose/mode.
        validate_response_shape(
            purpose,
            mode,
            decisions.as_deref(),
            &output_schema,
            &response,
        )
        .map_err(|reason| reject(HumanResponseRejection::InvalidShape { reason }))?;
        self.append_event(
            run_id,
            at_ms,
            &run_path,
            &RunLogPayload::HumanResponded {
                request_id: request_id.to_owned(),
                purpose,
                response,
                actor: actor.to_owned(),
            },
        )
    }

    /// Rebuilds the [`CheckpointView`] by folding the run's full log
    /// (07 §3.3 rebuild channel). Read-only; does not touch the
    /// materialized row.
    pub fn rebuild_checkpoint(&self, run_id: &str) -> Result<CheckpointView, StoreError> {
        Ok(self.refold(run_id)?.view)
    }

    /// I1's runtime self-check (backs `pointlock inspect
    /// --rebuild-checkpoint`): asserts materialized == rebuilt.
    ///
    /// Verifies that (a) the checkpoint row exists and its `log_seq` is the
    /// log head, (b) the stored view equals the full-log refold, and
    /// (c) `run.status` equals the folded status. Any inequality is a
    /// store-layer bug surfaced as a typed error. Returns the verified
    /// view.
    pub fn verify_checkpoint(&self, run_id: &str) -> Result<CheckpointView, StoreError> {
        let meta = read_meta(&self.conn, run_id)?;
        let events = read_events(&self.conn, run_id)?;
        let (log_seq, stored_json): (i64, String) = self
            .conn
            .query_row(
                "SELECT log_seq, view FROM checkpoint WHERE run_id = ?1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NoCheckpoint(run_id.to_owned()))?;
        let head = events.last().map(|event| event.seq).unwrap_or(0);
        if log_seq as u64 != head {
            return Err(StoreError::StaleCheckpoint {
                run_id: run_id.to_owned(),
                materialized_seq: log_seq as u64,
                log_seq: head,
            });
        }
        let stored: CheckpointView = serde_json::from_str(&stored_json)?;
        let folded = fold_checkpoint(&meta, &events)?;
        if folded.view != stored {
            return Err(StoreError::CheckpointMismatch {
                run_id: run_id.to_owned(),
                log_seq: log_seq as u64,
                materialized: to_canonical_json(&serde_json::to_value(&stored)?),
                rebuilt: to_canonical_json(&serde_json::to_value(&folded.view)?),
            });
        }
        let stored_status = self.run_status(run_id)?;
        if stored_status != folded.status {
            return Err(StoreError::StatusMismatch {
                run_id: run_id.to_owned(),
                stored: stored_status.as_str().to_owned(),
                folded: folded.status.as_str().to_owned(),
            });
        }
        Ok(folded.view)
    }

    /// Localizes evidence bytes into the content-addressed area and indexes
    /// them, idempotently. Layout:
    /// `<root>/evidence/sha256/<hex[0..2]>/<hex[2..4]>/<digest>`.
    ///
    /// **file-before-row (07 §3.3 rule 3)**: bytes are written to a temp
    /// file, fsynced, renamed into place (and the directory fsynced) before
    /// the `evidence` row is inserted; the caller appends the referencing
    /// RunLog event only after this returns. Re-putting identical bytes is
    /// a no-op dedup (`deduplicated: true`).
    pub fn put_evidence(
        &mut self,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<EvidencePut, StoreError> {
        let digest = sha256_hex(bytes);
        let rel_path = format!(
            "evidence/sha256/{}/{}/{}",
            &digest[0..2],
            &digest[2..4],
            digest
        );
        let abs_path = self.root.join(&rel_path);
        let deduplicated = abs_path.exists();
        if !deduplicated {
            let parent = abs_path.parent().expect("evidence path has a parent");
            fs::create_dir_all(parent)?;
            let tmp_path = parent.join(format!(".{}.tmp.{}", digest, std::process::id()));
            {
                let mut file = fs::File::create(&tmp_path)?;
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            fs::rename(&tmp_path, &abs_path)?;
            // Make the rename itself durable before the row exists.
            #[cfg(unix)]
            fs::File::open(parent)?.sync_all()?;
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO evidence (sha256, media_type, byte_size, local_path) \
             VALUES (?1, ?2, ?3, ?4)",
            params![digest, media_type, bytes.len() as i64, rel_path],
        )?;
        Ok(EvidencePut {
            sha256: digest,
            local_path: rel_path,
            abs_path,
            byte_size: bytes.len() as u64,
            deduplicated,
        })
    }

    /// Links a RunLog event to a localized evidence entry
    /// (`evidence_ref` row; idempotent). `foreign_keys=ON` rejects links
    /// to evidence that was never put.
    pub fn link_evidence(
        &mut self,
        run_id: &str,
        seq: u64,
        asset_id: &str,
        sha256: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO evidence_ref (run_id, seq, asset_id, sha256) \
             VALUES (?1, ?2, ?3, ?4)",
            params![run_id, seq as i64, asset_id, sha256],
        )?;
        Ok(())
    }

    /// Reads the run's metadata row (the fold input).
    pub fn run_meta(&self, run_id: &str) -> Result<RunMeta, StoreError> {
        read_meta(&self.conn, run_id)
    }

    /// Reads the run's current lifecycle status.
    pub fn run_status(&self, run_id: &str) -> Result<RunStatus, StoreError> {
        let status: String = self
            .conn
            .query_row(
                "SELECT status FROM run WHERE run_id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::UnknownRun(run_id.to_owned()))?;
        RunStatus::parse(&status).ok_or_else(|| StoreError::Corrupt {
            run_id: run_id.to_owned(),
            reason: format!("run.status holds unknown value {status:?}"),
        })
    }

    /// The run's current revision = its max ledger seq (0 before the
    /// first event) — the SSE invalidation currency (08 §5). Cheap by
    /// design: the pollers behind `--serve` call this a few times a
    /// second.
    pub fn revision(&self, run_id: &str) -> Result<u64, StoreError> {
        let _ = read_meta(&self.conn, run_id)?;
        let head: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM run_log WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )?;
        Ok(head as u64)
    }

    /// A store-wide monotonic revision (= sum of every run's head seq):
    /// the inbox stream's invalidation currency — any append anywhere
    /// moves it.
    pub fn global_revision(&self) -> Result<u64, StoreError> {
        let total: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(head), 0) FROM \
             (SELECT MAX(seq) AS head FROM run_log GROUP BY run_id)",
            [],
            |row| row.get(0),
        )?;
        Ok(total as u64)
    }

    /// Resolves one content-addressed evidence entry to its media type
    /// and absolute path (the `/evidence/:sha256` byte route — 08 §4.3
    /// dereference side; the address is the only key, never a path).
    pub fn evidence_meta(&self, sha256: &str) -> Result<Option<EvidenceMeta>, StoreError> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT media_type, local_path FROM evidence WHERE sha256 = ?1",
                [sha256],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(media_type, local_path)| EvidenceMeta {
            media_type,
            abs_path: self.root.join(&local_path),
            local_path,
        }))
    }

    /// Lists every run, in creation order (projection read side: the
    /// cross-run inbox and the flow run index consume this).
    pub fn list_runs(&self) -> Result<Vec<RunListEntry>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT run_id, flow_id, ir_hash, status, created_at_ms \
             FROM run ORDER BY created_at_ms, run_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut runs = Vec::new();
        for row in rows {
            let (run_id, flow_id, ir_hash, status, created_at_ms) = row?;
            let status = RunStatus::parse(&status).ok_or_else(|| StoreError::Corrupt {
                run_id: run_id.clone(),
                reason: format!("run.status holds unknown value {status:?}"),
            })?;
            runs.push(RunListEntry {
                run_id,
                flow_id,
                ir_hash,
                status,
                created_at_ms: created_at_ms as u64,
            });
        }
        Ok(runs)
    }

    /// Reads the run's full ordered event log.
    pub fn events(&self, run_id: &str) -> Result<Vec<RunLogEvent>, StoreError> {
        // Distinguish "unknown run" from "no events yet".
        let _ = read_meta(&self.conn, run_id)?;
        read_events(&self.conn, run_id)
    }

    /// Reads the materialized checkpoint row, if any:
    /// `(log_seq, view)`. `None` until the first event is appended.
    pub fn materialized_checkpoint(
        &self,
        run_id: &str,
    ) -> Result<Option<(u64, CheckpointView)>, StoreError> {
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT log_seq, view FROM checkpoint WHERE run_id = ?1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some((log_seq, view_json)) => {
                let view: CheckpointView = serde_json::from_str(&view_json)?;
                Ok(Some((log_seq as u64, view)))
            }
        }
    }

    fn refold(&self, run_id: &str) -> Result<FoldedRun, StoreError> {
        let meta = read_meta(&self.conn, run_id)?;
        let events = read_events(&self.conn, run_id)?;
        Ok(fold_checkpoint(&meta, &events)?)
    }
}

/// Validates one human response payload against the shape its request's
/// purpose/mode demands (06 §2.1 union as adjudicated). Strictly closed:
/// exactly the expected key plus an optional string `note`; anything else
/// is refused (bad data never enters the ledger).
fn validate_response_shape(
    purpose: HumanPurpose,
    mode: Option<HumanMode>,
    decisions: Option<&[String]>,
    output_schema: &Option<JsonSchemaDocument>,
    response: &Value,
) -> Result<(), String> {
    let Some(object) = response.as_object() else {
        return Err(format!("response must be a JSON object, got {response}"));
    };
    let (value_key, expects_input) = match (purpose, mode) {
        (HumanPurpose::Supervision, _) => ("decision", false),
        (HumanPurpose::Step, Some(HumanMode::Confirm)) => ("decision", false),
        (HumanPurpose::Step, Some(HumanMode::Judge)) => ("status", false),
        (HumanPurpose::Step, Some(HumanMode::ProvideInput)) => ("input", true),
        (HumanPurpose::Step, Some(HumanMode::RepairWorld)) => ("decision", false),
        (HumanPurpose::Step, None) => {
            return Err("the request carries no mode for its step purpose".to_owned());
        }
    };
    for key in object.keys() {
        if key != value_key && key != "note" {
            return Err(format!("unexpected field '{key}'"));
        }
    }
    if let Some(note) = object.get("note")
        && !note.is_string()
    {
        return Err(format!("'note' must be a string, got {note}"));
    }
    let Some(value) = object.get(value_key) else {
        return Err(format!("missing required field '{value_key}'"));
    };
    if expects_input {
        // provideInput: the input must satisfy the request's outputSchema
        // (06 §4.3 rule 3 — the channel re-prompts on rejection).
        let Some(schema) = output_schema else {
            return Err("the request carries no outputSchema for provideInput".to_owned());
        };
        return jsonschema::validate(schema.as_value(), value)
            .map_err(|error| format!("input failed the outputSchema: {error}"));
    }
    let Some(label) = value.as_str() else {
        return Err(format!("'{value_key}' must be a string, got {value}"));
    };
    match (purpose, mode) {
        (HumanPurpose::Supervision, _) => match label {
            // Deliberately no `skip` (spine §6.9).
            "proceed" | "abort" | "suspend" => Ok(()),
            other => Err(format!(
                "supervision decision must be proceed|abort|suspend, got '{other}'"
            )),
        },
        (_, Some(HumanMode::Confirm)) => {
            // Position-mapped double label (06 §2.2): membership is the
            // arbitration's check, the position mapping is the runner's.
            let labels = decisions.unwrap_or(&[]);
            if labels.iter().any(|candidate| candidate == label) {
                Ok(())
            } else {
                Err(format!(
                    "confirm decision '{label}' is not one of the request's labels {labels:?}"
                ))
            }
        }
        (_, Some(HumanMode::Judge)) => {
            // The three-valued vocabulary admits no aliases (06 §2.2),
            // narrowed further by the request's declared subset.
            if !matches!(label, "pass" | "fail" | "unknown") {
                return Err(format!(
                    "judge status must be pass|fail|unknown, got '{label}'"
                ));
            }
            if let Some(subset) = decisions
                && !subset.iter().any(|candidate| candidate == label)
            {
                return Err(format!(
                    "judge status '{label}' is outside the request's declared subset {subset:?}"
                ));
            }
            Ok(())
        }
        (_, Some(HumanMode::RepairWorld)) => {
            // A repairWorld request that declares its own vocabulary is
            // arbitrated against exactly it — the uncertain-reconcile
            // adjudication asks `adopt | redo | abort` (00 §6.7-B /
            // 07 §4.4), a different question from the world-repair
            // declaration. Without a declaration the 06 §2.1 base
            // vocabulary governs: `done | cannotRepair`, nothing else,
            // no aliases (2026-07-28 unification — the as-built
            // `repaired|abort` matched neither 06 nor the adjudication
            // and is retired).
            if let Some(declared) = decisions {
                return if declared.iter().any(|candidate| candidate == label) {
                    Ok(())
                } else {
                    Err(format!(
                        "repairWorld decision '{label}' is not one of the request's \
                         labels {declared:?}"
                    ))
                };
            }
            match label {
                "done" | "cannotRepair" => Ok(()),
                other => Err(format!(
                    "repairWorld decision must be done|cannotRepair (06 §2.1), got '{other}'"
                )),
            }
        }
        _ => unreachable!("provideInput and mode-less step requests returned above"),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn read_meta(conn: &Connection, run_id: &str) -> Result<RunMeta, StoreError> {
    let row: Option<(String, String, String, String, String, i64)> = conn
        .query_row(
            "SELECT flow_id, ir_hash, lockfile_digest, params_snapshot, binding, created_at_ms \
             FROM run WHERE run_id = ?1",
            [run_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((flow_id, ir_hash, lockfile_digest, params_json, binding_json, created_at_ms)) = row
    else {
        return Err(StoreError::UnknownRun(run_id.to_owned()));
    };
    let corrupt = |reason: String| StoreError::Corrupt {
        run_id: run_id.to_owned(),
        reason,
    };
    Ok(RunMeta {
        run_id: run_id.to_owned(),
        flow_id: FlowId::new(flow_id).map_err(|e| corrupt(e.to_string()))?,
        ir_hash: Hash::new(ir_hash).map_err(|e| corrupt(e.to_string()))?,
        lockfile_digest: Hash::new(lockfile_digest).map_err(|e| corrupt(e.to_string()))?,
        params_snapshot: serde_json::from_str(&params_json)?,
        binding: serde_json::from_str(&binding_json)?,
        created_at_ms: created_at_ms as u64,
    })
}

fn read_events(conn: &Connection, run_id: &str) -> Result<Vec<RunLogEvent>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT seq, type, at_ms, run_path, payload FROM run_log \
         WHERE run_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([run_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut events = Vec::new();
    for row in rows {
        let (seq, event_type, at_ms, run_path_json, payload_json) = row?;
        let payload: RunLogPayload = serde_json::from_str(&payload_json)?;
        // Self-check: the denormalized `type` column must agree with the
        // payload discriminant.
        if payload.event_type() != event_type {
            return Err(StoreError::Corrupt {
                run_id: run_id.to_owned(),
                reason: format!(
                    "run_log seq {seq}: type column {event_type:?} disagrees with payload \
                     discriminant {:?}",
                    payload.event_type()
                ),
            });
        }
        let run_path: RunPath = serde_json::from_str(&run_path_json)?;
        events.push(RunLogEvent {
            run_id: run_id.to_owned(),
            seq: seq as u64,
            at_ms: at_ms as u64,
            run_path,
            payload,
        });
    }
    Ok(events)
}
