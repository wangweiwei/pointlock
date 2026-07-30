/**
 * `@pointlock/nl-drafter` — the stage-0 authoring assistant (03 §4.0/§4.1).
 *
 * Core surface (no runtime dependencies): the drafting loop, the reply
 * protocol, the fallback-authorization guard, and the CLI compile gate.
 * The Claude adapter lives on `@pointlock/nl-drafter/claude`.
 */

export { draftFlow } from "./draft.ts";
export { unifiedDiff } from "./diff.ts";
export {
  buildRepairSystemPrompt,
  buildRepairUserPrompt,
  proposeRepair,
  type RepairOptions,
  type RepairPromptInput,
  type RepairResult,
} from "./repair.ts";
export { parseReply, type ParsedReply } from "./parse.ts";
export {
  checkFallbackAuthorization,
  isGrant,
  questionsOf,
  scanFallbackChains,
  type FallbackFinding,
  type GuardVerdict,
} from "./guard.ts";
export {
  buildSystemPrompt,
  buildUserPrompt,
  type UserPromptInput,
} from "./prompt.ts";
export { cliCompiler, type CliCompilerOptions } from "./compile.ts";
export {
  ELICITATION_CATEGORIES,
  type CompileDiagnostic,
  type CompileOutcome,
  type CompileRunner,
  type DraftContext,
  type DraftOptions,
  type DraftResult,
  type DrafterLlm,
  type ElicitationAnswer,
  type ElicitationCategory,
  type ElicitationQuestion,
  type LlmRequest,
} from "./types.ts";
