import assert from "node:assert/strict";
import { test } from "node:test";

import { walkFlow } from "../src/walk.ts";
import type {
  LlmRequest,
  WalkLlm,
  WalkLocatorLike,
  WalkPageLike,
  WalkSnapshot,
} from "../src/types.ts";

// ── scripted fakes ───────────────────────────────────────────────────────────

function scriptedLlm(replies: string[]): { llm: WalkLlm; requests: LlmRequest[] } {
  const requests: LlmRequest[] = [];
  const queue = [...replies];
  const llm: WalkLlm = async (request) => {
    requests.push(request);
    const next = queue.shift();
    assert.ok(next !== undefined, "LLM called more times than scripted");
    return next;
  };
  return { llm, requests };
}

const fence = (value: unknown): string => "```json\n" + JSON.stringify(value) + "\n```";

interface FakeBehavior {
  snapshot?: WalkSnapshot;
  /** innerText per selector; default "". */
  texts?: Record<string, string>;
  /** locator count per selector; default 1. */
  counts?: Record<string, number>;
  /** value the in-page readValueNearLabel mirror resolves to. */
  readNearValue?: string;
  /** selectors whose click rejects like a Playwright timeout. */
  failClicks?: string[];
}

const SNAPSHOT: WalkSnapshot = {
  url: "https://site.test/",
  title: "Site",
  elements: [
    { sel: "#headline", tag: "h1", kind: "text", text: "Example Domain" },
    { sel: "#go", tag: "button", kind: "interactive", text: "Go" },
  ],
};

function fakePage(behavior: FakeBehavior = {}): { page: WalkPageLike; calls: string[] } {
  const calls: string[] = [];
  const locator = (sel: string): WalkLocatorLike => ({
    first: () => locator(sel),
    count: async () => behavior.counts?.[sel] ?? 1,
    click: async () => {
      if (behavior.failClicks?.includes(sel)) throw new Error("Timeout 15000ms exceeded.");
      calls.push(`click:${sel}`);
    },
    fill: async (value) => {
      calls.push(`fill:${sel}:${value}`);
    },
    press: async (key) => {
      calls.push(`press:${sel}:${key}`);
    },
    selectOption: async (value) => {
      calls.push(`select:${sel}:${value}`);
    },
    waitFor: async () => {
      calls.push(`waitFor:${sel}`);
    },
    innerText: async () => behavior.texts?.[sel] ?? "",
    textContent: async () => behavior.texts?.[sel] ?? "",
  });
  const page: WalkPageLike = {
    goto: async (url) => {
      calls.push(`goto:${url}`);
    },
    evaluate: async (expression) => {
      if (expression.includes("interactiveSel")) return behavior.snapshot ?? SNAPSHOT;
      if (expression.includes("rowTol")) return behavior.readNearValue;
      throw new Error(`unexpected evaluate: ${expression.slice(0, 40)}`);
    },
    locator,
    getByText: (text) => locator(`text:${text}`),
    wheel: async () => {
      calls.push("wheel");
    },
    waitForLoadState: async () => {
      calls.push("loadState");
    },
  };
  return { page, calls };
}

// A minimal but realistic lockfile: required args, the ASCII selector
// pattern the real capability surface enforces, and the two label actions.
const LOCKFILE = {
  device: {
    actions: [
      { name: "navigate", inputSchema: { properties: { url: { type: "string" } }, required: ["url"] } },
      {
        name: "click",
        inputSchema: {
          properties: { selector: { type: "string", pattern: "^[ -~]+$" }, button: { type: "string" } },
          required: ["selector"],
        },
      },
      {
        name: "textContains",
        inputSchema: {
          properties: {
            selector: { type: "string", pattern: "^[ -~]+$" },
            text: { type: "string" },
            caseSensitive: { type: "boolean" },
          },
          required: ["selector", "text"],
        },
      },
      {
        name: "elementExists",
        inputSchema: { properties: { selector: { type: "string", pattern: "^[ -~]+$" } }, required: ["selector"] },
      },
      {
        name: "waitForSelector",
        inputSchema: {
          properties: { selector: { type: "string", pattern: "^[ -~]+$" }, state: { type: "string" } },
          required: ["selector"],
        },
      },
      {
        name: "readValueNearLabel",
        inputSchema: {
          properties: {
            label: { type: "string" },
            direction: { type: "string", enum: ["right", "below"] },
            exact: { type: "boolean" },
          },
          required: ["label"],
        },
      },
    ],
  },
};

const NAVIGATE = fence({
  done: false,
  step: { id: "open", action: "navigate", args: { url: "https://site.test/" } },
});
const ASSERT_HEADLINE = fence({
  done: false,
  step: {
    id: "check",
    action: "textContains",
    args: { selector: "#headline", text: "Example" },
    assert: true,
  },
});
const DONE = fence({ done: true });

function options(llm: WalkLlm, page: WalkPageLike) {
  return { intent: "open the site and confirm the headline", llm, page, lockfile: LOCKFILE, settleMs: 0 };
}

