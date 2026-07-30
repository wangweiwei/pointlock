//! `pointlock` — the assembly-layer CLI (spine A.2: `lock` | `compile` |
//! `run` | `resume` | `inspect` | `locate` | `report`).
//!
//! ## M0 scope (iron rule: the command skeleton and argument surfaces are
//! final; content may be narrow, nothing silent)
//!
//! - `lock` / `compile` / `run` / `resume` / `inspect` are functional over
//!   the M0 subset with the [`FakeProvider`] registered as the v0.1
//!   provider name `"devicerail"` (see [`assembly`]).
//! - `locate` graduated in M3a-W1 (the `StepDossierView` projection) and
//!   `report` in the M3a close (the run report, 08 §6.2/§6.3); their M0
//!   argument surfaces carried over unchanged per spine A.2.
//! - `--supervise <mutating|all>` graduated to a functional flag in M2.
//!
//! ## M1 addition: the real DeviceRail registration
//!
//! `--provider devicerail` assembles the real DeviceRail provider over a
//! spawned `devicerail-daemon`: `lock` replays the wire handshake and
//! freezes the live world (`lock_via_spawn`), `compile` binds against the
//! DeviceRail manifest plus that real lockfile, and `run`/`resume` open a
//! `DeviceRailProvider` session (spawn endpoint from `--daemon-cmd` /
//! `--daemon-env`, attestation against `--lockfile`). The fake
//! registration keeps its M0 behavior verbatim. Flags that only make sense
//! for the daemon-backed registration are typed refusals under `fake`.
//!
//! ## Exit codes (spine A.2, incorporated at M1)
//!
//! | code | meaning |
//! |---|---|
//! | 0 | success; for `run`/`resume`: flow verdict `pass`, or a finished run with no verdict (unverified) |
//! | 1 | flow verdict `fail`; for `compile`: rejected with diagnostics |
//! | 2 | flow verdict `unknown` |
//! | 3 | run suspended (stop token), blocked on a human, or a resume refused pending explicit confirmation |
//! | 64 | usage error / typed "not in M0 subset" refusal |
//! | 70 | infrastructure or internal error (I/O, store, provider) |
//!
//! [`FakeProvider`]: pointlock_provider_kit::FakeProvider

mod assembly;
mod commands;
mod report;
mod serve;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// Process exit codes (the spine A.2 table).
pub mod exit {
    /// Success / flow verdict `pass` (or finished unverified).
    pub const PASS: i32 = 0;
    /// Flow verdict `fail`; compile rejected with diagnostics.
    pub const FAIL: i32 = 1;
    /// Flow verdict `unknown`.
    pub const UNKNOWN: i32 = 2;
    /// Run suspended / blocked / resume awaiting explicit confirmation.
    pub const SUSPENDED: i32 = 3;
    /// Usage error or typed "not in M0 subset" refusal.
    pub const NOT_IN_M0: i32 = 64;
    /// Infrastructure or internal error.
    pub const INTERNAL: i32 = 70;
}

/// A command failure: a message for stderr plus the process exit code.
pub struct Failure {
    /// Exit code (see [`exit`]).
    pub code: i32,
    /// Human-readable message.
    pub message: String,
}

impl Failure {
    /// Builds a failure.
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Failure {
            code,
            message: message.into(),
        }
    }
}

/// Output format of machine-facing command output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable text.
    Text,
    /// JSON (e.g. the `CompileDiagnostic[]` array consumed by the LLM
    /// repair loop, 03 §4.4).
    Json,
}

/// The `--supervise` value surface (R13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SuperviseArg {
    /// Gate every mutating step.
    Mutating,
    /// Gate every action step.
    All,
}

/// The `--vision` value surface: which verifier answers `vision`
/// verify-chain tails (spine §6.3; principle 7 — verify only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VisionArg {
    /// No verifier: vision tails degrade honestly to `unknown`.
    Off,
    /// The Anthropic-backed verifier. Requires `ANTHROPIC_API_KEY`;
    /// `POINTLOCK_VISION_MODEL` and `ANTHROPIC_BASE_URL` override the
    /// model and endpoint.
    Anthropic,
}

