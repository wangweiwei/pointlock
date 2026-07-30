//! Implementations of the functional commands (`lock` / `compile` / `run` /
//! `resume` / `inspect`) over both registrations: the M0 fake and the M1
//! real DeviceRail provider. See the [`crate`] docs for the exit-code table
//! and the scope.

use std::path::Path;
use std::sync::atomic::AtomicUsize;

use pointlock_compiler::{CompileDiagnostic, CompileOptions, compile as compile_flow};
use pointlock_ir::{
    AlignmentReport, FlowIR, RunLogPayload, StepRecord, Verdict, VerdictStatus, render_run_path,
};
use pointlock_ir::{Hash, SupervisePolicy};
use pointlock_provider_devicerail::{DeviceRailProvider, devicerail_manifest, lock_via_spawn};
use pointlock_provider_kit::lockfile::CapabilityLockfile;
use pointlock_provider_kit::{
    CancellationToken, OpenSessionOptions, Provider, ProviderError, ProviderSession,
};
use pointlock_runner::{ResumeOptions, RunOptions, RunOutcome, Runner, RunnerError};
use pointlock_store::{Store, StoreError};
use serde_json::Value;

use crate::assembly::{
    DEFAULT_DEVICE_ID, DEVICERAIL_DEFAULT_DEVICE_ID, DEVICERAIL_REGISTRATION, DeviceRailAssembly,
    Registration, StopAfterPlan, StopAfterSession, assemble_fake, registration,
};
use crate::{
    Failure, LockCliArgs, OutputFormat, ResumeCliArgs, RunCliArgs, SuperviseArg, VisionArg, exit,
};

// ─── Shared helpers ─────────────────────────────────────────────────────────

fn io_failure(path: &Path, err: impl std::fmt::Display) -> Failure {
    Failure::new(exit::INTERNAL, format!("{}: {err}", path.display()))
}

pub(crate) fn store_failure(err: StoreError) -> Failure {
    Failure::new(exit::INTERNAL, format!("store error: {err}"))
}

pub(crate) fn usage_failure(message: impl Into<String>) -> Failure {
    Failure::new(exit::NOT_IN_M0, message)
}

fn read_to_string(path: &Path) -> Result<String, Failure> {
    std::fs::read_to_string(path).map_err(|err| io_failure(path, err))
}

/// A compiled artifact on disk: a bare `FlowIR`, or a bundle carrying the
/// linked subflow closure (`{"pointlockBundle": 1, "root", "subflows"}`).
struct LoadedArtifact {
    flow: FlowIR,
    subflows: std::collections::BTreeMap<pointlock_ir::Hash, FlowIR>,
}

fn load_artifact(path: &Path) -> Result<LoadedArtifact, Failure> {
    let raw = read_to_string(path)?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|err| io_failure(path, err))?;
    if value.get("pointlockBundle").is_some() {
        let root = value
            .get("root")
            .cloned()
            .ok_or_else(|| io_failure(path, "bundle missing 'root'"))?;
        let flow: FlowIR = serde_json::from_value(root).map_err(|err| io_failure(path, err))?;
        let mut subflows = std::collections::BTreeMap::new();
        for entry in value
            .get("subflows")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let callee: FlowIR =
                serde_json::from_value(entry).map_err(|err| io_failure(path, err))?;
            subflows.insert(callee.ir_hash.clone(), callee);
        }
        Ok(LoadedArtifact { flow, subflows })
    } else {
        let flow: FlowIR = serde_json::from_value(value).map_err(|err| io_failure(path, err))?;
        Ok(LoadedArtifact {
            flow,
            subflows: std::collections::BTreeMap::new(),
        })
    }
}

/// Artifact loading for the serve module's directory scan: the root flow
/// plus the bundle's subflow closure (best-effort — non-artifact JSON is
/// the caller's skip signal).
pub(crate) fn load_artifact_for_serve(path: &Path) -> Result<(FlowIR, Vec<FlowIR>), Failure> {
    let loaded = load_artifact(path)?;
    Ok((loaded.flow, loaded.subflows.into_values().collect()))
}

fn load_flow_ir(path: &Path) -> Result<FlowIR, Failure> {
    serde_json::from_str(&read_to_string(path)?)
        .map_err(|err| io_failure(path, format!("not a valid FlowIR artifact: {err}")))
}

pub(crate) fn load_lockfile(path: &Path) -> Result<CapabilityLockfile, Failure> {
    serde_json::from_str(&read_to_string(path)?)
        .map_err(|err| io_failure(path, format!("not a valid CapabilityLockfile: {err}")))
}

/// Writes pretty JSON with a trailing newline.
fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), Failure> {
    let mut body = serde_json::to_string_pretty(value)
        .map_err(|err| Failure::new(exit::INTERNAL, format!("serialization error: {err}")))?;
    body.push('\n');
    std::fs::write(path, body).map_err(|err| io_failure(path, err))
}

/// The camelCase wire literal of a unit-variant enum, for human output.
pub(crate) fn wire_str<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "?".to_owned())
}

/// A current-thread runtime is sufficient: the store is a synchronous
/// single writer and the SPI is the only async surface (runner module docs).
fn runtime() -> Result<tokio::runtime::Runtime, Failure> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| Failure::new(exit::INTERNAL, format!("tokio runtime: {err}")))
}

fn registration_or_fail(name: &str) -> Result<Registration, Failure> {
    registration(name).map_err(usage_failure)
}

fn provider_failure(err: ProviderError) -> Failure {
    Failure::new(exit::INTERNAL, format!("provider: {err}"))
}

