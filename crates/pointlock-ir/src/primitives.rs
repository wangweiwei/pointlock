//! Validated string primitives of the IR.
//!
//! Every grammar-bearing string in the baseline schema
//! (`schema/flow-ir.v0.1.schema.json`) is a newtype here whose constructor
//! enforces exactly the baseline pattern, so an in-memory value can never
//! hold a string the wire schema would reject. Deserialization goes through
//! the same constructor (`#[serde(try_from = "String")]`).

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// Error returned when a validated primitive is constructed from an
/// out-of-grammar string (or, for [`JsonSchemaDocument`], a non-object /
/// non-boolean JSON value).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind}: {reason} (value: {value:?})")]
pub struct ValidationError {
    /// The primitive type that rejected the value (e.g. `"StepId"`).
    pub kind: &'static str,
    /// Human-readable reason for the rejection.
    pub reason: &'static str,
    /// The offending input, verbatim.
    pub value: String,
}

// ─── Character-class helpers (hand-rolled; grammar identical to the baseline
//     schema regexes, kept regex-crate-free to preserve the "no runtime deps
//     beyond serde/schemars" stance of this crate) ──────────────────────────

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_step_segment_continue(c: char) -> bool {
    is_ident_continue(c) || c == '-'
}

/// `[A-Za-z_][A-Za-z0-9_]*`
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if is_ident_start(c)) && chars.all(is_ident_continue)
}

/// `[A-Za-z_][A-Za-z0-9_-]*` — one author-level step-id segment.
fn is_step_segment(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if is_ident_start(c)) && chars.all(is_step_segment_continue)
}

/// `Ident('.'Ident)*` — dotted identifier path with at least one segment.
fn is_dotted_identifiers(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(is_identifier)
}

// ─── Newtype factory ────────────────────────────────────────────────────────

macro_rules! string_newtype {
    (
        $(#[$meta:meta])*
        $name:ident {
            kind: $kind:literal,
            validate: $validate:expr,
            inline: $inline:expr,
            schema: $schema:tt $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validates `value` against the grammar and constructs the newtype.
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value = value.into();
                match ($validate)(value.as_str()) {
                    Ok(()) => Ok(Self(value)),
                    Err(reason) => Err(ValidationError { kind: $kind, reason, value }),
                }
            }

            /// Borrows the underlying string.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consumes the newtype, returning the underlying string.
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValidationError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = ValidationError;
            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl std::str::FromStr for $name {
            type Err = ValidationError;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl JsonSchema for $name {
            fn inline_schema() -> bool {
                $inline
            }
            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($name))
            }
            fn schema_id() -> Cow<'static, str> {
                Cow::Borrowed(concat!("pointlock_ir::", stringify!($name)))
            }
            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!($schema)
            }
        }
    };
}

// ─── The validated primitives ───────────────────────────────────────────────

string_newtype! {
    /// Content hash in canonical form `sha256:<64 lowercase hex>` (spine §3).
    Hash {
        kind: "Hash",
        validate: |s: &str| -> Result<(), &'static str> {
            let Some(hex) = s.strip_prefix("sha256:") else {
                return Err("must start with 'sha256:'");
            };
            if hex.len() != 64 {
                return Err("hex digest must be exactly 64 characters");
            }
            if !hex.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)) {
                return Err("hex digest must be lowercase [0-9a-f]");
            }
            Ok(())
        },
        inline: false,
        schema: {
            "type": "string",
            "pattern": "^sha256:[0-9a-f]{64}$",
            "description": "Content hash, canonical form 'sha256:<64 lowercase hex>'."
        },
    }
}

impl Hash {
    /// The 64-char lowercase hex digest, without the `sha256:` prefix.
    pub fn hex(&self) -> &str {
        &self.0["sha256:".len()..]
    }

    /// The first 8 hex chars of the digest — the abbreviation used by the
    /// canonical RunPath rendering (07 §2.1: `<flowId>@<hash8>`).
    pub fn hex_prefix8(&self) -> &str {
        &self.hex()[..8]
    }
}

