//! `AnthropicVisionVerifier` against a local canned-response server (the
//! `base_url` override): answer-protocol parsing, the fail-closed folds
//! (non-200, transport, chatty answers), and the request shape. No API
//! key, no network.

use std::sync::Arc;
use std::sync::Mutex;

use pointlock_ir::VerdictStatus;
use pointlock_vision::{AnthropicVisionVerifier, VisionRequest, VisionVerifier};

/// A one-shot local Messages endpoint: answers `body` with `status`,
/// records the request body it saw.
struct CannedServer {
    server: Arc<tiny_http::Server>,
    seen: Arc<Mutex<Vec<String>>>,
    url: String,
}

impl CannedServer {
    fn start(status: u16, body: impl Into<String>) -> CannedServer {
        let body: String = body.into();
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind canned server"));
        let url = format!(
            "http://127.0.0.1:{}",
            server.server_addr().to_ip().expect("ip").port()
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        {
            let server = Arc::clone(&server);
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                while let Ok(mut request) = server.recv() {
                    let mut raw = String::new();
                    let _ = request.as_reader().read_to_string(&mut raw);
                    seen.lock().expect("seen").push(raw);
                    let response = tiny_http::Response::from_string(body.clone())
                        .with_status_code(tiny_http::StatusCode(status));
                    let _ = request.respond(response);
                }
            });
        }
        CannedServer { server, seen, url }
    }

    fn verifier(&self) -> AnthropicVisionVerifier {
        AnthropicVisionVerifier::new("test-key", "claude-opus-4-8", &self.url)
    }
}

impl Drop for CannedServer {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

fn request<'a>() -> VisionRequest<'a> {
    VisionRequest {
        prompt: "the SSID field visibly contains the requested name",
        region: None,
        screenshot: b"fake-png-bytes",
        media_type: "image/png",
    }
}

fn messages_body(text: &str) -> String {
    serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "content": [
            { "type": "thinking", "thinking": "" },
            { "type": "text", "text": text },
        ],
        "stop_reason": "end_turn",
    })
    .to_string()
}

#[tokio::test]
async fn parses_the_three_verdict_lines() {
    for (answer, expected) in [
        ("PASS: the field shows HomeWifi", VerdictStatus::Pass),
        ("FAIL: the field is empty", VerdictStatus::Fail),
        ("UNKNOWN: the field is off-screen", VerdictStatus::Unknown),
    ] {
        let server = CannedServer::start(200, messages_body(answer));
        let verdict = server.verifier().verify(request()).await;
        assert_eq!(verdict.status, expected, "answer {answer:?}");
        assert!(verdict.reason.starts_with("vision: "), "{}", verdict.reason);
    }
}

#[tokio::test]
async fn chatty_answers_fold_to_unknown() {
    let server = CannedServer::start(
        200,
        messages_body("Sure! Looking at the screenshot, I believe it passes."),
    );
    let verdict = server.verifier().verify(request()).await;
    assert_eq!(verdict.status, VerdictStatus::Unknown);
    assert!(verdict.reason.contains("unparseable"), "{}", verdict.reason);
}

#[tokio::test]
async fn non_success_status_folds_to_unknown() {
    let server = CannedServer::start(429, r#"{"type":"error"}"#);
    let verdict = server.verifier().verify(request()).await;
    assert_eq!(verdict.status, VerdictStatus::Unknown);
    assert!(verdict.reason.contains("429"), "{}", verdict.reason);
}

#[tokio::test]
async fn transport_failure_folds_to_unknown() {
    // A port nothing listens on.
    let verifier = AnthropicVisionVerifier::new("k", "m", "http://127.0.0.1:9");
    let verdict = verifier.verify(request()).await;
    assert_eq!(verdict.status, VerdictStatus::Unknown);
    assert!(verdict.reason.contains("transport"), "{}", verdict.reason);
}

#[tokio::test]
async fn the_request_carries_prompt_image_and_region() {
    let server = CannedServer::start(200, messages_body("PASS: ok"));
    let region = pointlock_ir::RectIR {
        x: 10.into(),
        y: 20.into(),
        width: 300.into(),
        height: 40.into(),
    };
    let _ = server
        .verifier()
        .verify(VisionRequest {
            region: Some(&region),
            ..request()
        })
        .await;
    let seen = server.seen.lock().expect("seen");
    let sent: serde_json::Value = serde_json::from_str(&seen[0]).expect("request JSON");
    assert_eq!(sent["model"], "claude-opus-4-8");
    let content = sent["messages"][0]["content"].as_array().expect("content");
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["source"]["media_type"], "image/png");
    assert!(!content[0]["source"]["data"].as_str().unwrap().is_empty());
    let text = content[1]["text"].as_str().expect("instruction");
    assert!(text.contains("the SSID field visibly contains"), "{text}");
    assert!(text.contains("x=10, y=20, width=300, height=40"), "{text}");
    assert!(text.contains("Answer UNKNOWN unless"), "{text}");
}
