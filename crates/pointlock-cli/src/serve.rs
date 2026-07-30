//! `pointlock inspect --serve` — the projection protocol's HTTP+JSON
//! canonical form (spine §10.4; endpoint table 08 §5).
//!
//! The four iron laws of 08 §1 shape everything here:
//! 1. read-only projection — every response is a projection DTO (or a
//!    thin envelope composing them); the server never judges;
//! 2. no daemon contact — evidence bytes come from the local
//!    content-addressed store only;
//! 3. the write surface is EXACTLY the CLI equivalents (08 §2.7): the
//!    three `/api/repair/*` endpoints spawn this very binary
//!    (`compile` / `resume`) or call the runner's read-only alignment
//!    preview — the UI has no capability the terminal lacks;
//!    `/api/inbox/:id/respond` stays the reserved v0.2 `webUi` collect
//!    channel (06 §4.2) with a typed 501;
//! 4. loopback single-user: the listener binds `127.0.0.1` only and
//!    every request must carry the startup-printed random token — the
//!    endpoint is a temporary capability, not a service.
//!
//! Transport neutrality (spine §10.4): SSE pushes ONLY `{revision}`
//! invalidation ticks; data always travels through the pull endpoints,
//! so 2s polling of `/api/runs/:id/revision` is exactly equivalent.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use percent_encoding::percent_decode_str;
use pointlock_ir::FlowIR;
use pointlock_store::{Store, StoreError};
use serde::Serialize;
use serde_json::json;

use crate::{Failure, exit};

// ─── Configuration ──────────────────────────────────────────────────────────

/// Inputs of the serve loop.
pub struct ServeConfig {
    /// Store directory (RunLog + checkpoint + evidence).
    pub store_dir: PathBuf,
    /// The CURRENT capability lockfile, when supplied: the flow list
    /// compares each artifact's embedded digest against its digest and
    /// the UI flags staleness (08 §2.2).
    pub lockfile_path: Option<PathBuf>,
    /// Compile-artifact directory to scan for FlowIR files (08 §2.1 flow
    /// list; also supplies dossier IR nodes). Optional — without it the
    /// flow index is empty and dossiers carry the run record only.
    pub artifacts_dir: Option<PathBuf>,
    /// Port to bind on `127.0.0.1` (0 = ephemeral).
    pub port: u16,
    /// Built SPA directory (`@pointlock/ui` dist) to serve at `/`. The
    /// shell is public code and rides token-free; every DATA request
    /// (/api, /evidence) stays token-gated.
    pub ui_dir: Option<PathBuf>,
    /// Vision verifier of the host's repair surfaces: align-preview
    /// re-judges vision tails with it, and the self-exec resume forwards
    /// the flag — the UI-triggered closure must match a terminal
    /// `pointlock resume --vision ...` segment.
    pub vision: crate::VisionArg,
}

struct ServeCtx {
    store_dir: PathBuf,
    artifacts_dir: Option<PathBuf>,
    ui_dir: Option<PathBuf>,
    /// The current lockfile's digest, loaded once at startup.
    current_lockfile_digest: Option<String>,
    token: String,
    vision_arg: crate::VisionArg,
    vision: Option<std::sync::Arc<dyn pointlock_vision::VisionVerifier>>,
}

// ─── Entry ──────────────────────────────────────────────────────────────────

/// Runs the host until the process is killed. Prints the tokened URL on
/// startup (08 §1 iron law 4).
pub fn serve(config: ServeConfig) -> Result<i32, Failure> {
    // Fail early on an unopenable store, before binding.
    Store::open(&config.store_dir)
        .map_err(|err| Failure::new(exit::INTERNAL, format!("store error: {err}")))?;
    // Same pre-I/O usage guard as `run`/`resume`: `--vision anthropic`
    // without a key fails fast, before binding.
    let vision = crate::commands::vision_verifier(config.vision)?;
    // Load the comparison lockfile once — a bad path is a startup error,
    // never a silently digest-less flow list.
    let current_lockfile_digest = config
        .lockfile_path
        .as_deref()
        .map(crate::commands::load_lockfile)
        .transpose()?
        .map(|lockfile| lockfile.digest.to_string());

    let server = tiny_http::Server::http(("127.0.0.1", config.port))
        .map_err(|err| Failure::new(exit::INTERNAL, format!("bind 127.0.0.1: {err}")))?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|addr| addr.port())
        .unwrap_or(config.port);
    let ctx = Arc::new(ServeCtx {
        store_dir: config.store_dir,
        artifacts_dir: config.artifacts_dir,
        ui_dir: config.ui_dir,
        current_lockfile_digest,
        token: uuid::Uuid::new_v4().simple().to_string(),
        vision_arg: config.vision,
        vision,
    });

    println!("pointlock projection host (spine §10.4 canonical form)");
    if ctx.ui_dir.is_some() {
        println!("  http://127.0.0.1:{port}/?token={}", ctx.token);
    }
    println!("  http://127.0.0.1:{port}/api/flows?token={}", ctx.token);
    println!("  (the token is a temporary capability; the listener is loopback-only)");

    for request in server.incoming_requests() {
        let ctx = Arc::clone(&ctx);
        std::thread::spawn(move || handle(request, &ctx));
    }
    Ok(exit::PASS)
}

