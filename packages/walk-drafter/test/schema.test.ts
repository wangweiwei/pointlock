import assert from "node:assert/strict";
import { test } from "node:test";

import { extractActions, validateStep } from "../src/schema.ts";

const LOCKFILE = {
  device: {
    actions: [
      {
        name: "fill",
        inputSchema: {
          properties: { selector: { type: "string", pattern: "^[ -~]+$" }, text: { type: "string" } },
          required: ["selector", "text"],
        },
      },
      {
        name: "readValueNearLabel",
        inputSchema: {
          properties: {
            label: { type: "string", minLength: 1 },
            direction: { type: "string", enum: ["right", "below"] },
            exact: { type: "boolean" },
          },
          required: ["label"],
        },
      },
      { name: "scroll", inputSchema: { properties: { deltaX: { type: "integer" }, deltaY: { type: "integer" } }, required: ["deltaX", "deltaY"] } },
    ],
  },
};

test("extractActions mirrors the lockfile catalog and rejects an empty one", () => {
  const actions = extractActions(LOCKFILE);
  assert.deepEqual(Object.keys(actions).sort(), ["fill", "readValueNearLabel", "scroll"]);
  assert.deepEqual(actions["fill"]!.required, ["selector", "text"]);
  assert.throws(() => extractActions({}), /no device\.actions/);
  assert.throws(() => extractActions({ device: { actions: [] } }), /no device\.actions/);
});

test("validateStep enforces the schema the compiler will enforce later", () => {
  const actions = extractActions(LOCKFILE);
  const ok = { id: "s", action: "fill", args: { selector: "#q", text: "BNB" } };
  assert.equal(validateStep(ok, actions), null);

  assert.match(String(validateStep({ id: "s", action: "hover", args: {} }, actions)), /not one of the allowed/);
  assert.match(String(validateStep({ id: "s", action: "fill", args: { selector: "#q" } }, actions)), /requires args\.text/);
  assert.match(
    String(validateStep({ id: "s", action: "fill", args: { selector: "#q", text: "x", extra: 1 } }, actions)),
    /does not accept args\.extra/,
  );
  // the ASCII pattern that costs RF4007 at compile time bounces at draft time
  assert.match(
    String(validateStep({ id: "s", action: "fill", args: { selector: 'input[aria-label="查询"]', text: "x" } }, actions)),
    /capability schema pattern/,
  );
  assert.match(
    String(validateStep({ id: "s", action: "readValueNearLabel", args: { label: "x", direction: "above" } }, actions)),
    /must be one of/,
  );
  assert.match(
    String(validateStep({ id: "s", action: "scroll", args: { deltaX: 0, deltaY: 1.5 } }, actions)),
    /must be an integer/,
  );
  assert.equal(validateStep({ id: "s", action: "scroll", args: { deltaX: 0, deltaY: 600 } }, actions), null);
});
