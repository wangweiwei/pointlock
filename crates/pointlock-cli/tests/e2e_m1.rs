//! M1 end-to-end acceptance (08 §6.2), driving the real `pointlock` binary
//! via `std::process::Command` against a real `devicerail-daemon` (built
//! from the sibling DeviceRail checkout) running the built-in mock driver
//! (`mock-1`):
//!
//! (a) `lock --provider devicerail` freezes the live world; double runs
//!     are digest-identical (04 §10.2 — the digest domain excludes the
//!     volatile `attestedAt`);
//! (b) `compile examples/mock-daemon-demo.flow.yaml --lockfile` binds the
//!     driver-native actions against `lockfile.device.actions`;
//! (c) `run --provider devicerail` passes end to end: exit 0, flow verdict
//!     pass, inspect shows `finished` and the rebuild self-check passes;
//! (d) `run --stop-after scroll_feed` → exit 3 (suspended) → `resume` →
//!     exit 0 with only the remaining step re-dispatched (alignment says
//!     `reusable`/`new`; the event-count delta corroborates);
//! (e) compiling an action the daemon does not offer is an RF4004
//!     rejection (real capability-bound compile).
//!
//! Requires a sibling checkout of the DeviceRail repository at
//! `../../../device-rail` relative to this crate (i.e. next to the
//! pointlock repository) and a working `cargo` on PATH. The daemon build
//! is deduplicated per test process and cargo's own package lock
//! serializes concurrent builds across processes.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

// ─── Harness ────────────────────────────────────────────────────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique per-test scratch directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-cli-e2e-m1-{tag}-{}-{}",
            std::process::id(),
            DIR_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn pointlock<S: AsRef<OsStr>>(args: &[S]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pointlock"))
        .args(args)
        .output()
        .expect("spawn pointlock")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_exit(output: &Output, expected: i32, context: &str) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{context}: expected exit {expected}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout_of(output),
        stderr_of(output)
    );
}

fn demo_flow_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/mock-daemon-demo.flow.yaml")
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read JSON file")).expect("JSON")
}

// ─── Daemon harness ─────────────────────────────────────────────────────────
//
// Adapted from `pointlock-provider-devicerail/tests/common/mod.rs` (the CLI
// e2e keeps its own copy rather than importing across crate test trees).

/// Budget for the nested `cargo build -p devicerail-daemon` invocation.
const DAEMON_BUILD_BUDGET: Duration = Duration::from_secs(600);

/// Locates the sibling DeviceRail checkout relative to this crate directory.
fn device_rail_repo() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest_dir.join("../../../device-rail");
    match repo.canonicalize() {
        Ok(repo) if repo.join("Cargo.toml").is_file() => repo,
        _ => panic!(
            "the DeviceRail sibling checkout was not found at {} \
             (expected `device-rail` next to the `pointlock` repository); \
             the M1 e2e needs it to build and spawn `devicerail-daemon`",
            repo.display()
        ),
    }
}

/// Builds `devicerail-daemon` in the sibling repository and returns the
/// resulting debug binary path.
fn build_daemon(repo: &Path) -> PathBuf {
    let target_dir = repo.join("target");
    let mut child = Command::new("cargo")
        .args(["build", "-p", "devicerail-daemon", "--quiet"])
        .current_dir(repo)
        // Pin the target directory so the binary path below is deterministic
        // even when the outer environment redirects CARGO_TARGET_DIR.
        .env("CARGO_TARGET_DIR", &target_dir)
        // Let the DeviceRail repository's own rust-toolchain.toml choose the
        // toolchain instead of inheriting this test invocation's pin.
        .env_remove("RUSTUP_TOOLCHAIN")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn `cargo build -p devicerail-daemon` in the DeviceRail repo");

    let deadline = Instant::now() + DAEMON_BUILD_BUDGET;
    let status = loop {
        match child.try_wait().expect("poll daemon build") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "building devicerail-daemon exceeded the {}s budget",
                    DAEMON_BUILD_BUDGET.as_secs()
                );
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    assert!(status.success(), "devicerail-daemon build failed: {status}");

    let binary = target_dir.join("debug").join("devicerail-daemon");
    assert!(
        binary.is_file(),
        "daemon binary missing after successful build: {}",
        binary.display()
    );
    binary
}

/// The daemon binary, built once per test process (concurrent tests block
/// on the `OnceLock`; concurrent processes on cargo's package lock).
fn daemon_binary() -> &'static Path {
    static DAEMON: OnceLock<PathBuf> = OnceLock::new();
    DAEMON
        .get_or_init(|| build_daemon(&device_rail_repo()))
        .as_path()
}

// ─── Shared pipeline helpers ────────────────────────────────────────────────

