//! Sampling log — emits `tracing` events with `target: "sampling_log"`.
//! A dedicated layer in `echo-agent-telemetry` routes these to
//! `~/.echo-agent/logs/sampling.jsonl`. Enable with `--log-sampling`.

use crate::types::RequestId;

pub const TARGET: &str = "sampling_log";

#[derive(Debug, Clone)]
pub struct AuthInfo {
    pub auth_type: &'static str,
    pub has_auth: bool,
}

fn safe_origin(base_url: &str) -> String {
    reqwest::Url::parse(base_url)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|_| "<invalid>".to_owned())
}

pub fn request_span(
    request_id: &RequestId,
    model: &str,
    api_backend: &str,
    base_url: &str,
    auth: &AuthInfo,
) -> tracing::Span {
    let base_origin = safe_origin(base_url);
    tracing::info_span!(
        target: TARGET,
        "sampling_request",
        request_id = %request_id,
        model = model,
        api_backend = api_backend,
        base_url = %base_origin,
        auth_type = auth.auth_type,
        has_auth = auth.has_auth,
        // Recorded from `SamplerConfig` / response usage as the request
        // progresses; `field::Empty` lets callers `record()` them later.
        reasoning_effort = tracing::field::Empty,
        output_tokens = tracing::field::Empty,
        reasoning_tokens = tracing::field::Empty,
    )
}

#[cfg(test)]
mod tests {
    use super::safe_origin;

    #[test]
    fn safe_origin_drops_credentials_path_query_and_fragment() {
        assert_eq!(
            safe_origin("https://user:secret@example.com:8443/v1?token=secret#fragment"),
            "https://example.com:8443"
        );
    }

    #[test]
    fn safe_origin_does_not_echo_invalid_input() {
        assert_eq!(safe_origin("secret-value"), "<invalid>");
    }
}
