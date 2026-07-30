//! M0 end-to-end pipeline tests: a three-step demo flow over a fake
//! `devicerail` capability set (task requirement — the FakeProvider
//! manifest carries `name: "fake"` while `FlowIR.provider.name` is the
//! const `"devicerail"`, so the test builds its own
//! `ActionDefinitionStatic` set), plus one negative per rejection class
//! with its expected RF code.

use std::collections::BTreeMap;

use pointlock_compiler::{
    BindingActionSource, CompileDiagnostic, CompileOptions, Sealed, codes, compile,
};
use pointlock_ir::vocab::EffectClassAction;
use pointlock_ir::{
    ActionName, CanonicalVerb, Channel, FeatureId, Hash, JsonSchemaDocument, StepIR,
};
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

fn action(
    name: &str,
    input_schema: serde_json::Value,
    protection: ActionProtection,
) -> ActionDefinitionStatic {
    ActionDefinitionStatic {
        name: ActionName::new(name).expect("grammatical action name"),
        input_schema: JsonSchemaDocument::new(input_schema).expect("valid schema document"),
        output_schema: None,
        protection,
        synthetic: false,
    }
}

/// A fake devicerail capability set: `tap` / `setValue` / `readState` plus
/// one protected action for the R6 negative.
fn test_actions() -> Vec<ActionDefinitionStatic> {
    vec![
        action(
            "tap",
            json!({
                "type": "object",
                "properties": { "elementId": { "type": "string", "minLength": 1 } },
                "required": ["elementId"],
                "additionalProperties": false
            }),
            ActionProtection::Standard,
        ),
        action(
            "setValue",
            json!({
                "type": "object",
                "properties": {
                    "elementId": { "type": "string", "minLength": 1 },
                    "value": { "type": "string" }
                },
                "required": ["elementId", "value"],
                "additionalProperties": false
            }),
            ActionProtection::Standard,
        ),
        action(
            "readState",
            json!({
                "type": "object",
                "properties": { "elementId": { "type": "string", "minLength": 1 } },
                "required": ["elementId"],
                "additionalProperties": false
            }),
            ActionProtection::Standard,
        ),
        action(
            "injectCredentials",
            json!({ "type": "object" }),
            ActionProtection::Protected,
        ),
    ]
}

fn test_manifest() -> ProviderManifest {
    let semantic = feature("device.semanticActions.v1");
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
                semantic.clone(),
                feature("observation.uiSnapshot.v1"),
                // Every IR requires verdict write-back (04 §9.2); the
                // fixture models the fake daemon, which guarantees it.
                feature("verdict.record.v1"),
            ],
            conditional: Vec::new(),
        },
        verb_bindings: vec![VerbBinding {
            verb: CanonicalVerb::Tap,
            action_name: ActionName::new("tap").expect("grammatical action name"),
            requires_feature: Some(semantic),
            arg_map: BTreeMap::from([("element".to_owned(), "elementId".to_owned())]),
        }],
        channels: vec![ChannelSupport {
            channel: Channel::UiTree,
            role: ChannelRole::Both,
            requires_feature: Some(feature("observation.uiSnapshot.v1")),
            requires_platform: None,
        }],
        known_actions: test_actions(),
    }
}

fn test_lockfile(manifest: &ProviderManifest) -> CapabilityLockfile {
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
            platform: PlatformKind::Android,
            actions: manifest.known_actions.clone(),
        },
        digest: Hash::new(format!("sha256:{}", "0".repeat(64))).expect("grammatical placeholder"),
    };
    lockfile.seal();
    lockfile
}

const DEMO_FLOW: &str = r#"
flow: wifi_demo
provider: devicerail
params:
  ssid:
    schema: { type: string, minLength: 1 }
    required: true
  note:
    schema: { type: string }
    required: false
    default: smoke test
