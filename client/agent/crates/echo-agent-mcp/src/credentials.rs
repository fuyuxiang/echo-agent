//! Persistent credential storage for MCP server OAuth tokens.
//!
//! Credentials are stored in `$ECHO_AGENT_HOME/mcp_credentials.json`, keyed by a
//! composite key derived from the server name and URL. This keeps MCP OAuth
//! tokens isolated from the user's EchoAgent auth (`auth.json`).
//!
//! Stores rmcp's `StoredCredentials` type directly — the same type that
//! rmcp's `AuthorizationManager` uses internally.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::rmcp;

/// Ensure credential paths are owner-only (Unix `0o600`).
///
/// Local helper (not shell-base): `echo-agent-mcp` sits below `config-types` in the
/// dep graph, and shell-base pulls shared→config-types→mcp — a cycle if linked.
/// Windows ACL tightening stays on auth via shell-base; MCP is Unix-first here.
fn ensure_owner_only_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(metadata) => {
                let mode = metadata.permissions().mode();
                if mode & 0o777 != 0o600 {
                    let mut perms = metadata.permissions();
                    perms.set_mode(0o600);
                    std::fs::set_permissions(path, perms)?;
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

type Result<T> = std::result::Result<T, McpCredentialError>;

#[derive(Debug, thiserror::Error)]
pub enum McpCredentialError {
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// File name for the credential store inside `$ECHO_AGENT_HOME`.
const CREDENTIALS_FILENAME: &str = "mcp_credentials.json";

/// On-disk credential store: `$ECHO_AGENT_HOME/mcp_credentials.json`.
///
/// Stores rmcp `StoredCredentials` per MCP server, keyed by
/// `"{server_name}:{server_url}"`.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct McpCredentialStore {
    #[serde(flatten)]
    entries: BTreeMap<String, rmcp::transport::auth::StoredCredentials>,
}

impl std::fmt::Debug for McpCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpCredentialStore")
            .field("entry_count", &self.entries.len())
            .finish()
    }
}

impl McpCredentialStore {
    /// Build the composite key for a credential entry.
    pub fn key(server_name: &str, server_url: &Url) -> String {
        format!("{}:{}", server_name, server_url)
    }

    /// Load the credential store from the default path (`$ECHO_AGENT_HOME/mcp_credentials.json`).
    ///
    /// Returns an empty store if the file does not exist.
    pub fn load_default() -> Result<Self> {
        match Self::default_path() {
            Some(path) => Self::load_from(&path),
            None => Ok(Self::default()),
        }
    }

