//! `FlowGraphView` — the deterministic graph projection of a `FlowIR`
//! (spine §10.1, 08 §3.1). Pure function over `FlowIR.body`: node kinds
//! align with step kinds; edges are the closed three-class semantic set
//! seq / branch / hook; call nodes are the collapse nodes (callee loaded
//! lazily by `irHash`); foreach nodes are the aggregate nodes. No
//! coordinates, no layout, no React Flow concepts — those are renderer
//! concerns (spine §10.5).

use pointlock_ir::{
    ActionStepIR, AssertStepIR, CallStepIR, EffectClassAction, FlowIR, ForeachStepIR,
    HandlerBinding, HumanStepIR, IfStepIR, LetStepIR, PathFrame, StepIR, render_run_path,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ProjectionVersion;

/// The graph projection of one `FlowIR` (spine §10.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlowGraphView {
    /// Protocol version (spine §10.3).
    pub projection_version: ProjectionVersion,
    /// The projected flow.
    pub flow_id: String,
    /// The projected flow's content hash (full `sha256:` form).
    pub ir_hash: String,
    /// Flat node list; nesting is expressed by `parentId` + `region`.
    pub nodes: Vec<GraphNode>,
    /// The closed three-class semantic edges (seq / branch / hook).
    pub edges: Vec<GraphEdge>,
    /// Flow-level handler badges (hooks with no host step; step-level
    /// hooks ride on `hook` edges anchored at their host node).
    pub flow_hooks: Vec<HookBadge>,
}

/// Which nested sub-region of the parent a node lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum NodeRegion {
    /// `if.then` body.
    Then,
    /// `if.else` body.
    Else,
    /// `foreach.body`.
    Body,
}

/// One graph node. `id` is the flow-scoped `stepId` (unique across the
/// whole flow incl. nested regions — compiler-guaranteed, 08 §3.2).
///
/// Serde stays lenient because of the flattened kind body (the StepIR
/// pattern); the schema closes the shape via schemars.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct GraphNode {
    /// Node id = static `stepId`.
    pub id: String,
    /// The static runPath anchor (canonical string, no iteration/attempt
    /// frames): the join point for run-state overlay and deep links
    /// (08 §3.2 — strip `[i]`/`[i:key]` from `RunOverview.steps` keys to
    /// land on this anchor).
    pub run_path: String,
    /// Enclosing step node, when nested inside if/foreach.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Sub-region of the parent this node belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<NodeRegion>,
    /// Kind-specific node body; the discriminant aligns with step kinds.
    #[serde(flatten)]
    pub body: GraphNodeBody,
}

/// Kind-specific node payloads (discriminant = step kind, A.4-aligned).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GraphNodeBody {
    /// `ActionStepIR` (08 §3.1): verb/action, effect badge, act chain.
    #[serde(rename_all = "camelCase")]
    Action {
        /// Canonical verb, when the step used one (snake_case values).
        #[serde(skip_serializing_if = "Option::is_none")]
        verb: Option<String>,
        /// First bound action name (the primary attempt's).
        action_name: String,
        /// `mutating` (solid badge) vs `readonly` (hollow badge).
        mutating: bool,
        /// The act chain channel chips, in `binding.attempts` order
        /// (08 §3.4 — one attempt means no fallback, none is invented).
        act_chain: Vec<String>,
        /// Assertion count badge.
        assertion_count: u32,
    },
    /// `AssertStepIR`: observation source + assertion summaries.
    #[serde(rename_all = "camelCase")]
    Assert {
        /// `"fresh"` or the referenced `fromStep` step id.
        observe: String,
        /// Per-assertion summary rows: assertId + verify chain chips.
        assertions: Vec<AssertionSummary>,
    },
    /// `CallStepIR` — the collapse node (08 §3.3): callee identity only;
    /// the callee graph loads lazily by `calleeIrHash`.
    #[serde(rename_all = "camelCase")]
    Call {
        /// Callee flow id.
        callee_flow_id: String,
        /// Callee content hash (full `sha256:` form; renderers may
        /// abbreviate to the 8-hex prefix).
        callee_ir_hash: String,
        /// Input key names (values are authoring detail, not graph).
        input_keys: Vec<String>,
    },
    /// `HumanStepIR` (08 §3.5): the pause-for-a-person node.
    #[serde(rename_all = "camelCase")]
    Human {
        /// Interaction mode.
        mode: String,
        /// First line of the prompt.
        prompt_head: String,
        /// Response deadline; `on_timeout: unknown` is fixed vocabulary.
        timeout_ms: u64,
    },
    /// `IfStepIR`: condition summary; then/else children reference this
    /// node via `parentId` + `region`.
    #[serde(rename_all = "camelCase")]
    If {
        /// Rendered condition expression summary.
        cond: String,
        /// Whether an else region exists.
        has_else: bool,
    },
    /// `ForeachStepIR` — the aggregate node (08 §3.2): iterations are
    /// folded onto this single node, never fanned out.
    #[serde(rename_all = "camelCase")]
    Foreach {
        /// Rendered items expression summary.
        items: String,
        /// Iteration variable name.
        r#as: String,
    },
    /// `LetStepIR`: binding key names (small node).
    #[serde(rename_all = "camelCase")]
    Let {
        /// Bound variable names.
        binding_keys: Vec<String>,
    },
}