// ─── Request plumbing ───────────────────────────────────────────────────────

/// One materialized non-streaming reply.
struct Reply {
    status: u16,
    content_type: String,
    body: Vec<u8>,
    /// Extra response headers (evidence hardening — see [`evidence`]).
    extra_headers: Vec<(&'static str, &'static str)>,
}

impl Reply {
    fn json(status: u16, value: &impl Serialize) -> Reply {
        let mut body = serde_json::to_vec_pretty(value).unwrap_or_else(|_| b"{}".to_vec());
        body.push(b'\n');
        Reply {
            status,
            content_type: "application/json".to_owned(),
            body,
            extra_headers: Vec::new(),
        }
    }

    fn error(status: u16, message: impl Into<String>) -> Reply {
        Reply::json(status, &json!({ "error": message.into() }))
    }
}

fn handle(mut request: tiny_http::Request, ctx: &ServeCtx) {
    let url = request.url().to_owned();
    let (path, query) = split_url(&url);

    // The SPA shell (static assets) is public code, served token-free;
    // everything it can DO still goes through the gated data plane.
    if matches!(request.method(), tiny_http::Method::Get)
        && let Some(ui_dir) = &ctx.ui_dir
        && !path.starts_with("/api/")
        && !path.starts_with("/evidence/")
    {
        respond_reply(request, static_file(ui_dir, &path));
        return;
    }

    // Token gate first — the URL is the capability (08 §1 iron law 4).
    if query.get("token") != Some(&ctx.token) {
        respond_reply(
            request,
            Reply::error(401, "missing or wrong token; use the startup-printed URL"),
        );
        return;
    }

    let is_get = matches!(request.method(), tiny_http::Method::Get);

    // SSE endpoints stream from the request thread; everything else
    // materializes a Reply.
    if is_get && path == "/api/inbox/stream" {
        stream_revisions(request, ctx, None);
        return;
    }
    if is_get
        && let Some(run_id) = path
            .strip_prefix("/api/runs/")
            .and_then(|rest| rest.strip_suffix("/stream"))
        && !run_id.contains('/')
    {
        // Decoded like every pull route; an unknown run answers 404 up
        // front instead of a forever-silent 200 (SSE must stay exactly
        // equivalent to polling — 08 §5).
        let run_id = decode(run_id);
        if let Err(err) = Store::open(&ctx.store_dir).and_then(|store| store.revision(&run_id)) {
            respond_reply(request, store_reply(err));
            return;
        }
        stream_revisions(request, ctx, Some(run_id));
        return;
    }

    let reply = if is_get {
        route_get(ctx, &path, &query)
    } else if matches!(request.method(), tiny_http::Method::Post)
        && path.starts_with("/api/repair/")
    {
        match read_body(&mut request) {
            Ok(body) => route_repair(ctx, &path, &body),
            Err(reply) => reply,
        }
    } else {
        route_non_get(&path)
    };
    respond_reply(request, reply);
}

/// Reads a bounded JSON request body (1 MiB cap — local console traffic).
fn read_body(request: &mut tiny_http::Request) -> Result<serde_json::Value, Reply> {
    use std::io::Read as _;
    let mut raw = String::new();
    let mut reader = request.as_reader().take(1024 * 1024);
    if reader.read_to_string(&mut raw).is_err() {
        return Err(Reply::error(400, "unreadable request body"));
    }
    serde_json::from_str(&raw).map_err(|err| Reply::error(400, format!("body is not JSON: {err}")))
}

/// Only a conservative ASCII subset may travel as a Content-Type value:
/// stored media types are provider input, and CR/LF (header injection)
/// or non-ASCII (tiny_http refusal) must never reach the wire.
fn safe_content_type(media_type: &str) -> &str {
    let ok = !media_type.is_empty()
        && media_type.len() <= 200
        && media_type
            .bytes()
            .all(|byte| (0x20..=0x7e).contains(&byte) && byte != b'"');
    if ok {
        media_type
    } else {
        "application/octet-stream"
    }
}

fn respond_reply(request: tiny_http::Request, reply: Reply) {
    let content_type = safe_content_type(&reply.content_type).to_owned();
    let mut response = tiny_http::Response::from_data(reply.body)
        .with_status_code(tiny_http::StatusCode(reply.status))
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
                .expect("sanitized ascii header"),
        );
    for (name, value) in reply.extra_headers {
        if let Ok(header) = tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response = response.with_header(header);
        }
    }
    let _ = request.respond(response);
}

