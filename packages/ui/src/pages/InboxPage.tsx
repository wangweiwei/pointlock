// The human inbox (08 §2.6): READ-ONLY presentation. Responses go
// through pointlock-human-cli — the webUi collect channel is a reserved
// v0.2 surface (06 §4.2); each entry shows the copyable CLI guidance
// instead. Supervision gates share the box (R13), discriminated by
// `purpose`, with no countdown (they never time out).

import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import type { HumanInboxEntry } from "@pointlock/projection-types";
import { api, evidenceUrl, subscribeRevision } from "../api";

/** Renders the materialized exhibits (08 §2.6): plain values inline,
 * EvidenceRef-shaped items through the gallery route. */
function Presents(props: { presents: unknown }) {
  if (!Array.isArray(props.presents) || props.presents.length === 0) {
    return null;
  }
  return (
    <div className="gallery" style={{ margin: "6px 0" }}>
      {props.presents.map((item, index) => {
        const ref = item as {
          sha256?: string;
          asset?: { mediaType?: string };
          evidence?: { sha256?: string; asset?: { mediaType?: string } };
        };
        const evidence = ref.evidence ?? (ref.sha256 ? ref : null);
        if (evidence?.sha256) {
          const isImage = evidence.asset?.mediaType?.startsWith("image/");
          return isImage ? (
            <img
              key={index}
              src={evidenceUrl(evidence.sha256)}
              alt={evidence.sha256.slice(0, 8)}
              title={evidence.sha256}
            />
          ) : (
            <a
              key={index}
              className="chip"
              href={evidenceUrl(evidence.sha256)}
              target="_blank"
              rel="noreferrer"
            >
              ⬇ {evidence.sha256.slice(0, 8)}
            </a>
          );
        }
        return <pre key={index}>{JSON.stringify(item, null, 2)}</pre>;
      })}
    </div>
  );
}

function Countdown(props: { deadlineAtMs: number }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const left = props.deadlineAtMs - now;
  if (left <= 0) {
    return (
      <span className="chip verdict-unknown">
        deadline passed — settles to unknown
      </span>
    );
  }
  return (
    <span className="chip">
      {Math.ceil(left / 1000)}s left → then unknown
    </span>
  );
}

export function InboxPage() {
  const [entries, setEntries] = useState<HumanInboxEntry[]>([]);
  const [revision, setRevision] = useState(0);
  const [error, setError] = useState<string | null>(null);

  // The global stream (store-wide revision) keeps the box live.
  useEffect(() => subscribeRevision(null, setRevision), []);

  useEffect(() => {
    api
      .inbox()
      .then((body) => {
        setEntries(body.inbox);
        setError(null);
      })
      .catch((err: Error) => setError(err.message));
  }, [revision]);

  if (error) return <div className="page-pad">⚠ {error}</div>;

  return (
    <div className="page-pad">
      {entries.length === 0 && (
        <div className="panel" style={{ color: "var(--muted)" }}>
          inbox empty — no pending human requests
        </div>
      )}
      {entries.map((entry) => (
        <div key={entry.requestId} className="panel inbox-entry">
          <div>
            <span className="chip">
              {entry.purpose === "supervision"
                ? "🛡 supervision"
                : `🧍 ${entry.mode ?? "step"}`}
            </span>
            <Link to={`/runs/${encodeURIComponent(entry.runId)}`}>
              {entry.runId}
            </Link>{" "}
            · {entry.flowId} · <code>{entry.runPath}</code>
          </div>
          <p style={{ margin: "6px 0" }}>{entry.prompt}</p>
          <Presents presents={entry.presents} />
          {entry.decisions && (
            <div>
              {entry.decisions.map((decision) => (
                <span key={decision} className="chip">
                  {decision}
                </span>
              ))}
            </div>
          )}
          {entry.deadlineAtMs !== undefined && entry.deadlineAtMs !== null ? (
            <Countdown deadlineAtMs={entry.deadlineAtMs} />
          ) : (
            <span className="chip">no timeout — suspend anytime</span>
          )}
          <div className="respond" style={{ marginTop: 6 }}>
            respond in a terminal:{" "}
            <code>pointlock resume &lt;flow-ir&gt; --store &lt;store&gt; --run {entry.runId} --interactive</code>
          </div>
        </div>
      ))}
    </div>
  );
}
