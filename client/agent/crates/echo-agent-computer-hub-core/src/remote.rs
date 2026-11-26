//! `ConnectionClient` abstraction, `RemoteToolProxy`, and
//! `RemoteTransport`.
//!
//! `ConnectionClient` is the thin contract a downstream WebSocket SDK (or
//! an in-test channel-backed mock) implements; this crate stays free of
//! tokio-runtime / tokio-tungstenite deps so callers can pick their own.
//!
//! `RemoteToolProxy` wraps a remote tool registration so it implements
//! [`ToolHandle`] — the router routes through the same handle
//! type for local and remote registrations. `RemoteTransport` is the
//! transport-side equivalent: it forwards arbitrary `(tool_id, args)`
//! pairs over a [`ConnectionClient`] without needing a per-tool handle.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::Stream;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde_json::Value;
use tracing::warn;

use echo_agent_tool_protocol::{
    JsonRpcId, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, JsonRpcVersion, Method,
    ResponseOutcome, SessionId, ToolCallId, ToolCallParams, ToolCallProgressFrame, ToolCallResult,
    ToolCapabilities, ToolErrorWire, ToolId, ToolOutputWire, UserId, WORKSPACE_UNAVAILABLE_SUBCODE,
};
use echo_agent_tool_runtime::{
    BehaviorVersion, ContentBlock, Cwd, ListToolsContext, ToolCallContext,
    ToolChatCompletionResponse, ToolError, ToolErrorKind, ToolProgress, ToolStream, ToolStreamItem,
    TypedToolOutput, terminal_only,
};
use echo_agent_tool_types::ToolDescription;

use crate::resolver::ToolHandle;
use crate::transport::{Principal, Transport, TransportKind};

/// Object-safe contract for a connected remote endpoint.
///
/// Concrete implementations supply the wire transport — the Rust SDK uses
/// `tokio_tungstenite`; tests use channel-backed mocks. Implementations
/// are expected to:
///
/// - correlate request/response pairs by [`JsonRpcId`];
/// - deliver progress notifications matching `tool_call_id` to whichever
///   subscriber registered for them;
/// - surface transport-level disconnects as [`ToolError::NetworkError`].
#[async_trait]
pub trait ConnectionClient: Send + Sync + std::fmt::Debug {
    /// Send a JSON-RPC request and await the matching response. Errors
    /// signal a transport-level failure (write failed, connection closed
    /// before the response arrived); a successful return carries the
    /// response envelope verbatim, including method-level error outcomes.
    async fn request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse, ToolError>;

    /// Subscribe to progress notifications for `tool_call_id`.
    ///
    /// The returned stream closes when the call's terminal frame arrives,
    /// when the connection drops, or when the caller drops the receiver.
    /// Subscribers MUST be registered before the corresponding request is
    /// sent — otherwise progress frames that arrive before subscription
    /// is complete are lost.
    async fn subscribe_progress(
        &self,
        tool_call_id: ToolCallId,
    ) -> BoxStream<'static, ToolCallProgressFrame>;

    /// Send a one-way notification (no response expected). Useful for
    /// hook frames such as cancel.
    async fn notify(&self, notification: JsonRpcNotification) -> Result<(), ToolError>;
}

/// Wraps a remote registration so it dispatches through a connection.
///
/// Identity, description, and capabilities come from the registration
/// snapshot held on the proxy; execution forwards a `tool_call_request`
/// over the connection and merges progress + terminal frames into a
/// single [`ToolStream`].
#[derive(Debug, Clone)]
pub struct RemoteToolProxy {
    tool_id: ToolId,
    session_id: SessionId,
    description: ToolDescription,
    capabilities: ToolCapabilities,
    connection: Arc<dyn ConnectionClient>,
}

