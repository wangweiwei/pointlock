//! M1 compile-surface tests: the five element verbs over a DeviceRail-shaped
//! semantic-five capability set, the full fallback dual chain
//! (`locate_via` + `coordinate` / `verify_via` + `visual`), the static
//! predicate forms, the feature-bundling rule, plus one negative per
//! rejection class with its expected RF code.

use std::collections::BTreeMap;

use pointlock_compiler::{CompileDiagnostic, CompileOptions, Sealed, codes, compile};
use pointlock_ir::vocab::{
    ActChannel, CanonicalVerb, Channel, EffectClassAction, ElementState, ExecutionMode,
    TextMatchMode, VerifyChannel,
};
use pointlock_ir::{ActionName, FeatureId, Hash, JsonSchemaDocument, PredicateIR, StepIR};
use pointlock_provider_kit::lockfile::{
    CapabilityLockfile, LockfileDevice, LockfileHello, LockfileProvider, PeerInfo, ProtocolVersion,
};
use pointlock_provider_kit::manifest::{
    ActionDefinitionStatic, ActionProtection, ChannelRole, ChannelSupport, FeatureDeclarations,
    PlatformKind, ProtocolRange, ProviderManifest, VerbBinding,
};
use serde_json::json;

fn feature(id: &str) -> FeatureId {
    FeatureId::new(id).expect("grammatical feature id")
}

fn action_name(name: &str) -> ActionName {
    ActionName::new(name).expect("grammatical action name")
}

fn action(
    name: &str,
    input_schema: serde_json::Value,
    protection: ActionProtection,
) -> ActionDefinitionStatic {
    ActionDefinitionStatic {
        name: action_name(name),
        input_schema: JsonSchemaDocument::new(input_schema).expect("valid schema document"),
        output_schema: None,
        protection,
        synthetic: false,
    }
}

/// The DeviceRail wire `ElementSelector` schema (04 §9.5, condensed).
fn element_selector_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "context": {
                "type": "object",
                "properties": {
                    "contextKind": { "enum": ["native", "web"] },
                    "contextId": { "type": "string", "minLength": 1 }
                },
                "required": ["contextKind"],
                "additionalProperties": false
            },
            "role": { "type": "string", "minLength": 1 },
            "name": { "type": "string", "minLength": 1 },
            "identifier": { "type": "string", "minLength": 1 },
            "text": {
                "type": "object",
                "properties": {
                    "value": { "type": "string", "minLength": 1 },
                    "mode": { "enum": ["exact", "contains"] },
                    "caseSensitive": { "type": "boolean" }
                },
                "required": ["value", "mode", "caseSensitive"],
                "additionalProperties": false
            },
            "value": { "type": "string" },
            "css": { "type": "string", "minLength": 1 }
        },
        "minProperties": 1,
        "additionalProperties": false
    })
}

/// The DeviceRail wire `ElementTarget` schema (04 §9.5; the compiler only
/// ever produces `kind: "selector"`).
fn element_target_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "kind": { "const": "selector" },
            "selector": element_selector_schema()
        },
        "required": ["kind", "selector"],
        "additionalProperties": false
    })
}

/// The semantic five (spine A.8) plus the Android driver's coordinate `tap`.
fn m1_actions() -> Vec<ActionDefinitionStatic> {
    vec![
        action(
            "findElement",
            json!({
                "type": "object",
                "properties": { "selector": element_selector_schema() },
                "required": ["selector"],
                "additionalProperties": false
            }),
            ActionProtection::Standard,
        ),
        action(
            "tapElement",
            json!({
                "type": "object",
                "properties": { "target": element_target_schema() },
                "required": ["target"],
                "additionalProperties": false
            }),
            ActionProtection::Standard,
        ),
        action(
            "clearElement",
            json!({
                "type": "object",
                "properties": { "target": element_target_schema() },
                "required": ["target"],
                "additionalProperties": false
            }),
            ActionProtection::Standard,
        ),
        action(
            "setElementValue",
            json!({
                "type": "object",
                "properties": {
                    "target": element_target_schema(),
                    "value": { "type": "string" }
                },
                "required": ["target", "value"],
                "additionalProperties": false
            }),
            ActionProtection::Standard,
        ),
        action(
            "waitForElement",
            json!({
                "type": "object",
                "properties": {
                    "selector": element_selector_schema(),
                    "condition": { "enum": ["present", "visible", "enabled", "absent"] }
                },
                "required": ["selector"],
                "additionalProperties": false
            }),
            ActionProtection::Standard,
        ),
        action(
            "tap",
            json!({
                "type": "object",
                "properties": {
                    "x": { "type": "integer", "minimum": 0 },
                    "y": { "type": "integer", "minimum": 0 }
                },
                "required": ["x", "y"],
                "additionalProperties": false
            }),
            ActionProtection::Standard,
        ),
    ]
}

