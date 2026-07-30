//! Phase 4 `bind`: typed steps + `ProviderManifest` + `CapabilityLockfile`
//! → fully bound act-chains and verify-chains (spine §8 stage 4 — "where
//! the constitution is enforced"; 03 §4.3 groups D and E).
//!
//! Capability rule (spine §4.1): the available action set is
//! `lockfile.device.actions`; without a lockfile it falls back to
//! `manifest.knownActions` restricted to actions covered by guaranteed
//! features. The available feature set is `manifest.features.guaranteed ∪
//! lockfile.hello.featuresEnabled`. A step needing anything outside those
//! sets is a compile error — no IR is produced (principle 5).
//!
//! ## M1 surface
//!
//! - element verbs bind through the manifest's declarative [`VerbBinding`]
//!   table (`argMap` field mapping, zero provider code — R7): the verb
//!   vanishes, `BoundAttempt.actionName` is the provider-native name;
//! - `locate_via` compiles to one [`BoundAttempt`] per channel; the
//!   `coordinate` channel consumes the static `coordinate: {x,y}` key and
//!   binds to the verb's coordinate `VerbBinding` (e.g. driver `tap{x,y}`);
//! - `verify_via` compiles 1:1 into [`AssertionIR::verify_via`]; the six
//!   chain-legality rules of 03 §1.4 are enforced here (E1–E6), including
//!   "vision tail ⟹ explicit `visual` prompt" — the compiler never
//!   synthesizes vision prompts (principle 6);
//! - `acceptExecutionModes` is derived per attempt from its own channel
//!   only (02 §5.1 rule 6): semantic attempts are always
//!   `[nativeSemantic, webSemantic]`, coordinate attempts always include
//!   `coordinateFallback`;
//! - feature collection bundles `observation.uiSnapshot.v1` whenever
//!   `device.semanticActions.v1` is collected (daemon handshake coupling,
//!   see [`bundle_semantic_snapshot_dependency`]).

use std::collections::{BTreeSet, HashMap};

use pointlock_ir::vocab::{
    ActChannel, CanonicalVerb, Channel, ElementState, ExecutionMode, HumanMode, VerifyChannel,
};
use pointlock_ir::{
    ActionName, AssertionIR, BoundAttempt, ElementSelectorIR, Expr, ExprMap, FeatureId, Hash,
    Identifier, OnMissingInput, PredicateIR, Protection, domain_hash,
};
use pointlock_provider_kit::lockfile::CapabilityLockfile;
use pointlock_provider_kit::manifest::{
    ActionDefinitionStatic, ActionProtection, ChannelRole, PlatformKind, ProviderManifest,
    VerbBinding,
};
use serde::{Deserialize, Serialize};

use crate::check::{
    CheckedAction, CheckedArg, CheckedAssertion, CheckedFlow, CheckedHandler, CheckedHandlerAction,
    CheckedHead, CheckedHuman, CheckedStep, CheckedStepKind, CheckedVerb,
};
use crate::diagnostic::{CompileDiagnostic, DiagnosticSpan, codes};

/// Domain tag of the provisional lockfile digest a manifest-fallback
/// compile embeds into `FlowIR.lockfileDigest`. M0 decision, pending spine
/// incorporation: the spine defines the digest only for real lockfiles; an
/// artifact compiled without one cannot attest anyway (00 §4.6), so the
/// pseudo-digest merely keeps the field honest and deterministic.
const MANIFEST_FALLBACK_DIGEST_TAG: &str = "pointlock-compiler/1/manifest-fallback-digest";

/// The semantic five's gating feature (spine A.8).
const SEMANTIC_ACTIONS_FEATURE: &str = "device.semanticActions.v1";
/// The UI snapshot observation feature (spine A.8).
const UI_SNAPSHOT_FEATURE: &str = "observation.uiSnapshot.v1";

/// Canonical act-chain order (03 §1.4 rule 1).
const ACT_ORDER: [Channel; 3] = [Channel::Dom, Channel::UiTree, Channel::Coordinate];
/// Canonical verify-chain order (03 §1.4 rule 2).
const VERIFY_ORDER: [Channel; 3] = [Channel::Dom, Channel::UiTree, Channel::Vision];

/// Which capability set actions were bound against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BindingActionSource {
    /// `lockfile.device.actions` (the normal path).
    LockfileDevice,
    /// `manifest.knownActions` restricted to guaranteed features
    /// (no lockfile supplied).
    ManifestGuaranteed,
}

/// Human-reviewable binding report (00 §8 stage 5 / 03 §4.5 confirmation
/// point 3): what every step bound to and which argument fields the static
/// shape check actually covered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingReport {
    /// Which capability set was used.
    pub action_source: BindingActionSource,
    /// Union of features the flow requires (mirrors
    /// `FlowIR.requiredFeatures`).
    pub required_features: BTreeSet<FeatureId>,
    /// Per-attempt binding facts, in body order then chain order (one
    /// entry per bound attempt; a multi-channel `locate_via` yields one
    /// entry per channel).
    pub steps: Vec<StepBindingReport>,
}

/// Binding facts for one bound attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepBindingReport {
    /// The step.
    pub step_id: String,
    /// The provider-native action the attempt bound to.
    pub action_name: ActionName,
    /// The attempt's locating channel.
    pub channel: ActChannel,
    /// Feature the attempt depends on, if any.
    pub requires_feature: Option<FeatureId>,
    /// Argument names whose literal values passed the static inputSchema
    /// shape check (D4).
    pub statically_validated_args: Vec<String>,
    /// Argument names deferred to the runtime re-validation (expression
    /// values, or literals the schema shape made statically undecidable).
    pub runtime_deferred_args: Vec<String>,
}

/// One step's fully bound parts, mirroring the checked tree 1:1 (seal
/// zips the two trees).
pub(crate) struct BoundStep {
    /// The bound preflight probes (verify-chains resolved).
    pub preflight: Vec<AssertionIR>,
    /// The kind-specific bound material.
    pub kind: BoundStepKind,
}

/// Kind-specific bound material of one step.
pub(crate) enum BoundStepKind {
    /// The bound act-chain (≥ 1 attempt) plus post-hoc assertions.
    Action {
        /// The act-chain attempts, in declared order.
        attempts: Vec<BoundAttempt>,
        /// The bound assertions with their final verify-chains.
        assertions: Vec<AssertionIR>,
    },
    /// An assert step's bound assertions (≥ 1).
    Assert {
        /// The bound assertions.
        assertions: Vec<AssertionIR>,
    },
    /// Call steps bind no device material (inputs are expressions; the
    /// callee's features were merged into the caller).
    Call,
    /// Human steps bind no device material.
    Human,
    /// An if step's bound children.
    If {
        /// The then-branch.
        then: Vec<BoundStep>,
        /// The optional else-branch.
        r#else: Option<Vec<BoundStep>>,
    },
    /// A foreach step's bound children.
    Foreach {
        /// The loop body.
        body: Vec<BoundStep>,
    },
    /// Let steps bind no device material.
    Let,
}

/// Output of the bind phase.
pub(crate) struct BoundFlow {
    /// The checked flow (step metadata carried through).
    pub checked: CheckedFlow,
    /// One bound step per checked step, in body order.
    pub steps: Vec<BoundStep>,
    /// Union of required features.
    pub required_features: BTreeSet<FeatureId>,
    /// The digest sealed into `FlowIR.lockfileDigest`.
    pub lockfile_digest: Hash,
    /// Provider package version (into `FlowIR.provider.version`).
    pub provider_version: String,
    /// The human-reviewable report.
    pub report: BindingReport,
}