impl RemoteToolProxy {
    /// Build a proxy bound to a single remote registration.
    pub fn new(
        tool_id: ToolId,
        session_id: SessionId,
        description: ToolDescription,
        capabilities: ToolCapabilities,
        connection: Arc<dyn ConnectionClient>,
    ) -> Self {
        Self {
            tool_id,
            session_id,
            description,
            capabilities,
            connection,
        }
    }

    /// Bound session identifier.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

#[async_trait]
impl ToolHandle for RemoteToolProxy {
    fn id(&self) -> ToolId {
        self.tool_id.clone()
    }

    fn description(&self, _ctx: &ListToolsContext) -> ToolDescription {
        self.description.clone()
    }

    fn capabilities(&self) -> ToolCapabilities {
        self.capabilities.clone()
    }

    async fn execute(&self, ctx: ToolCallContext, args: Value) -> ToolStream<TypedToolOutput> {
        dispatch_via_connection(
            Arc::clone(&self.connection),
            self.tool_id.clone(),
            self.session_id.clone(),
            args,
            ctx,
        )
        .await
    }
}

/// Transport that forwards calls over a [`ConnectionClient`].
///
/// The transport is bound to a single `(user_id, session_id)` at
/// construction. Calls do not require a pre-built proxy — the transport
/// builds the request frame from the `tool_id` it is asked to dispatch.
#[derive(Debug)]
pub struct RemoteTransport {
    connection: Arc<dyn ConnectionClient>,
    session_id: SessionId,
    user_id: UserId,
}

impl RemoteTransport {
    /// Build a transport over `connection`, bound to `(user_id,
    /// session_id)`.
    pub fn new(
        connection: Arc<dyn ConnectionClient>,
        session_id: SessionId,
        user_id: UserId,
    ) -> Self {
        Self {
            connection,
            session_id,
            user_id,
        }
    }

    /// Bound session identifier.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Bound user identifier.
    pub fn user_id(&self) -> &UserId {
        &self.user_id
    }
}

#[async_trait]
impl Transport for RemoteTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Remote
    }

    async fn authorize(&self) -> Result<Principal, ToolError> {
        Ok(Principal::new(self.user_id.clone()).with_session(self.session_id.clone()))
    }

    async fn call(
        &self,
        tool_id: ToolId,
        args: Value,
        ctx: ToolCallContext,
    ) -> ToolStream<TypedToolOutput> {
        dispatch_via_connection(
            Arc::clone(&self.connection),
            tool_id,
            self.session_id.clone(),
            args,
            ctx,
        )
        .await
    }
}

/// Subscribe to progress for `ctx.call_id`, send the `tool_call_request`,
/// and return a stream interleaving progress frames with the eventual
/// terminal item.
///
/// Subscribing **before** sending is the contract that
/// [`ConnectionClient::subscribe_progress`] requires; doing so here keeps
/// individual transports / proxies from re-implementing the dance.
async fn dispatch_via_connection(
    connection: Arc<dyn ConnectionClient>,
    tool_id: ToolId,
    session_id: SessionId,
    arguments: Value,
    ctx: ToolCallContext,
) -> ToolStream<TypedToolOutput> {
    let cwd = ctx
        .extensions
        .get::<Cwd>()
        .map(|c| c.0.to_string_lossy().into_owned());
    let behavior_version = ctx.extensions.get::<BehaviorVersion>().map(|v| v.0.clone());
    let call_id = ctx.call_id;

    // Subscribe BEFORE sending. The single remaining `call_id.clone()`
    // is unavoidable: subscription needs an owned id and the same id has
    // to land in the request params below.
    let progress = connection.subscribe_progress(call_id.clone()).await;

    let params = ToolCallParams {
        tool_call_id: call_id,
        tool_id,
        arguments,
        deadline_ms: None,
        behavior_version,
        cwd,
        // The ctx `TraceContext` extension is receive-side state.
        trace_context: None,
    };

    let request = JsonRpcRequest {
        jsonrpc: JsonRpcVersion,
        id: JsonRpcId::new_uuid_v7(),
        session_id: Some(session_id),
        method: Method::ToolCallRequest.as_wire_str().to_string(),
        params: match serde_json::to_value(&params) {
            Ok(v) => v,
            Err(e) => {
                return terminal_only(Err(ToolError::custom("request_encoding", e.to_string())));
            }
        },
    };

    // Build the response future without awaiting it here so progress and
    // terminal can be polled concurrently from the returned stream.
    let request_fut = Box::pin(async move { connection.request(request).await });

    Box::pin(RequestStream {
        tool_id: Some(params.tool_id),
        progress,
        request: Some(request_fut),
        done: false,
    })
}