/// Typed refusal of devicerail-only flags under another registration
/// (fail-closed, never silently ignored). Each entry is `(flag, given)`.
fn refuse_daemon_flags(registration_name: &str, flags: &[(&str, bool)]) -> Result<(), Failure> {
    let offending: Vec<&str> = flags
        .iter()
        .filter(|(_, given)| *given)
        .map(|(name, _)| *name)
        .collect();
    if offending.is_empty() {
        return Ok(());
    }
    Err(usage_failure(format!(
        "{} only appl{} to `--provider {DEVICERAIL_REGISTRATION}`; the '{registration_name}' \
         registration has no daemon",
        offending.join(", "),
        if offending.len() == 1 { "ies" } else { "y" },
    )))
}

/// Parses repeated `--daemon-env KEY=VALUE` items (values verbatim).
fn parse_env_items(items: &[String]) -> Result<Vec<(String, String)>, Failure> {
    items
        .iter()
        .map(|item| {
            item.split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .ok_or_else(|| {
                    usage_failure(format!(
                        "--daemon-env '{item}' is not of the form KEY=VALUE"
                    ))
                })
        })
        .collect()
}

/// The devicerail run/resume `--lockfile` requirement (a usage guard,
/// checked before any I/O).
fn require_lockfile_flag(lockfile_path: Option<&Path>) -> Result<&Path, Failure> {
    lockfile_path.ok_or_else(|| {
        usage_failure(format!(
            "--lockfile is required with --provider {DEVICERAIL_REGISTRATION}: run-time \
             attestation compares the live world against the artifact `pointlock lock` wrote"
        ))
    })
}

/// Loads the devicerail run/resume prerequisites: the held lockfile
/// (attestation baseline — the artifact `pointlock lock` wrote), the
/// provider constructed around it, its `env.platform` string, and the
/// spawn assembly from the daemon flags.
fn devicerail_run_assembly(
    lockfile_path: Option<&Path>,
    daemon_cmd: Option<&Path>,
    daemon_env: &[String],
) -> Result<(DeviceRailProvider, String, DeviceRailAssembly), Failure> {
    let lockfile_path = require_lockfile_flag(lockfile_path)?;
    let lockfile = load_lockfile(lockfile_path)?;
    let platform = wire_str(&lockfile.device.platform);
    let provider = DeviceRailProvider::new(lockfile).map_err(provider_failure)?;
    let assembly = DeviceRailAssembly::new(daemon_cmd, parse_env_items(daemon_env)?)
        .map_err(|err| Failure::new(exit::INTERNAL, err))?;
    Ok((provider, platform, assembly))
}

/// Builds the `--stop-after` plan: cancel the cooperative stop token once
/// the named step's dispatch completes (see [`StopAfterPlan`] for the
/// dispatch-order determinism argument).
fn stop_after_plan(
    flow: &FlowIR,
    stop_after: Option<&str>,
    stop: &CancellationToken,
) -> Result<Option<StopAfterPlan>, Failure> {
    let Some(step_id) = stop_after else {
        return Ok(None);
    };
    let index = flow
        .body
        .iter()
        .position(|step| step.step_id().as_str() == step_id)
        .ok_or_else(|| {
            usage_failure(format!(
                "--stop-after '{step_id}' does not name a step of flow '{}'",
                flow.flow_id
            ))
        })?;
    // Dispatch order == body order in the executable subset (single
    // attempt, no retry; every demo dispatch succeeds), so the
    // (index+1)-th dispatch is exactly this step's.
    Ok(Some(StopAfterPlan {
        token: stop.clone(),
        remaining: AtomicUsize::new(index + 1),
    }))
}

/// Parses repeated `--param key=value` items. The value is parsed as a
/// JSON literal when possible (`true`, `42`, `"quoted"`, `{...}`), else
/// taken verbatim as a string — so `--param ssid=HomeWifi` binds a string.
/// Maps the CLI flag value to the runner policy (R13).
/// Builds the `--vision` verifier. `anthropic` without `ANTHROPIC_API_KEY`
/// is a usage error, not a silent degradation — an unconfigured verifier
/// would be indistinguishable from `off` in the ledger (principle 4).
pub(crate) fn vision_verifier(
    arg: VisionArg,
) -> Result<Option<std::sync::Arc<dyn pointlock_vision::VisionVerifier>>, Failure> {
    match arg {
        VisionArg::Off => Ok(None),
        VisionArg::Anthropic => match pointlock_vision::AnthropicVisionVerifier::from_env() {
            Some(verifier) => Ok(Some(std::sync::Arc::new(verifier))),
            None => Err(usage_failure(
                "--vision anthropic requires ANTHROPIC_API_KEY in the environment",
            )),
        },
    }
}

fn supervise_policy(arg: SuperviseArg) -> SupervisePolicy {
    match arg {
        SuperviseArg::Mutating => SupervisePolicy::Mutating,
        SuperviseArg::All => SupervisePolicy::All,
    }
}

/// Wall-clock ms for human response arbitration timestamps.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Attached collection of one pending request (06 §4 cli channel):
/// prompt on this terminal, submit through the store arbitration, and
/// report whether the answer was a supervision `suspend` (which leaves the
/// run suspended — the loop must stop re-prompting).
fn collect_interactive(store: &mut Store, run_id: &str, request_id: &str) -> Result<bool, Failure> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut writer = std::io::stdout();
    let (_, response) = pointlock_human_cli::collect(
        store,
        run_id,
        request_id,
        &pointlock_human_cli::cli_actor(),
        now_ms(),
        &mut reader,
        &mut writer,
    )
    .map_err(|err| Failure::new(exit::NOT_IN_M0, format!("human collection: {err}")))?;
    Ok(response.get("decision").and_then(Value::as_str) == Some("suspend"))
}