/// Which chain a channel is consumed on (role compatibility, E6).
#[derive(Clone, Copy, PartialEq)]
enum ChainRole {
    Act,
    Verify,
}

/// One argument prepared for an attempt: the expression plus the material
/// the D4 static shape check needs.
struct AttemptArg {
    name: Identifier,
    span: DiagnosticSpan,
    expr: Expr,
    dynamic: bool,
}

/// Binds the checked flow against the manifest (and lockfile when given).
pub(crate) fn bind(
    checked: CheckedFlow,
    manifest: &ProviderManifest,
    lockfile: Option<&CapabilityLockfile>,
) -> Result<BoundFlow, Vec<CompileDiagnostic>> {
    let mut diags = Vec::new();

    // D1: provider identity must line up across flow, manifest, lockfile.
    if manifest.name != checked.provider {
        diags.push(CompileDiagnostic::error(
            codes::BIND_PROVIDER_MISMATCH,
            format!(
                "flow declares provider '{}' but the manifest is for '{}'",
                checked.provider, manifest.name
            ),
            None,
        ));
    }
    if let Some(lockfile) = lockfile {
        if lockfile.provider.name != manifest.name {
            diags.push(CompileDiagnostic::error(
                codes::BIND_PROVIDER_MISMATCH,
                format!(
                    "lockfile was produced by provider '{}', manifest is for '{}'",
                    lockfile.provider.name, manifest.name
                ),
                None,
            ));
        }
        // Lockfile integrity: the digest must match its content.
        if !lockfile.digest_consistent() {
            diags.push(CompileDiagnostic::error(
                codes::BIND_LOCKFILE_DIGEST,
                "lockfile digest does not match its content (stale or hand-edited lockfile; \
                 rerun `pointlock lock`)",
                None,
            ));
        }
        // D2: negotiated protocol within the manifest window.
        let selected = lockfile.hello.protocol_selected;
        if selected.major != manifest.protocol.major
            || selected.minor < manifest.protocol.min_minor
            || selected.minor > manifest.protocol.max_minor
        {
            diags.push(CompileDiagnostic::error(
                codes::BIND_PROTOCOL_WINDOW,
                format!(
                    "lockfile protocol {}.{} is outside the manifest window {}.{}-{}.{}",
                    selected.major,
                    selected.minor,
                    manifest.protocol.major,
                    manifest.protocol.min_minor,
                    manifest.protocol.major,
                    manifest.protocol.max_minor,
                ),
                None,
            ));
        }
    }
    if !diags.is_empty() {
        // Identity/integrity failures poison everything downstream.
        return Err(diags);
    }

    // The compile-time feature set: guaranteed ∪ lockfile.featuresEnabled.
    let mut available_features: BTreeSet<FeatureId> =
        manifest.features.guaranteed.iter().cloned().collect();
    if let Some(lockfile) = lockfile {
        available_features.extend(lockfile.hello.features_enabled.iter().cloned());
    }

    // The action set (spine §4.1 compile rule).
    let (actions, action_source): (&[ActionDefinitionStatic], BindingActionSource) = match lockfile
    {
        Some(lockfile) => (
            &lockfile.device.actions,
            BindingActionSource::LockfileDevice,
        ),
        None => (
            &manifest.known_actions,
            BindingActionSource::ManifestGuaranteed,
        ),
    };
    let mut action_index: HashMap<&ActionName, &ActionDefinitionStatic> = actions
        .iter()
        .map(|action| (&action.name, action))
        .collect();
    // Provider-synthetic actions (04 §9.4.3) are implemented by the
    // provider itself, so no driver declares them and they never appear in
    // a lockfile. Overlay them — but only where the name is free: a driver
    // action of the same name shadows the synthetic entry, which is the
    // ruling's "lockfile 优先" and leaves nothing ambiguous to choose
    // between. (Without a lockfile the manifest already is the action set.)
    for action in &manifest.known_actions {
        if action.synthetic {
            action_index.entry(&action.name).or_insert(action);
        }
    }

    let mut binder = Binder {
        manifest,
        actions,
        action_index,
        action_source,
        available_features,
        platform: lockfile.map(|lockfile| lockfile.device.platform),
        required_features: BTreeSet::new(),
        step_reports: Vec::new(),
        diags,
    };

    // 04 §9.2: `verdict.record.v1` rides every IR into
    // `requiredFeatures` — the runner writes a verdict back for every
    // judged step (spine §6.3), so the table's "compile time already
    // guarantees recordVerdict is never reached without the feature"
    // is made true here, not assumed.
    let verdict_record = FeatureId::new("verdict.record.v1").expect("grammatical feature id");
    if binder.available_features.contains(&verdict_record) {
        binder.required_features.insert(verdict_record);
    } else {
        binder.error(
            codes::BIND_FEATURE_UNAVAILABLE,
            "every flow requires feature 'verdict.record.v1' (verdict write-back, 04 §9.2), \
             which is not in the compile-time feature set — compile against a lockfile \
             (`pointlock lock`) whose daemon enables it",
            None,
        );
    }

    let mut steps = Vec::new();
    for step in &checked.steps {
        if let Some(bound) = binder.bind_step(step) {
            steps.push(bound);
        }
    }
    // Flow-level repair handlers contribute their callee features too
    // (02 §6 item 5); their escalate humans obey 06 §3.1 like any step.
    binder.merge_handler_features(&checked.handlers);
    binder.check_handler_humans_not_secret(&checked.handlers, None);

    binder.bundle_semantic_snapshot_dependency();

    let Binder {
        required_features,
        step_reports,
        diags,
        ..
    } = binder;

    if !diags.is_empty() {
        return Err(diags);
    }

    let lockfile_digest = match lockfile {
        Some(lockfile) => lockfile.digest.clone(),
        None => domain_hash(
            MANIFEST_FALLBACK_DIGEST_TAG,
            &serde_json::to_value(manifest).expect("a manifest serializes to JSON"),
        ),
    };

    Ok(BoundFlow {
        steps,
        required_features: required_features.clone(),
        lockfile_digest,
        provider_version: manifest.version.clone(),
        report: BindingReport {
            action_source,
            required_features,
            steps: step_reports,
        },
        checked,
    })
}

/// Per-flow binding state.
struct Binder<'a> {
    manifest: &'a ProviderManifest,
    actions: &'a [ActionDefinitionStatic],
    action_index: HashMap<&'a ActionName, &'a ActionDefinitionStatic>,
    action_source: BindingActionSource,
    available_features: BTreeSet<FeatureId>,
    platform: Option<PlatformKind>,
    required_features: BTreeSet<FeatureId>,
    step_reports: Vec<StepBindingReport>,
    diags: Vec<CompileDiagnostic>,
}

impl<'a> Binder<'a> {
    /// The platform's default locating/verification channel (03 §1.4:
    /// devicerail = `uiTree` everywhere except web; no lockfile = `uiTree`,
    /// matching M0).
    fn default_channel(&self) -> Channel {
        match self.platform {
            Some(PlatformKind::Web) => Channel::Dom,
            _ => Channel::UiTree,
        }
    }

