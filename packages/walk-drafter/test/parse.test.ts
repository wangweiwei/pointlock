import assert from "node:assert/strict";
import { test } from "node:test";

import { parseWalkReply } from "../src/parse.ts";

test("accepts a json fence, a bare fence, and bare json; rejects prose", () => {
  const step = { done: false, step: { id: "s", action: "navigate", args: { url: "https://x.test/" } } };
  assert.deepEqual(parseWalkReply("```json\n" + JSON.stringify(step) + "\n```"), step);
  assert.deepEqual(parseWalkReply("```\n" + JSON.stringify(step) + "\n```"), step);
  assert.deepEqual(parseWalkReply(JSON.stringify(step)), step);
  assert.deepEqual(parseWalkReply('```json\n{"done": true}\n```'), { done: true, step: undefined });
  // done must be literally true — a truthy string is not a finish signal
  assert.equal(parseWalkReply('{"done": "yes"}').done, false);
  assert.throws(() => parseWalkReply("I think we should click the button"));
});
