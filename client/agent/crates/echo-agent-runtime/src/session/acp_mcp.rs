//! In-process SDK MCP servers over the ACP reverse channel (`echo.agent/mcp/sdk_call`).
//!
//! The official `echo-agent-agent-sdk` lets a host define in-process tools (`@tool` /
//! `create_sdk_mcp_server`). When `transport="acp"`, the SDK registers them in
//! `session/new` `_meta["echo.agent/mcp/servers"] = [{ "name", "serverId" }]` and the agent
//! invokes their tools by sending each MCP JSON-RPC message back to the client as a
//! reverse `echo.agent/mcp/sdk_call` request — handled here by [`GatewayAcpInvoker`].
//!
//! NOTE: the *reverse* route (agent -> client, `echo.agent/mcp/sdk_call`) invokes a tool that
//! lives in the SDK's process. It is the zero-IPC mirror of the *forward* route (client
//! -> agent, `echo.agent/mcp/call` in `extensions::mcp`), which invokes a tool on a server the
//! AGENT is connected to. They use distinct method strings and sit on opposite request
//! handlers, so they never collide.

use std::time::Duration;

use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender;
use echo_agent_mcp::acp_transport::AcpReverseInvoker;
use echo_agent_mcp::servers::AcpServerEntry;
use echo_agent_mcp::wire;

/// Parse `_meta["echo.agent/mcp/servers"]` into [`AcpServerEntry`] registrations. Each entry
/// is deserialized directly into the canonical type (so the `serverId` wire field is
/// serde-checked, not hand-read); entries missing `name`/`serverId` are skipped with a
/// warning. A name seen twice keeps the first (server names are the tool namespace, so a
/// duplicate would otherwise silently shadow). Absent meta yields none.
pub(crate) fn parse_acp_mcp_servers(meta: Option<&acp::Meta>) -> Vec<AcpServerEntry> {
    let Some(array) = meta
        .and_then(|m| m.get(wire::MCP_SERVERS))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut servers = Vec::new();
    for entry in array {
        let server: AcpServerEntry = match serde_json::from_value(entry.clone()) {
            Ok(server) => server,
            Err(err) => {
                tracing::warn!(entry = %entry, %err, "ignoring malformed echo.agent/mcp/servers entry");
                continue;
            }
        };
        if !seen.insert(server.name.clone()) {
            tracing::warn!(name = %server.name, "ignoring duplicate echo.agent/mcp/servers entry");
            continue;
        }
        servers.push(server);
    }
    servers
}

/// Reverse-RPC invoker for in-process SDK MCP servers.
///
/// Each [`invoke`](AcpReverseInvoker::invoke) sends one `echo.agent/mcp/sdk_call` reverse request
/// straight through the gateway. `AcpAgentGatewaySender::send` returns a `Send` future
/// (unlike the `?Send` `acp::Client::ext_method` trait method), so the rmcp transport's
/// `Send` invoker bound is satisfied with no relay task. Calls are independent and may
/// run concurrently — the gateway serializes them onto the session's message channel.
pub(crate) struct GatewayAcpInvoker {
    gateway: AcpAgentGatewaySender,
}

impl GatewayAcpInvoker {
    pub(crate) fn new(gateway: AcpAgentGatewaySender) -> Self {
        Self { gateway }
    }
}

/// Reverse `echo.agent/mcp/sdk_call` params. Declares the on-wire field names once (mirrors
/// the forward side's typed `McpCallRequest`) so the `serverId` literal isn't hand-spelled.
#[derive(serde::Serialize)]
struct SdkCallParams<'a> {
    #[serde(rename = "serverId")]
    server_id: &'a str,
    message: serde_json::Value,
}

#[async_trait::async_trait]
impl AcpReverseInvoker for GatewayAcpInvoker {
    async fn invoke(
        &self,
        server_id: &str,
        message: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let params = serde_json::value::to_raw_value(&SdkCallParams { server_id, message })
            .map_err(|err| err.to_string())?;
        let request = acp::ExtRequest::new(wire::MCP_SDK_CALL, params.into());
        // Bound the round trip so a missing or hung client fails this reverse call at
        // the configured per-server tool timeout rather than stalling the tool loop.
        let response = tokio::time::timeout(timeout, self.gateway.send(request))
            .await
            .map_err(|_| {
                format!(
                    "{} to server {server_id} timed out after {}ms",
                    wire::MCP_SDK_CALL,
                    timeout.as_millis()
                )
            })?
            .map_err(|err| err.to_string())?;
        serde_json::from_str(response.0.get()).map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_entries_and_skips_malformed() {
        let meta = serde_json::json!({
            "echo.agent/mcp/servers": [
                { "name": "harness-tools", "serverId": "srv_0" },
                { "name": "missing-id" },
                { "serverId": "no_name" },
            ]
        });
        let servers = parse_acp_mcp_servers(meta.as_object());
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "harness-tools");
        assert_eq!(servers[0].server_id, "srv_0");
    }

    #[test]
    fn duplicate_names_keep_the_first() {
        let meta = serde_json::json!({
            "echo.agent/mcp/servers": [
                { "name": "tools", "serverId": "srv_0" },
                { "name": "tools", "serverId": "srv_1" },
            ]
        });
        let servers = parse_acp_mcp_servers(meta.as_object());
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].server_id, "srv_0");
    }

    #[test]
    fn absent_meta_yields_none() {
        assert!(parse_acp_mcp_servers(None).is_empty());
        assert!(parse_acp_mcp_servers(serde_json::json!({}).as_object()).is_empty());
    }
}
