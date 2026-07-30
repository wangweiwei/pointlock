//! Subflow linking (03 §1.6, 02 §6): relative-path references are compiled
//! recursively during `normalize` with the same manifest/lockfile snapshot,
//! pinned by `irHash` into the caller's `FlowIR.subflows` registry — the
//! link-closure property.
//!
//! The linker is shared across the whole closure so it can enforce, once,
//! the closure-global obligations:
//!
//! - cycle rejection — the static call graph must be a DAG (02 §6 item 5);
//! - `MAX_CALL_DEPTH` (fail-closed resource bound on static nesting);
//! - version-conflict rejection — one `flowId` must resolve to exactly one
//!   `irHash` within a link closure (03 §1.6);
//! - compile-once caching per canonical path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pointlock_ir::{FeatureId, FlowId, FlowRef, JsonSchemaDocument};
use pointlock_provider_kit::lockfile::CapabilityLockfile;
use pointlock_provider_kit::manifest::ProviderManifest;

use crate::diagnostic::{CompileDiagnostic, DiagnosticSpan, codes};

/// Maximum static call-chain depth (root flow = depth 0; a call edge that
/// would push the chain past this bound is a compile error).
pub(crate) const MAX_CALL_DEPTH: usize = 8;

/// How many callee diagnostics are summarized at the call site.
const CALLEE_DIAG_SUMMARY: usize = 3;

/// The caller-visible contract of one linked subflow.
#[derive(Debug, Clone)]
pub(crate) struct SubflowRecord {
    /// The content-pinned reference (into `CallStepIR.flowRef` /
    /// `HandlerAction::Repair.flowRef` and the `subflows` registry).
    pub flow_ref: FlowRef,
    /// The callee's feature union, merged into the caller's
    /// `requiredFeatures` (02 §6 item 5).
    pub required_features: std::collections::BTreeSet<FeatureId>,
    /// The callee's declared params (input-contract check, 03 §4.3 F3).
    pub params: Vec<SubflowParam>,
}

/// One callee parameter, as the caller-side contract check needs it.
#[derive(Debug, Clone)]
pub(crate) struct SubflowParam {
    /// Parameter name.
    pub name: String,
    /// The declared value contract.
    pub schema: JsonSchemaDocument,
    /// Whether the caller must supply it.
    pub required: bool,
    /// Whether the callee declares a default.
    pub has_default: bool,
}

/// Closure-wide linking state, threaded through every nested compile.
pub(crate) struct SubflowLinker<'a> {
    /// The provider's static capability declaration (shared closure-wide).
    pub manifest: &'a ProviderManifest,
    /// The frozen device capability snapshot (shared closure-wide).
    pub lockfile: Option<&'a CapabilityLockfile>,
    /// Canonicalized paths currently being compiled (cycle + depth).
    stack: Vec<PathBuf>,
    /// Compile-once cache per canonical path.
    cache: HashMap<PathBuf, SubflowRecord>,
    /// flowId → (irHash source path) registry for conflict detection.
    registry: HashMap<FlowId, (pointlock_ir::Hash, PathBuf)>,
    /// Every compiled callee artifact, keyed by content hash — the link
    /// closure the CLI persists next to the root (and the runner loads
    /// as its subflow registry).
    compiled: std::collections::BTreeMap<pointlock_ir::Hash, pointlock_ir::FlowIR>,
}

impl<'a> SubflowLinker<'a> {
    /// Creates the linker for one compile closure and seeds the stack with
    /// the root file when it exists on disk (string-only compiles, as in
    /// the M0/M1 test suites, have no resolvable root path — and no call
    /// steps that could need one).
    pub fn new(
        manifest: &'a ProviderManifest,
        lockfile: Option<&'a CapabilityLockfile>,
        root_source_name: &str,
    ) -> Self {
        let mut stack = Vec::new();
        if let Ok(canonical) = std::fs::canonicalize(root_source_name) {
            stack.push(canonical);
        }
        SubflowLinker {
            manifest,
            lockfile,
            stack,
            cache: HashMap::new(),
            registry: HashMap::new(),
            compiled: std::collections::BTreeMap::new(),
        }
    }

    /// The compiled callee closure, content-keyed (empty when the root
    /// links no subflows).
    pub fn into_compiled(
        self,
    ) -> std::collections::BTreeMap<pointlock_ir::Hash, pointlock_ir::FlowIR> {
        self.compiled
    }

