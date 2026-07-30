//! # pointlock-expr
//!
//! Evaluator and minimal static checker for the Pointlock expression AST.
//!
//! The AST itself ([`Expr`] / [`PureFn`] / [`RefPath`]) lives in
//! `pointlock-ir` — the single type truth source (spine R12); this crate
//! owns the *runtime semantics*: [`eval`] over a materialized [`Scope`],
//! and the expression-local static [`check`] over a declared
//! [`ScopeShape`]. Authoritative design documents:
//! `docs/design/00-architecture-spine.md` §7 (closed scope, evaluation
//! timing, type checking) and `docs/design/02-typed-ir.md` §8 (grammar,
//! PureFn table, RefPath scope rules incl. the self-ref special case).
//!
//! ## Pinned by 02 §8 (implemented verbatim)
//!
//! - Closed scope roots: `params` / `env` / `vars` / `iter` / `steps`
//!   (`secrets.*` reserved for v0.2, rejected by the `RefPath` grammar).
//! - The ten-function whitelist and its arity table (§8.2), including the
//!   literal-only positions: `jsonPath` path, `regexMatch` pattern/flags.
//! - Non-Turing-completeness: evaluation is a pure computation — no I/O,
//!   no clock, no provider access — which is what makes offline
//!   re-judging (`judgeDirty` alignment) mathematically sound.
//! - *When* an expression is evaluated (ready-time argument snapshot vs
//!   probing/asserting, §8.3) and *which* refs are legal *where*
//!   (self-ref special case, §4.1.1) are positional rules owned by the
//!   runner and the compiler `check` stage; this crate evaluates whatever
//!   scope it is handed.
//!
//! ## Semantic choices where 02 §8 does not pin (documented at each site)
//!
//! - `eq`/`ne`: deep structural equality; numbers compare by mathematical
//!   value (`1 == 1.0`), no cross-type coercion.
//! - `and`/`or`: short-circuit left-to-right (the table notes no
//!   observable difference for pure expressions; errors in unreached
//!   arguments are consequently not surfaced).
//! - `len`: strings count Unicode scalar values.
//! - `coalesce`: JSON `null` and unresolvable *direct refs*
//!   (`missingRoot`/`missingPath`) count as absent; all-absent → `null`.
//! - `jsonPath`: nodelist collapses empty → `null`, one → node,
//!   many → array.
//! - `regexMatch`: flags vocabulary `i`/`m`/`s`/`x`; unanchored search.

mod check;
mod error;
mod eval;
mod scope;

pub use check::{ExprType, RefTypes, check, infer};
pub use error::{Arity, CheckDiagnostic, EvalError, JsonType, ScopeRoot};
pub use eval::{arity_of, eval};
pub use scope::{Scope, ScopeShape, StepScope};

// Re-exported for consumer convenience; definition home is pointlock-ir
// (type truth source, R12).
pub use pointlock_ir::{Expr, PureFn, RefPath};
