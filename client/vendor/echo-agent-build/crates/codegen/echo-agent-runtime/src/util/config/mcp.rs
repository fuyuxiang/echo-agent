use agent_client_protocol as acp;
use anyhow::Result;
use echo_agent_core::prompt::skills::SkillsConfig;
use echo_agent_tools::types::compat::{CompatConfig, CompatConfigToml};
use indexmap::IndexMap;
use std::collections::HashMap;
use std::path::PathBuf;
use toml::Value as TomlValue;
use toml::map::Map as TomlMap;

pub use echo_agent_mcp::oauth_config::{McpOAuthConfig, McpOAuthConfigMap};
// MCP server config value types extracted to `echo-agent-config-types` (config
// dependency inversion); re-exported so `crate::util::config::*` paths keep working.
pub use echo_agent_config_types::{
    KNOWN_MCP_SERVER_FIELDS, McpJsonOAuthBlock, McpPreferenceSource, McpPreferencesFile,
    McpServerConfig, McpServerConfigProblem, McpServerPreferences, McpServerProblemSeverity,
    McpServerTransportConfig, McpSetupConfig, McpSetupDerivedValue, McpSetupField,
    McpSetupFieldType, McpSetupOption, McpSetupResolution,
};
// Permission-policy value types likewise extracted; re-exported to keep paths stable.
pub use echo_agent_config_types::{
    PatternMode, PermissionConfig, PermissionRule, RuleAction, ToolFilter,
};
// Relay-sync + MCP-config value types extracted; re-exported to keep paths stable.
pub use echo_agent_config_types::{McpConfig, RelaySyncConfig};
// Worktree-pool config value type extracted; re-exported to keep paths stable.
pub use echo_agent_config_types::PoolConfig;

/// TUI/CLI settings. Composed from typed section configs defined in `agent::config`.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub cli: crate::agent::config::CliConfig,
    pub models: crate::agent::config::ModelsConfig,
    pub ui: crate::agent::config::UiConfig,
    pub harness: crate::agent::config::HarnessConfig,
    pub skills: SkillsConfig,
    /// `[compat]` vendor-compatibility config, round-tripped so the
    /// pager preserves per-vendor toggles when persisting other settings.
    pub compat: CompatConfigToml,
    /// Management API key from `[endpoints]`.
    pub management_api_key: Option<String>,
    /// Permission policy rules loaded from `[permission]` section in config.toml.
    pub permission: Option<PermissionConfig>,
    pub diagnostics: crate::agent::config::DiagnosticsConfig,
    /// `[session]` section — round-tripped through `merge_section` so
    /// pager setters can persist session fields (e.g. auto-compact threshold).
    pub session: crate::agent::config::SessionConfig,
    /// `[toolset.ask_user_question]` sub-table — the only `[toolset]` piece
    /// the settings modal writes; the rest of `[toolset]` never round-trips
    /// (it carries runtime-only structs whose defaults must not hit disk).
    pub ask_user_question: crate::tools::config::AskUserQuestionToolConfig,
    /// `[privacy]` — local banner ack (not auth-metadata).
    pub privacy: PrivacyConfig,
    pub consent: super::consent::ConsentConfig,
    /// `[telemetry]` — only the key the pager persists round-trips.
    pub telemetry: TelemetryPersistConfig,
    /// `[features]` — only the key the pager persists round-trips.
    pub features: FeaturesPersistConfig,
}

/// The `[telemetry]` slice the pager is allowed to write back. Unmodeled
/// keys under `[telemetry]` are preserved by the deep merge in
/// `save_config_locked`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TelemetryPersistConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_upload: Option<bool>,
}

/// The `[features]` slice the pager is allowed to write back. Unmodeled
/// keys under `[features]` are preserved by the deep merge in
/// `save_config_locked`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct FeaturesPersistConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback_trace_card: Option<bool>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PrivacyConfig {
    /// Last banner dismiss (Accept/Customize), RFC 3339 UTC. None/0 remote
    /// `privacy_banner_reshow_days` = never re-show once set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_banner_acked: Option<String>,
}

pub(crate) fn get_mcp_server_config(name: &str) -> Option<McpServerConfig> {
    let root: TomlValue = crate::config::load_effective_config().ok()?;
    let configs = parse_mcp_servers_from_toml(&root);
    configs.get(name).cloned()
}

/// Get MCP server config by name, checking project-scoped configs first.
/// Walks from cwd up to the git repo root checking `.echo-agent/config.toml` at each level.
/// Project-scoped `.echo-agent/config.toml` entries override global `~/.echo-agent/config.toml`
/// entries entirely (no deep merge of individual fields).
/// Closer directories (cwd) take priority over further ones (repo root).
pub(crate) fn get_mcp_server_config_with_project(
    name: &str,
    cwd: &std::path::Path,
) -> Option<McpServerConfig> {
    // Check project-scoped configs from cwd (highest priority) to repo root
    let project_configs = crate::config::find_project_configs(cwd);
    for config_path in project_configs.iter().rev() {
        if let Ok(root) = crate::config::load_config_file(config_path) {
            let configs = parse_mcp_servers_from_toml(&root);
            if let Some(config) = configs.get(name) {
                return Some(config.clone());
            }
        }
    }

    // Fall back to global config
    get_mcp_server_config(name)
}

/// Scope tags for an MCP server definition. Single source of truth shared by
/// the scope producers ([`mcp_server_scope`],
/// [`load_mcp_server_configs_with_project`]) and the folder-trust gate that
/// filters project-scoped names, so a retag can't silently desync the gate.
/// `MCP_SCOPE_PROJECT` is `pub(crate)` for the gate consumer in `folder_trust`;
/// `MCP_SCOPE_USER` stays private (only used here).
pub(crate) const MCP_SCOPE_PROJECT: &str = "project";
const MCP_SCOPE_USER: &str = "user";

/// Scope an MCP server resolves at: project when defined in any project-scoped
/// `.echo-agent/config.toml`, otherwise user (global config, `~/.claude.json`,
/// `~/.cursor/mcp.json`, etc.). See [`MCP_SCOPE_PROJECT`] / `MCP_SCOPE_USER`.
pub(crate) fn mcp_server_scope(name: &str, cwd: &std::path::Path) -> &'static str {
    for config_path in crate::config::find_project_configs(cwd) {
        if let Ok(root) = crate::config::load_config_file(&config_path)
            && parse_mcp_servers_from_toml(&root).contains_key(name)
        {
            return MCP_SCOPE_PROJECT;
        }
    }
    MCP_SCOPE_USER
}

/// Load MCP servers and their OAuth configurations from config.toml.
///
/// Returns both the `acp::McpServer` list and a parallel [`McpOAuthConfigMap`].
pub(crate) fn load_mcp_servers_with_oauth(
    cwd: &std::path::Path,
    compat: &CompatConfig,
) -> (Vec<acp::McpServer>, McpOAuthConfigMap) {
    // Read the same effective config that `load_mcp_servers` /
    // `reload_mcp_servers_merged` start servers from, so the server list and its
    // parallel OAuth map derive from one snapshot. This aligns the OAuth map
    // with the effective-config server list, so a managed- or
    // requirements-defined server cannot start without its OAuth client
    // settings. (The overlay is stripped of `mcp_servers`, so it never
    // contributes a server here.)
    let global_config = crate::config::load_effective_config()
        .unwrap_or_else(|_| TomlValue::Table(toml::map::Map::new()));

    let mut servers_map: IndexMap<String, McpServerConfig> = IndexMap::new();
    for (name, config) in parse_mcp_servers_from_toml(&global_config) {
        servers_map.insert(name, config);
    }

    let project_configs = crate::config::find_project_configs(cwd);
    for config_path in &project_configs {
        if let Ok(root) = crate::config::load_config_file(config_path) {
            for (name, config) in parse_mcp_servers_from_toml(&root) {
                servers_map.insert(name, config);
            }
        }
    }
    // Also load from ~/.claude.json (lower priority than TOML)
    for (name, config) in load_claude_json_mcp_servers_as_configs(cwd, compat) {
        servers_map.entry(name).or_insert(config);
    }

    // Also load from ~/.cursor/mcp.json (lower priority than TOML and ~/.claude.json)
    for (name, config) in load_cursor_mcp_servers_as_configs(cwd, compat) {
        servers_map.entry(name).or_insert(config);
    }

    // Also load from .mcp.json files (lower priority than TOML, ~/.claude.json, and ~/.cursor)
    for (name, config) in load_mcp_json_servers_as_configs(cwd) {
        servers_map.entry(name).or_insert(config);
    }

    let mut oauth_configs = McpOAuthConfigMap::new();
    let mut acp_servers = Vec::new();

    let preferences = load_mcp_preferences().file();
    let sub = &crate::config::expand_env_vars_in_string;
    for (name, config) in servers_map {
        let mut config = match config.resolve_setup(preferences.servers.get(&name)) {
            McpSetupResolution::Resolved(config) => config,
            McpSetupResolution::Required(_) => continue,
            McpSetupResolution::Invalid(reason) => {
                tracing::warn!(server = %name, error = %reason, "MCP setup config is invalid");
                continue;
            }
        };
        config.expand_strings(sub);
        if let Some(oauth) = config.oauth_config() {
            oauth_configs.insert(name.clone(), oauth);
        }
        if let Some(acp_server) = config.to_acp_mcp_server(name) {
            acp_servers.push(acp_server);
        }
    }

    (acp_servers, oauth_configs)
}

/// Load the worktree pool configuration from config.toml.
/// Returns the default config if the section is missing.
pub fn worktree_pool_from_toml(root: &TomlValue) -> PoolConfig {
    if let TomlValue::Table(table) = root
        && let Some(pool_val) = table.get("worktree_pool")
    {
        // Try to deserialize the section; fall back to defaults on error
        pool_val
            .clone()
            .try_into::<PoolConfig>()
            .unwrap_or_default()
    } else {
        PoolConfig::default()
    }
}

/// Load MCP servers with project-scoped overrides from `.echo-agent/config.toml`.
///
/// Merge strategy:
/// 1. Load MCP servers from global `~/.echo-agent/config.toml`
/// 2. Walk from git repo root down to `cwd`, loading `.echo-agent/config.toml` at each level
///    (matching the convention used by skills and AGENTS.md discovery)
/// 3. Each level's entries replace entries with the same name entirely
///    (no deep merge — omitted fields fall back to defaults)
/// 4. Closer directories (cwd) take priority over further ones (repo root)
pub fn load_mcp_servers(cwd: &std::path::Path, compat: &CompatConfig) -> Vec<acp::McpServer> {
    let global_config = crate::config::load_effective_config()
        .unwrap_or_else(|_| TomlValue::Table(toml::map::Map::new()));
    reload_mcp_servers_merged(&global_config, cwd, compat)
}

/// Load MCP servers from config.toml only (global + project-scoped), without
/// loading from `~/.claude.json`, `~/.cursor/mcp.json`, or
/// `.mcp.json` sources.
///
/// Used by [`crate::session::managed_mcp::merge_managed_mcp_servers_sourced`]
/// which handles those non-TOML sources separately with proper `ConfigSource`
/// tracking. Using [`load_mcp_servers`] there would cause all entries to be
/// tagged as `ConfigSource::ConfigToml`, hiding the true origin.
pub(crate) fn load_mcp_servers_toml_only(cwd: &std::path::Path) -> Vec<acp::McpServer> {
    let preferences = load_mcp_preferences().file();
    let sub = &crate::config::expand_env_vars_in_string;
    load_all_mcp_configs(cwd)
        .into_iter()
        .filter_map(|(name, config)| {
            materialize_mcp_config(&name, config, &preferences, sub, McpEnabledFilter::Respect)
        })
        .collect()
}

/// Whether materialization respects the config `enabled` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpEnabledFilter {
    /// Drop when `enabled = false` (live merge / load).
    Respect,
    /// Materialize even when `enabled = false` (list stubs / reenable probe).
    Ignore,
}

/// Resolve setup, expand strings, and convert to ACP.
pub(crate) fn materialize_mcp_config(
    name: &str,
    mut config: McpServerConfig,
    preferences: &McpPreferencesFile,
    sub: &dyn Fn(&str) -> String,
    enabled_filter: McpEnabledFilter,
) -> Option<acp::McpServer> {
    if matches!(enabled_filter, McpEnabledFilter::Ignore) {
        config.enabled = true;
    }
    let mut config = match config.resolve_setup(preferences.servers.get(name)) {
        McpSetupResolution::Resolved(config) => config,
        McpSetupResolution::Required(_) => return None,
        McpSetupResolution::Invalid(reason) => {
            tracing::warn!(server = %name, error = %reason, "MCP setup config is invalid");
            return None;
        }
    };
    config.expand_strings(sub);
    config.to_acp_mcp_server(name)
}