/// Owned response future with `'static` lifetime so the stream can hold
/// it across polls.
type ResponseFuture = BoxFuture<'static, Result<JsonRpcResponse, ToolError>>;

/// Stream that interleaves wire-side progress frames with the eventual
/// JSON-RPC response, ending with exactly one terminal item.
struct RequestStream {
    /// Consumed exactly once when the terminal is built.
    tool_id: Option<ToolId>,
    progress: BoxStream<'static, ToolCallProgressFrame>,
    request: Option<ResponseFuture>,
    done: bool,
}

impl Stream for RequestStream {
    type Item = ToolStreamItem<TypedToolOutput>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }

        // Poll the response first so the terminal short-circuits the
        // moment it lands. Any progress frames that arrived alongside
        // the response are dropped — once `Terminal` is emitted, `done`
        // is set and the next poll returns `None` immediately without
        // re-polling the progress stream. The router invariant is
        // "`Progress* Terminal`, exactly one terminal"; dropping any
        // post-terminal progress is what makes that invariant hold here.
        if let Some(req_fut) = self.request.as_mut() {
            match req_fut.as_mut().poll(cx) {
                Poll::Ready(result) => {
                    self.done = true;
                    self.request = None;
                    let Some(tool_id) = self.tool_id.take() else {
                        return Poll::Ready(None);
                    };
                    let terminal = match result {
                        Ok(resp) => terminal_from_response(tool_id, resp),
                        Err(err) => Err(err),
                    };
                    return Poll::Ready(Some(ToolStreamItem::Terminal(terminal)));
                }
                Poll::Pending => {}
            }
        } else {
            self.done = true;
            return Poll::Ready(None);
        }

        // Poll the progress stream while the request is pending. Closing
        // the progress stream is fine — the response future is still
        // registered for wake-up.
        match Pin::new(&mut self.progress).poll_next(cx) {
            Poll::Ready(Some(frame)) => {
                Poll::Ready(Some(ToolStreamItem::Progress(progress_from_frame(frame))))
            }
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
    }
}

/// Map a wire-side [`ToolCallProgressFrame`] into a runtime
/// [`ToolProgress`]. `kind` becomes the `Custom` subkind so callers can
/// dispatch on the producer-defined identifier without losing the body.
pub fn progress_from_frame(frame: ToolCallProgressFrame) -> ToolProgress {
    ToolProgress::Custom {
        subkind: frame.kind,
        payload: frame.body,
    }
}

/// Decode the response envelope into the terminal
/// `Result<TypedToolOutput, _>` the runtime expects.
fn terminal_from_response(
    tool_id: ToolId,
    resp: JsonRpcResponse,
) -> Result<TypedToolOutput, ToolError> {
    match resp.outcome {
        ResponseOutcome::Result(value) => decode_call_result(tool_id, value),
        ResponseOutcome::Error(err) => Err(error_from_envelope(err)),
    }
}

