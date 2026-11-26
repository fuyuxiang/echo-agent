//! In-memory registry of active plugins.
//!
//! The `PluginRegistry` is the single source of truth for which plugins
//! are loaded in a session.  It is built once during `MvpAgent` initialization
//! and can be rebuilt via `/plugins reload`.  Each session receives a snapshot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::discovery::{DiscoveredPlugin, PluginId, PluginOrigin, PluginScope};

/// A loaded plugin with resolved components, ready for use by the session.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    /// User-facing plugin name (from manifest or directory name).
    pub name: String,
    /// Stable internal identity.
    pub id: PluginId,
    /// Absolute path to the plugin root directory.
    pub root: PathBuf,
    /// Canonical (symlink-resolved) root path.
    pub canonical_root: PathBuf,
    /// Where this plugin was discovered.
    pub scope: PluginScope,
    /// The concrete discovery source this plugin came from.
    pub origin: PluginOrigin,
    /// Whether the plugin is trusted for executable operations (hooks, MCP,
    /// LSP). Derived from discovery scope: CLI and User plugins are auto-trusted;
    /// Project plugins require explicit trust grant.
    pub trusted: bool,
    /// Whether the plugin is enabled (not in `[plugins].disabled`).
    pub enabled: bool,
    /// Plugin version from manifest.
    pub version: Option<String>,
    /// Plugin description from manifest.
    pub description: Option<String>,
    /// Resolved skill directories.
    pub skill_dirs: Vec<PathBuf>,
    pub command_dirs: Vec<PathBuf>,
    /// Resolved agent directories.
    pub agent_dirs: Vec<PathBuf>,
    /// Resolved hooks file path.
    pub hooks_path: Option<PathBuf>,
    /// Resolved MCP config file path.
    pub mcp_config_path: Option<PathBuf>,
    /// Resolved LSP config file path.
    pub lsp_config_path: Option<PathBuf>,
    /// Number of skill subdirectories found.
    pub skill_count: usize,
    /// Number of agent files found.
    pub agent_count: usize,
    /// Skill names (directory names under skills/).
    pub skill_names: Vec<String>,
    /// Agent/persona names (filenames without .md extension).
    pub agent_names: Vec<String>,
    /// Whether hooks are present (file or inline).
    pub has_hooks: bool,
    /// Number of hook specs defined (file-based + inline).
    pub hook_count: usize,
    /// Whether hooks are inline-only (no file-based hooks.json).
    pub has_inline_hooks_only: bool,
    /// Number of MCP servers defined.
    pub mcp_server_count: usize,
    /// Whether MCP servers are inline-only (no file-based config).
    pub has_inline_mcp_only: bool,
    /// Number of LSP servers defined.
    pub lsp_server_count: usize,
    /// Whether LSP servers are inline-only (no file-based config).
    pub has_inline_lsp_only: bool,
    /// Inline hooks JSON from manifest (when hooks are defined inline, not file-based).
    pub inline_hooks: Option<serde_json::Value>,
    /// Inline MCP servers JSON from manifest (when defined inline, not file-based).
    pub inline_mcp_servers: Option<serde_json::Value>,
    /// Inline LSP servers JSON from manifest (when defined inline, not file-based).
    pub inline_lsp_servers: Option<serde_json::Value>,
    /// Warning if this plugin won a name collision with another plugin.
    pub conflict: Option<String>,
}

impl LoadedPlugin {
    /// Data directory for this plugin: `~/.echo-agent/plugin-data/<plugin_id>/`.
    pub fn data_dir(&self) -> PathBuf {
        echo_agent_config::echo_agent_home()
            .join("plugin-data")
            .join(&self.id.0)
    }

    /// Plugin root path as a string (for env var substitution).
    pub fn root_str(&self) -> String {
        self.root.to_string_lossy().to_string()
    }

    /// Plugin data dir path as a string (for env var substitution).
    pub fn data_dir_str(&self) -> String {
        self.data_dir().to_string_lossy().to_string()
    }
}

/// In-memory registry of active plugins.
///
/// Keyed by `plugin_name` (the user-facing namespace).
/// Handles enable/disable filtering and MCP server ownership lookups.
#[derive(Debug, Clone)]
pub struct PluginRegistry {
    /// Active plugins, keyed by plugin_name.
    plugins: HashMap<String, LoadedPlugin>,
    /// Map from MCP server name to the plugin that owns it.
    mcp_owners: HashMap<String, String>,
    /// Per-session dirs from `_meta.pluginDirs` that this registry was built
    /// with, carried so per-session rebuilds can re-merge them.
    session_plugin_dirs: Vec<std::path::PathBuf>,
}