/// Merge MCP servers from a pre-parsed global config with project-scoped overrides.
///
/// Same merge strategy as [`load_mcp_servers_with_project`] but takes the global
/// config as a pre-parsed `toml::Value` instead of re-reading from disk. Project
/// configs are still read from disk because the watcher signals paths, not content.
pub(crate) fn reload_mcp_servers_merged(
    global_config: &TomlValue,
    cwd: &std::path::Path,
    compat: &CompatConfig,
) -> Vec<acp::McpServer> {
    let mut servers: IndexMap<String, McpServerConfig> = IndexMap::new();

    for (name, config) in parse_mcp_servers_from_toml(global_config) {
        servers.insert(name, config);
    }

    let project_configs = crate::config::find_project_configs(cwd);
    for config_path in &project_configs {
        if let Ok(root) = crate::config::load_config_file(config_path) {
            let project_servers = parse_mcp_servers_from_toml(&root);
            if !project_servers.is_empty() {
                tracing::info!(
                    count = project_servers.len(),
                    path = %config_path.display(),
                    "Loaded project-scoped MCP servers from .echo-agent/config.toml"
                );
                for (name, config) in project_servers {
                    servers.insert(name, config);
                }
            }
        }
    }
    // Also load from ~/.claude.json (lower priority than TOML)
    let claude_servers = load_claude_json_mcp_servers_as_configs(cwd, compat);
    tracing::info!(
        count = claude_servers.len(),
        "Loaded MCP servers from ~/.claude.json"
    );
    for (name, config) in claude_servers {
        servers.entry(name).or_insert(config);
    }

    // Also load from ~/.cursor/mcp.json (lower priority than TOML and ~/.claude.json)
    let cursor_servers = load_cursor_mcp_servers_as_configs(cwd, compat);
    tracing::info!(
        count = cursor_servers.len(),
        "Loaded Cursor MCP servers from ~/.cursor/mcp.json"
    );
    for (name, config) in cursor_servers {
        servers.entry(name).or_insert(config);
    }

    // Also load from .mcp.json files (lower priority than TOML)
    let mcp_json_servers = load_mcp_json_servers_as_configs(cwd);
    tracing::info!(
        count = mcp_json_servers.len(),
        "Loaded .mcp.json MCP servers"
    );
    for (name, config) in mcp_json_servers {
        servers.entry(name).or_insert(config);
    }

    let preferences = load_mcp_preferences().file();
    let sub = &crate::config::expand_env_vars_in_string;
    servers
        .into_iter()
        .filter_map(|(name, config)| {
            let mut config = match config.resolve_setup(preferences.servers.get(&name)) {
                McpSetupResolution::Resolved(config) => config,
                McpSetupResolution::Required(_) => return None,
                McpSetupResolution::Invalid(reason) => {
                    tracing::warn!(server = %name, error = %reason, "MCP setup config is invalid");
                    return None;
                }
            };
            config.expand_strings(sub);
            config.to_acp_mcp_server(name)
        })
        .collect()
}

/// Load `.mcp.json` servers from repo root to `cwd` (closest wins on name conflict).
pub(crate) fn load_mcp_json_servers(cwd: &std::path::Path) -> Vec<acp::McpServer> {
    // Phase 2 cutoff: if the user has imported, skip reading .mcp.json.
    if crate::claude_import::is_claude_import_marked_with_log("load_mcp_json_servers") {
        return vec![];
    }

    let mcp_json_files = find_mcp_json_files(cwd);
    if mcp_json_files.is_empty() {
        return vec![];
    }

    let mut result = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    // Reverse so cwd entries win on name conflict.
    for mcp_path in mcp_json_files.iter().rev() {
        let json_servers = load_mcp_json_file(mcp_path);
        for server in json_servers {
            let name = match &server {
                acp::McpServer::Http(acp::McpServerHttp { name, .. })
                | acp::McpServer::Sse(acp::McpServerSse { name, .. })
                | acp::McpServer::Stdio(acp::McpServerStdio { name, .. }) => name.clone(),
                // TODO(acp-0.10): `McpServer` is #[non_exhaustive].
                _ => continue,
            };
            if seen_names.insert(name) {
                result.push(server);
            }
        }
    }

    result
}

/// All server names from config.toml (including `enabled = false`).
pub(crate) fn all_toml_mcp_server_names(
    cwd: &std::path::Path,
) -> std::collections::HashSet<String> {
    load_all_mcp_configs(cwd).keys().cloned().collect()
}

pub(crate) fn mcp_preferences_path() -> PathBuf {
    echo_agent_config::echo_agent_home().join("mcp_preferences.json")
}

/// Result of loading prefs. Corrupt files are readable as empty for resolution
/// but must not be overwritten (would clobber other servers).
#[derive(Debug, Clone)]
pub(crate) enum McpPreferencesLoad {
    Ok(McpPreferencesFile),
    Missing,
    Corrupt,
}

impl McpPreferencesLoad {
    pub(crate) fn file(&self) -> McpPreferencesFile {
        match self {
            Self::Ok(f) => f.clone(),
            Self::Missing | Self::Corrupt => McpPreferencesFile::default(),
        }
    }

    pub(crate) fn is_writable(&self) -> bool {
        !matches!(self, Self::Corrupt)
    }
}

pub(crate) fn load_mcp_preferences() -> McpPreferencesLoad {
    load_mcp_preferences_from(&mcp_preferences_path())
}

pub(crate) fn load_mcp_preferences_from(path: &std::path::Path) -> McpPreferencesLoad {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return McpPreferencesLoad::Missing,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read MCP preferences");
            return McpPreferencesLoad::Corrupt;
        }
    };
    match serde_json::from_str(&content) {
        Ok(file) => McpPreferencesLoad::Ok(file),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to parse MCP preferences");
            McpPreferencesLoad::Corrupt
        }
    }
}

pub(crate) async fn save_mcp_preferences(prefs: &McpPreferencesFile) -> Result<()> {
    save_mcp_preferences_to(&mcp_preferences_path(), prefs).await
}

pub(crate) async fn save_mcp_preferences_to(
    path: &std::path::Path,
    prefs: &McpPreferencesFile,
) -> Result<()> {
    if matches!(load_mcp_preferences_from(path), McpPreferencesLoad::Corrupt) {
        anyhow::bail!(
            "refusing to overwrite unreadable MCP preferences at {}",
            path.display()
        );
    }
    let json = serde_json::to_string_pretty(prefs)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_extension(format!(
        "json.tmp.{}{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    tokio::fs::write(&tmp, &json).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|e| anyhow::anyhow!("failed to set mcp preferences permissions: {e}"))?;
    }
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

/// Restore a single server key after a failed setup (best-effort).
pub(crate) async fn restore_mcp_preference_server(
    server_name: &str,
    previous: Option<McpServerPreferences>,
) -> Result<()> {
    let load = load_mcp_preferences();
    if !load.is_writable() {
        return Ok(());
    }
    let mut prefs = load.file();
    match previous {
        Some(entry) => {
            prefs.servers.insert(server_name.to_string(), entry);
        }
        None => {
            prefs.servers.remove(server_name);
        }
    }
    save_mcp_preferences(&prefs).await
}

/// Unresolved setup-bearing MCP config collected for `/mcps` list and auth.
#[derive(Debug, Clone)]
pub struct McpSetupServerEntry {
    pub name: String,
    pub config: McpServerConfig,
    pub source: McpPreferenceSource,
}

/// Collect MCP configs that declare a `setup` schema from config and plugins.
/// Used to surface setup-required rows and drive `echo.agent/mcp/setup`.
///
/// User/project **TOML** includes `enabled = false` so Space-disabled setup
/// servers stay visible (`handle_list` derives `session.enabled` from
/// `disabled_mcp_servers`). Other sources still skip native `enabled = false`
/// because Space cannot unstick those flags.
pub(crate) fn collect_mcp_setup_configs(
    cwd: &std::path::Path,
    plugin_registry: Option<&echo_agent_core::plugins::PluginRegistry>,
    compat: &CompatConfig,
) -> IndexMap<String, McpSetupServerEntry> {
    let mut result = IndexMap::new();
    for (name, (config, scope)) in load_mcp_server_configs_with_project(cwd) {
        if config.setup.is_none() {
            continue;
        }
        result.insert(
            name.clone(),
            McpSetupServerEntry {
                name,
                config,
                source: McpPreferenceSource {
                    kind: "config".to_string(),
                    plugin: None,
                    scope: Some(scope.to_string()),
                },
            },
        );
    }
    if !crate::claude_import::is_claude_import_marked_with_log("collect_mcp_setup_configs") {
        for (name, config) in load_claude_json_mcp_servers_as_configs(cwd, compat) {
            if !config.enabled || config.setup.is_none() {
                continue;
            }
            result.entry(name.clone()).or_insert(McpSetupServerEntry {
                name,
                config,
                source: McpPreferenceSource {
                    kind: "config".to_string(),
                    plugin: None,
                    scope: Some(MCP_SCOPE_USER.to_string()),
                },
            });
        }
        for (name, config) in load_cursor_mcp_servers_as_configs(cwd, compat) {
            if !config.enabled || config.setup.is_none() {
                continue;
            }
            result.entry(name.clone()).or_insert(McpSetupServerEntry {
                name,
                config,
                source: McpPreferenceSource {
                    kind: "config".to_string(),
                    plugin: None,
                    scope: Some(MCP_SCOPE_USER.to_string()),
                },
            });
        }
        for (name, config) in load_mcp_json_servers_as_configs(cwd) {
            if !config.enabled || config.setup.is_none() {
                continue;
            }
            result.entry(name.clone()).or_insert(McpSetupServerEntry {
                name,
                config,
                source: McpPreferenceSource {
                    kind: "config".to_string(),
                    plugin: None,
                    scope: Some(MCP_SCOPE_PROJECT.to_string()),
                },
            });
        }
    }
    if let Some(registry) = plugin_registry {
        let toml_claimed_names = all_toml_mcp_server_names(cwd);
        for plugin in registry.active_plugins() {
            // File first, then inline; first-wins matches runtime plugin load.
            let mut plugin_configs = IndexMap::new();
            if let Some(ref mcp_path) = plugin.mcp_config_path
                && let Some(config) = read_mcp_json(mcp_path)
            {
                for (name, server) in config.mcp_servers {
                    plugin_configs.entry(name).or_insert(server);
                }
            }
            if let Some(ref inline_value) = plugin.inline_mcp_servers {
                let normalized =
                    echo_agent_core::plugins::manifest::normalize_inline_mcp_servers(inline_value);
                for (name, server) in mcp_config_from_json_value(&normalized).mcp_servers {
                    plugin_configs.entry(name).or_insert(server);
                }
            }
            for (name, config) in plugin_configs {
                // Native plugin `enabled: false` is not unstuck by Space; skip.
                // Personal disable uses `disabled_mcp_servers` and leaves this true.
                if toml_claimed_names.contains(&name) || !config.enabled || config.setup.is_none() {
                    continue;
                }
                result.entry(name.clone()).or_insert(McpSetupServerEntry {
                    name,
                    config,
                    source: McpPreferenceSource {
                        kind: "plugin".to_string(),
                        plugin: Some(plugin.name.clone()),
                        scope: None,
                    },
                });
            }
        }
    }
    result
}

pub const MANAGED_GATEWAY_DISABLED_CONNECTORS_KEY: &str = "__managed_gateway_connectors";

/// Persist `disabled_tools` for a server under `[disabled_mcp_tools]` in config.toml.
///
/// Uses a dedicated top-level section (not `[mcp_servers]`) to avoid creating
/// incomplete server entries that fail to deserialize for managed servers.
pub(crate) async fn save_mcp_disabled_tools(
    server_name: &str,
    disabled_tools: &[String],
) -> Result<()> {
    let path = config_path();
    let _save_guard = super::persist::lock_config_writes().await;
    let _transaction = super::persist::acquire_config_transaction_lock_at(&path)?;
    let original = super::persist::read_to_string_or_empty(&path)?;
    let mut root: TomlValue = if original.is_empty() {
        TomlValue::Table(TomlMap::new())
    } else {
        toml::from_str(&original).map_err(|error| {
            anyhow::anyhow!(
                "refusing to overwrite unparseable {}: {error}",
                path.display()
            )
        })?
    };
    let table = root
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("config root is not a table"))?;

    let section = table
        .entry("disabled_mcp_tools")
        .or_insert_with(|| TomlValue::Table(TomlMap::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("disabled_mcp_tools is not a table"))?;

    if disabled_tools.is_empty() {
        section.remove(server_name);
        if section.is_empty() {
            table.remove("disabled_mcp_tools");
        }
    } else {
        let arr = disabled_tools
            .iter()
            .map(|s| TomlValue::String(s.clone()))
            .collect();
        section.insert(server_name.to_string(), TomlValue::Array(arr));
    }

    let toml_str = toml::to_string_pretty(&root)?;
    super::persist::atomic_write_string(&path, &toml_str)?;
    Ok(())
}

/// Like [`save_mcp_server_enabled`], with explicit cwd for project config walks.
///
/// On enable, if the project unstick fails after a successful user-tier write,
/// rolls back whatever was already written and returns the error so callers do
/// not invent a personal disable that was never part of the prior state.
pub async fn save_mcp_server_enabled_in(
    server_name: &str,
    enabled: bool,
    cwd: &std::path::Path,
) -> Result<Vec<PathBuf>> {
    let mut modified = Vec::new();

    let user_path = config_path();
    if write_toml_table_if_changed(&user_path, |table| {
        apply_mcp_server_enabled(table, server_name, enabled);
    })
    .await?
    {
        modified.push(user_path);
    }

    // Enable-only: unstick the nearest (winning) project def with
    // `enabled = false` via toml_edit (preserves comments/layout). Disable is
    // personal (user `disabled_mcp_servers`) and must not dirty shared files.
    if enabled && let Some(path) = nearest_project_mcp_definition(cwd, server_name) {
        match clear_sticky_project_disabled_at(&path, server_name).await {
            Ok(true) => modified.push(path),
            Ok(false) => {}
            Err(e) => {
                if let Err(re) =
                    restore_mcp_server_enabled_after_enable(server_name, &modified).await
                {
                    tracing::warn!(
                        server = server_name,
                        error = %re,
                        "failed to roll back partial enable after project unstick error"
                    );
                }
                return Err(e);
            }
        }
    }

    Ok(modified)
}

/// User config only — no project unstick.
///
/// Use after delete (or similar) when the toggle path dirtied
/// `disabled_mcp_servers` but shared project configs must stay untouched.
pub(crate) async fn save_user_mcp_server_enabled(server_name: &str, enabled: bool) -> Result<()> {
    write_toml_table_if_changed(&config_path(), |table| {
        apply_mcp_server_enabled(table, server_name, enabled);
    })
    .await
    .map(|_| ())
}

/// Undo a prior [`save_mcp_server_enabled_in`]`(…, true, …)` using the paths
/// that call returned. Restores only the tiers that were written.
///
/// Restores an **equivalent** disabled state, not necessarily the original
/// encoding. User-tier restore always goes through
/// [`save_user_mcp_server_enabled`]`(…, false)` (personal `disabled_mcp_servers`);
/// project-tier restore re-sticks `enabled = false` via toml_edit. A server that
/// was disabled only via a sticky project field may therefore pick up a personal
/// list entry if the user tier was also written during enable.
pub(crate) async fn restore_mcp_server_enabled_after_enable(
    server_name: &str,
    modified_paths: &[PathBuf],
) -> Result<()> {
    let user_path = config_path();
    for path in modified_paths {
        if path == user_path.as_path() {
            save_user_mcp_server_enabled(server_name, false).await?;
        } else {
            set_sticky_project_disabled_at(path, server_name).await?;
        }
    }
    Ok(())
}

/// Nearest project config defining `server_name` (cwd last → reverse; nearest wins).
fn nearest_project_mcp_definition(cwd: &std::path::Path, server_name: &str) -> Option<PathBuf> {
    crate::config::find_project_configs(cwd)
        .into_iter()
        .rev()
        .find(|path| mcp_server_defined_at(path, server_name))
}

/// Apply `f`, write only if the serialized table changed. Returns whether written.
///
/// Aligns with [`super::persist::save_config`] safety: refuse unparseable
/// files (no wipe-to-empty), unique tmp + mode preserve via
/// [`super::persist::atomic_write_string`], and the user-config write lock.
async fn write_toml_table_if_changed(
    path: &std::path::Path,
    f: impl FnOnce(&mut TomlMap<String, TomlValue>),
) -> Result<bool> {
    let is_user = path == config_path().as_path();
    let _guard = if is_user {
        Some(super::persist::lock_config_writes().await)
    } else {
        None
    };
    let _transaction = super::persist::acquire_config_transaction_lock_at(path)?;

    let original = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && is_user => String::new(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            return Err(anyhow::anyhow!("failed to read {}: {e}", path.display()));
        }
    };
    let mut root: TomlValue = if original.is_empty() {
        TomlValue::Table(TomlMap::new())
    } else {
        match toml::from_str(&original) {
            Ok(v) => v,
            Err(parse_err) => {
                return Err(anyhow::anyhow!(
                    "refusing to overwrite unparseable {}: {}; fix the syntax before retrying",
                    path.display(),
                    parse_err
                ));
            }
        }
    };
    let table = root
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("config root is not a table"))?;
    f(table);
    let toml_str = toml::to_string_pretty(&root)?;
    // Normalize empty original so first enable/disable still writes when needed.
    let before = if original.is_empty() {
        toml::to_string_pretty(&TomlValue::Table(TomlMap::new()))?
    } else {
        // Re-serialize original for stable comparison (ignore formatting noise).
        match toml::from_str::<TomlValue>(&original) {
            Ok(v) => toml::to_string_pretty(&v).unwrap_or(original),
            Err(_) => original,
        }
    };
    if before == toml_str {
        return Ok(false);
    }
    super::persist::atomic_write_string(path, &toml_str)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    Ok(true)
}