fn semantic_binding(verb: CanonicalVerb, native: &str, arg_map: &[(&str, &str)]) -> VerbBinding {
    VerbBinding {
        verb,
        action_name: action_name(native),
        requires_feature: Some(feature("device.semanticActions.v1")),
        arg_map: arg_map
            .iter()
            .map(|(surface, native)| ((*surface).to_owned(), (*native).to_owned()))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// The M1 test manifest: semantic five + coordinate `tap` binding, all
/// four channels declared with their spine roles.
fn m1_manifest() -> ProviderManifest {
    ProviderManifest {
        name: "devicerail".to_owned(),
        version: "0.1.0".to_owned(),
        protocol: ProtocolRange {
            major: 1,
            min_minor: 5,
            max_minor: 5,
        },
        features: FeatureDeclarations {
            guaranteed: vec![
                feature("device.semanticActions.v1"),
                feature("observation.uiSnapshot.v1"),
                // Every IR requires verdict write-back (04 §9.2).
                feature("verdict.record.v1"),
            ],
            conditional: Vec::new(),
        },
        verb_bindings: vec![
            semantic_binding(CanonicalVerb::Tap, "tapElement", &[("element", "target")]),
            semantic_binding(
                CanonicalVerb::SetValue,
                "setElementValue",
                &[("element", "target"), ("value", "value")],
            ),
            semantic_binding(
                CanonicalVerb::Clear,
                "clearElement",
                &[("element", "target")],
            ),
            semantic_binding(
                CanonicalVerb::WaitFor,
                "waitForElement",
                &[("element", "selector"), ("state", "condition")],
            ),
            semantic_binding(
                CanonicalVerb::Find,
                "findElement",
                &[("element", "selector")],
            ),
            // The coordinate binding of `tap` (03 §1.4: the static
            // coordinate key maps to the driver's coordinate action).
            VerbBinding {
                verb: CanonicalVerb::Tap,
                action_name: action_name("tap"),
                requires_feature: None,
                arg_map: BTreeMap::from([
                    ("x".to_owned(), "x".to_owned()),
                    ("y".to_owned(), "y".to_owned()),
                ]),
            },
        ],
        channels: vec![
            ChannelSupport {
                channel: Channel::Dom,
                role: ChannelRole::Both,
                requires_feature: None,
                requires_platform: None,
            },
            ChannelSupport {
                channel: Channel::UiTree,
                role: ChannelRole::Both,
                requires_feature: Some(feature("observation.uiSnapshot.v1")),
                requires_platform: None,
            },
            ChannelSupport {
                channel: Channel::Coordinate,
                role: ChannelRole::Act,
                requires_feature: None,
                requires_platform: None,
            },
            ChannelSupport {
                channel: Channel::Vision,
                role: ChannelRole::Verify,
                requires_feature: None,
                requires_platform: None,
            },
        ],
        known_actions: m1_actions(),
    }
}

fn m1_lockfile(manifest: &ProviderManifest, platform: PlatformKind) -> CapabilityLockfile {
    let mut lockfile = CapabilityLockfile {
        provider: LockfileProvider {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
        },
        attested_at: "2026-01-01T00:00:00Z".to_owned(),
        hello: LockfileHello {
            protocol_selected: ProtocolVersion { major: 1, minor: 5 },
            features_enabled: manifest.features.guaranteed.clone(),
            server: PeerInfo {
                name: "fake-daemon".to_owned(),
                version: "1.5.0".to_owned(),
            },
        },
        device: LockfileDevice {
            platform,
            actions: manifest.known_actions.clone(),
        },
        digest: Hash::new(format!("sha256:{}", "0".repeat(64))).expect("grammatical placeholder"),
    };
    lockfile.seal();
    lockfile
}

fn compile_with(
    source: &str,
    manifest: &ProviderManifest,
    platform: PlatformKind,
) -> Result<Sealed, Vec<CompileDiagnostic>> {
    let lockfile = m1_lockfile(manifest, platform);
    compile(
        source,
        &CompileOptions {
            source_name: "case.flow.yaml",
            manifest,
            lockfile: Some(&lockfile),
        },
    )
}

fn compile_android(source: &str) -> Result<Sealed, Vec<CompileDiagnostic>> {
    compile_with(source, &m1_manifest(), PlatformKind::Android)
}

/// Asserts that compilation fails and some diagnostic carries `code`.
fn expect_code(source: &str, code: &str) {
    let diags = compile_android(source)
        .err()
        .unwrap_or_else(|| panic!("expected a {code} rejection, got a sealed artifact"));
    assert!(
        diags.iter().any(|diag| diag.code == code),
        "expected code {code}, got: {diags:#?}"
    );
    // Hard rule (03 §4.4): diagnostics carry a span whenever one exists.
    assert!(
        diags
            .iter()
            .filter(|diag| diag.code == code)
            .all(|diag| diag.span.is_some()),
        "diagnostic {code} lost its span: {diags:#?}"
    );
}

fn action_step(step: &StepIR) -> &pointlock_ir::ActionStepIR {
    let StepIR::Action(action) = step else {
        panic!("M1 steps are action steps");
    };
    action
}

fn attempt_args_json(attempt: &pointlock_ir::BoundAttempt) -> serde_json::Value {
    serde_json::to_value(&attempt.args).expect("args serialize")
}

// ─── positives ──────────────────────────────────────────────────────────────

const FIVE_VERBS_FLOW: &str = r#"
flow: verbs_demo
provider: devicerail
params:
  username:
    schema: { type: string, minLength: 1 }
    required: true
steps:
  - id: open_login
    tap:
      element: { role: button, name: "登录" }
  - id: enter_username
    set_value:
      element: { identifier: "com.example.shop:id/username_input" }
      value: ${{ params.username }}
  - id: clear_note
    clear:
      element: { identifier: "com.example.shop:id/note" }
  - id: wait_home
    wait_for:
      element: { identifier: "com.example.shop:id/home_tab" }
      state: visible
  - id: find_avatar
    find:
      element: { text: { value: "头像", mode: contains } }
"#;

#[test]
fn five_verbs_bind_to_the_semantic_five() {
    let sealed = compile_android(FIVE_VERBS_FLOW).expect("the five-verb flow must compile");
    let flow = &sealed.flow_ir;
    assert_eq!(flow.body.len(), 5);

    // Verb metadata, inferred effects, native action names (spine A.6:
    // the verb vanishes, actionName is the provider-native name).
    let expected: [(CanonicalVerb, EffectClassAction, &str); 5] = [
        (
            CanonicalVerb::Tap,
            EffectClassAction::Mutating,
            "tapElement",
        ),
        (
            CanonicalVerb::SetValue,
            EffectClassAction::Mutating,
            "setElementValue",
        ),
        (
            CanonicalVerb::Clear,
            EffectClassAction::Mutating,
            "clearElement",
        ),
        (
            CanonicalVerb::WaitFor,
            EffectClassAction::Readonly,
            "waitForElement",
        ),
        (
            CanonicalVerb::Find,
            EffectClassAction::Readonly,
            "findElement",
        ),
    ];
    for (step, (verb, effect, native)) in flow.body.iter().zip(expected) {
        let step = action_step(step);
        assert_eq!(step.verb, Some(verb));
        assert_eq!(step.effect, effect);
        assert_eq!(
            step.binding.attempts.len(),
            1,
            "no declared chain ⇒ one attempt"
        );
        let attempt = &step.binding.attempts[0];
        assert_eq!(attempt.action_name.as_str(), native);
        // Android platform default channel is uiTree (03 §1.4).
        assert_eq!(attempt.channel, ActChannel::UiTree);
        assert_eq!(
            attempt.requires_feature,
            Some(feature("device.semanticActions.v1"))
        );
        // 02 §5.1 rule 6: semantic attempts never admit coordinateFallback.
        assert!(
            !attempt
                .accept_execution_modes
                .contains(&ExecutionMode::CoordinateFallback)
        );
    }

    // The target verbs wrap the selector into the DeviceRail ElementTarget
    // (`kind: "selector"`, 04 §9.5); the search verbs pass it through bare.
    let tap = action_step(&flow.body[0]);
    assert_eq!(
        attempt_args_json(&tap.binding.attempts[0]),
        json!({
            "target": { "lit": {
                "kind": "selector",
                "selector": { "role": "button", "name": "登录" }
            } }
        })
    );
    let set_value = action_step(&flow.body[1]);
    assert_eq!(
        attempt_args_json(&set_value.binding.attempts[0]),
        json!({
            "target": { "lit": {
                "kind": "selector",
                "selector": { "identifier": "com.example.shop:id/username_input" }
            } },
            "value": { "ref": "params.username" }
        })
    );
    let wait_for = action_step(&flow.body[3]);
    assert_eq!(
        attempt_args_json(&wait_for.binding.attempts[0]),
        json!({
            "selector": { "lit": { "identifier": "com.example.shop:id/home_tab" } },
            "condition": { "lit": "visible" }
        })
    );
    // Text matcher defaults are materialized (mode written, caseSensitive
    // defaulted — single-representation rule).
    let find = action_step(&flow.body[4]);
    assert_eq!(
        attempt_args_json(&find.binding.attempts[0]),
        json!({
            "selector": { "lit": {
                "text": { "value": "头像", "mode": "contains", "caseSensitive": false }
            } }
        })
    );

    // Feature collection: verb feature + channel feature.
    assert!(
        flow.required_features
            .contains(&feature("device.semanticActions.v1"))
    );
    assert!(
        flow.required_features
            .contains(&feature("observation.uiSnapshot.v1"))
    );

    // Binding report: the dynamic `value` defers to runtime, the static
    // selector target validates statically.
    let report = &sealed.binding_report;
    assert_eq!(report.steps.len(), 5);
    assert_eq!(report.steps[1].action_name.as_str(), "setElementValue");
    assert!(
        report.steps[1]
            .statically_validated_args
            .contains(&"target".to_owned())
    );
    assert!(
        report.steps[1]
            .runtime_deferred_args
            .contains(&"value".to_owned())
    );
}

const DUAL_CHAIN_FLOW: &str = r##"
flow: chain_demo
provider: devicerail
steps:
  - id: tap_login
    tap:
      element: { role: button, name: "登录", css: "#login-btn" }
    locate_via: [dom, uiTree, coordinate]
    coordinate: { x: 540, y: 1650 }
    expect:
      - element: { identifier: "login_form", css: "#login-form" }
        state: absent
        verify_via: [dom, uiTree, vision]
        visual: "登录表单已从屏幕上消失"
"##;

#[test]
fn full_dual_chain_compiles_with_derived_modes() {
    let sealed = compile_android(DUAL_CHAIN_FLOW).expect("the dual-chain flow must compile");
    let flow = &sealed.flow_ir;
    let step = action_step(&flow.body[0]);

    // act-chain: one attempt per channel, in declared order.
    let attempts = &step.binding.attempts;
    assert_eq!(attempts.len(), 3);
    assert_eq!(
        attempts.iter().map(|a| a.channel).collect::<Vec<_>>(),
        vec![ActChannel::Dom, ActChannel::UiTree, ActChannel::Coordinate]
    );
    assert_eq!(attempts[0].action_name.as_str(), "tapElement");
    assert_eq!(attempts[1].action_name.as_str(), "tapElement");
    // The coordinate attempt binds the driver's coordinate action with the
    // static literal coordinates (03 §1.4 rule 3).
    assert_eq!(attempts[2].action_name.as_str(), "tap");
    assert_eq!(
        attempt_args_json(&attempts[2]),
        json!({ "x": { "lit": 540 }, "y": { "lit": 1650 } })
    );

    // 02 §5.1 rule 6: modes derive from each attempt's own channel only.
    for semantic in &attempts[0..2] {
        assert_eq!(
            semantic
                .accept_execution_modes
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![ExecutionMode::NativeSemantic, ExecutionMode::WebSemantic]
        );
    }
    assert_eq!(
        attempts[2]
            .accept_execution_modes
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![
            ExecutionMode::NativeSemantic,
            ExecutionMode::WebSemantic,
            ExecutionMode::CoordinateFallback
        ]
    );

    // verify-chain: 1:1 into AssertionIR.verifyVia; the vision tail carries
    // the author's prompt verbatim (03 §1.4 rule 5).
    assert_eq!(step.assertions.len(), 1);
    let assertion = &step.assertions[0];
    assert_eq!(
        assertion.verify_via,
        vec![
            VerifyChannel::Dom,
            VerifyChannel::UiTree,
            VerifyChannel::Vision
        ]
    );
    assert_eq!(
        assertion.vision_prompt.as_deref(),
        Some("登录表单已从屏幕上消失")
    );
    assert!(matches!(
        &assertion.predicate,
        PredicateIR::ElementState {
            state: ElementState::Absent,
            ..
        }
    ));

    // Factory schema check, explicitly re-run in the test.
    let schema = pointlock_ir::schema_gen::flow_ir_schema();
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("generated schema compiles");
    let instance = serde_json::to_value(flow).expect("FlowIR serializes");
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|err| format!("{}: {err}", err.instance_path))
        .collect();
    assert!(errors.is_empty(), "schema violations: {errors:#?}");

    // Binding report: one entry per attempt.
    assert_eq!(sealed.binding_report.steps.len(), 3);
    assert!(
        sealed
            .binding_report
            .steps
            .iter()
            .all(|entry| entry.step_id == "tap_login")
    );
}

#[test]
fn ir_hash_is_deterministic_across_compiles() {
    let first = compile_android(DUAL_CHAIN_FLOW).expect("compiles");
    let second = compile_android(DUAL_CHAIN_FLOW).expect("compiles");
    assert_eq!(first.flow_ir.ir_hash, second.flow_ir.ir_hash);
    assert_eq!(first.flow_ir, second.flow_ir);
    assert_eq!(first.flow_ir.ir_hash, pointlock_ir::ir_hash(&first.flow_ir));
}

const PREDICATES_FLOW: &str = r#"
flow: predicates_demo
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - element: { identifier: "wifi_toggle" }
        state: enabled
      - element: { identifier: "current_network_label" }
        text: { value: "Pointlock-Lab", mode: contains }
      - visual: "购物车角标显示数字 1"
        region: { x: 900, y: 40, width: 160, height: 120 }
      - expr: ${{ eq(steps.probe.output.matched, true) }}
"#;

#[test]
fn predicate_forms_compile_with_default_chains() {
    let sealed = compile_android(PREDICATES_FLOW).expect("the predicate flow must compile");
    let step = action_step(&sealed.flow_ir.body[0]);
    assert_eq!(step.assertions.len(), 4);

    let element_state = &step.assertions[0];
    assert!(matches!(
        &element_state.predicate,
        PredicateIR::ElementState {
            state: ElementState::Enabled,
            ..
        }
    ));
    // Default verify-chain: the platform's single channel (android⇒uiTree).
    assert_eq!(element_state.verify_via, vec![VerifyChannel::UiTree]);
    assert!(element_state.vision_prompt.is_none());
    assert!(
        element_state
            .assert_id
            .as_str()
            .starts_with("elementState-")
    );

    let element_text = &step.assertions[1];
    let PredicateIR::ElementText { r#match, .. } = &element_text.predicate else {
        panic!("expected an elementText predicate");
    };
    assert_eq!(r#match.value, "Pointlock-Lab");
    assert_eq!(r#match.mode, TextMatchMode::Contains);
    assert!(
        !r#match.case_sensitive,
        "caseSensitive default materialized"
    );
    assert_eq!(element_text.verify_via, vec![VerifyChannel::UiTree]);
    assert!(element_text.assert_id.as_str().starts_with("elementText-"));

    let visual = &step.assertions[2];
    let PredicateIR::Visual { prompt, region } = &visual.predicate else {
        panic!("expected a visual predicate");
    };
    assert_eq!(prompt, "购物车角标显示数字 1");
    assert!(region.is_some());
    // Visual predicates are vision-only, prompt lives in the predicate.
    assert_eq!(visual.verify_via, vec![VerifyChannel::Vision]);
    assert!(visual.vision_prompt.is_none());
    assert!(visual.assert_id.as_str().starts_with("visual-"));

    let expr = &step.assertions[3];
    assert!(matches!(&expr.predicate, PredicateIR::Expr { .. }));
    assert!(expr.verify_via.is_empty());
    assert!(expr.assert_id.as_str().starts_with("expr-"));
}

const WEB_TAP_FLOW: &str = r##"
flow: web_tap
provider: devicerail
steps:
  - id: tap_submit
    tap:
      element: { css: "#submit" }
"##;

#[test]
fn semantic_feature_collection_bundles_ui_snapshot() {
    // On web the default channel is dom (no uiSnapshot requirement of its
    // own), so the snapshot feature can only arrive through the bundling
    // rule: semanticActions ⇒ uiSnapshot (daemon handshake coupling,
    // 04 §9.2 pending annotation).
    let sealed = compile_with(WEB_TAP_FLOW, &m1_manifest(), PlatformKind::Web)
        .expect("the web tap flow must compile");
    let features = &sealed.flow_ir.required_features;
    assert!(features.contains(&feature("device.semanticActions.v1")));
    assert!(
        features.contains(&feature("observation.uiSnapshot.v1")),
        "uiSnapshot must be bundled with semanticActions: {features:?}"
    );
    assert_eq!(
        sealed.binding_report.required_features,
        sealed.flow_ir.required_features
    );
    // And the default web channel really was dom.
    let step = action_step(&sealed.flow_ir.body[0]);
    assert_eq!(step.binding.attempts[0].channel, ActChannel::Dom);
}

#[test]
fn unsatisfiable_snapshot_bundle_is_rejected_rf4008() {
    // semanticActions available without uiSnapshot: the bundle cannot be
    // satisfied — fail-closed instead of shipping an IR whose handshake
    // must die with semantic_snapshot_dependency_unsatisfied.
    let mut manifest = m1_manifest();
    manifest.features.guaranteed = vec![feature("device.semanticActions.v1")];
    manifest
        .channels
        .retain(|support| support.channel != Channel::UiTree);
    let diags = compile_with(WEB_TAP_FLOW, &manifest, PlatformKind::Web)
        .expect_err("the bundle is unsatisfiable");
    assert!(
        diags
            .iter()
            .any(|diag| diag.code == codes::BIND_FEATURE_UNAVAILABLE),
        "expected {}, got: {diags:#?}",
        codes::BIND_FEATURE_UNAVAILABLE
    );
}

// ─── negatives ──────────────────────────────────────────────────────────────

#[test]
fn vision_in_locate_via_is_rejected_rf4014() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: tap_login
    tap:
      element: { role: button }
    locate_via: [uiTree, vision]
"#,
        codes::BIND_CHAIN_CHANNEL_ILLEGAL,
    );
}

#[test]
fn coordinate_in_verify_via_is_rejected_rf4014() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - element: { identifier: "wifi_toggle" }
        state: visible
        verify_via: [uiTree, coordinate]
"#,
        codes::BIND_CHAIN_CHANNEL_ILLEGAL,
    );
}

