//! Shell-side MCP merge: local/plugin/compat sources plus admitted client
//! servers. Managed connectors exist only via the gateway catalog
//! (`GET /v1/mcp/tools/list`), not as injected `echo_agent_com_*` HTTP servers.
//!
//! Merge layers are applied in order, keyed by server NAME (two names sharing
//! one URL are distinct servers); later `insert()` beats earlier `or_insert()`:
//!   - config.toml    — seeds the map; `enabled = false` blocks lower layers
//!   - Plugins        — `or_insert` (won't override config.toml)
//!   - ~/.claude.json — `or_insert` (imported user/local MCP servers)
//!   - `.mcp.json`    — `or_insert` (team baseline)
//!   - Client         — `insert` (wins except servers rejected by a disabled
//!                      vendor `mcps` kill switch, which matches by normalized
//!                      URL; see `admit_client_mcp_servers`)
//!
//! The gateway catalog/call core lives in
//! `echo_agent_runtime_session_support::managed_mcp` and is re-exported here so
//! `crate::session::managed_mcp::…` paths keep resolving unchanged.

pub use echo_agent_runtime_session_support::managed_mcp::*;

use std::collections::HashMap;

use agent_client_protocol as acp;

/// Vendor kill-switch attribution key: normalized URL for Http/Sse (a client
/// re-forwards the same endpoint under any display name), name for Stdio.
/// Only for [`admit_client_mcp_servers`]; merge/discovery maps key by name.
fn mcp_vendor_block_key(s: &acp::McpServer) -> String {
    match s {
        acp::McpServer::Http(acp::McpServerHttp { url, .. })
        | acp::McpServer::Sse(acp::McpServerSse { url, .. }) => normalize_url(url),
        acp::McpServer::Stdio(acp::McpServerStdio { name, .. }) => name.clone(),
        // TODO(acp-0.10): `McpServer` is #[non_exhaustive].
        _ => String::new(),
    }
}

pub(crate) fn mcp_server_name(s: &acp::McpServer) -> &str {
    match s {
        acp::McpServer::Http(acp::McpServerHttp { name, .. })
        | acp::McpServer::Sse(acp::McpServerSse { name, .. })
        | acp::McpServer::Stdio(acp::McpServerStdio { name, .. }) => name,
        // TODO(acp-0.10): `McpServer` is #[non_exhaustive].
        _ => "",
    }
}

/// Merge/discovery map key: server NAME is the sole merge identity (two names
/// sharing one URL are distinct servers). Every name-keyed map in this module
/// must derive its key through this helper so merge and discovery keying
/// cannot desynchronize.
fn mcp_merge_key(s: &acp::McpServer) -> String {
    mcp_server_name(s).to_string()
}

/// Apply an expert's connector boundary at every session ingress, including
/// reconnects and hot reloads. Declared references are additive to inheritance.
pub(crate) fn agent_allows_mcp_server(
    definition: &echo_agent_core::AgentDefinition,
    name: &str,
) -> bool {
    use echo_agent_core::config::{McpInheritance, McpServerRef};
    definition.mcp_servers.iter().any(|entry| match entry {
        McpServerRef::Named(declared) | McpServerRef::Inline { name: declared, .. } => {
            declared == name
        }
    }) || match &definition.mcp_inheritance {
        McpInheritance::All => true,
        McpInheritance::None => false,
        McpInheritance::Named(names) => names.iter().any(|entry| entry == name),
        McpInheritance::Except(names) => !names.iter().any(|entry| entry == name),
    }
}

pub(crate) fn resolve_agent_mcp_servers(
    servers: Vec<acp::McpServer>,
    definition: &echo_agent_core::AgentDefinition,
    cwd: &std::path::Path,
) -> Vec<acp::McpServer> {
    use echo_agent_core::config::McpServerRef;
    let mut selected: HashMap<String, acp::McpServer> = servers
        .into_iter()
        .filter(|server| agent_allows_mcp_server(definition, mcp_server_name(server)))
        .map(|server| (mcp_merge_key(&server), server))
        .collect();
    if definition.plugin_name.is_none() {
        for entry in &definition.mcp_servers {
            let McpServerRef::Inline { name, config } = entry else {
                continue;
            };
            let Some(mut object) = config.as_object().cloned() else {
                continue;
            };
            object.insert("name".into(), serde_json::json!(name));
            let transport =
                serde_json::from_value::<acp::McpServer>(serde_json::Value::Object(object))
                    .ok()
                    .or_else(|| {
                        let config = serde_json::from_value(config.clone()).ok()?;
                        crate::util::config::materialize_mcp_config(
                            name,
                            config,
                            &crate::util::config::load_mcp_preferences().file(),
                            &crate::config::expand_env_vars_in_string,
                            crate::util::config::McpEnabledFilter::Respect,
                        )
                    });
            if let Some(server) = transport {
                selected.insert(name.clone(), server);
            } else {
                tracing::warn!(agent = %definition.name, server = name, "Expert connector configuration could not be resolved");
            }
        }
    }
    let mut selected: Vec<_> = selected.into_values().collect();
    selected.sort_by(|a, b| mcp_server_name(a).cmp(mcp_server_name(b)));
    let selected = crate::agent::folder_trust::filter_untrusted_project_mcp(cwd, selected);
    apply_mcp_server_policy(
        selected,
        &crate::util::config::disabled_mcp_server_names(cwd),
        &echo_agent_workspace::permission::resolution::managed_settings().mcp_allowlist,
    )
    .into_iter()
    .filter(|entry| entry.disabled_reason.is_none())
    .map(|entry| entry.server)
    .collect()
}

pub(crate) fn merge_managed_mcp_servers(
    client_mcp_servers: Vec<acp::McpServer>,
    cwd: &std::path::Path,
    plugin_registry: Option<&echo_agent_core::plugins::PluginRegistry>,
    compat: &echo_agent_tools::types::compat::CompatConfig,
) -> Vec<acp::McpServer> {
    merge_managed_mcp_servers_with_policy(client_mcp_servers, cwd, plugin_registry, compat)
        .into_iter()
        .filter(|s| s.disabled_reason.is_none())
        .map(|s| s.server)
        .collect()
}

