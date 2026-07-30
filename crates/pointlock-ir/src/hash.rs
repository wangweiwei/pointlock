//! Content addressing: `irHash` / `effectHash` / `judgeHash` (02 §12,
//! spine §3 dual-hash constitutional clause).
//!
//! All three hashes share one construction (02 §12.2):
//!
//! ```text
//! sha256( utf8( domainTag + "\n" + JCS(subtree) ) )   →   "sha256:<hex>"
//! ```
//!
//! where `JCS(subtree)` is [`crate::canonical::to_canonical_json`] and the
//! domain tag both separates hash domains (no cross-domain collisions) and
//! carries the `irVersion` (02 §11: a version bump changes every hash, so
//! cross-generation resume alignment naturally reports everything
//! `effectDirty`):
//!
//! | hash | domain tag | input subtree |
//! |---|---|---|
//! | `irHash` | `pointlock-ir/1/irHash` | whole `FlowIR` minus `irHash` (self-reference) and `sourceMap` (pure diagnostics) |
//! | `effectHash` | `pointlock-ir/1/effectHash/<kind>` | the step's "what it does to the world" field domain (02 §12.3) |
//! | `judgeHash` | `pointlock-ir/1/judgeHash/<kind>` | the step's "how it is judged" field domain (02 §12.3) |
//!
//! Per-step `effectHash`/`judgeHash` stay inside the `irHash` input (they
//! are deterministic derived values; keeping them makes a single file
//! self-checkable — `pointlock inspect` recomputes and compares). `irHash`
//! covers every callee transitively through the `subflows.*.irHash` pins —
//! the link-closure property (02 §6).
//!
//! ## The per-kind dual-hash domain table (02 §12.3, transcribed verbatim)
//!
//! | kind | effectHash domain | judgeHash domain |
//! |---|---|---|
//! | `action` | `kind` `effect` `idempotent` `binding` `outputs` `outputSchema` | `preflight` `assertions` |
//! | `assert` | `kind` | `preflight` `observe` `assertions` |
//! | `call` | `kind` `flowRef` `inputs` | `preflight` |
//! | `human` | `kind` `mode` `prompt` `presents` `decisions` `outputSchema` | `preflight` |
//! | `if` | `kind` `cond` | `preflight` |
//! | `foreach` | `kind` `items` `as` | `preflight` |
//! | `let` | `kind` `bindings` | `preflight` |
//!
//! Adjudications this table encodes (02 §12.3, items 1–6):
//!
//! 1. `stepId` enters neither hash — identity vs. content, the pivot of
//!    resume alignment.
//! 2. `retry` / `timeoutMs` / `checkpoint` / `handlers` / `verb` enter
//!    neither hash: budgets, checkpoint granularity, error strategy and
//!    report labels do not touch the validity of an already-recorded
//!    execution. They still enter `irHash`.
//! 3. `outputs` / `outputSchema` sit in `effectHash` (conservative ruling:
//!    the output projection is the step's data contract downstream
//!    `resolvedInputs` depend on; offline re-projection is deferred to
//!    v0.2).
//! 4. A human step's entire question domain (`mode` `prompt` `presents`
//!    `decisions` `outputSchema`) is in `effectHash`: an answer binds to
//!    the question asked, so any question change must re-ask. Its
//!    `judgeHash` domain is `preflight` only. (`onTimeout` is const
//!    `"unknown"` in v0.1 and `timeoutMs` is a budget per item 2.)
//! 5. Container steps (`if`/`foreach`) hash WITHOUT their subtrees
//!    (`then`/`else`/`body` are absent from both domains): child steps
//!    carry their own identity and hashes, the container answers only for
//!    its control decision (`cond` / `items`+`as`).
//! 6. `preflight` is the sole non-assertion member of every judge domain;
//!    the aligner's preflight-only `judgeDirty` reuse rule (02 §12.3 item
//!    6) relies on comparing exactly that subdomain.
//!
//! Absence-by-omission carries through: an optional domain field that is
//! absent from the step is absent from the subtree (never `null`), matching
//! canonical-form rule 3.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::canonical::to_canonical_json;
use crate::flow::FlowIR;
use crate::primitives::{Hash, IrVersion};
use crate::step::StepIR;

