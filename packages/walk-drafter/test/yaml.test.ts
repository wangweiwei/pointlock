import assert from "node:assert/strict";
import { test } from "node:test";

import { toYaml } from "../src/yaml.ts";

test("effects derive from the readonly set and assertions become expect blocks", () => {
  const yaml = toYaml(
    [
      { id: "open", action: "navigate", args: { url: "https://x.test/" } },
      { id: "wait_check", action: "waitForSelector", args: { selector: "#v", state: "visible" } },
      { id: "check", action: "textContains", args: { selector: "#v", text: "ok" }, assert: true },
      { id: "there", action: "elementExists", args: { selector: "#v" }, assert: true },
      {
        id: "fdv",
        action: "readValueNearLabel",
        args: { label: "完全稀释市值" },
        assert: true,
        assertPattern: "^\\$[0-9]",
      },
    ],
    "demo_flow",
    "devicerail",
  );

  assert.match(yaml, /^flow: demo_flow\nprovider: devicerail\n/);
  assert.match(yaml, /id: open\n    invoke: \{ action: navigate, .*\n    effect: mutating/);
  assert.match(yaml, /id: wait_check\n.*\n    effect: readonly/);
  assert.match(yaml, /expr: "\$\{\{ steps\.check\.output\.contains \}\}"/);
  assert.match(yaml, /expr: "\$\{\{ steps\.there\.output\.exists \}\}"/);
  assert.match(yaml, /expr: \$\{\{ regexMatch\(steps\.fdv\.output\.value, "\^\\\\\$\[0-9\]"\) \}\}/);
  // args stay JSON: no yaml quoting hazards for arbitrary text
  assert.match(yaml, /args: \{"label":"完全稀释市值"\}/);
});

test("a step without assert emits no expect block", () => {
  const yaml = toYaml([{ id: "peek", action: "elementExists", args: { selector: "#x" } }]);
  assert.doesNotMatch(yaml, /expect:/);
  assert.match(yaml, /effect: readonly/);
});
