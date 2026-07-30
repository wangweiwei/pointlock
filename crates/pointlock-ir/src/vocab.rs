//! Closed vocabulary enums (spine Appendix A.4).
//!
//! Every enum here is a closed set: adding, removing or renaming a value is a
//! semantic change of the IR and requires an `irVersion` bump (02 §11).
//! Wire literals are aligned verbatim with Appendix A.4 via serde renames.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Effect classification of a step (full set, spine A.4).
///
/// Note: the FlowIR schema restricts action steps to [`EffectClassAction`]
/// (`pure` never crosses a Provider — pure computation belongs to `let`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum EffectClass {
    /// Changes the world; replays are only safe when declared idempotent.
    Mutating,
    /// Probes the world without changing it; always safe to replay.
    Readonly,
    /// Pure computation; never crosses a Provider.
    Pure,
}

/// `EffectClass` restricted for action steps; `pure` is excluded because pure
/// computation never crosses a Provider.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum EffectClassAction {
    /// Changes the world.
    Mutating,
    /// Probes the world without changing it.
    Readonly,
}

/// Locating/verification channel (full set, spine A.4).
///
/// `vision` is verify-only and `coordinate` is act-only; the FlowIR schema
/// encodes those restrictions structurally via [`ActChannel`] and
/// [`VerifyChannel`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum Channel {
    /// Web DOM channel.
    Dom,
    /// Native UI-tree channel (requires `observation.uiSnapshot.v1`).
    UiTree,
    /// Vision channel (verify-only, principle 7).
    Vision,
    /// Static-coordinate channel (act-only).
    Coordinate,
}

/// Channel subset legal on the act-chain. `vision` is structurally excluded
/// (principle 7: vision never locates or acts).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ActChannel {
    /// Web DOM channel.
    Dom,
    /// Native UI-tree channel.
    UiTree,
    /// Static-coordinate channel (must carry literal coordinates, bind-phase check).
    Coordinate,
}

/// Channel subset legal on the verify-chain. `coordinate` is structurally
/// excluded (a coordinate cannot verify anything).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum VerifyChannel {
    /// Web DOM channel.
    Dom,
    /// Native UI-tree channel.
    UiTree,
    /// Vision channel — only legal at the chain tail (bind-phase check).
    Vision,
}

/// Actual execution mode reported by the DeviceRail daemon; the whitelist
/// semantics live on [`crate::BoundAttempt::accept_execution_modes`]
/// (spine §6.4 R-degrade).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionMode {
    /// Native semantic execution.
    NativeSemantic,
    /// Web semantic execution.
    WebSemantic,
    /// Daemon-internal coordinate fallback (must be whitelisted per attempt).
    CoordinateFallback,
}

/// Pointlock-layer closed error taxonomy (spine §5). DeviceRail
/// `ErrorInfo.code` is an open string set mapped onto this enum.
///
/// snake_case on the wire, aligned with DeviceRail error-code style.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Attestation does not match `lockfileDigest`; refuse to start/resume.
    CapabilityDrift,
    /// Runtime arguments failed the action inputSchema — a compiler/expression bug signal.
    BindArgumentsInvalid,
    /// Action failed with `retryable: true` as reported by the daemon.
    ActionFailedRetryable,
    /// Action failed with `retryable: false`; try the next attempt or fail the step.
    ActionFailedFinal,
    /// Action timed out; auto-retry only when the step is declared idempotent.
    ActionTimedOut,
    /// Action cancelled (user cancellation → run aborted).
    ActionCancelled,
    /// `UiNodeRef` documentEpoch invalid or locate miss; re-observe before retry.
    TargetStale,
    /// Transport lost / daemon exited; suspend and resume from checkpoint.
    TransportLost,
    /// Session degraded; current step yields unknown, flow-level onError fires.
    SessionDegraded,
}

/// Canonical verbs — report/metadata only. The runner has no verb switch;
/// execution is driven exclusively by `BoundAttempt.actionName` (spine R7).
///
/// snake_case on the wire (spine A.4).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalVerb {
    /// Tap an element.
    Tap,
    /// Set an element's value.
    SetValue,
    /// Clear an element.
    Clear,
    /// Wait for an element condition.
    WaitFor,
    /// Find an element.
    Find,
    /// Explicit observation.
    Observe,
    /// Screenshot capture.
    Screenshot,
    /// Escape hatch for provider/driver-specific actions.
    Invoke,
}

/// Flow-level verdict folding policy. `strict` folds a degraded pass into
/// `unknown` (spine §6.3).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum VerdictPolicy {
    /// Degraded passes remain passes (flagged `degraded`).
    Standard,
    /// Degraded passes fold to `unknown`.
    Strict,
}

/// Interaction mode of a human step (principle 8).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum HumanMode {
    /// Human confirms/rejects; the decision is recorded.
    Confirm,
    /// Human produces the step verdict.
    Judge,
    /// Human provides typed input (requires `outputSchema`).
    ProvideInput,
    /// Human repairs the world; yields a disposition, never a verdict or output.
    RepairWorld,
}

/// Handler hook points (spine A.4). Handlers never appear in normal control
/// flow; they fire on specific state-machine transitions.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum HandlerHook {
    /// Fires when the step verdict folds to fail.
    OnFail,
    /// Fires when the step verdict folds to unknown.
    OnUnknown,
    /// Fires on an [`ErrorClass`] (optionally filtered via `errorClasses`).
    OnError,
    /// Fires when resume preflight probes detect world drift.
    OnResumeDrift,
}