/// The devicerail daemon flags shared by `lock` / `run` / `resume`: the
/// built daemon binary and a test-owned evidence directory. Since M2 the
/// screenshot policy no longer needs to be pinned to `omit`: the DeviceRail
/// control plane still has no asset byte channel (`fetchEvidence` is
/// typed-unsupported), but a failed localization is a typed gap that
/// degrades the dependent verify channel toward `unknown` instead of
/// aborting the run — captured screenshots are honestly recorded as
/// absent-from-the-local-archive.
fn daemon_args(evidence_dir: &Path) -> Vec<String> {
    vec![
        "--daemon-cmd".to_owned(),
        daemon_binary().to_str().expect("utf-8 path").to_owned(),
        "--daemon-env".to_owned(),
        format!("DEVICERAIL_EVIDENCE_DIR={}", evidence_dir.display()),
    ]
}

/// `pointlock lock --provider devicerail` into `dir/name`.
fn lock_devicerail(dir: &TempDir, name: &str) -> PathBuf {
    let out = dir.file(name);
    let mut args: Vec<String> = vec![
        "lock".to_owned(),
        "--provider".to_owned(),
        "devicerail".to_owned(),
    ];
    args.extend(daemon_args(&dir.file("evidence")));
    args.extend(["--out".to_owned(), out.to_str().unwrap().to_owned()]);
    let locked = pointlock(&args);
    assert_exit(&locked, 0, "lock devicerail");
    out
}

