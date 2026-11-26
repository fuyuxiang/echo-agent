//! Project plugin trust management.
//!
//! Plugins from project directories (`.echo-agent/plugins/`, `.claude/plugins/`)
//! are an execution surface.  A cloned repository could contain plugins with
//! hook scripts or MCP server commands that run arbitrary code.
//!
//! **Trust granularity**: per-plugin-root (not per-worktree).  Trusting one
//! plugin in a repo does not automatically trust other plugins in the same repo.
//!
//! **Trust key**: canonical absolute path of the plugin root directory,
//! resolved via `dunce::canonicalize()`.
//!
//! **Trust storage**: `~/.echo-agent/trusted-plugins` (one canonical path per line).
//!
//! **Behavior for untrusted plugins**:
//! - Skills and agents are **discovered and listed** (metadata-only).
//! - Hooks, MCP servers, and scripts are **blocked**.

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Name of the trust-store file under `~/.echo-agent/`.
const TRUST_FILE_NAME: &str = "trusted-plugins";

/// Manages the set of trusted plugin root directories.
#[derive(Debug, Clone)]
pub struct TrustStore {
    /// Canonical paths of trusted plugin roots.
    trusted: HashSet<PathBuf>,
    /// Path to the trust-store file on disk.
    file_path: PathBuf,
}

impl TrustStore {
    /// Load the trust store from disk.
    ///
    /// If `~/.echo-agent/trusted-plugins` does not exist, returns an empty store.
    /// If the file cannot be read, logs a warning and returns an empty store.
    pub fn load() -> Self {
        // Gate on user_echo_agent_home() so a project's `.echo-agent/trusted-plugins` is never
        // read as the user trust store when neither ECHO_AGENT_HOME nor a home dir resolves.
        let Some(echoagent) = echo_agent_config::user_echo_agent_home() else {
            return Self {
                trusted: HashSet::new(),
                file_path: PathBuf::new(),
            };
        };
        let file_path = echoagent.join(TRUST_FILE_NAME);
        let trusted = Self::read_trust_file(&file_path);
        Self { trusted, file_path }
    }

    /// Load from a custom file path (for testing).
    pub fn load_from(file_path: PathBuf) -> Self {
        let trusted = Self::read_trust_file(&file_path);
        Self { trusted, file_path }
    }

    /// Check whether a plugin root directory is trusted.
    ///
    /// Canonicalizes the path before lookup.  Returns `false` if
    /// canonicalization fails (broken symlink, permission error).
    pub fn is_trusted(&self, plugin_root: &Path) -> bool {
        match dunce::canonicalize(plugin_root) {
            Ok(canonical) => self.trusted.contains(&canonical),
            Err(_) => {
                tracing::warn!(
                    path = %plugin_root.display(),
                    "failed to canonicalize plugin root for trust check; treating as untrusted"
                );
                false
            }
        }
    }

    /// Grant trust to a plugin root directory.
    ///
    /// Canonicalizes the path and appends it to `~/.echo-agent/trusted-plugins`.
    /// If the path is already trusted, this is a no-op and returns `Ok(())`.
    pub fn grant_trust(&mut self, plugin_root: &Path) -> Result<(), TrustError> {
        let canonical =
            dunce::canonicalize(plugin_root).map_err(|e| TrustError::CanonicalizeFailed {
                path: plugin_root.to_path_buf(),
                source: e,
            })?;

        if self.trusted.contains(&canonical) {
            return Ok(());
        }

        // Ensure parent directory exists
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| TrustError::IoError {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        // Append to file
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
            .map_err(|e| TrustError::IoError {
                path: self.file_path.clone(),
                source: e,
            })?;

        writeln!(file, "{}", canonical.display()).map_err(|e| TrustError::IoError {
            path: self.file_path.clone(),
            source: e,
        })?;

        self.trusted.insert(canonical);
        Ok(())
    }

    /// Revoke trust for a plugin root directory.
    ///
    /// Canonicalizes the path, removes it from the in-memory set, and
    /// rewrites `~/.echo-agent/trusted-plugins` without the revoked entry.
    /// If the path is not currently trusted, this is a no-op.
    pub fn revoke_trust(&mut self, plugin_root: &Path) -> Result<(), TrustError> {
        let canonical =
            dunce::canonicalize(plugin_root).map_err(|e| TrustError::CanonicalizeFailed {
                path: plugin_root.to_path_buf(),
                source: e,
            })?;

        if !self.trusted.remove(&canonical) {
            return Ok(()); // wasn't trusted
        }

        // Rewrite the entire file without the revoked path
        self.rewrite_trust_file()
    }

    /// Rewrite the trust file from the current in-memory set.
    fn rewrite_trust_file(&self) -> Result<(), TrustError> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| TrustError::IoError {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        let mut file = std::fs::File::create(&self.file_path).map_err(|e| TrustError::IoError {
            path: self.file_path.clone(),
            source: e,
        })?;

        use std::io::Write;
        for path in &self.trusted {
            writeln!(file, "{}", path.display()).map_err(|e| TrustError::IoError {
                path: self.file_path.clone(),
                source: e,
            })?;
        }

        Ok(())
    }

    /// Check whether a config-path plugin should be auto-trusted.
    ///
    /// A `[plugins].paths` entry is auto-trusted if its canonicalized path
    /// is under the user's home directory.  Otherwise it requires explicit
    /// trust via `~/.echo-agent/trusted-plugins`.
    pub fn is_config_path_auto_trusted(plugin_root: &Path) -> bool {
        let Some(home) = echo_agent_config::discovery_home_dir() else {
            return false;
        };
        match dunce::canonicalize(plugin_root) {
            Ok(canonical) => canonical.starts_with(&home),
            Err(_) => false,
        }
    }

    // ── Internal ──────────────────────────────────────────────────────

    fn read_trust_file(path: &Path) -> HashSet<PathBuf> {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HashSet::new(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read trust store; no plugins will be trusted"
                );
                return HashSet::new();
            }
        };

