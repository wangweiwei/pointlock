// The flow list (08 §2.2): one row per flowId — latest irHash prefix,
// lockfileDigest, the recent-runs verdict strip, last run time. No
// search/tags — two-digit flow counts, a list is enough.

import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { api, type FlowIndexEntry } from "../api";

function verdictClass(run: { status: string; flowVerdictStatus?: string }) {
  if (run.flowVerdictStatus) return `verdict-${run.flowVerdictStatus}`;
  return "";
}

export function FlowsPage() {
  const [flows, setFlows] = useState<FlowIndexEntry[]>([]);
  const [currentDigest, setCurrentDigest] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .flows()
      .then((body) => {
        setFlows(body.flows);
        setCurrentDigest(body.currentLockfileDigest ?? null);
      })
      .catch((err: Error) => setError(err.message));
  }, []);

  if (error) return <div className="page-pad">⚠ {error}</div>;

  return (
    <div className="page-pad">
      <div className="panel">
        <table className="list">
          <thead>
            <tr>
              <th>flow</th>
              <th>latest irHash</th>
              <th>lockfileDigest</th>
              <th>versions</th>
              <th>recent runs</th>
              <th>last run</th>
            </tr>
          </thead>
          <tbody>
            {flows.map((flow) => {
              const recent = flow.runs.slice(-8);
              const last = flow.runs[flow.runs.length - 1];
              return (
                <tr key={flow.flowId}>
                  <td>
                    <Link to={`/flows/${encodeURIComponent(flow.flowId)}`}>
                      <b>{flow.flowId}</b>
                    </Link>
                  </td>
                  <td>
                    <code>
                      {flow.latestIrHash
                        ? flow.latestIrHash.replace("sha256:", "").slice(0, 8)
                        : "— artifact missing"}
                    </code>
                  </td>
                  <td>
                    {flow.versions.length > 0 ? (
                      (() => {
                        const digest =
                          flow.versions[flow.versions.length - 1].lockfileDigest;
                        // 08 §2.2: stale against the CURRENT lockfile →
                        // flagged; no comparison input → digest only,
                        // never a guessed judgment.
                        const stale =
                          currentDigest !== null && digest !== currentDigest;
                        return (
                          <code
                            className={stale ? "chip verdict-unknown" : undefined}
                            title={
                              stale
                                ? "compiled against an OLDER lockfile — re-lock or re-compile"
                                : currentDigest !== null
                                  ? "matches the current lockfile"
                                  : "no --lockfile on the host; staleness unknown"
                            }
                          >
                            {digest.replace("sha256:", "").slice(0, 8)}
                            {stale ? " ⚠ stale" : ""}
                          </code>
                        );
                      })()
                    ) : (
                      <code>—</code>
                    )}
                  </td>
                  <td>{flow.versions.length}</td>
                  <td>
                    {recent.map((run) => (
                      <Link
                        key={run.runId}
                        to={`/runs/${encodeURIComponent(run.runId)}`}
                        className={`chip ${verdictClass(run)}`}
                        title={`${run.runId}: ${run.flowVerdictStatus ?? run.status}`}
                      >
                        {run.flowVerdictStatus ?? run.status}
                      </Link>
                    ))}
                  </td>
                  <td>
                    {last ? new Date(last.createdAtMs).toLocaleString() : "—"}
                  </td>
                </tr>
              );
            })}
            {flows.length === 0 && (
              <tr>
                <td colSpan={6} style={{ color: "var(--muted)" }}>
                  No flows: pass --artifacts to `pointlock inspect --serve`, or
                  run one first.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
