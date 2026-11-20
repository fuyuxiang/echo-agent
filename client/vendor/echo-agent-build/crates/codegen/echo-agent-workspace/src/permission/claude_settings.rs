//! Reads and parses `.claude/settings.json` (vendor settings interop).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{debug, warn};

use crate::permission::rules::parse_permission_rule;
use crate::permission::types::{PermissionConfig, RuleAction};

// ═════════════════════════════════════════════════════════════════════════════
// Settings Types (Claude JSON subset)
// ═════════════════════════════════════════════════════════════════════════════

/// Subset of `.claude/settings.json` we care about.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSettings {
    #[serde(default)]
    pub permissions: Option<ParsedPermissions>,

    /// Raw `defaultMode` string when present (canonical under `permissions`, or
    /// echo-agent-only root legacy). Recognized values: `acceptEdits`,
    /// `bypassPermissions`, `default`, `plan`, `dontAsk`, `auto`.
    #[serde(default)]
    pub default_mode: Option<String>,

    /// Parsed but not acted on yet.
    #[serde(default)]
    pub additional_directories: Option<Vec<String>>,

    /// Environment variables applied to every session.
    /// Keys and values are strings; non-string values are coerced or skipped.
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

/// Parsed `permissions` object from Claude settings.
#[derive(Debug, Default, Deserialize)]
pub struct ParsedPermissions {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
}

impl ParsedPermissions {
    /// Translate into native `PermissionConfig`.
    /// Unsupported or malformed entries are skipped with warnings.
    pub fn into_permission_config(self) -> (PermissionConfig, Vec<String>) {
        let mut rules = Vec::new();
        let mut warnings = Vec::new();

        for (action, entries, label) in [
            (RuleAction::Allow, self.allow, "allow"),
            (RuleAction::Deny, self.deny, "deny"),
            (RuleAction::Ask, self.ask, "ask"),
        ] {
            for rule_str in entries {
                match parse_permission_rule(&rule_str, action) {
                    Ok(rule) => rules.push(rule),
                    Err(e) => warnings.push(format!("permissions.{label}: {rule_str} -- {e}")),
                }
            }
        }

        (PermissionConfig::new(rules), warnings)
    }
}

/// Load Claude settings from a file path.
///
/// Returns:
///   - `None` only for: file missing, unreadable, or unparseable JSON
///   - `Some(ClaudeSettings)` even if `permissions` key is absent
///
/// This allows callers to observe `defaultMode` / `additionalDirectories` even when
/// no `permissions` block exists, supporting the observability model.
///
/// **Tolerant parsing**:
///   - `permissions.allow` / `permissions.deny` are extracted element-by-element
///     from `serde_json::Value`. Non-string entries are skipped with warnings.
///   - This enables partial success when some entries are malformed.
///   - `defaultMode` / `additionalDirectories` prefer the canonical location
///     under `permissions.*`. Root-level keys are **echoagent legacy only** (not in
///     the vendor schema) and are used only when the nested key is **absent** —
///     not when it is present but the wrong type.
pub fn load_claude_settings(path: &Path) -> Option<ClaudeSettings> {
    // Opening a FIFO for read blocks until a writer appears; this runs on the
    // session actor, so refuse non-regular files up front.
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => {
            tracing::warn!(?path, "refusing to read non-regular settings file");
            return None;
        }
        Err(_) => return None,
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return None,
    };

    // Parse as generic JSON value for tolerant handling
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return None,
    };

    // Extract permissions tolerantly if present (with warnings for non-strings)
    let permissions = value.get("permissions").and_then(|p| {
        let (allow, allow_warnings) = extract_string_array(p.get("allow"));
        let (deny, deny_warnings) = extract_string_array(p.get("deny"));
        let (ask, ask_warnings) = extract_string_array(p.get("ask"));

        // Log any warnings from tolerant extraction
        for w in allow_warnings
            .iter()
            .chain(deny_warnings.iter())
            .chain(ask_warnings.iter())
        {
            tracing::warn!(path = %path.display(), "{}", w);
        }

        if allow.is_empty() && deny.is_empty() && ask.is_empty() {
            None
        } else {
            Some(ParsedPermissions { allow, deny, ask })
        }
    });

    // Canonical vendor settings store these under `permissions`; root is echoagent legacy only.
    let default_mode = extract_default_mode(&value, path);

    let additional_directories = extract_additional_directories(&value, path);

    let env = extract_string_map(value.get("env"), path);

    Some(ClaudeSettings {
        permissions,
        default_mode,
        additional_directories,
        env,
    })
}

