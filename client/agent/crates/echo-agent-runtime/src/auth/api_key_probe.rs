//! First-party env key probe (`GET {echo_agent_api_base_url}/api-key`) before
//! advertising `echoagent.api_key`. BYOK is not probed.
//!
//! The base URL is the caller's effective `endpoints.echo_agent_api_base_url` so the
//! probe hits the same host turn traffic uses (`ECHO_AGENT_API_BASE_URL` /
//! `[endpoints] echo_agent_api_base_url`), not a hardcoded production host.
//!
//! Unusable (auth error / blocked/disabled/team_blocked) → do not advertise.
//! Unknown (timeout / network / exhausted retries) → fail open.
//!
//! One retry within the wall budget on 429 / 5xx / transport errors.
//! Timeout default 400ms for the whole probe including retries
//! (live RTT p95≈250ms).

use std::time::{Duration, Instant};

use serde::Deserialize;

/// Wall-clock budget for the entire probe (all attempts + backoff).
pub(crate) const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// Last 12 chars of a key for diagnostic logs (never the full secret).
/// Local copy avoids a Bazel-visible import cycle with `auth::model`.
fn key_suffix(t: &str) -> &str {
    let len = t.len();
    if len > 12 { &t[len - 12..] } else { t }
}

/// Whether `initialize` should HTTP-probe the first-party env key.
///
/// Skip (and treat as usable) when:
/// - kill switch is on (key will not be advertised either way),
/// - BYOK is present (advertise without probing first-party env),
/// - no env key is set,
/// - any `preferred_method` pin: OIDC never advertises the key; ApiKey is
///   fail-closed so a false-negative probe empties `auth_methods` with no
///   login method to fall back to.
pub(crate) fn should_probe_first_party_env_key(
    disable_api_key_auth: bool,
    has_byok: bool,
    has_env_key: bool,
    preferred_method_pinned: bool,
) -> bool {
    !disable_api_key_auth && !has_byok && has_env_key && !preferred_method_pinned
}

/// Initial attempt + this many retries.
const MAX_RETRIES: u32 = 1;

/// Fixed backoff before the single retry.
const RETRY_BACKOFF: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiKeyProbeVerdict {
    Usable,
    /// Auth error or 200 with blocked/disabled/team_blocked flags.
    Unusable,
    /// Timeout / network / exhausted retries — fail open.
    Unknown,
}