#[test]
fn out_of_order_verify_via_is_rejected_rf4013() {
    expect_code(
        r##"
flow: bad
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row", css: "#wifi" }
    expect:
      - element: { identifier: "wifi_toggle", css: "#toggle" }
        state: visible
        verify_via: [uiTree, dom]
"##,
        codes::BIND_CHAIN_ORDER,
    );
}

#[test]
fn coordinate_channel_without_static_key_is_rejected_rf4015() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: tap_login
    tap:
      element: { role: button }
    locate_via: [uiTree, coordinate]
"#,
        codes::BIND_COORDINATE_STATIC,
    );
}

#[test]
fn coordinate_key_without_channel_is_rejected_rf4015() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: tap_login
    tap:
      element: { role: button }
    coordinate: { x: 10, y: 20 }
"#,
        codes::BIND_COORDINATE_STATIC,
    );
}

#[test]
fn vision_tail_without_visual_prompt_is_rejected_rf4017() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - element: { identifier: "wifi_toggle" }
        state: visible
        verify_via: [uiTree, vision]
"#,
        codes::BIND_VISION_PROMPT,
    );
}

#[test]
fn visual_prompt_without_vision_tail_is_rejected_rf4017() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - element: { identifier: "wifi_toggle" }
        state: visible
        verify_via: [uiTree]
        visual: "多余的提示词"
