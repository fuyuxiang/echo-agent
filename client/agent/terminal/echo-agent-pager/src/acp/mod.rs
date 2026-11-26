//! ACP (Agent Communication Protocol) connection management.
//!
//! Handles spawning the agent process, initializing the protocol,
//! authenticating, and providing the channel for communication.

pub mod leader_bridge;
pub mod meta;
pub mod model_state;
pub mod spawn;
pub mod tracker;
mod version_mismatch;

pub(crate) use version_mismatch::{is_version_mismatch_banner, version_mismatch_banner};

/// Ext methods that carry a session-scoped update and may stamp `isReplay`.
/// Shared by TUI/headless dispatch and the session-load ACP barrier so a new
/// method cannot be handled in one path and classified `Unrelated` in the other.
pub(crate) fn is_session_update_ext_method(method: &str) -> bool {
    matches!(
        method,
        "echo.agent/session_notification" | "echo.agent/session/update"
    )
}

use echo_agent_telemetry::process_info::{
    Entrypoint, Interactivity, LeaderMode, ProcessIdentity, set_identity,
};
use echo_agent_telemetry::startup;
pub use echo_agent_telemetry::startup::{
    AgentKind, Owner, StartupOutcome, StartupPhase, StartupTimer,
};

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::client_identity::{HEADLESS_CLIENT_TYPE, PAGER_CLIENT_TYPE, PAGER_CLIENT_VERSION};
use agent_client_protocol as acp;
use echo_agent_acp::{AcpAgentTx, AcpClientRx, acp_send};
use echo_agent_runtime::agent::auth_method::AuthMethodKind;
use echo_agent_runtime::agent::config::Config as AgentConfig;
use echo_agent_runtime::sampling::types::ReasoningEffort;

pub use model_state::ModelState;

/// Construct a `METHOD_NOT_FOUND` error for `WaitForTerminalExit`.
///
/// Both the interactive pager and headless mode reject this ACP method
/// (the adapter falls back to polling). Centralised here so the error
/// code and message format stay in sync.
pub(crate) fn wait_for_exit_not_supported(context: &str) -> acp::Error {
    acp::Error::new(
        acp::ErrorCode::MethodNotFound.into(),
        format!("{context} does not handle WaitForTerminalExit"),
    )
}

/// Initial auth mode hint from the agent's auth method metadata.
///
/// Determined at startup from `AuthMethod.meta.external_provider`.
/// Used by the welcome screen to decide whether to show a browser-opening
/// message or a manual token paste input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStartMode {
    /// Mode not yet known (will be resolved after AuthenticateRequest).
    Pending,
    /// External provider (meta.external_provider == true) — browser opens automatically.
    Command,
}

/// Result of connecting to an agent.
pub struct AcpConnection {
    /// Send requests to the agent.
    pub tx: AcpAgentTx,
    /// Receive notifications from the agent.
    pub rx: AcpClientRx,
    /// Available models and current selection.
    pub models: ModelState,
    /// Whether the agent is a echo-agent-shell instance.
    pub is_echo_agent_shell: bool,
    /// Auth methods advertised by the agent.
    pub auth_methods: Vec<acp::AuthMethod>,
    /// Cancellation token to stop the agent.
    pub cancel: CancellationToken,
    /// In-process agent worker thread (`connect` only). Join after cancel so
    /// session actors can flush SessionEnd hooks. `None` in leader mode.
    pub agent_thread: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    /// ACP-advertised slash commands parsed from `InitializeResponse.meta.availableCommands`.
    /// Seeded into every new `AgentSession` so autocomplete has shell builtins
    /// and skills immediately, before any `AvailableCommandsUpdate` arrives.
    pub available_commands: Vec<acp::AvailableCommand>,
    // NOTE: Startup announcements from InitializeResponse.meta are not yet supported.
    // Requires shell to include announcements in initialize metadata.
    // When available, add field: startup_announcements: Option<Vec<echo_agent_announcements::RemoteAnnouncement>>
    /// Whether interactive login is required (deferred auth for `cloud.echo-agent.invalid`).
    pub needs_login: bool,
    /// Login button label from `AuthMethod.name` (e.g., "cloud.echo-agent.invalid", "Acme Corp").
    pub login_label: Option<String>,
    /// The auth method ID to use for login (copied from the first advertised method).
    pub login_method_id: Option<acp::AuthMethodId>,
    /// Initial auth mode hint (Command vs Pending) from method metadata.
    pub auth_start_mode: AuthStartMode,
    /// Auth response metadata from eager authentication (cached token / API key).
    /// Contains `team_name`, etc. `None` when interactive login is required.
    pub auth_meta: Option<serde_json::Value>,
    /// Leader connection status. `Some` only when connected via leader.
    pub leader_status_rx: Option<tokio::sync::watch::Receiver<leader_bridge::ConnectionStatus>>,
    /// Whether cancel-rewind is enabled (resolved by shell from config layers).
    pub cancel_rewind_enabled: bool,
    /// Whether the session-recap feature is rolled out for this connection,
    /// resolved by the shell (remote settings / config / env; default OFF) and
    /// advertised in `InitializeResponse.meta.sessionRecap`. The client gates
    /// its automatic away-recap poll and the manual `/recap` on this so a
    /// disabled feature produces zero `echo.agent/recap` traffic. Defaults to `false`
    /// when absent (e.g. an older shell that predates the feature).
    pub session_recap_available: bool,
    /// Shell-side feedback trace-offer eligibility (see `feedbackTraceOffer`).
    pub feedback_trace_offer: bool,
    /// `AuthManager` for pager-side authenticated channels (voice STT/TTS).
    ///
    /// In-process mode shares the agent's instance (single token cache); leader
    /// mode builds a dedicated one off the same local `auth.json`. Either way it
    /// resolves a fresh bearer per request via the refresh chain.
    pub auth_manager: std::sync::Arc<echo_agent_runtime::auth::AuthManager>,
}

