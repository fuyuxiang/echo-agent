//! Hub [`AuthProvider`] from `~/.echo-agent/auth.json` for the standalone
//! `workspace_server` binary: loopback `ws://` uses a plain bearer, otherwise
//! an auto-refreshing OIDC provider that persists rotated tokens to disk.
//!
//! The in-leader `echoagent workspace` exposure does NOT use this path — it sources
//! an in-memory provider from the leader's `AuthManager` (see
//! `LeaderAuthProvider`) to avoid racing the leader's own auth.json writer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use echo_agent_computer_hub_sdk::{
    AuthCredential, AuthIdentity, AuthProvider, OidcAuthProviderBuilder, OnRefreshCallback,
    RefreshEvent,
};
use url::Url;

use crate::status_config::ProactiveRefreshConfig;

mod proactive;

pub use proactive::{ProactiveOidcAuthProvider, ProactiveOidcParams};

pub(crate) fn init_metrics() {
    proactive::init_metrics();
}

/// Plain bearer provider that also carries the owner identity parsed from the
/// same auth.json entry. Used for the loopback / local-dev path (no OIDC
/// refresh) so the workspace can still derive `WorkspaceIdentity` from the auth
/// provider — without a second auth.json read.
struct BearerWithIdentity {
    token: String,
    identity: AuthIdentity,
}

impl std::fmt::Debug for BearerWithIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log the bearer token; surface only the (non-secret) identity.
        f.debug_struct("BearerWithIdentity")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl AuthProvider for BearerWithIdentity {
    fn current(&self) -> AuthCredential {
        AuthCredential::bearer(self.token.clone())
    }

    fn identity(&self) -> Option<AuthIdentity> {
        Some(self.identity.clone())
    }
}