impl PluginRegistry {
    /// Build a new registry from discovered plugins.
    ///
    /// Applies the `disabled` and `enabled` lists from config.
    /// Project-scope plugins are disabled by default unless explicitly in the `enabled` list.
    /// Disabled plugins are still in the registry but their components are
    /// not loaded into the session.
    pub fn from_discovered(
        discovered: Vec<DiscoveredPlugin>,
        disabled: &[String],
        enabled: &[String],
    ) -> Self {
        let mut plugins = HashMap::new();
        let mut mcp_owners = HashMap::new();

        for dp in discovered {
            let name = dp.manifest.name.clone();

            // Verify the plugin is in exactly one of enabled/disabled.
            let in_enabled = enabled.iter().any(|e| e == &name || e == &dp.id.0);
            let in_disabled = disabled.iter().any(|d| d == &name || d == &dp.id.0);
            if in_enabled && in_disabled {
                tracing::warn!(
                    plugin = %name,
                    id = %dp.id.0,
                    "plugin appears in both enabled and disabled lists; disabled takes precedence",
                );
            } else if !in_enabled && !in_disabled {
                tracing::warn!(
                    plugin = %name,
                    id = %dp.id.0,
                    "plugin missing from both enabled and disabled lists; defaulting to disabled",
                );
            }

            // Count components
            let skill_count =
                count_skill_subdirs(&dp.skill_dirs) + count_md_files(&dp.command_dirs);
            let agent_count = count_md_files(&dp.agent_dirs);
            let skill_names = {
                let mut names = collect_skill_names(&dp.skill_dirs);
                names.extend(collect_md_names(&dp.command_dirs));
                names
            };
            let agent_names = collect_md_names(&dp.agent_dirs);
            let has_hooks = dp.hooks_path.is_some() || dp.manifest.inline_hooks().is_some();
            let has_inline_hooks_only =
                dp.hooks_path.is_none() && dp.manifest.inline_hooks().is_some();
            let mcp_server_names = plugin_mcp_server_names(&dp);
            let mcp_server_count = mcp_server_names.len();
            let hook_count = count_hook_specs(dp.hooks_path.as_deref(), dp.manifest.inline_hooks());
            let has_inline_mcp_only =
                dp.mcp_config_path.is_none() && dp.manifest.inline_mcp_servers().is_some();
            let lsp_server_count = count_lsp_servers(&dp);
            let has_inline_lsp_only =
                dp.lsp_config_path.is_none() && dp.manifest.inline_lsp_servers().is_some();

            // Capture inline data before consuming the manifest
            let inline_hooks = dp.manifest.inline_hooks().cloned();
            let inline_mcp_servers = dp.manifest.inline_mcp_servers().cloned();
            let inline_lsp_servers = dp.manifest.inline_lsp_servers().cloned();

            // Determine enabled status.
            // Every plugin should be in either the `enabled` or `disabled` list
            // (callers use `DiscoveryConfig::populate_plugin_lists` to ensure this).
            // A plugin is enabled only if it is in the `enabled` list and NOT in
            // the `disabled` list (disabled takes precedence on conflict).
            let explicitly_enabled = enabled
                .iter()
                .any(|e| e == &dp.id.0 || e == &dp.manifest.name);
            let enabled = !is_disabled(&dp, disabled) && explicitly_enabled;

            let loaded = LoadedPlugin {
                name: name.clone(),
                id: dp.id,
                root: dp.root,
                canonical_root: dp.canonical_root,
                scope: dp.scope,
                origin: dp.origin,
                trusted: dp.trusted,
                enabled,
                version: dp.manifest.version,
                description: dp.manifest.description,
                skill_names,
                agent_names,
                skill_dirs: dp.skill_dirs,
                command_dirs: dp.command_dirs,
                agent_dirs: dp.agent_dirs,
                hooks_path: dp.hooks_path,
                mcp_config_path: dp.mcp_config_path.clone(),
                lsp_config_path: dp.lsp_config_path.clone(),
                skill_count,
                agent_count,
                has_hooks,
                hook_count,
                has_inline_hooks_only,
                mcp_server_count,
                has_inline_mcp_only,
                lsp_server_count,
                has_inline_lsp_only,
                inline_hooks,
                inline_mcp_servers,
                inline_lsp_servers,
                conflict: dp.conflict,
            };

            // Track MCP server ownership for enabled + trusted plugins
            if loaded.enabled && loaded.trusted {
                for server_name in &mcp_server_names {
                    mcp_owners
                        .entry(server_name.clone())
                        .or_insert_with(|| name.clone());
                }
            }

            plugins.insert(name, loaded);
        }

        Self {
            plugins,
            mcp_owners,
            session_plugin_dirs: Vec::new(),
        }
    }

    /// Create an empty registry.
    pub fn empty() -> Self {
        Self {
            plugins: HashMap::new(),
            mcp_owners: HashMap::new(),
            session_plugin_dirs: Vec::new(),
        }
    }

    /// Record the per-session dirs this registry was built with (see field docs).
    pub fn with_session_plugin_dirs(mut self, dirs: Vec<std::path::PathBuf>) -> Self {
        self.session_plugin_dirs = dirs;
        self
    }

    /// Per-session dirs this registry was built with; empty for shared snapshots.
    pub fn session_plugin_dirs(&self) -> &[std::path::PathBuf] {
        &self.session_plugin_dirs
    }

    /// Get a plugin by name.
    pub fn get(&self, name: &str) -> Option<&LoadedPlugin> {
        self.plugins.get(name)
    }

    /// List all plugins (including disabled ones).
    pub fn list(&self) -> Vec<&LoadedPlugin> {
        let mut plugins: Vec<&LoadedPlugin> = self.plugins.values().collect();
        plugins.sort_by(|a, b| {
            // Sort by scope (priority order), then by name
            (a.scope as u8, &a.name).cmp(&(b.scope as u8, &b.name))
        });
        plugins
    }

    /// List only enabled + trusted plugins (active for the session).
    pub fn active_plugins(&self) -> Vec<&LoadedPlugin> {
        self.list()
            .into_iter()
            .filter(|p| p.enabled && p.trusted)
            .collect()
    }

    /// List enabled plugins (both trusted and untrusted).
    /// Useful for skill/agent discovery where trust only gates executables.
    pub fn enabled_plugins(&self) -> Vec<&LoadedPlugin> {
        self.list().into_iter().filter(|p| p.enabled).collect()
    }

    /// Look up which plugin owns an MCP server by server name.
    pub fn mcp_server_owner(&self, server_name: &str) -> Option<&str> {
        self.mcp_owners.get(server_name).map(|s| s.as_str())
    }

