//! Verify-chain evaluation of element/visual predicates (spine §6.3,
//! 02 §5.3).
//!
//! One assertion walks its explicit `verifyVia` chain in order:
//! 1. a channel that completes evaluation with the predicate holding →
//!    `pass` (the channel is recorded; a non-preferred channel marks
//!    `degradedVerify`);
//! 2. a channel that completes evaluation with the predicate *not*
//!    holding → `fail`, final — later channels are never consulted (the
//!    degradation chain solves "can't see", not "don't like the answer",
//!    spine R5);
//! 3. a channel that cannot complete (omission / missing material /
//!    `session_degraded`) advances the chain; an exhausted chain is
//!    `unknown` (`onMissingInput` semantics, principle 4).
//!
//! Everything except the vision tail is a pure function over the
//! [`ObserveMaterial`] localized during `observing` (spine §6.2: asserting
//! is pure computation, evidence is already local) — which is exactly what
//! makes the offline re-judge of `judgeDirty` steps (07 §5.3) replay the
//! same mathematics over the same bytes without a session.

use pointlock_ir::{
    AssertionIR, AssertionOutcomeRecord, Channel, ElementSelectorIR, ElementState, EvidenceRef,
    ObservationRecord, PredicateIR, TextMatchIR, TextMatchMode, UiContextKind, VerdictStatus,
    VerifyChannel,
};
use pointlock_store::Store;
use pointlock_vision::{STUB_REASON, VisionRequest, VisionVerifier};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The localized observation material of the step's *after* observation —
/// the sole input of the sync verify channels. `None` bytes always come
/// with a reason (the typed "why this channel cannot complete").
#[derive(Debug, Default)]
pub(crate) struct ObserveMaterial {
    /// Localized normalized-UI-tree bytes (DeviceRail `UiSnapshot` JSON).
    pub ui_tree: Option<Vec<u8>>,
    /// Why `ui_tree` is absent, when it is.
    pub ui_tree_gap: Option<String>,
    /// Localized screenshot bytes plus their media type.
    pub screenshot: Option<(Vec<u8>, String)>,
    /// Why `screenshot` is absent, when it is.
    pub screenshot_gap: Option<String>,
}

impl ObserveMaterial {
    /// Material with every part absent for the same reason (e.g. no after
    /// observation was captured at all).
    pub fn absent(reason: &str) -> Self {
        ObserveMaterial {
            ui_tree: None,
            ui_tree_gap: Some(reason.to_owned()),
            screenshot: None,
            screenshot_gap: Some(reason.to_owned()),
        }
    }
}

/// Rebuilds the verify-chain material of an archived [`ObservationRecord`]
/// from the store's content-addressed evidence area, integrity-checked
/// against the recorded digests. Anything unreadable is a typed gap — the
/// affected channel cannot complete and the chain degrades toward unknown,
/// never a guess (principle 4). Shared by the offline re-judge (07 §5.3)
/// and the assert step's `observe: { fromStep }` source (02 §4.2).
pub(crate) fn material_from_observation(
    store: &Store,
    observation: &ObservationRecord,
) -> ObserveMaterial {
    let mut material = ObserveMaterial::default();
    match &observation.ui_snapshot {
        Some(evidence) => match read_local_evidence(store, evidence) {
            Ok(bytes) => material.ui_tree = Some(bytes),
            Err(reason) => material.ui_tree_gap = Some(reason),
        },
        None => {
            material.ui_tree_gap = Some(match observation.ui_snapshot_omission {
                Some(reason) => format!("uiSnapshot omitted by the provider ({reason:?})"),
                None => "the archived observation carries no uiSnapshot".to_owned(),
            });
        }
    }
    match &observation.screenshot {
        Some(evidence) => match read_local_evidence(store, evidence) {
            Ok(bytes) => {
                material.screenshot = Some((bytes, evidence.asset.media_type.clone()));
            }
            Err(reason) => material.screenshot_gap = Some(reason),
        },
        None => {
            material.screenshot_gap = Some(match observation.screenshot_omission {
                Some(reason) => format!("screenshot omitted by the provider ({reason:?})"),
                None => "the archived observation carries no screenshot".to_owned(),
            });
        }
    }
    material
}

/// Reads a localized evidence entry from the store's content-addressed
/// area and re-verifies its digest (04 §4.3: evidence integrity is
/// non-negotiable — a corrupt or missing file is a reasoned gap, not a
/// silently different judgment basis).
pub(crate) fn read_local_evidence(
    store: &Store,
    evidence: &EvidenceRef,
) -> Result<Vec<u8>, String> {
    let path = store.root().join(&evidence.local_path);
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "localized evidence unreadable at '{}': {error}",
            evidence.local_path
        )
    })?;
    let digest = Sha256::digest(&bytes);
    let digest: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if digest != evidence.sha256 {
        return Err(format!(
            "localized evidence integrity failure: sha256 {digest} != recorded {}",
            evidence.sha256
        ));
    }
    Ok(bytes)
}