/// Owner identity parsed from an auth.json entry, for the [`AuthProvider`]s
/// built here to surface via [`AuthProvider::identity`].
fn identity_from_entry(entry: &AuthEntry) -> AuthIdentity {
    AuthIdentity {
        user_id: entry.user_id.clone(),
        principal_type: entry.principal_type.clone(),
        principal_id: entry.principal_id.clone(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct AuthEntry {
    key: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    oidc_issuer: Option<String>,
    #[serde(default)]
    oidc_client_id: Option<String>,
    #[serde(default)]
    principal_type: Option<String>,
    #[serde(default)]
    principal_id: Option<String>,
    #[serde(default)]
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub fn default_auth_path() -> anyhow::Result<PathBuf> {
    let echoagent = echo_agent_config::user_echo_agent_home()
        .ok_or_else(|| anyhow::anyhow!("no user echoagent home (set $ECHO_AGENT_HOME or $HOME)"))?;
    Ok(echoagent.join("auth.json"))
}

/// Read the active OIDC entry and its scope key. The key is threaded to the
/// refresh write so rotation updates exactly the entry that was read.
///
/// When several OIDC entries qualify, pick the **latest `expires_at`** — the
/// entry the shell is actively refreshing. The previous first-key selection
/// was alphabetical and could rotate a *different principal's* RT chain than
/// the one the user's sessions use.
fn read_auth_entry(path: &Path) -> anyhow::Result<(String, AuthEntry)> {
    if !path.exists() {
        anyhow::bail!(
            "No auth credentials found at {}. Run `echoagent login` first.",
            path.display()
        );
    }

    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    let entries: BTreeMap<String, AuthEntry> = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;

    entries
        .into_iter()
        .filter(|(_, e)| e.refresh_token.is_some() && e.oidc_issuer.is_some())
        // Strictly-greater comparison: ties (including all-`None`) keep the
        // first candidate in BTreeMap (alphabetical) order, so single-entry
        // and legacy no-`expires_at` files behave exactly as before.
        .fold(None::<(String, AuthEntry)>, |best, cand| match best {
            Some(b) if cand.1.expires_at <= b.1.expires_at => Some(b),
            _ => Some(cand),
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no OIDC auth entry found in {}. Run `echoagent login` first.",
                path.display()
            )
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OidcProviderKind {
    Sdk,
    Proactive,
}

/// Writes `auth.json` on the calling thread. The proactive provider already
/// offloads this onto its seq-guarded persist worker; a nested spawn here
/// would run `write_refreshed_token` *after* the seq check and reopen the
/// stale-clobber race.
pub(crate) fn persist_on_refresh(auth_path: PathBuf, scope_key: String) -> OnRefreshCallback {
    Arc::new(move |event: &RefreshEvent| {
        if let Err(e) = write_refreshed_token(&auth_path, &scope_key, event) {
            tracing::warn!(error = %e, "failed to persist refreshed token to auth.json");
        }
    })
}

/// SDK `on_refresh` is invoked from the async refresh path, which has no
/// PersistGate. Offload the same write so a contended flock cannot stall
/// the runtime.
fn persist_on_refresh_off_thread(auth_path: PathBuf, scope_key: String) -> OnRefreshCallback {
    let persist = persist_on_refresh(auth_path, scope_key);
    Arc::new(move |event: &RefreshEvent| {
        let persist = persist.clone();
        let event = event.clone();
        std::thread::spawn(move || persist(&event));
    })
}

fn build_oidc_provider(
    scope_key: String,
    entry: &AuthEntry,
    auth_path: PathBuf,
    refresh_cfg: &ProactiveRefreshConfig,
) -> anyhow::Result<(Arc<dyn AuthProvider>, OidcProviderKind)> {
    let refresh_token = entry.refresh_token.as_ref().ok_or_else(|| {
        anyhow::anyhow!("auth entry has no refresh_token — cannot refresh expired tokens")
    })?;
    let issuer = entry.oidc_issuer.as_ref().ok_or_else(|| {
        anyhow::anyhow!("auth entry has no oidc_issuer — cannot refresh expired tokens")
    })?;
    let client_id = entry.oidc_client_id.as_ref().ok_or_else(|| {
        anyhow::anyhow!("auth entry has no oidc_client_id — cannot refresh expired tokens")
    })?;

    if refresh_cfg.enabled {
        return Ok((
            Arc::new(ProactiveOidcAuthProvider::new(ProactiveOidcParams {
                access_token: entry.key.clone(),
                refresh_token: refresh_token.clone(),
                issuer: issuer.clone(),
                client_id: client_id.clone(),
                identity: identity_from_entry(entry),
                expires_at: entry.expires_at,
                refresh: refresh_cfg.clone(),
                on_refresh: Some(persist_on_refresh(auth_path, scope_key)),
            })),
            OidcProviderKind::Proactive,
        ));
    }

    let mut builder = OidcAuthProviderBuilder::new(&entry.key, refresh_token, issuer, client_id);

    // Owner identity is surfaced via `AuthProvider::identity()` so the workspace
    // derives `WorkspaceIdentity` from this provider — no separate auth.json read.
    builder = builder.user_id(&entry.user_id);
    if let Some(ref pt) = entry.principal_type {
        builder = builder.principal_type(pt);
    }
    if let Some(ref pid) = entry.principal_id {
        builder = builder.principal_id(pid);
    }
    if let Some(exp) = entry.expires_at {
        builder = builder.expires_at(exp);
    }
    builder = builder.on_refresh(persist_on_refresh_off_thread(auth_path, scope_key));

    Ok((Arc::new(builder.build()), OidcProviderKind::Sdk))
}

/// How long [`lock_auth_file`] polls for the shared `auth.json.lock` before
/// skipping the persist. Covers the shell's normal refresh hold (~1 s); its
/// worst-case 45 s budget is deliberately not waited out — losing one persist
/// is recoverable (see [`write_refreshed_token`]), stalling the persist thread
/// for a minute is not worth it.
const AUTH_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// RAII flock on the sibling `auth.json.lock` — the same advisory lock every
/// echo-agent-shell `auth.json` writer takes. Polling `try_lock` rather than a
/// blocking `flock` to bound the wait; never breaks a held lock (a stale
/// holder here would be the shell mid-refresh, exactly the writer we must not
/// race).
struct AuthFileLockGuard {
    _file: std::fs::File,
}

fn lock_auth_file(auth_json_path: &Path) -> Option<AuthFileLockGuard> {
    use fs2::FileExt;
    use std::io::Write;
    let lock_path = auth_json_path.with_file_name("auth.json.lock");
    let deadline = std::time::Instant::now() + AUTH_LOCK_TIMEOUT;
    loop {
        // The shell's stale-lock recovery breaks locks by unlink+recreate, so
        // only a flock on the live inode counts (mirrors the shell's own
        // acquire path). A dead inode falls through to the same deadline and
        // sleep as a busy lock — retrying without them spins this thread for
        // as long as the check keeps failing.
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            && file.try_lock_exclusive().is_ok()
            && lock_inode_is_live(&file, &lock_path)
        {
            // Holder info (`PID:TS`) through the locked fd, so the shell can
            // identify (and, if this process dies, break) our hold.
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let _ = file.set_len(0);
            let _ = write!(file, "{}:{ts}", std::process::id());
            let _ = file.sync_all();
            return Some(AuthFileLockGuard { _file: file });
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// `fstat(fd)` vs `stat(path)`: `false` when the locked file was concurrently
/// unlinked and recreated (our flock would be on the dead inode).
#[cfg(unix)]
fn lock_inode_is_live(file: &std::fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(fd), Ok(p)) => fd.ino() == p.ino() && fd.dev() == p.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn lock_inode_is_live(_file: &std::fs::File, _path: &Path) -> bool {
    true
}

pub(crate) fn write_refreshed_token(
    path: &Path,
    scope_key: &str,
    event: &RefreshEvent,
) -> anyhow::Result<()> {
    // Read-modify-write under the shared advisory lock. Writing unlocked here
    // raced the shell's own refresh writer: whichever wrote second silently
    // rolled back the other's freshly rotated refresh token on disk — a
    // guaranteed future `invalid_grant` for every session sharing the file.
    let Some(_lock) = lock_auth_file(path) else {
        // The rotated token still serves this process from memory, so warn
        // rather than fail — but disk now trails the IdP by one rotation, and
        // a fresh process that picks it up will present a spent token.
        tracing::warn!(
            timeout = ?AUTH_LOCK_TIMEOUT,
            "auth.json.lock busy; skipping refreshed-token persist (disk left one rotation behind)"
        );
        return Ok(());
    };

    let content = std::fs::read_to_string(path)?;
    let mut raw: serde_json::Value = serde_json::from_str(&content)?;

    let Some(obj) = raw.get_mut(scope_key).and_then(|e| e.as_object_mut()) else {
        anyhow::bail!("auth entry '{scope_key}' not found while persisting refreshed token");
    };

    // Never roll disk back to an older token. Each refresh persists on its own
    // thread and a sibling shell writes the same file, so writes can arrive out
    // of order; the loser would replace a live refresh token with a spent one
    // and guarantee a future `invalid_grant`.
    if let Some(new_expiry) = event.expires_at
        && let Some(disk_expiry) = obj
            .get("expires_at")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        && disk_expiry.with_timezone(&chrono::Utc) >= new_expiry
    {
        tracing::debug!("auth.json already holds a same-or-newer token; skipping persist");
        return Ok(());
    }

    obj.insert(
        "key".to_owned(),
        serde_json::Value::String(event.access_token.clone()),
    );
    if let Some(ref rt) = event.new_refresh_token {
        obj.insert(
            "refresh_token".to_owned(),
            serde_json::Value::String(rt.clone()),
        );
    }
    if let Some(exp) = event.expires_at {
        obj.insert(
            "expires_at".to_owned(),
            serde_json::Value::String(exp.to_rfc3339()),
        );
    }

    write_json_atomic(path, &raw)?;
    tracing::info!(path = %path.display(), "persisted refreshed token to auth.json");
    Ok(())
}

/// Atomically replace `path`: temp file (0600 on Unix) + fsync + rename. Avoids
/// the truncate-in-place corruption window when the long-lived binary rewrites
/// auth.json.
fn write_json_atomic(path: &Path, value: &serde_json::Value) -> anyhow::Result<()> {
    use std::io::Write;

    let json = serde_json::to_string_pretty(value)?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    let mut file = opts
        .open(&tmp)
        .map_err(|e| anyhow::anyhow!("failed to open {}: {e}", tmp.display()))?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    drop(file);

    #[cfg(windows)]
    let _ = std::fs::remove_file(path);

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow::anyhow!("failed to replace {}: {e}", path.display()));
    }
    Ok(())
}

/// Build a hub auth provider for `hub_url`. `auth_config` overrides
/// the default credential path (`~/.echo-agent/auth.json`).
///
/// `refresh_cfg.enabled` selects the workspace-owned proactive refresher;
/// when off (the default) this is the SDK `OidcAuthProvider`. Loopback
/// `ws://` ignores the flag and stays on a static bearer.
pub fn provider(
    hub_url: &Url,
    auth_config: Option<&Path>,
    refresh_cfg: &ProactiveRefreshConfig,
) -> anyhow::Result<Arc<dyn AuthProvider>> {
    let auth_path = match auth_config {
        Some(p) => p.to_path_buf(),
        None => default_auth_path()?,
    };
    let (scope_key, entry) = read_auth_entry(&auth_path)?;

    let is_loopback = hub_url.scheme() == "ws"
        && matches!(hub_url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));

    if is_loopback {
        tracing::info!("Using local-dev auth (loopback hub)");
        Ok(Arc::new(BearerWithIdentity {
            identity: identity_from_entry(&entry),
            token: entry.key.clone(),
        }))
    } else {
        build_oidc_provider(scope_key, &entry, auth_path, refresh_cfg).map(|(provider, _)| provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_auth_json(dir: &std::path::Path, json: &str) -> PathBuf {
        let path = dir.join("auth.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(json.as_bytes()).unwrap();
        path
    }

    #[test]
    fn read_auth_entry_picks_oidc_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{
            "legacy": { "key": "echo-agent-plainkey", "user_id": "u1" },
            "oidc": {
                "key": "eyJhbGciOiJFUzI1NiJ9.test",
                "user_id": "u2",
                "refresh_token": "rt",
                "oidc_issuer": "https://auth.example.com",
                "oidc_client_id": "client1"
            }
        }"#,
        );

        let (key, entry) = read_auth_entry(&path).unwrap();
        assert_eq!(key, "oidc");
        assert_eq!(entry.refresh_token.as_deref(), Some("rt"));
        assert_eq!(
            entry.oidc_issuer.as_deref(),
            Some("https://auth.example.com")
        );
    }

    #[test]
    fn read_auth_entry_rejects_non_oidc() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{
            "api_key": { "key": "echo-agent-plainkey", "user_id": "u1" }
        }"#,
        );

        let err = read_auth_entry(&path).unwrap_err();
        assert!(err.to_string().contains("no OIDC auth entry"));
    }

    #[test]
    fn read_auth_entry_missing_file() {
        let path = PathBuf::from("/nonexistent/auth.json");
        let err = read_auth_entry(&path).unwrap_err();
        assert!(err.to_string().contains("No auth credentials"));
    }

    #[test]
    fn read_auth_entry_tolerates_extra_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{
            "scope": {
                "key": "eyJhbGciOiJFUzI1NiJ9.tok",
                "user_id": "u1",
                "auth_mode": "oidc",
                "create_time": "2026-01-01T00:00:00Z",
                "email": "test@example.com",
                "first_name": "Test",
                "refresh_token": "rt1",
                "oidc_issuer": "https://auth.echo-agent.invalid",
                "oidc_client_id": "c1",
                "some_future_field": true
            }
        }"#,
        );

        let (_key, entry) = read_auth_entry(&path).unwrap();
        assert_eq!(entry.refresh_token.as_deref(), Some("rt1"));
    }

    #[test]
    fn build_oidc_provider_requires_refresh_token() {
        let entry = AuthEntry {
            key: "eyJ.tok".into(),
            user_id: "u1".into(),
            refresh_token: None,
            oidc_issuer: Some("https://auth.echo-agent.invalid".into()),
            oidc_client_id: Some("c1".into()),
            principal_type: None,
            principal_id: None,
            expires_at: None,
        };
        let err = build_oidc_provider(
            "oidc".into(),
            &entry,
            PathBuf::from("/tmp/x"),
            &ProactiveRefreshConfig::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("refresh_token"));
    }

    #[test]
    fn build_oidc_provider_requires_issuer() {
        let entry = AuthEntry {
            key: "eyJ.tok".into(),
            user_id: "u1".into(),
            refresh_token: Some("rt".into()),
            oidc_issuer: None,
            oidc_client_id: Some("c1".into()),
            principal_type: None,
            principal_id: None,
            expires_at: None,
        };
        let err = build_oidc_provider(
            "oidc".into(),
            &entry,
            PathBuf::from("/tmp/x"),
            &ProactiveRefreshConfig::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("oidc_issuer"));
    }

    #[test]
    fn build_oidc_provider_requires_client_id() {
        let entry = AuthEntry {
            key: "eyJ.tok".into(),
            user_id: "u1".into(),
            refresh_token: Some("rt".into()),
            oidc_issuer: Some("https://auth.echo-agent.invalid".into()),
            oidc_client_id: None,
            principal_type: None,
            principal_id: None,
            expires_at: None,
        };
        let err = build_oidc_provider(
            "oidc".into(),
            &entry,
            PathBuf::from("/tmp/x"),
            &ProactiveRefreshConfig::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("oidc_client_id"));
    }

    #[test]
    fn build_oidc_provider_succeeds_with_all_fields() {
        let entry = AuthEntry {
            key: "eyJ.tok".into(),
            user_id: "u1".into(),
            refresh_token: Some("rt".into()),
            oidc_issuer: Some("https://auth.echo-agent.invalid".into()),
            oidc_client_id: Some("c1".into()),
            principal_type: Some("Team".into()),
            principal_id: Some("t1".into()),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        };
        let (provider, kind) = build_oidc_provider(
            "oidc".into(),
            &entry,
            PathBuf::from("/tmp/x"),
            &ProactiveRefreshConfig::default(),
        )
        .unwrap();
        assert_eq!(kind, OidcProviderKind::Sdk);
        let cred = provider.current();
        match cred {
            echo_agent_computer_hub_sdk::AuthCredential::Bearer { token } => {
                assert_eq!(token, "eyJ.tok");
            }
            _ => panic!("expected Bearer"),
        }
        // Identity is surfaced from the parsed entry (no second auth.json read).
        let id = provider.identity().expect("identity present");
        assert_eq!(id.user_id, "u1");
        assert_eq!(id.principal_type.as_deref(), Some("Team"));
        assert_eq!(id.principal_id.as_deref(), Some("t1"));
    }

    #[test]
    fn write_refreshed_token_updates_jwt_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{
            "legacy": { "key": "echo-agent-old", "user_id": "u1" },
            "oidc": { "key": "eyJ.old", "user_id": "u2", "refresh_token": "rt-old", "oidc_issuer": "https://auth.echo-agent.invalid" }
        }"#,
        );

        let event = RefreshEvent {
            access_token: "eyJ.new".into(),
            new_refresh_token: Some("rt-new".into()),
            expires_at: None,
        };
        write_refreshed_token(&path, "oidc", &event).unwrap();

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(updated["oidc"]["key"], "eyJ.new");
        assert_eq!(updated["oidc"]["refresh_token"], "rt-new");
        assert_eq!(updated["legacy"]["key"], "echo-agent-old"); // untouched
    }

    /// With several OIDC entries (personal + enterprise login), the one with
    /// the latest `expires_at` wins — that's the entry the user's echoagent
    /// sessions actively refresh. Alphabetical-order selection could adopt a
    /// different principal's refresh token and rotate it out from under the
    /// shell.
    #[test]
    fn read_auth_entry_prefers_latest_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{
            "aaa-stale": { "key": "eyJ.a", "refresh_token": "rt-a", "oidc_issuer": "https://auth.echo-agent.invalid", "expires_at": "2026-01-01T00:00:00Z" },
            "zzz-active": { "key": "eyJ.z", "refresh_token": "rt-z", "oidc_issuer": "https://auth.echo-agent.invalid", "expires_at": "2026-06-01T00:00:00Z" }
        }"#,
        );

        let (key, entry) = read_auth_entry(&path).unwrap();
        assert_eq!(key, "zzz-active", "latest expires_at must win");
        assert_eq!(entry.refresh_token.as_deref(), Some("rt-z"));

        // An entry with no expires_at never beats one with a timestamp.
        let path = write_auth_json(
            dir.path(),
            r#"{
            "aaa-with-expiry": { "key": "eyJ.a", "refresh_token": "rt-a", "oidc_issuer": "https://auth.echo-agent.invalid", "expires_at": "2026-01-01T00:00:00Z" },
            "zzz-no-expiry": { "key": "eyJ.z", "refresh_token": "rt-z", "oidc_issuer": "https://auth.echo-agent.invalid" }
        }"#,
        );
        let (key, _) = read_auth_entry(&path).unwrap();
        assert_eq!(key, "aaa-with-expiry");
    }

    #[test]
    fn write_refreshed_token_targets_exact_scope_key() {
        // Non-sorted order: refresh must update the read-selected key ("aaa"),
        // not the first in file order ("zzz").
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{
            "zzz": { "key": "eyJ.z", "refresh_token": "rt-z", "oidc_issuer": "https://auth.echo-agent.invalid" },
            "aaa": { "key": "eyJ.a", "refresh_token": "rt-a", "oidc_issuer": "https://auth.echo-agent.invalid" }
        }"#,
        );

        let (key, _entry) = read_auth_entry(&path).unwrap();
        assert_eq!(key, "aaa");

        let event = RefreshEvent {
            access_token: "eyJ.a-new".into(),
            new_refresh_token: Some("rt-a-new".into()),
            expires_at: None,
        };
        write_refreshed_token(&path, &key, &event).unwrap();

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(updated["aaa"]["key"], "eyJ.a-new");
        assert_eq!(updated["aaa"]["refresh_token"], "rt-a-new");
        assert_eq!(updated["zzz"]["key"], "eyJ.z");
        assert_eq!(updated["zzz"]["refresh_token"], "rt-z");
    }

    /// Persists run on detached threads and race a sibling shell writing the
    /// same file, so a late write must not replace a live refresh token with
    /// the one it already rotated away.
    #[test]
    fn write_refreshed_token_does_not_roll_back_a_newer_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{
            "oidc": { "key": "eyJ.newer", "refresh_token": "rt-newer", "oidc_issuer": "https://auth.echo-agent.invalid", "expires_at": "2026-06-01T00:00:00Z" }
        }"#,
        );

        let stale = RefreshEvent {
            access_token: "eyJ.older".into(),
            new_refresh_token: Some("rt-older".into()),
            expires_at: Some("2026-05-01T00:00:00Z".parse().unwrap()),
        };
        write_refreshed_token(&path, "oidc", &stale).unwrap();

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(updated["oidc"]["refresh_token"], "rt-newer");
        assert_eq!(updated["oidc"]["key"], "eyJ.newer");
    }

    #[test]
    fn write_refreshed_token_preserves_existing_rt_when_not_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{
            "oidc": { "key": "eyJ.old", "user_id": "u1", "refresh_token": "rt-keep", "oidc_issuer": "https://auth.echo-agent.invalid" }
        }"#,
        );

        let event = RefreshEvent {
            access_token: "eyJ.new".into(),
            new_refresh_token: None,
            expires_at: None,
        };
        write_refreshed_token(&path, "oidc", &event).unwrap();

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(updated["oidc"]["key"], "eyJ.new");
        assert_eq!(updated["oidc"]["refresh_token"], "rt-keep");
    }

    #[test]
    fn provider_loopback_uses_bearer() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{ "oidc": { "key": "eyJ.tok", "user_id": "u1", "refresh_token": "rt", "oidc_issuer": "https://auth.echo-agent.invalid", "oidc_client_id": "c1" } }"#,
        );
        let url = Url::parse("ws://localhost:9988/v1/tools").unwrap();
        let auth = provider(&url, Some(&path), &ProactiveRefreshConfig::default()).unwrap();
        match auth.current() {
            AuthCredential::Bearer { token } => assert_eq!(token, "eyJ.tok"),
            _ => panic!("expected Bearer"),
        }
        // Loopback still surfaces identity from the same entry.
        let id = auth.identity().expect("loopback identity present");
        assert_eq!(id.user_id, "u1");
    }

    fn complete_oidc_entry() -> AuthEntry {
        AuthEntry {
            key: "eyJ.tok".into(),
            user_id: "u1".into(),
            refresh_token: Some("rt".into()),
            oidc_issuer: Some("https://auth.echo-agent.invalid".into()),
            oidc_client_id: Some("c1".into()),
            principal_type: Some("Team".into()),
            principal_id: Some("t1".into()),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        }
    }

    #[test]
    fn build_oidc_provider_flag_off_uses_sdk_provider() {
        let (provider, kind) = build_oidc_provider(
            "oidc".into(),
            &complete_oidc_entry(),
            PathBuf::from("/tmp/x"),
            &ProactiveRefreshConfig::default(),
        )
        .unwrap();
        assert_eq!(kind, OidcProviderKind::Sdk);
        match provider.current() {
            AuthCredential::Bearer { token } => assert_eq!(token, "eyJ.tok"),
            _ => panic!("expected Bearer"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn build_oidc_provider_flag_on_uses_proactive_provider() {
        let refresh = ProactiveRefreshConfig {
            enabled: true,
            ..ProactiveRefreshConfig::default()
        };
        let (provider, kind) = build_oidc_provider(
            "oidc".into(),
            &complete_oidc_entry(),
            PathBuf::from("/tmp/x"),
            &refresh,
        )
        .unwrap();
        assert_eq!(kind, OidcProviderKind::Proactive);
        match provider.current() {
            AuthCredential::Bearer { token } => assert_eq!(token, "eyJ.tok"),
            _ => panic!("expected Bearer"),
        }
    }

    #[test]
    fn provider_loopback_ignores_proactive_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{ "oidc": { "key": "eyJ.tok", "user_id": "u1", "refresh_token": "rt", "oidc_issuer": "https://auth.echo-agent.invalid", "oidc_client_id": "c1" } }"#,
        );
        let refresh = ProactiveRefreshConfig {
            enabled: true,
            ..ProactiveRefreshConfig::default()
        };
        let url = Url::parse("ws://localhost:9988/v1/tools").unwrap();
        let auth = provider(&url, Some(&path), &refresh).unwrap();
        match auth.current() {
            AuthCredential::Bearer { token } => assert_eq!(token, "eyJ.tok"),
            _ => panic!("expected Bearer"),
        }
        let id = auth.identity().expect("loopback identity present");
        assert_eq!(id.user_id, "u1");
        // Loopback never calls `build_oidc_provider`, so the proactive
        // flag cannot change the static-bearer path.
    }
}
