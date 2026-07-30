/**
 * Live execution of one drafted step — the ground truth that a selector
 * lands. Semantics MIRROR the DeviceRail web driver on purpose: a step that
 * only works under laxer walk-time rules would replay to a failure.
 */
import type { ExecResult, WalkPageLike, WalkStep } from "./types.ts";

const STEP_TIMEOUT_MS = 15_000;
const NAVIGATE_TIMEOUT_MS = 45_000;
const READ_NEAR_POLL_MS = 200;

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * In-page mirror of the driver's readValueNearLabelInPage: anchor-only band
 * tolerance, both-side whitespace normalization, distance tie → ambiguous.
 * Shipped as source (see snapshot.ts for why).
 */
const READ_NEAR_SOURCE = `(label, direction, exact) => {
  const own = (e) => { let t = ""; e.childNodes.forEach((n) => { if (n.nodeType === 3) t += n.textContent || ""; }); return t.replace(/\\s+/g, " ").trim(); };
  const shown = (e) => { const b = e.getBoundingClientRect(); if (b.width < 1 || b.height < 1) return false; const s = getComputedStyle(e); return s.visibility !== "hidden" && s.display !== "none"; };
  const wanted = label.replace(/\\s+/g, " ").trim();
  const all = Array.from(document.querySelectorAll("*"));
  const ls = all.filter((e) => shown(e) && (exact ? own(e) === wanted : own(e).includes(wanted)));
  if (ls.length !== 1) return undefined;
  const a = ls[0], ab = a.getBoundingClientRect();
  const rowTol = ab.height * 0.6, colTol = ab.width * 0.6;
  let best = null; let tied = false;
  for (const e of all) {
    if (e === a || a.contains(e) || e.contains(a)) continue;
    if (!shown(e) || own(e) === "") continue;
    const bb = e.getBoundingClientRect();
    let inBand, gap;
    if (direction === "right") {
      inBand = Math.abs((bb.top + bb.height / 2) - (ab.top + ab.height / 2)) <= rowTol;
      gap = bb.left - ab.right;
    } else {
      inBand = Math.abs((bb.left + bb.width / 2) - (ab.left + ab.width / 2)) <= colTol;
      gap = bb.top - ab.bottom;
    }
    if (!inBand || gap < -1) continue;
    if (!best || gap < best.gap - 0.5) { best = { t: own(e), gap }; tied = false; }
    else if (Math.abs(gap - best.gap) <= 0.5) tied = true;
  }
  return best && !tied ? best.t : undefined;
}`;

/** Executes one step on the live page. Never throws; failures are data. */
export async function execStep(page: WalkPageLike, step: WalkStep): Promise<ExecResult> {
  const args = step.args ?? {};
  const sel = String(args.selector ?? "");
  try {
    switch (step.action) {
      case "navigate":
        await page.goto(String(args.url), { waitUntil: "domcontentloaded", timeout: NAVIGATE_TIMEOUT_MS });
        return { ok: true };
      case "click":
        await page.locator(sel).first().click({ timeout: STEP_TIMEOUT_MS });
        return { ok: true };
      case "fill":
        await page.locator(sel).first().fill(String(args.value ?? args.text ?? ""), { timeout: STEP_TIMEOUT_MS });
        return { ok: true };
      case "press":
        await page.locator(sel).first().press(String(args.key ?? args.value), { timeout: STEP_TIMEOUT_MS });
        return { ok: true };
      case "select":
        await page.locator(sel).first().selectOption(String(args.value), { timeout: STEP_TIMEOUT_MS });
        return { ok: true };
      case "scroll":
        // mirror the driver exactly: wheel deltas only (no selector variant)
        await page.wheel(Number(args.deltaX ?? 0), Number(args.deltaY ?? 0));
        return { ok: true };
      case "waitFor":
        await page.waitForLoadState(
          (args.state as "load" | "domcontentloaded" | "networkidle") ?? "load",
          { timeout: STEP_TIMEOUT_MS },
        );
        return { ok: true };
      case "waitForSelector":
        // strict like the driver: no .first() — ambiguity fails, appearance passes
        await page.locator(sel).waitFor({ state: String(args.state ?? "visible"), timeout: STEP_TIMEOUT_MS });
        return { ok: true };
      case "readValueNearLabel": {
        // mirror the driver: poll the same geometric relation until it resolves
        const label = String(args.label);
        const direction = String(args.direction ?? "right");
        const exact = args.exact !== false;
        const expression = `(${READ_NEAR_SOURCE})(${JSON.stringify(label)}, ${JSON.stringify(direction)}, ${exact})`;
        const deadline = Date.now() + STEP_TIMEOUT_MS;
        for (;;) {
          const found = (await page.evaluate(expression)) as string | undefined;
          if (found !== undefined && found !== "") return { ok: true, output: { value: found } };
          if (Date.now() >= deadline) {
            return {
              ok: false,
              error: `readValueNearLabel: label ${JSON.stringify(label)} is absent/ambiguous, or nothing sits to the ${direction} of it`,
            };
          }
          await sleep(READ_NEAR_POLL_MS);
        }
      }
      case "clickByText": {
        // mirror the driver: VISIBLE matches only (adapter contract), wait to
        // mount, then count===1
        const locator = page.getByText(String(args.text), { exact: args.exact !== false });
        await locator.first().waitFor({ state: "visible", timeout: STEP_TIMEOUT_MS });
        const count = await locator.count();
        if (count !== 1) {
          return { ok: false, error: `clickByText: text ${JSON.stringify(args.text)} matched ${count} visible elements (need exactly 1)` };
        }
        await locator.click({ button: "left", timeout: STEP_TIMEOUT_MS });
        return { ok: true };
      }
      case "elementExists":
        return { ok: true, output: { exists: (await page.locator(sel).count()) > 0 } };
      case "textContains": {
        const count = await page.locator(sel).count();
        if (count === 0) return { ok: false, error: `textContains: selector matched 0 elements: ${sel}` };
        const first = page.locator(sel).first();
        const text = (await first.innerText().catch(async () => await first.textContent())) ?? "";
        return {
          ok: true,
          output: {
            contains: text.includes(String(args.text ?? "")),
            textLen: text.length,
            actualText: text.slice(0, 120),
          },
        };
      }
      default:
        return { ok: false, error: `unknown action ${step.action}` };
    }
  } catch (error) {
    const message = String((error as Error | null)?.message ?? error).split("\n")[0]!.slice(0, 160);
    return { ok: false, error: `${step.action} failed: ${message}` };
  }
}
