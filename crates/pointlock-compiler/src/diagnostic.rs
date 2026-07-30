//! Compile diagnostics (03 §4.4, M0 shape).
//!
//! `code` is `"RF" + stage digit + serial` — RF1xxx = parse, RF2xxx =
//! normalize, RF3xxx = check, RF4xxx = bind; seal produces no diagnostics
//! for a legal tree (03 §4.2), so the only seal-attributed code is the
//! factory self-check [`codes::INTERNAL_FACTORY_CHECK`], which signals a
//! compiler bug, never an authoring error.
//!
//! The M0 diagnostic is deliberately narrower than the 03 §4.4 target type
//! (`stage`/`irPath`/`macroTrace`/`candidates`/`hint` come with the full
//! pipeline); the `span` is optional here for the same reason — parse-limit
//! failures may predate any locatable node.

use serde::{Deserialize, Serialize};

/// A 1-based source span in the YAML input (start inclusive, end exclusive
/// on the column axis, matching the parser's marker convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticSpan {
    /// Start line (1-based).
    pub line: u32,
    /// Start column (1-based).
    pub col: u32,
    /// End line (1-based).
    pub end_line: u32,
    /// End column (1-based).
    pub end_col: u32,
}

impl DiagnosticSpan {
    /// Converts to the IR-side source span (same 1-based convention).
    pub(crate) fn to_ir(self) -> pointlock_ir::SourceSpan {
        pointlock_ir::SourceSpan {
            start_line: self.line,
            start_col: self.col,
            end_line: self.end_line,
            end_col: self.end_col,
        }
    }
}

/// Diagnostic severity. M0 emits only `error`; the vocabulary is final
/// (03 §4.3 F6 introduces the first `warning` with the full pipeline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// The flow is rejected; no IR is produced.
    Error,
    /// The flow still compiles; the author should look anyway.
    Warning,
}

/// One compile diagnostic (03 §4.4 shape — see the module docs).
///
/// Divergences from the spec shape, registered: `span` stays optional
/// (flow-level findings genuinely have no anchor; the spec's required span
/// would force fabricating one), and `macroTrace` is absent — the macro
/// surface is not in the compile subset yet, so there is no origin trace
/// to carry; the field lands with it rather than shipping always-empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompileDiagnostic {
    /// `"RF1xxx"`–`"RF4xxx"`, segmented by stage (03 §4.4).
    pub code: String,
    /// The pipeline stage the code's prefix names (03 §4.4:
    /// `parse | normalize | check | bind`). Derived from the code at
    /// construction — one source of truth, serialized for readers that
    /// should not re-parse code prefixes.
    #[serde(default)]
    pub stage: String,
    /// Severity of the finding.
    pub severity: Severity,
    /// One sentence stating the violated rule.
    pub message: String,
    /// The YAML source file, as the caller named it (03 §4.4 span.file).
    /// Stamped once by [`crate::compile`]; empty when compiling from
    /// memory with no name given.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
    /// YAML source location, when one is attributable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<DiagnosticSpan>,
    /// Nearest available alternatives (edit distance), for the bind-phase
    /// capability misses — 03 §4.4: fed to the LLM repair loop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    /// A repair suggestion pointing at the governing rule (03 §4.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// The pipeline stage a diagnostic code's prefix names (03 §4.4).
fn stage_of(code: &str) -> &'static str {
    match code.as_bytes().get(2) {
        Some(b'1') => "parse",
        Some(b'2') => "normalize",
        Some(b'3') => "check",
        Some(b'4') => "bind",
        _ => "",
    }
}

impl CompileDiagnostic {
    /// Builds an `error`-severity diagnostic.
    pub(crate) fn error(
        code: &'static str,
        message: impl Into<String>,
        span: Option<DiagnosticSpan>,
    ) -> Self {
        CompileDiagnostic {
            code: code.to_owned(),
            stage: stage_of(code).to_owned(),
            severity: Severity::Error,
            message: message.into(),
            file: String::new(),
            span,
            candidates: Vec::new(),
            hint: None,
        }
    }

    /// Builds a `warning`-severity diagnostic (03 §4.3 F6: the flow still
    /// compiles; the finding is carried on the sealed artifact).
    pub(crate) fn warning(
        code: &'static str,
        message: impl Into<String>,
        span: Option<DiagnosticSpan>,
    ) -> Self {
        CompileDiagnostic {
            code: code.to_owned(),
            stage: stage_of(code).to_owned(),
            severity: Severity::Warning,
            message: message.into(),
            file: String::new(),
            span,
            candidates: Vec::new(),
            hint: None,
        }
    }

