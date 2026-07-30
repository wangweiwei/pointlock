//! M3a-W2 end-to-end acceptance: `pointlock inspect --serve` — the
//! projection protocol's HTTP+JSON canonical form (spine §10.4, 08 §5).
//! Runs the demo flow with the fake provider, boots the host on an
//! ephemeral port, and exercises every endpoint class over a hand-rolled
//! loopback HTTP/1.1 client (no client dependency): token gate, flow
//! index/detail (graph), run index/overview, timeline paging, dossier by
//! encoded canonical path, revision, SSE first tick, evidence 404, and
//! the typed 501 write refusals.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// ─── Harness (the e2e_m0 pattern) ───────────────────────────────────────────

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pointlock-cli-e2e-serve-{tag}-{}-{}",
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

fn assert_exit(output: &Output, expected: i32, context: &str) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{context}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// lock + compile into `dir/artifacts/`, run once, return the store dir.
fn prepare_run(dir: &TempDir) -> (PathBuf, PathBuf) {
    let artifacts = dir.file("artifacts");
    std::fs::create_dir_all(&artifacts).expect("create artifacts dir");
    let lockfile = dir.file("devicerail.lock.json");
    let flow_ir = artifacts.join("wifi-demo.flow.ir.json");
    let store = dir.file("store");

    assert_exit(
        &pointlock(&[
            "lock",
            "--provider",
            "fake",
            "--out",
            lockfile.to_str().unwrap(),
        ]),
        0,
        "lock",
    );
    assert_exit(
        &pointlock(&[
            "compile",
            demo_flow_path().to_str().unwrap(),
            "--lockfile",
            lockfile.to_str().unwrap(),
            "--out",
            flow_ir.to_str().unwrap(),
        ]),
        0,
        "compile",
    );
    assert_exit(
        &pointlock(&[
            "run",
            flow_ir.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--param",
            "ssid=HomeWifi",
            "--run-id",
            "serve-run",
        ]),
        0,
        "run",
    );
    (store, artifacts)
}

/// A running `--serve` host: child process + parsed port/token.
struct ServeHost {
    child: Child,
    port: u16,
    token: String,
}

impl ServeHost {
    fn boot(store: &Path, artifacts: &Path) -> ServeHost {
        Self::boot_with(store, artifacts, &[])
    }

    fn boot_with(store: &Path, artifacts: &Path, extra: &[&str]) -> ServeHost {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pointlock"))
            .args([
                "inspect",
                "--store",
                store.to_str().unwrap(),
                "--serve",
                "--artifacts",
                artifacts.to_str().unwrap(),
            ])
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn serve");
        let stdout = child.stdout.take().expect("stdout piped");
        let mut lines = BufReader::new(stdout).lines();
        // Parse `  http://127.0.0.1:PORT/api/flows?token=TOKEN`.
        let url_line = loop {
            let line = lines
                .next()
                .expect("serve prints its URL")
                .expect("readable stdout");
            if line.contains("http://127.0.0.1:") {
                break line;
            }
        };
        let url = url_line.trim();
        let port: u16 = url
            .split(":")
            .nth(2)
            .and_then(|rest| rest.split('/').next())
            .expect("port in url")
            .parse()
            .expect("numeric port");
        let token = url.split("token=").nth(1).expect("token in url").to_owned();
        ServeHost { child, port, token }
    }

    /// Minimal HTTP/1.1 request over loopback; returns (status, body).
    fn request(&self, method: &str, path_query: &str) -> (u16, String) {
        let (status, _, body) = self.request_full(method, path_query);
        (status, body)
    }

    /// Like [`Self::request`] but also returns the raw header block.
    fn request_full(&self, method: &str, path_query: &str) -> (u16, String, String) {
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.port)).expect("connect loopback host");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        write!(
            stream,
            "{method} {path_query} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .expect("write request");
        let mut raw = String::new();
        stream.read_to_string(&mut raw).expect("read response");
        parse_response(&raw)
    }

    fn get(&self, path: &str) -> (u16, String) {
        let sep = if path.contains('?') { '&' } else { '?' };
        self.request("GET", &format!("{path}{sep}token={}", self.token))
    }

    fn get_json(&self, path: &str) -> serde_json::Value {
        let (status, body) = self.get(path);
        assert_eq!(status, 200, "GET {path}: {body}");
        serde_json::from_str(&body).unwrap_or_else(|err| panic!("GET {path}: {err}\n{body}"))
    }
}

