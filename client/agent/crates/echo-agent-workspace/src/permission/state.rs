#![allow(dead_code)] // Phase 1 internal helpers

use crate::permission::types::EditPolicy;
use echo_agent_paths::AbsPathBuf;
use echo_agent_tools::util::echo_agent_home::echo_agent_home;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const VALIDATED_MCP_SERVER_GRANTS_VERSION: i64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionState {
    pub edit_policy: EditPolicy,
    pub allow_bash_execute: bool,
    pub allowed_bash_commands: HashSet<String>,
    pub disallowed_bash_commands: HashSet<String>,
    /// Glob patterns the user authored via the "Always allow" pattern editor
    /// (e.g. `gh api repos/owner/*`). Matched with glob semantics, unlike the
    /// literal-prefix [`Self::allowed_bash_commands`]; kept separate so a command
    /// grant that happens to contain shell metacharacters is never a wildcard.
    pub allowed_bash_globs: HashSet<String>,
    /// Domains the user has approved for `web_fetch`. Persisted per project
    /// like every other grant in this store (not session-scoped).
    pub allowed_web_fetch_domains: HashSet<String>,
    /// Exact MCP tool names (e.g. `"echo_agent_com_notion__notion-fetch"`)
    /// the user has granted "always allow" for. Lookup is exact.
    pub allowed_mcp_tools: HashSet<String>,
    /// Server components of valid qualified MCP IDs (e.g. `"echo_agent_com_notion"`)
    /// for which the user has granted "always allow" to every tool. Lookup
    /// validates and parses the complete qualified ID before matching.
    pub allowed_mcp_servers: HashSet<String>,
    /// Exact MCP tool names the user has denied with "never allow". Checked
    /// before every MCP grant (deny wins). Always tool-scoped — there is
    /// deliberately no server-scope deny.
    pub disallowed_mcp_tools: HashSet<String>,
    /// Host keys the user has denied for `web_fetch` (lowercased, `www.` kept
    /// — never collapsed to a parent domain). Checked before every web-fetch
    /// grant (deny wins); a deny also covers subdomains of the entry.
    pub disallowed_web_fetch_domains: HashSet<String>,
    /// Version proving server-wide grants were minted from validated qualified IDs.
    /// Missing or malformed markers are legacy; future integer versions are preserved.
    #[serde(
        default = "legacy_mcp_server_grants_version",
        deserialize_with = "deserialize_mcp_server_grants_version"
    )]
    pub(crate) validated_mcp_server_grants_version: i64,
}

fn legacy_mcp_server_grants_version() -> i64 {
    0
}

fn deserialize_mcp_server_grants_version<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = toml::Value::deserialize(deserializer)?;
    Ok(match value.as_integer() {
        Some(version) if version >= 0 => version,
        _ => 0,
    })
}

impl Default for PermissionState {
    fn default() -> Self {
        Self {
            edit_policy: EditPolicy::default(),
            allow_bash_execute: false,
            allowed_bash_commands: HashSet::new(),
            disallowed_bash_commands: HashSet::new(),
            allowed_bash_globs: HashSet::new(),
            allowed_web_fetch_domains: HashSet::new(),
            allowed_mcp_tools: HashSet::new(),
            allowed_mcp_servers: HashSet::new(),
            disallowed_mcp_tools: HashSet::new(),
            disallowed_web_fetch_domains: HashSet::new(),
            validated_mcp_server_grants_version: VALIDATED_MCP_SERVER_GRANTS_VERSION,
        }
    }
}

impl PermissionState {
    /// Union `other`'s grants/denies into `self`. Set-valued fields merge, and
    /// so does `allow_bash_execute` (`|=`): it is a persisted blanket *grant*,
    /// so it follows grant semantics — additive until an explicit reset.
    /// Scalar policy fields keep `self`'s values (the in-memory session is
    /// authoritative for them, and `edit_policy` is migrated to `Ask` at
    /// manager startup regardless).
    ///
    /// Merging is deliberately additive in both directions: denies persist
    /// like grants, so an accepted "never allow" holds repo-wide until an
    /// explicit reset ([`replace_state_on_disk`], the non-merging path) —
    /// nothing removed only in one session's memory can win a merge.
    ///
    /// Exhaustive destructure: adding a `PermissionState` field breaks this
    /// fn until the merge decision for it is made explicitly.
    pub(crate) fn merge_grants_from(&mut self, other: PermissionState) {
        let PermissionState {
            edit_policy: _,
            allow_bash_execute,
            allowed_bash_commands,
            disallowed_bash_commands,
            allowed_bash_globs,
            allowed_web_fetch_domains,
            allowed_mcp_tools,
            allowed_mcp_servers,
            disallowed_mcp_tools,
            disallowed_web_fetch_domains,
            validated_mcp_server_grants_version: _,
        } = other;
        self.allow_bash_execute |= allow_bash_execute;
        self.allowed_bash_commands.extend(allowed_bash_commands);
        self.disallowed_bash_commands
            .extend(disallowed_bash_commands);
        self.allowed_bash_globs.extend(allowed_bash_globs);
        self.allowed_web_fetch_domains
            .extend(allowed_web_fetch_domains);
        self.allowed_mcp_tools.extend(allowed_mcp_tools);
        self.allowed_mcp_servers.extend(allowed_mcp_servers);
        self.disallowed_mcp_tools.extend(disallowed_mcp_tools);
        self.disallowed_web_fetch_domains
            .extend(disallowed_web_fetch_domains);
    }
}

/// The directory that keys the persistent permission store. Grants accepted
/// anywhere inside a git repository apply repo-wide; keying on the exact cwd
/// would hide a grant accepted at the repo root from a session started in a
/// subdirectory. Root discovery is [`RepoDirChain`] — the same resolver
/// folder trust and project-config discovery use, so all three agree on
/// where a project starts (including its home-directory exception: a
/// dotfiles-style repo at `$HOME` keys per-cwd, not repo-wide).
///
/// Synchronous filesystem work (git discovery + canonicalize): call from the
/// blocking pool via [`resolve_store_dirs`] on the async paths.
fn permission_scope_root(cwd: &AbsPathBuf) -> std::path::PathBuf {
    match echo_agent_core::repo::RepoDirChain::resolve(cwd.as_path()).git_root {
        // git2 workdirs can carry a trailing separator; re-collecting the
        // components drops it so the encoded store key matches the plain
        // spelling of the same directory.
        Some(root) => root.components().collect(),
        None => cwd.as_path().to_path_buf(),
    }
}

