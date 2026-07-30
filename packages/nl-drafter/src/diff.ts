/**
 * Minimal unified-diff rendering (no dependencies — the core stays
 * runtime-free). The repair loop presents its proposal as a diff
 * (R13, 08 §6.3 acceptance 9): the human approves a CHANGE, not a wall
 * of YAML.
 */

/** One aligned edit-script entry. */
type Edit =
  | { kind: "same"; line: string }
  | { kind: "del"; line: string }
  | { kind: "add"; line: string };

/** Classic LCS edit script over lines (flows are small; O(n·m) is fine). */
function editScript(before: string[], after: string[]): Edit[] {
  const n = before.length;
  const m = after.length;
  // lcs[i][j] = LCS length of before[i..] and after[j..].
  const lcs: number[][] = Array.from({ length: n + 1 }, () =>
    new Array<number>(m + 1).fill(0),
  );
  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      lcs[i][j] =
        before[i] === after[j]
          ? lcs[i + 1][j + 1] + 1
          : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }
  const edits: Edit[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (before[i] === after[j]) {
      edits.push({ kind: "same", line: before[i] });
      i += 1;
      j += 1;
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) {
      edits.push({ kind: "del", line: before[i] });
      i += 1;
    } else {
      edits.push({ kind: "add", line: after[j] });
      j += 1;
    }
  }
  for (; i < n; i += 1) {
    edits.push({ kind: "del", line: before[i] });
  }
  for (; j < m; j += 1) {
    edits.push({ kind: "add", line: after[j] });
  }
  return edits;
}

const CONTEXT = 3;

/**
 * Renders a unified diff of two texts. Identical inputs yield the empty
 * string — "no change" must be visibly nothing, never an empty-hunk
 * header.
 */
export function unifiedDiff(
  before: string,
  after: string,
  name: string,
): string {
  if (before === after) {
    return "";
  }
  const edits = editScript(before.split("\n"), after.split("\n"));

  // Group edits into hunks with CONTEXT lines of surrounding sameness.
  interface Hunk {
    beforeStart: number;
    afterStart: number;
    beforeCount: number;
    afterCount: number;
    lines: string[];
  }
  const hunks: Hunk[] = [];
  let current: Hunk | undefined;
  let beforeLine = 1;
  let afterLine = 1;
  let trailing = 0;

  const flush = () => {
    if (current !== undefined) {
      // Drop over-collected trailing context beyond CONTEXT lines.
      while (trailing > CONTEXT) {
        current.lines.pop();
        current.beforeCount -= 1;
        current.afterCount -= 1;
        trailing -= 1;
      }
      hunks.push(current);
      current = undefined;
    }
    trailing = 0;
  };

  const pending: string[] = [];
  for (const edit of edits) {
    if (edit.kind === "same") {
      if (current === undefined) {
        pending.push(` ${edit.line}`);
        if (pending.length > CONTEXT) {
          pending.shift();
        }
      } else {
        current.lines.push(` ${edit.line}`);
        current.beforeCount += 1;
        current.afterCount += 1;
        trailing += 1;
        if (trailing > CONTEXT * 2) {
          flush();
          pending.length = 0;
          pending.push(` ${edit.line}`);
        }
      }
      beforeLine += 1;
      afterLine += 1;
      continue;
    }
    if (current === undefined) {
      current = {
        beforeStart: beforeLine - pending.length,
        afterStart: afterLine - pending.length,
        beforeCount: pending.length,
        afterCount: pending.length,
        lines: [...pending],
      };
      pending.length = 0;
    } else {
      trailing = 0;
    }
    if (edit.kind === "del") {
      current.lines.push(`-${edit.line}`);
      current.beforeCount += 1;
      beforeLine += 1;
    } else {
      current.lines.push(`+${edit.line}`);
      current.afterCount += 1;
      afterLine += 1;
    }
    trailing = 0;
  }
  flush();

  const rendered = hunks
    .map(
      (hunk) =>
        `@@ -${hunk.beforeStart},${hunk.beforeCount} +${hunk.afterStart},${hunk.afterCount} @@\n` +
        hunk.lines.join("\n"),
    )
    .join("\n");
  return `--- a/${name}\n+++ b/${name}\n${rendered}\n`;
}
