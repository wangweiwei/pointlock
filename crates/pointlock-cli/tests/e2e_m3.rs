//! M3a-W1 end-to-end acceptance: `pointlock locate` graduated from the M0
//! typed refusal to the `StepDossierView` projection (spine §9 rule 3,
//! §10.1) — lock → compile → run the demo flow with the fake provider,
//! then locate by bare step id, by canonical path, and with the FlowIR
//! artifact supplied (IR node + YAML span parts).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

// ─── Harness (the e2e_m0 pattern) ───────────────────────────────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-cli-e2e-m3-{tag}-{}-{}",
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

/// Spawns the binary with `input` piped to stdin (the e2e_m2 pattern —
/// attached interactive collection).
fn pointlock_with_stdin(args: &[&str], input: &str) -> Output {
    use std::io::Write as _;
    use std::process::Stdio;
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
        .expect("stdin piped")
        .write_all(input.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait pointlock")
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
    let compiled = pointlock(&[
        "compile",
        demo_flow_path().to_str().unwrap(),
        "--lockfile",
        lockfile.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_exit(&compiled, 0, "compile");
    out
}

fn run_demo(dir: &TempDir, flow_ir: &Path, run_id: &str) {
    let store = dir.file("store");
    let ran = pointlock(&[
        "run",
        flow_ir.to_str().unwrap(),
        "--store",
        store.to_str().unwrap(),
        "--param",
        "ssid=HomeWifi",
        "--run-id",
        run_id,
    ]);
    assert_exit(&ran, 0, "run");
}

// ─── locate delivers the dossier ────────────────────────────────────────────

#[test]
fn locate_delivers_the_step_dossier() {
    let dir = TempDir::new("locate");
    let flow_ir = lock_and_compile(&dir);
    run_demo(&dir, &flow_ir, "m3-locate-run");
    let store = dir.file("store");
    let store = store.to_str().unwrap();

    // (1) Bare step id, JSON: the projection DTO on stdout.
    let located = pointlock(&[
        "locate",
        "--store",
        store,
        "--run",
        "m3-locate-run",
        "--step",
        "read_back",
        "--format",
        "json",
    ]);
    assert_exit(&located, 0, "locate by step id");
    let dossier: serde_json::Value =
        serde_json::from_str(&stdout_of(&located)).expect("stdout is the dossier JSON");
    assert_eq!(dossier["projectionVersion"], 1);
    assert_eq!(dossier["stepId"], "read_back");
    assert_eq!(dossier["verdict"]["status"], "pass");
    assert_eq!(
        dossier["attempts"].as_array().expect("attempts").len(),
        1,
        "one succeeded attempt"
    );
    assert_eq!(dossier["attempts"][0]["outcome"], "succeeded");
    assert!(
        dossier["attempts"][0]["startedAtMs"].is_u64(),
        "attempt enriched with provider timing from actionSettled"
    );
    assert_eq!(
        dossier["assertionOutcomes"]
            .as_array()
            .expect("outcomes")
            .len(),
        2,
        "both demo assertions"
    );
    assert!(
        dossier.get("irNode").is_none(),
        "no --flow-ir: the ledger cannot resolve IR"
    );

    // (2) The canonical path from (1) resolves to the same instance
    // (spine §9 round-trip; UI deep links are locate hyperlinks).
    let canonical = dossier["runPath"].as_str().expect("runPath");
    let by_path = pointlock(&[
        "locate",
        "--store",
        store,
        "--run",
        "m3-locate-run",
        "--step",
        canonical,
        "--format",
        "json",
    ]);
    assert_exit(&by_path, 0, "locate by canonical path");
    let same: serde_json::Value = serde_json::from_str(&stdout_of(&by_path)).expect("dossier JSON");
    assert_eq!(same["runPath"], dossier["runPath"]);
    assert_eq!(same["stepId"], "read_back");

    // (3) With the artifact: IR node + YAML span join the dossier.
    let with_ir = pointlock(&[
        "locate",
        "--store",
        store,
        "--run",
        "m3-locate-run",
        "--step",
        "read_back",
        "--flow-ir",
        flow_ir.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_exit(&with_ir, 0, "locate with artifact");
    let full: serde_json::Value = serde_json::from_str(&stdout_of(&with_ir)).expect("dossier JSON");
    assert_eq!(full["irNode"]["stepId"], "read_back");
    assert_eq!(full["irNode"]["kind"], "action");
    let span = &full["source"]["entry"]["span"];
    assert!(
        span["startLine"].as_u64().unwrap_or(0) >= 1,
        "YAML span resolved through the sourceMap: {span}"
    );
    assert!(
        full["source"]["entry"]["file"]
            .as_str()
            .expect("file")
            .ends_with("wifi-demo.flow.yaml"),
        "span points into the authored YAML"
    );

    // (4) Text format stays human-readable and exits 0.
    let text = pointlock(&[
        "locate",
        "--store",
        store,
        "--run",
        "m3-locate-run",
        "--step",
        "read_back",
    ]);
    assert_exit(&text, 0, "locate text");
    let rendered = stdout_of(&text);
    assert!(rendered.contains("step: read_back"), "{rendered}");
    assert!(rendered.contains("verdict: pass"), "{rendered}");

    // (5) Unknown step: typed usage error, exit 64.
    let missing = pointlock(&[
        "locate",
        "--store",
        store,
        "--run",
        "m3-locate-run",
        "--step",
        "no_such_step",
    ]);
    assert_exit(&missing, 64, "locate unknown step");
    assert!(stderr_of(&missing).contains("no step instance"));
}

// ─── report: the run report over the same demo run (08 §6.2/§6.3) ──────────

#[test]
fn report_renders_text_and_the_versioned_json_envelope() {
    let dir = TempDir::new("report");
    let flow_ir = lock_and_compile(&dir);
    run_demo(&dir, &flow_ir, "m3-report-run");
    let store = dir.file("store");

    let text = pointlock(&[
        "report",
        "--store",
        store.to_str().unwrap(),
        "--run",
        "m3-report-run",
    ]);
    assert_exit(&text, 0, "report text");
    let stdout = stdout_of(&text);
    assert!(stdout.contains("run: m3-report-run"), "{stdout}");
    assert!(stdout.contains("flow verdict: pass"), "{stdout}");
    // R4: the two unasserted mutating steps carry the annotation, the
    // judged step its verdict — and the tallies keep them apart.
    assert!(
        stdout.contains("1 pass, 0 fail, 0 unknown; 2 unverified"),
        "{stdout}"
    );
    assert!(
        stdout.contains("unverified (executed, no assertions)"),
        "{stdout}"
    );
    assert!(stdout.contains("segments:"), "{stdout}");
    assert!(stdout.contains("1. started at"), "{stdout}");

    let json = pointlock(&[
        "report",
        "--store",
        store.to_str().unwrap(),
        "--run",
        "m3-report-run",
        "--format",
        "json",
    ]);
    assert_exit(&json, 0, "report json");
    let report: serde_json::Value = serde_json::from_str(&stdout_of(&json)).expect("report JSON");
    assert_eq!(report["pointlockReport"], 1);
    assert_eq!(report["runId"], "m3-report-run");
    assert_eq!(report["status"], "finished");
    assert_eq!(report["flowVerdict"]["status"], "pass");
    assert_eq!(report["counts"]["pass"], 1);
    assert_eq!(report["counts"]["unverified"], 2);
    assert_eq!(report["counts"]["unknown"], 0);
    let steps = report["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 3);
    assert!(
        steps
            .iter()
            .any(|step| step["stepId"] == "open_panel" && step["unverified"] == true),
        "{steps:?}"
    );
    assert!(
        steps
            .iter()
            .any(|step| step["stepId"] == "read_back" && step["verdictStatus"] == "pass"),
        "{steps:?}"
    );

    // An unknown run is a typed usage error (the locate convention),
    // not an empty report.
    let missing = pointlock(&[
        "report",
        "--store",
        store.to_str().unwrap(),
        "--run",
        "no-such-run",
    ]);
    assert_exit(&missing, 64, "report unknown run");
}

// ─── report: repair lineage joins across the cross-IR boundary ──────────────

const REPORT_REPAIR_FLOW_BROKEN: &str = r#"flow: report_repair
provider: devicerail
params:
  ssid: { schema: { type: string }, required: true }
steps:
  - id: set_ssid
    invoke:
      action: setElementValue
      args: { element: ssid_field, value: "${{ params.ssid }}" }
    effect: mutating
    expect_schema:
      type: object
      properties:
        value: { type: string }
  - id: read_back
    invoke: { action: findElement, args: { element: ssid_field } }
    effect: readonly
    expect:
      - assert_id: ssid_was_typed
        expr: ${{ eq(steps.set_ssid.output.value, "WrongName") }}
"#;

const REPORT_REPAIR_FLOW_FIXED: &str = r#"flow: report_repair
provider: devicerail
params:
  ssid: { schema: { type: string }, required: true }
steps:
  - id: set_ssid
    invoke:
      action: setElementValue
      args: { element: ssid_field, value: "${{ params.ssid }}" }
    effect: mutating
    expect_schema:
      type: object
      properties:
        value: { type: string }
  - id: read_back
    invoke: { action: findElement, args: { element: ssid_field } }
    effect: readonly
    expect:
      - assert_id: ssid_was_typed
        expr: ${{ eq(steps.set_ssid.output.value, params.ssid) }}
"#;

/// A judge-domain repair resume: the superseded-lineage count must join
/// across the irHash change (site identity, not hash-bearing paths), the
/// flow verdict must come from the LAST runFinished, and the resumed
/// segment must carry its alignment tallies.
#[test]
fn report_joins_superseded_lineage_across_a_repair_boundary() {
    let dir = TempDir::new("report-repair");
    let lockfile = dir.file("fake.lock.json");
    assert_exit(
        &pointlock(&["lock", "--out", lockfile.to_str().unwrap()]),
        0,
        "lock",
    );
    let broken_yaml = dir.file("repair.flow.yaml");
    std::fs::write(&broken_yaml, REPORT_REPAIR_FLOW_BROKEN).expect("write broken flow");
    let broken_ir = dir.file("repair-broken.ir.json");
    assert_exit(
        &pointlock(&[
            "compile",
            broken_yaml.to_str().unwrap(),
            "--lockfile",
            lockfile.to_str().unwrap(),
            "--out",
            broken_ir.to_str().unwrap(),
        ]),
        0,
        "compile broken",
    );
    let store = dir.file("store");
    let ran = pointlock(&[
        "run",
        broken_ir.to_str().unwrap(),
        "--store",
        store.to_str().unwrap(),
        "--param",
        "ssid=HomeWifi",
        "--run-id",
        "repair-report-1",
    ]);
    assert_exit(&ran, 1, "broken run fails");

    // Judge-domain repair: fix the expected literal, recompile, resume.
    let fixed_yaml = dir.file("repair-fixed.flow.yaml");
    std::fs::write(&fixed_yaml, REPORT_REPAIR_FLOW_FIXED).expect("write fixed flow");
    let fixed_ir = dir.file("repair-fixed.ir.json");
    assert_exit(
        &pointlock(&[
            "compile",
            fixed_yaml.to_str().unwrap(),
            "--lockfile",
            lockfile.to_str().unwrap(),
            "--out",
            fixed_ir.to_str().unwrap(),
        ]),
        0,
        "compile fixed",
    );
    let resumed = pointlock(&[
        "resume",
        fixed_ir.to_str().unwrap(),
        "--store",
        store.to_str().unwrap(),
        "--run",
        "repair-report-1",
    ]);
    assert_exit(&resumed, 0, "judge-dirty resume passes");

    let json = pointlock(&[
        "report",
        "--store",
        store.to_str().unwrap(),
        "--run",
        "repair-report-1",
        "--format",
        "json",
    ]);
    assert_exit(&json, 0, "report json");
    let report: serde_json::Value = serde_json::from_str(&stdout_of(&json)).expect("report JSON");
    // The LAST runFinished wins: pass, not the first segment's fail.
    assert_eq!(report["flowVerdict"]["status"], "pass");
    assert_eq!(report["counts"]["pass"], 1);
    assert_eq!(report["counts"]["fail"], 0);
    // The re-judged verdict superseded the fail — joined across the
    // irHash change by site identity.
    let steps = report["steps"].as_array().expect("steps");
    let read_back = steps
        .iter()
        .find(|step| step["stepId"] == "read_back")
        .expect("read_back line");
    assert_eq!(read_back["verdictStatus"], "pass");
    assert_eq!(read_back["superseded"], 1, "{read_back}");
    // Segment history: started + resumed with judgeDirty alignment.
    let segments = report["segments"].as_array().expect("segments");
    assert_eq!(segments[0]["kind"], "started");
    assert_eq!(segments[1]["kind"], "resumed");
    assert_eq!(segments[1]["alignment"]["judgeDirty"], 1);
}

/// A supervision `suspend` answer is non-final (spine §6.9): the report's
/// human line must carry the eventual proceed as the arbitrated FINAL
/// response, never the suspend.
#[test]
fn report_shows_the_final_supervision_ruling_not_the_suspend() {
    let dir = TempDir::new("report-supervise");
    let flow_ir = lock_and_compile(&dir);
    let store = dir.file("store");
    let suspended = pointlock_with_stdin(
        &[
            "run",
            flow_ir.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--param",
            "ssid=lab-net",
            "--run-id",
            "sup-report",
            "--supervise",
            "mutating",
            "--interactive",
        ],
        "suspend\n",
    );
    assert_exit(&suspended, 3, "suspend answer leaves the run suspended");
    let resumed = pointlock_with_stdin(
        &[
            "resume",
            flow_ir.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--run",
            "sup-report",
            "--supervise",
            "mutating",
            "--interactive",
        ],
        "proceed\nproceed\n",
    );
    assert_exit(&resumed, 0, "resume after suspend");

    let json = pointlock(&[
        "report",
        "--store",
        store.to_str().unwrap(),
        "--run",
        "sup-report",
        "--format",
        "json",
    ]);
    assert_exit(&json, 0, "report json");
    let report: serde_json::Value = serde_json::from_str(&stdout_of(&json)).expect("report JSON");
    let humans = report["humans"].as_array().expect("humans");
    assert!(!humans.is_empty(), "supervision requests present");
    for human in humans {
        assert_eq!(human["purpose"], "supervision", "{human}");
        assert_eq!(
            human["response"]["decision"], "proceed",
            "the final ruling, not the suspend: {human}"
        );
        assert!(human["actor"].is_string(), "{human}");
    }
}
