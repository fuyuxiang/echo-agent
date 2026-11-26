// claude_import_state.rs
// Tracks what Claude settings have been imported/dismissed so we don't re-prompt.
//
// State is persisted to `~/.echo-agent/claude_import_state.json`.
// Hash is SHA-256 over sorted, concatenated contents of all Claude settings
// files at a given scope (global or project).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use echo_agent_workspace::permission::claude_settings::find_claude_settings_paths;

// Types

/// Persistent import state, loaded from / saved to `~/.echo-agent/claude_import_state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ImportState {
    /// Schema version for forward compatibility.
    pub version: u32,
    /// Hash of global Claude settings (`~/.claude/settings*.json`, `~/.claude.json`).
    #[serde(default)]
    pub global: Option<ScopeState>,
    /// Per-project hashes, keyed by canonical project root path.
    #[serde(default)]
    pub projects: HashMap<String, ScopeState>,
}

/// Import state for a single scope (global or one project).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScopeState {
    /// SHA-256 hex digest of the concatenated Claude settings file contents.
    pub last_hash: String,
    /// RFC 3339 timestamp of when the hash was last recorded.
    pub last_checked: String,
}

impl Default for ImportState {
    fn default() -> Self {
        Self {
            version: 1,
            global: None,
            projects: HashMap::new(),
        }
    }
}

// Persistence

/// Path to the import state file.
fn state_path() -> PathBuf {
    crate::util::echo_agent_home::echo_agent_home().join("claude_import_state.json")
}

/// Load the import state from disk. Returns default if missing or unreadable.
pub(crate) fn load_import_state() -> ImportState {
    let path = state_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!(
                path = %path.display(),
                error = %e,
                "Failed to parse claude_import_state.json, using default"
            );
            ImportState::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ImportState::default(),
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                "Failed to read claude_import_state.json, using default"
            );
            ImportState::default()
        }
    }
}

/// Save the import state to disk (atomic write via tmp + rename).
pub(crate) fn save_import_state(state: &ImportState) -> std::io::Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    // `.with_extension("json.tmp")` replaces `.json` → produces
    // `claude_import_state.json.tmp` (the last extension is replaced).
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// Hash Computation

/// Compute a SHA-256 hash over the contents of all Claude settings files for a
/// given set of paths. Files that don't exist or can't be read are skipped.
///
/// Paths are sorted before hashing so the result is deterministic regardless of
/// discovery order.
fn compute_settings_hash(paths: &[PathBuf]) -> String {
    let mut existing: Vec<(&PathBuf, Vec<u8>)> = paths
        .iter()
        .filter_map(|p| std::fs::read(p).ok().map(|content| (p, content)))
        .collect();

    // Sort by path for determinism.
    existing.sort_by(|a, b| a.0.cmp(b.0));

    let mut hasher = Sha256::new();
    for (path, content) in &existing {
        // Include path in the hash so renaming a file changes the hash.
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(b"\x00");
        hasher.update(content);
        hasher.update(b"\x00");
    }

    format!("sha256:{:x}", hasher.finalize())
}

/// Compute hash for global Claude settings (`~/.claude/settings*.json`, `~/.claude.json`).
///
/// Uses `echo_agent_config::discovery_home_dir()` to match the home directory resolution used by
/// `load_claude_json_mcp_servers_as_configs()` in `util/config.rs`.
fn compute_global_hash() -> (String, Vec<PathBuf>) {
    let mut paths = Vec::new();
    if let Some(home) = echo_agent_config::discovery_home_dir() {
        paths.push(home.join(".claude").join("settings.json"));
        paths.push(home.join(".claude").join("settings.local.json"));
        paths.push(home.join(".claude.json"));
    }
    let hash = compute_settings_hash(&paths);
    (hash, paths)
}

