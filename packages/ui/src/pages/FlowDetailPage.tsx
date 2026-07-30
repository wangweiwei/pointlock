// Flow detail (08 §2.3): the static graph of a selected version + the
// run selector. New-run creation is deliberately absent (v0.1 non-goal
// 13 — the CLI starts runs; the UI observes and repairs).

import { useEffect, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { GraphView } from "../components/GraphView";
import { api, type FlowDetail } from "../api";

export function FlowDetailPage() {
  const { flowId = "" } = useParams();
  const [search, setSearch] = useSearchParams();
  const irHash = search.get("irHash") ?? undefined;
  const [detail, setDetail] = useState<FlowDetail | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .flowDetail(flowId, irHash)
      .then((body) => {
        setDetail(body);
        setError(null);
      })
      .catch((err: Error) => setError(err.message));
  }, [flowId, irHash]);

  if (error) return <div className="page-pad">⚠ {error}</div>;
  if (!detail) return <div className="page-pad">loading…</div>;

  return (
    <div
      className="page-pad"
      style={{ display: "grid", gridTemplateColumns: "2fr 1fr", gap: 12 }}
    >
      <div className="panel graph-pane" style={{ height: "calc(100vh - 96px)" }}>
        <GraphView view={detail.graph} overview={null} />
      </div>
      <div className="panel" style={{ overflow: "auto" }}>
        <h4 style={{ marginTop: 0 }}>{detail.flowId}</h4>
        <div>
          version:{" "}
          <select
            value={irHash ?? detail.graph.irHash}
            onChange={(event) => setSearch({ irHash: event.target.value })}
          >
            {detail.versions.map((version) => (
              <option key={version.irHash} value={version.irHash}>
                {version.irHash.replace("sha256:", "").slice(0, 8)} ·{" "}
                {new Date(version.modifiedAtMs).toLocaleString()}
              </option>
            ))}
          </select>
        </div>
        <h4>runs</h4>
        <table className="list">
          <tbody>
            {detail.runs.map((run) => (
              <tr key={run.runId}>
                <td>
                  <Link to={`/runs/${encodeURIComponent(run.runId)}`}>
                    {run.runId}
                  </Link>
                </td>
                <td>
                  <span
                    className={`chip ${run.flowVerdictStatus ? `verdict-${run.flowVerdictStatus}` : ""}`}
                  >
                    {run.flowVerdictStatus ?? run.status}
                  </span>
                </td>
                <td>{run.deviceId}</td>
                <td>{new Date(run.createdAtMs).toLocaleString()}</td>
              </tr>
            ))}
            {detail.runs.length === 0 && (
              <tr>
                <td style={{ color: "var(--muted)" }}>
                  no runs yet — start one with `pointlock run`
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
