//! Outbound events emitted by the sampler.

use serde::{Deserialize, Serialize};

use echo_agent_sampling_types::{
    ApiErrorCode, ConversationResponse, EmptyResponseContext, ResponseModelMetadata, SamplingError,
    SentCredential,
};

use crate::metrics::InferenceLatencyStats;
use crate::types::RequestId;

/// Which content channel a token belongs to.
///
/// Extensible — adding a new channel (e.g., `Planning`) only requires a
/// new variant here, not new [`SamplingEvent`] variants. Mirrors the
/// agentic-sampler's `AgentChannel` pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SamplingChannel {
    Text,
    Reasoning,
}

/// Why the in-flight request was stripped. What to do about it (e.g. persist
/// the strip to stored history) is the consumer's decision, not the sampler's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripReason {
    /// A 400 stamped with the invalid-image code: the server's
    /// deterministic verdict on this exact payload.
    ServerRejected,
    /// A size/transport heuristic (413, connection reset on upload) or a
    /// non-deterministic rejection (proxy-wrapped 500, legacy phrase match,
    /// mid-stream error): may be transient, blames no particular image.
    PayloadHeuristic,
}

impl StripReason {
    /// snake_case label for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            StripReason::ServerRejected => "server_rejected",
            StripReason::PayloadHeuristic => "payload_heuristic",
        }
    }
}

/// Events emitted by the sampler for a single in-flight request.
///
/// Sent on the shared event channel that callers subscribe to. The
/// session translates these into ACP notifications.
#[derive(Debug, Clone)]
pub enum SamplingEvent {
    /// HTTP stream established, headers read. Emitted before any content.
    StreamStarted {
        request_id: RequestId,
        timestamp_ms: i64,
    },

    /// First content token received for a request.
    FirstToken { request_id: RequestId },

    /// Content token in a named channel (text or reasoning).
    ChannelToken {
        request_id: RequestId,
        channel: SamplingChannel,
        text: String,
        chunk_index: u64,
    },

    /// Streaming delta carrying a fragment of a tool call.
    ///
    /// Emitted by the L2 transforms (Chat Completions, Responses, Messages)
    /// per-chunk as the model streams tool-call arguments. Any single
    /// `arguments_delta` is NOT necessarily valid JSON in isolation.
    ToolCallDelta {
        request_id: RequestId,
        tool_index: u32,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
    },

    /// The provider opened a response (Messages `message_start`). Carries the
    /// real message id, model, and input-side token counts exactly as they
    /// arrive on the wire, before any content. Surfaced in order so partial-mode
    /// consumers can emit the real `message_start` id/usage instead of a
    /// synthesized placeholder. Emitted by the Messages L2 transform only; the
    /// Responses/Chat transforms lack these fields at stream open and emit
    /// nothing here.
    ///
    /// `input_tokens` is the uncached prompt portion; the Anthropic Messages API
    /// reports cache hits and writes in the separate `cache_read_input_tokens`
    /// and `cache_creation_input_tokens` buckets, both known at `message_start`.
    ResponseStarted {
        request_id: RequestId,
        message_id: String,
        model: String,
        input_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
    },

    /// The reasoning (thinking) block finished and its encrypted signature is
    /// known (Messages thinking `content_block_stop`). Surfaced in order so
    /// partial-mode consumers can emit `signature_delta` before the thinking
    /// block's `content_block_stop`. Emitted by the Messages L2 transform only.
    ReasoningCompleted {
        request_id: RequestId,
        signature: String,
    },

    /// Streaming completed successfully.
    Completed {
        request_id: RequestId,
        response: Box<ConversationResponse>,
        metrics: InferenceLatencyStats,
    },

    /// In-flight strip before retry. Persist on `ServerRejected`.
    ImagesStripped {
        request_id: RequestId,
        /// URLs actually stripped from this request.
        stripped_urls: Vec<std::sync::Arc<str>>,
        reason: StripReason,
    },

    /// Request is being retried.
    Retrying {
        request_id: RequestId,
        attempt: u32,
        max_retries: u32,
        /// Typed retry class so consumers never have to sniff `reason`
        /// (e.g. the shell's doom-loop recovery counter).
        kind: SamplingErrorKind,
        reason: String,
        /// Doom-loop telemetry payload when `kind == DoomLoopDetected`:
        /// raw trigger labels + the chunk index the mid-stream abort fired
        /// at (`None` for terminal-response detections). Labels only.
        doom_loop_triggers: Option<Vec<String>>,
        doom_loop_aborted_at_chunk: Option<u64>,
    },

