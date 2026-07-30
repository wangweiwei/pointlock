// Run detail (08 §2.4): THE page — three linked panes. Graph node click
// → timeline highlight + inspector dossier; timeline entry click → node
// highlight + inspector. Live updates ride the revision channel (SSE
// with polling fallback — 08 §5). Deep link: /runs/:runId/steps/:runPath.

import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import type { RunOverview } from "@pointlock/projection-types";
import { GraphView } from "../components/GraphView";
import { Inspector } from "../components/Inspector";
import { Timeline } from "../components/Timeline";
import { api, subscribeRevision, type FlowDetail } from "../api";

export function RunPage() {
  const { runId = "", "*": deepPath } = useParams();
  const navigate = useNavigate();
  const [overview, setOverview] = useState<RunOverview | null>(null);
  const [flowDetail, setFlowDetail] = useState<FlowDetail | null>(null);
  const [revision, setRevision] = useState(0);
  const [error, setError] = useState<string | null>(null);

  // react-router v7 already percent-decodes splat params — decoding
  // again would corrupt legitimate '%' sequences in canonical paths.
  const selectedPath = deepPath || null;

  // Revision subscription drives every refetch (invalidation-only SSE).
  useEffect(() => subscribeRevision(runId, setRevision), [runId]);

  useEffect(() => {
    api
      .overview(runId)
      .then((body) => {
        setOverview(body);
        setError(null);
      })
      .catch((err: Error) => setError(err.message));
  }, [runId, revision]);

  // The static graph of the executed irHash (version-pinned).
  useEffect(() => {
    if (!overview) return;
    api
      .flowDetail(overview.flowId, overview.irHash)
      .then(setFlowDetail)
      .catch(() => setFlowDetail(null)); // artifact absent → overlay-less page still works
  }, [overview?.flowId, overview?.irHash]);

  const select = (runPath: string) =>
    navigate(
      `/runs/${encodeURIComponent(runId)}/steps/${encodeURIComponent(runPath)}`,
    );

  const alignment = overview?.alignment;
  const alignmentSummary = useMemo(() => {
    if (!alignment) return null;
    return `align: ${alignment.reusable} reusable · ${alignment.judgeDirty} judgeDirty · ${alignment.effectDirty} effectDirty · ${alignment.new} new · ${alignment.orphaned} orphaned`;
  }, [alignment]);

  if (error) return <div className="page-pad">⚠ {error}</div>;
  if (!overview) return <div className="page-pad">loading…</div>;

  return (
    <div>
      <div className="topbar" style={{ borderTop: "1px solid var(--line)" }}>
        <b>{overview.runId}</b>
        <Link to={`/flows/${encodeURIComponent(overview.flowId)}`}>
          {overview.flowId}
        </Link>
        <code>{overview.irHash.replace("sha256:", "").slice(0, 8)}</code>
        <span>{overview.deviceId}</span>
        <span className="chip">
          lineage ×{overview.sessionLineage.length || 1}
        </span>
        <span
          className={`chip ${overview.flowVerdictStatus ? `verdict-${overview.flowVerdictStatus}` : ""}`}
        >
          {overview.flowVerdictStatus ?? overview.status}
        </span>
        {overview.supervisePolicy && (
          <span className="chip">supervise: {overview.supervisePolicy}</span>
        )}
        {alignmentSummary && <span className="chip">{alignmentSummary}</span>}
        <span className="spacer" />
        <span style={{ color: "var(--muted)" }}>rev {overview.revision}</span>
      </div>

      {overview.awaitingHuman && (
        <div className="banner human">
          ⏸ awaiting a human response —{" "}
          <Link to="/inbox">open the inbox</Link> (respond via
          pointlock-human-cli)
        </div>
      )}
      {overview.status === "suspended" && (
        <div className="banner">
          run suspended — resume with <code>pointlock resume … --run {runId}</code>
        </div>
      )}
      {overview.lastSuspensionProviderStateSummary && (
        <div className="banner" title="07 §2.2 suspension-instant provider profile">
          provider at suspension:{" "}
          <code>{overview.lastSuspensionProviderStateSummary.deviceId}</code>
          {" · health "}
          {overview.lastSuspensionProviderStateSummary.health.ok ? "ok" : "degraded"}
          {overview.lastSuspensionProviderStateSummary.eventCursor
            ? ` · cursor #${overview.lastSuspensionProviderStateSummary.eventCursor.lastSequence}`
            : " · cursor unavailable"}
          {" · sessions "}
          {overview.lastSuspensionProviderStateSummary.sessionLineage.join(" → ")}
        </div>
      )}

      <div className="run-grid">
        <div className="panel graph-pane">
          {flowDetail ? (
            <GraphView
              view={flowDetail.graph}
              overview={overview}
              onSelect={select}
              selectedPath={selectedPath}
            />
          ) : (
            <div className="page-pad" style={{ color: "var(--muted)" }}>
              graph unavailable: no artifact for{" "}
              <code>{overview.irHash.replace("sha256:", "").slice(0, 8)}</code>{" "}
              under --artifacts (the timeline and inspector still work)
            </div>
          )}
        </div>
        <div className="panel">
          <Timeline
            runId={runId}
            revision={revision}
            onSelect={select}
            selectedPath={selectedPath}
          />
        </div>
        <div className="panel">
          <Inspector
            runId={runId}
            runPath={selectedPath}
            revision={revision}
            overview={overview}
          />
        </div>
      </div>
    </div>
  );
}