    /// Resolves and compiles one relative subflow reference. `None` means
    /// diagnostics were recorded at the call site.
    pub fn link(
        &mut self,
        referrer: &str,
        path: &str,
        span: DiagnosticSpan,
        diags: &mut Vec<CompileDiagnostic>,
    ) -> Option<SubflowRecord> {
        let resolved = match Path::new(referrer).parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(path),
            _ => PathBuf::from(path),
        };
        let canonical = match std::fs::canonicalize(&resolved) {
            Ok(canonical) => canonical,
            Err(err) => {
                diags.push(CompileDiagnostic::error(
                    codes::NORM_SUBFLOW_LOAD,
                    format!(
                        "subflow '{path}' cannot be resolved (looked at '{}'): {err}",
                        resolved.display()
                    ),
                    Some(span),
                ));
                return None;
            }
        };

        // Cycle: the file is already somewhere up the compile stack.
        if self.stack.contains(&canonical) {
            diags.push(CompileDiagnostic::error(
                codes::NORM_CALL_CYCLE,
                format!(
                    "cyclic subflow reference: '{path}' is already being compiled on this \
                     call chain (the static call graph must be a DAG, 02 §6)"
                ),
                Some(span),
            ));
            return None;
        }

        // Cached compiles still pass the closure-global conflict check.
        if let Some(record) = self.cache.get(&canonical).cloned() {
            return Some(record);
        }

        // Depth: the root occupies stack slot 0; a chain deeper than
        // MAX_CALL_DEPTH call edges is rejected fail-closed.
        if self.stack.len() > MAX_CALL_DEPTH {
            diags.push(CompileDiagnostic::error(
                codes::NORM_CALL_DEPTH,
                format!(
                    "subflow '{path}' exceeds the maximum static call depth of {MAX_CALL_DEPTH}"
                ),
                Some(span),
            ));
            return None;
        }

        let source = match std::fs::read_to_string(&canonical) {
            Ok(source) => source,
            Err(err) => {
                diags.push(CompileDiagnostic::error(
                    codes::NORM_SUBFLOW_LOAD,
                    format!("subflow '{path}' cannot be read: {err}"),
                    Some(span),
                ));
                return None;
            }
        };

        let source_name = resolved.display().to_string();
        self.stack.push(canonical.clone());
        let compiled = crate::compile_linked(&source, &source_name, self);
        self.stack.pop();

        let sealed = match compiled {
            Ok(sealed) => sealed,
            Err(callee_diags) => {
                let summary: Vec<String> = callee_diags
                    .iter()
                    .take(CALLEE_DIAG_SUMMARY)
                    .map(|diag| format!("{}: {}", diag.code, diag.message))
                    .collect();
                let elided = callee_diags.len().saturating_sub(CALLEE_DIAG_SUMMARY);
                let suffix = if elided > 0 {
                    format!(" (+{elided} more)")
                } else {
                    String::new()
                };
                diags.push(CompileDiagnostic::error(
                    codes::NORM_SUBFLOW_COMPILE,
                    format!(
                        "subflow '{path}' failed to compile: {}{suffix}",
                        summary.join("; ")
                    ),
                    Some(span),
                ));
                return None;
            }
        };

        let flow = &sealed.flow_ir;
        // Version conflict: one flowId, one irHash per link closure.
        match self.registry.get(&flow.flow_id) {
            Some((ir_hash, first_path)) if *ir_hash != flow.ir_hash => {
                diags.push(CompileDiagnostic::error(
                    codes::NORM_SUBFLOW_CONFLICT,
                    format!(
                        "subflow flowId '{}' resolves to two different irHashes within this \
                         link closure ('{}' vs '{}')",
                        flow.flow_id,
                        first_path.display(),
                        canonical.display(),
                    ),
                    Some(span),
                ));
                return None;
            }
            Some(_) => {}
            None => {
                self.registry.insert(
                    flow.flow_id.clone(),
                    (flow.ir_hash.clone(), canonical.clone()),
                );
            }
        }

        self.compiled
            .insert(flow.ir_hash.clone(), sealed.flow_ir.clone());
        let record = SubflowRecord {
            flow_ref: FlowRef {
                flow_id: flow.flow_id.clone(),
                ir_hash: flow.ir_hash.clone(),
            },
            required_features: flow.required_features.clone(),
            params: flow
                .params
                .iter()
                .map(|param| SubflowParam {
                    name: param.name.to_string(),
                    schema: param.schema.clone(),
                    required: param.required,
                    has_default: param.default.is_some(),
                })
                .collect(),
        };
        self.cache.insert(canonical, record.clone());
        Some(record)
    }
}