impl Drop for ServeHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_response(raw: &str) -> (u16, String, String) {
    let mut parts = raw.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or_default();
    let mut body = parts.next().unwrap_or_default().to_owned();
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    // Chunked transfer: reassemble the chunks.
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        body = dechunk(&body);
    }
    (status, head.to_owned(), body)
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    loop {
        let Some((size_line, tail)) = rest.split_once("\r\n") else {
            break;
        };
        let Ok(size) = usize::from_str_radix(size_line.trim(), 16) else {
            break;
        };
        if size == 0 {
            break;
        }
        if tail.len() < size {
            out.push_str(tail);
            break;
        }
        out.push_str(&tail[..size]);
        rest = tail[size..].trim_start_matches("\r\n");
    }
    out
}

// ─── The scenario ───────────────────────────────────────────────────────────

#[test]
fn serve_hosts_the_projection_protocol() {
    let dir = TempDir::new("host");
    let (store, artifacts) = prepare_run(&dir);
    let host = ServeHost::boot(&store, &artifacts);

    // (1) Token gate: no token → 401; wrong token → 401.
    let (status, _) = host.request("GET", "/api/flows");
    assert_eq!(status, 401, "no token");
    let (status, _) = host.request("GET", "/api/flows?token=wrong");
    assert_eq!(status, 401, "wrong token");

    // (2) Flow index: the compiled artifact + its run row.
    let flows = host.get_json("/api/flows");
    assert_eq!(flows["projectionVersion"], 1);
    let flow = &flows["flows"][0];
    assert_eq!(flow["flowId"], "wifi_demo");
    assert!(
        flow["latestIrHash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(flow["runs"][0]["runId"], "serve-run");
    assert_eq!(flow["runs"][0]["flowVerdictStatus"], "pass");

    // (3) Flow detail: the graph projection rides along.
    let detail = host.get_json("/api/flows/wifi_demo");
    assert_eq!(detail["graph"]["projectionVersion"], 1);
    assert_eq!(detail["graph"]["flowId"], "wifi_demo");
    assert_eq!(detail["graph"]["nodes"].as_array().unwrap().len(), 3);

    // (4) Run index + overview.
    let runs = host.get_json("/api/runs?flowId=wifi_demo");
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1);
    assert_eq!(runs["runs"][0]["deviceId"], "fake-device-1");
    let overview = host.get_json("/api/runs/serve-run");
    assert_eq!(overview["status"], "finished");
    assert_eq!(overview["flowVerdictStatus"], "pass");
    let revision = overview["revision"].as_u64().expect("revision");
    assert!(revision > 0);

    // (5) Timeline paging + filter.
    let page = host.get_json("/api/runs/serve-run/timeline?filter=verdicts&pageSize=10");
    assert_eq!(page["filter"], "verdicts");
    assert_eq!(page["revision"].as_u64(), Some(revision));
    assert!(page["total"].as_u64().unwrap() >= 2);

    // (6) Dossier by URL-encoded canonical path — the deep-link form
    // (08 §2.1: the route param IS the locate argument).
    let steps = overview["steps"].as_object().expect("steps map");
    let key = steps
        .keys()
        .find(|key| key.ends_with("/read_back"))
        .expect("read_back key");
    let encoded: String = key
        .bytes()
        .flat_map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' || byte == b'.' {
                vec![byte as char]
            } else {
                format!("%{byte:02X}").chars().collect()
            }
        })
        .collect();
    let dossier = host.get_json(&format!("/api/runs/serve-run/steps/{encoded}"));
    assert_eq!(dossier["stepId"], "read_back");
    assert_eq!(dossier["verdict"]["status"], "pass");
    assert_eq!(
        dossier["irNode"]["stepId"], "read_back",
        "artifact scan supplies the IR node"
    );

    // (7) Revision endpoint matches the overview's.
    let rev = host.get_json("/api/runs/serve-run/revision");
    assert_eq!(rev["revision"].as_u64(), Some(revision));

    // (8) Inbox: empty for the finished run; the global revision endpoint
    // (the inbox stream's polling equivalent) reports the same head.
    let inbox = host.get_json("/api/inbox");
    assert_eq!(inbox["inbox"].as_array().unwrap().len(), 0);
    let global = host.get_json("/api/inbox/revision");
    assert_eq!(global["revision"].as_u64(), Some(revision));

    // (9) Evidence: unknown digest → 404; bad key → 400.
    let (status, _) = host.get(&format!("/evidence/{}", "a".repeat(64)));
    assert_eq!(status, 404, "unknown digest");
    let (status, _) = host.get("/evidence/nothex");
    assert_eq!(status, 400, "bad key");

    // (10) Write surface: the repair endpoints are live (W3b) — an
    // empty body is a typed 400; the v0.2-reserved respond stays 501.
    let (status, body) = host.request("POST", &format!("/api/repair/compile?token={}", host.token));
    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("not JSON") || body.contains("flowPath"),
        "{body}"
    );
    let (status, body) = host.request(
        "POST",
        &format!("/api/inbox/req-1/respond?token={}", host.token),
    );
    assert_eq!(status, 501);
    assert!(body.contains("pointlock-human-cli"), "{body}");

    // (11) Evidence hardening: stored bytes are served inert (nosniff +
    // CSP sandbox) and a hostile media type can neither inject headers
    // nor leave the sanitized fallback.
    let (html_sha, evil_sha) = {
        let mut raw_store = pointlock_store::Store::open(&store).expect("open store for evidence");
        let html = raw_store
            .put_evidence(b"<html><script>alert(1)</script></html>", "text/html")
            .expect("put html evidence");
        let evil = raw_store
            .put_evidence(b"payload", "text/plain\r\nX-Evil: 1")
            .expect("put evil evidence");
        (html.sha256, evil.sha256)
    };
    let (status, head, body) =
        host.request_full("GET", &format!("/evidence/{html_sha}?token={}", host.token));
    assert_eq!(status, 200, "{body}");
    let head_lower = head.to_ascii_lowercase();
    assert!(
        head_lower.contains("x-content-type-options: nosniff"),
        "{head}"
    );
    assert!(
        head_lower.contains("content-security-policy: sandbox"),
        "{head}"
    );
    let (status, head, _) =
        host.request_full("GET", &format!("/evidence/{evil_sha}?token={}", host.token));
    assert_eq!(status, 200);
    assert!(
        !head.to_ascii_lowercase().contains("x-evil"),
        "header injection must be neutralized: {head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: application/octet-stream"),
        "hostile media type falls back to octet-stream: {head}"
    );

    // (12) SSE: the stream's first tick carries the current revision.
    let mut stream = TcpStream::connect(("127.0.0.1", host.port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    write!(
        stream,
        "GET /api/runs/serve-run/stream?token={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        host.token
    )
    .expect("write sse request");
    let mut reader = BufReader::new(stream);
    let mut saw_event = false;
    for _ in 0..50 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.contains("data:") && line.contains(&format!("\"revision\":{revision}")) {
            saw_event = true;
            break;
        }
    }
    assert!(saw_event, "SSE pushes the {{revision}} invalidation tick");

    // (13) SSE for an unknown run answers 404 up front — never a
    // forever-silent 200 (polling equivalence, 08 §5).
    let (status, _) = host.get("/api/runs/no-such-run/stream");
    assert_eq!(status, 404, "unknown run stream");
}

