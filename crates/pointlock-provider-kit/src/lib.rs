//! # pointlock-provider-kit
//!
//! The Provider SPI of Pointlock (spine §4; 04 §1–§8): the authoritative
//! in-process Rust trait form of `Provider` / `ProviderSession` (a stdio
//! JSON-RPC sidecar adapter form is reserved for v0.2), the static
//! capability declaration types (`ProviderManifest`, `CapabilityLockfile`,
//! `CapabilityAttestation`), the unified [`ProviderError`] carrier, the
//! deterministic programmable [`FakeProvider`], and the provider
//! [`conformance`] suite every implementation must pass before the CLI
//! assembles it.
//!
//! Authoritative design documents:
//! - `docs/design/00-architecture-spine.md` §4 (SPI TS signatures, the
//!   canonical notation this crate renders into Rust), §5 (error taxonomy)
//! - `docs/design/04-provider-interface-and-devicerail.md` §1–§8 (contract
//!   clauses, error normalization, timeout/cancellation, conformance)
//!
//! Shared runtime DTOs (`ActionOutcome`, `ActionResult`, `ReconcileResult`,
//! `ErrorInfo`, `Observation`, `AssetRef`, …) are defined in `pointlock-ir`
//! (type truth source, R12) and re-exported here for one-stop SPI imports.
//!
//! ## Serde discipline
//!
//! Wire shape is camelCase (`rename_all`) with closed objects
//! (`deny_unknown_fields`); type, field and enum-literal spellings follow
//! spine Appendix A verbatim (error classes stay snake_case by decree).
//! M0 iron law: the public contract *shapes* here are final; contents may
//! still be narrow (each narrowing carries a pending-incorporation note).

pub mod conformance;
pub mod error;
pub mod fake;
pub mod lockfile;
pub mod manifest;
pub mod spi;

pub use conformance::{
    CheckResult, CheckStatus, ConformanceOptions, ConformanceReport, run_conformance,
};
pub use error::{ProviderError, RetryableSource};
pub use fake::{FakeHandle, FakeProvider, FakeProviderSession, ScriptedOutcome};
pub use lockfile::{
    CapabilityAttestation, CapabilityLockfile, LOCKFILE_DIGEST_DOMAIN_TAG, LockfileDevice,
    LockfileHello, LockfileProvider, PeerInfo, ProtocolVersion, lockfile_digest,
};
pub use manifest::{
    ActionDefinitionStatic, ActionProtection, ChannelRole, ChannelSupport, ConditionalFeature,
    FeatureDeclarations, PlatformKind, ProtocolRange, ProviderManifest, VerbBinding,
};
pub use spi::{
    BoundActionCall, CancellationToken, EvidenceStream, ObserveRequest, ObserveWant,
    OpenSessionOptions, Provider, ProviderSession, SessionHealth, SessionOutcome,
    UiSnapshotOutcome, VERDICT_EVIDENCE_MAX_ENTRIES, VERDICT_SUMMARY_MAX_CHARS, VerdictWrite,
    now_ms, observation_projection, synthetic_observation_wants,
};

// Shared runtime wire types (defined in `pointlock-ir`, spine R12).
pub use pointlock_ir::{
    ActionExecution, ActionOutcome, ActionResult, AssetRef, ErrorClass, ErrorInfo, EventCursor,
    Observation, ReconcileResult, UiContextRef, UiNodeRef, UiSnapshotRef, Verdict, Viewport,
};