    fn error(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Option<DiagnosticSpan>,
    ) {
        self.diags
            .push(CompileDiagnostic::error(code, message, span));
    }

    /// Merges the feature unions of repair handlers into the caller's
    /// closure-wide requirement set (02 §6 item 5).
    fn merge_handler_features(&mut self, handlers: &[CheckedHandler]) {
        for handler in handlers {
            if let CheckedHandlerAction::Repair {
                required_features, ..
            } = &handler.action
            {
                self.required_features
                    .extend(required_features.iter().cloned());
            }
        }
    }

    /// E7 (03 §1.5 / §4.3): a `visual` predicate must not be the only thing
    /// supporting a step's `pass`.
    ///
    /// The reason is principle 7 — vision never does the primary
    /// verification — and it is not decorative: `pointlock-vision`'s default
    /// implementation returns `unknown`, so a step verified by vision ALONE
    /// on a host with no vision capability folds to unknown rather than
    /// pass. That is the honest outcome, and the rule exists so the flow
    /// says so at compile time instead of discovering it in production.
    ///
    /// Two exemptions, both from §1.5: a structured-channel assertion in the
    /// same step (any non-`visual` predicate — `elementState`,
    /// `elementText`, `expr` — carries the pass on its own), or a step whose
    /// verdict is delegated to a human judge (an `escalate` handler with
    /// `mode: judge`, which produces the verdict itself).
    ///
    /// `preflight` is deliberately out of scope: it probes drift, it does
    /// not fold into a verdict, so nothing there supports a pass.
    fn check_visual_not_sole_support(&mut self, step: &CheckedStep, step_id: &str) {
        let assertions: &[CheckedAssertion] = match &step.kind {
            CheckedStepKind::Action(action) => &action.assertions,
            CheckedStepKind::Assert { assertions } => assertions,
            // Every other kind folds a verdict from something other than an
            // assertion list (a callee's flow verdict, a human decision).
            _ => return,
        };
        let visual = |assertion: &CheckedAssertion| {
            matches!(assertion.predicate, PredicateIR::Visual { .. })
        };
        if !assertions.iter().any(visual) || assertions.iter().any(|a| !visual(a)) {
            return;
        }
        let human_judged = step.handlers.iter().any(|handler| {
            matches!(
                &handler.action,
                CheckedHandlerAction::Escalate { human, .. } if human.mode == HumanMode::Judge
            )
        });
        if human_judged {
            return;
        }
        self.error(
            codes::BIND_VISUAL_SOLE_SUPPORT,
            format!(
                "step '{step_id}': a `visual` predicate is the only assertion supporting this \
                 step's pass — vision does not do the primary verification (03 §1.5 / §4.3 \
                 E7, principle 7). Add a structured assertion (`elementState` / \
                 `elementText` / `expr`) alongside it, or delegate the verdict to a `human` \
                 judge"
            ),
            Some(step.span),
        );
    }

    /// 06 §3.1: `provideInput` must never carry secret-semantic fields.
    /// Secret entry goes through a `repairWorld` pause — the human types
    /// on the device and the value never enters Pointlock's data plane
    /// (the same fail-closed family as R6's protected-action refusal).
    /// Covers the step body and its escalate handlers alike — the doc
    /// pins the rejection per outputSchema, not per step position.
    fn check_provide_input_not_secret(&mut self, step: &CheckedStep, step_id: &str) {
        if let CheckedStepKind::Human(human) = &step.kind {
            self.check_human_not_secret(human, step_id, Some(step.span));
        }
        self.check_handler_humans_not_secret(&step.handlers, Some(step.span));
    }

    /// The escalate-handler leg of the same 06 §3.1 rule (used for both
    /// step-level and flow-level handler lists).
    fn check_handler_humans_not_secret(
        &mut self,
        handlers: &[CheckedHandler],
        span: Option<DiagnosticSpan>,
    ) {
        for handler in handlers {
            if let CheckedHandlerAction::Escalate { step_id, human } = &handler.action {
                let step_id = step_id.to_string();
                self.check_human_not_secret(human, &step_id, span);
            }
        }
    }

    fn check_human_not_secret(
        &mut self,
        human: &CheckedHuman,
        step_id: &str,
        span: Option<DiagnosticSpan>,
    ) {
        if human.mode != HumanMode::ProvideInput {
            return;
        }
        let Some(schema) = &human.output_schema else {
            return;
        };
        let mut offenders = Vec::new();
        collect_secret_schema_fields(schema.as_value(), "", &mut offenders);
        for pointer in offenders {
            self.error(
                codes::BIND_SECRET_INPUT,
                format!(
                    "step '{step_id}': `expect_schema` marks `{pointer}` as secret \
                     (`sensitive: true` / `format: \"password\"`) — secret values never \
                     enter Pointlock's data plane. Use a `repairWorld` pause and have the \
                     human enter it on the device (06 §3.1)"
                ),
                span,
            );
        }
    }

    /// Binds one assertion list; `None` means diagnostics were recorded.
    fn bind_assertions(
        &mut self,
        step_id: &str,
        assertions: &[CheckedAssertion],
    ) -> Option<Vec<AssertionIR>> {
        let mut bound = Vec::new();
        let mut ok = true;
        for assertion in assertions {
            match self.bind_assertion(step_id, assertion) {
                Some(assertion) => bound.push(assertion),
                None => ok = false,
            }
        }
        ok.then_some(bound)
    }

    /// Binds one step (recursively for containers); `None` means
    /// diagnostics were recorded.
    fn bind_step(&mut self, step: &CheckedStep) -> Option<BoundStep> {
        let step_id = step.step_id.to_string();
        let preflight = self.bind_assertions(&step_id, &step.preflight);
        self.merge_handler_features(&step.handlers);
        self.check_visual_not_sole_support(step, &step_id);
        self.check_provide_input_not_secret(step, &step_id);

        let kind = match &step.kind {
            CheckedStepKind::Action(action) => self.bind_action(&step_id, step.span, action),
            CheckedStepKind::Assert { assertions } => self
                .bind_assertions(&step_id, assertions)
                .map(|assertions| BoundStepKind::Assert { assertions }),
            CheckedStepKind::Call(call) => {
                // The callee compiled against the same lockfile, so its
                // features were available by construction; the caller
                // still collects them (02 §6 item 5).
                self.required_features
                    .extend(call.required_features.iter().cloned());
                Some(BoundStepKind::Call)
            }
            CheckedStepKind::Human(_) => Some(BoundStepKind::Human),
            CheckedStepKind::If(if_step) => {
                let mut ok = true;
                let mut then = Vec::new();
                for child in &if_step.then {
                    match self.bind_step(child) {
                        Some(bound) => then.push(bound),
                        None => ok = false,
                    }
                }
                let r#else = if_step.r#else.as_ref().map(|steps| {
                    let mut bound = Vec::new();
                    for child in steps {
                        match self.bind_step(child) {
                            Some(step) => bound.push(step),
                            None => ok = false,
                        }
                    }
                    bound
                });
                ok.then_some(BoundStepKind::If { then, r#else })
            }
            CheckedStepKind::Foreach(foreach) => {
                let mut ok = true;
                let mut body = Vec::new();
                for child in &foreach.body {
                    match self.bind_step(child) {
                        Some(bound) => body.push(bound),
                        None => ok = false,
                    }
                }
                ok.then_some(BoundStepKind::Foreach { body })
            }
            CheckedStepKind::Let { .. } => Some(BoundStepKind::Let),
        };

        match (preflight, kind) {
            (Some(preflight), Some(kind)) => Some(BoundStep { preflight, kind }),
            _ => None,
        }
    }

