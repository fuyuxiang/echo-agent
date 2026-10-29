//! In-process EchoAgent runtime bridge.
//!
//! The agent (`MvpAgent` from `echo-agent-runtime`) is `!Send` (all fields are
//! `Rc`/`RefCell`), so it must live on one OS thread driven by a
//! current-thread tokio runtime + `LocalSet`. We spawn that thread once at
//! app startup and communicate with the agent purely through the typed ACP
//! mpsc channels from `echo-agent-acp::acp_channels()`.
//!
//! Pattern A (direct dispatch) from `echo-agent-pager/src/acp/spawn.rs`: the
//! gateway receiver calls `MvpAgent`'s `acp::Agent` methods directly over
//! `Rc<MvpAgent>` — no byte streams, no line framing, no WebSocket.
//!
//! `ClientCapabilities` advertise NO fs/terminal support, so the agent uses
//! its own built-in tool implementations (read_file, run_terminal_command,
//! etc.) rather than round-tripping them back to us. We only need to handle
//! `session/update` (streaming + tool calls) and `session/request_permission`.

use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use agent_client_protocol as acp;
use anyhow::{anyhow, Result};
use base64::Engine;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use echo_agent_acp::{
    acp_channels, acp_send, AcpAgentGatewaySender, AcpAgentTx, AcpClientRx, AcpGatewayReceiver,
};
use echo_agent_runtime::agent::init::bootstrap;
use echo_agent_runtime::agent::mvp_agent::MvpAgent;
use echo_agent_runtime::auth::{AuthManager, EchoAgentComConfig};
use echo_agent_runtime::util::config::load_effective_config;

// Re-aliased to mirror EchoAgent's own internal import style.
use echo_agent_runtime::agent::config::{Config as AgentConfig, RuntimeResolutionContext};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

const MAX_IMAGE_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
pub(crate) const DESKTOP_CLIENT_IDENTIFIER: &str = "echo-agent-desktop";
pub(crate) const OJLAB_BASE_URL: &str = "http://123.56.188.16:8088/v1";
pub(crate) const MEMORY_EMBEDDING_ENDPOINT: &str = "http://123.56.188.16:8088/v1/embeddings";
pub(crate) const MEMORY_EMBEDDING_MODEL: &str = "embed-pro";
pub(crate) const MEMORY_EMBEDDING_DIMENSIONS: usize = 1024;
pub(crate) const MEMORY_RERANK_ENDPOINT: &str = "http://123.56.188.16:8088/v1/rerank";
pub(crate) const MEMORY_RERANK_MODEL: &str = "rerank-pro";

fn configure_memory_retrieval(cfg: &mut AgentConfig) {
    let Some(memory) = cfg.memory_config.as_mut() else {
        return;
    };

    memory.embedding.provider = "api".to_owned();
    memory.embedding.model = Some(MEMORY_EMBEDDING_MODEL.to_owned());
    memory.embedding.dimensions = MEMORY_EMBEDDING_DIMENSIONS;
    memory.embedding.endpoint = Some(MEMORY_EMBEDDING_ENDPOINT.to_owned());
    // Session startup falls back to the chat model's API key when this is None.
    // An explicit empty key keeps the built-in embedding service keyless.
    memory.embedding.api_key = Some(String::new());
    memory.embedding.send_dimensions = false;

    // The model-based reranker becomes the final ordering stage. The runtime's
    // lightweight Jaccard MMR remains available as the network-failure fallback
    // ordering but is not applied before a successful model rerank.
    memory.search.reranker.enabled = true;
    memory.search.reranker.endpoint = Some(MEMORY_RERANK_ENDPOINT.to_owned());
    memory.search.reranker.model = Some(MEMORY_RERANK_MODEL.to_owned());
    memory.search.reranker.api_key = None;
}

/// One end of the ACP channel pair that lives on the Tauri (multi-thread) side.
/// The client sends requests to the agent via `tx` (`AcpAgentTx`) and receives
/// responses/notifications from the agent via `rx` (`AcpClientRx`). The agent
/// thread holds the other end.
pub struct AgentHandle {
    pub tx: AcpAgentTx,
    pub rx: AcpClientRx,
    pub cancel: CancellationToken,
    /// Becomes true only after live session actors have completed their
    /// graceful shutdown hooks and the ACP gateway has stopped.
    pub shutdown_complete: tokio::sync::watch::Receiver<bool>,
    /// Most recent final persistence failure while the worker retains its
    /// LocalSet for retry. The host includes it in a recoverable exit error.
    pub shutdown_failure: Arc<Mutex<Option<String>>>,
    /// Reports a failed gateway before its still-live persistence actors finish.
    pub gateway_failure: tokio::sync::watch::Receiver<Option<String>>,
    /// JoinHandle for the agent OS thread. Used to detect unexpected exits
    /// (panics, crashes) so the frontend can show a "restart agent" prompt.
    pub thread: Option<std::thread::JoinHandle<Result<()>>>,
}

/// Accept only an absolute http(s) URL, mirroring `providers::usable_model_ids`.
fn usable_base_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let parsed = url::Url::parse(trimmed).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| trimmed.to_string())
}

/// Pick the first usable BYOK inference base URL, provider entries first.
///
/// Split from the raw-TOML walk so the selection rule is unit-testable without
/// constructing a Runtime config: `provider_base_urls` comes from
/// `[model_providers.*].base_url` (the format the settings UI writes) and
/// `model_base_urls` from per-model `base_url` keys (the legacy format).
/// Providers win because a UI-configured model carries no `base_url` of its own.
fn pick_byok_base_url<'a>(
    provider_base_urls: impl IntoIterator<Item = &'a str>,
    model_base_urls: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    provider_base_urls
        .into_iter()
        .find_map(usable_base_url)
        .or_else(|| model_base_urls.into_iter().find_map(usable_base_url))
}

/// Per-model `base_url` values from the legacy config format.
fn legacy_model_base_urls(cfg: &AgentConfig) -> Vec<&str> {
    cfg.config_models
        .values()
        .filter_map(|model| model.base_url.as_deref())
        .collect()
}

