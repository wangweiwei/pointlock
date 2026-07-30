//! The static, package-shipped capability declaration of the DeviceRail
//! provider (spine §4.1; design doc 04 §9.2 / §9.4).
//!
//! Declaration precedes execution (04 §1 rule 3): the compiler consumes
//! this manifest (plus a capability lockfile) with no device online.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use pointlock_ir::{ActionName, CanonicalVerb, Channel, FeatureId, JsonSchemaDocument};
use pointlock_provider_kit::manifest::{
    ActionDefinitionStatic, ActionProtection, ChannelRole, ChannelSupport, ConditionalFeature,
    FeatureDeclarations, PlatformKind, ProtocolRange, ProviderManifest, VerbBinding,
};
use serde_json::{Value, json};

/// The v0.1 provider name (04 §8 assembly ruling: constant `devicerail`).
pub const PROVIDER_NAME: &str = "devicerail";

/// Provider-infrastructure features (04 §9.2 ★ rows): preconditions of this
/// provider's execution model itself, flow-independent, always placed in
/// `FeatureOffer.required`.
pub const INFRA_REQUIRED_FEATURES: [&str; 3] = [
    "device.routing.v1",
    "events.snapshot.v1",
    "request.control.v1",
];

/// Every non-infrastructure feature this provider offers in `system.hello`.
/// The full known universe is always offered (required ∪ optional covers
/// it), so `FeatureSelection.enabled` — and therefore the lockfile digest —
/// depends only on the daemon, never on which flow triggered the handshake.
/// `action.protected.v1` is deliberately absent: v0.1 does not offer and
/// does not consume it (04 §9.2).
pub const OFFERED_OPTIONAL_FEATURES: [&str; 6] = [
    "device.semanticActions.v1",
    "observation.uiSnapshot.v1",
    "verdict.record.v1",
    "events.stream.v1",
    "media.stream.v1",
    "session.export.page.v1",
];

fn feature(id: &str) -> FeatureId {
    FeatureId::new(id).expect("built-in feature ids are grammatical")
}

fn action_name(name: &str) -> ActionName {
    ActionName::new(name).expect("built-in action names are grammatical")
}

fn schema(value: Value) -> JsonSchemaDocument {
    JsonSchemaDocument::new(value).expect("built-in schemas are valid documents")
}

/// `ElementSelector` argument schema, transcribed from the DeviceRail
/// protocol schema (`element-selector.schema.json`; 04 §9.5 field table,
/// `additionalProperties: false`).
fn element_selector_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "context": {
                "type": "object",
                "additionalProperties": false,
                "required": ["contextKind"],
                "properties": {
                    "contextKind": { "enum": ["native", "web"] },
                    "contextId": { "type": "string", "minLength": 1, "maxLength": 4096 }
                }
            },
            "role": { "type": "string", "minLength": 1, "maxLength": 256 },
            "name": { "type": "string", "minLength": 1, "maxLength": 65536 },
            "value": { "type": "string", "maxLength": 65536 },
            "identifier": { "type": "string", "minLength": 1, "maxLength": 4096 },
            "text": {
                "type": "object",
                "additionalProperties": false,
                "required": ["value"],
                "properties": {
                    "value": { "type": "string", "minLength": 1, "maxLength": 65536 },
                    "mode": { "enum": ["exact", "contains"] },
                    "caseSensitive": { "type": "boolean" }
                }
            },
            "css": { "type": "string", "minLength": 1, "maxLength": 65536 }
        }
    })
}

/// `UiNodeRef` schema (DeviceRail `ui-node-ref`; carries the full context
/// with `documentEpoch` — the staleness token, 04 §9.5).
fn ui_node_ref_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["observationId", "context", "stableNodeId"],
        "properties": {
            "observationId": { "type": "string", "format": "uuid" },
            "context": {
                "type": "object",
                "additionalProperties": false,
                "required": ["contextKind", "contextId", "documentEpoch"],
                "properties": {
                    "contextKind": { "enum": ["native", "web"] },
                    "contextId": { "type": "string", "minLength": 1, "maxLength": 4096 },
                    "documentEpoch": { "type": "string", "minLength": 1, "maxLength": 4096 }
                }
            },
            "stableNodeId": { "type": "string", "minLength": 1, "maxLength": 4096 }
        }
    })
}