#[test]
fn sse_decodes_run_ids_like_every_pull_route() {
    let dir = TempDir::new("sse-encoded");
    let (store, artifacts) = prepare_run(&dir);
    // A second run whose id needs URL-encoding.
    let flow_ir = artifacts.join("wifi-demo.flow.ir.json");
    assert_exit(
        &pointlock(&[
            "run",
            flow_ir.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--param",
            "ssid=HomeWifi",
            "--run-id",
            "serve run 2",
        ]),
        0,
        "run with a space in the id",
    );
    let host = ServeHost::boot(&store, &artifacts);

    let rev = host.get_json("/api/runs/serve%20run%202/revision");
    let revision = rev["revision"].as_u64().expect("revision");
    assert!(revision > 0);

    let mut stream = TcpStream::connect(("127.0.0.1", host.port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    write!(
        stream,
        "GET /api/runs/serve%20run%202/stream?token={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        host.token
    )
    .expect("write sse request");
    let mut reader = BufReader::new(stream);
    let mut saw_event = false;
    for _ in 0..50 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.contains("data:") && line.contains(&format!("\"revision\":{revision}")) {
            saw_event = true;
            break;
        }
    }
    assert!(saw_event, "encoded run ids tick like decoded pull routes");
}

#[test]
fn serve_flag_combinations_are_typed_usage_errors() {
    let dir = TempDir::new("flags");
    let store = dir.file("store");
    std::fs::create_dir_all(&store).expect("store dir");

    let both = pointlock(&[
        "inspect",
        "--store",
        store.to_str().unwrap(),
        "--serve",
        "--run",
        "r1",
    ]);
    assert_exit(&both, 64, "serve + run");

    let orphan = pointlock(&[
        "inspect",
        "--store",
        store.to_str().unwrap(),
        "--run",
        "r1",
        "--port",
        "9999",
    ]);
    assert_exit(&orphan, 64, "port without serve");
}

#[test]
fn serve_hosts_the_spa_shell_token_free_with_gated_data() {
    let dir = TempDir::new("spa");
    let (store, artifacts) = prepare_run(&dir);

    // A stand-in SPA build (the real one is `pnpm --filter @pointlock/ui
    // build`; the host serves any static dir the same way).
    let ui = dir.file("ui");
    std::fs::create_dir_all(ui.join("assets")).expect("ui dirs");
    std::fs::write(ui.join("index.html"), "<html>pointlock-spa-shell</html>").expect("index");
    std::fs::write(ui.join("assets/app.js"), "console.log('shell')").expect("asset");

    let host = ServeHost::boot_with(&store, &artifacts, &["--ui", ui.to_str().unwrap()]);

    // The shell rides token-free…
    let (status, head, body) = host.request_full("GET", "/");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("pointlock-spa-shell"));
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: text/html")
    );
    let (status, head, _) = host.request_full("GET", "/assets/app.js");
    assert_eq!(status, 200);
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: text/javascript")
    );

    // …traversal out of the dir is refused…
    let (status, _) = host.request("GET", "/assets/..%2F..%2FCargo.toml");
    assert_eq!(status, 404, "encoded traversal");
    let (status, _) = host.request("GET", "/../store/pointlock.db");
    assert_eq!(status, 404, "plain traversal");

    // …and the data plane stays gated.
    let (status, _) = host.request("GET", "/api/flows");
    assert_eq!(status, 401, "data still needs the token");
    let flows = host.get_json("/api/flows");
    assert_eq!(flows["flows"][0]["flowId"], "wifi_demo");
}

