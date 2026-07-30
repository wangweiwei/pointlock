//! `pointlock report` — the run report (08 §6.2 + §6.3): header, flow
//! verdict, judged counts with `unknown` tallied separately from `fail`
//! (06 §7.3), the `unverified` execution annotation (R4 — an annotation,
//! never a verdict, never folded), a degraded tally, per-step lines with
//! superseded-verdict lineage counts (07 §5.3: re-judging keeps history,
//! greyed not deleted), the human/handler dimension (who judged what,
//! when), and the segment history (per-segment supervise policy + the
//! alignment counts of each resume).
//!
//! The report reads the run's own ledger and nothing else — a historical
//! run is always interpreted with the semantics it recorded (02 §12.3).
//!
//! The `--format json` shape is CLI-owned output, versioned by the
//! `pointlockReport: 1` marker (precedent: the `CompileDiagnostic[]`
//! surface of 03 §4.4 and the `pointlockBundle` artifact). It is NOT a
//! sixth projection DTO family — spine §10.1 pins that list closed.

use std::collections::BTreeMap;
use std::path::Path;

use pointlock_ir::{
    AlignmentClass, AlignmentReport, PathFrame, RunLogPayload, RunPath, StepRecord, render_run_path,
};
use pointlock_store::Store;
use serde::Serialize;

use crate::commands::{store_failure, usage_failure, wire_str};
use crate::{Failure, OutputFormat, exit};

/// The versioned JSON envelope of `pointlock report --format json`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunReport {
    /// Format marker/version of this CLI-owned shape.
    pointlock_report: u32,
    run_id: String,
    flow_id: String,
    ir_hash: String,
    lockfile_digest: String,
    device_id: String,
    /// Session lineage as the ledger records/derives it (07 §4.5,
    /// incorporated 2026-07-18): the bind-time session, extended by each
    /// cursor-bearing `runResumed`. Cursor-less resumes (pre-
    /// incorporation ledgers) extend nothing — single-entry lineage is
    /// the recorded truth there, not a limitation.
    session_lineage: Vec<String>,
    status: String,
    created_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at_ms: Option<u64>,
    /// The folded flow verdict; absent = unfinished, finished
    /// unverified, or aborted (the ledger's `runFinished.verdict` as-is).
    #[serde(skip_serializing_if = "Option::is_none")]
    flow_verdict: Option<VerdictLine>,
    counts: ReportCounts,
    steps: Vec<StepLine>,
    humans: Vec<HumanLine>,
    handlers: Vec<HandlerLine>,
    segments: Vec<SegmentLine>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VerdictLine {
    status: String,
    degraded: bool,
    summary: String,
}

/// The tallies of 08 §6.2: judged (pass / fail / unknown — unknown is
/// never folded into fail, 06 §7.3), `unverified` annotations (R4), and
/// the degraded-verdict count.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportCounts {
    steps: u32,
    pass: u32,
    fail: u32,
    unknown: u32,
    unverified: u32,
    degraded: u32,
    /// Verdict write-backs whose remote archival failed (04 §5 — the
    /// local verdicts are untouched; this is the report annotation).
    remote_archival_failed: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StepLine {
    /// Canonical rendered run path (spine §9 grammar, 8-hex digests).
    run_path: String,
    step_id: String,
    /// `pass`/`fail`/`unknown` once judged; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    verdict_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    degraded: Option<bool>,
    /// R4 annotation: executed (dispatched) with no assertions — an
    /// execution-state note, not a verdict.
    unverified: bool,
    /// The instance's final `stepExited` state (closed `StepState`
    /// vocabulary) — the honest label for verdict-less control steps
    /// (foreach/let containers exit `judged` without a verdict of their
    /// own; `skipped` marks untaken branches).
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    /// Dispatch attempts of this instance.
    attempts: u32,
    /// The last non-succeeded attempt's error class, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    error_class: Option<String>,
    /// Typed evidence-localization failures across this instance's
    /// judgments (item ③): count of `localizationGaps` entries — the
    /// honest-gap tally, never a silent omission.
    #[serde(skip_serializing_if = "is_zero")]
    evidence_gaps: u32,
    /// Prior verdicts overtaken by this instance's current verdict —
    /// the full supersedes lineage: in-run re-folds (handler retry
    /// rounds, 07 §1), human overrules (06), and offline re-judges on
    /// resume (07 §5.3). History kept, never deleted.
    superseded: u32,
}

