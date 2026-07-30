/**
 * The `pointlock compile` gate as a {@link CompileRunner}: writes the draft
 * to a temp dir, runs the real binary with `--format json`, and maps the
 * outcome. Authoring rejections come back as diagnostics; only tool
 * failures (missing binary, unparseable output) reject the promise.
 */

import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import type { CompileDiagnostic, CompileOutcome, CompileRunner } from "./types.ts";

/** Configuration of the CLI-backed compile gate. */
export interface CliCompilerOptions {
  /** Path to the `pointlock` binary. */
  bin: string;
  /** `--lockfile` to bind against (recommended; see 03 §4.0). */
  lockfile?: string;
  /** `--provider` registration; the CLI default applies when omitted. */
  provider?: string;
}

function isDiagnosticArray(value: unknown): value is CompileDiagnostic[] {
  return (
    Array.isArray(value) &&
    value.every(
      (item) =>
        typeof item === "object" &&
        item !== null &&
        typeof (item as Record<string, unknown>).code === "string" &&
        typeof (item as Record<string, unknown>).message === "string",
    )
  );
}

interface ExecResult {
  code: number;
  stdout: string;
  stderr: string;
}

function run(bin: string, args: string[]): Promise<ExecResult> {
  return new Promise((resolve, reject) => {
    execFile(bin, args, { encoding: "utf8" }, (error, stdout, stderr) => {
      const code = error?.code;
      if (error && typeof code !== "number") {
        // Spawn-level failure (ENOENT etc.) — a tool error, not a rejection.
        reject(error);
        return;
      }
      resolve({ code: typeof code === "number" ? code : 0, stdout, stderr });
    });
  });
}

/** Builds a {@link CompileRunner} over the real CLI. */
export function cliCompiler(options: CliCompilerOptions): CompileRunner {
  return async (yaml: string): Promise<CompileOutcome> => {
    const dir = await mkdtemp(join(tmpdir(), "pointlock-nl-"));
    try {
      const flowPath = join(dir, "draft.flow.yaml");
      const irPath = join(dir, "draft.ir.json");
      await writeFile(flowPath, yaml, "utf8");

      const args = ["compile", flowPath, "--out", irPath, "--format", "json"];
      if (options.lockfile !== undefined) {
        args.push("--lockfile", options.lockfile);
      }
      if (options.provider !== undefined) {
        args.push("--provider", options.provider);
      }

      const result = await run(options.bin, args);
      if (result.code === 0) {
        const irJson: unknown = JSON.parse(await readFile(irPath, "utf8"));
        return { ok: true, irJson, warnings: [] };
      }

      let parsed: unknown;
      try {
        parsed = JSON.parse(result.stdout);
      } catch {
        throw new Error(
          `pointlock compile exited ${result.code} without JSON diagnostics: ${
            result.stderr.trim() || result.stdout.trim() || "(no output)"
          }`,
        );
      }
      if (!isDiagnosticArray(parsed)) {
        throw new Error(
          `pointlock compile exited ${result.code} with unexpected JSON on stdout`,
        );
      }
      return { ok: false, diagnostics: parsed };
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  };
}
