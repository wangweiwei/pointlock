/**
 * Reply-protocol parsing. The LLM must answer with exactly one fenced
 * block: a ```yaml draft, or a ```json object `{ "elicitation": [...] }`.
 * Anything else is a protocol violation the loop feeds back for repair —
 * never something to silently "make work".
 */

import {
  ELICITATION_CATEGORIES,
  type ElicitationCategory,
  type ElicitationQuestion,
} from "./types.ts";

/** Parsed reply: one draft, one question batch, or a violation. */
export type ParsedReply =
  | { kind: "draft"; yaml: string }
  | { kind: "questions"; questions: ElicitationQuestion[] }
  | { kind: "invalid"; reason: string };

const FENCE = /```([A-Za-z0-9_-]*)\r?\n([\s\S]*?)```/g;

/** Extracts all fenced blocks as `[language, body]` pairs. */
function fences(reply: string): Array<{ lang: string; body: string }> {
  const out: Array<{ lang: string; body: string }> = [];
  for (const match of reply.matchAll(FENCE)) {
    out.push({ lang: (match[1] ?? "").toLowerCase(), body: match[2] ?? "" });
  }
  return out;
}

function isCategory(value: unknown): value is ElicitationCategory {
  return (
    typeof value === "string" &&
    (ELICITATION_CATEGORIES as readonly string[]).includes(value)
  );
}

/** Validates one raw question object against the 03 §4.1.1 shape. */
function toQuestion(raw: unknown, index: number): ElicitationQuestion | string {
  if (typeof raw !== "object" || raw === null) {
    return `elicitation[${index}] is not an object`;
  }
  const record = raw as Record<string, unknown>;
  if (!isCategory(record.category)) {
    return `elicitation[${index}].category must be one of ${ELICITATION_CATEGORIES.join(", ")}`;
  }
  if (typeof record.question !== "string" || record.question.trim() === "") {
    return `elicitation[${index}].question must be a non-empty string`;
  }
  if (typeof record.path !== "string" || record.path.trim() === "") {
    return `elicitation[${index}].path must be a non-empty string`;
  }
  let options: string[] | undefined;
  if (record.options !== undefined) {
    if (
      !Array.isArray(record.options) ||
      record.options.some((o) => typeof o !== "string")
    ) {
      return `elicitation[${index}].options must be an array of strings`;
    }
    options = record.options as string[];
  }
  return {
    category: record.category,
    question: record.question,
    path: record.path,
    ...(options === undefined ? {} : { options }),
  };
}

/** Parses one LLM reply under the drafting protocol. */
export function parseReply(reply: string): ParsedReply {
  const blocks = fences(reply);
  const yamlBlocks = blocks.filter((b) => b.lang === "yaml" || b.lang === "yml");
  const jsonBlocks = blocks.filter((b) => b.lang === "json");

  if (yamlBlocks.length === 0 && jsonBlocks.length === 0) {
    return {
      kind: "invalid",
      reason:
        "the reply carries neither a ```yaml draft nor a ```json elicitation block",
    };
  }
  if (yamlBlocks.length > 0 && jsonBlocks.length > 0) {
    return {
      kind: "invalid",
      reason: "the reply mixes a draft and an elicitation block; send exactly one",
    };
  }
  if (yamlBlocks.length > 1 || jsonBlocks.length > 1) {
    return {
      kind: "invalid",
      reason: "the reply carries more than one fenced block; send exactly one",
    };
  }

  if (yamlBlocks.length === 1) {
    const yaml = yamlBlocks[0].body.trim();
    if (yaml === "") {
      return { kind: "invalid", reason: "the yaml draft block is empty" };
    }
    return { kind: "draft", yaml: `${yaml}\n` };
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(jsonBlocks[0].body);
  } catch (error) {
    return {
      kind: "invalid",
      reason: `the json block does not parse: ${(error as Error).message}`,
    };
  }
  const elicitation = (parsed as Record<string, unknown> | null)?.elicitation;
  if (!Array.isArray(elicitation) || elicitation.length === 0) {
    return {
      kind: "invalid",
      reason:
        'the json block must be `{ "elicitation": [ { category, question, path, options? } ] }`',
    };
  }
  const questions: ElicitationQuestion[] = [];
  for (const [index, raw] of elicitation.entries()) {
    const question = toQuestion(raw, index);
    if (typeof question === "string") {
      return { kind: "invalid", reason: question };
    }
    questions.push(question);
  }
  return { kind: "questions", questions };
}
