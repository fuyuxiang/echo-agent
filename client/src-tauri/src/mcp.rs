//! Connectors panel — drives EchoAgent's `echo.agent/mcp/*` extension methods.
//!
//! MCP server configs live in `~/.echo-agent/config.toml` as `[mcp_servers.<name>]`
//! tables (see `echo-agent-config-types/src/mcp.rs` for the full schema). EchoAgent
//! owns the canonical state. With an active session we use its ACP CRUD methods
//! for hot start/stop; without a session we persist through the Runtime's public
//! config helpers so connector setup remains fully usable offline. Health changes
//! arrive via `echo.agent/mcp/server_status` (forwarded as `agent://mcp-status`).

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use agent_client_protocol as acp;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};

use crate::commands::AppState;
use crate::ext::{call_ext, call_ext_value, raw_params};

const MAX_MCP_CONFIG_BYTES: usize = 2 * 1024 * 1024;
const MAX_MCP_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_MCP_SERVERS: usize = 128;
const MAX_SESSION_ID_CHARS: usize = 256;
const MAX_SETUP_FIELDS: usize = 128;
const MAX_SETUP_KEY_CHARS: usize = 256;
const MAX_SETUP_VALUE_BYTES: usize = 64 * 1024;
const MAX_SETUP_TOTAL_BYTES: usize = 1024 * 1024;
const MAX_TOOL_NAME_CHARS: usize = 256;
const MUTATION_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

static MCP_MUTATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static MIRROR_TRANSACTION: OnceLock<Mutex<()>> = OnceLock::new();

async fn acquire_mcp_mutation() -> Result<tokio::sync::MutexGuard<'static, ()>, String> {
    tokio::time::timeout(MUTATION_LOCK_TIMEOUT, MCP_MUTATION.lock())
        .await
        .map_err(|_| "MCP 配置正在被其他操作修改，请稍后重试".to_string())
}

fn validate_optional_session(state: &AppState, session_id: Option<&str>) -> Result<(), String> {
    if let Some(session_id) = session_id {
        validate_session_id(session_id)?;
        state.session_workspace(session_id)?;
    }
    Ok(())
}

/// One MCP server entry surfaced to the UI. Mirrors the fields of EchoAgent's
/// `McpServerEntry` (`echo-agent-runtime/src/inspect/mod.rs:227`) plus the live
/// status that arrives via `echo.agent/mcp/server_status` notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerEntry {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    /// Transport kind: "stdio" | "streamable_http" (EchoAgent also has "sse" as a
    /// sub-variant of streamable_http; we normalize to the transport name).
    #[serde(default)]
    pub transport: Option<String>,
    /// For stdio: the command. For http: the URL.
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    /// Where the config came from: "user" | "project" | "bundled" | ...
    #[serde(default)]
    pub source: Option<String>,
    /// Reason the server is disabled (if any) — surfaced by EchoAgent inspect.
    #[serde(default)]
    pub disabled_reason: Option<String>,
    /// Vendor/plugin that contributed this server, if any.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Live session health: ready | initializing | setuprequired | unavailable.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub auth_required: bool,
    #[serde(default)]
    pub setup_required: bool,
    #[serde(default)]
    pub setup: Option<echo_agent_runtime::util::config::McpSetupConfig>,
    #[serde(default)]
    pub setup_values: HashMap<String, String>,
    #[serde(default)]
    pub tools: Vec<McpToolEntry>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Only user-global config entries are deletable from this panel.
    #[serde(default)]
    pub editable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolEntry {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Exact (forward-compatible) subset of the embedded Runtime's list wire shape.
/// The transport is a flattened tagged enum, while health lives under `session`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpServerWire {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    source_label: Option<String>,
    #[serde(default)]
    setup: Option<echo_agent_runtime::util::config::McpSetupConfig>,
    #[serde(default)]
    setup_values: Option<HashMap<String, String>>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Vec<McpEnvWire>,
    #[serde(default)]
    session: Option<McpSessionWire>,
}

#[derive(Debug, Deserialize)]
struct McpEnvWire {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpSessionWire {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tools: Vec<McpToolEntry>,
    #[serde(default)]
    auth_required: bool,
    #[serde(default)]
    setup_required: bool,
}

/// The list endpoint returns either a bare array or `{ servers: [...] }`,
/// depending on the EchoAgent build. Accept both.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum McpListResponse {
    Array(Vec<McpServerWire>),
    Wrapped {
        #[serde(default)]
        servers: Vec<McpServerWire>,
    },
}

impl McpListResponse {
    fn into_servers(self) -> Vec<McpServerWire> {
        match self {
            McpListResponse::Array(v) => v,
            McpListResponse::Wrapped { servers } => servers,
        }
    }
}

