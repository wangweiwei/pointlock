import assert from "node:assert/strict";
import { test } from "node:test";

import { parseReply } from "../src/parse.ts";

test("extracts a yaml draft", () => {
  const parsed = parseReply("Here you go:\n```yaml\nflow: demo\nsteps: []\n```\n");
  assert.equal(parsed.kind, "draft");
  assert.ok(parsed.kind === "draft" && parsed.yaml === "flow: demo\nsteps: []\n");
});

test("accepts the yml fence alias", () => {
  const parsed = parseReply("```yml\nflow: demo\n```");
  assert.equal(parsed.kind, "draft");
});

test("extracts a valid elicitation batch", () => {
  const reply = `\`\`\`json
{ "elicitation": [ { "category": "missingRequiredParam", "question": "Which SSID?", "path": "params/ssid", "options": ["lab", "office"] } ] }
\`\`\``;
  const parsed = parseReply(reply);
  assert.equal(parsed.kind, "questions");
  if (parsed.kind === "questions") {
    assert.equal(parsed.questions.length, 1);
    assert.equal(parsed.questions[0].category, "missingRequiredParam");
    assert.equal(parsed.questions[0].path, "params/ssid");
    assert.deepEqual(parsed.questions[0].options, ["lab", "office"]);
  }
});

test("rejects an unknown category", () => {
  const reply = `\`\`\`json
{ "elicitation": [ { "category": "vibes", "question": "?", "path": "x" } ] }
\`\`\``;
  const parsed = parseReply(reply);
  assert.equal(parsed.kind, "invalid");
});

test("rejects a reply with no fenced block", () => {
  assert.equal(parseReply("flow: demo").kind, "invalid");
});

test("rejects a reply mixing draft and questions", () => {
  const reply = "```yaml\nflow: demo\n```\n```json\n{\"elicitation\":[]}\n```";
  assert.equal(parseReply(reply).kind, "invalid");
});

test("rejects an empty elicitation array", () => {
  assert.equal(parseReply('```json\n{"elicitation": []}\n```').kind, "invalid");
});
