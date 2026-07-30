// Dossier-side pure helpers (renderer concerns over the protocol DTO —
// same layering rule as the graph adapter, spine §10.5).

import type { StepDossierView } from "@pointlock/projection-types";

/**
 * The declared verify chain of one assertion, looked up in the dossier's
 * IR node (present when the host was given `--artifacts`). The renderer
 * holds the IR — the runtime outcome deliberately carries only the
 * answering channel (08 §3.4).
 */
export function verifyViaOf(
  irNode: unknown,
  assertId: string,
): string[] | undefined {
  if (irNode === null || typeof irNode !== "object") return undefined;
  const node = irNode as Record<string, unknown>;
  for (const key of ["assertions", "preflight"]) {
    const list = node[key];
    if (!Array.isArray(list)) continue;
    for (const entry of list) {
      if (
        entry !== null &&
        typeof entry === "object" &&
        (entry as Record<string, unknown>).assertId === assertId &&
        Array.isArray((entry as Record<string, unknown>).verifyVia)
      ) {
        return (entry as { verifyVia: string[] }).verifyVia;
      }
    }
  }
  return undefined;
}

/**
 * Whether an assertion outcome rests on a non-preferred verify channel —
 * the `degradedVerify` row marker (08 §2.5/§3.4: 非首选 → 标
 * degradedVerify). Undecidable without the declared chain (no IR node,
 * or a channel-less expr outcome) → not marked, never guessed.
 */
export function isDegradedVerify(
  irNode: unknown,
  outcome: { assertId: string; channel?: string | null },
): boolean {
  if (!outcome.channel) return false;
  const chain = verifyViaOf(irNode, outcome.assertId);
  if (!chain || chain.length === 0) return false;
  return chain[0] !== outcome.channel;
}

/** Wall-clock duration of one attempt, when both ends were recorded. */
export function attemptDurationMs(attempt: {
  startedAtMs?: number | null;
  finishedAtMs?: number | null;
}): number | undefined {
  if (
    attempt.startedAtMs === undefined ||
    attempt.startedAtMs === null ||
    attempt.finishedAtMs === undefined ||
    attempt.finishedAtMs === null
  ) {
    return undefined;
  }
  return attempt.finishedAtMs - attempt.startedAtMs;
}

/** Narrow re-export spot for the dossier type consumers. */
export type Dossier = StepDossierView;
