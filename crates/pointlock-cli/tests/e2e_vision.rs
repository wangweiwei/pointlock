//! M3a-W4 vision acceptance (08 §6.4): the full chain — YAML `visual`
//! assertion → compile → run over the fake provider (which attaches a
//! screenshot to every observation) → `--vision anthropic` verifier →
//! flow verdict — driven through the real `pointlock` binary against a
//! local canned Messages endpoint (`ANTHROPIC_BASE_URL`). No API key,
//! no network.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

// ─── Harness (the e2e_m0 pattern) ───────────────────────────────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-cli-e2e-vision-{tag}-{}-{}",
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

/// Spawns the binary with the vision environment scrubbed, so a developer's
/// real `ANTHROPIC_API_KEY` never leaks into the acceptance semantics.
fn pointlock_env(args: &[&str], envs: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pointlock"))
        .args(args)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("POINTLOCK_VISION_MODEL")
        .envs(envs.iter().copied())
        .output()
        .expect("spawn pointlock")
}

fn demo_flow_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vision-demo.flow.yaml")
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
    let out = dir.file("vision-demo.flow.ir.json");
    let locked = pointlock_env(
        &[
            "lock",
            "--provider",
            "fake",
            "--out",
            lockfile.to_str().unwrap(),
        ],
        &[],
    );
    assert_exit(&locked, 0, "lock");
    let compiled = pointlock_env(
        &[
            "compile",
            demo_flow_path().to_str().unwrap(),
            "--lockfile",
            lockfile.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ],
        &[],
    );
    assert_exit(&compiled, 0, "compile");
    out
}

/// A local Messages endpoint answering every request with the same
/// verdict line (the vision-demo flow asks exactly once per run).
struct CannedAnthropic {
    server: Arc<tiny_http::Server>,
    url: String,
}

impl CannedAnthropic {
    fn start(answer: &'static str) -> CannedAnthropic {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind canned server"));
        let url = format!(
            "http://127.0.0.1:{}",
            server.server_addr().to_ip().expect("ip").port()
        );
        {
            let server = Arc::clone(&server);
            std::thread::spawn(move || {
                while let Ok(request) = server.recv() {
                    let body = serde_json::json!({
                        "id": "msg_canned",
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "text", "text": answer }],
                        "stop_reason": "end_turn",
                    })
                    .to_string();
                    let _ = request.respond(tiny_http::Response::from_string(body));
                }
            });
        }
        CannedAnthropic { server, url }
    }
}