/// Spawn the EchoAgent runtime in-process on a dedicated thread.
///
/// `cwd` is the working directory the agent binds sessions to (typically the
/// user's home or a chosen project). Model credentials come exclusively from
/// provider configuration; the upstream auth adapter is intentionally empty.
///
/// EchoAgent intentionally skips the upstream startup remote-settings/models
/// prefetch (the synchronous `start_early_prefetch` join inside `bootstrap`).
/// That call hits upstream remote backends and can block first launch for tens of seconds
/// on slow networks; BYOK users only need local `config.toml` models. We seed
/// an empty `RemoteSettings` so bootstrap treats remote config as already
/// supplied and never opens the network path.
pub fn spawn_agent_runtime(_cwd: PathBuf) -> Result<AgentHandle> {
    // Team tools 已迁移到内嵌 MCP server（team_mcp.rs，lib.rs 启动时 serve）。
    // 这里不再需要注册 —— new_session 会把 MCP server 传给 EchoAgent，EchoAgent 以
    // client 身份连接（工具名 echoagent__create_team 等）。对 EchoAgent 零补丁。

    crate::sessions::migrate_builtin_models_before_runtime()
        .map_err(|error| anyhow!("migrate saved task models: {error}"))?;
    crate::providers::ensure_builtin_model_config()
        .map_err(|error| anyhow!("install built-in model catalog: {error}"))?;

    // 0. Pin off the Runtime's upstream model-catalog fetch before the config is
    // read, so the load below already sees it. BYOK-only: the remote catalog adds
    // nothing but would merge upstream-branded model ids into the picker. A
    // failure here is not fatal — the branded-id filters downstream still hold —
    // so it is logged rather than propagated.
    match crate::agent_config::ensure_remote_fetch_disabled() {
        Ok(true) => tracing::info!("BYOK: pinned [features] remote_fetch = false"),
        Ok(false) => {}
        Err(error) => {
            tracing::warn!(%error, "could not pin [features] remote_fetch; continuing")
        }
    }

    // 1. Load + resolve config (~/.echo-agent/config.toml; defaults if absent).
    let raw = load_effective_config().map_err(|e| anyhow!("load config: {e}"))?;
    let mut cfg = AgentConfig::new_from_toml_cfg(&raw).map_err(|e| anyhow!("parse config: {e}"))?;
    // `default_yolo_mode` is a launch-only field (`serde(skip)`) and
    // `default_auto_mode` is not derived by `new_from_toml_cfg`. Seed both from
    // the desktop's canonical permission reader before runtime resolution so
    // managed requirements can still clamp them in the safe direction.
    let permission_mode = crate::permission_config::read_permission_mode();
    let (default_yolo_mode, default_auto_mode) =
        crate::permission_config::permission_mode_flags(&permission_mode);
    cfg.default_yolo_mode = default_yolo_mode;
    cfg.default_auto_mode = default_auto_mode;
    // The embedded Runtime ships memory as an opt-in feature. EchoAgent is a
    // local Agent workspace, so memory is a first-class feature and defaults
    // on unless the user explicitly disables `[memory].enabled`.
    // `raw` is the embedded Runtime's toml 0.9 value while the desktop
    // settings module intentionally uses its direct toml 0.8 dependency.
    // Read this host override in-place instead of crossing those crate types.
    // Keep the backend available while disabled so settings can take effect
    // in existing sessions. Each memory operation checks the live policy.
    let memory_enabled = true;
    // Local settings only. Must be set both here (runtime
    // resolution) and on `cfg.remote_settings` before bootstrap (see below).
    let mut local_remote_settings = echo_agent_runtime::util::config::RemoteSettings::default();
    // A hint has lower precedence than an explicit preference and Runtime can
    // ignore it when model filters exclude it. Never write Auto to disk.
    let configured = cfg.config_models.keys().cloned().collect::<Vec<_>>();
    local_remote_settings.default_model =
        crate::commands::effective_configured_model_id(&configured, &configured, None);
    if cfg
        .models
        .default
        .as_deref()
        .is_some_and(|id| id.trim().is_empty())
    {
        cfg.models.default = None;
    }
    cfg.resolve_runtime_fields(&RuntimeResolutionContext {
        raw_config: &raw,
        remote_settings: Some(&local_remote_settings),
        is_headless: true,
        cli_subagents: Some(false),
        cli_web_search_model: None,
        cli_session_summary_model: None,
        memory_enabled_override: Some(memory_enabled),
        disable_web_search: false,
        todo_gate: false,
        laziness_debug_log: None,
        storage_mode: None,
    });
    configure_memory_retrieval(&mut cfg);
    tracing::info!(
        permission_mode,
        default_yolo_mode = cfg.default_yolo_mode,
        default_auto_mode = cfg.default_auto_mode,
        "resolved desktop permission mode for agent launch"
    );

    // Skip bootstrap's shell-level remote_settings fallback fetch
    // (`start_early_prefetch` + thread join). See module comment above.
    cfg.remote_settings = Some(local_remote_settings);

    // BYOK model isolation: if the user has any BYOK connection with a custom
    // base_url, set endpoints.models_base_url so the runtime's `has_custom_endpoint()`
    // returns true → skips loading built-in default models. Those built-ins route
    // through an upstream proxy that requires upstream credentials, so selecting one
    // in a BYOK-only setup yields 401 — and their upstream-branded ids would surface
    // in the desktop UI (model picker, About dialog, usage table).
    //
    // `raw` is the embedded Runtime's toml 0.9 value while this crate pins its
    // own toml 0.8, so it is walked through inherent methods without naming the
    // type — the same in-place approach as the `[memory] enabled` read above.
    if cfg.endpoints.models_base_url.is_none() {
        let provider_urls: Vec<&str> = raw
            .get("model_providers")
            .and_then(|providers| providers.as_table())
            .into_iter()
            .flat_map(|providers| providers.values())
            .filter_map(|provider| provider.get("base_url")?.as_str())
            .collect();
        let model_urls = legacy_model_base_urls(&cfg);
        if let Some(first_byok_url) = pick_byok_base_url(provider_urls, model_urls) {
            tracing::info!(
                models_base_url = %first_byok_url,
                "BYOK: set endpoints.models_base_url to skip built-in default models"
            );
            cfg.endpoints.models_base_url = Some(first_byok_url);
        }
    }

    // 2. The upstream agent constructor requires an AuthManager even for
    // provider-configured models. Isolate it from legacy auth.json and disable
    // inline/alternate upstream credentials so it remains an inert adapter.
    let provider_runtime_home = crate::paths::echo_agent_home_dir().join(".provider-runtime");
    let auth_manager = Arc::new(AuthManager::new(
        &provider_runtime_home,
        EchoAgentComConfig::default(),
    ));

    // 3. Bootstrap: telemetry, bundled files, ModelsManager.
    // Network catalog prefetch is skipped because remote_settings is already Some.
    let (cfg, models_manager) =
        bootstrap(&cfg, &auth_manager, None).map_err(|e| anyhow!("bootstrap: {e}"))?;

    // 4. Typed ACP channel pair.
    let (acp_client, acp_agent) = acp_channels();
    let cancel = CancellationToken::new();

    // 5. Agent thread (!Send → own OS thread + current_thread runtime + LocalSet).
    let cancel_for_thread = cancel.clone();
    let (shutdown_complete_tx, shutdown_complete_rx) = tokio::sync::watch::channel(false);
    let shutdown_failure = Arc::new(Mutex::new(None));
    let failure_for_thread = shutdown_failure.clone();
    let (gateway_failure_tx, gateway_failure_rx) = tokio::sync::watch::channel(None);
    let thread_handle = std::thread::Builder::new()
        .name("echo-agent-runtime".into())
        .spawn(move || -> Result<()> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let client_tx = acp_agent.tx.clone();
                // The gateway sender implements `acp::Client` and forwards the
                // agent's reverse-direction calls onto our mpsc channel.
                // `AcpAgentGatewaySender` = `AcpGatewaySender<acp::AgentSide>`,
                // whose OutMessage is `AcpClientMessage` — matching what
                // `acp_agent.tx` (Sender<AcpClientMessage>) accepts.
                let gateway = AcpAgentGatewaySender::new(client_tx);
                let mut agent = MvpAgent::with_models(gateway, &cfg, auth_manager, models_manager);
                // `with_models` deliberately leaves MvpAgent::memory_config as None.
                // Upstream launchers apply the already-resolved configuration with
                // `set_memory_config`; the desktop's direct-dispatch launcher must do
                // the same or every session starts without a memory backend.
                if let Some(memory_config) = cfg.memory_config.clone() {
                    agent.set_memory_config(memory_config);
                }
                let agent_rc = Rc::new(agent);

                // Direct dispatch: the receiver calls MvpAgent's `acp::Agent`
                // methods directly (Pattern A from spawn_echo_agent_shell). Use the
                // generic AcpGatewayReceiver (not the AcpAgentGatewayReceiver
                // alias, which fixes C = AgentSideConnection).
                let shutdown_agent = agent_rc.clone();
                let gw_rx = AcpGatewayReceiver::new(acp_agent.rx, agent_rc).with_tracing(true);
                let gateway_task = tokio::task::spawn_local(gw_rx.run());
                tokio::pin!(gateway_task);
                tokio::task::yield_now().await;

                // The gateway is the actual ACP event loop. Supervising it is
                // essential: if it exits because the embedded agent panics or
                // its channel closes, waiting only for the cancellation token
                // would leave the desktop with a permanently dead sender that
                // still appears initialized.
                let (outcome, gateway_finished) = tokio::select! {
                    _ = cancel_for_thread.cancelled() => (Ok(()), false),
                    result = &mut gateway_task => {
                        (Err(anyhow!("ACP gateway exited unexpectedly: {result:?}")), true)
                    }
                };
                if gateway_finished {
                    let _ = gateway_failure_tx.send(outcome.as_ref().err().map(ToString::to_string));
                }
                // A gateway failure also needs a durable session barrier;
                // its LocalSet can still contain healthy persistence actors.
                loop {
                    match shutdown_agent.flush_all_sessions_checked(std::time::Duration::from_secs(10)).await {
                        Ok(()) => break,
                        Err(error) => {
                            tracing::warn!(%error, "retaining Runtime until final session persistence can be retried");
                            *failure_for_thread.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
                            // A cached negative ACK may return immediately.
                            // Yield so the actor's final-write retry can run.
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        }
                    }
                }
                if !gateway_finished {
                    gateway_task.as_mut().abort();
                    let _ = gateway_task.await;
                }
                *failure_for_thread.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                let _ = shutdown_complete_tx.send(true);
                outcome
            })
        })?;

    Ok(AgentHandle {
        tx: acp_client.tx,
        rx: acp_client.rx,
        cancel,
        shutdown_complete: shutdown_complete_rx,
        shutdown_failure,
        gateway_failure: gateway_failure_rx,
        thread: Some(thread_handle),
    })
}

// ---------- ACP lifecycle helpers ----------

/// Outcome of `initialize`. Provider authentication is configured separately.
#[derive(Debug, Serialize, Clone)]
pub struct InitOutcome {
    pub ok: bool,
    pub default_model_id: Option<String>,
    pub agent_version: Option<String>,
}

/// Run `initialize` against the agent. Advertises NO fs/terminal capability
/// so the agent runs its own tools.
pub async fn initialize(tx: &AcpAgentTx) -> Result<InitOutcome> {
    let req = acp::InitializeRequest::new(acp::ProtocolVersion::V1)
        .client_capabilities(
            // Advertise NO fs and NO terminal capability → the agent uses its
            // own built-in file/shell tools and never round-trips those
            // requests back to us. (ClientCapabilities::new() defaults to
            // both disabled; folder-trust is an EchoAgent extension capability
            // and must live under clientCapabilities._meta.)
            desktop_client_capabilities(),
        )
        .meta(Some(desktop_client_meta()));
    let resp: acp::InitializeResponse = acp_send(req, tx)
        .await
        .map_err(|e| anyhow!("initialize: {e:?}"))?;

    Ok(InitOutcome {
        ok: true,
        // EchoAgent's modelState uses `currentModelId` (not `defaultModelId`) for
        // the active model. Try both keys defensively in case of version skew.
        default_model_id: resp
            .meta
            .as_ref()
            .and_then(|m| m.get("modelState"))
            .and_then(|v| v.get("currentModelId").or_else(|| v.get("defaultModelId")))
            .and_then(|v| v.as_str())
            .map(String::from),
        agent_version: resp
            .meta
            .as_ref()
            .and_then(|m| m.get("agentVersion"))
            .and_then(|v| v.as_str())
            .map(String::from),
    })
}

fn desktop_client_meta() -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({
        // Runtime's ClientType serde spelling is underscored; the stable
        // product identifier is also supplied for origin/session scoping.
        "clientType": "echo_agent_desktop",
        "clientIdentifier": DESKTOP_CLIENT_IDENTIFIER,
        "clientVersion": env!("CARGO_PKG_VERSION"),
    })
    .as_object()
    .cloned()
    .expect("desktop client metadata is an object")
}

fn desktop_client_capabilities() -> acp::ClientCapabilities {
    let meta = serde_json::json!({
        "echo.agent/folderTrust": { "interactive": true },
    });
    acp::ClientCapabilities::new()
        .terminal(false)
        .meta(meta.as_object().cloned())
}

