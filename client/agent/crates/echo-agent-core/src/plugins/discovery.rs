//! Filesystem scanning for plugin directories.
//!
//! Discovers plugins from multiple sources in priority order:
//! 1. CLI `--plugin-dir` paths (scope: `CliOverride`)
//! 2. `.echo-agent/plugins/*/` (scope: `Project`, walked from cwd to worktree root)
//! 3. `.claude/plugins/*/` (scope: `Project`, compat)
//! 4. `~/.echo-agent/plugins/*/` (scope: `User`)
//! 5. `~/.claude/plugins/*/` (scope: `User`, compat)
//!    `~/.echo-agent/installed-plugins/*/` (scope: `User`, marketplace installs)
//!    Installed plugins from `~/.claude/plugins/installed_plugins.json` (scope: `User`)
//! 6. Paths from `[plugins].paths` in config (scope: `ConfigPath`)
//!
//! Deduplicates by canonical path and resolves name conflicts via
//! the canonical source precedence.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::manifest::{ManifestLoadResult, PluginManifest, load_manifest, name_from_dirname};
use super::trust::TrustStore;

// ── Public types ──────────────────────────────────────────────────────

/// Where a plugin was discovered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginScope {
    /// `--plugin-dir` (highest priority, always trusted)
    CliOverride = 0,
    /// `.echo-agent/plugins/` or `.claude/plugins/` in project (requires trust)
    Project = 1,
    /// `~/.echo-agent/plugins/` or `~/.claude/plugins/` (always trusted)
    User = 2,
    /// `[plugins].paths` in config (trust depends on location)
    ConfigPath = 3,
}

impl PluginScope {
    /// Label used in `PluginId` format.
    pub fn id_label(&self) -> &'static str {
        match self {
            Self::CliOverride => "cli",
            Self::Project => "project",
            Self::User => "user",
            Self::ConfigPath => "config",
        }
    }
}

impl std::fmt::Display for PluginScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CliOverride => write!(f, "cli"),
            Self::Project => write!(f, "project"),
            Self::User => write!(f, "user"),
            Self::ConfigPath => write!(f, "config"),
        }
    }
}

/// The concrete discovery source a plugin came from.
///
/// Finer-grained than [`PluginScope`]: recorded at scan time so consumers
/// (e.g. the pager's plugins list) don't have to re-derive provenance from
/// paths. Not part of [`PluginId`], which stays scope-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginOrigin {
    /// CLI `--plugin-dir`.
    CliOverride,
    /// Project `.echo-agent/plugins/`.
    ProjectEchoAgent,
    /// Project `.claude/plugins/`.
    ProjectClaude,
    /// `$ECHO_AGENT_HOME/plugins/`.
    UserEchoAgent,
    /// `~/.claude/plugins/`.
    UserClaude,
    /// A compat marketplace clone (project `extraKnownMarketplaces`
    /// or user `known_marketplaces.json`).
    ClaudeMarketplace {
        /// Marketplace name from the settings/registry entry.
        marketplace: String,
    },
    /// Install recorded in `~/.claude/plugins/installed_plugins.json`.
    ClaudeInstalled {
        /// Marketplace name from the `name@marketplace` JSON key, when present.
        marketplace: Option<String>,
    },
    /// EchoAgent's install registry (`~/.echo-agent/installed-plugins`).
    MarketplaceInstall {
        /// Marketplace source display name (None for direct git/local installs).
        source_name: Option<String>,
        /// Git URL of the installed repo (None for local installs).
        git_url: Option<String>,
    },
    /// `[plugins].paths` in config.
    ConfigPath,
}

/// Stable internal identity for a plugin.
///
/// Format: `<scope>/<hex8>/<name>`
/// - `<scope>`: lowercase scope string (cli, project, user, config)
/// - `<hex8>`: first 8 hex chars of SHA-256 of the canonical plugin root path
/// - `<name>`: the plugin_name
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginId(pub String);

impl PluginId {
    /// Construct a plugin ID from its components.
    pub fn new(scope: PluginScope, canonical_root: &Path, name: &str) -> Self {
        let path_str = canonical_root.to_string_lossy();
        let mut hasher = Sha256::new();
        hasher.update(path_str.as_bytes());
        let hash = hasher.finalize();
        let hex8 = format!(
            "{:02x}{:02x}{:02x}{:02x}",
            hash[0], hash[1], hash[2], hash[3]
        );
        Self(format!("{}/{}/{}", scope.id_label(), hex8, name))
    }
}

impl std::fmt::Display for PluginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A plugin candidate discovered on the filesystem.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    /// Parsed manifest (or synthetic for convention-based plugins).
    pub manifest: PluginManifest,
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
    /// Whether the plugin is trusted for executable operations.
    pub trusted: bool,
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
    /// Warning message when this plugin won a name collision.
    pub conflict: Option<String>,
}

impl DiscoveredPlugin {
    /// User-facing plugin name (from manifest or directory name).
    pub fn plugin_name(&self) -> &str {
        &self.manifest.name
    }
}

/// Configuration for plugin discovery.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryConfig {
    /// CLI `--plugin-dir` paths.
    pub cli_plugin_dirs: Vec<PathBuf>,
    /// `[plugins].paths` from config.
    pub config_paths: Vec<PathBuf>,
    /// `[plugins].disabled` plugin IDs or names.
    pub disabled: Vec<String>,
    /// `[plugins].enabled` plugin IDs or names (overrides default-disabled for project plugins).
    pub enabled: Vec<String>,
    /// Source identities revoked in the current Echo scope. Compatibility
    /// settings/installs stay read-only, but their components cannot load.
    pub disabled_marketplace_sources: Vec<String>,
    /// Logical local aliases captured when their canonical source was revoked.
    pub disabled_marketplace_source_aliases: std::collections::BTreeMap<String, String>,
}

impl DiscoveryConfig {
    /// Ensure every discovered plugin appears in either `enabled` or `disabled`.
    ///
    /// Plugins from auto-enabled scopes (`CliOverride`, `ConfigPath`) are added
    /// to `enabled`. All others (`User`, `Project`) are added to `disabled`.
    /// Plugins already present in either list are left untouched.
    pub fn populate_plugin_lists(&mut self, discovered: &[DiscoveredPlugin]) {
        for dp in discovered {
            let name = &dp.manifest.name;
            let already_listed = self.enabled.iter().any(|e| e == name || e == &dp.id.0)
                || self.disabled.iter().any(|d| d == name || d == &dp.id.0);
            if already_listed {
                tracing::debug!(plugin = %name, id = %dp.id.0, "plugin already in enabled/disabled list");
                continue;
            }
            if matches!(dp.scope, PluginScope::CliOverride | PluginScope::ConfigPath) {
                tracing::debug!(plugin = %name, scope = ?dp.scope, "auto-adding to enabled list");
                self.enabled.push(name.clone());
            } else {
                tracing::debug!(plugin = %name, scope = ?dp.scope, "auto-adding to disabled list");
                self.disabled.push(name.clone());
            }
        }
    }
}

// ── Discovery entry point ─────────────────────────────────────────────

/// User plugin directories in priority order: `$ECHO_AGENT_HOME/plugins` then
/// `~/.claude/plugins`.
///
/// Unlike agent discovery, plugins are intentionally NOT discovered from a
/// legacy `~/.echo-agent/plugins`: plugin trust, persisted plugin-data, and install
/// paths all resolve under `echo_agent_home()`, so a plugin scanned from the legacy
/// tree would appear untrusted and lose its persisted state. Keeping plugins on
/// `echo_agent_home()` only avoids that half-initialized state.
fn user_plugin_dirs(home: Option<&Path>, echoagent: Option<&Path>) -> Vec<(PathBuf, PluginOrigin)> {
    let mut dirs = Vec::new();
    if let Some(g) = echoagent {
        dirs.push((g.join("plugins"), PluginOrigin::UserEchoAgent));
    }
    if let Some(h) = home {
        dirs.push((h.join(".claude").join("plugins"), PluginOrigin::UserClaude));
    }
    dirs
}