    /// Binds an action step's act-chain and assertions.
    fn bind_action(
        &mut self,
        step_id: &str,
        step_span: DiagnosticSpan,
        action: &CheckedAction,
    ) -> Option<BoundStepKind> {
        // Effect declaration is mandatory for invoke (03 §1.3: no
        // inference); verbs had theirs materialized in normalize.
        if action.effect.is_none() {
            self.error(
                codes::BIND_EFFECT_REQUIRED,
                format!(
                    "step '{step_id}': `invoke` requires an explicit `effect: \
                     mutating|readonly` declaration"
                ),
                Some(step_span),
            );
        }

        let attempts = match &action.head {
            CheckedHead::Invoke {
                action: action_name,
                action_span,
                args,
            } => self.bind_invoke(step_id, step_span, action_name, *action_span, args),
            CheckedHead::Verb(verb) => self.bind_verb_chain(step_id, action, verb),
        };
        let assertions = self.bind_assertions(step_id, &action.assertions);

        match (attempts, assertions) {
            (Some(attempts), Some(assertions)) => Some(BoundStepKind::Action {
                attempts,
                assertions,
            }),
            _ => None,
        }
    }

    // ── act-chain construction ──────────────────────────────────────────

    /// Binds an `invoke` step: a single attempt on the platform's default
    /// locating channel (M0 shape; a declarative multi-channel chain cannot
    /// be constructed for an explicit native action — normalize rejects
    /// `locate_via` on invoke).
    fn bind_invoke(
        &mut self,
        step_id: &str,
        step_span: DiagnosticSpan,
        action_name: &ActionName,
        action_span: DiagnosticSpan,
        args: &[CheckedArg],
    ) -> Option<Vec<BoundAttempt>> {
        let channel = self.default_channel();
        let channel_ok = self.channel_available(channel, ChainRole::Act, step_id, Some(step_span));

        // Feature lookup: an invoke of a verb-bound native action inherits
        // that binding's feature gate (M0 behavior).
        let verb_feature = self
            .manifest
            .verb_bindings
            .iter()
            .find(|binding| binding.action_name == *action_name)
            .and_then(|binding| binding.requires_feature.clone());

        let attempt_args: Vec<AttemptArg> = args
            .iter()
            .map(|arg| AttemptArg {
                name: arg.name.clone(),
                span: arg.name_span,
                expr: arg.expr.clone(),
                dynamic: arg.dynamic,
            })
            .collect();

        let attempt = self.bind_attempt(
            step_id,
            step_span,
            channel,
            action_name,
            action_span,
            verb_feature,
            attempt_args,
        )?;
        channel_ok.then_some(vec![attempt])
    }

    /// Binds a verb step's act-chain: one attempt per declared channel
    /// (03 §1.4), or the single platform-default attempt.
    fn bind_verb_chain(
        &mut self,
        step_id: &str,
        action: &CheckedAction,
        verb: &CheckedVerb,
    ) -> Option<Vec<BoundAttempt>> {
        let (chain, chain_span) = match &action.locate_via {
            Some(declared) => (declared.channels.clone(), declared.span),
            None => (vec![self.default_channel()], verb.head_span),
        };

        // E1/E2 over the canonical act order.
        let mut ok = self.validate_chain_order(
            &chain,
            chain_span,
            &ACT_ORDER,
            "locate_via",
            "vision cannot locate or act (principle 7)",
            step_id,
        );

        // E4: coordinate channel ⟺ static coordinate key (both directions).
        let has_coordinate_channel = chain.contains(&Channel::Coordinate);
        match (&action.coordinate, has_coordinate_channel) {
            (None, true) => {
                self.error(
                    codes::BIND_COORDINATE_STATIC,
                    format!(
                        "step '{step_id}': the act-chain declares channel `coordinate` but the \
                         static `coordinate: {{ x, y }}` key is missing (03 §1.4 rule 3, \
                         principle 6)"
                    ),
                    Some(chain_span),
                );
                ok = false;
            }
            (Some(coordinate), false) => {
                self.error(
                    codes::BIND_COORDINATE_STATIC,
                    format!(
                        "step '{step_id}': a static `coordinate` key is present but \
                         `coordinate` is not in the act-chain (03 §1.4 rule 3 — the two are \
                         mutually required)"
                    ),
                    Some(coordinate.span),
                );
                ok = false;
            }
            _ => {}
        }
        if !ok {
            return None;
        }

        let mut attempts = Vec::new();
        let mut chain_ok = true;
        for channel in chain {
            let attempt = match channel {
                Channel::Dom | Channel::UiTree => {
                    self.bind_semantic_attempt(step_id, verb, channel)
                }
                Channel::Coordinate => self.bind_coordinate_attempt(step_id, action, verb),
                Channel::Vision => None, // E2 already diagnosed above.
            };
            match attempt {
                Some(attempt) => attempts.push(attempt),
                None => chain_ok = false,
            }
        }
        (chain_ok && !attempts.is_empty()).then_some(attempts)
    }

    /// Binds one semantic (dom/uiTree) attempt of a verb step.
    fn bind_semantic_attempt(
        &mut self,
        step_id: &str,
        verb: &CheckedVerb,
        channel: Channel,
    ) -> Option<BoundAttempt> {
        if !self.channel_available(channel, ChainRole::Act, step_id, Some(verb.head_span)) {
            return None;
        }
        // E5: the channel must have selector material to consume.
        if !selector_supports_channel(&verb.element, channel) {
            self.error(
                codes::BIND_SELECTOR_MATERIAL,
                format!(
                    "step '{step_id}': channel `{}` is in the act-chain but the selector \
                     carries {} (03 §1.4 rule 4)",
                    channel_token(channel),
                    material_requirement(channel),
                ),
                Some(verb.element_span),
            );
            return None;
        }

        let binding = self.semantic_binding(verb.verb, step_id, verb.head_span)?;
        let requires_feature = binding.requires_feature.clone();
        let action_name = binding.action_name.clone();
        let args = self.semantic_args(verb, binding, step_id)?;
        self.bind_attempt(
            step_id,
            verb.head_span,
            channel,
            &action_name,
            verb.head_span,
            requires_feature,
            args,
        )
    }

    /// Binds the explicit coordinate attempt of a verb step (03 §1.4: the
    /// static `coordinate` key maps onto the verb's coordinate binding,
    /// e.g. driver `tap{x,y}`).
    fn bind_coordinate_attempt(
        &mut self,
        step_id: &str,
        action: &CheckedAction,
        verb: &CheckedVerb,
    ) -> Option<BoundAttempt> {
        let coordinate = action
            .coordinate
            .as_ref()
            .expect("E4 guaranteed the static coordinate key");
        if !self.channel_available(
            Channel::Coordinate,
            ChainRole::Act,
            step_id,
            Some(coordinate.span),
        ) {
            return None;
        }
        let binding = self.coordinate_binding(verb.verb, step_id, coordinate.span)?;
        let requires_feature = binding.requires_feature.clone();
        let action_name = binding.action_name.clone();
        let mut args = Vec::new();
        for (surface, number) in [("x", &coordinate.x), ("y", &coordinate.y)] {
            let native = self.mapped_arg_name(binding, surface, step_id, coordinate.span)?;
            args.push(AttemptArg {
                name: native,
                span: coordinate.span,
                expr: Expr::lit(serde_json::Value::Number(number.clone())),
                dynamic: false,
            });
        }
        self.bind_attempt(
            step_id,
            coordinate.span,
            Channel::Coordinate,
            &action_name,
            coordinate.span,
            requires_feature,
            args,
        )
    }