/// Create a new session bound to `cwd`. Returns the new session id.
///
/// If `model_id` is supplied, it is passed as `_meta.modelId` so EchoAgent binds
/// the session to that model from the very start. This avoids the
/// new_session → set_session_model two-step, which could leave the session's
/// sampling config pinned to EchoAgent's default model (`echo-agent-build`, which has
/// no key in a BYOK-only setup) before the switch lands.
///
/// The in-process team MCP server (team_mcp.rs) rides along as a client-side
/// `mcp_servers` entry — EchoAgent's merge gives the client layer top priority,
/// so the team tools (`echoagent__create_team` etc.) are live from this
/// session's first turn. Team tools are persisted separately after the
/// session exists. Knowledge is intentionally not attached here: the desktop
/// synchronizes the user's explicit per-task source selection before sending.
/// Reasoning effort and permission mode are also session-owned. Automations
/// persist the legacy `modelIsThinking` switch; mapping it to `high` makes that
/// switch affect the actual runtime instead of being dead metadata.
pub async fn new_session_with_options(
    tx: &AcpAgentTx,
    cwd: &Path,
    model_id: Option<&str>,
    reasoning_effort: Option<&str>,
    permission_mode_override: Option<&str>,
) -> Result<String> {
    tracing::info!(cwd = %cwd.display(), model_id, "echoagent: new_session send");
    let permission_mode = permission_mode_override
        .map(str::to_string)
        .unwrap_or_else(crate::permission_config::permission_mode_for_session);
    let req = new_session_request(
        cwd,
        model_id,
        reasoning_effort,
        &permission_mode,
        current_desktop_mcp_servers(),
    );
    let resp: acp::NewSessionResponse = acp_send(req, tx).await.map_err(|e| {
        tracing::error!(error = ?e, "echoagent: new_session FAILED");
        anyhow!("new_session: {e:?}")
    })?;
    tracing::info!(session_id = %resp.session_id.0, "echoagent: new_session OK");
    Ok(resp.session_id.0.to_string())
}

/// Local service addresses and bearer tokens rotate with this desktop process.
/// New and restored sessions must receive this same current client-layer seed,
/// before the Runtime merges the previously persisted TOML registrations.
fn current_desktop_mcp_servers() -> Vec<acp::McpServer> {
    desktop_mcp_servers_from_registration([
        crate::team_mcp::server_url().zip(crate::team_mcp::authorization_header()),
        crate::automation::server_url().zip(crate::automation::authorization_header()),
        crate::office_mcp::server_url().zip(crate::office_mcp::authorization_header()),
        crate::live_info_mcp::server_url().zip(crate::live_info_mcp::authorization_header()),
    ])
}

fn desktop_mcp_servers_from_registration(
    registrations: [Option<(String, String)>; 4],
) -> Vec<acp::McpServer> {
    let [team, automation, office, live] = registrations;
    let mut servers = Vec::new();
    if let Some((url, authorization)) = team {
        servers.push(authenticated_team_mcp_server(url, authorization));
    }
    if let Some((url, authorization)) = automation {
        servers.push(authenticated_automation_mcp_server(url, authorization));
    }
    if let Some((url, authorization)) = office {
        servers.push(acp::McpServer::Http(
            acp::McpServerHttp::new(crate::office_mcp::MCP_SERVER_NAME, url).headers(vec![
                acp::HttpHeader::new("Authorization", authorization),
                acp::HttpHeader::new(crate::office_mcp::SESSION_HEADER, "${session_id}"),
            ]),
        ));
    }
    if let Some((url, authorization)) = live {
        servers.push(acp::McpServer::Http(
            acp::McpServerHttp::new(crate::live_info_mcp::MCP_SERVER_NAME, url)
                .headers(vec![acp::HttpHeader::new("Authorization", authorization)]),
        ));
    }
    servers
}

fn new_session_request(
    cwd: &Path,
    model_id: Option<&str>,
    reasoning_effort: Option<&str>,
    permission_mode: &str,
    servers: Vec<acp::McpServer>,
) -> acp::NewSessionRequest {
    let mut meta = permission_mode_meta(permission_mode);
    if let Some(mid) = model_id.filter(|s| !s.is_empty()) {
        meta.insert("modelId".into(), serde_json::Value::String(mid.into()));
    }
    if let Some(effort) = reasoning_effort.filter(|s| !s.is_empty()) {
        meta.insert(
            "reasoningEffort".into(),
            serde_json::Value::String(effort.into()),
        );
    }
    acp::NewSessionRequest::new(cwd.to_path_buf())
        .mcp_servers(servers)
        .meta(Some(meta))
}

fn authenticated_team_mcp_server(url: String, authorization: String) -> acp::McpServer {
    acp::McpServer::Http(
        acp::McpServerHttp::new(crate::team_mcp::MCP_SERVER_NAME, url).headers(vec![
            acp::HttpHeader::new(crate::team_mcp::AUTH_HEADER, authorization),
            acp::HttpHeader::new(crate::team_mcp::SESSION_HEADER, "${session_id}"),
        ]),
    )
}

fn authenticated_automation_mcp_server(url: String, authorization: String) -> acp::McpServer {
    acp::McpServer::Http(
        acp::McpServerHttp::new(crate::automation::MCP_SERVER_NAME, url).headers(vec![
            acp::HttpHeader::new(crate::automation::AUTH_HEADER, authorization),
            // The Runtime expands this placeholder from its authoritative
            // session actor. The model never chooses the automation session.
            acp::HttpHeader::new(crate::automation::SESSION_HEADER, "${session_id}"),
        ]),
    )
}

/// Resume an existing session by replaying its persisted history.
pub async fn load_session(
    tx: &AcpAgentTx,
    session_id: &str,
    cwd: &Path,
    permission_mode: &str,
) -> Result<Option<String>> {
    load_session_with_servers(
        tx,
        session_id,
        cwd,
        permission_mode,
        current_desktop_mcp_servers(),
    )
    .await
}

async fn load_session_with_servers(
    tx: &AcpAgentTx,
    session_id: &str,
    cwd: &Path,
    permission_mode: &str,
    servers: Vec<acp::McpServer>,
) -> Result<Option<String>> {
    crate::sessions::require_migrated_model(session_id, cwd).map_err(|error| anyhow!(error))?;
    let req = acp::LoadSessionRequest::new(
        acp::SessionId::new(session_id.to_string()),
        cwd.to_path_buf(),
    )
    .mcp_servers(servers)
    .meta(Some(permission_mode_meta(permission_mode)));
    let response: acp::LoadSessionResponse = acp_send(req, tx)
        .await
        .map_err(|e| anyhow!("load_session: {e:?}"))?;
    Ok(response
        .models
        .map(|models| models.current_model_id.0.to_string()))
}

fn permission_mode_meta(mode: &str) -> serde_json::Map<String, serde_json::Value> {
    let (yolo_mode, auto_mode) = crate::permission_config::permission_mode_flags(mode);
    serde_json::Map::from_iter([
        ("yoloMode".into(), serde_json::Value::Bool(yolo_mode)),
        ("autoMode".into(), serde_json::Value::Bool(auto_mode)),
    ])
}

/// Group sessions use the same ACP transport and session actor as ordinary
/// tasks. Their profile and memory namespace are supplied on both lifecycle
/// paths; a restore must never silently become the default assistant.
pub(crate) struct GroupSessionRequest<'a> {
    pub cwd: &'a Path,
    pub model: &'a str,
    pub profile: &'a serde_json::Value,
    pub group_id: &'a str,
    pub member_id: &'a str,
    pub generation: u64,
    pub discussion: bool,
    pub existing: Option<&'a str>,
}

pub(crate) async fn group_session(
    tx: &AcpAgentTx,
    request: GroupSessionRequest<'_>,
) -> Result<String> {
    let GroupSessionRequest {
        cwd,
        model,
        profile,
        group_id,
        member_id,
        generation,
        discussion,
        existing,
    } = request;
    let mut meta = permission_mode_meta("ask");
    meta.insert("modelId".into(), serde_json::json!(model));
    meta.insert("agentProfile".into(), profile.clone());
    meta.insert(
        "echoagent.group".into(),
        serde_json::json!({"groupId":group_id,"memberId":member_id,"generation":generation,"readOnly":discussion}),
    );
    meta.insert("noReplay".into(), serde_json::json!(true));
    let mut servers = current_desktop_mcp_servers();
    servers.retain(|server| !matches!(server,acp::McpServer::Http(http) if http.name == crate::team_mcp::MCP_SERVER_NAME));
    if let (Some(url), Some(authorization)) = (
        crate::team_mcp::server_url(),
        crate::team_mcp::authorization_header(),
    ) {
        servers.push(acp::McpServer::Http(
            acp::McpServerHttp::new(crate::team_mcp::MCP_SERVER_NAME, url).headers(vec![
                acp::HttpHeader::new(crate::team_mcp::AUTH_HEADER, authorization),
                acp::HttpHeader::new(crate::team_mcp::SESSION_HEADER, "${session_id}"),
                acp::HttpHeader::new("X-EchoAgent-Group-Id", group_id),
                acp::HttpHeader::new("X-EchoAgent-Member-Id", member_id),
            ]),
        ));
    }
    if let Some(sid) = existing {
        crate::sessions::require_migrated_model(sid, cwd).map_err(|error| anyhow!(error))?;
        let req =
            acp::LoadSessionRequest::new(acp::SessionId::new(sid.to_string()), cwd.to_path_buf())
                .mcp_servers(servers)
                .meta(Some(meta));
        let _: acp::LoadSessionResponse = acp_send(req, tx)
            .await
            .map_err(|e| anyhow!("恢复群成员会话失败：{e:?}"))?;
        Ok(sid.into())
    } else {
        let req = acp::NewSessionRequest::new(cwd.to_path_buf())
            .mcp_servers(servers)
            .meta(Some(meta));
        let response: acp::NewSessionResponse = acp_send(req, tx)
            .await
            .map_err(|e| anyhow!("创建群成员会话失败：{e:?}"))?;
        Ok(response.session_id.0.to_string())
    }
}