/// One human interaction — the 08 §6.3 "who judged what, when" line.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HumanLine {
    request_id: String,
    /// `step` vs `supervision` (R13).
    purpose: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    prompt: String,
    requested_at_ms: u64,
    /// The arbitrated response payload, verbatim; absent while pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    responded_at_ms: Option<u64>,
    /// A terminal exit of the awaiting step settled this request WITHOUT
    /// a response — the lazy timeout settlement / aborted disposition
    /// (06 §5.3; mirrors the inbox projection's rule).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    closed_unanswered: bool,
    /// The request event's verbatim path (the settle join key, exact
    /// equality as in the inbox projection). Not part of the output.
    #[serde(skip)]
    raw_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HandlerLine {
    /// The anchor path the hook fired at.
    run_path: String,
    hook: String,
    /// Highest one-based trigger count seen (toward `maxTriggers`).
    triggers: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SegmentLine {
    /// `started` or `resumed`.
    kind: String,
    at_ms: u64,
    /// Explicitly `null` when unsupervised (R13, per segment).
    supervise_policy: Option<String>,
    /// Alignment class tallies of a resume segment.
    #[serde(skip_serializing_if = "Option::is_none")]
    alignment: Option<AlignmentCounts>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AlignmentCounts {
    reusable: u32,
    judge_dirty: u32,
    effect_dirty: u32,
    new: u32,
    orphaned: u32,
}

/// `pointlock report`: assembles the report from the run's ledger and
/// checkpoint, renders text or the versioned JSON envelope. Read-only;
/// exit 0 on a successfully rendered report regardless of the run's own
/// verdict (`pointlock run`/`resume` carry the verdict exit codes).
pub fn report(store_dir: &Path, run_id: &str, format: OutputFormat) -> Result<i32, Failure> {
    // A caller-supplied run id that resolves to nothing is a usage
    // error, not an infrastructure failure (the `locate` convention).
    let report_failure = |err: pointlock_store::StoreError| match err {
        pointlock_store::StoreError::UnknownRun(_) => usage_failure(err.to_string()),
        other => store_failure(other),
    };
    let store = Store::open(store_dir).map_err(store_failure)?;
    let overview =
        pointlock_store::projection::run_overview(&store, run_id).map_err(report_failure)?;
    let view = store.rebuild_checkpoint(run_id).map_err(report_failure)?;
    let events = store.events(run_id).map_err(report_failure)?;

    // Verdict lineage per step instance: every verdictRecorded anchored
    // at an instance path; the count minus one is the superseded tally.
    let mut lineage: BTreeMap<String, u32> = BTreeMap::new();
    let mut gap_tally: BTreeMap<String, u32> = BTreeMap::new();
    let mut exit_states: BTreeMap<String, String> = BTreeMap::new();
    let mut humans: Vec<HumanLine> = Vec::new();
    let mut handlers: BTreeMap<(String, String), (String, u64)> = BTreeMap::new();
    let mut segments: Vec<SegmentLine> = Vec::new();
    for event in &events {
        match &event.payload {
            RunLogPayload::VerdictRecorded {
                localization_gaps, ..
            } => {
                let site = site_key(&instance_path(&event.run_path));
                // Gaps tally per INSTANCE (hash-bearing) — the dossier's
                // granularity; the supersedes lineage joins per SITE
                // (across repair boundaries).
                *gap_tally
                    .entry(render_run_path(&instance_path(&event.run_path)))
                    .or_insert(0) += localization_gaps.len() as u32;
                *lineage.entry(site).or_insert(0) += 1;
            }
            RunLogPayload::StepExited { state, .. } => {
                exit_states.insert(site_key(&instance_path(&event.run_path)), wire_str(state));
                // A terminal exit of the awaiting step settles its
                // pending request without a response (06 §5.3; the
                // inbox projection's exact-path rule).
                let exited = render_run_path(&event.run_path);
                for line in humans
                    .iter_mut()
                    .filter(|line| line.raw_path == exited && line.response.is_none())
                {
                    line.closed_unanswered = true;
                }
            }
            RunLogPayload::HumanRequested {
                request_id,
                purpose,
                mode,
                prompt,
                ..
            } => humans.push(HumanLine {
                request_id: request_id.clone(),
                purpose: wire_str(purpose),
                mode: mode.as_ref().map(wire_str),
                prompt: prompt.clone(),
                requested_at_ms: event.at_ms,
                response: None,
                actor: None,
                responded_at_ms: None,
                closed_unanswered: false,
                raw_path: render_run_path(&event.run_path),
            }),
            RunLogPayload::HumanResponded {
                request_id,
                response,
                actor,
                ..
            } => {
                // A supervision `suspend` answer is non-final (spine
                // §6.9; the store arbitration keeps the request open) —
                // the eventual proceed/abort ruling overwrites it, so
                // the line carries the arbitrated FINAL answer.
                if let Some(line) = humans.iter_mut().find(|line| {
                    line.request_id == *request_id
                        && (line.response.is_none() || nonfinal_suspend(line))
                }) {
                    line.response = Some(response.clone());
                    line.actor = Some(actor.clone());
                    line.responded_at_ms = Some(event.at_ms);
                }
            }
            RunLogPayload::HandlerTriggered { hook, trigger, .. } => {
                // Site-keyed so a hook firing across a repair boundary
                // (new irHash in the flow frame) stays one line; the
                // display path is the first-seen rendering.
                let key = (site_key(&event.run_path), wire_str(hook));
                let entry = handlers
                    .entry(key)
                    .or_insert_with(|| (render_run_path(&event.run_path), 0));
                entry.1 = entry.1.max(*trigger);
            }
            RunLogPayload::RunStarted {
                supervise_policy, ..
            } => segments.push(SegmentLine {
                kind: "started".to_owned(),
                at_ms: event.at_ms,
                supervise_policy: supervise_policy.as_ref().map(wire_str),
                alignment: None,
            }),
            RunLogPayload::RunResumed {
                alignment_report,
                supervise_policy,
                ..
            } => segments.push(SegmentLine {
                kind: "resumed".to_owned(),
                at_ms: event.at_ms,
                supervise_policy: supervise_policy.as_ref().map(wire_str),
                alignment: Some(alignment_counts(alignment_report)),
            }),
            _ => {}
        }
    }

    let steps: Vec<StepLine> = view
        .completed
        .iter()
        .map(|record| step_line(record, &lineage, &exit_states, &gap_tally))
        .collect();
    let mut counts = tally(&steps);
    counts.remote_archival_failed = count_remote_archival_failures(&events);

    let run_report = RunReport {
        pointlock_report: 1,
        run_id: overview.run_id,
        flow_id: overview.flow_id,
        ir_hash: overview.ir_hash,
        lockfile_digest: overview.lockfile_digest,
        device_id: overview.device_id,
        session_lineage: overview.session_lineage,
        status: overview.status,
        created_at_ms: overview.created_at_ms,
        finished_at_ms: overview.finished_at_ms,
        // The LAST runFinished wins, verbatim — a verdict-less final
        // finish (aborted segment) clears the field rather than
        // resurrecting an earlier segment's superseded verdict (the
        // run_overview semantics; the report and inspect must agree).
        flow_verdict: events
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                RunLogPayload::RunFinished { verdict, .. } => {
                    Some(verdict.as_ref().map(|verdict| VerdictLine {
                        status: wire_str(&verdict.status),
                        degraded: verdict.degraded,
                        summary: verdict.summary.clone(),
                    }))
                }
                _ => None,
            })
            .flatten(),
        counts,
        steps,
        humans,
        handlers: handlers
            .into_iter()
            .map(|((_site, hook), (run_path, triggers))| HandlerLine {
                run_path,
                hook,
                triggers,
            })
            .collect(),
        segments,
    };

    match format {
        OutputFormat::Json => {
            let body = serde_json::to_string_pretty(&run_report)
                .map_err(|err| Failure::new(exit::INTERNAL, format!("serialize report: {err}")))?;
            println!("{body}");
        }
        OutputFormat::Text => print_text(&run_report),
    }
    Ok(exit::PASS)
}