/// Canonical key is `permissions.defaultMode`.
///
/// Root `defaultMode` is echo-agent-only back-compat for older tests / hand-written
/// configs. Fall back to root only when the nested key is **absent**. If nested
/// is present but not a string, do not resurrect a root value (malformed
/// canonical key must not revive stale legacy).
pub(crate) fn extract_default_mode(value: &serde_json::Value, path: &Path) -> Option<String> {
    if let Some(perms) = value.get("permissions")
        && let Some(dm) = perms.get("defaultMode")
    {
        return match dm.as_str() {
            Some(s) => Some(s.to_string()),
            None => {
                warn!(
                    path = %path.display(),
                    actual_type = %dm.type_of(),
                    "permissions.defaultMode: expected string; not falling back to root defaultMode"
                );
                None
            }
        };
    }

    // Nested key absent — optional echoagent legacy root.
    match value.get("defaultMode") {
        Some(dm) => match dm.as_str() {
            Some(s) => Some(s.to_string()),
            None => {
                warn!(
                    path = %path.display(),
                    actual_type = %dm.type_of(),
                    "root defaultMode (echoagent legacy): expected string, ignoring"
                );
                None
            }
        },
        None => None,
    }
}

/// Claude-canonical key is `permissions.additionalDirectories`; root is
/// legacy/compat. Nested wins when both are present.
fn extract_additional_directories(value: &serde_json::Value, path: &Path) -> Option<Vec<String>> {
    // Mirror `extract_default_mode`: prefer the Claude-canonical nested key, and
    // when it is present but the wrong type, do *not* resurrect the echo-agent-legacy
    // root value (a malformed canonical key must not revive stale legacy).
    let arr = if let Some(nested) = value
        .get("permissions")
        .and_then(|p| p.get("additionalDirectories"))
    {
        match nested.as_array() {
            Some(arr) => arr,
            None => {
                warn!(
                    path = %path.display(),
                    actual_type = %nested.type_of(),
                    "permissions.additionalDirectories: expected array; not falling back to root additionalDirectories"
                );
                return None;
            }
        }
    } else {
        value
            .get("additionalDirectories")
            .and_then(|v| v.as_array())?
    };

    let mut result = Vec::new();
    for (i, v) in arr.iter().enumerate() {
        match v.as_str() {
            Some(s) => result.push(s.to_string()),
            None => tracing::warn!(
                path = %path.display(),
                index = i,
                actual_type = %v.type_of(),
                "additionalDirectories: expected string, skipping"
            ),
        }
    }
    Some(result)
}

/// Extract a string array from a JSON value, skipping non-strings with warnings.
///
/// Returns `(strings, warnings)` where warnings describe skipped entries.
fn extract_string_array(value: Option<&serde_json::Value>) -> (Vec<String>, Vec<String>) {
    match value {
        Some(serde_json::Value::Array(arr)) => {
            let mut strings = Vec::new();
            let mut warnings = Vec::new();
            for (i, v) in arr.iter().enumerate() {
                match v.as_str() {
                    Some(s) => strings.push(s.to_string()),
                    None => warnings.push(format!(
                        "permissions array index {}: expected string, got {}",
                        i,
                        v.type_of()
                    )),
                }
            }
            (strings, warnings)
        }
        Some(other) => {
            let warnings = vec![format!(
                "permissions field: expected array, got {}",
                other.type_of()
            )];
            (Vec::new(), warnings)
        }
        None => (Vec::new(), Vec::new()),
    }
}