/// `pointlock compile` of the M1 demo flow against a real lockfile.
fn compile_demo(dir: &TempDir, lockfile: &Path) -> PathBuf {
    let out = dir.file("mock-daemon-demo.flow.ir.json");
    let compiled = pointlock(&[
        "compile",
        demo_flow_path().to_str().unwrap(),
        "--provider",
        "devicerail",
        "--lockfile",
        lockfile.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_exit(&compiled, 0, "compile demo flow");
    out
}

/// `pointlock run --provider devicerail` with the shared daemon flags.
fn run_devicerail(dir: &TempDir, flow_ir: &Path, lockfile: &Path, extra: &[&str]) -> Output {
    let mut args: Vec<String> = vec![
        "run".to_owned(),
        flow_ir.to_str().unwrap().to_owned(),
        "--provider".to_owned(),
        "devicerail".to_owned(),
        "--device".to_owned(),
        "mock-1".to_owned(),
        "--lockfile".to_owned(),
        lockfile.to_str().unwrap().to_owned(),
        "--store".to_owned(),
        dir.file("store").to_str().unwrap().to_owned(),
    ];
    args.extend(daemon_args(&dir.file("evidence")));
    args.extend(extra.iter().map(|arg| (*arg).to_owned()));
    pointlock(&args)
}

/// `pointlock inspect`, returning the stdout and the parsed `events:` count.
fn inspect(dir: &TempDir, run_id: &str, rebuild: bool) -> (String, usize) {
    let mut args: Vec<String> = vec![
        "inspect".to_owned(),
        "--store".to_owned(),
        dir.file("store").to_str().unwrap().to_owned(),
        "--run".to_owned(),
        run_id.to_owned(),
    ];
    if rebuild {
        args.push("--rebuild-checkpoint".to_owned());
    }
    let output = pointlock(&args);
    assert_exit(&output, 0, "inspect");
    let stdout = stdout_of(&output);
    let events = stdout
        .lines()
        .find_map(|line| line.strip_prefix("events: "))
        .expect("inspect prints the event count")
        .trim()
        .parse()
        .expect("event count parses");
    (stdout, events)
}

// ─── (a) lock: the live world freezes deterministically ─────────────────────

#[test]
fn scenario_a_lock_freezes_the_live_world_with_a_stable_digest() {
    let dir = TempDir::new("lock");
    let first = lock_devicerail(&dir, "first.lock.json");
    let second = lock_devicerail(&dir, "second.lock.json");

    let first_json = read_json(&first);
    let second_json = read_json(&second);

    // The artifact is a real CapabilityLockfile frozen from the daemon.
    assert_eq!(first_json["provider"]["name"], "devicerail");
    assert_eq!(first_json["hello"]["protocolSelected"]["major"], 1);
    assert_eq!(first_json["hello"]["protocolSelected"]["minor"], 5);
    let digest = first_json["digest"].as_str().expect("digest");
    assert!(digest.starts_with("sha256:"), "sealed digest, got {digest}");
    assert!(
        !first_json["attestedAt"]
            .as_str()
            .expect("attestedAt")
            .is_empty(),
        "the freeze timestamp is recorded"
    );
    let actions: Vec<&str> = first_json["device"]["actions"]
        .as_array()
        .expect("actions")
        .iter()
        .map(|action| action["name"].as_str().expect("name"))
        .collect();
    for expected in ["tap", "inputText", "scroll"] {
        assert!(
            actions.contains(&expected),
            "mock driver action {expected} missing from the lockfile: {actions:?}"
        );
    }

    // 04 §10.2: two lock runs against the identical daemon freeze to the
    // same digest (the digest domain excludes the volatile attestedAt).
    assert_eq!(
        first_json["digest"], second_json["digest"],
        "double lock runs must be digest-identical"
    );
}

// ─── (b) compile: real capability binding against the lockfile ──────────────

#[test]
fn scenario_b_compile_binds_the_demo_against_the_real_capability_set() {
    let dir = TempDir::new("compile");
    let lockfile = lock_devicerail(&dir, "real.lock.json");
    let lock_digest = read_json(&lockfile)["digest"]
        .as_str()
        .expect("digest")
        .to_owned();

    let out = dir.file("mock-daemon-demo.flow.ir.json");
    let compiled = pointlock(&[
        "compile",
        demo_flow_path().to_str().unwrap(),
        "--provider",
        "devicerail",
        "--lockfile",
        lockfile.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_exit(&compiled, 0, "compile");
    let stdout = stdout_of(&compiled);
    // The binding report names the real capability source and the
    // driver-native actions each step bound to.
    assert!(
        stdout.contains("binding: actionSource=lockfileDevice"),
        "binding source: {stdout}"
    );
    for line in [
        "step tap_center -> tap @ uiTree",
        "step scroll_feed -> scroll @ uiTree",
        "step type_text -> inputText @ uiTree",
    ] {
        assert!(stdout.contains(line), "binding line `{line}` in: {stdout}");
    }

    let flow_ir = read_json(&out);
    assert_eq!(flow_ir["provider"]["name"], "devicerail");
    assert_eq!(flow_ir["flowId"], "mock_daemon_demo");
    assert_eq!(flow_ir["body"].as_array().expect("body").len(), 3);
    // The artifact is sealed against the very lockfile it was bound to.
    assert_eq!(flow_ir["lockfileDigest"], lock_digest.as_str());
}

// ─── (c) run: full pass through the real daemon ─────────────────────────────

#[test]
fn scenario_c_run_full_pass_and_inspect_self_check() {
    let dir = TempDir::new("run");
    let lockfile = lock_devicerail(&dir, "real.lock.json");
    let flow_ir = compile_demo(&dir, &lockfile);

    let ran = run_devicerail(&dir, &flow_ir, &lockfile, &["--run-id", "e2e-m1-full"]);
    assert_exit(&ran, 0, "run devicerail");
    let stdout = stdout_of(&ran);
    assert!(
        stdout.contains("step tap_center: verdict=pass"),
        "run output: {stdout}"
    );
    assert!(
        stdout.contains("step scroll_feed: unverified (executed, no assertions)"),
        "R4 unverified annotation missing: {stdout}"
    );
    assert!(
        stdout.contains("step type_text: verdict=pass"),
        "run output: {stdout}"
    );
    assert!(
        stdout.contains("flow verdict: pass"),
        "run output: {stdout}"
    );

    // Inspect: finished + the I1 rebuild self-check passes all three ways.
    let (inspected, _) = inspect(&dir, "e2e-m1-full", true);
    assert!(
        inspected.contains("status: finished"),
        "inspect output: {inspected}"
    );
    assert!(
        inspected.contains("completed steps: 3"),
        "inspect output: {inspected}"
    );
    assert!(
        inspected.contains("checkpoint self-check: PASS"),
        "self-check output: {inspected}"
    );
}

// ─── (d) stop-after → suspended → resume re-runs only the tail ──────────────

#[test]
fn scenario_d_stop_after_resume_reruns_only_the_remaining_step() {
    let dir = TempDir::new("stop-resume");
    let lockfile = lock_devicerail(&dir, "real.lock.json");
    let flow_ir = compile_demo(&dir, &lockfile);
    let run_id = "e2e-m1-stop";

    // Interrupt after the second step completes.
    let ran = run_devicerail(
        &dir,
        &flow_ir,
        &lockfile,
        &["--run-id", run_id, "--stop-after", "scroll_feed"],
    );
    assert_exit(&ran, 3, "run --stop-after");
    let stdout = stdout_of(&ran);
    assert!(
        stdout.contains("run suspended"),
        "suspension notice: {stdout}"
    );
    assert!(
        stdout.contains("step scroll_feed:"),
        "completed steps: {stdout}"
    );
    assert!(
        !stdout.contains("step type_text:"),
        "type_text must not have run: {stdout}"
    );

    let (suspended, events_at_suspension) = inspect(&dir, run_id, false);
    assert!(
        suspended.contains("status: suspended"),
        "inspect output: {suspended}"
    );

    // Resume with the same IR and lockfile: the completed steps are
    // adopted (`reusable`), only the tail dispatches.
    let mut args: Vec<String> = vec![
        "resume".to_owned(),
        flow_ir.to_str().unwrap().to_owned(),
        "--provider".to_owned(),
        "devicerail".to_owned(),
        "--lockfile".to_owned(),
        lockfile.to_str().unwrap().to_owned(),
        "--store".to_owned(),
        dir.file("store").to_str().unwrap().to_owned(),
        "--run".to_owned(),
        run_id.to_owned(),
    ];
    args.extend(daemon_args(&dir.file("evidence")));
    let resumed = pointlock(&args);
    assert_exit(&resumed, 0, "resume devicerail");
    let stdout = stdout_of(&resumed);
    assert!(
        stdout.contains("tap_center reusable"),
        "alignment: {stdout}"
    );
    assert!(
        stdout.contains("scroll_feed reusable"),
        "alignment: {stdout}"
    );
    assert!(stdout.contains("type_text new"), "alignment: {stdout}");
    assert!(
        stdout.contains("step type_text: verdict=pass"),
        "resumed step: {stdout}"
    );
    assert!(
        stdout.contains("flow verdict: pass"),
        "resume output: {stdout}"
    );

    // Inspect: finished, self-check passes, and the resumed segment
    // appended far fewer events than the two-step prefix did — the
    // ledger-level corroboration that the adopted steps did not re-run.
    let (inspected, events_after_resume) = inspect(&dir, run_id, true);
    assert!(
        inspected.contains("status: finished"),
        "inspect output: {inspected}"
    );
    assert!(
        inspected.contains("completed steps: 3"),
        "inspect output: {inspected}"
    );
    assert!(
        inspected.contains("checkpoint self-check: PASS"),
        "self-check output: {inspected}"
    );
    assert!(
        events_after_resume > events_at_suspension,
        "the resume segment appends events ({events_after_resume} vs {events_at_suspension})"
    );
    assert!(
        events_after_resume - events_at_suspension < events_at_suspension,
        "resume must only re-run the remaining step, not the whole flow \
         (suspension: {events_at_suspension} events, after resume: {events_after_resume})"
    );
}

// ─── (e) compile refuses actions the daemon does not offer (RF4004) ─────────

#[test]
fn scenario_e_compile_rejects_an_action_outside_the_daemon_capabilities() {
    let dir = TempDir::new("bad-capability");
    let lockfile = lock_devicerail(&dir, "real.lock.json");

    let bad = dir.file("bad.flow.yaml");
    std::fs::write(
        &bad,
        r#"
flow: bad_capability_demo
provider: devicerail
steps:
  - id: swipe_up
    invoke: { action: swipeElement, args: {} }
    effect: mutating
"#,
    )
    .expect("write bad yaml");

    let out = dir.file("bad.flow.ir.json");
    let compiled = pointlock(&[
        "compile",
        bad.to_str().unwrap(),
        "--provider",
        "devicerail",
        "--lockfile",
        lockfile.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_exit(&compiled, 1, "compile (rejected)");
    assert!(!out.exists(), "no artifact may be produced on rejection");

    let diagnostics: serde_json::Value =
        serde_json::from_str(&stdout_of(&compiled)).expect("stdout is a JSON diagnostics array");
    let array = diagnostics.as_array().expect("array");
    assert!(
        array.iter().any(|diag| diag["code"] == "RF4004"),
        "expected the capability-bound RF4004 rejection: {diagnostics}"
    );
    let message = array
        .iter()
        .find(|diag| diag["code"] == "RF4004")
        .and_then(|diag| diag["message"].as_str())
        .expect("message");
    assert!(
        message.contains("lockfile.device.actions"),
        "the diagnostic names the real capability source: {message}"
    );
}

// ─── typed refusals of the daemon-only surface ──────────────────────────────

#[test]
fn daemon_flags_are_typed_refusals_outside_the_devicerail_registration() {
    let dir = TempDir::new("refusals");

    // Daemon flags under the fake registration: refused, never ignored.
    let locked = pointlock(&[
        "lock",
        "--provider",
        "fake",
        "--daemon-cmd",
        "/nonexistent/devicerail-daemon",
        "--out",
        dir.file("x.lock.json").to_str().unwrap(),
    ]);
    assert_exit(&locked, 64, "lock --provider fake --daemon-cmd");
    assert!(stderr_of(&locked).contains("--daemon-cmd"));

    // A devicerail run without the attestation baseline: refused with the
    // reason, before any daemon is spawned.
    let ran = pointlock(&[
        "run",
        dir.file("missing.flow.ir.json").to_str().unwrap(),
        "--provider",
        "devicerail",
        "--store",
        dir.file("store").to_str().unwrap(),
    ]);
    assert_exit(&ran, 64, "run devicerail without --lockfile");
    assert!(stderr_of(&ran).contains("--lockfile is required"));
}