    /// Number of plugins in the registry.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

// ── Shared handle for cross-thread reload ─────────────────────────────

/// Thread-safe handle for plugin registry lifecycle.
///
/// Stores the process-lifetime CLI plugin dirs (which survive reload)
/// and provides methods to build per-session registries and rebuild
/// the shared "latest" registry for new sessions.
#[derive(Debug, Clone)]
pub struct SharedPluginRegistryHandle {
    /// The latest registry, rebuilt on `/plugins reload`.
    /// New sessions clone from here. Running sessions keep their snapshot.
    inner: std::sync::Arc<std::sync::RwLock<Option<std::sync::Arc<PluginRegistry>>>>,
    /// CLI `--plugin-dir` paths from process startup. Preserved across reloads.
    cli_plugin_dirs: std::sync::Arc<Vec<std::path::PathBuf>>,
}

impl SharedPluginRegistryHandle {
    /// Create a new handle with an initial registry and CLI plugin dirs.
    pub fn new(registry: Option<PluginRegistry>, cli_plugin_dirs: Vec<std::path::PathBuf>) -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::RwLock::new(registry.map(std::sync::Arc::new))),
            cli_plugin_dirs: std::sync::Arc::new(cli_plugin_dirs),
        }
    }

    /// Get the current registry snapshot (cheap Arc clone).
    pub fn snapshot(&self) -> Option<std::sync::Arc<PluginRegistry>> {
        self.inner.read().unwrap().clone()
    }

    /// Build a fresh registry for a specific session cwd.
    ///
    /// Pure registry construction (no disk mutation): also used by the read-only
    /// `commands/list` pull and the reload fan-out, so it must not refresh local
    /// installs. Use [`Self::refresh_and_build_for_cwd`] at genuine session spawn.
    /// CLI `--plugin-dir` paths from process startup are always included.
    ///
    /// `session_plugin_dirs` are per-session dirs from `session/new` / `session/load`
    /// `_meta.pluginDirs` — same CliOverride scope and trust as `--plugin-dir`, but
    /// only for the session whose registry this builds.
    ///
    /// `project_trusted` is the folder-trust verdict for `cwd`, threaded into
    /// discovery to gate Project-scope plugins.
    pub fn build_for_cwd(
        &self,
        cwd: &std::path::Path,
        disk_config: &super::discovery::DiscoveryConfig,
        session_plugin_dirs: &[std::path::PathBuf],
        project_trusted: bool,
    ) -> Option<std::sync::Arc<PluginRegistry>> {
        let mut config = disk_config.clone();
        // Merge startup CLI dirs back in (they're not in config.toml)
        config
            .cli_plugin_dirs
            .extend(self.cli_plugin_dirs.iter().cloned());
        config
            .cli_plugin_dirs
            .extend(session_plugin_dirs.iter().cloned());
        let trust_store = super::trust::TrustStore::load();
        let discovered =
            super::discovery::discover_plugins(Some(cwd), &config, &trust_store, project_trusted);
        if discovered.is_empty() {
            // Keep an empty registry alive when session dirs exist, so per-session
            // rebuilds can still recover them (see `session_plugin_dirs` field docs).
            (!session_plugin_dirs.is_empty()).then(|| {
                std::sync::Arc::new(
                    PluginRegistry::empty().with_session_plugin_dirs(session_plugin_dirs.to_vec()),
                )
            })
        } else {
            config.populate_plugin_lists(&discovered);
            Some(std::sync::Arc::new(
                PluginRegistry::from_discovered(discovered, &config.disabled, &config.enabled)
                    .with_session_plugin_dirs(session_plugin_dirs.to_vec()),
            ))
        }
    }

    /// Re-copy trusted / user-home local installs, then [`Self::build_for_cwd`].
    ///
    /// The refresh is the session-spawn boundary for picking up agents/skills
    /// added to a live local source after install (the snapshot is a copy, not a
    /// symlink). Wired only to genuine session spawn — not the read-only
    /// `commands/list` pull or the reload fan-out, which use the pure builder.
    pub fn refresh_and_build_for_cwd(
        &self,
        cwd: &std::path::Path,
        disk_config: &super::discovery::DiscoveryConfig,
        session_plugin_dirs: &[std::path::PathBuf],
        project_trusted: bool,
    ) -> Option<std::sync::Arc<PluginRegistry>> {
        let trust_store = super::trust::TrustStore::load();
        // Session spawn: cheap skip-unchanged (force=false).
        Self::run_local_refresh(&trust_store, false);
        self.build_for_cwd(cwd, disk_config, session_plugin_dirs, project_trusted)
    }

    /// Re-copy trusted local installs from disk and log the outcome. `force`
    /// bypasses the skip-unchanged guard: only the explicit `/plugins reload`
    /// passes `true`; session spawn and incidental rebuilds pass `false` (cheap
    /// structural skip).
    fn run_local_refresh(trust_store: &super::trust::TrustStore, force: bool) {
        let refresh = super::local_refresh::refresh_local_installs_from_disk(trust_store, force);
        tracing::debug!(
            refreshed = refresh.refreshed,
            skipped = refresh.skipped,
            errors = refresh.errors,
            force,
            "plugin registry local plugin refresh"
        );
    }

    /// Rebuild the shared registry from disk and replace the shared state.
    ///
    /// `cwd` should be the session's working directory.
    /// `disk_config` should be freshly loaded from config.toml.
    /// CLI `--plugin-dir` paths from startup are automatically merged.
    ///
    /// Returns the count of plugins discovered.
    ///
    /// `project_trusted` is the folder-trust verdict for `cwd`, threaded into
    /// discovery to gate Project-scope plugins.
    ///
    /// `force` controls the local-install refresh: only the explicit, user-initiated
    /// `/plugins reload` passes `true` (guaranteed full re-copy — the manual remedy);
    /// incidental rebuilds (boot, plugin enable/disable/add/remove) pass `false` for
    /// the cheap structural skip-unchanged path.
    pub fn reload(
        &self,
        cwd: Option<&std::path::Path>,
        disk_config: &super::discovery::DiscoveryConfig,
        project_trusted: bool,
        force: bool,
    ) -> usize {
        let mut config = disk_config.clone();
        config
            .cli_plugin_dirs
            .extend(self.cli_plugin_dirs.iter().cloned());
        let trust_store = super::trust::TrustStore::load();
        Self::run_local_refresh(&trust_store, force);
        let discovered =
            super::discovery::discover_plugins(cwd, &config, &trust_store, project_trusted);
        let count = discovered.len();
        config.populate_plugin_lists(&discovered);
        let registry =
            PluginRegistry::from_discovered(discovered, &config.disabled, &config.enabled);
        *self.inner.write().unwrap() = if registry.is_empty() {
            None
        } else {
            Some(std::sync::Arc::new(registry))
        };
        count
    }
}