/// One evaluated assertion: the durable record plus the `degradedVerify`
/// bit (spine §6.3 rule 1 — a pass answered by a non-preferred channel),
/// which folds into the step verdict's `degraded` flag.
pub(crate) struct EvaluatedAssertion {
    /// The record for `assertionEvaluated`.
    pub record: AssertionOutcomeRecord,
    /// Whether a pass was answered by a non-first chain channel.
    pub degraded_verify: bool,
}

/// What one channel said about the predicate.
enum ChannelEval {
    /// The channel completed evaluation; `holds` is the predicate's truth.
    Completed { holds: bool, reason: String },
    /// The channel could not complete evaluation; advance the chain.
    Incomplete(String),
}

/// Evaluates one element/visual assertion along its verify chain
/// (spine §6.3). `vision == None` is equivalent to the stub verifier:
/// the vision channel cannot complete, with the stub's fixed reason.
pub(crate) async fn eval_observed_assertion(
    assertion: &AssertionIR,
    material: &ObserveMaterial,
    vision: Option<&dyn VisionVerifier>,
) -> EvaluatedAssertion {
    debug_assert!(
        !matches!(assertion.predicate, PredicateIR::Expr { .. }),
        "expr predicates are evaluated by judge::eval_expr_assertion"
    );
    let mut gaps: Vec<String> = Vec::new();
    for (index, channel) in assertion.verify_via.iter().enumerate() {
        let eval = match channel {
            // The dom channel has no offline evaluation material in M1 —
            // there is no web context to capture a DOM from. Typed
            // incompleteness, never a guess; lands with the web provider
            // milestone (M3b, 08 §6).
            VerifyChannel::Dom => {
                ChannelEval::Incomplete("dom offline evaluation not available in M1".to_owned())
            }
            VerifyChannel::UiTree => eval_ui_tree_channel(&assertion.predicate, material),
            VerifyChannel::Vision => eval_vision_channel(assertion, material, vision).await,
        };
        match eval {
            ChannelEval::Completed { holds, reason } => {
                let degraded_verify = holds && index > 0;
                let mut reason = format!("{} channel completed: {reason}", wire_name(*channel));
                if !holds && index + 1 < assertion.verify_via.len() {
                    reason.push_str(
                        "; a completed negative is final — later channels are not consulted",
                    );
                }
                if degraded_verify {
                    reason.push_str("; degradedVerify: a non-preferred channel answered");
                }
                return EvaluatedAssertion {
                    record: AssertionOutcomeRecord {
                        assert_id: assertion.assert_id.clone(),
                        result: if holds {
                            VerdictStatus::Pass
                        } else {
                            VerdictStatus::Fail
                        },
                        channel: Some(as_channel(*channel)),
                        reason,
                    },
                    degraded_verify,
                };
            }
            ChannelEval::Incomplete(reason) => {
                gaps.push(format!("{}: {reason}", wire_name(*channel)));
            }
        }
    }
    // Chain exhausted: no channel completed → unknown (onMissingInput).
    EvaluatedAssertion {
        record: AssertionOutcomeRecord {
            assert_id: assertion.assert_id.clone(),
            result: VerdictStatus::Unknown,
            channel: None,
            reason: format!(
                "verify chain exhausted; no channel completed evaluation (onMissingInput: \
                 unknown) — {}",
                gaps.join("; ")
            ),
        },
        degraded_verify: false,
    }
}

/// The vision tail: hands the author-written prompt and the localized
/// screenshot bytes to the verifier. Missing screenshot / missing verifier
/// → cannot complete. A verifier `unknown` is likewise "cannot complete".
async fn eval_vision_channel(
    assertion: &AssertionIR,
    material: &ObserveMaterial,
    vision: Option<&dyn VisionVerifier>,
) -> ChannelEval {
    let (prompt, region) = match &assertion.predicate {
        PredicateIR::Visual { prompt, region } => (prompt.as_str(), region.as_ref()),
        PredicateIR::ElementState { .. } | PredicateIR::ElementText { .. } => {
            match &assertion.vision_prompt {
                Some(prompt) => (prompt.as_str(), None),
                // Schema-invalid shape (visionPrompt is required iff the
                // chain contains vision); fail-soft to incomplete rather
                // than guess a prompt (principle 6).
                None => {
                    return ChannelEval::Incomplete(
                        "the assertion declares no visionPrompt".to_owned(),
                    );
                }
            }
        }
        PredicateIR::Expr { .. } => {
            unreachable!("expr predicates never enter the verify chain (verifyVia is empty)")
        }
    };
    let Some((bytes, media_type)) = &material.screenshot else {
        return ChannelEval::Incomplete(
            material
                .screenshot_gap
                .clone()
                .unwrap_or_else(|| "no screenshot available".to_owned()),
        );
    };
    let Some(verifier) = vision else {
        // No verifier injected ≡ the stub: unknown with its fixed reason.
        return ChannelEval::Incomplete(STUB_REASON.to_owned());
    };
    let verdict = verifier
        .verify(VisionRequest {
            prompt,
            region,
            screenshot: bytes,
            media_type,
        })
        .await;
    match verdict.status {
        VerdictStatus::Pass => ChannelEval::Completed {
            holds: true,
            reason: verdict.reason,
        },
        VerdictStatus::Fail => ChannelEval::Completed {
            holds: false,
            reason: verdict.reason,
        },
        VerdictStatus::Unknown => ChannelEval::Incomplete(verdict.reason),
    }
}