"#,
        codes::BIND_VISION_PROMPT,
    );
}

#[test]
fn expression_in_predicate_field_is_rejected_rf2026() {
    // Selector field.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - element:
          identifier: ${{ params.target }}
        state: visible
"#,
        codes::NORM_PREDICATE_NOT_STATIC,
    );
    // Text value.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - element: { identifier: "welcome" }
        text:
          value: ${{ params.username }}
"#,
        codes::NORM_PREDICATE_NOT_STATIC,
    );
    // Visual prompt.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - visual: ${{ params.prompt }}
"#,
        codes::NORM_PREDICATE_NOT_STATIC,
    );
}

#[test]
fn dynamic_verb_selector_is_rejected_rf2024() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: tap_it
    tap:
      element: ${{ steps.probe.output.element }}
"#,
        codes::NORM_BAD_SELECTOR_SHAPE,
    );
}

#[test]
fn invalid_wait_for_state_is_rejected_rf2023() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: wait_home
    wait_for:
      element: { identifier: "home_tab" }
      state: blinking
"#,
        codes::NORM_BAD_VERB_SHAPE,
    );
}

#[test]
fn effect_conflicting_with_inference_is_rejected_rf2025() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: tap_login
    tap:
      element: { role: button }
    effect: readonly
"#,
        codes::NORM_EFFECT_CONFLICT,
    );
}

