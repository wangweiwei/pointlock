// The repair closure (08 §2.7): fail → open YAML → recompile → align
// preview → resume, with two disciplines pinned by the doc: the preview
// is READ-ONLY and not a promise (the real alignmentReport is born at
// resume time), and the UI never edits YAML — the edit happens in the
// user's editor via a deep link.
//
// R13 proposal display: a drafter-produced patch diff renders read-only
// here; there is deliberately NO approval control — approval runs
// through pointlock-human-cli / CLI equivalents (06 §4.2, M3a
// acceptance 7's negative assertion).

import { useState } from "react";
import type { StepDossierView } from "@pointlock/projection-types";
import {
  repair,
  type AlignPreviewResult,
  type CompileResult,
  type ResumeResult,
} from "../api";

const LOCKFILE_KEY = "pointlock.repair.lockfile";
const PROVIDER_KEY = "pointlock.repair.provider";
const EDITOR_KEY = "pointlock.editorTemplate";
const DEFAULT_EDITOR = "vscode://file/{file}:{line}:{col}";

/** §2.7 explanatory copy, verbatim semantics per class. */
const CLASS_LEGEND: Record<string, string> = {
  reusable: "history kept as-is",
  judgeDirty: "re-judged offline — no device redispatch",
  effectDirty: "this step and its downstream re-run",
  new: "no adoptable prior record",
  orphaned: "history lost its IR node",
};

const CLASS_STYLE: Record<string, string> = {
  reusable: "verdict-pass",
  judgeDirty: "align-judge",
  effectDirty: "align-effect",
  new: "",
  orphaned: "verdict-fail",
};

function DiffView(props: { diff: string }) {
  return (
    <pre className="diff">
      {props.diff.split("\n").map((line, index) => (
        <div
          key={index}
          className={
            line.startsWith("+") && !line.startsWith("+++")
              ? "diff-add"
              : line.startsWith("-") && !line.startsWith("---")
                ? "diff-del"
                : undefined
          }
        >
          {line}
        </div>
      ))}
    </pre>
  );
}