/// Serves one SPA file: `/` → `index.html`; every other path resolves
/// strictly INSIDE the ui dir (decoded segments; any `..`, absolute, or
/// empty segment is refused — the dir is the boundary).
fn static_file(ui_dir: &Path, path: &str) -> Reply {
    let relative = if path == "/" {
        "index.html"
    } else {
        &path[1..]
    };
    let mut resolved = ui_dir.to_path_buf();
    for segment in relative.split('/') {
        let segment = decode(segment);
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains(['/', '\\'])
        {
            return Reply::error(404, "no such asset");
        }
        resolved.push(segment);
    }
    match std::fs::read(&resolved) {
        Ok(bytes) => Reply {
            status: 200,
            content_type: mime_of(&resolved).to_owned(),
            body: bytes,
            extra_headers: vec![("X-Content-Type-Options", "nosniff")],
        },
        Err(_) => Reply::error(404, "no such asset"),
    }
}

fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json" | "map") => "application/json",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Splits a request URL into its decoded path and query map.
fn split_url(url: &str) -> (String, BTreeMap<String, String>) {
    let (path, query) = match url.split_once('?') {
        Some((path, query)) => (path, query),
        None => (url, ""),
    };
    let mut map = BTreeMap::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(decode(key), decode(value));
    }
    (path.to_owned(), map)
}

fn decode(text: &str) -> String {
    percent_decode_str(text)
        .decode_utf8()
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| text.to_owned())
}

// ─── Routing ────────────────────────────────────────────────────────────────

/// The write surface: typed refusals until the wave that owns each lands
/// (iron law 3 — never silently absent).
fn route_non_get(path: &str) -> Reply {
    if path.starts_with("/api/repair/") {
        return Reply::error(405, "repair endpoints are POST with a JSON body");
    }
    if path.starts_with("/api/inbox/") && path.ends_with("/respond") {
        return Reply::error(
            501,
            "the webUi collect channel is reserved for v0.2 (06 §4.2); respond via pointlock-human-cli",
        );
    }
    Reply::error(405, "read-only projection host: GET only")
}

fn route_get(ctx: &ServeCtx, path: &str, query: &BTreeMap<String, String>) -> Reply {
    let store = match Store::open(&ctx.store_dir) {
        Ok(store) => store,
        Err(err) => return Reply::error(500, format!("store error: {err}")),
    };

    if path.starts_with("/api/repair/") {
        return Reply::error(405, "repair endpoints are POST with a JSON body");
    }
    if path == "/api/flows" {
        return flows_index(ctx, &store);
    }
    if let Some(flow_id) = path.strip_prefix("/api/flows/")
        && !flow_id.is_empty()
        && !flow_id.contains('/')
    {
        return flow_detail(ctx, &store, &decode(flow_id), query.get("irHash"));
    }
    if path == "/api/runs" {
        return runs_index(&store, query.get("flowId"));
    }
    if let Some(rest) = path.strip_prefix("/api/runs/") {
        if let Some((run_id, tail)) = rest.split_once('/') {
            let run_id = decode(run_id);
            return match tail {
                "timeline" => timeline(&store, &run_id, query),
                "revision" => revision(&store, &run_id),
                _ => match tail.strip_prefix("steps/") {
                    Some(encoded) => dossier(ctx, &store, &run_id, &decode(encoded)),
                    None => Reply::error(404, format!("no such endpoint: {path}")),
                },
            };
        }
        if !rest.is_empty() {
            return overview(&store, &decode(rest));
        }
    }
    if path == "/api/inbox" {
        return inbox(&store);
    }
    if path == "/api/inbox/revision" {
        return match store.global_revision() {
            Ok(revision) => Reply::json(
                200,
                &json!({ "projectionVersion": 1, "revision": revision }),
            ),
            Err(err) => store_reply(err),
        };
    }
    if let Some(sha256) = path.strip_prefix("/evidence/")
        && !sha256.is_empty()
    {
        return evidence(&store, sha256);
    }
    Reply::error(404, format!("no such endpoint: {path}"))
}

// ─── The repair closure (08 §2.7): three CLI-equivalent write actions ───────

fn body_str<'a>(body: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(|value| value.as_str())
}

/// One resume in flight per run: the ledger is a single-writer story and
/// a double-click must not double-dispatch device effects. Process-wide
/// (the host is the only UI writer on this store).
fn resume_locks() -> &'static std::sync::Mutex<std::collections::BTreeSet<String>> {
    static LOCKS: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
        std::sync::OnceLock::new();
    LOCKS.get_or_init(Default::default)
}

/// Spawns THIS binary (the repair actions are exactly the CLI commands —
/// 08 §1 iron law 3) and captures its output.
fn run_self(args: &[&str]) -> Result<std::process::Output, Reply> {
    let exe = std::env::current_exe()
        .map_err(|err| Reply::error(500, format!("cannot resolve the pointlock binary: {err}")))?;
    std::process::Command::new(exe)
        .args(args)
        .output()
        .map_err(|err| Reply::error(500, format!("spawn pointlock: {err}")))
}

