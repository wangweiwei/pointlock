//! M2 compile-surface tests: the full 03 §1 authoring surface.
//!
//! The anchor is the canonical 02 §13.1 flow, transcribed verbatim with
//! one documented deviation: the four `${{ ... }}` scalars that sit inside
//! *flow-context* collections (`set_value: { ... }`, `inputs: { ... }`,
//! `presents: [ ... ]`, `let: { ... }`) are double-quoted here. As printed
//! in 02 §13.1 they are not valid YAML 1.2 — plain scalars in flow context
//! must not contain `{`/`}`/`,` — and the fail-closed parser (03 §1.9,
//! saphyr) rejects them, as does every conformant parser. Upstream doc gap
//! reported with this milestone.
//!
//! Alongside the anchor: structure-step positives (if/foreach/let/iter),
//! the R13 warning lint, preflight's judgeHash membership, and one
//! negative per new rejection class with its expected RF code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pointlock_compiler::{CompileDiagnostic, CompileOptions, Sealed, Severity, codes, compile};
use pointlock_ir::vocab::{
    ActChannel, CanonicalVerb, Channel, EffectClassAction, ElementState, HandlerHook, HumanMode,
    TextMatchMode, VerdictPolicy, VerifyChannel,
};
use pointlock_ir::{
    ActionName, BackoffMs, FeatureId, HandlerAction, Hash, JsonSchemaDocument, PredicateIR, StepIR,
};
use pointlock_provider_kit::lockfile::{
    CapabilityLockfile, LockfileDevice, LockfileHello, LockfileProvider, PeerInfo, ProtocolVersion,
};
use pointlock_provider_kit::manifest::{
    ActionDefinitionStatic, ActionProtection, ChannelRole, ChannelSupport, FeatureDeclarations,
    PlatformKind, ProtocolRange, ProviderManifest, VerbBinding,
};
use serde_json::json;

// ─── capability fixtures (the M1 semantic-five set, unchanged) ──────────────

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

/// A provider-synthetic readonly action (04 §9.4.3): the provider serves
/// it from `observe()`, so no driver declares it and it must stay out of
/// the lockfile — bind overlays it onto the capability set.
fn synthetic(name: &str, input_schema: serde_json::Value) -> ActionDefinitionStatic {
    ActionDefinitionStatic {
        name: action_name(name),
        input_schema: JsonSchemaDocument::new(input_schema).expect("valid schema document"),
        output_schema: None,
        protection: ActionProtection::Standard,
        synthetic: true,
    }
}

fn m2_actions() -> Vec<ActionDefinitionStatic> {
    vec![
        synthetic(
            "observe",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["wants"],
                "properties": {
                    "wants": {
                        "type": "array",
                        "minItems": 1,
                        "uniqueItems": true,
                        "items": { "enum": ["screenshot", "uiSnapshot"] }
                    }
                }
            }),
        ),
        synthetic(
            "screenshot",
            json!({ "type": "object", "additionalProperties": false, "properties": {} }),
        ),
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

fn m2_manifest() -> ProviderManifest {
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
            VerbBinding {
                verb: CanonicalVerb::Observe,
                action_name: action_name("observe"),
                requires_feature: None,
                arg_map: BTreeMap::from([("wants".to_owned(), "wants".to_owned())]),
            },
            VerbBinding {
                verb: CanonicalVerb::Screenshot,
                action_name: action_name("screenshot"),
                requires_feature: None,
                arg_map: BTreeMap::new(),
            },
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
        known_actions: m2_actions(),
    }
}

fn m2_lockfile(manifest: &ProviderManifest) -> CapabilityLockfile {
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
            // A lockfile lists DRIVER actions; the synthetic ones are the
            // provider's own and get overlaid at bind time.
            actions: manifest
                .known_actions
                .iter()
                .filter(|action| !action.synthetic)
                .cloned()
                .collect(),
        },
        digest: Hash::new(format!("sha256:{}", "0".repeat(64))).expect("grammatical placeholder"),
    };
    lockfile.seal();
    lockfile
}

// ─── harness ────────────────────────────────────────────────────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique per-test scratch directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-compile-m2-{tag}-{}-{}",
            std::process::id(),
            DIR_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    /// Writes a file under the temp root, creating parent directories.
    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(&path, content).expect("write flow file");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Compiles a source string with `source_name` naming a real or virtual
/// file (subflow references resolve against it).
fn compile_named(source: &str, source_name: &str) -> Result<Sealed, Vec<CompileDiagnostic>> {
    let manifest = m2_manifest();
    let lockfile = m2_lockfile(&manifest);
    compile(
        source,
        &CompileOptions {
            source_name,
            manifest: &manifest,
            lockfile: Some(&lockfile),
        },
    )
}

/// Compiles a flow file on disk.
fn compile_file(path: &Path) -> Result<Sealed, Vec<CompileDiagnostic>> {
    let source = std::fs::read_to_string(path).expect("read flow file");
    compile_named(&source, &path.display().to_string())
}

/// Asserts that compilation fails and some diagnostic carries `code`,
/// with a span (03 §4.4 hard rule).
fn expect_code(source: &str, code: &str) {
    let diags = compile_named(source, "case.flow.yaml")
        .err()
        .unwrap_or_else(|| panic!("expected a {code} rejection, got a sealed artifact"));
    assert!(
        diags.iter().any(|diag| diag.code == code),
        "expected code {code}, got: {diags:#?}"
    );
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
        panic!("expected an action step, got kind '{}'", step.kind());
    };
    action
}

fn attempt_args_json(attempt: &pointlock_ir::BoundAttempt) -> serde_json::Value {
    serde_json::to_value(&attempt.args).expect("args serialize")
}

fn assert_schema_valid(flow: &pointlock_ir::FlowIR) {
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
}

// ─── the 02 §13.1 canonical flow ────────────────────────────────────────────

/// 02 §13.1, verbatim except the four flow-context `${{ }}` scalars are
/// double-quoted (see the module docs — the printed form is not valid
/// YAML 1.2; upstream doc gap).
const CANONICAL_13_1: &str = r#"flow: wifi_toggle
provider: devicerail
verdict_policy: strict

params:
  ssid:     { schema: { type: string, minLength: 1 }, required: true }
  username: { schema: { type: string }, required: false, default: qa-bot }

outputs:
  wifi_verdict:
    schema: { enum: [pass, fail, unknown] }
    from: ${{ steps.wifi_on_visible.verdict }}

macros:
  fill_field:                            # 编译期蒸发（§7）；params 命名与 03 §1.7 的同名宏一致
    params: [element, value]
    steps:
      - id: field                        # 展开为 set_ssid:field（hygiene 合成前缀，§7）
        set_value: { element: "${{ macro.element }}", value: "${{ macro.value }}" }
        idempotent: true                 # 刻意不写 expect：展开步 assertions: [] → unverified（R4）