// ─── The repair closure over HTTP (08 §2.7, M3a-W3b) ────────────────────────

/// Writes a judge-only edit of the demo flow (assertion expression
/// changed, act surface untouched) into `dir`.
fn judge_edited_flow(dir: &TempDir) -> PathBuf {
    let original = std::fs::read_to_string(demo_flow_path()).expect("read demo yaml");
    let edited = original.replace(
        "eq(steps.read_back.output.element, \"ssid_field\")",
        "ne(steps.read_back.output.element, \"not_this\")",
    );
    assert_ne!(original, edited, "the judge edit must land");
    let path = dir.file("wifi-demo-judge-edit.flow.yaml");
    std::fs::write(&path, edited).expect("write edited yaml");
    path
}

impl ServeHost {
    /// Minimal POST with a JSON body; returns (status, body).
    fn post_json(&self, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.port)).expect("connect loopback host");
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("read timeout");
        let payload = body.to_string();
        write!(
            stream,
            "POST {path}?token={} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            self.token,
            payload.len(),
        )
        .expect("write request");
        let mut raw = String::new();
        stream.read_to_string(&mut raw).expect("read response");
        let (status, _, body) = parse_response(&raw);
        let json: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|err| panic!("POST {path}: {err}\n{body}"));
        (status, json)
    }
}

