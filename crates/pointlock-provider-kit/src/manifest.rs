//! Static provider capability declaration (spine §4.1 `ProviderManifest`).
//!
//! The manifest ships inside the provider package, versioned with it, and is
//! consumed by the compiler without any device online (declaration precedes
//! execution, 04 §1 rule 3). Shapes mirror the spine §4.1 TS signatures
//! verbatim on the wire (camelCase, closed objects).

use std::collections::BTreeMap;

use pointlock_ir::{ActionName, CanonicalVerb, Channel, FeatureId, JsonSchemaDocument};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Platform discriminator of a device (spine §4.1 `PlatformKind`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum PlatformKind {
    /// Android device.
    Android,
    /// iOS device.
    Ios,
    /// Web browser target.
    Web,
    /// HarmonyOS device.
    HarmonyOs,
    /// macOS desktop.
    MacOs,
    /// Windows desktop.
    Windows,
    /// Linux desktop.
    Linux,
    /// RDP-attached desktop.
    Rdp,
}

/// Protocol-level action protection (DeviceRail `ActionProtection`,
/// spine A.8). This is the *protocol* two-valued enum; the IR-side
/// `Protection` literal marker additionally pins bindable actions to
/// `"standard"` in v0.1 (R6, 02 §11).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ActionProtection {
    /// Regular action; bindable in v0.1.
    Standard,
    /// Protected action; rejected at bind in v0.1 (spine R6).
    Protected,
}

/// Role a channel plays for this provider (spine §4.1 `ChannelSupport.role`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ChannelRole {
    /// The channel can locate/act only.
    Act,
    /// The channel can verify only (the only legal role for `vision`).
    Verify,
    /// The channel can both act and verify.
    Both,
}

/// The protocol version window the provider supports (spine §4.1
/// `ProviderManifest.protocol`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolRange {
    /// Supported major version.
    pub major: u64,
    /// Lowest supported minor version.
    pub min_minor: u64,
    /// Highest supported minor version.
    pub max_minor: u64,
}

/// A feature the provider offers only under some platform condition
/// (spine §4.1 `ProviderManifest.features.conditional[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConditionalFeature {
    /// The feature id.
    pub feature: FeatureId,
    /// Platforms on which the feature is available; absent = unconditional
    /// on platform (some other runtime condition applies).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_platform: Option<Vec<PlatformKind>>,
}

/// The provider's feature declaration (spine §4.1
/// `ProviderManifest.features`). The compile-time available feature set is
/// `guaranteed ∪ lockfile.hello.featuresEnabled` (spine §4.1 compile rule).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeatureDeclarations {
    /// Features provided unconditionally.
    pub guaranteed: Vec<FeatureId>,
    /// Features provided only under declared conditions.
    pub conditional: Vec<ConditionalFeature>,
}

/// Declarative canonical-verb → native-action mapping (spine §4.1
/// `VerbBinding`; R7: the compiler executes zero provider code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerbBinding {
    /// The canonical verb being bound.
    pub verb: CanonicalVerb,
    /// The provider-native action name (e.g. `tap` → `tapElement`).
    pub action_name: ActionName,
    /// Feature gating the binding (e.g. the semantic five →
    /// `device.semanticActions.v1`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_feature: Option<FeatureId>,
    /// Declarative field mapping from verb argument names to native action
    /// argument names.
    pub arg_map: BTreeMap<String, String>,
}

/// A channel the provider supports and in which role (spine §4.1
/// `ChannelSupport`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelSupport {
    /// The channel.
    pub channel: Channel,
    /// Its role; `vision` may only declare `verify` (principle 7).
    pub role: ChannelRole,
    /// Feature gating the channel (e.g. `uiTree` →
    /// `observation.uiSnapshot.v1`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_feature: Option<FeatureId>,
    /// Platforms on which the channel is available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_platform: Option<Vec<PlatformKind>>,
}

/// Static definition of one provider action (spine §4.1
/// `ActionDefinitionStatic`; protocol-level actions such as the semantic
/// five can be built in).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionDefinitionStatic {
    /// Provider-native action name.
    pub name: ActionName,
    /// JSON Schema (Draft 2020-12) of the action arguments — same source as
    /// the protocol `ActionDefinition.inputSchema`.
    pub input_schema: JsonSchemaDocument,
    /// Pointlock-side output schema supplement (the protocol-side output is
    /// unconstrained JSON).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonSchemaDocument>,
    /// Protection level of the action.
    pub protection: ActionProtection,
    /// Provider-synthetic: the provider implements this action itself
    /// rather than forwarding it to a driver action (04 §9.4.3).
    ///
    /// `observe` and `screenshot` are the only such actions today: they
    /// map to the `device.observe` RPC, so no driver declares them and
    /// they never appear in a lockfile's action set. Bind therefore
    /// overlays them onto the lockfile's actions — **shadowed by name**,
    /// so a driver action of the same name wins and this entry is
    /// disabled for that lockfile.
    ///
    /// Only readonly actions may be synthetic: they are exempt from
    /// reconcile by construction, since a dangling readonly intent is
    /// always safe to replay (spine §6.7-B).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub synthetic: bool,
}