handlers:
  on_unknown:
    - escalate:
        mode: judge
        prompt: 机器无法判定本步结果，请依据证据人工裁决
        decisions: [pass, fail, unknown]
        timeout_ms: 3600000
        on_timeout: unknown
      max_triggers: 1

steps:
  - id: open_wifi_settings
    preflight:
      - element: { identifier: settings_root }
        state: visible
        verify_via: [uiTree]
    tap: { element: { identifier: wifi_row } }
    locate_via: [uiTree, coordinate]
    coordinate: { x: 512, y: 384 }
    effect: mutating
    idempotent: true
    timeout_ms: 15000
    retry:
      max_attempts: 2
      backoff_ms: { initial: 500, factor: 2, max: 4000 }
      retry_on: [action_failed_retryable, target_stale]
    expect:
      - element: { identifier: wifi_toggle }
        state: visible
        verify_via: [uiTree, vision]
        visual: "Wi-Fi 设置页已打开，Wi-Fi 开关控件在屏幕上可见"   # 链尾含 vision 时必填（03 §1.4 规则 5）

  - id: set_ssid                         # macro 调用：head key = 宏名（03 §1.2 分派第 4 支）
    fill_field:
      element: { identifier: ssid_field }
      value: ${{ params.ssid }}

  - id: wifi_on_visible
    expect:
      - element: { identifier: wifi_toggle }
        state: enabled
        verify_via: [uiTree, vision]
        visual: "Wi-Fi 开关处于打开（enabled）状态"
      - element: { identifier: current_network_label }
        text: { value: Pointlock-Lab, mode: contains }
        verify_via: [uiTree]

  - id: ensure_session
    call: ./flows/ensure-logged-in.flow.yaml   # 相对路径；normalize 独立编译并按 irHash 锁定（03 §1.6）
    inputs: { username: "${{ params.username }}" }

  - id: wait_if_passed
    if: ${{ eq(steps.wifi_on_visible.verdict, 'pass') }}
    then:
      - id: wait_connected
        wait_for: { element: { identifier: connected_banner }, state: visible }
        timeout_ms: 30000
        expect:
          - expr: ${{ eq(steps.wait_connected.output.matched, true) }}   # 本步 assertions 内 self-ref（§4.1.1）

  - id: confirm_wifi
    human:
      mode: confirm
      prompt: 确认设备已连接到目标 Wi-Fi 网络
      presents: ["${{ steps.wifi_on_visible.verdict }}", "${{ params.ssid }}"]
      decisions: [confirmed, rejected]
      on_timeout: unknown
    timeout_ms: 600000

  - id: label
    let: { report_label: "${{ concat('wifi:', params.ssid) }}" }
"#;

/// The `ensure_logged_in` callee referenced by the canonical flow.
const ENSURE_LOGGED_IN: &str = r#"flow: ensure_logged_in
provider: devicerail
params:
  username: { schema: { type: string }, required: true }
outputs:
  ok:
    schema: { type: boolean }
    from: ${{ eq(steps.probe_home.output.matched, true) }}
steps:
  - id: probe_home
    wait_for:
      element: { identifier: home_tab }
      state: visible
    timeout_ms: 5000
"#;

fn compile_canonical() -> (Sealed, PathBuf, TempDir) {
    let dir = TempDir::new("canonical");
    let callee = dir.write("flows/ensure-logged-in.flow.yaml", ENSURE_LOGGED_IN);
    let main = dir.write("wifi_toggle.flow.yaml", CANONICAL_13_1);
    let sealed = match compile_file(&main) {
        Ok(sealed) => sealed,
        Err(diags) => panic!("the canonical 02 §13.1 flow must compile, got: {diags:#?}"),
    };
    (sealed, callee, dir)
}

