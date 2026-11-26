//! Wire e2e + pure tests for user-facing API error sanitization.
//!
//! Edge proxies return non-JSON bodies (HTML). Those must never reach TUI
//! scrollback; only structured JSON error envelopes and status-based copy.

use std::sync::Arc;

use echo_agent_sampler::{SamplerConfig, SamplingClient};
use echo_agent_sampling_types::{
    ContentPart, ConversationItem, ConversationRequest, SamplingError, UserItem,
    status_user_message, user_facing_api_error_message,
};
use echo_agent_test_support::{MockInferenceServer, ScriptedResponse};

const CF_524_HTML: &str = r#"<!DOCTYPE html>
<html lang="en-US">
<head><title>cloud.echo-agent.invalid | 524: A timeout occurred</title></head>
<body>
  <h1>A timeout occurred <span>Error code 524</span></h1>
  <div>Visit cloudflare.com for more information.</div>
</body>
</html>"#;

const CF_522_HTML: &str = r#"<!DOCTYPE html>
<html lang="en-US">
<head><title>cloud.echo-agent.invalid | 522: Connection timed out</title></head>
<body>
  <h1>Connection timed out <span>Error code 522</span></h1>
  <div>Visit cloudflare.com for more information.</div>
</body>
</html>"#;

fn test_config(base_url: &str, api_key: &str) -> SamplerConfig {
    SamplerConfig {
        api_key: Some(api_key.to_string()),
        base_url: base_url.to_string(),
        model: "test-model".to_string(),
        ..SamplerConfig::default()
    }
}

fn user_request(text: &str) -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from(text),
            }],
            ..Default::default()
        })],
        ..Default::default()
    }
}

async fn stream_err(status: u16, body: &str) -> echo_agent_sampling_types::SamplingError {
    let server = MockInferenceServer::start().await.expect("start mock");
    server.enqueue_response("/v1/chat/completions", ScriptedResponse::text(status, body));
    let mut cfg = test_config(&server.url(), "test-key");
    cfg.max_retries = Some(0);
    let client = SamplingClient::new(cfg).expect("client");
    match client.conversation_stream(user_request("hi")).await {
        Ok(_) => panic!("expected API error"),
        Err(e) => e,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_524_html_uses_status_copy() {
    let err = stream_err(524, CF_524_HTML).await;
    let s = err.to_string();
    assert!(!s.contains("<!DOCTYPE") && !s.contains("<html"));
    assert!(s.contains(&status_user_message(
        reqwest::StatusCode::from_u16(524).unwrap()
    )));
    assert!(s.contains("524"));
}

/// SEV-576: the edge served a 522 HTML page while the origin was gone and the
/// turn died. The status has to survive the HTML body so retry can see it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_522_html_is_a_retryable_api_error() {
    let err = stream_err(522, CF_522_HTML).await;
    match &err {
        SamplingError::Api { status, .. } => assert_eq!(status.as_u16(), 522),
        other => panic!("expected Api error, got {other:?}"),
    }
    assert!(err.is_retryable(), "CF 522 off the wire must be retryable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_503_html_uses_unavailable_copy() {
    let err = stream_err(503, "<html><body>Service Unavailable</body></html>").await;
    let s = err.to_string();
    assert!(!s.contains("<html"));
    assert!(s.contains("temporarily unavailable"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_json_error_envelope_is_preserved() {
    let body = r#"{"error":{"message":"rate limit exceeded","type":"rate_limit_error"}}"#;
    let err = stream_err(429, body).await;
    let s = err.to_string();
    assert!(s.contains("rate limit exceeded"));
    assert!(!s.contains("temporarily unavailable"));
}

#[test]
fn status_user_message_matrix() {
    let cases: &[(u16, &str)] = &[
        (502, "temporarily unavailable"),
        (503, "temporarily unavailable"),
        (504, "temporarily unavailable"),
        (520, "timed out"),
        (524, "timed out"),
        (530, "timed out"),
        (525, "Secure connection"),
        (526, "Secure connection"),
        (529, "overloaded"),
        (500, "Something went wrong on the server"),
        (400, "Request failed"),
    ];
    for &(code, needle) in cases {
        let msg = status_user_message(reqwest::StatusCode::from_u16(code).unwrap());
        assert!(
            msg.contains(needle),
            "status {code}: expected {needle:?} in {msg:?}"
        );
        assert!(
            msg.contains(&format!("HTTP {code}")),
            "status {code}: expected HTTP code in {msg:?}"
        );
    }
}

#[test]
fn non_json_empty_body_falls_back_to_status() {
    let msg = user_facing_api_error_message(reqwest::StatusCode::BAD_GATEWAY, b"");
    assert_eq!(msg, status_user_message(reqwest::StatusCode::BAD_GATEWAY));
}

#[test]
fn structured_json_is_not_replaced_by_status_copy() {
    let bytes = br#"{"error":{"message":"credits exhausted","type":"server_error"}}"#;
    let msg = user_facing_api_error_message(reqwest::StatusCode::PAYMENT_REQUIRED, bytes);
    assert_eq!(msg, "credits exhausted");
}
