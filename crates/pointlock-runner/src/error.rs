//! Typed runner errors and blocked reasons.
//!
//! M0 iron rule: the error *shapes* are final API surface — content may be
//! narrow, but nothing outside the M0 subset is silently accepted or
//! silently skipped; it is refused with a typed error.

use std::fmt;

use pointlock_ir::{AlignmentReport, Hash, StepId};
use pointlock_provider_kit::ProviderError;
use pointlock_store::StoreError;

/// Why a run is blocked awaiting a human decision (spine §6.7-B: the
/// uncertain reconcile branch defaults to escalation; the `onResumeDrift` /
/// human pipeline itself is not implemented in M0 — the shape is final).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockedReason {
    /// The fate of a pending mutating intent could not be established
    /// (`logUnavailable` / `startedNoTerminal`) and the step is neither
    /// `readonly` nor declared `idempotent` — replay is forbidden (I2) and
    /// adjudication requires a human (`repairWorld`, not in M0).
    RequiresHuman {
        /// The unresolved action intent's callId.
        call_id: String,
        /// Human-readable explanation of why a human is required.
        detail: String,
    },
    /// A preflight probe found the world drifted (or unverifiable — an
    /// exhausted probe chain is treated as drift, 07 §4.2 rule 2) and no
    /// `onResumeDrift` disposition remained: none is declared, or the
    /// declared ladder ran out (`maxTriggers`) without the world coming
    /// back. The run records `runSuspended` and blocks for the operator.
    Drifted {
        /// The step whose preflight did not hold.
        step_id: String,
        /// Which probe failed and why.
        detail: String,
    },
}

impl fmt::Display for BlockedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockedReason::RequiresHuman { call_id, detail } => {
                write!(
                    f,
                    "requires human adjudication (callId {call_id}): {detail}"
                )
            }
            BlockedReason::Drifted { step_id, detail } => {
                write!(
                    f,
                    "drifted: preflight of step '{step_id}' did not hold: {detail} \
                     (no onResumeDrift disposition remained — declare one, or repair \
                     the world and resume)"
                )
            }
        }
    }
}