steps:
  - id: open_panel
    invoke:
      action: tap
      args:
        elementId: wifi_row
    effect: mutating
    idempotent: true
  - id: set_ssid
    invoke:
      action: setValue
      args:
        elementId: ssid_field
        value: ${{ params.ssid }}
    effect: mutating
    timeout_ms: 5000
  - id: read_back
    invoke:
      action: readState
      args:
        elementId: ssid_field
    effect: readonly
    checkpoint: false
    expect_schema:
      type: object
      properties:
        value: { type: string }
    expect:
      - expr: ${{ eq(steps.read_back.output.value, params.ssid) }}
      - assert_id: panel_was_opened
        expr: ${{ eq(steps.open_panel.verdict, "pass") }}
"#;

fn compile_demo() -> Result<Sealed, Vec<CompileDiagnostic>> {
    let manifest = test_manifest();
    let lockfile = test_lockfile(&manifest);
    compile(
        DEMO_FLOW,
        &CompileOptions {
            source_name: "wifi_demo.flow.yaml",
            manifest: &manifest,
            lockfile: Some(&lockfile),
        },
    )
}

/// Asserts that compilation fails and some diagnostic carries `code`.
fn expect_code(source: &str, code: &str) {
    let manifest = test_manifest();
    let lockfile = test_lockfile(&manifest);
    let diags = compile(
        source,
        &CompileOptions {
            source_name: "case.flow.yaml",
            manifest: &manifest,
            lockfile: Some(&lockfile),
        },
    )
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

#[test]
fn demo_flow_compiles_and_passes_the_generated_schema() {
    let sealed = compile_demo().expect("the demo flow must compile");
    let flow = &sealed.flow_ir;

    // Explicit factory re-check in the test (seal already enforces it).
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

    // Structure spot checks.
    assert_eq!(flow.flow_id.as_str(), "wifi_demo");
    assert_eq!(flow.body.len(), 3);
    assert_eq!(flow.params.len(), 2);
    assert!(
        flow.required_features
            .contains(&feature("device.semanticActions.v1")),
        "verb-bound feature must be collected"
    );
    assert!(
        flow.required_features
            .contains(&feature("observation.uiSnapshot.v1")),
        "channel feature must be collected"
    );
    let StepIR::Action(read_back) = &flow.body[2] else {
        panic!("M0 steps are action steps");
    };
    assert_eq!(read_back.effect, EffectClassAction::Readonly);
    assert!(!read_back.base.checkpoint);
    assert_eq!(read_back.assertions.len(), 2);
    assert_eq!(
        read_back.assertions[1].assert_id.as_str(),
        "panel_was_opened"
    );
    assert!(read_back.assertions.iter().all(|a| a.verify_via.is_empty()));
    assert_eq!(read_back.binding.attempts.len(), 1);
    assert_eq!(
        read_back.binding.attempts[0].action_name.as_str(),
        "readState"
    );

    // Materialized defaults.
    let StepIR::Action(set_ssid) = &flow.body[1] else {
        panic!("M0 steps are action steps");
    };
    assert!(set_ssid.base.checkpoint, "checkpoint defaults to true");
    assert!(!set_ssid.idempotent, "idempotent defaults to false");
    assert_eq!(set_ssid.base.timeout_ms, Some(5000));

    // Source map: one entry per step with a YAML span.
    assert_eq!(flow.source_map.len(), 3);
    assert_eq!(flow.source_map[0].ir_path.as_str(), "/body/0");
    assert_eq!(flow.source_map[0].file, "wifi_demo.flow.yaml");

    // Binding report facts.
    let report = &sealed.binding_report;
    assert_eq!(report.action_source, BindingActionSource::LockfileDevice);
    assert_eq!(report.steps.len(), 3);
    assert_eq!(report.steps[1].action_name.as_str(), "setValue");
    assert!(
        report.steps[1]
            .statically_validated_args
            .contains(&"elementId".to_owned()),
        "literal arg must be statically validated"
    );
    assert!(
        report.steps[1]
            .runtime_deferred_args
            .contains(&"value".to_owned()),
        "expression arg must defer to runtime validation"
    );

    // Lockfile digest embedded.
    let manifest = test_manifest();
    assert_eq!(flow.lockfile_digest, test_lockfile(&manifest).digest);
}

#[test]
fn ir_hash_is_deterministic_across_compiles() {
    let first = compile_demo().expect("compiles");
    let second = compile_demo().expect("compiles");
    assert_eq!(first.flow_ir.ir_hash, second.flow_ir.ir_hash);
    assert_eq!(first.flow_ir, second.flow_ir);
    // Self-check: the stored hash equals a recomputation.
    assert_eq!(first.flow_ir.ir_hash, pointlock_ir::ir_hash(&first.flow_ir));
}

#[test]
fn unknown_action_is_rejected_rf4004() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: swipe
    invoke: { action: swipeElement, args: {} }
    effect: mutating
"#,
        codes::BIND_UNKNOWN_ACTION,
    );
}

