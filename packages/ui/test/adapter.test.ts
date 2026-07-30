// M3a acceptance 8: the UI touches the store ONLY through the projection
// protocol — verified here by driving the adapter with the protocol's
// golden fixtures (the same bytes the Rust side pins) and asserting the
// renderer-side mapping rules (08 §3.1/§3.2).

import { describe, expect, it } from "vitest";
import type { FlowGraphView, RunOverview } from "@pointlock/projection-types";
import graphFixture from "../../../schema/fixtures/projection/flow-graph-view.golden.json";
import overviewFixture from "../../../schema/fixtures/projection/run-overview.golden.json";
import {
  adaptGraph,
  buildOverlay,
  matchesSelection,
  resolveInstances,
  stripIterations,
} from "../src/adapter/graph";

const view = graphFixture as unknown as FlowGraphView;
const overview = overviewFixture as unknown as RunOverview;

describe("adaptGraph over the golden FlowGraphView", () => {
  const graph = adaptGraph(view, null);

  it("maps every node with a §3.1 custom type and a layout position", () => {
    expect(graph.nodes.length).toBe(view.nodes.length);
    for (const node of graph.nodes) {
      expect(node.type).toMatch(
        /^step(Action|Assert|Call|Human|If|Foreach|Let)$/,
      );
      expect(Number.isFinite(node.position.x)).toBe(true);
      expect(Number.isFinite(node.position.y)).toBe(true);
    }
  });

  it("keeps the closed edge vocabulary: seq/branch drawn, hook folded", () => {
    // Hook edges never reach React Flow as edges — they fold onto the
    // host node as badges (handlers are not nodes, 08 §3.1).
    const drawn = graph.edges.length;
    const protocolDrawable = view.edges.filter(
      (edge) => edge.kind !== "hook",
    ).length;
    expect(drawn).toBe(protocolDrawable);
    const hookHosts = view.edges
      .filter((edge) => edge.kind === "hook")
      .map((edge) => edge.from);
    for (const host of hookHosts) {
      const node = graph.nodes.find((candidate) => candidate.id === host);
      expect(node?.data.hooks.length).toBeGreaterThan(0);
    }
  });

  it("labels branch edges with then/else", () => {
    const branches = view.edges.filter((edge) => edge.kind === "branch");
    for (const branch of branches) {
      const adapted = graph.edges.find(
        (edge) => edge.source === branch.from && edge.target === branch.to,
      );
      expect(adapted?.label).toBe(branch.label);
    }
  });

  it("carries the runPath anchor for deep links (locate hyperlinks)", () => {
    for (const node of graph.nodes) {
      expect(node.data.runPath).toContain("@");
    }
    const call = graph.nodes.find((node) => node.type === "stepCall");
    expect(call?.data.runPath).toContain("/call→");
  });
});