#[test]
fn dom_channel_without_css_material_is_rejected_rf4016() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: tap_login
    tap:
      element: { role: button }
    locate_via: [dom, uiTree]
"#,
        codes::BIND_SELECTOR_MATERIAL,
    );
}

#[test]
fn visual_predicate_with_non_vision_chain_is_rejected_rf4014() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - visual: "购物车角标显示数字 1"
        verify_via: [uiTree]
"#,
        codes::BIND_CHAIN_CHANNEL_ILLEGAL,
    );
}

#[test]
fn verb_without_verb_binding_is_rejected_rf4012() {
    let mut manifest = m1_manifest();
    manifest
        .verb_bindings
        .retain(|binding| binding.verb != CanonicalVerb::Find);
    let diags = compile_with(
        r#"
flow: bad
provider: devicerail
steps:
  - id: find_avatar
    find:
      element: { identifier: "avatar" }
"#,
        &manifest,
        PlatformKind::Android,
    )
    .expect_err("find has no verbBinding");
    assert!(
        diags
            .iter()
            .any(|diag| diag.code == codes::BIND_VERB_UNBOUND),
        "expected {}, got: {diags:#?}",
        codes::BIND_VERB_UNBOUND
    );
}

#[test]
fn value_assertion_sugar_desugars_to_text_exact() {
    // 03 §1.5: `value: <s>` IS `text: { value: <s>, mode: exact }` — the
    // two forms must produce identical assertion IR and identical
    // judgeHash (the sugar leaves no trace).
    let sugar = r#"
flow: sugar_demo
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - element: { identifier: "ssid_field" }
        value: "Pointlock-Lab"
"#;
    let desugared = r#"
flow: sugar_demo
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - element: { identifier: "ssid_field" }
        text: { value: "Pointlock-Lab", mode: exact }
"#;
    let a = compile_with(sugar, &m1_manifest(), PlatformKind::Android).expect("sugar compiles");
    let b = compile_with(desugared, &m1_manifest(), PlatformKind::Android).expect("text compiles");
    assert_eq!(
        a.flow_ir.ir_hash, b.flow_ir.ir_hash,
        "sugar must be invisible to hashing"
    );

    // Conflict control: both surfaces on one item is an explicit error,
    // and a non-static value is rejected (predicate staticness).
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - element: { identifier: "ssid_field" }
        text: { value: "Pointlock-Lab", mode: exact }
        value: "Pointlock-Lab"
"#,
        codes::NORM_BAD_EXPECT_SHAPE,
    );
    expect_code(
        r#"
flow: bad
provider: devicerail
params:
  ssid: { schema: { type: string }, required: true }
steps:
  - id: probe
    find:
      element: { identifier: "wifi_row" }
    expect:
      - element: { identifier: "ssid_field" }
        value: ${{ params.ssid }}
"#,
        codes::NORM_PREDICATE_NOT_STATIC,
    );
}