#[test]
fn canonical_13_1_compiles_to_the_13_2_structure() {
    let (sealed, callee_path, _dir) = compile_canonical();
    let flow = &sealed.flow_ir;

    // Factory schema check, explicitly re-run in the test (R12).
    assert_schema_valid(flow);

    // Top level (02 §13.2).
    assert_eq!(flow.flow_id.as_str(), "wifi_toggle");
    assert_eq!(flow.verdict_policy, VerdictPolicy::Strict);
    assert_eq!(flow.params.len(), 2);
    assert_eq!(flow.params[1].default, Some(json!("qa-bot")));
    assert_eq!(flow.outputs.len(), 1);
    assert_eq!(flow.outputs[0].name.as_str(), "wifi_verdict");
    assert_eq!(
        serde_json::to_value(&flow.outputs[0].from).expect("expr serializes"),
        json!({ "ref": "steps.wifi_on_visible.verdict" })
    );

    // Step kind sequence (02 §13.2 body order).
    let kinds: Vec<&str> = flow.body.iter().map(StepIR::kind).collect();
    assert_eq!(
        kinds,
        vec!["action", "action", "assert", "call", "if", "human", "let"]
    );

    // open_wifi_settings: dual-attempt act-chain, preflight, retry.
    let open = action_step(&flow.body[0]);
    assert_eq!(open.base.step_id.as_str(), "open_wifi_settings");
    assert!(open.base.checkpoint);
    assert_eq!(open.base.timeout_ms, Some(15000));
    assert_eq!(open.verb, Some(CanonicalVerb::Tap));
    assert_eq!(open.effect, EffectClassAction::Mutating);
    assert!(open.idempotent);
    let preflight = open.base.preflight.as_ref().expect("preflight present");
    assert_eq!(preflight.len(), 1);
    assert_eq!(preflight[0].verify_via, vec![VerifyChannel::UiTree]);
    assert!(matches!(
        &preflight[0].predicate,
        PredicateIR::ElementState {
            state: ElementState::Visible,
            ..
        }
    ));
    let retry = open.base.retry.as_ref().expect("retry present");
    assert_eq!(retry.max_attempts, 2);
    assert!(matches!(retry.backoff_ms, BackoffMs::Schedule(_)));
    assert_eq!(retry.retry_on.len(), 2);
    let attempts = &open.binding.attempts;
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].channel, ActChannel::UiTree);
    assert_eq!(attempts[0].action_name.as_str(), "tapElement");
    assert_eq!(attempts[1].channel, ActChannel::Coordinate);
    assert_eq!(attempts[1].action_name.as_str(), "tap");
    assert_eq!(
        attempt_args_json(&attempts[1]),
        json!({ "x": { "lit": 512 }, "y": { "lit": 384 } })
    );
    assert_eq!(open.assertions.len(), 1);
    assert_eq!(
        open.assertions[0].verify_via,
        vec![VerifyChannel::UiTree, VerifyChannel::Vision]
    );
    assert_eq!(
        open.assertions[0].vision_prompt.as_deref(),
        Some("Wi-Fi 设置页已打开，Wi-Fi 开关控件在屏幕上可见")
    );

    // set_ssid:field — the hygienic macro expansion (02 §13.2 callouts):
    // synthesized `:` id, materialized `checkpoint: false`, empty
    // assertions (unverified, R4).
    let field = action_step(&flow.body[1]);
    assert_eq!(field.base.step_id.as_str(), "set_ssid:field");
    assert!(!field.base.checkpoint);
    assert_eq!(field.verb, Some(CanonicalVerb::SetValue));
    assert_eq!(field.effect, EffectClassAction::Mutating);
    assert!(field.idempotent);
    assert!(field.assertions.is_empty());
    assert_eq!(
        attempt_args_json(&field.binding.attempts[0]),
        json!({
            "target": { "lit": {
                "kind": "selector",
                "selector": { "identifier": "ssid_field" }
            } },
            "value": { "ref": "params.ssid" }
        })
    );

    // wifi_on_visible — the expect-only assert step.
    let StepIR::Assert(assert_step) = &flow.body[2] else {
        panic!("wifi_on_visible must be an assert step");
    };
    assert_eq!(
        serde_json::to_value(&assert_step.observe).expect("observe serializes"),
        json!("fresh")
    );
    assert_eq!(assert_step.assertions.len(), 2);
    let PredicateIR::ElementText { r#match, .. } = &assert_step.assertions[1].predicate else {
        panic!("expected an elementText predicate");
    };
    assert_eq!(r#match.value, "Pointlock-Lab");
    assert_eq!(r#match.mode, TextMatchMode::Contains);
    assert!(!r#match.case_sensitive);

    // ensure_session — the call step and the subflows registry pin.
    let StepIR::Call(call) = &flow.body[3] else {
        panic!("ensure_session must be a call step");
    };
    assert_eq!(call.flow_ref.flow_id.as_str(), "ensure_logged_in");
    assert_eq!(
        serde_json::to_value(&call.inputs).expect("inputs serialize"),
        json!({ "username": { "ref": "params.username" } })
    );
    assert_eq!(flow.subflows.len(), 1);
    let registry = flow
        .subflows
        .get(&call.flow_ref.flow_id)
        .expect("registry entry for the callee");
    assert_eq!(registry.ir_hash, call.flow_ref.ir_hash);
    // The pin equals an independent compile of the callee (deterministic
    // link closure, 02 §6).
    let standalone = compile_file(&callee_path).expect("the callee compiles standalone");
    assert_eq!(standalone.flow_ir.ir_hash, registry.ir_hash);
    // The callee's features were merged into the caller (02 §6 item 5).
    for needed in &standalone.flow_ir.required_features {
        assert!(flow.required_features.contains(needed));
    }

    // wait_if_passed — the if container with the nested action step.
    let StepIR::If(if_step) = &flow.body[4] else {
        panic!("wait_if_passed must be an if step");
    };
    assert_eq!(
        serde_json::to_value(&if_step.cond).expect("cond serializes"),
        json!({ "fn": "eq",
                "args": [ { "ref": "steps.wifi_on_visible.verdict" }, { "lit": "pass" } ] })
    );
    assert!(if_step.r#else.is_none());
    assert_eq!(if_step.then.len(), 1);
    let wait_connected = action_step(&if_step.then[0]);
    assert_eq!(wait_connected.base.step_id.as_str(), "wait_connected");
    assert_eq!(wait_connected.verb, Some(CanonicalVerb::WaitFor));
    assert_eq!(wait_connected.base.timeout_ms, Some(30000));
    // The self-ref expr assertion (02 §4.1.1) with the empty verify-chain.
    assert_eq!(wait_connected.assertions.len(), 1);
    assert!(wait_connected.assertions[0].verify_via.is_empty());
    assert!(matches!(
        &wait_connected.assertions[0].predicate,
        PredicateIR::Expr { .. }
    ));

    // confirm_wifi — the human step (timeout at step level resolves into
    // HumanStepIR.timeoutMs; no duplicate surface).
    let StepIR::Human(human) = &flow.body[5] else {
        panic!("confirm_wifi must be a human step");
    };
    assert_eq!(human.mode, HumanMode::Confirm);
    assert_eq!(human.timeout_ms, 600000);
    assert!(human.base.timeout_ms.is_none());
    assert_eq!(
        human.decisions,
        Some(vec!["confirmed".to_owned(), "rejected".to_owned()])
    );
    assert_eq!(
        serde_json::to_value(&human.presents).expect("presents serialize"),
        json!([
            { "ref": "steps.wifi_on_visible.verdict" },
            { "ref": "params.ssid" }
        ])
    );
    assert_eq!(
        serde_json::to_value(human.on_timeout).expect("onTimeout serializes"),
        json!("unknown")
    );

    // label — the let step.
    let StepIR::Let(let_step) = &flow.body[6] else {
        panic!("label must be a let step");
    };
    assert_eq!(
        serde_json::to_value(&let_step.bindings).expect("bindings serialize"),
        json!({ "report_label": { "fn": "concat",
                                  "args": [ { "lit": "wifi:" }, { "ref": "params.ssid" } ] } })
    );

    // Flow-level handler: escalate under the synthesized id (02 §13.2).
    let handlers = flow.handlers.as_ref().expect("flow handlers present");
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].hook, HandlerHook::OnUnknown);
    assert_eq!(handlers[0].max_triggers, 1);
    let HandlerAction::Escalate { human } = &handlers[0].action else {
        panic!("expected an escalate action");
    };
    assert_eq!(human.base.step_id.as_str(), "flow:onUnknown:escalate");
    assert_eq!(human.mode, HumanMode::Judge);
    assert_eq!(human.timeout_ms, 3600000);
    assert!(human.base.checkpoint);
    let placeholder = format!("sha256:{}", "0".repeat(64));
    assert_ne!(human.base.effect_hash.as_str(), placeholder);
    assert_ne!(human.base.judge_hash.as_str(), placeholder);

    // sourceMap: the expanded step maps back through its origin frame
    // (span = macro definition body, origin = call site; 02 §13.2).
    let expanded_entry = flow
        .source_map
        .iter()
        .find(|entry| entry.ir_path.as_str() == "/body/1")
        .expect("sourceMap entry for the expanded step");
    let origin = expanded_entry.origin.as_ref().expect("origin present");
    assert_eq!(origin.len(), 1);
    assert_eq!(origin[0].r#macro.as_str(), "fill_field");
    assert!(origin[0].file.ends_with("wifi_toggle.flow.yaml"));
    // The entry's own span points at the macro definition body (line 18
    // region), the origin frame at the call site (line 55 region).
    assert!(expanded_entry.span.start_line < origin[0].span.start_line);
    // Nested steps carry tree-shaped IR paths.
    assert!(
        flow.source_map
            .iter()
            .any(|entry| entry.ir_path.as_str() == "/body/4/then/0")
    );
    // Author-written steps carry no origin.
    assert!(
        flow.source_map
            .iter()
            .filter(|entry| entry.ir_path.as_str() != "/body/1")
            .all(|entry| entry.origin.is_none())
    );

    // The canonical flow declares an on_unknown disposition — the R13
    // lint stays silent (03 §4.3 F6).
    assert!(
        sealed.warnings.is_empty(),
        "unexpected warnings: {:#?}",
        sealed.warnings
    );
}

#[test]
fn canonical_13_1_ir_hash_is_deterministic() {
    let (first, _, _dir1) = compile_canonical();
    let (second, _, _dir2) = compile_canonical();
    assert_eq!(first.flow_ir.ir_hash, second.flow_ir.ir_hash);
    // Everything except sourceMap is identical (the two compiles live in
    // different temp roots, so sourceMap file paths differ — which is
    // exactly why sourceMap is excluded from irHash, 02 §12.2).
    let strip = |sealed: &Sealed| {
        let mut value = serde_json::to_value(&sealed.flow_ir).expect("FlowIR serializes");
        value
            .as_object_mut()
            .expect("FlowIR is an object")
            .remove("sourceMap");
        value
    };
    assert_eq!(strip(&first), strip(&second));
    assert_eq!(first.flow_ir.ir_hash, pointlock_ir::ir_hash(&first.flow_ir));
}

// ─── structure-step positives ───────────────────────────────────────────────

#[test]
fn foreach_let_and_iter_compile() {
    let sealed = compile_named(
        r#"
flow: structures
provider: devicerail
params:
  items: { schema: { type: array }, required: true }
handlers:
  on_unknown:
    - escalate:
        mode: judge
        prompt: "please judge"
        decisions: [pass, fail]
        timeout_ms: 1000
      max_triggers: 1
steps:
  - id: compose
    let:
      label: ${{ concat('run-', env.runId) }}
  - id: fill_each
    foreach:
      in: ${{ params.items }}
      as: item
      steps:
        - id: fill_one
          set_value:
            element: { identifier: field }
            value: ${{ iter.item }}
  - id: after
    set_value:
      element: { identifier: label_field }
      value: ${{ vars.label }}
"#,
        "structures.flow.yaml",
    )
    .expect("the structure flow must compile");
    let flow = &sealed.flow_ir;
    assert_schema_valid(flow);
    assert_eq!(
        flow.body.iter().map(StepIR::kind).collect::<Vec<_>>(),
        vec!["let", "foreach", "action"]
    );
    let StepIR::Foreach(foreach) = &flow.body[1] else {
        panic!("expected a foreach step");
    };
    assert_eq!(foreach.r#as.as_str(), "item");
    assert_eq!(
        serde_json::to_value(&foreach.items).expect("items serialize"),
        json!({ "ref": "params.items" })
    );
    assert_eq!(foreach.body.len(), 1);
    let fill_one = action_step(&foreach.body[0]);
    assert_eq!(
        attempt_args_json(&fill_one.binding.attempts[0])["value"],
        json!({ "ref": "iter.item" })
    );
    assert!(sealed.warnings.is_empty());
}

#[test]
fn preflight_sits_in_the_judge_hash_domain() {
    let base = r#"
flow: hash_probe
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: wifi_row }
"#;
    let with_preflight = r#"
flow: hash_probe
provider: devicerail
steps:
  - id: probe
    preflight:
      - element: { identifier: settings_root }
        state: visible
    find:
      element: { identifier: wifi_row }
"#;
    let plain = compile_named(base, "a.flow.yaml").expect("compiles");
    let probed = compile_named(with_preflight, "b.flow.yaml").expect("compiles");
    let plain_step = action_step(&plain.flow_ir.body[0]);
    let probed_step = action_step(&probed.flow_ir.body[0]);
    // Preflight is judge material, not effect material (02 §12.3).
    assert_eq!(plain_step.base.effect_hash, probed_step.base.effect_hash);
    assert_ne!(plain_step.base.judge_hash, probed_step.base.judge_hash);
}

#[test]
fn missing_unknown_disposition_warns_rf3016() {
    let sealed = compile_named(
        r#"
flow: silent
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: wifi_row }
"#,
        "silent.flow.yaml",
    )
    .expect("the flow compiles with a warning");
    assert_eq!(sealed.warnings.len(), 1);
    let warning = &sealed.warnings[0];
    assert_eq!(warning.code, codes::LINT_NO_UNKNOWN_DISPOSITION);
    assert_eq!(warning.severity, Severity::Warning);
    assert!(warning.span.is_some());

    // A step-level on_unknown handler silences the lint too.
    let sealed = compile_named(
        r#"
flow: handled
provider: devicerail
steps:
  - id: probe
    find:
      element: { identifier: wifi_row }
    on_unknown:
      escalate:
        mode: judge
        prompt: "please judge"
        decisions: [pass, fail]
        timeout_ms: 1000
      max_triggers: 1
"#,
        "handled.flow.yaml",
    )
    .expect("compiles");
    assert!(sealed.warnings.is_empty());
    let step_handlers = flow_step_handlers(&sealed.flow_ir.body[0]);
    assert_eq!(step_handlers.len(), 1);
    let HandlerAction::Escalate { human } = &step_handlers[0].action else {
        panic!("expected escalate");
    };
    assert_eq!(human.base.step_id.as_str(), "probe:onUnknown:escalate");
}

fn flow_step_handlers(step: &StepIR) -> &[pointlock_ir::HandlerBinding] {
    step.base()
        .handlers
        .as_deref()
        .expect("step handlers present")
}

#[test]
fn repair_handler_pins_the_subflow() {
    let dir = TempDir::new("repair");
    dir.write(
        "dismiss.flow.yaml",
        r#"flow: dismiss_dialog
provider: devicerail
steps:
  - id: probe_dialog
    wait_for:
      element: { role: button, name: "允许" }
      state: visible
    timeout_ms: 2000
"#,
    );
    let main = dir.write(
        "main.flow.yaml",
        r#"flow: repair_demo
provider: devicerail
steps:
  - id: tap_it
    tap:
      element: { identifier: wifi_row }
    on_fail:
      repair: ./dismiss.flow.yaml
      max_triggers: 2
"#,
    );
    let sealed = compile_file(&main).expect("the repair flow must compile");
    let flow = &sealed.flow_ir;
    assert_schema_valid(flow);
    let handlers = flow_step_handlers(&flow.body[0]);
    assert_eq!(handlers[0].hook, HandlerHook::OnFail);
    assert_eq!(handlers[0].max_triggers, 2);
    let HandlerAction::Repair { flow_ref } = &handlers[0].action else {
        panic!("expected a repair action");
    };
    assert_eq!(flow_ref.flow_id.as_str(), "dismiss_dialog");
    assert_eq!(
        flow.subflows
            .get(&flow_ref.flow_id)
            .expect("repair callee pinned in the registry")
            .ir_hash,
        flow_ref.ir_hash
    );
}

// ─── negatives ──────────────────────────────────────────────────────────────

#[test]
fn macro_recursion_is_rejected_rf2029() {
    expect_code(
        r#"
flow: bad
provider: devicerail
macros:
  loop_a:
    params: []
    steps:
      - id: inner
        loop_a: {}
steps:
  - id: go
    loop_a: {}
"#,
        codes::NORM_MACRO_RECURSION,
    );
    // Mutual recursion.
    expect_code(
        r#"
flow: bad
provider: devicerail
macros:
  ping:
    params: []
    steps:
      - id: p
        pong: {}
  pong:
    params: []
    steps:
      - id: q
        ping: {}
steps:
  - id: go
    ping: {}
"#,
        codes::NORM_MACRO_RECURSION,
    );
}

#[test]
fn macro_body_cross_step_ref_is_rejected_rf2031() {
    expect_code(
        r#"
flow: bad
provider: devicerail
macros:
  two:
    params: []
    steps:
      - id: first
        find: { element: { identifier: x } }
      - id: second
        set_value:
          element: { identifier: y }
          value: ${{ steps.first.output.matched }}
steps:
  - id: go
    two: {}
"#,
        codes::NORM_MACRO_CROSS_STEP_REF,
    );
}

#[test]
fn macro_param_surface_is_closed_rf2030() {
    // Unknown call-site argument.
    expect_code(
        r#"
flow: bad
provider: devicerail
macros:
  fill:
    params: [element]
    steps:
      - id: f
        clear: { element: "${{ macro.element }}" }
steps:
  - id: go
    fill:
      element: { identifier: x }
      extra: 1
"#,
        codes::NORM_BAD_MACRO_PARAM,
    );
    // Missing required param.
    expect_code(
        r#"
flow: bad
provider: devicerail
macros:
  fill:
    params: [element]
    steps:
      - id: f
        clear: { element: "${{ macro.element }}" }
steps:
  - id: go
    fill: {}
"#,
        codes::NORM_BAD_MACRO_PARAM,
    );
    // A `macro.` reference that is not one full scalar.
    expect_code(
        r#"
flow: bad
provider: devicerail
macros:
  fill:
    params: [flag]
    steps:
      - id: f
        find: { element: { identifier: x } }
        expect:
          - expr: ${{ eq(macro.flag, true) }}
steps:
  - id: go
    fill:
      flag: true
"#,
        codes::NORM_BAD_MACRO_PARAM,
    );
}

#[test]
fn macro_shadowing_the_vocabulary_is_rejected_rf2028() {
    expect_code(
        r#"
flow: bad
provider: devicerail
macros:
  tap:
    params: []
    steps:
      - id: t
        find: { element: { identifier: x } }
steps:
  - id: go
    find: { element: { identifier: x } }
"#,
        codes::NORM_BAD_MACRO_DEF,
    );
}

#[test]
fn call_self_cycle_is_rejected_rf2035() {
    let dir = TempDir::new("cycle");
    let main = dir.write(
        "a.flow.yaml",
        r#"flow: a_flow
provider: devicerail
steps:
  - id: recurse
    call: ./a.flow.yaml
"#,
    );
    let diags = compile_file(&main).expect_err("self-cycle must fail");
    assert!(
        diags.iter().any(|d| d.code == codes::NORM_CALL_CYCLE),
        "expected {}, got: {diags:#?}",
        codes::NORM_CALL_CYCLE
    );
}

#[test]
fn call_mutual_cycle_is_rejected_via_the_callee_rf2034() {
    let dir = TempDir::new("cycle2");
    dir.write(
        "b.flow.yaml",
        r#"flow: b_flow
provider: devicerail
steps:
  - id: back
    call: ./a.flow.yaml
"#,
    );
    let main = dir.write(
        "a.flow.yaml",
        r#"flow: a_flow
provider: devicerail
steps:
  - id: forth
    call: ./b.flow.yaml
"#,
    );
    let diags = compile_file(&main).expect_err("mutual cycle must fail");
    let summary = diags
        .iter()
        .find(|d| d.code == codes::NORM_SUBFLOW_COMPILE)
        .unwrap_or_else(|| panic!("expected {}, got: {diags:#?}", codes::NORM_SUBFLOW_COMPILE));
    assert!(
        summary.message.contains(codes::NORM_CALL_CYCLE),
        "the callee's cycle rejection must surface at the call site: {summary:#?}"
    );
}

#[test]
fn call_depth_over_the_limit_is_rejected_rf2036() {
    let dir = TempDir::new("depth");
    // c9 is a leaf; c0..c8 each call the next — a 10-file static chain.
    dir.write(
        "c9.flow.yaml",
        r#"flow: leaf_flow
provider: devicerail
steps:
  - id: probe
    find: { element: { identifier: x } }
"#,
    );
    for index in (0..9).rev() {
        dir.write(
            &format!("c{index}.flow.yaml"),
            &format!(
                r#"flow: chain_{index}
provider: devicerail
steps:
  - id: next
    call: ./c{}.flow.yaml
"#,
                index + 1
            ),
        );
    }
    let main = dir.0.join("c0.flow.yaml");
    let diags = compile_file(&main).expect_err("the 10-deep chain must fail");
    assert!(
        diags.iter().any(|d| {
            d.code == codes::NORM_CALL_DEPTH
                || (d.code == codes::NORM_SUBFLOW_COMPILE
                    && d.message.contains(codes::NORM_CALL_DEPTH))
        }),
        "expected {} (possibly nested), got: {diags:#?}",
        codes::NORM_CALL_DEPTH
    );
}

#[test]
fn subflow_version_conflict_is_rejected_rf2037() {
    let dir = TempDir::new("conflict");
    dir.write(
        "one/dup.flow.yaml",
        r#"flow: dup_flow
provider: devicerail
steps:
  - id: probe
    find: { element: { identifier: x } }
"#,
    );
    dir.write(
        "two/dup.flow.yaml",
        r#"flow: dup_flow
provider: devicerail
steps:
  - id: probe
    find: { element: { identifier: y } }
"#,
    );
    let main = dir.write(
        "main.flow.yaml",
        r#"flow: conflict_demo
provider: devicerail
steps:
  - id: first
    call: ./one/dup.flow.yaml
  - id: second
    call: ./two/dup.flow.yaml
"#,
    );
    let diags = compile_file(&main).expect_err("the version conflict must fail");
    assert!(
        diags.iter().any(|d| d.code == codes::NORM_SUBFLOW_CONFLICT),
        "expected {}, got: {diags:#?}",
        codes::NORM_SUBFLOW_CONFLICT
    );
}

#[test]
fn call_input_contract_is_enforced_rf3015() {
    let dir = TempDir::new("contract");
    dir.write(
        "callee.flow.yaml",
        r#"flow: callee_flow
provider: devicerail
params:
  needed: { schema: { type: string }, required: true }
steps:
  - id: probe
    find: { element: { identifier: x } }
"#,
    );
    // Missing required input.
    let main = dir.write(
        "missing.flow.yaml",
        r#"flow: missing_demo
provider: devicerail
steps:
  - id: go
    call: ./callee.flow.yaml
"#,
    );
    let diags = compile_file(&main).expect_err("missing input must fail");
    assert!(
        diags
            .iter()
            .any(|d| d.code == codes::CHECK_CALL_INPUT_CONTRACT),
        "expected {}, got: {diags:#?}",
        codes::CHECK_CALL_INPUT_CONTRACT
    );
    // Unknown input name + literal failing the param schema.
    let main = dir.write(
        "unknown.flow.yaml",
        r#"flow: unknown_demo
provider: devicerail
steps:
  - id: go
    call: ./callee.flow.yaml
    inputs:
      needed: 42
      ghost: 1
"#,
    );
    let diags = compile_file(&main).expect_err("bad inputs must fail");
    let contract: Vec<_> = diags
        .iter()
        .filter(|d| d.code == codes::CHECK_CALL_INPUT_CONTRACT)
        .collect();
    assert_eq!(
        contract.len(),
        2,
        "expected an unknown-name and a schema finding, got: {diags:#?}"
    );
}

#[test]
fn handler_shape_errors_are_rejected_rf2042() {
    // Missing max_triggers.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: a
    find: { element: { identifier: x } }
    on_fail:
      abort: {}
"#,
        codes::NORM_BAD_HANDLER_SHAPE,
    );
    // error_classes outside on_error.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: a
    find: { element: { identifier: x } }
    on_fail:
      error_classes: [session_degraded]
      abort: {}
      max_triggers: 1
"#,
        codes::NORM_BAD_HANDLER_SHAPE,
    );
    // Two disposition head keys.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: a
    find: { element: { identifier: x } }
    on_fail:
      abort: {}
      continue: {}
      max_triggers: 1