    /// Request failed (after exhausting retries or non-retryable error).
    Failed {
        request_id: RequestId,
        error: SamplingErrorInfo,
    },

    /// Model metadata received from response headers.
    ModelMetadata {
        request_id: RequestId,
        metadata: ResponseModelMetadata,
    },

    /// A backend-hosted tool call has started execution on the server
    /// (e.g., web search is in progress). The client does NOT execute
    /// these — the backend's agentic sampler handles them.
    BackendToolCallStarted {
        request_id: RequestId,
        call_id: String,
        name: String,
    },

    /// A backend-hosted tool call has completed execution on the server.
    BackendToolCallCompleted {
        request_id: RequestId,
        call_id: String,
        name: String,
        /// Structured result data from the backend tool (tool-specific).
        /// For web search: `{"query": "...", "sources": [{"url": "..."}, ...]}`
        result: Option<serde_json::Value>,
    },
}

/// Serializable mirror of [`SamplingError`].
///
/// The rich `SamplingError` carries non-serializable inner values
/// (`reqwest::Error`, `serde_json::Error`) so it cannot cross a network
/// boundary. `SamplingErrorInfo` extracts the bits that downstream
/// consumers (UIs, gRPC adapters) actually need.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingErrorInfo {
    pub kind: SamplingErrorKind,
    pub status_code: Option<u16>,
    pub message: String,
    pub is_retryable: bool,
    pub retry_after_secs: Option<u64>,
    /// Parsed `x-should-retry` response header. `Some(false)` = the server
    /// says the failure is request-content-caused; never retry. `None` =
    /// header absent, or payload from an older peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub should_retry: Option<bool>,
    /// The server error envelope's `code` slot (e.g. `invalid_image`).
    /// Serializes as the plain wire string; `None` when absent or from an
    /// older peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ApiErrorCode>,
    pub model_metadata: Option<ResponseModelMetadata>,
    /// Present only when `kind == EmptyResponse`. Carries the structured
    /// context from the L2 stream so downstream consumers can distinguish
    /// reasoning-only completions from transport failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_response_context: Option<EmptyResponseContext>,
    /// Present only when `kind == DoomLoopDetected`. Raw trigger labels
    /// (never generation content) so the retry loop can reconstruct the
    /// rich error from a synthesized L2 failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doom_loop_triggers: Option<Vec<String>>,
    /// Stream chunk index the mid-stream doom-loop abort fired at.
    /// Telemetry only; `None` for terminal-response detections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doom_loop_aborted_at_chunk: Option<u64>,
    /// Meaningful only when `kind == Auth`: whether the rejected request
    /// actually carried a credential on the wire. Defaults to `Unknown`
    /// (charge-the-budget behavior) for payloads from older peers.
    #[serde(default, skip_serializing_if = "SentCredential::is_unknown")]
    pub credential: SentCredential,
}

/// Coarse-grained classification of a sampling failure.
///
/// Intentionally narrow — context-window-exceeded does NOT have its own
/// variant because the sampler cannot reliably detect it (it lacks
/// tracked token counts). Context-window errors arrive as
/// `Api { status: 400, .. }` with model metadata; the session inspects
/// the metadata and decides whether to compact.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SamplingErrorKind {
    Auth,
    Http,
    ChatStreamInterrupted,
    Api,
    Serialization,
    IdleTimeout,
    RateLimited,
    EmptyResponse,
    MaxTokensTruncation,
    DoomLoopDetected,
}

impl SamplingErrorKind {
    /// Stable, lowercase string form suitable for telemetry tags
    /// (e.g., analytics `error_type` columns and signals histograms).
    /// Mirrors the strings used in the shell's
    /// `stream_conversation_with_retries` error classifier so tags stay
    /// consistent across surfaces.
    pub fn as_str(self) -> &'static str {
        match self {
            SamplingErrorKind::Auth => "auth",
            SamplingErrorKind::Http => "http",
            SamplingErrorKind::ChatStreamInterrupted => "chat_stream_interrupted",
            SamplingErrorKind::Api => "api",
            SamplingErrorKind::Serialization => "serialization",
            SamplingErrorKind::IdleTimeout => "idle_timeout",
            SamplingErrorKind::RateLimited => "rate_limited",
            SamplingErrorKind::EmptyResponse => "empty_response",
            SamplingErrorKind::MaxTokensTruncation => "max_tokens_truncation",
            SamplingErrorKind::DoomLoopDetected => "doom_loop_detected",
        }
    }
}