// ─── uiTree channel: pure evaluation over localized UiSnapshot bytes ───────

/// DeviceRail `UiSnapshot` projection — only the fields the selector and
/// predicates consume. Unknown platform states arrive as `null` and stay
/// `None`; they are never coerced into a truth value (DeviceRail contract:
/// drivers must not manufacture optimistic states — neither may we).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UiSnapshotDoc {
    context: UiContextDoc,
    nodes: Vec<UiNodeDoc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UiContextDoc {
    context_kind: UiContextKind,
    context_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UiNodeDoc {
    stable_node_id: String,
    role: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    identifier: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    hittable: Option<bool>,
}

/// Evaluates an element predicate against the localized UI tree bytes.
fn eval_ui_tree_channel(predicate: &PredicateIR, material: &ObserveMaterial) -> ChannelEval {
    let Some(bytes) = &material.ui_tree else {
        return ChannelEval::Incomplete(
            material
                .ui_tree_gap
                .clone()
                .unwrap_or_else(|| "no uiSnapshot available".to_owned()),
        );
    };
    let doc: UiSnapshotDoc = match serde_json::from_slice(bytes) {
        Ok(doc) => doc,
        Err(error) => {
            return ChannelEval::Incomplete(format!(
                "localized uiSnapshot is not a parseable UiSnapshot document: {error}"
            ));
        }
    };
    let selector = match predicate {
        PredicateIR::ElementState { selector, .. } | PredicateIR::ElementText { selector, .. } => {
            selector
        }
        // A visual predicate is vision-only by schema; on the uiTree
        // channel it is structurally unanswerable.
        PredicateIR::Visual { .. } => {
            return ChannelEval::Incomplete(
                "a visual predicate has no uiTree evaluation".to_owned(),
            );
        }
        PredicateIR::Expr { .. } => {
            unreachable!("expr predicates never enter the verify chain (verifyVia is empty)")
        }
    };
    // css selectors need a DOM; the uiTree channel cannot answer them.
    if selector.css.is_some() {
        return ChannelEval::Incomplete(
            "css selectors are not evaluable on the uiTree channel".to_owned(),
        );
    }
    // Context scoping: a snapshot captured in a different UI context does
    // not see the selector's target — that is "cannot see", not "absent".
    if let Some(context) = &selector.context {
        if context.context_kind != doc.context.context_kind {
            return ChannelEval::Incomplete(format!(
                "selector is scoped to a {:?} context but the snapshot captured a {:?} context",
                context.context_kind, doc.context.context_kind
            ));
        }
        if let Some(context_id) = &context.context_id
            && context_id != &doc.context.context_id
        {
            return ChannelEval::Incomplete(format!(
                "selector is scoped to context '{context_id}' but the snapshot captured \
                 context '{}'",
                doc.context.context_id
            ));
        }
    }
    let matches: Vec<&UiNodeDoc> = doc
        .nodes
        .iter()
        .filter(|node| node_matches(node, selector))
        .collect();
    match predicate {
        PredicateIR::ElementState { state, .. } => eval_element_state(&matches, *state),
        PredicateIR::ElementText { r#match, .. } => eval_element_text(&matches, r#match),
        _ => unreachable!("narrowed above"),
    }
}

/// DeviceRail `ElementSelector` matching semantics: every present selector
/// field must match; a `null` node value matches nothing (unknown is not a
/// wildcard).
fn node_matches(node: &UiNodeDoc, selector: &ElementSelectorIR) -> bool {
    if let Some(role) = &selector.role
        && role != &node.role
    {
        return false;
    }
    if let Some(name) = &selector.name
        && node.name.as_ref() != Some(name)
    {
        return false;
    }
    if let Some(identifier) = &selector.identifier
        && node.identifier.as_ref() != Some(identifier)
    {
        return false;
    }
    if let Some(value) = &selector.value
        && node.value.as_ref() != Some(value)
    {
        return false;
    }
    if let Some(text) = &selector.text {
        match &node.text {
            Some(node_text) => {
                if !text_matches(text, node_text) {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

/// `TextMatchIR` semantics (isomorphic to DeviceRail `TextMatch`).
fn text_matches(matcher: &TextMatchIR, candidate: &str) -> bool {
    let (candidate, expected) = if matcher.case_sensitive {
        (candidate.to_owned(), matcher.value.clone())
    } else {
        (candidate.to_lowercase(), matcher.value.to_lowercase())
    };
    match matcher.mode {
        TextMatchMode::Exact => candidate == expected,
        TextMatchMode::Contains => candidate.contains(&expected),
    }
}

/// `elementState` adjudication. `present`/`absent` are match-count
/// predicates and tolerate multiplicity; `enabled`/`visible` are claims
/// about *one* element — an ambiguous selector is a completed negative
/// (aligned with DeviceRail `element_ambiguous`), and a `null` platform
/// state means the channel cannot complete (never fabricated).
fn eval_element_state(matches: &[&UiNodeDoc], state: ElementState) -> ChannelEval {
    match state {
        ElementState::Present => ChannelEval::Completed {
            holds: !matches.is_empty(),
            reason: format!("{} node(s) match the selector", matches.len()),
        },
        ElementState::Absent => ChannelEval::Completed {
            holds: matches.is_empty(),
            reason: format!("{} node(s) match the selector", matches.len()),
        },
        ElementState::Enabled => unique_state(matches, "enabled", |node| node.enabled),
        ElementState::Visible => unique_state(matches, "hittable", |node| node.hittable),
    }
}

/// The unique-match state check shared by `enabled` (node.enabled) and
/// `visible` (node.hittable).
fn unique_state(
    matches: &[&UiNodeDoc],
    field: &str,
    read: impl Fn(&UiNodeDoc) -> Option<bool>,
) -> ChannelEval {
    match matches {
        [] => ChannelEval::Completed {
            holds: false,
            reason: "no node matches the selector".to_owned(),
        },
        [node] => match read(node) {
            Some(holds) => ChannelEval::Completed {
                holds,
                reason: format!(
                    "the matched node '{}' reports {field}={holds}",
                    node.stable_node_id
                ),
            },
            None => ChannelEval::Incomplete(format!(
                "the matched node '{}' reports {field}=null (unknown platform state is never \
                 coerced)",
                node.stable_node_id
            )),
        },
        many => ChannelEval::Completed {
            holds: false,
            reason: format!(
                "ambiguous selector: {} nodes match (element_ambiguous)",
                many.len()
            ),
        },
    }
}

/// `elementText` adjudication: unique match required; the compared string
/// is the node's `text`, falling back to its accessible `name`; both
/// `null` → the channel cannot complete.
fn eval_element_text(matches: &[&UiNodeDoc], matcher: &TextMatchIR) -> ChannelEval {
    match matches {
        [] => ChannelEval::Completed {
            holds: false,
            reason: "no node matches the selector".to_owned(),
        },
        [node] => {
            let candidate = node.text.as_deref().or(node.name.as_deref());
            match candidate {
                Some(actual) => ChannelEval::Completed {
                    holds: text_matches(matcher, actual),
                    reason: format!(
                        "the matched node '{}' has text {actual:?}; expected {:?} (mode: {}, \
                         caseSensitive: {})",
                        node.stable_node_id,
                        matcher.value,
                        match matcher.mode {
                            TextMatchMode::Exact => "exact",
                            TextMatchMode::Contains => "contains",
                        },
                        matcher.case_sensitive
                    ),
                },
                None => ChannelEval::Incomplete(format!(
                    "the matched node '{}' reports text=null and name=null (unknown platform \
                     state is never coerced)",
                    node.stable_node_id
                )),
            }
        }
        many => ChannelEval::Completed {
            holds: false,
            reason: format!(
                "ambiguous selector: {} nodes match (element_ambiguous)",
                many.len()
            ),
        },
    }
}

/// The wire literal of a verify channel (reason strings).
fn wire_name(channel: VerifyChannel) -> &'static str {
    match channel {
        VerifyChannel::Dom => "dom",
        VerifyChannel::UiTree => "uiTree",
        VerifyChannel::Vision => "vision",
    }
}

/// Projects the verify-channel subset onto the full channel vocabulary
/// (the `AssertionOutcomeRecord.channel` carrier).
fn as_channel(channel: VerifyChannel) -> Channel {
    match channel {
        VerifyChannel::Dom => Channel::Dom,
        VerifyChannel::UiTree => Channel::UiTree,
        VerifyChannel::Vision => Channel::Vision,
    }
}