impl ApiKeyProbeVerdict {
    pub(crate) fn allows_advertise(self) -> bool {
        match self {
            Self::Usable | Self::Unknown => true,
            Self::Unusable => false,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ApiKeyInfoBody {
    #[serde(default)]
    api_key_blocked: bool,
    #[serde(default)]
    api_key_disabled: bool,
    #[serde(default)]
    team_blocked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptOutcome {
    Done(ApiKeyProbeVerdict),
    Retry,
}

/// `{api_base_url}/api-key` with trailing-slash normalization.
fn api_key_info_url(api_base_url: &str) -> String {
    let base = api_base_url.trim().trim_end_matches('/');
    format!("{base}/api-key")
}

/// Classify status + body. Pure for unit tests.
fn classify_probe_attempt(status: u16, body: &[u8]) -> AttemptOutcome {
    match status {
        200 => match serde_json::from_slice::<ApiKeyInfoBody>(body) {
            Ok(info) if info.api_key_blocked || info.api_key_disabled || info.team_blocked => {
                AttemptOutcome::Done(ApiKeyProbeVerdict::Unusable)
            }
            // Unparseable 200: fail open (API shape drift).
            Ok(_) | Err(_) => AttemptOutcome::Done(ApiKeyProbeVerdict::Usable),
        },
        // Permanent client/auth failures — do not retry.
        400..=403 => AttemptOutcome::Done(ApiKeyProbeVerdict::Unusable),
        // Rate limited / server errors — retry once.
        429 => AttemptOutcome::Retry,
        s if (500..600).contains(&s) => AttemptOutcome::Retry,
        // Other 4xx (e.g. 404 from test mocks that lack this route): fail open.
        _ => AttemptOutcome::Done(ApiKeyProbeVerdict::Unknown),
    }
}

/// Terminal view of [`classify_probe_attempt`] (retryable → Unknown).
#[cfg(test)]
fn classify_probe_response(status: u16, body: &[u8]) -> ApiKeyProbeVerdict {
    match classify_probe_attempt(status, body) {
        AttemptOutcome::Done(v) => v,
        AttemptOutcome::Retry => ApiKeyProbeVerdict::Unknown,
    }
}

/// Fail open on timeout/transport error after retries. Never logs the raw key.
///
/// `api_base_url` must be the endpoint the env key is actually sent to
/// (`endpoints.echo_agent_api_base_url`), not a hardcoded public default.
async fn probe_echo_agent_api_key(
    key: &str,
    api_base_url: &str,
    timeout: Duration,
) -> ApiKeyProbeVerdict {
    let url = api_key_info_url(api_base_url);
    probe_echo_agent_api_key_at_url(key, &url, timeout).await
}

/// Injectable full URL for tests.
async fn probe_echo_agent_api_key_at_url(
    key: &str,
    url: &str,
    timeout: Duration,
) -> ApiKeyProbeVerdict {
    if key.trim().is_empty() {
        return ApiKeyProbeVerdict::Unusable;
    }

    let client = crate::http::shared_client();
    let started = Instant::now();
    let deadline = started + timeout;
    let mut attempts: u32 = 0;
    let mut last_verdict = ApiKeyProbeVerdict::Unknown;

    loop {
        attempts += 1;
        let now = Instant::now();
        let remaining = deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            break;
        }

        let request = client
            .get(url)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {key}"))
            .timeout(remaining);

        let outcome = match request.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.bytes().await.unwrap_or_default();
                classify_probe_attempt(status, &body)
            }
            Err(_) => AttemptOutcome::Retry,
        };

        match outcome {
            AttemptOutcome::Done(v) => {
                last_verdict = v;
                break;
            }
            AttemptOutcome::Retry => {
                last_verdict = ApiKeyProbeVerdict::Unknown;
                if attempts > MAX_RETRIES {
                    break;
                }
                let now = Instant::now();
                let remaining = deadline.saturating_duration_since(now);
                if remaining.is_zero() {
                    break;
                }
                tokio::time::sleep(RETRY_BACKOFF.min(remaining)).await;
            }
        }
    }

    let elapsed_ms = started.elapsed().as_millis() as u64;
    echo_agent_telemetry::unified_log::info(
        "auth: first-party API key probe",
        None,
        Some(serde_json::json!({
            "verdict": format!("{last_verdict:?}"),
            "allows_advertise": last_verdict.allows_advertise(),
            "elapsed_ms": elapsed_ms,
            "timeout_ms": timeout.as_millis() as u64,
            "attempts": attempts,
            "key_suffix": key_suffix(key),
        })),
    );

    last_verdict
}

/// Probe env key if set; no env key → false (caller ORs with BYOK).
///
/// `api_base_url` is the caller's effective `endpoints.echo_agent_api_base_url`, so the
/// probe follows the same endpoint as turn traffic — and, in tests, the mock
/// server fixtures already set via `ECHO_AGENT_API_BASE_URL`.
pub(crate) async fn first_party_env_key_allows_advertise(
    api_base_url: &str,
    timeout: Duration,
) -> bool {
    let Ok(key) = crate::agent::auth_method::read_echo_agent_api_key_env() else {
        return false;
    };
    probe_echo_agent_api_key(&key, api_base_url, timeout)
        .await
        .allows_advertise()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_only_when_env_key_alone_would_suppress_login() {
        // Happy path: env key present, nothing else blocking.
        assert!(should_probe_first_party_env_key(false, false, true, false));
        // Kill switch / BYOK / no env / any pin → skip probe (treat as usable).
        assert!(!should_probe_first_party_env_key(true, false, true, false));
        assert!(!should_probe_first_party_env_key(false, true, true, false));
        assert!(!should_probe_first_party_env_key(
            false, false, false, false
        ));
        assert!(!should_probe_first_party_env_key(false, false, true, true));
    }

    #[test]
    fn joins_api_key_path_onto_base() {
        assert_eq!(
            api_key_info_url("https://api.echo-agent.invalid/v1"),
            "https://api.echo-agent.invalid/v1/api-key"
        );
        assert_eq!(
            api_key_info_url("https://api.echo-agent.invalid/v1/"),
            "https://api.echo-agent.invalid/v1/api-key"
        );
        assert_eq!(
            api_key_info_url("https://enterprise-api.acme.com/v1"),
            "https://enterprise-api.acme.com/v1/api-key"
        );
    }

    #[test]
    fn usable_on_200_clear_flags() {
        let body = br#"{"api_key_id":"k","api_key_blocked":false,"api_key_disabled":false,"team_blocked":false}"#;
        assert_eq!(
            classify_probe_response(200, body),
            ApiKeyProbeVerdict::Usable
        );
    }

    #[test]
    fn unusable_on_200_blocked() {
        let body = br#"{"api_key_blocked":true,"api_key_disabled":false}"#;
        assert_eq!(
            classify_probe_response(200, body),
            ApiKeyProbeVerdict::Unusable
        );
    }

    #[test]
    fn unusable_on_200_disabled() {
        let body = br#"{"api_key_blocked":false,"api_key_disabled":true}"#;
        assert_eq!(
            classify_probe_response(200, body),
            ApiKeyProbeVerdict::Unusable
        );
    }

    #[test]
    fn unusable_on_200_team_blocked() {
        let body = br#"{"api_key_blocked":false,"api_key_disabled":false,"team_blocked":true}"#;
        assert_eq!(
            classify_probe_response(200, body),
            ApiKeyProbeVerdict::Unusable
        );
    }

    #[test]
    fn usable_on_200_unparseable_body_fail_open() {
        assert_eq!(
            classify_probe_response(200, b"not-json"),
            ApiKeyProbeVerdict::Usable
        );
    }

    #[test]
    fn unusable_on_auth_errors() {
        for status in [400u16, 401, 402, 403] {
            assert_eq!(
                classify_probe_response(status, br#"{"error":"Incorrect API key"}"#),
                ApiKeyProbeVerdict::Unusable,
                "status {status}"
            );
        }
    }

    #[test]
    fn rate_limit_and_5xx_are_retryable() {
        assert_eq!(classify_probe_attempt(429, b""), AttemptOutcome::Retry);
        assert_eq!(classify_probe_attempt(503, b""), AttemptOutcome::Retry);
        assert_eq!(
            classify_probe_response(429, b""),
            ApiKeyProbeVerdict::Unknown
        );
    }

    #[test]
    fn unknown_on_other_4xx_fail_open() {
        assert_eq!(
            classify_probe_response(404, b""),
            ApiKeyProbeVerdict::Unknown
        );
    }

    #[tokio::test]
    async fn empty_key_is_unusable_without_network() {
        assert_eq!(
            probe_echo_agent_api_key(
                "   ",
                "https://example.invalid/v1",
                Duration::from_millis(50)
            )
            .await,
            ApiKeyProbeVerdict::Unusable
        );
    }

    /// Serves sequential responses (one connection per attempt).
    fn serve_sequence(responses: Vec<(String, Vec<u8>)>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for (status_line, body) in responses {
                if let Ok((mut stream, _)) = listener.accept() {
                    use std::io::{Read, Write};
                    let _ = stream.read(&mut [0u8; 2048]);
                    let resp = format!(
                        "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        String::from_utf8_lossy(&body)
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
            }
        });
        format!("http://{addr}/v1/api-key")
    }

    fn serve_one_http_response(status_line: &str, body: &[u8]) -> String {
        serve_sequence(vec![(status_line.to_string(), body.to_vec())])
    }

    #[tokio::test]
    async fn local_server_invalid_key_is_unusable() {
        let url = serve_one_http_response(
            "HTTP/1.1 400 Bad Request",
            br#"{"error":"Incorrect API key"}"#,
        );
        let v =
            probe_echo_agent_api_key_at_url("echo-agent-bad", &url, Duration::from_secs(2)).await;
        assert_eq!(v, ApiKeyProbeVerdict::Unusable);
    }

    #[tokio::test]
    async fn local_server_blocked_key_is_unusable() {
        let url = serve_one_http_response(
            "HTTP/1.1 200 OK",
            br#"{"api_key_blocked":true,"api_key_disabled":false}"#,
        );
        let v = probe_echo_agent_api_key_at_url("echo-agent-blocked", &url, Duration::from_secs(2))
            .await;
        assert_eq!(v, ApiKeyProbeVerdict::Unusable);
    }

    #[tokio::test]
    async fn local_server_ok_key_is_usable() {
        let url = serve_one_http_response(
            "HTTP/1.1 200 OK",
            br#"{"api_key_id":"abc","api_key_blocked":false,"api_key_disabled":false}"#,
        );
        let v =
            probe_echo_agent_api_key_at_url("echo-agent-good", &url, Duration::from_secs(2)).await;
        assert_eq!(v, ApiKeyProbeVerdict::Usable);
    }

    #[tokio::test]
    async fn probes_joined_base_url_path() {
        let url = serve_one_http_response(
            "HTTP/1.1 200 OK",
            br#"{"api_key_id":"abc","api_key_blocked":false,"api_key_disabled":false}"#,
        );
        // serve_sequence returns full .../v1/api-key; strip to base like config.
        let base = url.trim_end_matches("/api-key");
        let v = probe_echo_agent_api_key("echo-agent-good", base, Duration::from_secs(2)).await;
        assert_eq!(v, ApiKeyProbeVerdict::Usable);
    }

    #[tokio::test]
    async fn retries_429_then_succeeds() {
        let url = serve_sequence(vec![
            (
                "HTTP/1.1 429 Too Many Requests".into(),
                br#"{"error":"rate limited"}"#.to_vec(),
            ),
            (
                "HTTP/1.1 200 OK".into(),
                br#"{"api_key_id":"ok","api_key_blocked":false,"api_key_disabled":false}"#.to_vec(),
            ),
        ]);
        let v =
            probe_echo_agent_api_key_at_url("echo-agent-retry", &url, Duration::from_secs(2)).await;
        assert_eq!(v, ApiKeyProbeVerdict::Usable);
    }

    #[tokio::test]
    async fn timeout_is_unknown_fail_open() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            // Accept and stall past the client timeout (both attempts).
            for _ in 0..2 {
                if let Ok((_stream, _)) = listener.accept() {
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        });

        let url = format!("http://{addr}/v1/api-key");
        let v = probe_echo_agent_api_key_at_url("echo-agent-slow", &url, Duration::from_millis(80))
            .await;
        assert_eq!(v, ApiKeyProbeVerdict::Unknown);
        assert!(v.allows_advertise());
    }
}
