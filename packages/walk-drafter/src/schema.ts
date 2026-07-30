/**
 * Lockfile-derived action catalog + draft-time argument validation.
 *
 * Both the action menu shown to the model and the validator run off the
 * lockfile's inputSchemas, so the walker stays in lockstep with the
 * capability surface: a schema violation bounces at draft time with a
 * targeted message instead of surfacing later as a compile diagnostic
 * (e.g. RF4007), and new actions need no walker changes at all.
 */
import type { ActionCatalog, PropertySchema, WalkStep } from "./types.ts";

/** Actions that read the page rather than mutating it. */
export const READONLY_ACTIONS: ReadonlySet<string> = new Set([
  "waitFor",
  "waitForSelector",
  "elementExists",
  "textContains",
  "readValueNearLabel",
]);

/** Extracts the action catalog from a parsed lockfile. */
export function extractActions(lockfile: unknown): ActionCatalog {
  const device = (lockfile as { device?: { actions?: unknown } } | null)?.device;
  const list = Array.isArray(device?.actions) ? device.actions : undefined;
  if (!list || list.length === 0) {
    throw new Error("lockfile carries no device.actions — lock a device world first");
  }
  const catalog: ActionCatalog = {};
  for (const entry of list) {
    const action = entry as {
      name?: unknown;
      inputSchema?: { properties?: Record<string, PropertySchema>; required?: unknown };
    };
    if (typeof action.name !== "string" || action.name.length === 0) continue;
    const properties = action.inputSchema?.properties ?? {};
    catalog[action.name] = {
      props: Object.keys(properties),
      required: Array.isArray(action.inputSchema?.required)
        ? action.inputSchema.required.filter((k): k is string => typeof k === "string")
        : [],
      schema: properties,
    };
  }
  if (Object.keys(catalog).length === 0) {
    throw new Error("lockfile device.actions contained no usable entries");
  }
  return catalog;
}

/**
 * Validates one drafted step against the catalog. Returns a targeted,
 * model-facing message on the first violation, or null when the step is
 * schema-clean.
 */
export function validateStep(step: unknown, actions: ActionCatalog): string | null {
  const s = step as WalkStep | null;
  if (!s || typeof s !== "object") return "no step object";
  if (!s.id) return "step.id missing";
  const spec = actions[s.action];
  if (!spec) return `action "${s.action}" is not one of the allowed invoke actions`;
  const args: Record<string, unknown> = s.args ?? {};
  for (const required of spec.required) {
    if (args[required] == null) return `action ${s.action} requires args.${required}`;
  }
  for (const key of Object.keys(args)) {
    if (!spec.props.includes(key)) {
      return `action ${s.action} does not accept args.${key} (allowed: ${spec.props.join(", ")}) — the compiler enforces additionalProperties:false`;
    }
    const schema = spec.schema[key];
    const value = args[key];
    if (!schema || value == null) continue;
    if (schema.type === "string" && typeof value !== "string") return `args.${key} must be a string`;
    if (schema.type === "boolean" && typeof value !== "boolean") return `args.${key} must be a boolean`;
    if (schema.type === "integer" && !Number.isInteger(value)) return `args.${key} must be an integer`;
    if (schema.enum && !schema.enum.includes(value)) {
      return `args.${key} must be one of ${JSON.stringify(schema.enum)}`;
    }
    if (typeof value === "string") {
      if (schema.minLength != null && value.length < schema.minLength) return `args.${key} is too short`;
      if (schema.maxLength != null && value.length > schema.maxLength) return `args.${key} is too long`;
      if (schema.pattern && !new RegExp(schema.pattern).test(value)) {
        return `args.${key} value ${JSON.stringify(value)} does not match the capability schema pattern ${schema.pattern} (e.g. selectors must be printable ASCII — pick a different "sel" from the snapshot)`;
      }
    }
  }
  if (s.action === "navigate" && !/^https?:\/\//.test(String(args.url ?? ""))) {
    return "navigate needs an http(s) url";
  }
  return null;
}
