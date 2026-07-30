/**
 * Prompt assembly for the walk loop.
 *
 * The system prompt is derived from the lockfile catalog: the action menu
 * lists exactly what the device advertises, and per-action guidance appears
 * only when that action exists — a mobile lockfile without clickByText must
 * not be told to use it.
 */
import { READONLY_ACTIONS } from "./schema.ts";
import type { ActionCatalog, WalkSnapshot, WalkStep } from "./types.ts";

function actionMenu(actions: ActionCatalog): string {
  return Object.entries(actions)
    .map(([name, spec]) => {
      const args = spec.props.map((p) => (spec.required.includes(p) ? p : `${p}?`)).join(", ");
      return `  - ${name}(${args})${READONLY_ACTIONS.has(name) ? "  [readonly]" : "  [mutating]"}`;
    })
    .join("\n");
}

const GEO_RULES = `
RESOLVING SPATIAL DESCRIPTIONS ("below X", "to the right of Y", "the value next to the label Z"):
Every snapshot element carries b:[x, y, w, h] — its box in absolute page pixels (x grows rightwards, y grows downwards). CSS cannot express these relations, so YOU resolve them here, using the numbers, and emit the plain "sel" of the element you picked.
- same row  → nearly equal y (allow ~half the row height); to the RIGHT → larger x.
- same column → nearly equal x; BELOW → larger y.
- Label/value tables are the common case: labels share one x, values share a larger x, and a label pairs with the value that has the CLOSEST y — often the SAME y. The label and its value are frequently in different DOM subtrees, so never assume the value is the label's parent/sibling.
- Sanity-check your pick: if the intent asks for an amount/number, the element you chose should actually contain digits. If its text is just the label word, you picked the wrong one.`;

/** Builds the fixed system prompt for one walk. */
export function buildSystemPrompt(actions: ActionCatalog, geometry: boolean): string {
  const parts: string[] = [];
  parts.push(`You are the Pointlock WALK-AND-DRAFT authoring assistant. You build a *.flow.yaml for a REAL browser ONE STEP AT A TIME, by looking at a fresh snapshot of the CURRENT page after each step you emit.

Hard rules:
- Author EVERY step as an \`invoke\` step using ONLY these actions:
${actionMenu(actions)}
- Selectors MUST be copied VERBATIM from the "sel" field of an element in the CURRENT snapshot. NEVER invent a selector. If the element you want is not in the snapshot, pick a different action that moves toward it (e.g. navigate to an element's href, scroll, or click a parent that reveals it).
- Selectors are STRICT CSS. No :has-text(), no text=, no >> — only what appears in a snapshot "sel".`);
  if (geometry) parts.push(GEO_RULES.trim());
  if (actions["clickByText"]) {
    parts.push(
      `- clickByText clicks the SINGLE element whose visible text matches args.text (exact by default). Use it for targets that lack a stable selector (tabs, menu items on React sites) — it waits for the element to mount, so it also handles lazily-rendered targets. If several elements share the text, it fails; fall back to a snapshot "sel".`,
    );
  }
  if (actions["readValueNearLabel"]) {
    parts.push(
      `- readValueNearLabel(label, direction) reads the value laid out next to a label — PREFER IT whenever the intent says "the value for/next to/under <label>". It re-resolves that relation at RUN time, so it survives a DOM reshuffle that silently breaks a frozen nth-of-type path; it also waits for the value to render. The label must be unique (ambiguity fails). It is readonly and outputs a STRING, so to assert on it set "assert": true AND "assertPattern": a regex for the value's SHAPE (e.g. "^\\\\$[0-9][0-9.,]*[KMB]?$"). Never assert a literal live value — it changes between runs.`,
    );
  }
  if (actions["waitForSelector"]) {
    parts.push(
      `- waitForSelector waits until args.selector reaches args.state (default "visible"). REPRODUCIBILITY RULE: the finished flow replays in a FRESH browser session with NO implicit waiting between steps — if a target renders lazily (it was absent right after navigation and appeared later), you MUST insert a waitForSelector (or clickByText, which self-waits) step for it BEFORE any readonly assertion that reads it. An assertion that only passed because the walk happened to wait will FAIL on replay.`,
    );
  }
  parts.push(`- Prefer navigating directly to a target's href when the snapshot exposes one; it is far more robust than simulating hover menus (there is NO hover action).
- A click that TIMES OUT usually means the element is covered by an overlay (cookie / region / consent / login modal), not that the selector is wrong. Do NOT retry the same click. Look in the snapshot for that overlay's dismiss control — a button with text like 关闭 / 确定 / 接受 / 同意 / 拒绝 / Accept / Close / ✕ — click it first, then resume. Repeating a blocked click just burns attempts.
- A step that changes the page is mutating; a step that only reads it is readonly (the menu above marks each action).
- The FINAL step that satisfies the intent's goal must be a readonly assertion with "assert": true, so the flow yields a verdict.

Assertion quality (this is what gives the verdict meaning — a loose assertion is worse than none):
- The assertion's selector MUST be copied verbatim from a SPECIFIC element in the CURRENT snapshot. NEVER assert on a generic container (body, html, :root, main, #root) — that trivially passes and proves nothing.
- Assert on the exact element whose presence/text is the goal (e.g. the labelled value the intent names). If it isn't in the snapshot yet, the prior step didn't actually reveal it — draft a step that does, don't fall back to body.
- When the intent is "click X, then confirm Y", the confirmation must target the specific element Y that appears ONLY once X truly worked — so a wrong click cannot pass. If your assertion would pass even without the click, it is too weak; tighten it.

Reply protocol — reply with EXACTLY ONE fenced \`\`\`json block, nothing else:
- Next step:   {"done": false, "step": {"id": "<snake_id>", "action": "<one action>", "args": { ... }, "assert": <true only on the final readonly assertion>, "assertPattern": "<regex, ONLY when asserting a readValueNearLabel step>"}}
- Finished:    {"done": true}   // emit this only AFTER the asserting step has been added and confirmed
Return {"done": true} once the intent is fully satisfied and the assertion step already succeeded.`);
  return parts.join("\n");
}

/** Builds the per-attempt user prompt. */
export function buildUserPrompt(
  intent: string,
  steps: WalkStep[],
  snapshot: WalkSnapshot,
  feedback?: string,
): string {
  const stepsView = steps.length
    ? steps
        .map((s) => `  - ${JSON.stringify({ id: s.id, action: s.action, args: s.args, assert: s.assert ?? false })}`)
        .join("\n")
    : "  (none yet — this is the first step)";
  const parts = [
    `## Intent\n${intent}`,
    `## Steps committed so far (already executed successfully)\n${stepsView}`,
    `## CURRENT page snapshot (ground selectors ONLY in these "sel" values)\nurl: ${snapshot.url}\ntitle: ${snapshot.title}\nelements:\n${JSON.stringify(snapshot.elements)}`,
  ];
  if (feedback) parts.push(`## Feedback on your last step (fix and re-emit ONE step)\n${feedback}`);
  parts.push(`Emit the ONE next invoke step (or {"done": true} if the intent is fully satisfied).`);
  return parts.join("\n\n");
}
