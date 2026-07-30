//! The webhook notify-only channel (06 §4.2): the POST payload and its
//! optional HMAC signature. Notify-only by ruling — v0.1's single-process
//! local architecture has no authenticable inbound HTTP face, so
//! responses NEVER come back this way; collection stays with the `cli`
//! channel (store-arbitrated). Transport (the actual POST) lives with the
//! assembly layer; this module is the pure, testable half.
//!
//! Evidence BYTES are never embedded (06 §4.2): the inbox entries carry
//! values and references only, and the envelope names the local store as
//! the forensics path plus the recovery hint for responding.

use hmac::{Hmac, Mac};
use pointlock_store::projection::HumanInboxEntry;
use sha2::Sha256;

/// The HTTP header carrying the body signature.
pub const SIGNATURE_HEADER: &str = "X-Pointlock-Signature";

/// One ready-to-send webhook notification.
#[derive(Debug, Clone)]
pub struct WebhookNotification {
    /// The JSON body (the `pointlockWebhook: 1` envelope).
    pub body: String,
    /// The [`SIGNATURE_HEADER`] value, when a secret is configured.
    pub signature: Option<String>,
}

/// Builds the notification for one run's pending inbox entries: a
/// CLI-owned envelope (`pointlockWebhook: 1`, precedent: `pointlockReport`)
/// around the R14 inbox DTO projections, with the local forensics path
/// and the recovery hint (06 §4.2). Receivers deduplicate by each
/// entry's `requestId` — re-notification after another suspension is
/// legal and expected (notify is idempotent).
pub fn build_notification(
    entries: &[HumanInboxEntry],
    store_dir: &str,
    run_id: &str,
    secret: Option<&str>,
) -> WebhookNotification {
    let envelope = serde_json::json!({
        "pointlockWebhook": 1,
        "runId": run_id,
        "storeDir": store_dir,
        "respondHint": format!(
            "pointlock resume {run_id} --store {store_dir} (responses go through \
             pointlock-human-cli; this webhook never collects)"
        ),
        "entries": entries,
    });
    let body = serde_json::to_string(&envelope).expect("the envelope always serializes");
    let signature = secret.map(|secret| signature_for(&body, secret));
    WebhookNotification { body, signature }
}

/// The `sha256=<hex>` HMAC-SHA256 signature of `body` under `secret`
/// (06 §4.2's `X-Pointlock-Signature`). Computed over the exact body
/// bytes — any reformatting on the receiving side must verify against
/// the raw payload.
pub fn signature_for(body: &str, secret: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body.as_bytes());
    let digest = mac.finalize().into_bytes();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256={hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_the_rfc_4231_vector() {
        // RFC 4231 test case 2 — an external truth source, not an echo
        // of our own implementation.
        assert_eq!(
            signature_for("what do ya want for nothing?", "Jefe"),
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Negative control: a tampered body must not verify.
        assert_ne!(
            signature_for("what do ya want for nothing!", "Jefe"),
            signature_for("what do ya want for nothing?", "Jefe")
        );
    }

    #[test]
    fn notification_envelope_is_versioned_and_signable() {
        let notification = build_notification(&[], "/tmp/store", "run-1", None);
        let envelope: serde_json::Value =
            serde_json::from_str(&notification.body).expect("valid JSON");
        assert_eq!(envelope["pointlockWebhook"], 1);
        assert_eq!(envelope["runId"], "run-1");
        assert!(
            envelope["respondHint"]
                .as_str()
                .is_some_and(|hint| hint.contains("pointlock resume")),
            "the recovery hint rides the envelope (06 §4.2)"
        );
        assert!(envelope["entries"].as_array().is_some_and(Vec::is_empty));
        // No secret → no signature; with secret → signature over THIS body.
        assert_eq!(notification.signature, None);
        let signed = build_notification(&[], "/tmp/store", "run-1", Some("s3cret"));
        assert_eq!(
            signed.signature.as_deref(),
            Some(signature_for(&signed.body, "s3cret").as_str())
        );
    }
}
