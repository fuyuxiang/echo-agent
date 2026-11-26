mod external_refresher;
mod oidc_refresher;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::auth::manager::AuthManager;
pub(crate) use crate::auth::manager::RefreshReason;
use crate::auth::model::EchoAgentAuth;

use external_refresher::ExternalBinaryRefresher;
pub(crate) use oidc_refresher::OidcRefresher;

/// Callback for diagnostic log upload on auth refresh failure.
/// Args: `(log_bytes, auth_token_suffix, user_id)` — path key is user id, never email.
pub(crate) type DiagnosticUploader =
    Arc<dyn Fn(Vec<u8>, String, String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Read-only view of `AuthManager` for refreshers. Enforces the
/// no-mutation contract on *credential* state at the type level: refreshers
/// hold `Arc<dyn AuthSnapshot>` and physically cannot call `update()`,
/// `clear()`, `hot_swap()`, or `refresh_chain()`.
pub(crate) trait AuthSnapshot: Send + Sync {
    /// Read the current in-memory bearer outside the early-invalidation buffer.
    fn current(&self) -> Option<EchoAgentAuth>;
    /// Read the expired in-memory bearer (for its `refresh_token`).
    fn expired_auth(&self) -> Option<EchoAgentAuth>;
    /// Re-read auth.json from disk for the configured scope. Read-only w.r.t.
    /// credentials, but may advance disk-observation state and emit transition
    /// telemetry (not credential mutation).
    fn read_disk_auth(&self) -> Option<EchoAgentAuth>;
    /// Whether the in-memory bearer is expired.
    fn is_expired(&self) -> bool;
}

impl AuthSnapshot for AuthManager {
    fn current(&self) -> Option<EchoAgentAuth> {
        self.current()
    }
    fn expired_auth(&self) -> Option<EchoAgentAuth> {
        self.expired_auth()
    }
    fn read_disk_auth(&self) -> Option<EchoAgentAuth> {
        self.read_disk_auth()
    }
    fn is_expired(&self) -> bool {
        self.is_expired()
    }
}

/// Capability to run the operator's external auth binary. Split out of
/// [`AuthSnapshot`] so OIDC refreshers (read-only) physically cannot reach it
/// (interface segregation); only [`ExternalBinaryRefresher`] depends on it.
#[async_trait::async_trait]
pub(crate) trait ExternalCommandRunner: Send + Sync {
    /// Run the external auth binary and return the parsed output.
    async fn run_external_command(&self, command: &str) -> Option<EchoAgentAuth>;
}

#[async_trait::async_trait]
impl ExternalCommandRunner for AuthManager {
    async fn run_external_command(&self, command: &str) -> Option<EchoAgentAuth> {
        self.run_external_refresh_command(command).await
    }
}

/// The credential a refresh would send to the IdP: disk refresh-token first,
/// then the expired in-mem bearer, then current (only on `ServerRejected`).
/// Single source of truth shared by [`OidcRefresher::refresh`] (the attempt) and
/// `AuthManager::attempted_verdict_key` (the verdict scope), so the two can't
/// drift. The caller supplies the disk read: the verdict path passes a
/// side-effect-free read, the refresher the observing one.
pub(crate) fn resolve_refresh_credential(
    snap: &dyn AuthSnapshot,
    disk_auth: Option<EchoAgentAuth>,
    reason: RefreshReason,
) -> Option<EchoAgentAuth> {
    disk_auth
        .filter(|a| a.refresh_token.is_some())
        .or_else(|| snap.expired_auth())
        .or_else(|| {
            (reason == RefreshReason::ServerRejected)
                .then(|| snap.current())
                .flatten()
        })
}

/// Outcome of a refresh attempt. Data only -- `refresh_chain` handles mutations.
#[derive(Debug)]
#[must_use = "RefreshOutcome encodes a state transition; route it through refresh_chain"]
pub(crate) enum RefreshOutcome {
    /// Authority returned a fresh token. Caller persists via `update()`.
    Success(Box<EchoAgentAuth>),
    /// Terminal failure (e.g. invalid_grant), or a transient escalated to
    /// `Other` after repeated blips. Caller records a verdict scoped to the
    /// rejected credential. `refresh_chain` discards AT+RT only for
    /// `RefreshTokenRejected` (sticky until login); `ClientRejected` / `Other`
    /// retain credentials and age out past the TTL.
    PermanentFailure {
        error: crate::auth::error::RefreshTokenFailedError,
        /// Key of the credential the refresher actually sent to the IdP, so
        /// `refresh_chain` scopes the verdict to it. `None` when the authority
        /// has no token key (external binary flow); the caller falls back to
        /// its own resolution.
        tried_key: Option<String>,
        /// The **refresh token** actually spent at the IdP. `refresh_chain`
        /// compares it against disk to tell "this session is revoked" apart
        /// from "a sibling process rotated the RT out from under us" — the
        /// latter must never discard credentials.
        ///
        /// `tried_key` cannot answer that question: it is the *access* token,
        /// and a sibling's rotation changes the RT while the AT the loser
        /// holds may be untouched. `None` when the authority does not expose
        /// which RT it sent (external binary flow).
        tried_refresh_token: Option<String>,
    },
    /// Transient / unknown failure. Caller may retry later. Message-only: the
    /// underlying cause is logged structurally at the refresher, then flattened
    /// here (the retry decision needs recoverability, not the source chain).
    TransientFailure { message: String },
}

impl RefreshOutcome {
    /// A fresh credential from the authority (hides the `Box`).
    pub(crate) fn success(auth: EchoAgentAuth) -> Self {
        Self::Success(Box::new(auth))
    }

    /// Terminal failure for an already-classified reason against the credential
    /// `tried_key` (the one actually sent to the IdP).
    ///
    /// Leaves the tried **refresh token** unattributed, which disables the
    /// sibling-rotation check in `refresh_chain`. Only correct for authorities
    /// that genuinely cannot report which RT they spent (the external-binary
    /// flow). Any refresher holding the [`EchoAgentAuth`] it sent must use
    /// [`Self::permanent_for`] instead.
    pub(crate) fn permanent(
        reason: crate::auth::error::RefreshTokenFailedReason,
        tried_key: Option<String>,
    ) -> Self {
        Self::PermanentFailure {
            error: reason.into(),
            tried_key,
            tried_refresh_token: None,
        }
    }

    /// Terminal failure attributed to the exact credential sent to the IdP.
    ///
    /// Prefer this wherever the attempted [`EchoAgentAuth`] is in hand: it captures
    /// both the AT key (verdict scope) and the RT (sibling-rotation check), so
    /// a lost rotation race cannot be mistaken for a revoked session.
    pub(crate) fn permanent_for(
        reason: crate::auth::error::RefreshTokenFailedReason,
        tried: &EchoAgentAuth,
    ) -> Self {
        Self::PermanentFailure {
            error: reason.into(),
            tried_key: Some(tried.key.clone()),
            tried_refresh_token: tried.refresh_token.clone(),
        }
    }

    /// A retryable failure carrying a diagnostic message.
    pub(crate) fn transient(message: impl Into<String>) -> Self {
        Self::TransientFailure {
            message: message.into(),
        }
    }
}

#[async_trait::async_trait]
pub(crate) trait TokenRefresher: Send + Sync {
    /// Attempt to obtain a fresh token from the authority.
    ///
    /// Implementations MUST NOT call auth_manager.update(), clear(),
    /// hot_swap(), or any other state-mutating method. Return the
    /// result and let refresh_chain handle all mutations.
    async fn refresh(&self, reason: RefreshReason) -> RefreshOutcome;
}

pub(crate) fn build_refresher(
    auth_manager: Arc<AuthManager>,
    auth_provider_command: Option<String>,
    diagnostic_uploader: Option<DiagnosticUploader>,
) -> Arc<dyn TokenRefresher> {
    match auth_provider_command {
        Some(cmd) => {
            let runner: Arc<dyn ExternalCommandRunner> = auth_manager;
            Arc::new(ExternalBinaryRefresher::new(runner, cmd))
        }
        None => {
            let snapshot: Arc<dyn AuthSnapshot> = auth_manager;
            let refresher = OidcRefresher::new(snapshot);
            match diagnostic_uploader {
                Some(uploader) => Arc::new(refresher.with_diagnostic_upload(uploader)),
                None => Arc::new(refresher),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthMode, EchoAgentAuth, EchoAgentComConfig};
    use chrono::{Duration, Utc};

    /// auth_token_ttl makes is_token_expired use create_time + ttl for
    /// External tokens without expires_at, instead of the 30-day fallback.
    #[test]
    fn token_ttl_expires_external_token_by_create_time() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EchoAgentComConfig {
            auth_token_ttl: Some(3600), // 1 hour
            ..EchoAgentComConfig::default()
        };
        let mgr = AuthManager::new(dir.path(), cfg);

        // Token created 2 hours ago, no expires_at. With auth_token_ttl=3600,
        // is_token_expired should return true (age 2h > ttl 1h).
        let old_token = EchoAgentAuth {
            key: "old-external-token".into(),
            auth_mode: AuthMode::External,
            create_time: Utc::now() - Duration::hours(2),
            expires_at: None,
            ..EchoAgentAuth::test_default()
        };
        mgr.hot_swap(old_token);
        assert!(
            mgr.current().is_none(),
            "expired external token via auth_token_ttl"
        );
        assert!(mgr.is_expired());

        // Fresh token created just now — should be valid.
        let new_token = EchoAgentAuth {
            key: "new-external-token".into(),
            auth_mode: AuthMode::External,
            create_time: Utc::now(),
            expires_at: None,
            ..EchoAgentAuth::test_default()
        };
        mgr.hot_swap(new_token);
        assert!(
            mgr.current().is_some(),
            "fresh external token should be valid"
        );
    }
}