/// Frontend payload for creating/updating an MCP server. We keep this loose
/// (transport-discriminated) so the UI can support both stdio and HTTP without
/// a round of protocol churn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUpsertRequest {
    pub name: String,
    /// "stdio" or "http". http covers both streamable_http and SSE.
    pub transport: String,
    /// stdio: the executable command. http: the URL.
    pub target: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub bearer_token_env_var: Option<String>,
    #[serde(default)]
    pub oauth_client_id: Option<String>,
    #[serde(default)]
    pub oauth_client_secret_env_var: Option<String>,
    #[serde(default)]
    pub oauth_scopes: Vec<String>,
    #[serde(default)]
    pub startup_timeout_sec: Option<u64>,
    #[serde(default)]
    pub tool_timeout_sec: Option<u64>,
    /// Full Runtime options retained by the raw editor. These matter for
    /// connector setup forms, nested OAuth declarations and large tool output.
    #[serde(default)]
    pub oauth: Option<echo_agent_runtime::util::config::McpJsonOAuthBlock>,
    #[serde(default)]
    pub setup: Option<echo_agent_runtime::util::config::McpSetupConfig>,
    #[serde(default)]
    pub tool_timeouts: HashMap<String, u64>,
    #[serde(default)]
    pub expose_image_base64: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpMutationResult {
    pub persisted: bool,
    pub applied_live: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// List configured MCP servers. `session_id` is optional — EchoAgent's
/// `McpListRequest` accepts it (camelCase `sessionId`) to enrich entries with
/// live session state; without it the agent-level catalog is returned.
#[tauri::command]
pub async fn mcp_list(
    state: State<'_, AppState>,
    session_id: Option<String>,
    refresh: Option<bool>,
) -> Result<Vec<McpServerEntry>, String> {
    validate_optional_session(&state, session_id.as_deref())?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let mut entries = mcp_list_with_tx_cache(&tx, session_id, !refresh.unwrap_or(false)).await?;
    // Office generation is a first-party task capability, not a connector
    // that users need to install, configure, or toggle in the MCP panel.
    entries.retain(|entry| !is_builtin_mcp(&entry.name));
    Ok(entries)
}

fn is_builtin_mcp(name: &str) -> bool {
    name == crate::office_mcp::MCP_SERVER_NAME || name == crate::live_info_mcp::MCP_SERVER_NAME
}

fn reject_builtin_mcp_mutation(name: &str) -> Result<(), String> {
    if is_builtin_mcp(name) {
        Err("内置能力由应用管理，无需单独配置".into())
    } else {
        Ok(())
    }
}

/// Internal form used by automations to ensure every selected connector is
/// configured and enabled before a background run is dispatched.
pub async fn mcp_list_with_tx(
    tx: &echo_agent_acp::AcpAgentTx,
    session_id: Option<String>,
) -> Result<Vec<McpServerEntry>, String> {
    mcp_list_with_tx_cache(tx, session_id, true).await
}

async fn mcp_list_with_tx_cache(
    tx: &echo_agent_acp::AcpAgentTx,
    session_id: Option<String>,
    cache: bool,
) -> Result<Vec<McpServerEntry>, String> {
    let params = raw_params(&serde_json::json!({
        "sessionId": session_id,
        "cache": cache,
    }));
    let v: McpListResponse = call_ext(tx, "echo.agent/mcp/list", params)
        .await
        .map_err(|e| e.to_string())?;
    Ok(v.into_servers().into_iter().map(map_wire_entry).collect())
}

fn map_wire_entry(wire: McpServerWire) -> McpServerEntry {
    let session = wire.session;
    let enabled = session.as_ref().map(|s| s.enabled).unwrap_or(true);
    let kind = wire.kind.as_deref().unwrap_or("unknown");
    let transport = match kind {
        "http" => "streamable_http",
        "managedGateway" => "managed_gateway",
        other => other,
    };
    let target = match transport {
        "stdio" => wire.command.clone(),
        "streamable_http" | "sse" => wire.url.clone(),
        _ => None,
    };
    let source = wire.source.unwrap_or_else(|| "local".into());
    let editable = source == "local" && user_config_has_server(&wire.name).unwrap_or(false);
    let env = wire
        .env
        .into_iter()
        .map(|item| (item.name, item.value))
        .collect();
    McpServerEntry {
        name: wire.name,
        display_name: wire.display_name,
        transport: Some(transport.into()),
        target,
        enabled,
        source: Some(source),
        disabled_reason: (!enabled).then_some("已在本地配置中停用".into()),
        vendor: wire.source_label,
        status: session.as_ref().and_then(|s| s.status.clone()),
        live: session.is_some(),
        auth_required: session.as_ref().is_some_and(|s| s.auth_required),
        setup_required: session.as_ref().is_some_and(|s| s.setup_required),
        setup: wire.setup,
        setup_values: wire.setup_values.unwrap_or_default(),
        tools: session.map(|s| s.tools).unwrap_or_default(),
        args: wire.args,
        env,
        editable,
    }
}

/// Add or update an MCP server. Translates the frontend payload into the
/// `[mcp_servers.<name>]` shape EchoAgent expects (see McpServerTransportConfig).
#[tauri::command]
pub async fn mcp_upsert(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: Option<String>,
    server: McpUpsertRequest,
) -> Result<McpMutationResult, String> {
    let _mutation = acquire_mcp_mutation().await?;
    reject_builtin_mcp_mutation(&server.name)?;
    validate_upsert_request(&server)?;
    validate_optional_session(&state, session_id.as_deref())?;
    if stdio_change_requires_confirmation(&server, &read_user_config_requests()?) {
        confirm_stdio_execution(&app, std::slice::from_ref(&server))?;
    }
    let tx = state.tx.lock().unwrap().clone();
    apply_upsert(tx.as_ref(), session_id.as_deref(), &server).await
}

/// Delete an MCP server by name.
#[tauri::command]
pub async fn mcp_delete(
    state: State<'_, AppState>,
    session_id: Option<String>,
    name: String,
) -> Result<McpMutationResult, String> {
    let _mutation = acquire_mcp_mutation().await?;
    reject_builtin_mcp_mutation(&name)?;
    validate_server_name(&name)?;
    validate_optional_session(&state, session_id.as_deref())?;
    let tx = state.tx.lock().unwrap().clone();
    apply_delete(tx.as_ref(), session_id.as_deref(), &name).await
}

/// Enable or disable an MCP server at runtime (no restart needed).
#[tauri::command]
pub async fn mcp_toggle(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: Option<String>,
    name: String,
    enabled: bool,
) -> Result<McpMutationResult, String> {
    let _mutation = acquire_mcp_mutation().await?;
    reject_builtin_mcp_mutation(&name)?;
    validate_server_name(&name)?;
    validate_optional_session(&state, session_id.as_deref())?;
    if enabled {
        let configured = read_user_config_requests()?;
        if let Some(server) = configured
            .iter()
            .find(|server| server.name == name && server.transport == "stdio")
        {
            confirm_stdio_execution(&app, std::slice::from_ref(server))?;
        }
    }
    let tx = state.tx.lock().unwrap().clone();
    apply_toggle(tx.as_ref(), session_id.as_deref(), &name, enabled).await
}

/// Resolve a connector's Runtime-provided setup schema and start it in the
/// current session. Setup is inherently session-scoped because resolution
/// includes project/plugin policy and performs a live connection probe.
#[tauri::command]
pub async fn mcp_setup(
    state: State<'_, AppState>,
    session_id: String,
    name: String,
    values: HashMap<String, String>,
) -> Result<(), String> {
    reject_builtin_mcp_mutation(&name)?;
    validate_server_name(&name)?;
    validate_session_id(&session_id)?;
    validate_setup_values(&values)?;
    state.session_workspace(&session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("本地 Runtime 尚未初始化")?;
    let params = raw_params(&serde_json::json!({
        "sessionId": session_id,
        "serverName": name,
        "values": values,
    }));
    let _: acp::ExtResponse = call_ext_value(&tx, "echo.agent/mcp/setup", params)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Enable/disable one advertised tool for the current session.
#[tauri::command]
pub async fn mcp_toggle_tool(
    state: State<'_, AppState>,
    session_id: String,
    server_name: String,
    tool_name: String,
    enabled: bool,
) -> Result<(), String> {
    reject_builtin_mcp_mutation(&server_name)?;
    validate_server_name(&server_name)?;
    validate_session_id(&session_id)?;
    validate_tool_name(&tool_name)?;
    state.session_workspace(&session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("本地 Runtime 尚未初始化")?;
    let params = raw_params(&serde_json::json!({
        "session_id": session_id,
        "server_name": server_name,
        "tool_name": tool_name,
        "enabled": enabled,
    }));
    let _: acp::ExtResponse = call_ext_value(&tx, "echo.agent/mcp/toggle_tool", params)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

async fn apply_upsert(
    tx: Option<&echo_agent_acp::AcpAgentTx>,
    session_id: Option<&str>,
    server: &McpUpsertRequest,
) -> Result<McpMutationResult, String> {
    validate_upsert_request(server)?;
    // A corrupt compatibility mirror must fail before any Runtime/canonical
    // mutation; otherwise reporting the mirror error would conceal a partial
    // update and a later operation could overwrite evidence of corruption.
    let _ = read_mirror_servers()?;
    if server.enabled == Some(false) {
        persist_server_config(server).await?;
        mirror_single_server(server, false)?;
        let mut result = McpMutationResult {
            persisted: true,
            applied_live: false,
            warnings: Vec::new(),
        };
        if let (Some(session_id), Some(tx)) = (session_id.filter(|value| !value.is_empty()), tx) {
            let params = raw_params(&serde_json::json!({
                "session_id": session_id,
                "server_name": server.name,
                "enabled": false,
            }));
            let live: Result<acp::ExtResponse, _> =
                call_ext_value(tx, "echo.agent/mcp/toggle", params).await;
            match live {
                Ok(_) => result.applied_live = true,
                Err(error) => result
                    .warnings
                    .push(format!("配置已保存，但当前会话热停用失败：{error}")),
            }
        } else if session_id.is_some() {
            result
                .warnings
                .push("配置已保存，但本地 Runtime 尚未初始化；将在新会话中生效".into());
        } else {
            result
                .warnings
                .push("已按停用状态保存，将对新会话生效".into());
        }
        return Ok(result);
    }
    if let (Some(session_id), Some(tx)) = (session_id.filter(|value| !value.is_empty()), tx) {
        let payload = build_upsert_payload(session_id, server)?;
        let params = raw_params(&payload);
        let live: Result<acp::ExtResponse, _> =
            call_ext_value(tx, "echo.agent/mcp/upsert", params).await;
        return match live {
            Ok(_) => {
                mirror_single_server(server, false)?;
                Ok(McpMutationResult {
                    persisted: true,
                    applied_live: true,
                    warnings: Vec::new(),
                })
            }
            Err(error) => {
                persist_server_config(server).await?;
                mirror_single_server(server, false)?;
                Ok(McpMutationResult {
                    persisted: true,
                    applied_live: false,
                    warnings: vec![format!("配置已保存，但当前会话热加载失败：{error}")],
                })
            }
        };
    } else if session_id.is_some() {
        persist_server_config(server).await?;
        mirror_single_server(server, false)?;
        return Ok(McpMutationResult {
            persisted: true,
            applied_live: false,
            warnings: vec!["配置已保存，但本地 Runtime 尚未初始化；将在新会话中生效".into()],
        });
    }
    persist_server_config(server).await?;
    mirror_single_server(server, false)?;
    Ok(McpMutationResult {
        persisted: true,
        applied_live: false,
        warnings: vec!["配置已保存，将在下次创建会话时启动".into()],
    })
}

async fn apply_delete(
    tx: Option<&echo_agent_acp::AcpAgentTx>,
    session_id: Option<&str>,
    name: &str,
) -> Result<McpMutationResult, String> {
    let _ = read_mirror_servers()?;
    let mut warnings = Vec::new();
    let mut applied_live = false;
    let was_user_config = user_config_has_server(name)?;
    if let (Some(session_id), Some(tx)) = (session_id.filter(|value| !value.is_empty()), tx) {
        let params = raw_params(&serde_json::json!({
            "session_id": session_id,
            "server_name": name,
        }));
        let live: Result<acp::ExtResponse, _> =
            call_ext_value(tx, "echo.agent/mcp/delete", params).await;
        match live {
            Ok(_) => applied_live = true,
            Err(error) => warnings.push(format!("当前会话热卸载失败：{error}")),
        }
    } else if session_id.is_some() {
        warnings.push("本地 Runtime 尚未初始化；已删除持久化配置".into());
    }
    // The Runtime delete persists before resolving the session. Calling this
    // again is idempotent and also covers the no-session/stale-session path.
    let path = runtime_config_path();
    crate::providers::update_config(|root| apply_mcp_document(root, &[], &[name.to_string()]))
        .map_err(|error| format!("删除 MCP 配置失败：{error}"))?;
    if path.exists() {
        crate::paths::harden_private_file(&path)?;
    }
    if !applied_live && !was_user_config {
        return Err(format!("MCP 服务「{name}」不是可删除的用户配置"));
    }
    mirror_delete_server(name)?;
    Ok(McpMutationResult {
        persisted: true,
        applied_live,
        warnings,
    })
}

async fn apply_toggle(
    tx: Option<&echo_agent_acp::AcpAgentTx>,
    session_id: Option<&str>,
    name: &str,
    enabled: bool,
) -> Result<McpMutationResult, String> {
    let _ = read_mirror_servers()?;
    let mut warnings = Vec::new();
    let mut applied_live = false;
    if let (Some(session_id), Some(tx)) = (session_id.filter(|value| !value.is_empty()), tx) {
        let params = raw_params(&serde_json::json!({
            "session_id": session_id,
            "server_name": name,
            "enabled": enabled,
        }));
        let live: Result<acp::ExtResponse, _> =
            call_ext_value(tx, "echo.agent/mcp/toggle", params).await;
        match live {
            Ok(_) => applied_live = true,
            Err(error) => warnings.push(format!("当前会话热切换失败：{error}")),
        }
    } else if session_id.is_some() {
        warnings.push("本地 Runtime 尚未初始化；状态将在新会话中生效".into());
    }
    crate::providers::update_config(|root| apply_mcp_toggle(root, name, enabled))
        .map_err(|error| format!("保存 MCP 启用状态失败：{error}"))?;
    mirror_toggle_server(name, enabled)?;
    if session_id.is_none() {
        warnings.push("状态已保存，将对新会话生效".into());
    }
    Ok(McpMutationResult {
        persisted: true,
        applied_live,
        warnings,
    })
}

/// Translate the frontend payload to the JSON EchoAgent's `echo.agent/mcp/upsert` expects.
///
/// EchoAgent's `McpUpsertRequest` (`echo-agent-runtime/src/extensions/mcp.rs`) is:
/// ```json
/// { "session_id": "...", "server_name": "...", <flattened McpServerConfig> }
/// ```
/// where `McpServerConfig` flattens a `McpServerTransportConfig`:
/// ```toml
/// [mcp_servers.filesystem]            # stdio
/// command = "npx"
/// args = ["-y", "@modelcontextprotocol/server-filesystem", "/path"]
///
/// [mcp_servers.linear]                # streamable_http
/// url = "https://mcp.linear.app/mcp"
/// ```
/// NOTE: the wire keys are snake_case (`session_id` / `server_name`) — EchoAgent's
/// request struct has no `rename_all`, unlike `mcp/list` which takes
/// camelCase `sessionId`.
fn build_upsert_payload(
    session_id: &str,
    server: &McpUpsertRequest,
) -> Result<serde_json::Value, String> {
    let mut payload = serde_json::Map::new();
    payload.insert("session_id".into(), session_id.into());
    payload.insert("server_name".into(), server.name.clone().into());
    match server.transport.as_str() {
        "stdio" => {
            payload.insert("command".into(), server.target.clone().into());
            if !server.args.is_empty() {
                payload.insert(
                    "args".into(),
                    server
                        .args
                        .iter()
                        .cloned()
                        .map(serde_json::Value::from)
                        .collect::<Vec<_>>()
                        .into(),
                );
            }
            if !server.env.is_empty() {
                payload.insert("env".into(), serde_json::to_value(&server.env).unwrap());
            }
            if let Some(cwd) = &server.cwd {
                payload.insert("cwd".into(), cwd.clone().into());
            }
        }
        "http" | "streamable_http" | "sse" => {
            payload.insert("url".into(), server.target.clone().into());
            if server.transport == "sse" {
                payload.insert("type".into(), "sse".into());
            }
            if !server.headers.is_empty() {
                payload.insert(
                    "headers".into(),
                    serde_json::to_value(&server.headers).unwrap(),
                );
            }
            if let Some(value) = &server.bearer_token_env_var {
                payload.insert("bearer_token_env_var".into(), value.clone().into());
            }
            if let Some(value) = &server.oauth_client_id {
                payload.insert("oauth_client_id".into(), value.clone().into());
            }
            if let Some(value) = &server.oauth_client_secret_env_var {
                payload.insert("oauth_client_secret_env_var".into(), value.clone().into());
            }
            if !server.oauth_scopes.is_empty() {
                payload.insert(
                    "oauth_scopes".into(),
                    serde_json::to_value(&server.oauth_scopes).unwrap(),
                );
            }
        }
        other => {
            return Err(format!(
                "unknown transport '{other}': expected 'stdio' or 'http'"
            ));
        }
    }
    if let Some(enabled) = server.enabled {
        payload.insert("enabled".into(), enabled.into());
    }
    if let Some(value) = server.startup_timeout_sec {
        payload.insert("startup_timeout_sec".into(), value.into());
    }
    if let Some(value) = server.tool_timeout_sec {
        payload.insert("tool_timeout_sec".into(), value.into());
    }
    if let Some(value) = &server.oauth {
        payload.insert(
            "oauth".into(),
            serde_json::to_value(value).map_err(|e| format!("序列化 OAuth 配置失败：{e}"))?,
        );
    }
    if let Some(value) = &server.setup {
        payload.insert(
            "setup".into(),
            serde_json::to_value(value).map_err(|e| format!("序列化 Setup 配置失败：{e}"))?,
        );
    }
    if !server.tool_timeouts.is_empty() {
        payload.insert(
            "tool_timeouts".into(),
            serde_json::to_value(&server.tool_timeouts)
                .map_err(|e| format!("序列化逐工具超时失败：{e}"))?,
        );
    }
    if let Some(value) = server.expose_image_base64 {
        payload.insert("expose_image_base64".into(), value.into());
    }
    Ok(serde_json::Value::Object(payload))
}

fn validate_server_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 80 {
        return Err("MCP 服务名称必须为 1–80 个字符".into());
    }
    if trimmed.starts_with("managed_gateway:")
        || trimmed.chars().any(char::is_control)
        || trimmed.chars().any(|ch| matches!(ch, '\n' | '\r'))
    {
        return Err("MCP 服务名称包含保留前缀或非法字符".into());
    }
    Ok(())
}

fn validate_session_id(session_id: &str) -> Result<(), String> {
    if session_id.trim().is_empty()
        || session_id.chars().count() > MAX_SESSION_ID_CHARS
        || session_id.chars().any(char::is_control)
    {
        return Err("完成 MCP 操作需要有效的活动会话".into());
    }
    Ok(())
}

fn validate_setup_values(values: &HashMap<String, String>) -> Result<(), String> {
    if values.len() > MAX_SETUP_FIELDS {
        return Err(format!("Setup 字段不能超过 {MAX_SETUP_FIELDS} 个"));
    }
    let mut total = 0_usize;
    for (key, value) in values {
        if key.trim().is_empty()
            || key.chars().count() > MAX_SETUP_KEY_CHARS
            || key.chars().any(char::is_control)
            || value.trim().is_empty()
            || value.len() > MAX_SETUP_VALUE_BYTES
            || value.contains('\0')
        {
            return Err("Setup 字段名或值无效、过长".into());
        }
        total = total
            .checked_add(key.len())
            .and_then(|length| length.checked_add(value.len()))
            .ok_or_else(|| "Setup 字段总长度溢出".to_string())?;
        if total > MAX_SETUP_TOTAL_BYTES {
            return Err("Setup 字段总大小不能超过 1 MiB".into());
        }
    }
    Ok(())
}

fn validate_tool_name(tool_name: &str) -> Result<(), String> {
    if tool_name.trim().is_empty()
        || tool_name.chars().count() > MAX_TOOL_NAME_CHARS
        || tool_name.chars().any(char::is_control)
    {
        return Err("工具名称无效或过长".into());
    }
    Ok(())
}

fn validate_upsert_request(server: &McpUpsertRequest) -> Result<(), String> {
    validate_server_name(&server.name)?;
    let encoded =
        serde_json::to_vec(server).map_err(|error| format!("MCP 配置无法序列化：{error}"))?;
    if encoded.len() > MAX_MCP_REQUEST_BYTES {
        return Err("MCP 单服务配置超过 1 MiB 安全上限".into());
    }
    if server.target.trim().is_empty()
        || server.target.len() > 4096
        || server.target.chars().any(char::is_control)
    {
        return Err("MCP 命令或 URL 不能为空".into());
    }
    if server.args.len() > 256
        || server
            .args
            .iter()
            .any(|arg| arg.len() > 8192 || arg.chars().any(char::is_control))
    {
        return Err("MCP 参数数量、长度或控制字符不合法".into());
    }
    if server.env.len() > 256
        || server.env.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > 256
                || key.contains('=')
                || key.chars().any(char::is_control)
                || value.len() > 64 * 1024
                || value.contains('\0')
        })
    {
        return Err("MCP 环境变量数量、名称或值不合法".into());
    }
    if server.cwd.as_ref().is_some_and(|cwd| {
        cwd.len() > 4096 || cwd.contains('\0') || cwd.chars().any(|ch| matches!(ch, '\r' | '\n'))
    }) || server.oauth_scopes.len() > 128
        || server.oauth_scopes.iter().any(|scope| {
            scope.is_empty() || scope.len() > 1024 || scope.chars().any(char::is_control)
        })
    {
        return Err("MCP 工作目录或 OAuth scope 不合法".into());
    }
    match server.transport.as_str() {
        "stdio" => {}
        "http" | "streamable_http" | "sse" => {
            let url =
                url::Url::parse(server.target.trim()).map_err(|e| format!("MCP URL 无效：{e}"))?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err("MCP URL 仅支持 http:// 或 https://".into());
            }
            if url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err("MCP URL 缺少主机或包含用户凭据/fragment".into());
            }
            if url.scheme() == "http"
                && !url.host_str().is_some_and(|host| {
                    matches!(host, "127.0.0.1" | "localhost")
                        || host.trim_matches(['[', ']']) == "::1"
                })
            {
                return Err("远程 MCP 服务必须使用 HTTPS；HTTP 仅允许本机回环地址".into());
            }
            let header_bytes = server
                .headers
                .iter()
                .map(|(name, value)| name.len().saturating_add(value.len()))
                .sum::<usize>();
            if server.headers.len() > 128
                || header_bytes > 256 * 1024
                || server.headers.iter().any(|(name, value)| {
                    reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err()
                        || reqwest::header::HeaderValue::from_str(value).is_err()
                        || value.len() > 64 * 1024
                })
            {
                return Err("MCP Header 数量、名称或值不合法".into());
            }
        }
        other => return Err(format!("不支持的 MCP 传输类型：{other}")),
    }
    for (label, value) in [
        ("启动超时", server.startup_timeout_sec),
        ("工具超时", server.tool_timeout_sec),
    ] {
        if value.is_some_and(|v| v == 0 || v > 3600) {
            return Err(format!("{label}必须在 1–3600 秒之间"));
        }
    }
    if server.tool_timeouts.len() > 256
        || server.tool_timeouts.iter().any(|(name, value)| {
            name.trim().is_empty()
                || name.chars().count() > MAX_TOOL_NAME_CHARS
                || name.chars().any(char::is_control)
                || *value == 0
                || *value > 3600
        })
    {
        return Err("逐工具超时的名称不能为空，时长必须在 1–3600 秒之间".into());
    }
    if server.env.keys().any(|key| key.trim().is_empty())
        || server.headers.keys().any(|key| key.trim().is_empty())
    {
        return Err("环境变量名和 Header 名不能为空".into());
    }
    Ok(())
}