/// CLI flags that affect agent configuration, threaded from PagerArgs.
#[derive(Debug, Clone, Default)]
pub struct ConnectFlags {
    pub subagents: bool,
    /// CLI memory override set by a legacy compatibility flag.
    pub memory_enabled_override: Option<bool>,
    /// Original compatibility flag spelling for leader-mode warnings.
    pub memory_override_flag: Option<&'static str>,
    pub disable_web_search: bool,
    /// Session-scoped `--todo-gate` override. Forces
    /// `ReminderPolicy.todo_gate.enabled = true` for this session.
    pub todo_gate: bool,
    /// Session-scoped `--laziness-debug-log <path>` override. When set,
    /// the Layer-3 classifier fires after every turn regardless of the
    /// per-model enable gate, and the full outcome is appended to the
    /// given JSONL file. Observation-only (no nudges). Prototype/eval
    /// use only; not persisted to config.toml.
    pub laziness_debug_log: Option<std::path::PathBuf>,
    /// Storage mode override.
    pub storage_mode: Option<String>,
    /// Whether this client will draw a status row, advertised as
    /// `echo.agent/statusLine` so the agent can skip an unpainted payload.
    pub status_line: bool,
    /// Client identifier for ACP Initialize metadata.
    pub client_identifier: Option<String>,
    /// Hunk tracker mode for ACP Initialize capabilities.
    pub hunk_tracker_mode: Option<String>,
    /// Terminal capability in ACP Initialize.
    pub terminal: bool,
    /// Filesystem read capability in ACP Initialize.
    pub fs_read: bool,
    /// Filesystem write capability in ACP Initialize.
    pub fs_write: bool,
    /// Installer field for config.toml.
    pub installer: Option<String>,
    /// Remote settings from early prefetch (used for memory config resolution).
    pub remote_settings: Option<echo_agent_runtime::util::config::RemoteSettings>,
    /// Override the entire system prompt.
    pub system_prompt_override: Option<String>,
    /// Extra rules appended to the system prompt (from `--rules`).
    pub rules: Option<String>,
    /// Override reasoning effort for all models.
    pub reasoning_effort_override: Option<ReasoningEffort>,
    /// CLI permission rules from --allow / --deny flags.
    /// Not supported in leader mode (agent config is set at leader startup).
    pub permission_rules: Vec<echo_agent_workspace::permission::types::PermissionRule>,
    /// Seed agent sessions with always-approve (YOLO) permission mode.
    pub default_yolo_mode: bool,
    /// Seed agent sessions with auto (classifier) permission mode.
    /// Ignored when `default_yolo_mode` is true.
    pub default_auto_mode: bool,
}

/// Connect to an agent: spawn, initialize, authenticate.
pub async fn connect(cancel: &CancellationToken, flags: ConnectFlags) -> Result<AcpConnection> {
    startup::enter(StartupPhase::ConfigLoad);
    let raw_config = echo_agent_runtime::config::load_effective_config()
        .map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?;
    let mut agent_config = AgentConfig::new_from_toml_cfg(&raw_config)
        .map_err(|e| anyhow::anyhow!("Failed to create agent config: {}", e))?;

    agent_config.resolve_runtime_fields(
        &echo_agent_runtime::agent::config::RuntimeResolutionContext {
            raw_config: &raw_config,
            remote_settings: flags.remote_settings.as_ref(),
            is_headless: false,
            cli_subagents: Some(flags.subagents),
            cli_web_search_model: None,
            cli_session_summary_model: None,
            memory_enabled_override: flags.memory_enabled_override,
            disable_web_search: flags.disable_web_search,
            todo_gate: flags.todo_gate,
            laziness_debug_log: flags.laziness_debug_log.as_deref(),
            storage_mode: flags.storage_mode.as_deref(),
        },
    );

    // Permission mode seeds for every session this agent creates (CLI / config).
    agent_config.default_yolo_mode = flags.default_yolo_mode;
    agent_config.default_auto_mode = flags.default_auto_mode && !flags.default_yolo_mode;

    if let Some(effort) = flags.reasoning_effort_override {
        agent_config.reasoning_effort_override = Some(effort);
    }
    // Agent connect intentionally leaves hub URL unset; provider hub is
    // WorkspaceStartArgs only.

    if !flags.permission_rules.is_empty() {
        agent_config.cli_agent_overrides.permission_rules = flags.permission_rules.clone();
    }

    apply_config_writes(&flags);

    // Stamped before the spawn/handshake can fail: a failed connect still
    // reports under this identity.
    set_identity(ProcessIdentity {
        entrypoint: Entrypoint::Embedded,
        leader: LeaderMode::Standalone,
        interactivity: Interactivity::Interactive,
    });

    let memory_config = agent_config.memory_config.clone();
    let spawned = spawn::spawn_echo_agent_shell(agent_config, cancel, memory_config).await?;
    let auth_manager = spawned.auth_manager.clone();
    let (tx, rx) = (spawned.channel.tx, spawned.channel.rx);

    startup::enter(StartupPhase::AcpInitialize);
    let (
        models,
        is_echo_agent_shell,
        auth_methods,
        default_auth_method_id,
        available_commands,
        cancel_rewind_enabled,
        session_recap_available,
        feedback_trace_offer,
    ) = initialize(&tx, &flags).await?;

    // Determine whether interactive login is needed.
    let (needs_login, login_label, login_method_id, auth_start_mode) =
        startup_auth_metadata(&auth_methods);

    startup::enter(StartupPhase::EagerAuth);
    let (needs_login, login_label, login_method_id, auth_start_mode, auth_meta) =
        bounded_eager_auth(
            &tx,
            &auth_methods,
            default_auth_method_id.as_ref(),
            needs_login,
            login_label,
            login_method_id,
            auth_start_mode,
        )
        .await;

    Ok(AcpConnection {
        tx,
        rx,
        models,
        is_echo_agent_shell,
        auth_methods,
        cancel: spawned.cancel,
        agent_thread: Some(spawned.thread_handle),
        available_commands,
        needs_login,
        login_label,
        login_method_id,
        auth_start_mode,
        auth_meta,
        leader_status_rx: None,
        cancel_rewind_enabled,
        session_recap_available,
        feedback_trace_offer,
        auth_manager,
    })
}