/// One resume segment over an already-loaded artifact (shared by
/// `pointlock resume` and the attached-collection loop of `pointlock run`).
#[allow(clippy::too_many_arguments)]
fn resume_segment(
    reg: Registration,
    lockfile: Option<&Path>,
    daemon_cmd: Option<&Path>,
    daemon_env: &[String],
    flow: &FlowIR,
    subflows: &std::collections::BTreeMap<Hash, FlowIR>,
    run_id: &str,
    device_id: &str,
    store: &mut Store,
    supervise: Option<SupervisePolicy>,
    vision: Option<std::sync::Arc<dyn pointlock_vision::VisionVerifier>>,
    old_flow_ir: Option<FlowIR>,
    allow_mutating_reexec: Vec<String>,
    force_reexecute: Vec<String>,
) -> Result<Result<RunOutcome, RunnerError>, Failure> {
    let required_features: Vec<_> = flow.required_features.iter().cloned().collect();
    Ok(match reg {
        Registration::Fake => {
            let assembly = assemble_fake();
            runtime()?.block_on(async {
                let session = assembly
                    .open_session(device_id, required_features, None)
                    .await?;
                let opts = ResumeOptions {
                    stop: CancellationToken::new(),
                    platform: Some(assembly.platform()),
                    old_flow_ir,
                    supervise,
                    vision,
                    allow_mutating_reexec: allow_mutating_reexec.clone(),
                    force_reexecute: force_reexecute.clone(),
                    ..ResumeOptions::default()
                };
                Runner::resume_with_subflows(flow, subflows, run_id, session, store, opts).await
            })
        }
        Registration::DeviceRail => {
            let (provider, platform, assembly) =
                devicerail_run_assembly(lockfile, daemon_cmd, daemon_env)?;
            runtime()?.block_on(async {
                let session = provider
                    .open_session(OpenSessionOptions {
                        endpoint: assembly.endpoint(),
                        device_id: device_id.to_owned(),
                        required_features,
                        lockfile_digest: flow.lockfile_digest.clone(),
                    })
                    .await?;
                let opts = ResumeOptions {
                    stop: CancellationToken::new(),
                    platform: Some(platform),
                    old_flow_ir,
                    supervise,
                    vision,
                    allow_mutating_reexec: allow_mutating_reexec.clone(),
                    force_reexecute: force_reexecute.clone(),
                    ..ResumeOptions::default()
                };
                Runner::resume_with_subflows(flow, subflows, run_id, session, store, opts).await
            })
        }
    })
}

fn parse_params(items: &[String]) -> Result<serde_json::Map<String, Value>, Failure> {
    let mut map = serde_json::Map::new();
    for item in items {
        let Some((key, raw)) = item.split_once('=') else {
            return Err(usage_failure(format!(
                "--param '{item}' is not of the form KEY=VALUE"
            )));
        };
        let value =
            serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_owned()));
        map.insert(key.to_owned(), value);
    }
    Ok(map)
}

// ─── lock ───────────────────────────────────────────────────────────────────

/// `pointlock lock`: freeze the registered provider's capabilities into a
/// sealed [`CapabilityLockfile`] JSON file. The fake registration
/// synthesizes the handshake from the manifest (see [`crate::assembly`]);
/// the devicerail registration spawns the real daemon and replays the wire
/// sequence (`lock_via_spawn`, 04 §10.2).
pub fn lock(args: &LockCliArgs) -> Result<i32, Failure> {
    let lockfile = match registration_or_fail(&args.provider)? {
        Registration::Fake => {
            refuse_daemon_flags(
                &args.provider,
                &[
                    ("--daemon-cmd", args.daemon_cmd.is_some()),
                    ("--daemon-env", !args.daemon_env.is_empty()),
                    ("--device", args.device.is_some()),
                ],
            )?;
            assemble_fake().lockfile().clone()
        }
        Registration::DeviceRail => {
            let assembly = DeviceRailAssembly::new(
                args.daemon_cmd.as_deref(),
                parse_env_items(&args.daemon_env)?,
            )
            .map_err(|err| Failure::new(exit::INTERNAL, err))?;
            let device = args
                .device
                .clone()
                .unwrap_or_else(|| DEVICERAIL_DEFAULT_DEVICE_ID.to_owned());
            // `attestedAt` is the honest wall clock; the digest domain
            // excludes it, so double lock runs against an identical
            // daemon still freeze to the same digest (04 §10.2).
            runtime()?
                .block_on(lock_via_spawn(assembly.spawn_spec(), &device))
                .map_err(provider_failure)?
        }
    };
    write_json(&args.out, &lockfile)?;
    println!(
        "locked: {} {} (registration: {})",
        lockfile.provider.name, lockfile.provider.version, args.provider
    );
    println!(
        "protocol: {}.{}",
        lockfile.hello.protocol_selected.major, lockfile.hello.protocol_selected.minor
    );
    println!("platform: {}", wire_str(&lockfile.device.platform));
    println!("featuresEnabled: {}", lockfile.hello.features_enabled.len());
    println!("actions: {}", lockfile.device.actions.len());
    println!("digest: {}", lockfile.digest);
    println!("wrote: {}", args.out.display());
    Ok(exit::PASS)
}

// ─── compile ────────────────────────────────────────────────────────────────

/// `pointlock compile`: five-phase compile; on success writes the sealed
/// FlowIR JSON and prints the binding-report summary (03 §4.5 confirmation
/// point 3); on rejection prints the diagnostics (`--format json` emits the
/// `CompileDiagnostic[]` array of 03 §4.4 on stdout for the LLM repair
/// loop) and exits 1.
/// `pointlock compile --emit-authoring-schema`: writes the authoring
/// vocabulary document (03 §4.1, generated from the compiler's closed
/// tables — see `pointlock_compiler::authoring_schema`).
pub fn emit_authoring_schema(out: &Path) -> Result<i32, Failure> {
    write_json(out, &pointlock_compiler::authoring_schema())?;
    println!("authoring schema written to {}", out.display());
    Ok(exit::PASS)
}