    /// Attaches nearest-alternative suggestions (bind capability misses).
    pub(crate) fn with_candidates(mut self, candidates: Vec<String>) -> Self {
        self.candidates = candidates;
        self
    }

    /// Attaches a repair hint.
    pub(crate) fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// Renders diagnostics in the 03 §4.4 human-readable format: severity +
/// code + stage headline, `--> file:line:col` locator, the offending
/// source line with a caret run, then the hint. `source` is the compiled
/// YAML text (the caret needs the line); diagnostics without spans render
/// the headline alone.
pub fn render_pretty(diags: &[CompileDiagnostic], source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = String::new();
    for diag in diags {
        let severity = match diag.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let stage = if diag.stage.is_empty() {
            String::new()
        } else {
            format!(" ({})", diag.stage)
        };
        out.push_str(&format!(
            "{severity} {}{stage}: {}
",
            diag.code, diag.message
        ));
        if let Some(span) = diag.span {
            let file = if diag.file.is_empty() {
                "<memory>"
            } else {
                diag.file.as_str()
            };
            out.push_str(&format!(
                "  --> {file}:{}:{}
",
                span.line, span.col
            ));
            if let Some(text) = lines.get(span.line as usize - 1) {
                let gutter = span.line.to_string();
                let pad = " ".repeat(gutter.len());
                out.push_str(&format!(
                    "{pad} |
"
                ));
                out.push_str(&format!(
                    "{gutter} | {text}
"
                ));
                let caret_start = (span.col as usize).saturating_sub(1);
                let caret_len = if span.end_line == span.line {
                    (span.end_col as usize)
                        .saturating_sub(span.col as usize)
                        .max(1)
                } else {
                    text.chars().count().saturating_sub(caret_start).max(1)
                };
                out.push_str(&format!(
                    "{pad} | {}{}
",
                    " ".repeat(caret_start),
                    "^".repeat(caret_len)
                ));
            }
        }
        if !diag.candidates.is_empty() {
            out.push_str(&format!(
                "  = 候选: {}
",
                diag.candidates.join(", ")
            ));
        }
        if let Some(hint) = &diag.hint {
            out.push_str(&format!(
                "  = 提示: {hint}
"
            ));
        }
        out.push('\n');
    }
    out
}

/// Whether a diagnostic batch contains at least one hard error (warnings
/// alone never fail a phase, 03 §4.3 F6).
pub(crate) fn has_error(diags: &[CompileDiagnostic]) -> bool {
    diags.iter().any(|diag| diag.severity == Severity::Error)
}

/// The closed M0 diagnostic code table, segmented by stage (03 §4.4).
pub mod codes {
    // ── RF1xxx: parse ───────────────────────────────────────────────────
    /// YAML syntax / scanner error.
    pub const PARSE_SYNTAX: &str = "RF1001";
    /// Source exceeds the byte limit (fail-closed).
    pub const PARSE_BYTE_LIMIT: &str = "RF1002";
    /// Nesting depth exceeds the limit (fail-closed).
    pub const PARSE_DEPTH_LIMIT: &str = "RF1003";
    /// Node count (after alias expansion) exceeds the limit (fail-closed).
    pub const PARSE_NODE_LIMIT: &str = "RF1004";
    /// YAML tag rejected: values are limited to the JSON data model (R12).
    pub const PARSE_TAG_REJECTED: &str = "RF1005";
    /// YAML merge key `<<` rejected (00 §8 stage 1).
    pub const PARSE_MERGE_KEY: &str = "RF1006";
    /// Duplicate mapping key rejected.
    pub const PARSE_DUPLICATE_KEY: &str = "RF1007";
    /// Non-string mapping key rejected (JSON data model).
    pub const PARSE_NON_STRING_KEY: &str = "RF1008";
    /// Document shape invalid (zero/multiple documents, root not a mapping).
    pub const PARSE_DOCUMENT_SHAPE: &str = "RF1009";
    /// Scalar outside the JSON data model (non-finite float, overflow).
    pub const PARSE_NON_JSON_SCALAR: &str = "RF1010";