/// Connect to a leader process and return an `AcpConnection`.
///
/// The leader provides the ACP transport via IPC (raw JSON strings over a
/// Unix socket). This function bridges that transport into the same typed
/// `(AcpAgentTx, AcpClientRx)` pair that `connect()` produces, then runs
/// the standard initialize + authenticate sequence.
pub async fn connect_via_leader(
    cancel: &CancellationToken,
    flags: ConnectFlags,
    raw_config: &toml::Value,
) -> Result<AcpConnection> {
    use echo_agent_runtime::leader::{
        ClientCapabilities, ClientMode, LeaderReconnector, ReconnectPolicy, connect_or_spawn,
    };

    // These flags are baked into the agent at startup.  In leader mode the
    // agent is already running, so per-client overrides cannot be applied.
    warn_unsupported_leader_flags(&flags);

    apply_config_writes(&flags);

    startup::enter(StartupPhase::ConfigLoad);
    // The leader path never runs the managed-policy sync in this process.
    startup::set_auth_mode(echo_agent_runtime::managed_config::classify_auth_mode());
    let mut agent_config = AgentConfig::new_from_toml_cfg(raw_config)
        .map_err(|e| anyhow::anyhow!("Failed to create agent config: {e}"))?;
    // resolve_telemetry_mode reads remote_settings.
    agent_config.remote_settings = flags.remote_settings.clone();

    let client_type = flags
        .client_identifier
        .as_deref()
        .unwrap_or(HEADLESS_CLIENT_TYPE);
    let env_urls =
        echo_agent_runtime::leader::LeaderEnvUrls::from(&agent_config.echo_agent_com_config);
    let capabilities = ClientCapabilities {
        // Leader agent is pre-running; seed modes via capabilities → session meta.
        yolo_mode: flags.default_yolo_mode,
        auto_mode: flags.default_auto_mode && !flags.default_yolo_mode,
        default_model: agent_config.models.default.clone(),
        client_version: Some(PAGER_CLIENT_VERSION.to_string()),
        code_nav_enabled: false,
        terminal: flags.terminal,
        fs_read: flags.fs_read,
        fs_write: flags.fs_write,
        status_line: flags.status_line,
    };

    startup::enter(StartupPhase::LeaderConnect);
    let conn = connect_or_spawn(
        client_type,
        ClientMode::Stdio,
        &env_urls,
        capabilities.clone(),
    )
    .await?;

    let (status_tx, status_rx) = LeaderReconnector::status_channel();
    let reconnector = LeaderReconnector::new(
        client_type,
        ClientMode::Stdio,
        env_urls,
        capabilities,
        status_tx,
    );
    let bridge = leader_bridge::bridge_leader_connection(
        conn,
        cancel.clone(),
        Some(reconnector),
        ReconnectPolicy::unbounded(),
    )?;
    let (tx, rx) = (bridge.channel.tx, bridge.channel.rx);

    startup::enter(StartupPhase::AcpInitialize);
    let (
        models,
        is_echo_agent_shell,
        auth_methods,
        default_auth_method_id,
        available_commands,
        cancel_rewind_enabled,
        session_recap_available,
        feedback_trace_offer,
    ) = initialize(&tx, &flags).await?;

    let (needs_login, login_label, login_method_id, auth_start_mode) =
        startup_auth_metadata(&auth_methods);

    startup::enter(StartupPhase::EagerAuth);
    let (needs_login, login_label, login_method_id, auth_start_mode, auth_meta) =
        bounded_eager_auth(
            &tx,
            &auth_methods,
            default_auth_method_id.as_ref(),
            needs_login,
            login_label,
            login_method_id,
            auth_start_mode,
        )
        .await;

    // Leader mode runs the agent in a separate process, so there's no shared
    // in-process `AuthManager`. Build a dedicated *non-refreshing* one over the
    // same `auth.json`: skip `configure_refresher` so only the agent rotates the
    // token. A second refresher would race rotation and could clear credentials
    // on failure. This one just reads the valid token, and on expiry adopts the
    // agent's disk-rotated token under the file lock (`try_adopt_disk_token`).
    let auth_manager = std::sync::Arc::new(echo_agent_runtime::auth::AuthManager::new(
        &echo_agent_runtime::util::echo_agent_home::echo_agent_home(),
        agent_config.echo_agent_com_config.clone(),
    ));

    // Leader has no in-process agent; init this process's product telemetry client.
    set_identity(ProcessIdentity {
        entrypoint: Entrypoint::Pager,
        leader: LeaderMode::Attached,
        interactivity: Interactivity::Interactive,
    });
    echo_agent_runtime::agent::init::update_telemetry_config(&agent_config, &auth_manager);

    Ok(AcpConnection {
        tx,
        rx,
        models,
        is_echo_agent_shell,
        auth_methods,
        cancel: bridge.cancel,
        agent_thread: None,
        available_commands,
        needs_login,
        login_label,
        login_method_id,
        auth_start_mode,
        auth_meta,
        leader_status_rx: Some(status_rx),
        cancel_rewind_enabled,
        session_recap_available,
        feedback_trace_offer,
        auth_manager,
    })
}

