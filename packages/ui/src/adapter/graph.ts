// FlowGraphView → React Flow adapter (08 §3.0 two-layer structure): the
// protocol DTO is renderer-agnostic; node/edge type literals, layout,
// and the overlay join live HERE, as internal implementation detail —
// never in the protocol (spine §10.5).

import dagre from "@dagrejs/dagre";
import type {
  FlowGraphView,
  RunOverview,
} from "@pointlock/projection-types";

/** Overlay cell after the renderer-side aggregate join (08 §3.2). */
export interface NodeOverlay {
  /** Instance count folded onto this static node. */
  instances: number;
  /** Aggregate verdict/state class for fill (worst instance wins). */
  aggregate: "fail" | "unknown" | "active" | "pass" | "unverified" | "none";
  /** Last-seen state for the border/animation channel (08 §3.6). */
  state?: string;
  /** Degraded marker on any instance's verdict. */
  degraded: boolean;
  /** Verdict tally text for the aggregate badge (e.g. `2/3 pass`). */
  tally?: string;
  /** Act-chain runtime marks (08 §3.4) — threaded only when exactly one
   * instance folds onto the anchor (a multi-instance aggregate has no
   * single truthful chain story; the tally covers it). */
  actChainMarks?: RunOverview["steps"][string]["actChainMarks"];
}

export interface AdaptedNode {
  id: string;
  /** React Flow custom node type (adapter detail, 08 §3.1 mapping). */
  type: string;
  position: { x: number; y: number };
  data: {
    node: FlowGraphView["nodes"][number];
    hooks: NonNullable<FlowGraphView["edges"][number]["hook"]>[];
    overlay: NodeOverlay;
    /** Canonical anchor (deep-link target = locate argument). */
    runPath: string;
    /** Call nodes: currently expanded into a group container. */
    expanded?: boolean;
    /** Call nodes: expansion requested but the artifact is missing. */
    unavailable?: boolean;
  };
  parentId?: string;
  extent?: "parent";
  style?: Record<string, string | number>;
}

export interface AdaptedEdge {
  id: string;
  source: string;
  target: string;
  type?: string;
  label?: string;
  animated?: boolean;
  style?: Record<string, string | number>;
}

export interface AdaptedGraph {
  nodes: AdaptedNode[];
  edges: AdaptedEdge[];
  /** Flow-level hook badges (no host node — rendered in the toolbar). */
  flowHooks: FlowGraphView["flowHooks"];
}

/** The §3.1 reference mapping: node kind → React Flow custom type. */
const NODE_TYPE: Record<string, string> = {
  action: "stepAction",
  assert: "stepAssert",
  call: "stepCall",
  human: "stepHuman",
  if: "stepIf",
  foreach: "stepForeach",
  let: "stepLet",
};

/** Strips iteration suffixes (`[i]` / `[i:key]`) from an overlay key so
 * runtime instances land on their static node anchor (08 §3.2). */
export function stripIterations(key: string): string {
  return key.replace(/\[[^\]]*\]/g, "");
}

/**
 * Whether a runtime path belongs to the selected step (anchor compare on
 * BOTH sides, boundary-aware: `pay` must not match `payment` — the next
 * character after the prefix must be a frame delimiter or the end).
 */
export function matchesSelection(
  entryPath: string,
  selectedPath: string,
): boolean {
  const entry = stripIterations(entryPath);
  const selected = stripIterations(selectedPath);
  if (!entry.startsWith(selected)) return false;
  const next = entry[selected.length];
  return next === undefined || next === "/" || next === "#" || next === "!";
}

/**
 * Resolves the runtime instances folded onto a static anchor (08 §3.2
 * 实例展开): every `RunOverview.steps` key whose anchor equals the
 * selection. One instance → open it directly; several → the inspector
 * shows the selector; none → the step never entered.
 */
export function resolveInstances(
  overview: RunOverview | null,
  anchor: string,
): string[] {
  if (!overview) return [];
  const wanted = stripIterations(anchor);
  return Object.keys(overview.steps).filter(
    (key) => stripIterations(key) === wanted,
  );
}

/** Worst-instance ordering: fail > unknown > active > pass (08 §3.2). */
const AGGREGATE_RANK: Record<NodeOverlay["aggregate"], number> = {
  fail: 5,
  unknown: 4,
  active: 3,
  pass: 2,
  unverified: 1,
  none: 0,
};

const ACTIVE_STATES = new Set([
  "ready",
  "probing",
  "acting",
  "settling",
  "observing",
  "asserting",
  "awaitingHuman",
  "drifted",
]);

function cellAggregate(cell: {
  state: string;
  verdictStatus?: string | null;
}): NodeOverlay["aggregate"] {
  if (cell.verdictStatus === "fail") return "fail";
  if (cell.verdictStatus === "unknown") return "unknown";
  if (cell.verdictStatus === "pass") return "pass";
  if (ACTIVE_STATES.has(cell.state)) return "active";
  if (cell.state === "judged") return "unverified"; // executed, no verdict (R4)
  return "none";
}