/// Merge local/plugin/client MCP sources into ONE live session and push the
/// result via [`crate::session::SessionCommand::UpdateMcpServers`]; returns
/// `true` if the command was enqueued (session still alive).
///
/// Shared core for every "re-merge MCP sources into a live session" path
/// (config hot-reload, post-grant reload, plugin reload) so the merge
/// inputs and the dropped-oneshot-response contract can't drift between them.
pub(crate) fn merge_and_send_managed_mcp_update(
    cmd_tx: &tokio::sync::mpsc::UnboundedSender<crate::session::SessionCommand>,
    cwd: &std::path::Path,
    initial_client_mcp_servers: Vec<acp::McpServer>,
    plugin_registry: Option<&echo_agent_core::plugins::PluginRegistry>,
    compat: &echo_agent_tools::types::compat::CompatConfig,
) -> bool {
    let merged =
        merge_managed_mcp_servers(initial_client_mcp_servers, cwd, plugin_registry, compat);
    let (tx, _rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(crate::session::SessionCommand::UpdateMcpServers {
            mcp_servers: merged,
            respond_to: tx,
        })
        .is_ok()
}

/// Drop client-forwarded servers that match on-disk vendor MCP configs while
/// that vendor's `mcps` kill switch is off.
///
/// Call at session ingress before storing the hot-reload seed
/// (`initial_client_mcp_servers`) so later merges cannot re-admit a previously
/// rejected server merely because disk attribution vanished. Explicit later
/// client updates re-run this with current disk, so a now-client-only server
/// can be admitted once it no longer matches disabled-vendor disk.
pub(crate) fn admit_client_mcp_servers(
    client_mcp_servers: Vec<acp::McpServer>,
    cwd: &std::path::Path,
    compat: &echo_agent_tools::types::compat::CompatConfig,
) -> Vec<acp::McpServer> {
    let mut blocked: std::collections::HashSet<String> = std::collections::HashSet::new();
    if !compat.cursor.mcps {
        let mut forced = *compat;
        forced.cursor.mcps = true;
        blocked.extend(
            crate::util::config::load_cursor_mcp_servers(cwd, &forced)
                .iter()
                .map(mcp_vendor_block_key),
        );
    }
    if !compat.claude.mcps {
        // Attribution must see disk even when import-marker / runtime gates
        // empty the normal Claude loader.
        blocked.extend(
            crate::util::config::load_claude_json_mcp_servers_for_attribution(cwd)
                .iter()
                .map(mcp_vendor_block_key),
        );
    }
    if blocked.is_empty() {
        return client_mcp_servers;
    }
    client_mcp_servers
        .into_iter()
        .filter(|s| !blocked.contains(&mcp_vendor_block_key(s)))
        .collect()
}

pub(crate) fn merge_managed_mcp_servers_with_policy(
    client_mcp_servers: Vec<acp::McpServer>,
    cwd: &std::path::Path,
    plugin_registry: Option<&echo_agent_core::plugins::PluginRegistry>,
    compat: &echo_agent_tools::types::compat::CompatConfig,
) -> Vec<McpServerWithPolicy> {
    let mut servers: HashMap<String, acp::McpServer> =
        merge_managed_mcp_servers_sourced(cwd, plugin_registry, compat)
            .into_iter()
            .map(|(s, _source)| (mcp_merge_key(&s), s))
            .collect();

    // Re-admit at merge so a caller that forgot ingress sanitization cannot
    // spawn disabled-vendor client servers.
    for server in admit_client_mcp_servers(client_mcp_servers, cwd, compat) {
        servers.insert(mcp_merge_key(&server), server);
    }

    let disabled = crate::util::config::disabled_mcp_server_names(cwd);

    let mut merged: Vec<acp::McpServer> = servers.into_values().collect();
    // Deterministic order: this list is collected from a HashMap (random
    // iteration order). Downstream equality checks (`mcp_servers_equal`, used
    // by both `update_configs` and the `update_configs_diff` short-circuit) are
    // order-sensitive, so an unsorted list makes an unchanged server set look
    // changed — spuriously cancelling/restarting MCP init on e.g. a hooks-only
    // plugin reload. Sorting by name (the unique merge key) keeps reloads a
    // true no-op when nothing changed.
    merged.sort_by(|a, b| mcp_server_name(a).cmp(mcp_server_name(b)));
    // Folder-trust gate: when `cwd`'s workspace is untrusted, drop its
    // repo-local (project-scoped) servers before they can be spawned. No-op for
    // a trusted/unrecorded workspace. Composes with the managed-deny allowlist
    // applied next (both filters run on the survivors).
    let merged = crate::agent::folder_trust::filter_untrusted_project_mcp(cwd, merged);
    let allowlist = &echo_agent_workspace::permission::resolution::managed_settings().mcp_allowlist;
    apply_mcp_server_policy(merged, &disabled, allowlist)
}

/// Why an MCP server was disabled by policy.
#[derive(Debug, Clone)]
pub(crate) enum McpDisabledReason {
    Allowlist { source: std::path::PathBuf },
    Denylist { source: std::path::PathBuf },
}

impl std::fmt::Display for McpDisabledReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Allowlist { source } => {
                write!(f, "not in allowedMcpServers ({})", source.display())
            }
            Self::Denylist { source } => {
                write!(f, "matches deniedMcpServers ({})", source.display())
            }
        }
    }
}

impl McpDisabledReason {
    /// Classify why a blocked server was rejected by the managed-settings
    /// MCP policy: an explicit deny match vs a missing allowlist entry.
    pub(crate) fn for_blocked_server(
        policy: &echo_agent_workspace::permission::resolution::McpServerAllowlist,
        server: &acp::McpServer,
    ) -> Self {
        let source = policy.source_path.clone().unwrap_or_default();
        if policy.is_server_denied(server) {
            Self::Denylist { source }
        } else {
            Self::Allowlist { source }
        }
    }
}

/// An MCP server paired with its policy status.
pub(crate) struct McpServerWithPolicy {
    pub server: acp::McpServer,
    pub disabled_reason: Option<McpDisabledReason>,
}

/// Tag each merged MCP server with its managed-settings policy status and drop
/// names disabled in config.toml. A server that fails `is_server_allowed` is
/// tagged with a deny-vs-allowlist `McpDisabledReason` (via
/// `McpDisabledReason::for_blocked_server`); the public
/// `merge_managed_mcp_servers` then drops every tagged server. Split out from
/// `merge_managed_mcp_servers_with_policy` so the deny/allow enforcement
/// chokepoint can be tested with an injected allowlist — the runtime path reads
/// the process-wide managed-settings `OnceLock`, which a test can't populate.
fn apply_mcp_server_policy(
    merged: Vec<acp::McpServer>,
    disabled: &std::collections::HashSet<String>,
    allowlist: &echo_agent_workspace::permission::resolution::McpServerAllowlist,
) -> Vec<McpServerWithPolicy> {
    merged
        .into_iter()
        .filter_map(|server| {
            if disabled.contains(mcp_server_name(&server)) {
                return None;
            }
            if !allowlist.is_server_allowed(&server) {
                let reason = McpDisabledReason::for_blocked_server(allowlist, &server);
                tracing::warn!(
                    name = mcp_server_name(&server),
                    reason = %reason,
                    "MCP server blocked by managed settings policy"
                );
                return Some(McpServerWithPolicy {
                    server,
                    disabled_reason: Some(reason),
                });
            }
            Some(McpServerWithPolicy {
                server,
                disabled_reason: None,
            })
        })
        .collect()
}