/// The provider-package-shipped, package-versioned capability declaration
/// (spine §4.1 `ProviderManifest`). Consumed by the compiler; no device
/// needs to be online.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderManifest {
    /// Provider name (e.g. `"devicerail"`).
    pub name: String,
    /// Provider package version.
    pub version: String,
    /// Supported protocol window (e.g. supports 1.5).
    pub protocol: ProtocolRange,
    /// Feature declaration.
    pub features: FeatureDeclarations,
    /// Declarative (non-code) verb → native action mappings.
    pub verb_bindings: Vec<VerbBinding>,
    /// Supported channels.
    pub channels: Vec<ChannelSupport>,
    /// Built-in protocol-level action definitions. Without a lockfile the
    /// compiler falls back to these, restricted to actions covered by
    /// `guaranteed` features (spine §4.1 compile rule).
    pub known_actions: Vec<ActionDefinitionStatic>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_manifest() -> ProviderManifest {
        ProviderManifest {
            name: "devicerail".to_owned(),
            version: "0.1.0".to_owned(),
            protocol: ProtocolRange {
                major: 1,
                min_minor: 5,
                max_minor: 5,
            },
            features: FeatureDeclarations {
                guaranteed: vec![FeatureId::new("device.semanticActions.v1").unwrap()],
                conditional: vec![ConditionalFeature {
                    feature: FeatureId::new("observation.uiSnapshot.v1").unwrap(),
                    requires_platform: Some(vec![PlatformKind::Android, PlatformKind::HarmonyOs]),
                }],
            },
            verb_bindings: vec![VerbBinding {
                verb: CanonicalVerb::Tap,
                action_name: ActionName::new("tapElement").unwrap(),
                requires_feature: Some(FeatureId::new("device.semanticActions.v1").unwrap()),
                arg_map: BTreeMap::from([("element".to_owned(), "element".to_owned())]),
            }],
            channels: vec![ChannelSupport {
                channel: Channel::UiTree,
                role: ChannelRole::Both,
                requires_feature: Some(FeatureId::new("observation.uiSnapshot.v1").unwrap()),
                requires_platform: None,
            }],
            known_actions: vec![ActionDefinitionStatic {
                name: ActionName::new("tapElement").unwrap(),
                input_schema: JsonSchemaDocument::new(json!(true)).unwrap(),
                output_schema: None,
                protection: ActionProtection::Standard,
                synthetic: false,
            }],
        }
    }

    #[test]
    fn manifest_wire_shape_is_camel_case() {
        let wire = serde_json::to_value(sample_manifest()).expect("serialize");
        assert_eq!(wire["protocol"]["minMinor"], 5);
        assert_eq!(wire["verbBindings"][0]["verb"], "tap");
        assert_eq!(wire["verbBindings"][0]["actionName"], "tapElement");
        assert_eq!(wire["verbBindings"][0]["argMap"]["element"], "element");
        assert_eq!(
            wire["features"]["conditional"][0]["requiresPlatform"],
            json!(["android", "harmonyOs"])
        );
        assert_eq!(wire["channels"][0]["role"], "both");
        assert_eq!(wire["knownActions"][0]["protection"], "standard");
        let back: ProviderManifest = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back, sample_manifest());
    }

    #[test]
    fn manifest_rejects_unknown_fields() {
        let mut wire = serde_json::to_value(sample_manifest()).expect("serialize");
        wire["surprise"] = json!(1);
        assert!(serde_json::from_value::<ProviderManifest>(wire).is_err());
    }

    #[test]
    fn platform_kind_wire_literals() {
        for (kind, literal) in [
            (PlatformKind::Android, "android"),
            (PlatformKind::Ios, "ios"),
            (PlatformKind::Web, "web"),
            (PlatformKind::HarmonyOs, "harmonyOs"),
            (PlatformKind::MacOs, "macOs"),
            (PlatformKind::Windows, "windows"),
            (PlatformKind::Linux, "linux"),
            (PlatformKind::Rdp, "rdp"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(literal));
        }
    }
}
