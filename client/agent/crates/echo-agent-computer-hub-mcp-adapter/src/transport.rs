//! MCP transport abstraction.
//!
//! [`McpTransport`] defines the async interface consumed by [`crate::McpBridge`].
//! Concrete implementations (stdio, HTTP+SSE) live outside this crate;
//! the trait boundary keeps the bridge testable with in-memory mocks.

use async_trait::async_trait;
use serde_json::Value;

use crate::types::{McpCallResult, McpError, McpServerInfo, McpToolDefinition};

/// Async interface to a single MCP server connection.
///
/// Implementations manage the underlying JSON-RPC framing (stdio pipe,
/// HTTP+SSE stream, etc.) and expose the four lifecycle operations the
/// bridge needs.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Perform the MCP `initialize` handshake with the server.
    ///
    /// Must be called exactly once before any other method. Returns
    /// the server's advertised name, version, and capabilities.
    async fn initialize(&self) -> Result<McpServerInfo, McpError>;

    /// Discover available tools via MCP `tools/list`.
    async fn list_tools(&self) -> Result<Vec<McpToolDefinition>, McpError>;

    /// Invoke a tool via MCP `tools/call`.
    ///
    /// `arguments` is the JSON object the model produced for the tool's
    /// input schema.
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpCallResult, McpError>;

    /// Gracefully shut down the transport (close pipes, drop connections).
    /// Implementations must be idempotent — a second call after a
    /// successful close must return `Ok(())` without error.
    async fn close(&self) -> Result<(), McpError>;
}
