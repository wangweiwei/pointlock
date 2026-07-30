//! The CLI human channel (06 §4: the `cli` notification/collection
//! channel): renders pending human requests from the ledger and collects
//! responses through the store's single-writer arbitration.
//!
//! This crate is presentation + collection only. It never judges, never
//! writes ledger events itself (submission goes through
//! [`pointlock_store::Store::submit_human_response`], the one arbitration
//! door), and reads request context straight from the `humanRequested`
//! events — the log is the truth (I1).

pub mod webhook;

use std::io::{BufRead, Write};

use pointlock_ir::{HumanMode, HumanPurpose, RunLogPayload, RunPath};
use pointlock_store::{Store, StoreError};
use serde_json::Value;

/// One unanswered human request, reconstructed from the ledger (the
/// `humanRequested` payload plus its pairing state).
#[derive(Debug, Clone)]
pub struct PendingRequest {
    /// The id a response must pair with.
    pub request_id: String,
    /// Step vs supervision gate (R13).
    pub purpose: HumanPurpose,
    /// Interaction mode (`purpose = step` only).
    pub mode: Option<HumanMode>,
    /// The prompt shown to the human.
    pub prompt: String,
    /// The materialized exhibits.
    pub presents: Value,
    /// Confirm labels, when the mode declares them.
    pub decisions: Option<Vec<String>>,
    /// The provideInput contract, when the mode declares one.
    pub output_schema: Option<pointlock_ir::JsonSchemaDocument>,
    /// Absolute response deadline (ms); absent for supervision gates.
    pub deadline_at_ms: Option<u64>,
    /// The awaiting/gated step's run path.
    pub run_path: RunPath,
}

/// Errors of the collection channel.
#[derive(Debug, thiserror::Error)]
pub enum HumanCliError {
    /// Reading the ledger failed.
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// Terminal I/O failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The answer could not be interpreted for the request's mode.
    #[error("invalid answer: {0}")]
    InvalidAnswer(String),
    /// No pending request with that id exists on the ledger.
    #[error("no pending request '{0}' on the ledger")]
    NotPending(String),
}

/// Scans one run's ledger for unanswered human requests, in request
/// order. A supervision `suspend` answer is non-final and keeps its
/// request pending (spine §6.9).
pub fn pending_requests(store: &Store, run_id: &str) -> Result<Vec<PendingRequest>, HumanCliError> {
    let events = store.events(run_id)?;
    let mut pending: Vec<PendingRequest> = Vec::new();
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
            } => pending.push(PendingRequest {
                request_id: request_id.clone(),
                purpose: *purpose,
                mode: *mode,
                prompt: prompt.clone(),
                presents: presents.clone(),
                decisions: decisions.clone(),
                output_schema: output_schema.clone(),
                deadline_at_ms: *deadline_at_ms,
                run_path: event.run_path.clone(),
            }),
            RunLogPayload::HumanResponded {
                request_id,
                purpose,
                response,
                ..
            } => {
                let non_final = *purpose == HumanPurpose::Supervision
                    && response.get("decision").and_then(Value::as_str) == Some("suspend");
                if !non_final {
                    pending.retain(|request| request.request_id != *request_id);
                }
            }
            _ => {}
        }
    }
    Ok(pending)
}

/// Finds one pending request by id.
pub fn find_pending(
    store: &Store,
    run_id: &str,
    request_id: &str,
) -> Result<PendingRequest, HumanCliError> {
    pending_requests(store, run_id)?
        .into_iter()
        .find(|request| request.request_id == request_id)
        .ok_or_else(|| HumanCliError::NotPending(request_id.to_owned()))
}

/// The answer vocabulary line for a request (what the human may type).
pub fn answer_hint(request: &PendingRequest) -> String {
    match (request.purpose, request.mode) {
        (HumanPurpose::Supervision, _) => "answer: proceed | abort | suspend".to_owned(),
        (_, Some(HumanMode::Confirm)) => {
            let labels = request.decisions.as_deref().unwrap_or(&[]).join("' | '");
            format!("answer: '{labels}'")
        }
        (_, Some(HumanMode::Judge)) => "answer: pass | fail | unknown".to_owned(),
        (_, Some(HumanMode::ProvideInput)) => {
            "answer: one line of JSON matching the declared schema".to_owned()
        }
        (_, Some(HumanMode::RepairWorld)) => match request.decisions.as_deref() {
            // A declaring request (the reconcile adjudication's
            // adopt|redo|abort, 07 §4.4) is answered in its declared
            // vocabulary; otherwise the 06 §2.1 base vocabulary.
            Some(labels) => format!("answer: '{}'", labels.join("' | '")),
            None => "answer: done | cannotRepair".to_owned(),
        },
        (_, None) => "answer: (unknown request shape)".to_owned(),
    }
}