/// Decode a `tool_call_result` success body into the terminal
/// [`TypedToolOutput`]. Shared by the core remote proxy and the SDK
/// harness so both wire decoders reconstruct `chat_completion_output`
/// identically.
///
/// A body with a `tool_call_id` is decoded strictly (`response_decoding` on
/// failure), reconstructing `chat_completion_output` (an unparseable cco
/// degrades to `None`). A bare body — e.g. a hub-local tool's raw output —
/// passes through unchanged.
pub fn decode_call_result(tool_id: ToolId, value: Value) -> Result<TypedToolOutput, ToolError> {
    if value.get("tool_call_id").is_none() {
        return Ok(TypedToolOutput::from_value(tool_id, value));
    }
    let result: ToolCallResult = serde_json::from_value(value)
        .map_err(|e| ToolError::custom("response_decoding", e.to_string()))?;
    let chat_completion_output = result.chat_completion_output.and_then(|cco| {
        serde_json::from_value::<ToolChatCompletionResponse>(cco)
            .inspect_err(|e| {
                warn!(tool_id = %tool_id, error = %e, "dropping unparseable chat_completion_output");
            })
            .ok()
    });
    let value = output_to_value(result.output);
    Ok(TypedToolOutput::from_value(tool_id, value)
        .with_chat_completion_output(chat_completion_output))
}

/// Project a wire [`ToolOutputWire`] into a JSON [`Value`].
///
/// Three shapes collapse to one runtime type:
/// - `Text` becomes a JSON string;
/// - `Json` is forwarded verbatim;
/// - `Mcp { blocks }` is re-serialised as `{ "blocks": [ContentBlock, ...] }`
///   so the same downstream decoder used for in-process content blocks
///   works without case-by-case adaptation.
pub fn output_to_value(output: ToolOutputWire) -> Value {
    match output {
        ToolOutputWire::Text(s) => Value::String(s),
        ToolOutputWire::Json(v) => v,
        ToolOutputWire::Mcp { blocks } => {
            let runtime_blocks: Vec<ContentBlock> = blocks.into_iter().map(map_block).collect();
            // `ContentBlock`'s derived `Serialize` impl never fails for any
            // valid in-memory variant, but `to_value` is fallible at the
            // type level; collapse a hypothetical failure to `Value::Null`
            // before wrapping so this function stays total without an
            // `unwrap`. The outer `json!` only sees a `Value` expression
            // (which `to_value` round-trips infallibly), so the macro's
            // hidden `to_value` call cannot panic here.
            let blocks_value = serde_json::to_value(&runtime_blocks).unwrap_or(Value::Null);
            serde_json::json!({ "blocks": blocks_value })
        }
    }
}

fn map_block(block: echo_agent_tool_protocol::McpBlock) -> ContentBlock {
    use echo_agent_tool_protocol::McpBlock;
    match block {
        McpBlock::Text { text } => ContentBlock::Text { text },
        McpBlock::Image { mime_type, data } => ContentBlock::Image {
            mime_type,
            data,
            media_id: None,
            filename: None,
            path: None,
            metadata: Default::default(),
        },
        McpBlock::Resource {
            uri,
            mime_type,
            text,
        } => ContentBlock::Resource {
            uri,
            mime_type,
            text,
        },
    }
}

/// Decode a JSON-RPC error envelope into a [`ToolError`]. The envelope's
/// `data` field is expected to carry a serialised [`ToolErrorWire`] when
/// available; falls back to a [`ToolError::Custom`] keyed by the numeric
/// envelope code when the data shape is unknown.
pub fn error_from_envelope(err: echo_agent_tool_protocol::JsonRpcError) -> ToolError {
    if let Some(data) = err.data.clone()
        && let Ok(wire) = serde_json::from_value::<ToolErrorWire>(data)
    {
        return tool_error_from_wire(wire);
    }
    let mut e = ToolError::custom(format!("jsonrpc_{}", err.code), err.message);
    if let Some(data) = err.data {
        e = e.with_details(data);
    }
    e
}

