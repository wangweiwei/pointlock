//! Phase 1 `parse`: YAML bytes → spanned AST (spine §8 stage 1, 03 §4.2).
//!
//! Event-driven over `saphyr-parser` (yaml-rust2 lineage, span markers —
//! serde_yaml is unmaintained and out per R12). The builder enforces,
//! fail-closed:
//!
//! - byte / depth / node-count limits (node count includes alias
//!   expansion — billion-laughs protection);
//! - values limited to the JSON data model: every tag is rejected, mapping
//!   keys must be strings, non-finite floats and out-of-range integers are
//!   rejected;
//! - YAML merge key `<<` rejected (YAML 1.1 extension, 03 §1.9);
//! - duplicate mapping keys rejected;
//! - exactly one document whose root is a mapping.
//!
//! Anchors/aliases are value reuse (03 §1.9): aliases are resolved here by
//! deep copy; copied nodes keep the spans of the anchor definition site, so
//! later diagnostics point at the line the author actually wrote.

// The internal event-walk helpers return `CompileDiagnostic` directly; the
// 03 §4.4 fields (stage/file/candidates/hint) grew the struct past clippy's
// Err-size comfort. These are cold error paths in a batch compiler — boxing
// every construction site would trade real noise for a theoretical win.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;

use saphyr_parser::{Event, Parser, ScalarStyle, Span, Tag};

use crate::diagnostic::{CompileDiagnostic, DiagnosticSpan, codes};

/// Maximum accepted source size in bytes.
pub(crate) const MAX_SOURCE_BYTES: usize = 1024 * 1024;
/// Maximum container nesting depth.
pub(crate) const MAX_DEPTH: usize = 64;
/// Maximum node count after alias expansion.
pub(crate) const MAX_NODES: usize = 65_536;

/// A spanned YAML node, values limited to the JSON data model.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Node {
    /// The node's value.
    pub value: NodeValue,
    /// The node's source span (anchor definition site for alias copies).
    pub span: DiagnosticSpan,
}

/// The JSON data model, spanned.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NodeValue {
    /// JSON null.
    Null,
    /// JSON boolean.
    Bool(bool),
    /// JSON number.
    Number(serde_json::Number),
    /// JSON string.
    String(String),
    /// JSON array.
    Sequence(Vec<Node>),
    /// JSON object (order-preserving; keys are strings by construction).
    Mapping(Vec<MapEntry>),
}

/// One mapping entry with the key's own span.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MapEntry {
    /// The (string) key.
    pub key: String,
    /// Span of the key scalar.
    pub key_span: DiagnosticSpan,
    /// The value node.
    pub value: Node,
}

impl Node {
    /// Projects the node onto `serde_json::Value`, dropping spans.
    pub fn to_json(&self) -> serde_json::Value {
        match &self.value {
            NodeValue::Null => serde_json::Value::Null,
            NodeValue::Bool(b) => serde_json::Value::Bool(*b),
            NodeValue::Number(n) => serde_json::Value::Number(n.clone()),
            NodeValue::String(s) => serde_json::Value::String(s.clone()),
            NodeValue::Sequence(items) => {
                serde_json::Value::Array(items.iter().map(Node::to_json).collect())
            }
            NodeValue::Mapping(entries) => serde_json::Value::Object(
                entries
                    .iter()
                    .map(|entry| (entry.key.clone(), entry.value.to_json()))
                    .collect(),
            ),
        }
    }

    /// Human-readable name of the node's JSON type (for diagnostics).
    pub fn type_name(&self) -> &'static str {
        match &self.value {
            NodeValue::Null => "null",
            NodeValue::Bool(_) => "boolean",
            NodeValue::Number(_) => "number",
            NodeValue::String(_) => "string",
            NodeValue::Sequence(_) => "sequence",
            NodeValue::Mapping(_) => "mapping",
        }
    }
}

/// Number of nodes in a subtree (for the alias-expansion budget).
fn node_size(node: &Node) -> usize {
    match &node.value {
        NodeValue::Sequence(items) => 1 + items.iter().map(node_size).sum::<usize>(),
        NodeValue::Mapping(entries) => {
            1 + entries
                .iter()
                .map(|entry| 1 + node_size(&entry.value))
                .sum::<usize>()
        }
        _ => 1,
    }
}