/// Origin for a project plugins parent dir: `.claude/plugins` vs `.echo-agent/plugins`.
fn project_plugins_dir_origin(plugins_dir: &Path) -> PluginOrigin {
    let is_claude = plugins_dir
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == ".claude");
    if is_claude {
        PluginOrigin::ProjectClaude
    } else {
        PluginOrigin::ProjectEchoAgent
    }
}

/// Project-scoped plugin parent dirs (`.echo-agent/plugins`, `.claude/plugins`) that
/// exist along the `cwd`→git-worktree-root walk (inclusive), or just `cwd`'s own
/// when `cwd` is not inside a git repo, paired with the resolved git worktree
/// root (when any). This is the exact set [`discover_plugins`] scans for
/// `PluginScope::Project`; the folder-trust gate reuses the same chain via
/// [`project_plugin_dirs_in`] so detection and discovery can never drift. The
/// returned root lets `discover_plugins` reuse it for the marketplace
/// `resolve(root)` branch instead of resolving the repo a second time.
pub fn project_plugin_dirs(cwd: Option<&Path>) -> (Vec<PathBuf>, Option<PathBuf>) {
    let Some(cwd) = cwd else {
        return (Vec::new(), None);
    };
    let chain = crate::repo::RepoDirChain::resolve(cwd);
    (project_plugin_dirs_in(&chain.dirs), chain.git_root)
}

/// Existing project plugin parent dirs (`.echo-agent/plugins`, `.claude/plugins`)
/// under each dir of a precomputed cwd→git-root chain
/// ([`crate::repo::RepoDirChain`]). The folder-trust gate reuses its one shared
/// chain here so detection and discovery can never drift.
pub fn project_plugin_dirs_in(chain_dirs: &[PathBuf]) -> Vec<PathBuf> {
    crate::repo::existing_subdirs_along(chain_dirs, &[".echo-agent/plugins", ".claude/plugins"])
}

/// Discover all plugins from the filesystem.
///
/// `cwd` is used to find the git worktree root for project-scope plugins.
/// `project_trusted` is the folder-trust verdict for `cwd`; it gates
/// `Project`-scope plugins (CLI/User/ConfigPath scopes are unaffected).
/// Returns plugins deduplicated by canonical path, with name conflicts
/// resolved by scope precedence.
pub fn discover_plugins(
    cwd: Option<&Path>,
    config: &DiscoveryConfig,
    trust_store: &TrustStore,
    project_trusted: bool,
) -> Vec<DiscoveredPlugin> {
    discover_plugins_with_user_home(
        cwd,
        config,
        trust_store,
        project_trusted,
        echo_agent_config::discovery_home_dir().as_deref(),
    )
}

fn discover_plugins_with_user_home(
    cwd: Option<&Path>,
    config: &DiscoveryConfig,
    trust_store: &TrustStore,
    project_trusted: bool,
    user_home: Option<&Path>,
) -> Vec<DiscoveredPlugin> {
    let _plugin_discovery_timer = crate::timing::timer("plugin_discovery");
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    let mut candidates: Vec<DiscoveredPlugin> = Vec::new();
    let user_revocations = user_home.map(|home| {
        super::marketplace_scope::RevokedCompatSources::read_user_with_aliases_in(
            &home.join(".claude"),
            &config.disabled_marketplace_sources,
            &config.disabled_marketplace_source_aliases,
        )
    });
    let mut project_revocations = None;

    // 1. CLI --plugin-dir paths
    for dir in &config.cli_plugin_dirs {
        if dir.is_dir() {
            collect_plugin(
                dir,
                PluginScope::CliOverride,
                PluginOrigin::CliOverride,
                trust_store,
                project_trusted,
                &mut seen_paths,
                &mut candidates,
            );
        } else {
            tracing::warn!(path = %dir.display(), "CLI --plugin-dir path is not a directory; skipping");
        }
    }

    // 2-3. Project plugins (.echo-agent/plugins/, .claude/plugins/) — scan the SAME
    // dirs the folder-trust gate detects, via the shared `project_plugin_dirs`
    // walk (cwd→git root), so discovery and gating can never drift.
    if let Some(cwd) = cwd {
        let (project_dirs, git_root) = project_plugin_dirs(Some(cwd));
        if let Some(root) = git_root.as_ref() {
            project_revocations = Some(
                super::marketplace_scope::RevokedCompatSources::read_with_aliases_in(
                    &root.join(".claude"),
                    &config.disabled_marketplace_sources,
                    &config.disabled_marketplace_source_aliases,
                ),
            );
        }
        for plugins_dir in project_dirs {
            let origin = project_plugins_dir_origin(&plugins_dir);
            scan_plugin_dir(
                &plugins_dir,
                PluginScope::Project,
                origin,
                trust_store,
                project_trusted,
                &mut seen_paths,
                &mut candidates,
            );
        }

        // 3b. Marketplace plugins (extraKnownMarketplaces in .claude/settings.json).
        // Reuse the git root resolved above (one walk, no second discover).
        if let Some(ref root) = git_root {
            for marketplace in &super::marketplace::resolve(root) {
                for dir in &marketplace.plugin_dirs {
                    collect_plugin(
                        dir,
                        PluginScope::Project,
                        PluginOrigin::ClaudeMarketplace {
                            marketplace: marketplace.name.clone(),
                        },
                        trust_store,
                        project_trusted,
                        &mut seen_paths,
                        &mut candidates,
                    );
                }
            }
        }
    }

    // 4-5. User plugins: $ECHO_AGENT_HOME/plugins, legacy ~/.echo-agent/plugins, ~/.claude/plugins.
    // Gate the echoagent plugins dir on user_echo_agent_home() so a project's .echo-agent/plugins
    // is never scanned as user-global when no home resolves.
    let echoagent = echo_agent_config::user_echo_agent_home();
    let plugin_dirs = user_plugin_dirs(user_home, echoagent.as_deref());
    for (plugins_dir, origin) in plugin_dirs {
        if plugins_dir.is_dir() {
            scan_plugin_dir(
                &plugins_dir,
                PluginScope::User,
                origin,
                trust_store,
                project_trusted,
                &mut seen_paths,
                &mut candidates,
            );
        }
    }

    // 5a. Known marketplaces (~/.claude/plugins/known_marketplaces.json).
    // Marketplace repos are cloned locally and registered here.
    // Tracks marketplace installs in installed_plugins.json with explicit
    // Each marketplace has a plugins/ (and optionally external_plugins/) subdirectory.
    let marketplaces = user_home
        .map(|home| super::marketplace::resolve_known_marketplaces_in(&home.join(".claude")))
        .unwrap_or_default();
    for marketplace in &marketplaces {
        for dir in &marketplace.plugin_dirs {
            collect_plugin(
                dir,
                PluginScope::User,
                PluginOrigin::ClaudeMarketplace {
                    marketplace: marketplace.name.clone(),
                },
                trust_store,
                project_trusted,
                &mut seen_paths,
                &mut candidates,
            );
        }
    }

    // 5b. Installed plugins (from install registry's managed directory)
    {
        // Installed plugins are always User scope (auto-trusted).
        // The user explicitly installed them via marketplace or CLI,
        // so they should be trusted regardless of install_dir location.
        let mut registry = super::install_registry::InstallRegistry::load();
        super::marketplace_scope::filter_installed(
            &mut registry,
            &config.disabled_marketplace_sources,
            &config.disabled_marketplace_source_aliases,
        );
        collect_installed_plugins(
            &registry,
            PluginScope::User,
            trust_store,
            project_trusted,
            &mut seen_paths,
            &mut candidates,
        );
    }

    // 5c. Installed plugins (~/.claude/plugins/installed_plugins.json).
    // installPath entries (nested under cache/<marketplace>/<plugin>/<version>/).
    // scope wins. Within same scope, first-found (alphabetical by canonical
    // with explicit installPath entries (nested under cache/<marketplace>/<plugin>/<version>/).
    // The plugin name is extracted from the JSON key ("name@marketplace").
    if let Some(home) = user_home {
        let installed_json = home
            .join(".claude")
            .join("plugins")
            .join("installed_plugins.json");
        for (name, marketplace, path) in read_claude_installed_plugins(&installed_json, cwd) {
            if path.is_dir() {
                let before = candidates.len();
                collect_plugin(
                    &path,
                    PluginScope::User,
                    PluginOrigin::ClaudeInstalled { marketplace },
                    trust_store,
                    project_trusted,
                    &mut seen_paths,
                    &mut candidates,
                );
                // Override dirname-derived name with the real name from the JSON key.
                if candidates.len() > before {
                    let plugin = candidates.last_mut().unwrap();
                    if plugin.manifest.name != name {
                        plugin.id = PluginId::new(plugin.scope, &plugin.canonical_root, &name);
                        plugin.manifest.name = name;
                    }
                }
            }
        }
    }

    // Revoke before conflict resolution and before lower-priority explicit
    // config paths are considered. Release canonical-path dedup entries too:
    // an explicit ConfigPath/CLI choice must remain usable even when a compat
    // scan of the same directory was revoked by its marketplace identity.
    candidates.retain(|plugin| {
        let revocations = if plugin.scope == PluginScope::Project {
            project_revocations.as_ref()
        } else {
            user_revocations.as_ref()
        };
        !revocations.is_some_and(|sources| sources.blocks(&plugin.origin, &plugin.canonical_root))
    });
    seen_paths = candidates
        .iter()
        .map(|plugin| plugin.canonical_root.clone())
        .collect();

    // 6. Config-path plugins
    for dir in &config.config_paths {
        if dir.is_dir() {
            collect_plugin(
                dir,
                PluginScope::ConfigPath,
                PluginOrigin::ConfigPath,
                trust_store,
                project_trusted,
                &mut seen_paths,
                &mut candidates,
            );
        } else {
            tracing::warn!(path = %dir.display(), "[plugins].paths entry is not a directory; skipping");
        }
    }

    // Resolve name conflicts: within the same plugin_name, highest-priority
    // scope wins. Within same scope, first-found (alphabetical by canonical
    // path) wins.
    resolve_name_conflicts(&mut candidates);

    for p in &candidates {
        tracing::info!(
            name = %p.manifest.name,
            scope = %p.scope,
            root = %p.root.display(),
            skills = p.manifest.skill_dirs(&p.root).len(),
            agents = p.manifest.agent_dirs(&p.root).len(),
            has_hooks = p.hooks_path.is_some(),
            has_mcp = p.mcp_config_path.is_some(),
            has_lsp = p.lsp_config_path.is_some(),
            "plugin discovered"
        );
    }
    tracing::info!(
        discovered = candidates.len(),
        cli = candidates
            .iter()
            .filter(|p| p.scope == PluginScope::CliOverride)
            .count(),
        project = candidates
            .iter()
            .filter(|p| p.scope == PluginScope::Project)
            .count(),
        user = candidates
            .iter()
            .filter(|p| p.scope == PluginScope::User)
            .count(),
        config = candidates
            .iter()
            .filter(|p| p.scope == PluginScope::ConfigPath)
            .count(),
        "plugins: discovery complete"
    );

    candidates
}

