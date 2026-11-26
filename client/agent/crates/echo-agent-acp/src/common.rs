use agent_client_protocol as acp;
use tokio::sync::{mpsc, oneshot};

use crate::message::{AcpAgentMessage, AcpClientMessage};

pub type AcpResult<T> = Result<T, acp::Error>;

pub type AcpRxo<T> = oneshot::Receiver<AcpResult<T>>;
pub type AcpTxo<T> = oneshot::Sender<AcpResult<T>>;

pub type AcpClientRx = mpsc::UnboundedReceiver<AcpClientMessage>;
pub type AcpClientTx = mpsc::UnboundedSender<AcpClientMessage>;

pub type AcpAgentRx = mpsc::UnboundedReceiver<AcpAgentMessage>;
pub type AcpAgentTx = mpsc::UnboundedSender<AcpAgentMessage>;

pub fn acp_internal_error(message: impl Into<String>) -> acp::Error {
    acp::Error::new(acp::ErrorCode::InternalError.into(), message)
}

/// The two distinct ways an [`acp_send`](crate::acp_send) round-trip can fail
/// when the underlying channel is closed. Both surface as a JSON-RPC
/// `INTERNAL_ERROR` (so existing callers and the wire format are unaffected);
/// this typed discriminant — carried in the error's `data` — lets callers tell
/// them apart WITHOUT substring-matching the human-readable `message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpChannelFailure {
    /// The request could not be ENQUEUED: the receiver half (the peer's
    /// connection task) is already gone, so no peer is listening — e.g. a
    /// headless run with no client wired.
    SendFailed,
    /// The request was enqueued but the RESPONSE channel was dropped before a
    /// reply arrived: a peer received the request, then went away (disconnect /
    /// process exit) without answering.
    RecvFailed,
}

impl AcpChannelFailure {
    /// `data` object key under which [`acp_send`](crate::acp_send) records the
    /// kind. Namespaced so it can never collide with other `with_data` payloads.
    const DATA_KEY: &'static str = "echoagentAcpChannelFailure";

    const fn tag(self) -> &'static str {
        match self {
            Self::SendFailed => "send_failed",
            Self::RecvFailed => "recv_failed",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "send_failed" => Some(Self::SendFailed),
            "recv_failed" => Some(Self::RecvFailed),
            _ => None,
        }
    }
}

/// Build the channel-closed error for [`acp_send`](crate::acp_send), tagging it
/// with a typed [`AcpChannelFailure`] discriminant in `data`. The error `code`
/// stays `INTERNAL_ERROR`, so this is purely additive for callers that just
/// propagate the error.
pub(crate) fn acp_channel_failure_error(
    message: impl Into<String>,
    kind: AcpChannelFailure,
) -> acp::Error {
    acp_internal_error(message).data(serde_json::json!({ AcpChannelFailure::DATA_KEY: kind.tag() }))
}

/// Recover the [`AcpChannelFailure`] kind from an error, or `None` if the error
/// did not originate from [`acp_send`](crate::acp_send)'s channel-closed paths
/// (or predates the tag). Consumers use this instead of inspecting `message`.
pub fn acp_channel_failure(err: &acp::Error) -> Option<AcpChannelFailure> {
    err.data
        .as_ref()
        .and_then(|data| data.get(AcpChannelFailure::DATA_KEY))
        .and_then(|value| value.as_str())
        .and_then(AcpChannelFailure::from_tag)
}

/// Compact single-line JSON for gateway debug traces. Plain (uncolored)
/// output: this feeds `tracing::debug!`, which typically lands in log files
/// where ANSI colors are noise. Replaces the former `colored_json`-backed
/// `color_json` (dropped to shrink the shipped dependency tree).
#[doc(hidden)]
pub fn compact_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

#[cfg(test)]
mod channel_failure_tests {
    use super::{
        AcpChannelFailure, acp, acp_channel_failure, acp_channel_failure_error, acp_internal_error,
    };

    #[test]
    fn classifier_round_trips_both_kinds() {
        for kind in [AcpChannelFailure::SendFailed, AcpChannelFailure::RecvFailed] {
            let err = acp_channel_failure_error("boom", kind);
            // Code stays INTERNAL_ERROR for backward compatibility.
            assert_eq!(err.code, acp::ErrorCode::InternalError);
            assert_eq!(acp_channel_failure(&err), Some(kind));
        }
    }

    #[test]
    fn classifier_none_for_untagged_errors() {
        assert_eq!(acp_channel_failure(&acp_internal_error("plain")), None);
        // A different `with_data` payload must not be misread as a channel kind.
        assert_eq!(
            acp_channel_failure(&acp::Error::invalid_params().data("unknown session id")),
            None
        );
    }
}
