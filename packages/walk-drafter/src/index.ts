/**
 * `@pointlock/walk-drafter` — walk-and-draft authoring (the exploratory
 * sibling of `@pointlock/nl-drafter`).
 *
 * One-shot drafting cannot know a site's private selectors; this package
 * walks the LIVE pages instead: snapshot → the LLM emits one grounded step →
 * the step is executed to prove the selector lands → repeat, until the intent
 * is satisfied by an asserting readonly step. The result is a *.flow.yaml
 * draft; `pointlock compile` remains the sole enforcer and `pointlock run` the
 * sole judge.
 *
 * No runtime dependencies: the LLM, the browser page (a structural interface
 * a Playwright `Page` satisfies via a thin adapter), and the lockfile are all
 * injected.
 */

export { walkFlow } from "./walk.ts";
export { execStep } from "./exec.ts";
export { takeSnapshot, SNAPSHOT_SOURCE } from "./snapshot.ts";
export { buildSystemPrompt, buildUserPrompt } from "./prompt.ts";
export { parseWalkReply, type ParsedWalkReply } from "./parse.ts";
export { extractActions, validateStep, READONLY_ACTIONS } from "./schema.ts";
export { toYaml } from "./yaml.ts";
export type {
  ActionCatalog,
  ActionSpec,
  ExecResult,
  LlmRequest,
  PropertySchema,
  SnapshotElement,
  WalkLlm,
  WalkLocatorLike,
  WalkOptions,
  WalkPageLike,
  WalkResult,
  WalkSnapshot,
  WalkStatus,
  WalkStep,
} from "./types.ts";
