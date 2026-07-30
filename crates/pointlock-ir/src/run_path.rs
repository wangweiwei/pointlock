//! `failedStepPath`: the structured RunPath representation (spine §9) and
//! its canonical human-readable string form (07 §2.1).
//!
//! The frame array is authoritative — RunLog, `CheckpointView`,
//! `StepRecord.runPath` and `pointlock locate` all use the JSON
//! `PathFrame[]` form. The canonical string (e.g.
//! `checkout@a1f3c9d2/purchase/call→login@9c2e77b0/enterPassword#2!tokenVisible`)
//! is its deterministic rendering, implemented here by [`render_run_path`]
//! and parsed back by [`parse_run_path`]. Macro expansions never appear in
//! a path — the sourceMap translates back to YAML lines and macro call
//! chains. Not part of the FlowIR wire schema; consumed by
//! `pointlock-store` / `pointlock locate`.
//!
//! ## Rendering rules (07 §2.1, verbatim)
//!
//! | frame | rendering | notes |
//! |---|---|---|
//! | `flow` | `<flowId>@<hash8>` | leading segment; flows always carry the first 8 hex of their irHash |
//! | `step` | `/<stepId>` | |
//! | `call` | `/<stepId>/call→<calleeFlowId>@<hash8>` | one frame, two visual segments |
//! | `iteration` | `[<index>]` or `[<index>:<key>]` | no `/`; appends to the foreach step segment |
//! | `hook` | `/hook:<hookName>:<trigger>` | handler audit frame (spine R10) |
//! | `attempt` | `#<n>` | appends to the step segment, n ≥ 1 |
//! | `phase` | `:<preflight\|act\|observe\|assert>` | appends after the attempt |
//! | `assertion` | `!<assertId>` | path tail |
//!
//! An attempt (and a phase) following a `call` frame attaches to the call
//! step's own segment, before the arrow segment — 07's example
//! `purchase#2/call→login@9c2e77b0` (caller retried the callee: call step
//! attempt #2). A `call→` segment directly after a `hook` segment (07's
//! `pay/hook:onFail:1/call→repairCart@55d0ab12`) is a handler-launched
//! subflow and carries no call step id; [`ParsedPathFrame::Call::step_id`]
//! is `None` there.
//!
//! ## Grammar caveats (documented)
//!
//! - Rendering truncates hashes to their first 8 hex chars, so parsing
//!   yields [`ParsedPathFrame`] (hash *prefixes*), not [`PathFrame`].
//!   Round-tripping is exact in both directions modulo that truncation:
//!   `render(frames) == render_parsed(parse(render(frames)))`.
//! - A phase suffix is only recognized after an attempt (`#n:phase`, per
//!   07 §2.1 "appends after the attempt"); a bare `:` inside a segment
//!   parses as part of a compiler-synthesized step id (which legally
//!   contains `:`). A synthesized id whose last segment spells a phase
//!   keyword is therefore renderable but not distinguishable — the
//!   structured array remains authoritative for such paths.
//! - Segments starting with `hook:` always parse as hook frames (a real
//!   hook trigger count is numeric, which no step-id segment can be).
//! - Iteration keys must not contain `/`, `]` or be empty; v0.1 locates by
//!   index and reserves `key` for keyed foreach.

use std::fmt::Write as _;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::primitives::{AssertId, FlowId, Hash, StepId};
use crate::vocab::{HandlerHook, Phase};

/// A structured run path: an ordered stack of frames from the run's root
/// flow down to the addressed site.
pub type RunPath = Vec<PathFrame>;