/// Renders one pending request for a terminal.
pub fn render(w: &mut impl Write, request: &PendingRequest) -> std::io::Result<()> {
    writeln!(w, "── human request {} ──", request.request_id)?;
    let kind = match (request.purpose, request.mode) {
        (HumanPurpose::Supervision, _) => "supervision gate".to_owned(),
        (_, Some(mode)) => format!(
            "human step ({})",
            serde_json::to_value(mode)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        ),
        (_, None) => "human step".to_owned(),
    };
    writeln!(w, "kind: {kind}")?;
    writeln!(w, "prompt: {}", request.prompt)?;
    if let Value::Array(items) = &request.presents
        && !items.is_empty()
    {
        writeln!(w, "presents:")?;
        for (index, item) in items.iter().enumerate() {
            writeln!(w, "  [{index}] {item}")?;
        }
    }
    if let Some(deadline) = request.deadline_at_ms {
        writeln!(w, "deadlineAtMs: {deadline}")?;
    }
    writeln!(w, "{}", answer_hint(request))?;
    Ok(())
}

/// The CLI channel's actor string: `cli:os:<user>@<host>` (06 §4.4).
/// Attribution, not authentication — the v0.1 trust boundary is the
/// machine itself; the report honestly records who was at the keyboard.
pub fn cli_actor() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_owned());
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    format!("cli:os:{user}@{host}")
}

/// Interprets one answer line into the mode-shaped response payload the
/// store arbitration validates (06 §2.1 union).
pub fn interpret_answer(request: &PendingRequest, line: &str) -> Result<Value, HumanCliError> {
    let answer = line.trim();
    if answer.is_empty() {
        return Err(HumanCliError::InvalidAnswer("empty answer".to_owned()));
    }
    match (request.purpose, request.mode) {
        (HumanPurpose::Supervision, _) => match answer {
            "proceed" | "abort" | "suspend" => Ok(serde_json::json!({ "decision": answer })),
            other => Err(HumanCliError::InvalidAnswer(format!(
                "'{other}' is not a supervision decision (proceed|abort|suspend)"
            ))),
        },
        (_, Some(HumanMode::Confirm)) => {
            let labels = request.decisions.as_deref().unwrap_or(&[]);
            if labels.iter().any(|label| label == answer) {
                Ok(serde_json::json!({ "decision": answer }))
            } else {
                Err(HumanCliError::InvalidAnswer(format!(
                    "'{answer}' is not one of the confirm labels {labels:?}"
                )))
            }
        }
        (_, Some(HumanMode::Judge)) => match answer {
            "pass" | "fail" | "unknown" => Ok(serde_json::json!({ "status": answer })),
            other => Err(HumanCliError::InvalidAnswer(format!(
                "'{other}' is not a judge status (pass|fail|unknown)"
            ))),
        },
        (_, Some(HumanMode::ProvideInput)) => {
            let input: Value = serde_json::from_str(answer).map_err(|err| {
                HumanCliError::InvalidAnswer(format!("provideInput answer is not JSON: {err}"))
            })?;
            Ok(serde_json::json!({ "input": input }))
        }
        (_, Some(HumanMode::RepairWorld)) => match request.decisions.as_deref() {
            // Declared-first, mirroring the store arbitration exactly: a
            // declaring request (the reconcile adjudication's
            // adopt|redo|abort, 07 §4.4) is valid only in its declared
            // vocabulary; without a declaration the 06 §2.1 base
            // vocabulary governs.
            Some(labels) => {
                if labels.iter().any(|label| label == answer) {
                    Ok(serde_json::json!({ "decision": answer }))
                } else {
                    Err(HumanCliError::InvalidAnswer(format!(
                        "'{answer}' is not one of the declared repairWorld decisions {labels:?}"
                    )))
                }
            }
            None => match answer {
                "done" | "cannotRepair" => Ok(serde_json::json!({ "decision": answer })),
                other => Err(HumanCliError::InvalidAnswer(format!(
                    "'{other}' is not a repairWorld decision (done|cannotRepair)"
                ))),
            },
        },
        (_, None) => Err(HumanCliError::InvalidAnswer(
            "request carries no mode".to_owned(),
        )),
    }
}

