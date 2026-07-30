/** Reply-protocol parsing: exactly one fenced JSON block. */

/** The two legal reply shapes. */
export interface ParsedWalkReply {
  done: boolean;
  step?: unknown;
}

/**
 * Parses the model's reply. Accepts a ```json fence, a bare ``` fence, or (as
 * a last resort) the whole text as JSON. Throws on anything unparseable — the
 * caller turns that into protocol feedback.
 */
export function parseWalkReply(text: string): ParsedWalkReply {
  const match = text.match(/```json\s*([\s\S]*?)```/i) ?? text.match(/```\s*([\s\S]*?)```/i);
  const body = (match ? match[1]! : text).trim();
  const parsed: unknown = JSON.parse(body);
  const record = parsed as { done?: unknown; step?: unknown };
  return { done: record.done === true, step: record.step };
}