/// One frame of a [`RunPath`] (8 kinds, closed; spine §9), internally tagged
/// with `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub enum PathFrame {
    /// A flow root. Paths always carry the flow's `irHash` (hard rule 1).
    #[serde(rename_all = "camelCase")]
    Flow {
        /// The flow's id.
        flow_id: FlowId,
        /// The flow's content hash.
        ir_hash: Hash,
    },
    /// A step within the current flow body.
    #[serde(rename_all = "camelCase")]
    Step {
        /// The step's id.
        step_id: StepId,
    },
    /// A subflow call frame.
    #[serde(rename_all = "camelCase")]
    Call {
        /// The call step's id. Absent exactly when this call frame is a
        /// handler-repair launched flow (directly under a `hook` frame,
        /// no host call step — spine §9).
        #[serde(skip_serializing_if = "Option::is_none")]
        step_id: Option<StepId>,
        /// The callee's flow id.
        callee_flow_id: FlowId,
        /// The callee's content hash.
        callee_ir_hash: Hash,
    },
    /// A foreach iteration.
    Iteration {
        /// Zero-based iteration index.
        index: u64,
        /// Optional stable item key.
        #[serde(skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    /// A handler execution (audit trace of spine R10).
    Hook {
        /// The hook that fired.
        hook: HandlerHook,
        /// One-based trigger count for this hook.
        trigger: u64,
    },
    /// An act-chain attempt.
    Attempt {
        /// One-based attempt number.
        n: u64,
    },
    /// A step pipeline phase.
    Phase {
        /// The phase.
        phase: Phase,
    },
    /// An assertion within the assert phase.
    #[serde(rename_all = "camelCase")]
    Assertion {
        /// The assertion's id.
        assert_id: AssertId,
    },
}

/// A [`PathFrame`] as recoverable from the canonical string: identical in
/// shape except that flow hashes appear as their rendered 8-hex prefixes
/// (the full digest is not present in the string), and a call frame under a
/// hook segment carries no step id (handler-launched subflow, 07 §2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedPathFrame {
    /// A flow root.
    Flow {
        /// The flow's id.
        flow_id: FlowId,
        /// The first 8 hex chars of the flow's irHash.
        ir_hash_prefix: String,
    },
    /// A step within the current flow body.
    Step {
        /// The step's id.
        step_id: StepId,
    },
    /// A subflow call frame.
    Call {
        /// The call step's id; `None` for a handler-launched subflow
        /// (a `call→` segment directly under a `hook:` segment).
        step_id: Option<StepId>,
        /// The callee's flow id.
        callee_flow_id: FlowId,
        /// The first 8 hex chars of the callee's irHash.
        callee_ir_hash_prefix: String,
    },
    /// A foreach iteration.
    Iteration {
        /// Zero-based iteration index.
        index: u64,
        /// Optional stable item key.
        key: Option<String>,
    },
    /// A handler execution.
    Hook {
        /// The hook that fired.
        hook: HandlerHook,
        /// One-based trigger count for this hook.
        trigger: u64,
    },
    /// An act-chain attempt.
    Attempt {
        /// One-based attempt number.
        n: u64,
    },
    /// A step pipeline phase.
    Phase {
        /// The phase.
        phase: Phase,
    },
    /// An assertion within the assert phase.
    Assertion {
        /// The assertion's id.
        assert_id: AssertId,
    },
}

impl From<&PathFrame> for ParsedPathFrame {
    fn from(frame: &PathFrame) -> Self {
        match frame {
            PathFrame::Flow { flow_id, ir_hash } => ParsedPathFrame::Flow {
                flow_id: flow_id.clone(),
                ir_hash_prefix: ir_hash.hex_prefix8().to_owned(),
            },
            PathFrame::Step { step_id } => ParsedPathFrame::Step {
                step_id: step_id.clone(),
            },
            PathFrame::Call {
                step_id,
                callee_flow_id,
                callee_ir_hash,
            } => ParsedPathFrame::Call {
                step_id: step_id.clone(),
                callee_flow_id: callee_flow_id.clone(),
                callee_ir_hash_prefix: callee_ir_hash.hex_prefix8().to_owned(),
            },
            PathFrame::Iteration { index, key } => ParsedPathFrame::Iteration {
                index: *index,
                key: key.clone(),
            },
            PathFrame::Hook { hook, trigger } => ParsedPathFrame::Hook {
                hook: *hook,
                trigger: *trigger,
            },
            PathFrame::Attempt { n } => ParsedPathFrame::Attempt { n: *n },
            PathFrame::Phase { phase } => ParsedPathFrame::Phase { phase: *phase },
            PathFrame::Assertion { assert_id } => ParsedPathFrame::Assertion {
                assert_id: assert_id.clone(),
            },
        }
    }
}