// ── Component counting helpers ────────────────────────────────────────

/// Collect the SKILL.md paths that load from the given skill dirs.
///
/// Discovery via `find_skill_md_paths`; deduped by the loader's per-plugin
/// identity — the normalized parent-dir basename (`stamp_plugin_fields` names
/// plugin skills by directory basename, and `merge_skills_with_plugins`
/// dedupes on `plugin:<name>`) — so counts equal what actually loads, both
/// for overlapping dir entries and same-basename dirs at different paths.
pub fn skill_md_paths(skill_dirs: &[PathBuf]) -> Vec<PathBuf> {
    use echo_agent_tools::implementations::skills::discovery::{
        find_skill_md_paths, normalize_skill_name,
    };

    let mut paths: Vec<PathBuf> = skill_dirs
        .iter()
        .flat_map(|d| find_skill_md_paths(d))
        .collect();
    let mut seen = std::collections::HashSet::new();
    paths.retain(|p| {
        p.parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            .map(|name| seen.insert(normalize_skill_name(name)))
            .unwrap_or(true)
    });
    paths
}

/// Count skills under the skill dirs (each discovered SKILL.md = 1 skill).
fn count_skill_subdirs(skill_dirs: &[PathBuf]) -> usize {
    skill_md_paths(skill_dirs).len()
}

fn count_md_files(dirs: &[PathBuf]) -> usize {
    dirs.iter()
        .flat_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                && e.path().extension().is_some_and(|ext| ext == "md")
        })
        .count()
}

fn collect_md_names(dirs: &[PathBuf]) -> Vec<String> {
    dirs.iter()
        .flat_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                && e.path().extension().is_some_and(|ext| ext == "md")
        })
        .filter_map(|e| {
            e.path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(String::from)
        })
        .collect()
}

fn collect_skill_names(skill_dirs: &[PathBuf]) -> Vec<String> {
    // Skill name = basename of the directory containing SKILL.md.
    skill_md_paths(skill_dirs)
        .iter()
        .filter_map(|p| p.parent())
        .filter_map(|dir| dir.file_name().and_then(|n| n.to_str()).map(String::from))
        .collect()
}

/// Deduped union of MCP server names from a plugin's `.mcp.json` file and its
/// inline `mcpServers` manifest block (file names first, inline names appended).
fn plugin_mcp_server_names(dp: &DiscoveredPlugin) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    if let Some(ref path) = dp.mcp_config_path
        && let Ok(file_names) = read_mcp_server_names(path)
    {
        for server_name in file_names {
            if seen.insert(server_name.clone()) {
                names.push(server_name);
            }
        }
    }

    if let Some(inline) = dp.manifest.inline_mcp_servers() {
        let normalized = super::manifest::normalize_inline_mcp_servers(inline);
        if let Some(servers) = normalized.get("mcpServers").and_then(|v| v.as_object()) {
            for server_name in servers.keys() {
                if seen.insert(server_name.clone()) {
                    names.push(server_name.clone());
                }
            }
        }
    }

    names
}

fn count_lsp_servers(dp: &DiscoveredPlugin) -> usize {
    if let Some(ref path) = dp.lsp_config_path {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
            .and_then(|v| v.as_object().map(|o| o.len()))
            .unwrap_or(0)
    } else if let Some(inline) = dp.manifest.inline_lsp_servers() {
        inline.as_object().map(|o| o.len()).unwrap_or(0)
    } else {
        0
    }
}

/// Count hook specs defined in a plugin's hooks.json and/or inline hooks.
///
/// The hooks JSON structure is `{ "hooks": { "EventName": [ { "hooks": [...] } ] } }`.
/// Each entry in the inner `hooks` array is one hook handler spec.
fn count_hook_specs(hooks_path: Option<&Path>, inline_hooks: Option<&serde_json::Value>) -> usize {
    fn count_in_value(v: &serde_json::Value) -> usize {
        let Some(events) = v.get("hooks").and_then(|h| h.as_object()) else {
            return 0;
        };
        events
            .values()
            .filter_map(|matchers| matchers.as_array())
            .flat_map(|arr| arr.iter())
            .filter_map(|group| group.get("hooks").and_then(|h| h.as_array()))
            .map(|hooks| hooks.len())
            .sum()
    }

    let file_count = hooks_path
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .map(|v| count_in_value(&v))
        .unwrap_or(0);
    let inline_count = inline_hooks.map(count_in_value).unwrap_or(0);
    file_count + inline_count
}

