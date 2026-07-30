//! # pointlock-compiler
//!
//! The five compilation phases `parse → normalize → check → bind → seal`
//! (spine §8, 03 §4): the single, deterministic, zero-execution translator
//! from the authored YAML surface to the sealed [`pointlock_ir::FlowIR`]
//! artifact. Reads only [`ProviderManifest`] / [`CapabilityLockfile`]
//! snapshots — no device online, no provider code executed (R7).
//!
//! ## M2 scope (the full 03 §1 authoring surface)
//!
//! On top of the M1 verb/fallback/predicate core, the accepted YAML now
//! covers:
//!
//! - the structure heads `call` (relative-path subflow reference,
//!   recursively compiled against the same manifest/lockfile and
//!   `irHash`-pinned into `FlowIR.subflows` — cycle and `maxCallDepth`
//!   checked over the static link closure), `if`/`then`/`else`, `foreach`
//!   (`in`/`as`/`steps`) and `let` (03 §1.6);
//! - `human` steps (03 §1.6, 02 §4.4): `mode`/`prompt`/`presents`/
//!   `decisions`/`expect_schema`/`timeout_ms`; `on_timeout` is the fixed
//!   literal `unknown`;
//! - `macros` with hygienic expansion (03 §1.7): synthesized
//!   `<call id>:<body id>` step ids, AST-level `${{ macro.<param> }}`
//!   substitution, in-body cross-step references and recursion rejected,
//!   `checkpoint` defaulting to false inside expansions, `sourceMap`
//!   origin frames;
//! - flow- and step-level `handlers` (03 §1.8): the five Disposition head
//!   keys `retry`/`continue`/`escalate` (embedded human step under a
//!   synthesized id)/`abort`/`repair` (subflow-pinned), plus
//!   `error_classes` (on `on_error` only) and `max_triggers`;
//! - `preflight` (same assertion grammar as `expect`, judgeHash domain),
//!   step-level `retry`, top-level `outputs` and `verdict_policy`;
//! - the R13 lint (03 §4.3 F6): a flow with no `on_unknown` disposition
//!   anywhere compiles with a **warning** ([`Sealed::warnings`]).
//!
//! Still explicitly diagnosed as "not in the M2 subset" (fail-closed,
//! 03 §1.0): the step-level `outputs` projection, and `${{ }}` scalars
//! inside composite argument values. (`observe`/`screenshot` lower to
//! their provider-synthetic actions, 04 §9.4.3; string interpolation
//! desugars to `concat(...)`, `value:` to `text` exact, and
//! `expect_schema` narrows invoke outputs with C7 enforcing the
//! narrow-before-use rule — all delivered.)
//!
//! ## Phases and rejection surfaces (00 §8)
//!
//! | # | phase | rejects (RF code segment) |
//! |---|---|---|
//! | 1 | `parse` | YAML outside the JSON data model, merge keys, tags, duplicate keys, byte/depth/node limits (RF1xxx) |
//! | 2 | `normalize` | closed-vocabulary violations, shape errors, M1-subset boundaries, predicate staticness (F5); materializes defaults (`checkpoint: true`, `idempotent: false`, verb `effect` inference, `text` matcher defaults) (RF2xxx) |
//! | 3 | `check` | id grammar/uniqueness, `${{ }}` → [`Expr`](pointlock_ir::Expr) compilation, closed-scope reference topology, arity/lit-only obligations via `pointlock_expr` (RF3xxx) |
//! | 4 | `bind` | capability binding against lockfile (or manifest ∩ guaranteed), declarative verb binding (`VerbBinding`/`argMap`), the six fallback-chain rules (E1–E6, incl. vision-tail `visual` prompt), protected-action refusal (R6), static arg shape (D4), feature collection + semantic⇒uiSnapshot bundling (RF4xxx) |
//! | 5 | `seal` | none — stamps hashes, sourceMap, lockfileDigest; factory-checks the artifact against the generated schema (R12) |

mod authoring_schema;
mod bind;
mod check;
mod diagnostic;
mod expr_text;
mod normalize;
mod parse;
mod seal;
mod subflow;

pub use authoring_schema::authoring_schema;
pub use bind::{BindingActionSource, BindingReport, StepBindingReport};
pub use diagnostic::{CompileDiagnostic, DiagnosticSpan, Severity, codes, render_pretty};
pub use seal::Sealed;

use pointlock_provider_kit::lockfile::CapabilityLockfile;
use pointlock_provider_kit::manifest::ProviderManifest;

use crate::subflow::SubflowLinker;

/// Compilation inputs beyond the source text. The compiler consumes
/// declaration snapshots only — no device needs to be online (04 §1).
#[derive(Debug, Clone, Copy)]
pub struct CompileOptions<'a> {
    /// Name of the source file, recorded into `FlowIR.sourceMap[].file`.
    pub source_name: &'a str,
    /// The provider's static capability declaration.
    pub manifest: &'a ProviderManifest,
    /// The frozen device capability snapshot. Without one, binding falls
    /// back to `manifest.knownActions` restricted to guaranteed features
    /// (spine §4.1 compile rule), and `lockfileDigest` is a provisional
    /// manifest-derived digest that no runtime attestation will accept.
    pub lockfile: Option<&'a CapabilityLockfile>,
}

/// Compiles one `*.flow.yaml` source into a sealed [`Sealed`] artifact, or
/// the accumulated diagnostics of the first failing phase.
///
/// The five phases run strictly in order; a failing phase stops the
/// pipeline (later phases assume their input invariants). `call`/`repair`
/// references are resolved relative to `options.source_name` and compiled
/// recursively against the same manifest/lockfile (03 §1.6); the whole
/// link closure shares one linker (cycle, depth and version-conflict
/// checks). Seal validates the artifact against the schema generated by
/// `pointlock_ir::schema_gen` before returning it — the compiler's own
/// outgoing inspection.
pub fn compile(
    source: &str,
    options: &CompileOptions<'_>,
) -> Result<Sealed, Vec<CompileDiagnostic>> {
    let mut linker = SubflowLinker::new(options.manifest, options.lockfile, options.source_name);
    let mut sealed = match compile_linked(source, options.source_name, &mut linker) {
        Ok(sealed) => sealed,
        Err(mut diags) => {
            // 03 §4.4 span.file: stamped once, here — every span in one
            // compile belongs to the named source.
            for diag in &mut diags {
                diag.file = options.source_name.to_owned();
            }
            return Err(diags);
        }
    };
    for diag in &mut sealed.warnings {
        diag.file = options.source_name.to_owned();
    }
    // Attach the compiled callee closure: the CLI persists it next to the
    // root artifact, and the runner loads it as its subflow registry.
    sealed.subflow_irs = linker.into_compiled();
    Ok(sealed)
}

/// One compile within a link closure (the root flow, or a callee reached
/// through the shared [`SubflowLinker`]).
pub(crate) fn compile_linked(
    source: &str,
    source_name: &str,
    linker: &mut SubflowLinker<'_>,
) -> Result<Sealed, Vec<CompileDiagnostic>> {
    let root = parse::parse(source)?;
    let normalized = normalize::normalize(&root, source_name, linker)?;
    let (checked, warnings) = check::check(normalized)?;
    let bound = bind::bind(checked, linker.manifest, linker.lockfile)?;
    seal::seal(bound, source_name, warnings)
}