/// Renders a structured [`RunPath`] to its canonical human-readable string
/// (07 §2.1; rules in the module docs). Deterministic; hashes appear as
/// their first 8 hex chars.
pub fn render_run_path(path: &[PathFrame]) -> String {
    let parsed: Vec<ParsedPathFrame> = path.iter().map(ParsedPathFrame::from).collect();
    render_parsed_run_path(&parsed)
}

/// Renders a parsed run path back to the canonical string. For any `s`
/// accepted by [`parse_run_path`], `render_parsed_run_path(&parse_run_path(s)?) == s`.
pub fn render_parsed_run_path(path: &[ParsedPathFrame]) -> String {
    let mut out = String::new();
    // A call frame renders as two visual segments; the arrow segment is
    // held back so that a following attempt/phase can attach to the call
    // step's own segment (07: `purchase#2/call→login@9c2e77b0`).
    let mut pending_call: Option<String> = None;
    for frame in path {
        if !matches!(
            frame,
            ParsedPathFrame::Attempt { .. } | ParsedPathFrame::Phase { .. }
        ) {
            flush_pending_call(&mut out, &mut pending_call);
        }
        match frame {
            ParsedPathFrame::Flow {
                flow_id,
                ir_hash_prefix,
            } => {
                if !out.is_empty() {
                    out.push('/');
                }
                let _ = write!(out, "{flow_id}@{ir_hash_prefix}");
            }
            ParsedPathFrame::Step { step_id } => {
                let _ = write!(out, "/{step_id}");
            }
            ParsedPathFrame::Call {
                step_id,
                callee_flow_id,
                callee_ir_hash_prefix,
            } => {
                if let Some(id) = step_id {
                    let _ = write!(out, "/{id}");
                }
                pending_call = Some(format!("call→{callee_flow_id}@{callee_ir_hash_prefix}"));
            }
            ParsedPathFrame::Iteration { index, key } => {
                match key {
                    Some(key) => {
                        let _ = write!(out, "[{index}:{key}]");
                    }
                    None => {
                        let _ = write!(out, "[{index}]");
                    }
                };
            }
            ParsedPathFrame::Hook { hook, trigger } => {
                let _ = write!(out, "/hook:{}:{trigger}", hook_wire_name(*hook));
            }
            ParsedPathFrame::Attempt { n } => {
                let _ = write!(out, "#{n}");
            }
            ParsedPathFrame::Phase { phase } => {
                let _ = write!(out, ":{}", phase_wire_name(*phase));
            }
            ParsedPathFrame::Assertion { assert_id } => {
                let _ = write!(out, "!{assert_id}");
            }
        }
    }
    flush_pending_call(&mut out, &mut pending_call);
    out
}

fn flush_pending_call(out: &mut String, pending_call: &mut Option<String>) {
    if let Some(segment) = pending_call.take() {
        out.push('/');
        out.push_str(&segment);
    }
}

/// Error produced by [`parse_run_path`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid RunPath string at byte {offset}: {message}")]
pub struct RunPathParseError {
    /// Byte offset (of the offending segment's start) in the input.
    pub offset: usize,
    /// Human-readable reason.
    pub message: String,
}

fn parse_error(offset: usize, message: impl Into<String>) -> RunPathParseError {
    RunPathParseError {
        offset,
        message: message.into(),
    }
}