#[test]
fn protected_action_is_rejected_rf4005() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: inject
    invoke: { action: injectCredentials, args: {} }
    effect: mutating
"#,
        codes::BIND_PROTECTED_ACTION,
    );
}

#[test]
fn forward_reference_is_rejected_rf3007() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: first
    invoke:
      action: setValue
      args:
        elementId: field
        value: ${{ steps.second.output.value }}
    effect: mutating
  - id: second
    invoke:
      action: readState
      args: { elementId: field }
    effect: readonly
"#,
        codes::CHECK_FORWARD_REF,
    );
}

#[test]
fn arity_mismatch_is_rejected_rf3010() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke: { action: readState, args: { elementId: field } }
    effect: readonly
    expect:
      - expr: ${{ not(true, false) }}
"#,
        codes::CHECK_ARITY,
    );
}

#[test]
fn unknown_top_level_key_is_rejected_rf2001() {
    expect_code(
        r#"
flow: bad
provider: devicerail
pipeline: fancy
steps:
  - id: probe
    invoke: { action: readState, args: { elementId: field } }
    effect: readonly
"#,
        codes::NORM_UNKNOWN_TOP_KEY,
    );
}

#[test]
fn unimplemented_subset_keywords_are_diagnosed_not_ignored() {
    // Step-level `outputs` projection (recognized, outside M2).
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke: { action: readState, args: { elementId: field } }
    effect: readonly
    outputs: { value: 1 }
"#,
        codes::NORM_STEP_KEY_NOT_IMPLEMENTED,
    );
    // M2 implements the surfaces below — they stay fail-closed on their
    // own shape rules instead of a subset boundary.
    // `call` referencing a file that does not exist.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: login
    call: ./flows/login.flow.yaml
"#,
        codes::NORM_SUBFLOW_LOAD,
    );
    // A macro definition without a body.
    expect_code(
        r#"
flow: bad
provider: devicerail
macros:
  fill: { params: [], steps: [] }
steps:
  - id: probe
    invoke: { action: readState, args: { elementId: field } }
    effect: readonly
"#,
        codes::NORM_BAD_MACRO_DEF,
    );
    // An incomplete `retry` policy.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke: { action: readState, args: { elementId: field } }
    effect: readonly
    retry: { max_attempts: 2 }
"#,
        codes::NORM_BAD_RETRY_SHAPE,
    );
    // `locate_via` needs a verb head key; on `invoke` it is refused.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke: { action: readState, args: { elementId: field } }
    effect: readonly
    locate_via: [uiTree]
"#,
        codes::NORM_BAD_FALLBACK_SHAPE,
    );
}

#[test]
fn more_rejections_by_stage() {
    // parse: merge key.
    expect_code(
        "base: &b { x: 1 }\nflow: bad\nprovider: devicerail\nsteps:\n  - id: a\n    <<: *b\n",
        codes::PARSE_MERGE_KEY,
    );
    // normalize: effect value out of the enum.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke: { action: readState, args: { elementId: field } }
    effect: pure
"#,
        codes::NORM_BAD_EFFECT_VALUE,
    );
    // normalize: interpolation inside a composite literal member stays
    // refused (the delivered sugar covers top-level string scalars only).
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke:
      action: setValue
      args:
        elementId: field
        value: { nested: "hello ${{ params.ssid }}!" }
    effect: mutating