pub fn compile(
    flow_path: &Path,
    lockfile_path: Option<&Path>,
    registration_name: &str,
    out: &Path,
    format: OutputFormat,
) -> Result<i32, Failure> {
    // Declaration precedes execution: both registrations compile from a
    // static manifest (plus the lockfile when given) with no daemon online.
    let fake = match registration_or_fail(registration_name)? {
        Registration::Fake => Some(assemble_fake()),
        Registration::DeviceRail => None,
    };
    let manifest = match &fake {
        Some(assembly) => assembly.manifest(),
        None => devicerail_manifest(),
    };
    let source = read_to_string(flow_path)?;
    let lockfile = lockfile_path.map(load_lockfile).transpose()?;
    let source_name = flow_path.display().to_string();
    let options = CompileOptions {
        source_name: &source_name,
        manifest,
        lockfile: lockfile.as_ref(),
    };
    match compile_flow(&source, &options) {
        Ok(sealed) => {
            if sealed.subflow_irs.is_empty() {
                write_json(out, &sealed.flow_ir)?;
            } else {
                // A linked closure persists as a bundle: the root plus
                // every compiled callee, content-keyed. The runner's
                // subflow registry loads from exactly this artifact.
                let bundle = serde_json::json!({
                    "pointlockBundle": 1,
                    "root": &sealed.flow_ir,
                    "subflows": sealed.subflow_irs.values().collect::<Vec<_>>(),
                });
                write_json(out, &bundle)?;
            }
            let flow = &sealed.flow_ir;
            let report = &sealed.binding_report;
            println!("compiled: {source_name} -> {}", out.display());
            println!("flow: {} ({} steps)", flow.flow_id, flow.body.len());
            println!("irHash: {}", flow.ir_hash);
            println!("lockfileDigest: {}", flow.lockfile_digest);
            let features: Vec<&str> = report
                .required_features
                .iter()
                .map(|feature| feature.as_str())
                .collect();
            println!(
                "binding: actionSource={} requiredFeatures=[{}]",
                wire_str(&report.action_source),
                features.join(", ")
            );
            for step in &report.steps {
                println!(
                    "  step {} -> {} @ {} (static: [{}]; runtime: [{}])",
                    step.step_id,
                    step.action_name,
                    wire_str(&step.channel),
                    step.statically_validated_args.join(", "),
                    step.runtime_deferred_args.join(", "),
                );
            }
            Ok(exit::PASS)
        }
        Err(diagnostics) => {
            print_diagnostics(&diagnostics, &source, format)?;
            Ok(exit::FAIL)
        }
    }
}

fn print_diagnostics(
    diagnostics: &[CompileDiagnostic],
    source: &str,
    format: OutputFormat,
) -> Result<(), Failure> {
    match format {
        OutputFormat::Json => {
            // The parsable diagnostics array goes to stdout by contract.
            let body = serde_json::to_string_pretty(diagnostics)
                .map_err(|err| Failure::new(exit::INTERNAL, format!("serialization: {err}")))?;
            println!("{body}");
        }
        OutputFormat::Text => {
            // The 03 §4.4 human-readable format: headline, locator, the
            // offending source line with a caret run, candidates and hint.
            eprintln!("compile rejected with {} diagnostic(s):", diagnostics.len());
            eprint!("{}", pointlock_compiler::render_pretty(diagnostics, source));
        }
    }
    Ok(())
}

// ─── run ────────────────────────────────────────────────────────────────────