/// Saturates a parser marker coordinate into `u32`.
fn coord(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Converts a parser span (line 1-based, col 0-based) to the 1-based
/// diagnostic span.
fn to_span(span: Span) -> DiagnosticSpan {
    DiagnosticSpan {
        line: coord(span.start.line()),
        col: coord(span.start.col() + 1),
        end_line: coord(span.end.line()),
        end_col: coord(span.end.col() + 1),
    }
}

/// One open container during building.
enum Frame {
    Sequence {
        items: Vec<Node>,
        span: DiagnosticSpan,
        anchor: usize,
    },
    Mapping {
        entries: Vec<MapEntry>,
        pending_key: Option<(String, DiagnosticSpan)>,
        span: DiagnosticSpan,
        anchor: usize,
    },
}

/// The event-stream builder.
struct Builder {
    stack: Vec<Frame>,
    root: Option<Node>,
    documents: usize,
    node_count: usize,
    anchors: HashMap<usize, Node>,
}

impl Builder {
    fn new() -> Self {
        Builder {
            stack: Vec::new(),
            root: None,
            documents: 0,
            node_count: 0,
            anchors: HashMap::new(),
        }
    }

    fn budget(&mut self, nodes: usize, span: DiagnosticSpan) -> Result<(), CompileDiagnostic> {
        self.node_count = self.node_count.saturating_add(nodes);
        if self.node_count > MAX_NODES {
            return Err(CompileDiagnostic::error(
                codes::PARSE_NODE_LIMIT,
                format!("node count exceeds the limit of {MAX_NODES} (after alias expansion)"),
                Some(span),
            ));
        }
        Ok(())
    }

    fn check_depth(&self, span: DiagnosticSpan) -> Result<(), CompileDiagnostic> {
        if self.stack.len() >= MAX_DEPTH {
            return Err(CompileDiagnostic::error(
                codes::PARSE_DEPTH_LIMIT,
                format!("nesting depth exceeds the limit of {MAX_DEPTH}"),
                Some(span),
            ));
        }
        Ok(())
    }

    fn reject_tag(&self, tag: Option<&Tag>, span: DiagnosticSpan) -> Result<(), CompileDiagnostic> {
        if let Some(tag) = tag {
            return Err(CompileDiagnostic::error(
                codes::PARSE_TAG_REJECTED,
                format!("YAML tag '{tag}' rejected: values are limited to the JSON data model"),
                Some(span),
            ));
        }
        Ok(())
    }

    /// Routes a completed node to its parent (or the document root).
    fn push_node(&mut self, node: Node, anchor: usize) -> Result<(), CompileDiagnostic> {
        if anchor != 0 {
            self.anchors.insert(anchor, node.clone());
        }
        match self.stack.last_mut() {
            None => {
                if self.root.is_some() {
                    return Err(CompileDiagnostic::error(
                        codes::PARSE_DOCUMENT_SHAPE,
                        "the stream must contain exactly one YAML document",
                        Some(node.span),
                    ));
                }
                self.root = Some(node);
                Ok(())
            }
            Some(Frame::Sequence { items, .. }) => {
                items.push(node);
                Ok(())
            }
            Some(Frame::Mapping {
                entries,
                pending_key,
                ..
            }) => {
                match pending_key.take() {
                    None => {
                        // The node is a key: it must be a string of the JSON
                        // data model, and the merge key `<<` is rejected.
                        let NodeValue::String(key) = node.value else {
                            return Err(CompileDiagnostic::error(
                                codes::PARSE_NON_STRING_KEY,
                                format!(
                                    "mapping keys must be strings (JSON data model), got {}",
                                    node.type_name()
                                ),
                                Some(node.span),
                            ));
                        };
                        if key == "<<" {
                            return Err(CompileDiagnostic::error(
                                codes::PARSE_MERGE_KEY,
                                "YAML merge key '<<' is rejected (fail-closed, 03 §1.9)",
                                Some(node.span),
                            ));
                        }
                        if entries.iter().any(|entry| entry.key == key) {
                            return Err(CompileDiagnostic::error(
                                codes::PARSE_DUPLICATE_KEY,
                                format!("duplicate mapping key '{key}'"),
                                Some(node.span),
                            ));
                        }
                        *pending_key = Some((key, node.span));
                    }
                    Some((key, key_span)) => {
                        entries.push(MapEntry {
                            key,
                            key_span,
                            value: node,
                        });
                    }
                }
                Ok(())
            }
        }
    }

    fn on_event(&mut self, event: Event<'_>, span: Span) -> Result<(), CompileDiagnostic> {
        let dspan = to_span(span);
        match event {
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => Ok(()),
            Event::DocumentStart(_) => {
                self.documents += 1;
                if self.documents > 1 {
                    return Err(CompileDiagnostic::error(
                        codes::PARSE_DOCUMENT_SHAPE,
                        "the stream must contain exactly one YAML document",
                        Some(dspan),
                    ));
                }
                Ok(())
            }
            Event::Alias(anchor_id) => {
                let Some(copy) = self.anchors.get(&anchor_id).cloned() else {
                    // The parser errors on undefined aliases itself; this is
                    // defensive.
                    return Err(CompileDiagnostic::error(
                        codes::PARSE_SYNTAX,
                        "alias refers to an undefined anchor",
                        Some(dspan),
                    ));
                };
                self.budget(node_size(&copy), dspan)?;
                self.push_node(copy, 0)
            }
            Event::Scalar(value, style, anchor, tag) => {
                self.reject_tag(tag.as_deref(), dspan)?;
                self.budget(1, dspan)?;
                let node_value = if style == ScalarStyle::Plain {
                    resolve_plain_scalar(&value, dspan)?
                } else {
                    NodeValue::String(value.into_owned())
                };
                self.push_node(
                    Node {
                        value: node_value,
                        span: dspan,
                    },
                    anchor,
                )
            }
            Event::SequenceStart(anchor, tag) => {
                self.reject_tag(tag.as_deref(), dspan)?;
                self.check_depth(dspan)?;
                self.budget(1, dspan)?;
                self.stack.push(Frame::Sequence {
                    items: Vec::new(),
                    span: dspan,
                    anchor,
                });
                Ok(())
            }
            Event::SequenceEnd => {
                let Some(Frame::Sequence {
                    items,
                    span: start,
                    anchor,
                }) = self.stack.pop()
                else {
                    // Unreachable for a well-formed event stream.
                    return Err(CompileDiagnostic::error(
                        codes::PARSE_SYNTAX,
                        "unbalanced sequence end event",
                        Some(dspan),
                    ));
                };
                let node = Node {
                    value: NodeValue::Sequence(items),
                    span: join_spans(start, dspan),
                };
                self.push_node(node, anchor)
            }
            Event::MappingStart(anchor, tag) => {
                self.reject_tag(tag.as_deref(), dspan)?;
                self.check_depth(dspan)?;
                self.budget(1, dspan)?;
                self.stack.push(Frame::Mapping {
                    entries: Vec::new(),
                    pending_key: None,
                    span: dspan,
                    anchor,
                });
                Ok(())
            }
            Event::MappingEnd => {
                let Some(Frame::Mapping {
                    entries,
                    span: start,
                    anchor,
                    ..
                }) = self.stack.pop()
                else {
                    return Err(CompileDiagnostic::error(
                        codes::PARSE_SYNTAX,
                        "unbalanced mapping end event",
                        Some(dspan),
                    ));
                };
                // Mapping keys are budgeted here (values and the container
                // itself were budgeted on their own events).
                self.budget(entries.len(), dspan)?;
                let node = Node {
                    value: NodeValue::Mapping(entries),
                    span: join_spans(start, dspan),
                };
                self.push_node(node, anchor)
            }
        }
    }
}

/// Extends `start` to cover up to `end`.
fn join_spans(start: DiagnosticSpan, end: DiagnosticSpan) -> DiagnosticSpan {
    DiagnosticSpan {
        line: start.line,
        col: start.col,
        end_line: end.end_line,
        end_col: end.end_col,
    }
}

/// Resolves a plain scalar per the YAML 1.2 core schema, restricted to the
/// JSON data model (non-finite floats and integer overflow rejected).
fn resolve_plain_scalar(raw: &str, span: DiagnosticSpan) -> Result<NodeValue, CompileDiagnostic> {
    match raw {
        "" | "~" | "null" | "Null" | "NULL" => return Ok(NodeValue::Null),
        "true" | "True" | "TRUE" => return Ok(NodeValue::Bool(true)),
        "false" | "False" | "FALSE" => return Ok(NodeValue::Bool(false)),
        _ => {}
    }
    let non_json = |what: &str| {
        CompileDiagnostic::error(
            codes::PARSE_NON_JSON_SCALAR,
            format!("scalar '{raw}' is outside the JSON data model ({what})"),
            Some(span),
        )
    };
    // Explicitly reject the YAML core-schema non-finite floats.
    let unsigned = raw.strip_prefix(['-', '+']).unwrap_or(raw);
    if matches!(
        unsigned.to_ascii_lowercase().as_str(),
        ".inf" | ".nan" | "inf" | "nan"
    ) {
        return Err(non_json("non-finite float"));
    }
    // Integers: decimal, hex (0x), octal (0o).
    if let Some(int) = try_integer(raw) {
        return match int {
            Ok(number) => Ok(NodeValue::Number(number)),
            Err(()) => Err(non_json("integer out of range")),
        };
    }
    // Floats: only over an unambiguous numeric character set.
    let numeric_charset = raw
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'));
    if numeric_charset
        && raw.chars().any(|c| c.is_ascii_digit())
        && let Ok(float) = raw.parse::<f64>()
    {
        return serde_json::Number::from_f64(float)
            .map(NodeValue::Number)
            .ok_or_else(|| non_json("non-finite float"));
    }
    Ok(NodeValue::String(raw.to_owned()))
}

/// Attempts YAML 1.2 core-schema integer resolution. `None` = not an
/// integer shape; `Some(Err(()))` = integer shape but out of range.
fn try_integer(raw: &str) -> Option<Result<serde_json::Number, ()>> {
    let (negative, unsigned) = match raw.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, raw.strip_prefix('+').unwrap_or(raw)),
    };
    let parsed: Result<u64, ()> = if let Some(hex) = unsigned.strip_prefix("0x") {
        if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u64::from_str_radix(hex, 16).map_err(|_| ())
    } else if let Some(oct) = unsigned.strip_prefix("0o") {
        if oct.is_empty() || !oct.chars().all(|c| ('0'..='7').contains(&c)) {
            return None;
        }
        u64::from_str_radix(oct, 8).map_err(|_| ())
    } else {
        if unsigned.is_empty() || !unsigned.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        unsigned.parse::<u64>().map_err(|_| ())
    };
    Some(parsed.and_then(|magnitude| {
        if negative {
            i64::try_from(-i128::from(magnitude))
                .map(serde_json::Number::from)
                .map_err(|_| ())
        } else {
            Ok(serde_json::Number::from(magnitude))
        }
    }))
}