/// One assertion summary row on an assert/action node (08 §3.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssertionSummary {
    /// The assertion id.
    pub assert_id: String,
    /// Predicate discriminant (`elementState`/`elementText`/`expr`/`visual`).
    pub predicate: String,
    /// Verify chain chips in `verifyVia` order (vision is always the
    /// chain tail and verify-only — principle 7).
    pub verify_via: Vec<String>,
}

/// The closed semantic edge classes (spine §10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum GraphEdgeKind {
    /// Sibling textual order (= v0.1 execution order).
    Seq,
    /// `if` → then/else region entry.
    Branch,
    /// Step → handler badge (handlers are never regular nodes — spine
    /// concept 5; the badge payload rides on the edge).
    Hook,
}

/// One semantic edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphEdge {
    /// Edge class.
    pub kind: GraphEdgeKind,
    /// Source node id.
    pub from: String,
    /// Target node id; absent on `hook` edges (the badge is the target).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Branch label (`"then"` / `"else"`), present on `branch` edges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Handler badge payload, present on `hook` edges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook: Option<HookBadge>,
}

/// A handler badge (08 §3.1 hook row): hook + disposition + budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookBadge {
    /// Hook name (`onFail`/`onUnknown`/`onError`/`onResumeDrift`).
    pub hook: String,
    /// Disposition discriminant (`retry`/`continue`/`escalate`/`abort`/`repair`).
    pub disposition: String,
    /// Trigger budget per instance.
    pub max_triggers: u32,
    /// `onError` class filter, when declared (snake_case values).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_classes: Option<Vec<String>>,
    /// Repair target `flowId@sha256:…`, on `repair` dispositions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_target: Option<String>,
}

/// Projects one `FlowIR` into its graph view. Deterministic and pure
/// (08 §3.1); subflow bodies are NOT inlined — call nodes carry the
/// callee identity for lazy loading (spine §10.1).
pub fn flow_graph_view(flow: &FlowIR) -> FlowGraphView {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let root = vec![PathFrame::Flow {
        flow_id: flow.flow_id.clone(),
        ir_hash: flow.ir_hash.clone(),
    }];
    project_body(&flow.body, &root, None, None, &mut nodes, &mut edges);
    FlowGraphView {
        projection_version: ProjectionVersion,
        flow_id: flow.flow_id.to_string(),
        ir_hash: flow.ir_hash.to_string(),
        nodes,
        edges,
        flow_hooks: flow
            .handlers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(hook_badge)
            .collect(),
    }
}

/// Walks one body region: emits its nodes, seq edges between siblings,
/// and recurses into nested regions.
fn project_body(
    body: &[StepIR],
    prefix: &[PathFrame],
    parent_id: Option<&str>,
    region: Option<NodeRegion>,
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
) {
    let mut previous: Option<String> = None;
    for step in body {
        let step_id = step.step_id().to_string();
        let mut anchor = prefix.to_vec();
        // The anchor mirrors the runner's frame construction so overlay
        // keys and deep links join: call steps anchor at the `Call` frame
        // itself (rendered `/<stepId>/call→<callee>@<hash8>`, 07 §2.1),
        // every other kind at a `Step` frame.
        match step {
            StepIR::Call(CallStepIR { flow_ref, .. }) => anchor.push(PathFrame::Call {
                step_id: Some(step.step_id().clone()),
                callee_flow_id: flow_ref.flow_id.clone(),
                callee_ir_hash: flow_ref.ir_hash.clone(),
            }),
            _ => anchor.push(PathFrame::Step {
                step_id: step.step_id().clone(),
            }),
        }

        if let Some(prev) = previous.take() {
            edges.push(GraphEdge {
                kind: GraphEdgeKind::Seq,
                from: prev,
                to: Some(step_id.clone()),
                label: None,
                hook: None,
            });
        }
        previous = Some(step_id.clone());

        nodes.push(GraphNode {
            id: step_id.clone(),
            run_path: render_run_path(&anchor),
            parent_id: parent_id.map(str::to_owned),
            region,
            body: node_body(step),
        });

        // Step-level hooks become hook edges anchored at the host node.
        if let Some(handlers) = step.base().handlers.as_deref() {
            for binding in handlers {
                edges.push(GraphEdge {
                    kind: GraphEdgeKind::Hook,
                    from: step_id.clone(),
                    to: None,
                    label: None,
                    hook: Some(hook_badge(binding)),
                });
            }
        }

        // Nested regions: branch edges into region entries, then recurse.
        match step {
            StepIR::If(IfStepIR { then, r#else, .. }) => {
                if let Some(first) = then.first() {
                    edges.push(branch_edge(&step_id, first.step_id().as_ref(), "then"));
                }
                project_body(
                    then,
                    &anchor,
                    Some(&step_id),
                    Some(NodeRegion::Then),
                    nodes,
                    edges,
                );
                if let Some(else_body) = r#else.as_deref() {
                    if let Some(first) = else_body.first() {
                        edges.push(branch_edge(&step_id, first.step_id().as_ref(), "else"));
                    }
                    project_body(
                        else_body,
                        &anchor,
                        Some(&step_id),
                        Some(NodeRegion::Else),
                        nodes,
                        edges,
                    );
                }
            }
            StepIR::Foreach(ForeachStepIR { body, .. }) => {
                project_body(
                    body,
                    &anchor,
                    Some(&step_id),
                    Some(NodeRegion::Body),
                    nodes,
                    edges,
                );
            }
            _ => {}
        }
    }
}

