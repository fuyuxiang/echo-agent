//! Install registry for managing plugins installed from git repos or local directories.
//!
//! Tracks which repos have been cloned/symlinked into the managed install directory,
//! along with the plugins discovered within each repo.
//!
//! The registry is persisted as `registry.json` in the install directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default install directory name under `~/.echo-agent/`.
const DEFAULT_INSTALL_DIR_NAME: &str = "installed-plugins";

/// Registry of installed repos and their plugins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRegistry {
    /// Schema version for forward compatibility.
    pub version: u32,
    /// Installed repos, keyed by repo key (`<basename>-<hash8>`).
    pub repos: HashMap<String, InstalledRepo>,
    /// Absolute path to the install directory.
    #[serde(skip)]
    install_dir: PathBuf,
}

/// How a repo was installed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum InstallKind {
    /// Cloned from a remote git repo.
    Git {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        git_ref: Option<String>,
        commit: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subdir: Option<String>,
    },
    /// Copied from a local directory (full tree snapshot under installed-plugins).
    Local {
        source_path: PathBuf,
        /// Optional plugin subdirectory selector used at install time (e.g.
        /// multi-package `path#plugins/foo`). Preserved so refresh rediscovers
        /// the same scope.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subdir: Option<String>,
    },
}

/// A single installed repo, which may contain one or more plugins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledRepo {
    pub kind: InstallKind,
    pub installed_at: String,
    pub updated_at: String,
    /// Absolute path to the repo directory (or symlink) in the install dir.
    pub path: PathBuf,
    /// Plugins discovered within this repo.
    pub plugins: HashMap<String, RepoPlugin>,
    /// Marketplace provenance (None for non-marketplace installs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace: Option<MarketplaceProvenance>,
}

/// Marketplace provenance — tracks which marketplace a plugin was installed from.
/// Lives here (not in echo-agent-plugin-marketplace) to keep dependency direction sane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceProvenance {
    /// Public source reference (git URL or local path, possibly a local alias).
    pub source_url_or_path: String,
    /// Identity captured when copying/installing the plugin, independent of
    /// later local symlink changes. Absent in older registry records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_source_identity: Option<String>,
    /// User-facing source name (display only, not used for matching).
    pub source_display_name: String,
    /// Plugin subdirectory within marketplace (e.g., "plugins/echo-agent-code-review").
    pub plugin_subdir: String,
}

impl MarketplaceProvenance {
    pub fn identity_with_aliases(
        &self,
        aliases: &std::collections::BTreeMap<String, String>,
    ) -> String {
        use super::marketplace_scope::{source_identity, stored_source_identity};
        if let Some(identity) = &self.canonical_source_identity {
            return stored_source_identity(identity);
        }
        // A legacy copied snapshot must retain the owner captured at revocation,
        // even if the live catalog's same alias subsequently points elsewhere.
        aliases
            .get(&stored_source_identity(&self.source_url_or_path))
            .map(|identity| stored_source_identity(identity))
            .unwrap_or_else(|| source_identity(&self.source_url_or_path))
    }
}

/// A plugin discovered within an installed repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoPlugin {
    /// Subdirectory within the repo (None if plugin is at repo root).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    /// Plugin version from manifest (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

fn paths_match_plugin_root(
    installed_plugin_root: &Path,
    plugin_root: &Path,
    plugin_canonical_root: &Path,
) -> bool {
    installed_plugin_root == plugin_root
        || installed_plugin_root == plugin_canonical_root
        || dunce::canonicalize(installed_plugin_root)
            .ok()
            .is_some_and(|canonical| canonical == plugin_root || canonical == plugin_canonical_root)
}

impl InstallRegistry {
    /// Load the registry from the resolved install directory.
    ///
    /// If the registry file doesn't exist, returns an empty registry.
    pub fn load() -> Self {
        Self::load_from(Self::resolve_install_dir())
    }