/// Parses a canonical RunPath string back to its frames. Inverse of
/// [`render_run_path`] up to hash truncation (see [`ParsedPathFrame`] and
/// the grammar caveats in the module docs).
pub fn parse_run_path(input: &str) -> Result<Vec<ParsedPathFrame>, RunPathParseError> {
    if input.is_empty() {
        return Err(parse_error(0, "empty RunPath string"));
    }
    let mut frames: Vec<ParsedPathFrame> = Vec::new();
    let mut offset = 0usize;
    for (position, segment) in input.split('/').enumerate() {
        if position == 0 {
            let (name, prefix) = parse_name_at_hash(segment, offset, "flow root")?;
            let flow_id = FlowId::new(name)
                .map_err(|e| parse_error(offset, format!("invalid flow id: {e}")))?;
            frames.push(ParsedPathFrame::Flow {
                flow_id,
                ir_hash_prefix: prefix,
            });
        } else if let Some(rest) = segment.strip_prefix("call→") {
            parse_call_segment(rest, offset, &mut frames)?;
        } else if segment.starts_with("hook:") {
            frames.push(parse_hook_segment(segment, offset)?);
        } else {
            parse_step_segment(segment, offset, &mut frames)?;
        }
        offset += segment.len() + 1;
    }
    Ok(frames)
}

/// `<name>@<8 hex>` — the flow-root and call-arrow segment shape.
fn parse_name_at_hash<'a>(
    segment: &'a str,
    offset: usize,
    what: &str,
) -> Result<(&'a str, String), RunPathParseError> {
    let Some((name, prefix)) = segment.split_once('@') else {
        return Err(parse_error(
            offset,
            format!("a {what} segment must be '<flowId>@<8-hex irHash prefix>', got {segment:?}"),
        ));
    };
    if prefix.len() != 8
        || !prefix
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return Err(parse_error(
            offset,
            format!("hash prefix must be exactly 8 lowercase hex chars, got {prefix:?}"),
        ));
    }
    Ok((name, prefix.to_owned()))
}

/// `call→<calleeFlowId>@<hash8>`: converts the preceding step segment into
/// a call frame (walking back over attached attempt/phase frames), or —
/// directly after a hook segment — pushes a step-id-less call frame
/// (handler-launched subflow).
fn parse_call_segment(
    rest: &str,
    offset: usize,
    frames: &mut Vec<ParsedPathFrame>,
) -> Result<(), RunPathParseError> {
    let (name, prefix) = parse_name_at_hash(rest, offset, "call target")?;
    let callee_flow_id = FlowId::new(name)
        .map_err(|e| parse_error(offset, format!("invalid callee flow id: {e}")))?;

    let mut anchor = frames.len();
    while anchor > 0
        && matches!(
            frames[anchor - 1],
            ParsedPathFrame::Attempt { .. } | ParsedPathFrame::Phase { .. }
        )
    {
        anchor -= 1;
    }
    if anchor == 0 {
        return Err(parse_error(offset, "a call→ segment cannot open a path"));
    }
    match frames[anchor - 1].clone() {
        ParsedPathFrame::Step { step_id } => {
            frames[anchor - 1] = ParsedPathFrame::Call {
                step_id: Some(step_id),
                callee_flow_id,
                callee_ir_hash_prefix: prefix,
            };
            Ok(())
        }
        ParsedPathFrame::Hook { .. } => {
            frames.insert(
                anchor,
                ParsedPathFrame::Call {
                    step_id: None,
                    callee_flow_id,
                    callee_ir_hash_prefix: prefix,
                },
            );
            Ok(())
        }
        _ => Err(parse_error(
            offset,
            "a call→ segment must follow a step or hook segment",
        )),
    }
}

/// `hook:<hookName>:<trigger>`.
fn parse_hook_segment(segment: &str, offset: usize) -> Result<ParsedPathFrame, RunPathParseError> {
    let rest = segment
        .strip_prefix("hook:")
        .expect("caller checked the prefix");
    let Some((name, trigger)) = rest.split_once(':') else {
        return Err(parse_error(
            offset,
            format!("a hook segment must be 'hook:<hookName>:<trigger>', got {segment:?}"),
        ));
    };
    let Some(hook) = parse_hook_name(name) else {
        return Err(parse_error(offset, format!("unknown hook name {name:?}")));
    };
    let trigger: u64 = trigger.parse().map_err(|_| {
        parse_error(
            offset,
            format!("hook trigger must be a number, got {trigger:?}"),
        )
    })?;
    Ok(ParsedPathFrame::Hook { hook, trigger })
}

