import assert from "node:assert/strict";
import { test } from "node:test";

import { unifiedDiff } from "../src/diff.ts";
import { proposeRepair } from "../src/repair.ts";
import type {
  CompileOutcome,
  CompileRunner,
  DrafterLlm,
  LlmRequest,
} from "../src/types.ts";

const ORIGINAL = `flow: shop
provider: devicerail
steps:
  - id: open
    tap:
      element: { identifier: cart_button }
  - id: check
    find:
      element: { identifier: totls_label }
`;

const REVISED = `flow: shop
provider: devicerail
steps:
  - id: open
    tap:
      element: { identifier: cart_button }
  - id: check
    find:
      element: { identifier: totals_label }
`;

const DOSSIER = {
  stepId: "check",
  verdict: { status: "fail", summary: "selector matched nothing" },
};

function scriptedLlm(replies: string[]): {
  llm: DrafterLlm;
  requests: LlmRequest[];
} {
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

test("unified diff: minimal hunk, empty on identical inputs", () => {
  // Negative control first: no change renders as nothing at all.
  assert.equal(unifiedDiff(ORIGINAL, ORIGINAL, "shop.flow.yaml"), "");

  const diff = unifiedDiff(ORIGINAL, REVISED, "shop.flow.yaml");
  assert.ok(diff.startsWith("--- a/shop.flow.yaml\n+++ b/shop.flow.yaml\n"));
  assert.ok(diff.includes("-      element: { identifier: totls_label }"));
  assert.ok(diff.includes("+      element: { identifier: totals_label }"));
  // The untouched head of the file is NOT in the hunk (minimal patch,
  // context-bounded) — the first line never appears.
  assert.ok(!diff.includes("-flow: shop"));
  assert.ok(!diff.includes("+flow: shop"));
});

test("repair: dossier and source ride the prompt; proposal carries the diff", async () => {
  const { llm, requests } = scriptedLlm(["```yaml\n" + REVISED + "```"]);
  const result = await proposeRepair({
    dossier: DOSSIER,
    flowYaml: ORIGINAL,
    flowName: "shop.flow.yaml",
    llm,
    compile: compilerReturning(OK),
  });
  assert.equal(result.status, "proposed");
  assert.ok(result.status === "proposed");
  assert.equal(result.yaml.trim(), REVISED.trim());
  assert.ok(result.diff.includes("+      element: { identifier: totals_label }"));
  // The prompt wove in the evidence and the file under repair.
  const user = requests[0].user;
  assert.ok(user.includes("selector matched nothing"), "dossier in prompt");
  assert.ok(user.includes("totls_label"), "current source in prompt");
  assert.ok(requests[0].system.includes("MINIMAL revision"));
});

test("repair: compile diagnostics feed back until the proposal compiles", async () => {
  const rejected: CompileOutcome = {
    ok: false,
    diagnostics: [
      {
        code: "RF2001",
        severity: "error",
        message: "bad shape",
      },
    ],
  };
  const { llm, requests } = scriptedLlm([
    "```yaml\nflow: broken\n```",
    "```yaml\n" + REVISED + "```",
  ]);
  const result = await proposeRepair({
    dossier: DOSSIER,
    flowYaml: ORIGINAL,
    llm,
    compile: compilerReturning(rejected, OK),
  });
  assert.equal(result.status, "proposed");
  assert.equal(requests.length, 2);
  assert.ok(
    requests[1].user.includes("RF2001"),
    "diagnostics fed back to the second attempt",
  );
});

test("repair: an unauthorized degraded channel stops at the gate", async () => {
  // The tempting repair — "just add a coordinate fallback" — must come
  // back as a fallbackAuthorization question, never a silent proposal.
  const degraded = ORIGINAL.replace(
    "    find:\n      element: { identifier: totls_label }",
    "    find:\n      element: { identifier: totls_label }\n    locate_via: [uiTree, coordinate]",
  );
  const { llm } = scriptedLlm(["```yaml\n" + degraded + "```"]);
  const result = await proposeRepair({
    dossier: DOSSIER,
    flowYaml: ORIGINAL,
    llm,
    compile: compilerReturning(),
  });
  assert.equal(result.status, "needsInput");
  assert.ok(result.status === "needsInput");
  assert.equal(result.questions[0].category, "fallbackAuthorization");
});