/// 03 §4.3 E7 / §1.5: a `visual` predicate must not be the only thing
/// supporting a step's `pass`. `pointlock-vision`'s default implementation
/// returns `unknown`, so a step verified by vision ALONE folds to unknown
/// on a host with no vision capability — the rule makes the flow say that
/// at compile time instead of discovering it in production (principle 7).
const VISUAL_ONLY_FLOW: &str = r#"
flow: visual_only_demo
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "cart_badge" }
    expect:
      - visual: "购物车角标显示数字 1"
        region: { x: 900, y: 40, width: 160, height: 120 }
"#;

/// The same step with a structured assertion beside it — §1.5's first
/// exemption. The visual one is then a declared degradation, not the
/// primary verification.
const VISUAL_PLUS_STRUCTURED_FLOW: &str = r#"
flow: visual_plus_demo
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "cart_badge" }
    expect:
      - element: { identifier: "cart_badge" }
        state: enabled
      - visual: "购物车角标显示数字 1"
        region: { x: 900, y: 40, width: 160, height: 120 }
"#;

/// §1.5's second exemption: the verdict is delegated to a `human` judge,
/// which produces it itself, so nothing rests on the vision channel.
const VISUAL_ONLY_HUMAN_JUDGED_FLOW: &str = r#"
flow: visual_judged_demo
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: "cart_badge" }
    expect:
      - visual: "购物车角标显示数字 1"
        region: { x: 900, y: 40, width: 160, height: 120 }
    on_unknown:
      - escalate:
          mode: judge
          prompt: 机器无法判定本步结果，请依据证据人工裁决
          decisions: [pass, fail, unknown]
          timeout_ms: 3600000
          on_timeout: unknown
        max_triggers: 1
