// The data plane: typed fetches against the projection host
// (`pointlock inspect --serve`, spine §10.4). The token rides every
// request — the URL is the capability (08 §1 iron law 4). This module is
// the ONLY place that talks HTTP; components consume typed results from
// @pointlock/projection-types (M3a acceptance 8: no store internals).

import type {
  HumanInboxEntry,
  RunOverview,
  StepDossierView,
  TimelinePage,
} from "@pointlock/projection-types";

const TOKEN_KEY = "pointlock.token";

/** Captures `?token=` from the startup-printed URL, then keeps it across
 * hash navigation via sessionStorage. */
export function token(): string {
  const fromUrl = new URLSearchParams(window.location.search).get("token");
  if (fromUrl) {
    sessionStorage.setItem(TOKEN_KEY, fromUrl);
    return fromUrl;
  }
  return sessionStorage.getItem(TOKEN_KEY) ?? "";
}

export function withToken(path: string): string {
  const sep = path.includes("?") ? "&" : "?";
  return `${path}${sep}token=${encodeURIComponent(token())}`;
}

async function get<T>(path: string): Promise<T> {
  const response = await fetch(withToken(path));
  if (!response.ok) {
    const body = await response.text();
    throw new Error(`GET ${path} → ${response.status}: ${body}`);
  }
  return (await response.json()) as T;
}

// ── Endpoint envelopes (serve-side compositions of the DTO families) ──

export interface VersionEntry {
  irHash: string;
  lockfileDigest: string;
  file: string;
  modifiedAtMs: number;
}

export interface RunIndexEntry {
  runId: string;
  status: string;
  createdAtMs: number;
  irHash: string;
  deviceId: string;
  flowVerdictStatus?: string;
  flowVerdictDegraded?: boolean;
}

export interface FlowIndexEntry {
  flowId: string;
  latestIrHash: string | null;
  versions: VersionEntry[];
  runs: RunIndexEntry[];
}

/** The graph family type re-exported for the adapter (typed loosely at
 * the fetch boundary; the adapter narrows). */
export type { FlowGraphView } from "@pointlock/projection-types";

export interface FlowDetail {
  projectionVersion: number;
  flowId: string;
  versions: VersionEntry[];
  graph: import("@pointlock/projection-types").FlowGraphView;
  runs: RunIndexEntry[];
}

export const api = {
  flows: () =>
    get<{
      projectionVersion: number;
      /** The host's --lockfile digest; absent = no staleness judgment. */
      currentLockfileDigest?: string | null;
      flows: FlowIndexEntry[];
    }>("/api/flows"),
  flowDetail: (flowId: string, irHash?: string) =>
    get<FlowDetail>(
      `/api/flows/${encodeURIComponent(flowId)}${irHash ? `?irHash=${encodeURIComponent(irHash)}` : ""}`,
    ),
  runs: (flowId?: string) =>
    get<{ projectionVersion: number; runs: RunIndexEntry[] }>(
      `/api/runs${flowId ? `?flowId=${encodeURIComponent(flowId)}` : ""}`,
    ),
  overview: (runId: string) =>
    get<RunOverview>(`/api/runs/${encodeURIComponent(runId)}`),
  timeline: (runId: string, filter: string, page: number, pageSize = 50) =>
    get<TimelinePage>(
      `/api/runs/${encodeURIComponent(runId)}/timeline?filter=${filter}&page=${page}&pageSize=${pageSize}`,
    ),
  dossier: (runId: string, runPath: string) =>
    get<StepDossierView>(
      `/api/runs/${encodeURIComponent(runId)}/steps/${encodeURIComponent(runPath)}`,
    ),
  inbox: () =>
    get<{ projectionVersion: number; inbox: HumanInboxEntry[] }>("/api/inbox"),
};

async function post<T>(path: string, body: unknown): Promise<T> {
  const response = await fetch(withToken(path), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const text = await response.text();
    throw new Error(`POST ${path} → ${response.status}: ${text}`);
  }
  return (await response.json()) as T;
}