    // ── RF2xxx: normalize ───────────────────────────────────────────────
    /// Unknown top-level key (closed vocabulary, 03 §1.1).
    pub const NORM_UNKNOWN_TOP_KEY: &str = "RF2001";
    /// Recognized top-level key outside the implemented (M1) subset.
    pub const NORM_TOP_KEY_NOT_IMPLEMENTED: &str = "RF2002";
    /// Required top-level key missing (`flow` / `provider` / `steps`).
    pub const NORM_MISSING_TOP_KEY: &str = "RF2003";
    /// `flow` value out of the FlowId grammar.
    pub const NORM_BAD_FLOW_ID: &str = "RF2004";
    /// `provider` value invalid (v0.1: only `devicerail`).
    pub const NORM_BAD_PROVIDER: &str = "RF2005";
    /// `params` shape invalid (not a mapping / bad entry / bad name).
    pub const NORM_BAD_PARAMS_SHAPE: &str = "RF2006";
    /// Unknown param subkey (closed: `schema` / `required` / `default`).
    pub const NORM_UNKNOWN_PARAM_KEY: &str = "RF2007";
    /// Param subkey value invalid (schema meta-invalid, `required` not a
    /// boolean, missing `schema`).
    pub const NORM_BAD_PARAM_VALUE: &str = "RF2008";
    /// `steps` shape invalid (not a sequence / empty / step not a mapping).
    pub const NORM_BAD_STEPS_SHAPE: &str = "RF2009";
    /// Step `id` missing or not a string.
    pub const NORM_MISSING_STEP_ID: &str = "RF2010";
    /// Step head key missing.
    pub const NORM_MISSING_HEAD_KEY: &str = "RF2011";
    /// More than one head key on a step.
    pub const NORM_MULTIPLE_HEAD_KEYS: &str = "RF2012";
    /// Recognized head key outside the implemented subset.
    ///
    /// Retired, and deliberately not reused: its last holdouts were
    /// `observe`/`screenshot`, so with the provider-synthetic actions in
    /// place (04 §9.4.3) all eight canonical verbs compile and nothing
    /// emits this any more. The number stays reserved — a diagnostic code
    /// is a stable surface, and recycling one would silently change what
    /// an old report means.
    #[allow(dead_code)]
    pub const NORM_HEAD_KEY_NOT_IMPLEMENTED: &str = "RF2013";
    /// Unknown step key (closed vocabulary, 03 §1.2).
    pub const NORM_UNKNOWN_STEP_KEY: &str = "RF2014";
    /// Recognized step common key outside the implemented (M1) subset.
    pub const NORM_STEP_KEY_NOT_IMPLEMENTED: &str = "RF2015";
    /// `invoke` shape invalid (`action` string required, `args` mapping).
    pub const NORM_BAD_INVOKE_SHAPE: &str = "RF2016";
    /// `effect` value invalid (must be `mutating` | `readonly`).
    pub const NORM_BAD_EFFECT_VALUE: &str = "RF2017";
    /// Step common key value invalid (`idempotent`/`checkpoint` boolean,
    /// `timeout_ms` positive integer).
    pub const NORM_BAD_STEP_VALUE: &str = "RF2018";
    /// `expect` shape invalid (assertion item forms: `expr` / `element` +
    /// `state`|`text` / `visual`, optional `assert_id` / `verify_via`).
    pub const NORM_BAD_EXPECT_SHAPE: &str = "RF2019";
    /// String interpolation gone wrong: a malformed embedded `${{ }}`
    /// island (unterminated/empty), or interpolation inside a composite
    /// literal member (the delivered sugar covers top-level string
    /// scalars only — they desugar to `concat(...)`, 03 §1.9).
    pub const NORM_INTERPOLATION_INVALID: &str = "RF2020";
    /// A `${{ }}` scalar nested inside a composite (map/sequence) argument
    /// value — not in the M1 subset.
    pub const NORM_NESTED_EXPR_NOT_IMPLEMENTED: &str = "RF2021";
    /// `secrets` / `protected` keywords are reserved for v0.2 (spine R6).
    pub const NORM_RESERVED_KEYWORD: &str = "RF2022";
    /// Verb step shape invalid (closed subkeys per verb: `element` required,
    /// `set_value` needs `value`, `wait_for` needs `state` ∈ the
    /// `WaitForElementCondition` set — 03 §1.3).
    pub const NORM_BAD_VERB_SHAPE: &str = "RF2023";
    /// Element selector out of the `ElementSelectorIR`-isomorphic grammar
    /// (unknown field, wrong type, empty string, no field at all, or a
    /// dynamic value where a static selector is required — 03 §1.3).
    pub const NORM_BAD_SELECTOR_SHAPE: &str = "RF2024";
    /// Explicit `effect` conflicts with the verb's inferred effect; neither
    /// side is silently trusted (03 §1.3).
    pub const NORM_EFFECT_CONFLICT: &str = "RF2025";
    /// `${{ }}` inside a static predicate field (selector / `state` /
    /// `text` / `visual` / `region`): predicate staticness, 03 §1.5 / §4.3
    /// F5. The only dynamic-comparison surface is the `expr` predicate.
    pub const NORM_PREDICATE_NOT_STATIC: &str = "RF2026";
    /// `locate_via` / `verify_via` / `coordinate` surface shape invalid
    /// (not a sequence, unknown channel token, empty chain, coordinate not
    /// a static `{ x, y }`, or fallback keys on a non-verb step — 03 §1.4).
    pub const NORM_BAD_FALLBACK_SHAPE: &str = "RF2027";
    /// `macros` definition block invalid (not a mapping, bad macro name,
    /// name shadowing the closed vocabulary, bad `params`, missing/empty
    /// `steps` — 03 §1.7, §4.3 A3/A5).
    pub const NORM_BAD_MACRO_DEF: &str = "RF2028";
    /// Macro recursion (direct or mutual) or expansion depth over the
    /// fail-closed limit (03 §1.7 rule 3).
    pub const NORM_MACRO_RECURSION: &str = "RF2029";
    /// Macro parameter surface violated: unknown/missing parameter at the
    /// call site, `${{ macro.<param> }}` outside a macro body, or a
    /// `macro.*` reference that is not one full `${{ macro.<param> }}`
    /// scalar (03 §1.7 rule 1).
    pub const NORM_BAD_MACRO_PARAM: &str = "RF2030";
    /// `steps.<id>.*` reference to a step id defined inside the same macro
    /// body — synthesized ids have no ref surface (03 §1.7 rule 2, §4.3 B5).
    pub const NORM_MACRO_CROSS_STEP_REF: &str = "RF2031";
    /// `call` step shape invalid (`call` value not a relative-path string,
    /// `inputs` not a mapping, `inputs` without `call` — 03 §1.6).
    pub const NORM_BAD_CALL_SHAPE: &str = "RF2032";
    /// A referenced subflow file cannot be read (03 §1.1: relative-path
    /// reference, resolved against the current file).
    pub const NORM_SUBFLOW_LOAD: &str = "RF2033";
    /// A referenced subflow failed to compile (the callee's own
    /// diagnostics are summarized at the call site).
    pub const NORM_SUBFLOW_COMPILE: &str = "RF2034";
    /// Cyclic subflow reference — the static call graph must be a DAG
    /// (02 §6 item 5).
    pub const NORM_CALL_CYCLE: &str = "RF2035";
    /// Static call-chain depth exceeds `maxCallDepth` (fail-closed
    /// resource bound over the link closure).
    pub const NORM_CALL_DEPTH: &str = "RF2036";
    /// The same callee flowId resolves to two different irHashes within
    /// one link closure (03 §1.6: version conflict).
    pub const NORM_SUBFLOW_CONFLICT: &str = "RF2037";
    /// `human` step shape invalid (mode/prompt/decisions/timeout shapes,
    /// `on_timeout` not `unknown`, `provideInput` without `expect_schema`,
    /// duplicate or missing timeout — 03 §1.6, 02 §4.4).
    pub const NORM_BAD_HUMAN_SHAPE: &str = "RF2038";
    /// `if`/`then`/`else` shape invalid (03 §1.6).
    pub const NORM_BAD_IF_SHAPE: &str = "RF2039";
    /// `foreach` shape invalid (`in`/`as`/`steps` subkeys, 03 §1.6).
    pub const NORM_BAD_FOREACH_SHAPE: &str = "RF2040";
    /// `let` shape invalid (bindings mapping, ≥ 1 entry, identifier
    /// names — 03 §1.6).
    pub const NORM_BAD_LET_SHAPE: &str = "RF2041";
    /// Handler shape invalid (hook value shape, disposition head key,
    /// `error_classes` outside `on_error`, `max_triggers` missing or < 1,
    /// `escalate` without valid human subkeys, `repair` path — 03 §1.8).
    pub const NORM_BAD_HANDLER_SHAPE: &str = "RF2042";
    /// `retry` policy shape invalid (`max_attempts` ≥ 1, `backoff_ms`
    /// number or `{initial, factor, max}`, `retry_on` non-empty ErrorClass
    /// list — 03 §1.2), or `retry` on a step kind with no act phase.
    pub const NORM_BAD_RETRY_SHAPE: &str = "RF2043";
    /// Top-level `outputs` shape invalid (`schema` + `from` per entry).
    pub const NORM_BAD_OUTPUTS_SHAPE: &str = "RF2044";
    /// `verdict_policy` outside the closed `standard | strict` set.
    pub const NORM_BAD_VERDICT_POLICY: &str = "RF2045";

