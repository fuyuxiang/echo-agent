//! MCP server re-exports + shell-side wrappers for timeout override resolution.

pub use echo_agent_mcp::servers::{
    AcpServerEntry, HttpConfig, MCP_TOOL_NAME_DELIMITER, McpClient, McpClientTimeoutOverrides,
    McpConfigDiff, McpError, McpInitStrategy, McpMetaConfigMap, McpServerMetaConfig, McpServerName,
    McpService, McpSpawnCtx, McpState, McpTool, McpToolRegistration, OauthInteractivity,
    SharedMcpPool, install_tool_dispatch_guard, mcp_server_name, mcp_target_str, mcp_transport_str, parse_mcp_meta_config,
    parse_mcp_tool_name, sanitize_descriptor_segment, validate_tool_name,
};

use std::collections::HashMap;
use std::path::Path;

use agent_client_protocol as acp;
use echo_agent_mcp::oauth_config::{McpOAuthConfig, McpOAuthConfigMap};
use echo_agent_mcp::servers as inner;

fn resolve_overrides(
    server_name: &str,
    cwd: Option<&Path>,
) -> Option<inner::McpClientTimeoutOverrides> {
    let config = match cwd {
        Some(cwd) => crate::util::config::get_mcp_server_config_with_project(server_name, cwd),
        None => crate::util::config::get_mcp_server_config(server_name),
    };
    // Fall back to the globally-resolved startup timeout so servers without a
    // per-server `startup_timeout_sec` (e.g. `~/.claude.json` imports) still get it.
    let global_startup = crate::util::config::resolved_mcp_startup_timeout_secs();
    Some(inner::McpClientTimeoutOverrides {
        startup_timeout_sec: config
            .as_ref()
            .and_then(|c| c.startup_timeout_sec)
            .or(Some(global_startup)),
        tool_timeout_sec: config.as_ref().and_then(|c| c.tool_timeout_sec),
        tool_timeouts: config.as_ref().and_then(|c| c.tool_timeouts.clone()),
        expose_image_base64: config.as_ref().and_then(|c| c.expose_image_base64),
    })
}

/// Build the config-resolved event data from a list of MCP server configs.
pub(crate) fn build_config_resolved_event(
    configs: &[acp::McpServer],
    cwd: &Path,
) -> echo_agent_session_events::Event {
    let disabled: Vec<String> = crate::util::config::disabled_mcp_server_names(cwd)
        .into_iter()
        .collect();
    let servers = configs
        .iter()
        .map(|c| echo_agent_session_events::McpConfigServer {
            name: inner::mcp_server_name(c).to_string(),
            transport: inner::mcp_transport_str(c).to_string(),
            source:
                match crate::session::mcp_dispatcher::classify_source(inner::mcp_server_name(c)) {
                    crate::extensions::mcp::McpServerSource::Managed => "managed",
                    crate::extensions::mcp::McpServerSource::Local => "local",
                }
                .to_string(),
        })
        .collect();
    echo_agent_session_events::Event::McpConfigResolved { servers, disabled }
}

pub(crate) async fn start_mcp_server(
    mcp_server: acp::McpServer,
    cwd: Option<&Path>,
    meta_config: Option<&inner::McpServerMetaConfig>,
    byo_config: Option<&McpOAuthConfig>,
    ctx: &inner::McpSpawnCtx<'_>,
) -> Result<inner::McpClient, inner::McpError> {
    let overrides = resolve_overrides(inner::mcp_server_name(&mcp_server), cwd);
    inner::start_mcp_server(mcp_server, overrides.as_ref(), meta_config, byo_config, ctx).await
}

/// Build all pending MCP clients for one init pass as a single merged list: config-declared
/// servers (HTTP/stdio, spawned lock-free via [`start_mcp_servers`]) and SDK in-process
/// servers (built under a brief lock via `McpState::build_pending_acp_clients`). SDK clients
/// never fail to build, so they enter as `Ok`. One entry point so the init batch doesn't
/// invoke two builders.
pub(crate) async fn build_pending_clients(
    mcp_state: &tokio::sync::Mutex<inner::McpState>,
    configs_to_start: Vec<acp::McpServer>,
    cwd: Option<&Path>,
    meta_config_map: &inner::McpMetaConfigMap,
    oauth_config_map: &McpOAuthConfigMap,
    ctx: &inner::McpSpawnCtx<'_>,
) -> Vec<Result<inner::McpClient, inner::McpError>> {
    let mut results = start_mcp_servers(
        configs_to_start,
        cwd,
        meta_config_map,
        oauth_config_map,
        ctx,
    )
    .await;
    // Re-resolve SDK (ACP) config.toml overrides for THIS init, matching HTTP/stdio, so a
    // mid-session config change applies on the next init (resolved outside the lock — it
    // reads config.toml — then handed to the pure, under-lock builder).
    let acp_overrides: HashMap<String, inner::McpClientTimeoutOverrides> = {
        let names = mcp_state.lock().await.pending_acp_server_names();
        names
            .iter()
            .filter_map(|name| resolve_overrides(name, cwd).map(|o| (name.clone(), o)))
            .collect()
    };
    // Brief lock, no `.await` held: the SDK clients are built synchronously (pure).
    let acp_clients = mcp_state
        .lock()
        .await
        .build_pending_acp_clients(&acp_overrides);
    results.extend(acp_clients.into_iter().map(Ok));
    results
}

pub(crate) async fn start_mcp_servers(
    mcp_servers: Vec<acp::McpServer>,
    cwd: Option<&Path>,
    meta_config_map: &inner::McpMetaConfigMap,
    oauth_config_map: &McpOAuthConfigMap,
    ctx: &inner::McpSpawnCtx<'_>,
) -> Vec<Result<inner::McpClient, inner::McpError>> {
    let overrides_map: HashMap<String, inner::McpClientTimeoutOverrides> = mcp_servers
        .iter()
        .filter_map(|s| {
            let name = inner::mcp_server_name(s);
            resolve_overrides(name, cwd).map(|o| (name.to_string(), o))
        })
        .collect();
    inner::start_mcp_servers(
        mcp_servers,
        &overrides_map,
        meta_config_map,
        oauth_config_map,
        ctx,
    )
    .await
}
