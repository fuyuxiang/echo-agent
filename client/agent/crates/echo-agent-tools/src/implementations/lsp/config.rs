//! LSP server configuration from `.echo-agent/lsp.json`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 15_000;
pub const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Load LSP servers from user/project config, merge plugin-provided configs, and
/// return the [`ConfigSource`](crate::types::config_source::ConfigSource) of each.
///
/// Plugin configs fill gaps (new server names) but never override user/project config.
/// This is the canonical merge function — both session startup and `echoagent inspect` call it.
/// Accepts both file-based `.lsp.json` paths and inline `lspServers` JSON values
/// from plugin manifests (`plugin.json`).
pub fn load_servers_with_plugins_sourced(
    cwd: &Path,
    plugin_lsp_paths: &[PathBuf],
    plugin_inline_lsp: &[&serde_json::Value],
    plugin_names: &[&str],
    inline_plugin_names: &[&str],
) -> BTreeMap<String, (LspServerConfig, crate::types::config_source::ConfigSource)> {
    use crate::types::config_source::ConfigSource;

    debug_assert!(
        plugin_names.is_empty() || plugin_names.len() == plugin_lsp_paths.len(),
        "plugin_names must be empty or parallel to plugin_lsp_paths"
    );

    let user_path = crate::util::echo_agent_home::echo_agent_home().join("lsp.json");
    let project_path = cwd.join(".echo-agent").join("lsp.json");

    // User-level servers
    let mut servers: BTreeMap<String, (LspServerConfig, ConfigSource)> = load_file(&user_path)
        .into_iter()
        .map(|(name, cfg)| {
            (
                name,
                (
                    cfg,
                    ConfigSource::User {
                        path: user_path.clone(),
                    },
                ),
            )
        })
        .collect();

    // Project-level overrides
    for (name, cfg) in load_file(&project_path) {
        servers.insert(
            name,
            (
                cfg,
                ConfigSource::Project {
                    path: project_path.clone(),
                },
            ),
        );
    }

    // Plugin file-based configs
    for (i, lsp_path) in plugin_lsp_paths.iter().enumerate() {
        let pname = plugin_names.get(i).copied().unwrap_or("unknown");
        for (name, cfg) in load_file(lsp_path) {
            servers.entry(name).or_insert_with(|| {
                (
                    cfg,
                    ConfigSource::Plugin {
                        plugin_name: pname.to_string(),
                        path: lsp_path.clone(),
                    },
                )
            });
        }
    }

    // Plugin inline configs
    for (i, inline) in plugin_inline_lsp.iter().enumerate() {
        let pname = inline_plugin_names.get(i).copied().unwrap_or("unknown");
        match serde_json::from_value::<BTreeMap<String, LspServerConfig>>((*inline).clone()) {
            Ok(parsed) => {
                for (name, cfg) in parsed {
                    servers.entry(name).or_insert_with(|| {
                        (
                            cfg,
                            ConfigSource::Plugin {
                                plugin_name: pname.to_string(),
                                path: PathBuf::new(),
                            },
                        )
                    });
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to parse inline lspServers from plugin manifest");
            }
        }
    }

    servers
}

/// Drop repo-local (project-scoped) LSP servers from a sourced map when the
/// workspace is untrusted; keep user/plugin. Warns per drop. The trust verdict is
/// passed in (the folder-trust engine lives in the shell, out of this crate).
///
/// Single source of truth for the folder-trust LSP load gate, shared by the
/// workspace build path and the shell's per-session gate.
pub fn filter_project_lsp_when_untrusted(
    sourced: BTreeMap<String, (LspServerConfig, crate::types::config_source::ConfigSource)>,
    project_trusted: bool,
) -> BTreeMap<String, LspServerConfig> {
    use crate::types::config_source::ConfigSource;
    sourced
        .into_iter()
        .filter_map(|(name, (cfg, source))| {
            if !project_trusted && matches!(source, ConfigSource::Project { .. }) {
                tracing::warn!(
                    server = %name,
                    "folder untrusted: skipping repo-local (project-scoped) LSP server"
                );
                None
            } else {
                Some((name, cfg))
            }
        })
        .collect()
}

/// Load LSP server configs from `~/.echo-agent/lsp.json` and `<cwd>/.echo-agent/lsp.json`.
/// Project config overrides user config for the same server name.
pub fn load_servers(cwd: &Path) -> BTreeMap<String, LspServerConfig> {
    let user_path = crate::util::echo_agent_home::echo_agent_home().join("lsp.json");
    let project_path = cwd.join(".echo-agent").join("lsp.json");

    let mut merged = load_file(&user_path);
    let project = load_file(&project_path);

    if !merged.is_empty() {
        tracing::info!(
            source = "user",
            path = %user_path.display(),
            servers = ?merged.keys().collect::<Vec<_>>(),
            "loaded user lsp.json"
        );
    }
    if !project.is_empty() {
        tracing::info!(
            source = "project",
            path = %project_path.display(),
            servers = ?project.keys().collect::<Vec<_>>(),
            "loaded project lsp.json"
        );
    }

    for (key, val) in project {
        merged.insert(key, val);
    }

    let mut ext_owners: HashMap<&str, &str> = HashMap::new();
    for (server_name, server_cfg) in &merged {
        for ext in server_cfg.extensions.keys() {
            if let Some(prev) = ext_owners.insert(ext.as_str(), server_name.as_str()) {
                tracing::warn!(
                    extension = ext,
                    server_a = prev,
                    server_b = server_name,
                    "extension claimed by multiple LSP servers; \
                     '{prev}' will handle it (first alphabetically)"
                );
            }
        }
    }

    if merged.is_empty() {
        tracing::info!(
            user = %user_path.display(),
            project = %project_path.display(),
            "no LSP servers configured"
        );
    }
    merged
}

/// Load LSP server configs from a JSON file. Returns empty map on missing/invalid file.
pub fn load_file(path: &Path) -> BTreeMap<String, LspServerConfig> {
    let s = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(e) => {
            tracing::warn!(error = %e,"failed to read lsp.json");
            return BTreeMap::new();
        }
    };

    serde_json::from_str(&s).unwrap_or_else(|e| {
        tracing::warn!(?e, "failed to parse lsp.json");
        BTreeMap::new()
    })
}