string_newtype! {
    /// Flow identifier: `^[A-Za-z_][A-Za-z0-9_.-]*$`, max 256 chars.
    FlowId {
        kind: "FlowId",
        validate: |s: &str| -> Result<(), &'static str> {
            if s.len() > 256 {
                return Err("exceeds 256 characters");
            }
            let mut chars = s.chars();
            match chars.next() {
                Some(c) if is_ident_start(c) => {}
                _ => return Err("must start with [A-Za-z_]"),
            }
            if !chars.all(|c| is_ident_continue(c) || c == '.' || c == '-') {
                return Err("allowed characters after the first are [A-Za-z0-9_.-]");
            }
            Ok(())
        },
        inline: false,
        schema: {
            "type": "string",
            "pattern": "^[A-Za-z_][A-Za-z0-9_.-]*$",
            "maxLength": 256
        },
    }
}

string_newtype! {
    /// Step identifier (spine §3, 02 §3).
    ///
    /// Author-written ids are a single segment `[A-Za-z_][A-Za-z0-9_-]*`.
    /// `:`-separated additional segments are reserved for compiler-synthesized
    /// ids (handler-embedded human steps such as `flow:onUnknown:escalate`,
    /// macro hygiene prefixes such as `set_ssid:field`). Synthesized ids never
    /// appear in ref paths ([`RefPath`] has no `:` in its grammar).
    StepId {
        kind: "StepId",
        validate: |s: &str| -> Result<(), &'static str> {
            if s.len() > 256 {
                return Err("exceeds 256 characters");
            }
            if s.is_empty() {
                return Err("must not be empty");
            }
            if !s.split(':').all(is_step_segment) {
                return Err("each ':'-separated segment must match [A-Za-z_][A-Za-z0-9_-]*");
            }
            Ok(())
        },
        inline: false,
        schema: {
            "type": "string",
            "pattern": "^[A-Za-z_][A-Za-z0-9_-]*(:[A-Za-z_][A-Za-z0-9_-]*)*$",
            "maxLength": 256,
            "description": "Author-written ids use the first segment only ([A-Za-z_][A-Za-z0-9_-]*). ':'-separated segments are reserved for compiler-synthesized ids (e.g. handler-embedded human steps 'host:onFail:escalate'); synthesized ids never appear in ref paths."
        },
    }
}

impl StepId {
    /// `true` when this id carries a compiler-synthesized `:` segment
    /// (macro hygiene prefix or handler-embedded step id).
    pub fn is_synthesized(&self) -> bool {
        self.0.contains(':')
    }
}

string_newtype! {
    /// Plain identifier: `^[A-Za-z_][A-Za-z0-9_]*$`, max 128 chars.
    /// Used for param/output names, `ExprMap` keys, `foreach.as`, etc.
    Identifier {
        kind: "Identifier",
        validate: |s: &str| -> Result<(), &'static str> {
            if s.len() > 128 {
                return Err("exceeds 128 characters");
            }
            if !is_identifier(s) {
                return Err("must match [A-Za-z_][A-Za-z0-9_]*");
            }
            Ok(())
        },
        inline: false,
        schema: {
            "type": "string",
            "pattern": "^[A-Za-z_][A-Za-z0-9_]*$",
            "maxLength": 128
        },
    }
}

