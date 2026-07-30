/**
 * Shared types of the drafting loop (03 §4.1 / §4.1.1).
 *
 * The drafter lives outside the trust boundary: everything here is
 * advisory material for a human-reviewed YAML draft. The compiler is the
 * sole enforcer; nothing in this package ever touches the IR surface.
 */

/**
 * The four mandatory elicitation categories (03 §4.1.1). The drafter must
 * ask rather than invent when any of these gaps exists in the intent.
 */
export type ElicitationCategory =
  | "missingRequiredParam"
  | "ambiguousSelector"
  | "fallbackAuthorization"
  | "secretStrategy";

/** The closed category vocabulary, for validation. */
export const ELICITATION_CATEGORIES: readonly ElicitationCategory[] = [
  "missingRequiredParam",
  "ambiguousSelector",
  "fallbackAuthorization",
  "secretStrategy",
];

/**
 * One structured question (03 §4.1.1: `question` states the gap,
 * `options` are candidate answers, `path` points at the undecided spot in
 * the draft, JSON-pointer style, e.g. `steps/login/locate_via`).
 */
export interface ElicitationQuestion {
  category: ElicitationCategory;
  question: string;
  path: string;
  options?: string[];
}

/** The author's answer to a previously emitted question. */
export interface ElicitationAnswer {
  path: string;
  answer: string;
  category?: ElicitationCategory;
}

/** One compile diagnostic, as `pointlock compile --format json` emits it. */
export interface CompileDiagnostic {
  code: string;
  severity: "error" | "warning";
  message: string;
  span?: {
    line: number;
    col: number;
    endLine: number;
    endCol: number;
  };
}

/** Outcome of one compile attempt over a draft. */
export type CompileOutcome =
  | { ok: true; irJson: unknown; warnings: CompileDiagnostic[] }
  | { ok: false; diagnostics: CompileDiagnostic[] };

/**
 * Runs `pointlock compile` (or a stand-in) over one YAML draft. Rejects
 * only on tool failure (missing binary, unparseable output) — an
 * authoring rejection is the `ok: false` outcome, not an error.
 */
export type CompileRunner = (yaml: string) => Promise<CompileOutcome>;

/** One request to the injected LLM. */
export interface LlmRequest {
  system: string;
  user: string;
}

/**
 * The injected LLM function. The core loop only ever sees this shape, so
 * unit tests run on stubs and the Claude adapter is one implementation
 * (`@pointlock/nl-drafter/claude`).
 */
export type DrafterLlm = (request: LlmRequest) => Promise<string>;

/** Optional authoring context woven into the prompt (03 §4.1 inputs). */
export interface DraftContext {
  /** Authoring JSON Schema (closed keyword surface). */
  authoringSchema?: string | object;
  /** Lockfile vocabulary hints: available actions, features, platform. */
  lockfileHints?: string | object;
  /** Optional uiSnapshot material for grounding selectors. */
  uiSnapshot?: string | object;
}

/** Inputs of one {@link draftFlow} run. */
export interface DraftOptions {
  /** The natural-language intent. */
  intent: string;
  /** The injected LLM. */
  llm: DrafterLlm;
  /** The compile gate. */
  compile: CompileRunner;
  /** Optional authoring context. */
  context?: DraftContext;
  /** Answers to previously emitted questions, woven back for re-draft. */
  answers?: ElicitationAnswer[];
  /** LLM call budget per run (default 4). */
  maxAttempts?: number;
}

/** Result of one drafting run. */
export type DraftResult =
  | {
      /** The draft passed `pointlock compile`. */
      status: "compiled";
      yaml: string;
      irJson: unknown;
      warnings: CompileDiagnostic[];
      attempts: number;
    }
  | {
      /** The intent has gaps; the author must answer before re-drafting. */
      status: "needsInput";
      questions: ElicitationQuestion[];
      /** The draft so far, when the gap was found on a produced draft. */
      yaml?: string;
      attempts: number;
    }
  | {
      /** The attempt budget ran out with diagnostics still open. */
      status: "exhausted";
      yaml: string;
      diagnostics: CompileDiagnostic[];
      attempts: number;
    };
