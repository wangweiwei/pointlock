//! # pointlock-store
//!
//! RunLog + Checkpoint persistence (SQLite WAL via rusqlite,
//! `synchronous=FULL`) and the content-addressed evidence area.
//!
//! Authoritative design documents:
//! - `docs/design/07-subflow-checkpoint-resume-repair.md` §3 (checkpoint
//!   model, DDL, the four materialization moments, the actionIntent WAL
//!   transaction discipline, file-before-row-before-log, the
//!   rebuild-checkpoint self-check)
//! - `docs/design/00-architecture-spine.md` §6.1 (RunLog) / §6.6
//!   (CheckpointView)
//!
//! ## Shape of the API
//!
//! - [`Store`] — the single-writer handle: [`Store::open`],
//!   [`Store::begin_run`], [`Store::append_event`] (seq allocation, insert,
//!   and checkpoint materialization in one transaction),
//!   [`Store::write_action_intent`] (the WAL entry whose committed return
//!   gates provider dispatch), [`Store::submit_human_response`] (the
//!   single-writer arbitration of human responses — first response wins,
//!   deadline judged by the store-receipt clock, shape-validated per
//!   purpose/mode; 06 §4.3), [`Store::put_evidence`] /
//!   [`Store::link_evidence`], and the read side ([`Store::events`],
//!   [`Store::run_meta`], [`Store::run_status`],
//!   [`Store::materialized_checkpoint`]).
//! - [`fold_checkpoint`] — the deterministic pure fold
//!   `(RunMeta, events) → CheckpointView + RunStatus`, exposed separately
//!   so it is directly testable; [`Store::rebuild_checkpoint`] and
//!   [`Store::verify_checkpoint`] (materialized == rebuilt, I1's runtime
//!   self-check) are thin wrappers over it.
//!
//! Append-only is structural: no API updates or deletes `run_log` rows
//! (07 §3.3 rule 4 — re-judgement appends a new `verdictRecorded` carrying
//! `supersedes`; old events never move).
//!
//! The projection read side (R14, spine §10.2) homes in [`projection`]:
//! the five renderer-agnostic DTO families + their query layer — the only
//! contract any renderer (or `pointlock locate`) consumes.

pub mod error;
pub mod fold;
pub mod projection;
pub mod store;

pub use error::{FoldError, HumanResponseRejection, StoreError};
pub use fold::{FoldedRun, RunMeta, RunStatus, fold_checkpoint};
pub use store::{EvidenceMeta, EvidencePut, IntentDispatch, NewRun, RunListEntry, Store};