// ── Internal helpers ──────────────────────────────────────────────────

/// Scan a plugins parent directory (e.g. `~/.echo-agent/plugins/`) and collect
/// each subdirectory as a plugin candidate.
fn scan_plugin_dir(
    plugins_dir: &Path,
    scope: PluginScope,
    origin: PluginOrigin,
    trust_store: &TrustStore,
    project_trusted: bool,
    seen_paths: &mut HashSet<PathBuf>,
    candidates: &mut Vec<DiscoveredPlugin>,
) {
    let entries = match std::fs::read_dir(plugins_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(
                path = %plugins_dir.display(),
                error = %e,
                "failed to read plugins directory"
            );
            return;
        }
    };

    let mut subdirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir()) // follows symlinks
        .map(|e| e.path())
        .collect();

    // Sort for deterministic ordering within same scope
    subdirs.sort();

    for subdir in subdirs {
        collect_plugin(
            &subdir,
            scope,
            origin.clone(),
            trust_store,
            project_trusted,
            seen_paths,
            candidates,
        );
    }
}

fn collect_installed_plugins(
    registry: &super::install_registry::InstallRegistry,
    scope: PluginScope,
    trust_store: &TrustStore,
    project_trusted: bool,
    seen_paths: &mut HashSet<PathBuf>,
    candidates: &mut Vec<DiscoveredPlugin>,
) {
    for (_key, repo) in registry.list() {
        let origin = PluginOrigin::MarketplaceInstall {
            source_name: repo
                .marketplace
                .as_ref()
                .map(|mp| mp.source_display_name.clone()),
            git_url: match &repo.kind {
                super::install_registry::InstallKind::Git { url, .. } => Some(url.clone()),
                super::install_registry::InstallKind::Local { .. } => None,
            },
        };
        for plugin in repo.plugins.values() {
            let plugin_root = match plugin.subdir.as_deref() {
                Some(sub) => {
                    if subdir_escapes(sub) {
                        tracing::warn!(
                            repo = %repo.path.display(),
                            subdir = sub,
                            "skipping installed plugin: registry subdir escapes repo root"
                        );
                        continue;
                    }
                    repo.path.join(sub)
                }
                None => repo.path.clone(),
            };
            if plugin_root.is_dir() {
                collect_plugin(
                    &plugin_root,
                    scope,
                    origin.clone(),
                    trust_store,
                    project_trusted,
                    seen_paths,
                    candidates,
                );
            }
        }
    }
}

fn subdir_escapes(subdir: &str) -> bool {
    let p = Path::new(subdir);
    p.is_absolute()
        || p.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
}