/// `AssetRef` schema — the provider-side handle an observation yields
/// before `fetchEvidence` localizes it into an `EvidenceRef` (04 §9.4.1).
fn asset_ref_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "mediaType", "uri"],
        "properties": {
            "id": { "type": "string", "minLength": 1, "maxLength": 4096 },
            "mediaType": { "type": "string", "minLength": 1, "maxLength": 255 },
            "uri": { "type": "string", "minLength": 1, "maxLength": 8192 },
            "sha256": { "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" }
        }
    })
}

/// The output projection shared by both synthetic observation actions
/// (04 §9.4.3, 06 §4 example): the observation's identity plus whichever
/// parts were captured, each with its truthful omission reason when the
/// part was wanted but legitimately absent (omission is data, not an
/// error — 04 §4.1).
fn observation_projection_schema(parts: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "observationId".to_owned(),
        json!({ "type": "string", "minLength": 1, "maxLength": 4096 }),
    );
    if parts.contains(&"screenshot") {
        properties.insert("screenshot".to_owned(), asset_ref_schema());
        properties.insert(
            // The IR's closed `ScreenshotOmissionReason` vocabulary, wire
            // spelling (vocab.rs / spine A.8) — the projection serializes
            // that enum verbatim, so the schema must spell exactly it.
            "screenshotOmission".to_owned(),
            json!({ "enum": ["policy", "protectedAction"] }),
        );
    }
    if parts.contains(&"uiSnapshot") {
        properties.insert(
            "uiSnapshot".to_owned(),
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["observationId"],
                "properties": {
                    "observationId": { "type": "string", "minLength": 1, "maxLength": 4096 }
                }
            }),
        );
        properties.insert(
            // `UiSnapshotOmissionReason`, same rule.
            "uiSnapshotOmission".to_owned(),
            json!({ "enum": ["driverUnsupported", "policy", "protectedAction"] }),
        );
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["observationId"],
        "properties": Value::Object(properties)
    })
}

/// The two provider-synthetic readonly actions (04 §9.4.3).
///
/// `observe` and `screenshot` are the only canonical verbs with no driver
/// action behind them: they map to the `device.observe` RPC (plus its
/// `ui.snapshot.get` follow-up), not to `device.execute`. Declaring them
/// here keeps the spine invariant intact — the IR still carries only
/// native action names and the runner still has no verb switch; the
/// adapter does the routing (see `session.rs`).
///
/// Both are readonly, so they are exempt from reconcile by construction:
/// a dangling readonly intent is always safe to replay (spine §6.7-B).
fn synthetic_observation_actions() -> Vec<ActionDefinitionStatic> {
    vec![
        ActionDefinitionStatic {
            name: action_name("observe"),
            // `wants` is an intent declaration, not a wire parameter:
            // `device.observe` takes none. It tells the adapter whether to
            // chase the snapshot follow-up and which omissions to fill in.
            input_schema: schema(json!({
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
            })),
            output_schema: Some(schema(observation_projection_schema(&[
                "screenshot",
                "uiSnapshot",
            ]))),
            protection: ActionProtection::Standard,
            synthetic: true,
        },
        ActionDefinitionStatic {
            // The single-part shortcut form (`screenshot: {}`): the same
            // RPC with `wants: [screenshot]` fixed, so it takes no args.
            name: action_name("screenshot"),
            input_schema: schema(json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            })),
            output_schema: Some(schema(observation_projection_schema(&["screenshot"]))),
            protection: ActionProtection::Standard,
            synthetic: true,
        },
    ]
}

/// `ElementTarget` schema (oneOf selector/node, 04 §9.5).
fn element_target_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "selector"],
                "properties": {
                    "kind": { "const": "selector" },
                    "selector": element_selector_schema()
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "node"],
                "properties": {
                    "kind": { "const": "node" },
                    "node": ui_node_ref_schema()
                }
            }
        ]
    })
}