"#,
        codes::NORM_BAD_HANDLER_SHAPE,
    );
}

#[test]
fn escalate_without_human_material_is_rejected_rf2038() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: a
    find: { element: { identifier: x } }
    on_unknown:
      escalate: {}
      max_triggers: 1
"#,
        codes::NORM_BAD_HUMAN_SHAPE,
    );
    // Escalate blocks must carry their timeout in-block.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: a
    find: { element: { identifier: x } }
    on_unknown:
      escalate:
        mode: judge
        prompt: "please judge"
        decisions: [pass, fail]
      max_triggers: 1
"#,
        codes::NORM_BAD_HUMAN_SHAPE,
    );
}

#[test]
fn human_on_timeout_must_be_unknown_rf2038() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: ask
    human:
      mode: confirm
      prompt: "confirm it"
      on_timeout: pass
    timeout_ms: 1000
"#,
        codes::NORM_BAD_HUMAN_SHAPE,
    );
    // A human step without any timeout surface.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: ask
    human:
      mode: confirm
      prompt: "confirm it"
"#,
        codes::NORM_BAD_HUMAN_SHAPE,
    );
    // provideInput requires expect_schema.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: ask
    human:
      mode: provideInput
      prompt: "type it"
      timeout_ms: 1000
"#,
        codes::NORM_BAD_HUMAN_SHAPE,
    );
}