impl From<&SamplingError> for SamplingErrorInfo {
    fn from(err: &SamplingError) -> Self {
        let is_retryable = err.is_retryable();
        let message = err.to_string();

        let (kind, status_code, retry_after_secs, model_metadata) = match err {
            SamplingError::Auth { .. } => (SamplingErrorKind::Auth, None, None, None),
            SamplingError::InvalidConfiguration(_) => (SamplingErrorKind::Api, None, None, None),
            SamplingError::Http(_) => (SamplingErrorKind::Http, None, None, None),
            SamplingError::Serialization(_) => (SamplingErrorKind::Serialization, None, None, None),
            SamplingError::Api {
                status,
                model_metadata,
                retry_after_secs,
                ..
            } => {
                let kind = if err.is_rate_limited() {
                    SamplingErrorKind::RateLimited
                } else {
                    SamplingErrorKind::Api
                };
                (
                    kind,
                    Some(status.as_u16()),
                    *retry_after_secs,
                    model_metadata.clone(),
                )
            }
            SamplingError::EventStreamError(_) => (SamplingErrorKind::Http, None, None, None),
            SamplingError::ChatStreamInterrupted { .. } => {
                (SamplingErrorKind::ChatStreamInterrupted, None, None, None)
            }
            SamplingError::StreamError { .. } => (SamplingErrorKind::Api, None, None, None),
            SamplingError::IdleTimeout { .. } => (SamplingErrorKind::IdleTimeout, None, None, None),
            SamplingError::EmptyResponse { .. } => {
                (SamplingErrorKind::EmptyResponse, None, None, None)
            }
            SamplingError::MaxTokensTruncation => {
                (SamplingErrorKind::MaxTokensTruncation, None, None, None)
            }
            SamplingError::DoomLoopDetected { .. } => {
                (SamplingErrorKind::DoomLoopDetected, None, None, None)
            }
        };

        let empty_response_context = match err {
            SamplingError::EmptyResponse { context } => Some(context.clone()),
            _ => None,
        };
        let (doom_loop_triggers, doom_loop_aborted_at_chunk) = match err {
            SamplingError::DoomLoopDetected {
                triggers,
                aborted_at_chunk,
            } => (Some(triggers.clone()), *aborted_at_chunk),
            _ => (None, None),
        };
        let credential = match err {
            SamplingError::Auth { credential, .. } => *credential,
            _ => SentCredential::Unknown,
        };
        let error_code = match err {
            SamplingError::Api { error_code, .. } => error_code.clone(),
            SamplingError::StreamError { code, .. } => code.clone(),
            _ => None,
        };

        Self {
            kind,
            status_code,
            message,
            should_retry: err.should_retry_header(),
            error_code,
            is_retryable,
            retry_after_secs,
            model_metadata,
            empty_response_context,
            doom_loop_triggers,
            doom_loop_aborted_at_chunk,
            credential,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_sampling_types::ApiErrorCode;
    use reqwest::StatusCode;

    #[test]
    fn from_sampling_error_carries_should_retry_header() {
        let err = SamplingError::Api {
            status: StatusCode::from_u16(529).expect("valid status"),
            message: "Overloaded".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: Some(false),
            error_code: None,
        };
        let info = SamplingErrorInfo::from(&err);
        assert_eq!(info.should_retry, Some(false));

        // Non-Api variants have no header — stays None.
        let stream_err = SamplingError::StreamError {
            error_type: "overloaded_error".into(),
            message: "Overloaded".into(),
            code: None,
        };
        assert_eq!(SamplingErrorInfo::from(&stream_err).should_retry, None);
    }

    #[test]
    fn from_sampling_error_carries_error_code() {
        let api = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "bad image".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: Some(ApiErrorCode::InvalidImage),
        };
        assert_eq!(
            SamplingErrorInfo::from(&api).error_code,
            Some(ApiErrorCode::InvalidImage)
        );

        let stream = SamplingError::StreamError {
            error_type: "invalid_request_error".into(),
            message: "bad image".into(),
            code: Some(ApiErrorCode::InvalidImage),
        };
        assert_eq!(
            SamplingErrorInfo::from(&stream).error_code,
            Some(ApiErrorCode::InvalidImage)
        );

        // Non-wire variants carry none.
        assert_eq!(
            SamplingErrorInfo::from(&SamplingError::auth_unknown("x")).error_code,
            None
        );
    }

    #[test]
    fn auth_variant_classified_as_auth() {
        let err = SamplingError::auth_unknown("bad token");
        let info = SamplingErrorInfo::from(&err);
        assert_eq!(info.kind, SamplingErrorKind::Auth);
        assert_eq!(info.status_code, None);
        assert!(!info.is_retryable);
        assert_eq!(info.retry_after_secs, None);
        assert!(info.model_metadata.is_none());
        assert!(info.message.contains("bad token"));
    }

    /// A payload from a peer that predates `credential` must still parse,
    /// defaulting to `Unknown` (charge-the-budget behavior).
    #[test]
    fn info_without_credential_field_deserializes_to_unknown() {
        let info: SamplingErrorInfo = serde_json::from_str(
            r#"{"kind":"Auth","status_code":401,"message":"x","is_retryable":false,
                "retry_after_secs":null,"model_metadata":null}"#,
        )
        .unwrap();
        assert_eq!(info.credential, SentCredential::Unknown);
    }