#[cfg(test)]
pub(crate) async fn group_prompt(
    tx: &AcpAgentTx,
    session: &str,
    prompt_id: &str,
    text: &str,
    attachments: &[String],
) -> Result<crate::bridge::CompleteEvent> {
    begin_group_prompt(
        tx,
        GroupPromptRequest {
            session,
            prompt_id,
            text,
            attachments,
        },
    )?
    .complete()
    .await
}

pub(crate) struct GroupPromptRequest<'a> {
    pub session: &'a str,
    pub prompt_id: &'a str,
    pub text: &'a str,
    pub attachments: &'a [String],
}

pub(crate) struct GroupPrompt {
    session: String,
    prompt_id: String,
    response: tokio::sync::oneshot::Receiver<Result<acp::PromptResponse, acp::Error>>,
}

impl GroupPrompt {
    pub(crate) async fn complete(self) -> Result<crate::bridge::CompleteEvent> {
        let response = self
            .response
            .await
            .map_err(|_| anyhow!("群成员执行通道已关闭"))?
            .map_err(|e| anyhow!("群成员执行失败：{e:?}"))?;
        completion_from_prompt_response(response, &self.session, &self.prompt_id)
    }
}

/// Enqueue synchronously so the host can serialize prompt dispatch with Stop.
/// Waiting for the model is deliberately outside that short dispatch boundary.
pub(crate) fn begin_group_prompt(
    tx: &AcpAgentTx,
    request: GroupPromptRequest<'_>,
) -> Result<GroupPrompt> {
    let GroupPromptRequest {
        session,
        prompt_id,
        text,
        attachments,
    } = request;
    let blocks = build_prompt_blocks(text, attachments, None)?;
    let req = acp::PromptRequest::new(session.to_string(), blocks).meta(Some(
        acp::Meta::from_iter([("promptId".into(), serde_json::json!(prompt_id))]),
    ));
    let (response_tx, response) = tokio::sync::oneshot::channel();
    tx.send(
        echo_agent_acp::AcpArgs {
            request: req,
            response_tx,
        }
        .into(),
    )
    .map_err(|_| anyhow!("群成员执行通道已关闭"))?;
    Ok(GroupPrompt {
        session: session.into(),
        prompt_id: prompt_id.into(),
        response,
    })
}

