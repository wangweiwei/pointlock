//! # pointlock-vision
//!
//! The [`VisionVerifier`] plugin interface (verify role only, principle 7:
//! vision never locates or acts) and the default [`StubVisionVerifier`].
//!
//! The verifier is the runtime consumer of the `vision` verify channel
//! (spine §6.3): it answers an author-written prompt against localized
//! screenshot bytes with a three-valued verdict. A `pass`/`fail` answer is
//! a *completed* evaluation (fail is final, spine R5); an `unknown` answer
//! means the channel could not complete and the chain advances — for the
//! vision channel, which is only legal at the chain tail, that exhausts the
//! chain into assertion `unknown` (principle 4).
//!
//! The v0.1 default is the stub: it always answers `unknown` with the
//! reason `"vision verifier not configured"`. The runner treats an absent
//! verifier (`RunOptions::vision == None`) as exactly equivalent.

use async_trait::async_trait;
use pointlock_ir::{RectIR, VerdictStatus};

/// The reason the stub (and an unconfigured runner) yields `unknown`.
pub const STUB_REASON: &str = "vision verifier not configured";

/// One vision verification request: the author-written prompt (never
/// synthesized by the compiler, principle 6), an optional region of
/// interest, and the *localized* screenshot bytes (evidence is localized
/// during `observing`, spine §6.6 — the verifier never reaches back into a
/// provider session).
#[derive(Debug, Clone, PartialEq)]
pub struct VisionRequest<'a> {
    /// The author-written prompt, verbatim (`AssertionIR.visionPrompt` for
    /// `elementState`/`elementText` chain tails; `predicate.prompt` for
    /// `visual` predicates).
    pub prompt: &'a str,
    /// Optional region of interest (`visual` predicates only).
    pub region: Option<&'a RectIR>,
    /// The localized screenshot bytes.
    pub screenshot: &'a [u8],
    /// The screenshot's media type, e.g. `image/png`.
    pub media_type: &'a str,
}

/// The verifier's three-valued answer with a human-readable reason.
///
/// `pass`/`fail` are completed evaluations; `unknown` means the verifier
/// could not complete (unconfigured, low confidence, unusable image) and
/// carries the why. The verdict never panics its way out — model/transport
/// failures inside an implementation must fold to `unknown` (principle 4).
#[derive(Debug, Clone, PartialEq)]
pub struct VisionVerdict {
    /// The three-valued status.
    pub status: VerdictStatus,
    /// Why the verifier answered this way.
    pub reason: String,
}

/// The vision verification plugin interface (verify role only).
#[async_trait]
pub trait VisionVerifier: Send + Sync {
    /// Answers `request.prompt` against the screenshot. Infallible by
    /// construction: anything that prevents an answer is an `unknown`
    /// verdict with a reason, never an error (principle 4).
    async fn verify(&self, request: VisionRequest<'_>) -> VisionVerdict;
}

/// The v0.1 default verifier: always `unknown` with [`STUB_REASON`].
#[derive(Debug, Clone, Copy, Default)]
pub struct StubVisionVerifier;

#[async_trait]
impl VisionVerifier for StubVisionVerifier {
    async fn verify(&self, _request: VisionRequest<'_>) -> VisionVerdict {
        VisionVerdict {
            status: VerdictStatus::Unknown,
            reason: STUB_REASON.to_owned(),
        }
    }
}

// ─── The Anthropic-backed verifier (M3a-W4: the first usable one) ───────────

/// The default model of [`AnthropicVisionVerifier`].
pub const DEFAULT_VISION_MODEL: &str = "claude-opus-4-8";

/// Total per-request deadline. Without one, a TCP-accepted-but-silent
/// endpoint stalls `verify()` forever — an unbounded await is a failure
/// mode that never folds to `unknown`, breaking the module contract
/// (principle 4). The step's `timeout_ms` governs only provider execute,
/// not the assert phase, so the bound must live here.
const REQUEST_TIMEOUT_SECS: u64 = 60;

/// Connection-establishment deadline (part of the same fail-to-unknown
/// bound; kept tighter so dead endpoints answer fast).
const CONNECT_TIMEOUT_SECS: u64 = 10;

/// The first usable verifier (08 §6.4): asks an Anthropic vision model to
/// answer the author's prompt against the screenshot, over raw HTTP (no
/// official Rust SDK exists). Discipline unchanged from the trait docs:
/// verify-only, chain tail only, and every failure mode — missing key,
/// transport, non-200, unparseable answer, model uncertainty — folds to
/// `unknown` with a reason, never an error and never a guessed pass
/// (principles 4/7).
pub struct AnthropicVisionVerifier {
    api_key: String,
    model: String,
    base_url: String,
    client: reqwest::Client,
}

impl AnthropicVisionVerifier {
    /// Builds a verifier from explicit configuration.
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        AnthropicVisionVerifier {
            api_key: api_key.into(),
            model: model.into(),
            base_url: base_url.into(),
            // Deadlines: see the timeout consts. `no_proxy` keeps egress
            // deterministic (v0.1 makes no proxy promise) and the canned
            // local-endpoint tests hermetic under ambient HTTP(S)_PROXY /
            // ALL_PROXY environments. The `expect` matches
            // `reqwest::Client::new`'s own panic-on-TLS-init semantics.
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS))
                .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
                .no_proxy()
                .build()
                .expect("reqwest client construction"),
        }
    }

    /// Builds from the environment: `ANTHROPIC_API_KEY` (required — `None`
    /// without it), `POINTLOCK_VISION_MODEL` (default
    /// [`DEFAULT_VISION_MODEL`]), `ANTHROPIC_BASE_URL` (default the public
    /// API; overriding it is also how the tests run against a local
    /// canned-response server).
    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// The injectable body of [`Self::from_env`] (unit-testable without
    /// process-global env mutation).
    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let api_key = get("ANTHROPIC_API_KEY").filter(|key| !key.is_empty())?;
        let model = get("POINTLOCK_VISION_MODEL")
            .filter(|model| !model.is_empty())
            .unwrap_or_else(|| DEFAULT_VISION_MODEL.to_owned());
        let base_url = get("ANTHROPIC_BASE_URL")
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| "https://api.anthropic.com".to_owned());
        Some(Self::new(api_key, model, base_url))
    }

    fn unknown(reason: impl Into<String>) -> VisionVerdict {
        VisionVerdict {
            status: VerdictStatus::Unknown,
            reason: reason.into(),
        }
    }
}