/// Test-only; production resolves the scope root once in
/// [`resolve_store_dirs`] and derives everything from it.
#[cfg(test)]
fn state_dir_for_cwd(cwd: &AbsPathBuf) -> std::path::PathBuf {
    echo_agent_config::sessions_cwd_dir(&permission_scope_root(cwd).to_string_lossy())
}

/// The pre-repo-root store location (exact-cwd keyed), when it differs from
/// the pre-resolved repo-root store `dir`. Read-only migration source:
/// grants saved by older builds in a subdirectory still load until the
/// repo-root store exists, and the next persist carries them into it.
fn legacy_state_dir(cwd: &AbsPathBuf, dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let legacy = echo_agent_config::sessions_cwd_dir(cwd.as_str());
    (legacy != dir).then_some(legacy)
}

/// Both store locations for `cwd`, resolved once per call on the blocking
/// pool: root discovery walks the filesystem and, on the persist path,
/// `ensure` creates + chmods the sessions dir — none of it belongs on the
/// async worker. Falls back to exact-cwd keying if the blocking task dies.
struct StoreDirs {
    dir: std::path::PathBuf,
    legacy_dir: Option<std::path::PathBuf>,
}

async fn resolve_store_dirs(cwd: &AbsPathBuf, ensure: bool) -> StoreDirs {
    let cwd = cwd.clone();
    let fallback_dir = echo_agent_config::sessions_cwd_dir(cwd.as_str());
    tokio::task::spawn_blocking(move || {
        // Resolve the scope root ONCE: discovery walks the filesystem, and the
        // store dir, its ensure fallback, and the legacy compare all derive
        // from this single resolution (state_dir_for_cwd would re-resolve).
        let root = permission_scope_root(&cwd).to_string_lossy().into_owned();
        let dir = if ensure {
            // Canonical creator: tighten the sessions root this write may
            // create. Falls back to the computed path (persist_state_to_dir
            // re-creates it owner-only) so a failed ensure still gets a write.
            echo_agent_config::ensure_sessions_cwd_dir(&root).unwrap_or_else(|e| {
                tracing::warn!(?e, "failed ensuring sessions cwd dir for permission state");
                echo_agent_config::sessions_cwd_dir(&root)
            })
        } else {
            echo_agent_config::sessions_cwd_dir(&root)
        };
        StoreDirs {
            legacy_dir: legacy_state_dir(&cwd, &dir),
            dir,
        }
    })
    .await
    .unwrap_or(StoreDirs {
        dir: fallback_dir,
        legacy_dir: None,
    })
}

fn sanitize_client_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn state_file_path(dir: &std::path::Path, client_identifier: Option<&str>) -> std::path::PathBuf {
    match client_identifier {
        Some(id) => dir.join(format!("permission_{}.toml", sanitize_client_id(id))),
        None => dir.join("permission.toml"),
    }
}

async fn try_load_state_with_writer<F>(path: &std::path::Path, writer: F) -> Option<PermissionState>
where
    F: FnOnce(&std::path::Path, &str) -> std::io::Result<()> + Send + 'static,
{
    match tokio::fs::read_to_string(path).await {
        Ok(s) => {
            let mut state: PermissionState = toml::from_str(&s).unwrap_or_default();
            if state.validated_mcp_server_grants_version < VALIDATED_MCP_SERVER_GRANTS_VERSION {
                state.allowed_mcp_servers.clear();
                state.validated_mcp_server_grants_version = VALIDATED_MCP_SERVER_GRANTS_VERSION;
                tracing::info!(path = %path.display(), "invalidated legacy MCP server grants");
                if let Err(e) = persist_state_to_path_with_writer(path, &state, writer).await {
                    tracing::warn!(?e, path = %path.display(), "failed writing permission state");
                }
            }
            Some(state)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(?e, "failed reading permission state");
            None
        }
    }
}

async fn try_load_state(path: &std::path::Path) -> Option<PermissionState> {
    try_load_state_with_writer(path, |path, contents| {
        echo_agent_config::fs_atomic::write_atomically(path, contents, None)
    })
    .await
}

async fn load_state_from_dir(
    dir: &std::path::Path,
    client_identifier: Option<&str>,
) -> PermissionState {
    if let Some(id) = client_identifier {
        let per_client = state_file_path(dir, Some(id));
        if let Some(state) = try_load_state(&per_client).await {
            return state;
        }
    }
    try_load_state(&state_file_path(dir, None))
        .await
        .unwrap_or_default()
}

/// Whether `dir` holds a store file this client's load would read: the
/// per-client file or the shared fallback. Mirrors `load_state_from_dir`.
async fn state_dir_has_store(dir: &std::path::Path, client_identifier: Option<&str>) -> bool {
    // Fail closed: an IO error must count as "store present" — resolving it
    // to absent would reopen the legacy fallback and could re-seed grants a
    // reset cleared.
    let present =
        |p: std::path::PathBuf| async move { !matches!(tokio::fs::try_exists(p).await, Ok(false)) };
    if let Some(id) = client_identifier
        && present(state_file_path(dir, Some(id))).await
    {
        return true;
    }
    present(state_file_path(dir, None)).await
}

/// Load with legacy fallback — a fallback seed, never a merge: the exact-cwd
/// legacy store is consulted only while no scope-root store exists. Any root
/// write (a new grant or a reset) supersedes every legacy file, so a reset
/// cannot be undone by a stale legacy file in another subdirectory.
async fn load_state_with_fallback(
    dir: &std::path::Path,
    legacy_dir: Option<&std::path::Path>,
    client_identifier: Option<&str>,
) -> PermissionState {
    if let Some(legacy_dir) = legacy_dir
        && !state_dir_has_store(dir, client_identifier).await
    {
        return load_state_from_dir(legacy_dir, client_identifier).await;
    }
    load_state_from_dir(dir, client_identifier).await
}