    /// Builds the argument list of a semantic attempt through the
    /// binding's declarative `argMap` (spine §4.1, R7).
    fn semantic_args(
        &mut self,
        verb: &CheckedVerb,
        binding: &VerbBinding,
        step_id: &str,
    ) -> Option<Vec<AttemptArg>> {
        let mut args = Vec::new();

        let element_native =
            self.mapped_arg_name(binding, "element", step_id, verb.element_span)?;
        args.push(AttemptArg {
            name: element_native,
            span: verb.element_span,
            expr: Expr::lit(element_wire_value(verb.verb, &verb.element)),
            dynamic: false,
        });

        if let Some(value) = &verb.value {
            let native = self.mapped_arg_name(binding, "value", step_id, value.name_span)?;
            args.push(AttemptArg {
                name: native,
                span: value.name_span,
                expr: value.expr.clone(),
                dynamic: value.dynamic,
            });
        }

        if let Some(state) = verb.state {
            let native = self.mapped_arg_name(binding, "state", step_id, verb.head_span)?;
            args.push(AttemptArg {
                name: native,
                span: verb.head_span,
                expr: Expr::lit(element_state_wire(state)),
                dynamic: false,
            });
        }

        Some(args)
    }

    /// Shared attempt assembly: capability resolution (D3/D5/D6, manifest
    /// fallback), static arg shape (D4), mode derivation (02 §5.1 rule 6)
    /// and the per-attempt report entry.
    #[allow(clippy::too_many_arguments)]
    fn bind_attempt(
        &mut self,
        step_id: &str,
        step_span: DiagnosticSpan,
        channel: Channel,
        action_name: &ActionName,
        action_span: DiagnosticSpan,
        requires_feature: Option<FeatureId>,
        args: Vec<AttemptArg>,
    ) -> Option<BoundAttempt> {
        let step_id = step_id.to_owned();

        // D3: the action must exist in the capability set.
        let Some(action) = self.action_index.get(action_name).copied() else {
            let mut known: Vec<&str> = self.actions.iter().map(|a| a.name.as_str()).collect();
            known.sort_unstable();
            let source = match self.action_source {
                BindingActionSource::LockfileDevice => "lockfile.device.actions",
                BindingActionSource::ManifestGuaranteed => "manifest.knownActions, no lockfile",
            };
            // 03 §4.4 `candidates`: nearest capabilities by edit distance,
            // for the human and the LLM repair loop alike.
            let candidates = nearest_names(action_name.as_str(), &known);
            self.diags.push(
                CompileDiagnostic::error(
                    codes::BIND_UNKNOWN_ACTION,
                    format!(
                        "step '{step_id}': action '{action_name}' is not in the capability \
                         set ({source}); known actions: {}",
                        known.join(", "),
                    ),
                    Some(action_span),
                )
                .with_candidates(candidates)
                .with_hint(
                    "the capability set is frozen by `pointlock lock`; re-lock against the \
                     running daemon if the device gained the action (04 §2)",
                ),
            );
            return None;
        };

        // D6 / R6: protected actions are rejected in v0.1.
        if action.protection == ActionProtection::Protected {
            self.error(
                codes::BIND_PROTECTED_ACTION,
                format!(
                    "step '{step_id}': action '{action_name}' is protected; v0.1 rejects \
                     protected actions at bind (spine R6)",
                ),
                Some(action_span),
            );
            return None;
        }

        // Manifest fallback restriction: only actions covered by guaranteed
        // features are bindable without a lockfile (spine §4.1).
        if self.action_source == BindingActionSource::ManifestGuaranteed
            && let Some(feature) = &requires_feature
            && !self.manifest.features.guaranteed.contains(feature)
        {
            self.error(
                codes::BIND_ACTION_NOT_GUARANTEED,
                format!(
                    "step '{step_id}': action '{action_name}' requires feature '{feature}', \
                     which is not guaranteed — compile against a lockfile (`pointlock lock`)",
                ),
                Some(action_span),
            );
            return None;
        }

        // D5: the attempt's feature must be in the compile-time set.
        if let Some(feature) = &requires_feature {
            if self.available_features.contains(feature) {
                self.required_features.insert(feature.clone());
            } else {
                self.error(
                    codes::BIND_FEATURE_UNAVAILABLE,
                    format!(
                        "step '{step_id}': action '{action_name}' requires feature \
                         '{feature}', which is not in the compile-time feature set",
                    ),
                    Some(action_span),
                );
                return None;
            }
        }

        // D4: static shape check of the literal argument parts.
        let (validated, deferred) =
            self.check_args_shape(&step_id, step_span, action_span, &args, action);

        let act_channel = match channel {
            Channel::Dom => ActChannel::Dom,
            Channel::UiTree => ActChannel::UiTree,
            Channel::Coordinate => ActChannel::Coordinate,
            Channel::Vision => unreachable!("E2 excluded vision from act-chains"),
        };
        // 02 §5.1 rule 6: the whitelist is derived from the attempt's own
        // channel only — semantic attempts never admit the daemon-internal
        // coordinate fallback, regardless of what the chain declares.
        let accept_execution_modes = match act_channel {
            ActChannel::Coordinate => BTreeSet::from([
                ExecutionMode::NativeSemantic,
                ExecutionMode::WebSemantic,
                ExecutionMode::CoordinateFallback,
            ]),
            _ => BTreeSet::from([ExecutionMode::NativeSemantic, ExecutionMode::WebSemantic]),
        };

        self.step_reports.push(StepBindingReport {
            step_id,
            action_name: action_name.clone(),
            channel: act_channel,
            requires_feature: requires_feature.clone(),
            statically_validated_args: validated,
            runtime_deferred_args: deferred,
        });

        Some(BoundAttempt {
            channel: act_channel,
            action_name: action_name.clone(),
            args: args
                .into_iter()
                .map(|arg| (arg.name, arg.expr))
                .collect::<ExprMap>(),
            requires_feature,
            accept_execution_modes,
            protection: Protection::Value,
        })
    }

    // ── verify-chain construction ───────────────────────────────────────

