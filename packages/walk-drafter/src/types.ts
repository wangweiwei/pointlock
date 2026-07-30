/**
 * Shared types of the walk-and-draft loop.
 *
 * The walker lives OUTSIDE the trust boundary, exactly like the one-shot
 * drafter: it authors a *.flow.yaml draft by walking live pages, but
 * `pointlock compile` remains the sole enforcer and `pointlock run` the sole
 * judge. Everything here is advisory authoring material.
 *
 * No runtime dependencies: the LLM, the browser page, and the lockfile are
 * all injected. The page is a small structural interface implemented by a
 * Playwright `Page` (see `WalkPageLike`), so the package never links against
 * a browser runtime.
 */

/** One LLM exchange. Mirrors `@pointlock/nl-drafter`'s contract. */
export interface LlmRequest {
  system: string;
  user: string;
}

/** Provider-agnostic LLM: one request in, the raw reply text out. */
export type WalkLlm = (request: LlmRequest) => Promise<string>;

/**
 * The locator surface the walker needs. A Playwright `Locator` satisfies it
 * structurally.
 */
export interface WalkLocatorLike {
  first(): WalkLocatorLike;
  count(): Promise<number>;
  click(options: { button?: "left"; timeout: number }): Promise<unknown>;
  fill(value: string, options: { timeout: number }): Promise<unknown>;
  press(key: string, options: { timeout: number }): Promise<unknown>;
  selectOption(value: string, options: { timeout: number }): Promise<unknown>;
  waitFor(options: { state?: string; timeout: number }): Promise<unknown>;
  innerText(): Promise<string>;
  textContent(): Promise<string | null>;
}

/**
 * The page surface the walker needs. Adapters wrap a Playwright `Page`:
 *
 * - `evaluate` takes a self-contained JS EXPRESSION STRING (arguments are
 *   inlined as JSON literals). Keep it string-typed: it makes plain that the
 *   walker ships fixed source, never closures — a serialized closure that
 *   references module scope dies with a ReferenceError inside the page.
 * - `getByText` must match VISIBLE elements only (Playwright:
 *   `page.getByText(text, { exact }).filter({ visible: true })`) — the same
 *   contract the DeviceRail driver applies to `clickByText`.
 * - `locator` should pin the CSS engine (`css=` prefix) like the driver does.
 * - `wheel` maps to `page.mouse.wheel`.
 */
export interface WalkPageLike {
  goto(url: string, options: { waitUntil?: "domcontentloaded" | "load"; timeout?: number }): Promise<unknown>;
  evaluate(expression: string): Promise<unknown>;
  locator(selector: string): WalkLocatorLike;
  getByText(text: string, options: { exact: boolean }): WalkLocatorLike;
  wheel(deltaX: number, deltaY: number): Promise<unknown>;
  waitForLoadState(state: "load" | "domcontentloaded" | "networkidle", options?: { timeout?: number }): Promise<unknown>;
}

/** One advertised action, extracted from a lockfile's `device.actions`. */
export interface ActionSpec {
  /** Argument names accepted by the inputSchema (additionalProperties:false). */
  props: string[];
  /** Argument names the inputSchema requires. */
  required: string[];
  /** The raw per-property schemas, for draft-time mini validation. */
  schema: Record<string, PropertySchema>;
}

/** The subset of JSON Schema the draft-time validator understands. */
export interface PropertySchema {
  type?: string;
  enum?: unknown[];
  minLength?: number;
  maxLength?: number;
  pattern?: string;
}

/** Name → spec map for every action the lockfile advertises. */
export type ActionCatalog = Record<string, ActionSpec>;

/** One element of a page snapshot, as the in-page collector emits it. */
export interface SnapshotElement {
  /** Unique, printable-ASCII strict-CSS selector (verified in-page). */
  sel: string;
  tag: string;
  kind: "interactive" | "text";
  text?: string;
  role?: string;
  placeholder?: string;
  href?: string;
  type?: string;
  /** [x, y, w, h] in absolute page pixels; present only with geometry on. */
  b?: [number, number, number, number];
}

/** What the walker sees of the current page state. */
export interface WalkSnapshot {
  url: string;
  title: string;
  elements: SnapshotElement[];
}

/** One committed step of the draft. */
export interface WalkStep {
  id: string;
  action: string;
  args: Record<string, unknown>;
  /** True on the asserting readonly step(s) that yield the verdict. */
  assert?: boolean;
  /** Shape regex for a `readValueNearLabel` assertion (never a live literal). */
  assertPattern?: string;
}

/** Outcome of executing one step live. */
export interface ExecResult {
  ok: boolean;
  output?: Record<string, unknown>;
  error?: string;
}

/**
 * - `drafted`: finished with at least one asserting step — the useful outcome.
 * - `unasserted`: the model finished but no step asserts (no verdict on replay).
 * - `aborted`: a step burned every re-draft attempt; the partial draft is returned.
 * - `empty`: not a single step landed.
 */
export type WalkStatus = "drafted" | "unasserted" | "aborted" | "empty";

/** The walk's result. `yaml` is always assembled from whatever committed. */
export interface WalkResult {
  status: WalkStatus;
  yaml: string;
  steps: WalkStep[];
}

/** Options of `walkFlow`. */
export interface WalkOptions {
  /** The one-sentence intent to satisfy. */
  intent: string;
  llm: WalkLlm;
  page: WalkPageLike;
  /** Parsed lockfile JSON (only `device.actions` is consulted). */
  lockfile: unknown;
  /** `flow:` name of the produced YAML. Default: `walk_drafted_flow`. */
  flowName?: string;
  /** `provider:` of the produced YAML. Default: `devicerail`. */
  provider?: string;
  /** Maximum committed steps before the walk stops. Default 14. */
  maxSteps?: number;
  /** Re-draft attempts per step before aborting. Default 3. */
  maxRedraftsPerStep?: number;
  /** Extra settle time after each mutating step, for lazy-render SPAs. Default 1200. */
  settleMs?: number;
  /** Include b:[x,y,w,h] + spatial prompt rules. Default false (measured: no win). */
  geometry?: boolean;
  /** Progress sink; one human-readable line per event. Default: silent. */
  log?: (line: string) => void;
}