/// Read MCP server names from a .mcp.json file.
fn read_mcp_server_names(path: &Path) -> Result<Vec<String>, ()> {
    let content = std::fs::read_to_string(path).map_err(|_| ())?;
    let value: serde_json::Value = serde_json::from_str(&content).map_err(|_| ())?;
    let names = value
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(names)
}

/// Check if a discovered plugin is in the disabled list.
///
/// Matches by:
/// 1. Full `plugin_id` (e.g. `"user/a1b2c3d4/my-plugin"`)
/// 2. `plugin_name` shorthand (e.g. `"my-plugin"`) — only for convenience
fn is_disabled(dp: &DiscoveredPlugin, disabled: &[String]) -> bool {
    disabled
        .iter()
        .any(|d| d == &dp.id.0 || d == &dp.manifest.name)
}

#[cfg(test)]
mod tests {
    use super::super::discovery::PluginId;
    use super::super::manifest::PluginManifest;
    use super::*;

    fn make_discovered(name: &str, scope: PluginScope, trusted: bool) -> DiscoveredPlugin {
        let root = PathBuf::from(format!("/tmp/test-plugins/{name}"));
        DiscoveredPlugin {
            manifest: PluginManifest {
                name: name.to_string(),
                version: Some("1.0.0".to_string()),
                description: Some(format!("Test plugin {name}")),
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
            },
            id: PluginId::new(scope, &root, name),
            root: root.clone(),
            canonical_root: root,
            scope,
            origin: match scope {
                PluginScope::CliOverride => PluginOrigin::CliOverride,
                PluginScope::Project => PluginOrigin::ProjectEchoAgent,
                PluginScope::User => PluginOrigin::UserEchoAgent,
                PluginScope::ConfigPath => PluginOrigin::ConfigPath,
            },
            trusted,
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: None,
            mcp_config_path: None,
            lsp_config_path: None,
            conflict: None,
        }
    }