#[test]
fn iter_outside_the_foreach_body_is_rejected_rf3009() {
    expect_code(
        r#"
flow: bad
provider: devicerail
params:
  items: { schema: { type: array }, required: true }
steps:
  - id: each
    foreach:
      in: ${{ params.items }}
      as: item
      steps:
        - id: inner
          set_value:
            element: { identifier: f }
            value: ${{ iter.item }}
  - id: after
    set_value:
      element: { identifier: f }
      value: ${{ iter.item }}
"#,
        codes::CHECK_UNDECLARED_NAME,
    );
}

#[test]
fn vars_ssa_rebind_is_rejected_rf3014() {
    expect_code(
        r#"
flow: bad
provider: devicerail
params:
  ssid: { schema: { type: string }, required: true }
steps:
  - id: l1
    let:
      x: ${{ params.ssid }}
  - id: l2
    let:
      x: ${{ params.ssid }}
"#,
        codes::CHECK_SSA_REBIND,
    );
}

#[test]
fn else_branch_cannot_see_then_products_rf3007() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    find: { element: { identifier: x } }
  - id: branch
    if: ${{ eq(steps.probe.output.matched, true) }}
    then:
      - id: t1
        find: { element: { identifier: y } }
    else:
      - id: e1
        set_value:
          element: { identifier: z }
          value: ${{ steps.t1.output.matched }}
