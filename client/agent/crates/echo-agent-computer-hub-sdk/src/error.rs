//! Client-side error taxonomy.
//!
//! Wire-level [`echo_agent_tool_protocol::ToolErrorWire`] variants and JSON-RPC
//! error envelopes are mapped into the smaller [`ClientError`] vocabulary
//! at the SDK boundary so consumers can match on a single enum without
//! re-deriving the numeric/string code mapping.

use echo_agent_tool_protocol::{IdError, JsonRpcError, ToolCallId, ToolErrorWire};
use thiserror::Error;
use url::Url;

/// Errors surfaced by the client SDK.
#[derive(Debug, Error)]
pub enum ClientError {
    /// WebSocket transport failure: failed to connect, dropped socket,
    /// or in-flight request interrupted by a reconnect cycle.
    #[error("network error: {0}")]
    NetworkError(String),

    /// Wire-protocol violation: malformed JSON, unexpected method,
    /// hello/hello_ack mismatch, or unsupported `protocol_version`.
    #[error("protocol error: {0}")]
    ProtocolError(String),

    /// Authentication or authorisation rejected by the server.
    #[error("auth error: {0}")]
    AuthError(String),

    /// Server rejected the WebSocket upgrade with an HTTP auth status
    /// (401/403). Non-retryable: replaying the same credential is
    /// rejected identically, so the reconnect loop classifies this as
    /// fatal instead of retrying forever.
    #[error("handshake auth failed: HTTP {status}")]
    HandshakeAuthFailed { status: u16 },

    /// `register_tool` / `register_session` ack reported a conflict
    /// (cross-connection contention or an already-bound entry the
    /// caller did not expect).
    #[error("registration conflict: {0}")]
    RegistrationConflict(String),

    /// Outbound mpsc full or call-site bounded wait elapsed before the
    /// frame could be enqueued. Distinct from [`Self::NetworkError`]:
    /// the socket may still be healthy.
    #[error("backpressure: {0}")]
    BackpressureError(String),

    /// JSON serialise / deserialise failure inside the SDK.
    #[error("serde error: {0}")]
    Serde(String),

    /// Builder consistency error: missing URL, missing auth, etc.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// Wrapped wire-format tool error; surfaces the upstream
    /// [`ToolErrorWire`] variant verbatim for callers that need to
    /// switch on the stable string code.
    #[error(transparent)]
    Wire(ToolErrorWire),

    /// Server-side close / shutdown signal received during steady state.
    #[error("server closed connection: {0}")]
    Closed(String),

    /// Refused to send credentials over an insecure `ws://` scheme to a
    /// non-loopback host. Local-loopback (`127.0.0.1`, `::1`,
    /// `localhost`) is the only exception; every other host MUST be
    /// reached over `wss://` so the bearer token never crosses the
    /// network in plaintext.
    #[error(
        "insecure scheme: refusing to send credentials over plaintext ws:// to non-loopback host {url}"
    )]
    InsecureScheme { url: Url },

    /// Caller passed a `ToolCallId` that already keys an in-flight
    /// dispatch on the same connection. The prior call's progress
    /// waiter and response correlation are left intact; this error
    /// surfaces synchronously so the second caller can retry with a
    /// fresh id. Mint a fresh [`ToolCallId::new_v7`] (or use
    /// [`echo_agent_tool_runtime::ToolCallContext::default`], which does so)
    /// per call. This is client misuse, not a transport or server
    /// failure.
    #[error("call_id {call_id} already in flight on this connection")]
    CallIdInUse { call_id: ToolCallId },
}

impl ClientError {
    /// Map a JSON-RPC envelope error into a [`ClientError`]. The
    /// envelope's `data` payload (when present) carries the stable
    /// [`ToolErrorWire`] discriminator; the numeric `code` is used as a
    /// coarse fallback when `data` is absent or undecodable.
    pub fn from_jsonrpc_error(err: JsonRpcError) -> Self {
        if let Some(data) = err.data
            && let Ok(wire) = serde_json::from_value::<ToolErrorWire>(data)
        {
            return Self::from_wire(wire);
        }
        match err.code {
            -32002 | -32003 => Self::AuthError(err.message),
            -32004 => Self::NetworkError(err.message),
            -32600..=-32500 => Self::ProtocolError(err.message),
            _ => Self::Wire(ToolErrorWire::Custom {
                subcode: format!("jsonrpc_{}", err.code),
                message: err.message,
                details: None,
            }),
        }
    }

