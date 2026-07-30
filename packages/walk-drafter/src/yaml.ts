/**
 * Draft assembly. Args are emitted as JSON (valid YAML flow syntax), which
 * sidesteps every quoting hazard; effects derive from the readonly action
 * set; assertions become `expect` blocks:
 * - textContains / elementExists → their boolean output field;
 * - readValueNearLabel → regexMatch over the string output with the model's
 *   shape pattern (never a live literal).
 */
import { READONLY_ACTIONS } from "./schema.ts";
import type { WalkStep } from "./types.ts";

/** Renders the committed steps as a complete *.flow.yaml. */
export function toYaml(steps: WalkStep[], flowName = "walk_drafted_flow", provider = "devicerail"): string {
  const lines = [`flow: ${flowName}`, `provider: ${provider}`, `steps:`];
  for (const step of steps) {
    const effect = READONLY_ACTIONS.has(step.action) ? "readonly" : "mutating";
    lines.push(`  - id: ${step.id}`);
    lines.push(`    invoke: { action: ${step.action}, args: ${JSON.stringify(step.args)} }`);
    lines.push(`    effect: ${effect}`);
    if (step.assert && step.action === "readValueNearLabel") {
      lines.push(`    expect:`);
      lines.push(`      - assert_id: ${step.id}_ok`);
      lines.push(`        expr: \${{ regexMatch(steps.${step.id}.output.value, ${JSON.stringify(step.assertPattern)}) }}`);
    } else if (step.assert && READONLY_ACTIONS.has(step.action)) {
      const field = step.action === "textContains" ? "contains" : "exists";
      lines.push(`    expect:`);
      lines.push(`      - assert_id: ${step.id}_ok`);
      lines.push(`        expr: "\${{ steps.${step.id}.output.${field} }}"`);
    }
  }
  return lines.join("\n") + "\n";
}