describe("buildOverlay over the golden RunOverview", () => {
  it("joins per-step cells onto static anchors", () => {
    const overlay = buildOverlay(overview);
    for (const key of Object.keys(overview.steps)) {
      expect(overlay.has(stripIterations(key))).toBe(true);
    }
  });

  it("worst instance wins the aggregate (fail > unknown > pass)", () => {
    const synthetic = {
      ...overview,
      steps: {
        "demo@aaaaaaaa/each[0]/tap": {
          state: "judged",
          verdictStatus: "pass",
          degraded: false,
        },
        "demo@aaaaaaaa/each[1]/tap": {
          state: "judged",
          verdictStatus: "fail",
          degraded: false,
        },
        "demo@aaaaaaaa/each[2]/tap": {
          state: "judged",
          verdictStatus: "unknown",
          degraded: false,
        },
      },
    } as unknown as RunOverview;
    const overlay = buildOverlay(synthetic);
    const cell = overlay.get("demo@aaaaaaaa/each/tap");
    expect(cell?.instances).toBe(3);
    expect(cell?.aggregate).toBe("fail");
    expect(cell?.tally).toContain("3/3 judged");
    expect(cell?.tally).toContain("1 pass");
    expect(cell?.tally).toContain("1 fail");
  });

  it("unknown is its own class — never folded into fail", () => {
    const synthetic = {
      ...overview,
      steps: {
        "demo@aaaaaaaa/lookup": {
          state: "judged",
          verdictStatus: "unknown",
          degraded: false,
        },
      },
    } as unknown as RunOverview;
    const overlay = buildOverlay(synthetic);
    expect(overlay.get("demo@aaaaaaaa/lookup")?.aggregate).toBe("unknown");
  });

  it("executed-without-verdict reads unverified, not pass (R4)", () => {
    const synthetic = {
      ...overview,
      steps: {
        "demo@aaaaaaaa/fire": { state: "judged", degraded: false },
      },
    } as unknown as RunOverview;
    const overlay = buildOverlay(synthetic);
    expect(overlay.get("demo@aaaaaaaa/fire")?.aggregate).toBe("unverified");
  });

  it("stripIterations only strips iteration frames", () => {
    expect(stripIterations("f@aaaaaaaa/each[2]/tap#1!a")).toBe(
      "f@aaaaaaaa/each/tap#1!a",
    );
    expect(stripIterations("f@aaaaaaaa/each[0:key]/tap")).toBe(
      "f@aaaaaaaa/each/tap",
    );
    expect(stripIterations("f@aaaaaaaa/s/call→g@bbbbbbbb")).toBe(
      "f@aaaaaaaa/s/call→g@bbbbbbbb",
    );
  });
});

describe("selection matching and instance resolution (review wave)", () => {
  it("matchesSelection is boundary-aware — pay never matches payment", () => {
    expect(matchesSelection("f@aaaaaaaa/payment", "f@aaaaaaaa/pay")).toBe(false);
    expect(matchesSelection("f@aaaaaaaa/pay#1:act", "f@aaaaaaaa/pay")).toBe(true);
    expect(matchesSelection("f@aaaaaaaa/pay!token", "f@aaaaaaaa/pay")).toBe(true);
    expect(matchesSelection("f@aaaaaaaa/pay/sub", "f@aaaaaaaa/pay")).toBe(true);
  });

  it("matchesSelection joins iterated entries onto static anchors (both sides stripped)", () => {
    expect(matchesSelection("f@aaaaaaaa/each[0]/tap#1", "f@aaaaaaaa/each/tap")).toBe(true);
    expect(matchesSelection("f@aaaaaaaa/each[2:key]/tap", "f@aaaaaaaa/each[0]/tap")).toBe(true);
    expect(matchesSelection("f@aaaaaaaa/other/tap", "f@aaaaaaaa/each/tap")).toBe(false);
  });

  it("resolveInstances lists every runtime instance of an anchor", () => {
    const synthetic = {
      ...overview,
      steps: {
        "f@aaaaaaaa/each[0]/tap": { state: "judged", degraded: false },
        "f@aaaaaaaa/each[1]/tap": { state: "judged", degraded: false },
        "f@aaaaaaaa/solo": { state: "judged", degraded: false },
      },
    } as unknown as RunOverview;
    expect(resolveInstances(synthetic, "f@aaaaaaaa/each/tap")).toEqual([
      "f@aaaaaaaa/each[0]/tap",
      "f@aaaaaaaa/each[1]/tap",
    ]);
    expect(resolveInstances(synthetic, "f@aaaaaaaa/solo")).toEqual([
      "f@aaaaaaaa/solo",
    ]);
    expect(resolveInstances(synthetic, "f@aaaaaaaa/absent")).toEqual([]);
  });

  it("aggregate tally includes the unknown count", () => {
    const synthetic = {
      ...overview,
      steps: {
        "f@aaaaaaaa/each[0]/t": { state: "judged", verdictStatus: "pass", degraded: false },
        "f@aaaaaaaa/each[1]/t": { state: "judged", verdictStatus: "unknown", degraded: false },
      },
    } as unknown as RunOverview;
    const overlay = buildOverlay(synthetic);
    expect(overlay.get("f@aaaaaaaa/each/t")?.tally).toContain("1 unknown");
  });

  it("border state prefers a live instance over settled siblings", () => {
    const synthetic = {
      ...overview,
      steps: {
        // Lexicographically LAST key is settled; an earlier key is live —
        // the border channel must keep pulsing (map order ≠ time order).
        "f@aaaaaaaa/each[0]/t": { state: "acting", degraded: false },
        "f@aaaaaaaa/each[9]/t": { state: "judged", verdictStatus: "pass", degraded: false },
      },
    } as unknown as RunOverview;
    const overlay = buildOverlay(synthetic);
    expect(overlay.get("f@aaaaaaaa/each/t")?.state).toBe("acting");
  });
});