    /// Binds one assertion's verify-chain (03 §1.4 / 02 §5.3) — shared by
    /// `expect` and `preflight` (03 §1.5: one assertion grammar).
    fn bind_assertion(
        &mut self,
        step_id: &str,
        assertion: &CheckedAssertion,
    ) -> Option<AssertionIR> {
        let step_id = step_id.to_owned();
        let verify_via = match &assertion.predicate {
            // Expr predicates consume no observation channel (02 §5.3);
            // normalize rejected any written `verify_via`.
            PredicateIR::Expr { .. } => Vec::new(),
            PredicateIR::Visual { .. } => {
                // Visual predicates are vision-only (spine A.4 coupling).
                if let Some(declared) = &assertion.verify_via
                    && declared.channels != [Channel::Vision]
                {
                    self.error(
                        codes::BIND_CHAIN_CHANNEL_ILLEGAL,
                        format!(
                            "step '{step_id}': a `visual` predicate is vision-only; its \
                             `verify_via` must be exactly [vision] (02 §5.3)"
                        ),
                        Some(declared.span),
                    );
                    return None;
                }
                if !self.channel_available(
                    Channel::Vision,
                    ChainRole::Verify,
                    &step_id,
                    Some(assertion.span),
                ) {
                    return None;
                }
                vec![VerifyChannel::Vision]
            }
            PredicateIR::ElementState { selector, .. }
            | PredicateIR::ElementText { selector, .. } => {
                let (chain, chain_span) = match &assertion.verify_via {
                    Some(declared) => (declared.channels.clone(), declared.span),
                    None => (vec![self.default_channel()], assertion.span),
                };
                if !self.validate_chain_order(
                    &chain,
                    chain_span,
                    &VERIFY_ORDER,
                    "verify_via",
                    "a coordinate cannot verify anything",
                    &step_id,
                ) {
                    return None;
                }

                // Rule 5 / E3: a vision tail demands the author's explicit
                // `visual` prompt — the compiler never synthesizes one
                // (principle 6); without vision the prompt is forbidden
                // (single representation).
                let has_vision = chain.contains(&Channel::Vision);
                match (&assertion.visual, has_vision) {
                    (None, true) => {
                        self.error(
                            codes::BIND_VISION_PROMPT,
                            format!(
                                "step '{step_id}': the verify-chain degrades to vision but the \
                                 assertion carries no `visual:` prompt — the compiler never \
                                 generates vision prompts (03 §1.4 rule 5, principle 6)"
                            ),
                            Some(chain_span),
                        );
                        return None;
                    }
                    (Some((_, prompt_span)), false) => {
                        self.error(
                            codes::BIND_VISION_PROMPT,
                            format!(
                                "step '{step_id}': a `visual:` prompt is present but the \
                                 verify-chain never reaches vision (03 §1.4 rule 5)"
                            ),
                            Some(*prompt_span),
                        );
                        return None;
                    }
                    _ => {}
                }

                let mut ok = true;
                let mut verify_via = Vec::new();
                for channel in chain {
                    if !self.channel_available(
                        channel,
                        ChainRole::Verify,
                        &step_id,
                        Some(chain_span),
                    ) {
                        ok = false;
                        continue;
                    }
                    // E5: the channel must have selector material.
                    if !selector_supports_channel(selector, channel) {
                        self.error(
                            codes::BIND_SELECTOR_MATERIAL,
                            format!(
                                "step '{step_id}': channel `{}` is in the verify-chain but \
                                 the selector carries {} (03 §1.4 rule 4)",
                                channel_token(channel),
                                material_requirement(channel),
                            ),
                            Some(chain_span),
                        );
                        ok = false;
                        continue;
                    }
                    verify_via.push(match channel {
                        Channel::Dom => VerifyChannel::Dom,
                        Channel::UiTree => VerifyChannel::UiTree,
                        Channel::Vision => VerifyChannel::Vision,
                        Channel::Coordinate => unreachable!("E2 excluded coordinate"),
                    });
                }
                if !ok {
                    return None;
                }
                verify_via
            }
        };

        Some(AssertionIR {
            assert_id: assertion.assert_id.clone(),
            predicate: assertion.predicate.clone(),
            verify_via,
            vision_prompt: assertion.visual.as_ref().map(|(prompt, _)| prompt.clone()),
            on_missing_input: OnMissingInput::Value,
        })
    }

    // ── chain / channel / binding helpers ───────────────────────────────

    /// E1/E2: the chain must be a non-empty ordered subsequence of the
    /// canonical order; channels outside the order are structurally
    /// illegal for this chain kind.
    fn validate_chain_order(
        &mut self,
        chain: &[Channel],
        span: DiagnosticSpan,
        order: &[Channel; 3],
        chain_name: &str,
        illegal_reason: &str,
        step_id: &str,
    ) -> bool {
        let mut ok = true;
        let mut last_index: Option<usize> = None;
        for channel in chain {
            let Some(index) = order.iter().position(|c| c == channel) else {
                self.error(
                    codes::BIND_CHAIN_CHANNEL_ILLEGAL,
                    format!(
                        "step '{step_id}': channel `{}` is illegal in `{chain_name}` — \
                         {illegal_reason}",
                        channel_token(*channel),
                    ),
                    Some(span),
                );
                ok = false;
                continue;
            };
            if last_index.is_some_and(|last| index <= last) {
                let canonical: Vec<&str> = order.iter().map(|c| channel_token(*c)).collect();
                self.error(
                    codes::BIND_CHAIN_ORDER,
                    format!(
                        "step '{step_id}': `{chain_name}` must be an ordered subsequence of \
                         [{}] without duplicates (03 §1.4)",
                        canonical.join(", "),
                    ),
                    Some(span),
                );
                ok = false;
                break;
            }
            last_index = Some(index);
        }
        ok
    }

    /// E6: the channel must be declared in the manifest with a compatible
    /// role, its platform gate must admit the locked platform, and its
    /// feature must be available (then it is collected).
    fn channel_available(
        &mut self,
        channel: Channel,
        role: ChainRole,
        step_id: &str,
        span: Option<DiagnosticSpan>,
    ) -> bool {
        let support = self.manifest.channels.iter().find(|support| {
            support.channel == channel
                && match role {
                    ChainRole::Act => {
                        matches!(support.role, ChannelRole::Act | ChannelRole::Both)
                    }
                    ChainRole::Verify => {
                        matches!(support.role, ChannelRole::Verify | ChannelRole::Both)
                    }
                }
        });
        let Some(support) = support else {
            let role_name = match role {
                ChainRole::Act => "act",
                ChainRole::Verify => "verify",
            };
            self.error(
                codes::BIND_CHANNEL_UNAVAILABLE,
                format!(
                    "step '{step_id}': channel `{}` is not declared with an {role_name}-capable \
                     role in the provider manifest (03 §1.4 rule 6)",
                    channel_token(channel),
                ),
                span,
            );
            return false;
        };
        if let (Some(platforms), Some(platform)) = (&support.requires_platform, self.platform)
            && !platforms.contains(&platform)
        {
            self.error(
                codes::BIND_CHANNEL_UNAVAILABLE,
                format!(
                    "step '{step_id}': channel `{}` is not available on the locked platform \
                     (03 §1.4 rule 6)",
                    channel_token(channel),
                ),
                span,
            );
            return false;
        }
        if let Some(feature) = &support.requires_feature {
            if self.available_features.contains(feature) {
                self.required_features.insert(feature.clone());
            } else {
                self.error(
                    codes::BIND_FEATURE_UNAVAILABLE,
                    format!(
                        "step '{step_id}': channel `{}` requires feature '{feature}', which \
                         is not in the compile-time feature set",
                        channel_token(channel),
                    ),
                    span,
                );
                return false;
            }
        }
        true
    }

