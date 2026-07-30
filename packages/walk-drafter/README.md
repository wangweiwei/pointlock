# @pointlock/walk-drafter

Walk-and-draft authoring — the exploratory sibling of `@pointlock/nl-drafter`.

One-shot drafting cannot know a site's private, hashed selectors. This package
walks the **live** pages instead:

```
snapshot the current page → the LLM emits ONE step grounded on that snapshot
→ the step is EXECUTED live (a selector that doesn't land is bounced back
with targeted feedback) → repeat → assemble a *.flow.yaml draft
```

The walker lives **outside the trust boundary**: it only authors a draft.
`pointlock compile` remains the sole enforcer, `pointlock run` the sole judge.

## Usage

```ts
import { walkFlow } from "@pointlock/walk-drafter";

const result = await walkFlow({
  intent: "打开交易页，点击币种信息，确认出现成交量",
  llm,        // ({ system, user }) => Promise<string> — any provider
  page,       // a thin adapter over a Playwright Page (see below)
  lockfile,   // parsed *.lock.json — the action menu and validation derive from it
  log: (line) => console.error(line),
});
// result.status: "drafted" | "unasserted" | "aborted" | "empty"
// result.yaml:   the draft to feed `pointlock compile`
```

The page adapter (Playwright example):

```ts
const walkPage = {
  goto: (u, o) => page.goto(u, o),
  evaluate: (expr) => page.evaluate(expr),
  locator: (sel) => page.locator(`css=${sel}`),
  // visible-only, mirroring the DeviceRail driver's clickByText contract:
  getByText: (t, o) => page.getByText(t, o).filter({ visible: true }),
  wheel: (dx, dy) => page.mouse.wheel(dx, dy),
  waitForLoadState: (s, o) => page.waitForLoadState(s, o),
};
```

## What the loop guarantees mechanically

Never left to model memory:

- **Selectors are real**: copied verbatim from an in-page snapshot whose
  selectors are verified unique and printable-ASCII (the capability surface
  rejects anything else); every emitted step is executed live before it is
  committed.
- **Schema violations bounce at draft time**: the validator runs off the
  lockfile's inputSchemas (required, additionalProperties, enum, pattern), so
  what would be an RF4007 at compile time becomes targeted feedback instead.
- **A draft always judges something**: "done" without an asserting step is
  refused; assertions may not target generic containers (`body`, `html`, …)
  nor match oversized text.
- **Drafts replay**: asserting reads get an auto-inserted `waitForSelector`
  on the same target — replay has no implicit waiting between steps.
- **Live values are asserted by shape**: a `readValueNearLabel` assertion
  requires an `assertPattern` regex (checked against the live value during the
  walk) and compiles to `regexMatch(...)`, never to a literal that rots.

## Non-goals

No browser runtime, no LLM SDK, no lockfile IO: all three are injected. The
package is pure orchestration and stays dependency-free.