/// Ask the embedded Runtime for the model catalog it currently exposes over
/// ACP. This is deliberately queried after `internal/reload_models`; disk
/// configuration alone is not evidence that the in-memory catalog changed.
pub async fn model_ids(tx: &AcpAgentTx) -> Result<Vec<String>> {
    let params = crate::ext::raw_params(&serde_json::json!({}));
    let resp: acp::ExtResponse =
        acp_send(acp::ExtRequest::new("echo.agent/models/list", params), tx)
            .await
            .map_err(|e| anyhow!("models/list: {e:?}"))?;
    let state: acp::SessionModelState =
        crate::ext::parse_ext_response(&resp).map_err(|e| anyhow!("models/list: {e}"))?;
    let mut ids = state
        .available_models
        .into_iter()
        .map(|model| model.model_id.0.to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn image_mime(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn read_image_attachment(path: &Path) -> Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| anyhow!("attachment {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(anyhow!(
            "image attachment must be a regular file: {}",
            path.display()
        ));
    }
    if metadata.len() > MAX_IMAGE_ATTACHMENT_BYTES {
        return Err(anyhow!("image attachment exceeds 20MB: {}", path.display()));
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(windows)]
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    let file = options
        .open(path)
        .map_err(|error| anyhow!("read attachment {}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| anyhow!("read attachment {}: {error}", path.display()))?
        .is_file()
    {
        return Err(anyhow!(
            "image attachment changed while opening: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_IMAGE_ATTACHMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| anyhow!("read attachment {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_IMAGE_ATTACHMENT_BYTES {
        return Err(anyhow!("image attachment exceeds 20MB: {}", path.display()));
    }
    Ok(bytes)
}

/// Build a typed ACP prompt. `displayText` and the original attachment paths
/// live in content metadata so history replay can restore the exact user-facing
/// bubble without exposing injected project/expert context.
pub(crate) fn build_prompt_blocks(
    text: &str,
    attachments: &[String],
    display_text: Option<&str>,
) -> Result<Vec<acp::ContentBlock>> {
    let mut referenced = Vec::new();
    let mut images = Vec::new();
    for raw in attachments {
        let path = PathBuf::from(raw);
        if let Some(mime) = image_mime(&path) {
            let bytes = read_image_attachment(&path)?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            images.push(acp::ContentBlock::Image(
                acp::ImageContent::new(encoded, mime).uri(Some(raw.clone())),
            ));
        }
        referenced.push(format!("- @{raw}"));
    }
    let prompt_text = if referenced.is_empty() {
        text.to_string()
    } else {
        format!(
            "{text}\n\n附件（图片已作为多模态内容附加；其他文件请使用 read_file 读取）：\n{}",
            referenced.join("\n")
        )
    };
    let mut text_meta = acp::Meta::new();
    text_meta.insert("echoDesktopTask".into(), serde_json::Value::Bool(true));
    text_meta.insert(
        "displayText".into(),
        serde_json::Value::String(display_text.unwrap_or(text).to_string()),
    );
    if !attachments.is_empty() {
        text_meta.insert(
            "echoAgentAttachments".into(),
            serde_json::json!(attachments),
        );
    }
    let text_block = acp::ContentBlock::Text(acp::TextContent::new(prompt_text).meta(text_meta));
    let mut blocks = vec![text_block];
    blocks.extend(images);
    Ok(blocks)
}

/// Send a typed ACP prompt. Supported images are carried as real ImageContent;
/// other files are explicit @path references so the runtime's read_file tool
/// can load them on demand.
pub async fn prompt_for_automation(
    tx: &AcpAgentTx,
    session_id: &str,
    prompt_id: &str,
    text: &str,
) -> Result<crate::bridge::CompleteEvent> {
    let blocks = build_team_prompt_blocks(session_id, prompt_id, text, &[], None)?;
    let meta = serde_json::Map::from_iter([("promptId".into(), serde_json::json!(prompt_id))]);
    let req = acp::PromptRequest::new(session_id.to_string(), blocks).meta(Some(meta));
    let response: acp::PromptResponse = acp_send(req, tx)
        .await
        .map_err(|e| anyhow!("prompt: {e:?}"))?;
    completion_from_prompt_response(response, session_id, prompt_id)
}

pub(crate) fn completion_from_prompt_response(
    response: acp::PromptResponse,
    session_id: &str,
    prompt_id: &str,
) -> Result<crate::bridge::CompleteEvent> {
    let mut payload = serde_json::to_value(&response.meta)?
        .as_object()
        .cloned()
        .unwrap_or_default();
    payload.insert("sessionId".into(), serde_json::json!(session_id));
    payload.insert("promptId".into(), serde_json::json!(prompt_id));
    payload.insert(
        "stopReason".into(),
        serde_json::to_value(response.stop_reason)?,
    );
    Ok(crate::bridge::parse_complete_event(
        &serde_json::Value::Object(payload),
    ))
}

/// ACP resolves this request after the complete model turn. Streamed updates
/// arrive earlier on the client rx channel. Rate limiting belongs inside the
/// runtime sampler because resubmitting a prompt persists another user turn.
pub async fn prompt_with_attachments(
    tx: &AcpAgentTx,
    session_id: &str,
    text: &str,
    attachments: &[String],
    display_text: Option<&str>,
    prompt_id: Option<&str>,
    send_now: bool,
) -> Result<()> {
    let prompt_id = prompt_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let blocks = build_team_prompt_blocks(session_id, &prompt_id, text, attachments, display_text)?;
    let mut meta = acp::Meta::new();
    meta.insert(
        "promptId".into(),
        serde_json::Value::String(prompt_id.clone()),
    );
    if send_now {
        meta.insert("sendNow".into(), serde_json::Value::Bool(true));
    }
    tracing::info!(session_id, %prompt_id, text_len = text.len(), send_now, "echoagent: prompt send");
    let req = acp::PromptRequest::new(session_id.to_string(), blocks).meta(Some(meta));
    let _: acp::PromptResponse = acp_send(req, tx).await.map_err(|e| {
        tracing::error!(session_id, error = ?e, "echoagent: prompt acp_send FAILED");
        anyhow!("prompt: {e:?}")
    })?;
    tracing::info!(session_id, %prompt_id, "echoagent: prompt turn completed");
    Ok(())
}

pub(crate) fn build_team_prompt_blocks(
    session_id: &str,
    prompt_id: &str,
    text: &str,
    attachments: &[String],
    display_text: Option<&str>,
) -> Result<Vec<acp::ContentBlock>> {
    // Validate/read attachments before registering a team for a rejected send.
    let mut blocks = build_prompt_blocks(text, attachments, display_text)?;
    let visible_text = display_text.unwrap_or(text);
    let context = crate::team_mcp::prepare_prompt(session_id, prompt_id, visible_text)
        .map_err(|error| anyhow!(error))?;
    if !context.is_empty() {
        if let Some(acp::ContentBlock::Text(block)) = blocks.first_mut() {
            block.text.push_str(&context);
            block
                .meta
                .get_or_insert_with(Default::default)
                .insert("displayText".into(), serde_json::json!(visible_text));
        }
    }
    Ok(blocks)
}

/// Switch the model used by an existing session. Maps to EchoAgent's
/// `session/set_model` ACP method (`SetSessionModelRequest`). EchoAgent will
/// re-derive sampling config, sync the API key, and broadcast a
/// `ModelChanged` notification.
///
/// Caveat: if the session has existing turns and the new model requires a
/// different agent harness, EchoAgent rejects this with
/// `MODEL_SWITCH_INCOMPATIBLE_AGENT` — surface that error to the caller so
/// the UI can prompt for a new session.
pub async fn set_session_model(tx: &AcpAgentTx, session_id: &str, model_id: &str) -> Result<()> {
    tracing::info!(session_id, model_id, "echoagent: set_session_model send");
    let req = acp::SetSessionModelRequest::new(
        acp::SessionId::new(session_id.to_string()),
        acp::ModelId::new(std::sync::Arc::from(model_id)),
    );
    let _: acp::SetSessionModelResponse = acp_send(req, tx).await.map_err(|e| {
        tracing::error!(session_id, model_id, error = ?e, "echoagent: set_session_model FAILED");
        anyhow!("set_session_model: {e:?}")
    })?;
    tracing::info!(session_id, model_id, "echoagent: set_session_model OK");
    Ok(())
}

/// Set (never toggle) the authoritative ACP session mode. The Runtime emits a
/// `CurrentModeUpdate` that the frontend consumes; callers must not infer the
/// resulting mode from stale local state.
pub async fn set_session_mode_id(tx: &AcpAgentTx, session_id: &str, mode_id: &str) -> Result<()> {
    if !matches!(
        mode_id,
        "default" | "plan" | "ask" | "browser_use" | "computer_use"
    ) {
        return Err(anyhow!("unsupported session mode: {mode_id}"));
    }
    tracing::info!(session_id, mode_id, "echoagent: set_session_mode send");
    let request = acp::SetSessionModeRequest::new(
        acp::SessionId::new(session_id.to_string()),
        acp::SessionModeId::new(mode_id),
    );
    let _: acp::SetSessionModeResponse = acp_send(request, tx).await.map_err(|error| {
        tracing::error!(session_id, mode_id, error = ?error, "echoagent: set_session_mode FAILED");
        anyhow!("set_session_mode: {error:?}")
    })?;
    Ok(())
}

/// Compatibility adapter for the existing plan-mode toggle API.
pub async fn set_session_mode(tx: &AcpAgentTx, session_id: &str, enabled: bool) -> Result<()> {
    set_session_mode_id(tx, session_id, if enabled { "plan" } else { "default" }).await
}

/// Cancel the in-flight prompt for a session.
///
/// Cancel the active turn and wait until the session actor has applied it.
///
/// ACP models cancel as a notification, but the in-process gateway still owns a
/// response channel. EchoAgent completes that channel only after the session
/// actor has torn down the running turn, giving the desktop a trustworthy UI
/// boundary instead of merely acknowledging channel enqueue.
pub async fn cancel(
    tx: &AcpAgentTx,
    session_id: &str,
    cancel_trigger: &str,
    prompt_id: Option<&str>,
) -> Result<()> {
    use echo_agent_acp::{AcpAgentMessage, AcpArgs};
    let mut meta = serde_json::Map::new();
    meta.insert(
        "cancelTrigger".to_string(),
        serde_json::Value::String(cancel_trigger.to_string()),
    );
    if let Some(prompt_id) = prompt_id {
        meta.insert(
            "promptId".to_string(),
            serde_json::Value::String(prompt_id.to_string()),
        );
    }
    let notif =
        acp::CancelNotification::new(acp::SessionId::new(session_id.to_string())).meta(Some(meta));
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let msg = AcpAgentMessage::Cancel(AcpArgs {
        request: notif,
        response_tx,
    });
    tx.send(msg).map_err(|e| anyhow!("cancel send: {e}"))?;
    let response = tokio::time::timeout(std::time::Duration::from_secs(12), response_rx)
        .await
        .map_err(|_| anyhow!("cancel acknowledgement timed out"))?
        .map_err(|_| anyhow!("cancel acknowledgement channel closed"))?;
    response.map_err(|error| anyhow!("cancel: {error:?}"))?;
    Ok(())
}

/// Rename a session by calling EchoAgent's `echo.agent/session/rename` extension method.
///
/// This is the canonical path (see `echo-agent-runtime/src/extensions/session_admin.rs:60`):
/// it writes `summary.json`'s `generated_title` with `title_is_manual=true`,
/// refreshes the FTS search index, and broadcasts `SessionSummaryGenerated`.
/// **Do not** edit `summary.json` directly — the agent holds the Summary in
/// memory and flushes periodically, so a direct write would be clobbered.
///
/// `cwd` is optional but recommended: EchoAgent uses it to narrow the summary scan
/// when locating the session on disk.
pub async fn rename_session(
    tx: &AcpAgentTx,
    session_id: &str,
    title: &str,
    cwd: Option<&str>,
) -> Result<()> {
    let params = crate::ext::raw_params(&serde_json::json!({
        "sessionId": session_id,
        "title": title,
        // `cwd` null is fine — EchoAgent treats it as "search all sessions".
        "cwd": cwd,
    }));
    let _: acp::ExtResponse =
        crate::ext::call_ext_value(tx, "echo.agent/session/rename", params).await?;
    Ok(())
}

/// Delete a session's persisted history by calling EchoAgent's
/// `echo.agent/session/delete` extension method (session_admin.rs:230).
///
/// Removes the on-disk session directory, drops it from the FTS index, and
/// if the session is live in memory, requests a graceful shutdown. The
/// sidebar's local entry is removed by the frontend on success.
pub async fn delete_session(tx: &AcpAgentTx, session_id: &str, cwd: Option<&str>) -> Result<()> {
    let params = crate::ext::raw_params(&serde_json::json!({
        "sessionId": session_id,
        "cwd": cwd,
    }));
    let _: acp::ExtResponse =
        crate::ext::call_ext_value(tx, "echo.agent/session/delete", params).await?;
    Ok(())
}

/// Fetch a session's context-window snapshot via EchoAgent's `echo.agent/session/info`
/// extension method (session/handlers/session.rs). The response is the
/// camelCase `SessionInfoResponse` — its `context` field carries
/// used/total/usagePct plus the token breakdown (system prompt, tool
/// definitions, messages, skills/MCP categories) shown by the composer pill.
/// Returned as raw JSON: the wire shape is owned by EchoAgent, so we pass it
/// through rather than mirroring the struct in Rust.
pub async fn session_info(tx: &AcpAgentTx, session_id: &str) -> Result<serde_json::Value> {
    let params = crate::ext::raw_params(&serde_json::json!({
        "sessionId": session_id,
    }));
    let resp: serde_json::Value =
        crate::ext::call_ext(tx, "echo.agent/session/info", params).await?;
    // Unlike most echo.agent/* methods, session/info wraps its payload in
    // `ExtMethodResult { result, error? }` (session/result.rs) — and reports
    // "session not live" as success({}) rather than an error. Unwrap the
    // envelope so the frontend always sees the bare SessionInfoResponse.
    if let Some(result) = resp.get("result") {
        return Ok(result.clone());
    }
    Ok(resp)
}

/// Fetch a session's cumulative token usage via EchoAgent's `echo.agent/session/usage`
/// extension method (extensions/usage.rs). The response's `usage` field is a
/// `PromptUsage` whose totals include `inputTokens` and `cachedReadTokens` —
/// the frontend derives the average cache hit rate from those two.
pub async fn session_usage(tx: &AcpAgentTx, session_id: &str) -> Result<serde_json::Value> {
    let params = crate::ext::raw_params(&serde_json::json!({
        "sessionId": session_id,
    }));
    crate::ext::call_ext(tx, "echo.agent/session/usage", params).await
}

#[cfg(test)]
mod tests {
    //! End-to-end smoke test for the embedded EchoAgent runtime (no model call):
    //! spawn the agent thread, run the ACP `initialize` handshake, create a
    //! session, and verify EchoAgent connects to the team MCP server. `new_session`
    //! exercises the full `AgentBuilder::build` path — including the client
    //! side MCP merge (team_mcp.rs) — so an echo-agent-build upgrade that breaks
    //! toolset assembly or the MCP handshake fails here rather than at first
    //! chat in the GUI. Marked `#[ignore]` because it starts a real Runtime
    //! and local MCP service. Run `pnpm test:runtime-smoke`: that entry owns a
    //! fresh data home, empty legacy/compatibility homes and synthetic model
    //! config, cleans inherited credentials, and never sends a model prompt.
    use super::*;

    #[test]
    fn desktop_advertises_interactive_folder_trust_under_capability_meta() {
        let value = serde_json::to_value(desktop_client_capabilities())
            .expect("serialize client capabilities");
        assert_eq!(
            value["_meta"]["echo.agent/folderTrust"]["interactive"],
            true
        );
        assert_eq!(value["terminal"], false);
    }

    #[test]
    fn desktop_initialize_identity_matches_runtime_client_type() {
        let meta = desktop_client_meta();
        assert_eq!(meta["clientType"], "echo_agent_desktop");
        assert_eq!(meta["clientIdentifier"], DESKTOP_CLIENT_IDENTIFIER);
        assert_eq!(meta["clientVersion"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn session_permission_meta_explicitly_covers_every_mode() {
        let ask = permission_mode_meta("ask");
        assert_eq!(ask["yoloMode"], false);
        assert_eq!(ask["autoMode"], false);

        let auto = permission_mode_meta("auto");
        assert_eq!(auto["yoloMode"], false);
        assert_eq!(auto["autoMode"], true);

        let always = permission_mode_meta("always-approve");
        assert_eq!(always["yoloMode"], true);
        assert_eq!(always["autoMode"], false);
    }

    #[test]
    fn injected_team_mcp_server_carries_bearer_authorization() {
        let value = serde_json::to_value(authenticated_team_mcp_server(
            "http://127.0.0.1:1234/mcp".into(),
            "Bearer test-token".into(),
        ))
        .expect("serialize MCP server");
        let encoded = value.to_string();
        assert!(encoded.contains(crate::team_mcp::AUTH_HEADER));
        assert!(encoded.contains("Bearer test-token"));
        assert!(encoded.contains(crate::team_mcp::SESSION_HEADER));
        assert!(encoded.contains("${session_id}"));
    }

    #[test]
    fn injected_automation_mcp_server_is_authenticated_and_session_bound() {
        let value = serde_json::to_value(authenticated_automation_mcp_server(
            "http://127.0.0.1:4321/mcp".into(),
            "Bearer automation-token".into(),
        ))
        .expect("serialize automation MCP server");
        let encoded = value.to_string();
        assert!(encoded.contains(crate::automation::AUTH_HEADER));
        assert!(encoded.contains("Bearer automation-token"));
        assert!(encoded.contains(crate::automation::SESSION_HEADER));
        assert!(encoded.contains("${session_id}"));
    }

    #[tokio::test]
    async fn restored_session_receives_the_same_fresh_local_mcp_seed_as_new_sessions() {
        let registrations = std::array::from_fn(|index| {
            Some((
                format!("http://127.0.0.1:{}/mcp", 62001 + index),
                format!("Bearer current-process-{index}"),
            ))
        });
        let servers = desktop_mcp_servers_from_registration(registrations);
        let new_request = new_session_request(
            Path::new("/workspace"),
            Some("model-a"),
            Some("high"),
            "auto",
            servers.clone(),
        );
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let task = tokio::spawn(async move {
            load_session_with_servers(
                &client.tx,
                "restored-session",
                Path::new("/workspace"),
                "auto",
                servers,
            )
            .await
        });
        let message = agent.rx.recv().await.expect("load request");
        let echo_agent_acp::AcpAgentMessage::LoadSession(arguments) = message else {
            panic!("expected LoadSession request")
        };
        assert_eq!(arguments.request.session_id.0.as_ref(), "restored-session");
        assert_eq!(arguments.request.cwd, Path::new("/workspace"));
        assert_eq!(
            serde_json::to_value(&arguments.request.mcp_servers).unwrap(),
            serde_json::to_value(&new_request.mcp_servers).unwrap()
        );
        assert_eq!(arguments.request.mcp_servers.len(), 4);
        let meta = arguments
            .request
            .meta
            .as_ref()
            .expect("load permission metadata");
        assert_eq!(meta, &permission_mode_meta("auto"));
        let new_meta = new_request.meta.as_ref().unwrap();
        assert_eq!(new_meta["modelId"], "model-a");
        assert_eq!(new_meta["reasoningEffort"], "high");
        for (index, server) in arguments.request.mcp_servers.iter().enumerate() {
            let acp::McpServer::Http(server) = server else {
                panic!("local server must be HTTP")
            };
            assert_eq!(
                server.url,
                format!("http://127.0.0.1:{}/mcp", 62001 + index)
            );
            assert!(server
                .headers
                .iter()
                .any(|header| header.name == "Authorization"
                    && header.value == format!("Bearer current-process-{index}")));
            if server.name == crate::office_mcp::MCP_SERVER_NAME
                || server.name == crate::automation::MCP_SERVER_NAME
            {
                let expected_header = if server.name == crate::office_mcp::MCP_SERVER_NAME {
                    crate::office_mcp::SESSION_HEADER
                } else {
                    crate::automation::SESSION_HEADER
                };
                assert!(server.headers.iter().any(
                    |header| header.name == expected_header && header.value == "${session_id}"
                ));
            }
        }
        arguments
            .response_tx
            .send(Ok(acp::LoadSessionResponse::new()))
            .expect("load response");
        assert!(task
            .await
            .expect("load task")
            .expect("load success")
            .is_none());
    }

    #[test]
    fn unregistered_local_mcp_services_are_not_faked_by_the_shared_seed() {
        assert!(desktop_mcp_servers_from_registration([None, None, None, None]).is_empty());
        let servers = desktop_mcp_servers_from_registration([
            None,
            None,
            Some((
                "http://127.0.0.1:63000/mcp".into(),
                "Bearer office-only".into(),
            )),
            None,
        ]);
        assert_eq!(servers.len(), 1);
        let acp::McpServer::Http(server) = &servers[0] else {
            panic!("expected HTTP")
        };
        assert_eq!(server.name, crate::office_mcp::MCP_SERVER_NAME);
    }

    #[tokio::test]
    async fn set_session_mode_sends_idempotent_standard_acp_request() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let task = tokio::spawn(async move { set_session_mode(&client.tx, "s-1", true).await });
        let message = agent.rx.recv().await.expect("set mode request");
        let echo_agent_acp::AcpAgentMessage::SetSessionMode(arguments) = message else {
            panic!("expected SetSessionMode request")
        };
        assert_eq!(arguments.request.session_id.0.as_ref(), "s-1");
        assert_eq!(arguments.request.mode_id.0.as_ref(), "plan");
        arguments
            .response_tx
            .send(Ok(acp::SetSessionModeResponse::default()))
            .expect("set mode response");
        task.await
            .expect("set mode task")
            .expect("set mode success");
    }

    #[tokio::test]
    async fn set_session_mode_id_preserves_native_ask_mode() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let task =
            tokio::spawn(async move { set_session_mode_id(&client.tx, "s-ask", "ask").await });
        let message = agent.rx.recv().await.expect("set mode request");
        let echo_agent_acp::AcpAgentMessage::SetSessionMode(arguments) = message else {
            panic!("expected SetSessionMode request")
        };
        assert_eq!(arguments.request.session_id.0.as_ref(), "s-ask");
        assert_eq!(arguments.request.mode_id.0.as_ref(), "ask");
        arguments
            .response_tx
            .send(Ok(acp::SetSessionModeResponse::default()))
            .expect("set mode response");
        task.await
            .expect("set mode task")
            .expect("set mode success");
    }

    #[tokio::test]
    async fn prompt_rate_limit_does_not_resubmit_the_whole_user_turn() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let tx = client.tx;
        let task = tokio::spawn(async move {
            prompt_with_attachments(&tx, "session-1", "hello", &[], None, None, false).await
        });

        let message = agent.rx.recv().await.expect("prompt request");
        let echo_agent_acp::AcpAgentMessage::Prompt(arguments) = message else {
            panic!("expected Prompt request");
        };
        arguments
            .response_tx
            .send(Err(
                acp::Error::new(-32003, "rate limited").data(serde_json::json!({ "status": 429 }))
            ))
            .expect("rate-limit response");

        let result = tokio::time::timeout(std::time::Duration::from_millis(250), task)
            .await
            .expect("prompt handler must return instead of scheduling a whole-turn retry")
            .expect("prompt task");
        assert!(result.is_err());
        assert!(
            agent.rx.try_recv().is_err(),
            "prompt must be sent exactly once"
        );
    }

    #[tokio::test]
    async fn prompt_send_now_stamps_atomic_replacement_metadata() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let tx = client.tx;
        let task = tokio::spawn(async move {
            prompt_with_attachments(
                &tx,
                "session-1",
                "2",
                &[],
                Some("2"),
                Some("01990f7d-0000-7000-8000-000000000001"),
                true,
            )
            .await
        });

        let message = agent.rx.recv().await.expect("prompt request");
        let echo_agent_acp::AcpAgentMessage::Prompt(arguments) = message else {
            panic!("expected Prompt request");
        };
        let meta = arguments.request.meta.as_ref().expect("prompt metadata");
        assert_eq!(
            meta.get("promptId").and_then(serde_json::Value::as_str),
            Some("01990f7d-0000-7000-8000-000000000001")
        );
        assert_eq!(
            meta.get("sendNow").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        arguments
            .response_tx
            .send(Ok(acp::PromptResponse::new(acp::StopReason::EndTurn)))
            .expect("prompt response");
        task.await.expect("prompt task").expect("prompt success");
    }

    #[tokio::test]
    async fn model_ids_reads_the_runtime_catalog() {
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let task = tokio::spawn(async move { model_ids(&client.tx).await });

        let message = agent.rx.recv().await.expect("models/list request");
        let echo_agent_acp::AcpAgentMessage::ExtMethod(arguments) = message else {
            panic!("expected ExtMethod");
        };
        assert_eq!(arguments.request.method.as_ref(), "echo.agent/models/list");
        let model_state = acp::SessionModelState::new(
            "model-a",
            vec![
                acp::ModelInfo::new("model-b", "Model B"),
                acp::ModelInfo::new("model-a", "Model A"),
            ],
        );
        arguments
            .response_tx
            .send(Ok(acp::ExtResponse::new(crate::ext::raw_params(
                &serde_json::json!({ "result": model_state }),
            ))))
            .expect("models/list response");

        assert_eq!(
            task.await.expect("model list task").expect("model list"),
            vec!["model-a".to_string(), "model-b".to_string()]
        );
    }

    #[test]
    fn document_attachment_keeps_model_reference_and_replay_metadata() {
        let attachments = vec!["/tmp/AI数据集平台-数据回流方案.docx".to_string()];
        let blocks = build_prompt_blocks(
            "<system-reminder>hidden</system-reminder>\n\n请优化文档",
            &attachments,
            Some("请优化文档"),
        )
        .expect("document attachment should not require reading the file");

        let acp::ContentBlock::Text(text) = &blocks[0] else {
            panic!("first prompt block must be text");
        };
        assert!(text.text.contains("- @/tmp/AI数据集平台-数据回流方案.docx"));
        let meta = text.meta.as_ref().expect("replay metadata");
        assert_eq!(
            meta.get("displayText"),
            Some(&serde_json::json!("请优化文档"))
        );
        assert_eq!(
            meta.get("echoAgentAttachments"),
            Some(&serde_json::json!(attachments)),
        );
    }

    #[test]
    fn image_attachment_is_bounded_before_encoding() {
        let dir = tempfile::tempdir().expect("temp dir");
        let image = dir.path().join("large.png");
        let file = std::fs::File::create(&image).expect("create image");
        file.set_len(MAX_IMAGE_ATTACHMENT_BYTES + 1)
            .expect("make sparse image");
        let error = build_prompt_blocks("inspect", &[image.to_string_lossy().into_owned()], None)
            .expect_err("oversized image must be rejected");
        assert!(error.to_string().contains("exceeds 20MB"));
    }

    #[cfg(unix)]
    #[test]
    fn image_attachment_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("target.png");
        let link = dir.path().join("link.png");
        std::fs::write(&target, b"image").expect("write image");
        symlink(&target, &link).expect("create link");
        let error = build_prompt_blocks("inspect", &[link.to_string_lossy().into_owned()], None)
            .expect_err("image symlink must be rejected");
        assert!(error.to_string().contains("regular file"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "explicit isolated home and stalled loopback model; run scripts/test-memory-shutdown.mjs"]
    async fn memory_shutdown_checkpoints_and_cancels_a_stalled_model() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        assert_eq!(
            std::env::var("ECHO_AGENT_REVIEW_ISOLATED").as_deref(),
            Ok("1")
        );
        let home = crate::paths::redirected_data_home().expect("isolated data home");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config_path = home.join("config.toml");
        let config = std::fs::read_to_string(&config_path).unwrap();
        assert!(config.contains("synthetic-no-model-calls"));
        std::fs::write(
            &config_path,
            config.replace(
                "http://127.0.0.1:9/v1",
                &format!("http://{}/v1", listener.local_addr().unwrap()),
            ),
        )
        .unwrap();
        let (started, request_started) = tokio::sync::oneshot::channel();
        let stalled = tokio::spawn(async move {
            let mut extraction_attempts = 0;
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut bytes = [0u8; 4096];
                loop {
                    let n = socket.read(&mut bytes).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&bytes[..n]);
                    assert!(request.len() < 1024 * 1024);
                    if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                if String::from_utf8_lossy(&request).contains("Now write the memory summary") {
                    extraction_attempts += 1;
                    if extraction_attempts == 1 {
                        // A completed model response with invalid extraction data
                        // fails the job without provoking HTTP/provider retries.
                        let body = concat!(
                            "data: {\"id\":\"review\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"mock\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"invalid memory extraction\"},\"finish_reason\":null}]}\n\n",
                            "data: {\"id\":\"review\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"mock\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                            "data: [DONE]\n\n"
                        );
                        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                        continue;
                    }
                    let _ = started.send(());
                    // Stall the idle retry: this session received no new chat.
                    while socket.read(&mut bytes).await.unwrap_or(0) > 0 {}
                    break;
                }
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"data\":[]}").await.unwrap();
            }
        });
        // Memory skips ephemeral CWDs. Keep only this synthetic workspace in
        // the repository's ignored Cargo target; all agent data uses TempDir.
        let target = std::env::current_dir().unwrap().join("target");
        std::fs::create_dir_all(&target).unwrap();
        let workspace = tempfile::Builder::new()
            .prefix("memory-shutdown-")
            .tempdir_in(target)
            .unwrap();
        let cwd = workspace.path();
        crate::team_mcp::serve();
        let launch = || {
            let cwd = cwd.to_path_buf();
            tokio::task::spawn_blocking(move || spawn_agent_runtime(cwd))
        };
        let mut first = launch().await.unwrap().unwrap();
        let init = initialize(&first.tx).await.unwrap();
        let sid =
            new_session_with_options(&first.tx, cwd, init.default_model_id.as_deref(), None, None)
                .await
                .unwrap();
        first.cancel.cancel();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            first.shutdown_complete.wait_for(|done| *done),
        )
        .await
        .unwrap()
        .unwrap();
        if let Some(thread) = first.thread.take() {
            tokio::task::spawn_blocking(move || thread.join().unwrap().unwrap())
                .await
                .unwrap();
        }
        let storage = echo_agent_memory::MemoryStorage::new(cwd, None);
        assert!(!storage.is_ephemeral());
        let job = echo_agent_runtime::session::memory::pending::PendingMemory {
            source_id: sid.clone(),
            history: vec![echo_agent_runtime::sampling::ConversationItem::user(
                "Remember this synthetic project preference for the shutdown regression.",
            )],
            conversation_len: 1,
            queued_at: chrono::Utc::now().timestamp(),
        };
        echo_agent_runtime::session::memory::pending::checkpoint(&storage, &job).unwrap();
        let mut second = launch().await.unwrap().unwrap();
        initialize(&second.tx).await.unwrap();
        load_session(&second.tx, &sid, cwd, "default")
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), request_started)
            .await
            .expect("failed recovery was not retried in an unchanged session")
            .unwrap();
        let started = std::time::Instant::now();
        second.cancel.cancel();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            second.shutdown_complete.wait_for(|done| *done),
        )
        .await
        .expect("shutdown waited for the stalled memory model")
        .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        if let Some(thread) = second.thread.take() {
            tokio::task::spawn_blocking(move || thread.join().unwrap().unwrap())
                .await
                .unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), stalled)
            .await
            .expect("memory HTTP request survived Runtime shutdown")
            .unwrap();
        assert_eq!(
            std::fs::read_dir(storage.workspace_dir().join("pending"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            echo_agent_memory::records::activity(&storage).outcome,
            "queued"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires an explicit isolated ECHO_AGENT_HOME and local MCP port (~10s)"]
    async fn spawn_smoke_spawn_initialize_new_session() {
        fn receive_notifications(mut rx: AcpClientRx) -> tokio::task::JoinHandle<()> {
            tokio::spawn(async move {
                while let Some(message) = rx.recv().await {
                    match message {
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        echo_agent_acp::AcpClientMessage::ExtNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        // This acceptance never performs inference or requests
                        // client-side tools. Dropping an unexpected request also
                        // closes its response instead of hanging the test.
                        unexpected => panic!("unexpected no-model client request: {unexpected:?}"),
                    }
                }
            })
        }
        assert!(
            crate::paths::redirected_data_home().is_some(),
            "Set ECHO_AGENT_HOME to an isolated directory before running this smoke test"
        );
        assert_eq!(
            std::env::var("ECHO_AGENT_REVIEW_ISOLATED").as_deref(),
            Ok("1"),
            "Use pnpm test:runtime-smoke to prevent discovery of real HOME configuration"
        );
        let workspace = tempfile::tempdir().expect("isolated smoke workspace");
        let cwd = workspace.path().to_path_buf();

        // 0. 先起 team MCP server —— new_session 会把它注入 EchoAgent（这是去
        //    补丁化后的工具注入路径，替代原 patch 02）。
        crate::team_mcp::serve();

        // 1. Spawn: config load → resolve_runtime_fields → bootstrap →
        //    MvpAgent thread.
        let handle = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            tokio::task::spawn_blocking({
                let cwd = cwd.clone();
                move || spawn_agent_runtime(cwd)
            }),
        )
        .await
        .expect("spawn_agent_runtime timed out (60s)")
        .expect("spawn task join failed")
        .expect("spawn_agent_runtime failed");
        // Real clients acknowledge replay notifications before LoadSession
        // returns. The no-UI acceptance must provide the same receiver.
        let receiver = receive_notifications(handle.rx);

        // 2. ACP initialize handshake: protocol version + auth methods.
        let init = tokio::time::timeout(std::time::Duration::from_secs(30), initialize(&handle.tx))
            .await
            .expect("initialize timed out (30s)")
            .expect("initialize failed");
        assert!(init.ok, "initialize reported not-ok");
        let preferred = crate::permission_config::read_defaults().default_model;
        let runtime_ids = model_ids(&handle.tx).await.expect("Runtime model catalog");
        let (configured, _) = crate::providers::usable_model_ids();
        assert_eq!(
            init.default_model_id,
            crate::commands::effective_configured_model_id(
                &configured,
                &runtime_ids,
                (!preferred.trim().is_empty()).then_some(preferred.as_str())
            ),
            "Runtime uses the local Auto hint without persisting it as a preference"
        );
        // Check the effective Runtime catalog, including IDs containing spaces.
        // A settings-only update can otherwise display models the actor rejects.
        let mut builtin_ids = runtime_ids
            .into_iter()
            .filter(|id| id.starts_with(crate::providers::BUILTIN_MODEL_PREFIX))
            .collect::<Vec<_>>();
        builtin_ids.sort();
        assert_eq!(
            builtin_ids,
            [
                "echoagent-ojlab/Echo Brook",
                "echoagent-ojlab/Echo Cedar",
                "echoagent-ojlab/Echo Grove",
                "echoagent-ojlab/Echo Reed",
                "echoagent-ojlab/Echo Willow",
            ]
        );
        // 3. New session: runs AgentBuilder::build + merges the client-side
        //    MCP server entry (team tools live as echoagent__* now).
        let session_id = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            new_session_with_options(
                &handle.tx,
                &cwd,
                init.default_model_id.as_deref(),
                None,
                None,
            ),
        )
        .await
        .expect("new_session timed out (60s)")
        .expect("new_session failed");
        assert!(!session_id.is_empty(), "empty session id");
        for model_id in &builtin_ids {
            set_session_model(&handle.tx, &session_id, model_id)
                .await
                .expect("switch to built-in chat model without inference");
        }
        set_session_model(
            &handle.tx,
            &session_id,
            crate::providers::BUILTIN_DEFAULT_MODEL_ID,
        )
        .await
        .expect("select Echo Reed");
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                load_session(&handle.tx, &session_id, &cwd, "default"),
            )
            .await
            .expect("reconnect replay timed out")
            .unwrap()
            .as_deref(),
            Some(crate::providers::BUILTIN_DEFAULT_MODEL_ID),
        );
        if let Some(default_model) = init.default_model_id.as_deref() {
            set_session_model(&handle.tx, &session_id, default_model)
                .await
                .expect("restore isolated test default");
        }

        // 4. EchoAgent 的 MCP client 必须真的连上 team MCP server（initialize
        //    握手完成）。轮询最多 10s —— EchoAgent 异步启动 server 连接。
        let mut connected = false;
        for _ in 0..100 {
            if crate::team_mcp::client_connected() {
                connected = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            connected,
            "EchoAgent never connected to the team MCP server — tools would be missing"
        );

        // 5. Exercise the actual ACP actor without an inference request or
        // desktop input. The regular tool inventory must return even for plan,
        // whose handler exits before the ordinary mode prompt rewrite.
        let baseline = session_info(&handle.tx, &session_id)
            .await
            .expect("regular tool inventory");
        let regular_tools = baseline["context"]["toolDefinitionsCount"]
            .as_u64()
            .expect("tool count");
        assert!(
            regular_tools > 3,
            "regular mode must provide more than discovery/MCP tools"
        );
        for automation in ["computer_use", "browser_use"] {
            for target in ["plan", "ask", "default"] {
                set_session_mode_id(&handle.tx, &session_id, automation)
                    .await
                    .expect("enter automation");
                let restricted = session_info(&handle.tx, &session_id)
                    .await
                    .expect("restricted inventory");
                let restricted_count = restricted["context"]["toolDefinitionsCount"]
                    .as_u64()
                    .expect("automation tool count");
                assert!(
                    restricted_count < regular_tools,
                    "automation must remove regular tools: {restricted}"
                );
                set_session_mode_id(&handle.tx, &session_id, target)
                    .await
                    .expect("leave automation");
                let restored = session_info(&handle.tx, &session_id)
                    .await
                    .expect("restored inventory");
                assert_eq!(
                    restored["context"]["toolDefinitionsCount"].as_u64(),
                    Some(regular_tools),
                    "{automation} -> {target}: {restored}"
                );
                assert_ne!(restored["agentName"], automation.replace('_', "-"));
            }
        }

        // 6. Clean shutdown: cancel the agent thread and join it.
        handle.cancel.cancel();
        if let Some(thread) = handle.thread {
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        receiver.abort();
        if let Err(error) = receiver.await {
            assert!(error.is_cancelled(), "client receiver failed: {error}");
        }

        // Restore a genuine persisted actor from an old built-in selection.
        // Do this only after shutdown, exactly as an application upgrade does.
        let root = crate::paths::echo_agent_home_dir().join("sessions");
        let summary_path = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .map(|entry| entry.path().join(&session_id).join("summary.json"))
            .find(|path| path.is_file())
            .expect("persisted smoke summary");
        let mut summary: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
        summary["current_model_id"] = serde_json::json!("echoagent-ojlab/chat-qwen");
        crate::paths::write_private_file(&summary_path, &serde_json::to_vec(&summary).unwrap())
            .unwrap();
        let restored = tokio::task::spawn_blocking({
            let cwd = cwd.clone();
            move || spawn_agent_runtime(cwd)
        })
        .await
        .unwrap()
        .unwrap();
        let receiver = receive_notifications(restored.rx);
        initialize(&restored.tx).await.unwrap();
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                load_session(&restored.tx, &session_id, &cwd, "default"),
            )
            .await
            .expect("upgraded task replay timed out")
            .unwrap()
            .as_deref(),
            Some(crate::providers::BUILTIN_DEFAULT_MODEL_ID),
            "upgrade restores the selected built-in, never an unrelated provider"
        );
        restored.cancel.cancel();
        if let Some(thread) = restored.thread {
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        receiver.abort();
        if let Err(error) = receiver.await {
            assert!(error.is_cancelled(), "client receiver failed: {error}");
        }
    }
}