#[test]
fn repair_closure_compile_preview_resume_over_http() {
    let dir = TempDir::new("repair");
    let artifacts = dir.file("artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let lockfile = dir.file("devicerail.lock.json");
    let flow_ir = artifacts.join("wifi-demo.flow.ir.json");
    let store = dir.file("store");

    assert_exit(
        &pointlock(&[
            "lock",
            "--provider",
            "fake",
            "--out",
            lockfile.to_str().unwrap(),
        ]),
        0,
        "lock",
    );
    assert_exit(
        &pointlock(&[
            "compile",
            demo_flow_path().to_str().unwrap(),
            "--lockfile",
            lockfile.to_str().unwrap(),
            "--out",
            flow_ir.to_str().unwrap(),
        ]),
        0,
        "compile",
    );
    // A run suspended after set_ssid: two steps executed, read_back pending.
    assert_exit(
        &pointlock(&[
            "run",
            flow_ir.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--param",
            "ssid=HomeWifi",
            "--run-id",
            "repair-run",
            "--stop-after",
            "set_ssid",
        ]),
        3,
        "run suspended",
    );

    let host = ServeHost::boot(&store, &artifacts);
    let edited = judge_edited_flow(&dir);

    // (1) Recompile the edited YAML through the endpoint — the artifact
    // lands in the served artifacts dir.
    let (status, compiled) = host.post_json(
        "/api/repair/compile",
        &serde_json::json!({
            "flowPath": edited.to_str().unwrap(),
            "lockfilePath": lockfile.to_str().unwrap(),
        }),
    );
    assert_eq!(status, 200, "{compiled}");
    assert_eq!(compiled["ok"], true, "{compiled}");
    let artifact = compiled["artifact"].as_str().expect("artifact path");
    assert!(
        compiled["irHash"]
            .as_str()
            .unwrap_or_default()
            .starts_with("sha256:"),
        "{compiled}"
    );
    // The artifact lands under the served --artifacts dir and the flow
    // list immediately reflects the new version.
    assert!(
        Path::new(artifact).starts_with(&artifacts),
        "artifact {artifact} outside {artifacts:?}"
    );
    let flows = host.get_json("/api/flows");
    assert_eq!(
        flows["flows"][0]["versions"].as_array().map(Vec::len),
        Some(2),
        "{flows}"
    );

    // (2) The read-only alignment preview (lockfile → env.platform
    // parity with the real resume): executed steps reusable, the
    // pending step new; nothing gated.
    let (status, preview) = host.post_json(
        "/api/repair/align-preview",
        &serde_json::json!({
            "runId": "repair-run",
            "flowIrPath": artifact,
            "lockfilePath": lockfile.to_str().unwrap(),
        }),
    );
    assert_eq!(status, 200, "{preview}");
    let entries = preview["report"]["entries"].as_array().expect("entries");
    let class_of = |step: &str| {
        entries
            .iter()
            .find(|entry| entry["stepId"] == step)
            .map(|entry| entry["class"].as_str().unwrap_or_default().to_owned())
            .unwrap_or_default()
    };
    assert_eq!(class_of("open_panel"), "reusable", "{preview}");
    assert_eq!(class_of("set_ssid"), "reusable", "{preview}");
    assert_eq!(class_of("read_back"), "new", "{preview}");
    assert_eq!(
        preview["report"]["requiresConfirmation"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );
    assert!(
        preview["resumePointRendered"]
            .as_str()
            .unwrap_or_default()
            .starts_with("wifi_demo@"),
        "{preview}"
    );

    // (3) Resume from the checkpoint under the repaired IR — the run
    // finishes pass, visible through the projection.
    let (status, resumed) = host.post_json(
        "/api/repair/resume",
        &serde_json::json!({ "runId": "repair-run", "flowIrPath": artifact }),
    );
    assert_eq!(status, 200, "{resumed}");
    assert_eq!(resumed["exitCode"], 0, "{resumed}");
    let overview = host.get_json("/api/runs/repair-run");
    assert_eq!(overview["status"], "finished");
    assert_eq!(overview["flowVerdictStatus"], "pass");

    // (4) The diagnostics leg: a broken YAML answers ok:false + RF codes.
    let broken = dir.file("broken.flow.yaml");
    std::fs::write(&broken, "flow: broken\nsteps: []\n").expect("write broken");
    let (status, rejected) = host.post_json(
        "/api/repair/compile",
        &serde_json::json!({ "flowPath": broken.to_str().unwrap() }),
    );
    assert_eq!(status, 200, "{rejected}");
    assert_eq!(rejected["ok"], false, "{rejected}");
    assert!(
        rejected["diagnostics"][0]["code"]
            .as_str()
            .unwrap_or_default()
            .starts_with("RF"),
        "{rejected}"
    );

    // (5) GET on a repair endpoint: typed 405.
    let (status, _) = host.get("/api/repair/compile");
    assert_eq!(status, 405);
}

#[test]
fn align_preview_shows_judge_dirty_offline_rejudgement() {
    let dir = TempDir::new("judge-dirty");
    let (store, artifacts) = prepare_run(&dir);
    let lockfile = dir.file("devicerail.lock.json");
    let host = ServeHost::boot(&store, &artifacts);

    // The finished run + a judge-only edit: the executed read_back's
    // judgeHash changes while its effectHash stays — the preview shows
    // the dual-hash narrative (judgeDirty: re-judged offline, no device
    // redispatch) without writing anything.
    let edited = judge_edited_flow(&dir);
    let (status, compiled) = host.post_json(
        "/api/repair/compile",
        &serde_json::json!({
            "flowPath": edited.to_str().unwrap(),
            "lockfilePath": lockfile.to_str().unwrap(),
        }),
    );
    assert_eq!(status, 200, "{compiled}");
    assert_eq!(compiled["ok"], true, "{compiled}");
    let artifact = compiled["artifact"].as_str().expect("artifact");

    let revision_before = host.get_json("/api/runs/serve-run/revision")["revision"]
        .as_u64()
        .expect("revision");

    let (status, preview) = host.post_json(
        "/api/repair/align-preview",
        &serde_json::json!({ "runId": "serve-run", "flowIrPath": artifact }),
    );
    assert_eq!(status, 200, "{preview}");
    let entries = preview["report"]["entries"].as_array().expect("entries");
    let read_back = entries
        .iter()
        .find(|entry| entry["stepId"] == "read_back")
        .expect("read_back entry");
    assert_eq!(read_back["class"], "judgeDirty", "{preview}");

    // Read-only discipline: the preview appended nothing to the ledger.
    let revision_after = host.get_json("/api/runs/serve-run/revision")["revision"]
        .as_u64()
        .expect("revision");
    assert_eq!(revision_before, revision_after, "the preview never writes");
}

/// Retargets the mutating `set_ssid` step, so its effectHash moves and the
/// 07 §5.4 gate fires on resume.
fn mutating_edited_flow(dir: &TempDir) -> PathBuf {
    let original = std::fs::read_to_string(demo_flow_path()).expect("read demo yaml");
    let edited = original.replace(
        "        element: ssid_field\n        value: ${{ params.ssid }}",
        "        element: ssid_field_v2\n        value: ${{ params.ssid }}",
    );
    assert_ne!(original, edited, "the mutating edit must land");
    let path = dir.file("wifi-demo-mutating-edit.flow.yaml");
    std::fs::write(&path, edited).expect("write edited yaml");
    path
}

/// 07 §5.4 through the host: the gate reaches the UI, and the UI can
/// release it. Before this the endpoint forwarded no authorization at all,
/// so a preview could show the gate and the operator then had to leave the
/// UI for a terminal to actually resume (08 §6.4).
#[test]
fn repair_resume_forwards_the_mutating_reexec_authorization() {
    let dir = TempDir::new("repair-gate");
    let artifacts = dir.file("artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let lockfile = dir.file("devicerail.lock.json");
    let flow_ir = artifacts.join("wifi-demo.flow.ir.json");
    let store = dir.file("store");

    assert_exit(
        &pointlock(&[
            "lock",
            "--provider",
            "fake",
            "--out",
            lockfile.to_str().unwrap(),
        ]),
        0,
        "lock",
    );
    assert_exit(
        &pointlock(&[
            "compile",
            demo_flow_path().to_str().unwrap(),
            "--lockfile",
            lockfile.to_str().unwrap(),
            "--out",
            flow_ir.to_str().unwrap(),
        ]),
        0,
        "compile",
    );
    // set_ssid ran, passed, and is `effect: mutating` without
    // `idempotent: true` — its effect is in the world.
    assert_exit(
        &pointlock(&[
            "run",
            flow_ir.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--param",
            "ssid=HomeWifi",
            "--run-id",
            "gate-run",
            "--stop-after",
            "set_ssid",
        ]),
        3,
        "run suspended",
    );

    let host = ServeHost::boot(&store, &artifacts);
    let edited = mutating_edited_flow(&dir);
    let (status, compiled) = host.post_json(
        "/api/repair/compile",
        &serde_json::json!({
            "flowPath": edited.to_str().unwrap(),
            "lockfilePath": lockfile.to_str().unwrap(),
        }),
    );
    assert_eq!(status, 200, "{compiled}");
    assert_eq!(compiled["ok"], true, "{compiled}");
    let artifact = compiled["artifact"].as_str().expect("artifact path");

    // The preview names the gate AND the step id the authorization takes —
    // without the id the UI would have to re-parse the run path.
    let (status, preview) = host.post_json(
        "/api/repair/align-preview",
        &serde_json::json!({
            "runId": "gate-run",
            "flowIrPath": artifact,
            "lockfilePath": lockfile.to_str().unwrap(),
        }),
    );
    assert_eq!(status, 200, "{preview}");
    let gates = preview["report"]["requiresConfirmation"]
        .as_array()
        .expect("requiresConfirmation array");
    assert_eq!(gates.len(), 1, "{preview}");
    assert_eq!(gates[0]["cause"], "mutatingReexec", "{preview}");
    assert_eq!(gates[0]["stepId"], "set_ssid", "{preview}");

    // Unauthorized: the resume refuses, exactly as the CLI does.
    let (status, refused) = host.post_json(
        "/api/repair/resume",
        &serde_json::json!({
            "runId": "gate-run",
            "flowIrPath": artifact,
        }),
    );
    assert_eq!(status, 200, "{refused}");
    assert_eq!(refused["exitCode"], 3, "{refused}");
    let stderr = refused["stderr"].as_str().unwrap_or_default();
    assert!(
        stderr.contains("requires confirmation") && stderr.contains("set_ssid"),
        "the refusal must name the gated step: {stderr}"
    );

    // Authorized BY NAME: the same request with the id goes through.
    let (status, allowed) = host.post_json(
        "/api/repair/resume",
        &serde_json::json!({
            "runId": "gate-run",
            "flowIrPath": artifact,
            "allowMutatingReexec": ["set_ssid"],
        }),
    );
    assert_eq!(status, 200, "{allowed}");
    assert_ne!(
        allowed["exitCode"], 3,
        "the authorized resume must not refuse again: {allowed}"
    );
    let stderr = allowed["stderr"].as_str().unwrap_or_default();
    assert!(
        !stderr.contains("requires confirmation"),
        "the gate must be released: {stderr}"
    );
}

// ─── lockfileDigest staleness comparison (08 §2.2) ──────────────────────────

#[test]
fn flow_list_carries_the_current_lockfile_digest_for_comparison() {
    let dir = TempDir::new("lock-compare");
    let (store, artifacts) = prepare_run(&dir);
    let lockfile = dir.file("devicerail.lock.json");

    // With --lockfile the read side gets the comparison input, and it
    // matches the artifact's embedded digest (same lockfile compiled it).
    let host = ServeHost::boot_with(
        &store,
        &artifacts,
        &["--lockfile", lockfile.to_str().unwrap()],
    );
    let flows = host.get_json("/api/flows");
    let current = flows["currentLockfileDigest"]
        .as_str()
        .expect("comparison digest present");
    assert!(current.starts_with("sha256:"));
    assert_eq!(
        flows["flows"][0]["versions"][0]["lockfileDigest"], current,
        "the artifact was compiled against this very lockfile"
    );
    drop(host);

    // Negative control: without --lockfile there is NO comparison input
    // — the field is null, never a fabricated judgment.
    let bare = ServeHost::boot(&store, &artifacts);
    let flows = bare.get_json("/api/flows");
    assert!(
        flows["currentLockfileDigest"].is_null(),
        "no --lockfile → no staleness judgment: {flows}"
    );
}

// ─── callee graph lazy-load by irHash (08 §3.3 expansion) ───────────────────

#[test]
fn callee_graph_is_served_from_the_bundle_pool_by_ir_hash() {
    let dir = TempDir::new("callee-graph");
    let artifacts = dir.file("artifacts");
    std::fs::create_dir_all(&artifacts).expect("create artifacts dir");
    std::fs::write(
        dir.file("main.flow.yaml"),
        r#"flow: expansion_demo
provider: devicerail
steps:
  - id: warmup
    invoke:
      action: tapElement
      args:
        element: warm_row
    effect: mutating
    idempotent: true
  - id: buy
    call: ./callee.flow.yaml
    inputs: {}
"#,
    )
    .expect("write main");
    std::fs::write(
        dir.file("callee.flow.yaml"),
        r#"flow: checkout_probe
provider: devicerail
steps:
  - id: probe
    invoke:
      action: findElement
      args:
        element: cart_badge
    effect: readonly
"#,
    )
    .expect("write callee");
    let lockfile = dir.file("fake.lock.json");
    assert_exit(
        &pointlock(&[
            "lock",
            "--provider",
            "fake",
            "--out",
            lockfile.to_str().unwrap(),
        ]),
        0,
        "lock",
    );
    let bundle = artifacts.join("expansion-demo.ir.json");
    assert_exit(
        &pointlock(&[
            "compile",
            dir.file("main.flow.yaml").to_str().unwrap(),
            "--provider",
            "fake",
            "--lockfile",
            lockfile.to_str().unwrap(),
            "--out",
            bundle.to_str().unwrap(),
        ]),
        0,
        "compile the bundle",
    );
    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bundle).expect("read bundle"))
            .expect("bundle JSON");
    let callee_hash = artifact["subflows"][0]["irHash"]
        .as_str()
        .expect("callee irHash")
        .to_owned();

    let store = dir.file("store");
    std::fs::create_dir_all(&store).expect("store dir");
    let host = ServeHost::boot(&store, &artifacts);

    // The callee is not a root artifact — but the irHash-pinned request
    // resolves it from the bundle pool (the expansion's lazy-load path).
    let detail = host.get_json(&format!("/api/flows/checkout_probe?irHash={callee_hash}"));
    assert_eq!(detail["graph"]["flowId"], "checkout_probe");
    assert_eq!(detail["graph"]["nodes"][0]["id"], "probe");
    assert_eq!(detail["versions"].as_array().expect("versions").len(), 0);

    // Negative control: without the irHash pin the callee stays
    // invisible (it is not a root flow).
    let (status, _) = host.request(
        "GET",
        &format!("/api/flows/checkout_probe?token={}", host.token),
    );
    assert_eq!(status, 404, "a callee is not a root artifact");
}