/// Compute hash for project-level Claude settings.
///
/// Uses `echo_agent_config::discovery_home_dir()` to match the home directory resolution used by
/// the scanner in `claude_import.rs`.
fn compute_project_hash(cwd: &Path) -> (String, Vec<PathBuf>) {
    // Use find_claude_settings_paths but filter to only project-level paths
    // (exclude global ~/.claude/ paths).
    let all_paths = find_claude_settings_paths(cwd);
    let home = echo_agent_config::discovery_home_dir();

    let project_paths: Vec<PathBuf> = all_paths
        .into_iter()
        .filter(|p| {
            // Exclude global paths (under ~/.claude/).
            if let Some(ref h) = home {
                !p.starts_with(h.join(".claude"))
            } else {
                true
            }
        })
        .collect();

    // Also include .mcp.json candidate paths from cwd up to repo root.
    // Non-existent files are skipped by compute_settings_hash(), so we
    // unconditionally add candidates (avoids TOCTOU race vs .exists()).
    let mut all = project_paths;
    let mut current = cwd.to_path_buf();
    loop {
        all.push(current.join(".mcp.json"));
        if current.join(".git").exists() {
            break;
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }

    let hash = compute_settings_hash(&all);
    (hash, all)
}

// Change Detection

// State Updates

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Record the current hash for both global and project scopes.
///
/// Called after a successful import or explicit dismiss.
pub fn mark_imported(cwd: &Path) {
    let mut state = load_import_state();

    let (global_hash, _) = compute_global_hash();
    state.global = Some(ScopeState {
        last_hash: global_hash,
        last_checked: now_rfc3339(),
    });

    let (project_hash, _) = compute_project_hash(cwd);
    let cwd_key = cwd.to_string_lossy().to_string();
    state.projects.insert(
        cwd_key,
        ScopeState {
            last_hash: project_hash,
            last_checked: now_rfc3339(),
        },
    );

    if let Err(e) = save_import_state(&state) {
        warn!(error = %e, "Failed to save claude_import_state.json");
    }
}

/// Alias for `mark_imported` — dismissing records the same hash so we don't
/// re-prompt until the files actually change.
pub fn mark_dismissed(cwd: &Path) {
    mark_imported(cwd);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_settings_hash_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.json");
        let f2 = dir.path().join("b.json");
        std::fs::write(&f1, r#"{"allow": ["Bash"]}"#).unwrap();
        std::fs::write(&f2, r#"{"env": {"FOO": "bar"}}"#).unwrap();

        // Same order.
        let h1 = compute_settings_hash(&[f1.clone(), f2.clone()]);
        let h2 = compute_settings_hash(&[f1.clone(), f2.clone()]);
        assert_eq!(h1, h2, "same order should produce same hash");

        // Reversed order should also produce the same hash (sorted internally).
        let h3 = compute_settings_hash(&[f2.clone(), f1.clone()]);
        assert_eq!(
            h1, h3,
            "reversed order should produce same hash due to sorting"
        );
    }

    #[test]
    fn compute_settings_hash_skips_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("exists.json");
        let missing = dir.path().join("does_not_exist.json");
        std::fs::write(&existing, "content").unwrap();

        let h1 = compute_settings_hash(std::slice::from_ref(&existing));
        let h2 = compute_settings_hash(&[existing.clone(), missing]);
        assert_eq!(h1, h2, "missing files should be skipped");
    }

    #[test]
    fn compute_settings_hash_changes_on_content_change() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("settings.json");

        std::fs::write(&f, "version1").unwrap();
        let h1 = compute_settings_hash(std::slice::from_ref(&f));

        std::fs::write(&f, "version2").unwrap();
        let h2 = compute_settings_hash(std::slice::from_ref(&f));

        assert_ne!(h1, h2, "different content should produce different hash");
    }

    #[test]
    fn compute_settings_hash_empty_input() {
        // No paths at all should produce a deterministic hash.
        let h1 = compute_settings_hash(&[]);
        let h2 = compute_settings_hash(&[]);
        assert_eq!(h1, h2, "empty input should produce same hash");
        assert!(h1.starts_with("sha256:"), "hash should have sha256: prefix");
    }
}