/// The hash-elided site key of a path: joins that must survive a
/// cross-IR repair boundary (superseded-lineage counts, exit states,
/// handler tallies) key by step-site identity — the flow frame's irHash
/// changes across a repair while the site stays the same step chain
/// (07 §5 adoption semantics). Never user-visible; display paths keep
/// the canonical hash-bearing rendering.
fn site_key(path: &RunPath) -> String {
    path.iter()
        .map(|frame| match frame {
            PathFrame::Flow { flow_id, .. } => format!("f:{flow_id}"),
            PathFrame::Step { step_id } => format!("s:{step_id}"),
            PathFrame::Call {
                step_id,
                callee_flow_id,
                ..
            } => format!(
                "c:{}→{callee_flow_id}",
                step_id
                    .as_ref()
                    .map(|step_id| step_id.as_str())
                    .unwrap_or("")
            ),
            other => pointlock_ir::render_run_path(std::slice::from_ref(other)),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether a line's stored response is a non-final supervision
/// `suspend` (spine §6.9) — overwritable by the eventual final ruling.
fn nonfinal_suspend(line: &HumanLine) -> bool {
    line.purpose == "supervision"
        && line
            .response
            .as_ref()
            .and_then(|response| response.get("decision"))
            .and_then(serde_json::Value::as_str)
            == Some("suspend")
}

/// serde gate for the gap tally.
fn is_zero(count: &u32) -> bool {
    *count == 0
}

/// Instance identity: strip attempt/phase/assertion frames (spine §9).
fn instance_path(path: &RunPath) -> RunPath {
    path.iter()
        .filter(|frame| {
            !matches!(
                frame,
                PathFrame::Attempt { .. } | PathFrame::Phase { .. } | PathFrame::Assertion { .. }
            )
        })
        .cloned()
        .collect()
}

fn step_line(
    record: &StepRecord,
    lineage: &BTreeMap<String, u32>,
    exit_states: &BTreeMap<String, String>,
    gap_tally: &BTreeMap<String, u32>,
) -> StepLine {
    let site = site_key(&instance_path(&record.run_path));
    let error_class = record
        .attempts
        .iter()
        .rev()
        .find_map(|attempt| attempt.error_class.as_ref().map(wire_str));
    let state = exit_states.get(&site).cloned();
    StepLine {
        step_id: record.step_id.as_str().to_owned(),
        verdict_status: record
            .verdict
            .as_ref()
            .map(|verdict| wire_str(&verdict.status)),
        degraded: record.verdict.as_ref().map(|verdict| verdict.degraded),
        // R4: dispatched with no assertions AND a judged exit ⇒ the
        // annotation. A cancelled/aborted dispatch is labeled by its
        // exit state instead — "executed, unverified" would overclaim.
        unverified: record.verdict.is_none()
            && !record.attempts.is_empty()
            && state.as_deref() == Some("judged"),
        state,
        attempts: record.attempts.len() as u32,
        error_class,
        superseded: lineage
            .get(&site)
            .map(|count| count.saturating_sub(1))
            .unwrap_or(0),
        evidence_gaps: gap_tally
            .get(&render_run_path(&instance_path(&record.run_path)))
            .copied()
            .unwrap_or(0),
        run_path: render_run_path(&record.run_path),
    }
}

fn tally(steps: &[StepLine]) -> ReportCounts {
    let mut counts = ReportCounts {
        steps: steps.len() as u32,
        pass: 0,
        fail: 0,
        unknown: 0,
        unverified: 0,
        degraded: 0,
        // Tallied from the event stream by the caller (not step-derived).
        remote_archival_failed: 0,
    };
    for step in steps {
        match step.verdict_status.as_deref() {
            Some("pass") => counts.pass += 1,
            Some("fail") => counts.fail += 1,
            // 06 §7.3: unknown is its own tally, never folded into fail.
            Some("unknown") => counts.unknown += 1,
            _ => {}
        }
        if step.unverified {
            counts.unverified += 1;
        }
        if step.degraded == Some(true) {
            counts.degraded += 1;
        }
    }
    counts
}

/// Tallies verdict write-backs whose remote archival failed — 04 §5's
/// report annotation, across step verdicts and run-finish flow verdicts.
fn count_remote_archival_failures(events: &[pointlock_ir::RunLogEvent]) -> u32 {
    events
        .iter()
        .filter(|event| {
            matches!(
                &event.payload,
                RunLogPayload::VerdictRecorded {
                    remote_archival_error: Some(_),
                    ..
                } | RunLogPayload::RunFinished {
                    remote_archival_error: Some(_),
                    ..
                }
            )
        })
        .count() as u32
}

fn alignment_counts(report: &AlignmentReport) -> AlignmentCounts {
    let mut counts = AlignmentCounts {
        reusable: 0,
        judge_dirty: 0,
        effect_dirty: 0,
        new: 0,
        orphaned: 0,
    };
    for entry in &report.entries {
        match entry.class {
            AlignmentClass::Reusable => counts.reusable += 1,
            AlignmentClass::JudgeDirty => counts.judge_dirty += 1,
            AlignmentClass::EffectDirty => counts.effect_dirty += 1,
            AlignmentClass::New => counts.new += 1,
            AlignmentClass::Orphaned => counts.orphaned += 1,
        }
    }
    counts
}

fn print_text(report: &RunReport) {
    println!("run: {}", report.run_id);
    println!("flow: {} ({})", report.flow_id, report.ir_hash);
    println!("lockfile: {}", report.lockfile_digest);
    println!(
        "device: {} (sessions: {})",
        report.device_id,
        report.session_lineage.join(" → ")
    );
    print!("status: {}", report.status);
    match report.finished_at_ms {
        Some(finished) => println!(
            " (created {} ms, finished {finished} ms)",
            report.created_at_ms
        ),
        None => println!(" (created {} ms)", report.created_at_ms),
    }
    match &report.flow_verdict {
        Some(verdict) => println!(
            "flow verdict: {}{} — {}",
            verdict.status,
            if verdict.degraded { " [degraded]" } else { "" },
            verdict.summary
        ),
        None => println!("flow verdict: none (unfinished, finished unverified, or aborted)"),
    }
    let counts = &report.counts;
    println!(
        "steps: {} — {} pass, {} fail, {} unknown; {} unverified; {} degraded",
        counts.steps, counts.pass, counts.fail, counts.unknown, counts.unverified, counts.degraded
    );
    if counts.remote_archival_failed > 0 {
        println!(
            "remote archival failed: {} verdict write-back(s) — local verdicts \
             unaffected (04 §5)",
            counts.remote_archival_failed
        );
    }

    println!();
    println!("steps:");
    for step in &report.steps {
        let mut line = format!("  {}  ", step.run_path);
        match &step.verdict_status {
            Some(status) => {
                line.push_str(status);
                if step.degraded == Some(true) {
                    line.push_str(" [degraded]");
                }
            }
            None if step.unverified => line.push_str("unverified (executed, no assertions)"),
            // A verdict-less, dispatch-less record: a control step or an
            // untaken branch — its exit state is the honest label.
            None => match &step.state {
                Some(state) if state == "skipped" => line.push_str("skipped (branch not taken)"),
                Some(state) => line.push_str(&format!("no verdict (state {state})")),
                None => line.push_str("no verdict"),
            },
        }
        if step.superseded > 0 {
            line.push_str(&format!(" (supersedes {} prior)", step.superseded));
        }
        if let Some(error_class) = &step.error_class {
            line.push_str(&format!(" [last error: {error_class}]"));
        }
        if step.evidence_gaps > 0 {
            line.push_str(&format!(" [evidence gaps: {}]", step.evidence_gaps));
        }
        println!("{line}");
    }

    if !report.humans.is_empty() {
        println!();
        println!("humans:");
        for human in &report.humans {
            let mode = human.mode.as_deref().unwrap_or("-");
            let mut line = format!(
                "  {} {} ({mode}) \"{}\" requested at {} ms",
                human.request_id, human.purpose, human.prompt, human.requested_at_ms
            );
            match (&human.response, &human.actor, human.responded_at_ms) {
                (Some(response), Some(actor), Some(at_ms)) => {
                    line.push_str(&format!(" → {response} by {actor} at {at_ms} ms"));
                }
                _ if human.closed_unanswered => {
                    line.push_str(" → closed unanswered (settled by step exit, 06 §5.3)");
                }
                _ => line.push_str(" → pending"),
            }
            println!("{line}");
        }
    }

    if !report.handlers.is_empty() {
        println!();
        println!("handlers:");
        for handler in &report.handlers {
            println!(
                "  {} {} ×{}",
                handler.run_path, handler.hook, handler.triggers
            );
        }
    }

    println!();
    println!("segments:");
    for (index, segment) in report.segments.iter().enumerate() {
        let supervise = segment.supervise_policy.as_deref().unwrap_or("null");
        let mut line = format!(
            "  {}. {} at {} ms, supervise: {supervise}",
            index + 1,
            segment.kind,
            segment.at_ms
        );
        if let Some(alignment) = &segment.alignment {
            line.push_str(&format!(
                ", alignment: {} reusable / {} judgeDirty / {} effectDirty / {} new / {} orphaned",
                alignment.reusable,
                alignment.judge_dirty,
                alignment.effect_dirty,
                alignment.new,
                alignment.orphaned
            ));
        }
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pointlock_ir::{RunLogEvent, Verdict, VerdictStatus};

    fn event(seq: u64, payload: RunLogPayload) -> RunLogEvent {
        RunLogEvent {
            run_id: "run-1".to_owned(),
            seq,
            at_ms: 1_000 + seq,
            run_path: Vec::new(),
            payload,
        }
    }

    fn verdict() -> Verdict {
        Verdict {
            status: VerdictStatus::Pass,
            degraded: false,
            summary: "pass".to_owned(),
            evidence: Vec::new(),
            supersedes: None,
        }
    }

    #[test]
    fn remote_archival_failures_are_tallied_across_both_carriers() {
        // 04 §5: the report is THE annotation surface — both the step
        // verdict and the run-finish flow verdict carriers count.
        let annotated = vec![
            event(
                1,
                RunLogPayload::VerdictRecorded {
                    verdict: verdict(),
                    localized: Vec::new(),
                    localization_gaps: Vec::new(),
                    remote_archival_error: Some("remote archival failed: gone".to_owned()),
                },
            ),
            event(
                2,
                RunLogPayload::VerdictRecorded {
                    verdict: verdict(),
                    localized: Vec::new(),
                    localization_gaps: Vec::new(),
                    remote_archival_error: None,
                },
            ),
            event(
                3,
                RunLogPayload::RunFinished {
                    verdict: Some(verdict()),
                    remote_archival_error: Some("remote archival failed: gone".to_owned()),
                },
            ),
        ];
        assert_eq!(count_remote_archival_failures(&annotated), 2);

        // Negative control: annotation-free ledgers tally zero.
        let clean = vec![
            event(
                1,
                RunLogPayload::VerdictRecorded {
                    verdict: verdict(),
                    localized: Vec::new(),
                    localization_gaps: Vec::new(),
                    remote_archival_error: None,
                },
            ),
            event(
                2,
                RunLogPayload::RunFinished {
                    verdict: Some(verdict()),
                    remote_archival_error: None,
                },
            ),
        ];
        assert_eq!(count_remote_archival_failures(&clean), 0);
    }
}