/// The domain tag of `irHash` for this crate's `irVersion`:
/// `pointlock-ir/1/irHash`.
pub fn ir_hash_domain_tag() -> String {
    format!("pointlock-ir/{}/irHash", IrVersion::VALUE)
}

/// The domain tag of `effectHash` for a step kind:
/// `pointlock-ir/1/effectHash/<kind>`.
pub fn effect_hash_domain_tag(kind: &str) -> String {
    format!("pointlock-ir/{}/effectHash/{kind}", IrVersion::VALUE)
}

/// The domain tag of `judgeHash` for a step kind:
/// `pointlock-ir/1/judgeHash/<kind>`.
pub fn judge_hash_domain_tag(kind: &str) -> String {
    format!("pointlock-ir/{}/judgeHash/{kind}", IrVersion::VALUE)
}

/// The generic hash construction of 02 §12.2:
/// `sha256(utf8(domainTag + "\n" + JCS(subtree)))`, rendered as
/// `"sha256:<64 lowercase hex>"`.
pub fn domain_hash(domain_tag: &str, subtree: &Value) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update(domain_tag.as_bytes());
    hasher.update(b"\n");
    hasher.update(to_canonical_json(subtree).as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Hash::new(format!("sha256:{hex}")).expect("a sha256 hex digest is always grammatical")
}

/// The wire-field allowlist of a step kind's `effectHash` domain (02 §12.3).
fn effect_domain_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "action" => &[
            "kind",
            "effect",
            "idempotent",
            "binding",
            "outputs",
            "outputSchema",
        ],
        "assert" => &["kind"],
        "call" => &["kind", "flowRef", "inputs"],
        "human" => &[
            "kind",
            "mode",
            "prompt",
            "presents",
            "decisions",
            "outputSchema",
        ],
        "if" => &["kind", "cond"],
        "foreach" => &["kind", "items", "as"],
        "let" => &["kind", "bindings"],
        other => unreachable!("StepIR::kind() is a closed 7-value set, got {other:?}"),
    }
}

/// The wire-field allowlist of a step kind's `judgeHash` domain (02 §12.3).
fn judge_domain_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "action" => &["preflight", "assertions"],
        "assert" => &["preflight", "observe", "assertions"],
        "call" | "human" | "if" | "foreach" | "let" => &["preflight"],
        other => unreachable!("StepIR::kind() is a closed 7-value set, got {other:?}"),
    }
}

/// Projects a step's wire form onto a field allowlist. Fields absent from
/// the step (unset optionals) stay absent from the subtree — the
/// absence-by-omission rule carries into the hash input.
fn step_domain_subtree(step: &StepIR, fields: &[&str]) -> Value {
    let Value::Object(mut map) =
        serde_json::to_value(step).expect("a StepIR always serializes to a JSON object")
    else {
        unreachable!("StepIR wire form is a kind-discriminated JSON object");
    };
    let mut domain = serde_json::Map::new();
    for &field in fields {
        if let Some(value) = map.remove(field) {
            domain.insert(field.to_owned(), value);
        }
    }
    Value::Object(domain)
}

/// Computes a step's `effectHash`: the canonical hash of "what this step
/// does to the world" (02 §12.3; see the module docs for the per-kind
/// domain table). Ignores the `effectHash`/`judgeHash` values currently
/// stored on the step, so it serves both sealing (compute) and inspection
/// (recompute and compare).
pub fn effect_hash(step: &StepIR) -> Hash {
    let kind = step.kind();
    domain_hash(
        &effect_hash_domain_tag(kind),
        &step_domain_subtree(step, effect_domain_fields(kind)),
    )
}

/// Projects a step's judge domain WITHOUT its `preflight` sub-domain
/// (02 §12.3 ruling 6): the part of "how this step is judged" that an
/// offline re-judge would actually re-evaluate. Two steps whose values here
/// are byte-equal differ — if their `judgeHash`es differ at all — only in
/// `preflight`, and a preflight change never invalidates an archived
/// verdict (probes run before the act; the judgment judged the same
/// question). The comparison needs both IRs, which is exactly what
/// `ResumeOptions::old_flow_ir` exists to supply.
pub fn judge_subdomain_sans_preflight(step: &StepIR) -> Value {
    let fields: Vec<&str> = judge_domain_fields(step.kind())
        .iter()
        .copied()
        .filter(|field| *field != "preflight")
        .collect();
    step_domain_subtree(step, &fields)
}