/// `pointlock run`: load the sealed FlowIR (the runner recomputes and
/// verifies `irHash` at load), assemble the registered provider, open a
/// session, and execute. `--stop-after <step-id>` cancels the cooperative
/// stop token once that step's dispatch completes, so the run suspends at
/// the next step boundary.
pub fn run(args: &RunCliArgs) -> Result<i32, Failure> {
    let reg = registration_or_fail(&args.provider)?;
    // Usage guards fire before any I/O (fail-closed, and a missing input
    // file must not mask a surface misuse).
    match reg {
        Registration::Fake => refuse_daemon_flags(
            &args.provider,
            &[
                ("--daemon-cmd", args.daemon_cmd.is_some()),
                ("--daemon-env", !args.daemon_env.is_empty()),
                ("--lockfile", args.lockfile.is_some()),
            ],
        )?,
        Registration::DeviceRail => {
            require_lockfile_flag(args.lockfile.as_deref())?;
        }
    }
    // Part of the pre-I/O usage guards: a missing ANTHROPIC_API_KEY must
    // surface before any artifact is read.
    let vision = vision_verifier(args.vision)?;
    let artifact = load_artifact(&args.flow_ir)?;
    let flow = artifact.flow;
    let subflows = artifact.subflows;
    let params = parse_params(&args.params)?;
    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let stop = CancellationToken::new();
    let stop_after = stop_after_plan(&flow, args.stop_after.as_deref(), &stop)?;

    let mut store = Store::open(&args.store).map_err(store_failure)?;
    println!("run: {run_id}");
    println!("flow: {} ({})", flow.flow_id, flow.ir_hash);

    let required_features: Vec<_> = flow.required_features.iter().cloned().collect();
    let mut resume_hint = format!(
        "pointlock resume {} --store {} --run {run_id}",
        args.flow_ir.display(),
        args.store.display()
    );
    if args.vision == VisionArg::Anthropic {
        // Following the printed hint must not silently drop the verifier
        // for the next segment (per-segment semantics, as --supervise).
        resume_hint.push_str(" --vision anthropic");
    }
    let outcome = match reg {
        Registration::Fake => {
            let assembly = assemble_fake();
            let device = args
                .device
                .clone()
                .unwrap_or_else(|| DEFAULT_DEVICE_ID.to_owned());
            runtime()?.block_on(async {
                let session = assembly
                    .open_session(&device, required_features, stop_after)
                    .await?;
                let mut opts = RunOptions::new(device.clone());
                opts.run_id = Some(run_id.clone());
                opts.stop = stop;
                opts.platform = Some(assembly.platform());
                opts.subflows = subflows.clone();
                opts.supervise = args.supervise.map(supervise_policy);
                opts.vision = vision.clone();
                Runner::run(&flow, Value::Object(params), session, &mut store, opts).await
            })
        }
        Registration::DeviceRail => {
            let (provider, platform, assembly) = devicerail_run_assembly(
                args.lockfile.as_deref(),
                args.daemon_cmd.as_deref(),
                &args.daemon_env,
            )?;
            let device = args
                .device
                .clone()
                .unwrap_or_else(|| DEVICERAIL_DEFAULT_DEVICE_ID.to_owned());
            resume_hint.push_str(&format!(
                " --provider {DEVICERAIL_REGISTRATION} --lockfile {}",
                args.lockfile.as_deref().expect("checked above").display()
            ));
            if let Some(daemon_cmd) = &args.daemon_cmd {
                resume_hint.push_str(&format!(" --daemon-cmd {}", daemon_cmd.display()));
            }
            runtime()?.block_on(async {
                let session = provider
                    .open_session(OpenSessionOptions {
                        endpoint: assembly.endpoint(),
                        device_id: device.clone(),
                        required_features,
                        lockfile_digest: flow.lockfile_digest.clone(),
                    })
                    .await?;
                let session: Box<dyn ProviderSession> = match stop_after {
                    Some(plan) => Box::new(StopAfterSession::new(session, plan)),
                    None => session,
                };
                let mut opts = RunOptions::new(device.clone());
                opts.run_id = Some(run_id.clone());
                opts.stop = stop;
                opts.platform = Some(platform);
                opts.subflows = subflows.clone();
                opts.supervise = args.supervise.map(supervise_policy);
                opts.vision = vision.clone();
                Runner::run(&flow, Value::Object(params), session, &mut store, opts).await
            })
        }
    };

    let mut outcome = outcome;
    if args.interactive {
        // Attached collection (06 §4): prompt, arbitrate, and resume in
        // the same process until the run leaves the human-waiting state
        // (a supervision `suspend` answer deliberately stays suspended).
        while let Ok(RunOutcome::AwaitingHuman { pending }) = &outcome {
            let suspended = collect_interactive(&mut store, &run_id, &pending.request_id)?;
            if suspended {
                break;
            }
            let device_for_resume = match registration_or_fail(&args.provider)? {
                Registration::Fake => args
                    .device
                    .clone()
                    .unwrap_or_else(|| DEFAULT_DEVICE_ID.to_owned()),
                Registration::DeviceRail => args
                    .device
                    .clone()
                    .unwrap_or_else(|| DEVICERAIL_DEFAULT_DEVICE_ID.to_owned()),
            };
            outcome = resume_segment(
                registration_or_fail(&args.provider)?,
                args.lockfile.as_deref(),
                args.daemon_cmd.as_deref(),
                &args.daemon_env,
                &flow,
                &subflows,
                &run_id,
                &device_for_resume,
                &mut store,
                args.supervise.map(supervise_policy),
                vision.clone(),
                None,
                // same-IR continuation of a fresh run: no cross-IR gate applies
                Vec::new(),
                Vec::new(),
            )?;
        }
    }
    conclude(
        outcome,
        &store,
        &run_id,
        &resume_hint,
        &args.store,
        args.webhook_url.as_deref(),
    )
}

// ─── resume ─────────────────────────────────────────────────────────────────

