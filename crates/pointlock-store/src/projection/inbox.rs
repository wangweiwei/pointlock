//! `HumanInboxEntry` — the unified inbox projection (spine §10.1,
//! 08 §2.6): every `humanRequested` without a paired FINAL
//! `humanResponded`, across all runs, human steps and supervision gates
//! in the SAME box (R13 — one arbitration, one channel, one inbox). A
//! supervision `suspend` answer is non-final and keeps its request
//! pending (spine §6.9).
//!
//! v0.1 is notify-side only: entries render, responses go through
//! `pointlock-human-cli` (06 §4.2 — the `webUi` collect channel is a
//! reserved v0.2 surface).

use pointlock_ir::{JsonSchemaDocument, RunLogPayload, RunPath, render_run_path};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ProjectionVersion;
use crate::error::StoreError;
use crate::store::Store;

/// One pending human request (step or supervision — `purpose` is the
/// discriminator, spine A.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HumanInboxEntry {
    /// Protocol version (spine §10.3).
    pub projection_version: ProjectionVersion,
    /// The run awaiting the response.
    pub run_id: String,
    /// The run's flow.
    pub flow_id: String,
    /// The pairing id a response must carry.
    pub request_id: String,
    /// `step` vs `supervision` (R13).
    pub purpose: String,
    /// Interaction mode (`purpose = step` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The prompt.
    pub prompt: String,
    /// Materialized exhibits (values render directly; evidence refs go
    /// through the gallery route — 08 §2.6).
    pub presents: Value,
    /// Confirm labels, when the mode declares them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decisions: Option<Vec<String>>,
    /// The provideInput contract, when the mode declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonSchemaDocument>,
    /// Absolute response deadline (ms); absent for supervision gates —
    /// they never time out (spine §6.9). After the deadline the runner
    /// settles the request to `unknown` (fixed vocabulary).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<u64>,
    /// When the request was recorded.
    pub requested_at_ms: u64,
    /// Ledger seq of the request.
    pub requested_seq: u64,
    /// Canonical string of the awaiting/gated step's path.
    pub run_path: String,
    /// The structured path (frames are the authority — spine §9).
    pub run_path_frames: RunPath,
}

/// Scans one run's ledger for pending entries, in request order.
pub fn run_inbox(store: &Store, run_id: &str) -> Result<Vec<HumanInboxEntry>, StoreError> {
    let meta = store.run_meta(run_id)?;
    let events = store.events(run_id)?;
    let mut pending: Vec<HumanInboxEntry> = Vec::new();
    for event in &events {
        match &event.payload {
            RunLogPayload::HumanRequested {
                request_id,
                purpose,
                mode,
                prompt,
                presents,
                decisions,
                output_schema,
                deadline_at_ms,
            } => pending.push(HumanInboxEntry {
                projection_version: ProjectionVersion,
                run_id: run_id.to_owned(),
                flow_id: meta.flow_id.to_string(),
                request_id: request_id.clone(),
                purpose: wire(purpose),
                mode: mode.as_ref().map(wire),
                prompt: prompt.clone(),
                presents: presents.clone(),
                decisions: decisions.clone(),
                output_schema: output_schema.clone(),
                deadline_at_ms: *deadline_at_ms,
                requested_at_ms: event.at_ms,
                requested_seq: event.seq,
                run_path: render_run_path(&event.run_path),
                run_path_frames: event.run_path.clone(),
            }),
            RunLogPayload::HumanResponded {
                request_id,
                purpose,
                response,
                ..
            } => {
                let non_final = *purpose == pointlock_ir::HumanPurpose::Supervision
                    && response.get("decision").and_then(Value::as_str) == Some("suspend");
                if !non_final {
                    pending.retain(|entry| entry.request_id != *request_id);
                }
            }
            // A terminal exit of the awaiting step settles its request
            // WITHOUT a response — the lazy timeout settlement (verdict
            // unknown) and the aborted disposition both take this path
            // (06 §5.3; mirrors the checkpoint fold's rule).
            RunLogPayload::StepExited { .. } => {
                pending.retain(|entry| entry.run_path_frames != event.run_path);
            }
            _ => {}
        }
    }
    Ok(pending)
}

/// The cross-run inbox (08 §2.6): pending entries of every run, ordered
/// by run creation then request seq.
pub fn human_inbox(store: &Store) -> Result<Vec<HumanInboxEntry>, StoreError> {
    let mut entries = Vec::new();
    for run in store.list_runs()? {
        entries.extend(run_inbox(store, &run.run_id)?);
    }
    Ok(entries)
}

/// Serializes a unit-enum value to its wire literal.
fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
