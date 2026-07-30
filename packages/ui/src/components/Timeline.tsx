// The timeline pane (08 §4): monotonic seq order, the five-filter
// vocabulary, 50/page synchronized snapshots. A moved revision never
// shifts the visible page — it lights the "updates" chip; the user (or a
// selection change) re-pulls. Selection highlight joins on instance
// anchors (boundary-aware — 08 §2.4 linkage) and scrolls into view.

import { useEffect, useRef, useState } from "react";
import type { RunTimelineEntry, TimelinePage } from "@pointlock/projection-types";
import { api } from "../api";
import { matchesSelection } from "../adapter/graph";

const FILTERS = ["all", "observations", "actions", "errors", "verdicts"] as const;

function summaryOf(entry: RunTimelineEntry): string {
  const detail = entry.detail as Record<string, unknown> & { type: string };
  switch (detail.type) {
    case "runStarted":
      return detail.supervisePolicy
        ? `supervised (${detail.supervisePolicy})`
        : "unsupervised";
    case "stepEntered":
      return String(detail.stepId);
    case "stepExited":
      return String(detail.state);
    case "preflightProbed":
      // I3 / 07 §4.2 rule 1: an unprobed resume SAYS so — never three
      // zeroes the reader must decode.
      return detail.unprobed
        ? "no probes declared — this resume re-touched the world unchecked"
        : `${detail.pass} pass · ${detail.fail} fail · ${detail.unknown} unknown`;
    case "actionIntent":
      return `callId ${detail.callId}`;
    case "actionSettled":
      return `${detail.callId} → ${detail.outcome}${detail.executionMode ? ` (${detail.executionMode})` : ""}`;
    case "observationRecorded":
      return String(detail.observationId);
    case "assertionEvaluated":
      return `${detail.assertId}: ${detail.result}${detail.channel ? ` @${detail.channel}` : ""}`;
    case "verdictRecorded":
      return `${detail.status}${detail.degraded ? " [degraded]" : ""}${detail.supersedes ? " (re-judged)" : ""}`;
    case "humanRequested":
      return `${detail.purpose}: ${detail.prompt}`;
    case "humanResponded":
      return `${detail.requestId} by ${detail.actor}`;
    case "handlerTriggered":
      return `${detail.hook}${detail.disposition ? `→${detail.disposition}` : ""} #${detail.trigger}`;
    case "callFramePushed":
      return `${detail.callee}${detail.rebase ? " — re-entered under the repaired callee" : ""}`;
    case "callFramePopped":
      return detail.hasOutputs ? "returned outputs" : "returned";
    case "runSuspended":
      return detail.reason ? `suspended: ${detail.reason}` : "suspended";
    case "runResumed": {
      const alignment = (detail.alignment as { value?: unknown } | undefined)
        ?.value;
      return `resumed${alignment ? ` — align ${JSON.stringify(alignment)}` : ""}`;
    }
    case "runFinished":
      return detail.status ? String(detail.status) : "unverified/aborted";
    case "overLimit":
      return `over-limit ${detail.eventType} — see the inspector`;
    default:
      return "";
  }
}

/** Honest-marker chips of one entry (08 §3.4/§4.2: every degradation
 * conspicuous — unprobed resumes, frame rebases, degraded dispatch,
 * failed remote archival). */
function markerChips(entry: RunTimelineEntry): [string, string][] {
  const detail = entry.detail as Record<string, unknown> & { type: string };
  const chips: [string, string][] = [];
  if (detail.type === "preflightProbed" && detail.unprobed === true) {
    chips.push(["unprobed", "verdict-unknown"]);
  }
  if (detail.type === "callFramePushed" && detail.rebase === true) {
    chips.push(["rebase", ""]);
  }
  if (detail.type === "actionSettled" && detail.fallbackReason) {
    chips.push([`degraded: ${String(detail.fallbackReason)}`, "verdict-unknown"]);
  }
  if (
    (detail.type === "verdictRecorded" || detail.type === "runFinished") &&
    detail.remoteArchivalError
  ) {
    chips.push(["remote archival failed", "verdict-unknown"]);
  }
  return chips;
}