pub(crate) async fn load_state_from_disk(
    cwd: &AbsPathBuf,
    client_identifier: Option<&str>,
) -> PermissionState {
    let dirs = resolve_store_dirs(cwd, false).await;
    load_state_with_fallback(&dirs.dir, dirs.legacy_dir.as_deref(), client_identifier).await
}

async fn persist_state_to_path_with_writer<F>(
    path: &std::path::Path,
    state: &PermissionState,
    writer: F,
) -> std::io::Result<()>
where
    F: FnOnce(&std::path::Path, &str) -> std::io::Result<()> + Send + 'static,
{
    let contents = toml::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || writer(&path, &contents))
        .await
        .map_err(std::io::Error::other)?
}

async fn persist_state_to_dir(
    dir: &std::path::Path,
    state: &PermissionState,
    client_identifier: Option<&str>,
) {
    let path = state_file_path(dir, client_identifier);
    let dir = dir.to_path_buf();
    // Owner-only dir creation rides the writer's spawn_blocking: ECHO_AGENT_HOME may
    // sit on a slow filesystem, so no blocking fs work on the async worker.
    let result = persist_state_to_path_with_writer(&path, state, move |path, contents| {
        echo_agent_config::create_dir_all_owner_only(&dir)?;
        echo_agent_config::fs_atomic::write_atomically(path, contents, None)
    })
    .await;
    if let Err(e) = result {
        tracing::warn!(?e, path = %path.display(), "failed persisting permission state");
    }
}

/// Merge-on-write: a concurrent session in the same project may have
/// persisted new grants since this actor loaded its snapshot at spawn.
/// A whole-file replace would silently erase them (last-writer-wins);
/// union the on-disk grants back in before writing. Not a lock — the
/// read-modify-write race window remains — but it turns "other session's
/// grants always lost" into "lost only on a same-instant write".
async fn persist_state_merging_to_dir(
    dir: &std::path::Path,
    state: &PermissionState,
    client_identifier: Option<&str>,
) {
    let mut merged = state.clone();
    if let Some(on_disk) = try_load_state(&state_file_path(dir, client_identifier)).await {
        merged.merge_grants_from(on_disk);
    }
    persist_state_to_dir(dir, &merged, client_identifier).await
}

pub(crate) async fn persist_state(
    cwd: &AbsPathBuf,
    state: &PermissionState,
    client_identifier: Option<&str>,
) {
    let dirs = resolve_store_dirs(cwd, true).await;
    persist_state_merging_to_dir(&dirs.dir, state, client_identifier).await
}

/// Replace the on-disk state without merging — reset semantics, where the
/// whole point is discarding grants. Writing the scope-root store also ends
/// the legacy fallback for every subdirectory of the repository (see
/// [`load_state_with_fallback`]), so no legacy cleanup is needed.
pub(crate) async fn replace_state_on_disk(
    cwd: &AbsPathBuf,
    state: &PermissionState,
    client_identifier: Option<&str>,
) {
    let dirs = resolve_store_dirs(cwd, true).await;
    persist_state_to_dir(&dirs.dir, state, client_identifier).await;
}

