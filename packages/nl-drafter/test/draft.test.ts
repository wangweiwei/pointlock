import assert from "node:assert/strict";
import { test } from "node:test";

import { draftFlow } from "../src/draft.ts";
import type {
  CompileOutcome,
  CompileRunner,
  DrafterLlm,
  LlmRequest,
} from "../src/types.ts";

const YAML_OK = "```yaml\nflow: demo\nsteps: []\n```";
const YAML_FIXED = "```yaml\nflow: demo_fixed\nsteps: []\n```";

function scriptedLlm(replies: string[]): { llm: DrafterLlm; requests: LlmRequest[] } {
  const requests: LlmRequest[] = [];
  const queue = [...replies];
  const llm: DrafterLlm = async (request) => {
    requests.push(request);
    const next = queue.shift();
    assert.ok(next !== undefined, "LLM called more times than scripted");
    return next;
  };
  return { llm, requests };
}

function compilerReturning(...outcomes: CompileOutcome[]): CompileRunner {
  const queue = [...outcomes];
  return async () => {
    const next = queue.shift();
    assert.ok(next !== undefined, "compiler called more times than scripted");
    return next;
  };
}

const OK: CompileOutcome = { ok: true, irJson: { irHash: "x" }, warnings: [] };
const REJECTED: CompileOutcome = {
  ok: false,
  diagnostics: [
    {
      code: "RF3001",
      severity: "error",
      message: "unknown step reference `steps.missing`",
      span: { line: 3, col: 5, endLine: 3, endCol: 20 },
    },
  ],
};

test("happy path: one draft, one compile, done", async () => {
  const { llm, requests } = scriptedLlm([YAML_OK]);
  const result = await draftFlow({
    intent: "connect to wifi",
    llm,
    compile: compilerReturning(OK),
  });
  assert.equal(result.status, "compiled");
  if (result.status === "compiled") {
    assert.equal(result.yaml, "flow: demo\nsteps: []\n");
    assert.equal(result.attempts, 1);
  }
  assert.equal(requests.length, 1);
  assert.match(requests[0].system, /fallbackAuthorization/);
  assert.match(requests[0].user, /connect to wifi/);
});

test("diagnostic repair loop: rejection is fed back verbatim", async () => {
  const { llm, requests } = scriptedLlm([YAML_OK, YAML_FIXED]);
  const result = await draftFlow({
    intent: "connect",
    llm,
    compile: compilerReturning(REJECTED, OK),
  });
  assert.equal(result.status, "compiled");
  if (result.status === "compiled") {
    assert.equal(result.attempts, 2);
    assert.equal(result.yaml, "flow: demo_fixed\nsteps: []\n");
  }
  // The second prompt carries the diagnostic and the previous draft.
  assert.match(requests[1].user, /RF3001/);
  assert.match(requests[1].user, /unknown step reference/);
  assert.match(requests[1].user, /3:5/);
  assert.match(requests[1].user, /flow: demo\n/);
});

test("elicitation: questions surface as needsInput", async () => {
  const { llm } = scriptedLlm([
    `\`\`\`json
{ "elicitation": [ { "category": "secretStrategy", "question": "How to handle the password?", "path": "params/password", "options": ["plaintext param", "wait for secrets.*"] } ] }
\`\`\``,
  ]);
  const result = await draftFlow({
    intent: "log in",
    llm,
    compile: compilerReturning(),
  });
  assert.equal(result.status, "needsInput");
  if (result.status === "needsInput") {
    assert.equal(result.questions[0].category, "secretStrategy");
    assert.equal(result.attempts, 1);
  }
});

test("answers are woven back into the prompt on re-draft", async () => {
  const { llm, requests } = scriptedLlm([YAML_OK]);
  const result = await draftFlow({
    intent: "log in",
    llm,
    compile: compilerReturning(OK),
    answers: [
      { path: "params/password", answer: "plaintext param", category: "secretStrategy" },
    ],
  });
  assert.equal(result.status, "compiled");
  assert.match(requests[0].user, /params\/password.*plaintext param/);
});

test("guard: unauthorized coordinate becomes a fallbackAuthorization question", async () => {
  const draft = `\`\`\`yaml
flow: demo
steps:
  - id: tap_it
    tap:
      element: { css: ".x" }
    locate_via: [dom, coordinate]
\`\`\``;
  const { llm } = scriptedLlm([draft]);
  const result = await draftFlow({
    intent: "tap the thing",
    llm,
    compile: compilerReturning(),
  });
  assert.equal(result.status, "needsInput");
  if (result.status === "needsInput") {
    assert.equal(result.questions[0].category, "fallbackAuthorization");
    assert.equal(result.questions[0].path, "steps/tap_it/locate_via");
  }
});

test("guard: a granted channel passes through to compile", async () => {
  const draft = `\`\`\`yaml
flow: demo
steps:
  - id: tap_it
    tap:
      element: { css: ".x" }
    locate_via: [dom, coordinate]
\`\`\``;
  const { llm } = scriptedLlm([draft]);
  const result = await draftFlow({
    intent: "tap the thing",
    llm,
    compile: compilerReturning(OK),
    answers: [{ path: "steps/tap_it/locate_via", answer: "yes" }],
  });
  assert.equal(result.status, "compiled");
});

test("guard: a denied channel is fed back for removal, not re-asked", async () => {
  const draft = `\`\`\`yaml
flow: demo
steps:
  - id: tap_it
    tap:
      element: { css: ".x" }
    locate_via: [dom, coordinate]
\`\`\``;
  const clean = `\`\`\`yaml
flow: demo
steps:
  - id: tap_it
    tap:
      element: { css: ".x" }
    locate_via: [dom, uiTree]
\`\`\``;
  const { llm, requests } = scriptedLlm([draft, clean]);
  const result = await draftFlow({
    intent: "tap the thing",
    llm,
    compile: compilerReturning(OK),
    answers: [{ path: "steps/tap_it/locate_via", answer: "no" }],
  });
  assert.equal(result.status, "compiled");
  assert.match(requests[1].user, /DENIED/);
});

test("protocol violations are fed back and count as attempts", async () => {
  const { llm, requests } = scriptedLlm(["no fences here", YAML_OK]);
  const result = await draftFlow({
    intent: "connect",
    llm,
    compile: compilerReturning(OK),
  });
  assert.equal(result.status, "compiled");
  if (result.status === "compiled") {
    assert.equal(result.attempts, 2);
  }
  assert.match(requests[1].user, /Protocol violation/);
});

test("exhaustion returns the last draft and open diagnostics", async () => {
  const { llm } = scriptedLlm([YAML_OK, YAML_OK]);
  const result = await draftFlow({
    intent: "connect",
    llm,
    compile: compilerReturning(REJECTED, REJECTED),
    maxAttempts: 2,
  });
  assert.equal(result.status, "exhausted");
  if (result.status === "exhausted") {
    assert.equal(result.attempts, 2);
    assert.equal(result.diagnostics[0].code, "RF3001");
    assert.equal(result.yaml, "flow: demo\nsteps: []\n");
  }
});