#[derive(Parser)]
#[command(
    name = "pointlock",
    version,
    about = "Pointlock assembly-layer CLI (spine A.2: the seven commands)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Freeze the registered provider's capabilities into a lockfile.
    Lock(LockArgs),
    /// Compile a *.flow.yaml source into a sealed FlowIR artifact.
    Compile(CompileArgs),
    /// Run a sealed FlowIR against the registered provider.
    Run(RunArgs),
    /// Resume a suspended run from its checkpoint.
    Resume(ResumeArgs),
    /// Inspect a run's status, ledger and checkpoint.
    Inspect(InspectArgs),
    /// Produce the adjudicable step dossier (StepDossierView).
    Locate(LocateArgs),
    /// Produce the run report (08 §6.2/§6.3).
    Report(ReportArgs),
}

/// `pointlock lock`: freeze the registered provider's capabilities into a
/// sealed `CapabilityLockfile` JSON file. The fake registration
/// synthesizes the handshake from the manifest; the devicerail
/// registration spawns the real daemon and replays the wire sequence
/// (04 §10.2).
#[derive(clap::Args)]
struct LockArgs {
    /// Provider registration to lock (`fake` or `devicerail`).
    #[arg(long, default_value = assembly::FAKE_REGISTRATION)]
    provider: String,
    /// Output path of the lockfile JSON.
    #[arg(long)]
    out: PathBuf,
    /// Daemon executable to spawn (devicerail registration; default
    /// `devicerail-daemon`, PATH-resolved).
    #[arg(long, value_name = "PATH")]
    daemon_cmd: Option<PathBuf>,
    /// Extra daemon environment, `KEY=VALUE` (devicerail registration;
    /// `DEVICERAIL_*` pass-through). Defaults inject a fresh evidence
    /// tempdir and `DEVICERAIL_ANDROID=off`; explicit entries override.
    /// Repeatable.
    #[arg(long = "daemon-env", value_name = "KEY=VALUE")]
    daemon_env: Vec<String>,
    /// Device whose capabilities to freeze (devicerail registration;
    /// default `mock-1`, the daemon's built-in mock driver).
    #[arg(long)]
    device: Option<String>,
}