/// Computes a step's `judgeHash`: the canonical hash of "how this step is
/// judged" (02 §12.3; see the module docs for the per-kind domain table).
/// Ignores the hash values currently stored on the step.
pub fn judge_hash(step: &StepIR) -> Hash {
    let kind = step.kind();
    domain_hash(
        &judge_hash_domain_tag(kind),
        &step_domain_subtree(step, judge_domain_fields(kind)),
    )
}

/// Computes a flow's `irHash`: the canonical whole-tree hash, excluding
/// exactly two root fields (02 §12.2) — `irHash` itself (self-reference)
/// and `sourceMap` (pure diagnostics: moving a comment or a macro call site
/// must not invalidate resume history). Everything else participates,
/// including each step's stored `effectHash`/`judgeHash` and the
/// `subflows.*.irHash` pins that extend coverage over the whole link
/// closure (02 §6). Ignores the `irHash` value currently stored on the
/// flow, so it serves both sealing and `pointlock inspect` recomputation.
pub fn ir_hash(flow: &FlowIR) -> Hash {
    let mut value =
        serde_json::to_value(flow).expect("a FlowIR always serializes to a JSON object");
    let root = value
        .as_object_mut()
        .expect("FlowIR wire form is a JSON object");
    root.remove("irHash");
    root.remove("sourceMap");
    domain_hash(&ir_hash_domain_tag(), &value)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::assertion::PredicateIR;
    use crate::expr::Expr;
    use crate::primitives::{Identifier, StepId};
    use crate::step::StepIR;
    use crate::vocab::{CanonicalVerb, ElementState};

    /// `"sha256:"` + 64 repetitions of `c` — placeholder hashes for
    /// fixtures (the functions under test ignore stored hash values).
    fn h64(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn fixture_value() -> Value {
        json!({
            "irVersion": 1,
            "flowId": "checkout",
            "irHash": h64('a'),
            "provider": { "name": "devicerail", "version": "0.4.2" },
            "requiredFeatures": ["device.semanticActions.v1"],
            "lockfileDigest": h64('b'),
            "params": [
                { "name": "ssid", "schema": { "type": "string", "minLength": 1 }, "required": true }
            ],
            "outputs": [
                { "name": "wifi_verdict", "schema": { "enum": ["pass", "fail", "unknown"] },
                  "from": { "ref": "steps.open_wifi.verdict" } }
            ],
            "body": [
                {
                    "kind": "action",
                    "stepId": "open_wifi",
                    "effectHash": h64('c'),
                    "judgeHash": h64('d'),
                    "checkpoint": true,
                    "effect": "mutating",
                    "idempotent": true,
                    "binding": { "attempts": [ {
                        "channel": "uiTree",
                        "actionName": "tapElement",
                        "args": { "elementId": { "lit": "wifi_row" } },
                        "acceptExecutionModes": ["nativeSemantic"],
                        "protection": "standard"
                    } ] },
                    "assertions": [ {
                        "assertId": "wifi_toggle_visible",
                        "predicate": { "type": "elementState",
                                       "selector": { "identifier": "wifi_toggle" },
                                       "state": "visible" },
                        "verifyVia": ["uiTree"],
                        "onMissingInput": "unknown"
                    } ]
                },
                {
                    "kind": "call",
                    "stepId": "login",
                    "effectHash": h64('e'),
                    "judgeHash": h64('f'),
                    "checkpoint": true,
                    "flowRef": { "flowId": "ensure_logged_in", "irHash": h64('1') },
                    "inputs": { "user": { "ref": "params.ssid" } }
                }
            ],
            "verdictPolicy": "strict",
            "sourceMap": [
                { "irPath": "/body/0", "file": "checkout.yaml",
                  "span": { "startLine": 3, "startCol": 1, "endLine": 9, "endCol": 20 } }
            ],
            "subflows": {
                "ensure_logged_in": { "flowId": "ensure_logged_in", "irHash": h64('1') }
            }
        })
    }

    fn fixture() -> FlowIR {
        serde_json::from_value(fixture_value()).expect("fixture is schema-valid FlowIR")
    }

    fn action_step_mut(flow: &mut FlowIR) -> &mut crate::step::ActionStepIR {
        match &mut flow.body[0] {
            StepIR::Action(step) => step,
            other => panic!(
                "fixture body[0] must be an action step, got {}",
                other.kind()
            ),
        }
    }

    #[test]
    fn domain_hash_matches_known_vector() {
        // sha256("t\n{}") — pins the exact construction
        // domainTag + "\n" + JCS(subtree).
        assert_eq!(
            domain_hash("t", &json!({})).as_str(),
            "sha256:53483cb46c6e871463e91efe3683f0ad7de603f6fff8eafbb2c42f5fef6d124e"
        );
    }

    #[test]
    fn domain_tags_separate_hash_domains() {
        let subtree = json!({});
        let e = domain_hash(&effect_hash_domain_tag("assert"), &subtree);
        let j = domain_hash(&judge_hash_domain_tag("assert"), &subtree);
        let i = domain_hash(&ir_hash_domain_tag(), &subtree);
        assert_ne!(e, j);
        assert_ne!(e, i);
        assert_ne!(j, i);
        // Deterministic.
        assert_eq!(e, domain_hash(&effect_hash_domain_tag("assert"), &subtree));
    }

    /// Task check 1: the same IR with reordered object keys hashes
    /// identically end to end (parse → FlowIR → irHash).
    #[test]
    fn reordered_object_keys_do_not_move_ir_hash() {
        const FLOW_KEYS_A: &str = r#"{
            "irVersion": 1,
            "flowId": "mini",
            "irHash": "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "provider": { "name": "devicerail", "version": "1.0.0" },
            "requiredFeatures": [],
            "lockfileDigest": "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "params": [ { "name": "label", "schema": { "type": "string", "minLength": 1 }, "required": false, "default": "x" } ],
            "outputs": [],
            "body": [ { "kind": "let", "stepId": "bind_label",
                        "effectHash": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                        "judgeHash": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                        "checkpoint": true, "bindings": { "label": { "lit": "x" } } } ],
            "verdictPolicy": "standard",
            "sourceMap": [],
            "subflows": {}
        }"#;
        // Same document, object member order permuted at every level.
        const FLOW_KEYS_B: &str = r#"{
            "subflows": {},
            "sourceMap": [],
            "verdictPolicy": "standard",
            "body": [ { "bindings": { "label": { "lit": "x" } }, "checkpoint": true,
                        "judgeHash": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                        "effectHash": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                        "stepId": "bind_label", "kind": "let" } ],
            "outputs": [],
            "params": [ { "default": "x", "required": false, "schema": { "minLength": 1, "type": "string" }, "name": "label" } ],
            "lockfileDigest": "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "requiredFeatures": [],
            "provider": { "version": "1.0.0", "name": "devicerail" },
            "irHash": "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "flowId": "mini",
            "irVersion": 1
        }"#;
        let flow_a: FlowIR = serde_json::from_str(FLOW_KEYS_A).expect("valid FlowIR");
        let flow_b: FlowIR = serde_json::from_str(FLOW_KEYS_B).expect("valid FlowIR");
        assert_eq!(flow_a, flow_b);
        assert_eq!(ir_hash(&flow_a), ir_hash(&flow_b));
    }

    /// Task check 2: changing only an assertion moves judgeHash and leaves
    /// effectHash untouched (spine §3: repair only the judgment → history
    /// stays re-judgeable offline).
    #[test]
    fn assertion_change_moves_judge_hash_only() {
        let flow = fixture();
        let (e1, j1) = (effect_hash(&flow.body[0]), judge_hash(&flow.body[0]));

        let mut changed = fixture();
        {
            let step = action_step_mut(&mut changed);
            match &mut step.assertions[0].predicate {
                PredicateIR::ElementState { state, .. } => *state = ElementState::Enabled,
                other => panic!("fixture assertion must be elementState, got {other:?}"),
            }
        }
        assert_eq!(e1, effect_hash(&changed.body[0]));
        assert_ne!(j1, judge_hash(&changed.body[0]));
        // The whole-tree hash still moves: new IR is a different flow.
        assert_ne!(ir_hash(&flow), ir_hash(&changed));
    }

    /// Task check 3: changing only an argument expression moves effectHash
    /// and leaves judgeHash untouched.
    #[test]
    fn argument_change_moves_effect_hash_only() {
        let flow = fixture();
        let (e1, j1) = (effect_hash(&flow.body[0]), judge_hash(&flow.body[0]));

        let mut changed = fixture();
        {
            let step = action_step_mut(&mut changed);
            step.binding.attempts[0].args.insert(
                Identifier::new("elementId").expect("valid identifier"),
                Expr::lit("bluetooth_row"),
            );
        }
        assert_ne!(e1, effect_hash(&changed.body[0]));
        assert_eq!(j1, judge_hash(&changed.body[0]));
    }

    /// Adjudication 6 groundwork: preflight lives in the judge domain (of
    /// every kind), never in the effect domain.
    #[test]
    fn preflight_change_moves_judge_hash_only() {
        let flow = fixture();
        let (e1, j1) = (effect_hash(&flow.body[0]), judge_hash(&flow.body[0]));

        let mut changed = fixture();
        {
            let step = action_step_mut(&mut changed);
            let probe = step.assertions[0].clone();
            step.base.preflight = Some(vec![probe]);
        }
        assert_eq!(e1, effect_hash(&changed.body[0]));
        assert_ne!(j1, judge_hash(&changed.body[0]));
    }

    /// Adjudications 1–2: identity (stepId) and budget/strategy/label
    /// fields (checkpoint, timeoutMs, verb) move neither hash — but still
    /// move irHash.
    #[test]
    fn identity_and_budget_fields_move_neither_hash() {
        let flow = fixture();
        let (e1, j1) = (effect_hash(&flow.body[0]), judge_hash(&flow.body[0]));

        let mut changed = fixture();
        {
            let step = action_step_mut(&mut changed);
            step.base.step_id = StepId::new("open_wifi_renamed").expect("valid step id");
            step.base.checkpoint = false;
            step.base.timeout_ms = Some(9999);
            step.verb = Some(CanonicalVerb::Tap);
        }
        assert_eq!(e1, effect_hash(&changed.body[0]));
        assert_eq!(j1, judge_hash(&changed.body[0]));
        assert_ne!(ir_hash(&flow), ir_hash(&changed));
    }

    /// Call steps: flowRef + inputs are effect; the judge domain is
    /// preflight only.
    #[test]
    fn call_inputs_change_moves_effect_hash_only() {
        let flow = fixture();
        let (e1, j1) = (effect_hash(&flow.body[1]), judge_hash(&flow.body[1]));

        let mut changed = fixture();
        match &mut changed.body[1] {
            StepIR::Call(call) => {
                call.inputs.insert(
                    Identifier::new("user").expect("valid identifier"),
                    Expr::lit("admin"),
                );
            }
            other => panic!("fixture body[1] must be a call step, got {}", other.kind()),
        }
        assert_ne!(e1, effect_hash(&changed.body[1]));
        assert_eq!(j1, judge_hash(&changed.body[1]));
    }

    /// Adjudication 4: a human step's question domain (here: prompt) is
    /// effect — old answers do not transfer to a changed question.
    #[test]
    fn human_prompt_change_moves_effect_hash_only() {
        let human = |prompt: &str| -> StepIR {
            serde_json::from_value(json!({
                "kind": "human", "stepId": "approve",
                "effectHash": h64('2'), "judgeHash": h64('3'), "checkpoint": true,
                "mode": "judge", "prompt": prompt, "presents": [],
                "decisions": ["pass", "fail", "unknown"],
                "timeoutMs": 3600000, "onTimeout": "unknown"
            }))
            .expect("valid human step")
        };
        let a = human("Approve the run?");
        let b = human("Approve the release?");
        assert_ne!(effect_hash(&a), effect_hash(&b));
        assert_eq!(judge_hash(&a), judge_hash(&b));
    }

    /// Adjudication 5: container hashes exclude the subtree — changing a
    /// child step moves neither container hash.
    #[test]
    fn container_hashes_exclude_the_subtree() {
        let if_step = |child_value: &str| -> StepIR {
            serde_json::from_value(json!({
                "kind": "if", "stepId": "branch",
                "effectHash": h64('4'), "judgeHash": h64('5'), "checkpoint": true,
                "cond": { "lit": true },
                "then": [ { "kind": "let", "stepId": "bind",
                            "effectHash": h64('6'), "judgeHash": h64('7'),
                            "checkpoint": false,
                            "bindings": { "x": { "lit": child_value } } } ]
            }))
            .expect("valid if step")
        };
        let a = if_step("a");
        let b = if_step("b");
        assert_eq!(effect_hash(&a), effect_hash(&b));
        assert_eq!(judge_hash(&a), judge_hash(&b));
        // But the control decision is effect:
        let mut cond_changed = if_step("a");
        match &mut cond_changed {
            StepIR::If(s) => s.cond = Expr::lit(false),
            _ => unreachable!(),
        }
        assert_ne!(effect_hash(&a), effect_hash(&cond_changed));
    }

    /// The assert kind's effect domain is `kind` alone: two structurally
    /// different assert steps share one effectHash (and it is pinned by the
    /// known vector for `{"kind":"assert"}`).
    #[test]
    fn assert_step_effect_domain_is_kind_only() {
        let assert_step = |observe: Value, state: &str| -> StepIR {
            serde_json::from_value(json!({
                "kind": "assert", "stepId": "check",
                "effectHash": h64('8'), "judgeHash": h64('9'), "checkpoint": true,
                "observe": observe,
                "assertions": [ {
                    "assertId": "toggle_state",
                    "predicate": { "type": "elementState",
                                   "selector": { "identifier": "wifi_toggle" },
                                   "state": state },
                    "verifyVia": ["uiTree"],
                    "onMissingInput": "unknown"
                } ]
            }))
            .expect("valid assert step")
        };
        let a = assert_step(json!("fresh"), "visible");
        let b = assert_step(
            json!({ "fromStep": "open_wifi", "which": "after" }),
            "enabled",
        );
        assert_eq!(effect_hash(&a), effect_hash(&b));
        // sha256("pointlock-ir/1/effectHash/assert\n{\"kind\":\"assert\"}")
        assert_eq!(
            effect_hash(&a).as_str(),
            "sha256:9c76efbea403620940da9e87d44460e22c1b6f6081c9a95821838d1d34991d67"
        );
        // observe is judge-side (offline re-judgeability declaration):
        assert_ne!(judge_hash(&a), judge_hash(&b));
    }

    /// irHash exclusions (02 §12.2): the stored irHash value and the whole
    /// sourceMap are outside the hash; everything else is inside.
    #[test]
    fn ir_hash_excludes_ir_hash_and_source_map() {
        let flow = fixture();

        let mut cosmetic = fixture();
        cosmetic.ir_hash = Hash::new(h64('9')).expect("valid hash literal");
        cosmetic.source_map.clear();
        assert_eq!(ir_hash(&flow), ir_hash(&cosmetic));

        // Stored per-step hashes DO participate (self-checkable artifact):
        let mut step_hash_changed = fixture();
        action_step_mut(&mut step_hash_changed).base.effect_hash =
            Hash::new(h64('9')).expect("valid hash literal");
        assert_ne!(ir_hash(&flow), ir_hash(&step_hash_changed));
    }

    /// Link closure (02 §12.2/§6): a callee pin change in `subflows`
    /// changes the caller's irHash.
    #[test]
    fn ir_hash_covers_subflow_pins() {
        let flow = fixture();
        let mut repinned = fixture();
        let callee = crate::primitives::FlowId::new("ensure_logged_in").expect("valid flow id");
        repinned
            .subflows
            .get_mut(&callee)
            .expect("fixture has the subflow entry")
            .ir_hash = Hash::new(h64('2')).expect("valid hash literal");
        assert_ne!(ir_hash(&flow), ir_hash(&repinned));
    }
}
