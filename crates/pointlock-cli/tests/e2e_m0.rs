//! M0 end-to-end acceptance (08 §6.1), driving the real `pointlock` binary
//! via `std::process::Command` (`CARGO_BIN_EXE_pointlock`):
//!
//! (a) lock → compile `examples/wifi-demo.flow.yaml` → run: exit 0, flow
//!     verdict pass;
//! (b) run `--stop-after set_ssid` → exit 3 (suspended) → resume → exit 0,
//!     and inspect shows `finished` with the rebuild self-check passing;
//! (c) compile of a broken YAML: non-zero exit and `--format json` prints
//!     a parsable `CompileDiagnostic[]` array;
//! plus the typed "not in M0" surfaces (locate / report, exit 64;
//! `--supervise` graduated to a functional flag in M2-W4).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

// ─── Harness ────────────────────────────────────────────────────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique per-test scratch directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-cli-e2e-{tag}-{}-{}",
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

fn demo_flow_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/wifi-demo.flow.yaml")
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

/// `lock` + `compile` into `dir`, returning the FlowIR artifact path.
fn lock_and_compile(dir: &TempDir) -> PathBuf {
    let lockfile = dir.file("devicerail.lock.json");
    let out = dir.file("wifi-demo.flow.ir.json");

    let locked = pointlock(&[
        "lock",
        "--provider",
        "fake",
        "--out",
        lockfile.to_str().unwrap(),
    ]);
    assert_exit(&locked, 0, "lock");
    let lock_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&lockfile).expect("read lockfile"))
            .expect("lockfile is JSON");
    // The M0 provider-registration ruling: the fake is locked under the
    // v0.1 provider name.
    assert_eq!(lock_json["provider"]["name"], "devicerail");
    assert_eq!(lock_json["hello"]["protocolSelected"]["minor"], 5);

    let compiled = pointlock(&[
        "compile",
        demo_flow_path().to_str().unwrap(),
        "--lockfile",
        lockfile.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_exit(&compiled, 0, "compile");
    let stdout = stdout_of(&compiled);
    assert!(
        stdout.contains("irHash: sha256:"),
        "compile summary: {stdout}"
    );

    let flow_ir: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("read FlowIR")).expect("JSON");
    assert_eq!(flow_ir["provider"]["name"], "devicerail");
    assert_eq!(flow_ir["flowId"], "wifi_demo");
    assert_eq!(flow_ir["body"].as_array().expect("body").len(), 3);
    out
}

// ─── (a) lock → compile → run passes end to end ────────────────────────────

#[test]
fn scenario_a_lock_compile_run_full_pass() {
    let dir = TempDir::new("full-pass");
    let flow_ir = lock_and_compile(&dir);
    let store = dir.file("store");

    let ran = pointlock(&[
        "run",
        flow_ir.to_str().unwrap(),
        "--store",
        store.to_str().unwrap(),
        "--param",
        "ssid=HomeWifi",
    ]);
    assert_exit(&ran, 0, "run");
    let stdout = stdout_of(&ran);
    assert!(
        stdout.contains("step read_back: verdict=pass"),
        "run output: {stdout}"
    );
    assert!(
        stdout.contains("step set_ssid: unverified (executed, no assertions)"),
        "R4 unverified annotation missing: {stdout}"
    );
    assert!(
        stdout.contains("flow verdict: pass"),
        "run output: {stdout}"
    );
}

// ─── (a2) determinism: recompilation is hash-identical ──────────────────────

#[test]
fn scenario_a2_recompile_is_hash_identical() {
    let dir = TempDir::new("determinism");
    let first = lock_and_compile(&dir);
    let first_bytes = std::fs::read_to_string(&first).expect("read first artifact");
    std::fs::remove_file(&first).expect("remove artifact");
    let second = lock_and_compile(&dir);
    let second_bytes = std::fs::read_to_string(&second).expect("read second artifact");
    assert_eq!(
        first_bytes, second_bytes,
        "same input must reseal byte-identically"
    );
}

// ─── (b) stop-after → suspended → resume → finished + self-check ───────────

