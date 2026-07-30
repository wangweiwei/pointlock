// The step inspector (08 §2.5): the dossier rendered block by block in
// pipeline order. This IS `pointlock locate` graphically — same query
// layer, same shape, byte-identical JSON. The repair block arrives with
// the repair wave (M3a-W3b); until then it shows the CLI equivalents.

import { useEffect, useState } from "react";
import type { RunOverview, StepDossierView } from "@pointlock/projection-types";
import { api, evidenceUrl } from "../api";
import { attemptDurationMs, isDegradedVerify } from "../adapter/dossier";
import { resolveInstances, stripIterations } from "../adapter/graph";
import { RepairPanel } from "./RepairPanel";

function VerdictChip(props: { status?: string; degraded?: boolean }) {
  if (!props.status) return <span className="chip">none</span>;
  return (
    <span
      className={`chip verdict-${props.status}${props.degraded ? " degraded" : ""}`}
    >
      {props.status}
      {props.degraded ? " [degraded]" : ""}
    </span>
  );
}

function Json(props: { value: unknown }) {
  return <pre>{JSON.stringify(props.value, null, 2)}</pre>;
}

export function Inspector(props: {
  runId: string;
  runPath: string | null;
  revision: number;
  overview: RunOverview | null;
}) {
  const [dossier, setDossier] = useState<StepDossierView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [instance, setInstance] = useState<string | null>(null);

  // §3.2 实例展开: a static anchor selection resolves to its runtime
  // instances; several → the selector below picks one (default first).
  const instances = props.runPath
    ? resolveInstances(props.overview, props.runPath)
    : [];
  const effectivePath =
    instance && instances.includes(instance)
      ? instance
      : (instances[0] ?? props.runPath);

  useEffect(() => {
    setInstance(null);
  }, [props.runPath]);

  useEffect(() => {
    if (!effectivePath) {
      setDossier(null);
      return;
    }
    let alive = true;
    api
      .dossier(props.runId, effectivePath)
      .then((result) => {
        if (!alive) return;
        setDossier(result);
        setError(null);
      })
      .catch((err: Error) => alive && setError(err.message));
    return () => {
      alive = false;
    };
  }, [props.runId, effectivePath, props.revision]);

  if (!props.runPath) {
    return <div className="insp">Select a node or timeline entry.</div>;
  }
  if (props.runPath && instances.length === 0 && props.overview) {
    return (
      <div className="insp">
        <code>{props.runPath}</code>
        <div className="kind">not executed in this run (no instance entered)</div>
      </div>
    );
  }
  if (error) return <div className="insp">⚠ {error}</div>;
  if (!dossier) return <div className="insp">loading…</div>;

  const instanceSelector =
    instances.length > 1 ? (
      <div style={{ marginBottom: 6 }}>
        {instances.map((path) => (
          <button
            key={path}
            className={`chip${path === effectivePath ? " verdict-pass" : ""}`}
            style={{ cursor: "pointer" }}
            onClick={() => setInstance(path)}
            title={path}
          >
            {path.slice(stripIterations(path) === path ? 0 : path.indexOf("["))}
          </button>
        ))}
      </div>
    ) : null;

  return (
    <div className="insp">
      {instanceSelector}
      <h4>identity</h4>
      <div>
        <b>{dossier.stepId}</b>{" "}
        <code className="path" title="copyable — the pointlock locate argument">
          {dossier.runPath}
        </code>
      </div>
      <div>
        effect <code>{dossier.effectHash.slice(0, 15)}…</code> · judge{" "}
        <code>{dossier.judgeHash.slice(0, 15)}…</code>
        {dossier.state && <span className="chip">{String(dossier.state)}</span>}
      </div>
      {dossier.source ? (
        <div>
          source: <code>{dossier.source.entry.file}</code> @{" "}
          {dossier.source.entry.span.startLine}:
          {dossier.source.entry.span.startCol}
          {dossier.source.entry.origin?.map((frame, index) => (
            <div key={index} className="kind">
              expanded by macro <code>{frame.macro}</code> at {frame.file}:
              {frame.span.startLine}
            </div>
          ))}
        </div>
      ) : (
        <div className="kind">
          source: pass --artifacts to the host for YAML spans
        </div>
      )}

      <h4>inputs</h4>
      <Json value={dossier.resolvedInputs} />

      {dossier.preflight.length > 0 && (
        <>
          <h4>preflight</h4>
          {dossier.preflight.map((outcome, index) => (
            <div key={index}>
              <code>{outcome.assertId}</code>{" "}
              <VerdictChip status={outcome.result} /> {outcome.reason}
            </div>
          ))}
        </>
      )}

      <h4>attempts ({dossier.attempts.length})</h4>
      {dossier.attempts.map((attempt) => (
        <div key={attempt.callId}>
          #{attempt.n ?? "?"} <code>{attempt.callId}</code>{" "}
          {attempt.actionName && (
            <>
              <code>{attempt.actionName}</code>
              {attempt.channel ? <span className="chip">@{attempt.channel}</span> : null}{" "}
            </>
          )}
          {typeof attempt.chainIndex === "number" && (
            <span className="chip" title="binding.attempts position">
              chain #{attempt.chainIndex}
            </span>
          )}
          <span className="chip">{attempt.outcome ?? "unsettled"}</span>
          {attemptDurationMs(attempt) !== undefined && (
            <span className="kind"> {attemptDurationMs(attempt)}ms</span>
          )}
          {attempt.executionMode && (
            <span
              className={`chip${attempt.executionMode === "coordinateFallback" ? " verdict-unknown" : ""}`}
            >
              {attempt.executionMode}
              {attempt.fallbackReason ? `: ${attempt.fallbackReason}` : ""}
            </span>
          )}
          {attempt.errorClass && (
            <span className="chip verdict-fail">{attempt.errorClass}</span>
          )}
          {attempt.error && (
            <div className="kind">
              {attempt.error.code}: {attempt.error.message}
              {attempt.error.retryable ? " (retryable)" : ""}
            </div>
          )}
        </div>
      ))}

      {dossier.output !== undefined && dossier.output !== null && (
        <>
          <h4>output</h4>
          <Json value={dossier.output} />
        </>
      )}

      {dossier.observations.length > 0 && (
        <>
          <h4>observations ({dossier.observations.length})</h4>
          {dossier.observations.map((observation, index) => (
            <div key={index}>
              <code>{observation.observationId}</code> @{" "}
              {observation.capturedAtMs}
              {observation.viewport && (
                <span className="chip">
                  {observation.viewport.width}×{observation.viewport.height} @
                  {observation.viewport.scaleFactor}x
                </span>
              )}
              {observation.screenshotOmission && (
                <span className="chip verdict-unknown">
                  screenshot omitted: {observation.screenshotOmission} → unknown
                  source
                </span>
              )}
              {observation.uiSnapshotOmission && (
                <span className="chip verdict-unknown">
                  uiTree omitted: {observation.uiSnapshotOmission}
                </span>
              )}
            </div>
          ))}
        </>
      )}

      <h4>assertions</h4>
      {dossier.assertionOutcomes.length === 0 && (
        <div className="kind">none</div>
      )}
      {dossier.assertionOutcomes.map((outcome, index) => (
        <div key={index}>
          <code>{outcome.assertId}</code>{" "}
          <VerdictChip status={outcome.result} />
          {outcome.channel && <span className="chip">@{outcome.channel}</span>}
          {isDegradedVerify(dossier.irNode, outcome) && (
            <span
              className="chip verdict-unknown"
              title="answered by a non-preferred verify channel"
            >
              degradedVerify
            </span>
          )}{" "}
          {outcome.reason}
        </div>
      ))}

      <h4>verdict</h4>
      {dossier.verdict ? (
        <div>
          <VerdictChip
            status={dossier.verdict.status}
            degraded={dossier.verdict.degraded}
          />
          {dossier.verdictHistory.length > 1 && (
            <div className="lineage">
              {dossier.verdictHistory
                .slice(0, -1)
                .reverse()
                .map((record) => (
                  <div key={record.seq} className="kind superseded">
                    superseded @ seq {record.seq}:{" "}
                    <VerdictChip
                      status={record.verdict.status}
                      degraded={record.verdict.degraded}
                    />{" "}
                    {record.verdict.summary}
                  </div>
                ))}
            </div>
          )}
        </div>
      ) : dossier.state === "judged" && dossier.attempts.length > 0 ? (
        <div className="kind">
          unverified — executed, no assertions (this is not a verdict)
        </div>
      ) : (
        <div className="kind">
          none{dossier.state ? ` (state: ${String(dossier.state)})` : ""}
        </div>
      )}

      {dossier.handlerTriggers.length > 0 && (
        <>
          <h4>handlers</h4>
          {dossier.handlerTriggers.map((trigger, index) => (
            <div key={index}>
              {trigger.hook}
              {trigger.disposition ? `→${trigger.disposition}` : ""} #
              {trigger.trigger} @ seq {trigger.seq}
            </div>
          ))}
        </>
      )}

      {dossier.evidence.length > 0 && (
        <>
          <h4>evidence ({dossier.evidence.length})</h4>
          <div className="gallery">
            {dossier.evidence.map((item) =>
              item.asset.mediaType.startsWith("image/") ? (
                <img
                  key={item.sha256}
                  src={evidenceUrl(item.sha256)}
                  alt={item.sha256.slice(0, 8)}
                  title={`${item.asset.mediaType} ${item.sha256} — ${item.source}`}
                />
              ) : (
                <a
                  key={item.sha256}
                  className="chip"
                  href={evidenceUrl(item.sha256)}
                  target="_blank"
                  rel="noreferrer"
                  title={item.source}
                >
                  ⬇ {item.sha256.slice(0, 8)} ({item.asset.mediaType} ·{" "}
                  {item.source})
                </a>
              ),
            )}
          </div>
        </>
      )}

      {dossier.providerStateSummary && (
        <>
          <h4>provider state at capture</h4>
          <div>
            <code>{dossier.providerStateSummary.deviceId}</code>
            {dossier.providerStateSummary.platform && (
              <span className="chip">{dossier.providerStateSummary.platform}</span>
            )}
            <span
              className={`chip${dossier.providerStateSummary.health.ok ? " verdict-pass" : " verdict-unknown"}`}
            >
              health {dossier.providerStateSummary.health.ok ? "ok" : "degraded"}
            </span>
            {dossier.providerStateSummary.eventCursor && (
              <span className="chip">
                cursor #{dossier.providerStateSummary.eventCursor.lastSequence}
              </span>
            )}
          </div>
          <div className="kind">
            sessions: {dossier.providerStateSummary.sessionLineage.join(" → ")}
            {" · attested "}
            {dossier.providerStateSummary.attestation.attestedAt}
          </div>
        </>
      )}

      <h4>repair</h4>
      <RepairPanel
        key={`${props.runId}:${dossier.runPath}`}
        runId={props.runId}
        dossier={dossier}
        lockfileDigest={props.overview?.lockfileDigest}
      />
    </div>
  );
}