/// Resolve which LSP server handles a file based on extension.
pub fn resolve_server(
    servers: &BTreeMap<String, LspServerConfig>,
    path: &Path,
) -> Option<(String, String)> {
    let ext = path.extension()?.to_str()?;
    let dot_ext = format!(".{ext}");
    for (server_name, server_cfg) in servers {
        if let Some(lang_id) = server_cfg.extensions.get(&dot_ext) {
            return Some((server_name.clone(), lang_id.clone()));
        }
    }
    None
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LspTransport {
    #[default]
    Stdio,
    Socket,
}

/// Which solution or projects the server should load once it is running.
///
/// Some servers do not derive their workspace from `rootUri`/`workspaceFolders`
/// and instead load it through a protocol extension. Roslyn is the notable one:
/// left alone it treats every file as a loose "miscellaneous file" and reports
/// no project-level diagnostics at all, until it is sent `solution/open` or
/// `project/open`. Wrappers such as `roslyn-language-server` do this for you; a
/// bare `Microsoft.CodeAnalysis.LanguageServer` does not.
///
/// Paths may be absolute or relative to the workspace root.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceOpen {
    /// A single solution file, sent as `solution/open`.
    #[serde(default)]
    pub solution: Option<String>,
    /// Project files, sent as `project/open`.
    #[serde(default)]
    pub projects: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LspServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub transport: LspTransport,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(
        default,
        alias = "extensionToLanguage",
        alias = "extensionToLanguageId"
    )]
    pub extensions: HashMap<String, String>,
    #[serde(default, alias = "initializationOptions")]
    pub initialization_options: Option<serde_json::Value>,
    #[serde(default)]
    pub settings: Option<serde_json::Value>,
    #[serde(default, alias = "workspaceFolder")]
    pub workspace_folder: Option<String>,
    #[serde(default, alias = "workspaceOpen")]
    pub workspace_open: Option<WorkspaceOpen>,
    #[serde(default, alias = "startupTimeout")]
    pub startup_timeout: Option<u64>,
    #[serde(default, alias = "shutdownTimeout")]
    pub shutdown_timeout: Option<u64>,
    #[serde(default, alias = "restartOnCrash")]
    pub restart_on_crash: Option<bool>,
    #[serde(default, alias = "maxRestarts")]
    pub max_restarts: Option<u32>,
}

impl LspServerConfig {
    pub fn startup_timeout_ms(&self) -> u64 {
        self.startup_timeout.unwrap_or(DEFAULT_STARTUP_TIMEOUT_MS)
    }

    pub fn shutdown_timeout_ms(&self) -> u64 {
        self.shutdown_timeout.unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_MS)
    }

    pub fn restart_on_crash(&self) -> bool {
        self.restart_on_crash.unwrap_or(false)
    }

    /// Maximum restart attempts across the lifetime of a server monitor.
    /// This is a lifetime restart budget, not a per-crash-episode counter.
    pub fn max_restarts(&self) -> u32 {
        self.max_restarts.unwrap_or(3)
    }

    /// The directory this server should treat as its workspace: the per-server
    /// override if there is one, otherwise the session cwd. Everything that
    /// needs to name the server's root — `rootUri`, `workspaceFolders`,
    /// `workspaceOpen` — resolves it here so they cannot drift apart.
    pub fn effective_root<'a>(
        &'a self,
        workspace_root: &'a std::path::Path,
    ) -> &'a std::path::Path {
        self.workspace_folder
            .as_deref()
            .map(std::path::Path::new)
            .unwrap_or(workspace_root)
    }
}

#[cfg(test)]
mod tests {
    use super::{LspServerConfig, filter_project_lsp_when_untrusted};
    use crate::types::config_source::ConfigSource;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn sourced() -> BTreeMap<String, (LspServerConfig, ConfigSource)> {
        let mut m = BTreeMap::new();
        m.insert(
            "proj".to_string(),
            (
                LspServerConfig::default(),
                ConfigSource::Project {
                    path: PathBuf::from("/repo/.echo-agent/lsp.json"),
                },
            ),
        );
        m.insert(
            "usr".to_string(),
            (
                LspServerConfig::default(),
                ConfigSource::User {
                    path: PathBuf::from("/home/.echo-agent/lsp.json"),
                },
            ),
        );
        m.insert(
            "plug".to_string(),
            (
                LspServerConfig::default(),
                ConfigSource::Plugin {
                    plugin_name: "p".to_string(),
                    path: PathBuf::from("/plug/lsp.json"),
                },
            ),
        );
        m
    }

    #[test]
    fn untrusted_drops_only_project_keeps_user_and_plugin() {
        let kept = filter_project_lsp_when_untrusted(sourced(), false);
        assert_eq!(kept.len(), 2);
        assert!(!kept.contains_key("proj"));
        assert!(kept.contains_key("usr"));
        assert!(kept.contains_key("plug"));
    }

    #[test]
    fn trusted_keeps_all_including_project() {
        let kept = filter_project_lsp_when_untrusted(sourced(), true);
        assert_eq!(kept.len(), 3);
        assert!(kept.contains_key("proj"));
        assert!(kept.contains_key("usr"));
        assert!(kept.contains_key("plug"));
    }
}