/// Extract a `HashMap<String, String>` from a JSON object value.
///
/// Non-string scalars are coerced to their string form: numbers and booleans
/// become their string representation. Null, array, and object values are
/// skipped with warnings.
///
/// Note: nulls are intentionally skipped rather than coerced to the literal
/// `"null"` — setting an env var to `"null"` is rarely useful and more likely a
/// user mistake.
fn extract_string_map(
    value: Option<&serde_json::Value>,
    path: &Path,
) -> Option<HashMap<String, String>> {
    let obj = match value {
        Some(serde_json::Value::Object(map)) => map,
        Some(other) => {
            tracing::warn!(
                path = %path.display(),
                actual_type = %other.type_of(),
                "env: expected object, skipping"
            );
            return None;
        }
        None => return None,
    };

    let mut result = HashMap::new();
    for (key, val) in obj {
        match val {
            serde_json::Value::String(s) => {
                result.insert(key.clone(), s.clone());
            }
            serde_json::Value::Number(n) => {
                result.insert(key.clone(), n.to_string());
            }
            serde_json::Value::Bool(b) => {
                result.insert(key.clone(), b.to_string());
            }
            other => {
                tracing::warn!(
                    path = %path.display(),
                    key = %key,
                    actual_type = %other.type_of(),
                    "env: expected string value, skipping"
                );
            }
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Small helper to get a JSON value's type name for diagnostics.
trait JsonTypeName {
    fn type_of(&self) -> &'static str;
}
impl JsonTypeName for serde_json::Value {
    fn type_of(&self) -> &'static str {
        match self {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "boolean",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Discovery
// ═════════════════════════════════════════════════════════════════════════════

// TODO(follow-up): The discovery logic here (find_claude_settings_paths,
// collect_project_claude_paths, find_repo_root) is local to this module.
// If the Claude settings compatibility surface grows (more consumers beyond
// permissions), consider extracting to a shared helper (e.g., in echo-agent-hooks
// or a new claude-discovery crate).

/// Discover `.claude/settings.json` and `.claude/settings.local.json` paths
/// for permission loading.
///
/// Files are returned in priority order (most-specific first):
///   - Project: `<cwd>/.claude/settings.local.json`, `<cwd>/.claude/settings.json`
///     (walking up to repo root; cwd entries listed first)
///   - Global:  `~/.claude/settings.local.json`, `~/.claude/settings.json`
///
/// Returns `true` if any `.claude/` configuration files exist in the project
/// or user home directory.
pub fn has_claude_compat(cwd: &Path) -> bool {
    find_claude_settings_paths(cwd).iter().any(|p| p.exists())
}

/// Permission rules from all files are merged (later / more specific sources win
/// for conflicts as documented below).
/// `defaultMode` uses scope precedence: the most specific file that sets it wins.
pub fn find_claude_settings_paths(cwd: &Path) -> Vec<PathBuf> {
    find_claude_settings_paths_with_home(cwd, echo_agent_config::discovery_home_dir())
}

pub(super) fn find_claude_settings_paths_with_home(
    cwd: &Path,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut paths = collect_project_claude_paths(cwd);
    paths.extend(global_claude_settings_paths_from(home));
    paths
}

/// Global (user-tier) `~/.claude` settings paths, highest-priority-first. Split
/// out of [`find_claude_settings_paths`] so [`claude_settings_paths_for_trust`]
/// can load ONLY the user tier when a folder is untrusted.
///
/// Use `echo_agent_config::discovery_home_dir()` to match the home-resolution strategy used by
/// `claude_import.rs::scan_importable_settings` and `claude_import_state.rs`,
/// so a path returned here reliably tests as global in the import scanner's
/// `is_global` check.
fn global_claude_settings_paths() -> Vec<PathBuf> {
    global_claude_settings_paths_from(echo_agent_config::discovery_home_dir())
}

fn global_claude_settings_paths_from(home: Option<PathBuf>) -> Vec<PathBuf> {
    home.map(|home| vec![home.join(".claude/settings.local.json"), home.join(".claude/settings.json")])
        .unwrap_or_default()
}

/// Claude settings files to load under the folder-trust gate.
///
/// When `project_trusted` is true, same as [`find_claude_settings_paths`]
/// (project tree + user `~/.claude`). When false, only user-tier `~/.claude`
/// — the single choke point for env injection and permission resolution so
/// the two cannot drift on which files an untrusted clone may contribute.
pub(crate) fn claude_settings_paths_for_trust(cwd: &Path, project_trusted: bool) -> Vec<PathBuf> {
    if project_trusted {
        find_claude_settings_paths(cwd)
    } else {
        global_claude_settings_paths()
    }
}

/// Whether a project-tree `.claude/settings.json` / `settings.local.json` exists
/// anywhere along the SAME `cwd`→repo-root walk the env/permission loaders read
/// ([`collect_project_claude_paths`]). The folder-trust detector calls this so
/// detection can never drift from the loader: a settings file in a SUBDIR — whose
/// `env` is injected into every spawned subprocess — must flip the folder
/// untrusted, not just one at the git root.
pub fn project_claude_settings_present(cwd: &Path) -> bool {
    collect_project_claude_paths(cwd)
        .iter()
        .any(|p| p.is_file())
}

/// Collect .claude settings file paths from cwd up to repo root.
///
/// Resolves the repo root by `.git` EXISTENCE (not `git2` validity), kept
/// separate from the folder-trust gate's shared `git2` walk on purpose: the
/// env/permission loader ([`find_claude_settings_paths`]) and this detector both
/// go through here, so they share ONE root resolution and can't drift — but a
/// directory with a bare/empty `.git` (no valid repo) must still bound the walk.
fn collect_project_claude_paths(cwd: &Path) -> Vec<PathBuf> {
    // Home-is-a-git-repo (dotfiles in $HOME): drop a resolved repo root that is
    // $HOME, or the walk would treat `~/.claude` as project-tier (injecting its
    // env / applying its rules for any cwd under home). Fall back to cwd so the
    // walk stays within the working dir. This is the shared choke point for both
    // `project_claude_settings_present` and `find_claude_settings_paths`.
    let repo_root = find_repo_root(cwd)
        .filter(|root| !crate::trust::is_home_dir(root))
        .unwrap_or_else(|| cwd.to_path_buf());

    // Walk from cwd up to repo_root, collecting .claude paths (cwd-first priority).
    let mut paths = Vec::new();
    let mut current = cwd.to_path_buf();
    loop {
        let claude_dir = current.join(".claude");
        paths.push(claude_dir.join("settings.local.json"));
        paths.push(claude_dir.join("settings.json"));

        if current == repo_root {
            break;
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }
    paths
}

/// Find the git repo root by walking up from cwd.
fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return None,
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Environment Variables
// ═════════════════════════════════════════════════════════════════════════════

/// Load merged environment variables from Claude settings files, gating the
/// repo-tree `.claude/settings.json` `env` on `project_trusted`.
///
/// Like permissions, env vars are merged cumulatively across all settings files
/// (later layers override earlier keys) — walking `find_claude_settings_paths()`
/// with precedence:
///   - Global `~/.claude/settings.json` / `settings.local.json` (lowest)
///   - Repo-root `.claude/settings.json` / `settings.local.json`
///   - ... (intermediate directories up to cwd)
///   - CWD `.claude/settings.json` / `settings.local.json` (highest)
///
/// Within each directory, `settings.local.json` overrides `settings.json`.
/// Higher-precedence keys override lower via `HashMap::extend`.
///
/// The repo-tree `env` is injected into every spawned subprocess (`BASH_ENV` /
/// `GIT_SSH_COMMAND` / `PATH` / `LD_PRELOAD` …), so when `project_trusted` is
/// false it is dropped — an untrusted clone must not contribute it; the user's
/// own `~/.claude` env is always loaded.
pub fn load_claude_env_with_project(cwd: &Path, project_trusted: bool) -> HashMap<String, String> {
    // Phase 2 cutoff: if the user has imported, skip reading .claude/ at runtime.
    if is_claude_import_marked_with_log("load_claude_env_with_project") {
        return HashMap::new();
    }

    // Untrusted folder: load ONLY the user-tier `~/.claude` env, dropping the
    // repo-tree (project) contribution.
    let paths = claude_settings_paths_for_trust(cwd, project_trusted);
    let mut merged = HashMap::new();

    // Paths are ordered highest-priority-first. Process in reverse so that
    // higher-priority values overwrite lower-priority ones via `extend`.
    for path in paths.iter().rev() {
        if let Some(settings) = load_claude_settings(path)
            && let Some(env) = settings.env
        {
            debug!(
                path = %path.display(),
                count = env.len(),
                "Loaded env from Claude settings"
            );
            merged.extend(env);
        }
    }

    merged
}

// =============================================================================
// Phase 2 cutoff marker
// =============================================================================
//
// `echo-agent-runtime::claude_import` writes the marker. We re-implement a small
// reader here because the gate consumers live in this crate and can't depend
// on shell (it would create a cycle). Caching is intentionally omitted; if
// this becomes a hotspot we can lift it into a shared crate.

/// True when the user marked Claude settings imported (`[claude_compat].imported`
/// in config.toml, or the test override). Public so gate-mirroring callers stay consistent.
pub fn is_claude_import_marked() -> bool {
    // Test escape hatch: shell tests call `refresh_marker_cache(true)` which
    // lives in echo-agent-runtime (inaccessible from here at runtime). They also
    // set this env var so the workspace-resident gate honours the override
    // without a cross-crate dependency.
    if std::env::var("_ECHO_AGENT_CLAUDE_MARKER_OVERRIDE").as_deref() == Ok("1") {
        return true;
    }
    let Some(config_path) =
        echo_agent_config::user_echo_agent_home().map(|g| g.join("config.toml"))
    else {
        return false;
    };
    let Ok(contents) = std::fs::read_to_string(&config_path) else {
        return false;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&contents) else {
        return false;
    };
    value
        .get("claude_compat")
        .and_then(|v| v.get("imported"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Returns true when the user has marked their Claude settings as imported.
///
/// Logs a single info line the first time the gate is hit (per process) so
/// we can confirm the cutoff is taking effect without flooding logs.
pub(crate) fn is_claude_import_marked_with_log(gate_name: &'static str) -> bool {
    use std::sync::OnceLock;
    static LOGGED: OnceLock<()> = OnceLock::new();

    let marked = is_claude_import_marked();
    if marked {
        LOGGED.get_or_init(|| {
            tracing::info!(
                first_gate = gate_name,
                "Claude compat disabled (marker set in config.toml)"
            );
        });
    }
    marked
}

#[cfg(test)]
mod review_env_discovery_tests {
    use super::*;
    #[test]
    fn session_claude_env_reads_only_controlled_markers_and_keeps_normal_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let owned = temp.path().join("review/compat-home");
        let foreign = temp.path().join("synthetic-normal-home");
        for (home, marker) in [(&owned, "owned"), (&foreign, "foreign")] {
            std::fs::create_dir_all(home.join(".claude")).unwrap();
            std::fs::write(home.join(".claude/settings.json"),
                format!(r#"{{"env":{{"CONTROLLED_MARKER":"{marker}"}}}}"#)).unwrap();
        }
        let selected = echo_agent_config::resolve_discovery_home_with(Some(owned),
            || panic!("review session consulted normal HOME"));
        let paths = global_claude_settings_paths_from(selected);
        let settings = paths.iter().find_map(|p| load_claude_settings(p)).unwrap();
        assert_eq!(settings.env.unwrap()["CONTROLLED_MARKER"], "owned");
        let normal = echo_agent_config::resolve_discovery_home_with(None, || Some(foreign));
        let settings = global_claude_settings_paths_from(normal).iter().find_map(|p| load_claude_settings(p)).unwrap();
        assert_eq!(settings.env.unwrap()["CONTROLLED_MARKER"], "foreign");
    }
}