/// `pointlock resume`: checkpoint-driven resume. Alignment reads the
/// archived execution-time per-step hashes from the checkpoint (harvested
/// from `stepEntered`, spine §6.1 M1 note), so resuming under a changed IR
/// needs no `--old-ir`; when supplied, it is verified against the
/// checkpoint's `irHash`. The device binding is hard: it comes from the
/// checkpoint, never from a flag.
pub fn resume(args: &ResumeCliArgs) -> Result<i32, Failure> {
    let reg = registration_or_fail(&args.provider)?;
    // Usage guards fire before any I/O, as in `run`.
    match reg {
        Registration::Fake => refuse_daemon_flags(
            &args.provider,
            &[
                ("--daemon-cmd", args.daemon_cmd.is_some()),
                ("--daemon-env", !args.daemon_env.is_empty()),
                ("--lockfile", args.lockfile.is_some()),
            ],
        )?,
        Registration::DeviceRail => {
            require_lockfile_flag(args.lockfile.as_deref())?;
        }
    }
    // Pre-I/O usage guard, as on `run`.
    let vision = vision_verifier(args.vision)?;
    let artifact = load_artifact(&args.flow_ir)?;
    let flow = artifact.flow;
    let subflows = artifact.subflows;
    let old_flow_ir = args.old_ir.as_deref().map(load_flow_ir).transpose()?;
    let mut store = Store::open(&args.store).map_err(store_failure)?;
    let view = store.rebuild_checkpoint(&args.run).map_err(store_failure)?;
    let device_id = view.binding.device_id.clone();

    println!("run: {}", args.run);
    println!("flow: {} ({})", flow.flow_id, flow.ir_hash);

    // The R13 approval gate's CLI form (08 §2.7): rehearse the alignment
    // and stop — no session, no daemon, no events. Approval IS the rerun
    // without the flag (the response entry point stays `resume`, 06 §4.2).
    if args.preview {
        if args.interactive || !args.allow_mutating_reexec.is_empty() {
            return Err(usage_failure(
                "--preview is read-only: it collects nothing (--interactive) and releases \
                 nothing (--allow-mutating-reexec); rerun without --preview to approve",
            ));
        }
        let platform = match reg {
            Registration::DeviceRail => args
                .lockfile
                .as_deref()
                .map(load_lockfile)
                .transpose()?
                .map(|lockfile| wire_str(&lockfile.device.platform)),
            Registration::Fake => None,
        };
        let report = runtime()?
            .block_on(Runner::align_preview(
                &flow,
                &subflows,
                &args.run,
                &store,
                platform.as_deref(),
                vision.as_deref(),
                &args.force_reexecute,
                old_flow_ir.as_ref(),
            ))
            .map_err(|err| match err {
                RunnerError::M0Unsupported { .. } => usage_failure(format!("{err}")),
                other => Failure::new(exit::INTERNAL, format!("align preview: {other}")),
            })?;
        print_alignment(&report);
        for gated in &report.requires_confirmation {
            println!(
                "would require confirmation ({}): {} — {}",
                gated.cause,
                render_run_path(&gated.run_path),
                gated.reason
            );
        }
        println!("preview only — nothing executed; approve by rerunning without --preview");
        return Ok(exit::PASS);
    }

    let mut outcome = resume_segment(
        reg,
        args.lockfile.as_deref(),
        args.daemon_cmd.as_deref(),
        &args.daemon_env,
        &flow,
        &subflows,
        &args.run,
        &device_id,
        &mut store,
        args.supervise.map(supervise_policy),
        vision.clone(),
        old_flow_ir,
        args.allow_mutating_reexec.clone(),
        args.force_reexecute.clone(),
    )?;
    if args.interactive {
        while let Ok(RunOutcome::AwaitingHuman { pending }) = &outcome {
            let suspended = collect_interactive(&mut store, &args.run, &pending.request_id)?;
            if suspended {
                break;
            }
            outcome = resume_segment(
                registration_or_fail(&args.provider)?,
                args.lockfile.as_deref(),
                args.daemon_cmd.as_deref(),
                &args.daemon_env,
                &flow,
                &subflows,
                &args.run,
                &device_id,
                &mut store,
                args.supervise.map(supervise_policy),
                vision.clone(),
                None,
                // one authorization covers the whole resume invocation,
                // human wave included; the forced upgrade was consumed by
                // the first segment's alignment and repeats harmlessly
                args.allow_mutating_reexec.clone(),
                args.force_reexecute.clone(),
            )?;
        }
    }

    // The alignment report of this segment (recorded in `runResumed`).
    // A resume refused before its segment header (RequiresConfirmation)
    // appended nothing — its report is printed from the error instead.
    if outcome.is_ok()
        && let Ok(events) = store.events(&args.run)
        && let Some(report) = events.iter().rev().find_map(|event| match &event.payload {
            RunLogPayload::RunResumed {
                alignment_report, ..
            } => Some(alignment_report.clone()),
            _ => None,
        })
    {
        print_alignment(&report);
    }

    let mut resume_hint = format!(
        "pointlock resume {} --store {} --run {}",
        args.flow_ir.display(),
        args.store.display(),
        args.run
    );
    if args.vision == VisionArg::Anthropic {
        resume_hint.push_str(" --vision anthropic");
    }
    conclude(
        outcome,
        &store,
        &args.run,
        &resume_hint,
        &args.store,
        args.webhook_url.as_deref(),
    )
}

// ─── inspect ────────────────────────────────────────────────────────────────

/// `pointlock inspect`: run status, event count, checkpoint summary, and —
/// with `--rebuild-checkpoint` — the I1 rebuild self-check (materialized ==
/// rebuilt, verified three ways: head-seq match, view equality, status
/// equality).
pub fn inspect(store_dir: &Path, run_id: &str, rebuild_checkpoint: bool) -> Result<i32, Failure> {
    let store = Store::open(store_dir).map_err(store_failure)?;
    let status = store.run_status(run_id).map_err(store_failure)?;
    let events = store.events(run_id).map_err(store_failure)?;
    println!("run: {run_id}");
    println!("status: {}", status.as_str());
    println!("events: {}", events.len());
    match store
        .materialized_checkpoint(run_id)
        .map_err(store_failure)?
    {
        None => println!("checkpoint: none (no events appended yet)"),
        Some((log_seq, view)) => {
            println!("checkpoint @ seq {log_seq}");
            println!("  completed steps: {}", view.completed.len());
            for record in &view.completed {
                println!("    {}", step_line(record));
            }
            let pending_intent = view
                .frontier
                .pending_intent
                .as_ref()
                .map(|intent| intent.call_id.as_str())
                .unwrap_or("none");
            println!(
                "  frontier: {} state={} pendingIntent={}",
                render_run_path(&view.frontier.run_path),
                wire_str(&view.frontier.state),
                pending_intent
            );
            match &view.human_pending {
                None => println!("  humanPending: none"),
                Some(pending) => println!(
                    "  humanPending: {} ({}) — {}",
                    pending.request_id,
                    wire_str(&pending.purpose),
                    pending.prompt
                ),
            }
        }
    }
    if rebuild_checkpoint {
        match store.verify_checkpoint(run_id) {
            Ok(_) => println!(
                "checkpoint self-check: PASS (head-seq match, view equality, status equality)"
            ),
            Err(err) => {
                let failed_check = match &err {
                    StoreError::StaleCheckpoint { .. } => "head-seq match",
                    StoreError::CheckpointMismatch { .. } => "view equality",
                    StoreError::StatusMismatch { .. } => "status equality",
                    _ => "self-check",
                };
                return Err(Failure::new(
                    exit::INTERNAL,
                    format!("checkpoint self-check FAILED ({failed_check}): {err}"),
                ));
            }
        }
    }
    Ok(exit::PASS)
}