/// Like [`merge_managed_mcp_servers`] but returns `ConfigSource` alongside each server.
pub(crate) fn merge_managed_mcp_servers_sourced(
    cwd: &std::path::Path,
    plugin_registry: Option<&echo_agent_core::plugins::PluginRegistry>,
    compat: &echo_agent_tools::types::compat::CompatConfig,
) -> Vec<(
    acp::McpServer,
    echo_agent_tools::types::config_source::ConfigSource,
)> {
    let _mcp_merge_timer = crate::instrumentation::timer("mcp_merge_managed");
    use echo_agent_tools::types::config_source::ConfigSource;

    let toml_claimed_names = crate::util::config::all_toml_mcp_server_names(cwd);

    let config_source = ConfigSource::ConfigToml {
        path: echo_agent_tools::util::echo_agent_home::echo_agent_home().join("config.toml"),
    };

    // Use the TOML-only loader so that entries from imported editor configs
    // and .mcp.json are not pre-loaded with ConfigSource::ConfigToml.  Those
    // sources are added below with their correct ConfigSource variants.
    let mut servers: HashMap<String, (acp::McpServer, ConfigSource)> =
        crate::util::config::load_mcp_servers_toml_only(cwd)
            .into_iter()
            .map(|s| {
                let key = mcp_merge_key(&s);
                (key, (s, config_source.clone()))
            })
            .collect();
    for (name, (_, source)) in &servers {
        tracing::info!(server = name, source = ?source, "MCP server loaded from source");
    }

    for (server, source) in
        non_toml_mcp_servers_with_source(cwd, plugin_registry, compat, &toml_claimed_names)
    {
        servers
            .entry(mcp_merge_key(&server))
            .or_insert((server, source));
    }

    servers.into_values().collect()
}

/// Plugin / Claude / Cursor / `.mcp.json` servers in merge priority order.
///
/// Callers insert with `entry(name).or_insert` so the first listed source wins
/// a shared name. TOML is applied separately (last-wins for merge and for
/// discovery force-enable).
fn non_toml_mcp_servers_with_source(
    cwd: &std::path::Path,
    plugin_registry: Option<&echo_agent_core::plugins::PluginRegistry>,
    compat: &echo_agent_tools::types::compat::CompatConfig,
    toml_claimed_names: &std::collections::HashSet<String>,
) -> Vec<(
    acp::McpServer,
    echo_agent_tools::types::config_source::ConfigSource,
)> {
    use echo_agent_tools::types::config_source::ConfigSource;

    let mut out = Vec::new();

    if let Some(registry) = plugin_registry {
        for plugin in registry.active_plugins() {
            let mut plugin_servers: Vec<acp::McpServer> = Vec::new();
            if let Some(ref mcp_path) = plugin.mcp_config_path {
                let (servers, _) = load_plugin_mcp_servers(
                    mcp_path,
                    &plugin.name,
                    &plugin.root_str(),
                    &plugin.data_dir_str(),
                );
                plugin_servers.extend(servers);
            }
            if let Some(ref inline_value) = plugin.inline_mcp_servers {
                let (servers, _) = load_plugin_mcp_servers_from_value(
                    inline_value,
                    &plugin.name,
                    &plugin.root_str(),
                    &plugin.data_dir_str(),
                );
                plugin_servers.extend(servers);
            }
            if plugin_servers.is_empty() {
                continue;
            }
            let mut seen_names: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            plugin_servers.retain(|server| seen_names.insert(mcp_server_name(server).to_string()));
            let source = ConfigSource::Plugin {
                plugin_name: plugin.name.clone(),
                path: plugin.root.clone(),
            };
            for server in plugin_servers {
                if toml_claimed_names.contains(mcp_server_name(&server)) {
                    continue;
                }
                out.push((server, source.clone()));
            }
        }
    }

    let claude_json_source = ConfigSource::ClaudeJson {
        path: echo_agent_config::discovery_home_dir()
            .map(|h| h.join(".claude.json"))
            .unwrap_or_default(),
    };
    for server in crate::util::config::load_claude_json_mcp_servers(cwd, compat) {
        if toml_claimed_names.contains(mcp_server_name(&server)) {
            continue;
        }
        out.push((server, claude_json_source.clone()));
    }

    let cursor_mcp_source = ConfigSource::McpJson {
        path: echo_agent_config::discovery_home_dir()
            .map(|h| h.join(".cursor").join("mcp.json"))
            .unwrap_or_default(),
    };
    for server in crate::util::config::load_cursor_mcp_servers(cwd, compat) {
        if toml_claimed_names.contains(mcp_server_name(&server)) {
            continue;
        }
        out.push((server, cursor_mcp_source.clone()));
    }

    let mcp_json_source = ConfigSource::McpJson {
        path: cwd.join(".mcp.json"),
    };
    for server in crate::util::config::load_mcp_json_servers(cwd) {
        if toml_claimed_names.contains(mcp_server_name(&server)) {
            continue;
        }
        out.push((server, mcp_json_source.clone()));
    }

    out
}

/// Shared inputs for MCP definition discovery (list stubs and setup probe).
#[derive(Clone, Copy)]
pub(crate) struct McpDiscoveryInputs<'a> {
    pub cwd: &'a std::path::Path,
    pub plugin_registry: Option<&'a echo_agent_core::plugins::PluginRegistry>,
    pub compat: &'a echo_agent_tools::types::compat::CompatConfig,
}

/// Definitions that would exist if personal disable were cleared.
///
/// Presence probe for list stubs (not a spawnable merge result). Shares the
/// non-TOML walk ([`non_toml_mcp_servers_with_source`]), name keying (TOML
/// wins a name, lower tiers `or_insert`), and folder-trust last with
/// [`merge_managed_mcp_servers_with_policy`]. Does **not** include client
/// forwarded servers. TOML `enabled = false` is force-enabled for stubs.
/// Returns transports keyed by server name.
pub(crate) fn discover_mcp_definitions_ignoring_disable(
    inputs: &McpDiscoveryInputs<'_>,
) -> HashMap<String, acp::McpServer> {
    use crate::util::config::{
        McpEnabledFilter, load_mcp_preferences, load_mcp_server_configs_with_project,
        materialize_mcp_config,
    };

    let cwd = inputs.cwd;
    let plugin_registry = inputs.plugin_registry;
    let compat = inputs.compat;

    let preferences = load_mcp_preferences().file();
    let sub = &crate::config::expand_env_vars_in_string;
    let toml_claimed = crate::util::config::all_toml_mcp_server_names(cwd);

    // TOML wins its name (insert); lower tiers or_insert.
    let mut by_name: HashMap<String, acp::McpServer> = HashMap::new();
    for (name, (config, _scope)) in load_mcp_server_configs_with_project(cwd) {
        let Some(transport) =
            materialize_mcp_config(&name, config, &preferences, sub, McpEnabledFilter::Ignore)
        else {
            continue;
        };
        by_name.insert(name, transport);
    }
    for (server, _source) in
        non_toml_mcp_servers_with_source(cwd, plugin_registry, compat, &toml_claimed)
    {
        by_name.entry(mcp_merge_key(&server)).or_insert(server);
    }

    let servers: Vec<acp::McpServer> = by_name.into_values().collect();
    crate::agent::folder_trust::filter_untrusted_project_mcp(cwd, servers)
        .into_iter()
        .map(|s| (mcp_merge_key(&s), s))
        .collect()
}

fn load_plugin_mcp_servers(
    mcp_path: &std::path::Path,
    plugin_name: &str,
    plugin_root: &str,
    plugin_data: &str,
) -> (Vec<acp::McpServer>, crate::util::config::McpOAuthConfigMap) {
    let Some(config) = crate::util::config::read_mcp_json(mcp_path) else {
        return (vec![], crate::util::config::McpOAuthConfigMap::new());
    };
    load_plugin_mcp_servers_from_config(&config, plugin_name, plugin_root, plugin_data)
}