/// Attempt to load a single plugin directory and add it to candidates.
fn collect_plugin(
    plugin_root: &Path,
    scope: PluginScope,
    origin: PluginOrigin,
    trust_store: &TrustStore,
    project_trusted: bool,
    seen_paths: &mut HashSet<PathBuf>,
    candidates: &mut Vec<DiscoveredPlugin>,
) {
    // Canonicalize for dedup
    let canonical = match dunce::canonicalize(plugin_root) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                path = %plugin_root.display(),
                error = %e,
                "failed to canonicalize plugin root; skipping"
            );
            return;
        }
    };

    // Deduplicate by canonical path
    if !seen_paths.insert(canonical.clone()) {
        tracing::debug!(
            path = %plugin_root.display(),
            canonical = %canonical.display(),
            "plugin directory already discovered via another path; skipping"
        );
        return;
    }

    // Load manifest or derive from directory name
    let manifest = match load_manifest(plugin_root) {
        Ok(ManifestLoadResult::Found(m)) => *m,
        Ok(ManifestLoadResult::NotFound) => {
            // Convention-based: derive name from directory, check for
            // skills/ or agents/ or .mcp.json or hooks/hooks.json
            let Some(name) = name_from_dirname(plugin_root) else {
                tracing::debug!(
                    path = %plugin_root.display(),
                    "cannot derive plugin name from directory; skipping"
                );
                return;
            };

            // Only treat as a plugin if it has at least one component
            let has_skills =
                plugin_root.join("skills").is_dir() || plugin_root.join("commands").is_dir();
            let has_agents = plugin_root.join("agents").is_dir();
            let has_mcp = plugin_root.join(".mcp.json").is_file();
            let has_lsp = plugin_root.join(".lsp.json").is_file();
            let has_hooks = plugin_root.join("hooks").join("hooks.json").is_file();

            if !has_skills && !has_agents && !has_mcp && !has_lsp && !has_hooks {
                tracing::debug!(
                    path = %plugin_root.display(),
                    "directory has no manifest and no recognized plugin components; skipping"
                );
                return;
            }

            PluginManifest {
                name,
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
            }
        }
        Err(e) => {
            tracing::warn!(
                path = %plugin_root.display(),
                error = %e,
                "failed to load plugin manifest; skipping"
            );
            return;
        }
    };

    // Determine trust status. Exhaustive match so a new PluginScope variant is a
    // compile error rather than a silent default.
    let trusted = match scope {
        PluginScope::CliOverride | PluginScope::User => true,
        PluginScope::ConfigPath => {
            TrustStore::is_config_path_auto_trusted(plugin_root)
                || trust_store.is_trusted(plugin_root)
        }
        // Project trust now comes from folder-trust (passed by the caller).
        PluginScope::Project => project_trusted,
    };

    // Build PluginId
    let id = PluginId::new(scope, &canonical, &manifest.name);

    // Resolve component paths
    let skill_dirs = manifest.skill_dirs(plugin_root);
    let command_dirs = manifest.command_dirs(plugin_root);
    let agent_dirs = manifest.agent_dirs(plugin_root);
    let hooks_path = manifest.hooks_path(plugin_root);
    let mcp_config_path = manifest.mcp_config_path(plugin_root);
    let lsp_config_path = manifest.lsp_config_path(plugin_root);

    candidates.push(DiscoveredPlugin {
        manifest,
        id,
        root: plugin_root.to_path_buf(),
        canonical_root: canonical,
        scope,
        origin,
        trusted,
        skill_dirs,
        command_dirs,
        agent_dirs,
        hooks_path,
        mcp_config_path,
        lsp_config_path,
        conflict: None,
    });
}

/// Resolve plugin_name conflicts across scopes.
///
/// Within each name group, keep only the highest-priority candidate
/// (lowest scope ordinal). Log warnings for dropped duplicates.
fn resolve_name_conflicts(candidates: &mut Vec<DiscoveredPlugin>) {
    let mut name_map: HashMap<String, usize> = HashMap::new();
    let mut to_remove: Vec<usize> = Vec::new();
    // (winner_idx, conflict message) pairs to apply after the scan.
    let mut conflict_msgs: Vec<(usize, String)> = Vec::new();

    for (idx, candidate) in candidates.iter().enumerate() {
        let name = candidate.manifest.name.clone();
        match name_map.get(&name) {
            Some(&existing_idx) => {
                let existing = &candidates[existing_idx];
                // Lower scope ordinal = higher priority
                if (candidate.scope as u8) < (existing.scope as u8) {
                    // New candidate wins
                    tracing::warn!(
                        plugin_name = %name,
                        winner = %candidate.root.display(),
                        loser = %existing.root.display(),
                        "plugin name collision resolved by scope precedence"
                    );
                    let msg = format!(
                        "Name collision: shadowing \"{}\" from {}",
                        name,
                        existing.root.display()
                    );
                    conflict_msgs.push((idx, msg));
                    to_remove.push(existing_idx);
                    name_map.insert(name, idx);
                } else {
                    // Existing wins
                    tracing::warn!(
                        plugin_name = %name,
                        winner = %existing.root.display(),
                        loser = %candidate.root.display(),
                        "plugin name collision resolved by scope precedence"
                    );
                    let msg = format!(
                        "Name collision: shadowing \"{}\" from {}",
                        name,
                        candidate.root.display()
                    );
                    conflict_msgs.push((existing_idx, msg));
                    to_remove.push(idx);
                }
            }
            None => {
                name_map.insert(name, idx);
            }
        }
    }

    // Apply conflict messages to winners.
    for (idx, msg) in conflict_msgs {
        candidates[idx].conflict = Some(msg);
    }

    // Remove losers (reverse order to preserve indices)
    to_remove.sort_unstable();
    to_remove.dedup();
    for idx in to_remove.into_iter().rev() {
        candidates.remove(idx);
    }
}

// ── Compat installed_plugins.json types ───────────────────────────────

/// Compat `installed_plugins.json` format.
#[derive(serde::Deserialize)]
struct ClaudeInstalledPlugins {
    #[serde(default)]
    plugins: HashMap<String, Vec<ClaudeInstalledEntry>>,
}

/// A single install entry within `installed_plugins.json`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeInstalledEntry {
    install_path: PathBuf,
    scope: Option<String>,
    project_path: Option<PathBuf>,
}

/// Whether a compat install entry should surface for this session `cwd`.
///
/// The `local` and `project` scopes are project-tied (as is any entry with
/// a non-empty `projectPath`): only visible when `cwd` is under `project_path`
/// (path-component prefix). Missing/empty project path or cwd cannot prove
/// in-project, so they stay hidden. User/unscoped with no path always surface.
fn claude_install_visible(
    scope: Option<&str>,
    project_path: Option<&Path>,
    cwd: Option<&Path>,
) -> bool {
    let has_project_path = project_path.is_some_and(|p| !p.as_os_str().is_empty());
    let project_gated = matches!(scope, Some("local") | Some("project")) || has_project_path;
    if !project_gated {
        return true;
    }
    let Some(project) = project_path.filter(|p| !p.as_os_str().is_empty()) else {
        return false;
    };
    let Some(cwd) = cwd else {
        return false;
    };
    let cwd = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let project = dunce::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    cwd.starts_with(&project)
}