/// Recognize the hub's `workspace_unavailable` error on an already-decoded
/// [`ToolError`]. Keys on `details["code"]` — the field that survives
/// `ToolError::custom` + `with_details` — not the numeric code or the wire
/// `Custom.subcode`.
pub fn is_workspace_unavailable(err: &ToolError) -> bool {
    err.kind == ToolErrorKind::Custom
        && err
            .details
            .as_ref()
            .and_then(|d| d.get("code"))
            .and_then(|v| v.as_str())
            == Some(WORKSPACE_UNAVAILABLE_SUBCODE)
}

/// Map [`ToolErrorWire`] back into the runtime [`ToolError`]. The runtime
/// error variants are the source-of-truth taxonomy; the wire form is a
/// lossy projection onto stable codes for serialisation, so a few wire
/// variants land on [`ToolError::Custom`] keyed by their wire code
/// rather than a dedicated runtime variant.
pub fn tool_error_from_wire(wire: ToolErrorWire) -> ToolError {
    match wire {
        ToolErrorWire::InvalidArguments { message, details } => {
            let e = ToolError::invalid_arguments(message);
            match details {
                Some(d) => e.with_details(d),
                None => e,
            }
        }
        ToolErrorWire::ToolNotFound { tool_id } => {
            let detail = format!("tool not found: {tool_id}");
            ToolError::not_found(tool_id, detail)
        }
        ToolErrorWire::PermissionDenied { reason } => ToolError::permission_denied(reason),
        ToolErrorWire::Timeout {
            tool_id,
            elapsed_ms,
        } => ToolError::new(
            ToolErrorKind::Timeout,
            format!("timed out after {elapsed_ms}ms"),
        )
        .with_details(serde_json::json!({"tool_id": tool_id.as_str(), "elapsed_ms": elapsed_ms})),
        ToolErrorWire::Cancelled { tool_id } => ToolError::cancelled(tool_id, "cancelled"),
        ToolErrorWire::Execution { tool_id, message } => ToolError::execution(tool_id, message),
        ToolErrorWire::BehaviorVersionUnsupported { tool_id, requested } => ToolError::new(
            ToolErrorKind::BehaviorVersionUnsupported,
            format!("behavior version {requested} not supported"),
        )
        .with_details(serde_json::json!({"tool_id": tool_id.as_str(), "requested": requested})),
        ToolErrorWire::RenderLimited {
            tool_id,
            card_id,
            reason,
        } => ToolError::new(ToolErrorKind::RenderLimited, reason)
            .with_details(serde_json::json!({"tool_id": tool_id.as_str(), "card_id": card_id})),
        ToolErrorWire::TerminalError { tool_id, message } => {
            ToolError::terminal_error(tool_id, message)
        }
        ToolErrorWire::Custom {
            subcode,
            message,
            details,
        } => {
            let e = ToolError::custom(subcode, message);
            match details {
                Some(d) => e.with_details(d),
                None => e,
            }
        }
        ToolErrorWire::SessionMismatch => ToolError::custom("session_mismatch", "session mismatch"),
        ToolErrorWire::TransportClosed { tool_id } => {
            ToolError::network_error(format!("transport closed for {tool_id}"))
        }
        ToolErrorWire::UnsupportedProtocolVersion { supported } => ToolError::custom(
            "unsupported_protocol_version",
            format!("supported versions: {supported:?}"),
        ),
        ToolErrorWire::PayloadTooLarge { bytes, limit } => ToolError::custom(
            "payload_too_large",
            format!("payload {bytes} bytes exceeds limit {limit}"),
        ),
        ToolErrorWire::Internal { request_id, detail } => {
            let e = ToolError::custom(
                "internal_error",
                detail.unwrap_or_else(|| "internal router error".to_owned()),
            );
            match request_id {
                // Keep `code` alongside `request_id`: `with_details` replaces
                // the `{"code": …}` object `ToolError::custom` installed.
                Some(id) => e.with_details(
                    serde_json::json!({ "code": "internal_error", "request_id": id.as_str() }),
                ),
                None => e,
            }
        }
    }
}