"#,
        codes::CHECK_FORWARD_REF,
    );
}

#[test]
fn then_without_if_is_rejected_rf2039() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: a
    find: { element: { identifier: x } }
    then:
      - id: b
        find: { element: { identifier: y } }
"#,
        codes::NORM_BAD_IF_SHAPE,
    );
}

#[test]
fn duplicate_ids_across_nesting_are_rejected_rf3001() {
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    find: { element: { identifier: x } }
  - id: branch
    if: ${{ eq(steps.probe.output.matched, true) }}
    then:
      - id: probe
        find: { element: { identifier: y } }
"#,
        codes::CHECK_DUPLICATE_STEP_ID,
    );
}

// ─── the observation verbs (04 §9.4.3) ──────────────────────────────────────

/// `observe` / `screenshot` are the two canonical verbs with no driver
/// action behind them: they lower to the provider-synthetic readonly
/// actions of the same name, which bind overlays onto the capability set.
/// Their effect is inferred, never declared, and they carry no selector.
#[test]
fn observation_verbs_lower_to_the_synthetic_readonly_actions() {
    let sealed = compile_named(
        r#"
flow: observation
provider: devicerail
steps:
  - id: capture_state
    observe: [screenshot, uiSnapshot]
  - id: captcha_shot
    screenshot: {}
"#,
        "observation.flow.yaml",
    )
    .expect("both observation verbs compile");

    let steps: Vec<(&str, &str, EffectClassAction)> = sealed
        .flow_ir
        .body
        .iter()
        .map(|step| {
            let StepIR::Action(action) = step else {
                panic!("an observation verb is an action step");
            };
            (
                action.base.step_id.as_str(),
                action.binding.attempts[0].action_name.as_str(),
                action.effect,
            )
        })
        .collect();
    assert_eq!(
        steps,
        vec![
            ("capture_state", "observe", EffectClassAction::Readonly),
            ("captcha_shot", "screenshot", EffectClassAction::Readonly),
        ]
    );

    // The requested parts travel as the literal `wants` argument; the
    // shortcut form fixes them provider-side and passes none.
    let StepIR::Action(observe) = &sealed.flow_ir.body[0] else {
        panic!("action step");
    };
    assert_eq!(
        serde_json::to_value(&observe.binding.attempts[0].args).expect("args serialize"),
        serde_json::json!({ "wants": { "lit": ["screenshot", "uiSnapshot"] } })
    );
    let StepIR::Action(shot) = &sealed.flow_ir.body[1] else {
        panic!("action step");
    };
    assert!(shot.binding.attempts[0].args.is_empty());
}