/// Flip sticky project `enabled = false` → true with toml_edit (comments kept).
async fn clear_sticky_project_disabled_at(
    path: &std::path::Path,
    server_name: &str,
) -> Result<bool> {
    let _transaction = super::persist::acquire_config_transaction_lock_at(path)?;
    let original = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            return Err(anyhow::anyhow!("failed to read {}: {e}", path.display()));
        }
    };
    let mut doc: toml_edit::DocumentMut = original
        .parse()
        .map_err(|e| anyhow::anyhow!("refusing to rewrite unparseable {}: {e}", path.display()))?;

    let Some(servers) = doc
        .get_mut("mcp_servers")
        .and_then(|item| item.as_table_like_mut())
    else {
        return Ok(false);
    };
    let Some(entry) = servers.get_mut(server_name) else {
        return Ok(false);
    };
    let Some(server_table) = entry.as_table_like_mut() else {
        return Ok(false);
    };
    if server_table.get("enabled").and_then(|v| v.as_bool()) != Some(false) {
        return Ok(false);
    }
    server_table.insert("enabled", toml_edit::value(true));

    let updated = doc.to_string();
    if updated == original {
        return Ok(false);
    }
    super::persist::atomic_write_string(path, &updated)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    Ok(true)
}

/// Flip sticky project `enabled = true` → false with toml_edit (comments kept).
/// Inverse of [`clear_sticky_project_disabled_at`].
async fn set_sticky_project_disabled_at(path: &std::path::Path, server_name: &str) -> Result<bool> {
    let _transaction = super::persist::acquire_config_transaction_lock_at(path)?;
    let original = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            return Err(anyhow::anyhow!("failed to read {}: {e}", path.display()));
        }
    };
    let mut doc: toml_edit::DocumentMut = original
        .parse()
        .map_err(|e| anyhow::anyhow!("refusing to rewrite unparseable {}: {e}", path.display()))?;

    let Some(servers) = doc
        .get_mut("mcp_servers")
        .and_then(|item| item.as_table_like_mut())
    else {
        return Ok(false);
    };
    let Some(entry) = servers.get_mut(server_name) else {
        return Ok(false);
    };
    let Some(server_table) = entry.as_table_like_mut() else {
        return Ok(false);
    };
    if server_table.get("enabled").and_then(|v| v.as_bool()) != Some(true) {
        return Ok(false);
    }
    server_table.insert("enabled", toml_edit::value(false));

    let updated = doc.to_string();
    if updated == original {
        return Ok(false);
    }
    super::persist::atomic_write_string(path, &updated)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    Ok(true)
}

/// Update user `disabled_mcp_servers` and, if present, per-server `enabled`.
fn apply_mcp_server_enabled(
    table: &mut TomlMap<String, TomlValue>,
    server_name: &str,
    enabled: bool,
) {
    let mut disabled_list: Vec<String> = table
        .get("disabled_mcp_servers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if enabled {
        disabled_list.retain(|n| n != server_name);
    } else if !disabled_list.contains(&server_name.to_string()) {
        disabled_list.push(server_name.to_string());
    }

    if disabled_list.is_empty() {
        table.remove("disabled_mcp_servers");
    } else {
        let arr = disabled_list
            .iter()
            .map(|s| TomlValue::String(s.clone()))
            .collect();
        table.insert("disabled_mcp_servers".to_string(), TomlValue::Array(arr));
    }

    set_mcp_server_enabled_field(table, server_name, enabled);
}

fn set_mcp_server_enabled_field(
    table: &mut TomlMap<String, TomlValue>,
    server_name: &str,
    enabled: bool,
) {
    if let Some(servers) = table.get_mut("mcp_servers").and_then(|v| v.as_table_mut())
        && let Some(entry) = servers.get_mut(server_name)
        && let Some(server_table) = entry.as_table_mut()
    {
        server_table.insert("enabled".to_string(), TomlValue::Boolean(enabled));
    }
}

/// Upsert an MCP server entry in `~/.echo-agent/config.toml`.
///
/// Creates or replaces `[mcp_servers.<name>]` with the given config.
/// Also removes the server from `disabled_mcp_servers` if present (a newly
/// defined server should start enabled).
pub(crate) async fn save_mcp_server_config(
    server_name: &str,
    config: &McpServerConfig,
) -> Result<()> {
    save_mcp_server_config_at(&config_path(), server_name, config).await
}

/// Upsert an MCP server entry in the config file at `path`.
///
/// Same semantics as [`save_mcp_server_config`] but targets an explicit
/// config file, e.g. a project-scoped `.echo-agent/config.toml`.
pub async fn save_mcp_server_config_at(
    path: &std::path::Path,
    server_name: &str,
    config: &McpServerConfig,
) -> Result<()> {
    let is_user = path == config_path().as_path();
    let _save_guard = if is_user {
        Some(super::persist::lock_config_writes().await)
    } else {
        None
    };
    let _transaction = super::persist::acquire_config_transaction_lock_at(path)?;
    let original = super::persist::read_to_string_or_empty(path)?;
    let mut root: TomlValue = if original.is_empty() {
        TomlValue::Table(TomlMap::new())
    } else {
        toml::from_str(&original).map_err(|error| {
            anyhow::anyhow!(
                "refusing to overwrite unparseable {}: {error}",
                path.display()
            )
        })?
    };
    let table = root
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("config root is not a table"))?;

    let servers = table
        .entry("mcp_servers")
        .or_insert_with(|| TomlValue::Table(TomlMap::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("mcp_servers is not a table"))?;

    let serialized = toml::Value::try_from(config)
        .map_err(|e| anyhow::anyhow!("failed to serialize MCP server config: {e}"))?;
    servers.insert(server_name.to_string(), serialized);

    // Ensure the server isn't in the disabled list.
    if let Some(arr) = table
        .get_mut("disabled_mcp_servers")
        .and_then(|v| v.as_array_mut())
    {
        arr.retain(|v| v.as_str() != Some(server_name));
        if arr.is_empty() {
            table.remove("disabled_mcp_servers");
        }
    }

    let toml_str = toml::to_string_pretty(&root)?;
    super::persist::atomic_write_string(path, &toml_str)?;
    Ok(())
}

/// Delete an MCP server entry from `~/.echo-agent/config.toml`.
///
/// Removes `[mcp_servers.<name>]`, cleans up `disabled_mcp_servers` and
/// `[disabled_mcp_tools.<name>]` entries. Returns `true` if the entry existed.
pub(crate) async fn delete_mcp_server_config(server_name: &str) -> Result<bool> {
    delete_mcp_server_config_at(&config_path(), server_name).await
}