fn is_enabled(server: &McpUpsertRequest) -> bool {
    server.enabled.unwrap_or(true)
}

fn same_stdio_execution(left: &McpUpsertRequest, right: &McpUpsertRequest) -> bool {
    left.transport == "stdio"
        && right.transport == "stdio"
        && left.target == right.target
        && left.args == right.args
        && left.env == right.env
        && left.cwd == right.cwd
        && is_enabled(left) == is_enabled(right)
}

fn stdio_change_requires_confirmation(
    incoming: &McpUpsertRequest,
    existing: &[McpUpsertRequest],
) -> bool {
    if incoming.transport != "stdio" || !is_enabled(incoming) {
        return false;
    }
    existing
        .iter()
        .find(|server| server.name == incoming.name)
        .is_none_or(|server| !same_stdio_execution(incoming, server))
}

fn stdio_confirmation_approved(result: MessageDialogResult) -> bool {
    matches!(result, MessageDialogResult::Custom(label) if label == "允许本次")
}

fn confirm_stdio_execution(app: &AppHandle, servers: &[McpUpsertRequest]) -> Result<(), String> {
    if servers.is_empty() {
        return Ok(());
    }
    let details = servers
        .iter()
        .take(5)
        .map(|server| {
            let mut command = std::iter::once(server.target.as_str())
                .chain(server.args.iter().take(12).map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            if command.chars().count() > 1200 {
                command = command.chars().take(1200).collect::<String>();
                command.push('…');
            } else if server.args.len() > 12 {
                command.push_str(" …");
            }
            let cwd = server.cwd.as_deref().unwrap_or("默认工作目录");
            let env_names = if server.env.is_empty() {
                "无".to_string()
            } else {
                server.env.keys().cloned().collect::<Vec<_>>().join(", ")
            };
            format!(
                "• {}\n  命令: {}\n  工作目录: {}\n  环境变量名: {}",
                server.name, command, cwd, env_names
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let extra = if servers.len() > 5 {
        format!("\n\n另有 {} 个本地进程配置。", servers.len() - 5)
    } else {
        String::new()
    };
    // An unparented macOS dialog uses CFUserNotification instead of an
    // application-owned sheet. Keep this execution decision attached to the
    // actual client window; a missing window must never imply permission.
    let window = app
        .get_webview_window("main")
        .ok_or("无法打开 MCP 启动确认：客户端主窗口不可用")?;
    tracing::info!(
        servers = servers.len(),
        "MCP stdio execution confirmation requested"
    );
    let result = app
        .dialog()
        .message(format!(
            "MCP stdio 会以当前用户权限启动本地进程。仅当你信任命令及其来源时允许。\n\n{details}{extra}"
        ))
        .parent(&window)
        .title("允许 MCP 启动本地进程？")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "允许本次".into(),
            "取消".into(),
        ))
        .blocking_show_with_result();
    let approved = stdio_confirmation_approved(result);
    tracing::info!(approved, "MCP stdio execution confirmation resolved");
    if approved {
        Ok(())
    } else {
        Err("MCP_STDIO_CONFIRMATION_REQUIRED: 用户未允许启动本地 MCP 进程".into())
    }
}

fn runtime_config_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("config.toml")
}

fn request_to_runtime_config(
    server: &McpUpsertRequest,
) -> echo_agent_runtime::util::config::McpServerConfig {
    use echo_agent_runtime::util::config::{McpServerConfig, McpServerTransportConfig};
    let transport = match server.transport.as_str() {
        "stdio" => McpServerTransportConfig::Stdio {
            command: server.target.clone(),
            args: server.args.clone(),
            env: (!server.env.is_empty()).then(|| server.env.clone()),
            cwd: server.cwd.clone(),
        },
        _ => McpServerTransportConfig::StreamableHttp {
            url: server.target.clone(),
            transport_type: (server.transport == "sse").then(|| "sse".into()),
            bearer_token_env_var: server.bearer_token_env_var.clone(),
            headers: (!server.headers.is_empty()).then(|| server.headers.clone()),
            oauth_client_id: server.oauth_client_id.clone(),
            oauth_client_secret_env_var: server.oauth_client_secret_env_var.clone(),
            oauth_scopes: (!server.oauth_scopes.is_empty()).then(|| server.oauth_scopes.clone()),
        },
    };
    McpServerConfig {
        transport,
        enabled: server.enabled.unwrap_or(true),
        oauth: server.oauth.clone(),
        setup: server.setup.clone(),
        startup_timeout_sec: server.startup_timeout_sec,
        tool_timeout_sec: server.tool_timeout_sec,
        tool_timeouts: (!server.tool_timeouts.is_empty()).then(|| server.tool_timeouts.clone()),
        expose_image_base64: server.expose_image_base64,
    }
}

async fn persist_server_config(server: &McpUpsertRequest) -> Result<(), String> {
    let path = runtime_config_path();
    crate::providers::update_config(|root| {
        apply_mcp_document(root, std::slice::from_ref(server), &[])
    })
    .map_err(|error| format!("保存 MCP 配置失败：{error}"))?;
    if path.exists() {
        crate::paths::harden_private_file(&path)?;
    }
    Ok(())
}

fn read_bounded_text(path: &Path, max_bytes: usize, label: &str) -> Result<Option<String>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("读取 {label} 信息失败：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label} 必须是普通文件，不能是符号链接"));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(format!(
            "{label} 超过 {} MiB 安全上限",
            max_bytes / 1024 / 1024
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("打开 {label} 失败：{error}"))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("读取已打开 {label} 信息失败：{error}"))?;
    if !opened_metadata.is_file() || opened_metadata.len() > max_bytes as u64 {
        return Err(format!(
            "{label} 超过 {} MiB 安全上限",
            max_bytes / 1024 / 1024
        ));
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取 {label} 失败：{error}"))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "{label} 超过 {} MiB 安全上限",
            max_bytes / 1024 / 1024
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| format!("{label} 不是有效 UTF-8"))
}