describe("act-chain runtime marks on the overlay (08 §3.4)", () => {
  const cell = (marks?: unknown) => ({
    state: "judged",
    verdictStatus: "pass",
    degraded: false,
    ...(marks ? { actChainMarks: marks } : {}),
  });
  const marks = [{ chainIndex: 1, mark: "succeeded" }];

  it("threads marks for a single-instance anchor", () => {
    const overlay = buildOverlay({
      steps: { "flow!aaaaaaaa/step:pay": cell(marks) },
    } as unknown as RunOverview);
    expect(overlay.get("flow!aaaaaaaa/step:pay")?.actChainMarks).toEqual(marks);
  });

  it("retires marks when several instances fold onto one anchor", () => {
    // A multi-instance aggregate has no single truthful chain story —
    // the marks must vanish rather than claim one instance's dispatches
    // for all of them.
    const overlay = buildOverlay({
      steps: {
        "flow!aaaaaaaa/each:loop[0]/step:pay": cell(marks),
        "flow!aaaaaaaa/each:loop[1]/step:pay": cell(),
      },
    } as unknown as RunOverview);
    expect(
      overlay.get("flow!aaaaaaaa/each:loop/step:pay")?.actChainMarks,
    ).toBeUndefined();
    expect(overlay.get("flow!aaaaaaaa/each:loop/step:pay")?.instances).toBe(2);
  });
});

describe("dossier helpers", () => {
  const irNode = {
    assertions: [
      { assertId: "a1", verifyVia: ["uiTree", "vision"] },
      { assertId: "a2", verifyVia: ["dom"] },
    ],
  };

  it("marks degradedVerify only on a non-preferred answering channel", async () => {
    const { isDegradedVerify } = await import("../src/adapter/dossier");
    expect(isDegradedVerify(irNode, { assertId: "a1", channel: "vision" })).toBe(
      true,
    );
    // Negative controls: preferred channel, channel-less outcome, and a
    // missing IR node are never marked — undecidable is not degraded.
    expect(isDegradedVerify(irNode, { assertId: "a1", channel: "uiTree" })).toBe(
      false,
    );
    expect(isDegradedVerify(irNode, { assertId: "a1" })).toBe(false);
    expect(isDegradedVerify(null, { assertId: "a1", channel: "vision" })).toBe(
      false,
    );
  });

  it("computes attempt durations only when both ends were recorded", async () => {
    const { attemptDurationMs } = await import("../src/adapter/dossier");
    expect(attemptDurationMs({ startedAtMs: 1000, finishedAtMs: 1450 })).toBe(450);
    expect(attemptDurationMs({ startedAtMs: 1000 })).toBeUndefined();
    expect(attemptDurationMs({})).toBeUndefined();
  });
});

