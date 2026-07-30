// The graph pane: React Flow over the adapted FlowGraphView. Custom node
// components render the §3.1 body payloads; state rides the border,
// verdict rides the fill (§3.6); the human node is the only non-rectangle
// (§3.5). Layout is renderer-side dagre — never persisted.

import {
  Background,
  Controls,
  Handle,
  Panel,
  Position,
  ReactFlow,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useMemo, useState } from "react";
import type { FlowGraphView, RunOverview } from "@pointlock/projection-types";
import { api } from "../api";
import {
  adaptGraph,
  matchesSelection,
  type AdaptedNode,
  type Expansion,
} from "../adapter/graph";

const ACTIVE_STATES = new Set([
  "ready",
  "probing",
  "acting",
  "settling",
  "observing",
  "asserting",
]);

function nodeClasses(
  data: AdaptedNode["data"],
  human: boolean,
  selected: boolean,
): string {
  const classes = ["rf-node"];
  if (human) classes.push("human");
  if (data.expanded) classes.push("call-expanded");
  if (selected) classes.push("selected");
  const state = data.overlay.state;
  if (state && ACTIVE_STATES.has(state)) classes.push("state-active");
  else if (state) classes.push(`state-${state}`);
  if (data.overlay.aggregate === "pass") classes.push("fill-pass");
  if (data.overlay.aggregate === "fail") classes.push("fill-fail");
  if (data.overlay.aggregate === "unknown") classes.push("fill-unknown");
  if (data.overlay.aggregate === "unverified") classes.push("fill-unverified");
  if (data.overlay.degraded) classes.push("degraded");
  return classes.join(" ");
}

type ChainMark = {
  chainIndex: number;
  mark: string;
  executionMode?: string | null;
  fallbackReason?: string | null;
};

function chainChips(
  channels: string[],
  vision = false,
  marks?: ChainMark[] | null,
) {
  const markAt = (position: number) =>
    marks?.find((mark) => mark.chainIndex === position);
  return (
    <span className="chain">
      {channels.map((channel, index) => {
        const mark = markAt(index + 1);
        const classes = ["chip"];
        if (vision && channel === "vision") classes.push("vision");
        // 08 §3.4 runtime overlay: absence = untried (only meaningful
        // once ANY mark exists for the chain).
        if (marks && marks.length > 0) {
          if (mark?.mark === "succeeded") classes.push("mark-ok");
          else if (mark?.mark === "crossed") classes.push("mark-crossed");
          else classes.push("mark-untried");
        }
        if (mark?.executionMode === "coordinateFallback") {
          classes.push("verdict-unknown");
        }
        const title = mark
          ? `${mark.mark}${mark.executionMode ? ` · ${mark.executionMode}` : ""}${mark.fallbackReason ? ` · ${mark.fallbackReason}` : ""}`
          : undefined;
        return (
          <span key={index} className={classes.join(" ")} title={title}>
            {channel === "vision" ? "👁 vision" : channel}
          </span>
        );
      })}
    </span>
  );
}

function StepNode({ data }: NodeProps) {
  const nodeData = data as unknown as AdaptedNode["data"] & {
    selected?: boolean;
    onToggleExpand?: () => void;
  };
  const node = nodeData.node;
  const body = node as Record<string, unknown>;
  const human = node.kind === "human";

  return (
    <div
      className={nodeClasses(nodeData, human, nodeData.selected ?? false)}
      title={nodeData.runPath}
    >
      <Handle type="target" position={Position.Top} />
      <div className="kind">
        {node.kind}
        {nodeData.overlay.instances > 1 ? ` · ${nodeData.overlay.tally ?? ""}` : ""}
      </div>
      <div className="title">
        {human ? "🧍 " : ""}
        {node.id}
        {node.kind === "action" && body.mutating === true ? " ●" : ""}
        {node.kind === "action" && body.mutating === false ? " ○" : ""}
      </div>
      {node.kind === "action" && (
        <>
          <div>
            <code>{String(body.verb ?? body.actionName ?? "")}</code>
            {typeof body.assertionCount === "number" && body.assertionCount > 0 && (
              <span className="chip">✓×{String(body.assertionCount)}</span>
            )}
          </div>
          {Array.isArray(body.actChain) &&
            chainChips(
              body.actChain as string[],
              false,
              nodeData.overlay.actChainMarks as ChainMark[] | undefined,
            )}
        </>
      )}
      {node.kind === "assert" &&
        Array.isArray(body.assertions) &&
        (body.assertions as { assertId: string; verifyVia: string[] }[]).map(
          (assertion) => (
            <div key={assertion.assertId}>
              <code>{assertion.assertId}</code>{" "}
              {chainChips(assertion.verifyVia, true)}
            </div>
          ),
        )}
      {node.kind === "call" && (
        <div>
          <code>
            {String(body.calleeFlowId)}@
            {String(body.calleeIrHash ?? "").replace("sha256:", "").slice(0, 8)}
          </code>
          {nodeData.onToggleExpand && (
            <button
              className="expand-btn"
              title={
                nodeData.expanded
                  ? "collapse the callee subgraph"
                  : "expand the callee subgraph (lazy-loaded by irHash, 08 §3.3)"
              }
              onClick={(event) => {
                event.stopPropagation();
                nodeData.onToggleExpand?.();
              }}
            >
              {nodeData.expanded ? "⊟" : "⊞"}
            </button>
          )}
          {Array.isArray(body.inputKeys) && (body.inputKeys as string[]).length > 0 && (
            <div className="kind">
              inputs: {(body.inputKeys as string[]).join(", ")}
            </div>
          )}
          {nodeData.unavailable && (
            <div className="kind">
              callee artifact unavailable — pass --artifacts with the bundle
              (the graph stays drawable, 08 §3.3)
            </div>
          )}
        </div>
      )}
      {node.kind === "human" && (
        <div>
          <span className="chip">{String(body.mode)}</span>
          {String(body.promptHead ?? "")}
          <div className="kind">timeout {String(body.timeoutMs)}ms → unknown</div>
        </div>
      )}
      {node.kind === "if" && <code>if {String(body.cond ?? "")}</code>}
      {node.kind === "foreach" && (
        <code>
          for {String(body.as)} in {String(body.items ?? "")}
        </code>
      )}
      {node.kind === "let" && (
        <code>let {(body.bindingKeys as string[] | undefined)?.join(", ")}</code>
      )}
      {nodeData.hooks.length > 0 && (
        <div className="hooks">
          {nodeData.hooks.map((hook, index) => (
            <span key={index} className="hook-badge" title={`maxTriggers ${hook.maxTriggers}`}>
              {hook.hook}→{hook.disposition}
            </span>
          ))}
        </div>
      )}
      <Handle type="source" position={Position.Bottom} />
    </div>
  );
}