/// Like [`load_plugin_mcp_servers`] but from an in-memory JSON value (no I/O).
fn load_plugin_mcp_servers_from_value(
    root: &serde_json::Value,
    plugin_name: &str,
    plugin_root: &str,
    plugin_data: &str,
) -> (Vec<acp::McpServer>, crate::util::config::McpOAuthConfigMap) {
    let normalized = echo_agent_core::plugins::manifest::normalize_inline_mcp_servers(root);
    let Ok(config) = serde_json::from_value::<crate::util::config::McpConfig>(normalized) else {
        tracing::warn!(plugin = plugin_name, "failed to parse plugin MCP config");
        return (vec![], crate::util::config::McpOAuthConfigMap::new());
    };
    load_plugin_mcp_servers_from_config(&config, plugin_name, plugin_root, plugin_data)
}

fn load_plugin_mcp_servers_from_config(
    config: &crate::util::config::McpConfig,
    plugin_name: &str,
    plugin_root: &str,
    plugin_data: &str,
) -> (Vec<acp::McpServer>, crate::util::config::McpOAuthConfigMap) {
    let sub = |s: &str| -> String {
        let s =
            echo_agent_core::plugins::manifest::substitute_env_vars(s, plugin_root, plugin_data);
        crate::config::expand_env_vars_in_string(&s)
    };
    let label = format!("plugin:{}", plugin_name);
    crate::util::config::parse_mcp_config_with_oauth(config, &label, &sub)
}

pub(crate) fn collect_plugin_oauth_configs(
    plugin_registry: Option<&echo_agent_core::plugins::PluginRegistry>,
) -> crate::util::config::McpOAuthConfigMap {
    let mut oauth_configs = crate::util::config::McpOAuthConfigMap::new();
    let Some(registry) = plugin_registry else {
        return oauth_configs;
    };

    for plugin in registry.active_plugins() {
        if let Some(ref mcp_path) = plugin.mcp_config_path {
            let (_, oauth) = load_plugin_mcp_servers(
                mcp_path,
                &plugin.name,
                &plugin.root_str(),
                &plugin.data_dir_str(),
            );
            for (name, cfg) in oauth {
                oauth_configs.entry(name).or_insert(cfg);
            }
        }
        if let Some(ref inline_value) = plugin.inline_mcp_servers {
            let (_, oauth) = load_plugin_mcp_servers_from_value(
                inline_value,
                &plugin.name,
                &plugin.root_str(),
                &plugin.data_dir_str(),
            );
            for (name, cfg) in oauth {
                oauth_configs.entry(name).or_insert(cfg);
            }
        }
    }

    oauth_configs
}