"#;

#[test]
fn e7_refuses_a_visual_predicate_as_the_sole_support_for_a_pass() {
    expect_code(VISUAL_ONLY_FLOW, codes::BIND_VISUAL_SOLE_SUPPORT);

    // Exemption 1: a structured assertion carries the pass.
    compile_android(VISUAL_PLUS_STRUCTURED_FLOW)
        .expect("a structured assertion beside the visual one is legal");

    // Exemption 2: a human judge produces the verdict.
    compile_android(VISUAL_ONLY_HUMAN_JUDGED_FLOW)
        .expect("delegating the verdict to a human judge is legal");
}

/// 03 §4.3 C5/C6 (02 §8.2): definite type contradictions refuse at
/// compile; `Unknown`-typed positions stay legal (gradual typing — the
/// runtime unknown fallback is unchanged for them).
#[test]
fn c5_c6_reject_definite_type_contradictions() {
    // C6: a cond that is statically a string can never hold.
    expect_code(
        r#"
flow: t
provider: devicerail
steps:
  - id: s
    find: { element: { identifier: "x" } }
  - id: b
    if: ${{ concat("a", "b") }}
    then:
      - id: t1
        find: { element: { identifier: "y" } }
"#,
        codes::CHECK_PREDICATE_NOT_BOOLEAN,
    );
    // C5: `and` over a number literal.
    expect_code(
        r#"
flow: t
provider: devicerail
steps:
  - id: s
    find: { element: { identifier: "x" } }
    expect:
      - expr: ${{ and(eq(steps.s.output.ok, true), 5) }}
"#,
        codes::CHECK_TYPE_MISMATCH,
    );
    // C5: eq over two definite, different types.
    expect_code(
        r#"
flow: t
provider: devicerail
params:
  mode:
    schema: { type: string }
    required: true
steps:
  - id: s
    find: { element: { identifier: "x" } }
    expect:
      - expr: ${{ eq(params.mode, 5) }}
"#,
        codes::CHECK_TYPE_MISMATCH,
    );
    // Gradual: an Unknown-typed reference in a boolean position is legal.
    compile_android(
        r#"
flow: t
provider: devicerail
steps:
  - id: s
    find: { element: { identifier: "x" } }
    expect:
      - expr: ${{ and(eq(steps.s.output.ok, true), steps.s.output.ready) }}
"#,
    )
    .expect("unknown-typed positions stay legal");
}