describe("subflow expansion (08 §3.3)", () => {
  const callNode = (view as FlowGraphView).nodes.find(
    (n: FlowGraphView["nodes"][number]) => n.kind === "call",
  );

  it("the golden call node exists and is collapsed by default", () => {
    expect(callNode).toBeDefined();
    const collapsed = adaptGraph(view, null);
    expect(collapsed.nodes.some((n) => n.parentId)).toBe(false);
  });

  const calleeView = {
    projectionVersion: 1,
    flowId: "ensure_logged_in",
    irHash: callNode!.calleeIrHash,
    nodes: [
      {
        id: "enter_pin",
        runPath: "ensure_logged_in@bbbbbbbb/enter_pin",
        kind: "action",
        verb: "set_value",
        mutating: true,
        actChain: ["uiTree"],
        assertionCount: 0,
      },
      {
        id: "nested",
        runPath: "ensure_logged_in@bbbbbbbb/nested/call→otp@cccccccc",
        kind: "call",
        calleeFlowId: "otp",
        calleeIrHash: "sha256:" + "c".repeat(64),
        inputKeys: [],
      },
    ],
    edges: [{ from: "enter_pin", to: "nested", kind: "seq" }],
    flowHooks: [],
  } as unknown as FlowGraphView;

  it("expands a call into a group with composed runtime anchors", () => {
    const expansions = new Map([[callNode!.id, calleeView]]);
    const graph = adaptGraph(view, null, expansions);
    const parent = graph.nodes.find((n) => n.id === callNode!.id)!;
    expect(parent.data.expanded).toBe(true);
    const child = graph.nodes.find(
      (n) => n.id === `${callNode!.id}::enter_pin`,
    )!;
    expect(child.parentId).toBe(callNode!.id);
    expect(child.extent).toBe("parent");
    // The composed anchor IS the spine §9 grammar of the callee step
    // instance — byte-exact.
    expect(child.data.runPath).toBe(
      "wifi_toggle@aaaaaaaa/ensure_session/call→ensure_logged_in@bbbbbbbb/enter_pin",
    );
    // The container reserves room for its children.
    const style = parent.style as { width: number; height: number };
    expect(style.width).toBeGreaterThan(0);
    expect(style.height).toBeGreaterThan(0);
  });

  it("run overlay joins onto the composed anchor (call-frame prefix filter)", () => {
    const expansions = new Map([[callNode!.id, calleeView]]);
    const overview = {
      steps: {
        "wifi_toggle@aaaaaaaa/ensure_session/call→ensure_logged_in@bbbbbbbb/enter_pin":
          { state: "judged", verdictStatus: "fail", degraded: false },
      },
    } as unknown as RunOverview;
    const graph = adaptGraph(view, overview, expansions);
    const child = graph.nodes.find(
      (n) => n.id === `${callNode!.id}::enter_pin`,
    )!;
    expect(child.data.overlay.aggregate).toBe("fail");
    // Negative control: without expansion the internal record still
    // aggregates nowhere else — the collapsed call node's own anchor is
    // NOT the inner step's.
    const collapsed = adaptGraph(view, overview);
    const call = collapsed.nodes.find((n) => n.id === callNode!.id)!;
    expect(call.data.overlay.aggregate).toBe("none");
  });

  it("nested expansion composes level by level", () => {
    const otpView = {
      projectionVersion: 1,
      flowId: "otp",
      irHash: "sha256:" + "c".repeat(64),
      nodes: [
        {
          id: "type_code",
          runPath: "otp@cccccccc/type_code",
          kind: "action",
          verb: "set_value",
          mutating: true,
          actChain: ["uiTree"],
          assertionCount: 0,
        },
      ],
      edges: [],
      flowHooks: [],
    } as unknown as FlowGraphView;
    const expansions = new Map<string, FlowGraphView>([
      [callNode!.id, calleeView],
      [`${callNode!.id}::nested`, otpView],
    ]);
    const graph = adaptGraph(view, null, expansions);
    const grandchild = graph.nodes.find(
      (n) => n.id === `${callNode!.id}::nested::type_code`,
    )!;
    expect(grandchild.parentId).toBe(`${callNode!.id}::nested`);
    expect(grandchild.data.runPath).toBe(
      "wifi_toggle@aaaaaaaa/ensure_session/call→ensure_logged_in@bbbbbbbb/nested/call→otp@cccccccc/type_code",
    );
  });

  it("a missing artifact expands to the honest placeholder, never an error", () => {
    const expansions = new Map<string, "unavailable">([
      [callNode!.id, "unavailable"],
    ]);
    const graph = adaptGraph(view, null, expansions);
    const call = graph.nodes.find((n) => n.id === callNode!.id)!;
    expect(call.data.unavailable).toBe(true);
    expect(graph.nodes.some((n) => n.parentId)).toBe(false);
  });
});