fn route_repair(ctx: &ServeCtx, path: &str, body: &serde_json::Value) -> Reply {
    match path {
        "/api/repair/compile" => repair_compile(ctx, body),
        "/api/repair/align-preview" => repair_align_preview(ctx, body),
        "/api/repair/resume" => repair_resume(ctx, body),
        _ => Reply::error(404, format!("no such repair action: {path}")),
    }
}

/// `POST /api/repair/compile` — the "重编译" leg: spawns
/// `pointlock compile <flow> --out <artifact> --format json`. Compile
/// diagnostics are a RESULT (`ok: false` + the array), not an HTTP error.
fn repair_compile(ctx: &ServeCtx, body: &serde_json::Value) -> Reply {
    let Some(flow_path) = body_str(body, "flowPath") else {
        return Reply::error(400, "body needs flowPath (the *.flow.yaml to compile)");
    };
    let out_path = match body_str(body, "outPath") {
        Some(out) => PathBuf::from(out),
        None => match &ctx.artifacts_dir {
            Some(dir) => {
                let stem = Path::new(flow_path)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("repaired");
                dir.join(format!("{stem}.ir.json"))
            }
            None => {
                return Reply::error(
                    400,
                    "no outPath and the host has no --artifacts dir to write into",
                );
            }
        },
    };
    let out_str = out_path.display().to_string();
    // Flags first, positional after `--`: a caller path starting with a
    // dash must never parse as a flag.
    let mut args = vec!["compile", "--out", &out_str, "--format", "json"];
    if let Some(lockfile) = body_str(body, "lockfilePath") {
        args.extend(["--lockfile", lockfile]);
    }
    if let Some(provider) = body_str(body, "provider") {
        args.extend(["--provider", provider]);
    }
    args.extend(["--", flow_path]);
    let output = match run_self(&args) {
        Ok(output) => output,
        Err(reply) => return reply,
    };
    if output.status.success() {
        let ir_hash = std::fs::read_to_string(&out_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|value| {
                value
                    .get("irHash")
                    .or_else(|| value.get("root").and_then(|root| root.get("irHash")))
                    .and_then(|hash| hash.as_str())
                    .map(str::to_owned)
            });
        return Reply::json(
            200,
            &json!({ "ok": true, "artifact": out_str, "irHash": ir_hash }),
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<serde_json::Value>(&stdout) {
        Ok(diagnostics) if diagnostics.is_array() => {
            Reply::json(200, &json!({ "ok": false, "diagnostics": diagnostics }))
        }
        _ => Reply::error(
            500,
            format!(
                "compile failed without JSON diagnostics: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ),
    }
}

/// `POST /api/repair/align-preview` — the read-only preview (08 §2.7):
/// the runner's classification with no session, no writes, no promise.
/// `lockfilePath` (the SAME lockfile the run uses) supplies
/// `env.platform` so offline re-judgement matches the real resume.
fn repair_align_preview(ctx: &ServeCtx, body: &serde_json::Value) -> Reply {
    let (Some(run_id), Some(flow_ir_path)) =
        (body_str(body, "runId"), body_str(body, "flowIrPath"))
    else {
        return Reply::error(400, "body needs runId and flowIrPath");
    };
    let (flow, subflow_list) =
        match crate::commands::load_artifact_for_serve(Path::new(flow_ir_path)) {
            Ok(loaded) => loaded,
            Err(failure) => return Reply::error(400, failure.message),
        };
    let subflows: BTreeMap<pointlock_ir::Hash, FlowIR> = subflow_list
        .into_iter()
        .map(|callee| (callee.ir_hash.clone(), callee))
        .collect();
    let platform = match body_str(body, "lockfilePath") {
        Some(lockfile_path) => match crate::commands::load_lockfile(Path::new(lockfile_path)) {
            Ok(lockfile) => Some(crate::commands::wire_str(&lockfile.device.platform)),
            Err(failure) => return Reply::error(400, failure.message),
        },
        None => None,
    };
    let store = match Store::open(&ctx.store_dir) {
        Ok(store) => store,
        Err(err) => return store_reply(err),
    };
    // A mid-flight snapshot previews a resume that cannot happen — the
    // repair narrative starts at fail/suspend (08 §2.7).
    match store.run_status(run_id) {
        Ok(pointlock_store::RunStatus::Running) => {
            return Reply::error(409, "the run is still running; preview after it suspends");
        }
        Ok(_) => {}
        Err(err) => return store_reply(err),
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return Reply::error(500, format!("tokio runtime: {err}")),
    };
    // 07 §5.3: the preview classifies the same steps dirty the real resume
    // will, so the forced list rides along; authorization does not (a
    // preview releases nothing).
    let force_reexec: Vec<String> = body
        .get("forceReexecute")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    // The optional old-IR (07 §5.3 preflight-only sub-domain comparison):
    // the preview classifies exactly as a `resume --old-ir` would.
    let old_flow_ir: Option<pointlock_ir::FlowIR> = match body_str(body, "oldIrPath") {
        None => None,
        Some(path) => match std::fs::read_to_string(path)
            .map_err(|err| err.to_string())
            .and_then(|raw| serde_json::from_str(&raw).map_err(|err| err.to_string()))
        {
            Ok(flow) => Some(flow),
            Err(err) => return Reply::error(400, format!("oldIrPath unreadable: {err}")),
        },
    };
    match runtime.block_on(pointlock_runner::Runner::align_preview(
        &flow,
        &subflows,
        run_id,
        &store,
        platform.as_deref(),
        ctx.vision.as_deref(),
        &force_reexec,
        old_flow_ir.as_ref(),
    )) {
        Ok(report) => {
            let rendered = report
                .resume_point
                .as_ref()
                .map(|path| pointlock_ir::render_run_path(path));
            Reply::json(
                200,
                &json!({
                    "projectionVersion": 1,
                    "runId": run_id,
                    "report": report,
                    "resumePointRendered": rendered,
                }),
            )
        }
        Err(pointlock_runner::RunnerError::M0Unsupported { detail }) => Reply::error(400, detail),
        Err(pointlock_runner::RunnerError::Store(err)) => store_reply(err),
        Err(err) => Reply::error(500, err.to_string()),
    }
}

/// `POST /api/repair/resume` — the "从 checkpoint 继续" leg: spawns
/// `pointlock resume`. The exit code IS the outcome (the crate's exit
/// table); the run's progress streams through the revision channel.
fn repair_resume(ctx: &ServeCtx, body: &serde_json::Value) -> Reply {
    let (Some(run_id), Some(flow_ir_path)) =
        (body_str(body, "runId"), body_str(body, "flowIrPath"))
    else {
        return Reply::error(400, "body needs runId and flowIrPath");
    };

    // A running run must not grow a second writer segment (the ledger is
    // a single-writer story); a double-click must not double-dispatch.
    match Store::open(&ctx.store_dir).and_then(|store| store.run_status(run_id)) {
        Ok(pointlock_store::RunStatus::Running) => {
            return Reply::error(
                409,
                "the run is still running; resume applies after it suspends",
            );
        }
        Ok(_) => {}
        Err(err) => return store_reply(err),
    }
    {
        let mut locks = resume_locks().lock().expect("resume lock poisoned");
        if !locks.insert(run_id.to_owned()) {
            return Reply::error(409, "a resume for this run is already in flight");
        }
    }
    let unlock = |run_id: &str| {
        resume_locks()
            .lock()
            .expect("resume lock poisoned")
            .remove(run_id);
    };

    let store_str = ctx.store_dir.display().to_string();
    // Flags first, positional after `--`: a caller path starting with a
    // dash must never parse as a flag.
    let mut args = vec!["resume", "--store", &store_str, "--run", run_id];
    if ctx.vision_arg == crate::VisionArg::Anthropic {
        // The UI-triggered segment must not silently degrade relative to
        // a terminal `pointlock resume --vision anthropic` (per-segment
        // semantics; the same reason the resume hint carries the flag).
        args.extend(["--vision", "anthropic"]);
    }
    if let Some(lockfile) = body_str(body, "lockfilePath") {
        args.extend(["--lockfile", lockfile]);
    }
    if let Some(provider) = body_str(body, "provider") {
        args.extend(["--provider", provider]);
    }
    if let Some(daemon_cmd) = body_str(body, "daemonCmd") {
        args.extend(["--daemon-cmd", daemon_cmd]);
    }
    let daemon_env: Vec<String> = body
        .get("daemonEnv")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    for item in &daemon_env {
        args.extend(["--daemon-env", item]);
    }
    // 07 §5.4 step 2: the gate is released by naming steps ONE BY ONE, so
    // the endpoint forwards a list of ids and never a "release everything"
    // switch — a wildcard here would be the same wildcard the CLI refuses.
    // Without this the UI could preview a gated resume and then had to send
    // the operator back to a terminal to actually run it (08 §6.4).
    let allow_reexec: Vec<String> = body
        .get("allowMutatingReexec")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    for step_id in &allow_reexec {
        args.extend(["--allow-mutating-reexec", step_id]);
    }
    let force_reexec: Vec<String> = body
        .get("forceReexecute")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    for step_id in &force_reexec {
        args.extend(["--force-reexecute", step_id]);
    }
    args.extend(["--", flow_ir_path]);
    let output = match run_self(&args) {
        Ok(output) => output,
        Err(reply) => {
            unlock(run_id);
            return reply;
        }
    };
    unlock(run_id);
    Reply::json(
        200,
        &json!({
            "exitCode": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        }),
    )
}

// ─── Store-error mapping ────────────────────────────────────────────────────

fn store_reply(err: StoreError) -> Reply {
    match err {
        StoreError::UnknownRun(_)
        | StoreError::UnknownStepInstance { .. }
        | StoreError::NoCheckpoint(_) => Reply::error(404, err.to_string()),
        StoreError::AmbiguousStep { .. } | StoreError::BadRunPath { .. } => {
            Reply::error(400, err.to_string())
        }
        other => Reply::error(500, other.to_string()),
    }
}

// ─── Artifact scanning (08 §2.1: the flow list's data source) ───────────────

/// One scanned artifact version.
struct ScannedArtifact {
    flow: FlowIR,
    file: PathBuf,
    modified_at_ms: u64,
}

/// Scans the artifact directory for FlowIR files (bare or bundle roots;
/// bundle subflows join the pool for dossier IR resolution but are not
/// flow-list versions of their own).
fn scan_artifacts(dir: &Path) -> (Vec<ScannedArtifact>, Vec<FlowIR>) {
    let mut roots = Vec::new();
    let mut pool = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (roots, pool);
    };
    for entry in entries.flatten() {
        let file = entry.path();
        if file.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(loaded) = crate::commands::load_artifact_for_serve(&file) else {
            continue; // not a FlowIR artifact — the scan is best-effort
        };
        let modified_at_ms = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        pool.push(loaded.0.clone());
        pool.extend(loaded.1.clone());
        roots.push(ScannedArtifact {
            flow: loaded.0,
            file,
            modified_at_ms,
        });
    }
    (roots, pool)
}

fn artifact_pool(ctx: &ServeCtx) -> Vec<FlowIR> {
    match &ctx.artifacts_dir {
        Some(dir) => scan_artifacts(dir).1,
        None => Vec::new(),
    }
}

// ─── Endpoint bodies ────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionEntry {
    ir_hash: String,
    /// The IR-embedded lockfile digest (08 §2.2: the flow list flags a
    /// mismatch against the CURRENT lockfile — the comparison input is
    /// the host's `--lockfile`, surfaced as `currentLockfileDigest`).
    lockfile_digest: String,
    file: String,
    modified_at_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunIndexEntry {
    run_id: String,
    status: String,
    created_at_ms: u64,
    ir_hash: String,
    device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    flow_verdict_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flow_verdict_degraded: Option<bool>,
}

/// Builds the per-flow run rows (verdict strip — 08 §2.2) from the
/// overview projection; two-digit run counts make the per-run fold fine
/// (principle 10).
fn run_rows(store: &Store, flow_id: Option<&str>) -> Result<Vec<RunIndexEntry>, StoreError> {
    let mut rows = Vec::new();
    for run in store.list_runs()? {
        if flow_id.is_some_and(|wanted| wanted != run.flow_id) {
            continue;
        }
        let overview = pointlock_store::projection::run_overview(store, &run.run_id)?;
        rows.push(RunIndexEntry {
            run_id: run.run_id,
            status: overview.status,
            created_at_ms: overview.created_at_ms,
            ir_hash: overview.ir_hash,
            device_id: overview.device_id,
            flow_verdict_status: overview.flow_verdict_status,
            flow_verdict_degraded: overview.flow_verdict_degraded,
        });
    }
    Ok(rows)
}

fn flows_index(ctx: &ServeCtx, store: &Store) -> Reply {
    let roots = match &ctx.artifacts_dir {
        Some(dir) => scan_artifacts(dir).0,
        None => Vec::new(),
    };
    // Group versions by flowId; latest = newest file mtime.
    let mut by_flow: BTreeMap<String, Vec<&ScannedArtifact>> = BTreeMap::new();
    for artifact in &roots {
        by_flow
            .entry(artifact.flow.flow_id.to_string())
            .or_default()
            .push(artifact);
    }
    // Flows can also exist purely as runs (artifact file moved away).
    let runs = match store.list_runs() {
        Ok(runs) => runs,
        Err(err) => return store_reply(err),
    };
    for run in &runs {
        by_flow.entry(run.flow_id.clone()).or_default();
    }

    let mut flows = Vec::new();
    for (flow_id, mut artifacts) in by_flow {
        artifacts.sort_by_key(|artifact| artifact.modified_at_ms);
        let versions: Vec<VersionEntry> = artifacts
            .iter()
            .map(|artifact| VersionEntry {
                ir_hash: artifact.flow.ir_hash.to_string(),
                lockfile_digest: artifact.flow.lockfile_digest.to_string(),
                file: artifact.file.display().to_string(),
                modified_at_ms: artifact.modified_at_ms,
            })
            .collect();
        let rows = match run_rows(store, Some(&flow_id)) {
            Ok(rows) => rows,
            Err(err) => return store_reply(err),
        };
        flows.push(json!({
            "flowId": flow_id,
            "latestIrHash": versions.last().map(|version| version.ir_hash.clone()),
            "versions": versions,
            "runs": rows,
        }));
    }
    Reply::json(
        200,
        &json!({
            "projectionVersion": 1,
            // 08 §2.2: the read-side comparison input — absent when the
            // host was started without --lockfile (the UI then shows the
            // digests without a staleness judgment; never a guess).
            "currentLockfileDigest": ctx.current_lockfile_digest,
            "flows": flows,
        }),
    )
}

fn flow_detail(ctx: &ServeCtx, store: &Store, flow_id: &str, ir_hash: Option<&String>) -> Reply {
    let roots = match &ctx.artifacts_dir {
        Some(dir) => scan_artifacts(dir).0,
        None => Vec::new(),
    };
    let mut versions: Vec<&ScannedArtifact> = roots
        .iter()
        .filter(|artifact| artifact.flow.flow_id.as_ref() == flow_id)
        .collect();
    versions.sort_by_key(|artifact| artifact.modified_at_ms);
    let selected = match ir_hash {
        Some(wanted) => versions
            .iter()
            .find(|artifact| artifact.flow.ir_hash.to_string() == *wanted)
            .copied(),
        None => versions.last().copied(),
    };
    let Some(selected) = selected else {
        // Callee flows live inside bundles, not as root artifacts: an
        // irHash-pinned miss falls back to the pool so the subflow
        // expansion (08 §3.3) can lazy-load the callee graph. The
        // version/run material is root-scoped and rides empty here.
        if let Some(wanted) = ir_hash {
            let pool = artifact_pool(ctx);
            if let Some(callee) = pool.iter().find(|flow| {
                flow.flow_id.as_ref() == flow_id && flow.ir_hash.to_string() == *wanted
            }) {
                let graph = pointlock_store::projection::flow_graph_view(callee);
                return Reply::json(
                    200,
                    &json!({
                        "projectionVersion": 1,
                        "flowId": flow_id,
                        "versions": [],
                        "graph": graph,
                        "runs": [],
                    }),
                );
            }
        }
        return Reply::error(
            404,
            format!("no artifact for flow '{flow_id}' (scan --artifacts, check --serve flags)"),
        );
    };
    let graph = pointlock_store::projection::flow_graph_view(&selected.flow);
    let rows = match run_rows(store, Some(flow_id)) {
        Ok(rows) => rows,
        Err(err) => return store_reply(err),
    };
    Reply::json(
        200,
        &json!({
            "projectionVersion": 1,
            "flowId": flow_id,
            "versions": versions
                .iter()
                .map(|artifact| json!({
                    "irHash": artifact.flow.ir_hash.to_string(),
                    "lockfileDigest": artifact.flow.lockfile_digest.to_string(),
                    "file": artifact.file.display().to_string(),
                    "modifiedAtMs": artifact.modified_at_ms,
                }))
                .collect::<Vec<_>>(),
            "graph": graph,
            "runs": rows,
        }),
    )
}

fn runs_index(store: &Store, flow_id: Option<&String>) -> Reply {
    match run_rows(store, flow_id.map(String::as_str)) {
        Ok(rows) => Reply::json(200, &json!({ "projectionVersion": 1, "runs": rows })),
        Err(err) => store_reply(err),
    }
}

fn overview(store: &Store, run_id: &str) -> Reply {
    match pointlock_store::projection::run_overview(store, run_id) {
        Ok(overview) => Reply::json(200, &overview),
        Err(err) => store_reply(err),
    }
}

fn timeline(store: &Store, run_id: &str, query: &BTreeMap<String, String>) -> Reply {
    let filter = match query.get("filter").map(String::as_str) {
        None | Some("all") => pointlock_store::projection::RunTimelineFilter::All,
        Some("observations") => pointlock_store::projection::RunTimelineFilter::Observations,
        Some("actions") => pointlock_store::projection::RunTimelineFilter::Actions,
        Some("errors") => pointlock_store::projection::RunTimelineFilter::Errors,
        Some("verdicts") => pointlock_store::projection::RunTimelineFilter::Verdicts,
        Some(other) => {
            return Reply::error(
                400,
                format!("unknown filter '{other}' (all|observations|actions|errors|verdicts)"),
            );
        }
    };
    let page = match parse_number(query.get("page"), 1) {
        Ok(page) => page,
        Err(reply) => return reply,
    };
    let page_size = match parse_number(query.get("pageSize"), 50) {
        Ok(size) => size,
        Err(reply) => return reply,
    };
    match pointlock_store::projection::timeline_page(store, run_id, filter, page, page_size) {
        Ok(page) => Reply::json(200, &page),
        Err(err) => store_reply(err),
    }
}

fn parse_number(raw: Option<&String>, default: u32) -> Result<u32, Reply> {
    match raw {
        None => Ok(default),
        Some(text) => text
            .parse::<u32>()
            .map_err(|_| Reply::error(400, format!("not a number: '{text}'"))),
    }
}

fn dossier(ctx: &ServeCtx, store: &Store, run_id: &str, step: &str) -> Reply {
    let path = match pointlock_store::projection::locate_step(store, run_id, step) {
        Ok(path) => path,
        Err(err) => return store_reply(err),
    };
    let artifacts = artifact_pool(ctx);
    match pointlock_store::projection::step_dossier(store, run_id, &path, &artifacts) {
        Ok(dossier) => Reply::json(200, &dossier),
        Err(err) => store_reply(err),
    }
}

fn revision(store: &Store, run_id: &str) -> Reply {
    match store.revision(run_id) {
        Ok(revision) => Reply::json(
            200,
            &json!({ "projectionVersion": 1, "runId": run_id, "revision": revision }),
        ),
        Err(err) => store_reply(err),
    }
}

fn inbox(store: &Store) -> Reply {
    match pointlock_store::projection::human_inbox(store) {
        Ok(entries) => Reply::json(200, &json!({ "projectionVersion": 1, "inbox": entries })),
        Err(err) => store_reply(err),
    }
}

/// The `/evidence/:sha256` byte route: content address in, bytes out
/// with the stored media type (08 §4.3 — the address is the ONLY key).
///
/// Evidence is captured DEVICE content — untrusted by definition. It is
/// served inert: `nosniff` pins the declared type and `CSP: sandbox`
/// strips scripting/origin powers on top-level renders, so an HTML/SVG
/// blob can never run on the capability origin and read the token
/// (`<img>` gallery loads are unaffected).
fn evidence(store: &Store, sha256: &str) -> Reply {
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Reply::error(400, "evidence key must be 64 lowercase hex chars");
    }
    match store.evidence_meta(sha256) {
        Ok(Some(meta)) => match std::fs::read(&meta.abs_path) {
            Ok(bytes) => Reply {
                status: 200,
                content_type: meta.media_type,
                body: bytes,
                extra_headers: vec![
                    ("X-Content-Type-Options", "nosniff"),
                    ("Content-Security-Policy", "sandbox"),
                ],
            },
            Err(err) => Reply::error(500, format!("evidence bytes unreadable: {err}")),
        },
        Ok(None) => Reply::error(404, format!("no evidence with sha256 {sha256}")),
        Err(err) => store_reply(err),
    }
}

// ─── SSE (revision invalidation only — 08 §5) ───────────────────────────────

/// Streams `data: {"revision":N}` ticks: one immediately, then one per
/// change, plus comment heartbeats. `run_id: None` = the inbox stream
/// (store-wide revision).
///
/// Uses the raw writer (`Request::into_writer`) instead of a chunked
/// `Response`: tiny_http buffers response bodies and only flushes at
/// EOF, which would hold invalidation ticks hostage — SSE needs a flush
/// per event. `Connection: close` makes the EOF-terminated body valid
/// HTTP/1.1.
fn stream_revisions(request: tiny_http::Request, ctx: &ServeCtx, run_id: Option<String>) {
    let mut writer = request.into_writer();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    if write_flush(&mut writer, head.as_bytes()).is_err() {
        return;
    }
    let mut last: Option<u64> = None;
    let mut next_heartbeat = Instant::now() + HEARTBEAT;
    loop {
        let revision = current_revision(ctx, run_id.as_deref());
        let payload = if let Some(revision) = revision.filter(|_| revision != last) {
            last = Some(revision);
            Some(format!("data: {{\"revision\":{revision}}}\n\n"))
        } else if Instant::now() >= next_heartbeat {
            next_heartbeat = Instant::now() + HEARTBEAT;
            Some(": ping\n\n".to_owned())
        } else {
            None
        };
        if let Some(payload) = payload
            && write_flush(&mut writer, payload.as_bytes()).is_err()
        {
            return; // the client went away — the capability expired
        }
        std::thread::sleep(POLL);
    }
}

const POLL: Duration = Duration::from_millis(250);
const HEARTBEAT: Duration = Duration::from_secs(15);

fn write_flush(writer: &mut (impl Write + ?Sized), bytes: &[u8]) -> std::io::Result<()> {
    writer.write_all(bytes)?;
    writer.flush()
}

fn current_revision(ctx: &ServeCtx, run_id: Option<&str>) -> Option<u64> {
    let store = Store::open(&ctx.store_dir).ok()?;
    match run_id {
        Some(run_id) => store.revision(run_id).ok(),
        None => store.global_revision().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_urls_and_decodes() {
        let (path, query) = split_url("/api/runs/r1/steps/demo%40aaaaaaaa%2Fstep_a?token=t&x=1");
        assert_eq!(path, "/api/runs/r1/steps/demo%40aaaaaaaa%2Fstep_a");
        assert_eq!(query.get("token").map(String::as_str), Some("t"));
        assert_eq!(decode("demo%40aaaaaaaa%2Fstep_a"), "demo@aaaaaaaa/step_a");
    }

    #[test]
    fn non_get_routes_answer_typed_refusals() {
        // Repair is live (W3b): a non-POST method on it is 405; the
        // v0.2-reserved webUi collect channel stays a typed 501.
        assert_eq!(route_non_get("/api/repair/compile").status, 405);
        assert_eq!(route_non_get("/api/inbox/req-1/respond").status, 501);
        assert_eq!(route_non_get("/api/flows").status, 405);
    }
}