// ── the loop ─────────────────────────────────────────────────────────────────

test("happy path commits, auto-inserts waitForSelector before the assertion, and yields yaml", async () => {
  const { llm, requests } = scriptedLlm([NAVIGATE, ASSERT_HEADLINE, DONE]);
  const { page, calls } = fakePage({ texts: { "#headline": "Example Domain" } });
  const result = await walkFlow(options(llm, page));

  assert.equal(result.status, "drafted");
  assert.deepEqual(
    result.steps.map((s) => s.action),
    ["navigate", "waitForSelector", "textContains"],
  );
  assert.equal(result.steps[1]?.args.selector, "#headline");
  assert.ok(calls.includes("goto:https://site.test/"));
  assert.match(result.yaml, /flow: walk_drafted_flow/);
  assert.match(result.yaml, /waitForSelector/);
  assert.match(result.yaml, /expr: "\$\{\{ steps\.check\.output\.contains \}\}"/);
  // the system prompt is lockfile-derived: it lists the advertised actions
  assert.match(requests[0]!.system, /navigate\(url\)/);
  assert.match(requests[0]!.system, /readValueNearLabel/);
});

test("done without any asserting step is refused until an assertion lands", async () => {
  const { llm } = scriptedLlm([NAVIGATE, DONE, ASSERT_HEADLINE, DONE]);
  const { page } = fakePage({ texts: { "#headline": "Example Domain" } });
  const result = await walkFlow(options(llm, page));
  assert.equal(result.status, "drafted");
  assert.equal(result.steps.filter((s) => s.assert).length, 1);
});

test("schema violations bounce with targeted feedback and never execute", async () => {
  const invalidAction = fence({ done: false, step: { id: "x", action: "hover", args: {} } });
  const nonAsciiSelector = fence({
    done: false,
    step: {
      id: "y",
      action: "textContains",
      args: { selector: 'input[aria-label="查询"]', text: "x" },
      assert: true,
    },
  });
  const { llm, requests } = scriptedLlm([invalidAction, nonAsciiSelector, ASSERT_HEADLINE, DONE]);
  const { page, calls } = fakePage({ texts: { "#headline": "Example Domain" } });
  const result = await walkFlow(options(llm, page));

  assert.equal(result.status, "drafted");
  // feedback after attempt 1 names the closed action set; after attempt 2 the pattern
  assert.match(requests[1]!.user, /not one of the allowed invoke actions/);
  assert.match(requests[2]!.user, /does not match the capability schema pattern/);
  // the rejected selector was never executed against the page
  assert.equal(calls.some((c) => c.includes("查询")), false);
});

test("a rooted nth-path on an id-less page is NOT mistaken for a generic container", async () => {
  // pages without id anchors root every selector at html:… — the guard must
  // judge the targeted FINAL segment, not the path prefix
  const rootedAssert = fence({
    done: false,
    step: {
      id: "check",
      action: "textContains",
      args: { selector: "html:nth-of-type(1) > body:nth-of-type(1) > div:nth-of-type(2)", text: "Example" },
      assert: true,
    },
  });
  const { llm } = scriptedLlm([rootedAssert, DONE]);
  const { page } = fakePage({
    texts: { "html:nth-of-type(1) > body:nth-of-type(1) > div:nth-of-type(2)": "Example Domain" },
  });
  const result = await walkFlow(options(llm, page));
  assert.equal(result.status, "drafted");
  assert.equal(result.steps.filter((s) => s.assert).length, 1);
});

test("a generic assertion selector is rejected before execution", async () => {
  const bodyAssert = fence({
    done: false,
    step: { id: "weak", action: "textContains", args: { selector: "body", text: "Example" }, assert: true },
  });
  const { llm, requests } = scriptedLlm([NAVIGATE, bodyAssert, ASSERT_HEADLINE, DONE]);
  const { page } = fakePage({ texts: { "#headline": "Example Domain", body: "Example everywhere" } });
  const result = await walkFlow(options(llm, page));
  assert.equal(result.status, "drafted");
  assert.match(requests[2]!.user, /generic container — it proves nothing/);
});

test("an assertion matching an oversized container is bounced as too broad", async () => {
  const broad = fence({
    done: false,
    step: { id: "broad", action: "textContains", args: { selector: "#wall", text: "Example" }, assert: true },
  });
  const { llm, requests } = scriptedLlm([broad, ASSERT_HEADLINE, DONE]);
  const { page } = fakePage({
    texts: { "#wall": "Example " + "x".repeat(300), "#headline": "Example Domain" },
  });
  const result = await walkFlow(options(llm, page));
  assert.equal(result.status, "drafted");
  assert.match(requests[1]!.user, /too broad to prove the goal/);
  assert.equal(result.steps.some((s) => s.args.selector === "#wall"), false);
});

