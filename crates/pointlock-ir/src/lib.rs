//! # pointlock-ir
//!
//! All Typed IR types of Pointlock as Rust DTOs (serde + schemars) — the
//! **single source of truth for types** (spine R12). CI generates from this
//! crate: the JSON Schema (Draft 2020-12), the type-only npm package
//! `@pointlock/ir-types`, and golden fixtures.
//!
//! Authoritative design documents:
//! - `docs/design/00-architecture-spine.md` §3/§7/§9 and Appendix A
//! - `docs/design/02-typed-ir.md` (the anchor document for IR semantics)
//! - `schema/flow-ir.v0.1.schema.json` (acceptance baseline for the wire
//!   shape; the generated schema must be *behaviorally equivalent* to it
//!   over the golden fixture corpus, 02 §1.1)
//!
//! ## Serde discipline
//!
//! - Wire shape is camelCase (`rename_all`); enum literals align verbatim
//!   with spine Appendix A.4 (error classes and canonical verbs are
//!   snake_case by decree).
//! - Objects the baseline closes with `additionalProperties: false` carry
//!   `#[serde(deny_unknown_fields)]`. The seven step variants compose
//!   [`StepBase`] via `#[serde(flatten)]`, which serde cannot combine with
//!   `deny_unknown_fields`; there the schema-only
//!   `#[schemars(deny_unknown_fields)]` closes the *generated schema*
//!   (mirroring the baseline's `unevaluatedProperties: false`) while the
//!   serde loader stays lenient — see the note in [`step`].
//! - Absent optional fields are omitted (`skip_serializing_if`), never
//!   `null` — the single-representation rule that content addressing
//!   depends on (02 §2.4/§12.1).
//!
//! The golden fixture corpus lives in `schema/fixtures/` (positive +
//! negative), and `tests/behavioral_equivalence.rs` judges the generated
//! schema against the acceptance baseline over it (accept/reject parity,
//! 02 §1.1) plus the serde round-trip guarantee. Normalization and hashing
//! (irHash/effectHash/judgeHash) live in [`canonical`] and [`hash`]; the
//! canonical RunPath string rendering and parsing (07 §2.1) live in
//! [`run_path`].

pub mod assertion;
pub mod canonical;
pub mod expr;
pub mod flow;
pub mod handler;
pub mod hash;
pub mod primitives;
pub mod record;
pub mod run_log;
pub mod run_path;
pub mod runtime;
pub mod schema_gen;
pub mod selector;
pub mod source_map;
pub mod step;
pub mod vocab;

pub use assertion::{AssertionIR, PredicateIR};
pub use canonical::to_canonical_json;
pub use expr::{Expr, ExprMap, FnExpr, LitExpr, PureFn, RefExpr};
pub use flow::{FlowIR, FlowRef, OutputDecl, ParamDecl, ProviderRef};
pub use handler::{BackoffMs, BackoffSchedule, HandlerAction, HandlerBinding, RetryPolicy};
pub use hash::{
    domain_hash, effect_hash, effect_hash_domain_tag, ir_hash, ir_hash_domain_tag, judge_hash,
    judge_hash_domain_tag, judge_subdomain_sans_preflight,
};
pub use primitives::{
    ActionName, AssertId, FeatureId, FlowId, Hash, Identifier, IrVersion, JsonPointer,
    JsonSchemaDocument, OnMissingInput, OnTimeout, Protection, ProviderName, RefPath, StepId,
    ValidationError,
};
pub use record::{
    ActionOutcomeKind, AlignmentEntry, AlignmentReport, AssertionOutcomeRecord, AttemptRecord,
    AttestationSnapshot, BindingState, CallFrame, CheckpointView, EventCursor, Frontier,
    HumanPending, IterState, ObservationRecord, PendingIntent, ProviderStateSummary,
    RequiresConfirmation, SessionHealthSnapshot, StepRecord,
};
pub use run_log::{RunLogEvent, RunLogPayload};
pub use run_path::{
    ParsedPathFrame, PathFrame, RunPath, RunPathParseError, parse_run_path, render_parsed_run_path,
    render_run_path,
};
pub use runtime::{
    ActionExecution, ActionOutcome, ActionResult, AssetRef, ErrorInfo, EvidenceGap, EvidenceRef,
    Observation, ReconcileResult, StepVerdict, UiContextRef, UiNodeRef, UiSnapshotRef, Verdict,
    Viewport,
};
pub use selector::{ElementSelectorIR, RectIR, SelectorContext, TextMatchIR};
pub use source_map::{MacroOriginFrame, SourceMapEntry, SourceSpan};
pub use step::{
    ActionBinding, ActionStepIR, AssertStepIR, BoundAttempt, CallStepIR, ForeachStepIR,
    HumanStepIR, IfStepIR, LetStepIR, ObservationFromStep, ObservationSource, StepBase, StepIR,
};
pub use vocab::{
    ActChannel, AlignmentClass, CanonicalVerb, Channel, CoordinateFallbackReason, EffectClass,
    EffectClassAction, ElementState, ErrorClass, ExecutionMode, HandlerHook, HumanMode,
    HumanPurpose, ObservationWhich, Phase, ScreenshotOmissionReason, StepState, SupervisePolicy,
    SupervisionDecision, TextMatchMode, UiContextKind, UiSnapshotOmissionReason, VerdictPolicy,
    VerdictStatus, VerifyChannel,
};
