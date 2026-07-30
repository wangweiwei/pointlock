/**
 * Deterministic pre-gate for the one mandatory elicitation category the
 * drafter can check on the draft itself: fallback-chain authorization
 * (03 §4.1.1 case 3). `coordinate` in `locate_via` and `vision` in
 * `verify_via` are author decisions the LLM must not sign for — if such a
 * channel shows up in a draft without a recorded grant, the drafter asks
 * instead of shipping the draft.
 *
 * This is a line-scan heuristic over the two chain keys (the prompt
 * instructs inline arrays; block sequences are also handled). It is a
 * courtesy net in front of human review — the compiler and the reviewer
 * stay the actual enforcement.
 */

import type { ElicitationAnswer, ElicitationQuestion } from "./types.ts";

/** One degraded channel found in a draft chain. */
export interface FallbackFinding {
  channel: "coordinate" | "vision";
  key: "locate_via" | "verify_via";
  /** JSON-pointer-ish target for the question, e.g. `steps/login/locate_via`. */
  path: string;
  /** 1-based line of the chain key in the draft. */
  line: number;
}

const CHAIN_KEY = /^(\s*)-?\s*(locate_via|verify_via)\s*:\s*(.*?)\s*$/;
const STEP_ID = /^\s*-?\s*id\s*:\s*["']?([A-Za-z0-9_.:-]+)["']?\s*$/;
const BLOCK_ITEM = /^(\s*)-\s*["']?([A-Za-z0-9_]+)["']?\s*$/;

function itemsOfInline(rest: string): string[] {
  const match = rest.match(/^\[(.*)\]$/);
  if (!match) {
    return [];
  }
  return match[1]
    .split(",")
    .map((item) => item.trim().replace(/^["']|["']$/g, ""))
    .filter((item) => item !== "");
}

/** Scans a draft for degraded channels in the two fallback chains. */
export function scanFallbackChains(yaml: string): FallbackFinding[] {
  const lines = yaml.split(/\r?\n/);
  const findings: FallbackFinding[] = [];
  let lastStepId: string | undefined;

  for (let i = 0; i < lines.length; i += 1) {
    const idMatch = lines[i].match(STEP_ID);
    if (idMatch) {
      lastStepId = idMatch[1];
      continue;
    }
    const keyMatch = lines[i].match(CHAIN_KEY);
    if (!keyMatch) {
      continue;
    }
    const [, indent, key, rest] = keyMatch;
    const items: string[] = [];
    if (rest !== "") {
      items.push(...itemsOfInline(rest));
    } else {
      for (let j = i + 1; j < lines.length; j += 1) {
        const itemMatch = lines[j].match(BLOCK_ITEM);
        if (!itemMatch || itemMatch[1].length <= indent.length) {
          break;
        }
        items.push(itemMatch[2]);
      }
    }
    const chainKey = key as FallbackFinding["key"];
    const degraded: FallbackFinding["channel"] | undefined =
      chainKey === "locate_via" && items.includes("coordinate")
        ? "coordinate"
        : chainKey === "verify_via" && items.includes("vision")
          ? "vision"
          : undefined;
    if (degraded) {
      findings.push({
        channel: degraded,
        key: chainKey,
        path: `steps/${lastStepId ?? "?"}/${chainKey}`,
        line: i + 1,
      });
    }
  }
  return findings;
}

const NEGATIVE = /^\s*(no|deny|denied|reject|rejected|否|不|拒绝)\b/i;

/** Whether an answer text reads as a grant. */
export function isGrant(answer: string): boolean {
  return !NEGATIVE.test(answer);
}

/** Verdict of the guard over one draft. */
export interface GuardVerdict {
  /** Findings with no recorded answer → ask the author. */
  unanswered: FallbackFinding[];
  /** Findings the author denied → the LLM must remove the channel. */
  denied: FallbackFinding[];
}

/** Checks every degraded channel against the recorded answers. */
export function checkFallbackAuthorization(
  yaml: string,
  answers: ElicitationAnswer[],
): GuardVerdict {
  const unanswered: FallbackFinding[] = [];
  const denied: FallbackFinding[] = [];
  for (const finding of scanFallbackChains(yaml)) {
    const answer = answers.find(
      (a) =>
        a.path === finding.path &&
        (a.category === undefined || a.category === "fallbackAuthorization"),
    );
    if (answer === undefined) {
      unanswered.push(finding);
    } else if (!isGrant(answer.answer)) {
      denied.push(finding);
    }
  }
  return { unanswered, denied };
}

/** Renders guard findings as the structured questions the author sees. */
export function questionsOf(findings: FallbackFinding[]): ElicitationQuestion[] {
  return findings.map((finding) => ({
    category: "fallbackAuthorization",
    question:
      finding.channel === "coordinate"
        ? `Step chain ${finding.path} wants the \`coordinate\` act channel (raw coordinates, no structural anchor). Authorize this degradation?`
        : `Step chain ${finding.path} wants the \`vision\` verify channel (model eyesight, degraded evidence). Authorize this degradation?`,
    path: finding.path,
    options: ["yes", "no"],
  }));
}
