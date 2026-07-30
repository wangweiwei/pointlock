//! Source mapping: IR path → YAML span, plus macro origin traces (02 §7).
//!
//! Pure diagnostics — excluded from `irHash` (02 §12.2): moving a comment or
//! a macro call site must not invalidate resume history.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::primitives::{Identifier, JsonPointer};

/// One source-map entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceMapEntry {
    /// RFC 6901 JSON Pointer into this FlowIR document.
    pub ir_path: JsonPointer,
    /// The YAML source file.
    #[schemars(length(min = 1))]
    pub file: String,
    /// The source span.
    pub span: SourceSpan,
    /// Macro expansion chain, innermost first. Present iff the IR node was
    /// produced by macro expansion — the only structural residue macros
    /// leave in the IR (02 §7).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub origin: Option<Vec<MacroOriginFrame>>,
}

/// A 1-based line/column span in a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSpan {
    /// Start line (1-based).
    #[schemars(range(min = 1))]
    pub start_line: u32,
    /// Start column (1-based).
    #[schemars(range(min = 1))]
    pub start_col: u32,
    /// End line (1-based).
    #[schemars(range(min = 1))]
    pub end_line: u32,
    /// End column (1-based).
    #[schemars(range(min = 1))]
    pub end_col: u32,
}

/// One frame of a macro expansion chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MacroOriginFrame {
    /// The macro's name.
    pub r#macro: Identifier,
    /// The file containing the expansion site.
    #[schemars(length(min = 1))]
    pub file: String,
    /// The span of the expansion site.
    pub span: SourceSpan,
}