/// `pointlock compile`: the five-phase compiler over the registered
/// provider's manifest (and lockfile, when given).
#[derive(clap::Args)]
struct CompileArgs {
    /// The *.flow.yaml source file (not with `--emit-authoring-schema`).
    flow: Option<PathBuf>,
    /// Emit the authoring-surface vocabulary document instead of
    /// compiling (03 §4.1): generated from the compiler's closed keyword
    /// tables, written to `--out` (`pointlockAuthoringSchema: 1`). The NL
    /// drafter consumes it as prompt context.
    #[arg(long)]
    emit_authoring_schema: bool,
    /// Capability lockfile to bind against; without one, binding falls
    /// back to `manifest.knownActions` restricted to guaranteed features
    /// (spine §4.1) and the artifact cannot attest at runtime.
    #[arg(long)]
    lockfile: Option<PathBuf>,
    /// Provider registration supplying the manifest.
    #[arg(long, default_value = assembly::FAKE_REGISTRATION)]
    provider: String,
    /// Output path of the sealed FlowIR JSON.
    #[arg(long)]
    out: PathBuf,
    /// Diagnostics format on rejection: `text` (stderr) or `json` (stdout,
    /// the `CompileDiagnostic[]` array of 03 §4.4).
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

/// `pointlock run`: load FlowIR (hash self-check happens in the runner),
/// assemble the provider, open a session, execute.
#[derive(clap::Args)]
struct RunArgs {
    /// The sealed FlowIR JSON artifact.
    flow_ir: PathBuf,
    /// Store directory (RunLog + checkpoint + evidence).
    #[arg(long)]
    store: PathBuf,
    /// Provider registration to assemble.
    #[arg(long, default_value = assembly::FAKE_REGISTRATION)]
    provider: String,
    /// Run parameter, `key=value`; the value is parsed as JSON when it is a
    /// JSON literal, otherwise taken as a string. Repeatable.
    #[arg(long = "param", value_name = "KEY=VALUE")]
    params: Vec<String>,
    /// Device to bind (default: `fake-device-1` under the fake
    /// registration, `mock-1` under devicerail).
    #[arg(long)]
    device: Option<String>,
    /// Capability lockfile the FlowIR was compiled against (devicerail
    /// registration; run-time attestation compares the live world against
    /// the artifact `pointlock lock` wrote).
    #[arg(long)]
    lockfile: Option<PathBuf>,
    /// Daemon executable to spawn (devicerail registration; default
    /// `devicerail-daemon`, PATH-resolved).
    #[arg(long, value_name = "PATH")]
    daemon_cmd: Option<PathBuf>,
    /// Extra daemon environment, `KEY=VALUE` (devicerail registration;
    /// `DEVICERAIL_*` pass-through). Repeatable.
    #[arg(long = "daemon-env", value_name = "KEY=VALUE")]
    daemon_env: Vec<String>,
    /// Explicit run id (a UUIDv4 is generated when absent).
    #[arg(long)]
    run_id: Option<String>,
    /// Trigger the cooperative stop token after the named step completes —
    /// the run suspends at the following step boundary (the controlled
    /// interruption for e2e and demos, both registrations).
    #[arg(long, value_name = "STEP_ID")]
    stop_after: Option<String>,
    /// Supervision policy (R13): gate mutating (or all) action steps on
    /// an explicit human proceed before their intent is written.
    #[arg(long, value_enum)]
    supervise: Option<SuperviseArg>,
    /// Vision verifier for `vision` verify-chain tails (default `off`:
    /// tails degrade to `unknown`).
    #[arg(long, value_enum, default_value_t = VisionArg::Off)]
    vision: VisionArg,
    /// Attached collection (06 §4 cli channel): on AwaitingHuman, prompt
    /// on this terminal, submit the answer, and continue in-process.
    #[arg(long)]
    interactive: bool,
    /// Notify-only webhook channel (06 §4.2): on suspension, POST the
    /// pending human requests' inbox projection to this URL. Signature
    /// secret via POINTLOCK_WEBHOOK_SECRET (never argv). Responses never
    /// come back this way — respond via pointlock-human-cli.
    #[arg(long, value_name = "URL")]
    webhook_url: Option<String>,
}

/// `pointlock resume`: checkpoint-driven resume. The device binding is hard
/// (taken from the checkpoint, never a flag).
#[derive(clap::Args)]
struct ResumeArgs {
    /// The (possibly repaired) FlowIR JSON artifact to resume under.
    flow_ir: PathBuf,
    /// Store directory of the suspended run.
    #[arg(long)]
    store: PathBuf,
    /// The run to resume.
    #[arg(long)]
    run: String,
    /// Provider registration to assemble.
    #[arg(long, default_value = assembly::FAKE_REGISTRATION)]
    provider: String,
    /// Capability lockfile the FlowIR was compiled against (devicerail
    /// registration). The device binding stays hard — it comes from the
    /// checkpoint, never a flag.
    #[arg(long)]
    lockfile: Option<PathBuf>,
    /// Daemon executable to spawn (devicerail registration; default
    /// `devicerail-daemon`, PATH-resolved).
    #[arg(long, value_name = "PATH")]
    daemon_cmd: Option<PathBuf>,
    /// Extra daemon environment, `KEY=VALUE` (devicerail registration;
    /// `DEVICERAIL_*` pass-through). Repeatable.
    #[arg(long = "daemon-env", value_name = "KEY=VALUE")]
    daemon_env: Vec<String>,
    /// The FlowIR the run originally executed — optional. Alignment reads
    /// the archived per-step hashes from the checkpoint; when supplied,
    /// this IR is verified against the checkpoint's irHash
    /// (`ResumeOptions::old_flow_ir`).
    #[arg(long, value_name = "FLOW_IR")]
    old_ir: Option<PathBuf>,
    /// Supervision policy (R13) for this segment (per segment, never
    /// inherited — spine §6.9).
    #[arg(long, value_enum)]
    supervise: Option<SuperviseArg>,
    /// Vision verifier for this segment's `vision` verify-chain tails
    /// (per segment, as `--supervise`; default `off`).
    #[arg(long, value_enum, default_value_t = VisionArg::Off)]
    vision: VisionArg,
    /// Authorize one gated step for mutating re-execution (07 §5.4).
    ///
    /// Resume refuses by default when re-execution would repeat an
    /// already-effective, non-idempotent mutating action, and prints the
    /// gated step ids. Name each one you have reviewed; repeat the flag
    /// per step. There is no wildcard, and the authorization applies to
    /// this resume only — the next one gates again. An id that matches no
    /// gated step is an error, not a no-op.
    ///
    /// Releasing the gate does not skip the world check: the step still
    /// evaluates its `preflight` on entry, which is what meets the residue
    /// of the earlier effect.
    #[arg(long = "allow-mutating-reexec", value_name = "STEP_ID")]
    allow_mutating_reexec: Vec<String>,
    /// Force one step back to execution on a cross-IR resume (07 §5.3).
    ///
    /// The named step classifies `effectDirty` regardless of its hashes:
    /// the resume point rolls back to it and it re-runs against the live
    /// world. The escape hatch for an offline re-judge stuck at `unknown`
    /// on missing archive material, or an adopted result you no longer
    /// trust. Repeat the flag per step; classification only — a forced
    /// step that is mutating and already effective still gates and needs
    /// `--allow-mutating-reexec` besides.
    #[arg(long = "force-reexecute", value_name = "STEP_ID")]
    force_reexecute: Vec<String>,
    /// Attached collection (06 §4 cli channel), as on `run`.
    #[arg(long)]
    interactive: bool,
    /// Notify-only webhook channel (06 §4.2), as on `run`.
    #[arg(long, value_name = "URL")]
    webhook_url: Option<String>,
    /// Read-only approval-gate preview (R13's CLI gate, 08 §2.7): print
    /// the alignment report this resume WOULD produce — classes, resume
    /// point, confirmation-gated steps — and execute nothing. Review it
    /// beside the proposed patch diff; approve by rerunning without the
    /// flag.
    #[arg(long)]
    preview: bool,
}

/// `pointlock inspect`: run status, event count, checkpoint summary, and
/// the I1 rebuild self-check.
#[derive(clap::Args)]
struct InspectArgs {
    /// Store directory.
    #[arg(long)]
    store: PathBuf,
    /// The run to inspect (required unless `--serve`).
    #[arg(long)]
    run: Option<String>,
    /// Run the checkpoint rebuild self-check (materialized == rebuilt,
    /// three-way: head seq, view equality, status equality).
    #[arg(long)]
    rebuild_checkpoint: bool,
    /// Serve the projection protocol over loopback HTTP+JSON instead of
    /// printing a one-shot summary (spine §10.4 canonical form: the five
    /// DTO families, SSE `{revision}` invalidation, evidence bytes).
    #[arg(long)]
    serve: bool,
    /// Compile-artifact directory the host scans for FlowIR files (flow
    /// list + dossier IR nodes); `--serve` only.
    #[arg(long)]
    artifacts: Option<PathBuf>,
    /// Built `@pointlock/ui` dist directory to serve at `/` (the SPA
    /// shell rides token-free; the data plane stays gated); `--serve`
    /// only.
    #[arg(long)]
    ui: Option<PathBuf>,
    /// The CURRENT capability lockfile: the flow list compares each
    /// artifact's embedded digest against it and flags staleness
    /// (08 §2.2 「不一致标黄」); `--serve` only.
    #[arg(long)]
    lockfile: Option<PathBuf>,
    /// Port to bind on 127.0.0.1 (0 = ephemeral); `--serve` only.
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Vision verifier the host's repair surfaces use (`--serve` only):
    /// align-preview re-judges vision tails with it, and the self-exec
    /// resume forwards it — so the UI-triggered closure matches a
    /// terminal `pointlock resume --vision ...` (default `off`).
    #[arg(long, value_enum, default_value_t = VisionArg::Off)]
    vision: VisionArg,
}

/// `pointlock locate` — the adjudicable step dossier (`StepDossierView`,
/// spine §9 rule 3 / §10.1): IR node + YAML span + the full run record.
#[derive(clap::Args)]
struct LocateArgs {
    /// Store directory.
    #[arg(long)]
    store: PathBuf,
    /// The run to locate within.
    #[arg(long)]
    run: String,
    /// The step whose dossier to produce: a canonical run-path string
    /// (spine §9) or a bare step id (must be unique in the run).
    #[arg(long)]
    step: String,
    /// The FlowIR artifact (or bundle) the run executed; supplies the IR
    /// node + YAML source span parts of the dossier. Without it the
    /// dossier carries the run record only (the ledger cannot resolve IR).
    #[arg(long)]
    flow_ir: Option<PathBuf>,
    /// Output format (`json` feeds the LLM repair loop, spine §9.4).
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

/// `pointlock report` — the run report with `unverified` annotations and
/// degraded summary (08 §6.2; delivered in the M3a close), on the argument
/// surface reserved by spine A.2 since M0.
#[derive(clap::Args)]
struct ReportArgs {
    /// Store directory.
    #[arg(long)]
    store: PathBuf,
    /// The run to report on.
    #[arg(long)]
    run: String,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // Help/version are successful surfaces; anything else is a
            // usage error (64) — clap's default exit code 2 would collide
            // with the `unknown` verdict code.
            use clap::error::ErrorKind;
            let code = match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => exit::PASS,
                _ => exit::NOT_IN_M0,
            };
            let _ = err.print();
            std::process::exit(code);
        }
    };

    let result = match cli.command {
        Command::Lock(args) => commands::lock(&args),
        Command::Compile(args) => match (args.emit_authoring_schema, &args.flow) {
            (true, None) => commands::emit_authoring_schema(&args.out),
            (true, Some(_)) => Err(Failure::new(
                exit::NOT_IN_M0,
                "--emit-authoring-schema takes no flow source; drop the positional argument"
                    .to_owned(),
            )),
            (false, Some(flow)) => commands::compile(
                flow,
                args.lockfile.as_deref(),
                &args.provider,
                &args.out,
                args.format,
            ),
            (false, None) => Err(Failure::new(
                exit::NOT_IN_M0,
                "a *.flow.yaml source is required (or use --emit-authoring-schema)".to_owned(),
            )),
        },
        Command::Run(args) => commands::run(&args),
        Command::Resume(args) => commands::resume(&args),
        Command::Inspect(args) => {
            if args.serve {
                if args.run.is_some() || args.rebuild_checkpoint {
                    Err(Failure::new(
                        exit::NOT_IN_M0,
                        "--serve is a mode of its own; drop --run/--rebuild-checkpoint".to_owned(),
                    ))
                } else {
                    serve::serve(serve::ServeConfig {
                        store_dir: args.store,
                        artifacts_dir: args.artifacts,
                        port: args.port,
                        ui_dir: args.ui,
                        lockfile_path: args.lockfile,
                        vision: args.vision,
                    })
                }
            } else if args.artifacts.is_some()
                || args.port != 0
                || args.ui.is_some()
                || args.lockfile.is_some()
                || args.vision != VisionArg::Off
            {
                Err(Failure::new(
                    exit::NOT_IN_M0,
                    "--artifacts/--ui/--port/--lockfile/--vision only apply to --serve".to_owned(),
                ))
            } else {
                match args.run {
                    Some(run) => commands::inspect(&args.store, &run, args.rebuild_checkpoint),
                    None => Err(Failure::new(
                        exit::NOT_IN_M0,
                        "--run is required (or use --serve)".to_owned(),
                    )),
                }
            }
        }
        Command::Locate(args) => commands::locate(
            &args.store,
            &args.run,
            &args.step,
            args.flow_ir.as_deref(),
            args.format,
        ),
        Command::Report(args) => report::report(&args.store, &args.run, args.format),
    };

    match result {
        Ok(code) => std::process::exit(code),
        Err(failure) => {
            eprintln!("pointlock: {}", failure.message);
            std::process::exit(failure.code);
        }
    }
}

// Re-exported for the command implementations.
pub(crate) use {LockArgs as LockCliArgs, ResumeArgs as ResumeCliArgs, RunArgs as RunCliArgs};