#[test]
fn observation_verbs_reject_everything_outside_their_closed_shape() {
    // The parts vocabulary is closed, non-empty and duplicate-free.
    for source in [
        "observe: [screenshot, video]",
        "observe: []",
        "observe: [screenshot, screenshot]",
        "observe: {}",
        // the shortcut form is exactly `{}` — it has nothing left to say
        "screenshot: { region: full }",
    ] {
        expect_code(
            &format!("\nflow: bad\nprovider: devicerail\nsteps:\n  - id: s\n    {source}\n"),
            codes::NORM_BAD_VERB_SHAPE,
        );
    }
    // Readonly is inferred; a contradicting declaration is refused rather
    // than trusted (03 §1.3).
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: s
    observe: [screenshot]
    effect: mutating
"#,
        codes::NORM_EFFECT_CONFLICT,
    );
    // The act-chain surface needs a selector to locate.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: s
    observe: [screenshot]
    locate_via: [dom]
"#,
        codes::NORM_BAD_FALLBACK_SHAPE,
    );
}

/// The shadowing rule (04 §9.4.3): a driver action of the same name wins
/// and the synthetic entry is disabled for that lockfile — the verb then
/// binds straight to the driver action. Nothing is silently chosen between.
#[test]
fn a_driver_action_of_the_same_name_shadows_the_synthetic_one() {
    let manifest = m2_manifest();
    let mut lockfile = m2_lockfile(&manifest);
    // This device's driver declares its own `screenshot`, with a shape the
    // synthetic entry does not have.
    lockfile.device.actions.push(ActionDefinitionStatic {
        name: action_name("screenshot"),
        input_schema: JsonSchemaDocument::new(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["quality"],
            "properties": { "quality": { "type": "integer" } }
        }))
        .expect("valid schema document"),
        output_schema: None,
        protection: ActionProtection::Standard,
        synthetic: false,
    });
    lockfile.seal();

    // The driver action requires `quality`, which the shortcut form does
    // not supply — proof that the lockfile entry, not the synthetic one,
    // is what the verb bound to.
    let diags = compile(
        "\nflow: shadowed\nprovider: devicerail\nsteps:\n  - id: s\n    screenshot: {}\n",
        &CompileOptions {
            source_name: "shadowed.flow.yaml",
            manifest: &manifest,
            lockfile: Some(&lockfile),
        },
    )
    .expect_err("the driver action's own schema is enforced");
    assert!(
        diags.iter().any(|diag| diag.code == codes::BIND_ARGS_SHAPE),
        "expected the driver schema to reject the empty args, got: {diags:#?}"
    );
}

// ─── provideInput secret-semantic rejection (06 §3.1, RF4019) ───────────────

#[test]
fn provide_input_sensitive_field_is_refused_at_bind_rf4019() {
    // `sensitive: true` anywhere in `expect_schema` is a bind refusal —
    // secret values never enter Pointlock's data plane (06 §3.1).
    let diags = compile_named(
        r#"
flow: ask
provider: devicerail
steps:
  - id: ask
    human:
      mode: provideInput
      prompt: "enter the code"
      timeout_ms: 60000
      expect_schema:
        type: object
        properties:
          code: { type: string, sensitive: true }
"#,
        "secret.flow.yaml",
    )
    .expect_err("a sensitive field must not compile");
    let diag = diags
        .iter()
        .find(|diag| diag.code == codes::BIND_SECRET_INPUT)
        .unwrap_or_else(|| panic!("expected RF4019, got: {diags:#?}"));
    // The refusal names the offending location and the sanctioned path.
    assert!(
        diag.message.contains("/properties/code"),
        "{}",
        diag.message
    );
    assert!(diag.message.contains("repairWorld"), "{}", diag.message);
}

#[test]
fn provide_input_password_format_is_refused_even_nested_rf4019() {
    // `format: "password"` counts as secret semantics too, and the walk
    // reaches it under nesting (fail-closed over-approximation).
    expect_code(
        r#"
flow: ask
provider: devicerail
steps:
  - id: ask
    human:
      mode: provideInput
      prompt: "enter the credentials"
      timeout_ms: 60000
      expect_schema:
        type: object
        properties:
          account:
            type: object
            properties:
              password: { type: string, format: password }
"#,
        codes::BIND_SECRET_INPUT,
    );
}

#[test]
fn provide_input_without_secret_semantics_still_compiles() {
    // Negative control: the same shape minus the secret markers is the
    // ordinary provideInput contract and must stay green.
    compile_named(
        r#"
flow: ask
provider: devicerail
steps:
  - id: ask
    human:
      mode: provideInput
      prompt: "enter the ssid"
      timeout_ms: 60000
      expect_schema:
        type: object
        properties:
          ssid: { type: string }
"#,
        "plain.flow.yaml",
    )
    .expect("a secret-free provideInput contract compiles");
}

#[test]
fn escalate_provide_input_secret_schema_is_refused_rf4019() {
    // The 06 §3.1 rejection covers escalate humans too — the doc pins it
    // per outputSchema, not per step position. Flow-level handler here
    // (span-less by construction); the step-level path shares the walk.
    let diags = compile_named(
        r#"
flow: ask
provider: devicerail
handlers:
  on_unknown:
    - escalate:
        mode: provideInput
        prompt: "enter the code"
        timeout_ms: 60000
        on_timeout: unknown
        expect_schema:
          type: object
          properties:
            code: { type: string, sensitive: true }
      max_triggers: 1
steps:
  - id: probe
    tap: { element: { identifier: wifi_row } }
"#,
        "escalate-secret.flow.yaml",
    )
    .expect_err("a secret escalate schema must not compile");
    assert!(
        diags
            .iter()
            .any(|diag| diag.code == codes::BIND_SECRET_INPUT
                && diag.message.contains("/properties/code")),
        "expected RF4019 on the escalate schema, got: {diags:#?}"
    );
}