fn user_config_has_server(name: &str) -> Result<bool, String> {
    let Some(content) =
        read_bounded_text(&runtime_config_path(), MAX_MCP_CONFIG_BYTES, "config.toml")?
    else {
        return Ok(false);
    };
    let root = toml::from_str::<toml::Value>(&content)
        .map_err(|error| format!("config.toml 格式无效，无法读取 MCP 配置：{error}"))?;
    let Some(value) = root.get("mcp_servers") else {
        return Ok(false);
    };
    let servers = value
        .as_table()
        .ok_or("config.toml 的 mcp_servers 必须是 TOML 表")?;
    if servers.len() > MAX_MCP_SERVERS {
        return Err(format!("Runtime MCP 服务数量超过 {MAX_MCP_SERVERS} 个"));
    }
    Ok(servers.contains_key(name))
}

fn read_user_config_requests() -> Result<Vec<McpUpsertRequest>, String> {
    use echo_agent_runtime::util::config::{McpServerConfig, McpServerTransportConfig};
    let content =
        match read_bounded_text(&runtime_config_path(), MAX_MCP_CONFIG_BYTES, "config.toml")? {
            Some(content) => content,
            None => return Ok(Vec::new()),
        };
    let root: toml::Value = toml::from_str(&content)
        .map_err(|error| format!("config.toml 格式无效，无法读取 MCP 配置：{error}"))?;
    let disabled: HashSet<String> = match root.get("disabled_mcp_servers") {
        None => HashSet::new(),
        Some(value) => {
            let values = value
                .as_array()
                .ok_or("config.toml 的 disabled_mcp_servers 必须是字符串数组")?;
            if values.len() > MAX_MCP_SERVERS {
                return Err(format!(
                    "Runtime 已停用 MCP 服务数量超过 {MAX_MCP_SERVERS} 个"
                ));
            }
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .ok_or("config.toml 的 disabled_mcp_servers 必须是字符串数组")
                        .map(str::to_string)
                })
                .collect::<Result<HashSet<_>, _>>()?
        }
    };
    let Some(value) = root.get("mcp_servers") else {
        return Ok(Vec::new());
    };
    let servers = value
        .as_table()
        .ok_or("config.toml 的 mcp_servers 必须是 TOML 表")?;
    if servers.len() > MAX_MCP_SERVERS {
        return Err(format!("Runtime MCP 服务数量超过 {MAX_MCP_SERVERS} 个"));
    }
    let mut out = Vec::new();
    for (name, value) in servers {
        let config: McpServerConfig = value
            .clone()
            .try_into()
            .map_err(|error| format!("MCP 服务「{name}」配置无效：{error}"))?;
        let enabled = config.enabled;
        let startup_timeout_sec = config.startup_timeout_sec;
        let tool_timeout_sec = config.tool_timeout_sec;
        let oauth = config.oauth;
        let setup = config.setup;
        let tool_timeouts = config.tool_timeouts.unwrap_or_default();
        let expose_image_base64 = config.expose_image_base64;
        let (
            transport,
            target,
            args,
            env,
            headers,
            cwd,
            bearer_token_env_var,
            oauth_client_id,
            oauth_client_secret_env_var,
            oauth_scopes,
        ) = match config.transport {
            McpServerTransportConfig::Stdio {
                command,
                args,
                env,
                cwd,
            } => (
                "stdio".into(),
                command,
                args,
                env.unwrap_or_default(),
                HashMap::new(),
                cwd,
                None,
                None,
                None,
                Vec::new(),
            ),
            McpServerTransportConfig::StreamableHttp {
                url,
                transport_type,
                bearer_token_env_var,
                headers,
                oauth_client_id,
                oauth_client_secret_env_var,
                oauth_scopes,
            } => (
                if transport_type.as_deref() == Some("sse") {
                    "sse".into()
                } else {
                    "streamable_http".into()
                },
                url,
                Vec::new(),
                HashMap::new(),
                headers.unwrap_or_default(),
                None,
                bearer_token_env_var,
                oauth_client_id,
                oauth_client_secret_env_var,
                oauth_scopes.unwrap_or_default(),
            ),
        };
        let request = McpUpsertRequest {
            name: name.clone(),
            transport,
            target,
            args,
            env,
            headers,
            enabled: Some(enabled && !disabled.contains(name)),
            cwd,
            bearer_token_env_var,
            oauth_client_id,
            oauth_client_secret_env_var,
            oauth_scopes,
            startup_timeout_sec,
            tool_timeout_sec,
            oauth,
            setup,
            tool_timeouts,
            expose_image_base64,
        };
        validate_upsert_request(&request)
            .map_err(|error| format!("MCP 服务「{name}」配置无效：{error}"))?;
        out.push(request);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

// ---------- standalone mcp.json editor (截图 6 / 7) ----------
//
// The modal edits standard `{ "mcpServers": { ... } }` JSON. Runtime
// `config.toml` remains canonical; `mcp.json` is a private compatibility mirror
// for import/export and token-form prefills. Reads merge canonical entries and
// saves validate the whole document before synchronizing both representations.

/// Default content shown when the file does not exist yet.
const EMPTY_MCP_JSON: &str = "{\n  \"mcpServers\": {}\n}";

/// Raw `mcp.json` payload returned to the editor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfigFile {
    pub file_path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfigSaveResult {
    pub server_count: usize,
    pub removed_count: usize,
    pub applied_live: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Absolute path of the standalone MCP config file: `~/.echo-agent/mcp.json`.
fn mcp_json_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("mcp.json")
}

/// Return the resolved config-file path (shown in the editor header).
#[tauri::command]
pub async fn mcp_config_path() -> Result<String, String> {
    Ok(mcp_json_path().to_string_lossy().into_owned())
}

/// Read the config file. Missing file yields the empty template (not an error)
/// so the editor always opens with valid JSON.
#[tauri::command]
pub async fn mcp_config_read() -> Result<McpConfigFile, String> {
    let path = mcp_json_path();
    let mut servers = read_mirror_servers()?;
    // config.toml is canonical. Merge its valid user entries into the editor,
    // while preserving mirror-only fields (for example token-form prefill env
    // on an HTTP connector) and legacy entries awaiting first save/import.
    for request in read_user_config_requests()? {
        let mut generated = request_to_mirror_json(&request);
        if let (Some(old), Some(next)) = (servers.get(&request.name), generated.as_object_mut()) {
            if let Some(env) = old.get("env") {
                next.entry("env").or_insert_with(|| env.clone());
            }
        }
        servers.insert(request.name, generated);
    }
    if servers.len() > MAX_MCP_SERVERS {
        return Err(format!("合并后的 MCP 服务数量超过 {MAX_MCP_SERVERS} 个"));
    }
    let content = serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": servers }))
        .map_err(|e| format!("生成 mcp.json 编辑内容失败：{e}"))?;
    if content.len() > MAX_MCP_CONFIG_BYTES {
        return Err("MCP 配置编辑内容超过 2 MiB 安全上限".into());
    }
    Ok(McpConfigFile {
        file_path: path.to_string_lossy().into_owned(),
        content,
    })
}

/// Validate the complete document, persist every entry to canonical Runtime
/// config and update the private JSON mirror. With an active session, changes
/// are also hot-applied; without one they take effect on the next session.
#[tauri::command]
pub async fn mcp_config_save(
    app: AppHandle,
    state: State<'_, AppState>,
    content: String,
    session_id: Option<String>,
) -> Result<McpConfigSaveResult, String> {
    let _mutation = acquire_mcp_mutation().await?;
    validate_optional_session(&state, session_id.as_deref())?;
    if content.len() > MAX_MCP_CONFIG_BYTES {
        return Err("MCP 配置超过 2MB 限制".into());
    }
    let trimmed = content.trim();
    let parsed: serde_json::Value = if trimmed.is_empty() {
        serde_json::from_str(EMPTY_MCP_JSON).unwrap()
    } else {
        serde_json::from_str(trimmed).map_err(|e| format!("无效的 JSON：{e}"))?
    };
    if !parsed.is_object() {
        return Err("配置文件顶层必须是 JSON 对象".into());
    }
    let map = parsed
        .get("mcpServers")
        .map(|servers| {
            servers
                .as_object()
                .ok_or_else(|| "\"mcpServers\" 必须是 JSON 对象".to_string())
        })
        .transpose()?
        .cloned()
        .unwrap_or_default();
    if map.len() > MAX_MCP_SERVERS {
        return Err(format!("MCP 服务数量超过 {MAX_MCP_SERVERS} 个的上限"));
    }

    // Validate the complete document before any mutation. One malformed entry
    // must never leave a half-applied config.
    let mut requests = Vec::with_capacity(map.len());
    for (name, config) in &map {
        let request =
            json_to_upsert(name, config).map_err(|e| format!("MCP 服务「{name}」配置无效：{e}"))?;
        validate_upsert_request(&request)
            .map_err(|e| format!("MCP 服务「{name}」配置无效：{e}"))?;
        requests.push(request);
    }

    let existing_requests = read_user_config_requests()?;
    let changed_stdio = requests
        .iter()
        .filter(|request| stdio_change_requires_confirmation(request, &existing_requests))
        .collect::<Vec<_>>();
    if !changed_stdio.is_empty() {
        let changed_stdio = changed_stdio.into_iter().cloned().collect::<Vec<_>>();
        confirm_stdio_execution(&app, &changed_stdio)?;
    }

    let previous = read_mirror_servers()?;
    let mut previous_names: HashSet<String> = previous.keys().cloned().collect();
    previous_names.extend(existing_requests.into_iter().map(|request| request.name));
    let incoming_names: HashSet<String> = map.keys().cloned().collect();
    let removed: Vec<String> = previous_names
        .iter()
        .filter(|name| !incoming_names.contains(*name))
        .cloned()
        .collect();
    let tx = state.tx.lock().unwrap().clone();
    let sid = session_id.filter(|value| !value.is_empty());
    let mut warnings = Vec::new();
    let mut applied_live = sid.is_some() && tx.is_some();

    // Use the Runtime extension for hot delete/upsert when a session exists.
    // Every operation is persisted again below, so stale sessions degrade to
    // "saved for next session" rather than losing the user's configuration.
    if let (Some(session_id), Some(tx)) = (sid.as_deref(), tx.as_ref()) {
        for name in &removed {
            let params = raw_params(&serde_json::json!({
                "session_id": session_id,
                "server_name": name,
            }));
            let result: Result<acp::ExtResponse, _> =
                call_ext_value(tx, "echo.agent/mcp/delete", params).await;
            if let Err(error) = result {
                applied_live = false;
                warnings.push(format!("服务「{name}」热卸载失败：{error}"));
            }
        }
        for request in &requests {
            if request.enabled == Some(false) {
                let params = raw_params(&serde_json::json!({
                    "session_id": session_id,
                    "server_name": request.name,
                    "enabled": false,
                }));
                let result: Result<acp::ExtResponse, _> =
                    call_ext_value(tx, "echo.agent/mcp/toggle", params).await;
                if let Err(error) = result {
                    applied_live = false;
                    warnings.push(format!("服务「{}」热停用失败：{error}", request.name));
                }
                continue;
            }
            let payload = build_upsert_payload(session_id, request)?;
            let params = raw_params(&payload);
            let result: Result<acp::ExtResponse, _> =
                call_ext_value(tx, "echo.agent/mcp/upsert", params).await;
            if let Err(error) = result {
                applied_live = false;
                warnings.push(format!("服务「{}」热加载失败：{error}", request.name));
            }
        }
    } else if sid.is_some() {
        warnings.push("本地 Runtime 尚未初始化；配置将在新会话中生效".into());
    }

    // Persist the complete MCP document in one shared config transaction. A
    // per-server loop can expose intermediate states and, more importantly,
    // lets an unrelated Runtime settings write land between snapshots.
    persist_mcp_document(&requests, &removed)?;
    for name in &removed {
        echo_agent_runtime::util::config::remove_mcp_server_credentials(name);
    }

    let normalized = if trimmed.is_empty() {
        EMPTY_MCP_JSON.as_bytes()
    } else {
        content.as_bytes()
    };
    if let Err(error) = write_mirror_document(normalized) {
        // config.toml is canonical and has already committed successfully. Do
        // not restore a stale whole-file snapshot here: that could erase an
        // unrelated concurrent provider/settings update. The editor rebuilds
        // its view from canonical config on the next read.
        warnings.push(format!(
            "Runtime 配置已保存，但 mcp.json 兼容镜像更新失败：{error}"
        ));
    }
    if sid.is_none() && (!requests.is_empty() || !removed.is_empty()) {
        warnings.push("配置已保存，将在下次创建会话时启动".into());
    }
    Ok(McpConfigSaveResult {
        server_count: requests.len(),
        removed_count: removed.len(),
        applied_live,
        warnings,
    })
}

fn persist_mcp_document(requests: &[McpUpsertRequest], removed: &[String]) -> Result<(), String> {
    crate::providers::update_config(|root| apply_mcp_document(root, requests, removed))?;
    let path = runtime_config_path();
    if path.exists() {
        crate::paths::harden_private_file(&path)?;
    }
    Ok(())
}

fn apply_mcp_document(
    root: &mut toml::Value,
    requests: &[McpUpsertRequest],
    removed: &[String],
) -> Result<(), String> {
    let table = root
        .as_table_mut()
        .ok_or_else(|| "config.toml 顶层必须是 TOML 表".to_string())?;

    {
        let servers = table
            .entry("mcp_servers")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| "[mcp_servers] 必须是 TOML 表".to_string())?;
        for request in requests {
            let serialized = toml::Value::try_from(request_to_runtime_config(request))
                .map_err(|error| format!("MCP 服务「{}」序列化失败：{error}", request.name))?;
            servers.insert(request.name.clone(), serialized);
        }
        for name in removed {
            servers.remove(name);
        }
    }
    if table
        .get("mcp_servers")
        .and_then(toml::Value::as_table)
        .is_some_and(toml::map::Map::is_empty)
    {
        table.remove("mcp_servers");
    }

    let affected_names = requests
        .iter()
        .map(|request| request.name.as_str())
        .chain(removed.iter().map(String::as_str))
        .collect::<HashSet<_>>();
    let mut disabled = table
        .get("disabled_mcp_servers")
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .filter(|name| !affected_names.contains(*name))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    disabled.extend(
        requests
            .iter()
            .filter(|request| request.enabled == Some(false))
            .map(|request| request.name.clone()),
    );
    if disabled.is_empty() {
        table.remove("disabled_mcp_servers");
    } else {
        table.insert(
            "disabled_mcp_servers".into(),
            toml::Value::Array(disabled.into_iter().map(toml::Value::String).collect()),
        );
    }

    if let Some(disabled_tools) = table
        .get_mut("disabled_mcp_tools")
        .and_then(toml::Value::as_table_mut)
    {
        for name in removed {
            disabled_tools.remove(name);
        }
        if disabled_tools.is_empty() {
            table.remove("disabled_mcp_tools");
        }
    }
    Ok(())
}