    /// The verb's semantic binding: the `VerbBinding` whose `argMap` maps
    /// the verb's `element` surface argument.
    ///
    /// M1 decision, pending spine incorporation: `VerbBinding` carries no
    /// channel discriminator, so the argMap domain discriminates — a
    /// binding mapping `element` serves the semantic channels, a binding
    /// mapping `x`/`y` serves the coordinate channel. Ambiguity is a
    /// compile error, never a silent pick.
    fn semantic_binding(
        &mut self,
        verb: CanonicalVerb,
        step_id: &str,
        span: DiagnosticSpan,
    ) -> Option<&'a VerbBinding> {
        self.select_binding(verb, step_id, span, "semantic", |binding| {
            binding.arg_map.contains_key("element")
        })
    }

    /// The verb's coordinate binding: the `VerbBinding` whose `argMap`
    /// maps the static `x`/`y` coordinate surface (see
    /// [`Self::semantic_binding`] for the discrimination rule).
    fn coordinate_binding(
        &mut self,
        verb: CanonicalVerb,
        step_id: &str,
        span: DiagnosticSpan,
    ) -> Option<&'a VerbBinding> {
        self.select_binding(verb, step_id, span, "coordinate", |binding| {
            binding.arg_map.contains_key("x") && binding.arg_map.contains_key("y")
        })
    }

    fn select_binding(
        &mut self,
        verb: CanonicalVerb,
        step_id: &str,
        span: DiagnosticSpan,
        kind: &str,
        matches: impl Fn(&VerbBinding) -> bool,
    ) -> Option<&'a VerbBinding> {
        let verb_token = verb_token(verb);
        let mut candidates = self
            .manifest
            .verb_bindings
            .iter()
            .filter(|binding| binding.verb == verb && matches(binding));
        let first = candidates.next();
        let second = candidates.next();
        match (first, second) {
            (Some(binding), None) => Some(binding),
            (None, _) => {
                self.error(
                    codes::BIND_VERB_UNBOUND,
                    format!(
                        "step '{step_id}': the provider manifest declares no {kind} \
                         `verbBinding` for verb `{verb_token}` (spine §4.1)"
                    ),
                    Some(span),
                );
                None
            }
            (Some(_), Some(_)) => {
                self.error(
                    codes::BIND_VERB_UNBOUND,
                    format!(
                        "step '{step_id}': the provider manifest declares more than one {kind} \
                         `verbBinding` for verb `{verb_token}` — ambiguous, refusing to pick"
                    ),
                    Some(span),
                );
                None
            }
        }
    }

    /// Resolves one surface argument name through the binding's `argMap`.
    fn mapped_arg_name(
        &mut self,
        binding: &VerbBinding,
        surface: &str,
        step_id: &str,
        span: DiagnosticSpan,
    ) -> Option<Identifier> {
        let Some(native) = binding.arg_map.get(surface) else {
            self.error(
                codes::BIND_VERB_UNBOUND,
                format!(
                    "step '{step_id}': the `verbBinding` for verb `{}` lacks an argMap entry \
                     for `{surface}`",
                    verb_token(binding.verb),
                ),
                Some(span),
            );
            return None;
        };
        match Identifier::new(native.clone()) {
            Ok(identifier) => Some(identifier),
            Err(err) => {
                self.error(
                    codes::BIND_VERB_UNBOUND,
                    format!(
                        "step '{step_id}': argMap target '{native}' for `{surface}` is not a \
                         grammatical argument name: {}",
                        err.reason,
                    ),
                    Some(span),
                );
                None
            }
        }
    }

    /// Feature-collection bundling rule (M1 ruling from live daemon
    /// testing): the DeviceRail handshake hard-couples the semantic five to
    /// the UI snapshot observation channel — offering
    /// `device.semanticActions.v1` without `observation.uiSnapshot.v1`
    /// fails with `semantic_snapshot_dependency_unsatisfied`. The compiler
    /// therefore bundles the snapshot feature whenever the semantic feature
    /// is collected (04 §9.2, doc annotation pending).
    fn bundle_semantic_snapshot_dependency(&mut self) {
        let semantic = FeatureId::new(SEMANTIC_ACTIONS_FEATURE)
            .expect("the semantic-actions feature id is grammatical");
        if !self.required_features.contains(&semantic) {
            return;
        }
        let snapshot =
            FeatureId::new(UI_SNAPSHOT_FEATURE).expect("the ui-snapshot feature id is grammatical");
        if self.required_features.contains(&snapshot) {
            return;
        }
        if self.available_features.contains(&snapshot) {
            self.required_features.insert(snapshot);
        } else {
            self.error(
                codes::BIND_FEATURE_UNAVAILABLE,
                format!(
                    "'{SEMANTIC_ACTIONS_FEATURE}' requires bundling \
                     '{UI_SNAPSHOT_FEATURE}' (daemon handshake coupling, 04 §9.2), which is \
                     not in the compile-time feature set"
                ),
                None,
            );
        }
    }

    /// Statically shape-checks the literal argument parts against the
    /// action's `inputSchema` (D4). Expression-valued fields are skipped
    /// here and re-validated at runtime after evaluation (spine §7).
    /// Returns the names of (statically validated, runtime-deferred)
    /// arguments.
    fn check_args_shape(
        &mut self,
        step_id: &str,
        step_span: DiagnosticSpan,
        action_span: DiagnosticSpan,
        args: &[AttemptArg],
        action: &ActionDefinitionStatic,
    ) -> (Vec<String>, Vec<String>) {
        let schema = action.input_schema.as_value();
        let mut validated = Vec::new();
        let mut deferred = Vec::new();

        // The schema itself must compile (provider data problem otherwise).
        if let Err(err) = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(schema)
        {
            self.error(
                codes::BIND_ACTION_SCHEMA_INVALID,
                format!(
                    "action '{}': inputSchema does not compile: {err}",
                    action.name
                ),
                Some(action_span),
            );
            return (validated, deferred);
        }

        let all_names: Vec<&str> = args.iter().map(|arg| arg.name.as_str()).collect();
        for arg in args {
            if arg.dynamic {
                deferred.push(arg.name.to_string());
            }
        }

        match schema {
            serde_json::Value::Bool(true) => {
                // Everything is admissible; literal args are trivially valid.
                for arg in args {
                    if !arg.dynamic {
                        validated.push(arg.name.to_string());
                    }
                }
            }
            serde_json::Value::Bool(false) => {
                self.error(
                    codes::BIND_ARGS_SHAPE,
                    format!(
                        "step '{step_id}': action '{}' declares inputSchema `false` — no \
                         arguments are admissible",
                        action.name
                    ),
                    Some(action_span),
                );
            }
            serde_json::Value::Object(schema_object) => {
                // Required properties must be supplied by *some* argument
                // (literal or expression).
                if let Some(serde_json::Value::Array(required)) = schema_object.get("required") {
                    for name in required.iter().filter_map(serde_json::Value::as_str) {
                        if !all_names.contains(&name) {
                            self.error(
                                codes::BIND_ARGS_SHAPE,
                                format!(
                                    "step '{step_id}': required argument '{name}' of action \
                                     '{}' is missing",
                                    action.name
                                ),
                                Some(step_span),
                            );
                        }
                    }
                }
                let properties = schema_object
                    .get("properties")
                    .and_then(serde_json::Value::as_object);
                // additionalProperties: false closes the argument vocabulary.
                if schema_object.get("additionalProperties")
                    == Some(&serde_json::Value::Bool(false))
                {
                    for arg in args {
                        let known =
                            properties.is_some_and(|props| props.contains_key(arg.name.as_str()));
                        if !known {
                            self.error(
                                codes::BIND_ARGS_SHAPE,
                                format!(
                                    "step '{step_id}': argument '{}' is not accepted by action \
                                     '{}' (additionalProperties: false)",
                                    arg.name, action.name
                                ),
                                Some(arg.span),
                            );
                        }
                    }
                }
                // Literal fields validate against their property subschema.
                for arg in args {
                    if arg.dynamic {
                        continue;
                    }
                    let Some(subschema) = properties.and_then(|props| props.get(arg.name.as_str()))
                    else {
                        // No per-field schema to check against statically;
                        // the runtime whole-object validation covers it.
                        deferred.push(arg.name.to_string());
                        continue;
                    };
                    if contains_ref(subschema) {
                        // A $ref may point outside the subschema; validating
                        // it out of context would be wrong. Defer to runtime.
                        deferred.push(arg.name.to_string());
                        continue;
                    }
                    let Expr::Lit(literal) = &arg.expr else {
                        unreachable!("non-dynamic args are literals by construction");
                    };
                    match jsonschema::validate(subschema, &literal.lit) {
                        Ok(()) => validated.push(arg.name.to_string()),
                        Err(err) => self.error(
                            codes::BIND_ARGS_SHAPE,
                            format!(
                                "step '{step_id}': argument '{}' fails the inputSchema of \
                                 action '{}': {err}",
                                arg.name, action.name
                            ),
                            Some(arg.span),
                        ),
                    }
                }
            }
            _ => {
                // Non-object, non-boolean schema documents are rejected by
                // JsonSchemaDocument's constructor; unreachable via that type.
                self.error(
                    codes::BIND_ACTION_SCHEMA_INVALID,
                    format!(
                        "action '{}': inputSchema must be an object or boolean",
                        action.name
                    ),
                    Some(action_span),
                );
            }
        }
        (validated, deferred)
    }
}