/// Pure unit tests for the BYOK endpoint-isolation rule. Unlike the smoke test
/// above these spawn nothing and always run.
#[cfg(test)]
mod byok_isolation_tests {
    use super::{pick_byok_base_url, usable_base_url};

    #[test]
    fn accepts_http_and_https_only() {
        assert_eq!(
            usable_base_url("https://api.deepseek.com"),
            Some("https://api.deepseek.com".to_string())
        );
        assert_eq!(
            usable_base_url("http://127.0.0.1:11434/v1"),
            Some("http://127.0.0.1:11434/v1".to_string())
        );
        for rejected in [
            "",
            "   ",
            "not-a-url",
            "ftp://example.com",
            "file:///etc/hosts",
        ] {
            assert_eq!(
                usable_base_url(rejected),
                None,
                "{rejected:?} must be rejected"
            );
        }
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            usable_base_url("  https://api.openai.com/v1  "),
            Some("https://api.openai.com/v1".to_string())
        );
    }

    /// The regression this whole change exists for: a connection saved by the
    /// settings UI keeps `base_url` on `[model_providers.<id>]`, so the model
    /// entry carries none. Scanning models alone found nothing and the bundled
    /// upstream catalog leaked into the picker.
    #[test]
    fn provider_base_url_is_found_when_models_carry_none() {
        assert_eq!(
            pick_byok_base_url(["https://api.deepseek.com"], std::iter::empty()),
            Some("https://api.deepseek.com".to_string())
        );
    }

    #[test]
    fn legacy_per_model_base_url_still_works() {
        assert_eq!(
            pick_byok_base_url(std::iter::empty(), ["https://legacy.example.com/v1"]),
            Some("https://legacy.example.com/v1".to_string())
        );
    }

    #[test]
    fn providers_win_over_legacy_model_entries() {
        assert_eq!(
            pick_byok_base_url(
                ["https://provider.example.com"],
                ["https://legacy.example.com"]
            ),
            Some("https://provider.example.com".to_string())
        );
    }

    #[test]
    fn skips_unusable_entries_before_settling_on_a_valid_one() {
        assert_eq!(
            pick_byok_base_url(
                ["", "not-a-url", "https://third.example.com"],
                std::iter::empty()
            ),
            Some("https://third.example.com".to_string())
        );
    }

    #[test]
    fn falls_back_to_models_when_every_provider_url_is_unusable() {
        assert_eq!(
            pick_byok_base_url(["", "ftp://nope"], ["https://model.example.com"]),
            Some("https://model.example.com".to_string())
        );
    }

    /// No configured connection yet (first launch): nothing to isolate against.
    /// This is the one case where the bundled catalog can still load, which is
    /// why the branded-id filters downstream are not optional.
    #[test]
    fn no_configuration_yields_none() {
        assert_eq!(
            pick_byok_base_url(std::iter::empty(), std::iter::empty()),
            None
        );
    }
}