/// Renders the request, reads one answer line, and submits it through the
/// store arbitration. Returns the appended `humanResponded` seq and the
/// interpreted response (the interactive loop inspects supervision
/// `suspend` answers to stop re-prompting).
pub fn collect(
    store: &mut Store,
    run_id: &str,
    request_id: &str,
    actor: &str,
    at_ms: u64,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<(u64, Value), HumanCliError> {
    let request = find_pending(store, run_id, request_id)?;
    render(writer, &request)?;
    writer.flush()?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let response = interpret_answer(&request, &line)?;
    let seq = store.submit_human_response(run_id, request_id, actor, at_ms, response.clone())?;
    writeln!(writer, "response recorded (seq {seq})")?;
    Ok((seq, response))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(purpose: HumanPurpose, mode: Option<HumanMode>) -> PendingRequest {
        PendingRequest {
            request_id: "req-1".to_owned(),
            purpose,
            mode,
            prompt: "p".to_owned(),
            presents: Value::Array(Vec::new()),
            decisions: Some(vec!["yes".to_owned(), "no".to_owned()]),
            output_schema: None,
            deadline_at_ms: None,
            run_path: Vec::new(),
        }
    }

    #[test]
    fn interprets_the_mode_vocabularies() {
        let judge = request(HumanPurpose::Step, Some(HumanMode::Judge));
        assert_eq!(
            interpret_answer(&judge, "pass\n").expect("judge"),
            serde_json::json!({ "status": "pass" })
        );
        assert!(interpret_answer(&judge, "yes").is_err());

        let confirm = request(HumanPurpose::Step, Some(HumanMode::Confirm));
        assert_eq!(
            interpret_answer(&confirm, "no").expect("confirm"),
            serde_json::json!({ "decision": "no" })
        );
        assert!(interpret_answer(&confirm, "maybe").is_err());

        let gate = request(HumanPurpose::Supervision, None);
        assert_eq!(
            interpret_answer(&gate, "suspend").expect("gate"),
            serde_json::json!({ "decision": "suspend" })
        );

        let provide = request(HumanPurpose::Step, Some(HumanMode::ProvideInput));
        assert_eq!(
            interpret_answer(&provide, r#"{"ssid":"lab"}"#).expect("provide"),
            serde_json::json!({ "input": { "ssid": "lab" } })
        );
        assert!(interpret_answer(&provide, "not json").is_err());

        let mut repair = request(HumanPurpose::Step, Some(HumanMode::RepairWorld));
        repair.decisions = None;
        assert_eq!(
            interpret_answer(&repair, "done").expect("repair"),
            serde_json::json!({ "decision": "done" })
        );
        assert_eq!(
            interpret_answer(&repair, "cannotRepair").expect("repair"),
            serde_json::json!({ "decision": "cannotRepair" })
        );
        // The retired as-built vocabulary must stay rejected
        // (2026-07-28 unification).
        assert!(interpret_answer(&repair, "repaired").is_err());
        assert!(interpret_answer(&repair, "abort").is_err());
    }

    #[test]
    fn repair_world_honors_declared_decisions() {
        // The reconcile adjudication (07 §4.4) declares its own
        // vocabulary; the CLI must accept exactly that set — mirroring
        // the store's declared-first arbitration.
        let mut adjudicate = request(HumanPurpose::Step, Some(HumanMode::RepairWorld));
        adjudicate.decisions = Some(vec![
            "adopt".to_owned(),
            "redo".to_owned(),
            "abort".to_owned(),
        ]);
        assert_eq!(
            interpret_answer(&adjudicate, "adopt").expect("declared"),
            serde_json::json!({ "decision": "adopt" })
        );
        // Negative control: the base vocabulary does NOT leak into a
        // declaring request.
        assert!(interpret_answer(&adjudicate, "done").is_err());
        assert!(answer_hint(&adjudicate).contains("'adopt' | 'redo' | 'abort'"));
    }

    #[test]
    fn cli_actor_carries_the_os_principal() {
        // 06 §4.4: `cli:os:<user>@<host>` — attribution from OS identity,
        // never a hardcoded placeholder.
        let actor = cli_actor();
        assert!(actor.starts_with("cli:os:"), "{actor}");
        assert!(actor.contains('@'), "{actor}");
        assert_ne!(actor, "cli:os:@");
    }

    #[test]
    fn repair_world_hint_matches_the_accepted_vocabulary() {
        // The hint must never advertise words interpret_answer rejects
        // (the retired `repaired | abort` hint did exactly that).
        let mut repair = request(HumanPurpose::Step, Some(HumanMode::RepairWorld));
        repair.decisions = None;
        let hint = answer_hint(&repair);
        assert!(hint.contains("done") && hint.contains("cannotRepair"));
        assert!(!hint.contains("repaired"));
    }
}