/// Remove a reserved, application-owned MCP bridge from the durable Runtime
/// configuration without touching the user's standalone `mcp.json` mirror.
/// Internal bridges are session capabilities, not user connectors; this also
/// cleans registrations written by older desktop releases at startup.
pub(crate) fn remove_internal_server_registration(name: &str) -> Result<(), String> {
    validate_server_name(name)?;
    crate::providers::update_config(|root| apply_mcp_document(root, &[], &[name.to_string()]))
}

fn apply_mcp_toggle(root: &mut toml::Value, name: &str, enabled: bool) -> Result<(), String> {
    let table = root
        .as_table_mut()
        .ok_or_else(|| "config.toml 顶层必须是 TOML 表".to_string())?;
    if let Some(server) = table
        .get_mut("mcp_servers")
        .and_then(toml::Value::as_table_mut)
        .and_then(|servers| servers.get_mut(name))
        .and_then(toml::Value::as_table_mut)
    {
        server.insert("enabled".into(), toml::Value::Boolean(enabled));
    }
    let mut disabled = match table.get("disabled_mcp_servers") {
        None => Vec::new(),
        Some(value) => {
            let values = value
                .as_array()
                .ok_or("disabled_mcp_servers 必须是字符串数组")?;
            if values.len() > MAX_MCP_SERVERS {
                return Err(format!("已停用 MCP 服务数量超过 {MAX_MCP_SERVERS} 个"));
            }
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .ok_or("disabled_mcp_servers 必须是字符串数组")
                        .map(str::to_string)
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|value| value != name)
                .collect()
        }
    };
    if !enabled {
        if disabled.len() >= MAX_MCP_SERVERS {
            return Err(format!("已停用 MCP 服务数量超过 {MAX_MCP_SERVERS} 个"));
        }
        disabled.push(name.to_string());
    }
    if disabled.is_empty() {
        table.remove("disabled_mcp_servers");
    } else {
        table.insert(
            "disabled_mcp_servers".into(),
            toml::Value::Array(disabled.into_iter().map(toml::Value::String).collect()),
        );
    }
    Ok(())
}