    #[test]
    fn invalid_configuration_classified_as_api() {
        let err = SamplingError::InvalidConfiguration("missing model");
        let info = SamplingErrorInfo::from(&err);
        assert_eq!(info.kind, SamplingErrorKind::Api);
        assert_eq!(info.status_code, None);
        assert!(!info.is_retryable);
    }

    #[test]
    fn serialization_variant_classified_as_serialization() {
        let json_err = serde_json::from_str::<i32>("not a number").unwrap_err();
        let err: SamplingError = json_err.into();
        let info = SamplingErrorInfo::from(&err);
        assert_eq!(info.kind, SamplingErrorKind::Serialization);
        assert!(!info.is_retryable);
    }

    #[test]
    fn api_500_classified_as_api_and_retryable() {
        let err = SamplingError::Api {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "boom".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        let info = SamplingErrorInfo::from(&err);
        assert_eq!(info.kind, SamplingErrorKind::Api);
        assert_eq!(info.status_code, Some(500));
        assert!(info.is_retryable, "5xx should be retryable");
    }

    #[test]
    fn api_429_classified_as_rate_limited_and_extracts_retry_after() {
        let err = SamplingError::Api {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: "slow down".into(),
            model_metadata: None,
            retry_after_secs: Some(15),
            should_retry: None,
            error_code: None,
        };
        let info = SamplingErrorInfo::from(&err);
        assert_eq!(info.kind, SamplingErrorKind::RateLimited);
        assert_eq!(info.status_code, Some(429));
        assert_eq!(info.retry_after_secs, Some(15));
        assert!(info.is_retryable, "429 should be retryable");
    }

    #[test]
    fn api_400_classified_as_api_and_not_retryable() {
        let err = SamplingError::Api {
            status: StatusCode::BAD_REQUEST,
            message: "context window exceeded".into(),
            model_metadata: Some(ResponseModelMetadata {
                context_window: Some(8000),
                ..Default::default()
            }),
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        };
        let info = SamplingErrorInfo::from(&err);
        assert_eq!(info.kind, SamplingErrorKind::Api);
        assert_eq!(info.status_code, Some(400));
        assert!(!info.is_retryable, "4xx (non-429) should not be retryable");
        let metadata = info.model_metadata.expect("metadata preserved");
        assert_eq!(metadata.context_window, Some(8000));
    }

    #[test]
    fn event_stream_error_classified_as_http_and_retryable() {
        let err = SamplingError::EventStreamError("conn reset".into());
        let info = SamplingErrorInfo::from(&err);
        assert_eq!(info.kind, SamplingErrorKind::Http);
        assert!(info.is_retryable);
    }

    #[test]
    fn stream_error_classified_as_api_and_retryable() {
        let err = SamplingError::StreamError {
            error_type: "server_error".into(),
            message: "transient".into(),
            code: None,
        };
        let info = SamplingErrorInfo::from(&err);
        assert_eq!(info.kind, SamplingErrorKind::Api);
        assert_eq!(info.status_code, None);
        assert!(info.is_retryable, "stream errors should be retryable");
    }

    #[test]
    fn idle_timeout_classified_as_idle_timeout_and_not_retryable() {
        let err = SamplingError::IdleTimeout { elapsed_secs: 300 };
        let info = SamplingErrorInfo::from(&err);
        assert_eq!(info.kind, SamplingErrorKind::IdleTimeout);
        assert!(!info.is_retryable);
        assert!(info.message.contains("300s"));
    }
}
