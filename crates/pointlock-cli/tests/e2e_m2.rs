//! M2 end-to-end acceptance over the real binary:
//! (a) a linked closure (call step) compiles to a bundle artifact and runs
//!     against the real DeviceRail daemon (driver-mock device);
//! (b) supervised runs (R13) over the fake registration with attached
//!     collection: proceed/proceed passes, abort ends the run;
//! (c) an onFail escalate-judge handler collected interactively rules the
//!     failing step pass.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// ─── Harness ────────────────────────────────────────────────────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-cli-e2e-m2-{tag}-{}-{}",
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

fn pointlock(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pointlock"))
        .args(args)
        .output()
        .expect("spawn pointlock")
}

/// Runs the binary with the given lines piped to stdin (the attached
/// `--interactive` collection channel).
fn pointlock_with_stdin(args: &[&str], stdin_lines: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pointlock"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pointlock");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(stdin_lines.as_bytes())
        .expect("write answers");
    child.wait_with_output().expect("wait pointlock")
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

fn write_file(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write fixture");
}

// ─── (b) supervised runs over the fake registration ─────────────────────────

/// The fake-registration demo (echo semantics; two mutating steps).
fn demo_flow_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/wifi-demo.flow.yaml")
}

fn compile_fake_demo(dir: &TempDir) -> (String, String) {
    let lock = dir.file("fake.lock.json");
    let locked = pointlock(&["lock", "--out", lock.to_str().unwrap()]);
    assert_exit(&locked, 0, "lock");
    let ir = dir.file("demo.ir.json");
    let compiled = pointlock(&[
        "compile",
        demo_flow_path().to_str().unwrap(),
        "--lockfile",
        lock.to_str().unwrap(),
        "--out",
        ir.to_str().unwrap(),
    ]);
    assert_exit(&compiled, 0, "compile");
    (
        ir.to_str().unwrap().to_owned(),
        lock.to_str().unwrap().to_owned(),
    )
}

#[test]
fn supervised_run_proceeds_through_both_gates() {
    let dir = TempDir::new("supervise-proceed");
    let (ir, _) = compile_fake_demo(&dir);
    let store = dir.file("store");
    let output = pointlock_with_stdin(
        &[
            "run",
            &ir,
            "--store",
            store.to_str().unwrap(),
            "--param",
            "ssid=lab-net",
            "--run-id",
            "sup-1",
            "--supervise",
            "mutating",
            "--interactive",
        ],
        "proceed\nproceed\n",
    );
    assert_exit(&output, 0, "supervised run");
    let stdout = stdout_of(&output);
    assert!(
        stdout.matches("supervision gate").count() >= 2,
        "two mutating steps gate: {stdout}"
    );
    assert!(stdout.contains("flow verdict: pass"), "{stdout}");
}

#[test]
fn supervised_run_abort_ends_the_run() {
    let dir = TempDir::new("supervise-abort");
    let (ir, _) = compile_fake_demo(&dir);
    let store = dir.file("store");
    let output = pointlock_with_stdin(
        &[
            "run",
            &ir,
            "--store",
            store.to_str().unwrap(),
            "--param",
            "ssid=lab-net",
            "--run-id",
            "sup-abort",
            "--supervise",
            "mutating",
            "--interactive",
        ],
        "abort\n",
    );
    // An aborted run makes no semantic claim: no flow verdict.
    let stdout = stdout_of(&output);
    assert!(
        !stdout.contains("flow verdict: pass"),
        "an aborted run must not pass: {stdout}"
    );
    let inspected = pointlock(&[
        "inspect",
        "--store",
        store.to_str().unwrap(),
        "--run",
        "sup-abort",
    ]);
    assert_exit(&inspected, 0, "inspect");
    let inspect_out = stdout_of(&inspected);
    assert!(
        inspect_out.contains("finished"),
        "the aborted run reached a terminal state: {inspect_out}"
    );
}