/// Parses one YAML document into a spanned root mapping, fail-closed on
/// every limit and every step outside the JSON data model.
pub(crate) fn parse(source: &str) -> Result<Node, Vec<CompileDiagnostic>> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(vec![CompileDiagnostic::error(
            codes::PARSE_BYTE_LIMIT,
            format!(
                "source is {} bytes, exceeding the limit of {MAX_SOURCE_BYTES}",
                source.len()
            ),
            None,
        )]);
    }
    let mut builder = Builder::new();
    for item in &mut Parser::new_from_str(source) {
        let (event, span) = item.map_err(|err| {
            let marker = DiagnosticSpan {
                line: coord(err.marker().line()),
                col: coord(err.marker().col() + 1),
                end_line: coord(err.marker().line()),
                end_col: coord(err.marker().col() + 1),
            };
            vec![CompileDiagnostic::error(
                codes::PARSE_SYNTAX,
                format!("YAML syntax error: {}", err.info()),
                Some(marker),
            )]
        })?;
        builder.on_event(event, span).map_err(|diag| vec![diag])?;
    }
    let Some(root) = builder.root else {
        return Err(vec![CompileDiagnostic::error(
            codes::PARSE_DOCUMENT_SHAPE,
            "the stream must contain exactly one YAML document",
            None,
        )]);
    };
    if !matches!(root.value, NodeValue::Mapping(_)) {
        return Err(vec![CompileDiagnostic::error(
            codes::PARSE_DOCUMENT_SHAPE,
            format!(
                "the document root must be a mapping, got {}",
                root.type_name()
            ),
            Some(root.span),
        )]);
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_of(source: &str) -> String {
        parse(source).expect_err("expected a parse rejection")[0]
            .code
            .clone()
    }

    #[test]
    fn parses_scalars_into_the_json_data_model() {
        let root = parse(
            "a: 1\nb: -2\nc: 3.5\nd: true\ne: null\nf: text\ng: '007'\nh: 0x10\ni: [1, 2]\nj: { k: v }\n",
        )
        .expect("valid document");
        let json = root.to_json();
        assert_eq!(
            json,
            serde_json::json!({
                "a": 1, "b": -2, "c": 3.5, "d": true, "e": null,
                "f": "text", "g": "007", "h": 16, "i": [1, 2], "j": { "k": "v" }
            })
        );
    }

    #[test]
    fn spans_are_one_based() {
        let root = parse("alpha: 1\nbeta: x\n").expect("valid document");
        let NodeValue::Mapping(entries) = &root.value else {
            panic!("root must be a mapping");
        };
        assert_eq!(entries[0].key_span.line, 1);
        assert_eq!(entries[0].key_span.col, 1);
        assert_eq!(entries[1].key_span.line, 2);
        assert_eq!(entries[1].value.span.col, 7);
    }

    #[test]
    fn rejections_fail_closed() {
        assert_eq!(code_of("a: !custom 1\n"), codes::PARSE_TAG_REJECTED);
        assert_eq!(code_of("a: !!str 1\n"), codes::PARSE_TAG_REJECTED);
        assert_eq!(
            code_of("base: &b { x: 1 }\nchild:\n  <<: *b\n"),
            codes::PARSE_MERGE_KEY
        );
        assert_eq!(code_of("a: 1\na: 2\n"), codes::PARSE_DUPLICATE_KEY);
        assert_eq!(code_of("1: x\n"), codes::PARSE_NON_STRING_KEY);
        assert_eq!(code_of("a: .inf\n"), codes::PARSE_NON_JSON_SCALAR);
        assert_eq!(code_of("a: .nan\n"), codes::PARSE_NON_JSON_SCALAR);
        assert_eq!(
            code_of("a: 99999999999999999999999999\n"),
            codes::PARSE_NON_JSON_SCALAR
        );
        assert_eq!(
            code_of("---\na: 1\n---\nb: 2\n"),
            codes::PARSE_DOCUMENT_SHAPE
        );
        assert_eq!(code_of("- 1\n- 2\n"), codes::PARSE_DOCUMENT_SHAPE);
        assert_eq!(code_of(""), codes::PARSE_DOCUMENT_SHAPE);
        assert_eq!(code_of("a: [1, 2\n"), codes::PARSE_SYNTAX);
    }

    #[test]
    fn depth_limit_fails_closed() {
        let mut source = String::from("a:\n");
        for depth in 1..=(MAX_DEPTH + 1) {
            source.push_str(&"  ".repeat(depth));
            source.push_str("a:\n");
        }
        assert_eq!(code_of(&source), codes::PARSE_DEPTH_LIMIT);
    }

    #[test]
    fn alias_expansion_counts_against_the_node_budget() {
        // Billion-laughs shape: each level aliases the previous one twice.
        let mut source = String::from("l0: &a0 [x, x, x, x, x, x, x, x]\n");
        for level in 1..=8 {
            source.push_str(&format!(
                "l{level}: &a{level} [*a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}]\n",
                prev = level - 1
            ));
        }
        assert_eq!(code_of(&source), codes::PARSE_NODE_LIMIT);
    }

    #[test]
    fn aliases_are_value_copies_with_definition_spans() {
        let root = parse("a: &x hello\nb: *x\n").expect("valid document");
        let NodeValue::Mapping(entries) = &root.value else {
            panic!("root must be a mapping");
        };
        assert_eq!(entries[1].value.to_json(), serde_json::json!("hello"));
        // The copy points at the anchor definition site (line 1).
        assert_eq!(entries[1].value.span.line, 1);
    }

    #[test]
    fn byte_limit_fails_closed() {
        let big = format!("a: {}\n", "x".repeat(MAX_SOURCE_BYTES));
        assert_eq!(code_of(&big), codes::PARSE_BYTE_LIMIT);
    }
}