fn semantic_actions() -> Vec<ActionDefinitionStatic> {
    let target_input = |name: &str| ActionDefinitionStatic {
        name: action_name(name),
        input_schema: schema(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["target"],
            "properties": { "target": element_target_schema() }
        })),
        output_schema: Some(schema(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["element"],
            "properties": { "element": ui_node_ref_schema() }
        }))),
        protection: ActionProtection::Standard,
        synthetic: false,
    };
    vec![
        ActionDefinitionStatic {
            name: action_name("findElement"),
            input_schema: schema(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["selector"],
                "properties": { "selector": element_selector_schema() }
            })),
            output_schema: Some(schema(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["element"],
                "properties": { "element": ui_node_ref_schema() }
            }))),
            protection: ActionProtection::Standard,
            synthetic: false,
        },
        target_input("tapElement"),
        target_input("clearElement"),
        ActionDefinitionStatic {
            name: action_name("setElementValue"),
            input_schema: schema(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["target", "value"],
                "properties": {
                    "target": element_target_schema(),
                    "value": { "type": "string", "maxLength": 65536 }
                }
            })),
            output_schema: Some(schema(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["element"],
                "properties": { "element": ui_node_ref_schema() }
            }))),
            protection: ActionProtection::Standard,
            synthetic: false,
        },
        ActionDefinitionStatic {
            name: action_name("waitForElement"),
            input_schema: schema(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["selector"],
                "properties": {
                    "selector": element_selector_schema(),
                    "condition": { "enum": ["present", "visible", "enabled", "absent"] }
                }
            })),
            // `matched: false` is a succeeded terminal with observational
            // output, never an error (04 §6.2 note).
            output_schema: Some(schema(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["matched", "condition"],
                "properties": {
                    "matched": { "type": "boolean" },
                    "condition": { "enum": ["present", "visible", "enabled", "absent"] },
                    "element": ui_node_ref_schema()
                }
            }))),
            protection: ActionProtection::Standard,
            synthetic: false,
        },
    ]
}