/// A step segment: `<stepId>` followed by attached suffixes
/// (`[i]`/`[i:key]`, `#n`, `:phase` after an attempt, `!assertId`).
fn parse_step_segment(
    segment: &str,
    offset: usize,
    frames: &mut Vec<ParsedPathFrame>,
) -> Result<(), RunPathParseError> {
    let is_id_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ':';
    let id_end = segment
        .char_indices()
        .find(|(_, c)| !is_id_char(*c))
        .map_or(segment.len(), |(i, _)| i);
    let id = &segment[..id_end];
    if id.is_empty() {
        return Err(parse_error(
            offset,
            format!("expected a step id, got {segment:?}"),
        ));
    }
    let step_id =
        StepId::new(id).map_err(|e| parse_error(offset, format!("invalid step id: {e}")))?;
    frames.push(ParsedPathFrame::Step { step_id });

    let mut rest = &segment[id_end..];
    let mut after_attempt = false;
    while let Some(next) = rest.chars().next() {
        match next {
            '#' => {
                let (digits, tail) = take_ascii_digits(&rest[1..]);
                if digits.is_empty() {
                    return Err(parse_error(
                        offset,
                        "'#' must be followed by an attempt number",
                    ));
                }
                let n: u64 = digits.parse().map_err(|_| {
                    parse_error(offset, format!("attempt number out of range: {digits:?}"))
                })?;
                frames.push(ParsedPathFrame::Attempt { n });
                rest = tail;
                after_attempt = true;
            }
            ':' => {
                // Phases attach after attempts (07 §2.1); a ':' anywhere
                // else belongs to a synthesized step id and was consumed by
                // the id token above.
                if !after_attempt {
                    return Err(parse_error(
                        offset,
                        "a ':<phase>' suffix is only valid directly after an attempt '#n'",
                    ));
                }
                let keyword_end = rest[1..]
                    .char_indices()
                    .find(|(_, c)| !c.is_ascii_lowercase())
                    .map_or(rest.len(), |(i, _)| i + 1);
                let keyword = &rest[1..keyword_end];
                let Some(phase) = parse_phase_name(keyword) else {
                    return Err(parse_error(offset, format!("unknown phase {keyword:?}")));
                };
                frames.push(ParsedPathFrame::Phase { phase });
                rest = &rest[keyword_end..];
                after_attempt = false;
            }
            '[' => {
                let Some(close) = rest.find(']') else {
                    return Err(parse_error(offset, "unterminated '[' iteration suffix"));
                };
                let body = &rest[1..close];
                let (index_str, key) = match body.split_once(':') {
                    Some((index, key)) => (index, Some(key)),
                    None => (body, None),
                };
                let index: u64 = index_str.parse().map_err(|_| {
                    parse_error(
                        offset,
                        format!("iteration index must be a number, got {index_str:?}"),
                    )
                })?;
                if key == Some("") {
                    return Err(parse_error(offset, "iteration key must not be empty"));
                }
                frames.push(ParsedPathFrame::Iteration {
                    index,
                    key: key.map(str::to_owned),
                });
                rest = &rest[close + 1..];
                after_attempt = false;
            }
            '!' => {
                let assert_id = AssertId::new(&rest[1..])
                    .map_err(|e| parse_error(offset, format!("invalid assertion id: {e}")))?;
                frames.push(ParsedPathFrame::Assertion { assert_id });
                rest = "";
            }
            other => {
                return Err(parse_error(
                    offset,
                    format!("unexpected character {other:?} in step segment {segment:?}"),
                ));
            }
        }
    }
    Ok(())
}

fn take_ascii_digits(s: &str) -> (&str, &str) {
    let end = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(s.len(), |(i, _)| i);
    s.split_at(end)
}

fn hook_wire_name(hook: HandlerHook) -> &'static str {
    match hook {
        HandlerHook::OnFail => "onFail",
        HandlerHook::OnUnknown => "onUnknown",
        HandlerHook::OnError => "onError",
        HandlerHook::OnResumeDrift => "onResumeDrift",
    }
}

fn parse_hook_name(name: &str) -> Option<HandlerHook> {
    match name {
        "onFail" => Some(HandlerHook::OnFail),
        "onUnknown" => Some(HandlerHook::OnUnknown),
        "onError" => Some(HandlerHook::OnError),
        "onResumeDrift" => Some(HandlerHook::OnResumeDrift),
        _ => None,
    }
}

