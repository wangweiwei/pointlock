/**
 * Integration over the real `pointlock` binary (skips when not built).
 * Exercises the CLI compile gate and the full drafting loop with a
 * scripted LLM — the diagnostic repair loop against real diagnostics.
 */

import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { cliCompiler } from "../src/compile.ts";
import { draftFlow } from "../src/draft.ts";
import type { DrafterLlm } from "../src/types.ts";

const BIN = join(
  fileURLToPath(new URL(".", import.meta.url)),
  "../../..",
  "target/debug/pointlock",
);
const HAVE_BIN = existsSync(BIN);

const VALID_FLOW = `flow: nl_demo
provider: devicerail
params:
  ssid:
    schema: { type: string, minLength: 1 }
    required: true
steps:
  - id: set_ssid
    invoke:
      action: setElementValue
      args:
        element: ssid_field
        value: \${{ params.ssid }}
    effect: mutating
    expect_schema:
      type: object
      properties:
        value: { type: string }
  - id: read_back
    invoke:
      action: findElement
      args:
        element: ssid_field
    effect: readonly
    expect:
      - assert_id: ssid_was_typed
        expr: \${{ eq(steps.set_ssid.output.value, params.ssid) }}
`;

// References an undefined step -> RF3xxx check diagnostic.
const BROKEN_FLOW = VALID_FLOW.replace("steps.set_ssid.output", "steps.missing.output");

test("cliCompiler: a valid flow compiles to IR", { skip: !HAVE_BIN }, async () => {
  const compile = cliCompiler({ bin: BIN });
  const outcome = await compile(VALID_FLOW);
  assert.ok(outcome.ok, "expected the valid flow to compile");
  if (outcome.ok) {
    assert.ok(
      typeof outcome.irJson === "object" && outcome.irJson !== null,
      "IR artifact should be a JSON object",
    );
  }
});

test("cliCompiler: a broken flow yields JSON diagnostics", { skip: !HAVE_BIN }, async () => {
  const compile = cliCompiler({ bin: BIN });
  const outcome = await compile(BROKEN_FLOW);
  assert.ok(!outcome.ok, "expected diagnostics");
  if (!outcome.ok) {
    assert.ok(outcome.diagnostics.length > 0);
    assert.match(outcome.diagnostics[0].code, /^RF\d{4}$/);
  }
});

test(
  "drafting loop repairs a real compile rejection",
  { skip: !HAVE_BIN },
  async () => {
    const replies = [
      `\`\`\`yaml\n${BROKEN_FLOW}\`\`\``,
      `\`\`\`yaml\n${VALID_FLOW}\`\`\``,
    ];
    let sawDiagnostics = false;
    const llm: DrafterLlm = async (request) => {
      if (request.user.includes("Compile diagnostics")) {
        sawDiagnostics = true;
      }
      const next = replies.shift();
      assert.ok(next !== undefined);
      return next;
    };
    const result = await draftFlow({
      intent: "type the ssid and read it back",
      llm,
      compile: cliCompiler({ bin: BIN }),
    });
    assert.equal(result.status, "compiled");
    if (result.status === "compiled") {
      assert.equal(result.attempts, 2);
    }
    assert.ok(sawDiagnostics, "the second prompt should carry real diagnostics");
  },
);