/// Map one standard `mcpServers.<name>` value (the shape editors like EchoAgent
/// / Claude Desktop use) onto our `McpUpsertRequest` so we can reuse the EchoAgent
/// upsert path. Recognizes stdio (`command`) and http/sse (`url`) entries.
fn json_to_upsert(name: &str, cfg: &serde_json::Value) -> Result<McpUpsertRequest, String> {
    let obj = cfg
        .as_object()
        .ok_or_else(|| "server 配置必须是对象".to_string())?;
    validate_mcp_json_keys(obj)?;
    let get_str = |key: &str| optional_json_string(obj, key);
    let enabled = match obj.get("enabled") {
        Some(value) => Some(
            value
                .as_bool()
                .ok_or_else(|| "'enabled' 必须是布尔值".to_string())?,
        ),
        None => None,
    };
    let url = get_str("url")?
        .or(get_str("urlTemplate")?)
        .or(get_str("url_template")?);
    let command = get_str("command")?;
    if url.is_some() && command.is_some() {
        return Err("'command' 和 'url' 不能同时存在".into());
    }
    if let Some(url) = url {
        let transport = match get_str("type")?.as_deref() {
            Some("sse") => "sse".to_string(),
            Some("streamable-http") | Some("streamable_http") | Some("streamableHttp") => {
                "streamable_http".to_string()
            }
            Some("http") | None => "http".to_string(),
            Some(other) => return Err(format!("不支持的 HTTP MCP type：{other}")),
        };
        // EchoAgent connector manifests use `staticHeaders` (e.g. kdocs);
        // standard mcp.json uses `headers`. Merge both, `headers` winning.
        let mut headers = json_string_map(obj, "staticHeaders")?;
        headers.extend(json_string_map(obj, "headers")?);
        Ok(McpUpsertRequest {
            name: name.to_string(),
            transport,
            target: url,
            args: Vec::new(),
            env: HashMap::new(),
            headers,
            enabled,
            cwd: None,
            bearer_token_env_var: get_str("bearerTokenEnvVar")?
                .or(get_str("bearer_token_env_var")?),
            oauth_client_id: get_str("oauthClientId")?.or(get_str("oauth_client_id")?),
            oauth_client_secret_env_var: get_str("oauthClientSecretEnvVar")?
                .or(get_str("oauth_client_secret_env_var")?),
            oauth_scopes: json_string_array_alias(obj, "oauthScopes", "oauth_scopes")?,
            startup_timeout_sec: json_u64_alias(obj, "startupTimeoutSec", "startup_timeout_sec")?,
            tool_timeout_sec: json_u64_alias(obj, "toolTimeoutSec", "tool_timeout_sec")?,
            oauth: json_typed(obj, "oauth")?,
            setup: json_typed(obj, "setup")?,
            tool_timeouts: json_u64_map_alias(obj, "toolTimeouts", "tool_timeouts")?,
            expose_image_base64: json_bool_alias(obj, "exposeImageBase64", "expose_image_base64")?,
        })
    } else if let Some(command) = command {
        let args = json_string_array(obj, "args")?;
        let env = json_string_map(obj, "env")?;
        Ok(McpUpsertRequest {
            name: name.to_string(),
            transport: "stdio".to_string(),
            target: command,
            args,
            env,
            headers: HashMap::new(),
            enabled,
            cwd: get_str("cwd")?,
            bearer_token_env_var: None,
            oauth_client_id: None,
            oauth_client_secret_env_var: None,
            oauth_scopes: Vec::new(),
            startup_timeout_sec: json_u64_alias(obj, "startupTimeoutSec", "startup_timeout_sec")?,
            tool_timeout_sec: json_u64_alias(obj, "toolTimeoutSec", "tool_timeout_sec")?,
            oauth: json_typed(obj, "oauth")?,
            setup: json_typed(obj, "setup")?,
            tool_timeouts: json_u64_map_alias(obj, "toolTimeouts", "tool_timeouts")?,
            expose_image_base64: json_bool_alias(obj, "exposeImageBase64", "expose_image_base64")?,
        })
    } else {
        Err("缺少 'command' 或 'url' 字段".to_string())
    }
}

fn validate_mcp_json_keys(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    const SUPPORTED: &[&str] = &[
        "args",
        "bearerTokenEnvVar",
        "bearer_token_env_var",
        "command",
        "cwd",
        "enabled",
        "env",
        "exposeImageBase64",
        "expose_image_base64",
        "headers",
        "oauth",
        "oauthClientId",
        "oauthClientSecretEnvVar",
        "oauthScopes",
        "oauth_client_id",
        "oauth_client_secret_env_var",
        "oauth_scopes",
        "setup",
        "staticHeaders",
        "startupTimeoutSec",
        "startup_timeout_sec",
        "toolTimeoutSec",
        "toolTimeouts",
        "tool_timeout_sec",
        "tool_timeouts",
        "type",
        "url",
        "urlTemplate",
        "url_template",
    ];
    let unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !SUPPORTED.contains(key))
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "包含不支持的字段：{}（请检查是否拼写错误）",
            unknown.join(", ")
        ))
    }
}

fn optional_json_string(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| format!("'{key}' 必须是字符串")),
        None => Ok(None),
    }
}

fn json_string_map(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<HashMap<String, String>, String> {
    let Some(value) = object.get(key) else {
        return Ok(HashMap::new());
    };
    let map = value
        .as_object()
        .ok_or_else(|| format!("'{key}' 必须是对象"))?;
    map.iter()
        .map(|(name, value)| {
            value
                .as_str()
                .map(|value| (name.clone(), value.to_string()))
                .ok_or_else(|| format!("'{key}.{name}' 必须是字符串"))
        })
        .collect()
}

fn json_string_array(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Vec<String>, String> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("'{key}' 必须是字符串数组"))?;
    array
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("'{key}[{index}]' 必须是字符串"))
        })
        .collect()
}

fn json_string_array_alias(
    object: &serde_json::Map<String, serde_json::Value>,
    camel: &str,
    snake: &str,
) -> Result<Vec<String>, String> {
    if object.contains_key(camel) {
        json_string_array(object, camel)
    } else {
        json_string_array(object, snake)
    }
}

fn json_u64_alias(
    object: &serde_json::Map<String, serde_json::Value>,
    camel: &str,
    snake: &str,
) -> Result<Option<u64>, String> {
    let (key, value) = if let Some(value) = object.get(camel) {
        (camel, value)
    } else if let Some(value) = object.get(snake) {
        (snake, value)
    } else {
        return Ok(None);
    };
    value
        .as_u64()
        .map(Some)
        .ok_or_else(|| format!("'{key}' 必须是正整数"))
}

fn json_typed<T: serde::de::DeserializeOwned>(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<T>, String> {
    object
        .get(key)
        .cloned()
        .map(|value| serde_json::from_value(value).map_err(|e| format!("'{key}' 配置无效：{e}")))
        .transpose()
}

fn json_u64_map_alias(
    object: &serde_json::Map<String, serde_json::Value>,
    camel: &str,
    snake: &str,
) -> Result<HashMap<String, u64>, String> {
    let Some((key, value)) = object
        .get(camel)
        .map(|value| (camel, value))
        .or_else(|| object.get(snake).map(|value| (snake, value)))
    else {
        return Ok(HashMap::new());
    };
    serde_json::from_value(value.clone()).map_err(|e| format!("'{key}' 必须是整数映射：{e}"))
}

fn json_bool_alias(
    object: &serde_json::Map<String, serde_json::Value>,
    camel: &str,
    snake: &str,
) -> Result<Option<bool>, String> {
    let Some((key, value)) = object
        .get(camel)
        .map(|value| (camel, value))
        .or_else(|| object.get(snake).map(|value| (snake, value)))
    else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| format!("'{key}' 必须是布尔值"))
}