/// E5 material rule (03 §1.4 rule 4): `dom` consumes `css`; `uiTree`
/// consumes the structured accessibility fields; `vision` consumes the
/// prompt (checked separately) and `coordinate` consumes no selector.
fn selector_supports_channel(selector: &ElementSelectorIR, channel: Channel) -> bool {
    match channel {
        Channel::Dom => selector.css.is_some(),
        Channel::UiTree => {
            selector.role.is_some()
                || selector.name.is_some()
                || selector.identifier.is_some()
                || selector.text.is_some()
        }
        Channel::Vision | Channel::Coordinate => true,
    }
}

/// The human-readable material requirement of a channel (E5 diagnostics).
fn material_requirement(channel: Channel) -> &'static str {
    match channel {
        Channel::Dom => "no `css` selector",
        Channel::UiTree => "none of the structured fields (role, name, identifier, text)",
        _ => "no consumable selector material",
    }
}

/// The wire token of a channel (spine A.4).
fn channel_token(channel: Channel) -> &'static str {
    match channel {
        Channel::Dom => "dom",
        Channel::UiTree => "uiTree",
        Channel::Vision => "vision",
        Channel::Coordinate => "coordinate",
    }
}

/// The YAML token of a canonical verb (spine A.4, snake_case).
fn verb_token(verb: CanonicalVerb) -> &'static str {
    match verb {
        CanonicalVerb::Tap => "tap",
        CanonicalVerb::SetValue => "set_value",
        CanonicalVerb::Clear => "clear",
        CanonicalVerb::WaitFor => "wait_for",
        CanonicalVerb::Find => "find",
        CanonicalVerb::Observe => "observe",
        CanonicalVerb::Screenshot => "screenshot",
        CanonicalVerb::Invoke => "invoke",
    }
}

/// The `ElementState` wire literal (equal to DeviceRail
/// `WaitForElementCondition` verbatim).
fn element_state_wire(state: ElementState) -> serde_json::Value {
    serde_json::to_value(state).expect("an ElementState always serializes")
}

/// The element argument's wire value per verb (04 §9.4.2/§9.5): the target
/// verbs (`tap`/`set_value`/`clear`) take a DeviceRail `ElementTarget` —
/// the compiler defaults to `kind: "selector"` — while the search verbs
/// (`find`/`wait_for`) take the bare `ElementSelector`, isomorphic
/// passthrough of [`ElementSelectorIR`].
fn element_wire_value(verb: CanonicalVerb, selector: &ElementSelectorIR) -> serde_json::Value {
    let selector_json =
        serde_json::to_value(selector).expect("an ElementSelectorIR always serializes");
    match verb {
        CanonicalVerb::Tap | CanonicalVerb::SetValue | CanonicalVerb::Clear => serde_json::json!({
            "kind": "selector",
            "selector": selector_json,
        }),
        _ => selector_json,
    }
}

/// Collects the JSON-pointer labels of every schema location carrying
/// secret semantics (`sensitive: true` / `format: "password"`, 06 §3.1).
/// Deliberately over-approximate: the walk visits every object/array in
/// the document, so the markers are found under any combinator or nesting
/// — fail-closed beats schema-keyword lawyering here.
fn collect_secret_schema_fields(schema: &serde_json::Value, pointer: &str, out: &mut Vec<String>) {
    match schema {
        serde_json::Value::Object(object) => {
            let secret = matches!(object.get("sensitive"), Some(serde_json::Value::Bool(true)))
                || matches!(
                    object.get("format"),
                    Some(serde_json::Value::String(format)) if format == "password"
                );
            if secret {
                out.push(if pointer.is_empty() {
                    "/".to_owned()
                } else {
                    pointer.to_owned()
                });
            }
            for (key, value) in object {
                collect_secret_schema_fields(value, &format!("{pointer}/{key}"), out);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_secret_schema_fields(item, &format!("{pointer}/{index}"), out);
            }
        }
        _ => {}
    }
}

/// Whether a schema subtree contains a `$ref` keyword anywhere.
fn contains_ref(schema: &serde_json::Value) -> bool {
    match schema {
        serde_json::Value::Object(object) => {
            object.contains_key("$ref")
                || object.contains_key("$dynamicRef")
                || object.values().any(contains_ref)
        }
        serde_json::Value::Array(items) => items.iter().any(contains_ref),
        _ => false,
    }
}

/// The capability names nearest to `wanted` (Levenshtein ≤ 2, capped at
/// three, closest first) — 03 §4.4's `candidates`.
fn nearest_names(wanted: &str, known: &[&str]) -> Vec<String> {
    fn distance(a: &str, b: &str) -> usize {
        let a: Vec<char> = a.chars().collect();
        let b: Vec<char> = b.chars().collect();
        let mut previous: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.iter().enumerate() {
            let mut current = vec![i + 1];
            for (j, cb) in b.iter().enumerate() {
                let substitute = previous[j] + usize::from(ca != cb);
                current.push(substitute.min(previous[j + 1] + 1).min(current[j] + 1));
            }
            previous = current;
        }
        previous[b.len()]
    }
    let mut scored: Vec<(usize, &str)> = known
        .iter()
        .map(|name| (distance(wanted, name), *name))
        .filter(|(score, _)| *score <= 2)
        .collect();
    scored.sort_unstable();
    scored
        .into_iter()
        .take(3)
        .map(|(_, name)| name.to_owned())
        .collect()
}