fn phase_wire_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Preflight => "preflight",
        Phase::Act => "act",
        Phase::Observe => "observe",
        Phase::Assert => "assert",
    }
}

fn parse_phase_name(name: &str) -> Option<Phase> {
    match name {
        "preflight" => Some(Phase::Preflight),
        "act" => Some(Phase::Act),
        "observe" => Some(Phase::Observe),
        "assert" => Some(Phase::Assert),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_with_prefix(prefix: &str) -> Hash {
        Hash::new(format!("sha256:{prefix}{}", "0".repeat(64 - prefix.len())))
            .expect("valid hash literal")
    }

    fn flow(id: &str, prefix: &str) -> PathFrame {
        PathFrame::Flow {
            flow_id: FlowId::new(id).expect("valid flow id"),
            ir_hash: hash_with_prefix(prefix),
        }
    }

    fn step(id: &str) -> PathFrame {
        PathFrame::Step {
            step_id: StepId::new(id).expect("valid step id"),
        }
    }

    fn call(id: &str, callee: &str, prefix: &str) -> PathFrame {
        PathFrame::Call {
            step_id: Some(StepId::new(id).expect("valid step id")),
            callee_flow_id: FlowId::new(callee).expect("valid flow id"),
            callee_ir_hash: hash_with_prefix(prefix),
        }
    }

    fn attempt(n: u64) -> PathFrame {
        PathFrame::Attempt { n }
    }

    fn phase(phase: Phase) -> PathFrame {
        PathFrame::Phase { phase }
    }

    fn assertion(id: &str) -> PathFrame {
        PathFrame::Assertion {
            assert_id: AssertId::new(id).expect("valid assert id"),
        }
    }

    /// render → parse → render must reproduce the string, and the parsed
    /// frames must equal the truncated originals.
    fn assert_round_trip(frames: &[PathFrame], expected: &str) {
        let rendered = render_run_path(frames);
        assert_eq!(rendered, expected);
        let parsed = parse_run_path(&rendered).expect("canonical rendering must parse");
        let truncated: Vec<ParsedPathFrame> = frames.iter().map(ParsedPathFrame::from).collect();
        assert_eq!(parsed, truncated);
        assert_eq!(render_parsed_run_path(&parsed), rendered);
    }

    #[test]
    fn renders_step_attempt_phase() {
        // 07 §2.1 example 1.
        assert_round_trip(
            &[
                flow("checkout", "a1f3c9d2"),
                step("loadCart"),
                attempt(1),
                phase(Phase::Act),
            ],
            "checkout@a1f3c9d2/loadCart#1:act",
        );
    }

    #[test]
    fn renders_iteration_and_assertion() {
        // 07 §2.1 example 2.
        assert_round_trip(
            &[
                flow("checkout", "a1f3c9d2"),
                step("eachItem"),
                PathFrame::Iteration {
                    index: 2,
                    key: None,
                },
                step("addToCart"),
                attempt(3),
                assertion("itemInCart"),
            ],
            "checkout@a1f3c9d2/eachItem[2]/addToCart#3!itemInCart",
        );
    }

    #[test]
    fn renders_call_crossing_into_callee() {
        // 07 §2.1 example 3 (assertId adapted to the AssertId grammar,
        // which admits no '.').
        assert_round_trip(
            &[
                flow("checkout", "a1f3c9d2"),
                call("purchase", "login", "9c2e77b0"),
                step("enterPassword"),
                attempt(2),
                assertion("tokenVisible"),
            ],
            "checkout@a1f3c9d2/purchase/call→login@9c2e77b0/enterPassword#2!tokenVisible",
        );
    }

    #[test]
    fn call_attempt_attaches_to_the_call_step_segment() {
        // 07 §2.1 example 4: caller re-invoked the callee (call step
        // attempt #2); the attempt renders before the arrow segment.
        assert_round_trip(
            &[
                flow("checkout", "a1f3c9d2"),
                call("purchase", "login", "9c2e77b0"),
                attempt(2),
                step("focusAccount"),
                attempt(1),
                phase(Phase::Preflight),
            ],
            "checkout@a1f3c9d2/purchase#2/call→login@9c2e77b0/focusAccount#1:preflight",
        );
    }

    #[test]
    fn path_may_end_at_the_call_frame() {
        // A call frame always renders both visual segments.
        assert_round_trip(
            &[
                flow("checkout", "a1f3c9d2"),
                call("purchase", "login", "9c2e77b0"),
            ],
            "checkout@a1f3c9d2/purchase/call→login@9c2e77b0",
        );
    }

    #[test]
    fn keyed_iteration_round_trips() {
        assert_round_trip(
            &[
                flow("checkout", "a1f3c9d2"),
                step("eachItem"),
                PathFrame::Iteration {
                    index: 3,
                    key: Some("sku-42".to_owned()),
                },
                step("addToCart"),
            ],
            "checkout@a1f3c9d2/eachItem[3:sku-42]/addToCart",
        );
    }

    #[test]
    fn synthesized_step_ids_keep_their_colons() {
        assert_round_trip(
            &[
                flow("checkout", "a1f3c9d2"),
                step("pay"),
                PathFrame::Hook {
                    hook: HandlerHook::OnUnknown,
                    trigger: 1,
                },
                step("pay:onUnknown:escalate"),
            ],
            "checkout@a1f3c9d2/pay/hook:onUnknown:1/pay:onUnknown:escalate",
        );
    }

    #[test]
    fn hook_launched_subflow_parses_with_step_less_call_frame() {
        // 07 §2.1 example 5: handler repair subflow — the call arrow
        // follows the hook segment directly, with no call step id.
        let input = "checkout@a1f3c9d2/pay/hook:onFail:1/call→repairCart@55d0ab12/clearStale#1:act";
        let parsed = parse_run_path(input).expect("doc example must parse");
        assert_eq!(
            parsed,
            vec![
                ParsedPathFrame::Flow {
                    flow_id: FlowId::new("checkout").unwrap(),
                    ir_hash_prefix: "a1f3c9d2".to_owned(),
                },
                ParsedPathFrame::Step {
                    step_id: StepId::new("pay").unwrap()
                },
                ParsedPathFrame::Hook {
                    hook: HandlerHook::OnFail,
                    trigger: 1
                },
                ParsedPathFrame::Call {
                    step_id: None,
                    callee_flow_id: FlowId::new("repairCart").unwrap(),
                    callee_ir_hash_prefix: "55d0ab12".to_owned(),
                },
                ParsedPathFrame::Step {
                    step_id: StepId::new("clearStale").unwrap()
                },
                ParsedPathFrame::Attempt { n: 1 },
                ParsedPathFrame::Phase { phase: Phase::Act },
            ],
        );
        assert_eq!(render_parsed_run_path(&parsed), input);
    }

    #[test]
    fn rejects_malformed_paths() {
        for bad in [
            "",                                  // empty
            "checkout",                          // flow root without @hash8
            "checkout@a1f3",                     // hash prefix too short
            "checkout@A1F3C9D2",                 // hash prefix not lowercase hex
            "checkout@a1f3c9d2/x#",              // '#' without digits
            "checkout@a1f3c9d2/x#1:sleep",       // unknown phase
            "checkout@a1f3c9d2/eachItem[2]:act", // phase not after an attempt
            "checkout@a1f3c9d2/eachItem[a]",     // non-numeric iteration index
            "checkout@a1f3c9d2/eachItem[2",      // unterminated iteration
            "checkout@a1f3c9d2/hook:onFoo:1",    // unknown hook name
            "checkout@a1f3c9d2/hook:onFail",     // hook without trigger
            "checkout@a1f3c9d2/call→x@11223344", // call arrow directly after flow root
            "checkout@a1f3c9d2/9bad",            // invalid step id
            "checkout@a1f3c9d2//x",              // empty segment
        ] {
            assert!(parse_run_path(bad).is_err(), "expected reject: {bad:?}");
        }
    }
}