fn request_to_mirror_json(server: &McpUpsertRequest) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    if server.transport == "stdio" {
        object.insert("command".into(), server.target.clone().into());
        if !server.args.is_empty() {
            object.insert("args".into(), serde_json::json!(server.args));
        }
        if !server.env.is_empty() {
            object.insert("env".into(), serde_json::json!(server.env));
        }
        if let Some(cwd) = &server.cwd {
            object.insert("cwd".into(), cwd.clone().into());
        }
    } else {
        object.insert("url".into(), server.target.clone().into());
        if server.transport == "sse" {
            object.insert("type".into(), "sse".into());
        } else {
            object.insert("type".into(), "streamable-http".into());
        }
        if !server.headers.is_empty() {
            object.insert("headers".into(), serde_json::json!(server.headers));
        }
        if let Some(value) = &server.bearer_token_env_var {
            object.insert("bearerTokenEnvVar".into(), value.clone().into());
        }
        if let Some(value) = &server.oauth_client_id {
            object.insert("oauthClientId".into(), value.clone().into());
        }
        if let Some(value) = &server.oauth_client_secret_env_var {
            object.insert("oauthClientSecretEnvVar".into(), value.clone().into());
        }
        if !server.oauth_scopes.is_empty() {
            object.insert("oauthScopes".into(), serde_json::json!(server.oauth_scopes));
        }
    }
    if let Some(enabled) = server.enabled {
        object.insert("enabled".into(), enabled.into());
    }
    if let Some(value) = server.startup_timeout_sec {
        object.insert("startupTimeoutSec".into(), value.into());
    }
    if let Some(value) = server.tool_timeout_sec {
        object.insert("toolTimeoutSec".into(), value.into());
    }
    if let Some(value) = &server.oauth {
        object.insert("oauth".into(), serde_json::json!(value));
    }
    if let Some(value) = &server.setup {
        object.insert("setup".into(), serde_json::json!(value));
    }
    if !server.tool_timeouts.is_empty() {
        object.insert(
            "toolTimeouts".into(),
            serde_json::json!(server.tool_timeouts),
        );
    }
    if let Some(value) = server.expose_image_base64 {
        object.insert("exposeImageBase64".into(), value.into());
    }
    serde_json::Value::Object(object)
}

fn with_mirror_transaction<T>(operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _process = MIRROR_TRANSACTION
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "mcp.json 事务锁已损坏".to_string())?;
    let path = mcp_json_path();
    let parent = path
        .parent()
        .ok_or_else(|| "mcp.json 路径没有父目录".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| format!("创建 MCP 配置目录失败：{error}"))?;
    crate::paths::harden_private_dir(parent)?;
    let lock_path = parent.join(".mcp-json.lock");
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock_file = options
        .open(&lock_path)
        .map_err(|error| format!("打开 mcp.json 事务锁失败：{error}"))?;
    crate::paths::harden_private_file(&lock_path)?;
    let mut acquired = false;
    for _ in 0..200 {
        match lock_file.try_lock_exclusive() {
            Ok(()) => {
                acquired = true;
                break;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(format!("锁定 mcp.json 失败：{error}")),
        }
    }
    if !acquired {
        return Err("mcp.json 正在被其他进程修改，请稍后重试".into());
    }
    let result = operation();
    let unlock =
        FileExt::unlock(&lock_file).map_err(|error| format!("解锁 mcp.json 失败：{error}"));
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn read_mirror_servers_at(
    path: &Path,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let Some(content) = read_bounded_text(path, MAX_MCP_CONFIG_BYTES, "mcp.json")? else {
        return Ok(serde_json::Map::new());
    };
    let root: serde_json::Value =
        serde_json::from_str(&content).map_err(|error| format!("mcp.json 格式无效：{error}"))?;
    let servers = root
        .get("mcpServers")
        .ok_or("mcp.json 缺少 mcpServers 字段")?
        .as_object()
        .ok_or("mcp.json 的 mcpServers 必须是对象")?
        .clone();
    if servers.len() > MAX_MCP_SERVERS {
        return Err(format!("mcp.json 服务数量超过 {MAX_MCP_SERVERS} 个"));
    }
    Ok(servers)
}

fn read_mirror_servers_unlocked() -> Result<serde_json::Map<String, serde_json::Value>, String> {
    read_mirror_servers_at(&mcp_json_path())
}

fn read_mirror_servers() -> Result<serde_json::Map<String, serde_json::Value>, String> {
    with_mirror_transaction(read_mirror_servers_unlocked)
}

fn write_mirror_servers_unlocked(
    servers: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    write_mirror_servers_at(&mcp_json_path(), servers)
}

fn write_mirror_servers_at(
    path: &Path,
    servers: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if servers.len() > MAX_MCP_SERVERS {
        return Err(format!("mcp.json 服务数量超过 {MAX_MCP_SERVERS} 个"));
    }
    let content = serde_json::to_vec_pretty(&serde_json::json!({ "mcpServers": servers }))
        .map_err(|e| format!("序列化 mcp.json 失败：{e}"))?;
    if content.len() > MAX_MCP_CONFIG_BYTES {
        return Err("mcp.json 超过 2 MiB 安全上限".into());
    }
    crate::paths::write_private_file(path, &content)
}

fn write_mirror_document(content: &[u8]) -> Result<(), String> {
    if content.len() > MAX_MCP_CONFIG_BYTES {
        return Err("mcp.json 超过 2 MiB 安全上限".into());
    }
    with_mirror_transaction(|| crate::paths::write_private_file(&mcp_json_path(), content))
}

fn mirror_single_server(server: &McpUpsertRequest, _preserve_runtime: bool) -> Result<(), String> {
    with_mirror_transaction(|| {
        let mut servers = read_mirror_servers_unlocked()?;
        let mut next = request_to_mirror_json(server);
        // Keep editor-only token prefill fields that are intentionally not valid
        // in the Runtime's HTTP schema.
        if let (Some(old), Some(next_obj)) = (servers.get(&server.name), next.as_object_mut()) {
            if let Some(old_env) = old.get("env") {
                next_obj.entry("env").or_insert_with(|| old_env.clone());
            }
        }
        servers.insert(server.name.clone(), next);
        write_mirror_servers_unlocked(servers)
    })
}

fn mirror_delete_server(name: &str) -> Result<(), String> {
    with_mirror_transaction(|| {
        let mut servers = read_mirror_servers_unlocked()?;
        servers.remove(name);
        write_mirror_servers_unlocked(servers)
    })
}

fn mirror_toggle_server(name: &str, enabled: bool) -> Result<(), String> {
    with_mirror_transaction(|| {
        let mut servers = read_mirror_servers_unlocked()?;
        if let Some(config) = servers
            .get_mut(name)
            .and_then(|value| value.as_object_mut())
        {
            config.insert("enabled".into(), enabled.into());
            write_mirror_servers_unlocked(servers)?;
        }
        Ok(())
    })
}

// ---------- MCP OAuth authorization (echo.agent/mcp/auth_*) ----------
//
// EchoAgent implements the full MCP OAuth flow itself (RFC 9728/8414 discovery,
// DCR, PKCE, system-browser redirect + loopback callback; tokens persist to
// `~/.echo-agent/mcp_credentials.json`). We only need to *trigger* it: the browser
// opens from inside the EchoAgent process, and the call resolves when the flow
// completes — mirroring echo-agent's "跳浏览器授权 + 轮询状态" UX.

/// Result of `echo.agent/mcp/auth_trigger`, surfaced to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAuthTriggerResult {
    /// "authenticated" | "failed" | "setup_required".
    pub status: String,
    /// Failure detail from EchoAgent (present when status == "failed").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpAuthTriggerWire {
    status: String,
    #[serde(default)]
    error: Option<String>,
}