/// Delete an MCP server entry from the config file at `path`.
///
/// Same semantics as [`delete_mcp_server_config`] but targets an explicit
/// config file, e.g. a project-scoped `.echo-agent/config.toml`.
pub async fn delete_mcp_server_config_at(
    path: &std::path::Path,
    server_name: &str,
) -> Result<bool> {
    let is_user = path == config_path().as_path();
    let _save_guard = if is_user {
        Some(super::persist::lock_config_writes().await)
    } else {
        None
    };
    let _transaction = super::persist::acquire_config_transaction_lock_at(path)?;
    let original = match super::persist::read_to_string_or_empty(path) {
        Ok(content) if content.is_empty() => return Ok(false),
        Ok(content) => content,
        Err(error) => return Err(error.into()),
    };
    let mut root: TomlValue = toml::from_str(&original).map_err(|error| {
        anyhow::anyhow!(
            "refusing to overwrite unparseable {}: {error}",
            path.display()
        )
    })?;
    let table = root
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("config root is not a table"))?;

    let removed = table
        .get_mut("mcp_servers")
        .and_then(|v| v.as_table_mut())
        .and_then(|servers| servers.remove(server_name));

    let Some(removed) = removed else {
        return Ok(false);
    };

    // Clean up empty mcp_servers table.
    if table
        .get("mcp_servers")
        .and_then(|v| v.as_table())
        .is_some_and(|t| t.is_empty())
    {
        table.remove("mcp_servers");
    }

    // Remove from disabled_mcp_servers list.
    if let Some(arr) = table
        .get_mut("disabled_mcp_servers")
        .and_then(|v| v.as_array_mut())
    {
        arr.retain(|v| v.as_str() != Some(server_name));
        if arr.is_empty() {
            table.remove("disabled_mcp_servers");
        }
    }

    // Remove disabled_mcp_tools entry.
    if let Some(section) = table
        .get_mut("disabled_mcp_tools")
        .and_then(|v| v.as_table_mut())
    {
        section.remove(server_name);
        if section.is_empty() {
            table.remove("disabled_mcp_tools");
        }
    }

    let toml_str = toml::to_string_pretty(&root)?;
    super::persist::atomic_write_string(path, &toml_str)?;

    if let Ok(config) = removed.try_into::<McpServerConfig>()
        && let Err(error) = remove_mcp_server_credentials(server_name, &config)
    {
        tracing::warn!(%error, server_name, "could not remove MCP credentials");
    }

    Ok(true)
}

/// Resolve the deleted definition just as at startup, then remove its exact
/// credential key under the credential store's cross-process lock.
pub fn remove_mcp_server_credentials(server_name: &str, config: &McpServerConfig) -> Result<()> {
    let preferences = load_mcp_preferences().file();
    let McpSetupResolution::Resolved(mut config) = config.resolve_setup(preferences.servers.get(server_name)) else {
        return Ok(());
    };
    config.expand_strings(&crate::config::expand_env_vars_in_string);
    if let McpServerTransportConfig::StreamableHttp { url, .. } = &config.transport
        && let Ok(url) = url::Url::parse(url)
    {
        let mut store = echo_agent_mcp::credentials::McpCredentialStore::load_default()?;
        if store.has_credentials(server_name, &url) {
            store.remove_and_save(server_name, &url)?;
        }
    }
    Ok(())
}

/// Load disabled_tools for all MCP servers from `[disabled_mcp_tools]` in config.toml.
pub(crate) fn get_all_mcp_disabled_tools(
    _cwd: &std::path::Path,
) -> std::collections::HashMap<String, std::collections::HashSet<String>> {
    let root = match crate::config::load_effective_config() {
        Ok(r) => r,
        Err(_) => return std::collections::HashMap::new(),
    };
    let Some(section) = root.get("disabled_mcp_tools").and_then(|v| v.as_table()) else {
        return std::collections::HashMap::new();
    };
    section
        .iter()
        .filter_map(|(server, val)| {
            let tools: std::collections::HashSet<String> = val
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if tools.is_empty() {
                None
            } else {
                Some((server.clone(), tools))
            }
        })
        .collect()
}

/// Deserialize one `[mcp_servers.<name>]` table, also returning any unrecognized keys.
fn deserialize_mcp_server_config(
    value: &TomlValue,
) -> Result<(McpServerConfig, Vec<String>), String> {
    let unknown_fields = value.as_table().map_or_else(Vec::new, |table| {
        table
            .keys()
            .filter(|field| !KNOWN_MCP_SERVER_FIELDS.contains(&field.as_str()))
            .cloned()
            .collect()
    });
    let config = toml::Value::try_into::<McpServerConfig>(value.clone())
        .map_err(|error| error.to_string())?;
    Ok((config, unknown_fields))
}

/// Turn a failed `[mcp_servers.<name>]` entry into an actionable problem. The
/// transport-less case is steered to `disabled_mcp_servers`, EchoAgent's real
/// disable mechanism.
fn diagnose_invalid_entry(name: &str, value: &TomlValue, error: &str) -> McpServerConfigProblem {
    let has_command = value.get("command").is_some();
    let has_url = value.get("url").is_some();
    let message = if !has_command && !has_url {
        format!(
            "`mcp_servers.{name}` has no transport. To run it, set `command = \"...\"` or \
             `url = \"...\"`. To turn it off, add \"{name}\" to `disabled_mcp_servers` instead of \
             leaving an entry with no transport. \
             See ~/.echo-agent/docs/user-guide/07-mcp-servers.md"
        )
    } else {
        format!(
            "`mcp_servers.{name}` has an invalid transport: {error}. \
             See ~/.echo-agent/docs/user-guide/07-mcp-servers.md"
        )
    };
    McpServerConfigProblem {
        server: name.to_string(),
        field: None,
        severity: McpServerProblemSeverity::Error,
        message,
    }
}

pub(crate) struct ParsedMcpServers {
    pub servers: IndexMap<String, McpServerConfig>,
    pub problems: Vec<McpServerConfigProblem>,
}

/// Parse `[mcp_servers.*]` without ever failing the whole config: valid servers
/// load, invalid entries are reported (GBT-4128).
pub(crate) fn parse_mcp_servers_with_problems(root: &TomlValue) -> ParsedMcpServers {
    let mut servers = IndexMap::new();
    let mut problems = Vec::new();

    let entries = match root {
        TomlValue::Table(table) => match table.get("mcp_servers") {
            Some(TomlValue::Table(mcp_servers)) => mcp_servers,
            _ => return ParsedMcpServers { servers, problems },
        },
        _ => return ParsedMcpServers { servers, problems },
    };

    for (name, value) in entries {
        match deserialize_mcp_server_config(value) {
            Ok((config, unknown_fields)) => {
                for field in unknown_fields {
                    problems.push(McpServerConfigProblem {
                        server: name.clone(),
                        field: Some(field.clone()),
                        severity: McpServerProblemSeverity::Warning,
                        message: format!(
                            "`mcp_servers.{name}` has an unrecognized field `{field}`; it is \
                             ignored. See ~/.echo-agent/docs/user-guide/07-mcp-servers.md"
                        ),
                    });
                }
                if config.enabled
                    && let Some(field) = config.blank_transport_field()
                {
                    problems.push(McpServerConfigProblem {
                        server: name.clone(),
                        field: Some(field.to_string()),
                        severity: McpServerProblemSeverity::Error,
                        message: format!(
                            "`mcp_servers.{name}` is enabled but its `{field}` is blank. \
                             Set a value, or add \"{name}\" to `disabled_mcp_servers` to turn it \
                             off. See ~/.echo-agent/docs/user-guide/07-mcp-servers.md"
                        ),
                    });
                    continue;
                }
                servers.insert(name.clone(), config);
            }
            Err(error) => problems.push(diagnose_invalid_entry(name, value, &error)),
        }
    }
    ParsedMcpServers { servers, problems }
}

/// Wrapper that logs problems and returns only the valid servers.
pub(crate) fn parse_mcp_servers_from_toml(root: &TomlValue) -> IndexMap<String, McpServerConfig> {
    let ParsedMcpServers { servers, problems } = parse_mcp_servers_with_problems(root);
    for problem in &problems {
        tracing::warn!(server = %problem.server, "{}", problem.message);
    }
    servers
}

// ── .mcp.json support ────────────────────────────────────────────────

// `.mcp.json` discovery moved to `echo-agent-workspace` (client-side, shared with
// the folder-trust gate); re-exported so `crate::util::config::*` paths keep working.
pub use echo_agent_workspace::project_config::{
    MCP_JSON_FILENAME, find_mcp_json_files, mcp_json_candidate_paths,
};

pub(crate) fn load_mcp_json_file(path: &std::path::Path) -> Vec<acp::McpServer> {
    if !path.is_file() {
        return vec![];
    }
    let Some(value) = read_mcp_json(path) else {
        return vec![];
    };
    let label = path.display().to_string();
    parse_mcp_config(&value, &label, &crate::config::expand_env_vars_in_string)
}
/// Load .mcp.json servers as McpServerConfig map (for merging into load_mcp_servers).
pub(crate) fn load_mcp_json_servers_as_configs(
    cwd: &std::path::Path,
) -> IndexMap<String, McpServerConfig> {
    // Phase 2 cutoff: if the user has imported, skip reading .mcp.json.
    if crate::claude_import::is_claude_import_marked_with_log("load_mcp_json_servers_as_configs") {
        return IndexMap::new();
    }
    load_mcp_json_servers_as_configs_unfiltered(cwd)
}

/// Like [`load_mcp_json_servers_as_configs`] but bypasses the import-marker
/// gate. Used by the `/import-claude` scanner so users can re-import items
/// they previously skipped, even after the runtime cutoff is active.
pub(crate) fn load_mcp_json_servers_as_configs_unfiltered(
    cwd: &std::path::Path,
) -> IndexMap<String, McpServerConfig> {
    let mcp_json_files = find_mcp_json_files(cwd);
    if mcp_json_files.is_empty() {
        return IndexMap::new();
    }

    let mut result = IndexMap::new();

    // Reverse so cwd entries win on name conflict.
    for mcp_path in mcp_json_files.iter().rev() {
        if let Some(config) = read_mcp_json(mcp_path) {
            for (name, cfg) in config.mcp_servers {
                result.entry(name).or_insert(cfg);
            }
        }
    }

    result
}

pub(crate) fn parse_mcp_config(
    config: &McpConfig,
    source_label: &str,
    sub: &dyn Fn(&str) -> String,
) -> Vec<acp::McpServer> {
    parse_mcp_config_with_oauth(config, source_label, sub).0
}

pub(crate) fn parse_mcp_config_with_oauth(
    config: &McpConfig,
    source_label: &str,
    sub: &dyn Fn(&str) -> String,
) -> (Vec<acp::McpServer>, McpOAuthConfigMap) {
    let preferences = load_mcp_preferences().file();
    let mut servers = Vec::new();
    let mut oauth_configs = McpOAuthConfigMap::new();
    for (name, server_config) in &config.mcp_servers {
        let mut server_config = match server_config.resolve_setup(preferences.servers.get(name)) {
            McpSetupResolution::Resolved(config) => config,
            McpSetupResolution::Required(_) => continue,
            McpSetupResolution::Invalid(reason) => {
                tracing::warn!(
                    source = source_label,
                    server = %name,
                    error = %reason,
                    "MCP setup config is invalid"
                );
                continue;
            }
        };
        server_config.expand_strings(sub);
        if let Some(oauth) = server_config.oauth_config() {
            oauth_configs.insert(name.clone(), oauth);
        }
        if let Some(server) = server_config.to_acp_mcp_server(name.clone()) {
            servers.push(server);
        } else {
            tracing::warn!(
                source = source_label,
                server = name,
                "MCP server has no 'command' (stdio) or 'url' (http/sse); skipping"
            );
        }
    }

    if !servers.is_empty() {
        tracing::info!(
            source = source_label,
            count = servers.len(),
            "loaded MCP servers"
        );
    }

    (servers, oauth_configs)
}

/// Load MCP servers from `~/.claude.json`.
///
/// User-level MCP servers live at the top-level `mcpServers` key,
/// and per-project (local-scope) MCP servers under `projects.<cwd>.mcpServers`.
///
/// Returns servers from both locations (project-specific first, then user-level).
pub(crate) fn load_claude_json_mcp_servers(
    cwd: &std::path::Path,
    compat: &CompatConfig,
) -> Vec<acp::McpServer> {
    // Compat gate: skip ~/.claude.json MCP loading when disabled.
    if !compat.claude.mcps {
        return vec![];
    }
    // Phase 2 cutoff: if the user has imported, skip reading ~/.claude.json.
    if crate::claude_import::is_claude_import_marked_with_log("load_claude_json_mcp_servers") {
        return vec![];
    }

    let Some(home) = echo_agent_config::discovery_home_dir() else {
        return vec![];
    };
    let claude_json_path = home.join(".claude.json");
    load_claude_json_mcp_servers_from(&claude_json_path, cwd)
}

