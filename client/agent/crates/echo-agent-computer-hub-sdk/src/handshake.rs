//! Hello handshake helpers used by the connection actor and the
//! reconnect-replay path.
//!
//! Splitting these into a dedicated module keeps the connection state
//! machine readable: send the frame, parse the ack, surface a typed
//! [`crate::ClientError`].

use echo_agent_tool_protocol::{ConnectionKind, HelloAckMsg, HelloMsg};
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use crate::error::ClientError;

/// Wire-protocol version both ends speak. Re-exported from the
/// protocol crate so the SDK and the IC service share one source of
/// truth.
pub use echo_agent_tool_protocol::PROTOCOL_VERSION;

/// Send the [`HelloMsg`] and wait for the matching [`HelloAckMsg`].
///
/// `kind` should be [`ConnectionKind::ToolServer`] for tool-server
/// builds (the only consumer today). The function returns the parsed
/// ack so callers can observe the server-issued `connection_id` and
/// the server-derived `user_id`.
///
/// When `server_id` is `Some`, it is included in the hello frame so the
/// server can identify itself without a separate `register_server` call.
pub async fn send_hello<Si, St>(
    sink: &mut Si,
    stream: &mut St,
    kind: ConnectionKind,
    server_id: Option<echo_agent_tool_protocol::ServerId>,
    description: Option<String>,
    metadata: Option<serde_json::Value>,
) -> Result<HelloAckMsg, ClientError>
where
    Si: SinkExt<Message> + Unpin,
    Si::Error: std::fmt::Display,
    St: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let hello = HelloMsg {
        protocol_version: PROTOCOL_VERSION.to_owned(),
        kind,
        server_id,
        description,
        metadata,
    };
    let text = serde_json::to_string(&hello)?;
    sink.send(Message::Text(text.into()))
        .await
        .map_err(|e| ClientError::NetworkError(format!("hello send failed: {e}")))?;

    while let Some(msg) = stream.next().await {
        let msg = msg?;
        match msg {
            Message::Text(text) => {
                let ack: HelloAckMsg = serde_json::from_str(text.as_ref())
                    .map_err(|e| ClientError::ProtocolError(format!("malformed hello_ack: {e}")))?;
                if !ack
                    .supported_protocol_versions
                    .iter()
                    .any(|v| v == PROTOCOL_VERSION)
                {
                    return Err(ClientError::ProtocolError(format!(
                        "server does not support {PROTOCOL_VERSION}; supported: {:?}",
                        ack.supported_protocol_versions
                    )));
                }
                return Ok(ack);
            }
            Message::Ping(payload) => {
                sink.send(Message::Pong(payload))
                    .await
                    .map_err(|e| ClientError::NetworkError(format!("pong send failed: {e}")))?;
            }
            Message::Close(frame) => {
                let reason = frame.map(|f| f.reason.to_string()).unwrap_or_default();
                return Err(ClientError::Closed(format!(
                    "server closed during handshake: {reason}"
                )));
            }
            Message::Pong(_) | Message::Frame(_) => continue,
            Message::Binary(_) => {
                return Err(ClientError::ProtocolError(
                    "server sent binary frame during handshake".to_owned(),
                ));
            }
        }
    }
    Err(ClientError::NetworkError(
        "server closed before hello_ack".to_owned(),
    ))
}
