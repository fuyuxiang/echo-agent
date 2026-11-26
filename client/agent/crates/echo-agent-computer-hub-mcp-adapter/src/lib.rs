//! Unified MCP adapter for the EchoAgent Computer Hub.
//!
//! This crate bridges MCP (Model Context Protocol) servers into the
//! computer hub's tool routing infrastructure. An [`McpBridge`] connects
//! to an MCP server via an [`McpTransport`], discovers the server's
//! tools, and produces [`ToolServerHandler`](echo_agent_computer_hub_sdk::ToolServerHandler)
//! implementations that can be registered with a hub
//! [`ToolServerBuilder`](echo_agent_computer_hub_sdk::ToolServerBuilder).
//!
//! # Architecture
//!
//! ```text
//! MCP Server  <──McpTransport──>  McpBridge  ──handlers──>  ToolServerBuilder
//!   (stdio/SSE)                 (discover+forward)           (register with hub)
//! ```
//!
//! The [`McpTransport`] trait abstracts the wire protocol so the bridge
//! is testable with in-memory mocks. Concrete transport implementations
//! (stdio, HTTP+SSE) are provided by downstream crates.
//!
//! # Usage
//!
//! ```rust,ignore
//! let transport: Arc<dyn McpTransport> = /* ... */;
//! let config = McpBridgeConfig {
//!     session_id: SessionId::new("session-1").unwrap(),
//!     namespace: Some("my-mcp-server".into()),
//! };
//! let handle = McpBridge::connect(transport, &config).await?;
//!
//! let mut builder = ToolServerBuilder::default()
//!     .pool(pool)
//!     .url(hub_url)
//!     .auth(auth);
//!
//! for handler in handle.bridge.handlers() {
//!     builder = builder.tool(handler.clone());
//! }
//!
//! let server = builder.build().await?;
//! ```

#![forbid(unsafe_code)]

pub mod bridge;
pub(crate) mod metrics;
pub mod transport;
pub mod types;

pub use bridge::{McpBridge, McpBridgeConfig, McpBridgeHandle, McpToolHandler};
pub use transport::McpTransport;
pub use types::{McpCallResult, McpContent, McpError, McpServerInfo, McpToolDefinition};