#[test]
fn supervision_suspend_answer_leaves_the_run_suspended() {
    let dir = TempDir::new("supervise-suspend");
    let (ir, _) = compile_fake_demo(&dir);
    let store = dir.file("store");
    let output = pointlock_with_stdin(
        &[
            "run",
            &ir,
            "--store",
            store.to_str().unwrap(),
            "--param",
            "ssid=lab-net",
            "--run-id",
            "sup-susp",
            "--supervise",
            "mutating",
            "--interactive",
        ],
        "suspend\n",
    );
    assert_exit(&output, 3, "suspend answer leaves the run suspended");
    // The request survives the segment (non-final answer, spine §6.9):
    // resuming interactively re-prompts the same gate; proceed twice
    // finishes the run.
    let resumed = pointlock_with_stdin(
        &[
            "resume",
            &ir,
            "--store",
            store.to_str().unwrap(),
            "--run",
            "sup-susp",
            "--supervise",
            "mutating",
            "--interactive",
        ],
        "proceed\nproceed\n",
    );
    assert_exit(&resumed, 0, "resume after suspend");
    assert!(stdout_of(&resumed).contains("flow verdict: pass"));
}

// ─── (c) escalate judge collected interactively ─────────────────────────────

const ESCALATE_FLOW: &str = r#"flow: escalate_demo
provider: devicerail
handlers:
  on_fail:
    - escalate:
        mode: judge
        prompt: the machine judged this step fail; please rule
        decisions: [pass, fail, unknown]
        timeout_ms: 3600000
        on_timeout: unknown
      max_triggers: 1
steps:
  - id: doomed
    invoke:
      action: tapElement
      args:
        element: wifi_row
    effect: mutating
    expect:
      - assert_id: never_true
        expr: ${{ eq(1, 2) }}
"#;

#[test]
fn escalate_judge_collected_interactively_supersedes() {
    let dir = TempDir::new("escalate");
    let flow_path = dir.file("escalate.flow.yaml");
    write_file(&flow_path, ESCALATE_FLOW);
    let lock = dir.file("fake.lock.json");
    assert_exit(
        &pointlock(&["lock", "--out", lock.to_str().unwrap()]),
        0,
        "lock",
    );
    let ir = dir.file("escalate.ir.json");
    let compiled = pointlock(&[
        "compile",
        flow_path.to_str().unwrap(),
        "--lockfile",
        lock.to_str().unwrap(),
        "--out",
        ir.to_str().unwrap(),
    ]);
    assert_exit(&compiled, 0, "compile escalate flow");

    let store = dir.file("store");
    let output = pointlock_with_stdin(
        &[
            "run",
            ir.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--run-id",
            "esc-1",
            "--interactive",
        ],
        "pass\n",
    );
    assert_exit(&output, 0, "escalate ruled pass");
    let stdout = stdout_of(&output);
    assert!(stdout.contains("human step (judge)"), "{stdout}");
    assert!(stdout.contains("flow verdict: pass"), "{stdout}");
}

// ─── (a) linked closure bundle against the real daemon ──────────────────────

const BUNDLE_MAIN: &str = r#"flow: bundle_demo
provider: devicerail
steps:
  - id: warmup
    invoke:
      action: tap
      args: { x: 5, y: 5 }
    effect: mutating
    idempotent: true

  - id: probe_session
    call: ./probe.flow.yaml
    inputs: {}
"#;

const BUNDLE_CALLEE: &str = r#"flow: probe_session
provider: devicerail
steps:
  - id: probe
    invoke:
      action: scroll
      args: { deltaX: 0, deltaY: 120 }
    effect: mutating
    expect_schema:
      type: object
      properties:
        deltaY: { type: number }
    expect:
      - assert_id: delta_echoed
        expr: ${{ eq(steps.probe.output.deltaY, 120) }}
"#;

/// Locates the sibling DeviceRail checkout (same convention as e2e_m1).
fn device_rail_repo() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest_dir.join("../../../device-rail");
    match repo.canonicalize() {
        Ok(repo) if repo.join("Cargo.toml").is_file() => repo,
        _ => panic!(
            "the DeviceRail sibling checkout was not found at {}",
            repo.display()
        ),
    }
}

fn build_daemon(repo: &Path) -> PathBuf {
    let status = Command::new("cargo")
        .args(["build", "-p", "devicerail-daemon", "--quiet"])
        .current_dir(repo)
        .stdin(Stdio::null())
        .status()
        .expect("spawn cargo build for devicerail-daemon");
    assert!(status.success(), "devicerail-daemon build failed");
    let bin = repo.join("target/debug/devicerail-daemon");
    assert!(bin.is_file(), "daemon binary missing at {}", bin.display());
    bin
}

