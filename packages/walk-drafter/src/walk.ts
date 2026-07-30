/**
 * The walk-and-draft loop.
 *
 * snapshot → the LLM emits ONE grounded step → the step is EXECUTED live (a
 * selector that does not land is bounced back with targeted feedback) →
 * repeat until the intent is satisfied by an asserting readonly step.
 *
 * Every guard here exists because a session proved the model alone does not
 * deliver it reliably. The stance throughout: anything that must hold for the
 * draft to replay is enforced MECHANICALLY, never left to model memory.
 */
import { execStep } from "./exec.ts";
import { parseWalkReply } from "./parse.ts";
import { buildSystemPrompt, buildUserPrompt } from "./prompt.ts";
import { READONLY_ACTIONS, extractActions, validateStep } from "./schema.ts";
import { takeSnapshot } from "./snapshot.ts";
import { toYaml } from "./yaml.ts";
import type { WalkOptions, WalkPageLike, WalkResult, WalkStep } from "./types.ts";

const DEFAULT_MAX_STEPS = 14;
const DEFAULT_MAX_REDRAFTS = 3;
const DEFAULT_SETTLE_MS = 1_200;
/** An asserting textContains that matched more text than this proves nothing. */
const MAX_ASSERT_TEXT_LEN = 200;
const GENERIC_CONTAINER = /^(body|html|:root|head|main|#root|#app|document)\b/i;

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

async function settle(page: WalkPageLike, settleMs: number): Promise<void> {
  await Promise.race([
    page.waitForLoadState("networkidle").catch(() => undefined),
    sleep(4_000),
  ]);
  await sleep(settleMs); // let an SPA finish painting / lazy-mounting the new state
}

/** Walks the live page and drafts a flow for the given intent. */
export async function walkFlow(options: WalkOptions): Promise<WalkResult> {
  const log = options.log ?? (() => {});
  const maxSteps = options.maxSteps ?? DEFAULT_MAX_STEPS;
  const maxRedrafts = options.maxRedraftsPerStep ?? DEFAULT_MAX_REDRAFTS;
  const settleMs = options.settleMs ?? DEFAULT_SETTLE_MS;
  const geometry = options.geometry ?? false;
  const actions = extractActions(options.lockfile);
  const system = buildSystemPrompt(actions, geometry);

  const steps: WalkStep[] = [];
  let aborted = false;
  let finished = false;

  for (let index = 0; index < maxSteps && !finished; index++) {
    const snapshot = await takeSnapshot(options.page, geometry);
    log(`── step ${index + 1} · snapshot: ${snapshot.elements.length} elements @ ${snapshot.url}`);
    let feedback: string | undefined;
    let committed = false;

    for (let attempt = 1; attempt <= maxRedrafts; attempt++) {
      const reply = await options.llm({ system, user: buildUserPrompt(options.intent, steps, snapshot, feedback) });
      let parsed: { done: boolean; step?: unknown };
      try {
        parsed = parseWalkReply(reply);
      } catch (error) {
        feedback = `Your reply was not one parseable json block. Error: ${(error as Error).message}. Reply with exactly one \`\`\`json block.`;
        log(`  [draft ${attempt}] unparseable reply`);
        continue;
      }

      if (parsed.done) {
        // A flow with no assertion judges nothing. Don't rely on the model
        // remembering — refuse to finish until an asserting step exists.
        if (!steps.some((s) => s.assert)) {
          feedback = `You cannot finish yet: not one step carries an assertion, so this flow would execute and judge NOTHING. Emit the final readonly asserting step now — for readValueNearLabel set "assert": true AND "assertPattern" (a regex for the value's shape); for textContains/elementExists set "assert": true.`;
          log(`  [draft ${attempt}] "done" with no assertion — requiring one`);
          continue;
        }
        log(`  drafter says: done.`);
        finished = true;
        committed = true;
        break;
      }

      const bad = validateStep(parsed.step, actions);
      if (bad) {
        feedback = bad;
        log(`  [draft ${attempt}] invalid: ${bad}`);
        continue;
      }
      const step = parsed.step as WalkStep;

      // Assertion grounding (pre-exec): a verdict from a generic container
      // proves nothing. Schema-driven — only actions that actually take a
      // selector can have a too-generic one (readValueNearLabel anchors on a
      // label instead).
      if (step.assert && (actions[step.action]?.props ?? []).includes("selector")) {
        const asel = String(step.args?.selector ?? "").trim();
        // judge the FINAL segment — the element actually targeted. A rooted
        // nth-path on an id-less page legitimately STARTS at html; only a
        // selector whose target IS the container is generic.
        const target = (asel.split(">").pop() ?? "").trim();
        if (asel === "" || GENERIC_CONTAINER.test(target)) {
          feedback = `The assertion selector "${asel}" is a generic container — it proves nothing. Target a SPECIFIC element (a label / value / tab from the current snapshot) whose presence would FAIL if the previous step had not truly worked.`;
          log(`  [draft ${attempt}] generic assertion selector: ${asel}`);
          continue;
        }
      }

      // A true repeat makes no progress — but re-emitting the SAME read with an
      // assertion added is exactly how a draft is supposed to finish, so the
      // signature must include the assert fields (learned the hard way: the
      // model's correct closing move was bounced three times and the walk
      // aborted). Such a re-emit UPGRADES the committed step in place below.
      const signatureOf = (s: WalkStep) =>
        JSON.stringify([s.action, s.args ?? null, s.assert === true, s.assertPattern ?? null]);
      const last = steps[steps.length - 1];
      if (last && signatureOf(step) === signatureOf(last)) {
        // An asserting step has already landed and the model has nothing new
        // to add: that is "done" in all but name. A live walk once aborted a
        // COMPLETE draft here because the model repeated instead of saying
        // done — the goal condition is objective, so finish mechanically.
        if (steps.some((s) => s.assert)) {
          log(`  [draft ${attempt}] repeat after the assertion landed — treating as done`);
          finished = true;
          committed = true;
          break;
        }
        feedback = `You just committed that exact step and it did not change the page. Do NOT repeat it. If that step already read/checked the right target, re-emit it WITH "assert": true (plus "assertPattern" for readValueNearLabel) — that upgrades it into the final assertion. Otherwise choose a different action/target that makes progress.`;
        log(`  [draft ${attempt}] repeat of last step — nudging forward`);
        // full incoming step: truncated diagnostics have already cost cycles
        log(`      incoming=${JSON.stringify(parsed.step).slice(0, 300)}`);
        continue;
      }

      log(`  → ${step.action}  ${JSON.stringify(step.args).slice(0, 120)}`);
      const result = await execStep(options.page, step);
      if (!result.ok) {
        feedback = `Executing that step FAILED: ${result.error}. The selector may be wrong or the element isn't on THIS page yet. Pick a selector that is present in the current snapshot, or a step that navigates toward the target.`;
        log(`  ✗ ${result.error}`);
        continue;
      }

      if (step.assert && step.action === "readValueNearLabel") {
        // this action yields a STRING, so its assertion is a shape test via
        // regexMatch — a pattern survives live values, a literal does not
        const pattern = String(step.assertPattern ?? "");
        if (!pattern) {
          feedback = `A readValueNearLabel assertion needs "assertPattern": a regex describing the expected SHAPE of the value (e.g. "^\\\\$[0-9][0-9.,]*[KMB]?$"). Do not assert a literal live value — it changes between runs.`;
          log(`  [draft ${attempt}] readValueNearLabel assert without assertPattern`);
          continue;
        }
        let matched = false;
        try {
          matched = new RegExp(pattern).test(String(result.output?.value ?? ""));
        } catch {
          feedback = `assertPattern ${JSON.stringify(pattern)} is not a valid regex.`;
          log(`  [draft ${attempt}] bad assertPattern`);
          continue;
        }
        if (!matched) {
          feedback = `The value read next to ${JSON.stringify(step.args.label)} was ${JSON.stringify(result.output?.value)}, which does not match assertPattern ${JSON.stringify(pattern)}. Fix the pattern (describe the shape) or read a different label.`;
          log(`  ✗ pattern mismatch: value=${JSON.stringify(result.output?.value)} pattern=${pattern}`);
          continue;
        }
      } else if (step.assert) {
        const field = step.action === "textContains" ? "contains" : "exists";
        const value = result.output?.[field];
        if (value !== true) {
          feedback = `The assertion did not hold: output.${field} = ${JSON.stringify(value)}${result.output?.actualText ? ` (actual text: "${String(result.output.actualText)}")` : ""}. Choose a selector/text that is actually present so the assertion is true. If you asserted a live value you copied from the snapshot, note that such values change between the snapshot and execution — assert only what the intent actually asks for.`;
          // full args + what was really on screen: truncated logs cost two
          // diagnostic cycles before this line existed
          log(`  ✗ assertion false: output.${field}=${JSON.stringify(value)}`);
          log(`      args=${JSON.stringify(step.args)}`);
          log(`      onScreen=${JSON.stringify(result.output?.actualText ?? null)}`);
          continue;
        }
        if (step.action === "textContains" && Number(result.output?.textLen ?? 0) > MAX_ASSERT_TEXT_LEN) {
          feedback = `That assertion matched a LARGE container (${String(result.output?.textLen)} chars of text) — too broad to prove the goal. Target a specific short-text label/value element instead.`;
          log(`  ✗ assertion too broad: textLen=${String(result.output?.textLen)}`);
          continue;
        }
      }

      // Reproducibility: replay executes steps back-to-back with no implicit
      // waiting, so an asserting read must be preceded by a waitForSelector on
      // the same target (the walk only passed because we settled between steps).
      if (
        step.assert
        && (step.action === "textContains" || step.action === "elementExists")
        && step.args?.selector
        && actions["waitForSelector"]
      ) {
        const previous = steps[steps.length - 1];
        const covered =
          previous !== undefined
          && previous.action === "waitForSelector"
          && previous.args?.selector === step.args.selector;
        if (!covered) {
          steps.push({
            id: `wait_${step.id}`.slice(0, 60),
            action: "waitForSelector",
            args: { selector: step.args.selector, state: "visible" },
          });
          log(`  + auto waitForSelector(${String(step.args.selector).slice(0, 60)}…) before assertion`);
        }
      }

      const upgrades =
        last !== undefined
        && !last.assert
        && step.assert === true
        && last.action === step.action
        && JSON.stringify(last.args ?? null) === JSON.stringify(step.args ?? null);
      if (upgrades) {
        // same action+args, now asserting: replace instead of duplicating the read
        steps[steps.length - 1] = step;
        log(`  ✓ upgraded the previous step with the assertion${result.output ? "  output=" + JSON.stringify(result.output) : ""}`);
      } else {
        steps.push(step);
        log(`  ✓ committed${result.output ? "  output=" + JSON.stringify(result.output) : ""}`);
      }
      committed = true;
      break;
    }

    if (!committed) {
      aborted = true;
      log(`! step ${index + 1}: no valid step after ${maxRedrafts} attempts — stopping the walk.`);
      break;
    }
    if (!finished) await settle(options.page, settleMs);
  }

  const yaml = toYaml(steps, options.flowName, options.provider);
  const asserted = steps.some((s) => s.assert && READONLY_ACTIONS.has(s.action));
  const status = steps.length === 0 ? "empty" : aborted ? "aborted" : asserted ? "drafted" : "unasserted";
  return { status, yaml, steps };
}
