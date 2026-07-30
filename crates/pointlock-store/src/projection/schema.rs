//! Projection JSON Schema generation — the same pipeline leg as the IR's
//! `schema_gen` (02 §1.1, spine §10.2): Rust DTOs → JSON Schema →
//! `@pointlock/projection-types` + golden fixtures.
//!
//! Unlike the IR schema, projection schemas do NOT strip the null branch
//! schemars adds for `Option<T>`: the projection deliberately carries
//! BOTH option regimes of the ledger — absent-when-none fields (never
//! emitted as null) and explicit-null fields (`supervisePolicy`,
//! suspension `reason` — the ledger is self-describing there). A schema
//! that accepts null-or-absent covers both regimes; the wire never emits
//! anything the schema rejects.

use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::Value;

use super::{FlowGraphView, HumanInboxEntry, RunOverview, StepDossierView, TimelinePage};

/// One schema family of the projection protocol (closed five — spine
/// §10.1; the timeline family's root is the page envelope, which embeds
/// `RunTimelineEntry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionSchemaFamily {
    /// `FlowGraphView`.
    FlowGraph,
    /// `TimelinePage` (embeds `RunTimelineEntry`).
    RunTimeline,
    /// `StepDossierView`.
    StepDossier,
    /// `HumanInboxEntry`.
    HumanInbox,
    /// `RunOverview`.
    RunOverview,
}

/// The closed family list, in canonical emission order.
pub const PROJECTION_SCHEMA_FAMILIES: [ProjectionSchemaFamily; 5] = [
    ProjectionSchemaFamily::FlowGraph,
    ProjectionSchemaFamily::RunTimeline,
    ProjectionSchemaFamily::StepDossier,
    ProjectionSchemaFamily::HumanInbox,
    ProjectionSchemaFamily::RunOverview,
];

impl ProjectionSchemaFamily {
    /// Kebab-case artifact stem (`<stem>.schema.json`).
    pub fn stem(self) -> &'static str {
        match self {
            ProjectionSchemaFamily::FlowGraph => "flow-graph-view",
            ProjectionSchemaFamily::RunTimeline => "run-timeline",
            ProjectionSchemaFamily::StepDossier => "step-dossier-view",
            ProjectionSchemaFamily::HumanInbox => "human-inbox-entry",
            ProjectionSchemaFamily::RunOverview => "run-overview",
        }
    }

    /// Root type title.
    pub fn title(self) -> &'static str {
        match self {
            ProjectionSchemaFamily::FlowGraph => "FlowGraphView",
            ProjectionSchemaFamily::RunTimeline => "TimelinePage",
            ProjectionSchemaFamily::StepDossier => "StepDossierView",
            ProjectionSchemaFamily::HumanInbox => "HumanInboxEntry",
            ProjectionSchemaFamily::RunOverview => "RunOverview",
        }
    }

    /// The pinned `$id` URN (versioned with `projectionVersion`).
    pub fn schema_id(self) -> String {
        format!("urn:pointlock:schema:projection:v1:{}", self.stem())
    }
}

fn root_schema<T: JsonSchema>(family: ProjectionSchemaFamily) -> Value {
    let settings = SchemaSettings::draft2020_12();
    let mut generator = settings.into_generator();
    let schema = generator.root_schema_for::<T>();
    let mut doc = serde_json::to_value(&schema).expect("schema serializes to JSON");
    if let Value::Object(root) = &mut doc {
        root.insert("$id".to_owned(), Value::String(family.schema_id()));
        root.insert("title".to_owned(), Value::String(family.title().to_owned()));
    }
    doc
}

/// Generates one family's schema.
pub fn projection_schema(family: ProjectionSchemaFamily) -> Value {
    match family {
        ProjectionSchemaFamily::FlowGraph => root_schema::<FlowGraphView>(family),
        ProjectionSchemaFamily::RunTimeline => root_schema::<TimelinePage>(family),
        ProjectionSchemaFamily::StepDossier => root_schema::<StepDossierView>(family),
        ProjectionSchemaFamily::HumanInbox => root_schema::<HumanInboxEntry>(family),
        ProjectionSchemaFamily::RunOverview => root_schema::<RunOverview>(family),
    }
}

/// Generates all five families, in canonical order.
pub fn projection_schemas() -> Vec<(ProjectionSchemaFamily, Value)> {
    PROJECTION_SCHEMA_FAMILIES
        .iter()
        .map(|&family| (family, projection_schema(family)))
        .collect()
}