/** The typed error surface of a non-success settlement (08 §4.2). */
function errorLineOf(entry: RunTimelineEntry): string | null {
  const detail = entry.detail as Record<string, unknown> & { type: string };
  if (detail.type !== "actionSettled" || !detail.error) return null;
  const error = detail.error as {
    code: string;
    message: string;
    retryable: boolean;
    errorClass?: string | null;
  };
  return `${error.errorClass ? `[${error.errorClass}] ` : ""}${error.code}: ${error.message}${error.retryable ? " (retryable)" : ""}`;
}

/** Run-level / structural entries carry no step dossier to open. */
function isStructural(entry: RunTimelineEntry): boolean {
  const type = (entry.detail as { type: string }).type;
  return (
    type === "runStarted" ||
    type === "runFinished" ||
    type === "runSuspended" ||
    type === "runResumed"
  );
}

export function Timeline(props: {
  runId: string;
  revision: number;
  onSelect: (runPath: string) => void;
  selectedPath: string | null;
}) {
  const [filter, setFilter] = useState<(typeof FILTERS)[number]>("all");
  const [page, setPage] = useState(1);
  const [snapshot, setSnapshot] = useState<TimelinePage | null>(null);
  const [stale, setStale] = useState(false);
  const selectedRef = useRef<HTMLDivElement | null>(null);

  const load = (p = page, f = filter) => {
    let alive = true;
    api
      .timeline(props.runId, f, p)
      .then((result) => {
        if (!alive) return;
        setSnapshot(result);
        setStale(false);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  };

  useEffect(() => {
    setPage(1);
    return load(1, filter);
  }, [props.runId, filter]);

  useEffect(() => {
    if (snapshot && props.revision > snapshot.revision) setStale(true);
  }, [props.revision]);

  // §2.4 linkage: a new selection scrolls its first visible event into
  // view on the current snapshot.
  useEffect(() => {
    selectedRef.current?.scrollIntoView({ block: "nearest" });
  }, [props.selectedPath, snapshot]);

  const pages = snapshot
    ? Math.max(1, Math.ceil(snapshot.total / snapshot.pageSize))
    : 1;

  let firstSelected = true;

  return (
    <div>
      <div className="tl-filters">
        {FILTERS.map((name) => (
          <button
            key={name}
            className={name === filter ? "on" : ""}
            onClick={() => setFilter(name)}
          >
            {name}
          </button>
        ))}
        {stale && (
          <button className="tl-more" onClick={() => load()}>
            ⟳ updates available
          </button>
        )}
      </div>
      {snapshot?.entries.map((entry) => {
        const structural = isStructural(entry);
        const selected =
          !structural &&
          props.selectedPath !== null &&
          matchesSelection(entry.runPath, props.selectedPath);
        const anchor = selected && firstSelected;
        if (anchor) firstSelected = false;
        return (
          <div
            key={entry.seq}
            ref={anchor ? selectedRef : undefined}
            className={`tl-entry${structural ? " divider" : ""}${selected ? " selected" : ""}`}
            onClick={() => !structural && props.onSelect(entry.runPath)}
          >
            <span className="seq">#{entry.seq}</span>
            <span className="etype">
              {(entry.detail as { type: string }).type}
            </span>
            {summaryOf(entry)}
            {markerChips(entry).map(([label, cls], index) => (
              <span key={index} className={`chip ${cls}`.trim()}>
                {label}
              </span>
            ))}
            {errorLineOf(entry) && (
              <div className="kind">{errorLineOf(entry)}</div>
            )}
            {entry.truncated && (
              <span className="chip verdict-unknown">truncated</span>
            )}
            {entry.evidenceOmitted > 0 && (
              <span className="chip">
                +{entry.evidenceOmitted} evidence omitted
              </span>
            )}
          </div>
        );
      })}
      <div style={{ display: "flex", gap: 8, padding: 6, alignItems: "center" }}>
        <button
          disabled={page <= 1}
          onClick={() => {
            setPage(page - 1);
            load(page - 1);
          }}
        >
          ‹
        </button>
        <span>
          page {snapshot?.page ?? page} / {pages} · {snapshot?.total ?? 0}{" "}
          events
        </span>
        <button
          disabled={page >= pages}
          onClick={() => {
            setPage(page + 1);
            load(page + 1);
          }}
        >
          ›
        </button>
      </div>
    </div>
  );
}