/// On-disk `~/.claude.json` MCP servers for kill-switch attribution only.
///
/// Bypasses `compat.claude.mcps` and the import-marker cutoff so a client
/// reseed cannot re-admit Claude-sourced servers while the kill switch is off
/// merely because the normal runtime loader is gated empty.
pub(crate) fn load_claude_json_mcp_servers_for_attribution(
    cwd: &std::path::Path,
) -> Vec<acp::McpServer> {
    let Some(home) = echo_agent_config::discovery_home_dir() else {
        return vec![];
    };
    load_claude_json_mcp_servers_from(&home.join(".claude.json"), cwd)
}

/// Load ~/.claude.json MCP servers as McpServerConfig map (for merging into load_mcp_servers).
pub(crate) fn load_claude_json_mcp_servers_as_configs(
    cwd: &std::path::Path,
    compat: &CompatConfig,
) -> IndexMap<String, McpServerConfig> {
    // Compat gate: skip ~/.claude.json MCP loading when disabled.
    if !compat.claude.mcps {
        return IndexMap::new();
    }
    // Phase 2 cutoff: if the user has imported, skip reading ~/.claude.json.
    if crate::claude_import::is_claude_import_marked_with_log(
        "load_claude_json_mcp_servers_as_configs",
    ) {
        return IndexMap::new();
    }
    load_claude_json_mcp_servers_as_configs_unfiltered(cwd)
}

/// Like [`load_claude_json_mcp_servers_as_configs`] but bypasses the
/// import-marker gate. Used by the `/import-claude` scanner so users can
/// re-import items they previously skipped, even after the runtime cutoff
/// is active.
pub(crate) fn load_claude_json_mcp_servers_as_configs_unfiltered(
    cwd: &std::path::Path,
) -> IndexMap<String, McpServerConfig> {
    let Some(home) = echo_agent_config::discovery_home_dir() else {
        return IndexMap::new();
    };
    let claude_json_path = home.join(".claude.json");
    load_claude_json_mcp_servers_from_as_configs(&claude_json_path, cwd)
}

fn load_claude_json_mcp_servers_from_as_configs(
    claude_json_path: &std::path::Path,
    cwd: &std::path::Path,
) -> IndexMap<String, McpServerConfig> {
    let content = match std::fs::read_to_string(claude_json_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(
                path = %claude_json_path.display(),
                error = %e,
                "failed to read ~/.claude.json"
            );
            return IndexMap::new();
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                path = %claude_json_path.display(),
                error = %e,
                "failed to parse ~/.claude.json"
            );
            return IndexMap::new();
        }
    };
    let config = claude_json_mcp_from_value(&value);

    let mut result = IndexMap::new();

    // Per-project MCP servers (local scope, higher priority)
    let cwd_key = cwd.to_string_lossy();
    if let Some(project) = config.projects.get(cwd_key.as_ref()) {
        for (name, cfg) in &project.mcp_servers {
            result.insert(name.clone(), cfg.clone());
        }
    }

    // User-level MCP servers (lower priority)
    for (name, cfg) in &config.user_mcp.mcp_servers {
        result.entry(name.clone()).or_insert(cfg.clone());
    }
    tracing::info!(
        project_count = config
            .projects
            .get(cwd_key.as_ref())
            .map(|p| p.mcp_servers.len())
            .unwrap_or(0),
        user_level_count = config.user_mcp.mcp_servers.len(),
        total_count = result.len(),
        "MCP servers loaded from ~/.claude.json"
    );

    result
}

/// Load MCP servers from editor MCP config files.
///
/// Scans project-level `<cwd>/.cursor/mcp.json` first (higher priority),
/// then global `~/.cursor/mcp.json`. Both use the `{"mcpServers": {...}}`
/// format identical to `.mcp.json`. Gated by `compat.cursor.mcps`.
pub(crate) fn load_cursor_mcp_servers(
    cwd: &std::path::Path,
    compat: &CompatConfig,
) -> Vec<acp::McpServer> {
    // Compat gate: skip Cursor MCP loading when disabled.
    if !compat.cursor.mcps {
        return vec![];
    }
    let mut result = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    // Project-level (higher priority)
    let project_path = cwd.join(".cursor").join("mcp.json");
    for server in load_mcp_json_file(&project_path) {
        let name = match &server {
            acp::McpServer::Http(acp::McpServerHttp { name, .. })
            | acp::McpServer::Sse(acp::McpServerSse { name, .. })
            | acp::McpServer::Stdio(acp::McpServerStdio { name, .. }) => name.clone(),
            // TODO(acp-0.10): `McpServer` is #[non_exhaustive].
            _ => continue,
        };
        if seen_names.insert(name) {
            result.push(server);
        }
    }

    // Global (lower priority)
    if let Some(home) = echo_agent_config::discovery_home_dir() {
        let global_path = home.join(".cursor").join("mcp.json");
        for server in load_mcp_json_file(&global_path) {
            let name = match &server {
                acp::McpServer::Http(acp::McpServerHttp { name, .. })
                | acp::McpServer::Sse(acp::McpServerSse { name, .. })
                | acp::McpServer::Stdio(acp::McpServerStdio { name, .. }) => name.clone(),
                // TODO(acp-0.10): `McpServer` is #[non_exhaustive].
                _ => continue,
            };
            if seen_names.insert(name) {
                result.push(server);
            }
        }
    }

    result
}

/// Load Cursor MCP servers as McpServerConfig map (for merging into load_mcp_servers).
///
/// Scans project-level `<cwd>/.cursor/mcp.json` first, then global.
pub(crate) fn load_cursor_mcp_servers_as_configs(
    cwd: &std::path::Path,
    compat: &CompatConfig,
) -> IndexMap<String, McpServerConfig> {
    // Compat gate: skip Cursor MCP loading when disabled.
    if !compat.cursor.mcps {
        return IndexMap::new();
    }
    let mut result = IndexMap::new();

    // Project-level (higher priority)
    let project_path = cwd.join(".cursor").join("mcp.json");
    if project_path.is_file()
        && let Some(config) = read_mcp_json(&project_path)
    {
        for (name, cfg) in config.mcp_servers {
            result.insert(name, cfg);
        }
    }

    // Global (lower priority — or_insert so project wins)
    if let Some(home) = echo_agent_config::discovery_home_dir() {
        let global_path = home.join(".cursor").join("mcp.json");
        if global_path.is_file()
            && let Some(config) = read_mcp_json(&global_path)
        {
            for (name, cfg) in config.mcp_servers {
                result.entry(name).or_insert(cfg);
            }
        }
    }

    result
}

/// Inner implementation that accepts the file path, making it testable.
fn load_claude_json_mcp_servers_from(
    claude_json_path: &std::path::Path,
    cwd: &std::path::Path,
) -> Vec<acp::McpServer> {
    let content = match std::fs::read_to_string(claude_json_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                path = %claude_json_path.display(),
                error = %e,
                "failed to parse claude.json"
            );
            return vec![];
        }
    };
    let config = claude_json_mcp_from_value(&value);

    let sub = &crate::config::expand_env_vars_in_string;
    let mut servers = Vec::new();

    // Per-project MCP servers (local scope, higher priority)
    let cwd_key = cwd.to_string_lossy();
    if let Some(project) = config.projects.get(cwd_key.as_ref()) {
        let label = format!("~/.claude.json projects[{}]", cwd_key);
        servers.extend(parse_mcp_config(project, &label, sub));
    }

    // User-level MCP servers (lower priority)
    if !config.user_mcp.mcp_servers.is_empty() {
        servers.extend(parse_mcp_config(&config.user_mcp, "~/.claude.json", sub));
    }

    servers
}

/// Build an `McpConfig` from a JSON value, skipping any `mcpServers` entry that
/// fails to deserialize instead of dropping the whole file. Mirrors the
/// per-entry tolerance of [`parse_mcp_servers_with_problems`] for TOML, so one
/// bad entry in a `.mcp.json` or `~/.claude.json` cannot take out its siblings.
fn mcp_config_from_json_value(value: &serde_json::Value) -> McpConfig {
    let mut mcp_servers = IndexMap::new();
    if let Some(entries) = value.get("mcpServers").and_then(|v| v.as_object()) {
        for (name, entry) in entries {
            match serde_json::from_value::<McpServerConfig>(entry.clone()) {
                Ok(config) => {
                    mcp_servers.insert(name.clone(), config);
                }
                Err(error) => tracing::warn!(
                    server = %name,
                    error = %error,
                    "skipping invalid MCP server entry in JSON config"
                ),
            }
        }
    }
    McpConfig { mcp_servers }
}

/// Parsed `~/.claude.json` MCP view: top-level user servers plus per-project maps.
struct ClaudeJsonMcp {
    user_mcp: McpConfig,
    projects: HashMap<String, McpConfig>,
}

/// Build the `~/.claude.json` MCP view from a JSON value, tolerating bad entries
/// per server (see [`mcp_config_from_json_value`]).
fn claude_json_mcp_from_value(value: &serde_json::Value) -> ClaudeJsonMcp {
    let user_mcp = mcp_config_from_json_value(value);
    let mut projects = HashMap::new();
    if let Some(entries) = value.get("projects").and_then(|v| v.as_object()) {
        for (path, project) in entries {
            projects.insert(path.clone(), mcp_config_from_json_value(project));
        }
    }
    ClaudeJsonMcp { user_mcp, projects }
}

/// Read and parse a JSON file. Returns `None` on I/O or top-level parse errors
/// (logged); individual bad `mcpServers` entries are skipped, not fatal.
pub(crate) fn read_mcp_json(path: &std::path::Path) -> Option<McpConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| {
            tracing::warn!(error = %e, "failed to read MCP JSON");
        })
        .ok()?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| {
            tracing::warn!(error = %e, "failed to parse MCP JSON");
        })
        .ok()?;
    Some(mcp_config_from_json_value(&value))
}

/// Like `load_mcp_servers_with_project` but returns raw configs without filtering by `enabled`.
fn load_all_mcp_configs(cwd: &std::path::Path) -> IndexMap<String, McpServerConfig> {
    load_mcp_server_configs_with_project(cwd)
        .into_iter()
        .map(|(name, (config, _))| (name, config))
        .collect()
}