test("repeating the last committed step is nudged forward without re-execution", async () => {
  const { llm, requests } = scriptedLlm([NAVIGATE, NAVIGATE, ASSERT_HEADLINE, DONE]);
  const { page, calls } = fakePage({ texts: { "#headline": "Example Domain" } });
  const result = await walkFlow(options(llm, page));
  assert.equal(result.status, "drafted");
  assert.match(requests[2]!.user, /Do NOT repeat it/);
  assert.equal(calls.filter((c) => c.startsWith("goto:")).length, 1);
});

test("a failing execution feeds back and the walk aborts after the attempt budget", async () => {
  const clickStep = fence({ done: false, step: { id: "tap", action: "click", args: { selector: "#go" } } });
  const { llm, requests } = scriptedLlm([NAVIGATE, clickStep, clickStep, clickStep]);
  const { page } = fakePage({ failClicks: ["#go"] });
  const result = await walkFlow(options(llm, page));

  assert.equal(result.status, "aborted");
  assert.deepEqual(result.steps.map((s) => s.action), ["navigate"]);
  assert.match(requests[2]!.user, /Executing that step FAILED/);
  // the partial draft is still assembled for inspection
  assert.match(result.yaml, /action: navigate/);
});

test("readValueNearLabel asserts by shape: pattern required, validated, and emitted as regexMatch", async () => {
  const noPattern = fence({
    done: false,
    step: { id: "fdv", action: "readValueNearLabel", args: { label: "完全稀释市值" }, assert: true },
  });
  const wrongPattern = fence({
    done: false,
    step: {
      id: "fdv",
      action: "readValueNearLabel",
      args: { label: "完全稀释市值" },
      assert: true,
      assertPattern: "^€",
    },
  });
  const goodPattern = fence({
    done: false,
    step: {
      id: "fdv",
      action: "readValueNearLabel",
      args: { label: "完全稀释市值" },
      assert: true,
      assertPattern: "^\\$[0-9][0-9.,]*[KMB]?$",
    },
  });
  const { llm, requests } = scriptedLlm([noPattern, wrongPattern, goodPattern, DONE]);
  const { page } = fakePage({ readNearValue: "$76.55B" });
  const result = await walkFlow(options(llm, page));

  assert.equal(result.status, "drafted");
  assert.match(requests[1]!.user, /needs "assertPattern"/);
  assert.match(requests[2]!.user, /does not match assertPattern/);
  // label-anchored: no selector, so no generic-selector rejection and no auto-wait
  assert.deepEqual(result.steps.map((s) => s.action), ["readValueNearLabel"]);
  assert.match(result.yaml, /regexMatch\(steps\.fdv\.output\.value, "\^\\\\\$\[0-9\]\[0-9.,\]\*\[KMB\]\?\$"\)/);
});

test("re-emitting the same read WITH an assertion upgrades it in place, not a repeat", async () => {
  // the exact sequence that once aborted a live walk: a plain read commits,
  // then the model closes by re-emitting the identical read as the assertion
  const plainRead = fence({
    done: false,
    step: { id: "fdv", action: "readValueNearLabel", args: { label: "总市值" } },
  });
  const assertingRead = fence({
    done: false,
    step: {
      id: "fdv",
      action: "readValueNearLabel",
      args: { label: "总市值" },
      assert: true,
      assertPattern: "^\\$[0-9]",
    },
  });
  const { llm } = scriptedLlm([plainRead, assertingRead, DONE]);
  const { page } = fakePage({ readNearValue: "$1.23B" });
  const result = await walkFlow(options(llm, page));

  assert.equal(result.status, "drafted");
  // upgraded in place: ONE read step, now asserting — not a duplicated read
  assert.deepEqual(result.steps.map((s) => s.action), ["readValueNearLabel"]);
  assert.equal(result.steps[0]?.assert, true);
  assert.match(result.yaml, /regexMatch\(steps\.fdv\.output\.value/);
});

test("a repeat AFTER the assertion landed finishes the walk instead of aborting it", async () => {
  // observed live: the model committed the asserting read, then re-emitted it
  // verbatim instead of saying done — the goal condition is objective, so the
  // walk must finish, not burn attempts on a complete draft
  const assertingRead = fence({
    done: false,
    step: {
      id: "cap",
      action: "readValueNearLabel",
      args: { label: "总市值" },
      assert: true,
      assertPattern: "^\\$[0-9]",
    },
  });
  const { llm, requests } = scriptedLlm([assertingRead, assertingRead]);
  const { page } = fakePage({ readNearValue: "$1.23B" });
  const result = await walkFlow(options(llm, page));

  assert.equal(result.status, "drafted");
  assert.equal(requests.length, 2); // no third attempt, no DONE required
  assert.deepEqual(result.steps.map((s) => s.action), ["readValueNearLabel"]);
});

test("a walk that never lands a step reports empty", async () => {
  const { llm } = scriptedLlm(["not json at all", "still not json", "nope"]);
  const { page } = fakePage();
  const result = await walkFlow(options(llm, page));
  assert.equal(result.status, "empty");
  assert.deepEqual(result.steps, []);
});
