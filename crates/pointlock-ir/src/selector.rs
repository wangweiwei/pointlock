//! Element selectors and geometric types.
//!
//! [`ElementSelectorIR`] and [`TextMatchIR`] are isomorphic to DeviceRail
//! `ElementSelector` / `TextMatch` (field names and limits verbatim) with one
//! canonicalization difference (02 §2.4): absence is expressed by omitting
//! the field, never by null — the single-representation rule for hashing.
//! `pointlock-provider-devicerail` re-inserts nulls at the wire boundary.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::vocab::{TextMatchMode, UiContextKind};

/// Structured element selector (isomorphic to DeviceRail `ElementSelector`).
///
/// At least one property must be present (`minProperties: 1`) — enforced by
/// the schema; serde-side an all-`None` value is representable and left to
/// the compiler to reject.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(extend("minProperties" = 1))]
pub struct ElementSelectorIR {
    /// UI context scoping (native/web, optional context id).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<SelectorContext>,
    /// Accessibility role.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 256))]
    pub role: Option<String>,
    /// Accessible name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 65536))]
    pub name: Option<String>,
    /// Stable identifier (resource id / test id).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 4096))]
    pub identifier: Option<String>,
    /// Text content match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextMatchIR>,
    /// Current value match.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 65536))]
    pub value: Option<String>,
    /// CSS selector (web contexts).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 65536))]
    pub css: Option<String>,
}

/// UI context reference inside a selector (inline object in the baseline
/// schema, hence `#[schemars(inline)]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(inline)]
pub struct SelectorContext {
    /// Context kind (`native` | `web`).
    pub context_kind: UiContextKind,
    /// Optional concrete context id.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 4096))]
    pub context_id: Option<String>,
}

/// Text matcher (isomorphic to DeviceRail `TextMatch`).
///
/// `mode` and `caseSensitive` are required because sealed IR materializes
/// defaults (`exact` / `false`) — the single-representation rule (02 §2.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextMatchIR {
    /// The text to match.
    #[schemars(length(min = 1, max = 65536))]
    pub value: String,
    /// Match mode (materialized default: `exact`).
    pub mode: TextMatchMode,
    /// Case sensitivity (materialized default: `false`).
    pub case_sensitive: bool,
}

/// Rectangular region, field-compatible with DeviceRail `UiRect`.
///
/// Uses `serde_json::Number` (not `f64`) so integer inputs round-trip as
/// integers — required by the canonical-form rule that numbers keep their
/// shortest ES representation (02 §12.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RectIR {
    /// X origin.
    pub x: serde_json::Number,
    /// Y origin.
    pub y: serde_json::Number,
    /// Width (≥ 0).
    #[schemars(range(min = 0))]
    pub width: serde_json::Number,
    /// Height (≥ 0).
    #[schemars(range(min = 0))]
    pub height: serde_json::Number,
}