impl Drop for CannedAnthropic {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

fn run_flow(
    dir: &TempDir,
    artifact: &Path,
    store: &str,
    envs: &[(&str, &str)],
    vision: bool,
) -> Output {
    let store = dir.file(store);
    let mut args = vec![
        "run",
        artifact.to_str().unwrap(),
        "--store",
        store.to_str().unwrap(),
        "--provider",
        "fake",
        "--param",
        "ssid=HomeWifi",
    ];
    if vision {
        args.extend(["--vision", "anthropic"]);
    }
    pointlock_env(&args, envs)
}

// ─── Acceptance ─────────────────────────────────────────────────────────────

/// PASS from the verifier completes the chain: both assertions pass and
/// the flow verdict is `pass`, exit 0.
#[test]
fn canned_pass_drives_the_flow_verdict_to_pass() {
    let dir = TempDir::new("pass");
    let artifact = lock_and_compile(&dir);
    let canned = CannedAnthropic::start("PASS: the field visibly shows HomeWifi");
    let run = run_flow(
        &dir,
        &artifact,
        "store-pass",
        &[
            ("ANTHROPIC_API_KEY", "test-key"),
            ("ANTHROPIC_BASE_URL", canned.url.as_str()),
        ],
        true,
    );
    assert_exit(&run, 0, "run with canned PASS");
    let stdout = stdout_of(&run);
    assert!(stdout.contains("flow verdict: pass"), "{stdout}");
}

/// FAIL from the verifier is a *completed* evaluation (spine R5: fail is
/// final): the visual assertion fails and the flow fails, exit 1.
#[test]
fn canned_fail_drives_the_flow_verdict_to_fail() {
    let dir = TempDir::new("fail");
    let artifact = lock_and_compile(&dir);
    let canned = CannedAnthropic::start("FAIL: the field is visibly empty");
    let run = run_flow(
        &dir,
        &artifact,
        "store-fail",
        &[
            ("ANTHROPIC_API_KEY", "test-key"),
            ("ANTHROPIC_BASE_URL", canned.url.as_str()),
        ],
        true,
    );
    assert_exit(&run, 1, "run with canned FAIL");
    let stdout = stdout_of(&run);
    assert!(stdout.contains("flow verdict: fail"), "{stdout}");
}

/// Without `--vision` the tail degrades honestly to `unknown` — never a
/// guessed pass (principle 4) — and the flow verdict is `unknown`, exit 2.
#[test]
fn without_a_verifier_the_visual_assertion_degrades_to_unknown() {
    let dir = TempDir::new("off");
    let artifact = lock_and_compile(&dir);
    let run = run_flow(&dir, &artifact, "store-off", &[], false);
    assert_exit(&run, 2, "run without --vision");
    let stdout = stdout_of(&run);
    assert!(stdout.contains("flow verdict: unknown"), "{stdout}");
}

/// `--vision anthropic` without `ANTHROPIC_API_KEY` is a typed usage
/// error before any I/O — an unconfigured verifier must not be
/// indistinguishable from `off`.
#[test]
fn vision_anthropic_without_a_key_is_a_usage_error() {
    let dir = TempDir::new("nokey");
    let artifact = lock_and_compile(&dir);
    let run = run_flow(&dir, &artifact, "store-nokey", &[], true);
    assert_exit(&run, 64, "run --vision anthropic without a key");
    let stderr = stderr_of(&run);
    assert!(stderr.contains("ANTHROPIC_API_KEY"), "{stderr}");
}

/// The resume segment carries the verifier (`ResumeOptions.vision` via
/// `pointlock resume --vision anthropic`): suspend before the visual
/// step, then resume against the canned PASS endpoint — the segment
/// executes read_back live and the flow passes.
#[test]
fn a_resume_segment_carries_the_verifier() {
    let dir = TempDir::new("resume");
    let artifact = lock_and_compile(&dir);
    let store = dir.file("store-resume");
    let suspended = pointlock_env(
        &[
            "run",
            artifact.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--provider",
            "fake",
            "--param",
            "ssid=HomeWifi",
            "--run-id",
            "vis-resume-1",
            "--stop-after",
            "set_ssid",
        ],
        &[],
    );
    assert_exit(&suspended, 3, "run --stop-after set_ssid");
    // The printed resume hint must carry the flag when the segment was
    // vision-configured; this segment was not, so no hint assertion —
    // the resume below configures vision explicitly (per segment).
    let canned = CannedAnthropic::start("PASS: the field visibly shows HomeWifi");
    let resumed = pointlock_env(
        &[
            "resume",
            artifact.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--run",
            "vis-resume-1",
            "--provider",
            "fake",
            "--vision",
            "anthropic",
        ],
        &[
            ("ANTHROPIC_API_KEY", "test-key"),
            ("ANTHROPIC_BASE_URL", canned.url.as_str()),
        ],
    );
    assert_exit(&resumed, 0, "resume --vision anthropic with canned PASS");
    let stdout = stdout_of(&resumed);
    assert!(stdout.contains("flow verdict: pass"), "{stdout}");
}

/// The dossier records what the vision channel did: the echo session's
/// synthesized after observation is journaled (screenshot evidence
/// localized) and the visual assertion's reason carries the verifier's
/// one-line answer.
#[test]
fn the_dossier_carries_the_observation_and_the_vision_reason() {
    let dir = TempDir::new("dossier");
    let artifact = lock_and_compile(&dir);
    let store = dir.file("store-dossier");
    let canned = CannedAnthropic::start("PASS: the field visibly shows HomeWifi");
    let run = pointlock_env(
        &[
            "run",
            artifact.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--provider",
            "fake",
            "--param",
            "ssid=HomeWifi",
            "--run-id",
            "vis-dossier-1",
            "--vision",
            "anthropic",
        ],
        &[
            ("ANTHROPIC_API_KEY", "test-key"),
            ("ANTHROPIC_BASE_URL", canned.url.as_str()),
        ],
    );
    assert_exit(&run, 0, "run with canned PASS");

    let located = pointlock_env(
        &[
            "locate",
            "--store",
            store.to_str().unwrap(),
            "--run",
            "vis-dossier-1",
            "--step",
            "read_back",
            "--format",
            "json",
        ],
        &[],
    );
    assert_exit(&located, 0, "locate read_back");
    let dossier: serde_json::Value =
        serde_json::from_str(&stdout_of(&located)).expect("dossier JSON");
    let outcomes = dossier["assertionOutcomes"]
        .as_array()
        .expect("assertion outcomes");
    let visual = outcomes
        .iter()
        .find(|outcome| outcome["assertId"] == "field_visibly_filled")
        .expect("the visual assertion outcome");
    assert_eq!(visual["result"], "pass");
    let reason = visual["reason"].as_str().expect("reason");
    // The chain evaluator prefixes the completing channel; the verifier's
    // one-line answer rides behind it.
    assert!(reason.contains("vision: "), "{reason}");
    // The synthesized after observation was journaled with its localized
    // screenshot (the evidence the verifier judged).
    let observations = dossier["observations"].as_array().expect("observations");
    assert!(!observations.is_empty(), "no observations in the dossier");
    // The capture-time viewport travels verbatim into the record (the
    // M3a additive close of the registered W1 gap).
    assert_eq!(observations[0]["viewport"]["width"], 1080);
    assert_eq!(observations[0]["viewport"]["height"], 2400);
    assert_eq!(observations[0]["viewport"]["scaleFactor"], 2.0);
    assert!(
        observations
            .iter()
            .any(|observation| observation["screenshot"].is_object()),
        "no localized screenshot on the observations"
    );
}