#[cfg(test)]
mod memory_retrieval_tests {
    use super::*;

    #[test]
    fn retrieval_always_uses_builtin_service() {
        let raw = serde_json::from_value(serde_json::json!({})).unwrap();
        let mut config = AgentConfig::new_from_toml_cfg(&raw).unwrap();
        config.memory_config = Some(Default::default());
        let memory = config.memory_config.as_mut().unwrap();
        memory.embedding.provider = "api".into();
        memory.embedding.endpoint = Some("https://custom.example/v1/embeddings".into());
        memory.embedding.api_key = Some("preserve-key".into());
        memory.embedding.model = Some("custom-model".into());
        memory.search.reranker.enabled = true;
        memory.search.reranker.endpoint = Some("https://custom.example/rerank".into());
        configure_memory_retrieval(&mut config);
        let memory = config.memory_config.as_ref().unwrap();
        assert_eq!(memory.embedding.provider, "api");
        assert_eq!(memory.embedding.api_key.as_deref(), Some(""));
        assert_eq!(
            memory.embedding.endpoint.as_deref(),
            Some(MEMORY_EMBEDDING_ENDPOINT)
        );
        assert_eq!(
            memory.embedding.model.as_deref(),
            Some(MEMORY_EMBEDDING_MODEL)
        );
        assert_eq!(memory.embedding.dimensions, MEMORY_EMBEDDING_DIMENSIONS);
        assert!(memory.search.reranker.enabled);
        assert_eq!(
            memory.search.reranker.endpoint.as_deref(),
            Some(MEMORY_RERANK_ENDPOINT)
        );
        assert_eq!(
            memory.search.reranker.model.as_deref(),
            Some(MEMORY_RERANK_MODEL)
        );
    }

    #[tokio::test]
    async fn builtin_embedding_does_not_send_the_chat_model_key() {
        use echo_agent_memory::embedding::{ApiEmbeddingProvider, EmbeddingProvider};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let raw = serde_json::from_value(serde_json::json!({})).unwrap();
        let mut config = AgentConfig::new_from_toml_cfg(&raw).unwrap();
        config.memory_config = Some(Default::default());
        configure_memory_retrieval(&mut config);
        let embedding = &config.memory_config.as_ref().unwrap().embedding;
        let embedding_key = embedding
            .api_key
            .clone()
            .or_else(|| Some("model-secret".to_owned()))
            .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/embeddings", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|part| part == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            String::from_utf8(request).unwrap()
        });

        let provider =
            ApiEmbeddingProvider::from_session(embedding, endpoint, embedding_key).unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            provider.embed_batch(&["private memory"]),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        assert!(!request.contains("model-secret"));
    }
}