/** Joins `RunOverview.steps` onto static anchors (renderer-side
 * aggregation — the protocol keys carry iteration frames). */
export function buildOverlay(
  overview: RunOverview | null,
): Map<string, NodeOverlay> {
  const map = new Map<string, NodeOverlay>();
  if (!overview) return map;
  const tallies = new Map<string, { pass: number; fail: number; unknown: number }>();
  for (const [key, cell] of Object.entries(overview.steps)) {
    const anchor = stripIterations(key);
    const aggregate = cellAggregate(cell);
    const existing = map.get(anchor);
    const tally = tallies.get(anchor) ?? { pass: 0, fail: 0, unknown: 0 };
    if (cell.verdictStatus === "pass") tally.pass += 1;
    if (cell.verdictStatus === "fail") tally.fail += 1;
    if (cell.verdictStatus === "unknown") tally.unknown += 1;
    tallies.set(anchor, tally);
    if (!existing) {
      map.set(anchor, {
        instances: 1,
        aggregate,
        state: cell.state,
        degraded: cell.degraded ?? false,
        ...(cell.actChainMarks ? { actChainMarks: cell.actChainMarks } : {}),
      });
    } else {
      existing.instances += 1;
      existing.degraded ||= cell.degraded ?? false;
      // A second instance retires the marks: the chips would otherwise
      // claim one instance's dispatch story for all of them.
      delete existing.actChainMarks;
      // The border channel prefers a LIVE state over settled ones — the
      // map is keyed (not temporal), so preference, not last-write.
      if (
        ACTIVE_STATES.has(cell.state) ||
        !ACTIVE_STATES.has(existing.state ?? "")
      ) {
        existing.state = cell.state;
      }
      if (AGGREGATE_RANK[aggregate] > AGGREGATE_RANK[existing.aggregate]) {
        existing.aggregate = aggregate;
      }
    }
  }
  for (const [anchor, overlay] of map) {
    if (overlay.instances > 1) {
      const tally = tallies.get(anchor);
      const judged =
        (tally?.pass ?? 0) + (tally?.fail ?? 0) + (tally?.unknown ?? 0);
      overlay.tally = `${judged}/${overlay.instances} judged (${tally?.pass ?? 0} pass · ${tally?.fail ?? 0} fail · ${tally?.unknown ?? 0} unknown)`;
    }
  }
  return map;
}

const NODE_WIDTH = 216;
const NODE_HEIGHT = 72;
/** Header strip of an expanded call container (identity + contract). */
const GROUP_HEADER = 64;
const GROUP_PAD = 16;

/** One expansion slot: the lazily-loaded callee graph, or the honest
 * placeholder when the irHash-pinned artifact is missing (08 §3.3: the
 * graph is always drawable — never an error). */
export type Expansion = FlowGraphView | "unavailable";

/** Composes an expanded child's runtime anchor: the call node's own
 * anchor plus the callee node's path MINUS its root flow segment —
 * exactly the spine §9 grammar of a callee step instance
 * (`root@ab/buy/call→login@cd/inner`). */
export function composeCalleePath(callAnchor: string, calleeNodePath: string): string {
  const slash = calleeNodePath.indexOf("/");
  if (slash < 0) return callAnchor; // the callee root frame itself
  return `${callAnchor}${calleeNodePath.slice(slash)}`;
}

/** One laid-out (sub)graph: nodes with positions relative to (0,0) of
 * its own frame, plus the frame's bounding size. */
interface SubLayout {
  nodes: AdaptedNode[];
  edges: AdaptedEdge[];
  width: number;
  height: number;
}

/** Lays out one FlowGraphView, recursing into expanded call nodes
 * (08 §3.3): an expanded call becomes a group container sized to its
 * lazily-loaded callee sub-layout; children carry namespaced ids
 * (`parent::child`), `parentId`, and COMPOSED runtime anchors. */