export function RepairPanel(props: {
  runId: string;
  dossier: StepDossierView;
  lockfileDigest?: string;
}) {
  const source = props.dossier.source?.entry;
  const [flowPath, setFlowPath] = useState(source?.file ?? "");
  const [editorTemplate, setEditorTemplate] = useState(
    localStorage.getItem(EDITOR_KEY) ?? DEFAULT_EDITOR,
  );
  const [lockfile, setLockfile] = useState(
    localStorage.getItem(LOCKFILE_KEY) ?? "",
  );
  const [provider, setProvider] = useState(
    localStorage.getItem(PROVIDER_KEY) ?? "",
  );
  const [compileResult, setCompileResult] = useState<CompileResult | null>(null);
  const [preview, setPreview] = useState<AlignPreviewResult | null>(null);
  const [resumeResult, setResumeResult] = useState<ResumeResult | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [proposal, setProposal] = useState("");
  /**
   * Step ids the operator has explicitly released through the 07 §5.4 gate.
   * One tick per gated step: the authorization vocabulary has no wildcard,
   * so there is deliberately no "authorize all" control. Cleared whenever a
   * new preview arrives — a confirmation is scoped to the report it was
   * given against, exactly as it is scoped to one resume in the CLI.
   */
  const [allowed, setAllowed] = useState<string[]>([]);

  const editorHref = source
    ? editorTemplate
        .replace("{file}", source.file)
        .replace("{line}", String(source.span.startLine))
        .replace("{col}", String(source.span.startCol))
    : null;

  const recompile = async () => {
    setBusy("compiling…");
    setError(null);
    setPreview(null);
    setResumeResult(null);
    try {
      localStorage.setItem(LOCKFILE_KEY, lockfile);
      localStorage.setItem(PROVIDER_KEY, provider);
      const compiled = await repair.compile({
        flowPath,
        lockfilePath: lockfile || undefined,
        provider: provider || undefined,
      });
      setCompileResult(compiled);
      if (compiled.ok && compiled.artifact) {
        setBusy("previewing alignment…");
        setPreview(
          await repair.alignPreview({
            runId: props.runId,
            flowIrPath: compiled.artifact,
            lockfilePath: lockfile || undefined,
          }),
        );
        setAllowed([]);
      }
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(null);
    }
  };

  const resume = async () => {
    if (!compileResult?.artifact) return;
    setBusy("resuming…");
    setError(null);
    try {
      setResumeResult(
        await repair.resume({
          runId: props.runId,
          flowIrPath: compileResult.artifact,
          lockfilePath: lockfile || undefined,
          provider: provider || undefined,
          allowMutatingReexec: allowed.length ? allowed : undefined,
        }),
      );
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="repair">
      <div>
        ①{" "}
        {editorHref ? (
          <a href={editorHref}>open in YAML</a>
        ) : (
          <span className="kind">
            open in YAML (needs --artifacts for the source span)
          </span>
        )}
        {source && (
          <span className="kind">
            {" "}
            {source.file}:{source.span.startLine}
          </span>
        )}
      </div>

      <div style={{ margin: "6px 0" }}>
        ② edit in your editor, then{" "}
        <button onClick={recompile} disabled={busy !== null || !flowPath}>
          recompile & align preview
        </button>
        <div className="repair-form">
          <input
            value={flowPath}
            onChange={(event) => setFlowPath(event.target.value)}
            placeholder="*.flow.yaml path"
          />
          <input
            value={lockfile}
            onChange={(event) => setLockfile(event.target.value)}
            placeholder="lockfile path — use the run's own lockfile"
          />
          {props.lockfileDigest && (
            <span className="kind">
              the run bound lockfileDigest{" "}
              <code>
                {props.lockfileDigest.replace("sha256:", "").slice(0, 8)}
              </code>{" "}
              — recompile against the SAME lockfile (§2.7)
            </span>
          )}
          <input
            value={editorTemplate}
            onChange={(event) => {
              setEditorTemplate(event.target.value);
              localStorage.setItem(EDITOR_KEY, event.target.value);
            }}
            placeholder="editor link template: vscode://file/{file}:{line}:{col}"
          />
          <input
            value={provider}
            onChange={(event) => setProvider(event.target.value)}
            placeholder="provider (default: fake)"
          />
        </div>
      </div>

      {busy && <div className="kind">{busy}</div>}
      {error && <div style={{ color: "var(--fail)" }}>⚠ {error}</div>}

      {compileResult && !compileResult.ok && (
        <div>
          <b>compile rejected:</b>
          {compileResult.diagnostics?.map((diagnostic, index) => (
            <div key={index} style={{ color: "var(--fail)" }}>
              {diagnostic.code}
              {diagnostic.span ? ` @ ${diagnostic.span.line}:${diagnostic.span.col}` : ""}
              : {diagnostic.message}
            </div>
          ))}
        </div>
      )}

      {preview && (
        <div>
          <b>alignment preview</b>{" "}
          <span className="kind">
            (read-only rehearsal — the committed report is born at resume;
            preflight probes stay the final judge)
          </span>
          <div style={{ margin: "4px 0" }}>
            {preview.report.entries.map((entry) => (
              <div key={entry.stepId}>
                <span className={`chip ${CLASS_STYLE[entry.class] ?? ""}`}>
                  {entry.class}
                </span>{" "}
                {entry.stepId}
                {entry.reason && <span className="kind"> — {entry.reason}</span>}
              </div>
            ))}
          </div>
          {preview.resumePointRendered && (
            <div>
              resume point: <code>{preview.resumePointRendered}</code>
            </div>
          )}
          <div className="kind">
            {preview.report.entries
              .map((entry) => entry.class)
              .filter((value, index, all) => all.indexOf(value) === index)
              .map((cls) => `${cls}: ${CLASS_LEGEND[cls] ?? ""}`)
              .join(" · ")}
          </div>
          {preview.report.requiresConfirmation.length > 0 && (
            <div style={{ color: "var(--unknown)" }}>
              requires confirmation:
              {preview.report.requiresConfirmation.map((item, index) => {
                const stepId = item.stepId;
                return (
                  <div key={stepId ?? index} style={{ marginTop: 2 }}>
                    <label>
                      <input
                        type="checkbox"
                        disabled={!stepId || busy !== null}
                        checked={!!stepId && allowed.includes(stepId)}
                        onChange={(event) => {
                          if (!stepId) return;
                          setAllowed((prev) =>
                            event.target.checked
                              ? [...prev, stepId]
                              : prev.filter((id) => id !== stepId),
                          );
                        }}
                      />{" "}
                      <code>{stepId ?? "(id not in this report)"}</code>{" "}
                      {item.cause}
                    </label>
                    <div className="kind">{item.reason}</div>
                  </div>
                );
              })}
              <div className="kind">
                each gate is released by name — there is no wildcard, and the
                release covers this resume only. The step still probes its
                preflight when it runs.
              </div>
            </div>
          )}
          <div style={{ marginTop: 4 }}>
            ③{" "}
            <button
              onClick={resume}
              disabled={
                busy !== null ||
                preview.report.requiresConfirmation.some(
                  (item) => !item.stepId || !allowed.includes(item.stepId),
                )
              }
            >
              resume from checkpoint
            </button>
          </div>
        </div>
      )}

      {resumeResult && (
        <div>
          <b>resume exit {resumeResult.exitCode ?? "?"}</b>{" "}
          <span className="kind">
            (0 pass · 1 fail · 2 unknown · 3 suspended/blocked)
          </span>
          <pre>{resumeResult.stdout.trim().split("\n").slice(-8).join("\n")}</pre>
        </div>
      )}

      <div style={{ marginTop: 8 }}>
        <b>LLM repair proposal</b>{" "}
        <span className="kind">
          (display only — approval never happens in this UI; it runs through
          pointlock-human-cli / CLI, 06 §4.2)
        </span>
        <textarea
          value={proposal}
          onChange={(event) => setProposal(event.target.value)}
          placeholder="paste a drafter-produced unified diff to review it here"
          rows={3}
          style={{ width: "100%" }}
        />
        {proposal && <DiffView diff={proposal} />}
      </div>
    </div>
  );
}