#[test]
fn bundle_compiles_and_runs_against_the_real_daemon() {
    // Serialized daemon build (cargo locks its own target dir).
    let _budget = Duration::from_secs(600);
    let repo = device_rail_repo();
    let daemon = build_daemon(&repo);
    let daemon = daemon.to_str().unwrap();

    let dir = TempDir::new("bundle");
    write_file(&dir.file("main.flow.yaml"), BUNDLE_MAIN);
    write_file(&dir.file("probe.flow.yaml"), BUNDLE_CALLEE);

    let lock = dir.file("real.lock.json");
    let locked = pointlock(&[
        "lock",
        "--provider",
        "devicerail",
        "--daemon-cmd",
        daemon,
        "--out",
        lock.to_str().unwrap(),
    ]);
    assert_exit(&locked, 0, "lock against the real daemon");

    let ir = dir.file("bundle.ir.json");
    let compiled = pointlock(&[
        "compile",
        dir.file("main.flow.yaml").to_str().unwrap(),
        "--provider",
        "devicerail",
        "--lockfile",
        lock.to_str().unwrap(),
        "--out",
        ir.to_str().unwrap(),
    ]);
    assert_exit(&compiled, 0, "compile the linked closure");

    // The artifact is a bundle: root + the compiled callee.
    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&ir).expect("read artifact"))
            .expect("artifact JSON");
    assert_eq!(artifact["pointlockBundle"], 1, "bundle discriminator");
    assert_eq!(
        artifact["subflows"].as_array().map(Vec::len),
        Some(1),
        "one compiled callee travels with the root"
    );

    let store = dir.file("store");
    let output = pointlock(&[
        "run",
        ir.to_str().unwrap(),
        "--provider",
        "devicerail",
        "--daemon-cmd",
        daemon,
        "--lockfile",
        lock.to_str().unwrap(),
        "--store",
        store.to_str().unwrap(),
        "--run-id",
        "bundle-1",
    ]);
    assert_exit(&output, 0, "bundle run against the real daemon");
    let stdout = stdout_of(&output);
    assert!(stdout.contains("flow verdict: pass"), "{stdout}");

    let inspected = pointlock(&[
        "inspect",
        "--store",
        store.to_str().unwrap(),
        "--run",
        "bundle-1",
        "--rebuild-checkpoint",
    ]);
    assert_exit(&inspected, 0, "inspect");
    let inspect_out = stdout_of(&inspected);
    assert!(inspect_out.contains("finished"), "{inspect_out}");
}

// ─── (e) webhook notify-only channel (06 §4.2) ──────────────────────────────

/// A canned webhook receiver capturing each POST's signature header and
/// body.
/// One captured POST: the `X-Pointlock-Signature` value (when sent) and
/// the raw body.
type CapturedPost = (Option<String>, String);

struct CannedWebhook {
    server: std::sync::Arc<tiny_http::Server>,
    url: String,
    received: std::sync::Arc<std::sync::Mutex<Vec<CapturedPost>>>,
}

impl CannedWebhook {
    fn start() -> CannedWebhook {
        let server = std::sync::Arc::new(
            tiny_http::Server::http("127.0.0.1:0").expect("bind canned webhook"),
        );
        let url = format!(
            "http://127.0.0.1:{}",
            server.server_addr().to_ip().expect("ip").port()
        );
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let server = std::sync::Arc::clone(&server);
            let received = std::sync::Arc::clone(&received);
            std::thread::spawn(move || {
                while let Ok(mut request) = server.recv() {
                    let signature = request
                        .headers()
                        .iter()
                        .find(|header| {
                            header.field.as_str().as_str().eq_ignore_ascii_case(
                                pointlock_human_cli::webhook::SIGNATURE_HEADER,
                            )
                        })
                        .map(|header| header.value.as_str().to_owned());
                    let mut body = String::new();
                    let _ = request.as_reader().read_to_string(&mut body);
                    received.lock().expect("lock").push((signature, body));
                    let _ = request.respond(tiny_http::Response::from_string("ok"));
                }
            });
        }
        CannedWebhook {
            server,
            url,
            received,
        }
    }
}

