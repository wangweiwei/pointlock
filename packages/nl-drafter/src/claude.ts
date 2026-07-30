/**
 * The Claude adapter: one {@link DrafterLlm} implementation over the
 * official Anthropic SDK. Kept on its own export path
 * (`@pointlock/nl-drafter/claude`) so the core loop stays dependency-free
 * and unit-testable without an API key.
 */

import Anthropic from "@anthropic-ai/sdk";

import type { DrafterLlm, LlmRequest } from "./types.ts";

/** Configuration of the Claude-backed drafter LLM. */
export interface ClaudeLlmOptions {
  /** A pre-built client; defaults to `new Anthropic()` (env credentials). */
  client?: Anthropic;
  /** Model ID; defaults to `claude-opus-4-8`. */
  model?: string;
  /** Output budget; drafts can be long, so the default is generous. */
  maxTokens?: number;
}

/** Builds the Claude-backed {@link DrafterLlm}. */
export function claudeLlm(options: ClaudeLlmOptions = {}): DrafterLlm {
  const client = options.client ?? new Anthropic();
  const model = options.model ?? "claude-opus-4-8";
  const maxTokens = options.maxTokens ?? 64000;

  return async (request: LlmRequest): Promise<string> => {
    // Streaming keeps long drafts clear of HTTP timeouts; adaptive
    // thinking lets the model plan chains/assertions before writing.
    const stream = client.messages.stream({
      model,
      max_tokens: maxTokens,
      thinking: { type: "adaptive" },
      system: request.system,
      messages: [{ role: "user", content: request.user }],
    });
    const message = await stream.finalMessage();
    return message.content
      .filter((block): block is Anthropic.TextBlock => block.type === "text")
      .map((block) => block.text)
      .join("");
  };
}