fn branch_edge(from: &str, to: &str, label: &str) -> GraphEdge {
    GraphEdge {
        kind: GraphEdgeKind::Branch,
        from: from.to_owned(),
        to: Some(to.to_owned()),
        label: Some(label.to_owned()),
        hook: None,
    }
}

/// Serializes a unit-enum value to its wire literal.
fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Renders an expression as a compact human-readable summary.
fn expr_summary(expr: &pointlock_ir::Expr) -> String {
    serde_json::to_string(expr).unwrap_or_default()
}

fn hook_badge(binding: &HandlerBinding) -> HookBadge {
    let (disposition, repair_target) = match &binding.action {
        pointlock_ir::HandlerAction::Retry { .. } => ("retry", None),
        pointlock_ir::HandlerAction::Continue => ("continue", None),
        pointlock_ir::HandlerAction::Escalate { .. } => ("escalate", None),
        pointlock_ir::HandlerAction::Abort => ("abort", None),
        pointlock_ir::HandlerAction::Repair { flow_ref } => (
            "repair",
            Some(format!("{}@{}", flow_ref.flow_id, flow_ref.ir_hash)),
        ),
    };
    HookBadge {
        hook: wire(&binding.hook),
        disposition: disposition.to_owned(),
        max_triggers: binding.max_triggers,
        error_classes: binding
            .error_classes
            .as_ref()
            .map(|classes| classes.iter().map(wire).collect()),
        repair_target,
    }
}

fn node_body(step: &StepIR) -> GraphNodeBody {
    match step {
        StepIR::Action(ActionStepIR {
            verb,
            effect,
            binding,
            assertions,
            ..
        }) => GraphNodeBody::Action {
            verb: verb.as_ref().map(wire),
            action_name: binding
                .attempts
                .first()
                .map(|attempt| attempt.action_name.to_string())
                .unwrap_or_default(),
            mutating: *effect == EffectClassAction::Mutating,
            act_chain: binding
                .attempts
                .iter()
                .map(|attempt| wire(&attempt.channel))
                .collect(),
            assertion_count: assertions.len() as u32,
        },
        StepIR::Assert(AssertStepIR {
            observe,
            assertions,
            ..
        }) => GraphNodeBody::Assert {
            observe: match serde_json::to_value(observe) {
                Ok(serde_json::Value::String(fresh)) => fresh,
                Ok(other) => other
                    .get("fromStep")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                Err(_) => String::new(),
            },
            assertions: assertions.iter().map(assertion_summary).collect(),
        },
        StepIR::Call(CallStepIR {
            flow_ref, inputs, ..
        }) => GraphNodeBody::Call {
            callee_flow_id: flow_ref.flow_id.to_string(),
            callee_ir_hash: flow_ref.ir_hash.to_string(),
            input_keys: inputs.keys().map(ToString::to_string).collect(),
        },
        StepIR::Human(HumanStepIR {
            mode,
            prompt,
            timeout_ms,
            ..
        }) => GraphNodeBody::Human {
            mode: wire(mode),
            prompt_head: prompt.lines().next().unwrap_or_default().to_owned(),
            timeout_ms: *timeout_ms,
        },
        StepIR::If(IfStepIR { cond, r#else, .. }) => GraphNodeBody::If {
            cond: expr_summary(cond),
            has_else: r#else.is_some(),
        },
        StepIR::Foreach(ForeachStepIR { items, r#as, .. }) => GraphNodeBody::Foreach {
            items: expr_summary(items),
            r#as: r#as.to_string(),
        },
        StepIR::Let(LetStepIR { bindings, .. }) => GraphNodeBody::Let {
            binding_keys: bindings.keys().map(ToString::to_string).collect(),
        },
    }
}

fn assertion_summary(assertion: &pointlock_ir::AssertionIR) -> AssertionSummary {
    let predicate = serde_json::to_value(&assertion.predicate)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_owned))
        .unwrap_or_default();
    AssertionSummary {
        assert_id: assertion.assert_id.to_string(),
        predicate,
        verify_via: assertion.verify_via.iter().map(wire).collect(),
    }
}
