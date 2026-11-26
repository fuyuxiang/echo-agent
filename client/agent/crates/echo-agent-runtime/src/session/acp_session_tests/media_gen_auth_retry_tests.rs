use super::*;
use crate::auth::{AuthManager, AuthMode, EchoAgentAuth, EchoAgentComConfig};
use echo_agent_tools::types::output::{ToolOutput, ToolRunResult};
use std::sync::atomic::{AtomicUsize, Ordering};

fn succeeding_am() -> Arc<AuthManager> {
    let dir = tempfile::tempdir().unwrap();
    let am = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    am.hot_swap(EchoAgentAuth {
        key: "expired".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    struct Ok;
    #[async_trait::async_trait]
    impl crate::auth::refresh::TokenRefresher for Ok {
        async fn refresh(
            &self,
            _: crate::auth::refresh::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            crate::auth::refresh::RefreshOutcome::Success(Box::new(EchoAgentAuth {
                key: "fresh".into(),
                expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                refresh_token: Some("rt-new".into()),
                ..EchoAgentAuth::test_default()
            }))
        }
    }
    am.set_refresher(Arc::new(Ok));
    // Keep the tempdir alive for the manager's lifetime (its auth.json backs `am`).
    std::mem::forget(dir);
    am
}

fn failing_am() -> Arc<AuthManager> {
    let dir = tempfile::tempdir().unwrap();
    let am = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    am.hot_swap(EchoAgentAuth {
        key: "expired".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    struct Fail;
    #[async_trait::async_trait]
    impl crate::auth::refresh::TokenRefresher for Fail {
        async fn refresh(
            &self,
            _: crate::auth::refresh::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            crate::auth::refresh::RefreshOutcome::permanent(
                crate::auth::RefreshTokenFailedReason::RefreshTokenRejected,
                None,
            )
        }
    }
    am.set_refresher(Arc::new(Fail));
    // Keep the tempdir alive for the manager's lifetime (its auth.json backs `am`).
    std::mem::forget(dir);
    am
}

fn ok_result(text: &str) -> Result<ToolRunResult, echo_agent_tool_runtime::ToolError> {
    Ok(ToolRunResult {
        output: ToolOutput::Text(text.to_owned().into()),
        prompt_text: text.to_owned(),
        effective_tool_name: None,
    })
}

fn err(msg: &str) -> Result<ToolRunResult, echo_agent_tool_runtime::ToolError> {
    Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
        msg.to_owned(),
    ))
}

/// Production-shaped HTTP failure (image_gen / video_gen emit this on
/// any non-success status). Use for retry tests that should exercise
/// the structured status-code path rather than the string fallback.
fn http_err(status: u16, msg: &str) -> Result<ToolRunResult, echo_agent_tool_runtime::ToolError> {
    Err(echo_agent_tool_runtime::ToolError::new(
        echo_agent_tool_runtime::ToolErrorKind::Custom,
        msg.to_owned(),
    )
    .with_details(serde_json::json!({"code": "http_failure", HTTP_STATUS_DETAILS_KEY: status})))
}

// ── is_auth_tool_error ────────────────────────────────────────

/// Single source of truth for which error strings/variants the helper
/// must classify. Adding a new pattern is a one-line change here.
#[test]
fn is_auth_tool_error_classification() {
    // (expected, error) — covers every branch + a sample of negatives
    // a careless edit could plausibly break.
    let cases: Vec<(bool, echo_agent_tool_runtime::ToolError)> = vec![
        // Primary path: image_gen / video_gen now surface 401s as
        // structured custom errors with status in details; classifier
        // matches the status code, not the rendered string.
        (
            true,
            echo_agent_tool_runtime::ToolError::new(
                echo_agent_tool_runtime::ToolErrorKind::Custom,
                "Image generation failed with HTTP 401 Unauthorized: missing token",
            )
            .with_details(
                serde_json::json!({"code": "http_failure", HTTP_STATUS_DETAILS_KEY: 401}),
            ),
        ),
        // Negative: 403 Forbidden must NOT trigger a refresh. Mirrors
        // the inference path's gate in echo-agent-sampling-types/src/error.rs:
        // 403 means "authenticated but not permitted" (content safety,
        // ZDR, remote settings gates) and refreshing the token is a no-op
        // that surfaces as a spurious auth_required teardown.
        (
            false,
            echo_agent_tool_runtime::ToolError::new(
                echo_agent_tool_runtime::ToolErrorKind::Custom,
                "Forbidden: ZDR-blocked operation",
            )
            .with_details(
                serde_json::json!({"code": "http_failure", HTTP_STATUS_DETAILS_KEY: 403}),
            ),
        ),
        // Regression guard: a 403 whose body happens to contain
        // "unauthorized" must still be classified as not-auth. Without
        // the structured-variant short-circuit in is_auth_tool_error,
        // the keyword fallback would mis-fire here.
        (
            false,
            echo_agent_tool_runtime::ToolError::new(
                echo_agent_tool_runtime::ToolErrorKind::Custom,
                "Forbidden: unauthorized to perform this action",
            )
            .with_details(
                serde_json::json!({"code": "http_failure", HTTP_STATUS_DETAILS_KEY: 403}),
            ),
        ),
        // Negative: any other non-success HTTP status falls through.
        (
            false,
            echo_agent_tool_runtime::ToolError::new(
                echo_agent_tool_runtime::ToolErrorKind::Custom,
                "internal server error",
            )
            .with_details(
                serde_json::json!({"code": "http_failure", HTTP_STATUS_DETAILS_KEY: 500}),
            ),
        ),
        // Fallback path: BYOK / provider key validation arrives as a
        // ValidationError without a status code. Classifier still
        // catches it via the message-string fallback.
        (
            true,
            echo_agent_tool_runtime::ToolError::invalid_arguments(
                "response: invalid api key for project",
            ),
        ),
        // Fallback path: OAuth 2.0 `invalid_token` payload (RFC 6749)
        // surfaced as raw JSON without a structured status code.
        (
            true,
            echo_agent_tool_runtime::ToolError::invalid_arguments(r#"{"error":"invalid_token"}"#),
        ),
        // Fallback path: case-insensitive "unauthorized" anywhere in
        // the message body.
        (
            true,
            echo_agent_tool_runtime::ToolError::invalid_arguments("UNAUTHORIZED"),
        ),
        // Negative: transport failure must not trigger a token refresh.
        (
            false,
            echo_agent_tool_runtime::ToolError::invalid_arguments(
                "Image generation timed out after 60s",
            ),
        ),
        // Negative: structural not-found error; not a network response.
        (
            false,
            echo_agent_tool_runtime::ToolError::not_found(
                echo_agent_tool_protocol::ToolId::new("image_gen").expect("valid"),
                "Tool not found: image_gen",
            ),
        ),
        // Negative: bare digits embedded in a request id must not trigger
        // a refresh (regression guard for any future bare-`401` substring
        // match accidentally re-introduced into the fallback path).
        (
            false,
            echo_agent_tool_runtime::ToolError::invalid_arguments("request id req_401abc failed"),
        ),
    ];

    for (expected, err) in &cases {
        assert_eq!(
            is_auth_tool_error(err),
            *expected,
            "wrong classification for: {err}"
        );
    }
}

// ── call_with_auth_retry: each test exercises one exit path ───

#[tokio::test]
async fn first_call_succeeds_no_refresh() {
    let am = failing_am();
    let calls = AtomicUsize::new(0);

    let r = call_with_auth_retry(Some(&am), None, "test_tool", || {
        calls.fetch_add(1, Ordering::SeqCst);
        async { ok_result("ok") }
    })
    .await;

    assert!(matches!(r.unwrap().output, ToolOutput::Text(t) if t.text == "ok"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn non_auth_error_is_returned_without_refresh() {
    let am = succeeding_am();
    let calls = AtomicUsize::new(0);

    let r = call_with_auth_retry(Some(&am), None, "test_tool", || {
        calls.fetch_add(1, Ordering::SeqCst);
        async { err("request timed out") }
    })
    .await;

    assert!(r.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn auth_error_with_successful_refresh_retries() {
    let am = succeeding_am();
    let calls = AtomicUsize::new(0);

    let r = call_with_auth_retry(Some(&am), None, "image_gen", || {
        let n = calls.fetch_add(1, Ordering::SeqCst);
        async move {
            if n == 0 {
                http_err(401, "Image generation failed with HTTP 401 Unauthorized: x")
            } else {
                ok_result("retried-ok")
            }
        }
    })
    .await;

    assert!(matches!(r.unwrap().output, ToolOutput::Text(t) if t.text == "retried-ok"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn auth_error_with_failed_refresh_returns_original_error() {
    let am = failing_am();
    let calls = AtomicUsize::new(0);

    let r = call_with_auth_retry(Some(&am), None, "test_tool", || {
        calls.fetch_add(1, Ordering::SeqCst);
        async { err("HTTP 401 Unauthorized") }
    })
    .await;

    assert!(r.unwrap_err().to_string().contains("401"));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "must not retry when refresh fails"
    );
}

#[tokio::test]
async fn auth_error_without_refresher_returns_original_error() {
    let calls = AtomicUsize::new(0);

    let r = call_with_auth_retry(None, None, "test_tool", || {
        calls.fetch_add(1, Ordering::SeqCst);
        async { err("HTTP 401 Unauthorized") }
    })
    .await;

    assert!(r.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Defensive bound: if the freshly-refreshed token also 401s
/// (server-side revocation, clock skew, IdP/RP desync), give up
/// after one retry rather than spinning.
#[tokio::test]
async fn retry_is_bounded_at_one_even_if_retry_also_fails_with_auth() {
    let am = succeeding_am();
    let calls = AtomicUsize::new(0);

    let r = call_with_auth_retry(Some(&am), None, "test_tool", || {
        calls.fetch_add(1, Ordering::SeqCst);
        async { http_err(401, "Image generation failed with HTTP 401 Unauthorized: x") }
    })
    .await;

    assert!(r.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2, "exactly one retry");
}
