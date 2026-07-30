//! The projection protocol (spine §10, R14): renderer-agnostic read-only
//! DTOs — the ONLY contract between any renderer and the store.
//!
//! Five closed DTO families (spine §10.1): [`FlowGraphView`] (from FlowIR),
//! [`RunTimelineEntry`] (from RunLog), [`StepDossierView`] (= the
//! `pointlock locate` JSON shape), [`HumanInboxEntry`] (from the
//! humanRequested/humanResponded pairing), and [`RunOverview`] (run
//! summary + `revision` + per-step state map). Every top-level DTO carries
//! `projectionVersion: 1`; evolution is additive-only, breaking changes
//! bump the version, and the version is independent of `irVersion`
//! (spine §10.3).
//!
//! Discipline (08 §1 iron law 1, typed here): projections fold ledger
//! facts, they never judge — every verdict/state below is what the runner
//! recorded. No coordinates, no layout, no React Flow concepts
//! (spine §10.1/§10.5): rendering concerns stay in the renderer.

mod dossier;
mod graph;
mod inbox;
mod overview;
mod schema;
mod timeline;

pub use dossier::{
    AttemptView, FrameEnvironment, HandlerTriggerView, SourceLocation, StepDossierView,
    VerdictRecordView, locate_step, step_dossier,
};
pub use graph::{
    AssertionSummary, FlowGraphView, GraphEdge, GraphEdgeKind, GraphNode, GraphNodeBody, HookBadge,
    NodeRegion, flow_graph_view,
};
pub use inbox::{HumanInboxEntry, human_inbox, run_inbox};
pub use overview::{AlignmentSummary, RunOverview, StepStateSummary, run_overview};
pub use schema::{
    PROJECTION_SCHEMA_FAMILIES, ProjectionSchemaFamily, projection_schema, projection_schemas,
};
pub use timeline::{
    BoundedValue, RunTimelineEntry, RunTimelineFilter, TIMELINE_EVIDENCE_MAX,
    TIMELINE_JSON_MAX_BYTES, TIMELINE_JSON_MAX_DEPTH, TIMELINE_MAX_PAGE_SIZE,
    TIMELINE_TEXT_MAX_BYTES, TimelineDetail, TimelineErrorView, TimelineEvidenceRef, TimelinePage,
    timeline_page,
};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// The projection-protocol version marker, pinned to the JSON number `1`
/// (spine §10.3). Additive evolution keeps the value; breaking changes
/// bump it. Independent of `irVersion` — the projection is a read-side
/// contract and never touches IR or checkpoint semantics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProjectionVersion;

impl ProjectionVersion {
    /// The numeric value of this version marker.
    pub const VALUE: u64 = 1;
}

impl Serialize for ProjectionVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(Self::VALUE)
    }
}

impl<'de> Deserialize<'de> for ProjectionVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u64::deserialize(deserializer)?;
        if value == Self::VALUE {
            Ok(ProjectionVersion)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported projectionVersion {value}: this crate implements projectionVersion {}",
                Self::VALUE
            )))
        }
    }
}

impl JsonSchema for ProjectionVersion {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ProjectionVersion")
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("pointlock_store::projection::ProjectionVersion")
    }
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({ "const": 1 })
    }
}