// ─── string interpolation sugar (03 §1.9) ───────────────────────────────────

#[test]
fn embedded_interpolation_desugars_to_concat() {
    // `"hello ${{ params.ssid }}!"` compiles to
    // `concat("hello ", params.ssid, "!")` — always a string.
    let sealed = compile_named(
        r#"
flow: interp
provider: devicerail
params:
  ssid: { schema: { type: string }, required: true }
steps:
  - id: fill
    set_value:
      element: { identifier: ssid_field }
      value: "hello ${{ params.ssid }}!"
"#,
        "interp.flow.yaml",
    )
    .expect("embedded interpolation compiles");
    let StepIR::Action(action) = &sealed.flow_ir.body[0] else {
        panic!("expected an action step");
    };
    let args = serde_json::to_value(&action.binding.attempts[0].args).expect("args serialize");
    assert_eq!(
        args["value"],
        serde_json::json!({
            "fn": "concat",
            "args": [
                { "lit": "hello " },
                { "ref": "params.ssid" },
                { "lit": "!" },
            ]
        })
    );
}

#[test]
fn interpolation_escapes_literal_runs_and_rejects_malformed_islands() {
    // Literal runs with quotes/backslashes survive the desugar through
    // JSON escaping in the expr-text grammar.
    let sealed = compile_named(
        r#"
flow: interp
provider: devicerail
params:
  ssid: { schema: { type: string }, required: true }
steps:
  - id: fill
    set_value:
      element: { identifier: ssid_field }
      value: "say \"${{ params.ssid }}\" \\ done"
"#,
        "escape.flow.yaml",
    )
    .expect("escaped literal runs compile");
    let StepIR::Action(action) = &sealed.flow_ir.body[0] else {
        panic!("expected an action step");
    };
    let args = serde_json::to_value(&action.binding.attempts[0].args).expect("args serialize");
    let value = &args["value"];
    assert_eq!(value["args"][0]["lit"], "say \"");
    assert_eq!(value["args"][2]["lit"], "\" \\ done");

    // Negative controls: an unterminated island is a malformed-
    // interpolation rejection, not a silent literal.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: fill
    set_value:
      element: { identifier: ssid_field }
      value: "hello ${{ params.ssid"
"#,
        codes::NORM_INTERPOLATION_INVALID,
    );
    // And interpolation inside a composite member stays refused.
    expect_code(
        r#"
flow: bad
provider: devicerail
steps:
  - id: fill
    invoke:
      action: tapElement
      args:
        target: { nested: "hello ${{ params.ssid }}!" }
    effect: mutating
"#,
        codes::NORM_INTERPOLATION_INVALID,
    );
}

// ─── expect_schema narrowing + C7 (03 §1.3, RF3020) ─────────────────────────

#[test]
fn expect_schema_narrows_the_invoke_output_with_real_types() {
    // The narrowed field is a TYPED position: a definite type
    // contradiction against it is refused — proof the schema feeds the
    // C5/C6 lattice, not decoration.
    let sealed = compile_named(
        r#"
flow: narrow
provider: devicerail
steps:
  - id: probe
    invoke:
      action: tapElement
      args:
        target: { kind: selector, selector: { identifier: field } }
    effect: mutating
    expect_schema:
      type: object
      properties:
        count: { type: number }
    expect:
      - assert_id: counted
        expr: ${{ eq(steps.probe.output.count, 3) }}
"#,
        "narrow.flow.yaml",
    )
    .expect("a narrowed field access compiles");
    // The contract rides the sealed IR (and its effect-hash domain).
    let StepIR::Action(action) = &sealed.flow_ir.body[0] else {
        panic!("expected an action step");
    };
    assert!(
        action.output_schema.is_some(),
        "expect_schema must seal into ActionStepIR.outputSchema"
    );

    let diags = compile_named(
        r#"
flow: narrow
provider: devicerail
steps:
  - id: probe
    invoke:
      action: tapElement
      args:
        target: { kind: selector, selector: { identifier: field } }
    effect: mutating
    expect_schema:
      type: object
      properties:
        count: { type: number }
    expect:
      - assert_id: contradiction
        expr: ${{ eq(steps.probe.output.count, "three") }}
"#,
        "contradiction.flow.yaml",
    )
    .expect_err("a number-typed field compared to a string is a definite contradiction");
    assert!(
        diags.iter().any(|diag| diag.code.starts_with("RF30")),
        "expected a check-stage type rejection, got: {diags:#?}"
    );
}

#[test]
fn c7_refuses_fields_of_un_narrowed_invoke_outputs_rf3020() {
    let diags = compile_named(
        r#"
flow: bad
provider: devicerail
steps:
  - id: probe
    invoke:
      action: tapElement
      args:
        target: { kind: selector, selector: { identifier: field } }
    effect: mutating
    expect:
      - assert_id: blind
        expr: ${{ eq(steps.probe.output.count, 3) }}
"#,
        "c7.flow.yaml",
    )
    .expect_err("an un-narrowed invoke output field access must not compile");
    let diag = diags
        .iter()
        .find(|diag| diag.code == codes::CHECK_UNNARROWED_INVOKE_OUTPUT)
        .unwrap_or_else(|| panic!("expected RF3020, got: {diags:#?}"));
    assert!(
        diag.hint
            .as_deref()
            .is_some_and(|hint| hint.contains("expect_schema")),
        "the hint names the fix: {diag:?}"
    );
}

#[test]
fn whole_outputs_and_observation_heads_stay_legal_without_narrowing() {
    // Negative controls of the C7 scope: passing the WHOLE output on is
    // not dereferencing it, and observation heads have the documented
    // output shape — neither demands a narrowing.
    compile_named(
        r#"
flow: fine
provider: devicerail
steps:
  - id: probe
    invoke:
      action: tapElement
      args:
        target: { kind: selector, selector: { identifier: field } }
    effect: mutating
  - id: shot
    observe: [uiSnapshot]
  - id: judge
    expect:
      - assert_id: whole_output_passes
        expr: ${{ ne(steps.probe.output, null) }}
      - assert_id: observation_field_passes
        expr: ${{ ne(steps.shot.output.observationId, null) }}
"#,
        "fine.flow.yaml",
    )
    .expect("whole-output and observation-head accesses stay legal");
}