    // ── RF3xxx: check ───────────────────────────────────────────────────
    /// Duplicate step id within the flow.
    pub const CHECK_DUPLICATE_STEP_ID: &str = "RF3001";
    /// Step id out of the author grammar (no `:` segments, no `.`).
    pub const CHECK_BAD_STEP_ID: &str = "RF3002";
    /// `${{ ... }}` expression syntax error.
    pub const CHECK_EXPR_SYNTAX: &str = "RF3003";
    /// Unknown function (outside the ten-function whitelist, 02 §8.2).
    pub const CHECK_UNKNOWN_FUNCTION: &str = "RF3004";
    /// Reference path out of the closed scope grammar (02 §8.1).
    pub const CHECK_BAD_REF_PATH: &str = "RF3005";
    /// Reference to a step id that does not exist in the flow.
    pub const CHECK_UNDEFINED_STEP: &str = "RF3006";
    /// Forward reference (only topologically earlier steps are visible).
    pub const CHECK_FORWARD_REF: &str = "RF3007";
    /// Illegal self-reference (02 §4.1.1: only `expect` expr predicates may
    /// read `steps.<self>.output.*`).
    pub const CHECK_ILLEGAL_SELF_REF: &str = "RF3008";
    /// Undeclared name under `params.` / `env.` / `vars.` / `iter.`.
    pub const CHECK_UNDECLARED_NAME: &str = "RF3009";
    /// Pure-function arity mismatch (02 §8.2 table).
    pub const CHECK_ARITY: &str = "RF3010";
    /// Literal-only argument position violated (`jsonPath` path,
    /// `regexMatch` pattern/flags).
    pub const CHECK_LIT_ONLY: &str = "RF3011";
    /// Invalid `jsonPath` / `regexMatch` literal (parse/compile failure).
    pub const CHECK_BAD_FN_LITERAL: &str = "RF3012";
    /// Duplicate assert id within one step.
    pub const CHECK_DUPLICATE_ASSERT_ID: &str = "RF3013";
    /// `vars.*` SSA single assignment violated (`let` rebinding an
    /// existing var name — 02 §4.7, §4.3 C8).
    pub const CHECK_SSA_REBIND: &str = "RF3014";
    /// `call.inputs` violate the callee's declared `params` contract
    /// (unknown input, missing required input, literal failing the param
    /// schema — 03 §4.3 F3).
    pub const CHECK_CALL_INPUT_CONTRACT: &str = "RF3015";
    /// R13 default-escalation lint (**warning**, 03 §1.8 / §4.3 F6): the
    /// flow has no disposition for `unknown` at all — neither a flow-level
    /// nor any step-level `on_unknown` handler.
    pub const LINT_NO_UNKNOWN_DISPOSITION: &str = "RF3016";
    /// A PureFn argument's definite type contradicts the 02 §8.2
    /// signature, or `eq`/`ne` compares two definite, different types
    /// (03 §4.3 C5).
    pub const CHECK_TYPE_MISMATCH: &str = "RF3017";
    /// A predicate position (`if.cond`, an `expr` assertion) is statically
    /// non-boolean (03 §4.3 C6).
    pub const CHECK_PREDICATE_NOT_BOOLEAN: &str = "RF3018";
    /// A `retry_on` entry the retry machinery can never fire on
    /// (spine §5: only `action_failed_retryable`, `target_stale`, and —
    /// idempotent steps only — `action_timed_out` retry). Warning: the
    /// entry is dead weight, not a hazard.
    pub const LINT_DEAD_RETRY_ON: &str = "RF3019";
    /// C7 (03 §1.3/§4.3): a field taken from an `invoke` output that no
    /// `expect_schema` narrowed — unknown-typed data must be narrowed
    /// before use, never dereferenced on faith.
    pub const CHECK_UNNARROWED_INVOKE_OUTPUT: &str = "RF3020";