const NODE_TYPES = {
  stepAction: StepNode,
  stepAssert: StepNode,
  stepCall: StepNode,
  stepHuman: StepNode,
  stepIf: StepNode,
  stepForeach: StepNode,
  stepLet: StepNode,
};

export function GraphView(props: {
  view: FlowGraphView;
  overview: RunOverview | null;
  onSelect?: (runPath: string) => void;
  selectedPath?: string | null;
}) {
  // 08 §3.3 expansion state: namespaced call-node id → the lazily
  // loaded callee graph (cached per node) or the honest placeholder.
  const [expansions, setExpansions] = useState<Map<string, Expansion>>(
    new Map(),
  );
  const toggleExpand = (
    nodeId: string,
    calleeFlowId: string,
    calleeIrHash: string,
  ) => {
    if (expansions.has(nodeId)) {
      const next = new Map(expansions);
      next.delete(nodeId);
      setExpansions(next);
      return;
    }
    api
      .flowDetail(calleeFlowId, calleeIrHash)
      .then((detail) =>
        setExpansions((current) =>
          new Map(current).set(nodeId, detail.graph as unknown as Expansion),
        ),
      )
      .catch(() =>
        setExpansions((current) => new Map(current).set(nodeId, "unavailable")),
      );
  };
  const graph = useMemo(
    () => adaptGraph(props.view, props.overview, expansions),
    [props.view, props.overview, expansions],
  );
  // §2.4 linkage: the selection (from a timeline click or deep link)
  // highlights its node — anchor compare, iteration frames ignored.
  const nodes = useMemo(
    () =>
      graph.nodes.map((node) => ({
        ...node,
        data: {
          ...node.data,
          selected:
            props.selectedPath != null &&
            matchesSelection(props.selectedPath, node.data.runPath),
          ...(node.data.node.kind === "call"
            ? {
                onToggleExpand: () =>
                  toggleExpand(
                    node.id,
                    String(
                      (node.data.node as Record<string, unknown>).calleeFlowId,
                    ),
                    String(
                      (node.data.node as Record<string, unknown>).calleeIrHash,
                    ),
                  ),
              }
            : {}),
        },
      })),
    [graph, props.selectedPath, expansions],
  );
  return (
    <ReactFlow
      nodes={nodes}
      edges={graph.edges}
      nodeTypes={NODE_TYPES}
      onNodeClick={(_, node) =>
        props.onSelect?.((node.data as unknown as AdaptedNode["data"]).runPath)
      }
      fitView
      minZoom={0.2}
      nodesDraggable={false}
      nodesConnectable={false}
      edgesFocusable={false}
      proOptions={{ hideAttribution: true }}
    >
      <Background gap={18} />
      <Controls showInteractive={false} />
      {graph.flowHooks.length > 0 && (
        <Panel position="top-left">
          <div className="flow-hooks">
            {graph.flowHooks.map((hook, index) => (
              <span
                key={index}
                className="hook-badge"
                title={`flow-level handler · maxTriggers ${hook.maxTriggers}`}
              >
                flow: {hook.hook}→{hook.disposition}
              </span>
            ))}
          </div>
        </Panel>
      )}
    </ReactFlow>
  );
}