    /// Load from a specific path.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        // Tighten world-readable credential files on load (hand copies, etc.).
        // Best-effort: chmod failure must not block using existing tokens.
        if let Err(e) = ensure_owner_only_permissions(path) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "mcp credentials: failed to enforce owner-only permissions"
            );
        }
        let store: McpCredentialStore = serde_json::from_str(&content)?;
        Ok(store)
    }

    /// Save the credential store to the default path.
    pub fn save_default(&self) -> Result<()> {
        let path = Self::default_path().ok_or_else(|| {
            McpCredentialError::Other(
                "no user echoagent home (set $ECHO_AGENT_HOME or $HOME)".into(),
            )
        })?;
        self.save_to(&path)
    }

    /// Read-modify-write the **default** store under the cross-process
    /// `mcp_credentials.json.lock` flock: reload from disk (merging concurrent
    /// writers), apply `mutate`, save atomically, and update `self` with the
    /// merged result. On flock failure (non-EINTR error, or non-Unix), falls
    /// back to mutating `self` and saving best-effort — the pre-lock behavior.
    fn locked_mutate_and_save(&mut self, mutate: &dyn Fn(&mut Self)) -> Result<()> {
        let path = Self::default_path().ok_or_else(|| {
            McpCredentialError::Other(
                "no user echoagent home (set $ECHO_AGENT_HOME or $HOME)".into(),
            )
        })?;
        let lock_path = path.with_extension("lock");

        // Ensure parent dir exists.
        if let Some(parent) = lock_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;

            let lock_file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&lock_path)?;
            let fd = lock_file.as_raw_fd();
            loop {
                if unsafe { libc::flock(fd, libc::LOCK_EX) } == 0 {
                    break;
                }
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue; // Retry on EINTR.
                }
                // Lock failed for another reason — fall back to non-atomic write.
                mutate(self);
                return self.save_to(&path);
            }

            // Reload from disk under lock to merge with concurrent writes.
            let mut fresh = Self::load_from(&path).unwrap_or_default();
            mutate(&mut fresh);
            fresh.save_to(&path)?;
            *self = fresh;

            // Lock released when lock_file is dropped.
        }

        #[cfg(not(unix))]
        {
            // No flock on non-unix — best-effort.
            mutate(self);
            self.save_to(&path)?;
        }

        Ok(())
    }

    /// Locked insert ([`Self::locked_mutate_and_save`]) with a freshness
    /// guard: skipped when the disk entry is strictly newer by
    /// `token_received_at` (see [`disk_entry_is_newer`]) — otherwise a slow
    /// writer (canonically a refresh suspended across system sleep that
    /// completes after wake) rolls the stored refresh token back to a
    /// rotated-out value (`invalid_grant` on its next use).
    pub fn insert_and_save(
        &mut self,
        server_name: &str,
        server_url: &url::Url,
        creds: rmcp::transport::auth::StoredCredentials,
    ) -> Result<()> {
        let key = Self::key(server_name, server_url);
        self.locked_mutate_and_save(&move |store: &mut Self| {
            if disk_entry_is_newer(store.entries.get(&key), &creds) {
                tracing::info!(
                    key = key.as_str(),
                    "mcp credentials: skipping stale save (disk entry is newer)"
                );
                return;
            }
            store.entries.insert(key.clone(), creds.clone());
        })
    }

    /// Save to a specific path.
    ///
    /// Writes atomically via temp file + rename to prevent credential loss on
    /// crash. On Unix, the temp file is created with 0600 permissions from the
    /// start (no TOCTOU window where secrets are world-readable).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        let tmp_path = path.with_extension("tmp");

        {
            use std::io::Write;

            #[cfg(unix)]
            let file = {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&tmp_path)?
            };
            #[cfg(not(unix))]
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?;

            let mut writer = std::io::BufWriter::new(file);
            writer.write_all(content.as_bytes())?;
            writer.flush()?;
        }

        // `mode(0o600)` only applies on create; tighten before rename.
        // Fail hard on tmp: credentials are not published yet.
        ensure_owner_only_permissions(&tmp_path)?;
        std::fs::rename(&tmp_path, path)?;
        // Best-effort after rename: new tokens are already published.
        if let Err(e) = ensure_owner_only_permissions(path) {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "mcp: failed to ensure owner-only permissions after credential save"
            );
        }
        Ok(())
    }

    /// Look up credentials for a server.
    pub fn get(
        &self,
        server_name: &str,
        server_url: &Url,
    ) -> Option<&rmcp::transport::auth::StoredCredentials> {
        self.entries.get(&Self::key(server_name, server_url))
    }

    /// Insert rmcp `StoredCredentials` for a server.
    pub fn insert_rmcp(
        &mut self,
        server_name: &str,
        server_url: &Url,
        creds: rmcp::transport::auth::StoredCredentials,
    ) {
        self.entries
            .insert(Self::key(server_name, server_url), creds);
    }

    /// Check if credentials exist for a server (regardless of expiry).
    pub fn has_credentials(&self, server_name: &str, server_url: &Url) -> bool {
        self.entries
            .contains_key(&Self::key(server_name, server_url))
    }

    /// Remove credentials for a server.
    pub fn remove(&mut self, server_name: &str, server_url: &Url) {
        self.entries.remove(&Self::key(server_name, server_url));
    }

    /// Remove a server's credentials and persist, under the cross-process
    /// file lock (reload-merge → remove → atomic save). The locked
    /// counterpart of [`Self::remove`] + [`Self::save_default`] for callers
    /// that persist the removal — an unlocked whole-file rewrite can drop
    /// other processes' concurrent writes for unrelated servers.
    pub fn remove_and_save(&mut self, server_name: &str, server_url: &Url) -> Result<()> {
        let key = Self::key(server_name, server_url);
        self.locked_mutate_and_save(&move |store: &mut Self| {
            store.entries.remove(&key);
        })
    }

    /// Remove all credentials for a server by name (any URL).
    pub fn remove_by_server_name(&mut self, server_name: &str) -> usize {
        let prefix = format!("{server_name}:");
        let before = self.entries.len();
        self.entries.retain(|k, _| !k.starts_with(&prefix));
        before - self.entries.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Default path: `$ECHO_AGENT_HOME/mcp_credentials.json`.
    fn default_path() -> Option<PathBuf> {
        Some(echo_agent_config::user_echo_agent_home()?.join(CREDENTIALS_FILENAME))
    }
}