string_newtype! {
    /// DeviceRail feature id passthrough (e.g. `device.semanticActions.v1`);
    /// future Pointlock-owned features use the `pointlock.` prefix.
    FeatureId {
        kind: "FeatureId",
        validate: |s: &str| -> Result<(), &'static str> {
            // ^[a-z][a-zA-Z0-9]*(\.[a-zA-Z0-9]+)*\.v[0-9]+$
            let segments: Vec<&str> = s.split('.').collect();
            if segments.len() < 2 {
                return Err("must have at least two '.'-separated segments ending in vN");
            }
            let first = segments[0];
            let mut chars = first.chars();
            match chars.next() {
                Some(c) if c.is_ascii_lowercase() => {}
                _ => return Err("first segment must start with a lowercase letter"),
            }
            if !chars.all(|c| c.is_ascii_alphanumeric()) {
                return Err("first segment must be alphanumeric");
            }
            let last = segments[segments.len() - 1];
            let Some(version) = last.strip_prefix('v') else {
                return Err("last segment must be 'v' followed by digits");
            };
            if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit()) {
                return Err("last segment must be 'v' followed by digits");
            }
            for middle in &segments[1..segments.len() - 1] {
                if middle.is_empty() || !middle.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return Err("middle segments must be non-empty alphanumeric");
                }
            }
            Ok(())
        },
        inline: false,
        schema: {
            "type": "string",
            "pattern": "^[a-z][a-zA-Z0-9]*(\\.[a-zA-Z0-9]+)*\\.v[0-9]+$",
            "description": "DeviceRail feature id passthrough (e.g. 'device.semanticActions.v1'); future Pointlock-owned features use the 'pointlock.' prefix."
        },
    }
}

string_newtype! {
    /// Provider-native action name (DeviceRail camelCase, e.g. `tapElement`).
    ///
    /// Canonical verbs never appear here; provider-qualified names
    /// (`<provider>:<action>`) are reserved for multi-provider futures and
    /// rejected in v0.1 (spine R7 / A.6).
    ActionName {
        kind: "ActionName",
        validate: |s: &str| -> Result<(), &'static str> {
            if s.len() > 256 {
                return Err("exceeds 256 characters");
            }
            let mut chars = s.chars();
            match chars.next() {
                Some(c) if c.is_ascii_alphabetic() => {}
                _ => return Err("must start with [A-Za-z]"),
            }
            if !chars.all(is_ident_continue) {
                return Err("allowed characters after the first are [A-Za-z0-9_]");
            }
            Ok(())
        },
        inline: false,
        schema: {
            "type": "string",
            "pattern": "^[A-Za-z][A-Za-z0-9_]*$",
            "maxLength": 256,
            "description": "Provider-native action name (DeviceRail camelCase, e.g. 'tapElement'). Canonical verbs never appear here; provider-qualified names ('<provider>:<action>') are reserved for multi-provider futures and rejected in v0.1."
        },
    }
}

string_newtype! {
    /// Assertion identifier: `^[A-Za-z_][A-Za-z0-9_-]*$`, max 128 chars.
    ///
    /// Inlined in the schema (the baseline declares this grammar inline on
    /// `AssertionIR.assertId` rather than as a named `$def`).
    AssertId {
        kind: "AssertId",
        validate: |s: &str| -> Result<(), &'static str> {
            if s.len() > 128 {
                return Err("exceeds 128 characters");
            }
            if !is_step_segment(s) {
                return Err("must match [A-Za-z_][A-Za-z0-9_-]*");
            }
            Ok(())
        },
        inline: true,
        schema: {
            "type": "string",
            "pattern": "^[A-Za-z_][A-Za-z0-9_-]*$",
            "maxLength": 128
        },
    }
}