function layoutView(
  view: FlowGraphView,
  overlay: Map<string, NodeOverlay>,
  expansions: Map<string, Expansion>,
  idPrefix: string,
  anchorOf: (nodePath: string) => string,
): SubLayout {
  const hooksByNode = new Map<string, AdaptedNode["data"]["hooks"]>();
  const edges: AdaptedEdge[] = [];
  for (const [index, edge] of view.edges.entries()) {
    if (edge.kind === "hook") {
      if (edge.hook) {
        const badges = hooksByNode.get(edge.from) ?? [];
        badges.push(edge.hook);
        hooksByNode.set(edge.from, badges);
      }
      continue; // badge, not a drawn edge
    }
    if (!edge.to) continue;
    edges.push({
      id: `${idPrefix}e${index}`,
      source: `${idPrefix}${edge.from}`,
      target: `${idPrefix}${edge.to}`,
      type: "smoothstep",
      label: edge.kind === "branch" ? (edge.label ?? undefined) : undefined,
      style: edge.kind === "branch" ? { strokeDasharray: "none" } : undefined,
    });
  }

  // Expanded calls first: their sub-layouts decide the node sizes the
  // outer dagre must reserve.
  const subgraphs = new Map<string, SubLayout | "unavailable">();
  const sizeOf = (node: FlowGraphView["nodes"][number]): { w: number; h: number } => {
    const sub = subgraphs.get(`${idPrefix}${node.id}`);
    if (sub && sub !== "unavailable") {
      return {
        w: Math.max(NODE_WIDTH, sub.width + 2 * GROUP_PAD),
        h: GROUP_HEADER + sub.height + 2 * GROUP_PAD,
      };
    }
    return { w: NODE_WIDTH, h: NODE_HEIGHT };
  };
  for (const node of view.nodes) {
    const namespaced = `${idPrefix}${node.id}`;
    if (node.kind !== "call" || !expansions.has(namespaced)) continue;
    const expansion = expansions.get(namespaced)!;
    if (expansion === "unavailable") {
      subgraphs.set(namespaced, "unavailable");
      continue;
    }
    const callAnchor = anchorOf(node.runPath);
    subgraphs.set(
      namespaced,
      layoutView(expansion, overlay, expansions, `${namespaced}::`, (childPath) =>
        composeCalleePath(callAnchor, childPath),
      ),
    );
  }

  const layout = new dagre.graphlib.Graph();
  layout.setGraph({ rankdir: "TB", nodesep: 40, ranksep: 56 });
  layout.setDefaultEdgeLabel(() => ({}));
  for (const node of view.nodes) {
    const { w, h } = sizeOf(node);
    layout.setNode(node.id, { width: w, height: h });
  }
  for (const edge of edges) {
    layout.setEdge(
      edge.source.slice(idPrefix.length),
      edge.target.slice(idPrefix.length),
    );
  }
  // Parent→child ordering edges keep regions below their step.
  for (const node of view.nodes) {
    if (node.parentId) layout.setEdge(node.parentId, node.id);
  }
  dagre.layout(layout);

  let width = 0;
  let height = 0;
  const nodes: AdaptedNode[] = [];
  for (const node of view.nodes) {
    const namespaced = `${idPrefix}${node.id}`;
    const placed = layout.node(node.id);
    const { w, h } = sizeOf(node);
    const x = (placed?.x ?? 0) - w / 2;
    const y = (placed?.y ?? 0) - h / 2;
    width = Math.max(width, x + w);
    height = Math.max(height, y + h);
    const runPath = anchorOf(node.runPath);
    const anchor = stripIterations(runPath);
    const sub = subgraphs.get(namespaced);
    nodes.push({
      id: namespaced,
      type: NODE_TYPE[node.kind] ?? "stepAction",
      position: { x, y },
      data: {
        node,
        hooks: hooksByNode.get(node.id) ?? [],
        overlay:
          overlay.get(anchor) ?? {
            instances: 0,
            aggregate: "none",
            degraded: false,
          },
        runPath,
        ...(sub !== undefined && sub !== "unavailable" ? { expanded: true } : {}),
        ...(sub === "unavailable" ? { unavailable: true } : {}),
      },
      ...(sub !== undefined && sub !== "unavailable"
        ? { style: { width: w, height: h } }
        : {}),
    });
    // The expanded call's children ride RELATIVE to the container, below
    // its header strip.
    if (sub !== undefined && sub !== "unavailable") {
      for (const child of sub.nodes) {
        nodes.push({
          ...child,
          parentId: child.parentId ?? namespaced,
          extent: "parent",
          position: child.parentId
            ? child.position
            : {
                x: child.position.x + GROUP_PAD,
                y: child.position.y + GROUP_HEADER + GROUP_PAD,
              },
        });
      }
      edges.push(...sub.edges);
    }
  }
  return { nodes, edges, width, height };
}

/**
 * Adapts one FlowGraphView (+ optional run overlay, + the expansion map
 * of 08 §3.3) into laid-out React Flow nodes/edges. Layout is dagre
 * top-to-bottom — a renderer concern, never persisted (spine §10.1).
 * Hook edges (no target) fold into node badges (08 §3.1: handlers are
 * corner badges, not nodes).
 */
export function adaptGraph(
  view: FlowGraphView,
  overview: RunOverview | null,
  expansions: Map<string, Expansion> = new Map(),
): AdaptedGraph {
  const overlay = buildOverlay(overview);
  const laid = layoutView(view, overlay, expansions, "", (path) => path);
  return { nodes: laid.nodes, edges: laid.edges, flowHooks: view.flowHooks };
}