/// Read plugin names and install paths from compat `installed_plugins.json`.
///
/// Keys are `"plugin-name@marketplace"` — the plugin name is extracted from
/// before `@`, the marketplace name from after it (when present).
/// Returns `(name, marketplace, path)` tuples. Empty vec on any error.
///
/// Project-tied entries are filtered by `cwd` vs `projectPath` (see
/// [`claude_install_visible`]).
fn read_claude_installed_plugins(
    json_path: &Path,
    cwd: Option<&Path>,
) -> Vec<(String, Option<String>, PathBuf)> {
    let content = match std::fs::read_to_string(json_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let registry: ClaudeInstalledPlugins = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                path = %json_path.display(),
                error = %e,
                "failed to parse installed_plugins.json"
            );
            return vec![];
        }
    };

    registry
        .plugins
        .into_iter()
        .flat_map(|(key, entries)| {
            let mut parts = key.splitn(2, '@');
            let name = parts.next().unwrap_or(&key).to_string();
            let marketplace = parts.next().map(String::from);
            entries
                .into_iter()
                .filter(|entry| {
                    claude_install_visible(
                        entry.scope.as_deref(),
                        entry.project_path.as_deref(),
                        cwd,
                    )
                })
                .map(move |entry| (name.clone(), marketplace.clone(), entry.install_path))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_marketplace_scope_removes_real_discovery_components_but_keeps_explicit_path() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("external-user");
        let claude = fake_home.join(".claude");
        let install = tmp.path().join("external-marketplace");
        let name = "scoped-marketplace-component-regression";
        let url = "https://127.0.0.1:4443/component-regression.git";
        let plugin = install.join("plugins").join(name);
        std::fs::create_dir_all(plugin.join("skills/notes")).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            serde_json::json!({"name":name,"skills":"skills","version":"1.0.0"}).to_string(),
        )
        .unwrap();
        std::fs::write(
            plugin.join("skills/notes/SKILL.md"),
            "# Scoped static notes\n",
        )
        .unwrap();
        std::fs::create_dir_all(claude.join("plugins")).unwrap();
        let settings = claude.join("settings.json");
        let registry = claude.join("plugins/known_marketplaces.json");
        std::fs::write(
            &settings,
            serde_json::json!({"enabledPlugins":{format!("{name}@review-market"):true}})
                .to_string(),
        )
        .unwrap();
        std::fs::write(&registry, serde_json::json!({"review-market":{"source":{"source":"git","url":url},"installLocation":install}}).to_string()).unwrap();
        let before = [
            std::fs::read(&settings).unwrap(),
            std::fs::read(&registry).unwrap(),
        ];
        let trust = TrustStore::load_from(tmp.path().join("owned-trust"));
        let mut config = DiscoveryConfig {
            enabled: vec![name.into()],
            ..Default::default()
        };
        let discovered =
            discover_plugins_with_user_home(None, &config, &trust, true, Some(&fake_home));
        let active = super::super::registry::PluginRegistry::from_discovered(
            discovered,
            &config.disabled,
            &config.enabled,
        );
        assert_eq!(
            active.get(name).unwrap().skill_count,
            1,
            "Fixture must reach the real executable-component registry before removal"
        );
        config.disabled_marketplace_sources = vec![url.into()];
        let discovered =
            discover_plugins_with_user_home(None, &config, &trust, true, Some(&fake_home));
        let revoked = super::super::registry::PluginRegistry::from_discovered(
            discovered,
            &config.disabled,
            &config.enabled,
        );
        assert!(
            revoked.get(name).is_none(),
            "Removed source still registered its skill namespace"
        );
        config.config_paths = vec![plugin.clone()];
        let discovered =
            discover_plugins_with_user_home(None, &config, &trust, true, Some(&fake_home));
        let explicit = discovered
            .iter()
            .find(|candidate| candidate.manifest.name == name)
            .unwrap();
        assert_eq!(
            explicit.origin,
            PluginOrigin::ConfigPath,
            "Revoked compat scan incorrectly consumed the explicit canonical path"
        );
        assert_eq!(std::fs::read(&settings).unwrap(), before[0]);
        assert_eq!(std::fs::read(&registry).unwrap(), before[1]);
        assert!(plugin.join("skills/notes/SKILL.md").is_file());
    }

    fn make_manifest_plugin(tmp: &Path, name: &str) -> PathBuf {
        let plugin_dir = tmp.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            format!(r#"{{"name": "{}"}}"#, name),
        )
        .unwrap();
        plugin_dir
    }

    fn make_convention_plugin(tmp: &Path, name: &str) -> PathBuf {
        let plugin_dir = tmp.join(name);
        std::fs::create_dir_all(plugin_dir.join("skills")).unwrap();
        plugin_dir
    }

    #[test]
    fn user_plugin_dirs_are_echo_agent_and_claude_only_no_legacy() {
        let home = Path::new("/home/u");
        let echoagent = Path::new("/custom/echoagenthome");
        let dirs = user_plugin_dirs(Some(home), Some(echoagent));
        assert!(dirs.contains(&(echoagent.join("plugins"), PluginOrigin::UserEchoAgent)));
        assert!(dirs.contains(&(
            home.join(".claude").join("plugins"),
            PluginOrigin::UserClaude
        )));
        // Plugins are not discovered from the legacy ~/.echo-agent tree.
        assert!(
            !dirs
                .iter()
                .any(|(p, _)| p == &home.join(".echo-agent").join("plugins"))
        );
    }

    #[test]
    fn user_plugin_dirs_empty_without_home_or_echo_agent() {
        assert!(user_plugin_dirs(None, None).is_empty());
    }

    #[test]
    fn discover_cli_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = make_manifest_plugin(tmp.path(), "cli-tool");

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        collect_plugin(
            &plugin_dir,
            PluginScope::CliOverride,
            PluginOrigin::CliOverride,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].plugin_name(), "cli-tool");
        assert_eq!(candidates[0].scope, PluginScope::CliOverride);
        assert_eq!(candidates[0].origin, PluginOrigin::CliOverride);
        assert!(candidates[0].trusted);
    }

    #[test]
    fn project_plugins_dir_origin_distinguishes_echo_agent_and_claude() {
        assert_eq!(
            project_plugins_dir_origin(Path::new("/repo/.echo-agent/plugins")),
            PluginOrigin::ProjectEchoAgent
        );
        assert_eq!(
            project_plugins_dir_origin(Path::new("/repo/.claude/plugins")),
            PluginOrigin::ProjectClaude
        );
    }

    #[test]
    fn discover_user_plugins() {
        let tmp = tempfile::tempdir().unwrap();

        // Create ~/.echo-agent/plugins/ structure
        let echo_agent_plugins = tmp.path().join(".echo-agent").join("plugins");
        std::fs::create_dir_all(&echo_agent_plugins).unwrap();
        make_manifest_plugin(&echo_agent_plugins, "user-tool");

        // Override home dir by directly scanning
        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        scan_plugin_dir(
            &echo_agent_plugins,
            PluginScope::User,
            PluginOrigin::UserEchoAgent,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].plugin_name(), "user-tool");
        assert!(candidates[0].trusted);
    }

    #[test]
    fn discover_convention_plugin_no_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        make_convention_plugin(&plugins_dir, "my-tool");

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        scan_plugin_dir(
            &plugins_dir,
            PluginScope::User,
            PluginOrigin::UserEchoAgent,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].plugin_name(), "my-tool");
    }

    #[test]
    fn installed_plugins_load_at_registered_subdir_and_root() {
        use crate::plugins::install_registry::{
            InstallKind, InstallRegistry, InstalledRepo, RepoPlugin,
        };
        use std::collections::HashMap;

        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path().join("installed-plugins");

        let sub_key = "acme-deadbeef";
        let sub_repo_path = install_dir.join(sub_key);
        let sub_plugin_dir = sub_repo_path.join("plugins").join("acme");
        std::fs::create_dir_all(&sub_plugin_dir).unwrap();
        std::fs::write(sub_plugin_dir.join("plugin.json"), r#"{"name": "acme"}"#).unwrap();

        let root_key = "cloud-cafef00d";
        let root_repo_path = install_dir.join(root_key);
        std::fs::create_dir_all(&root_repo_path).unwrap();
        std::fs::write(root_repo_path.join("plugin.json"), r#"{"name": "cloud"}"#).unwrap();

        let mut registry = InstallRegistry::empty(install_dir);
        registry.insert(
            sub_key.to_string(),
            InstalledRepo {
                kind: InstallKind::Local {
                    source_path: sub_repo_path.clone(),
                    subdir: None,
                },
                installed_at: String::new(),
                updated_at: String::new(),
                path: sub_repo_path,
                plugins: HashMap::from([(
                    "acme".to_string(),
                    RepoPlugin {
                        subdir: Some("plugins/acme".to_string()),
                        version: None,
                    },
                )]),
                marketplace: None,
            },
        );
        registry.insert(
            root_key.to_string(),
            InstalledRepo {
                kind: InstallKind::Local {
                    source_path: root_repo_path.clone(),
                    subdir: None,
                },
                installed_at: String::new(),
                updated_at: String::new(),
                path: root_repo_path,
                plugins: HashMap::from([(
                    "cloud".to_string(),
                    RepoPlugin {
                        subdir: None,
                        version: None,
                    },
                )]),
                marketplace: None,
            },
        );

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        collect_installed_plugins(
            &registry,
            PluginScope::User,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );

        let names: Vec<&str> = candidates.iter().map(|p| p.plugin_name()).collect();
        assert_eq!(candidates.len(), 2, "got {names:?}");
        assert!(
            names.contains(&"acme"),
            "subdir plugin should load, got {names:?}"
        );
        assert!(
            names.contains(&"cloud"),
            "root plugin should load, got {names:?}"
        );
        for p in &candidates {
            assert_eq!(
                p.origin,
                PluginOrigin::MarketplaceInstall {
                    source_name: None,
                    git_url: None,
                },
                "direct local installs carry neither source name nor git URL"
            );
        }
    }

    #[test]
    fn installed_plugins_record_marketplace_and_git_origin() {
        use crate::plugins::install_registry::{
            InstallKind, InstallRegistry, InstalledRepo, MarketplaceProvenance, RepoPlugin,
        };
        use std::collections::HashMap;

        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path().join("installed-plugins");

        let mp_repo_path = install_dir.join("mp-plugin-11111111");
        std::fs::create_dir_all(&mp_repo_path).unwrap();
        std::fs::write(mp_repo_path.join("plugin.json"), r#"{"name": "mp-plugin"}"#).unwrap();

        let git_repo_path = install_dir.join("git-plugin-22222222");
        std::fs::create_dir_all(&git_repo_path).unwrap();
        std::fs::write(
            git_repo_path.join("plugin.json"),
            r#"{"name": "git-plugin"}"#,
        )
        .unwrap();

        let mut registry = InstallRegistry::empty(install_dir);
        registry.insert(
            "mp-plugin-11111111".to_string(),
            InstalledRepo {
                kind: InstallKind::Git {
                    url: "https://example.com/mp.git".to_string(),
                    git_ref: None,
                    commit: "abc".to_string(),
                    subdir: None,
                },
                installed_at: String::new(),
                updated_at: String::new(),
                path: mp_repo_path,
                plugins: HashMap::from([(
                    "mp-plugin".to_string(),
                    RepoPlugin {
                        subdir: None,
                        version: None,
                    },
                )]),
                marketplace: Some(MarketplaceProvenance {
                    source_url_or_path: "https://example.com/mp.git".to_string(),
                    canonical_source_identity: None,
                    source_display_name: "Demo Marketplace".to_string(),
                    plugin_subdir: "plugins/mp-plugin".to_string(),
                }),
            },
        );
        registry.insert(
            "git-plugin-22222222".to_string(),
            InstalledRepo {
                kind: InstallKind::Git {
                    url: "https://github.com/owner/repo.git".to_string(),
                    git_ref: None,
                    commit: "def".to_string(),
                    subdir: None,
                },
                installed_at: String::new(),
                updated_at: String::new(),
                path: git_repo_path,
                plugins: HashMap::from([(
                    "git-plugin".to_string(),
                    RepoPlugin {
                        subdir: None,
                        version: None,
                    },
                )]),
                marketplace: None,
            },
        );

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        collect_installed_plugins(
            &registry,
            PluginScope::User,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );

        let mp = candidates
            .iter()
            .find(|p| p.plugin_name() == "mp-plugin")
            .unwrap();
        assert_eq!(
            mp.origin,
            PluginOrigin::MarketplaceInstall {
                source_name: Some("Demo Marketplace".to_string()),
                git_url: Some("https://example.com/mp.git".to_string()),
            }
        );

        let git = candidates
            .iter()
            .find(|p| p.plugin_name() == "git-plugin")
            .unwrap();
        assert_eq!(
            git.origin,
            PluginOrigin::MarketplaceInstall {
                source_name: None,
                git_url: Some("https://github.com/owner/repo.git".to_string()),
            }
        );
    }

    #[test]
    fn installed_plugins_skip_subdir_that_escapes_repo() {
        use crate::plugins::install_registry::{
            InstallKind, InstallRegistry, InstalledRepo, RepoPlugin,
        };
        use std::collections::HashMap;

        let tmp = tempfile::tempdir().unwrap();
        let install_dir = tmp.path().join("installed-plugins");

        let escaped_dir = install_dir.join("escaped");
        std::fs::create_dir_all(&escaped_dir).unwrap();
        std::fs::write(
            escaped_dir.join("plugin.json"),
            r#"{"name": "escaped-evil"}"#,
        )
        .unwrap();

        let evil_key = "evil-deadbeef";
        let evil_repo_path = install_dir.join(evil_key);
        std::fs::create_dir_all(&evil_repo_path).unwrap();

        let good_key = "good-cafef00d";
        let good_repo_path = install_dir.join(good_key);
        let good_plugin_dir = good_repo_path.join("plugins").join("good");
        std::fs::create_dir_all(&good_plugin_dir).unwrap();
        std::fs::write(good_plugin_dir.join("plugin.json"), r#"{"name": "good"}"#).unwrap();

        let mut registry = InstallRegistry::empty(install_dir);
        registry.insert(
            evil_key.to_string(),
            InstalledRepo {
                kind: InstallKind::Local {
                    source_path: evil_repo_path.clone(),
                    subdir: None,
                },
                installed_at: String::new(),
                updated_at: String::new(),
                path: evil_repo_path,
                plugins: HashMap::from([(
                    "escaped-evil".to_string(),
                    RepoPlugin {
                        subdir: Some("../escaped".to_string()),
                        version: None,
                    },
                )]),
                marketplace: None,
            },
        );
        registry.insert(
            good_key.to_string(),
            InstalledRepo {
                kind: InstallKind::Local {
                    source_path: good_repo_path.clone(),
                    subdir: None,
                },
                installed_at: String::new(),
                updated_at: String::new(),
                path: good_repo_path,
                plugins: HashMap::from([(
                    "good".to_string(),
                    RepoPlugin {
                        subdir: Some("plugins/good".to_string()),
                        version: None,
                    },
                )]),
                marketplace: None,
            },
        );

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        collect_installed_plugins(
            &registry,
            PluginScope::User,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );

        let names: Vec<&str> = candidates.iter().map(|p| p.plugin_name()).collect();
        assert!(
            names.contains(&"good"),
            "legit subdir plugin should load, got {names:?}"
        );
        assert!(
            !names.contains(&"escaped-evil"),
            "escaping subdir must be skipped, got {names:?}"
        );
    }

    #[test]
    fn dedup_by_canonical_path() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = make_manifest_plugin(tmp.path(), "dup-test");

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        // Add same plugin twice
        collect_plugin(
            &plugin_dir,
            PluginScope::CliOverride,
            PluginOrigin::CliOverride,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );
        collect_plugin(
            &plugin_dir,
            PluginScope::CliOverride,
            PluginOrigin::CliOverride,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );

        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn name_conflict_higher_priority_wins() {
        let tmp = tempfile::tempdir().unwrap();

        // Create two plugins with the same name but different scopes
        let cli_dir = tmp.path().join("cli");
        std::fs::create_dir_all(&cli_dir).unwrap();
        std::fs::write(cli_dir.join("plugin.json"), r#"{"name": "my-plugin"}"#).unwrap();

        let user_dir = tmp.path().join("user-plugins");
        std::fs::create_dir_all(&user_dir).unwrap();
        let user_plugin = user_dir.join("my-plugin");
        std::fs::create_dir_all(&user_plugin).unwrap();
        std::fs::write(user_plugin.join("plugin.json"), r#"{"name": "my-plugin"}"#).unwrap();

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();

        // Add CLI plugin first (higher priority)
        collect_plugin(
            &cli_dir,
            PluginScope::CliOverride,
            PluginOrigin::CliOverride,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );
        // Add user plugin (lower priority)
        collect_plugin(
            &user_plugin,
            PluginScope::User,
            PluginOrigin::UserEchoAgent,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );

        resolve_name_conflicts(&mut candidates);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].scope, PluginScope::CliOverride);
    }

    #[test]
    fn plugin_id_format() {
        let id = PluginId::new(
            PluginScope::User,
            Path::new("/home/user/.echo-agent/plugins/my-plugin"),
            "my-plugin",
        );
        assert!(id.0.starts_with("user/"));
        assert!(id.0.ends_with("/my-plugin"));
        // Format: user/<hex8>/my-plugin
        let parts: Vec<&str> = id.0.split('/').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "user");
        assert_eq!(parts[1].len(), 8); // 8 hex chars
        assert_eq!(parts[2], "my-plugin");
    }

    #[test]
    fn plugin_id_deterministic() {
        let id1 = PluginId::new(PluginScope::User, Path::new("/same/path"), "test");
        let id2 = PluginId::new(PluginScope::User, Path::new("/same/path"), "test");
        assert_eq!(id1, id2);
    }

    #[test]
    fn plugin_id_different_paths() {
        let id1 = PluginId::new(PluginScope::User, Path::new("/path/a"), "test");
        let id2 = PluginId::new(PluginScope::User, Path::new("/path/b"), "test");
        assert_ne!(id1, id2);
    }

    #[test]
    fn empty_dir_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let empty_dir = tmp.path().join("empty");
        std::fs::create_dir_all(&empty_dir).unwrap();

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        collect_plugin(
            &empty_dir,
            PluginScope::User,
            PluginOrigin::UserEchoAgent,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );

        // No manifest + no skills/agents/mcp/hooks = skipped
        assert!(candidates.is_empty());
    }

    #[test]
    fn project_plugin_untrusted_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = make_manifest_plugin(tmp.path(), "project-tool");

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        // Untrusted folder (project_trusted = false) blocks the Project plugin.
        collect_plugin(
            &plugin_dir,
            PluginScope::Project,
            PluginOrigin::ProjectEchoAgent,
            &trust,
            false,
            &mut seen,
            &mut candidates,
        );

        assert_eq!(candidates.len(), 1);
        assert!(!candidates[0].trusted);
    }

    #[test]
    fn project_plugin_trusted_when_folder_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = make_manifest_plugin(tmp.path(), "trusted-tool");

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        // Trusted folder (project_trusted = true) allows the Project plugin.
        collect_plugin(
            &plugin_dir,
            PluginScope::Project,
            PluginOrigin::ProjectEchoAgent,
            &trust,
            true,
            &mut seen,
            &mut candidates,
        );

        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].trusted);
    }

    #[test]
    fn non_project_scopes_unaffected_by_project_trusted() {
        // project_trusted = false gates Project scope only: CLI/User stay
        // auto-trusted and ConfigPath keeps using its own trust store.
        let tmp = tempfile::tempdir().unwrap();
        let cli_dir = make_manifest_plugin(tmp.path(), "cli-tool");
        let user_dir = make_manifest_plugin(tmp.path(), "user-tool");
        let config_dir = make_manifest_plugin(tmp.path(), "config-tool");

        let mut trust = TrustStore::load_from(tmp.path().join("trust"));
        trust.grant_trust(&config_dir).unwrap();

        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        for (dir, scope, origin) in [
            (
                &cli_dir,
                PluginScope::CliOverride,
                PluginOrigin::CliOverride,
            ),
            (&user_dir, PluginScope::User, PluginOrigin::UserEchoAgent),
            (
                &config_dir,
                PluginScope::ConfigPath,
                PluginOrigin::ConfigPath,
            ),
        ] {
            collect_plugin(
                dir,
                scope,
                origin,
                &trust,
                false,
                &mut seen,
                &mut candidates,
            );
        }

        assert_eq!(candidates.len(), 3);
        assert!(
            candidates.iter().all(|c| c.trusted),
            "CLI/User/ConfigPath plugins must stay trusted under project_trusted=false"
        );
    }

    #[test]
    fn discover_real_project_plugin_gated_on_project_trusted() {
        // End-to-end through discover_plugins: a repo-local `.echo-agent/plugins/<x>/`
        // plugin with an MCP component is trusted iff the folder-trust verdict
        // (project_trusted) allows it. Found by name so any user-scoped plugins
        // on the test host are irrelevant.
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp
            .path()
            .join(".echo-agent")
            .join("plugins")
            .join("proj-mcp");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.json"), r#"{"name": "proj-mcp"}"#).unwrap();
        std::fs::write(plugin_dir.join(".mcp.json"), r#"{"mcpServers":{}}"#).unwrap();

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let config = DiscoveryConfig::default();

        // Untrusted folder: the project plugin comes back blocked.
        let untrusted = discover_plugins(Some(tmp.path()), &config, &trust, false);
        let p = untrusted
            .iter()
            .find(|p| p.manifest.name == "proj-mcp")
            .expect("project plugin discovered");
        assert_eq!(p.scope, PluginScope::Project);
        assert_eq!(p.origin, PluginOrigin::ProjectEchoAgent);
        assert!(!p.trusted, "untrusted folder must block the project plugin");

        // Trusted folder: the same plugin is allowed.
        let trusted = discover_plugins(Some(tmp.path()), &config, &trust, true);
        let p = trusted
            .iter()
            .find(|p| p.manifest.name == "proj-mcp")
            .expect("project plugin discovered");
        assert!(p.trusted, "trusted folder must allow the project plugin");
    }

    #[test]
    fn discover_project_claude_plugin_records_claude_origin() {
        // Unique name: discover_plugins also scans the dev machine's real
        // user dirs, and this test finds its plugin by name.
        let name = format!("proj-claude-tool-{}", std::process::id());
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join(".claude").join("plugins").join(&name);
        std::fs::create_dir_all(plugin_dir.join("skills")).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            format!(r#"{{"name": "{name}"}}"#),
        )
        .unwrap();

        let trust = TrustStore::load_from(tmp.path().join("trust"));
        let config = DiscoveryConfig::default();
        let discovered = discover_plugins(Some(tmp.path()), &config, &trust, true);
        let p = discovered
            .iter()
            .find(|p| p.manifest.name == name)
            .expect("project claude plugin discovered");
        assert_eq!(p.scope, PluginScope::Project);
        assert_eq!(p.origin, PluginOrigin::ProjectClaude);
    }

    #[test]
    fn read_claude_installed_plugins_json() {
        let tmp = tempfile::tempdir().unwrap();

        // Create fake plugin directories matching the compat cache layout
        let cache = tmp.path().join("cache");
        let plugin_a = cache.join("marketplace-a").join("my-lsp").join("1.0.0");
        let plugin_b = cache.join("marketplace-b").join("my-tool").join("2.0.0");
        std::fs::create_dir_all(plugin_a.join("skills")).unwrap();
        std::fs::create_dir_all(&plugin_b).unwrap();
        std::fs::write(plugin_b.join("plugin.json"), r#"{"name": "my-tool"}"#).unwrap();

        // Write installed_plugins.json
        let json_path = tmp.path().join("installed_plugins.json");
        let json = serde_json::json!({
            "version": 2,
            "plugins": {
                "my-lsp@marketplace-a": [{
                    "scope": "user",
                    "installPath": plugin_a.to_string_lossy(),
                    "version": "1.0.0"
                }],
                "my-tool@marketplace-b": [{
                    "scope": "user",
                    "installPath": plugin_b.to_string_lossy(),
                    "version": "2.0.0"
                }]
            }
        });
        std::fs::write(&json_path, serde_json::to_string(&json).unwrap()).unwrap();

        let results = read_claude_installed_plugins(&json_path, None);
        assert_eq!(results.len(), 2);

        let names: Vec<&str> = results.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"my-lsp"));
        assert!(names.contains(&"my-tool"));

        let paths: Vec<&PathBuf> = results.iter().map(|(_, _, p)| p).collect();
        assert!(paths.contains(&&plugin_a));
        assert!(paths.contains(&&plugin_b));

        let lsp = results.iter().find(|(n, _, _)| n == "my-lsp").unwrap();
        assert_eq!(lsp.1.as_deref(), Some("marketplace-a"));
        let tool = results.iter().find(|(n, _, _)| n == "my-tool").unwrap();
        assert_eq!(tool.1.as_deref(), Some("marketplace-b"));
    }

    #[test]
    fn read_claude_installed_plugins_key_without_marketplace() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = tmp.path().join("bare");
        std::fs::create_dir_all(&plugin).unwrap();
        let json_path = tmp.path().join("installed_plugins.json");
        let json = serde_json::json!({
            "plugins": { "bare": [{ "installPath": plugin.to_string_lossy() }] }
        });
        std::fs::write(&json_path, serde_json::to_string(&json).unwrap()).unwrap();

        let results = read_claude_installed_plugins(&json_path, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "bare");
        assert_eq!(results[0].1, None);
    }

    #[test]
    fn claude_local_install_included_when_cwd_under_project() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        let nested = project.join("src");
        let plugin = tmp.path().join("cache").join("local-plug");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&plugin).unwrap();
        let json_path = tmp.path().join("installed_plugins.json");
        let json = serde_json::json!({
            "plugins": {
                "local-plug@mp": [{
                    "scope": "local",
                    "projectPath": project.to_string_lossy(),
                    "installPath": plugin.to_string_lossy()
                }]
            }
        });
        std::fs::write(&json_path, serde_json::to_string(&json).unwrap()).unwrap();

        let results = read_claude_installed_plugins(&json_path, Some(&nested));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "local-plug");
        assert_eq!(results[0].2, plugin);
    }

    #[test]
    fn claude_local_install_excluded_when_cwd_outside_project() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        let other = tmp.path().join("other");
        let plugin = tmp.path().join("cache").join("local-plug");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(&plugin).unwrap();
        let json_path = tmp.path().join("installed_plugins.json");
        let json = serde_json::json!({
            "plugins": {
                "local-plug@mp": [{
                    "scope": "local",
                    "projectPath": project.to_string_lossy(),
                    "installPath": plugin.to_string_lossy()
                }]
            }
        });
        std::fs::write(&json_path, serde_json::to_string(&json).unwrap()).unwrap();

        let results = read_claude_installed_plugins(&json_path, Some(&other));
        assert!(results.is_empty());
    }

    #[test]
    fn claude_user_and_unscoped_installs_included_regardless_of_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        let other = tmp.path().join("other");
        let user_plugin = tmp.path().join("cache").join("user-plug");
        let bare_plugin = tmp.path().join("cache").join("bare-plug");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(&user_plugin).unwrap();
        std::fs::create_dir_all(&bare_plugin).unwrap();
        let json_path = tmp.path().join("installed_plugins.json");
        let json = serde_json::json!({
            "plugins": {
                "user-plug@mp": [{
                    "scope": "user",
                    "installPath": user_plugin.to_string_lossy()
                }],
                "bare-plug@mp": [{
                    "installPath": bare_plugin.to_string_lossy()
                }],
                "local-plug@mp": [{
                    "scope": "local",
                    "projectPath": project.to_string_lossy(),
                    "installPath": tmp.path().join("cache").join("local-plug").to_string_lossy()
                }]
            }
        });
        std::fs::write(&json_path, serde_json::to_string(&json).unwrap()).unwrap();

        let results = read_claude_installed_plugins(&json_path, Some(&other));
        let names: Vec<&str> = results.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"user-plug"));
        assert!(names.contains(&"bare-plug"));
        assert!(!names.contains(&"local-plug"));
    }

    #[test]
    fn claude_local_install_excluded_without_project_path_or_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = tmp.path().join("cache").join("local-plug");
        std::fs::create_dir_all(&plugin).unwrap();
        let json_path = tmp.path().join("installed_plugins.json");
        let json = serde_json::json!({
            "plugins": {
                "no-path@mp": [{
                    "scope": "local",
                    "installPath": plugin.to_string_lossy()
                }],
                "empty-path@mp": [{
                    "scope": "local",
                    "projectPath": "",
                    "installPath": plugin.to_string_lossy()
                }]
            }
        });
        std::fs::write(&json_path, serde_json::to_string(&json).unwrap()).unwrap();

        assert!(read_claude_installed_plugins(&json_path, Some(tmp.path())).is_empty());

        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let json = serde_json::json!({
            "plugins": {
                "has-path@mp": [{
                    "scope": "local",
                    "projectPath": project.to_string_lossy(),
                    "installPath": plugin.to_string_lossy()
                }]
            }
        });
        std::fs::write(&json_path, serde_json::to_string(&json).unwrap()).unwrap();
        assert!(read_claude_installed_plugins(&json_path, None).is_empty());
    }

    #[test]
    fn claude_install_visible_path_boundary_not_string_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        let sibling = tmp.path().join("proj-other");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(claude_install_visible(
            Some("local"),
            Some(project.as_path()),
            Some(project.as_path())
        ));
        assert!(!claude_install_visible(
            Some("local"),
            Some(project.as_path()),
            Some(sibling.as_path())
        ));
    }

    #[test]
    fn claude_project_install_excluded_when_cwd_outside_project() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        let other = tmp.path().join("other");
        let plugin = tmp.path().join("cache").join("team-plug");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(&plugin).unwrap();
        let json_path = tmp.path().join("installed_plugins.json");
        let json = serde_json::json!({
            "plugins": {
                "team-plug@mp": [{
                    "scope": "project",
                    "projectPath": project.to_string_lossy(),
                    "installPath": plugin.to_string_lossy()
                }]
            }
        });
        std::fs::write(&json_path, serde_json::to_string(&json).unwrap()).unwrap();

        assert!(read_claude_installed_plugins(&json_path, Some(&other)).is_empty());
        let results = read_claude_installed_plugins(&json_path, Some(&project));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "team-plug");
    }

    #[test]
    fn claude_entry_with_project_path_gated_even_without_local_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        let other = tmp.path().join("other");
        let plugin = tmp.path().join("cache").join("path-only");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(&plugin).unwrap();
        let json_path = tmp.path().join("installed_plugins.json");
        let json = serde_json::json!({
            "plugins": {
                "path-only@mp": [{
                    "projectPath": project.to_string_lossy(),
                    "installPath": plugin.to_string_lossy()
                }]
            }
        });
        std::fs::write(&json_path, serde_json::to_string(&json).unwrap()).unwrap();

        assert!(read_claude_installed_plugins(&json_path, Some(&other)).is_empty());
        assert_eq!(
            read_claude_installed_plugins(&json_path, Some(&project)).len(),
            1
        );
    }
}