/// RF3019 (warning): `retry_on` entries the retry machinery never fires on
/// are flagged without failing the compile.
#[test]
fn dead_retry_on_entries_warn_but_compile() {
    let sealed = compile_android(
        r#"
flow: t
provider: devicerail
steps:
  - id: s
    invoke:
      action: tapElement
      args:
        target: { kind: selector, selector: { identifier: "x" } }
    effect: mutating
    retry:
      max_attempts: 2
      backoff_ms: 10
      retry_on: [action_failed_retryable, action_timed_out, session_degraded]
"#,
    )
    .expect("warnings never fail a compile");
    let dead: Vec<&str> = sealed
        .warnings
        .iter()
        .filter(|diag| diag.code == codes::LINT_DEAD_RETRY_ON)
        .map(|diag| diag.message.as_str())
        .collect();
    // Non-idempotent step: both `action_timed_out` and `session_degraded`
    // are dead; `action_failed_retryable` is live and unflagged.
    assert_eq!(dead.len(), 2, "{dead:?}");
    assert!(
        dead.iter()
            .any(|message| message.contains("action_timed_out"))
    );
    assert!(
        dead.iter()
            .any(|message| message.contains("session_degraded"))
    );
}

/// 03 §4.4: an unknown action carries edit-distance `candidates` and a
/// `hint`; the pretty renderer prints the locator, the source line, a caret
/// run, and both trailers.
#[test]
fn unknown_action_carries_candidates_and_renders_pretty() {
    let source = r#"
flow: t
provider: devicerail
steps:
  - id: s
    invoke: { action: tapElemnt, args: { target: { identifier: "x" } } }
    effect: mutating
"#;
    let diags = compile_android(source).expect_err("tapElemnt is not a capability");
    let miss = diags
        .iter()
        .find(|diag| diag.code == codes::BIND_UNKNOWN_ACTION)
        .expect("the capability miss is diagnosed");
    assert_eq!(miss.candidates, vec!["tapElement".to_owned()], "{miss:?}");
    assert!(miss.hint.is_some());
    assert_eq!(miss.stage, "bind");

    let pretty = pointlock_compiler::render_pretty(&diags, source);
    assert!(pretty.contains("error RF4004 (bind):"), "{pretty}");
    assert!(pretty.contains("--> "), "{pretty}");
    assert!(pretty.contains("^"), "{pretty}");
    assert!(pretty.contains("候选: tapElement"), "{pretty}");
    assert!(pretty.contains("提示:"), "{pretty}");
}

// ─── verdict.record.v1 rides every IR (04 §9.2) ─────────────────────────────

#[test]
fn every_ir_requires_verdict_record() {
    // 04 §9.2: `verdict.record.v1` goes into `requiredFeatures`
    // unconditionally — that is what makes the daemon-side
    // capability-drift arm unreachable by construction.
    let sealed = compile_with(WEB_TAP_FLOW, &m1_manifest(), PlatformKind::Web)
        .expect("the tap flow must compile");
    assert!(
        sealed
            .flow_ir
            .required_features
            .contains(&feature("verdict.record.v1")),
        "verdict.record.v1 must ride every IR: {:?}",
        sealed.flow_ir.required_features
    );
}

#[test]
fn a_daemon_without_verdict_record_fails_compile_rf4008() {
    // Negative control: strip the feature from the fixture manifest —
    // the compile-time guarantee must fail closed, not silently drop
    // the requirement.
    let mut manifest = m1_manifest();
    manifest
        .features
        .guaranteed
        .retain(|id| id.as_str() != "verdict.record.v1");
    let diags = compile_with(WEB_TAP_FLOW, &manifest, PlatformKind::Web)
        .expect_err("a verdict-archival-less daemon must not compile");
    assert!(
        diags
            .iter()
            .any(|diag| diag.code == codes::BIND_FEATURE_UNAVAILABLE
                && diag.message.contains("verdict.record.v1")),
        "expected the RF4008 verdict.record refusal, got: {diags:#?}"
    );
}