    /// Load the registry from an explicit install directory.
    ///
    /// Missing file → empty registry. Read/parse errors → empty registry after a warning.
    pub fn load_from(install_dir: PathBuf) -> Self {
        match Self::try_load_from(install_dir.clone()) {
            Ok(reg) => reg,
            Err(e) => {
                tracing::warn!(
                    path = %install_dir.join("registry.json").display(),
                    error = %e,
                    "failed to load install registry; starting fresh"
                );
                Self::empty(install_dir)
            }
        }
    }

    /// Fallible load: missing `registry.json` is empty; read/parse errors are `Err`.
    pub fn try_load_from(install_dir: PathBuf) -> Result<Self, InstallError> {
        let registry_path = install_dir.join("registry.json");
        match std::fs::read_to_string(&registry_path) {
            Ok(content) => {
                let mut reg: InstallRegistry =
                    serde_json::from_str(&content).map_err(|e| InstallError::Json {
                        detail: e.to_string(),
                    })?;
                reg.install_dir = install_dir;
                Ok(reg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty(install_dir)),
            Err(e) => Err(InstallError::Io {
                path: registry_path,
                source: e,
            }),
        }
    }

    /// Create an empty registry for the given install directory.
    pub fn empty(install_dir: PathBuf) -> Self {
        Self {
            version: 1,
            repos: HashMap::new(),
            install_dir,
        }
    }

    /// Save the registry to disk.
    pub fn save(&self) -> Result<(), InstallError> {
        self.save_atomic()
    }

    pub fn save_atomic(&self) -> Result<(), InstallError> {
        std::fs::create_dir_all(&self.install_dir).map_err(|e| InstallError::Io {
            path: self.install_dir.clone(),
            source: e,
        })?;

        let registry_path = self.install_dir.join("registry.json");
        let content = serde_json::to_string_pretty(self).map_err(|e| InstallError::Json {
            detail: e.to_string(),
        })?;
        if std::env::var_os("ECHO_AGENT_TEST_FAIL_REGISTRY_SAVE_AFTER_SERIALIZE").is_some() {
            return Err(InstallError::InstallFailed {
                detail: "test-injected registry save failure".into(),
            });
        }
        let temp_path = self.install_dir.join(format!(
            ".registry.json.tmp-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(&temp_path, content).map_err(|e| InstallError::Io {
            path: temp_path.clone(),
            source: e,
        })?;

        if let Err(e) = std::fs::rename(&temp_path, &registry_path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(InstallError::Io {
                path: registry_path,
                source: e,
            });
        }

        Ok(())
    }

    /// Get a repo by its repo key.
    pub fn get_repo(&self, repo_key: &str) -> Option<&InstalledRepo> {
        self.repos.get(repo_key)
    }

    /// Get a mutable reference to a repo by its repo key.
    pub fn get_repo_mut(&mut self, repo_key: &str) -> Option<&mut InstalledRepo> {
        self.repos.get_mut(repo_key)
    }

    /// Find which repo a plugin belongs to.
    ///
    /// Returns `(repo_key, repo, plugin)` if found.
    pub fn find_plugin(&self, plugin_name: &str) -> Option<(&str, &InstalledRepo, &RepoPlugin)> {
        for (repo_key, repo) in &self.repos {
            if let Some(plugin) = repo.plugins.get(plugin_name) {
                return Some((repo_key, repo, plugin));
            }
        }
        None
    }

    pub fn find_repo_key_by_plugin_root(
        &self,
        plugin_root: &Path,
        plugin_canonical_root: &Path,
    ) -> Option<&str> {
        self.list().into_iter().find_map(|(repo_key, repo)| {
            repo.plugins.values().find_map(|plugin| {
                let installed_plugin_root = match plugin.subdir.as_deref() {
                    Some(subdir) => repo.path.join(subdir),
                    None => repo.path.clone(),
                };
                paths_match_plugin_root(&installed_plugin_root, plugin_root, plugin_canonical_root)
                    .then_some(repo_key)
            })
        })
    }

    /// Insert a repo into the registry.
    pub fn insert(&mut self, repo_key: String, repo: InstalledRepo) {
        self.repos.insert(repo_key, repo);
    }

    /// Remove a repo from the registry.
    pub fn remove(&mut self, repo_key: &str) -> Option<InstalledRepo> {
        self.repos.remove(repo_key)
    }

    /// List all installed repos.
    pub fn list(&self) -> Vec<(&str, &InstalledRepo)> {
        let mut entries: Vec<_> = self.repos.iter().map(|(k, v)| (k.as_str(), v)).collect();
        entries.sort_by_key(|(k, _)| *k);
        entries
    }

    /// Get the install directory path.
    pub fn install_dir(&self) -> &Path {
        &self.install_dir
    }

    /// Resolve the install directory from config or default.
    ///
    /// Resolution order:
    /// 1. `[plugins].install_dir` from effective config (requirements > config > managed)
    /// 2. Default: `~/.echo-agent/installed-plugins/`
    pub fn resolve_install_dir() -> PathBuf {
        if let Some(dir) = Self::read_install_dir_from_config() {
            return dir;
        }

        echo_agent_config::echo_agent_home().join(DEFAULT_INSTALL_DIR_NAME)
    }

    /// Read `[plugins].install_dir` from the effective config
    /// (managed_config.toml merged under config.toml — user wins).
    fn read_install_dir_from_config() -> Option<PathBuf> {
        let root = echo_agent_config::load_effective_config_disk_only().ok()?;
        let value = root.get("plugins")?.get("install_dir")?.as_str()?;
        let expanded = if let Some(stripped) = value.strip_prefix("~/") {
            echo_agent_config::discovery_home_dir()?.join(stripped)
        } else {
            PathBuf::from(value)
        };
        Some(expanded)
    }

    /// Generate a unique repo key from a source identifier.
    ///
    /// Format: `<basename>-<hash8>` where hash8 = first 8 hex chars of
    /// SHA-256(normalized source).
    ///
    /// Examples:
    /// - `https://github.com/org-a/tools` → `tools-a1b2c3d4`
    /// - `/Users/me/projects/my-plugin` → `my-plugin-e5f6g7h8`
    pub fn repo_key(source: &str) -> String {
        let basename = source
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .rsplit('/')
            .next()
            .unwrap_or("plugin");

        // Sanitize basename to kebab-case
        let sanitized: String = basename
            .to_ascii_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let trimmed = sanitized.trim_matches('-');

        // Hash the full source for uniqueness
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut hasher);
        let hash = hasher.finish();
        let hash8 = format!("{:08x}", hash & 0xFFFFFFFF);

        format!("{trimmed}-{hash8}")
    }
}