/// Warn about flags that only take effect in direct-spawn mode.
///
/// In leader mode the agent is already running; these per-agent settings
/// cannot be changed after the fact.
fn warn_unsupported_leader_flags(flags: &ConnectFlags) {
    // eprintln rather than tracing::warn because this runs before pager
    // TUI tracing is initialised — tracing output would be silently dropped.
    for flag in unsupported_leader_flags(flags) {
        eprintln!(
            "warning: {flag} has no effect in leader mode \
             (agent config is set at leader startup)"
        );
    }
}

fn unsupported_leader_flags(flags: &ConnectFlags) -> Vec<&'static str> {
    let mut out = Vec::new();
    if let Some(flag) = flags.memory_override_flag {
        out.push(flag);
    }
    if flags.disable_web_search {
        out.push("--disable-web-search");
    }
    if flags.storage_mode.is_some() {
        out.push("--storage-mode");
    }
    if flags.subagents {
        out.push("--subagents");
    }
    if !flags.permission_rules.is_empty() {
        out.push("--allow/--deny permission rules");
    }
    out
}

/// Write config.toml fields based on CLI flags.
fn apply_config_writes(flags: &ConnectFlags) {
    // Use toml_edit to preserve existing config structure
    let config_path =
        echo_agent_runtime::util::echo_agent_home::echo_agent_home().join("config.toml");
    let content = std::fs::read_to_string(&config_path).unwrap_or_default();
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .unwrap_or_default();

    let mut changed = false;

    if let Some(ref installer) = flags.installer {
        let cli = doc
            .entry("cli")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        if let Some(tbl) = cli.as_table_mut() {
            tbl["installer"] = toml_edit::value(installer.as_str());
            changed = true;
        }
    }

    if changed {
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&config_path, doc.to_string()) {
            tracing::warn!(error = %e, "failed to write config.toml");
        }
    }
}

/// Build the per-session `_meta` for `InitializeRequest` (TUI and leader).
fn build_initialize_meta(flags: &ConnectFlags) -> serde_json::Value {
    let client_type = flags
        .client_identifier
        .as_deref()
        .unwrap_or(PAGER_CLIENT_TYPE);
    let mut meta = serde_json::json!({
        "clientType": client_type,
        "clientVersion": PAGER_CLIENT_VERSION,
    });
    if let Some(spo) = &flags.system_prompt_override {
        meta["systemPromptOverride"] = serde_json::Value::String(spo.clone());
    }
    if let Some(rules) = &flags.rules {
        meta["rules"] = serde_json::Value::String(rules.clone());
    }
    meta
}

/// Build `client_capabilities.meta`. The hunk-tracker mode is canonicalized at
/// this connect read so the agent runs exactly what the settings modal displays.
fn client_capabilities_meta(flags: &ConnectFlags) -> serde_json::Value {
    let hunk_mode =
        crate::settings::canonical_hunk_tracker_mode(flags.hunk_tracker_mode.as_deref());
    let mut meta = serde_json::json!({
        "echo.agent/incrementalBashOutput": true,
        "echo.agent/hunkTracker": { "mode": hunk_mode },
        "echo.agent/bashOutputNoColor": true,
        "echo.agent/gitHeadChanged": true,
    });
    meta[echo_agent_status_line::STATUS_LINE_CAPABILITY] = flags.status_line.into();
    meta
}

/// Parse `defaultAuthMethodId` from `InitializeResponse.meta`.
///
/// The agent is the source of truth for preferred-method selection (including
/// `[auth] preferred_method`); clients must not re-derive api_key vs session.
pub fn parse_default_auth_method_id(meta: Option<&acp::Meta>) -> Option<acp::AuthMethodId> {
    meta.and_then(|m| m.get("defaultAuthMethodId"))
        .and_then(|v| v.as_str())
        .map(|s| acp::AuthMethodId::new(s.to_owned()))
}