fn build_manifest() -> ProviderManifest {
    let semantic = feature("device.semanticActions.v1");
    let ui_snapshot = feature("observation.uiSnapshot.v1");
    // The semantic five (04 §9.4.2): verb argument names on the left are
    // the compiler's surface names (`element` / `value` / `state`), native
    // wire argument names on the right.
    let semantic_binding = |verb: CanonicalVerb, action: &str, args: &[(&str, &str)]| VerbBinding {
        verb,
        action_name: action_name(action),
        requires_feature: Some(semantic.clone()),
        arg_map: args
            .iter()
            .map(|(surface, native)| ((*surface).to_owned(), (*native).to_owned()))
            .collect::<BTreeMap<_, _>>(),
    };

    ProviderManifest {
        name: PROVIDER_NAME.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: ProtocolRange {
            major: 1,
            min_minor: 5,
            max_minor: 5,
        },
        // Nothing is guaranteed unconditionally: every feature depends on
        // the daemon negotiation, so availability flows exclusively through
        // `lockfile.hello.featuresEnabled` (spine §4.1 compile rule).
        features: FeatureDeclarations {
            guaranteed: Vec::new(),
            conditional: vec![
                // Daemon-level features: conditional on the daemon
                // version/runtime, not on the device platform.
                ConditionalFeature {
                    feature: feature("device.routing.v1"),
                    requires_platform: None,
                },
                ConditionalFeature {
                    feature: feature("events.snapshot.v1"),
                    requires_platform: None,
                },
                ConditionalFeature {
                    feature: feature("request.control.v1"),
                    requires_platform: None,
                },
                ConditionalFeature {
                    feature: feature("verdict.record.v1"),
                    requires_platform: None,
                },
                ConditionalFeature {
                    feature: feature("session.export.page.v1"),
                    requires_platform: None,
                },
                ConditionalFeature {
                    feature: feature("events.stream.v1"),
                    requires_platform: None,
                },
                ConditionalFeature {
                    feature: feature("media.stream.v1"),
                    requires_platform: None,
                },
                // Driver-dependent features: platforms with a verified
                // semantic-UI driver (04 §9.4.2: Android confirmed; iOS
                // WebDriver and Harmony HDC implement the same surface).
                ConditionalFeature {
                    feature: semantic.clone(),
                    requires_platform: Some(vec![
                        PlatformKind::Android,
                        PlatformKind::Ios,
                        PlatformKind::HarmonyOs,
                    ]),
                },
                ConditionalFeature {
                    feature: ui_snapshot.clone(),
                    requires_platform: Some(vec![
                        PlatformKind::Android,
                        PlatformKind::Ios,
                        PlatformKind::HarmonyOs,
                    ]),
                },
            ],
        },
        verb_bindings: vec![
            // The semantic five (04 §9.4.2). Element-target verbs take a
            // wire `ElementTarget` under `target`; locator verbs take the
            // bare `ElementSelector` under `selector`.
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
            // The provider-synthetic observation verbs (04 §9.4.3). They
            // bind like any other verb, so the shadowing rule falls out of
            // the existing machinery: a driver action of the same name in
            // the lockfile makes the binding ambiguous and bind refuses
            // rather than silently choosing. No feature gate — an
            // observation is the one thing every session can do; the parts
            // it cannot produce come back as typed omissions.
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
            // Coordinate fallback row (04 §9.4.2 "screen.tap（坐标兜底）"):
            // binds the tap verb's coordinate channel to the driver-native
            // `tap {x, y}` action; the bind phase still requires the action
            // to exist in the lockfile (D3) and the attempt to carry static
            // coordinates.
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
            // uiTree: locate/act through semantic actions, verify through
            // normalized snapshots. The channel gate is the snapshot
            // feature (the daemon couples semanticActions ⇒ uiSnapshot,
            // 04 §9.2); acting additionally pulls
            // `device.semanticActions.v1` through the verb-binding gate.
            ChannelSupport {
                channel: Channel::UiTree,
                role: ChannelRole::Both,
                requires_feature: Some(ui_snapshot),
                requires_platform: None,
            },
            // dom: web-context semantic execution (css selectors legal only
            // with `contextKind: "web"`, 04 §9.5).
            ChannelSupport {
                channel: Channel::Dom,
                role: ChannelRole::Both,
                requires_feature: Some(semantic),
                requires_platform: None,
            },
            // coordinate: act-only by construction (a coordinate cannot
            // verify anything).
            ChannelSupport {
                channel: Channel::Coordinate,
                role: ChannelRole::Act,
                requires_feature: None,
                requires_platform: None,
            },
            // vision: verify-only (principle 7); the verification itself is
            // Pointlock-side (pointlock-vision), the provider only supplies
            // screenshots through observations.
            ChannelSupport {
                channel: Channel::Vision,
                role: ChannelRole::Verify,
                requires_feature: None,
                requires_platform: None,
            },
        ],
        known_actions: semantic_actions()
            .into_iter()
            .chain(synthetic_observation_actions())
            .collect(),
    }
}

