import assert from "node:assert/strict";
import { test } from "node:test";

import {
  checkFallbackAuthorization,
  questionsOf,
  scanFallbackChains,
} from "../src/guard.ts";

const DRAFT = `flow: demo
steps:
  - id: open_panel
    tap:
      element: { css: ".wifi" }
    locate_via: [dom, uiTree, coordinate]
    effect: mutating
  - id: verify_done
    invoke:
      action: findElement
      args: { element: banner }
    effect: readonly
    expect:
      - assert_id: shows_banner
        element: { css: ".banner" }
        verify_via:
          - dom
          - vision
`;

test("finds coordinate in locate_via and vision in verify_via", () => {
  const findings = scanFallbackChains(DRAFT);
  assert.deepEqual(
    findings.map((f) => [f.channel, f.path]),
    [
      ["coordinate", "steps/open_panel/locate_via"],
      ["vision", "steps/verify_done/verify_via"],
    ],
  );
});

test("does not flag clean chains", () => {
  const clean = DRAFT.replace(", coordinate", "").replace("          - vision\n", "");
  assert.deepEqual(scanFallbackChains(clean), []);
});

test("unanswered findings become questions; grants clear them", () => {
  const open = checkFallbackAuthorization(DRAFT, []);
  assert.equal(open.unanswered.length, 2);
  assert.equal(open.denied.length, 0);

  const granted = checkFallbackAuthorization(DRAFT, [
    { path: "steps/open_panel/locate_via", answer: "yes", category: "fallbackAuthorization" },
    { path: "steps/verify_done/verify_via", answer: "yes" },
  ]);
  assert.equal(granted.unanswered.length, 0);
  assert.equal(granted.denied.length, 0);
});

test("a negative answer is a denial, not a grant", () => {
  const verdict = checkFallbackAuthorization(DRAFT, [
    { path: "steps/open_panel/locate_via", answer: "no, keep it structural" },
    { path: "steps/verify_done/verify_via", answer: "yes" },
  ]);
  assert.equal(verdict.denied.length, 1);
  assert.equal(verdict.denied[0].channel, "coordinate");
  assert.equal(verdict.unanswered.length, 0);
});

test("questionsOf renders fallbackAuthorization questions with paths", () => {
  const questions = questionsOf(scanFallbackChains(DRAFT));
  assert.equal(questions.length, 2);
  for (const q of questions) {
    assert.equal(q.category, "fallbackAuthorization");
    assert.ok(q.path.startsWith("steps/"));
    assert.deepEqual(q.options, ["yes", "no"]);
  }
});