/// Element state predicate values — verbatim equal to DeviceRail
/// `WaitForElementCondition`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ElementState {
    /// Element exists in the UI tree.
    Present,
    /// Element is visible.
    Visible,
    /// Element is enabled.
    Enabled,
    /// Element is absent.
    Absent,
}

/// Text match mode — isomorphic to DeviceRail `TextMatchMode`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum TextMatchMode {
    /// Exact match.
    Exact,
    /// Substring match.
    Contains,
}

/// UI context kind — isomorphic to DeviceRail `UiContextKind`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum UiContextKind {
    /// Native UI context.
    Native,
    /// Web UI context.
    Web,
}

/// Which observation of the source action step an assert step reuses.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ObservationWhich {
    /// The `ActionResult.after` observation.
    After,
    /// The `ActionResult.before` observation.
    Before,
}

/// Step pipeline phase (spine A.4, used by `PathFrame::Phase`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// Pre-entry world probe.
    Preflight,
    /// Action execution.
    Act,
    /// Observation capture.
    Observe,
    /// Assertion evaluation.
    Assert,
}

/// Step lifecycle state (spine §6.2/A.4, closed 14-value set).
///
/// `awaitingHuman` doubles as the supervision-gate wait state (R13):
/// the exit path is discriminated by the pending request's `purpose`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum StepState {
    /// Not yet reached.
    Pending,
    /// Inputs resolved and snapshotted; ready to act.
    Ready,
    /// Preflight probes running.
    Probing,
    /// Action dispatched (entered only after the actionIntent WAL fsync).
    Acting,
    /// Awaiting the four-way terminal outcome.
    Settling,
    /// Capturing/localizing observations and evidence.
    Observing,
    /// Pure assertion evaluation.
    Asserting,
    /// Terminal: verdict folded (or no-verdict for unasserted mutations).
    Judged,
    /// Terminal: an `if` branch not taken.
    Skipped,
    /// Terminal: upstream failure with halt policy.
    Blocked,
    /// Resume probe failed; awaiting onResumeDrift disposition.
    Drifted,
    /// Suspended awaiting a human response (step or supervision purpose).
    AwaitingHuman,
    /// Run suspended while this step was in flight.
    Suspended,
    /// Terminal: aborted by handler or user decision.
    Aborted,
}

/// Alignment classification of an old step record against the new IR
/// (spine §6.7-A, closed five-value set).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum AlignmentClass {
    /// Same effect and judge hash: record adopted as-is.
    Reusable,
    /// Judge hash changed only: offline re-judgement over archived evidence.
    JudgeDirty,
    /// Effect hash changed: record and its data-dependent downstream invalid.
    EffectDirty,
    /// Present in the new IR only; resume point must not pass it.
    New,
    /// Present in the old record only; archived, never adopted.
    Orphaned,
}

/// Why a human interaction exists (R13, spine §6.1/A.4).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum HumanPurpose {
    /// A `human` step declared in the IR.
    Step,
    /// A supervised-run gate before a mutating dispatch.
    Supervision,
}

/// Supervised-run policy (R13, spine §6.9/A.4). Recorded per segment in
/// `runStarted`/`runResumed` payloads; never enters any hash domain.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum SupervisePolicy {
    /// Gate every mutating step.
    Mutating,
    /// Gate every action step.
    All,
}

/// Human decision at a supervision gate (R13, spine §6.9/A.4; deliberately
/// no `skip` — skipping a mutating step breaks data dependencies, changes
/// go through the repair path instead).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum SupervisionDecision {
    /// Write the actionIntent and dispatch.
    Proceed,
    /// Abort the run (no handler consulted — the human ruling is final).
    Abort,
    /// Suspend the run; the request stays pending across resume.
    Suspend,
}

/// Reason DeviceRail fell back to coordinate execution inside the daemon
/// (transparent passthrough of `CoordinateFallbackReason`, spine A.8).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum CoordinateFallbackReason {
    /// The semantic interaction channel is unavailable for the target.
    SemanticInteractionUnavailable,
    /// The platform cannot express the semantic interaction.
    PlatformLimitation,
}

/// Three-valued verdict status (spine A.4; `unknown` is never optimistically
/// folded to `pass`, principle 4).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum VerdictStatus {
    /// The assertion/step is confirmed to hold.
    Pass,
    /// The assertion/step is confirmed not to hold.
    Fail,
    /// Could not be confirmed either way.
    Unknown,
}

/// Reason a screenshot was legitimately omitted from an observation
/// (DeviceRail `ScreenshotOmissionReason`, spine A.8; omission is data,
/// not an error — it degrades the verify chain toward `unknown`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ScreenshotOmissionReason {
    /// Omitted by daemon policy.
    Policy,
    /// Omitted because a protected action was in flight.
    ProtectedAction,
}

/// Reason a UI snapshot was legitimately omitted from an observation
/// (DeviceRail `UiSnapshotOmissionReason`, spine A.8).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum UiSnapshotOmissionReason {
    /// The driver has no semantic UI channel.
    DriverUnsupported,
    /// Omitted by daemon policy.
    Policy,
    /// Omitted because a protected action was in flight.
    ProtectedAction,
}