// ── The repair closure (08 §2.7): CLI-equivalent write actions ──

export interface CompileResult {
  ok: boolean;
  artifact?: string;
  irHash?: string | null;
  diagnostics?: {
    code: string;
    severity: string;
    message: string;
    span?: { line: number; col: number };
  }[];
}

export interface AlignPreviewResult {
  projectionVersion: number;
  runId: string;
  report: {
    entries: { stepId: string; class: string; reason?: string }[];
    resumePoint?: unknown;
    requiresConfirmation: {
      runPath: unknown;
      /** Absent on pre-incorporation ledgers (07 §5.4 step 2 names ids). */
      stepId?: string;
      cause: string;
      reason: string;
    }[];
  };
  /** Canonical string of the computed resume point (§2.7 display item). */
  resumePointRendered?: string | null;
}

export interface ResumeResult {
  exitCode: number | null;
  stdout: string;
  stderr: string;
}

export const repair = {
  compile: (body: {
    flowPath: string;
    lockfilePath?: string;
    provider?: string;
    outPath?: string;
  }) => post<CompileResult>("/api/repair/compile", body),
  alignPreview: (body: {
    runId: string;
    flowIrPath: string;
    lockfilePath?: string;
    /** Steps forced back to execution (07 §5.3) — the preview classifies
     * them dirty exactly as the real resume will. */
    forceReexecute?: string[];
  }) => post<AlignPreviewResult>("/api/repair/align-preview", body),
  resume: (body: {
    runId: string;
    flowIrPath: string;
    lockfilePath?: string;
    provider?: string;
    /**
     * Step ids released through the 07 §5.4 gate, named ONE BY ONE — the
     * authorization vocabulary has no wildcard, here or in the CLI, and
     * it does not carry over to the next resume.
     */
    allowMutatingReexec?: string[];
    /** Steps forced back to execution (07 §5.3), classification only. */
    forceReexecute?: string[];
  }) => post<ResumeResult>("/api/repair/resume", body),
};

/** Evidence bytes URL for `<img>`/download (gallery route, 08 §4.3). */
export function evidenceUrl(sha256: string): string {
  return withToken(`/evidence/${sha256}`);
}

/**
 * Revision invalidation subscription (08 §5): SSE first, 2s polling as
 * the fully-equivalent fallback. Calls `onRevision` whenever the run's
 * revision moves; returns an unsubscribe.
 */
export function subscribeRevision(
  runId: string | null,
  onRevision: (revision: number) => void,
): () => void {
  const streamPath =
    runId === null
      ? "/api/inbox/stream"
      : `/api/runs/${encodeURIComponent(runId)}/stream`;
  let closed = false;
  let pollTimer: number | undefined;
  let source: EventSource | undefined;

  const startPolling = () => {
    if (closed || pollTimer !== undefined) return;
    const revisionPath =
      runId === null
        ? "/api/inbox/revision"
        : `/api/runs/${encodeURIComponent(runId)}/revision`;
    const poll = async () => {
      if (closed) return;
      try {
        const body = await get<{ revision: number }>(revisionPath);
        onRevision(body.revision);
      } catch {
        // Host away — keep trying; the console is a local tool.
      }
    };
    pollTimer = window.setInterval(poll, 2000);
    void poll();
  };

  try {
    source = new EventSource(withToken(streamPath));
    source.onmessage = (event) => {
      try {
        const { revision } = JSON.parse(event.data) as { revision: number };
        onRevision(revision);
      } catch {
        // Malformed tick — ignore; the next one re-syncs.
      }
    };
    source.onerror = () => {
      // SSE unavailable/dropped: polling is exactly equivalent (08 §5).
      source?.close();
      startPolling();
    };
  } catch {
    startPolling();
  }

  return () => {
    closed = true;
    source?.close();
    if (pollTimer !== undefined) window.clearInterval(pollTimer);
  };
}