/// `true` when the on-disk `existing` entry is strictly newer than the
/// `incoming` credentials by `token_received_at` — the [`Self::insert_and_save`]
/// freshness guard. Missing timestamps on either side compare as "not newer"
/// (the write proceeds), preserving pre-guard behavior for expiry-less tokens.
fn disk_entry_is_newer(
    existing: Option<&rmcp::transport::auth::StoredCredentials>,
    incoming: &rmcp::transport::auth::StoredCredentials,
) -> bool {
    match (
        existing.and_then(|e| e.token_received_at),
        incoming.token_received_at,
    ) {
        (Some(existing), Some(incoming)) => existing > incoming,
        _ => false,
    }
}

/// Adapter implementing rmcp's `CredentialStore` trait backed by the on-disk
/// `McpCredentialStore`. Each adapter instance is scoped to a single MCP server
/// (keyed by name + URL); rmcp's `AuthorizationManager` calls load/save/clear
/// transparently during token exchange and refresh.
pub struct McpCredentialStoreAdapter {
    server_name: String,
    server_url: url::Url,
}

impl McpCredentialStoreAdapter {
    pub fn new(server_name: String, server_url: url::Url) -> Self {
        Self {
            server_name,
            server_url,
        }
    }
}

#[async_trait::async_trait]
impl rmcp::transport::auth::CredentialStore for McpCredentialStoreAdapter {
    async fn load(
        &self,
    ) -> std::result::Result<
        Option<rmcp::transport::auth::StoredCredentials>,
        rmcp::transport::auth::AuthError,
    > {
        let name = self.server_name.clone();
        let url = self.server_url.clone();
        tokio::task::spawn_blocking(move || {
            let store = McpCredentialStore::load_default()
                .map_err(|e| rmcp::transport::auth::AuthError::InternalError(e.to_string()))?;
            Ok(store.get(&name, &url).cloned())
        })
        .await
        .map_err(|e| rmcp::transport::auth::AuthError::InternalError(e.to_string()))?
    }