// ── Errors ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("JSON error: {detail}")]
    Json { detail: String },

    #[error("plugin '{name}' not found in install registry")]
    PluginNotFound { name: String },

    #[error("repo '{key}' already installed")]
    AlreadyInstalled { key: String },

    #[error("SHA verification failed: expected {expected}, got {actual}")]
    ShaMismatch { expected: String, actual: String },

    #[error(
        "refusing unpinned remote plugin code for '{plugin}' from {url}: \
         marketplace.require_sha / ECHO_AGENT_MARKETPLACE_REQUIRE_SHA is enabled and \
         no full commit sha (40/64 hex) is pinned"
    )]
    UnpinnedRemoteRefused { plugin: String, url: String },

    #[error("install failed: {detail}")]
    InstallFailed { detail: String },
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_key_from_https_url() {
        let key = InstallRegistry::repo_key("https://github.com/user/my-linter");
        assert!(key.starts_with("my-linter-"));
        assert_eq!(key.len(), "my-linter-".len() + 8);
    }

    #[test]
    fn repo_key_from_ssh_url() {
        let key = InstallRegistry::repo_key("git@github.com:user/my-plugin.git");
        assert!(key.starts_with("my-plugin-"));
    }

    #[test]
    fn repo_key_from_local_path() {
        let key = InstallRegistry::repo_key("/Users/me/projects/my-tools");
        assert!(key.starts_with("my-tools-"));
    }

    #[test]
    fn repo_key_collision_safety() {
        let key_a = InstallRegistry::repo_key("https://github.com/org-a/tools");
        let key_b = InstallRegistry::repo_key("https://github.com/org-b/tools");
        assert_ne!(
            key_a, key_b,
            "different sources should produce different keys"
        );
        assert!(key_a.starts_with("tools-"));
        assert!(key_b.starts_with("tools-"));
    }

    #[test]
    fn empty_registry_crud() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = InstallRegistry::empty(tmp.path().to_path_buf());
        assert!(reg.repos.is_empty());
        assert!(reg.list().is_empty());

        // Insert
        reg.insert(
            "test-repo-12345678".to_string(),
            InstalledRepo {
                kind: InstallKind::Git {
                    url: "https://github.com/user/test".to_string(),
                    git_ref: Some("main".to_string()),
                    commit: "abc123".to_string(),
                    subdir: None,
                },
                installed_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                path: tmp.path().join("test-repo-12345678"),
                plugins: HashMap::from([(
                    "my-plugin".to_string(),
                    RepoPlugin {
                        subdir: None,
                        version: Some("1.0.0".to_string()),
                    },
                )]),
                marketplace: None,
            },
        );

        assert_eq!(reg.repos.len(), 1);
        assert!(reg.get_repo("test-repo-12345678").is_some());
        assert!(reg.find_plugin("my-plugin").is_some());
        assert!(reg.find_plugin("nonexistent").is_none());

        // Save and reload
        reg.save().unwrap();
        let registry_path = tmp.path().join("registry.json");
        assert!(registry_path.exists());

        // Remove
        let removed = reg.remove("test-repo-12345678");
        assert!(removed.is_some());
        assert!(reg.repos.is_empty());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = InstallRegistry::empty(tmp.path().to_path_buf());

        reg.insert(
            "my-linter-aabbccdd".to_string(),
            InstalledRepo {
                kind: InstallKind::Local {
                    source_path: PathBuf::from("/home/user/plugins/linter"),
                    subdir: None,
                },
                installed_at: "2026-03-26T12:00:00Z".to_string(),
                updated_at: "2026-03-26T12:00:00Z".to_string(),
                path: tmp.path().join("my-linter-aabbccdd"),
                plugins: HashMap::from([
                    (
                        "lint-check".to_string(),
                        RepoPlugin {
                            subdir: Some("lint-check".to_string()),
                            version: None,
                        },
                    ),
                    (
                        "lint-fix".to_string(),
                        RepoPlugin {
                            subdir: Some("lint-fix".to_string()),
                            version: Some("2.0.0".to_string()),
                        },
                    ),
                ]),
                marketplace: None,
            },
        );

        reg.save().unwrap();

        // Read the JSON back and parse
        let content = std::fs::read_to_string(tmp.path().join("registry.json")).unwrap();
        let loaded: InstallRegistry = serde_json::from_str(&content).unwrap();

        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.repos.len(), 1);
        let repo = loaded.get_repo("my-linter-aabbccdd").unwrap();
        assert_eq!(repo.plugins.len(), 2);
        assert!(repo.plugins.contains_key("lint-check"));
        assert!(repo.plugins.contains_key("lint-fix"));
    }

    #[test]
    fn find_plugin_across_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = InstallRegistry::empty(tmp.path().to_path_buf());

        reg.insert(
            "repo-a-11111111".to_string(),
            InstalledRepo {
                kind: InstallKind::Git {
                    url: "https://example.com/a".to_string(),
                    git_ref: None,
                    commit: "aaa".to_string(),
                    subdir: None,
                },
                installed_at: String::new(),
                updated_at: String::new(),
                path: tmp.path().join("repo-a-11111111"),
                plugins: HashMap::from([(
                    "alpha".to_string(),
                    RepoPlugin {
                        subdir: None,
                        version: None,
                    },
                )]),
                marketplace: None,
            },
        );

        reg.insert(
            "repo-b-22222222".to_string(),
            InstalledRepo {
                kind: InstallKind::Git {
                    url: "https://example.com/b".to_string(),
                    git_ref: None,
                    commit: "bbb".to_string(),
                    subdir: None,
                },
                installed_at: String::new(),
                updated_at: String::new(),
                path: tmp.path().join("repo-b-22222222"),
                plugins: HashMap::from([(
                    "beta".to_string(),
                    RepoPlugin {
                        subdir: Some("beta".to_string()),
                        version: None,
                    },
                )]),
                marketplace: None,
            },
        );

        let (key, _, _) = reg.find_plugin("alpha").unwrap();
        assert_eq!(key, "repo-a-11111111");

        let (key, _, plugin) = reg.find_plugin("beta").unwrap();
        assert_eq!(key, "repo-b-22222222");
        assert_eq!(plugin.subdir.as_deref(), Some("beta"));

        assert!(reg.find_plugin("gamma").is_none());
    }

    #[test]
    fn find_repo_key_by_plugin_root_handles_subdir_plugins() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo-a-11111111");
        let plugin_root = repo_root.join("plugins").join("nested");
        std::fs::create_dir_all(&plugin_root).unwrap();
        let canonical_plugin_root = dunce::canonicalize(&plugin_root).unwrap();
        let mut reg = InstallRegistry::empty(tmp.path().to_path_buf());
        reg.insert(
            "repo-a-11111111".to_string(),
            InstalledRepo {
                kind: InstallKind::Local {
                    source_path: repo_root.clone(),
                    subdir: None,
                },
                installed_at: String::new(),
                updated_at: String::new(),
                path: repo_root,
                plugins: HashMap::from([(
                    "nested".to_string(),
                    RepoPlugin {
                        subdir: Some("plugins/nested".to_string()),
                        version: None,
                    },
                )]),
                marketplace: None,
            },
        );

        assert_eq!(
            reg.find_repo_key_by_plugin_root(&plugin_root, &canonical_plugin_root),
            Some("repo-a-11111111")
        );
    }

    #[test]
    fn git_kind_without_subdir_field_deserializes_to_none() {
        let json = r#"{"type":"Git","url":"https://example.com/r","commit":"abc"}"#;
        let kind: InstallKind = serde_json::from_str(json).unwrap();
        match kind {
            InstallKind::Git { url, subdir, .. } => {
                assert_eq!(url, "https://example.com/r");
                assert!(subdir.is_none());
            }
            _ => panic!("expected Git"),
        }
    }

    #[test]
    fn local_kind_without_subdir_field_deserializes_to_none() {
        let json = r#"{"type":"Local","source_path":"/home/user/plugin"}"#;
        let kind: InstallKind = serde_json::from_str(json).unwrap();
        match kind {
            InstallKind::Local {
                source_path,
                subdir,
            } => {
                assert_eq!(source_path, PathBuf::from("/home/user/plugin"));
                assert!(subdir.is_none());
            }
            _ => panic!("expected Local"),
        }
    }

    #[test]
    fn local_kind_with_subdir_round_trips() {
        let kind = InstallKind::Local {
            source_path: PathBuf::from("/home/user/workspace"),
            subdir: Some("plugins/foo".to_string()),
        };
        let json = serde_json::to_string(&kind).unwrap();
        let back: InstallKind = serde_json::from_str(&json).unwrap();
        match back {
            InstallKind::Local { subdir, .. } => {
                assert_eq!(subdir.as_deref(), Some("plugins/foo"));
            }
            _ => panic!("expected Local"),
        }
    }
}