/// Send InitializeRequest and parse the response.
async fn initialize(
    tx: &AcpAgentTx,
    flags: &ConnectFlags,
) -> Result<(
    ModelState,
    bool,
    Vec<acp::AuthMethod>,
    Option<acp::AuthMethodId>,
    Vec<acp::AvailableCommand>,
    bool,
    bool,
    bool,
)> {
    let req = acp::InitializeRequest::new(acp::ProtocolVersion::V1)
        .client_capabilities(
            acp::ClientCapabilities::new()
                .fs(acp::FileSystemCapabilities::new()
                    .read_text_file(flags.fs_read)
                    .write_text_file(flags.fs_write))
                .terminal(flags.terminal)
                .meta(client_capabilities_meta(flags).as_object().cloned()),
        )
        .meta(build_initialize_meta(flags).as_object().cloned());

    let resp: acp::InitializeResponse = acp_send(req, tx).await?;

    // Check if this is a echo-agent-shell agent
    let is_echo_agent_shell = resp
        .meta
        .as_ref()
        .and_then(|m| m.get("echoagentShell"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Parse model state from response meta
    let models: ModelState = resp
        .meta
        .as_ref()
        .and_then(|m| m.get("modelState"))
        .and_then(|v| serde_json::from_value::<acp::SessionModelState>(v.clone()).ok())
        .into();

    // Parse available commands from response meta (shell builtins + skills).
    // These seed the slash command registry so autocomplete works immediately.
    let available_commands = parse_available_commands(resp.meta.as_ref());

    let cancel_rewind_enabled = resp
        .meta
        .as_ref()
        .and_then(|m| m.get("cancelRewind"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let session_recap_available = parse_session_recap_available(resp.meta.as_ref());
    let feedback_trace_offer = parse_feedback_trace_offer(resp.meta.as_ref());
    let default_auth_method_id = parse_default_auth_method_id(resp.meta.as_ref());

    Ok((
        models,
        is_echo_agent_shell,
        resp.auth_methods,
        default_auth_method_id,
        available_commands,
        cancel_rewind_enabled,
        session_recap_available,
        feedback_trace_offer,
    ))
}

/// Parse `availableCommands` from an `InitializeResponse.meta` value.
///
/// Extracted as a standalone function for testability (the full `initialize()`
/// function requires an ACP connection).
pub fn parse_available_commands(meta: Option<&acp::Meta>) -> Vec<acp::AvailableCommand> {
    meta.and_then(|m| m.get("availableCommands"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Parse `sessionRecap` from `InitializeResponse.meta` (shell rollout gate).
///
/// Default `false` when missing or non-bool so older agents and dark-launch
/// defaults produce zero automatic recap traffic.
pub fn parse_session_recap_available(meta: Option<&acp::Meta>) -> bool {
    meta.and_then(|m| m.get("sessionRecap"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub fn parse_feedback_trace_offer(meta: Option<&acp::Meta>) -> bool {
    meta.and_then(|m| m.get("feedbackTraceOffer"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Determine whether interactive login is needed based on the advertised auth methods.
///
/// Matches TUI startup behavior: if the first method is `cloud.echo-agent.invalid`, defer auth
/// and show the login-aware welcome flow. Otherwise, authenticate eagerly.
///
/// Returns `(needs_login, login_label, login_method_id, auth_start_mode)`.
pub fn startup_auth_metadata(
    auth_methods: &[acp::AuthMethod],
) -> (
    bool,
    Option<String>,
    Option<acp::AuthMethodId>,
    AuthStartMode,
) {
    let first_method = auth_methods.first();
    let needs_login = first_method
        .map(|m| AuthMethodKind::from_id(m.id()).needs_interactive_login())
        .unwrap_or(false);

    if !needs_login {
        return (false, None, None, AuthStartMode::Pending);
    }

    let method = first_method.unwrap(); // safe: needs_login == true implies first_method.is_some()
    let login_label = Some(method.name().to_string());
    let login_method_id = Some(method.id().clone());

    let is_provider = method
        .meta()
        .as_ref()
        .and_then(|v| v.get("external_provider"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let auth_start_mode = if is_provider {
        AuthStartMode::Command
    } else {
        AuthStartMode::Pending
    };

    (needs_login, login_label, login_method_id, auth_start_mode)
}

/// Find an interactive login method from the auth methods list.
///
/// Used when eager auth (cached_token / API key) fails and we need to fall
/// back to the welcome screen with a working login button. Scans the list
/// for a `cloud.echo-agent.invalid` or `oidc` method — these are the ones that can trigger
/// a browser-based re-auth flow.
pub fn find_interactive_login_method(
    auth_methods: &[acp::AuthMethod],
) -> (Option<String>, Option<acp::AuthMethodId>, AuthStartMode) {
    let interactive = auth_methods
        .iter()
        .find(|m| AuthMethodKind::from_id(m.id()).needs_interactive_login());

    match interactive {
        Some(method) => {
            let is_provider = method
                .meta()
                .as_ref()
                .and_then(|v| v.get("external_provider"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mode = if is_provider {
                AuthStartMode::Command
            } else {
                AuthStartMode::Pending
            };
            (
                Some(method.name().to_string()),
                Some(method.id().clone()),
                mode,
            )
        }
        None => (None, None, AuthStartMode::Pending),
    }
}

/// Attempt eager auth; on failure fall back to the interactive login screen.
///
/// Errors from `authenticate` are caught so the connection still succeeds.
/// When `echoagent.api_key` was advertised, non-interactive credentials were
/// available — do not promote to interactive auto-Login (shell owns
/// unpinned fallthrough; a failed api_key must not open a browser). Otherwise
/// hand the interactive method for the login screen.
///
/// Empty `auth_methods` (e.g. `preferred_method=api_key` with no key) is
/// fail-closed: needs_login without an interactive method.
///
/// Returns `(needs_login, login_label, login_method_id, auth_start_mode, auth_meta)`.
async fn eager_auth_or_login_fallback(
    tx: &AcpAgentTx,
    auth_methods: &[acp::AuthMethod],
    default_auth_method_id: Option<&acp::AuthMethodId>,
    needs_login: bool,
    login_label: Option<String>,
    login_method_id: Option<acp::AuthMethodId>,
    auth_start_mode: AuthStartMode,
) -> (
    bool,
    Option<String>,
    Option<acp::AuthMethodId>,
    AuthStartMode,
    Option<serde_json::Value>,
) {
    if auth_methods.is_empty() {
        // preferred_method pin unavailable — fail closed, no invented method.
        return (true, None, None, AuthStartMode::Pending, None);
    }
    if needs_login {
        return (
            needs_login,
            login_label,
            login_method_id,
            auth_start_mode,
            None,
        );
    }
    match authenticate(tx, auth_methods, default_auth_method_id).await {
        Ok(meta) => (
            needs_login,
            login_label,
            login_method_id,
            auth_start_mode,
            meta,
        ),
        Err(_) => {
            // Non-interactive credentials were advertised; shell fallthrough
            // already preferred them — do not auto-open browser login.
            let has_api_key = auth_methods
                .iter()
                .any(|m| AuthMethodKind::from_id(m.id()) == AuthMethodKind::EchoAgentApiKey);
            if has_api_key {
                return (false, login_label, login_method_id, auth_start_mode, None);
            }
            let (label, method_id, mode) = find_interactive_login_method(auth_methods);
            (true, label, method_id, mode, None)
        }
    }
}

/// [`eager_auth_or_login_fallback`] bounded by `STARTUP_AUTH_REFRESH_TIMEOUT`,
/// so a hung agent cannot gate the first draw. On timeout the inputs pass
/// through unchanged and the agent finishes authentication in the background.
async fn bounded_eager_auth(
    tx: &AcpAgentTx,
    auth_methods: &[acp::AuthMethod],
    default_auth_method_id: Option<&acp::AuthMethodId>,
    needs_login: bool,
    login_label: Option<String>,
    login_method_id: Option<acp::AuthMethodId>,
    auth_start_mode: AuthStartMode,
) -> (
    bool,
    Option<String>,
    Option<acp::AuthMethodId>,
    AuthStartMode,
    Option<serde_json::Value>,
) {
    match tokio::time::timeout(
        echo_agent_runtime::http::STARTUP_AUTH_REFRESH_TIMEOUT,
        eager_auth_or_login_fallback(
            tx,
            auth_methods,
            default_auth_method_id,
            needs_login,
            login_label.clone(),
            login_method_id.clone(),
            auth_start_mode,
        ),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(_) => (
            needs_login,
            login_label,
            login_method_id,
            auth_start_mode,
            None,
        ),
    }
}

/// Authenticate with the agent using the agent's chosen default method.
///
/// Prefer `defaultAuthMethodId` from initialize meta when present and listed.
/// Do not re-derive api_key vs session ordering client-side (that has regressed
/// OIDC refresh before). Legacy fallback: `cached_token` then first method.
///
/// Returns the response `meta` (contains `team_name`, etc.) so callers can
/// propagate it to the UI.
async fn authenticate(
    tx: &AcpAgentTx,
    auth_methods: &[acp::AuthMethod],
    default_auth_method_id: Option<&acp::AuthMethodId>,
) -> Result<Option<serde_json::Value>> {
    let method_id = select_eager_auth_method(auth_methods, default_auth_method_id)
        .ok_or_else(|| anyhow::anyhow!("No auth methods available"))?;
    crate::unified_log::info(
        "pager eager auth method selected",
        None,
        Some(serde_json::json!({
            "method_id": method_id.0.as_ref(),
            "from_default_auth_method_id": default_auth_method_id
                .is_some_and(|d| d.0.as_ref() == method_id.0.as_ref()),
            "methods_count": auth_methods.len(),
            "first_method": auth_methods.first().map(|m| m.id().0.as_ref()),
        })),
    );

    let resp: acp::AuthenticateResponse =
        acp_send(acp::AuthenticateRequest::new(method_id), tx).await?;
    Ok(resp.meta.map(serde_json::Value::Object))
}

/// Pick the method id for eager authenticate.
///
/// 1. Agent's `defaultAuthMethodId` when present in the advertised list
/// 2. Legacy: `cached_token` if advertised, else first method
pub fn select_eager_auth_method(
    auth_methods: &[acp::AuthMethod],
    default_auth_method_id: Option<&acp::AuthMethodId>,
) -> Option<acp::AuthMethodId> {
    if let Some(default_id) = default_auth_method_id
        && auth_methods.iter().any(|m| m.id() == default_id)
    {
        return Some(default_id.clone());
    }
    let cached_token_method = auth_methods
        .iter()
        .find(|m| AuthMethodKind::from_id(m.id()) == AuthMethodKind::CachedToken);
    cached_token_method
        .or_else(|| auth_methods.first())
        .map(|m| m.id().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_session_update_ext_method_covers_both_carriers() {
        assert!(is_session_update_ext_method(
            "echo.agent/session_notification"
        ));
        assert!(is_session_update_ext_method("echo.agent/session/update"));
        assert!(!is_session_update_ext_method("echo.agent/task_completed"));
        assert!(!is_session_update_ext_method("session/update"));
    }

    #[test]
    fn parse_available_commands_from_meta() {
        let meta = serde_json::json!({
            "availableCommands": [
                {
                    "name": "compact",
                    "description": "Compact conversation history",
                    "input": { "hint": "<focus>" }
                },
                {
                    "name": "flush",
                    "description": "Flush memory"
                }
            ]
        });
        let cmds = parse_available_commands(meta.as_object());
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].name, "compact");
        assert_eq!(cmds[0].description, "Compact conversation history");
        assert!(cmds[0].input.is_some());
        assert_eq!(cmds[1].name, "flush");
        assert!(cmds[1].input.is_none());
    }

    #[test]
    fn parse_available_commands_missing_key_returns_empty() {
        let meta = serde_json::json!({ "echoagentShell": true });
        let cmds = parse_available_commands(meta.as_object());
        assert!(cmds.is_empty());
    }

    #[test]
    fn parse_available_commands_none_meta_returns_empty() {
        let cmds = parse_available_commands(None);
        assert!(cmds.is_empty());
    }

    #[test]
    fn parse_available_commands_invalid_json_returns_empty() {
        let meta = serde_json::json!({
            "availableCommands": "not-an-array"
        });
        let cmds = parse_available_commands(meta.as_object());
        assert!(cmds.is_empty());
    }

    #[test]
    fn parse_session_recap_available_true() {
        let meta = serde_json::json!({ "sessionRecap": true });
        assert!(parse_session_recap_available(meta.as_object()));
    }

    #[test]
    fn parse_session_recap_available_false_explicit() {
        let meta = serde_json::json!({ "sessionRecap": false });
        assert!(!parse_session_recap_available(meta.as_object()));
    }

    #[test]
    fn parse_session_recap_available_defaults_off_when_missing() {
        let meta = serde_json::json!({ "echoagentShell": true, "cancelRewind": true });
        assert!(!parse_session_recap_available(meta.as_object()));
        assert!(!parse_session_recap_available(None));
    }

    #[test]
    fn parse_session_recap_available_non_bool_defaults_off() {
        let meta = serde_json::json!({ "sessionRecap": "yes" });
        assert!(!parse_session_recap_available(meta.as_object()));
    }

    // ── startup_auth_metadata ──────────────────────────────────────

    fn make_auth_method(id: &str, name: &str, meta: Option<serde_json::Value>) -> acp::AuthMethod {
        let mut agent = acp::AuthMethodAgent::new(acp::AuthMethodId::new(id), name.to_string());
        if let Some(m) = meta.and_then(|v| v.as_object().cloned()) {
            agent = agent.meta(m);
        }
        acp::AuthMethod::Agent(agent)
    }

    #[test]
    fn startup_auth_empty_methods_no_login() {
        let (needs, label, method_id, mode) = startup_auth_metadata(&[]);
        assert!(!needs);
        assert!(label.is_none());
        assert!(method_id.is_none());
        assert_eq!(mode, AuthStartMode::Pending);
    }

    #[test]
    fn startup_auth_echo_agent_com_no_provider_needs_login_pending() {
        let methods = vec![make_auth_method(
            "cloud.echo-agent.invalid",
            "cloud.echo-agent.invalid",
            None,
        )];
        let (needs, label, method_id, mode) = startup_auth_metadata(&methods);
        assert!(needs);
        assert_eq!(label.as_deref(), Some("cloud.echo-agent.invalid"));
        assert_eq!(
            method_id.as_ref().unwrap().0.as_ref(),
            "cloud.echo-agent.invalid"
        );
        assert_eq!(mode, AuthStartMode::Pending);
    }

    #[test]
    fn startup_auth_echo_agent_com_with_external_provider_command() {
        let meta = serde_json::json!({ "external_provider": true });
        let methods = vec![make_auth_method(
            "cloud.echo-agent.invalid",
            "Acme Corp",
            Some(meta),
        )];
        let (needs, label, method_id, mode) = startup_auth_metadata(&methods);
        assert!(needs);
        assert_eq!(label.as_deref(), Some("Acme Corp"));
        assert_eq!(
            method_id.as_ref().unwrap().0.as_ref(),
            "cloud.echo-agent.invalid"
        );
        assert_eq!(mode, AuthStartMode::Command);
    }

    #[test]
    fn startup_auth_non_echo_agent_com_no_login() {
        let methods = vec![make_auth_method("api-key", "API Key", None)];
        let (needs, label, method_id, mode) = startup_auth_metadata(&methods);
        assert!(!needs);
        assert!(label.is_none());
        assert!(method_id.is_none());
        assert_eq!(mode, AuthStartMode::Pending);
    }

    /// CROSS-CRATE REGRESSION GUARD:
    ///
    /// Enterprise/BYOK configs (e.g. an enterprise `~/.echo-agent/config.toml` with a
    /// `[model.*]` table containing `env_key = "ANTHROPIC_AUTH_TOKEN"`) MUST
    /// NOT send the user to the login screen at startup.
    ///
    /// This test exercises the SHELL-PAGER JOIN, not just the pager half:
    /// it calls the shell-side `build_auth_methods()` with the exact inputs
    /// `MvpAgent::initialize()` would compute for an enterprise user, then feeds
    /// the result into the pager's `startup_auth_metadata()`. If a future
    /// change re-orders `build_auth_methods()` to put `echoagent.api_key` anywhere
    /// other than first (the shape of a past regression), this test fails
    /// because `startup_auth_metadata()` returns `needs_login = true`.
    ///
    /// Counterpart shell-side tests
    /// (`agent::auth_method::tests::enterprise_byok_first_method_is_echo_agent_api_key`
    /// and `enterprise_byok_config_does_not_require_login`) pin the same
    /// invariant from the shell side; this test pins the cross-crate
    /// contract that the pager actually consumes the shell's output as
    /// expected.
    #[test]
    fn shell_built_auth_methods_for_byok_user_skip_login_screen() {
        use echo_agent_runtime::agent::auth_method::{AuthMethodsBuildInputs, build_auth_methods};

        let built = build_auth_methods(AuthMethodsBuildInputs {
            // enterprise-style: model has `env_key` set and the env var resolves,
            // so the shell-side predicate returns true.
            has_external_api_key: true,
            // Realistic enterprise user: no cached session token, default `cloud.echo-agent.invalid`
            // login (no enterprise OIDC).
            has_cached_token: false,
            has_enterprise_oidc: false,
            enterprise_oidc_issuer: None,
            login_label: None,
            has_auth_provider_command: false,
            preferred_method: None,
        });

        let (needs, label, method_id, mode) = startup_auth_metadata(&built.methods);
        assert!(
            !needs,
            "shell built auth_methods for a BYOK user, but the pager still \
             reports needs_login = true. Either the shell stopped putting \
             echoagent.api_key first or the pager stopped treating echoagent.api_key as \
             a no-login method.",
        );
        assert!(label.is_none());
        assert!(method_id.is_none());
        assert_eq!(mode, AuthStartMode::Pending);
    }

    /// Inverse direction: when `echoagent.api_key` is NOT in the list, the pager
    /// MUST show the login screen. We assert this with `echoagent.api_key` present
    /// LATER in the list (the shape of a past regression) and confirm the
    /// pager still requires login -- because the pager only inspects
    /// `auth_methods.first()`. This locks the failure mode of the regression:
    /// if a future refactor makes the pager scan past `.first()`, this test
    /// stops being equivalent to
    /// `startup_auth_echo_agent_com_no_provider_needs_login_pending` above and
    /// either passes or fails on a meaningful new code path.
    #[test]
    fn startup_auth_echo_agent_api_key_not_first_still_requires_login() {
        use echo_agent_runtime::agent::auth_method::{
            ECHO_AGENT_API_KEY_METHOD_ID, ECHO_AGENT_COM_METHOD_ID,
        };

        let methods = vec![
            make_auth_method(ECHO_AGENT_COM_METHOD_ID, "EchoAgent", None),
            make_auth_method(ECHO_AGENT_API_KEY_METHOD_ID, "echoagent.api_key", None),
        ];
        let (needs, _, _, _) = startup_auth_metadata(&methods);
        assert!(
            needs,
            "with cloud.echo-agent.invalid first, the pager must require login -- pinning \
             the BAD-ordering failure mode (echoagent.api_key not first)",
        );
    }

    #[test]
    fn startup_auth_method_id_is_copied_not_synthesized() {
        let methods = vec![make_auth_method(
            "cloud.echo-agent.invalid",
            "My Login",
            None,
        )];
        let (_, _, method_id, _) = startup_auth_metadata(&methods);
        // Verify it's the exact same ID from the method, not hardcoded
        assert_eq!(&method_id.unwrap(), methods[0].id());
    }

    #[test]
    fn startup_auth_external_provider_false_is_pending() {
        let meta = serde_json::json!({ "external_provider": false });
        let methods = vec![make_auth_method(
            "cloud.echo-agent.invalid",
            "cloud.echo-agent.invalid",
            Some(meta),
        )];
        let (_, _, _, mode) = startup_auth_metadata(&methods);
        assert_eq!(mode, AuthStartMode::Pending);
    }

    // ── unsupported_leader_flags ──────────────────────────────────

    #[test]
    fn unsupported_leader_flags_empty_when_none_set() {
        let flags = ConnectFlags::default();
        assert!(unsupported_leader_flags(&flags).is_empty());
    }

    #[test]
    fn unsupported_leader_flags_detects_all() {
        let flags = ConnectFlags {
            memory_enabled_override: Some(true),
            memory_override_flag: Some("--experimental-memory"),
            disable_web_search: true,
            storage_mode: Some("writeback".into()),
            subagents: true,
            ..Default::default()
        };
        let detected = unsupported_leader_flags(&flags);
        assert_eq!(detected.len(), 4);
        assert!(detected.contains(&"--experimental-memory"));
        assert!(detected.contains(&"--disable-web-search"));
        assert!(detected.contains(&"--storage-mode"));
        assert!(detected.contains(&"--subagents"));
    }

    #[test]
    fn unsupported_leader_flags_preserves_no_memory_spelling() {
        let flags = ConnectFlags {
            memory_enabled_override: Some(false),
            memory_override_flag: Some("--no-memory"),
            ..Default::default()
        };
        assert_eq!(unsupported_leader_flags(&flags), vec!["--no-memory"]);
    }

    #[test]
    fn unsupported_leader_flags_ignores_supported() {
        let flags = ConnectFlags {
            terminal: true,
            fs_read: true,
            fs_write: true,
            ..Default::default()
        };
        assert!(unsupported_leader_flags(&flags).is_empty());
    }

    #[test]
    fn build_initialize_meta_includes_rules_when_set() {
        let flags = ConnectFlags {
            rules: Some("Always reply in French.".into()),
            ..Default::default()
        };
        let meta = build_initialize_meta(&flags);
        assert_eq!(meta["rules"], "Always reply in French.");
    }

    #[test]
    fn build_initialize_meta_omits_rules_when_unset() {
        let flags = ConnectFlags::default();
        let meta = build_initialize_meta(&flags);
        assert!(
            meta.get("rules").is_none(),
            "rules key must be absent when --rules is not set; meta={meta:?}"
        );
    }

    #[test]
    fn build_initialize_meta_carries_system_prompt_override() {
        let flags = ConnectFlags {
            system_prompt_override: Some("YOU ARE A PIRATE.".into()),
            ..Default::default()
        };
        let meta = build_initialize_meta(&flags);
        assert_eq!(meta["systemPromptOverride"], "YOU ARE A PIRATE.");
    }

    #[test]
    fn build_initialize_meta_uses_custom_client_identifier_when_set() {
        let flags = ConnectFlags {
            client_identifier: Some("zed".into()),
            ..Default::default()
        };
        let meta = build_initialize_meta(&flags);
        assert_eq!(meta["clientType"], "zed");
    }

    #[test]
    fn client_capabilities_meta_defaults_absent_or_blank_mode_to_off() {
        // Rows 1 & 2 of the truth table: nothing set, and a set-but-blank value,
        // both advertise the `off` default (never `""` → AllDirty).
        let absent = client_capabilities_meta(&ConnectFlags::default());
        assert_eq!(absent["echo.agent/hunkTracker"]["mode"], "off");
        let blank = client_capabilities_meta(&ConnectFlags {
            hunk_tracker_mode: Some("   ".into()),
            ..Default::default()
        });
        assert_eq!(blank["echo.agent/hunkTracker"]["mode"], "off");
    }

    /// The agent gates the whole payload on this key, so a misspelling on
    /// either side switches the feature off with nothing to show for it.
    #[test]
    fn client_capabilities_meta_advertises_the_status_line_the_config_asked_for() {
        let key = echo_agent_status_line::STATUS_LINE_CAPABILITY;
        for wants_a_row in [true, false] {
            let meta = client_capabilities_meta(&ConnectFlags {
                status_line: wants_a_row,
                ..Default::default()
            });
            assert_eq!(meta[key], wants_a_row, "status_line={wants_a_row}");
        }
    }

    #[test]
    fn client_capabilities_meta_canonicalizes_off_and_mixed_case() {
        // Mixed-case / alias values are canonicalized so the agent runtime
        // matches the modal display.
        for raw in ["off", "OFF", "Disabled"] {
            let meta = client_capabilities_meta(&ConnectFlags {
                hunk_tracker_mode: Some(raw.into()),
                ..Default::default()
            });
            assert_eq!(meta["echo.agent/hunkTracker"]["mode"], "off", "raw={raw}");
        }
    }
}