/// Load all configured MCP servers with the scope each definition came from
/// (`"user"` or `"project"`).
///
/// Overlays project-scoped `.echo-agent/config.toml` files from `cwd` up to the
/// repo root onto the user-tier config, nearest definition winning — the same
/// override semantics as [`get_mcp_server_config_with_project`].
pub fn load_mcp_server_configs_with_project(
    cwd: &std::path::Path,
) -> IndexMap<String, (McpServerConfig, &'static str)> {
    let global_config = crate::config::load_effective_config()
        .unwrap_or_else(|_| TomlValue::Table(toml::map::Map::new()));

    let mut servers: IndexMap<String, (McpServerConfig, &'static str)> =
        parse_mcp_servers_from_toml(&global_config)
            .into_iter()
            .map(|(name, config)| (name, (config, MCP_SCOPE_USER)))
            .collect();

    // find_project_configs is repo-root-first, so nearer files overwrite.
    for config_path in crate::config::find_project_configs(cwd) {
        if let Ok(root) = crate::config::load_config_file(&config_path) {
            for (name, config) in parse_mcp_servers_from_toml(&root) {
                servers.insert(name, (config, MCP_SCOPE_PROJECT));
            }
        }
    }

    servers
}

/// MCP config problems across the same layers as
/// [`load_mcp_server_configs_with_project`], for `echoagent inspect`.
pub(crate) fn load_mcp_server_problems_with_project(
    cwd: &std::path::Path,
) -> Vec<McpServerConfigProblem> {
    let mut problems = Vec::new();
    if let Ok(global_config) = crate::config::load_effective_config() {
        problems.extend(parse_mcp_servers_with_problems(&global_config).problems);
    }
    for config_path in crate::config::find_project_configs(cwd) {
        if let Ok(root) = crate::config::load_config_file(&config_path) {
            problems.extend(parse_mcp_servers_with_problems(&root).problems);
        }
    }
    problems
}

/// MCP server names with `enabled = false` in config.toml (including project overrides).
pub fn disabled_mcp_server_names(cwd: &std::path::Path) -> std::collections::HashSet<String> {
    let mut disabled: std::collections::HashSet<String> = load_all_mcp_configs(cwd)
        .into_iter()
        .filter(|(_, cfg)| !cfg.enabled)
        .map(|(name, _)| name)
        .collect();

    // Also check the `disabled_mcp_servers` array in config.toml.
    if let Ok(root) = crate::config::load_effective_config()
        && let Some(arr) = root.get("disabled_mcp_servers").and_then(|v| v.as_array())
    {
        for val in arr {
            if let Some(name) = val.as_str() {
                disabled.insert(name.to_string());
            }
        }
    }

    disabled
}

/// Names `echoagent mcp enable`/`disable` may target: user/project TOML (including
/// setup-required/invalid entries that session merge drops), the user
/// `disabled_mcp_servers` list, compat JSON (`.mcp.json`, Claude, Cursor), and
/// **plugin** MCP servers (same discovery as doctor/`/mcps`).
///
/// Does **not** include gateway connectors (`managed_gateway:…`); those use
/// `disabled_mcp_tools.__managed_gateway_connectors` via the `/mcps` Space.
/// `echo_agent_com_*` is known only when a TOML / disabled / compat / plugin
/// definition exists — not by prefix.
pub fn cli_known_mcp_server_names(cwd: &std::path::Path) -> std::collections::HashSet<String> {
    let mut names = disabled_mcp_server_names(cwd);
    // Full TOML key set (list parity) — merge drops setup-required/invalid.
    names.extend(all_toml_mcp_server_names(cwd));

    // Doctor/Space path: resolved TOML + plugins + Claude + Cursor + `.mcp.json`.
    let registry = load_cli_plugin_registry(cwd);
    let compat = CompatConfig::default();
    for (server, _) in crate::session::managed_mcp::merge_managed_mcp_servers_sourced(
        cwd,
        Some(&registry),
        &compat,
    ) {
        let name = crate::session::managed_mcp::mcp_server_name(&server);
        if !name.is_empty() {
            names.insert(name.to_string());
        }
    }
    names
}

/// Plugin registry for one-shot CLI discovery (matches mcp doctor gating).
///
/// Resolves the same cwd-effective `[plugins]` table as session startup
/// (`resolve_effective_plugins_config`), so trusted project `[plugins].paths`
/// plugins are included, not just the global config. Also used by the pager's
/// `/agents` modal to list plugin-provided agents without a live session
/// registry snapshot.
pub fn load_cli_plugin_registry(cwd: &std::path::Path) -> echo_agent_core::plugins::PluginRegistry {
    let trust_store = echo_agent_core::plugins::TrustStore::load();
    // Resolve/record the folder-trust verdict first: the effective-plugins
    // resolve below gates project [plugins].paths on the cached verdict.
    let project_trusted = crate::agent::folder_trust::resolve_and_record(cwd, None, false);
    let plugins_cfg = crate::config::resolve_effective_plugins_config(cwd);
    let mut plugin_config = plugins_cfg.to_discovery_config();
    let discovered = echo_agent_core::plugins::discover_plugins(
        Some(cwd),
        &plugin_config,
        &trust_store,
        project_trusted,
    );
    plugin_config.populate_plugin_lists(&discovered);
    echo_agent_core::plugins::PluginRegistry::from_discovered(
        discovered,
        &plugin_config.disabled,
        &plugin_config.enabled,
    )
}

fn config_path() -> PathBuf {
    crate::util::echo_agent_home::echo_agent_home().join("config.toml")
}

/// Path to the user-level config file (`~/.echo-agent/config.toml`).
pub fn user_config_path() -> PathBuf {
    config_path()
}

/// Path to a project-level config file (`<dir>/.echo-agent/config.toml`).
pub fn project_config_path(dir: &std::path::Path) -> PathBuf {
    dir.join(".echo-agent").join("config.toml")
}

/// True when the config file at `path` defines `[mcp_servers.<name>]`.
///
/// Checks raw key presence rather than deserializing, so malformed entries
/// (the ones users most need `mcp remove` for) are still reported.
pub fn mcp_server_defined_at(path: &std::path::Path, server_name: &str) -> bool {
    let Ok(root) = crate::config::load_config_file(path) else {
        return false;
    };
    root.get("mcp_servers")
        .and_then(|v| v.as_table())
        .is_some_and(|servers| servers.contains_key(server_name))
}

/// Synchronously load `[cli] npm_registry` from config.toml.
pub fn load_npm_registry_sync() -> Option<String> {
    let root: TomlValue = crate::config::load_effective_config().ok()?;
    if let TomlValue::Table(table) = root
        && let Some(TomlValue::Table(cli)) = table.get("cli")
    {
        cli.get("npm_registry")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    }
}

/// Synchronously load the gcs_service_account_key from the config file.
/// This is intended for use in contexts where async is not available.
pub(crate) fn load_gcs_service_account_key_sync() -> Option<String> {
    let root: TomlValue = crate::config::load_effective_config().ok()?;
    if let TomlValue::Table(table) = root
        && let Some(TomlValue::Table(endpoints)) = table.get("endpoints")
    {
        endpoints
            .get("gcs_service_account_key")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    }
}
/// Returns `None` when `[cli] use_leader` is not set in the config
/// (allowing a remote settings fallback), or `Some(true/false)` when
/// explicitly configured. This distinction lets callers fall through
/// to a remote flag when the user hasn't expressed a local preference.
pub fn use_leader_from_toml_opt(root: &TomlValue) -> Option<bool> {
    if let TomlValue::Table(table) = root
        && let Some(TomlValue::Table(cli)) = table.get("cli")
    {
        cli.get("use_leader").and_then(|v| v.as_bool())
    } else {
        None
    }
}

/// Check if leader mode is enabled in the config.
/// When true, the agent will connect to a shared leader process instead of
/// running the agent directly. This allows multiple agent instances to share one backend.
/// Defaults to false when not explicitly set.
pub fn use_leader_from_toml(root: &TomlValue) -> bool {
    use_leader_from_toml_opt(root).unwrap_or(false)
}

/// Returns `Some(true/false)` when `[cli] session_registry` is set in config.toml,
/// `None` when absent (allowing remote settings fallback).
/// Local config takes precedence over remote settings.
pub(crate) fn session_registry_from_toml_opt(root: &TomlValue) -> Option<bool> {
    if let TomlValue::Table(table) = root
        && let Some(TomlValue::Table(cli)) = table.get("cli")
    {
        cli.get("session_registry").and_then(|v| v.as_bool())
    } else {
        None
    }
}

/// Overrides `[cli] session_registry`; usable before `~/.echo-agent/config.toml` exists.
pub const SESSION_REGISTRY_ENV_VAR: &str = "ECHO_AGENT_SESSION_REGISTRY";

pub(crate) fn session_registry_from_env_opt() -> Option<bool> {
    echo_agent_config::env_bool(SESSION_REGISTRY_ENV_VAR)
}

/// Where a local session-registry override came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrySource {
    /// [`SESSION_REGISTRY_ENV_VAR`].
    Env,
    /// `[cli] session_registry` in config.toml.
    ConfigToml,
}

impl RegistrySource {
    /// The user-facing name of this source, for diagnostics.
    pub const fn label(self) -> &'static str {
        match self {
            RegistrySource::Env => SESSION_REGISTRY_ENV_VAR,
            RegistrySource::ConfigToml => "[cli] session_registry",
        }
    }
}

/// Env var, then `[cli] session_registry`; `None` defers to remote settings.
pub fn session_registry_local_override_sourced(
    root: Option<&TomlValue>,
) -> Option<(bool, RegistrySource)> {
    if let Some(v) = session_registry_from_env_opt() {
        return Some((v, RegistrySource::Env));
    }
    root.and_then(session_registry_from_toml_opt)
        .map(|v| (v, RegistrySource::ConfigToml))
}