string_newtype! {
    /// Closed-scope reference path of the expression grammar (spine §7, 02 §8.1):
    ///
    /// ```ebnf
    /// RefPath ::= 'params.' Ident ('.' Ident)*
    ///           | 'env.'    Ident
    ///           | 'vars.'   Ident
    ///           | 'iter.'   Ident
    ///           | 'steps.'  StepIdRef '.' 'output' ('.' Ident)*
    ///           | 'steps.'  StepIdRef '.' 'verdict'
    /// ```
    ///
    /// Dotted identifier segments only; array/deep access goes through the
    /// `jsonPath` pure function. `secrets.*` is reserved for v0.2 and rejected
    /// today. `StepIdRef` deliberately has no `:`: compiler-synthesized step
    /// ids can never be referenced.
    RefPath {
        kind: "RefPath",
        validate: |s: &str| -> Result<(), &'static str> {
            if s.len() > 1024 {
                return Err("exceeds 1024 characters");
            }
            if let Some(rest) = s.strip_prefix("params.") {
                return if is_dotted_identifiers(rest) {
                    Ok(())
                } else {
                    Err("params.* takes a dotted identifier path")
                };
            }
            for prefix in ["env.", "vars.", "iter."] {
                if let Some(rest) = s.strip_prefix(prefix) {
                    return if is_identifier(rest) {
                        Ok(())
                    } else {
                        Err("env./vars./iter. take exactly one identifier")
                    };
                }
            }
            if let Some(rest) = s.strip_prefix("steps.") {
                let Some((step_id, tail)) = rest.split_once('.') else {
                    return Err("steps.<id> must be followed by .output or .verdict");
                };
                if !is_step_segment(step_id) {
                    return Err("step id segment must match [A-Za-z_][A-Za-z0-9_-]* (synthesized ':' ids are not referenceable)");
                }
                if tail == "verdict" || tail == "output" {
                    return Ok(());
                }
                if let Some(fields) = tail.strip_prefix("output.") {
                    return if is_dotted_identifiers(fields) {
                        Ok(())
                    } else {
                        Err("output field access takes a dotted identifier path")
                    };
                }
                return Err("steps.<id> supports only .verdict and .output(.field)*");
            }
            Err("scope root must be one of params/env/vars/iter/steps ('secrets' is reserved for v0.2)")
        },
        inline: false,
        schema: {
            "type": "string",
            "pattern": "^(params\\.[A-Za-z_][A-Za-z0-9_]*(\\.[A-Za-z_][A-Za-z0-9_]*)*|env\\.[A-Za-z_][A-Za-z0-9_]*|vars\\.[A-Za-z_][A-Za-z0-9_]*|iter\\.[A-Za-z_][A-Za-z0-9_]*|steps\\.[A-Za-z_][A-Za-z0-9_-]*\\.(verdict|output(\\.[A-Za-z_][A-Za-z0-9_]*)*))$",
            "maxLength": 1024,
            "description": "Closed scope grammar: params.* | env.* | vars.* | iter.* | steps.<id>.output(.field)* | steps.<id>.verdict. Dotted identifier segments only; array/deep access goes through the jsonPath pure function."
        },
    }
}

impl RefPath {
    /// Returns the referenced upstream step id when this path is rooted at
    /// `steps.` (the syntactic data-dependency answer of 02 §8.1), or `None`
    /// for the other scope roots.
    pub fn referenced_step(&self) -> Option<&str> {
        let rest = self.0.strip_prefix("steps.")?;
        rest.split_once('.').map(|(step_id, _)| step_id)
    }

    /// Returns the scope root of the path (`params`, `env`, `vars`, `iter`
    /// or `steps`).
    pub fn scope_root(&self) -> &str {
        self.0
            .split_once('.')
            .map(|(root, _)| root)
            .unwrap_or(&self.0)
    }
}

string_newtype! {
    /// RFC 6901 JSON Pointer into a FlowIR document (`SourceMapEntry.irPath`).
    JsonPointer {
        kind: "JsonPointer",
        validate: |s: &str| -> Result<(), &'static str> {
            // ^(/([^~/]|~0|~1)*)*$  — empty is legal (whole document);
            // otherwise each token starts with '/', and '~' must be followed
            // by '0' or '1'.
            if !s.is_empty() && !s.starts_with('/') {
                return Err("non-empty pointer must start with '/'");
            }
            let mut chars = s.chars();
            while let Some(c) = chars.next() {
                if c == '~' && !matches!(chars.next(), Some('0') | Some('1')) {
                    return Err("'~' must be followed by '0' or '1'");
                }
            }
            Ok(())
        },
        inline: true,
        schema: {
            "type": "string",
            "pattern": "^(/([^~/]|~0|~1)*)*$",
            "description": "RFC 6901 JSON Pointer into this FlowIR document."
        },
    }
}