    /// `true` when a `data`-less envelope collapsed to the given `jsonrpc_<code>`
    /// subcode (see [`Self::from_jsonrpc_error`]); shared by the bind recognizers.
    fn has_collapsed_jsonrpc_subcode(&self, subcode: &str) -> bool {
        matches!(
            self,
            Self::Wire(ToolErrorWire::Custom { subcode: s, .. }) if s == subcode
        )
    }

    /// `true` for the server's "server not found" bind rejection (JSON-RPC `-32601`):
    /// no workspace-server is registered for this user.
    pub fn is_server_not_found(&self) -> bool {
        self.has_collapsed_jsonrpc_subcode("jsonrpc_-32601")
    }

    /// `true` for the server's `-32013` "server found but bind did not complete" error
    /// (the `ServerBindOutcome::Unavailable` cases). Recognized so the harness
    /// re-provisions this recoverable case, distinct from [`Self::is_server_not_found`].
    pub fn is_tool_unavailable(&self) -> bool {
        self.has_collapsed_jsonrpc_subcode("jsonrpc_-32013")
    }

    /// The server's `-32099` `rate_limited` rejection: it did not act on the
    /// request, so retrying after a backoff is duplicate-safe.
    pub fn is_rate_limited(&self) -> bool {
        self.has_collapsed_jsonrpc_subcode("jsonrpc_-32099")
    }

    /// Map a [`ToolErrorWire`] variant into the SDK error taxonomy.
    pub fn from_wire(wire: ToolErrorWire) -> Self {
        match wire {
            ToolErrorWire::PermissionDenied { reason } => Self::AuthError(reason),
            ToolErrorWire::TransportClosed { tool_id } => {
                Self::NetworkError(format!("transport closed for {tool_id}"))
            }
            ToolErrorWire::UnsupportedProtocolVersion { supported } => {
                Self::ProtocolError(format!("unsupported protocol; supported: {supported:?}"))
            }
            other => Self::Wire(other),
        }
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serde(err.to_string())
    }
}

impl From<IdError> for ClientError {
    fn from(err: IdError) -> Self {
        Self::ProtocolError(err.to_string())
    }
}