impl Drop for CannedWebhook {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

#[test]
fn webhook_notifies_pending_requests_on_suspension() {
    let webhook = CannedWebhook::start();
    let dir = TempDir::new("webhook");
    let (ir, _) = compile_fake_demo(&dir);
    let store = dir.file("store");
    // A supervision gate answered `suspend` leaves the run suspended with
    // the request still pending — exactly the detached scenario the
    // notify-only channel exists for.
    let mut child = Command::new(env!("CARGO_BIN_EXE_pointlock"))
        .args([
            "run",
            &ir,
            "--store",
            store.to_str().unwrap(),
            "--param",
            "ssid=lab-net",
            "--run-id",
            "hook-run",
            "--supervise",
            "mutating",
            "--interactive",
            "--webhook-url",
            &webhook.url,
        ])
        .env("POINTLOCK_WEBHOOK_SECRET", "e2e-secret")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pointlock");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(b"suspend\n")
        .expect("write answer");
    let output = child.wait_with_output().expect("wait pointlock");
    assert_exit(&output, 3, "suspend answer leaves the run suspended");
    assert!(
        stdout_of(&output).contains("webhook notified: 1 pending request(s)"),
        "{}",
        stdout_of(&output)
    );

    let received = webhook.received.lock().expect("lock");
    assert_eq!(received.len(), 1, "exactly one notification POST");
    let (signature, body) = &received[0];
    // The envelope wraps the R14 inbox projection with recovery material.
    let envelope: serde_json::Value = serde_json::from_str(body).expect("valid JSON body");
    assert_eq!(envelope["pointlockWebhook"], 1);
    assert_eq!(envelope["runId"], "hook-run");
    let entries = envelope["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["purpose"], "supervision");
    assert!(
        envelope["respondHint"]
            .as_str()
            .is_some_and(|hint| hint.contains("pointlock resume")),
    );
    // The HMAC signature verifies against the exact body bytes.
    assert_eq!(
        signature.as_deref(),
        Some(pointlock_human_cli::webhook::signature_for(body, "e2e-secret").as_str()),
        "X-Pointlock-Signature must verify"
    );
}

#[test]
fn no_webhook_flag_means_no_notification() {
    // Negative control: the same suspension without --webhook-url makes
    // no POST anywhere (and prints no webhook line).
    let dir = TempDir::new("webhook-off");
    let (ir, _) = compile_fake_demo(&dir);
    let store = dir.file("store");
    let output = pointlock_with_stdin(
        &[
            "run",
            &ir,
            "--store",
            store.to_str().unwrap(),
            "--param",
            "ssid=lab-net",
            "--run-id",
            "no-hook",
            "--supervise",
            "mutating",
            "--interactive",
        ],
        "suspend\n",
    );
    assert_exit(&output, 3, "suspend without webhook");
    assert!(!stdout_of(&output).contains("webhook notified"));
}

// ─── (f) the authoring vocabulary document (03 §4.1) ────────────────────────

#[test]
fn emit_authoring_schema_writes_the_vocabulary_document() {
    let dir = TempDir::new("authoring-schema");
    let out = dir.file("authoring.json");
    let output = pointlock(&[
        "compile",
        "--emit-authoring-schema",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_exit(&output, 0, "emit authoring schema");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("read document"))
            .expect("valid JSON");
    assert_eq!(doc["pointlockAuthoringSchema"], 1);
    assert!(
        doc["step"]["verbHeads"]
            .as_array()
            .expect("verbs")
            .iter()
            .any(|v| v == "tap")
    );
    assert!(
        doc["expression"]["functions"]
            .as_array()
            .expect("functions")
            .iter()
            .any(|f| f == "concat")
    );

    // Guards: the flag takes no flow source, and plain compile still
    // requires one.
    let both = pointlock(&[
        "compile",
        "--emit-authoring-schema",
        "demo.flow.yaml",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_exit(&both, 64, "flag plus flow source refused");
    let neither = pointlock(&["compile", "--out", out.to_str().unwrap()]);
    assert_exit(&neither, 64, "plain compile still requires a source");
}