    // ── RF4xxx: bind ────────────────────────────────────────────────────
    /// Provider name mismatch between flow, manifest and lockfile (D1).
    pub const BIND_PROVIDER_MISMATCH: &str = "RF4001";
    /// Lockfile digest does not match its content (tampered/stale).
    pub const BIND_LOCKFILE_DIGEST: &str = "RF4002";
    /// Lockfile protocol outside the manifest's supported window (D2).
    pub const BIND_PROTOCOL_WINDOW: &str = "RF4003";
    /// Action not in the capability set (D3).
    pub const BIND_UNKNOWN_ACTION: &str = "RF4004";
    /// Protected action rejected in v0.1 (D6, spine R6).
    pub const BIND_PROTECTED_ACTION: &str = "RF4005";
    /// Manifest fallback: action not covered by guaranteed features.
    pub const BIND_ACTION_NOT_GUARANTEED: &str = "RF4006";
    /// Arguments fail the static inputSchema shape check (D4).
    pub const BIND_ARGS_SHAPE: &str = "RF4007";
    /// Required feature unavailable in the compile-time feature set (D5).
    pub const BIND_FEATURE_UNAVAILABLE: &str = "RF4008";
    /// `effect` declaration missing (`invoke` has no inference, 03 §1.3).
    pub const BIND_EFFECT_REQUIRED: &str = "RF4009";
    /// A chain channel unavailable (not declared in manifest `channels`,
    /// role-incompatible, or platform-gated away — 03 §1.4 rule 6 / E6).
    pub const BIND_CHANNEL_UNAVAILABLE: &str = "RF4010";
    /// The action's own inputSchema does not compile (provider data bug).
    pub const BIND_ACTION_SCHEMA_INVALID: &str = "RF4011";
    /// No usable `VerbBinding` in the manifest for the verb/channel (or the
    /// binding's `argMap` lacks a needed entry) — spine §4.1 / A.6.
    pub const BIND_VERB_UNBOUND: &str = "RF4012";
    /// `locate_via` / `verify_via` is not an ordered subsequence of its
    /// canonical channel order (03 §1.4 rule 1/2 / E1).
    pub const BIND_CHAIN_ORDER: &str = "RF4013";
    /// A channel structurally illegal for the chain: `vision` in the
    /// act-chain, `coordinate` in the verify-chain, or a non-vision chain on
    /// a `visual` predicate (03 §1.4 rules 1/2 / E2, principle 7).
    pub const BIND_CHAIN_CHANNEL_ILLEGAL: &str = "RF4014";
    /// `coordinate` channel in the act-chain ⟺ static `coordinate: {x,y}`
    /// key violated, in either direction (03 §1.4 rule 3 / E4).
    pub const BIND_COORDINATE_STATIC: &str = "RF4015";
    /// Chain/selector material mismatch: `dom` needs `css`, `uiTree` needs
    /// at least one structured field (03 §1.4 rule 4 / E5, E8).
    pub const BIND_SELECTOR_MATERIAL: &str = "RF4016";
    /// vision in an `elementState`/`elementText` verify-chain ⟺ explicit
    /// `visual` prompt violated (03 §1.4 rule 5 / E3; the compiler never
    /// synthesizes vision prompts — principle 6).
    pub const BIND_VISION_PROMPT: &str = "RF4017";
    /// A `visual` predicate is the sole support for a step's `pass`
    /// (03 §1.5 / §4.3 E7, principle 7: vision never does the primary
    /// verification).
    pub const BIND_VISUAL_SOLE_SUPPORT: &str = "RF4018";
    /// A `provideInput` output schema declares a secret-semantic field
    /// (`sensitive: true` / `format: "password"`) — secret values never
    /// enter Pointlock's data plane; secret entry goes through a
    /// `repairWorld` pause instead (06 §3.1, the R6 fail-closed family).
    pub const BIND_SECRET_INPUT: &str = "RF4019";

    // ── seal (factory self-check only; not an authoring error) ─────────
    /// The sealed FlowIR failed validation against the generated schema —
    /// a compiler bug, surfaced instead of shipping an invalid artifact.
    pub const INTERNAL_FACTORY_CHECK: &str = "RF4901";
}