#[test]
fn scenario_b_stop_after_resume_and_inspect() {
    let dir = TempDir::new("stop-resume");
    let flow_ir = lock_and_compile(&dir);
    let store = dir.file("store");
    let run_id = "e2e-stop-resume";

    // Run with the controlled interruption after step `set_ssid`.
    let ran = pointlock(&[
        "run",
        flow_ir.to_str().unwrap(),
        "--store",
        store.to_str().unwrap(),
        "--param",
        "ssid=HomeWifi",
        "--run-id",
        run_id,
        "--stop-after",
        "set_ssid",
    ]);
    assert_exit(&ran, 3, "run --stop-after");
    let stdout = stdout_of(&ran);
    assert!(
        stdout.contains("run suspended"),
        "suspension notice: {stdout}"
    );
    assert!(stdout.contains("resume with:"), "resume hint: {stdout}");
    // The interrupted step completed; the third step never entered.
    assert!(
        stdout.contains("step set_ssid:"),
        "completed steps: {stdout}"
    );
    assert!(
        !stdout.contains("step read_back:"),
        "read_back must not have run: {stdout}"
    );

    // Inspect the suspended run.
    let suspended = pointlock(&[
        "inspect",
        "--store",
        store.to_str().unwrap(),
        "--run",
        run_id,
    ]);
    assert_exit(&suspended, 0, "inspect (suspended)");
    assert!(stdout_of(&suspended).contains("status: suspended"));

    // The R13 approval-gate preview: rehearses the alignment, executes
    // nothing, appends nothing.
    let events_before = pointlock(&[
        "inspect",
        "--store",
        store.to_str().unwrap(),
        "--run",
        run_id,
    ]);
    let preview = pointlock(&[
        "resume",
        flow_ir.to_str().unwrap(),
        "--store",
        store.to_str().unwrap(),
        "--run",
        run_id,
        "--preview",
    ]);
    assert_exit(&preview, 0, "resume --preview");
    let stdout = stdout_of(&preview);
    assert!(stdout.contains("alignment:"), "preview report: {stdout}");
    assert!(stdout.contains("set_ssid reusable"), "preview: {stdout}");
    assert!(stdout.contains("read_back new"), "preview: {stdout}");
    assert!(
        stdout.contains("preview only — nothing executed"),
        "approval hint: {stdout}"
    );
    // Zero side effects: the ledger is byte-for-byte where it was.
    let events_after = pointlock(&[
        "inspect",
        "--store",
        store.to_str().unwrap(),
        "--run",
        run_id,
    ]);
    assert_eq!(
        stdout_of(&events_before),
        stdout_of(&events_after),
        "a preview must append nothing"
    );
    // And the gate is read-only by construction: collection/release
    // flags are refused with it.
    let misuse = pointlock(&[
        "resume",
        flow_ir.to_str().unwrap(),
        "--store",
        store.to_str().unwrap(),
        "--run",
        run_id,
        "--preview",
        "--interactive",
    ]);
    assert_exit(&misuse, 64, "--preview refuses --interactive");

    // Resume: fast path (same irHash), completed steps adopted, only the
    // remaining step dispatches; the flow verdict folds to pass.
    let resumed = pointlock(&[
        "resume",
        flow_ir.to_str().unwrap(),
        "--store",
        store.to_str().unwrap(),
        "--run",
        run_id,
    ]);
    assert_exit(&resumed, 0, "resume");
    let stdout = stdout_of(&resumed);
    assert!(stdout.contains("set_ssid reusable"), "alignment: {stdout}");
    assert!(stdout.contains("read_back new"), "alignment: {stdout}");
    assert!(
        stdout.contains("step read_back: verdict=pass"),
        "resumed step: {stdout}"
    );
    assert!(
        stdout.contains("flow verdict: pass"),
        "resume output: {stdout}"
    );

    // Inspect: finished + the I1 rebuild self-check passes all three ways.
    let inspected = pointlock(&[
        "inspect",
        "--store",
        store.to_str().unwrap(),
        "--run",
        run_id,
        "--rebuild-checkpoint",
    ]);
    assert_exit(&inspected, 0, "inspect --rebuild-checkpoint");
    let stdout = stdout_of(&inspected);
    assert!(
        stdout.contains("status: finished"),
        "inspect output: {stdout}"
    );
    assert!(
        stdout.contains("completed steps: 3"),
        "inspect output: {stdout}"
    );
    assert!(
        stdout.contains("checkpoint self-check: PASS"),
        "self-check output: {stdout}"
    );
}

// ─── (c) broken YAML → non-zero exit + parsable diagnostics array ───────────

#[test]
fn scenario_c_bad_yaml_yields_parsable_json_diagnostics() {
    let dir = TempDir::new("bad-yaml");
    let bad = dir.file("bad.flow.yaml");
    // Two rejections: an `observe` head key (recognized verb, outside the
    // implemented subset — M2 implements `call`, so the boundary example
    // moved) and an action unknown to the capability set.
    std::fs::write(
        &bad,
        r#"
flow: bad_demo
provider: devicerail
steps:
  - id: capture
    invoke: { action: readState, args: {} }
    effect: readonly
    outputs: { seen: "${{ steps.capture.output.ok }}" }
  - id: swipe
    invoke: { action: swipeElement, args: {} }
    effect: mutating
"#,
    )
    .expect("write bad yaml");

    let out = dir.file("bad.flow.ir.json");
    let compiled = pointlock(&[
        "compile",
        bad.to_str().unwrap(),
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
    assert!(!array.is_empty(), "at least one diagnostic");
    for diagnostic in array {
        let code = diagnostic["code"].as_str().expect("code");
        assert!(code.starts_with("RF"), "RF-prefixed code, got {code}");
        assert_eq!(diagnostic["severity"], "error");
        assert!(diagnostic["message"].as_str().is_some());
    }
    // The subset refusal is a diagnostic, never a silent skip.
    assert!(
        array.iter().any(|diag| diag["code"] == "RF2015"),
        "expected the step-key-not-implemented diagnostic: {diagnostics}"
    );
}

// ─── typed "not in M0" surfaces ─────────────────────────────────────────────

#[test]
fn locate_and_report_reject_unknown_runs_as_usage_errors() {
    let dir = TempDir::new("not-in-m0");
    let store = dir.file("store");
    let store = store.to_str().unwrap();

    // locate graduated in M3a: an unknown run is now a typed usage
    // error, not a subset refusal.
    let located = pointlock(&[
        "locate", "--store", store, "--run", "r1", "--step", "s1", "--format", "json",
    ]);
    assert_exit(&located, 64, "locate unknown run");
    assert!(stderr_of(&located).contains("unknown run"));

    // report graduated at the M3a close: an unknown run is the same
    // typed usage error.
    let reported = pointlock(&["report", "--store", store, "--run", "r1"]);
    assert_exit(&reported, 64, "report unknown run");
    assert!(stderr_of(&reported).contains("unknown run"));
}