/// Kick off the browser OAuth flow for one server. Long-running: resolves
/// when the user finishes (or abandons) the browser flow. EchoAgent opens the
/// system browser itself (`webbrowser::open` in echo-agent-mcp).
#[tauri::command]
pub async fn mcp_auth_trigger(
    state: State<'_, AppState>,
    session_id: String,
    server_name: String,
) -> Result<McpAuthTriggerResult, String> {
    validate_session_id(&session_id)?;
    validate_server_name(&server_name)?;
    state.session_workspace(&session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let params = raw_params(&serde_json::json!({
        "session_id": session_id,
        "server_name": server_name,
    }));
    let wire: McpAuthTriggerWire = call_ext(&tx, "echo.agent/mcp/auth_trigger", params)
        .await
        .map_err(|e| e.to_string())?;
    if wire.status.chars().count() > 64
        || wire.status.chars().any(char::is_control)
        || wire
            .error
            .as_ref()
            .is_some_and(|error| error.chars().count() > 4_096)
    {
        return Err("MCP 授权响应无效或过长".into());
    }
    Ok(McpAuthTriggerResult {
        status: wire.status,
        error: wire.error,
    })
}

/// One entry of `echo.agent/mcp/auth_status` — a server EchoAgent has flagged as
/// requiring authorization (status is currently always "needs_auth").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAuthStatusEntry {
    /// EchoAgent's wire format is snake_case (`server_name`); the frontend gets
    /// camelCase (`serverName`). Accept both on decode.
    #[serde(alias = "server_name")]
    pub server_name: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
struct McpAuthStatusWire {
    #[serde(default)]
    servers: Vec<McpAuthStatusEntry>,
}

/// List servers that EchoAgent has marked `needs_auth` for this session.
#[tauri::command]
pub async fn mcp_auth_status(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<McpAuthStatusEntry>, String> {
    validate_session_id(&session_id)?;
    state.session_workspace(&session_id)?;
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let params = raw_params(&serde_json::json!({ "session_id": session_id }));
    let wire: McpAuthStatusWire = call_ext(&tx, "echo.agent/mcp/auth_status", params)
        .await
        .map_err(|e| e.to_string())?;
    if wire.servers.len() > MAX_MCP_SERVERS
        || wire.servers.iter().any(|entry| {
            validate_server_name(&entry.server_name).is_err()
                || entry.status.chars().count() > 64
                || entry.status.chars().any(char::is_control)
        })
    {
        return Err("MCP 授权状态响应无效或过大".into());
    }
    Ok(wire.servers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_confirmation_requires_the_explicit_allow_button() {
        assert!(stdio_confirmation_approved(MessageDialogResult::Custom(
            "允许本次".into()
        )));
        for result in [
            MessageDialogResult::Cancel,
            MessageDialogResult::No,
            MessageDialogResult::Ok,
            MessageDialogResult::Yes,
            MessageDialogResult::Custom("取消".into()),
            MessageDialogResult::Custom("允许本次 ".into()),
        ] {
            assert!(!stdio_confirmation_approved(result));
        }
    }

    #[test]
    fn setup_and_tool_payloads_are_bounded() {
        let mut too_many = HashMap::new();
        for index in 0..=MAX_SETUP_FIELDS {
            too_many.insert(format!("key-{index}"), "value".to_string());
        }
        assert!(validate_setup_values(&too_many).is_err());
        assert!(validate_setup_values(&HashMap::from([(
            "token".into(),
            "x".repeat(MAX_SETUP_VALUE_BYTES + 1),
        )]))
        .is_err());
        assert!(
            validate_setup_values(&HashMap::from([("token".into(), "secret".into(),)])).is_ok()
        );
        assert!(validate_session_id(&"s".repeat(MAX_SESSION_ID_CHARS + 1)).is_err());
        assert!(validate_tool_name(&"t".repeat(MAX_TOOL_NAME_CHARS + 1)).is_err());
    }

    #[test]
    fn runtime_list_wire_maps_transport_health_and_tools() {
        let response: McpListResponse = serde_json::from_value(serde_json::json!({
            "servers": [{
                "name": "filesystem",
                "source": "local",
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "server-filesystem"],
                "env": [{ "name": "ROOT", "value": "/tmp" }],
                "setup": {
                    "fields": [{
                        "id": "region",
                        "label": "Region",
                        "type": "select",
                        "required": true,
                        "options": [{ "label": "US", "value": "us" }]
                    }],
                    "variables": {}
                },
                "setupValues": { "region": "us" },
                "session": {
                    "enabled": true,
                    "status": "ready",
                    "tools": [{
                        "name": "filesystem__read_file",
                        "displayName": "read_file",
                        "description": "Read one file",
                        "enabled": true
                    }],
                    "authRequired": false,
                    "setupRequired": false
                }
            }]
        }))
        .unwrap();
        let entry = map_wire_entry(response.into_servers().remove(0));
        assert_eq!(entry.transport.as_deref(), Some("stdio"));
        assert_eq!(entry.target.as_deref(), Some("npx"));
        assert_eq!(entry.status.as_deref(), Some("ready"));
        assert!(entry.enabled);
        assert!(entry.live);
        assert_eq!(entry.tools.len(), 1);
        assert_eq!(entry.env.get("ROOT").map(String::as_str), Some("/tmp"));
        assert_eq!(entry.setup.as_ref().unwrap().fields[0].id, "region");
        assert_eq!(
            entry.setup_values.get("region").map(String::as_str),
            Some("us")
        );
    }

    #[test]
    fn mcp_json_parser_preserves_advanced_http_options() {
        let request = json_to_upsert(
            "linear",
            &serde_json::json!({
                "url": "https://mcp.example.test/mcp",
                "type": "streamable-http",
                "headers": { "X-Tenant": "demo" },
                "bearerTokenEnvVar": "LINEAR_TOKEN",
                "oauthClientId": "desktop",
                "oauthScopes": ["read", "write"],
                "startupTimeoutSec": 20,
                "toolTimeoutSec": 90,
                "oauth": {
                    "clientId": "nested-client",
                    "scopes": ["offline_access"],
                    "callbackPort": 34567
                },
                "toolTimeouts": { "search": 120 },
                "exposeImageBase64": true
            }),
        )
        .unwrap();
        assert_eq!(request.transport, "streamable_http");
        assert_eq!(
            request.bearer_token_env_var.as_deref(),
            Some("LINEAR_TOKEN")
        );
        assert_eq!(request.oauth_scopes, vec!["read", "write"]);
        assert_eq!(request.tool_timeout_sec, Some(90));
        assert_eq!(request.tool_timeouts.get("search"), Some(&120));
        assert_eq!(
            request.oauth.as_ref().and_then(|oauth| oauth.callback_port),
            Some(34567)
        );
        assert_eq!(request.expose_image_base64, Some(true));
        let roundtrip = request_to_mirror_json(&request);
        assert_eq!(roundtrip["toolTimeouts"]["search"], 120);
        assert_eq!(roundtrip["oauth"]["clientId"], "nested-client");
        validate_upsert_request(&request).unwrap();
    }

    #[test]
    fn validation_rejects_insecure_remote_http_but_allows_loopback() {
        let mut request = McpUpsertRequest {
            name: "demo".into(),
            transport: "streamable_http".into(),
            target: "http://example.test/mcp".into(),
            args: Vec::new(),
            env: HashMap::new(),
            headers: HashMap::new(),
            enabled: Some(true),
            cwd: None,
            bearer_token_env_var: None,
            oauth_client_id: None,
            oauth_client_secret_env_var: None,
            oauth_scopes: Vec::new(),
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            oauth: None,
            setup: None,
            tool_timeouts: HashMap::new(),
            expose_image_base64: None,
        };
        assert!(validate_upsert_request(&request)
            .unwrap_err()
            .contains("HTTPS"));
        request.target = "http://127.0.0.1:3000/mcp".into();
        validate_upsert_request(&request).unwrap();

        request.transport = "stdio".into();
        request.target = "npx".into();
        assert!(stdio_change_requires_confirmation(&request, &[]));
        assert!(!stdio_change_requires_confirmation(
            &request,
            std::slice::from_ref(&request)
        ));
        let mut changed = request.clone();
        changed.args.push("untrusted-package".into());
        assert!(stdio_change_requires_confirmation(
            &changed,
            std::slice::from_ref(&request)
        ));
        changed.enabled = Some(false);
        assert!(!stdio_change_requires_confirmation(
            &changed,
            std::slice::from_ref(&request)
        ));
    }

    #[test]
    fn complete_mcp_document_update_preserves_unrelated_runtime_config() {
        let mut root: toml::Value = r#"
disabled_mcp_servers = ["old", "concurrent"]

[custom]
keep = "hand-written"

[model_providers.openai]
base_url = "https://api.example.test/v1"

[mcp_servers.old]
command = "old-command"

[mcp_servers.concurrent]
command = "must-survive"

[disabled_mcp_tools]
old = ["legacy"]
"#
        .parse()
        .unwrap();
        let replacement = McpUpsertRequest {
            name: "replacement".into(),
            transport: "stdio".into(),
            target: "new-command".into(),
            args: vec!["--safe".into()],
            env: HashMap::new(),
            headers: HashMap::new(),
            enabled: Some(false),
            cwd: None,
            bearer_token_env_var: None,
            oauth_client_id: None,
            oauth_client_secret_env_var: None,
            oauth_scopes: Vec::new(),
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            oauth: None,
            setup: None,
            tool_timeouts: HashMap::new(),
            expose_image_base64: None,
        };

        apply_mcp_document(&mut root, &[replacement], &["old".into()]).unwrap();

        assert_eq!(root["custom"]["keep"].as_str(), Some("hand-written"));
        assert_eq!(
            root["model_providers"]["openai"]["base_url"].as_str(),
            Some("https://api.example.test/v1")
        );
        assert!(root["mcp_servers"].get("old").is_none());
        assert_eq!(
            root["mcp_servers"]["concurrent"]["command"].as_str(),
            Some("must-survive"),
            "an MCP entry added after the editor snapshot must not be erased"
        );
        assert_eq!(
            root["mcp_servers"]["replacement"]["command"].as_str(),
            Some("new-command")
        );
        let disabled = root["disabled_mcp_servers"].as_array().unwrap();
        assert!(disabled
            .iter()
            .any(|value| value.as_str() == Some("concurrent")));
        assert!(disabled
            .iter()
            .any(|value| value.as_str() == Some("replacement")));
        assert!(!disabled.iter().any(|value| value.as_str() == Some("old")));
        assert!(root
            .get("disabled_mcp_tools")
            .and_then(|value| value.get("old"))
            .is_none());
    }

    #[test]
    fn corrupt_or_oversized_mirror_is_rejected_without_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mcp.json");
        let corrupt = b"{ definitely-not-json";
        std::fs::write(&path, corrupt).unwrap();

        assert!(read_mirror_servers_at(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);

        std::fs::write(&path, vec![b'x'; MAX_MCP_CONFIG_BYTES + 1]).unwrap();
        assert!(read_mirror_servers_at(&path)
            .unwrap_err()
            .contains("安全上限"));
    }

    #[test]
    fn mirror_writer_roundtrips_a_bounded_document() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mcp.json");
        let servers = serde_json::Map::from_iter([(
            "demo".to_string(),
            serde_json::json!({ "command": "echo", "args": ["ok"] }),
        )]);

        write_mirror_servers_at(&path, servers.clone()).unwrap();
        assert_eq!(read_mirror_servers_at(&path).unwrap(), servers);
    }

    #[test]
    fn toggle_rejects_malformed_or_unbounded_disabled_lists() {
        let mut malformed: toml::Value = r#"disabled_mcp_servers = ["ok", 42]"#.parse().unwrap();
        assert!(apply_mcp_toggle(&mut malformed, "demo", true).is_err());

        let values = (0..=MAX_MCP_SERVERS)
            .map(|index| toml::Value::String(format!("server-{index}")))
            .collect();
        let mut oversized = toml::Value::Table(toml::map::Map::from_iter([(
            "disabled_mcp_servers".into(),
            toml::Value::Array(values),
        )]));
        assert!(apply_mcp_toggle(&mut oversized, "demo", true).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn bounded_reader_rejects_symlinked_mirror() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("outside.json");
        let link = temp.path().join("mcp.json");
        std::fs::write(&target, EMPTY_MCP_JSON).unwrap();
        symlink(&target, &link).unwrap();

        assert!(read_mirror_servers_at(&link)
            .unwrap_err()
            .contains("符号链接"));
    }
}
