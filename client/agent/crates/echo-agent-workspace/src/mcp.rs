//! MCP integration for the workspace server.
//!
//! Bridges [`McpClient`] to the server's [`McpTransport`] trait and wraps
//! tool handlers with qualified `server__tool` names.

use std::sync::Arc;

use async_trait::async_trait;
use echo_agent_computer_hub_mcp_adapter::{
    McpBridgeConfig, McpCallResult, McpContent, McpServerInfo, McpToolDefinition, McpToolHandler,
    McpTransport,
};
use echo_agent_computer_hub_sdk::ToolServerHandler;
use echo_agent_mcp::rmcp;
use echo_agent_mcp::servers::{McpClient, parse_mcp_qualified_name};
use echo_agent_tool_protocol::ToolId;
use echo_agent_tool_runtime::{ToolCallContext, ToolStream, TypedToolOutput};
use echo_agent_tool_types::ToolDescription;
use serde_json::Value;

/// Adapts [`McpClient`] to the [`McpTransport`] trait for [`McpBridge`].
pub(crate) struct McpClientTransportAdapter {
    client: Arc<McpClient>,
}

impl McpClientTransportAdapter {
    pub fn new(client: Arc<McpClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl McpTransport for McpClientTransportAdapter {
    async fn initialize(
        &self,
    ) -> Result<McpServerInfo, echo_agent_computer_hub_mcp_adapter::McpError> {
        let service =
            self.client.ensure_initialized().await.map_err(|e| {
                echo_agent_computer_hub_mcp_adapter::McpError::Transport(e.to_string())
            })?;
        let info = service.peer_info().ok_or_else(|| {
            echo_agent_computer_hub_mcp_adapter::McpError::Transport(
                "no peer info after init".into(),
            )
        })?;
        let server = info.server_info.as_ref().ok_or_else(|| {
            echo_agent_computer_hub_mcp_adapter::McpError::Transport(
                "no server identity after init".into(),
            )
        })?;
        Ok(McpServerInfo {
            name: server.name.clone(),
            version: server.version.clone(),
            capabilities: serde_json::to_value(&info.capabilities).unwrap_or_default(),
        })
    }

    async fn list_tools(
        &self,
    ) -> Result<Vec<McpToolDefinition>, echo_agent_computer_hub_mcp_adapter::McpError> {
        let service =
            self.client.ensure_initialized().await.map_err(|e| {
                echo_agent_computer_hub_mcp_adapter::McpError::Transport(e.to_string())
            })?;

        let mut all_tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let result = service
                .list_tools(Some(
                    rmcp::model::PaginatedRequestParams::default().with_cursor(cursor.clone()),
                ))
                .await
                .map_err(|e| {
                    echo_agent_computer_hub_mcp_adapter::McpError::Transport(e.to_string())
                })?;

            all_tools.extend(result.tools.into_iter().map(|t| McpToolDefinition {
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()),
                input_schema: serde_json::to_value(&t.input_schema).ok(),
            }));

            match result.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        Ok(all_tools)
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<McpCallResult, echo_agent_computer_hub_mcp_adapter::McpError> {
        let service =
            self.client.ensure_initialized().await.map_err(|e| {
                echo_agent_computer_hub_mcp_adapter::McpError::Transport(e.to_string())
            })?;
        // MCP spec requires arguments to be an object; coerce if needed.
        let args_object = match arguments {
            Value::Object(map) => Some(map),
            Value::Null => None,
            other => {
                let mut wrapper = serde_json::Map::new();
                wrapper.insert("value".to_string(), other);
                Some(wrapper)
            }
        };
        let result = service
            .call_tool({
                let mut params = rmcp::model::CallToolRequestParams::new(name.to_string());
                params.arguments = args_object;
                params
            })
            .await
            .map_err(|e| echo_agent_computer_hub_mcp_adapter::McpError::Transport(e.to_string()))?;

        Ok(McpCallResult {
            content: result
                .content
                .into_iter()
                .map(|c| match c {
                    rmcp::model::ContentBlock::Text(t) => McpContent::Text { text: t.text },
                    rmcp::model::ContentBlock::Image(img) => McpContent::Image {
                        mime_type: img.mime_type,
                        data: img.data,
                    },
                    _ => McpContent::Text {
                        text: "[unsupported content type]".to_string(),
                    },
                })
                .collect(),
            is_error: result.is_error.unwrap_or(false),
        })
    }

    async fn close(&self) -> Result<(), echo_agent_computer_hub_mcp_adapter::McpError> {
        // No-op: cleanup happens when McpClient is dropped.
        Ok(())
    }
}

/// Wraps an [`McpToolHandler`] to qualify tool names as `server__tool`.
pub(crate) struct QualifiedMcpToolHandler {
    qualified_id: ToolId,
    qualified_name: String,
    inner: Arc<McpToolHandler>,
}

impl QualifiedMcpToolHandler {
    /// Returns `None` if the qualified name is invalid or ambiguous.
    pub fn try_new(qualified_name: String, inner: Arc<McpToolHandler>) -> Option<Self> {
        let qualified_id = match parse_mcp_qualified_name(&qualified_name) {
            Some((id, _, _)) => id,
            None => {
                tracing::warn!(
                    qualified_name,
                    "skipping MCP tool: qualified name is invalid or ambiguous"
                );
                return None;
            }
        };
        Some(Self {
            qualified_id,
            qualified_name,
            inner,
        })
    }
}

#[async_trait]
impl ToolServerHandler for QualifiedMcpToolHandler {
    fn tool_id(&self) -> ToolId {
        self.qualified_id.clone()
    }

    fn description(&self) -> ToolDescription {
        let inner_desc = self.inner.description();
        ToolDescription::new(self.qualified_name.clone(), inner_desc.description)
    }

    fn input_schema(&self) -> Option<Value> {
        self.inner.input_schema()
    }

    async fn handle_call(&self, ctx: ToolCallContext, args: Value) -> ToolStream<TypedToolOutput> {
        self.inner.handle_call(ctx, args).await
    }
}

/// Result of a `workspace.configure_mcp` RPC call.
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpStartResult {
    /// Server names that started successfully.
    pub started: Vec<String>,
    /// Servers that failed to start.
    pub failed: Vec<McpStartFailure>,
}

/// A single MCP server startup failure.
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpStartFailure {
    /// Server name.
    pub name: String,
    /// Human-readable error description.
    pub error: String,
}

/// Extract a server name from an [`McpError`](echo_agent_mcp::servers::McpError),
/// falling back to `"unknown"`.
pub(crate) fn server_name_from_mcp_error(e: &echo_agent_mcp::servers::McpError) -> &str {
    e.server_name().unwrap_or("unknown")
}

/// Bridge config factory for MCP bridge connections.
pub(crate) fn make_bridge_config(
    session_id: echo_agent_tool_protocol::SessionId,
    server_name: &str,
) -> McpBridgeConfig {
    McpBridgeConfig {
        session_id,
        namespace: Some(server_name.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_computer_hub_mcp_adapter::{McpBridge, McpError};
    use echo_agent_tool_protocol::SessionId;

    struct TestTransport;

    #[async_trait]
    impl McpTransport for TestTransport {
        async fn initialize(&self) -> Result<McpServerInfo, McpError> {
            Ok(McpServerInfo {
                name: "test".to_owned(),
                version: "1".to_owned(),
                capabilities: Value::Null,
            })
        }

        async fn list_tools(&self) -> Result<Vec<McpToolDefinition>, McpError> {
            Ok(vec![McpToolDefinition {
                name: "tool".to_owned(),
                description: None,
                input_schema: None,
            }])
        }

        async fn call_tool(
            &self,
            _name: &str,
            _arguments: Value,
        ) -> Result<McpCallResult, McpError> {
            unreachable!("constructor test does not call the tool")
        }

        async fn close(&self) -> Result<(), McpError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn qualified_handler_rejects_ambiguous_name() {
        let bridge = McpBridge::connect(
            Arc::new(TestTransport),
            &make_bridge_config(SessionId::new("session").unwrap(), "test"),
        )
        .await
        .unwrap()
        .bridge;
        let inner = bridge.handlers()[0].clone();

        let valid = QualifiedMcpToolHandler::try_new("123__lookup".to_owned(), inner.clone())
            .expect("valid qualified ToolId");
        assert_eq!(valid.tool_id().as_str(), "123__lookup");
        assert!(QualifiedMcpToolHandler::try_new("foo___bar".to_owned(), inner).is_none());
    }
}