    #[test]
    fn skill_counts_include_root_level_skill_md() {
        // Manifest `skills` entries pointing directly at skill dirs
        // (SKILL.md at the dir root) must be counted and named.
        let tmp = tempfile::tempdir().unwrap();
        let one = tmp.path().join("one");
        let two = tmp.path().join("two");
        for (dir, name) in [(&one, "one"), (&two, "two")] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d\n---\nbody\n"),
            )
            .unwrap();
        }

        let dirs = vec![one, two];
        assert_eq!(count_skill_subdirs(&dirs), 2);
        let mut names = collect_skill_names(&dirs);
        names.sort();
        assert_eq!(names, vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn skill_counts_dedupe_same_basename_different_paths() {
        // Two skill dirs with the same basename at different paths collide on
        // the loader's per-plugin name identity — only one loads, so only one
        // may be counted.
        let tmp = tempfile::tempdir().unwrap();
        for group in ["a", "b"] {
            let d = tmp.path().join(group).join("dup-skill");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("SKILL.md"), "---\nname: dup\n---\n").unwrap();
        }

        let dirs = vec![tmp.path().join("a"), tmp.path().join("b")];
        assert_eq!(count_skill_subdirs(&dirs), 1);
        assert_eq!(collect_skill_names(&dirs), vec!["dup-skill".to_string()]);
    }

    #[test]
    fn skill_counts_dedupe_overlapping_dirs() {
        // Manifest lists both the parent skills dir and a child skill dir:
        // the child's SKILL.md is reachable via both entries but must count once.
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("skills");
        let child = parent.join("one");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(child.join("SKILL.md"), "---\nname: one\n---\n").unwrap();

        let dirs = vec![parent, child];
        assert_eq!(count_skill_subdirs(&dirs), 1);
        assert_eq!(collect_skill_names(&dirs), vec!["one".to_string()]);
    }

    #[test]
    fn skill_counts_convention_parent_dir() {
        // Convention layout: one parent dir with skill subdirectories.
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("skills");
        for name in ["alpha", "beta"] {
            let d = parent.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        }

        let dirs = vec![parent];
        assert_eq!(count_skill_subdirs(&dirs), 2);
        let mut names = collect_skill_names(&dirs);
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn empty_registry() {
        let reg = PluginRegistry::empty();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.list().is_empty());
    }

    #[test]
    fn from_discovered_basic() {
        let plugins = vec![
            make_discovered("alpha", PluginScope::User, true),
            make_discovered("beta", PluginScope::CliOverride, true),
        ];

        let reg = PluginRegistry::from_discovered(plugins, &[], &[]);
        assert_eq!(reg.len(), 2);
        assert!(reg.get("alpha").is_some());
        assert!(reg.get("beta").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn origin_threaded_to_loaded_plugin() {
        let mut dp = make_discovered("mp-tool", PluginScope::User, true);
        dp.origin = PluginOrigin::ClaudeMarketplace {
            marketplace: "demo-mp".to_string(),
        };

        let reg = PluginRegistry::from_discovered(vec![dp], &[], &[]);
        assert_eq!(
            reg.get("mp-tool").unwrap().origin,
            PluginOrigin::ClaudeMarketplace {
                marketplace: "demo-mp".to_string(),
            }
        );
    }

    #[test]
    fn disabled_plugins_filtered_from_active() {
        let plugins = vec![
            make_discovered("enabled-plugin", PluginScope::User, true),
            make_discovered("disabled-plugin", PluginScope::User, true),
        ];

        let reg = PluginRegistry::from_discovered(
            plugins,
            &["disabled-plugin".to_string()],
            &["enabled-plugin".to_string()],
        );

        assert_eq!(reg.len(), 2); // Both in registry
        let active = reg.active_plugins();
        assert_eq!(active.len(), 1); // Only enabled one is active
        assert_eq!(active[0].name, "enabled-plugin");

        // Disabled one is in list but marked disabled
        let disabled = reg.get("disabled-plugin").unwrap();
        assert!(!disabled.enabled);
    }

    #[test]
    fn disabled_by_plugin_id() {
        let dp = make_discovered("my-tool", PluginScope::User, true);
        let plugin_id = dp.id.0.clone();

        let reg = PluginRegistry::from_discovered(vec![dp], &[plugin_id], &[]);
        assert!(!reg.get("my-tool").unwrap().enabled);
    }

    #[test]
    fn plugins_disabled_by_default() {
        // Both User and Project plugins default to disabled when not in enabled list.
        let plugins = vec![
            make_discovered("user-one", PluginScope::User, true),
            make_discovered("project-one", PluginScope::Project, false),
        ];

        let reg = PluginRegistry::from_discovered(plugins, &[], &[]);
        let active = reg.active_plugins();
        assert_eq!(active.len(), 0);

        assert!(!reg.get("user-one").unwrap().enabled);
        assert!(!reg.get("project-one").unwrap().enabled);
    }

    #[test]
    fn cli_override_auto_enabled_via_populate() {
        // CliOverride plugins are added to the enabled list by populate_plugin_lists.
        use super::super::discovery::DiscoveryConfig;
        let plugins = vec![
            make_discovered("cli-plugin", PluginScope::CliOverride, true),
            make_discovered("user-plugin", PluginScope::User, true),
        ];
        let mut config = DiscoveryConfig::default();
        config.populate_plugin_lists(&plugins);

        assert!(config.enabled.contains(&"cli-plugin".to_string()));
        assert!(config.disabled.contains(&"user-plugin".to_string()));

        let reg = PluginRegistry::from_discovered(plugins, &config.disabled, &config.enabled);
        assert!(reg.get("cli-plugin").unwrap().enabled);
        assert!(!reg.get("user-plugin").unwrap().enabled);
    }

    #[test]
    fn config_path_auto_enabled_via_populate() {
        // ConfigPath plugins are added to the enabled list by populate_plugin_lists.
        use super::super::discovery::DiscoveryConfig;
        let plugins = vec![
            make_discovered("config-plugin", PluginScope::ConfigPath, true),
            make_discovered("user-plugin", PluginScope::User, true),
        ];
        let mut config = DiscoveryConfig::default();
        config.populate_plugin_lists(&plugins);

        assert!(config.enabled.contains(&"config-plugin".to_string()));
        assert!(config.disabled.contains(&"user-plugin".to_string()));

        let reg = PluginRegistry::from_discovered(plugins, &config.disabled, &config.enabled);
        assert!(reg.get("config-plugin").unwrap().enabled);
        assert!(!reg.get("user-plugin").unwrap().enabled);
    }

    #[test]
    fn list_sorted_by_scope_then_name() {
        let plugins = vec![
            make_discovered("zebra", PluginScope::User, true),
            make_discovered("alpha", PluginScope::CliOverride, true),
            make_discovered("beta", PluginScope::Project, false),
        ];

        let reg = PluginRegistry::from_discovered(plugins, &[], &[]);
        let list = reg.list();
        assert_eq!(list[0].name, "alpha"); // CliOverride = 0
        assert_eq!(list[1].name, "beta"); // Project = 1
        assert_eq!(list[2].name, "zebra"); // User = 2
    }

    #[test]
    fn data_dir_uses_plugin_id() {
        let dp = make_discovered("my-plugin", PluginScope::User, true);
        let reg = PluginRegistry::from_discovered(vec![dp], &[], &[]);
        let plugin = reg.get("my-plugin").unwrap();
        let data_dir = plugin.data_dir();
        // Should be under ~/.echo-agent/plugin-data/<plugin_id>/
        let data_dir_str = data_dir.to_string_lossy();
        assert!(data_dir_str.contains("plugin-data"));
        assert!(data_dir_str.contains("user/"));
        assert!(data_dir_str.contains("/my-plugin"));
    }

    #[test]
    fn inline_hooks_populated_from_manifest() {
        use super::super::manifest::PathOrInline;

        let mut dp = make_discovered("hook-plugin", PluginScope::User, true);
        let inline_json = serde_json::json!({
            "hooks": {
                "PostToolUse": [{"hooks": [{"type": "command", "command": "lint"}]}]
            }
        });
        dp.manifest.hooks = Some(PathOrInline::Inline(inline_json.clone()));

        let reg = PluginRegistry::from_discovered(vec![dp], &[], &[]);
        let plugin = reg.get("hook-plugin").unwrap();
        assert!(plugin.has_hooks);
        assert!(plugin.has_inline_hooks_only);
        assert!(plugin.hooks_path.is_none());
        assert_eq!(plugin.inline_hooks.as_ref().unwrap(), &inline_json);
    }

    #[test]
    fn inline_mcp_populated_from_manifest() {
        use super::super::manifest::PathOrInline;

        let mut dp = make_discovered("mcp-plugin", PluginScope::User, true);
        let inline_json = serde_json::json!({
            "mcpServers": {
                "my-server": {"command": "./server", "args": []}
            }
        });
        dp.manifest.mcp_servers = Some(PathOrInline::Inline(inline_json.clone()));

        let reg = PluginRegistry::from_discovered(vec![dp], &[], &["mcp-plugin".to_string()]);
        let plugin = reg.get("mcp-plugin").unwrap();
        assert!(plugin.has_inline_mcp_only);
        assert!(plugin.mcp_config_path.is_none());
        assert_eq!(plugin.inline_mcp_servers.as_ref().unwrap(), &inline_json);
        // Should track ownership from inline (plugin is enabled)
        assert_eq!(reg.mcp_server_owner("my-server"), Some("mcp-plugin"));
    }

    // ── Combined disabled + untrusted scenarios ─────────────────

    #[test]
    fn disabled_project_plugin_excluded_from_active_and_enabled() {
        let plugins = vec![
            make_discovered("good-plugin", PluginScope::User, true),
            make_discovered("bad-plugin", PluginScope::Project, false),
        ];
        let reg = PluginRegistry::from_discovered(
            plugins,
            &["bad-plugin".to_string()],
            &["good-plugin".to_string()],
        );

        assert_eq!(reg.len(), 2);

        let active = reg.active_plugins();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "good-plugin");

        let enabled = reg.enabled_plugins();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "good-plugin");

        let bad = reg.get("bad-plugin").unwrap();
        assert!(!bad.enabled);
        // trusted is now propagated from discovery (was false for Project scope)
        assert!(!bad.trusted);
    }

    #[test]
    fn unlisted_plugins_disabled_by_default() {
        // Plugins not in either enabled or disabled list default to disabled.
        let plugins = vec![
            make_discovered("listed", PluginScope::User, true),
            make_discovered("unlisted", PluginScope::User, true),
        ];
        let reg = PluginRegistry::from_discovered(plugins, &[], &["listed".to_string()]);

        let enabled = reg.enabled_plugins();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "listed");

        assert!(!reg.get("unlisted").unwrap().enabled);
    }

    #[test]
    fn enabled_plugins_excludes_disabled() {
        // enabled_plugins() must NOT include disabled plugins, even if trusted.
        let plugins = vec![
            make_discovered("enabled-trusted", PluginScope::User, true),
            make_discovered("disabled-trusted", PluginScope::User, true),
        ];
        let reg = PluginRegistry::from_discovered(
            plugins,
            &["disabled-trusted".to_string()],
            &["enabled-trusted".to_string()],
        );

        let enabled = reg.enabled_plugins();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "enabled-trusted");
    }

    #[test]
    fn is_disabled_matches_by_name() {
        let dp = make_discovered("my-tool", PluginScope::User, true);
        assert!(is_disabled(&dp, &["my-tool".to_string()]));
    }

    #[test]
    fn is_disabled_matches_by_id() {
        let dp = make_discovered("my-tool", PluginScope::User, true);
        let id = dp.id.0.clone();
        assert!(is_disabled(&dp, &[id]));
    }

    #[test]
    fn is_disabled_no_match() {
        let dp = make_discovered("my-tool", PluginScope::User, true);
        assert!(!is_disabled(&dp, &["other-tool".to_string()]));
    }

    #[test]
    fn is_disabled_empty_list() {
        let dp = make_discovered("my-tool", PluginScope::User, true);
        assert!(!is_disabled(&dp, &[]));
    }

    #[test]
    fn multiple_disabled_plugins() {
        let plugins = vec![
            make_discovered("alpha", PluginScope::User, true),
            make_discovered("beta", PluginScope::User, true),
            make_discovered("gamma", PluginScope::User, true),
        ];
        let reg = PluginRegistry::from_discovered(
            plugins,
            &["alpha".to_string(), "gamma".to_string()],
            &["beta".to_string()],
        );

        assert_eq!(reg.len(), 3);
        assert!(!reg.get("alpha").unwrap().enabled);
        assert!(reg.get("beta").unwrap().enabled);
        assert!(!reg.get("gamma").unwrap().enabled);

        let active = reg.active_plugins();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "beta");
    }

    #[test]
    fn mcp_deduped_when_declared_in_file_and_inline() {
        use super::super::manifest::PathOrInline;

        let tmp = tempfile::tempdir().unwrap();
        let mcp_json = tmp.path().join(".mcp.json");
        std::fs::write(
            &mcp_json,
            r#"{"mcpServers":{"sentry":{"type":"http","url":"https://mcp.sentry.dev/mcp"}}}"#,
        )
        .unwrap();

        let mut dp = make_discovered("sentry", PluginScope::User, true);
        dp.mcp_config_path = Some(mcp_json);
        dp.manifest.mcp_servers = Some(PathOrInline::Inline(serde_json::json!({
            "sentry": { "type": "http", "url": "https://mcp.sentry.dev/mcp" }
        })));

        let reg = PluginRegistry::from_discovered(vec![dp], &[], &["sentry".to_string()]);
        let plugin = reg.get("sentry").unwrap();
        assert_eq!(
            plugin.mcp_server_count, 1,
            "same server in both .mcp.json and inline must dedupe to 1"
        );
        assert_eq!(reg.mcp_server_owner("sentry"), Some("sentry"));
    }

    #[test]
    fn mcp_count_from_inline_direct_map() {
        use super::super::manifest::PathOrInline;

        let mut dp = make_discovered("sentry", PluginScope::User, true);
        dp.manifest.mcp_servers = Some(PathOrInline::Inline(serde_json::json!({
            "sentry": { "type": "http", "url": "https://mcp.sentry.dev/mcp" }
        })));

        let reg = PluginRegistry::from_discovered(vec![dp], &[], &["sentry".to_string()]);
        let plugin = reg.get("sentry").unwrap();
        assert_eq!(plugin.mcp_server_count, 1);
        assert_eq!(reg.mcp_server_owner("sentry"), Some("sentry"));
    }

    #[test]
    fn mcp_ownership_not_tracked_for_disabled() {
        use super::super::manifest::PathOrInline;

        let mut dp = make_discovered("disabled-mcp", PluginScope::User, true);
        dp.manifest.mcp_servers = Some(PathOrInline::Inline(serde_json::json!({
            "mcpServers": {
                "my-server": {"command": "./server"}
            }
        })));

        let reg = PluginRegistry::from_discovered(vec![dp], &["disabled-mcp".to_string()], &[]);
        // Disabled plugins should NOT have MCP ownership tracked
        assert_eq!(reg.mcp_server_owner("my-server"), None);
    }

    #[test]
    fn inline_mcp_ownership_not_tracked_for_untrusted() {
        use super::super::manifest::PathOrInline;

        let mut dp = make_discovered("untrusted-mcp", PluginScope::Project, false);
        dp.manifest.mcp_servers = Some(PathOrInline::Inline(serde_json::json!({
            "mcpServers": {
                "blocked-server": {"command": "./server"}
            }
        })));

        let reg = PluginRegistry::from_discovered(vec![dp], &[], &[]);
        // Untrusted plugins should NOT have MCP ownership tracked
        assert_eq!(reg.mcp_server_owner("blocked-server"), None);
    }

    #[test]
    fn populate_plugin_lists_exhaustive() {
        use super::super::discovery::DiscoveryConfig;
        let plugins = vec![
            make_discovered("cli", PluginScope::CliOverride, true),
            make_discovered("user", PluginScope::User, true),
            make_discovered("project", PluginScope::Project, false),
            make_discovered("config", PluginScope::ConfigPath, true),
        ];
        let mut config = DiscoveryConfig::default();
        config.populate_plugin_lists(&plugins);

        // CliOverride and ConfigPath → enabled; User and Project → disabled
        assert!(config.enabled.contains(&"cli".to_string()));
        assert!(config.enabled.contains(&"config".to_string()));
        assert!(config.disabled.contains(&"user".to_string()));
        assert!(config.disabled.contains(&"project".to_string()));
        // Every plugin accounted for
        assert_eq!(config.enabled.len() + config.disabled.len(), 4);
    }

    #[test]
    fn populate_plugin_lists_preserves_existing() {
        use super::super::discovery::DiscoveryConfig;
        let plugins = vec![
            make_discovered("already-enabled", PluginScope::User, true),
            make_discovered("already-disabled", PluginScope::User, true),
            make_discovered("new-user", PluginScope::User, true),
        ];
        let mut config = DiscoveryConfig {
            enabled: vec!["already-enabled".to_string()],
            disabled: vec!["already-disabled".to_string()],
            ..Default::default()
        };
        config.populate_plugin_lists(&plugins);

        // Pre-existing entries untouched
        assert!(config.enabled.contains(&"already-enabled".to_string()));
        assert!(config.disabled.contains(&"already-disabled".to_string()));
        // New user plugin defaults to disabled
        assert!(config.disabled.contains(&"new-user".to_string()));
        assert_eq!(config.enabled.len(), 1);
        assert_eq!(config.disabled.len(), 2);
    }

    // ── Security: trust propagation from discovery ──────────────

    #[test]
    fn untrusted_project_plugin_excluded_from_active_even_when_enabled() {
        // Simulates the attack: a project plugin is enabled (e.g. via
        // pre-populated enabledPlugins) but NOT trusted. It must NOT
        // appear in active_plugins() so its hooks never fire.
        let plugins = vec![
            make_discovered("malicious", PluginScope::Project, false), // untrusted
        ];
        let reg = PluginRegistry::from_discovered(
            plugins,
            &[],
            &["malicious".to_string()], // attacker got it into enabled list
        );

        // Plugin is enabled but not trusted
        let plugin = reg.get("malicious").unwrap();
        assert!(plugin.enabled);
        assert!(!plugin.trusted);

        // Must NOT appear in active_plugins (hooks would fire)
        let active = reg.active_plugins();
        assert!(
            active.is_empty(),
            "untrusted project plugin must not be in active_plugins"
        );
    }

    #[test]
    fn trusted_field_propagated_from_discovery() {
        // Verify that from_discovered preserves the trust value from
        // discovery rather than hardcoding it to true.
        let trusted_plugin = make_discovered("user-tool", PluginScope::User, true);
        let untrusted_plugin = make_discovered("project-tool", PluginScope::Project, false);

        let reg = PluginRegistry::from_discovered(
            vec![trusted_plugin, untrusted_plugin],
            &[],
            &["user-tool".to_string(), "project-tool".to_string()],
        );

        assert!(reg.get("user-tool").unwrap().trusted);
        assert!(!reg.get("project-tool").unwrap().trusted);
    }

    #[test]
    fn pre_enabled_project_plugin_blocked_without_trust() {
        // End-to-end: populate_plugin_lists won't auto-disable a plugin
        // that's already in the enabled list, but active_plugins() must
        // still block it when untrusted.
        use super::super::discovery::DiscoveryConfig;

        let plugins = vec![make_discovered(
            "attacker-plugin",
            PluginScope::Project,
            false,
        )];
        let mut config = DiscoveryConfig {
            // Attacker pre-populated the enabled list (via .claude/settings.json)
            enabled: vec!["attacker-plugin".to_string()],
            ..Default::default()
        };
        config.populate_plugin_lists(&plugins);
        // populate_plugin_lists skips it because it's already listed
        assert!(config.enabled.contains(&"attacker-plugin".to_string()));

        let reg = PluginRegistry::from_discovered(plugins, &config.disabled, &config.enabled);
        let plugin = reg.get("attacker-plugin").unwrap();
        assert!(plugin.enabled, "plugin is in enabled list");
        assert!(!plugin.trusted, "project plugin is not trusted");
        assert!(
            reg.active_plugins().is_empty(),
            "untrusted plugin must not appear in active_plugins"
        );
    }
}