pub(crate) fn merge_plugin_oauth_into(
    oauth_config_map: &mut crate::util::config::McpOAuthConfigMap,
    plugin_oauth: crate::util::config::McpOAuthConfigMap,
    toml_mcp_names: &std::collections::HashSet<String>,
) {
    for (name, cfg) in plugin_oauth {
        if toml_mcp_names.contains(&name) {
            continue;
        }
        oauth_config_map.insert(name, cfg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_cwd() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// A client-provided server (e.g. a client session binding injected at
    /// `session/new`) exists in no on-disk config and no managed catalog —
    /// the merge must keep it. Config hot-reload handlers
    /// (`reload_all_mcp_servers` / `reload_project_mcp_servers`) rely on this
    /// by re-seeding the merge with the session's
    /// `initial_client_mcp_servers`; if this property breaks, those reloads
    /// silently tear down client-injected servers mid-session.
    #[test]
    fn client_provided_servers_survive_merge() {
        let client = vec![acp::McpServer::Http(
            acp::McpServerHttp::new(
                "demo-mcp".to_string(),
                "http://mcp.example.test/api/mcp".to_string(),
            )
            .headers(vec![]),
        )];
        let cwd = empty_cwd();
        let compat = echo_agent_tools::types::compat::CompatConfig::default();
        let merged = merge_managed_mcp_servers(client, cwd.path(), None, &compat);
        assert!(
            merged.iter().any(|s| matches!(
                s,
                acp::McpServer::Http(acp::McpServerHttp { name, .. }) if name == "demo-mcp"
            )),
            "client-provided server must survive a merge with no disk/managed sources"
        );
    }

    fn write_cursor_project_mcp(cwd: &std::path::Path, name: &str) {
        std::fs::create_dir_all(cwd.join(".cursor")).unwrap();
        std::fs::write(
            cwd.join(".cursor").join("mcp.json"),
            format!(r#"{{"mcpServers": {{"{name}": {{"command": "true"}}}}}}"#),
        )
        .unwrap();
    }

    fn client_stdio(name: &str) -> acp::McpServer {
        acp::McpServer::Stdio(acp::McpServerStdio::new(name.to_string(), "true"))
    }

    /// Vendor mcps kill switch must drop client-forwarded servers that match
    /// on-disk vendor config (pager may still load with default-on compat).
    #[test]
    fn client_cursor_server_dropped_when_cursor_mcps_disabled() {
        let cwd = empty_cwd();
        write_cursor_project_mcp(cwd.path(), "killswitch-cache");
        let mut compat = echo_agent_tools::types::compat::CompatConfig::default();
        compat.cursor.mcps = false;
        let merged = merge_managed_mcp_servers(
            vec![client_stdio("killswitch-cache")],
            cwd.path(),
            None,
            &compat,
        );
        assert!(
            !merged
                .iter()
                .any(|s| mcp_server_name(s) == "killswitch-cache"),
            "client-forwarded cursor server must be dropped when cursor.mcps is off"
        );
    }

    #[test]
    fn client_cursor_server_kept_when_cursor_mcps_enabled() {
        let cwd = empty_cwd();
        write_cursor_project_mcp(cwd.path(), "killswitch-cache");
        let compat = echo_agent_tools::types::compat::CompatConfig::default();
        let merged = merge_managed_mcp_servers(
            vec![client_stdio("killswitch-cache")],
            cwd.path(),
            None,
            &compat,
        );
        assert!(
            merged
                .iter()
                .any(|s| mcp_server_name(s) == "killswitch-cache"),
            "client-forwarded cursor server must remain when cursor.mcps is on"
        );
    }

    #[test]
    fn unrelated_client_server_survives_when_cursor_mcps_disabled() {
        let cwd = empty_cwd();
        write_cursor_project_mcp(cwd.path(), "killswitch-cache");
        let mut compat = echo_agent_tools::types::compat::CompatConfig::default();
        compat.cursor.mcps = false;
        let merged = merge_managed_mcp_servers(
            vec![
                client_stdio("killswitch-cache"),
                client_stdio("client-only-binding"),
            ],
            cwd.path(),
            None,
            &compat,
        );
        assert!(
            !merged
                .iter()
                .any(|s| mcp_server_name(s) == "killswitch-cache"),
            "matching cursor client server must be dropped"
        );
        assert!(
            merged
                .iter()
                .any(|s| mcp_server_name(s) == "client-only-binding"),
            "unrelated client-only server must survive vendor kill switch"
        );
    }

    #[test]
    fn toml_claim_survives_when_client_cursor_insert_skipped() {
        let cwd = empty_cwd();
        write_cursor_project_mcp(cwd.path(), "killswitch-cache");
        std::fs::create_dir_all(cwd.path().join(".echo-agent")).unwrap();
        std::fs::write(
            cwd.path().join(".echo-agent").join("config.toml"),
            r#"
[mcp_servers.killswitch-cache]
command = "echo"
args = ["ok"]
"#,
        )
        .unwrap();
        git2::Repository::init(cwd.path()).unwrap();

        let mut compat = echo_agent_tools::types::compat::CompatConfig::default();
        compat.cursor.mcps = false;
        let merged = merge_managed_mcp_servers(
            vec![client_stdio("killswitch-cache")],
            cwd.path(),
            None,
            &compat,
        );
        let server = merged
            .iter()
            .find(|s| mcp_server_name(s) == "killswitch-cache")
            .expect("toml-claimed server must remain when client cursor insert is skipped");
        match server {
            acp::McpServer::Stdio(acp::McpServerStdio { command, args, .. }) => {
                assert_eq!(command.display().to_string(), "echo");
                assert_eq!(args.as_slice(), &["ok"]);
            }
            other => panic!("expected toml stdio server, got {other:?}"),
        }
    }

    /// Admitted seed must stay empty of the blocked server after vendor disk
    /// vanishes (hot-reload must not re-admit from a sanitized seed).
    #[test]
    fn admitted_seed_stays_blocked_after_vendor_disk_vanishes() {
        let cwd = empty_cwd();
        write_cursor_project_mcp(cwd.path(), "killswitch-cache");
        let mut compat = echo_agent_tools::types::compat::CompatConfig::default();
        compat.cursor.mcps = false;

        let admitted =
            admit_client_mcp_servers(vec![client_stdio("killswitch-cache")], cwd.path(), &compat);
        assert!(
            !admitted
                .iter()
                .any(|s| mcp_server_name(s) == "killswitch-cache"),
            "admit must drop matching vendor client server while flag is off"
        );

        std::fs::remove_file(cwd.path().join(".cursor").join("mcp.json")).unwrap();

        let merged = merge_managed_mcp_servers(admitted, cwd.path(), None, &compat);
        assert!(
            !merged
                .iter()
                .any(|s| mcp_server_name(s) == "killswitch-cache"),
            "admitted seed must not re-admit after vendor disk vanishes"
        );
    }

    /// Http/Sse client identity is normalized URL, not display name.
    #[test]
    fn client_cursor_http_dropped_by_normalized_url_when_mcps_disabled() {
        let cwd = empty_cwd();
        std::fs::create_dir_all(cwd.path().join(".cursor")).unwrap();
        std::fs::write(
            cwd.path().join(".cursor").join("mcp.json"),
            r#"{"mcpServers": {"disk-name": {"url": "https://killswitch.example.test/mcp/"}}}"#,
        )
        .unwrap();
        let mut compat = echo_agent_tools::types::compat::CompatConfig::default();
        compat.cursor.mcps = false;

        let matching = acp::McpServer::Http(
            acp::McpServerHttp::new(
                "killswitch-http".to_string(),
                "https://killswitch.example.test/mcp".to_string(),
            )
            .headers(vec![]),
        );
        let other = acp::McpServer::Http(
            acp::McpServerHttp::new(
                "other-http".to_string(),
                "https://other.example.test/mcp".to_string(),
            )
            .headers(vec![]),
        );
        let merged = merge_managed_mcp_servers(vec![matching, other], cwd.path(), None, &compat);
        assert!(
            !merged
                .iter()
                .any(|s| mcp_server_name(s) == "killswitch-http"),
            "same normalized URL with different name must be dropped"
        );
        assert!(
            merged.iter().any(|s| mcp_server_name(s) == "other-http"),
            "different URL client server must survive"
        );
    }

    /// The merge chokepoint must actually DROP a server matching
    /// `deniedMcpServers`, and classify the drop as a `Denylist` hit (not a
    /// missing `allowlist` entry) — that reason is the user-visible payload for
    /// the toggle error and `mcp doctor` detail. The runtime merge reads the
    /// process-wide managed-settings `OnceLock`, so we exercise the extracted
    /// `apply_mcp_server_policy` seam with an injected allowlist built via the
    /// public `McpServerAllowlist::new`.
    #[test]
    fn merge_drops_denied_server_and_classifies_as_denylist() {
        use echo_agent_workspace::permission::resolution::{AllowedMcpServer, McpServerAllowlist};

        // Deny-only policy (no allowlist) blocking one host.
        let allowlist = McpServerAllowlist::new(
            vec![],
            vec![AllowedMcpServer::Http {
                url_pattern: "https://blocked.corp.com/*".into(),
            }],
            Some(std::path::PathBuf::from("/test/managed-settings.json")),
        );

        let tagged = apply_mcp_server_policy(
            vec![
                acp::McpServer::Http(
                    acp::McpServerHttp::new("blocked", "https://blocked.corp.com/mcp")
                        .headers(vec![]),
                ),
                acp::McpServer::Http(
                    acp::McpServerHttp::new("ok", "https://ok.corp.com/mcp").headers(vec![]),
                ),
            ],
            &std::collections::HashSet::new(),
            &allowlist,
        );

        // Denied server is classified as a denylist hit, not a missing-allow.
        let blocked = tagged
            .iter()
            .find(|s| mcp_server_name(&s.server) == "blocked")
            .expect("denied server present in policy output");
        assert!(
            matches!(
                blocked.disabled_reason,
                Some(McpDisabledReason::Denylist { .. })
            ),
            "expected Denylist reason, got {:?}",
            blocked.disabled_reason
        );

        // Non-denied server passes untouched.
        let ok = tagged
            .iter()
            .find(|s| mcp_server_name(&s.server) == "ok")
            .expect("allowed server present in policy output");
        assert!(ok.disabled_reason.is_none());

        // The public `merge_managed_mcp_servers` drop predicate removes exactly
        // the denied server.
        let surviving: Vec<&str> = tagged
            .iter()
            .filter(|s| s.disabled_reason.is_none())
            .map(|s| mcp_server_name(&s.server))
            .collect();
        assert_eq!(
            surviving,
            ["ok"],
            "denied server must be dropped by the merge"
        );
    }

    /// A bare policy `serverName` deny drops the managed (prefixed) server as a
    /// `Denylist` hit, exact-match only (no substring over-match).
    #[test]
    fn merge_drops_server_denied_by_name_including_managed_prefix() {
        use echo_agent_workspace::permission::resolution::{AllowedMcpServer, McpServerAllowlist};

        let allowlist = McpServerAllowlist::new(
            vec![],
            vec![AllowedMcpServer::Name {
                name: "slack".into(),
            }],
            Some(std::path::PathBuf::from("/test/managed-settings.json")),
        );

        let tagged = apply_mcp_server_policy(
            vec![
                acp::McpServer::Http(
                    acp::McpServerHttp::new("echo_agent_com_slack", "https://mcp.slack.com/sse")
                        .headers(vec![]),
                ),
                // Substring-only match must not be denied.
                acp::McpServer::Http(
                    acp::McpServerHttp::new("slackbot", "https://slackbot.example.com/mcp")
                        .headers(vec![]),
                ),
            ],
            &std::collections::HashSet::new(),
            &allowlist,
        );

        let slack = tagged
            .iter()
            .find(|s| mcp_server_name(&s.server) == "echo_agent_com_slack")
            .expect("managed server present in policy output");
        assert!(
            matches!(
                slack.disabled_reason,
                Some(McpDisabledReason::Denylist { .. })
            ),
            "name-denied managed server must classify as Denylist, got {:?}",
            slack.disabled_reason
        );

        let bot = tagged
            .iter()
            .find(|s| mcp_server_name(&s.server) == "slackbot")
            .expect("unrelated server present in policy output");
        assert!(
            bot.disabled_reason.is_none(),
            "substring-only match must not be denied by name"
        );

        let surviving: Vec<&str> = tagged
            .iter()
            .filter(|s| s.disabled_reason.is_none())
            .map(|s| mcp_server_name(&s.server))
            .collect();
        assert_eq!(surviving, ["slackbot"]);
    }

    #[test]
    fn policy_server_name_matches_legacy_echo_agent_com_runtime_spelling() {
        use echo_agent_workspace::permission::resolution::{AllowedMcpServer, McpServerAllowlist};

        let managed_server = |runtime: &str| {
            vec![acp::McpServer::Http(
                acp::McpServerHttp::new(runtime.to_string(), "https://mcp.example.com/sse")
                    .headers(vec![]),
            )]
        };
        let name_entry = |display: &str| AllowedMcpServer::Name {
            name: display.to_string(),
        };
        let source = || Some(std::path::PathBuf::from("/test/managed-settings.json"));

        for (display, runtime) in [
            ("Slack", "echo_agent_com_slack"),
            ("My Server", "echo_agent_com_my_server"),
        ] {
            let deny = McpServerAllowlist::new(vec![], vec![name_entry(display)], source());
            let tagged = apply_mcp_server_policy(
                managed_server(runtime),
                &std::collections::HashSet::new(),
                &deny,
            );
            assert!(
                matches!(
                    tagged[0].disabled_reason,
                    Some(McpDisabledReason::Denylist { .. })
                ),
                "deny serverName {display:?} must block runtime {runtime:?}, got {:?}",
                tagged[0].disabled_reason
            );

            let allow = McpServerAllowlist::new(vec![name_entry(display)], vec![], source());
            let tagged = apply_mcp_server_policy(
                managed_server(runtime),
                &std::collections::HashSet::new(),
                &allow,
            );
            assert!(
                tagged[0].disabled_reason.is_none(),
                "allow serverName {display:?} must keep runtime {runtime:?}, got {:?}",
                tagged[0].disabled_reason
            );
        }
    }

    #[test]
    fn lower_precedence_http_servers_are_blocked_by_toml_name_claims() {
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cwd.path().join(".echo-agent")).unwrap();
        std::fs::write(
            cwd.path().join(".echo-agent").join("config.toml"),
            r#"
[mcp_servers.github]
url = "https://config.example.com/mcp"
enabled = false
"#,
        )
        .unwrap();
        git2::Repository::init(cwd.path()).unwrap();
        std::fs::write(
            cwd.path().join(".mcp.json"),
            r#"{
                "mcpServers": {
                    "github": {
                        "url": "https://json.example.com/mcp"
                    }
                }
            }"#,
        )
        .unwrap();

        let compat = echo_agent_tools::types::compat::CompatConfig::default();
        let merged = merge_managed_mcp_servers(vec![], cwd.path(), None, &compat);
        assert!(
            !merged.iter().any(|server| matches!(
                server,
                acp::McpServer::Http(acp::McpServerHttp { name, .. }) if name == "github"
            )),
            "config.toml should block same-named lower-precedence HTTP servers"
        );
    }

    /// Trusted git repo whose project config.toml declares two HTTP servers
    /// sharing one URL, each with its own auth header (GB-5207 shape: one
    /// ClickHouse endpoint, two orgs).
    fn same_url_project_repo() -> tempfile::TempDir {
        let cwd = empty_cwd();
        std::fs::create_dir_all(cwd.path().join(".echo-agent")).unwrap();
        std::fs::write(
            cwd.path().join(".echo-agent").join("config.toml"),
            r#"
[mcp_servers.gb5207-org1]
url = "https://dup-url.example.test/mcp"

[mcp_servers.gb5207-org1.headers]
Authorization = "Bearer org1-token"

[mcp_servers.gb5207-org2]
url = "https://dup-url.example.test/mcp"

[mcp_servers.gb5207-org2.headers]
Authorization = "Bearer org2-token"
"#,
        )
        .unwrap();
        git2::Repository::init(cwd.path()).unwrap();
        crate::agent::folder_trust::record_for_test(cwd.path(), true);
        cwd
    }

    /// Server NAME is the identity: two entries sharing one URL are distinct
    /// servers, and each keeps its own transport config.
    #[test]
    fn same_url_different_names_both_survive_merge() {
        let cwd = same_url_project_repo();
        let compat = echo_agent_tools::types::compat::CompatConfig::default();
        let merged = merge_managed_mcp_servers(vec![], cwd.path(), None, &compat);

        let auth_header = |name: &str| -> &str {
            let server = merged
                .iter()
                .find(|s| mcp_server_name(s) == name)
                .unwrap_or_else(|| panic!("{name} must survive the merge"));
            match server {
                acp::McpServer::Http(acp::McpServerHttp { headers, .. }) => headers
                    .iter()
                    .find(|h| h.name == "Authorization")
                    .unwrap_or_else(|| panic!("{name} must keep its Authorization header"))
                    .value
                    .as_str(),
                other => panic!("expected Http server, got {other:?}"),
            }
        };
        assert_eq!(auth_header("gb5207-org1"), "Bearer org1-token");
        assert_eq!(auth_header("gb5207-org2"), "Bearer org2-token");
    }

    #[test]
    fn same_url_different_names_both_sourced_from_toml() {
        use echo_agent_tools::types::config_source::ConfigSource;

        let cwd = same_url_project_repo();
        let compat = echo_agent_tools::types::compat::CompatConfig::default();
        let sourced = merge_managed_mcp_servers_sourced(cwd.path(), None, &compat);

        for name in ["gb5207-org1", "gb5207-org2"] {
            let (_, source) = sourced
                .iter()
                .find(|(s, _)| mcp_server_name(s) == name)
                .unwrap_or_else(|| panic!("{name} must be in the sourced merge"));
            assert!(
                matches!(source, ConfigSource::ConfigToml { .. }),
                "{name} must be attributed to config.toml, got {source:?}"
            );
        }
    }

    /// End-to-end folder-trust gate through the public merge: an untrusted
    /// workspace's project `.mcp.json` server is dropped before spawn (a
    /// client-supplied server still survives), while a trusted workspace keeps
    /// it. Existing merge tests record no decision, so the default-allowed gate
    /// leaves them a no-op.
    #[test]
    fn untrusted_workspace_drops_project_mcp_servers() {
        fn repo_with_project_server() -> tempfile::TempDir {
            let cwd = tempfile::tempdir().unwrap();
            git2::Repository::init(cwd.path()).unwrap();
            std::fs::write(
                cwd.path().join(".mcp.json"),
                r#"{"mcpServers": {"projsrv": {"url": "https://proj.example.com/mcp"}}}"#,
            )
            .unwrap();
            cwd
        }
        let compat = echo_agent_tools::types::compat::CompatConfig::default();

        let untrusted = repo_with_project_server();
        crate::agent::folder_trust::record_for_test(untrusted.path(), false);
        let client = vec![acp::McpServer::Http(
            acp::McpServerHttp::new(
                "clientsrv".to_string(),
                "https://client.example.com/mcp".to_string(),
            )
            .headers(vec![]),
        )];
        let merged = merge_managed_mcp_servers(client, untrusted.path(), None, &compat);
        assert!(
            !merged.iter().any(|s| mcp_server_name(s) == "projsrv"),
            "untrusted workspace must drop its repo-local MCP server"
        );
        assert!(
            merged.iter().any(|s| mcp_server_name(s) == "clientsrv"),
            "client-supplied server must be retained when untrusted"
        );

        let trusted = repo_with_project_server();
        crate::agent::folder_trust::record_for_test(trusted.path(), true);
        let merged = merge_managed_mcp_servers(vec![], trusted.path(), None, &compat);
        assert!(
            merged.iter().any(|s| mcp_server_name(s) == "projsrv"),
            "trusted workspace must keep its repo-local MCP server"
        );
    }

    #[test]
    fn load_plugin_mcp_creates_stdio_server_with_env_substitution() {
        let config: crate::util::config::McpConfig = serde_json::from_value(serde_json::json!({
            "mcpServers": {
                "echo-mcp": {
                    "command": "python3",
                    "args": ["${ECHO_AGENT_PLUGIN_ROOT}/mcp-echo-server.py"]
                }
            }
        }))
        .expect("parse test MCP config");

        let (servers, _) = load_plugin_mcp_servers_from_config(
            &config,
            "team-tool",
            "/home/user/.echo-agent/plugins/team-tool",
            "/home/user/.echo-agent/plugin-data/team-tool",
        );

        assert_eq!(servers.len(), 1, "should create one server");
        match &servers[0] {
            acp::McpServer::Stdio(acp::McpServerStdio {
                name,
                command,
                args,
                ..
            }) => {
                assert_eq!(name, "echo-mcp");
                assert_eq!(command.display().to_string(), "python3");
                assert_eq!(
                    args.as_slice(),
                    &["/home/user/.echo-agent/plugins/team-tool/mcp-echo-server.py"]
                );
            }
            _other => panic!("expected Stdio server"),
        }
    }

    #[test]
    fn load_plugin_mcp_disabled_server_excluded_from_merge() {
        let config: crate::util::config::McpConfig = serde_json::from_value(serde_json::json!({
            "mcpServers": {
                "test-server": {
                    "command": "node",
                    "args": ["server.js"]
                }
            }
        }))
        .expect("parse test MCP config");

        let (servers, _) = load_plugin_mcp_servers_from_config(
            &config,
            "my-plugin",
            "/tmp/plugin",
            "/tmp/plugin-data",
        );
        assert_eq!(servers.len(), 1);

        // Simulate disabling via disabled_mcp_server_names.
        let disabled: std::collections::HashSet<String> =
            ["test-server".to_string()].into_iter().collect();
        assert!(
            disabled.contains("test-server"),
            "disabled set should contain the server name used in .mcp.json"
        );
    }

    #[test]
    fn load_plugin_mcp_from_value_accepts_direct_map() {
        let value = serde_json::json!({
            "sentry": { "type": "http", "url": "https://mcp.sentry.dev/mcp" }
        });
        let (servers, _) =
            load_plugin_mcp_servers_from_value(&value, "sentry", "/tmp/p", "/tmp/pd");
        assert_eq!(servers.len(), 1);
        match &servers[0] {
            acp::McpServer::Http(acp::McpServerHttp { name, url, .. }) => {
                assert_eq!(name, "sentry");
                assert_eq!(url, "https://mcp.sentry.dev/mcp");
            }
            _other => panic!("expected Http server"),
        }
    }

    #[test]
    fn plugin_server_deduped_across_file_and_inline() {
        use echo_agent_core::plugins::PluginRegistry;
        use echo_agent_core::plugins::PluginScope;
        use echo_agent_core::plugins::discovery::{DiscoveredPlugin, PluginId};
        use echo_agent_core::plugins::manifest::{PathOrInline, PluginManifest};

        let tmp = tempfile::tempdir().unwrap();
        let plugin_root = tmp.path().join("sentry");
        std::fs::create_dir_all(&plugin_root).unwrap();
        let mcp_json = plugin_root.join(".mcp.json");
        std::fs::write(
            &mcp_json,
            r#"{"mcpServers":{"sentry":{"type":"http","url":"https://mcp.sentry.dev/mcp"}}}"#,
        )
        .unwrap();

        let manifest = PluginManifest {
            name: "sentry".into(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: vec![],
            skills: None,
            commands: None,
            agents: None,
            hooks: None,
            mcp_servers: Some(PathOrInline::Inline(serde_json::json!({
                "sentry": { "type": "http", "url": "https://mcp.sentry.dev/mcp" }
            }))),
            lsp_servers: None,
        };
        let id = PluginId::new(PluginScope::User, &plugin_root, "sentry");
        let dp = DiscoveredPlugin {
            manifest,
            id,
            root: plugin_root.clone(),
            canonical_root: plugin_root.clone(),
            scope: PluginScope::User,
            origin: echo_agent_core::plugins::PluginOrigin::UserEchoAgent,
            trusted: true,
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: None,
            mcp_config_path: Some(mcp_json),
            lsp_config_path: None,
            conflict: None,
        };
        let registry = PluginRegistry::from_discovered(vec![dp], &[], &["sentry".to_string()]);

        let cwd = tempfile::tempdir().unwrap();
        let compat = echo_agent_tools::types::compat::CompatConfig::default();
        let sourced = merge_managed_mcp_servers_sourced(cwd.path(), Some(&registry), &compat);

        let sentry_count = sourced
            .iter()
            .filter(|(s, _)| mcp_server_name(s) == "sentry")
            .count();
        assert_eq!(
            sentry_count, 1,
            "sentry declared in both .mcp.json and inline must register exactly once"
        );
    }

    #[test]
    fn plugin_same_name_different_url_keeps_file_server() {
        use echo_agent_core::plugins::PluginRegistry;
        use echo_agent_core::plugins::PluginScope;
        use echo_agent_core::plugins::discovery::{DiscoveredPlugin, PluginId};
        use echo_agent_core::plugins::manifest::{PathOrInline, PluginManifest};

        let tmp = tempfile::tempdir().unwrap();
        let plugin_root = tmp.path().join("sentry");
        std::fs::create_dir_all(&plugin_root).unwrap();
        let mcp_json = plugin_root.join(".mcp.json");
        std::fs::write(
            &mcp_json,
            r#"{"mcpServers":{"sentry":{"type":"http","url":"https://file.example/mcp"}}}"#,
        )
        .unwrap();

        let manifest = PluginManifest {
            name: "sentry".into(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: vec![],
            skills: None,
            commands: None,
            agents: None,
            hooks: None,
            mcp_servers: Some(PathOrInline::Inline(serde_json::json!({
                "sentry": { "type": "http", "url": "https://inline.example/mcp" }
            }))),
            lsp_servers: None,
        };
        let id = PluginId::new(PluginScope::User, &plugin_root, "sentry");
        let dp = DiscoveredPlugin {
            manifest,
            id,
            root: plugin_root.clone(),
            canonical_root: plugin_root.clone(),
            scope: PluginScope::User,
            origin: echo_agent_core::plugins::PluginOrigin::UserEchoAgent,
            trusted: true,
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: None,
            mcp_config_path: Some(mcp_json),
            lsp_config_path: None,
            conflict: None,
        };
        let registry = PluginRegistry::from_discovered(vec![dp], &[], &["sentry".to_string()]);

        let cwd = tempfile::tempdir().unwrap();
        let compat = echo_agent_tools::types::compat::CompatConfig::default();
        let sourced = merge_managed_mcp_servers_sourced(cwd.path(), Some(&registry), &compat);

        let sentry: Vec<&acp::McpServer> = sourced
            .iter()
            .map(|(s, _)| s)
            .filter(|s| mcp_server_name(s) == "sentry")
            .collect();
        assert_eq!(
            sentry.len(),
            1,
            "same plugin declaring one server name twice must register exactly once"
        );
        match sentry[0] {
            acp::McpServer::Http(acp::McpServerHttp { url, .. }) => {
                assert_eq!(url, "https://file.example/mcp", "file source must win");
            }
            other => panic!("expected Http server, got {:?}", other),
        }
    }

    #[test]
    fn collect_plugin_oauth_configs_reads_byo_client_id_from_mcp_json() {
        use echo_agent_core::plugins::PluginRegistry;
        use echo_agent_core::plugins::PluginScope;
        use echo_agent_core::plugins::discovery::{DiscoveredPlugin, PluginId};
        use echo_agent_core::plugins::manifest::PluginManifest;

        let tmp = tempfile::tempdir().unwrap();
        let plugin_root = tmp.path().join("slack");
        std::fs::create_dir_all(&plugin_root).unwrap();
        let mcp_json = plugin_root.join(".mcp.json");
        std::fs::write(
            &mcp_json,
            r#"{"mcpServers":{"slack":{"type":"http","url":"https://mcp.slack.example/mcp","oauth":{"clientId":"slack-byo-client","callbackPort":3118}}}}"#,
        )
        .unwrap();

        let manifest = PluginManifest {
            name: "slack".into(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            repository: None,
            license: None,
            keywords: vec![],
            skills: None,
            commands: None,
            agents: None,
            hooks: None,
            mcp_servers: None,
            lsp_servers: None,
        };
        let id = PluginId::new(PluginScope::User, &plugin_root, "slack");
        let dp = DiscoveredPlugin {
            manifest,
            id,
            root: plugin_root.clone(),
            canonical_root: plugin_root.clone(),
            scope: PluginScope::User,
            origin: echo_agent_core::plugins::PluginOrigin::UserEchoAgent,
            trusted: true,
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: None,
            mcp_config_path: Some(mcp_json),
            lsp_config_path: None,
            conflict: None,
        };
        let registry = PluginRegistry::from_discovered(vec![dp], &[], &["slack".to_string()]);

        let oauth = collect_plugin_oauth_configs(Some(&registry));
        assert_eq!(
            oauth
                .get("slack")
                .expect("slack oauth")
                .client_id
                .as_deref(),
            Some("slack-byo-client")
        );
    }

    #[test]
    fn collect_plugin_oauth_configs_none_registry_is_empty() {
        assert!(collect_plugin_oauth_configs(None).is_empty());
    }

    #[test]
    fn merge_plugin_oauth_respects_source_precedence() {
        use crate::util::config::{McpOAuthConfig, McpOAuthConfigMap};

        let byo = |id: &str| McpOAuthConfig {
            client_id: Some(id.to_string()),
            ..Default::default()
        };

        let mut base = McpOAuthConfigMap::new();
        base.insert("shared".to_string(), byo("file-client"));
        base.insert("toml-svc".to_string(), byo("toml-client"));

        let mut plugin = McpOAuthConfigMap::new();
        plugin.insert("shared".to_string(), byo("plugin-client"));
        plugin.insert("toml-svc".to_string(), byo("plugin-shadow"));
        plugin.insert("plugin-only".to_string(), byo("plugin-only-client"));

        let toml_names: std::collections::HashSet<String> =
            ["toml-svc".to_string()].into_iter().collect();
        merge_plugin_oauth_into(&mut base, plugin, &toml_names);

        assert_eq!(
            base.get("shared").unwrap().client_id.as_deref(),
            Some("plugin-client")
        );
        assert_eq!(
            base.get("toml-svc").unwrap().client_id.as_deref(),
            Some("toml-client")
        );
        assert_eq!(
            base.get("plugin-only").unwrap().client_id.as_deref(),
            Some("plugin-only-client")
        );
    }
}

#[cfg(test)]
mod expert_connector_tests {
    use super::*;
    use echo_agent_core::config::{AgentDefinition, McpInheritance, McpServerRef};
    fn server(name: &str) -> acp::McpServer {
        acp::McpServer::Http(acp::McpServerHttp::new(
            name,
            format!("https://{name}.example/mcp"),
        ))
    }
    #[test]
    fn expert_connector_scope_survives_reconnects_and_newly_added_servers() {
        let cwd = tempfile::tempdir().unwrap();
        let mut definition = AgentDefinition::default_echo_agent_build();
        definition.mcp_inheritance = McpInheritance::None;
        definition.mcp_servers = vec![McpServerRef::Named("repository".into())];
        let selected = resolve_agent_mcp_servers(
            vec![server("repository"), server("mail")],
            &definition,
            cwd.path(),
        );
        assert_eq!(
            selected.iter().map(mcp_server_name).collect::<Vec<_>>(),
            ["repository"]
        );
        let reloaded = resolve_agent_mcp_servers(
            vec![server("mail"), server("calendar"), server("repository")],
            &definition,
            cwd.path(),
        );
        assert_eq!(
            reloaded.iter().map(mcp_server_name).collect::<Vec<_>>(),
            ["repository"]
        );
        assert!(!agent_allows_mcp_server(&definition, "calendar"));
        definition.mcp_inheritance = McpInheritance::Named(vec!["mail".into()]);
        assert!(agent_allows_mcp_server(&definition, "mail"));
        assert!(agent_allows_mcp_server(&definition, "repository"));
        assert!(!agent_allows_mcp_server(&definition, "calendar"));
        definition.mcp_inheritance = McpInheritance::Except(vec!["calendar".into()]);
        assert!(agent_allows_mcp_server(&definition, "mail"));
        assert!(!agent_allows_mcp_server(&definition, "calendar"));
    }
    #[test]
    fn explicitly_declared_inline_connector_is_resolved_without_inheriting_others() {
        let cwd = tempfile::tempdir().unwrap();
        let definition = AgentDefinition::parse("---\nname: Scoped\ndescription: Scoped connectors\nmcpInheritance: none\nmcpServers:\n  - docs:\n      type: http\n      url: https://docs.example/mcp\n      headers: []\n---\nRead docs.").unwrap();
        let servers = resolve_agent_mcp_servers(vec![server("mail")], &definition, cwd.path());
        assert_eq!(
            servers.iter().map(mcp_server_name).collect::<Vec<_>>(),
            ["docs"]
        );
    }
}
