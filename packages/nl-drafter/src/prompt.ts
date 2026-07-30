/**
 * Prompt assembly (03 §4.1). The system prompt fixes the reply protocol
 * and the four mandatory elicitation categories; the user prompt weaves
 * intent, authoring context, recorded answers, the previous draft, and
 * compile diagnostics.
 */

import type {
  CompileDiagnostic,
  DraftContext,
  ElicitationAnswer,
} from "./types.ts";

function asText(value: string | object | undefined): string | undefined {
  if (value === undefined) {
    return undefined;
  }
  return typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

/** The fixed drafting contract. */
export function buildSystemPrompt(): string {
  return `You are the Pointlock authoring assistant. You draft *.flow.yaml files from natural-language intent. You are OUTSIDE the trust boundary: your draft is reviewed by a human and enforced by \`pointlock compile\` — you never invent capabilities, never touch IR fields, and never guess values the author did not give you.

Reply protocol — every reply is EXACTLY ONE fenced block, nothing else:
- A draft: one \`\`\`yaml block containing the complete *.flow.yaml.
- Or questions: one \`\`\`json block of the shape
  { "elicitation": [ { "category": "...", "question": "...", "path": "...", "options": ["..."] } ] }

Mandatory elicitation — when the intent has any of these four gaps you MUST ask instead of filling in (category values are closed):
1. "missingRequiredParam": a required parameter has no value source in the intent.
2. "ambiguousSelector": the described target matches several candidate elements in the provided uiSnapshot material; the selector cannot land uniquely.
3. "fallbackAuthorization": you may NOT put \`coordinate\` into \`locate_via\` nor \`vision\` into \`verify_via\` unless a recorded answer explicitly authorizes that exact chain — degradation authorization is the author's signature, never yours.
4. "secretStrategy": the intent involves passwords/tokens; ask how the author wants the secret handled (v0.1 plaintext param vs waiting for v0.2 \`secrets.*\`) — never embed a literal secret on your own.

\`path\` points at the undecided spot, JSON-pointer style over the draft (e.g. "params/password", "steps/login/locate_via").

Drafting rules:
- Use only keywords from the authoring schema and only actions/features from the lockfile hints. The compiler rejects anything else; do not try.
- Write fallback chains as inline arrays, e.g. \`locate_via: [dom, uiTree]\`.
- Steps that mutate the world carry \`effect: mutating\`; judgments go into \`expect\` assertions — never assume success without one.
- When compile diagnostics are provided, fix exactly what they point at (line/col spans reference your previous draft) and resend the full corrected draft.`;
}

/** Inputs of the per-attempt user prompt. */
export interface UserPromptInput {
  intent: string;
  context?: DraftContext;
  answers?: ElicitationAnswer[];
  previousYaml?: string;
  diagnostics?: CompileDiagnostic[];
  /** Protocol/guard feedback about the previous reply, when any. */
  feedback?: string;
}

function renderDiagnostic(d: CompileDiagnostic): string {
  const at = d.span ? ` @ ${d.span.line}:${d.span.col}` : "";
  return `- [${d.severity}] ${d.code}${at}: ${d.message}`;
}

/** Builds the user prompt for one attempt. */
export function buildUserPrompt(input: UserPromptInput): string {
  const sections: string[] = [];
  sections.push(`## Intent\n\n${input.intent.trim()}`);

  const schema = asText(input.context?.authoringSchema);
  if (schema !== undefined) {
    sections.push(`## Authoring schema (closed keyword surface)\n\n${schema}`);
  }
  const hints = asText(input.context?.lockfileHints);
  if (hints !== undefined) {
    sections.push(
      `## Capability vocabulary (what this device can actually do)\n\n${hints}`,
    );
  }
  const snapshot = asText(input.context?.uiSnapshot);
  if (snapshot !== undefined) {
    sections.push(`## uiSnapshot material (ground selectors here)\n\n${snapshot}`);
  }

  if (input.answers !== undefined && input.answers.length > 0) {
    const woven = input.answers
      .map((a) => `- ${a.path}${a.category ? ` (${a.category})` : ""}: ${a.answer}`)
      .join("\n");
    sections.push(
      `## Author answers (weave these into the draft; they are decisions, not suggestions)\n\n${woven}`,
    );
  }

  if (input.previousYaml !== undefined) {
    sections.push(
      `## Your previous draft\n\n\`\`\`yaml\n${input.previousYaml.trimEnd()}\n\`\`\``,
    );
  }
  if (input.diagnostics !== undefined && input.diagnostics.length > 0) {
    sections.push(
      `## Compile diagnostics (pointlock compile --format json)\n\n${input.diagnostics
        .map(renderDiagnostic)
        .join("\n")}\n\nFix these and resend the full draft.`,
    );
  }
  if (input.feedback !== undefined) {
    sections.push(`## Feedback on your previous reply\n\n${input.feedback}`);
  }

  return sections.join("\n\n");
}