"#,
        codes::NORM_INTERPOLATION_INVALID,
    );
    // check: reference to an undefined step.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke:
      action: setValue
      args:
        elementId: field
        value: ${{ steps.ghost.output.value }}
    effect: mutating
"#,
        codes::CHECK_UNDEFINED_STEP,
    );
    // check: illegal self-reference in argument position.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke:
      action: setValue
      args:
        elementId: field
        value: ${{ steps.probe.output.value }}
    effect: mutating
"#,
        codes::CHECK_ILLEGAL_SELF_REF,
    );
    // check: undeclared param.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke:
      action: setValue
      args:
        elementId: field
        value: ${{ params.ghost }}
    effect: mutating
"#,
        codes::CHECK_UNDECLARED_NAME,
    );
    // bind: missing effect on invoke.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke: { action: readState, args: { elementId: field } }
"#,
        codes::BIND_EFFECT_REQUIRED,
    );
    // bind: static arg shape violation (literal fails the subschema).
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke: { action: readState, args: { elementId: "" } }
    effect: readonly
"#,
        codes::BIND_ARGS_SHAPE,
    );
    // bind: unknown argument under additionalProperties: false.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke: { action: readState, args: { elementId: field, wrong: 1 } }
    effect: readonly
"#,
        codes::BIND_ARGS_SHAPE,
    );
}

#[test]
fn manifest_fallback_binds_guaranteed_actions_only() {
    let manifest = test_manifest();
    // Without a lockfile the demo still compiles (all features guaranteed)…
    let sealed = compile(
        DEMO_FLOW,
        &CompileOptions {
            source_name: "wifi_demo.flow.yaml",
            manifest: &manifest,
            lockfile: None,
        },
    )
    .expect("manifest fallback must bind guaranteed actions");
    assert_eq!(
        sealed.binding_report.action_source,
        BindingActionSource::ManifestGuaranteed
    );

    // …but an action whose feature is not guaranteed is refused.
    let mut gated = test_manifest();
    gated.features.guaranteed = vec![feature("observation.uiSnapshot.v1")];
    let diags = compile(
        DEMO_FLOW,
        &CompileOptions {
            source_name: "wifi_demo.flow.yaml",
            manifest: &gated,
            lockfile: None,
        },
    )
    .expect_err("tap requires device.semanticActions.v1, which is no longer guaranteed");
    assert!(
        diags
            .iter()
            .any(|diag| diag.code == codes::BIND_ACTION_NOT_GUARANTEED),
        "expected {}, got: {diags:#?}",
        codes::BIND_ACTION_NOT_GUARANTEED
    );
}

#[test]
fn diagnostics_serialize_with_the_wire_shape() {
    let manifest = test_manifest();
    let lockfile = test_lockfile(&manifest);
    let diags = compile(
        "flow: bad\nprovider: devicerail\nsteps:\n  - id: a\n    invoke: { action: readState, args: {} }\n    effect: readonly\n    outputs: { seen: 1 }\n",
        &CompileOptions {
            source_name: "case.flow.yaml",
            manifest: &manifest,
            lockfile: Some(&lockfile),
        },
    )
    .expect_err("must fail");
    let wire = serde_json::to_value(&diags[0]).expect("diagnostic serializes");
    assert_eq!(wire["code"], "RF2015");
    assert_eq!(wire["severity"], "error");
    assert!(wire["message"].as_str().expect("message").contains("M2"));
    let span = &wire["span"];
    assert!(span["line"].is_u64());
    assert!(span["col"].is_u64());
    assert!(span["endLine"].is_u64());
    assert!(span["endCol"].is_u64());
}