/// The DeviceRail provider manifest (built once, shared).
pub fn devicerail_manifest() -> &'static ProviderManifest {
    static MANIFEST: OnceLock<ProviderManifest> = OnceLock::new();
    MANIFEST.get_or_init(build_manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_the_semantic_five_and_wire_names_verbatim() {
        let manifest = devicerail_manifest();
        assert_eq!(manifest.name, "devicerail");
        assert_eq!(manifest.protocol.major, 1);
        assert_eq!(manifest.protocol.min_minor, 5);
        assert_eq!(manifest.protocol.max_minor, 5);
        assert!(manifest.features.guaranteed.is_empty());

        let bindings: Vec<(CanonicalVerb, &str)> = manifest
            .verb_bindings
            .iter()
            .map(|binding| (binding.verb, binding.action_name.as_str()))
            .collect();
        for expected in [
            (CanonicalVerb::Tap, "tapElement"),
            (CanonicalVerb::SetValue, "setElementValue"),
            (CanonicalVerb::Clear, "clearElement"),
            (CanonicalVerb::WaitFor, "waitForElement"),
            (CanonicalVerb::Find, "findElement"),
            (CanonicalVerb::Tap, "tap"),
            // The provider-synthetic observation verbs (04 §9.4.3).
            (CanonicalVerb::Observe, "observe"),
            (CanonicalVerb::Screenshot, "screenshot"),
        ] {
            assert!(bindings.contains(&expected), "missing binding {expected:?}");
        }

        let known: Vec<&str> = manifest
            .known_actions
            .iter()
            .map(|action| action.name.as_str())
            .collect();
        assert_eq!(
            known,
            [
                "findElement",
                "tapElement",
                "clearElement",
                "setElementValue",
                "waitForElement",
                // Provider-synthetic, and marked as such: they map to
                // `device.observe`, so no driver declares them and bind
                // overlays them onto the lockfile's action set.
                "observe",
                "screenshot",
            ]
        );
        let synthetic: Vec<&str> = manifest
            .known_actions
            .iter()
            .filter(|action| action.synthetic)
            .map(|action| action.name.as_str())
            .collect();
        assert_eq!(synthetic, ["observe", "screenshot"]);
    }

    #[test]
    fn semantic_bindings_map_surface_args_per_04_9_4() {
        let manifest = devicerail_manifest();
        let arg_map = |verb: CanonicalVerb, action: &str| {
            &manifest
                .verb_bindings
                .iter()
                .find(|binding| binding.verb == verb && binding.action_name.as_str() == action)
                .expect("binding present")
                .arg_map
        };
        assert_eq!(
            arg_map(CanonicalVerb::Tap, "tapElement")["element"],
            "target"
        );
        assert_eq!(
            arg_map(CanonicalVerb::SetValue, "setElementValue")["value"],
            "value"
        );
        assert_eq!(
            arg_map(CanonicalVerb::WaitFor, "waitForElement")["element"],
            "selector"
        );
        assert_eq!(
            arg_map(CanonicalVerb::WaitFor, "waitForElement")["state"],
            "condition"
        );
        assert_eq!(
            arg_map(CanonicalVerb::Find, "findElement")["element"],
            "selector"
        );
        let coordinate = arg_map(CanonicalVerb::Tap, "tap");
        assert_eq!(coordinate["x"], "x");
        assert_eq!(coordinate["y"], "y");
    }

    #[test]
    fn channels_respect_role_restrictions() {
        let manifest = devicerail_manifest();
        let role = |channel: Channel| {
            manifest
                .channels
                .iter()
                .find(|support| support.channel == channel)
                .map(|support| support.role)
        };
        assert_eq!(role(Channel::UiTree), Some(ChannelRole::Both));
        assert_eq!(role(Channel::Dom), Some(ChannelRole::Both));
        assert_eq!(role(Channel::Coordinate), Some(ChannelRole::Act));
        // vision may only ever verify (principle 7).
        assert_eq!(role(Channel::Vision), Some(ChannelRole::Verify));
        let ui_tree = manifest
            .channels
            .iter()
            .find(|support| support.channel == Channel::UiTree)
            .unwrap();
        assert_eq!(
            ui_tree.requires_feature.as_ref().map(FeatureId::as_str),
            Some("observation.uiSnapshot.v1")
        );
    }

    #[test]
    fn manifest_serializes_to_the_spine_wire_shape() {
        let wire = serde_json::to_value(devicerail_manifest()).expect("serialize");
        assert_eq!(wire["name"], "devicerail");
        assert_eq!(wire["protocol"]["minMinor"], 5);
        assert_eq!(wire["verbBindings"][0]["argMap"]["element"], "target");
        assert_eq!(wire["knownActions"][0]["name"], "findElement");
        assert_eq!(wire["knownActions"][0]["protection"], "standard");
        let back: ProviderManifest = serde_json::from_value(wire).expect("round trip");
        assert_eq!(&back, devicerail_manifest());
    }
}