pub async fn cleanup_stale_permission_state(max_age: std::time::Duration) {
    let sessions_dir = echo_agent_home().join("sessions");
    let Ok(mut entries) = tokio::fs::read_dir(&sessions_dir).await else {
        return;
    };
    while let Ok(Some(session_entry)) = entries.next_entry().await {
        let Ok(ft) = session_entry.file_type().await else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        let session_dir = session_entry.path();
        let Ok(mut files) = tokio::fs::read_dir(&session_dir).await else {
            continue;
        };
        while let Ok(Some(file_entry)) = files.next_entry().await {
            let path = file_entry.path();
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !file_name.starts_with("permission") || !file_name.ends_with(".toml") {
                continue;
            }
            if let Ok(metadata) = tokio::fs::metadata(&path).await
                && let Ok(modified) = metadata.modified()
                && let Ok(age) = modified.elapsed()
                && age > max_age
            {
                tracing::debug!(path = %path.display(), "removing stale permission state");
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    // ── PermissionState serialization roundtrip tests ─────────────

    #[test]
    fn default_state_serialization() {
        let state = PermissionState::default();
        let toml_str = toml::to_string_pretty(&state).unwrap();
        let restored: PermissionState = toml::from_str(&toml_str).unwrap();
        assert!(!restored.allow_bash_execute);
        assert!(restored.allowed_bash_commands.is_empty());
        assert!(restored.disallowed_bash_commands.is_empty());
        assert_eq!(
            restored.validated_mcp_server_grants_version,
            VALIDATED_MCP_SERVER_GRANTS_VERSION
        );
    }

    #[test]
    fn roundtrip_with_allowed_commands() {
        let mut state = PermissionState::default();
        state.allow_bash_execute = true;
        state.allowed_bash_commands.insert("cargo test".to_string());
        state
            .allowed_bash_commands
            .insert("npm run build".to_string());

        let toml_str = toml::to_string_pretty(&state).unwrap();
        let restored: PermissionState = toml::from_str(&toml_str).unwrap();

        assert!(restored.allow_bash_execute);
        assert!(restored.allowed_bash_commands.contains("cargo test"));
        assert!(restored.allowed_bash_commands.contains("npm run build"));
        assert_eq!(restored.allowed_bash_commands.len(), 2);
    }

    #[test]
    fn roundtrip_with_disallowed_commands() {
        let mut state = PermissionState::default();
        state.disallowed_bash_commands.insert("rm -rf".to_string());
        state
            .disallowed_bash_commands
            .insert("git push --force".to_string());

        let toml_str = toml::to_string_pretty(&state).unwrap();
        let restored: PermissionState = toml::from_str(&toml_str).unwrap();

        let denied = &restored.disallowed_bash_commands;
        assert!(denied.contains("rm -rf"));
        assert!(denied.contains("git push --force"));
        assert_eq!(denied.len(), 2);
    }

    /// Pre-deny stores (no `disallowed_mcp_tools` / `disallowed_web_fetch_domains`
    /// keys on disk) must load with empty deny sets.
    #[test]
    fn missing_deny_fields_default_empty() {
        let restored: PermissionState = toml::from_str(
            r#"
allowed_mcp_tools = ["linear__list"]
"#,
        )
        .unwrap();
        assert!(restored.allowed_mcp_tools.contains("linear__list"));
        assert!(restored.disallowed_mcp_tools.is_empty());
        assert!(restored.disallowed_web_fetch_domains.is_empty());
    }

    #[test]
    fn roundtrip_with_both_allowed_and_disallowed() {
        // Simulate a real scenario: some commands explicitly allowed,
        // others explicitly denied.
        let mut state = PermissionState::default();
        state.allow_bash_execute = false;
        state.allowed_bash_commands.insert("cargo test".to_string());
        state.allowed_bash_commands.insert("git status".to_string());
        state
            .disallowed_bash_commands
            .insert("rm -rf /".to_string());
        state.disallowed_bash_commands.insert("curl".to_string());

        let toml_str = toml::to_string_pretty(&state).unwrap();
        let restored: PermissionState = toml::from_str(&toml_str).unwrap();

        assert!(!restored.allow_bash_execute);
        assert_eq!(restored.allowed_bash_commands.len(), 2);
        assert_eq!(restored.disallowed_bash_commands.len(), 2);
        assert!(restored.allowed_bash_commands.contains("cargo test"));
        assert!(restored.disallowed_bash_commands.contains("curl"));
    }

    #[test]
    fn edit_policy_is_persisted() {
        let mut state = PermissionState::default();
        state.edit_policy = EditPolicy::Allow;

        let toml_str = toml::to_string_pretty(&state).unwrap();
        assert!(toml_str.contains("edit_policy"));

        let restored: PermissionState = toml::from_str(&toml_str).unwrap();
        assert_eq!(restored.edit_policy, EditPolicy::Allow);
    }

    #[test]
    fn edit_policy_reject_roundtrip() {
        let mut state = PermissionState::default();
        state.edit_policy = EditPolicy::Reject;

        let toml_str = toml::to_string_pretty(&state).unwrap();
        let restored: PermissionState = toml::from_str(&toml_str).unwrap();
        assert_eq!(restored.edit_policy, EditPolicy::Reject);
    }

    #[test]
    fn missing_edit_policy_defaults_to_ask() {
        let toml_str = r#"allow_bash_execute = false"#;
        let state: PermissionState = toml::from_str(toml_str).unwrap();
        assert_eq!(state.edit_policy, EditPolicy::Ask);
    }

    #[test]
    fn deserialize_from_empty_toml_is_legacy() {
        let state: PermissionState = toml::from_str("").unwrap();
        assert!(!state.allow_bash_execute);
        assert!(state.allowed_bash_commands.is_empty());
        assert!(state.disallowed_bash_commands.is_empty());
        assert_eq!(state.validated_mcp_server_grants_version, 0);
    }

    #[test]
    fn deserialize_partial_toml() {
        // Only some fields present — others should default.
        let toml_str = r#"allow_bash_execute = true"#;
        let state: PermissionState = toml::from_str(toml_str).unwrap();
        assert!(state.allow_bash_execute);
        assert!(state.allowed_bash_commands.is_empty());
        assert!(state.disallowed_bash_commands.is_empty());
    }

    #[test]
    fn roundtrip_with_allowed_web_fetch_domains() {
        let mut state = PermissionState::default();
        state
            .allowed_web_fetch_domains
            .insert("stackoverflow.com".to_string());
        state
            .allowed_web_fetch_domains
            .insert("custom.example.com".to_string());

        let toml_str = toml::to_string_pretty(&state).unwrap();
        let restored: PermissionState = toml::from_str(&toml_str).unwrap();

        assert_eq!(restored.allowed_web_fetch_domains.len(), 2);
        assert!(
            restored
                .allowed_web_fetch_domains
                .contains("stackoverflow.com")
        );
        assert!(
            restored
                .allowed_web_fetch_domains
                .contains("custom.example.com")
        );
    }

    #[test]
    fn roundtrip_with_allowed_mcp_tools() {
        let mut state = PermissionState::default();
        state
            .allowed_mcp_tools
            .insert("echo_agent_com_notion__notion-fetch".to_string());
        state
            .allowed_mcp_tools
            .insert("linear__list_issues".to_string());

        let toml_str = toml::to_string_pretty(&state).unwrap();
        let restored: PermissionState = toml::from_str(&toml_str).unwrap();

        assert_eq!(restored.allowed_mcp_tools.len(), 2);
        assert!(
            restored
                .allowed_mcp_tools
                .contains("echo_agent_com_notion__notion-fetch")
        );
        assert!(restored.allowed_mcp_tools.contains("linear__list_issues"));
        assert!(restored.allowed_mcp_servers.is_empty());
    }

    #[test]
    fn roundtrip_with_allowed_mcp_servers() {
        let mut state = PermissionState::default();
        state
            .allowed_mcp_servers
            .insert("echo_agent_com_slack".to_string());
        state.allowed_mcp_servers.insert("linear".to_string());

        let toml_str = toml::to_string_pretty(&state).unwrap();
        let restored: PermissionState = toml::from_str(&toml_str).unwrap();

        assert_eq!(restored.allowed_mcp_servers.len(), 2);
        assert!(
            restored
                .allowed_mcp_servers
                .contains("echo_agent_com_slack")
        );
        assert!(restored.allowed_mcp_servers.contains("linear"));
        assert!(restored.allowed_mcp_tools.is_empty());
    }

    #[test]
    fn roundtrip_with_both_mcp_sets() {
        let mut state = PermissionState::default();
        state.allowed_mcp_tools.insert("notion__fetch".to_string());
        state.allowed_mcp_servers.insert("linear".to_string());

        let toml_str = toml::to_string_pretty(&state).unwrap();
        let restored: PermissionState = toml::from_str(&toml_str).unwrap();

        assert_eq!(restored.allowed_mcp_tools.len(), 1);
        assert_eq!(restored.allowed_mcp_servers.len(), 1);
        assert!(restored.allowed_mcp_tools.contains("notion__fetch"));
        assert!(restored.allowed_mcp_servers.contains("linear"));
    }

    #[test]
    fn deserialize_old_state_without_mcp_fields() {
        // A state file from a binary that predates this design has
        // neither MCP field. #[serde(default)] should yield empty sets.
        let toml_str = r#"
allow_bash_execute = true
allowed_bash_commands = ["cargo test"]
allowed_web_fetch_domains = ["github.com"]
"#;
        let state: PermissionState = toml::from_str(toml_str).unwrap();
        assert!(state.allow_bash_execute);
        assert!(state.allowed_bash_commands.contains("cargo test"));
        assert!(state.allowed_web_fetch_domains.contains("github.com"));
        assert!(state.allowed_mcp_tools.is_empty());
        assert!(state.allowed_mcp_servers.is_empty());
        assert_eq!(state.validated_mcp_server_grants_version, 0);
    }

    #[test]
    fn malformed_mcp_server_grants_version_is_legacy() {
        for marker in ["-1", "\"invalid\""] {
            let state: PermissionState =
                toml::from_str(&format!("validated_mcp_server_grants_version = {marker}")).unwrap();
            assert_eq!(state.validated_mcp_server_grants_version, 0);
        }
    }

    #[test]
    fn deserialize_unknown_fields_tolerated() {
        // PermissionState uses #[serde(default)] which provides defaults for
        // missing fields. It does NOT use #[serde(deny_unknown_fields)], so
        // unknown keys in TOML are silently ignored. This is important for
        // forward compatibility: older versions of the binary should be able
        // to read state files written by newer versions that may have added
        // new fields.
        let toml_str = r#"
allow_bash_execute = false
unknown_field = "should be ignored"
allowed_bash_commands = ["ls"]
"#;
        let state: PermissionState = toml::from_str(toml_str).unwrap();
        assert!(!state.allow_bash_execute);
        assert!(state.allowed_bash_commands.contains("ls"));
        assert!(state.disallowed_bash_commands.is_empty());
    }

    // ── Disk persistence roundtrip tests ─────────────────────────

    async fn write_legacy_mcp_state(path: &std::path::Path) {
        tokio::fs::write(
            path,
            r#"
edit_policy = "reject"
allow_bash_execute = true
allowed_bash_commands = ["cargo test"]
disallowed_bash_commands = ["rm"]
allowed_web_fetch_domains = ["example.com"]
allowed_mcp_tools = ["a__b__c"]
allowed_mcp_servers = ["a"]
"#,
        )
        .await
        .unwrap();
    }

    fn assert_legacy_mcp_state_migrated(state: &PermissionState) {
        assert!(state.allowed_mcp_servers.is_empty());
        assert!(state.allowed_mcp_tools.contains("a__b__c"));
        assert!(state.allow_bash_execute);
        assert!(state.allowed_bash_commands.contains("cargo test"));
        assert!(state.disallowed_bash_commands.contains("rm"));
        assert!(state.allowed_web_fetch_domains.contains("example.com"));
        assert_eq!(state.edit_policy, EditPolicy::Reject);
        assert_eq!(
            state.validated_mcp_server_grants_version,
            VALIDATED_MCP_SERVER_GRANTS_VERSION
        );
    }

    #[tokio::test]
    async fn legacy_shared_mcp_server_grants_migrate_and_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        let path = state_file_path(tmp.path(), None);
        write_legacy_mcp_state(&path).await;

        assert_legacy_mcp_state_migrated(&load_state_from_dir(tmp.path(), None).await);
        let rewritten: PermissionState =
            toml::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_legacy_mcp_state_migrated(&rewritten);
    }

    #[tokio::test]
    async fn failed_migration_rewrite_preserves_legacy_file_for_retry() {
        fn fail_write(_: &std::path::Path, _: &str) -> std::io::Result<()> {
            Err(std::io::Error::other("injected write failure"))
        }

        let tmp = tempfile::tempdir().unwrap();
        let path = state_file_path(tmp.path(), None);
        write_legacy_mcp_state(&path).await;
        let legacy_contents = tokio::fs::read_to_string(&path).await.unwrap();

        let in_memory = try_load_state_with_writer(&path, fail_write).await.unwrap();
        assert_legacy_mcp_state_migrated(&in_memory);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            legacy_contents
        );
        let still_legacy: PermissionState = toml::from_str(&legacy_contents).unwrap();
        assert_eq!(still_legacy.validated_mcp_server_grants_version, 0);
        assert!(still_legacy.allowed_mcp_servers.contains("a"));

        assert_legacy_mcp_state_migrated(&try_load_state(&path).await.unwrap());
        let rewritten: PermissionState =
            toml::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_legacy_mcp_state_migrated(&rewritten);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn persist_state_to_dir_creates_owner_only_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sessions").join("%2Fsome%2Fcwd");

        persist_state_to_dir(&dir, &PermissionState::default(), None).await;

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[tokio::test]
    async fn current_and_future_mcp_server_grants_are_retained_exactly() {
        for version in [
            VALIDATED_MCP_SERVER_GRANTS_VERSION,
            VALIDATED_MCP_SERVER_GRANTS_VERSION + 1,
            4_294_967_296,
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let mut state = PermissionState::default();
            state.validated_mcp_server_grants_version = version;
            state.allowed_mcp_servers.insert("linear".to_owned());
            persist_state_to_dir(tmp.path(), &state, None).await;

            let loaded = load_state_from_dir(tmp.path(), None).await;
            assert!(loaded.allowed_mcp_servers.contains("linear"));
            assert_eq!(loaded.validated_mcp_server_grants_version, version);
            let persisted: PermissionState = toml::from_str(
                &tokio::fs::read_to_string(state_file_path(tmp.path(), None))
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(persisted.validated_mcp_server_grants_version, version);
        }
    }

    #[tokio::test]
    async fn per_client_legacy_migration_rewrites_only_loaded_file() {
        let tmp = tempfile::tempdir().unwrap();
        let shared = state_file_path(tmp.path(), None);
        let per_client = state_file_path(tmp.path(), Some("desktop"));
        let mut shared_state = PermissionState::default();
        shared_state.allowed_mcp_servers.insert("shared".to_owned());
        persist_state_to_dir(tmp.path(), &shared_state, None).await;
        write_legacy_mcp_state(&per_client).await;

        assert_legacy_mcp_state_migrated(&load_state_from_dir(tmp.path(), Some("desktop")).await);
        let shared_after: PermissionState =
            toml::from_str(&tokio::fs::read_to_string(shared).await.unwrap()).unwrap();
        assert!(shared_after.allowed_mcp_servers.contains("shared"));
        let client_after: PermissionState =
            toml::from_str(&tokio::fs::read_to_string(per_client).await.unwrap()).unwrap();
        assert_legacy_mcp_state_migrated(&client_after);
    }

    #[tokio::test]
    async fn per_client_fallback_migrates_shared_file() {
        let tmp = tempfile::tempdir().unwrap();
        let shared = state_file_path(tmp.path(), None);
        write_legacy_mcp_state(&shared).await;

        assert_legacy_mcp_state_migrated(
            &load_state_from_dir(tmp.path(), Some("missing-client")).await,
        );
        let shared_after: PermissionState =
            toml::from_str(&tokio::fs::read_to_string(shared).await.unwrap()).unwrap();
        assert_legacy_mcp_state_migrated(&shared_after);
        assert!(!state_file_path(tmp.path(), Some("missing-client")).exists());
    }

    #[tokio::test]
    async fn persist_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = PermissionState::default();
        state.allow_bash_execute = true;
        state
            .allowed_bash_commands
            .insert("cargo build".to_string());
        state.disallowed_bash_commands.insert("rm -rf".to_string());

        persist_state_to_dir(tmp.path(), &state, None).await;
        let restored = load_state_from_dir(tmp.path(), None).await;
        assert!(restored.allow_bash_execute);
        assert!(restored.allowed_bash_commands.contains("cargo build"));
        assert!(restored.disallowed_bash_commands.contains("rm -rf"));
    }

    #[tokio::test]
    async fn load_missing_file_returns_default() {
        // Simulates load_state_from_disk behavior for a missing file.
        let path = std::path::Path::new("/nonexistent/permission.toml");
        let result = tokio::fs::read_to_string(path).await;
        match result {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let state = PermissionState::default();
                assert!(!state.allow_bash_execute);
            }
            _ => panic!("expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn load_corrupt_file_returns_default() {
        // Simulates load_state_from_disk behavior for corrupt TOML.
        let corrupt = "this is not valid toml {{{{";
        let state: PermissionState = toml::from_str(corrupt).unwrap_or_default();
        assert!(!state.allow_bash_execute);
        assert!(state.allowed_bash_commands.is_empty());
    }

    // ── Per-client state file path tests ──────────────────────────

    #[test]
    fn state_file_path_without_client_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = state_file_path(tmp.path(), None);
        assert_eq!(path.file_name().unwrap(), "permission.toml");
    }

    #[test]
    fn state_file_path_with_client_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = state_file_path(tmp.path(), Some("vscode-ext"));
        assert_eq!(path.file_name().unwrap(), "permission_vscode-ext.toml");
    }

    #[test]
    fn state_file_path_empty_client_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = state_file_path(tmp.path(), Some(""));
        assert_eq!(path.file_name().unwrap(), "permission_.toml");
    }

    #[test]
    fn state_file_path_sanitizes_path_separators() {
        let tmp = tempfile::tempdir().unwrap();
        let path = state_file_path(tmp.path(), Some("foo/bar"));
        assert_eq!(path.file_name().unwrap(), "permission_foo_bar.toml");

        let path = state_file_path(tmp.path(), Some("foo\\bar"));
        assert_eq!(path.file_name().unwrap(), "permission_foo_bar.toml");
    }

    #[test]
    fn sanitize_client_id_prevents_traversal() {
        assert_eq!(sanitize_client_id("foo/../../attack"), "foo_______attack");
        assert_eq!(sanitize_client_id("normal-id"), "normal-id");
        assert_eq!(sanitize_client_id("has\0null"), "has_null");
        assert_eq!(sanitize_client_id("back\\slash"), "back_slash");
    }

    #[tokio::test]
    async fn try_load_state_missing_returns_none() {
        let result = try_load_state(std::path::Path::new("/nonexistent/permission.toml")).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn try_load_state_valid_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("permission.toml");
        let mut expected = PermissionState::default();
        expected.allow_bash_execute = true;
        tokio::fs::write(&path, toml::to_string_pretty(&expected).unwrap())
            .await
            .unwrap();
        let state = try_load_state(&path).await.unwrap();
        assert!(state.allow_bash_execute);
        assert_eq!(
            state.validated_mcp_server_grants_version,
            VALIDATED_MCP_SERVER_GRANTS_VERSION
        );
    }

    #[tokio::test]
    async fn per_client_persist_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        let mut state = PermissionState::default();
        state.allow_bash_execute = true;
        state.allowed_bash_commands.insert("cargo test".to_string());

        persist_state_to_dir(dir, &state, Some("client_a")).await;

        let loaded = load_state_from_dir(dir, Some("client_a")).await;
        assert!(loaded.allow_bash_execute);
        assert!(loaded.allowed_bash_commands.contains("cargo test"));
    }

    #[tokio::test]
    async fn per_client_load_falls_back_to_shared() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        let mut shared_state = PermissionState::default();
        shared_state.allow_bash_execute = true;
        shared_state
            .allowed_bash_commands
            .insert("cargo test".to_string());
        persist_state_to_dir(dir, &shared_state, None).await;

        let loaded = load_state_from_dir(dir, Some("new_client")).await;
        assert!(loaded.allow_bash_execute);
        assert!(loaded.allowed_bash_commands.contains("cargo test"));
    }

    #[tokio::test]
    async fn per_client_file_takes_priority_over_shared() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        let mut shared_state = PermissionState::default();
        shared_state.allow_bash_execute = true;
        persist_state_to_dir(dir, &shared_state, None).await;

        let mut client_state = PermissionState::default();
        client_state.allow_bash_execute = false;
        client_state
            .allowed_bash_commands
            .insert("npm test".to_string());
        persist_state_to_dir(dir, &client_state, Some("my-client")).await;

        let loaded = load_state_from_dir(dir, Some("my-client")).await;
        assert!(!loaded.allow_bash_execute);
        assert!(loaded.allowed_bash_commands.contains("npm test"));

        let shared_loaded = load_state_from_dir(dir, None).await;
        assert!(shared_loaded.allow_bash_execute);
    }

    #[tokio::test]
    async fn load_none_client_returns_default_when_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = load_state_from_dir(tmp.path(), None).await;
        assert!(!loaded.allow_bash_execute);
        assert!(loaded.allowed_bash_commands.is_empty());
    }

    #[tokio::test]
    async fn per_client_isolation_between_clients() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        let mut state_a = PermissionState::default();
        state_a
            .allowed_bash_commands
            .insert("cargo test".to_string());
        persist_state_to_dir(dir, &state_a, Some("client_a")).await;

        let mut state_b = PermissionState::default();
        state_b.allowed_bash_commands.insert("npm test".to_string());
        persist_state_to_dir(dir, &state_b, Some("client_b")).await;

        let loaded_a = load_state_from_dir(dir, Some("client_a")).await;
        assert!(loaded_a.allowed_bash_commands.contains("cargo test"));
        assert!(!loaded_a.allowed_bash_commands.contains("npm test"));

        let loaded_b = load_state_from_dir(dir, Some("client_b")).await;
        assert!(loaded_b.allowed_bash_commands.contains("npm test"));
        assert!(!loaded_b.allowed_bash_commands.contains("cargo test"));
    }

    // ── merge-on-write / concurrent sessions ─────────────────────

    /// Two managers in the same project persist independently; the second
    /// write must not erase grants the first one saved (last-writer-wins).
    #[tokio::test]
    async fn persist_merges_grants_already_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        let mut session_a = PermissionState::default();
        session_a
            .allowed_bash_commands
            .insert("cargo test".to_string());
        session_a
            .allowed_web_fetch_domains
            .insert("docs.rs".to_string());
        persist_state_merging_to_dir(dir, &session_a, None).await;

        // Session B loaded before A persisted, so its snapshot lacks A's grants.
        let mut session_b = PermissionState::default();
        session_b
            .allowed_bash_commands
            .insert("npm test".to_string());
        persist_state_merging_to_dir(dir, &session_b, None).await;

        let loaded = load_state_from_dir(dir, None).await;
        assert!(loaded.allowed_bash_commands.contains("cargo test"));
        assert!(loaded.allowed_bash_commands.contains("npm test"));
        assert!(loaded.allowed_web_fetch_domains.contains("docs.rs"));
    }

    #[tokio::test]
    async fn legacy_fallback_ends_once_root_store_exists() {
        let root = tempfile::tempdir().unwrap();
        let legacy = tempfile::tempdir().unwrap();

        let mut old = PermissionState::default();
        old.allowed_bash_commands.insert("cargo test".to_string());
        persist_state_to_dir(legacy.path(), &old, None).await;

        // No root store yet: the legacy store seeds the load.
        let seeded = load_state_with_fallback(root.path(), Some(legacy.path()), None).await;
        assert!(seeded.allowed_bash_commands.contains("cargo test"));

        // A root write (here: a reset) permanently ends the fallback.
        persist_state_to_dir(root.path(), &PermissionState::default(), None).await;
        let after_reset = load_state_with_fallback(root.path(), Some(legacy.path()), None).await;
        assert!(
            after_reset.allowed_bash_commands.is_empty(),
            "legacy grants must not revive once the root store exists"
        );
    }

    #[test]
    fn merge_grants_unions_sets_and_keeps_scalars() {
        let mut a = PermissionState::default();
        a.allowed_bash_commands.insert("cargo test".to_string());
        a.edit_policy = EditPolicy::Ask;

        let mut b = PermissionState::default();
        b.allowed_bash_commands.insert("npm test".to_string());
        b.disallowed_bash_commands.insert("rm -rf".to_string());
        b.allowed_mcp_tools.insert("linear__get_issue".to_string());
        b.allow_bash_execute = true;
        b.edit_policy = EditPolicy::Allow;

        a.merge_grants_from(b);
        assert!(a.allowed_bash_commands.contains("cargo test"));
        assert!(a.allowed_bash_commands.contains("npm test"));
        assert!(a.disallowed_bash_commands.contains("rm -rf"));
        assert!(a.allowed_mcp_tools.contains("linear__get_issue"));
        assert!(a.allow_bash_execute);
        // Scalar policy keeps the in-memory session's value.
        assert_eq!(a.edit_policy, EditPolicy::Ask);
    }

    /// The MCP/domain deny sets merge additively in both directions, like
    /// `disallowed_bash_commands`: a deny persisted by another session
    /// survives a merge with this session's state and vice versa.
    #[test]
    fn merge_grants_unions_mcp_and_domain_denies_both_directions() {
        let mut a = PermissionState::default();
        a.disallowed_mcp_tools.insert("linear__delete".to_string());
        a.disallowed_web_fetch_domains
            .insert("tracker.example".to_string());

        let mut b = PermissionState::default();
        b.disallowed_mcp_tools.insert("notion__purge".to_string());
        b.disallowed_web_fetch_domains
            .insert("evil.example".to_string());

        let mut a2 = a.clone();
        a2.merge_grants_from(b.clone());
        assert!(a2.disallowed_mcp_tools.contains("linear__delete"));
        assert!(a2.disallowed_mcp_tools.contains("notion__purge"));
        assert!(a2.disallowed_web_fetch_domains.contains("tracker.example"));
        assert!(a2.disallowed_web_fetch_domains.contains("evil.example"));

        let mut b2 = b;
        b2.merge_grants_from(a);
        assert!(b2.disallowed_mcp_tools.contains("linear__delete"));
        assert!(b2.disallowed_mcp_tools.contains("notion__purge"));
        assert!(b2.disallowed_web_fetch_domains.contains("tracker.example"));
        assert!(b2.disallowed_web_fetch_domains.contains("evil.example"));
    }

    // ── repo-root store keying ───────────────────────────────────

    /// A grant accepted at the repo root must be visible to a session started
    /// in a subdirectory of the same repository, and vice versa: both key the
    /// store to the repository root, not the exact cwd.
    #[test]
    fn scope_root_unifies_repo_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        git2::Repository::init(&root).unwrap();
        let sub = root.join("crates/nested");
        std::fs::create_dir_all(&sub).unwrap();

        let root_cwd = AbsPathBuf::new(root.clone()).unwrap();
        let sub_cwd = AbsPathBuf::new(sub).unwrap();
        assert_eq!(permission_scope_root(&root_cwd), root);
        assert_eq!(permission_scope_root(&sub_cwd), root);
        assert_eq!(state_dir_for_cwd(&root_cwd), state_dir_for_cwd(&sub_cwd));
        // The subdirectory keeps a distinct legacy (exact-cwd) location to
        // migrate old grants from; the root has none.
        assert!(legacy_state_dir(&sub_cwd, &state_dir_for_cwd(&sub_cwd)).is_some());
        assert!(legacy_state_dir(&root_cwd, &state_dir_for_cwd(&root_cwd)).is_none());
    }

    /// Linked worktrees must not share a grant store: discovery resolves each
    /// worktree to its own `workdir`, so a grant accepted in one never
    /// auto-allows in another.
    #[test]
    fn scope_root_is_per_worktree() {
        let main = tempfile::tempdir().unwrap();
        let main_root = dunce::canonicalize(main.path()).unwrap();
        let repo = git2::Repository::init(&main_root).unwrap();
        // Worktree creation requires a commit for HEAD to point at.
        {
            let tree_id = repo.index().unwrap().write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = git2::Signature::now("t", "t@example.com").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        let wt_parent = tempfile::tempdir().unwrap();
        // Canonicalize BEFORE creation so git2 records the canonical spelling
        // (macOS tempdirs live behind a /var → /private/var symlink).
        let wt_path = dunce::canonicalize(wt_parent.path()).unwrap().join("wt");
        repo.worktree("wt", &wt_path, None).unwrap();
        let wt_root = wt_path;

        let main_cwd = AbsPathBuf::new(main_root.clone()).unwrap();
        let wt_cwd = AbsPathBuf::new(wt_root.clone()).unwrap();
        assert_eq!(permission_scope_root(&main_cwd), main_root);
        assert_eq!(permission_scope_root(&wt_cwd), wt_root);
        assert_ne!(
            state_dir_for_cwd(&main_cwd),
            state_dir_for_cwd(&wt_cwd),
            "worktrees must keep distinct grant stores"
        );
        // A subdirectory of the worktree still keys to the worktree root.
        let wt_sub = wt_root.join("nested");
        std::fs::create_dir_all(&wt_sub).unwrap();
        assert_eq!(
            permission_scope_root(&AbsPathBuf::new(wt_sub).unwrap()),
            wt_root
        );
    }

    /// A legacy (exact-cwd) store seeds the first load, and the next persist
    /// carries the seeded grants into the scope-root store — after which the
    /// legacy file is dead even if it later gains new content.
    #[tokio::test]
    async fn legacy_grants_migrate_to_root_store_on_persist() {
        let root = tempfile::tempdir().unwrap();
        let legacy = tempfile::tempdir().unwrap();

        let mut old = PermissionState::default();
        old.allowed_bash_commands.insert("cargo test".to_string());
        persist_state_to_dir(legacy.path(), &old, None).await;

        // First load in the repo-root world: seeded from legacy.
        let seeded = load_state_with_fallback(root.path(), Some(legacy.path()), None).await;
        assert!(seeded.allowed_bash_commands.contains("cargo test"));

        // The session's next persist (a new grant) lands the seeded state in
        // the root store.
        let mut session = seeded.clone();
        session.allowed_bash_commands.insert("npm test".to_string());
        persist_state_merging_to_dir(root.path(), &session, None).await;

        let migrated = load_state_with_fallback(root.path(), Some(legacy.path()), None).await;
        assert!(migrated.allowed_bash_commands.contains("cargo test"));
        assert!(migrated.allowed_bash_commands.contains("npm test"));

        // The asserts above would also pass if the loader *merged* legacy in
        // (the resurrection bug the seed-never-merge design avoids), so prove
        // legacy is dead: content added to it after migration must not surface.
        let mut stale = PermissionState::default();
        stale.allowed_bash_commands.insert("stale".to_string());
        persist_state_to_dir(legacy.path(), &stale, None).await;
        let after = load_state_with_fallback(root.path(), Some(legacy.path()), None).await;
        assert!(!after.allowed_bash_commands.contains("stale"));
    }

    #[test]
    fn scope_root_outside_a_repo_stays_the_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dunce::canonicalize(tmp.path()).unwrap();
        let cwd = AbsPathBuf::new(dir.clone()).unwrap();
        // Guard: skip if the system temp dir is itself inside a repo.
        if echo_agent_core::repo::RepoDirChain::resolve(&dir)
            .git_root
            .is_none()
        {
            assert_eq!(permission_scope_root(&cwd), dir);
            assert!(legacy_state_dir(&cwd, &state_dir_for_cwd(&cwd)).is_none());
        }
    }
}
