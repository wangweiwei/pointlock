/**
 * The drafting loop (03 §4.0 stage 0): draft → guard → compile →
 * diagnostic feedback, until the draft compiles, the author must answer,
 * or the attempt budget runs out. Two loops share the same machinery
 * (03 §4.1.1): the diagnostic loop burns compile errors, the elicitation
 * loop burns intent gaps.
 */

import { checkFallbackAuthorization, questionsOf } from "./guard.ts";
import { parseReply } from "./parse.ts";
import { buildSystemPrompt, buildUserPrompt } from "./prompt.ts";
import type {
  CompileDiagnostic,
  DraftOptions,
  DraftResult,
  ElicitationQuestion,
} from "./types.ts";

const DEFAULT_MAX_ATTEMPTS = 4;

/** Drops questions the caller has already answered (path + category). */
function unansweredOnly(
  questions: ElicitationQuestion[],
  options: DraftOptions,
): ElicitationQuestion[] {
  const answers = options.answers ?? [];
  return questions.filter(
    (q) =>
      !answers.some(
        (a) => a.path === q.path && (a.category ?? q.category) === q.category,
      ),
  );
}

/** Runs one drafting session. See {@link DraftOptions}. */
export async function draftFlow(options: DraftOptions): Promise<DraftResult> {
  const maxAttempts = options.maxAttempts ?? DEFAULT_MAX_ATTEMPTS;
  const system = buildSystemPrompt();

  let previousYaml: string | undefined;
  let diagnostics: CompileDiagnostic[] | undefined;
  let feedback: string | undefined;
  let attempts = 0;

  while (attempts < maxAttempts) {
    attempts += 1;
    const user = buildUserPrompt({
      intent: options.intent,
      context: options.context,
      answers: options.answers,
      previousYaml,
      diagnostics,
      feedback,
    });
    const reply = await options.llm({ system, user });
    const parsed = parseReply(reply);
    feedback = undefined;
    diagnostics = undefined;

    if (parsed.kind === "invalid") {
      feedback = `Protocol violation: ${parsed.reason}. Resend as exactly one fenced block.`;
      continue;
    }

    if (parsed.kind === "questions") {
      const open = unansweredOnly(parsed.questions, options);
      if (open.length === 0) {
        feedback =
          "Every question you asked is already answered in the Author answers section; weave those answers in and send the draft.";
        continue;
      }
      return {
        status: "needsInput",
        questions: open,
        ...(previousYaml === undefined ? {} : { yaml: previousYaml }),
        attempts,
      };
    }

    previousYaml = parsed.yaml;

    // Guard: degraded channels need the author's signature, not the LLM's.
    const verdict = checkFallbackAuthorization(parsed.yaml, options.answers ?? []);
    if (verdict.denied.length > 0) {
      feedback = `The author DENIED these degraded channels; remove them from the chains: ${verdict.denied
        .map((f) => `${f.channel} at ${f.path}`)
        .join(", ")}.`;
      continue;
    }
    if (verdict.unanswered.length > 0) {
      return {
        status: "needsInput",
        questions: questionsOf(verdict.unanswered),
        yaml: parsed.yaml,
        attempts,
      };
    }

    const outcome = await options.compile(parsed.yaml);
    if (outcome.ok) {
      return {
        status: "compiled",
        yaml: parsed.yaml,
        irJson: outcome.irJson,
        warnings: outcome.warnings,
        attempts,
      };
    }
    diagnostics = outcome.diagnostics;
  }

  return {
    status: "exhausted",
    yaml: previousYaml ?? "",
    diagnostics: diagnostics ?? [],
    attempts,
  };
}