// ─── Literal markers (const wire values) ────────────────────────────────────

/// Declares a single-variant enum that serializes to exactly one string
/// literal — the Rust encoding of a schema `const` field. Deserialization of
/// any other value fails, which is how `onMissingInput: "unknown"`,
/// `protection: "standard"`, step `kind` tags, etc. are enforced at the type
/// level.
macro_rules! literal_marker {
    (
        $(#[$meta:meta])*
        $name:ident => $lit:literal
    ) => {
        $(#[$meta])*
        #[derive(
            Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
            Serialize, Deserialize, JsonSchema,
        )]
        #[schemars(inline)]
        pub enum $name {
            /// The only admissible wire value.
            #[default]
            #[serde(rename = $lit)]
            Value,
        }
    };
}
pub(crate) use literal_marker;

literal_marker! {
    /// `onMissingInput: "unknown"` — principle 4 typed: an assertion whose
    /// input is missing yields `unknown`, never a guess.
    OnMissingInput => "unknown"
}

literal_marker! {
    /// `onTimeout: "unknown"` — a human step that times out never defaults
    /// to pass or fail (principles 4/8).
    OnTimeout => "unknown"
}

literal_marker! {
    /// `protection: "standard"` — v0.1 casts spine R6 into the type system:
    /// protected actions are rejected at bind. Widening this to an enum in
    /// v0.2 (`secrets.*` / `protected: true`) is a breaking change (02 §11).
    Protection => "standard"
}

literal_marker! {
    /// `provider.name: "devicerail"` — the only provider of v0.1.
    ProviderName => "devicerail"
}

// ─── IrVersion (const 1) ────────────────────────────────────────────────────

/// The IR semantic-generation number, pinned to the integer `1` in v0.1
/// (02 §11). Serializes as the JSON number `1`; any other value fails
/// deserialization (runners match `irVersion` exactly, fail-closed).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IrVersion;

impl IrVersion {
    /// The numeric value of this version marker.
    pub const VALUE: u64 = 1;
}

impl Serialize for IrVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(Self::VALUE)
    }
}

impl<'de> Deserialize<'de> for IrVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u64::deserialize(deserializer)?;
        if value == Self::VALUE {
            Ok(IrVersion)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported irVersion {value}: this crate implements irVersion {}",
                Self::VALUE
            )))
        }
    }
}

impl JsonSchema for IrVersion {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("IrVersion")
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("pointlock_ir::IrVersion")
    }
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({ "const": 1 })
    }
}

// ─── JsonSchemaDocument ─────────────────────────────────────────────────────

/// An embedded JSON Schema (Draft 2020-12) document.
///
/// Deliberately open (baseline exemption class 1): its internal grammar is
/// governed by the JSON Schema meta-schema, not by the IR schema. The only
/// shape constraint enforced here is the JSON Schema data model itself: a
/// schema document is an object or a boolean.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "serde_json::Value", into = "serde_json::Value")]
pub struct JsonSchemaDocument(serde_json::Value);

impl JsonSchemaDocument {
    /// Validates that `value` is an object or boolean and wraps it.
    pub fn new(value: serde_json::Value) -> Result<Self, ValidationError> {
        match &value {
            serde_json::Value::Object(_) | serde_json::Value::Bool(_) => Ok(Self(value)),
            other => Err(ValidationError {
                kind: "JsonSchemaDocument",
                reason: "a JSON Schema document must be an object or a boolean",
                value: other.to_string(),
            }),
        }
    }

    /// Borrows the underlying JSON value.
    pub fn as_value(&self) -> &serde_json::Value {
        &self.0
    }

    /// Consumes the wrapper, returning the underlying JSON value.
    pub fn into_value(self) -> serde_json::Value {
        self.0
    }
}