/// The verifier's answer protocol: exactly one leading verdict line. The
/// instruction pins the vocabulary; anything else parses to `unknown`
/// (fail-closed — a chatty answer is not a verdict).
fn parse_answer(text: &str) -> VisionVerdict {
    let first = text.trim().lines().next().unwrap_or_default().trim();
    let (status, rest) = if let Some(rest) = first.strip_prefix("PASS:") {
        (VerdictStatus::Pass, rest)
    } else if let Some(rest) = first.strip_prefix("FAIL:") {
        (VerdictStatus::Fail, rest)
    } else if let Some(rest) = first.strip_prefix("UNKNOWN:") {
        (VerdictStatus::Unknown, rest)
    } else {
        return AnthropicVisionVerifier::unknown(format!(
            "unparseable verifier answer: {first:.120}"
        ));
    };
    VisionVerdict {
        status,
        reason: format!("vision: {}", rest.trim()),
    }
}

#[async_trait]
impl VisionVerifier for AnthropicVisionVerifier {
    async fn verify(&self, request: VisionRequest<'_>) -> VisionVerdict {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(request.screenshot);
        let region_note = request.region.map_or(String::new(), |region| {
            format!(
                " Consider ONLY the region at x={}, y={}, width={}, height={} (pixels from the top-left).",
                region.x, region.y, region.width, region.height
            )
        });
        let instruction = format!(
            "You are a visual verification oracle for a device-automation audit trail. \
             Judge the following claim against the screenshot.{region_note}\n\
             Claim: {}\n\
             Answer with EXACTLY one line and nothing else:\n\
             PASS: <what you see that confirms it>\n\
             FAIL: <what you see that contradicts it>\n\
             UNKNOWN: <why it cannot be determined>\n\
             Answer UNKNOWN unless the claim is clearly confirmed or clearly contradicted.",
            request.prompt
        );
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 1024,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image", "source": {
                        "type": "base64",
                        "media_type": request.media_type,
                        "data": data,
                    }},
                    { "type": "text", "text": instruction },
                ],
            }],
        });

        let response = match self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => return Self::unknown(format!("vision transport failed: {err}")),
        };
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Self::unknown(format!(
                "vision API answered {status}: {:.200}",
                body.trim()
            ));
        }
        let parsed: serde_json::Value = match response.json().await {
            Ok(parsed) => parsed,
            Err(err) => return Self::unknown(format!("vision response unreadable: {err}")),
        };
        // Concatenate the text blocks (thinking blocks are skipped).
        let text: String = parsed
            .get("content")
            .and_then(|content| content.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Self::unknown("vision response carried no text answer");
        }
        parse_answer(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_lookup_requires_a_nonempty_api_key() {
        assert!(AnthropicVisionVerifier::from_lookup(|_| None).is_none());
        assert!(
            AnthropicVisionVerifier::from_lookup(|key| {
                (key == "ANTHROPIC_API_KEY").then(String::new)
            })
            .is_none()
        );
    }

    #[test]
    fn from_lookup_honors_the_model_override_and_defaults_without_it() {
        let with_override = AnthropicVisionVerifier::from_lookup(|key| match key {
            "ANTHROPIC_API_KEY" => Some("k".to_owned()),
            "POINTLOCK_VISION_MODEL" => Some("claude-haiku-4-5".to_owned()),
            _ => None,
        })
        .expect("key present");
        assert_eq!(with_override.model, "claude-haiku-4-5");

        let defaulted = AnthropicVisionVerifier::from_lookup(|key| {
            (key == "ANTHROPIC_API_KEY").then(|| "k".to_owned())
        })
        .expect("key present");
        assert_eq!(defaulted.model, DEFAULT_VISION_MODEL);
        assert_eq!(defaulted.base_url, "https://api.anthropic.com");
    }

    #[tokio::test]
    async fn stub_always_answers_unknown_with_the_fixed_reason() {
        let verdict = StubVisionVerifier
            .verify(VisionRequest {
                prompt: "the Wi-Fi toggle is visible",
                region: None,
                screenshot: b"png-bytes",
                media_type: "image/png",
            })
            .await;
        assert_eq!(verdict.status, VerdictStatus::Unknown);
        assert_eq!(verdict.reason, STUB_REASON);
    }
}
