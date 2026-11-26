//! Object-safe `ToolDispatch` trait — the runtime contract for handling tool calls.
//!
//! `Tool` itself is not object-safe (it carries associated `Args` /
//! `Output` types), so implementations expose a JSON-typed surface and rely on
//! per-tool adapters to encode/decode at the boundary. The default
//! `call_terminal` impl drains the stream so the common "I just want the
//! result" path doesn't have to depend on `futures` internals.
//!
//! This crate is upstream of every concrete impl. Doc-comments here describe
//! trait semantics in terms of "the runtime" or "the implementation" —
//! concrete dispatch routers live downstream and are intentionally not named
//! here.

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;

use echo_agent_tool_protocol::ToolId;

use crate::context::ToolCallContext;
use crate::error::ToolError;
use crate::tool::{ToolStream, ToolStreamItem, TypedToolOutput};

/// Object-safe tool dispatch interface.
///
/// Implementations route the `tool_id` to the correct tool, decode `args`
/// against the tool's typed `Args`, and return the streaming result as
/// [`TypedToolOutput`] — preserving model-facing content blocks and
/// optional chat-completion metadata end-to-end. Raw `Value` only appears
/// at JSON-RPC wire encode/decode boundaries.
#[async_trait]
pub trait ToolDispatch: Send + Sync {
    /// Streaming dispatch. The returned stream MUST end with exactly one
    /// `Terminal` item per the [`ToolStream`] invariant.
    async fn call(
        &self,
        tool_id: ToolId,
        args: Value,
        ctx: ToolCallContext,
    ) -> ToolStream<TypedToolOutput>;

    /// Drain the stream and return only the terminal result. Useful for
    /// callers that don't care about progress chunks.
    ///
    /// Default impl pulls items off the stream and discards `Progress`
    /// items; the first `Terminal` short-circuits. A stream that ends
    /// without a `Terminal` is a protocol violation by the implementation;
    /// the default surfaces this as `ToolError::Custom { code:
    /// "stream_no_terminal", ... }`.
    async fn call_terminal(
        &self,
        tool_id: ToolId,
        args: Value,
        ctx: ToolCallContext,
    ) -> Result<TypedToolOutput, ToolError> {
        let mut stream = self.call(tool_id, args, ctx).await;
        while let Some(item) = stream.next().await {
            match item {
                ToolStreamItem::Progress(_) => continue,
                ToolStreamItem::Terminal(result) => return result,
            }
        }
        Err(ToolError::custom(
            "stream_no_terminal",
            "dispatch stream ended without a terminal item",
        ))
    }
}