impl From<url::ParseError> for ClientError {
    fn from(err: url::ParseError) -> Self {
        Self::InvalidConfig(format!("invalid url: {err}"))
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for ClientError {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::NetworkError(err.to_string())
    }
}

impl ClientError {
    /// Classify a failed WebSocket upgrade. A `401`/`403` on the HTTP
    /// upgrade is a non-retryable auth rejection
    /// ([`Self::HandshakeAuthFailed`]); every other failure stays a
    /// transport [`Self::NetworkError`] via the blanket `From` impl. The
    /// distinction must be made here, before `From` collapses the typed
    /// `Http` response status into an opaque string.
    pub(crate) fn from_handshake_error(err: tokio_tungstenite::tungstenite::Error) -> Self {
        if let tokio_tungstenite::tungstenite::Error::Http(resp) = &err {
            let status = resp.status().as_u16();
            if status == 401 || status == 403 {
                return Self::HandshakeAuthFailed { status };
            }
        }
        Self::from(err)
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for ClientError {
    fn from(_: tokio::sync::oneshot::error::RecvError) -> Self {
        Self::NetworkError("response waiter dropped (connection closed)".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use echo_agent_tool_protocol::{
        WORKSPACE_UNAVAILABLE_SUBCODE, WorkspaceGonePhase, WorkspaceGoneReason,
        workspace_unavailable_wire,
    };
    use serde_json::json;

    use super::*;

    fn workspace_gone_envelope() -> JsonRpcError {
        let wire = workspace_unavailable_wire(
            WorkspaceGoneReason::Disconnect,
            WorkspaceGonePhase::RouteMissing,
        );
        JsonRpcError {
            code: -32005,
            message: "workspace server gone".to_owned(),
            data: Some(serde_json::to_value(&wire).unwrap()),
        }
    }

    fn http_upgrade_error(status: u16) -> tokio_tungstenite::tungstenite::Error {
        let resp = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(status)
            .body(None::<Vec<u8>>)
            .expect("response builds");
        tokio_tungstenite::tungstenite::Error::Http(resp)
    }

    #[test]
    fn handshake_401_and_403_map_to_handshake_auth_failed() {
        for status in [401u16, 403] {
            match ClientError::from_handshake_error(http_upgrade_error(status)) {
                ClientError::HandshakeAuthFailed { status: got } => assert_eq!(got, status),
                other => panic!("expected HandshakeAuthFailed for {status}; got {other:?}"),
            }
        }
    }

    #[test]
    fn handshake_non_auth_status_stays_network_error() {
        for status in [500u16, 502, 429] {
            match ClientError::from_handshake_error(http_upgrade_error(status)) {
                ClientError::NetworkError(_) => {}
                other => panic!("expected NetworkError for {status}; got {other:?}"),
            }
        }
    }

    #[test]
    fn from_jsonrpc_error_preserves_workspace_subcode_and_details() {
        // The `data` payload decodes as `ToolErrorWire` first, so the stable
        // subcode and structured details reach the SDK consumer intact rather
        // than collapsing to the numeric code.
        match ClientError::from_jsonrpc_error(workspace_gone_envelope()) {
            ClientError::Wire(ToolErrorWire::Custom {
                subcode, details, ..
            }) => {
                assert_eq!(subcode, WORKSPACE_UNAVAILABLE_SUBCODE);
                let details = details.expect("details present");
                assert_eq!(details["code"], json!(WORKSPACE_UNAVAILABLE_SUBCODE));
                assert_eq!(details["reason"], json!("disconnect"));
                assert_eq!(details["phase"], json!("route_missing"));
                assert_eq!(details["retryable"], json!(true));
            }
            other => panic!("expected Wire(Custom), got {other:?}"),
        }
    }

    #[test]
    fn is_server_not_found_recognizes_bare_minus_32601() {
        // data-less -32601 -> custom subcode.
        let err = ClientError::from_jsonrpc_error(JsonRpcError {
            code: -32601,
            message: "server abc not found for user".to_owned(),
            data: None,
        });
        assert!(err.is_server_not_found());
    }

    #[test]
    fn is_tool_unavailable_recognizes_bare_minus_32013() {
        let err = ClientError::from_jsonrpc_error(JsonRpcError {
            code: -32013,
            message: "server abc did not complete the bind".to_owned(),
            data: None,
        });
        assert!(err.is_tool_unavailable());
    }

    #[test]
    fn is_server_not_found_rejects_other_errors() {
        let auth = ClientError::from_jsonrpc_error(JsonRpcError {
            code: -32002,
            message: "nope".to_owned(),
            data: None,
        });
        assert!(!auth.is_server_not_found());
        // workspace-gone is the tool-call re-provision path, not bind ServerNotFound.
        assert!(!ClientError::from_jsonrpc_error(workspace_gone_envelope()).is_server_not_found());
    }

    #[test]
    fn bind_recognizers_are_mutually_exclusive() {
        let not_found = ClientError::from_jsonrpc_error(JsonRpcError {
            code: -32601,
            message: "not found".to_owned(),
            data: None,
        });
        let unavailable = ClientError::from_jsonrpc_error(JsonRpcError {
            code: -32013,
            message: "unavailable".to_owned(),
            data: None,
        });
        assert!(not_found.is_server_not_found());
        assert!(
            !not_found.is_tool_unavailable(),
            "-32601 must not be recognized as tool_unavailable"
        );
        assert!(unavailable.is_tool_unavailable());
        assert!(
            !unavailable.is_server_not_found(),
            "-32013 must not be recognized as server_not_found"
        );
    }

    #[test]
    fn sdk_reexported_recognizer_matches_decoded_error() {
        // SDK-only consumers reach the recognizer through the SDK re-export and
        // the core decode path.
        let err = echo_agent_computer_hub_core::error_from_envelope(workspace_gone_envelope());
        assert!(crate::is_workspace_unavailable(&err));
    }

    #[test]
    fn sdk_reexported_recognizer_rejects_unrelated_custom_error() {
        let wire = ToolErrorWire::Custom {
            subcode: "unrelated".to_owned(),
            message: "nope".to_owned(),
            details: Some(json!({ "code": "unrelated" })),
        };
        let env = JsonRpcError {
            code: -32000,
            message: "nope".to_owned(),
            data: Some(serde_json::to_value(&wire).unwrap()),
        };
        let err = echo_agent_computer_hub_core::error_from_envelope(env);
        assert!(!crate::is_workspace_unavailable(&err));
    }
}