    async fn save(
        &self,
        credentials: rmcp::transport::auth::StoredCredentials,
    ) -> std::result::Result<(), rmcp::transport::auth::AuthError> {
        let name = self.server_name.clone();
        let url = self.server_url.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = McpCredentialStore::load_default().unwrap_or_default();
            store
                .insert_and_save(&name, &url, credentials)
                .map_err(|e| rmcp::transport::auth::AuthError::InternalError(e.to_string()))
        })
        .await
        .map_err(|e| rmcp::transport::auth::AuthError::InternalError(e.to_string()))?
    }

    async fn clear(&self) -> std::result::Result<(), rmcp::transport::auth::AuthError> {
        let name = self.server_name.clone();
        let url = self.server_url.clone();
        tokio::task::spawn_blocking(move || {
            // Under the same flock as `insert_and_save`: this is a whole-file
            // read-modify-write, and an unlocked snapshot here could silently
            // drop *other servers'* entries written concurrently by another
            // process (their just-rotated refresh tokens with them).
            let mut store = McpCredentialStore::load_default().unwrap_or_default();
            store
                .remove_and_save(&name, &url)
                .map_err(|e| rmcp::transport::auth::AuthError::InternalError(e.to_string()))
        })
        .await
        .map_err(|e| rmcp::transport::auth::AuthError::InternalError(e.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_stored_creds(client_id: &str) -> rmcp::transport::auth::StoredCredentials {
        rmcp::transport::auth::StoredCredentials::new(client_id.to_string(), None, Vec::new(), None)
    }

    #[test]
    fn insert_and_get() {
        let mut store = McpCredentialStore::default();
        let url = Url::parse("https://test.example.com/mcp").unwrap();
        store.insert_rmcp("test", &url, test_stored_creds("test-client"));
        assert!(store.get("test", &url).is_some());
        assert_eq!(store.get("test", &url).unwrap().client_id, "test-client");
    }

    #[test]
    fn remove_entry() {
        let mut store = McpCredentialStore::default();
        let url = Url::parse("https://test.example.com/mcp").unwrap();
        store.insert_rmcp("test", &url, test_stored_creds("test-client"));
        store.remove("test", &url);
        assert!(store.get("test", &url).is_none());
    }

    #[test]
    fn has_credentials() {
        let mut store = McpCredentialStore::default();
        let url = Url::parse("https://test.example.com/mcp").unwrap();
        assert!(!store.has_credentials("test", &url));
        store.insert_rmcp("test", &url, test_stored_creds("c"));
        assert!(store.has_credentials("test", &url));
    }

    #[test]
    fn roundtrip_serialization() {
        let mut store = McpCredentialStore::default();
        let url = Url::parse("https://test.example.com/mcp").unwrap();
        store.insert_rmcp("test", &url, test_stored_creds("test-client"));

        let json = serde_json::to_string(&store).unwrap();
        let loaded: McpCredentialStore = serde_json::from_str(&json).unwrap();
        assert!(loaded.get("test", &url).is_some());
    }

    /// Raw JSON fixture in the exact shape rmcp 0.17 persisted to
    /// `$ECHO_AGENT_HOME/mcp_credentials.json`. Existing credential files must keep
    /// loading across rmcp upgrades (2.1's `OAuthTokenResponse` gained vendor
    /// extra token fields), so this must be a string literal — never JSON
    /// serialized by the current code.
    #[test]
    fn legacy_on_disk_fixture_still_deserializes() {
        use oauth2::TokenResponse as _;

        let fixture = r#"{
            "linear:https://mcp.example.com/mcp": {
                "client_id": "legacy-client-id",
                "token_response": {
                    "access_token": "at-123",
                    "token_type": "bearer",
                    "expires_in": 3600,
                    "refresh_token": "rt-456",
                    "scope": "read write"
                },
                "granted_scopes": ["read", "write"],
                "token_received_at": 1730000000
            },
            "noauth:https://example.com/mcp": {
                "client_id": "c2",
                "token_response": null
            }
        }"#;

        let store: McpCredentialStore = serde_json::from_str(fixture).unwrap();
        let url = Url::parse("https://mcp.example.com/mcp").unwrap();
        let creds = store.get("linear", &url).expect("legacy entry loads");
        assert_eq!(creds.client_id, "legacy-client-id");
        let token = creds.token_response.as_ref().expect("token loads");
        assert_eq!(token.access_token().secret(), "at-123");
        assert_eq!(token.refresh_token().unwrap().secret(), "rt-456");
        assert_eq!(creds.granted_scopes, vec!["read", "write"]);
        assert_eq!(creds.token_received_at, Some(1730000000));

        // Entry without the `#[serde(default)]` fields on disk still loads.
        let url2 = Url::parse("https://example.com/mcp").unwrap();
        let creds2 = store.get("noauth", &url2).expect("minimal entry loads");
        assert!(creds2.token_response.is_none());
        assert!(creds2.granted_scopes.is_empty());
        assert!(creds2.token_received_at.is_none());

        // Round-trip through the current serializer and reload.
        let json = serde_json::to_string(&store).unwrap();
        let reloaded: McpCredentialStore = serde_json::from_str(&json).unwrap();
        let re = reloaded
            .get("linear", &url)
            .expect("round-trip keeps entry");
        assert_eq!(re.client_id, "legacy-client-id");
        let re_token = re.token_response.as_ref().expect("round-trip keeps token");
        assert_eq!(re_token.access_token().secret(), "at-123");
        assert_eq!(re_token.refresh_token().unwrap().secret(), "rt-456");
        assert_eq!(re.granted_scopes, vec!["read", "write"]);
        assert_eq!(re.token_received_at, Some(1730000000));
    }

    #[test]
    fn save_and_load_from_file() {
        let dir = std::env::temp_dir().join("echo-agent-mcp-credentials-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_creds.json");

        let mut store = McpCredentialStore::default();
        let url = Url::parse("https://test.example.com/mcp").unwrap();
        store.insert_rmcp("test", &url, test_stored_creds("test-client"));
        store.save_to(&path).unwrap();

        let loaded = McpCredentialStore::load_from(&path).unwrap();
        assert!(loaded.get("test", &url).is_some());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn save_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let mut store = McpCredentialStore::default();
        let url = Url::parse("https://test.example.com/mcp").unwrap();
        store.insert_rmcp("test", &url, test_stored_creds("c"));
        store.save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn load_tightens_world_readable_credentials() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let mut store = McpCredentialStore::default();
        let url = Url::parse("https://test.example.com/mcp").unwrap();
        store.insert_rmcp("test", &url, test_stored_creds("c"));
        store.save_to(&path).unwrap();
        let mut loose = std::fs::metadata(&path).unwrap().permissions();
        loose.set_mode(0o644);
        std::fs::set_permissions(&path, loose).unwrap();

        let _ = McpCredentialStore::load_from(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// The `insert_and_save` freshness guard: a save older (by
    /// `token_received_at`) than the on-disk entry must be skipped.
    #[test]
    fn stale_save_does_not_clobber_newer_disk_entry() {
        // `StoredCredentials` is #[non_exhaustive]; construct via `new` and
        // set the (public) timestamp field afterwards.
        let mut older = test_stored_creds("c");
        older.token_received_at = Some(1_000);
        let mut newer = test_stored_creds("c");
        newer.token_received_at = Some(2_000);
        let no_ts = test_stored_creds("c");

        assert!(
            disk_entry_is_newer(Some(&newer), &older),
            "older incoming vs newer disk → skip the write"
        );
        assert!(
            !disk_entry_is_newer(Some(&older), &newer),
            "newer incoming vs older disk → write proceeds"
        );
        assert!(
            !disk_entry_is_newer(Some(&older), &older),
            "equal timestamps → write proceeds (idempotent re-save)"
        );
        assert!(
            !disk_entry_is_newer(None, &older),
            "no disk entry → write proceeds"
        );
        assert!(
            !disk_entry_is_newer(Some(&newer), &no_ts),
            "timestamp-less incoming keeps pre-guard behavior (writes)"
        );
        assert!(
            !disk_entry_is_newer(Some(&no_ts), &older),
            "timestamp-less disk entry keeps pre-guard behavior (writes)"
        );
    }

    /// The refresh-failure classifier that gates browser escalation
    /// (`force_reauth`): network-level failures — the `oauth2` crate's
    /// `Display` for request/parse errors — are transient; IdP rejections and
    /// missing-credential states stay terminal (escalate, as before).
    #[test]
    fn refresh_failure_transient_classification() {
        use crate::servers::mcp_refresh_failure_is_transient;
        use rmcp::transport::auth::AuthError;

        // oauth2 RequestTokenError::Request renders exactly "Request failed".
        assert!(mcp_refresh_failure_is_transient(
            &AuthError::TokenRefreshFailed("Request failed".into())
        ));
        // 5xx/proxy bodies that aren't OAuth JSON parse-fail.
        assert!(mcp_refresh_failure_is_transient(
            &AuthError::TokenRefreshFailed("Failed to parse server response".into())
        ));

        // IdP rejections carry the RFC 6749 code → terminal.
        assert!(!mcp_refresh_failure_is_transient(
            &AuthError::TokenRefreshFailed(
                "Server returned error response: invalid_grant: token revoked".into()
            )
        ));
        // No refresh token at all → only the browser flow can help.
        assert!(!mcp_refresh_failure_is_transient(
            &AuthError::TokenRefreshFailed("No refresh token available".into())
        ));
        // Empty credential store → interactive auth required.
        assert!(!mcp_refresh_failure_is_transient(
            &AuthError::AuthorizationRequired
        ));
    }
}
