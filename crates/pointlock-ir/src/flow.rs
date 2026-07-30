//! The flow root type and its contract declarations (spine §3, 02 §2–§3).

use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::expr::Expr;
use crate::handler::HandlerBinding;
use crate::primitives::{
    FeatureId, FlowId, Hash, Identifier, IrVersion, JsonSchemaDocument, ProviderName,
};
use crate::source_map::SourceMapEntry;
use crate::step::StepIR;
use crate::vocab::VerdictPolicy;

/// Pointlock Typed IR v0.1 — the sole input accepted by `pointlock-runner` and
/// the sole output of the `pointlock-compiler` seal phase.
///
/// Closed vocabulary per spine Appendix A. All objects are closed except the
/// three documented exemption classes (02 §2.2): embedded JSON Schema
/// documents, identifier-keyed maps, and `StepBase` (composed into variants).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlowIR {
    /// IR semantic-generation number, const `1` in v0.1.
    pub ir_version: IrVersion,
    /// The flow's name-identity.
    pub flow_id: FlowId,
    /// Canonical whole-tree hash (excluding `irHash` itself and `sourceMap`;
    /// covers callee irHashes via `subflows` — the link-closure property,
    /// 02 §12.2).
    pub ir_hash: Hash,
    /// The provider this flow was compiled against.
    pub provider: ProviderRef,
    /// Union of features required by the whole flow; fed into
    /// `FeatureOffer.required` at session open (free enforcement).
    /// Set semantics — serialized in lexicographic order.
    pub required_features: std::collections::BTreeSet<FeatureId>,
    /// Digest of the `CapabilityLockfile` used at bind time; attestation
    /// mismatch at runtime is `capability_drift`, refuse to run.
    pub lockfile_digest: Hash,
    /// Input contract.
    pub params: Vec<ParamDecl>,
    /// Output contract.
    pub outputs: Vec<OutputDecl>,
    /// The step body (≥ 1 step).
    #[schemars(length(min = 1))]
    pub body: Vec<StepIR>,
    /// Flow-level handler hooks.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub handlers: Option<Vec<HandlerBinding>>,
    /// Verdict folding policy (`strict` folds degraded pass to unknown).
    pub verdict_policy: VerdictPolicy,
    /// IR path → YAML span mapping, plus macro origin traces. Pure
    /// diagnostics: excluded from `irHash` (02 §12.2).
    pub source_map: Vec<SourceMapEntry>,
    /// Subflow registry: reference, not inline — callees are independent
    /// artifacts pinned by `irHash` (02 §6).
    #[schemars(schema_with = "subflows_schema")]
    pub subflows: BTreeMap<FlowId, FlowRef>,
}

fn subflows_schema(generator: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "object",
        "propertyNames": { "pattern": "^[A-Za-z_][A-Za-z0-9_.-]*$" },
        "additionalProperties": generator.subschema_for::<FlowRef>()
    })
}

/// The provider a flow is bound to (inline object in the baseline).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
pub struct ProviderRef {
    /// Const `"devicerail"` — the only provider of v0.1.
    pub name: ProviderName,
    /// Provider package version the manifest came from.
    #[schemars(length(min = 1))]
    pub version: String,
}

/// One declared flow parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParamDecl {
    /// Parameter name.
    pub name: Identifier,
    /// JSON Schema contract of the value.
    pub schema: JsonSchemaDocument,
    /// Whether the run must supply this parameter.
    pub required: bool,
    /// Default value (any JSON). Note: an explicit JSON `null` default does
    /// not survive a serde round-trip (absence-by-omission rule, 02 §2.4);
    /// the compiler never emits one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

/// One declared flow output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputDecl {
    /// Output name.
    pub name: Identifier,
    /// JSON Schema contract of the value.
    pub schema: JsonSchemaDocument,
    /// Projection expression producing the value.
    pub from: Expr,
}

/// Content-pinned reference to a compiled flow artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlowRef {
    /// The callee's flow id.
    pub flow_id: FlowId,
    /// The callee's content hash (integrity pin; runner verifies on load).
    pub ir_hash: Hash,
}