pub(crate) fn session_registry_local_override(root: Option<&TomlValue>) -> Option<bool> {
    session_registry_local_override_sourced(root).map(|(v, _)| v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml::Value as TomlValue;

    /// Env beats config.toml; unrecognized env defers; both absent defers to remote.
    #[test]
    #[serial_test::serial]
    fn session_registry_local_override_precedence() {
        let toml_true: TomlValue = toml::from_str("[cli]\nsession_registry = true").unwrap();
        {
            let _g = echo_agent_test_support::EnvGuard::set(SESSION_REGISTRY_ENV_VAR, "false");
            assert_eq!(
                session_registry_local_override_sourced(Some(&toml_true)),
                Some((false, RegistrySource::Env)),
                "env wins and reports itself as the source"
            );
        }
        {
            let _g = echo_agent_test_support::EnvGuard::set(SESSION_REGISTRY_ENV_VAR, "bogus");
            assert_eq!(
                session_registry_local_override_sourced(Some(&toml_true)),
                Some((true, RegistrySource::ConfigToml)),
                "unrecognized env values defer to config.toml"
            );
        }
        {
            let _g = echo_agent_test_support::EnvGuard::unset(SESSION_REGISTRY_ENV_VAR);
            assert_eq!(session_registry_local_override_sourced(None), None);
        }
    }

    /// A trusted project's `[plugins].paths` plugin (session-startup config
    /// source) must be discovered by the one-shot CLI registry, including its
    /// agents. Dev builds are folder-trust-inert, so the project path merges.
    #[test]
    #[serial_test::serial]
    fn load_cli_plugin_registry_includes_project_config_path_plugins() {
        let home = tempfile::tempdir().unwrap();
        let _env = echo_agent_test_support::EnvGuard::set("ECHO_AGENT_HOME", home.path());

        let repo = tempfile::tempdir().unwrap();
        git2::Repository::init(repo.path()).unwrap();

        // Project-declared [plugins].paths plugin providing one agent.
        let plugin_dir = repo.path().join("proj-plugin");
        let agents_dir = plugin_dir.join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"name": "proj-plugin", "agents": "./agents"}"#,
        )
        .unwrap();
        std::fs::write(
            agents_dir.join("reviewer.md"),
            "---\nname: reviewer\ndescription: Project reviewer\n---\nBody.\n",
        )
        .unwrap();

        let echoagent = repo.path().join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(
            echoagent.join("config.toml"),
            format!("[plugins]\npaths = [\"{}\"]\n", plugin_dir.display()),
        )
        .unwrap();

        let registry = load_cli_plugin_registry(repo.path());
        assert!(
            registry.get("proj-plugin").is_some(),
            "project [plugins].paths plugin must be discovered"
        );
        let agents = echo_agent_core::discovery::plugin_agents(&registry);
        assert!(
            agents
                .iter()
                .any(|a| a.qualified_name == "proj-plugin:reviewer"),
            "project config-path plugin agent must be enumerable, got: {:?}",
            agents.iter().map(|a| &a.qualified_name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn mcp_server_defined_at_checks_raw_key_presence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // `urll` fails McpServerConfig deserialization; the raw key must
        // still be reported so `mcp remove` can delete broken entries.
        std::fs::write(
            &path,
            "[mcp_servers.broken]\nurll = \"https://x.example\"\n",
        )
        .unwrap();

        assert!(mcp_server_defined_at(&path, "broken"));
        assert!(!mcp_server_defined_at(&path, "other"));
        assert!(!mcp_server_defined_at(
            &dir.path().join("missing.toml"),
            "broken"
        ));
    }

    /// Covers all canonical wire values plus the unknown/corrupt fallback.
    #[test]
    fn parse_mcp_servers_skips_unparseable_entries() {
        let root = toml::from_str::<TomlValue>(
            r#"
mcp_servers.broken = "not-a-table"

[mcp_servers.also_broken]
enabled = "yes"

[mcp_servers.ok]
command = "echo"
args = ["hi"]
"#,
        )
        .unwrap();
        let servers = parse_mcp_servers_from_toml(&root);
        assert!(!servers.contains_key("broken"));
        assert!(!servers.contains_key("also_broken"));
        assert!(servers.contains_key("ok"));
    }

    #[test]
    fn parse_mcp_server_config_reports_unknown_fields() {
        let value = toml::from_str::<TomlValue>(
            r#"
command = "echo"
enabeld = false
"#,
        )
        .unwrap();
        let (config, unknown_fields) = deserialize_mcp_server_config(&value).unwrap();
        assert!(
            config.enabled,
            "the misspelled field must not silently disable the server"
        );
        assert_eq!(unknown_fields, vec!["enabeld"]);
    }

    #[test]
    fn parse_mcp_servers_drops_transport_less_entry() {
        let root = toml::from_str::<TomlValue>(
            r#"
[mcp_servers.github]
enabled = false

[mcp_servers.linear]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.linear.app/mcp"]
"#,
        )
        .unwrap();
        let ParsedMcpServers { servers, problems } = parse_mcp_servers_with_problems(&root);
        assert!(
            !servers.contains_key("github"),
            "transport-less entry is dropped, not kept"
        );
        assert!(servers.contains_key("linear"));
        assert!(servers["linear"].enabled);
        let problem = problems
            .iter()
            .find(|p| p.server == "github")
            .expect("github problem reported");
        assert_eq!(problem.severity, McpServerProblemSeverity::Error);
        assert!(
            problem.message.contains("disabled_mcp_servers"),
            "{problem:?}"
        );
    }

    #[test]
    fn parse_mcp_servers_rejects_enabled_without_transport() {
        let root = toml::from_str::<TomlValue>(
            r#"
[mcp_servers.half]
enabled = true
"#,
        )
        .unwrap();
        let ParsedMcpServers { servers, problems } = parse_mcp_servers_with_problems(&root);
        assert!(
            !servers.contains_key("half"),
            "enabled without command/url must be dropped"
        );
        assert!(problems.iter().any(|p| p.server == "half"));
    }

    #[test]
    fn parse_mcp_servers_rejects_blank_transport() {
        let root = toml::from_str::<TomlValue>(
            r#"
[mcp_servers.blank_url]
url = "  "

[mcp_servers.blank_cmd]
command = ""
"#,
        )
        .unwrap();
        let ParsedMcpServers { servers, problems } = parse_mcp_servers_with_problems(&root);
        assert!(!servers.contains_key("blank_url"), "blank url dropped");
        assert!(!servers.contains_key("blank_cmd"), "blank command dropped");
        assert_eq!(
            problems
                .iter()
                .filter(|p| p.severity == McpServerProblemSeverity::Error)
                .count(),
            2,
            "both blank transports reported: {problems:?}"
        );
    }

    #[test]
    fn json_map_skips_bad_entry_and_keeps_the_rest() {
        // One transport-less entry must not drop its siblings in the same JSON
        // file (.mcp.json / ~/.claude.json).
        let value = serde_json::json!({
            "mcpServers": {
                "bad": { "enabled": false },
                "good": { "command": "npx", "args": ["-y", "pkg"] }
            }
        });
        let config = mcp_config_from_json_value(&value);
        assert!(!config.mcp_servers.contains_key("bad"));
        assert!(config.mcp_servers.contains_key("good"));
        assert!(config.mcp_servers["good"].enabled);
    }

    #[test]
    fn test_parse_mcp_servers_empty() {
        let root = toml::from_str::<TomlValue>("").unwrap();
        let servers = parse_mcp_servers_from_toml(&root);
        assert!(servers.is_empty());
    }

    #[test]
    fn test_parse_mcp_servers_stdio() {
        let toml_str = r#"
[mcp_servers.test_server]
command = "node"
args = ["server.js"]
"#;
        let root = toml::from_str::<TomlValue>(toml_str).unwrap();
        let servers = parse_mcp_servers_from_toml(&root);
        assert_eq!(servers.len(), 1);
        assert!(servers.contains_key("test_server"));
        let config = servers.get("test_server").unwrap();
        assert!(config.enabled);
        match &config.transport {
            McpServerTransportConfig::Stdio { command, args, .. } => {
                assert_eq!(command, "node");
                assert_eq!(args, &["server.js"]);
            }
            _ => panic!("Expected Stdio transport"),
        }
    }

    #[test]
    fn test_use_leader_parsing_true() {
        // Test that we can parse a config with use_leader = true
        let toml_str = r#"
[cli]
use_leader = true
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        assert!(use_leader_from_toml(&root));
    }

    #[test]
    fn test_use_leader_parsing_false() {
        // Test that we can parse a config with use_leader = false
        let toml_str = r#"
[cli]
use_leader = false
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        assert!(!use_leader_from_toml(&root));
    }

    #[test]
    fn test_use_leader_default_false() {
        // Test that missing use_leader defaults to false
        let toml_str = r#"
[cli]
auto_update = true
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        assert!(!use_leader_from_toml(&root));
    }

    #[test]
    fn test_use_leader_no_cli_section() {
        // Test with no cli section at all
        let toml_str = r#"
[models]
default = "echo-agent-code-fast-1"
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        if let TomlValue::Table(ref table) = root {
            let has_cli = table.get("cli").is_some();
            assert!(!has_cli);
        }
        // use_leader_from_toml() should default to false when no cli section
        assert!(!use_leader_from_toml(&root));
    }

    #[test]
    fn test_use_leader_opt_returns_some_true() {
        let toml_str = r#"
[cli]
use_leader = true
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        assert_eq!(use_leader_from_toml_opt(&root), Some(true));
    }

    #[test]
    fn test_use_leader_opt_returns_some_false() {
        let toml_str = r#"
[cli]
use_leader = false
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        assert_eq!(use_leader_from_toml_opt(&root), Some(false));
    }

    #[test]
    fn test_use_leader_opt_returns_none_when_absent() {
        let toml_str = r#"
[cli]
auto_update = true
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        assert_eq!(use_leader_from_toml_opt(&root), None);
    }

    #[test]
    fn test_use_leader_opt_returns_none_when_no_cli_section() {
        let toml_str = r#"
[models]
default = "echo-agent-code-fast-1"
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        assert_eq!(use_leader_from_toml_opt(&root), None);
    }

    // WorktreeType tests
    #[test]
    fn test_project_scoped_mcp_override_replaces_entirely() {
        // Simulate global config with timeouts
        let global_toml = r#"
[mcp_servers.linear]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.linear.app/mcp"]
enabled = true
startup_timeout_sec = 10
tool_timeout_sec = 60
"#;
        let global_root = toml::from_str::<TomlValue>(global_toml).unwrap();
        let global_servers = parse_mcp_servers_from_toml(&global_root);

        // Simulate project config WITHOUT timeouts
        let project_toml = r#"
[mcp_servers.linear]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.linear.app/mcp"]
enabled = true
"#;
        let project_root = toml::from_str::<TomlValue>(project_toml).unwrap();
        let project_servers = parse_mcp_servers_from_toml(&project_root);

        // Global config should have timeouts
        let global_linear = global_servers.get("linear").unwrap();
        assert_eq!(global_linear.startup_timeout_sec, Some(10));
        assert_eq!(global_linear.tool_timeout_sec, Some(60));

        // Project config should NOT have timeouts (defaults apply)
        let project_linear = project_servers.get("linear").unwrap();
        assert_eq!(project_linear.startup_timeout_sec, None);
        assert_eq!(project_linear.tool_timeout_sec, None);

        // Merge: project overrides global entirely
        let mut merged: IndexMap<String, McpServerConfig> = IndexMap::new();
        for (name, config) in &global_servers {
            merged.insert(name.clone(), config.clone());
        }
        for (name, config) in &project_servers {
            merged.insert(name.clone(), config.clone());
        }

        // After merge, the project config should have replaced the global one entirely
        let merged_linear = merged.get("linear").unwrap();
        assert_eq!(merged_linear.startup_timeout_sec, None);
        assert_eq!(merged_linear.tool_timeout_sec, None);
    }

    #[test]
    fn test_project_scoped_mcp_adds_new_servers() {
        let global_toml = r#"
[mcp_servers.linear]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.linear.app/mcp"]
enabled = true
"#;
        let global_root = toml::from_str::<TomlValue>(global_toml).unwrap();
        let global_servers = parse_mcp_servers_from_toml(&global_root);

        let project_toml = r#"
[mcp_servers.buildkite]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.buildkite.com/mcp"]
enabled = true
"#;
        let project_root = toml::from_str::<TomlValue>(project_toml).unwrap();
        let project_servers = parse_mcp_servers_from_toml(&project_root);

        // Merge: project adds new server
        let mut merged: IndexMap<String, McpServerConfig> = IndexMap::new();
        for (name, config) in &global_servers {
            merged.insert(name.clone(), config.clone());
        }
        for (name, config) in &project_servers {
            merged.insert(name.clone(), config.clone());
        }

        assert_eq!(merged.len(), 2);
        assert!(merged.contains_key("linear"));
        assert!(merged.contains_key("buildkite"));
    }

    #[test]
    fn test_project_scoped_mcp_can_disable_server() {
        let global_toml = r#"
[mcp_servers.linear]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.linear.app/mcp"]
enabled = true
"#;
        let global_root = toml::from_str::<TomlValue>(global_toml).unwrap();
        let global_servers = parse_mcp_servers_from_toml(&global_root);
        assert!(global_servers.get("linear").unwrap().enabled);

        // Project config disables the server
        let project_toml = r#"
[mcp_servers.linear]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.linear.app/mcp"]
enabled = false
"#;
        let project_root = toml::from_str::<TomlValue>(project_toml).unwrap();
        let project_servers = parse_mcp_servers_from_toml(&project_root);

        let mut merged: IndexMap<String, McpServerConfig> = IndexMap::new();
        for (name, config) in &global_servers {
            merged.insert(name.clone(), config.clone());
        }
        for (name, config) in &project_servers {
            merged.insert(name.clone(), config.clone());
        }

        // After merge, the server should be disabled by project config
        assert!(!merged.get("linear").unwrap().enabled);
    }

    #[test]
    fn skills_config_default_is_empty() {
        let cfg = SkillsConfig::default();
        assert!(cfg.paths.is_empty());
        assert!(cfg.ignore.is_empty());
    }

    #[test]
    fn skills_config_parses_paths_and_ignore() {
        let root = toml::from_str::<TomlValue>(
            r#"
[skills]
paths = ["~/.echo-agent/skills", "~/.echo-agent/skills/special/SKILL.md"]
ignore = ["~/.echo-agent/skills/noisy/SKILL.md"]
"#,
        )
        .unwrap();
        let TomlValue::Table(ref table) = root else {
            panic!()
        };
        let cfg = table
            .get("skills")
            .and_then(|v| v.clone().try_into::<SkillsConfig>().ok())
            .unwrap_or_default();
        assert_eq!(
            cfg.paths,
            vec![
                "~/.echo-agent/skills",
                "~/.echo-agent/skills/special/SKILL.md"
            ]
        );
        assert_eq!(cfg.ignore, vec!["~/.echo-agent/skills/noisy/SKILL.md"]);
    }

    #[test]
    fn test_project_scoped_mcp_preserves_unrelated_global_servers() {
        let global_toml = r#"
[mcp_servers.linear]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.linear.app/mcp"]
enabled = true

[mcp_servers.buildkite]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.buildkite.com/mcp"]
enabled = true
"#;
        let global_root = toml::from_str::<TomlValue>(global_toml).unwrap();
        let global_servers = parse_mcp_servers_from_toml(&global_root);

        // Project only overrides linear
        let project_toml = r#"
[mcp_servers.linear]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.linear.app/mcp"]
enabled = false
"#;
        let project_root = toml::from_str::<TomlValue>(project_toml).unwrap();
        let project_servers = parse_mcp_servers_from_toml(&project_root);

        let mut merged: IndexMap<String, McpServerConfig> = IndexMap::new();
        for (name, config) in &global_servers {
            merged.insert(name.clone(), config.clone());
        }
        for (name, config) in &project_servers {
            merged.insert(name.clone(), config.clone());
        }

        // buildkite should be preserved from global config
        assert_eq!(merged.len(), 2);
        assert!(merged.get("buildkite").unwrap().enabled);
        assert!(!merged.get("linear").unwrap().enabled);
    }

    #[test]
    fn test_mcp_server_config_parses_tool_timeouts() {
        let toml_str = r#"
[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
tool_timeout_sec = 60
tool_timeouts = { create_issue = 120, search_repositories = 30 }
"#;
        let root = toml::from_str::<TomlValue>(toml_str).unwrap();
        let servers = parse_mcp_servers_from_toml(&root);
        let github = servers.get("github").unwrap();

        assert_eq!(github.tool_timeout_sec, Some(60));
        let tt = github.tool_timeouts.as_ref().unwrap();
        assert_eq!(tt.get("create_issue"), Some(&120));
        assert_eq!(tt.get("search_repositories"), Some(&30));
        assert_eq!(tt.get("nonexistent"), None);
    }

    #[test]
    fn test_mcp_server_config_tool_timeouts_defaults_to_none() {
        let toml_str = r#"
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem"]
"#;
        let root = toml::from_str::<TomlValue>(toml_str).unwrap();
        let servers = parse_mcp_servers_from_toml(&root);
        let fs = servers.get("filesystem").unwrap();

        assert!(fs.tool_timeouts.is_none());
        assert!(fs.tool_timeout_sec.is_none());
        assert!(fs.expose_image_base64.is_none());
    }

    #[test]
    fn test_mcp_server_config_parses_expose_image_base64() {
        let toml_str = r#"
[mcp_servers.grafana]
url = "https://grafana.example/mcp"
expose_image_base64 = true
"#;
        let root = toml::from_str::<TomlValue>(toml_str).unwrap();
        let servers = parse_mcp_servers_from_toml(&root);
        let grafana = servers.get("grafana").unwrap();
        assert_eq!(grafana.expose_image_base64, Some(true));
    }

    #[test]
    fn mcp_json_oauth_block_parsed_into_oauth_config() {
        let json = r#"{
            "mcpServers": {
                "slack": {
                    "type": "http",
                    "url": "https://mcp.slack.example/mcp",
                    "oauth": { "clientId": "slack-byo-client", "callbackPort": 3118 }
                }
            }
        }"#;
        let config: McpConfig = serde_json::from_str(json).expect("parse .mcp.json");
        let slack = config.mcp_servers.get("slack").expect("slack server");

        let block = slack.oauth.as_ref().expect("oauth block parsed");
        assert_eq!(block.client_id.as_deref(), Some("slack-byo-client"));
        assert_eq!(block.callback_port, Some(3118));

        let oauth = slack.oauth_config().expect("oauth_config from block");
        assert_eq!(oauth.client_id.as_deref(), Some("slack-byo-client"));
        assert_eq!(oauth.callback_port, Some(3118));
    }

    #[test]
    fn load_cursor_mcp_servers_as_configs_parses_cursor_mcp_json() {
        // NOTE: This test cannot override HOME (dirs::home_dir is not
        // controlled by an env var on all platforms), so we test the
        // underlying read_mcp_json + McpConfig round-trip instead.
        let dir = tempfile::tempdir().unwrap();
        let mcp_json_path = dir.path().join("mcp.json");
        std::fs::write(
            &mcp_json_path,
            r#"{
                "mcpServers": {
                    "test_server": {
                        "command": "node",
                        "args": ["server.js"]
                    }
                }
            }"#,
        )
        .unwrap();
        let config = read_mcp_json(&mcp_json_path).expect("should parse cursor mcp.json");
        assert_eq!(config.mcp_servers.len(), 1);
        assert!(config.mcp_servers.contains_key("test_server"));
    }

    /// Path-based loader used by kill-switch attribution (no mcps/import gates).
    #[test]
    fn load_claude_json_mcp_servers_from_reads_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let claude_json = dir.path().join("claude.json");
        let cwd = dir.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        // Build via serde so Windows paths with `\` are JSON-escaped correctly
        // (string replace into a JSON literal produces invalid escapes).
        let cwd_key = cwd.to_string_lossy().into_owned();
        let fixture = serde_json::json!({
            "mcpServers": {
                "killswitch-claude": { "command": "true" }
            },
            "projects": {
                cwd_key: {
                    "mcpServers": {
                        "killswitch-claude-proj": { "command": "true" }
                    }
                }
            }
        });
        std::fs::write(
            &claude_json,
            serde_json::to_string(&fixture).expect("serialize claude.json fixture"),
        )
        .unwrap();

        let servers = load_claude_json_mcp_servers_from(&claude_json, &cwd);
        let names: Vec<_> = servers
            .iter()
            .map(|s| match s {
                acp::McpServer::Stdio(stdio) => stdio.name.as_str(),
                acp::McpServer::Http(http) => http.name.as_str(),
                acp::McpServer::Sse(sse) => sse.name.as_str(),
                _ => "",
            })
            .collect();
        assert!(
            names.contains(&"killswitch-claude"),
            "user-level server must load: {names:?}"
        );
        assert!(
            names.contains(&"killswitch-claude-proj"),
            "project-scoped server must load: {names:?}"
        );
    }

    #[test]
    fn load_cursor_mcp_servers_as_configs_returns_empty_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let mcp_json_path = dir.path().join("mcp.json");
        // File does not exist — should not panic, just return None.
        assert!(read_mcp_json(&mcp_json_path).is_none());
    }

    #[test]
    fn mcp_json_env_var_default_value() {
        let tmp = tempfile::tempdir().unwrap();
        let mcp_path = tmp.path().join(".mcp.json");
        std::fs::write(
            &mcp_path,
            r#"{
                "mcpServers": {
                    "api": {
                        "url": "${ECHO_AGENT_TEST_MCP_UNSET_VAR_12345:-https://fallback.example.com}/mcp"
                    }
                }
            }"#,
        )
        .unwrap();

        let servers = load_mcp_json_file(&mcp_path);
        assert_eq!(servers.len(), 1);
        match &servers[0] {
            acp::McpServer::Http(acp::McpServerHttp { url, .. }) => {
                assert_eq!(url, "https://fallback.example.com/mcp");
            }
            _other => panic!("expected Http"),
        }
    }

    #[test]
    fn mcp_json_all_toml_names_includes_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let echo_agent_dir = tmp.path().join(".echo-agent");
        std::fs::create_dir_all(&echo_agent_dir).unwrap();
        std::fs::write(
            echo_agent_dir.join("config.toml"),
            r#"
[mcp_servers.enabled_one]
url = "https://example.com"

[mcp_servers.disabled_one]
command = "/ignored"
enabled = false
"#,
        )
        .unwrap();
        git2::Repository::init(tmp.path()).unwrap();

        let names = all_toml_mcp_server_names(tmp.path());
        assert!(names.contains("enabled_one"));
        assert!(names.contains("disabled_one"));
    }

    #[test]
    fn mcp_json_candidate_paths_include_missing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        git2::Repository::init(tmp.path()).unwrap();

        let paths = mcp_json_candidate_paths(&nested);
        assert_eq!(
            paths,
            vec![
                tmp.path().join(".mcp.json"),
                tmp.path().join("a").join(".mcp.json"),
                nested.join(".mcp.json"),
            ]
        );
    }

    #[tokio::test]
    async fn mcp_preferences_missing_malformed_and_save_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp_preferences.json");
        assert!(matches!(
            load_mcp_preferences_from(&path),
            McpPreferencesLoad::Missing
        ));
        assert!(load_mcp_preferences_from(&path).file().servers.is_empty());

        std::fs::write(&path, "not json").unwrap();
        assert!(matches!(
            load_mcp_preferences_from(&path),
            McpPreferencesLoad::Corrupt
        ));
        let prefs = McpPreferencesFile {
            version: 1,
            servers: HashMap::from([(
                "acme".to_string(),
                McpServerPreferences {
                    values: HashMap::from([("site".to_string(), "us5".to_string())]),
                    source: Some(McpPreferenceSource {
                        kind: "plugin".to_string(),
                        plugin: Some("acme".to_string()),
                        scope: None,
                    }),
                    updated_at: Some("2026-06-19T00:00:00Z".to_string()),
                },
            )]),
        };
        assert!(save_mcp_preferences_to(&path, &prefs).await.is_err());

        std::fs::remove_file(&path).unwrap();
        save_mcp_preferences_to(&path, &prefs).await.unwrap();
        let loaded = load_mcp_preferences_from(&path).file();
        assert_eq!(loaded.servers["acme"].values["site"], "us5");
        assert_eq!(
            loaded.servers["acme"]
                .source
                .as_ref()
                .unwrap()
                .plugin
                .as_deref(),
            Some("acme")
        );
    }

    #[test]
    fn apply_mcp_server_enabled_updates_array_and_per_server_field() {
        let mut root: TomlValue = toml::from_str(
            r#"
[mcp_servers.local]
command = "npx"
enabled = true
"#,
        )
        .unwrap();
        let table = root.as_table_mut().unwrap();

        apply_mcp_server_enabled(table, "local", false);
        let disabled = table
            .get("disabled_mcp_servers")
            .and_then(|v| v.as_array())
            .expect("disabled_mcp_servers array");
        assert_eq!(disabled.len(), 1);
        assert_eq!(disabled[0].as_str(), Some("local"));
        assert_eq!(
            table
                .get("mcp_servers")
                .and_then(|v| v.as_table())
                .and_then(|s| s.get("local"))
                .and_then(|v| v.as_table())
                .and_then(|s| s.get("enabled"))
                .and_then(|v| v.as_bool()),
            Some(false)
        );

        apply_mcp_server_enabled(table, "local", true);
        assert!(table.get("disabled_mcp_servers").is_none());
        assert_eq!(
            table
                .get("mcp_servers")
                .and_then(|v| v.as_table())
                .and_then(|s| s.get("local"))
                .and_then(|v| v.as_table())
                .and_then(|s| s.get("enabled"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn apply_mcp_server_enabled_managed_name_only_updates_array() {
        let mut root: TomlValue = toml::from_str("disabled_mcp_servers = []\n").unwrap();
        let table = root.as_table_mut().unwrap();

        apply_mcp_server_enabled(table, "echo_agent_com_slack", false);
        let disabled = table
            .get("disabled_mcp_servers")
            .and_then(|v| v.as_array())
            .expect("disabled_mcp_servers array");
        assert_eq!(disabled.len(), 1);
        assert_eq!(disabled[0].as_str(), Some("echo_agent_com_slack"));
        assert!(table.get("mcp_servers").is_none());

        apply_mcp_server_enabled(table, "echo_agent_com_slack", true);
        assert!(table.get("disabled_mcp_servers").is_none());
        assert!(table.get("mcp_servers").is_none());
    }

    #[tokio::test]
    async fn clear_sticky_project_disabled_at_only_flips_false() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mcp_servers.proj]
command = "npx"
enabled = false
"#,
        )
        .unwrap();
        assert!(
            clear_sticky_project_disabled_at(&path, "proj")
                .await
                .unwrap()
        );
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("enabled = true"), "{body}");
        // Already true: no write.
        assert!(
            !clear_sticky_project_disabled_at(&path, "proj")
                .await
                .unwrap()
        );
    }

    /// Nested project configs both sticky-disable the same server; enable
    /// unstick must rewrite cwd-nearest only (not the shadowed ancestor).
    #[tokio::test]
    async fn enable_unstick_only_touches_nearest_project_definition() {
        let tmp = tempfile::tempdir().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        let nested = tmp.path().join("pkg");
        std::fs::create_dir_all(nested.join(".echo-agent")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".echo-agent")).unwrap();

        let sticky = r#"