        let reader = std::io::BufReader::new(file);
        reader
            .lines()
            .filter_map(|line| {
                let line = line.ok()?;
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    return None;
                }
                // Entries may predate dunce (Windows \\?\ verbatim form); simplify so lookups match.
                Some(dunce::simplified(Path::new(trimmed)).to_path_buf())
            })
            .collect()
    }
}

// ── Errors ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("failed to canonicalize path {path}: {source}")]
    CanonicalizeFailed {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("I/O error on {path}: {source}")]
    IoError {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_trust_store() {
        let tmp = tempfile::tempdir().unwrap();
        let trust_file = tmp.path().join("trusted-plugins");
        let store = TrustStore::load_from(trust_file);
        // Nothing is trusted
        assert!(!store.is_trusted(tmp.path()));
    }

    #[test]
    fn grant_and_check_trust() {
        let tmp = tempfile::tempdir().unwrap();
        let trust_file = tmp.path().join("trusted-plugins");

        let plugin_dir = tmp.path().join("my-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        let mut store = TrustStore::load_from(trust_file.clone());
        assert!(!store.is_trusted(&plugin_dir));

        store.grant_trust(&plugin_dir).unwrap();
        assert!(store.is_trusted(&plugin_dir));

        // Granting again is a no-op
        store.grant_trust(&plugin_dir).unwrap();

        // Reload from disk and verify persistence
        let reloaded = TrustStore::load_from(trust_file);
        assert!(reloaded.is_trusted(&plugin_dir));
    }

    #[test]
    fn trust_file_skips_comments_and_blanks() {
        let tmp = tempfile::tempdir().unwrap();
        let trust_file = tmp.path().join("trusted-plugins");

        let plugin_dir = tmp.path().join("real-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let canonical = dunce::canonicalize(&plugin_dir).unwrap();

        // Write file with comments and blank lines
        std::fs::write(
            &trust_file,
            format!(
                "# This is a comment\n\n{}\n  \n# Another comment\n",
                canonical.display()
            ),
        )
        .unwrap();

        let store = TrustStore::load_from(trust_file);
        assert!(store.is_trusted(&plugin_dir));
    }

    /// Legacy entries written under std canonicalize use the verbatim `\\?\`
    /// form; `read_trust_file` must normalize them so lookups keep matching.
    #[cfg(windows)]
    #[test]
    fn legacy_verbatim_entry_is_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let trust_file = tmp.path().join("trusted-plugins");

        let plugin_dir = tmp.path().join("legacy-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let canonical = dunce::canonicalize(&plugin_dir).unwrap();
        std::fs::write(&trust_file, format!("\\\\?\\{}\n", canonical.display())).unwrap();

        let mut store = TrustStore::load_from(trust_file.clone());
        assert!(store.is_trusted(&plugin_dir));

        // Revoke rewrites the file in simplified form, dropping the legacy line.
        store.revoke_trust(&plugin_dir).unwrap();
        assert!(!TrustStore::load_from(trust_file).is_trusted(&plugin_dir));
    }

    #[test]
    fn nonexistent_path_is_not_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let trust_file = tmp.path().join("trusted-plugins");
        let store = TrustStore::load_from(trust_file);

        // Path that doesn't exist on disk
        let fake = tmp.path().join("does-not-exist");
        assert!(!store.is_trusted(&fake));
    }

    #[test]
    fn config_path_auto_trust_under_home() {
        // This test checks the logic but can't easily mock $HOME.
        // We verify the function exists and returns a boolean.
        let result = TrustStore::is_config_path_auto_trusted(Path::new("/nonexistent/path"));
        assert!(!result); // nonexistent path can't be canonicalized
    }

    #[test]
    fn revoke_trust_removes_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        let trust_file = tmp.path().join("trusted-plugins");

        let plugin_a = tmp.path().join("plugin-a");
        let plugin_b = tmp.path().join("plugin-b");
        std::fs::create_dir_all(&plugin_a).unwrap();
        std::fs::create_dir_all(&plugin_b).unwrap();

        let mut store = TrustStore::load_from(trust_file.clone());
        store.grant_trust(&plugin_a).unwrap();
        store.grant_trust(&plugin_b).unwrap();
        assert!(store.is_trusted(&plugin_a));
        assert!(store.is_trusted(&plugin_b));

        // Revoke plugin_a
        store.revoke_trust(&plugin_a).unwrap();
        assert!(!store.is_trusted(&plugin_a));
        assert!(store.is_trusted(&plugin_b));

        // Verify persistence
        let reloaded = TrustStore::load_from(trust_file);
        assert!(!reloaded.is_trusted(&plugin_a));
        assert!(reloaded.is_trusted(&plugin_b));
    }

    #[test]
    fn revoke_trust_noop_if_not_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let trust_file = tmp.path().join("trusted-plugins");
        let plugin = tmp.path().join("some-plugin");
        std::fs::create_dir_all(&plugin).unwrap();

        let mut store = TrustStore::load_from(trust_file);
        // Not trusted — revoke should be a no-op
        store.revoke_trust(&plugin).unwrap();
        assert!(!store.is_trusted(&plugin));
    }
}
