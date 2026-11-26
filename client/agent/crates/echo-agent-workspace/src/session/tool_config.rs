//! Tool config resolution pipeline.
//!
//! Five-step resolution:
//! 1. `effective_tool_config = config.tool_config.unwrap_or_else(|| parent.effective_tool_config.clone())`
//! 2. `merged = merge_mcp_tools(effective_tool_config, shared.mcp_servers.snapshot())`
//! 3. `merged = merge_hub_tools(merged, shared.hub_tools_snapshot())`
//! 4. `filtered = config.capability_mode.filter(merged)`
//! 5. `toolset = build_finalized_toolset(filtered, &session.cwd, &session.session_env, ...)`
use crate::capability::{CapabilityMode, kind_allowed};
use crate::config::SessionContextFactory;
use crate::error::{WorkspaceError, WorkspaceResult};
use echo_agent_tools::registry::types::{
    FinalizedToolset, ToolConfig, ToolRegistryBuilder, ToolServerConfig,
};
use echo_agent_tools::types::tool::ToolKind;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
/// Create-shaped entry of the resolution pipeline: run
/// [`resolve_session_toolset_rebuild`] around a FRESH factory-built
/// session-lifetime terminal backend, and return that backend so the caller
/// can store it on the session it is creating. Session-less resolves (the
/// `__template__` catalog resolve in `connect_hub`) also use this entry and
/// simply drop the returned backend with the toolset.
pub(crate) fn resolve_session_toolset(
    effective_tool_config: ToolServerConfig,
    capability_mode: CapabilityMode,
    mcp_snapshot: &[ToolConfig],
    hub_snapshot: &[ToolConfig],
    cwd: PathBuf,
    session_env: Arc<HashMap<String, String>>,
    session_id: &str,
    factory: &dyn SessionContextFactory,
    local_registry: Option<echo_agent_computer_hub_sdk::LocalRegistry>,
    lsp: Option<std::sync::Arc<dyn echo_agent_tools::implementations::lsp::LspBackend>>,
    viewer_ctx: Option<echo_agent_tool_runtime::WorkspaceViewerContext>,
    notification_handle: Option<echo_agent_tools::notification::types::ToolNotificationHandle>,
) -> WorkspaceResult<(
    ToolServerConfig,
    Arc<FinalizedToolset>,
    crate::config::SessionTerminalBackend,
)> {
    let terminal_backend = factory.build_terminal_backend();
    let (effective, toolset) = resolve_session_toolset_rebuild(
        effective_tool_config,
        capability_mode,
        mcp_snapshot,
        hub_snapshot,
        cwd,
        session_env,
        session_id,
        factory,
        local_registry,
        lsp,
        viewer_ctx,
        notification_handle,
        terminal_backend.backend().clone(),
    )?;
    Ok((effective, toolset, terminal_backend))
}
/// Rebuild-shaped entry: run steps 2-5 of the resolution pipeline around an
/// EXISTING session-owned terminal backend. The parameter is non-optional on
/// purpose: every toolset-swap call site must state which backend it rebuilds
/// around, so background tasks and shell state can never be orphaned by a
/// resolve that silently built a fresh backend.
///
/// Returns the *unmodified* `effective_tool_config` (step-1 baseline) so
/// the caller can store it on the session. The FinalizedToolset reflects
/// MCP + hub merging and capability filtering on top of that baseline.
///
/// **MCP-origin and hub-origin `kind: None` tools are dropped under
/// every non-`All` mode.** Baseline `kind: None` tools are always kept —
/// but before filtering, kind-less baseline entries whose id the binary's
/// registry knows get their [`ToolKind`] backfilled (see
/// [`backfill_tool_kinds`]), so the capability filter applies to pinned
/// server-bind toolsets whose wire entries cannot carry a kind.
pub(crate) fn resolve_session_toolset_rebuild(
    effective_tool_config: ToolServerConfig,
    capability_mode: CapabilityMode,
    mcp_snapshot: &[ToolConfig],
    hub_snapshot: &[ToolConfig],
    cwd: PathBuf,
    session_env: Arc<HashMap<String, String>>,
    session_id: &str,
    factory: &dyn SessionContextFactory,
    local_registry: Option<echo_agent_computer_hub_sdk::LocalRegistry>,
    lsp: Option<std::sync::Arc<dyn echo_agent_tools::implementations::lsp::LspBackend>>,
    viewer_ctx: Option<echo_agent_tool_runtime::WorkspaceViewerContext>,
    notification_handle: Option<echo_agent_tools::notification::types::ToolNotificationHandle>,
    terminal_backend: Arc<dyn echo_agent_tools::computer::types::TerminalBackend>,
) -> WorkspaceResult<(ToolServerConfig, Arc<FinalizedToolset>)> {
    let mut builder = factory.registry_builder();
    if let Some(lr) = local_registry {
        builder = builder.with_local_registry(lr);
    }
    let baseline = backfill_tool_kinds(&effective_tool_config, &builder.known_tool_kinds());
    let filtered = merge_and_filter(
        &baseline,
        mcp_snapshot,
        hub_snapshot,
        capability_mode,
        session_id,
    );
    let hub_ids: std::collections::HashSet<&str> =
        hub_snapshot.iter().map(|t| t.id.as_str()).collect();
    let finalize_config = ToolServerConfig {
        tools: filtered
            .tools
            .iter()
            .filter(|t| !hub_ids.contains(t.id.as_str()))
            .cloned()
            .collect(),
        behavior_preset: filtered.behavior_preset.clone(),
    };
    let mut ctx = factory.build_session_context(session_id, cwd, session_env, terminal_backend);
    if let Some(lsp_handle) = lsp {
        ctx.lsp = Some(lsp_handle);
    }
    if let Some(handle) = notification_handle {
        ctx.notification_handle = handle;
    }
    let toolset = builder
        .finalize_with_trunc_config(
            finalize_config,
            ctx,
            echo_agent_tools::types::context::TruncationConfig::default(),
            viewer_ctx,
        )
        .map_err(|errs| {
            let summary: Vec<String> = errs.iter().map(|e| e.summary()).collect();
            WorkspaceError::Finalize(summary.join("; "))
        })?;
    Ok((effective_tool_config, Arc::new(toolset)))
}
/// Backfill `kind: None` baseline entries from the binary's own registry
/// (fully-qualified id -> declared [`ToolKind`]).
///
/// Ids unknown to the registry stay `None` and keep the always-kept
/// baseline behavior (ad-hoc `ToolConfig::simple` tools). Entries that
/// already carry a kind are left untouched.
fn backfill_tool_kinds(
    config: &ToolServerConfig,
    kinds: &HashMap<String, ToolKind>,
) -> ToolServerConfig {
    ToolServerConfig {
        tools: config
            .tools
            .iter()
            .map(|tool| {
                let mut tool = tool.clone();
                if tool.kind.is_none() {
                    tool.kind = kinds.get(&tool.id).copied();
                }
                tool
            })
            .collect(),
        behavior_preset: config.behavior_preset.clone(),
    }
}
/// Steps 2-4 of the resolution pipeline, without step 5 (`finalize`):
///
/// - **Step 2** -- MCP merge: append MCP-origin tools, skipping ID/name collisions with baseline.
/// - **Step 3** -- Hub merge: append hub-origin tools, skipping ID/name collisions with baseline or MCP.
/// - **Step 4** -- Capability filter: drop tools whose `kind` is not allowed by the mode.
///   External (MCP/hub) `kind: None` tools are only kept under `CapabilityMode::All`.
///
/// Priority on ID/name collision: baseline wins > MCP wins > hub is skipped.
pub(crate) fn merge_and_filter(
    baseline: &ToolServerConfig,
    mcp_snapshot: &[ToolConfig],
    hub_snapshot: &[ToolConfig],
    mode: CapabilityMode,
    session_id: &str,
) -> ToolServerConfig {
    if mcp_snapshot.is_empty() && hub_snapshot.is_empty() {
        return mode.filter(baseline);
    }
    let baseline_ids: std::collections::HashSet<&str> =
        baseline.tools.iter().map(|t| t.id.as_str()).collect();
    let mut taken_names: std::collections::HashSet<String> = baseline
        .tools
        .iter()
        .map(|t| {
            let unqualified = t.id.rsplit_once(':').map_or(t.id.as_str(), |(_, n)| n);
            t.resolve_client_name(unqualified)
        })
        .collect();
    let mut tagged: Vec<(ToolConfig, bool)> =
        baseline.tools.iter().cloned().map(|t| (t, false)).collect();
    let mut mcp_tool_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for mcp_tool in mcp_snapshot {
        if baseline_ids.contains(mcp_tool.id.as_str()) {
            tracing::warn!(
                mcp_id = %mcp_tool.id,
                session = %session_id,
                "skipping MCP tool: id collides with baseline"
            );
            continue;
        }
        let client_name = mcp_tool.resolve_client_name(&mcp_tool.id);
        if !taken_names.insert(client_name.clone()) {
            tracing::warn!(
                mcp_id = %mcp_tool.id,
                client_name = %client_name,
                session = %session_id,
                "skipping MCP tool: resolved client name collides with another tool"
            );
            continue;
        }
        mcp_tool_ids.insert(mcp_tool.id.as_str());
        tagged.push((mcp_tool.clone(), true));
    }
    for hub_tool in hub_snapshot {
        if baseline_ids.contains(hub_tool.id.as_str()) {
            tracing::debug!(
                hub_id = %hub_tool.id,
                session = %session_id,
                "skipping remote tool: id collides with baseline"
            );
            continue;
        }
        if mcp_tool_ids.contains(hub_tool.id.as_str()) {
            tracing::debug!(
                hub_id = %hub_tool.id,
                session = %session_id,
                "skipping remote tool: id collides with MCP tool"
            );
            continue;
        }
        let client_name = hub_tool.resolve_client_name(&hub_tool.id);
        if !taken_names.insert(client_name.clone()) {
            tracing::debug!(
                hub_id = %hub_tool.id,
                client_name = %client_name,
                session = %session_id,
                "skipping remote tool: resolved client name collides with another tool"
            );
            continue;
        }
        tagged.push((hub_tool.clone(), true));
    }
    let kept: Vec<ToolConfig> = tagged
        .into_iter()
        .filter(|(tool, is_external)| match tool.kind {
            Some(k) => kind_allowed(mode, k),
            None if !*is_external => true,
            None => matches!(mode, CapabilityMode::All),
        })
        .map(|(t, _)| t)
        .collect();
    ToolServerConfig {
        tools: kept,
        behavior_preset: baseline.behavior_preset.clone(),
    }
}
/// Alias for backward compatibility.
pub type NoopSessionContextFactory = WorkspaceSessionContextFactory;
/// Whether per-session `tool_state.json` persistence + per-turn upload is
/// enabled (`ECHO_AGENT_WORKSPACE_TOOL_STATE_ENABLED=true`; any other value keeps
/// legacy behavior).
pub fn tool_state_enabled() -> bool {
    std::env::var("ECHO_AGENT_WORKSPACE_TOOL_STATE_ENABLED").as_deref() == Ok("true")
}
/// Sanitize a `session_id` into a single safe filesystem path segment: chars
/// outside `[A-Za-z0-9_-]` become `_`, empty becomes `anon`. When any
/// replacement happened, an 8-hex digest of the ORIGINAL id is appended so the
/// mapping stays injective — plain substitution would collide distinct ids
/// (`sess/1` and `sess_1`) into one directory, cross-contaminating
/// persistence, rehydration, and [`crate::recovery::cleanup_stale_sessions`].
/// Already-safe ids (the common UUID case) map to themselves.
fn sanitize_session_id(session_id: &str) -> String {
    let mut safe = String::with_capacity(session_id.len());
    let mut modified = false;
    for c in session_id.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            safe.push(c);
        } else {
            safe.push('_');
            modified = true;
        }
    }
    if safe.is_empty() {
        safe.push_str("anon");
        modified = true;
    }
    if modified {
        let digest = echo_agent_file_utils::sha256_hex(session_id.as_bytes());
        safe.push('-');
        safe.push_str(&digest[..8]);
    }
    safe
}
/// `<root>/sessions/<sanitized_id>`, creating the directory when possible.
fn ensure_session_dir(root: &std::path::Path, session_id: &str) -> (PathBuf, std::io::Result<()>) {
    let dir = root.join("sessions").join(sanitize_session_id(session_id));
    let created = std::fs::create_dir_all(&dir);
    (dir, created)
}
/// Serializes tests (across modules) that mutate the process-global
/// `ECHO_AGENT_WORKSPACE_TOOL_STATE_ENABLED`. Aliased to the crate-wide
/// [`crate::ENV_TEST_LOCK`] so ALL env-mutating tests share ONE lock (the
/// hazard is the global `environ` array, not the variable's value).
#[cfg(test)]
pub(crate) use crate::ENV_TEST_LOCK as TOOL_STATE_ENV_LOCK;
/// [`SessionContextFactory`] for workspace server sessions.
///
/// When constructed with an [`AuthProvider`] and API base URL, gen tools
/// (image_gen, video_gen) are enabled using the provider's current
/// OAuth token. Without auth, gen tools default to `Disabled`.
///
/// When [`with_tool_state_home`](Self::with_tool_state_home) is set, each
/// session's [`SessionContext::state_path`] is rooted at
/// `<home>/sessions/<session_id>/`; left unset, `state_path` stays empty
/// (legacy behavior).
///
/// [`SessionContext::session_folder`] is `/tmp/sessions/<sanitized_id>/`
/// (terminal logs and other tool artifacts — not the project `cwd`).
///
/// Terminal backends are persistent-shell [`LocalTerminalBackend`]s, built
/// once per session by [`build_terminal_backend`] and passed into every
/// [`build_session_context`] call.
///
/// [`build_terminal_backend`]: crate::config::SessionContextFactory::build_terminal_backend
/// [`build_session_context`]: crate::config::SessionContextFactory::build_session_context
/// [`LocalTerminalBackend`]: echo_agent_tools::computer::local::LocalTerminalBackend
pub struct WorkspaceSessionContextFactory {
    auth: Option<echo_agent_computer_hub_sdk::SharedAuthProvider>,
    api_base_url: Option<String>,
    /// Resolved `$ECHO_AGENT_WORKSPACE_HOME` when tool-state persistence is enabled;
    /// `None` disables it. Resolved once by the caller so the factory performs
    /// no per-build env reads.
    tool_state_home: Option<PathBuf>,
}
impl Default for WorkspaceSessionContextFactory {
    fn default() -> Self {
        Self::new()
    }
}
impl WorkspaceSessionContextFactory {
    pub fn new() -> Self {
        Self {
            auth: None,
            api_base_url: None,
            tool_state_home: None,
        }
    }
    /// Factory with auth — gen tools use the provider's live token.
    pub fn with_auth(
        auth: echo_agent_computer_hub_sdk::SharedAuthProvider,
        api_base_url: String,
    ) -> Self {
        Self {
            auth: Some(auth),
            api_base_url: Some(api_base_url),
            tool_state_home: None,
        }
    }
    /// Enable session-keyed tool-state persistence rooted at `home`
    /// (`$ECHO_AGENT_WORKSPACE_HOME`). Callers should only invoke this when
    /// [`tool_state_enabled`] is `true`.
    pub fn with_tool_state_home(mut self, home: PathBuf) -> Self {
        self.tool_state_home = Some(home);
        self
    }
    /// `<tool_state_home>/sessions/<sanitized_id>/tool_state.json`, or empty
    /// when persistence is disabled / dir creation fails.
    fn resolve_state_path(&self, session_id: &str) -> PathBuf {
        let Some(home) = self.tool_state_home.as_ref() else {
            return PathBuf::new();
        };
        let (dir, created) = ensure_session_dir(home, session_id);
        if let Err(e) = created {
            tracing::warn!(
                session = %session_id,
                dir = %dir.display(),
                error = %e,
                "tool_state: failed to create session dir; persistence disabled for session"
            );
            return PathBuf::new();
        }
        tracing::debug!(
            session = %session_id,
            dir = %dir.display(),
            "tool_state: persistence bound to session-keyed dir"
        );
        dir.join("tool_state.json")
    }
    /// `/tmp/sessions/<sanitized_id>/` for terminal logs and other tool artifacts.
    fn resolve_session_folder(session_id: &str) -> PathBuf {
        let (dir, created) = ensure_session_dir(std::path::Path::new("/tmp"), session_id);
        if let Err(e) = created {
            tracing::warn!(
                session = %session_id,
                dir = %dir.display(),
                error = %e,
                "session_folder: failed to create dir; tools may create it on write"
            );
        }
        dir
    }
}
impl SessionContextFactory for WorkspaceSessionContextFactory {
    fn build_session_context(
        &self,
        session_id: &str,
        cwd: PathBuf,
        session_env: Arc<HashMap<String, String>>,
        backend: Arc<dyn echo_agent_tools::computer::types::TerminalBackend>,
    ) -> echo_agent_tools::registry::types::SessionContext {
        use echo_agent_tools::implementations::echo_agent_build::app_builder::AppBuilderDeployerConfig;
        use echo_agent_tools::implementations::echo_agent_build::image_gen::ImageGenConfig;
        use echo_agent_tools::implementations::echo_agent_build::video_gen::VideoGenConfig;
        use echo_agent_tools::implementations::web_search::WebSearchConfig;
        let fs = Arc::new(echo_agent_tools::computer::local::LocalFs)
            as Arc<dyn echo_agent_tools::computer::types::AsyncFileSystem>;
        let notification_handle = echo_agent_tools::notification::ToolNotificationHandle::noop();
        let (image_gen_config, video_gen_config, web_search_config, app_builder_deployer_config) =
            if let (Some(auth), Some(url)) = (&self.auth, &self.api_base_url) {
                let cred = auth.current();
                match cred {
                    echo_agent_computer_hub_sdk::AuthCredential::Bearer { token, .. } => {
                        let headers = build_proxy_headers(url);
                        (
                            ImageGenConfig::Enabled {
                                api_key: token.clone(),
                                base_url: url.clone(),
                                extra_headers: headers.clone(),
                                image_gen_enabled: true,
                                image_edit_enabled: true,
                                model_override: None,
                                edit_model_override: None,
                                tier_restricted: false,
                            },
                            VideoGenConfig::Enabled {
                                api_key: token.clone(),
                                base_url: url.clone(),
                                extra_headers: headers.clone(),
                                zdr_video_output_s3: None,
                                tier_restricted: false,
                                zdr_restricted: false,
                            },
                            WebSearchConfig::Enabled {
                                api_key: token,
                                base_url: url.clone(),
                                model: default_web_search_model(),
                                extra_headers: headers,
                                alpha_test_key: None,
                                allowed_domains: None,
                                excluded_domains: None,
                            },
                            AppBuilderDeployerConfig::default(),
                        )
                    }
                    _ => (
                        ImageGenConfig::default(),
                        VideoGenConfig::default(),
                        WebSearchConfig::default(),
                        AppBuilderDeployerConfig::default(),
                    ),
                }
            } else {
                (
                    ImageGenConfig::default(),
                    VideoGenConfig::default(),
                    WebSearchConfig::default(),
                    AppBuilderDeployerConfig::default(),
                )
            };
        echo_agent_tools::registry::types::SessionContext {
            backend,
            fs,
            cwd,
            session_folder: Self::resolve_session_folder(session_id),
            session_env,
            notification_handle,
            owner_session_id: Some(session_id.to_string()),
            subagent: None,
            parent_scheduler_handle: None,
            skills: vec![],
            state_path: self.resolve_state_path(session_id),
            memory_backend: None,
            web_search_config,
            web_fetch_config: build_web_fetch_config(),
            lsp: None,
            image_gen_config,
            video_gen_config,
            app_builder_deployer_config,
            api_key_provider: None,
            auth_provider: self.auth.clone(),
            attribution_callback: None,
            system_reminder_tag: echo_agent_tools::reminders::DEFAULT_REMINDER_TAG,
        }
    }
    fn build_terminal_backend(&self) -> crate::config::SessionTerminalBackend {
        crate::config::SessionTerminalBackend::local(
            echo_agent_tools::computer::local::LocalTerminalBackend::new(),
        )
    }
    fn registry_builder(&self) -> ToolRegistryBuilder {
        ToolRegistryBuilder::new()
    }
    fn known_tool_ids(&self) -> Arc<std::collections::HashSet<String>> {
        static IDS: std::sync::LazyLock<Arc<std::collections::HashSet<String>>> =
            std::sync::LazyLock::new(|| Arc::new(ToolRegistryBuilder::new().known_tool_ids()));
        IDS.clone()
    }
}
/// Build extra headers for API calls routed through the chat proxy.
/// Mirrors the shell's `inject_proxy_headers` logic.
fn build_proxy_headers(base_url: &str) -> indexmap::IndexMap<String, String> {
    let mut headers = indexmap::IndexMap::new();
    let version = echo_agent_version::VERSION;
    headers.insert(
        "user-agent".to_string(),
        format!("echo-agent-workspace/{version}"),
    );
    headers.insert(
        "x-echo-agent-client-version".to_string(),
        version.to_string(),
    );
    headers.insert(
        "x-echo-agent-client-identifier".to_string(),
        std::env::var("ECHO_AGENT_CLIENT_NAME").unwrap_or_else(|_| "echo-agent-shell".to_string()),
    );
    if base_url.contains("cli-chat-proxy") || base_url.contains("chat-proxy") {
        headers.insert(
            "X-Echo-Agent-Token-Auth".to_string(),
            "echo-agent-cli".to_string(),
        );
        headers.insert(
            "x-authenticateresponse".to_string(),
            "authenticate-response".to_string(),
        );
    }
    headers
}
/// Build web fetch config. Enabled with default params unless
/// `ECHO_AGENT_DISABLE_WEB_FETCH=1` is set.
fn build_web_fetch_config()
-> echo_agent_tools::implementations::echo_agent_build::web_fetch::WebFetchConfig {
    use echo_agent_tools::implementations::echo_agent_build::web_fetch::{
        WebFetchConfig, WebFetchParams,
    };
    if std::env::var("ECHO_AGENT_DISABLE_WEB_FETCH").is_ok_and(|v| v == "1" || v == "true") {
        return WebFetchConfig::Disabled;
    }
    let mut params = WebFetchParams::default();
    if let Ok(proxy) = std::env::var("ECHO_AGENT_WEB_FETCH_PROXY") {
        params.proxy_endpoint = Some(proxy);
    }
    if echo_agent_config::env_bool("ECHO_AGENT_WEB_FETCH_ALLOW_LOCAL") == Some(true) {
        params.allow_local = Some(true);
    }
    WebFetchConfig::Enabled { params }
}
fn default_web_search_model() -> String {
    std::env::var("ECHO_AGENT_WEB_SEARCH_MODEL").unwrap_or_else(|_| "echo-agent-4.5".to_string())
}
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use crate::config::SessionContextFactory;
    use echo_agent_tools::computer::local::{LocalFs, LocalTerminalBackend};
    use echo_agent_tools::notification::ToolNotificationHandle;
    use echo_agent_tools::registry::types::{
        SessionContext, ToolConfig, ToolRegistryBuilder, ToolServerConfig,
    };
    use echo_agent_tools::types::tool::ToolKind;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;
    /// Test factory: builds a `SessionContext` rooted at a per-test temp dir.
    pub struct TestSessionContextFactory {
        pub temp: TempDir,
        tool_state: bool,
    }
    impl Default for TestSessionContextFactory {
        fn default() -> Self {
            Self::new()
        }
    }
    impl TestSessionContextFactory {
        pub fn new() -> Self {
            Self {
                temp: TempDir::new().expect("create temp dir"),
                tool_state: true,
            }
        }
        /// Matches production, where `ECHO_AGENT_WORKSPACE_TOOL_STATE_ENABLED` is unset and the real factory returns an empty path.
        pub fn without_tool_state() -> Self {
            Self {
                tool_state: false,
                ..Self::new()
            }
        }
    }
    impl SessionContextFactory for TestSessionContextFactory {
        fn build_session_context(
            &self,
            session_id: &str,
            cwd: PathBuf,
            session_env: Arc<HashMap<String, String>>,
            backend: Arc<dyn echo_agent_tools::computer::types::TerminalBackend>,
        ) -> SessionContext {
            let session_root = self
                .temp
                .path()
                .join(super::sanitize_session_id(session_id));
            std::fs::create_dir_all(&session_root).expect("create session root");
            SessionContext {
                backend,
                fs: Arc::new(LocalFs),
                cwd,
                session_folder: session_root.clone(),
                session_env,
                notification_handle: ToolNotificationHandle::noop(),
                owner_session_id: None,
                subagent: None,
                parent_scheduler_handle: None,
                skills: vec![],
                state_path: if self.tool_state {
                    session_root.join("tool_state.json")
                } else {
                    PathBuf::new()
                },
                memory_backend: None,
                web_search_config: Default::default(),
                web_fetch_config: Default::default(),
                lsp: None,
                image_gen_config: Default::default(),
                video_gen_config: Default::default(),
                app_builder_deployer_config: Default::default(),
                api_key_provider: None,
                auth_provider: None,
                attribution_callback: None,
                system_reminder_tag: echo_agent_tools::reminders::DEFAULT_REMINDER_TAG,
            }
        }
        fn build_terminal_backend(&self) -> crate::config::SessionTerminalBackend {
            crate::config::SessionTerminalBackend::local(LocalTerminalBackend::new())
        }
        fn registry_builder(&self) -> ToolRegistryBuilder {
            ToolRegistryBuilder::new()
        }
    }
    /// `ToolConfig` builder helper.
    pub fn tc(id: &str, kind: Option<ToolKind>) -> ToolConfig {
        ToolConfig {
            id: id.to_owned(),
            params: None,
            name_override: None,
            params_name_overrides: None,
            description_override: None,
            behavior_version: None,
            kind,
        }
    }
    /// Minimal valid `ToolServerConfig` for finalize-time tests.
    pub fn baseline_config() -> ToolServerConfig {
        ToolServerConfig {
            tools: vec![
                tc("EchoAgentBuild:read_file", Some(ToolKind::Read)),
                tc("EchoAgentBuild:search_replace", Some(ToolKind::Edit)),
                tc("EchoAgentBuild:grep", Some(ToolKind::Search)),
                tc("EchoAgentBuild:list_dir", Some(ToolKind::ListDir)),
            ],
            behavior_preset: None,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SessionContextFactory;
    use echo_agent_tools::types::tool::ToolKind;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    fn factory_for_test() -> Arc<dyn SessionContextFactory> {
        Arc::new(test_support::TestSessionContextFactory::new())
    }
    fn empty_env() -> Arc<HashMap<String, String>> {
        Arc::new(HashMap::new())
    }
    #[tokio::test]
    async fn resolve_session_toolset_empty_mcp_snapshot_is_noop_for_baseline() {
        let factory = factory_for_test();
        let cwd = PathBuf::from("/tmp");
        let baseline = test_support::baseline_config();
        let baseline_ids: Vec<String> = baseline.tools.iter().map(|t| t.id.clone()).collect();
        let (eff, ts, _backend) = resolve_session_toolset(
            baseline,
            CapabilityMode::ReadWrite,
            &[],
            &[],
            cwd,
            empty_env(),
            "main",
            factory.as_ref(),
            None,
            None,
            None,
            None,
        )
        .expect("resolve");
        assert_eq!(
            eff.tools
                .iter()
                .map(|t| t.id.clone())
                .collect::<Vec<String>>(),
            baseline_ids
        );
        assert!(!ts.tool_definitions().is_empty());
    }
    #[tokio::test]
    async fn resolve_session_toolset_mcp_merge_dedup_by_id_baseline_wins() {
        let factory = factory_for_test();
        let baseline = ToolServerConfig {
            tools: vec![test_support::tc(
                "EchoAgentBuild:read_file",
                Some(ToolKind::Read),
            )],
            behavior_preset: None,
        };
        let mut mcp_dup = test_support::tc("EchoAgentBuild:read_file", Some(ToolKind::Read));
        mcp_dup.name_override = Some("mcp_read".into());
        let snapshot = vec![mcp_dup];
        let (_eff, ts, _backend) = resolve_session_toolset(
            baseline,
            CapabilityMode::ReadWrite,
            &snapshot,
            &[],
            PathBuf::from("/tmp"),
            empty_env(),
            "main",
            factory.as_ref(),
            None,
            None,
            None,
            None,
        )
        .expect("resolve");
        let defs = ts.tool_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(
            names.contains(&"read_file"),
            "baseline read_file must survive: {names:?}"
        );
        assert!(
            !names.contains(&"mcp_read"),
            "MCP duplicate must be skipped: {names:?}"
        );
    }
    #[test]
    fn backfill_tool_kinds_fills_known_kindless_ids_only() {
        let kinds = HashMap::from([
            ("EchoAgentBuild:search_replace".to_owned(), ToolKind::Edit),
            ("EchoAgentBuild:read_file".to_owned(), ToolKind::Read),
        ]);
        let config = ToolServerConfig {
            tools: vec![
                test_support::tc("EchoAgentBuild:search_replace", None),
                test_support::tc("adhoc.opaque", None),
                // Pre-set kinds must never be overwritten by the registry.
                test_support::tc("EchoAgentBuild:read_file", Some(ToolKind::Search)),
            ],
            behavior_preset: Some("current".to_owned()),
        };
        let backfilled = backfill_tool_kinds(&config, &kinds);
        let kind_of = |id: &str| {
            backfilled
                .tools
                .iter()
                .find(|t| t.id == id)
                .expect("tool present")
                .kind
        };
        assert_eq!(
            kind_of("EchoAgentBuild:search_replace"),
            Some(ToolKind::Edit)
        );
        assert_eq!(
            kind_of("adhoc.opaque"),
            None,
            "ids unknown to the registry stay kind-less"
        );
        assert_eq!(
            kind_of("EchoAgentBuild:read_file"),
            Some(ToolKind::Search),
            "an explicit kind wins over the registry's"
        );
        assert_eq!(backfilled.behavior_preset.as_deref(), Some("current"));
    }
    /// Regression: pinned server-bind toolsets arrive kind-less (the gRPC
    /// `ToolConfigEntry` has no kind field), which used to make every
    /// baseline entry bypass the capability filter — a `read_only`
    /// sub-agent kept edit + execute tools. The registry backfill must
    /// restore the filter.
    #[tokio::test]
    async fn resolve_session_toolset_readonly_filters_kindless_pinned_tools() {
        let factory = factory_for_test();
        let baseline = ToolServerConfig {
            tools: vec![
                test_support::tc("EchoAgentBuild:read_file", None),
                test_support::tc("EchoAgentBuild:grep", None),
                test_support::tc("EchoAgentBuild:list_dir", None),
                test_support::tc("EchoAgentBuild:search_replace", None),
                test_support::tc("EchoAgentBuild:run_terminal_cmd", None),
            ],
            behavior_preset: None,
        };
        let (eff, ts, _backend) = resolve_session_toolset(
            baseline,
            CapabilityMode::ReadOnly,
            &[],
            &[],
            PathBuf::from("/tmp"),
            empty_env(),
            "main",
            factory.as_ref(),
            None,
            None,
            None,
            None,
        )
        .expect("resolve");
        assert!(eff.tools.iter().all(|t| t.kind.is_none()));
        let names: Vec<String> = ts
            .tool_definitions()
            .into_iter()
            .map(|d| d.function.name)
            .collect();
        for kept in ["read_file", "grep", "list_dir"] {
            assert!(
                names.iter().any(|n| n == kept),
                "{kept} must survive ReadOnly: {names:?}"
            );
        }
        for dropped in ["search_replace", "run_terminal_cmd"] {
            assert!(
                !names.iter().any(|n| n == dropped),
                "{dropped} must be dropped under ReadOnly: {names:?}"
            );
        }
    }
    #[test]
    fn resolve_session_toolset_mcp_edit_dropped_under_readonly() {
        let baseline = ToolServerConfig {
            tools: vec![test_support::tc(
                "EchoAgentBuild:read_file",
                Some(ToolKind::Read),
            )],
            behavior_preset: None,
        };
        let mcp_edit = test_support::tc("mcp.editor", Some(ToolKind::Edit));
        let filtered = merge_and_filter(
            &baseline,
            &[mcp_edit],
            &[],
            CapabilityMode::ReadOnly,
            "test",
        );
        assert!(!filtered.tools.iter().any(|t| t.id == "mcp.editor"));
    }
    #[tokio::test]
    async fn resolve_session_toolset_mcp_kind_none_dropped_under_readonly() {
        let factory = factory_for_test();
        let baseline = ToolServerConfig {
            tools: vec![
                test_support::tc("EchoAgentBuild:read_file", Some(ToolKind::Read)),
                test_support::tc("baseline.opaque", None),
            ],
            behavior_preset: None,
        };
        let mcp = vec![test_support::tc("mcp.opaque", None)];
        let filtered = merge_and_filter(
            &baseline,
            &mcp,
            &[],
            CapabilityMode::ReadOnly,
            "test_session",
        );
        let kept_ids: Vec<&str> = filtered.tools.iter().map(|t| t.id.as_str()).collect();
        assert!(
            kept_ids.contains(&"baseline.opaque"),
            "baseline kind: None must survive ReadOnly: {kept_ids:?}"
        );
        assert!(
            !kept_ids.contains(&"mcp.opaque"),
            "MCP kind: None MUST be dropped under ReadOnly: {kept_ids:?}"
        );
        assert!(
            kept_ids.contains(&"EchoAgentBuild:read_file"),
            "baseline Read kind must survive ReadOnly: {kept_ids:?}"
        );
        let _ = factory;
    }
    #[tokio::test]
    async fn resolve_session_toolset_mcp_kind_none_kept_under_all() {
        let baseline = ToolServerConfig {
            tools: vec![],
            behavior_preset: None,
        };
        let mcp = vec![test_support::tc("mcp.opaque", None)];
        let filtered = merge_and_filter(&baseline, &mcp, &[], CapabilityMode::All, "test_session");
        let kept_ids: Vec<&str> = filtered.tools.iter().map(|t| t.id.as_str()).collect();
        assert!(
            kept_ids.contains(&"mcp.opaque"),
            "All mode keeps MCP kind: None: {kept_ids:?}"
        );
    }
    #[tokio::test]
    async fn resolve_session_toolset_mcp_name_override_collision_skipped() {
        let baseline = ToolServerConfig {
            tools: vec![],
            behavior_preset: None,
        };
        let mut mcp_a = test_support::tc("mcp.tool_a", Some(ToolKind::Read));
        mcp_a.name_override = Some("shared_name".into());
        let mut mcp_b = test_support::tc("mcp.tool_b", Some(ToolKind::Read));
        mcp_b.name_override = Some("shared_name".into());
        let mcp = vec![mcp_a, mcp_b];
        let filtered = merge_and_filter(
            &baseline,
            &mcp,
            &[],
            CapabilityMode::ReadOnly,
            "test_session",
        );
        let ids: Vec<&str> = filtered.tools.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"mcp.tool_a"), "first wins: {ids:?}");
        assert!(
            !ids.contains(&"mcp.tool_b"),
            "duplicate name dropped: {ids:?}"
        );
    }
    #[test]
    fn hub_tool_merged_into_empty_baseline() {
        let baseline = ToolServerConfig {
            tools: vec![],
            behavior_preset: None,
        };
        let hub = vec![test_support::tc("hub:remote_exec", None)];
        let filtered = merge_and_filter(&baseline, &[], &hub, CapabilityMode::All, "test");
        let ids: Vec<&str> = filtered.tools.iter().map(|t| t.id.as_str()).collect();
        assert!(
            ids.contains(&"hub:remote_exec"),
            "remote tool should appear under All mode: {ids:?}"
        );
    }
    #[test]
    fn hub_tool_dropped_under_readonly_because_kind_none() {
        let baseline = ToolServerConfig {
            tools: vec![test_support::tc(
                "EchoAgentBuild:read_file",
                Some(ToolKind::Read),
            )],
            behavior_preset: None,
        };
        let hub = vec![test_support::tc("hub:remote_exec", None)];
        let filtered = merge_and_filter(&baseline, &[], &hub, CapabilityMode::ReadOnly, "test");
        let ids: Vec<&str> = filtered.tools.iter().map(|t| t.id.as_str()).collect();
        assert!(
            !ids.contains(&"hub:remote_exec"),
            "hub kind: None MUST be dropped under ReadOnly: {ids:?}"
        );
    }
    #[test]
    fn hub_tool_dedup_baseline_wins() {
        let baseline = ToolServerConfig {
            tools: vec![test_support::tc("hub:read_file", Some(ToolKind::Read))],
            behavior_preset: None,
        };
        let hub = vec![test_support::tc("hub:read_file", None)];
        let filtered = merge_and_filter(&baseline, &[], &hub, CapabilityMode::All, "test");
        let count = filtered
            .tools
            .iter()
            .filter(|t| t.id == "hub:read_file")
            .count();
        assert_eq!(count, 1, "duplicate should be deduped");
    }
    #[test]
    fn hub_tool_dedup_mcp_wins_over_hub() {
        let baseline = ToolServerConfig {
            tools: vec![],
            behavior_preset: None,
        };
        let mcp = vec![test_support::tc("hub:shared_tool", Some(ToolKind::Read))];
        let hub = vec![test_support::tc("hub:shared_tool", None)];
        let filtered = merge_and_filter(&baseline, &mcp, &hub, CapabilityMode::All, "test");
        let count = filtered
            .tools
            .iter()
            .filter(|t| t.id == "hub:shared_tool")
            .count();
        assert_eq!(count, 1, "MCP wins; hub duplicate skipped");
        let tool = filtered
            .tools
            .iter()
            .find(|t| t.id == "hub:shared_tool")
            .unwrap();
        assert_eq!(tool.kind, Some(ToolKind::Read));
    }
    #[test]
    fn hub_tool_name_collision_with_baseline_skipped() {
        let baseline = ToolServerConfig {
            tools: vec![test_support::tc(
                "EchoAgentBuild:read_file",
                Some(ToolKind::Read),
            )],
            behavior_preset: None,
        };
        let mut hub_tool = test_support::tc("hub:read_file_v2", None);
        hub_tool.name_override = Some("read_file".into());
        let hub = vec![hub_tool];
        let filtered = merge_and_filter(&baseline, &[], &hub, CapabilityMode::All, "test");
        let ids: Vec<&str> = filtered.tools.iter().map(|t| t.id.as_str()).collect();
        assert!(
            !ids.contains(&"hub:read_file_v2"),
            "remote tool with colliding client name must be skipped: {ids:?}"
        );
    }
    #[test]
    fn empty_hub_snapshot_is_noop() {
        let baseline = test_support::baseline_config();
        let baseline_ids: Vec<String> = baseline.tools.iter().map(|t| t.id.clone()).collect();
        let filtered = merge_and_filter(&baseline, &[], &[], CapabilityMode::ReadWrite, "test");
        let filtered_ids: Vec<String> = filtered.tools.iter().map(|t| t.id.clone()).collect();
        assert_eq!(filtered_ids, baseline_ids);
    }
    /// Only the literal `"true"` enables tool-state persistence.
    #[test]
    fn tool_state_enabled_only_true_enables() {
        let _guard = super::TOOL_STATE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TOOL_STATE_ENABLED";
        unsafe { std::env::remove_var(var) };
        assert!(!tool_state_enabled(), "unset → disabled");
        unsafe { std::env::set_var(var, "false") };
        assert!(!tool_state_enabled(), "false → disabled");
        unsafe { std::env::set_var(var, "1") };
        assert!(!tool_state_enabled(), "1 → disabled (only \"true\")");
        unsafe { std::env::set_var(var, "true") };
        assert!(tool_state_enabled(), "true → enabled");
        unsafe { std::env::remove_var(var) };
    }
    /// With a tool-state home set, state is rooted at
    /// `<home>/sessions/<session_id>/tool_state.json` and the dir is created.
    #[test]
    fn factory_resolves_session_keyed_state_path_when_home_set() {
        let home = tempfile::TempDir::new().unwrap();
        let factory =
            WorkspaceSessionContextFactory::new().with_tool_state_home(home.path().to_path_buf());
        let p = factory.resolve_state_path("sess-1");
        assert_eq!(
            p,
            home.path()
                .join("sessions")
                .join("sess-1")
                .join("tool_state.json")
        );
        assert!(
            home.path().join("sessions").join("sess-1").is_dir(),
            "the session dir must be created so the persistence writer can rename into it"
        );
    }
    /// Without a tool-state home, `state_path` stays empty (legacy behavior).
    #[test]
    fn factory_state_path_empty_when_home_unset() {
        let factory = WorkspaceSessionContextFactory::new();
        assert_eq!(factory.resolve_state_path("sess-1"), PathBuf::new());
    }
    #[test]
    fn factory_session_folder_is_tmp_sessions_not_project_cwd() {
        let cwd = PathBuf::from("/workspace");
        let folder = WorkspaceSessionContextFactory::resolve_session_folder("sess-1");
        let expected = PathBuf::from("/tmp/sessions/sess-1");
        assert_eq!(folder, expected);
        assert!(folder.is_dir());
        assert!(!folder.starts_with(&cwd));
        assert_eq!(
            folder.join("terminal").join("call-42.log"),
            PathBuf::from("/tmp/sessions/sess-1/terminal/call-42.log")
        );
    }
    #[test]
    fn factory_session_folder_sanitizes_and_isolates_ids() {
        let sessions = PathBuf::from("/tmp/sessions");
        let hostile = WorkspaceSessionContextFactory::resolve_session_folder("../../etc");
        assert!(hostile.starts_with(&sessions));
        assert_eq!(hostile.parent(), Some(sessions.as_path()));
        assert_ne!(hostile, sessions.join("etc"));
        let a = WorkspaceSessionContextFactory::resolve_session_folder("sess/1");
        let b = WorkspaceSessionContextFactory::resolve_session_folder("sess_1");
        assert_ne!(a, b);
        let empty = WorkspaceSessionContextFactory::resolve_session_folder("");
        assert_eq!(empty.parent(), Some(sessions.as_path()));
        assert!(
            empty
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|s| s.starts_with("anon-"))
        );
        let uuid = "019f3c0a-e2c2-79a3-908d-e8a8f088fe52";
        assert_eq!(
            WorkspaceSessionContextFactory::resolve_session_folder(uuid),
            sessions.join(uuid)
        );
    }
    #[test]
    fn ensure_session_dir_shared_by_state_and_session_folder() {
        let home = tempfile::TempDir::new().unwrap();
        let (under_home, ok) = ensure_session_dir(home.path(), "shared-id");
        assert!(ok.is_ok());
        assert_eq!(under_home, home.path().join("sessions").join("shared-id"));
        assert!(under_home.is_dir());
        let (under_tmp, ok) = ensure_session_dir(std::path::Path::new("/tmp"), "shared-id");
        assert!(ok.is_ok());
        assert_eq!(under_tmp, PathBuf::from("/tmp/sessions/shared-id"));
        assert!(under_tmp.is_dir());
    }
    /// A hostile `session_id` (`../../etc`) is sanitized to a single safe
    /// segment and cannot traverse outside `<home>/sessions/`.
    #[test]
    fn factory_sanitizes_malicious_session_id_no_traversal() {
        let home = tempfile::TempDir::new().unwrap();
        let factory =
            WorkspaceSessionContextFactory::new().with_tool_state_home(home.path().to_path_buf());
        let sessions = home.path().join("sessions");
        let p = factory.resolve_state_path("../../etc");
        assert!(
            p.starts_with(&sessions),
            "state path escaped sessions/: {}",
            p.display()
        );
        let session_dir = p.parent().expect("state path has a parent dir");
        assert_eq!(
            session_dir.parent(),
            Some(sessions.as_path()),
            "session dir must be a direct child of sessions/, got {}",
            session_dir.display()
        );
        let seg = session_dir
            .file_name()
            .and_then(|n| n.to_str())
            .expect("segment is valid utf-8");
        assert!(
            seg.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "sanitized segment must contain no path separators or dots: {seg:?}"
        );
        assert!(
            session_dir.is_dir(),
            "the confined session dir must be the only thing created"
        );
    }
    /// Sanitization is injective: distinct ids that substitute to the same
    /// base string still map to distinct directories (hash disambiguator),
    /// while already-safe ids map to themselves.
    #[test]
    fn sanitize_session_id_is_injective() {
        assert_eq!(super::sanitize_session_id("sess-1_a"), "sess-1_a");
        assert_ne!(
            super::sanitize_session_id("sess/1"),
            super::sanitize_session_id("sess_1"),
            "substitution collisions must be disambiguated"
        );
        assert_ne!(
            super::sanitize_session_id("sess/1"),
            super::sanitize_session_id("sess.1"),
        );
        assert!(super::sanitize_session_id("").starts_with("anon-"));
    }
    /// A toolset rebuilt for the SAME session rehydrates persisted state from
    /// disk; a DIFFERENT session_id cold-starts with no cross-contamination.
    #[tokio::test]
    async fn tool_state_rehydrates_same_session_and_cold_starts_other() {
        use echo_agent_tools::types::resources::{State, WebCitationCounter};
        let factory = test_support::TestSessionContextFactory::new();
        let cwd = PathBuf::from("/tmp");
        let (_eff, ts_a, _backend_a) = resolve_session_toolset(
            test_support::baseline_config(),
            CapabilityMode::ReadWrite,
            &[],
            &[],
            cwd.clone(),
            empty_env(),
            "sess-A",
            &factory,
            None,
            None,
            None,
            None,
        )
        .expect("build toolset A");
        {
            let mut res = ts_a.resources.lock().await;
            let counter = res.get_or_default::<State<WebCitationCounter>>();
            counter.counter = 123;
        }
        ts_a.save_and_flush_persistence()
            .await
            .expect("the test factory gives this session a state path");
        drop(ts_a);
        let (_eff, ts_b, _backend_b) = resolve_session_toolset(
            test_support::baseline_config(),
            CapabilityMode::ReadWrite,
            &[],
            &[],
            cwd.clone(),
            empty_env(),
            "sess-A",
            &factory,
            None,
            None,
            None,
            None,
        )
        .expect("build toolset B");
        {
            let res = ts_b.resources.lock().await;
            let counter = res
                .get::<State<WebCitationCounter>>()
                .expect("WebCitationCounter must be present after rehydration");
            assert_eq!(
                counter.counter, 123,
                "tool state must survive a rebuild for the same session (rehydration)"
            );
        }
        let (_eff, ts_c, _backend_c) = resolve_session_toolset(
            test_support::baseline_config(),
            CapabilityMode::ReadWrite,
            &[],
            &[],
            cwd,
            empty_env(),
            "sess-B",
            &factory,
            None,
            None,
            None,
            None,
        )
        .expect("build toolset C");
        {
            let res = ts_c.resources.lock().await;
            let contaminated = res
                .get::<State<WebCitationCounter>>()
                .is_some_and(|c| c.counter == 123);
            assert!(
                !contaminated,
                "a different session_id must cold-start, never inherit sess-A state"
            );
        }
    }
}