// ─── locate ─────────────────────────────────────────────────────────────────

/// `pointlock locate`: the adjudicable step dossier (`StepDossierView`,
/// spine §9 rule 3). One query layer shared with every renderer (spine
/// §10.2): the JSON output IS the projection DTO, byte-identical to what
/// the UI inspector consumes (08 §2.5).
pub fn locate(
    store_dir: &Path,
    run_id: &str,
    step: &str,
    flow_ir: Option<&Path>,
    format: OutputFormat,
) -> Result<i32, Failure> {
    let store = Store::open(store_dir).map_err(store_failure)?;
    let artifacts: Vec<FlowIR> = match flow_ir {
        None => Vec::new(),
        Some(path) => {
            let loaded = load_artifact(path)?;
            std::iter::once(loaded.flow)
                .chain(loaded.subflows.into_values())
                .collect()
        }
    };
    let locate_failure = |err: StoreError| match err {
        StoreError::UnknownRun(_)
        | StoreError::UnknownStepInstance { .. }
        | StoreError::AmbiguousStep { .. }
        | StoreError::BadRunPath { .. } => usage_failure(err.to_string()),
        other => store_failure(other),
    };
    let path =
        pointlock_store::projection::locate_step(&store, run_id, step).map_err(locate_failure)?;
    let dossier = pointlock_store::projection::step_dossier(&store, run_id, &path, &artifacts)
        .map_err(locate_failure)?;

    match format {
        OutputFormat::Json => {
            let body = serde_json::to_string_pretty(&dossier)
                .map_err(|err| Failure::new(exit::INTERNAL, format!("serialize dossier: {err}")))?;
            println!("{body}");
        }
        OutputFormat::Text => {
            println!("step: {} @ {}", dossier.step_id, dossier.run_path);
            println!(
                "hashes: effect={} judge={}",
                dossier.effect_hash, dossier.judge_hash
            );
            if let Some(state) = dossier.state {
                println!("state: {}", wire_str(&state));
            }
            match &dossier.source {
                Some(location) => println!(
                    "source: {} @ {}:{}",
                    location.entry.file,
                    location.entry.span.start_line,
                    location.entry.span.start_col
                ),
                None => println!("source: unavailable (pass --flow-ir for IR node + YAML span)"),
            }
            println!("attempts: {}", dossier.attempts.len());
            for attempt in &dossier.attempts {
                println!(
                    "  #{} callId={} outcome={}{}",
                    attempt.n.map_or_else(|| "?".to_owned(), |n| n.to_string()),
                    attempt.call_id,
                    attempt.outcome.as_deref().unwrap_or("unsettled"),
                    attempt
                        .error
                        .as_ref()
                        .map(|error| format!(" error={} ({})", error.code, error.message))
                        .unwrap_or_default(),
                );
            }
            println!("observations: {}", dossier.observations.len());
            println!("assertions: {}", dossier.assertion_outcomes.len());
            for outcome in &dossier.assertion_outcomes {
                println!(
                    "  {}: {} — {}",
                    outcome.assert_id,
                    wire_str(&outcome.result),
                    outcome.reason
                );
            }
            match &dossier.verdict {
                Some(verdict) => println!(
                    "verdict: {}{}",
                    wire_str(&verdict.status),
                    if verdict.degraded { " [degraded]" } else { "" }
                ),
                None => println!("verdict: none"),
            }
            println!("evidence: {}", dossier.evidence.len());
        }
    }
    Ok(exit::PASS)
}

// ─── outcome rendering ──────────────────────────────────────────────────────

/// Renders the outcome of a run/resume: per-step verdict lines (from the
/// rebuilt checkpoint), the flow verdict, and the exit code per the crate
/// table. `Suspended`/`Blocked` and a confirmation-gated resume map to
/// [`exit::SUSPENDED`].
/// The webhook notify-only channel (06 §4.2): POSTs the run's pending
/// inbox entries (the R14 `HumanInboxEntry` projection, wrapped in the
/// `pointlockWebhook: 1` envelope) to the configured URL on suspension.
/// Best-effort by design — the channel is a ledger bystander: delivery
/// failure never changes the outcome or exit code, it is reported on
/// stderr, and the next suspension re-notifies (receivers deduplicate
/// by `requestId`). The signature secret comes from
/// `POINTLOCK_WEBHOOK_SECRET` (env, never argv).
fn notify_webhook(store: &Store, store_dir: &Path, run_id: &str, url: Option<&str>) {
    let Some(url) = url else { return };
    let entries = match pointlock_store::projection::run_inbox(store, run_id) {
        Ok(entries) if !entries.is_empty() => entries,
        Ok(_) => return,
        Err(error) => {
            eprintln!("webhook notify skipped (inbox projection failed): {error}");
            return;
        }
    };
    let secret = std::env::var("POINTLOCK_WEBHOOK_SECRET").ok();
    let notification = pointlock_human_cli::webhook::build_notification(
        &entries,
        &store_dir.display().to_string(),
        run_id,
        secret.as_deref(),
    );
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("webhook notify failed (client): {error}");
            return;
        }
    };
    let mut request = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(notification.body);
    if let Some(signature) = &notification.signature {
        request = request.header(
            pointlock_human_cli::webhook::SIGNATURE_HEADER,
            signature.clone(),
        );
    }
    match request.send() {
        Ok(response) if response.status().is_success() => {
            println!("webhook notified: {} pending request(s)", entries.len());
        }
        Ok(response) => eprintln!("webhook notify failed: HTTP {}", response.status()),
        Err(error) => eprintln!("webhook notify failed: {error}"),
    }
}