/// Runner-level typed error (thiserror). Step-level failures are *not*
/// errors — they fold into verdicts and the run finishes; this enum covers
/// refusals (load checks, capability drift, M0 subset, resume gates) and
/// infrastructure failures (store, provider).
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// The FlowIR's stored `irHash` does not match recomputation
    /// (load check, spine §1.2: the runner recomputes and compares).
    #[error("irHash mismatch: declared {declared}, computed {computed}")]
    IrHashMismatch {
        /// The hash stored on the FlowIR.
        declared: Hash,
        /// The hash recomputed from the FlowIR content.
        computed: Hash,
    },

    /// A step's stored `effectHash`/`judgeHash` does not match
    /// recomputation (the artifact is self-checkable, 02 §12.2).
    #[error("step '{step_id}': stored {domain}Hash {declared} != computed {computed}")]
    StepHashMismatch {
        /// The offending step.
        step_id: StepId,
        /// `"effect"` or `"judge"`.
        domain: &'static str,
        /// The hash stored on the step.
        declared: Hash,
        /// The hash recomputed from the step content.
        computed: Hash,
    },

    /// The IR uses a construct outside the current execution subset —
    /// fail-closed, never silently skipped. The vocabulary that remains
    /// outside: the re-invocation dispositions on call/human hosts
    /// (07 §1's attempt-framed re-call and the fresh-request re-ask),
    /// which the load gate keeps as typed refusals.
    #[error("not in the M0 subset: {construct} (step: {step_id:?})")]
    NotInM0Subset {
        /// The offending step, when the construct is step-scoped.
        step_id: Option<StepId>,
        /// The refused construct, human-readable.
        construct: String,
    },

    /// A human step's declared shape is inconsistent (runtime defense line
    /// against hand-built IR; the compiler check phase refuses these
    /// first): `confirm` without exactly two decision labels, `judge`
    /// decisions outside the three-valued vocabulary, or `provideInput`
    /// without an `outputSchema` (06 §2.2).
    #[error("human step '{step_id}' is invalid: {reason}")]
    InvalidHumanStep {
        /// The offending step.
        step_id: StepId,
        /// What exactly is wrong.
        reason: String,
    },

    /// The static call closure exceeds `maxCallDepth` (07 §1.3: 8 frames
    /// including the root; the compiler already refuses this — the load
    /// check is the runtime defense against hand-built IR).
    #[error("call depth {depth} exceeds maxCallDepth {max} (07 §1.3)")]
    CallDepthExceeded {
        /// The offending static depth (frames, root included).
        depth: usize,
        /// The pinned maximum.
        max: usize,
    },

    /// The subflow registry handed to the runner does not close over the
    /// IR's `subflows` pins (a call target is missing, a hash key does not
    /// self-verify, or a pin disagrees with the flow's own table).
    #[error("subflow registry: {detail}")]
    SubflowRegistry {
        /// What exactly is wrong, human-readable.
        detail: String,
    },

    /// The session attestation does not match the IR's `lockfileDigest`
    /// (spine §4.1/§5 `capability_drift`): refuse to run or resume, never
    /// silently degrade.
    #[error(
        "capability drift: IR lockfileDigest {expected} != session attestation {attested}; \
         refusing to run"
    )]
    CapabilityDrift {
        /// The digest the IR was bound against.
        expected: Hash,
        /// The digest the live session attested.
        attested: Hash,
    },

    /// The run params are not usable (not an object, or a required param
    /// without default is missing).
    #[error("invalid params: {reason}")]
    InvalidParams {
        /// What is wrong with the params.
        reason: String,
    },

    /// The supplied old FlowIR is not the IR the run executed.
    #[error(
        "old FlowIR mismatch: the run executed irHash {expected}, supplied IR computes {computed}"
    )]
    OldIrMismatch {
        /// The irHash recorded in the checkpoint.
        expected: Hash,
        /// The irHash recomputed from the supplied old IR.
        computed: Hash,
    },

    /// Re-execution of already-effective mutating steps requires explicit
    /// human authorization (07 §5.4 unified gate). Fails closed; the
    /// author releases entries by naming them in
    /// `ResumeOptions::allow_mutating_reexec` (the CLI's repeatable
    /// `--allow-mutating-reexec <stepId>`). The report carries whatever
    /// remains gated.
    #[error(
        "resume requires explicit confirmation for {} mutating step(s) (07 §5.4); \
         authorize each with --allow-mutating-reexec <stepId>",
        report.requires_confirmation.len()
    )]
    RequiresConfirmation {
        /// The alignment report whose `requiresConfirmation` entries name
        /// the gated steps.
        report: Box<AlignmentReport>,
    },

    /// A combination that is valid in the design but deliberately not
    /// implemented in M0 (each site documents the pending incorporation).
    #[error("not supported in M0: {detail}")]
    M0Unsupported {
        /// What exactly is unsupported.
        detail: String,
    },

    /// Localized evidence bytes do not match the provider-declared sha256
    /// (04 §4.3: evidence integrity is non-negotiable).
    #[error(
        "evidence integrity failure for asset {asset_id}: sha256 {actual} != declared {expected}"
    )]
    EvidenceIntegrity {
        /// The provider asset id.
        asset_id: String,
        /// The sha256 the provider declared.
        expected: String,
        /// The sha256 of the bytes actually fetched.
        actual: String,
    },

    /// Store-layer failure (SQLite / fold / IO).
    #[error(transparent)]
    Store(#[from] StoreError),

    /// Provider-layer failure outside an action terminal (e.g. reconcile
    /// or verdict write-back failed).
    #[error(transparent)]
    Provider(#[from] ProviderError),
}