# keep me
[mcp_servers.svc]
command = "npx"
enabled = false
"#;
        let ancestor = tmp.path().join(".echo-agent").join("config.toml");
        let nearer = nested.join(".echo-agent").join("config.toml");
        std::fs::write(&ancestor, sticky).unwrap();
        std::fs::write(&nearer, sticky).unwrap();

        assert_eq!(
            nearest_project_mcp_definition(&nested, "svc").as_ref(),
            Some(&nearer)
        );

        let path = nearest_project_mcp_definition(&nested, "svc").unwrap();
        clear_sticky_project_disabled_at(&path, "svc")
            .await
            .unwrap();

        let nearer_body = std::fs::read_to_string(&nearer).unwrap();
        assert!(
            nearer_body.contains("enabled = true"),
            "nearest should be unstuck: {nearer_body}"
        );
        assert!(
            nearer_body.contains("# keep me"),
            "toml_edit must preserve comments: {nearer_body}"
        );
        let ancestor_body = std::fs::read_to_string(&ancestor).unwrap();
        assert!(
            ancestor_body.contains("enabled = false"),
            "shadowed ancestor must stay sticky: {ancestor_body}"
        );

        set_sticky_project_disabled_at(&nearer, "svc")
            .await
            .unwrap();
        let re_stuck = std::fs::read_to_string(&nearer).unwrap();
        assert!(
            re_stuck.contains("enabled = false"),
            "re-stick must set enabled=false: {re_stuck}"
        );
        assert!(
            re_stuck.contains("# keep me"),
            "re-stick must preserve comments: {re_stuck}"
        );
    }

    #[tokio::test]
    async fn restore_mcp_server_enabled_after_enable_scopes_tiers() {
        // Hermetic: only touch a temp project path. Do not call
        // save_mcp_server_enabled_in (that reads ambient config_path / echo_agent_home).
        let project = tempfile::tempdir().unwrap();
        let project_cfg = project.path().join("config.toml");
        std::fs::write(
            &project_cfg,
            r#"
# keep me
[mcp_servers.svc]
command = "true"
enabled = false
"#,
        )
        .unwrap();

        clear_sticky_project_disabled_at(&project_cfg, "svc")
            .await
            .unwrap();
        let unstuck = std::fs::read_to_string(&project_cfg).unwrap();
        assert!(unstuck.contains("enabled = true"), "{unstuck}");
        assert!(unstuck.contains("# keep me"), "{unstuck}");

        // Project-only modified list: restore must re-stick without needing user tier.
        restore_mcp_server_enabled_after_enable("svc", std::slice::from_ref(&project_cfg))
            .await
            .unwrap();

        let project_body = std::fs::read_to_string(&project_cfg).unwrap();
        assert!(project_body.contains("enabled = false"), "{project_body}");
        assert!(project_body.contains("# keep me"), "{project_body}");
    }

    #[tokio::test]
    async fn write_toml_table_if_changed_refuses_unparseable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("broken.toml");
        std::fs::write(&path, "not = [valid\n").unwrap();
        let err = write_toml_table_if_changed(&path, |_t| {})
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("unparseable"),
            "expected refuse, got {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "not = [valid\n",
            "file must be left intact"
        );
    }

    // === merge_section tests ===
}