fn conclude(
    outcome: Result<RunOutcome, RunnerError>,
    store: &Store,
    run_id: &str,
    resume_hint: &str,
    store_dir: &Path,
    webhook_url: Option<&str>,
) -> Result<i32, Failure> {
    match outcome {
        Ok(RunOutcome::Finished { verdict }) => {
            print_step_lines(store, run_id)?;
            match &verdict {
                Some(verdict) => println!(
                    "flow verdict: {}{} — {}",
                    wire_str(&verdict.status),
                    if verdict.degraded { " [degraded]" } else { "" },
                    verdict.summary
                ),
                // R4: no assertions ⇒ no verdict; the run finished without
                // a semantic claim (reports annotate `unverified`).
                None => println!("flow verdict: none (finished unverified or aborted)"),
            }
            Ok(verdict_exit(verdict.as_ref()))
        }
        Ok(RunOutcome::Suspended) => {
            print_step_lines(store, run_id)?;
            println!("run suspended at a step boundary");
            println!("resume with: {resume_hint}");
            notify_webhook(store, store_dir, run_id, webhook_url);
            Ok(exit::SUSPENDED)
        }
        Ok(RunOutcome::Blocked { reason }) => {
            print_step_lines(store, run_id)?;
            println!("run blocked awaiting a human decision: {reason}");
            notify_webhook(store, store_dir, run_id, webhook_url);
            Ok(exit::SUSPENDED)
        }
        Ok(RunOutcome::AwaitingHuman { pending }) => {
            // Minimal surfacing only: the attached-TTY collection flow is
            // the CLI human wave, not this subset.
            print_step_lines(store, run_id)?;
            println!(
                "run awaiting a human response (requestId {}): {}",
                pending.request_id, pending.prompt
            );
            println!("resume with: {resume_hint}");
            notify_webhook(store, store_dir, run_id, webhook_url);
            Ok(exit::SUSPENDED)
        }
        Err(RunnerError::RequiresConfirmation { report }) => {
            print_alignment(&report);
            for gated in &report.requires_confirmation {
                eprintln!(
                    "requires confirmation ({}): {} — {}",
                    gated.cause,
                    render_run_path(&gated.run_path),
                    gated.reason
                );
            }
            // Hand back the exact command that clears these entries. The
            // ids come from the gated run paths, so a mistyped `--allow`
            // shows up as a step still listed here.
            let ids: Vec<String> = report
                .requires_confirmation
                .iter()
                .filter_map(|gated| {
                    // The report names the id since the stepId incorporation; older
                    // ledgers only carry the path, so it stays derivable.
                    gated
                        .step_id
                        .as_ref()
                        .map(|id| id.as_str().to_owned())
                        .or_else(|| step_id_of(&gated.run_path))
                })
                .map(|id| format!("--allow-mutating-reexec {id}"))
                .collect();
            if !ids.is_empty() {
                eprintln!(
                    "\nreview each step above, then re-run this resume with:\n  {}",
                    ids.join(" ")
                );
            }
            Err(Failure::new(
                exit::SUSPENDED,
                "resume requires explicit confirmation for mutating re-execution (07 §5.4)",
            ))
        }
        Err(err) => Err(Failure::new(exit::INTERNAL, format!("runner: {err}"))),
    }
}

fn verdict_exit(verdict: Option<&Verdict>) -> i32 {
    match verdict {
        None => exit::PASS,
        Some(verdict) => match verdict.status {
            VerdictStatus::Pass => exit::PASS,
            VerdictStatus::Fail => exit::FAIL,
            VerdictStatus::Unknown => exit::UNKNOWN,
        },
    }
}

fn print_step_lines(store: &Store, run_id: &str) -> Result<(), Failure> {
    let view = store.rebuild_checkpoint(run_id).map_err(store_failure)?;
    for record in &view.completed {
        println!("{}", step_line(record));
    }
    Ok(())
}

/// One human-readable line per completed step record: verdict when judged,
/// `unverified` for executed-but-unasserted steps (R4), `blocked` for steps
/// that never executed (halt-on-fail tail).
fn step_line(record: &StepRecord) -> String {
    let verdict = match &record.verdict {
        Some(verdict) => format!(
            "verdict={}{}",
            wire_str(&verdict.status),
            if verdict.degraded { " [degraded]" } else { "" }
        ),
        None if !record.attempts.is_empty() => "unverified (executed, no assertions)".to_owned(),
        None => "blocked (not executed)".to_owned(),
    };
    format!("step {}: {verdict}", record.step_id)
}

/// The step id a gated run path names — what `--allow-mutating-reexec`
/// takes. The last `Step` frame is the gated step itself; deeper frames
/// (attempt, phase) qualify it rather than rename it.
///
/// Compatibility fallback only: `RequiresConfirmation.stepId` carries the
/// id directly since its incorporation, and pre-incorporation ledgers are
/// the sole reason this derivation survives.
fn step_id_of(path: &[pointlock_ir::PathFrame]) -> Option<String> {
    path.iter().rev().find_map(|frame| match frame {
        pointlock_ir::PathFrame::Step { step_id } => Some(step_id.as_str().to_owned()),
        // A call step's own frame IS a Call frame, so a gated call would
        // otherwise yield no id at all and print no authorization hint.
        pointlock_ir::PathFrame::Call { step_id, .. } => {
            step_id.as_ref().map(|id| id.as_str().to_owned())
        }
        _ => None,
    })
}

fn print_alignment(report: &AlignmentReport) {
    println!("alignment:");
    for entry in &report.entries {
        match &entry.reason {
            Some(reason) => println!("  {} {} — {reason}", entry.step_id, wire_str(&entry.class)),
            None => println!("  {} {}", entry.step_id, wire_str(&entry.class)),
        }
    }
    if let Some(resume_point) = &report.resume_point {
        println!("  resume point: {}", render_run_path(resume_point));
    }
}