impl TryFrom<serde_json::Value> for JsonSchemaDocument {
    type Error = ValidationError;
    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<JsonSchemaDocument> for serde_json::Value {
    fn from(doc: JsonSchemaDocument) -> serde_json::Value {
        doc.0
    }
}

impl JsonSchema for JsonSchemaDocument {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("JsonSchemaDocument")
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("pointlock_ir::JsonSchemaDocument")
    }
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "anyOf": [{ "type": "object" }, { "type": "boolean" }],
            "description": "An embedded JSON Schema (Draft 2020-12) document. Deliberately open (exemption class 1): its internal grammar is governed by the JSON Schema meta-schema, not by this schema."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_grammar() {
        assert!(Hash::new(format!("sha256:{}", "a".repeat(64))).is_ok());
        assert!(Hash::new(format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(Hash::new("sha256:abcd").is_err());
        assert!(Hash::new(format!("sha1:{}", "a".repeat(64))).is_err());
    }

    #[test]
    fn step_id_grammar() {
        assert!(StepId::new("open_wifi_settings").is_ok());
        assert!(StepId::new("set_ssid:field").is_ok());
        assert!(StepId::new("flow:onUnknown:escalate").is_ok());
        assert!(StepId::new("9bad").is_err());
        assert!(StepId::new("a:").is_err());
        assert!(StepId::new(":a").is_err());
        assert!(StepId::new("a..b").is_err());
        assert!(StepId::new("").is_err());
        assert!(!StepId::new("plain").unwrap().is_synthesized());
        assert!(StepId::new("m:x").unwrap().is_synthesized());
    }

    #[test]
    fn ref_path_grammar() {
        for ok in [
            "params.ssid",
            "params.a.b.c",
            "env.deviceId",
            "vars.report_label",
            "iter.item",
            "steps.wifi_on_visible.verdict",
            "steps.wait_connected.output",
            "steps.wait_connected.output.matched",
            "steps.x.output.a.b",
        ] {
            assert!(RefPath::new(ok).is_ok(), "expected accept: {ok}");
        }
        for bad in [
            "secrets.token",
            "params.",
            "env.a.b",
            "steps.x",
            "steps.x.outputs",
            "steps.x.verdict.status",
            "steps.set_ssid:field.output",
            "steps..output",
            "output.x",
            "",
        ] {
            assert!(RefPath::new(bad).is_err(), "expected reject: {bad}");
        }
        assert_eq!(
            RefPath::new("steps.wait_connected.output.matched")
                .unwrap()
                .referenced_step(),
            Some("wait_connected")
        );
        assert_eq!(RefPath::new("params.ssid").unwrap().referenced_step(), None);
    }

    #[test]
    fn feature_id_grammar() {
        assert!(FeatureId::new("device.semanticActions.v1").is_ok());
        assert!(FeatureId::new("verdict.record.v1").is_ok());
        assert!(FeatureId::new("session.export.page.v1").is_ok());
        assert!(FeatureId::new("Device.foo.v1").is_err());
        assert!(FeatureId::new("device.foo").is_err());
        assert!(FeatureId::new("device..v1").is_err());
        assert!(FeatureId::new("v1").is_err());
    }

    #[test]
    fn json_pointer_grammar() {
        assert!(JsonPointer::new("").is_ok());
        assert!(JsonPointer::new("/body/0").is_ok());
        assert!(JsonPointer::new("/a~0b/~1c").is_ok());
        assert!(JsonPointer::new("body/0").is_err());
        assert!(JsonPointer::new("/a~2b").is_err());
        assert!(JsonPointer::new("/a~").is_err());
    }

    #[test]
    fn ir_version_round_trip() {
        assert_eq!(
            serde_json::to_value(IrVersion).unwrap(),
            serde_json::json!(1)
        );
        assert!(serde_json::from_value::<IrVersion>(serde_json::json!(1)).is_ok());
        assert!(serde_json::from_value::<IrVersion>(serde_json::json!(2)).is_err());
    }
}
