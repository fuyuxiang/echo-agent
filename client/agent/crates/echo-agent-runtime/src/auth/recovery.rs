//! Unauthorized (401) recovery state machine.
//!
//! When the server rejects a token, `UnauthorizedRecovery` walks through
//! a sequence of recovery steps before giving up:
//!
//! 1. **ReloadFromDisk** — re-read `auth.json` under a file lock; if the
//!    on-disk token differs from the rejected one, accept it (another
//!    process may have refreshed).
//! 2. **RefreshFromAuthority** — run the appropriate refresh chain
//!    (OIDC token refresh, external binary, etc.) based on `TokenType`,
//!    unless the live token was minted moments ago (fresh-mint guard).
//! 3. **DevboxRecovery** — on devboxes, purge `auth.json` and mint fresh
//!    OIDC credentials.
//! 4. **Done** — all recovery strategies exhausted.

use std::sync::Arc;

use crate::auth::error::{AuthError, RefreshTokenError, RefreshTokenFailedReason};
use crate::auth::manager::AuthManager;
use crate::auth::model::EchoAgentAuth;
use crate::auth::token_type::TokenType;
use echo_agent_telemetry::events::{
    AuthTokenKind, ManualAuth, ManualAuthReason, ManualAuthSurface,
};

/// `manual_auth` KPI reason for a terminal `AuthError`, or `None` when it
/// doesn't force a manual re-login. Lives here (not on `AuthError`) so the error
/// model stays telemetry-free.
pub(crate) fn manual_auth_reason(err: &AuthError) -> Option<ManualAuthReason> {
    use ManualAuthReason as R;
    Some(match err {
        AuthError::Refresh(RefreshTokenError::Permanent(e)) => match e.reason {
            RefreshTokenFailedReason::RefreshTokenRejected => R::RefreshTokenRejected,
            RefreshTokenFailedReason::ProviderInteractiveRequired => R::ProviderInteractiveRequired,
            // Self-healing via the TTL, not a manual re-auth.
            RefreshTokenFailedReason::ClientRejected | RefreshTokenFailedReason::Other => {
                return None;
            }
        },
        AuthError::ServerRejectedNoRecovery => R::NoRefreshAuthority,
        AuthError::RecoveryExhausted => R::RecoveryExhausted,
        AuthError::TokenExpiredNoRefresh => R::TokenExpiredNoRefresh,
        AuthError::PinnedTeamMismatch { .. } => R::WrongTeam,
        // API-key lockouts are out of scope: this KPI tracks OIDC refresh
        // failures forcing a re-login, not an admin disabling API-key auth.
        AuthError::ApiKeyAuthDisabled
        | AuthError::Refresh(RefreshTokenError::Transient(_))
        | AuthError::NotLoggedIn => {
            return None;
        }
    })
}

/// Whether the relay should stop reconnecting on this recovery error. Its own
/// predicate rather than reusing `manual_auth_reason`: the relay must give up on
/// any terminal auth failure, including `ApiKeyAuthDisabled` (a kill-switched
/// API key), which is deliberately out of the `manual_auth` KPI's scope.
pub(crate) fn relay_should_cancel(err: &AuthError) -> bool {
    manual_auth_reason(err).is_some() || matches!(err, AuthError::ApiKeyAuthDisabled)
}

/// Fresh-mint guard window (±) for `ServerRejected` refreshes
/// ([`UnauthorizedRecovery::fresh_mint_guard`]). 120s outlasts in-flight
/// requests sent with a previous key plus validation lag (observed stale
/// 401s land ~20s after mint), while `current()`'s 300s early-invalidation
/// buffer keeps any guard-returned token wire-valid. A genuinely-dead fresh
/// token waits at most this long to re-mint; the symmetric bound caps that
/// delay when the clock stepped back.
const FRESH_MINT_GUARD_SECS: i64 = 120;

/// Where a 401 recovery was initiated — drives the `manual_auth` KPI.
/// Required at every call site so suppressing the KPI is explicit, not default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoverySource {
    /// A chat/inference turn — surfaces the `ReAuthRequired` banner.
    Turn,
    /// The relay / leader connection handshake.
    Relay,
    /// Uploads, telemetry, tool calls. Never emits the KPI.
    Background,
}

impl RecoverySource {
    fn trigger(self) -> Option<ManualAuthSurface> {
        match self {
            RecoverySource::Turn => Some(ManualAuthSurface::Turn),
            RecoverySource::Relay => Some(ManualAuthSurface::Relay),
            RecoverySource::Background => None,
        }
    }
}

/// Identity of the rejected credential for `manual_auth`, captured from
/// the rejected bearer (not live `inner`) so attribution is correct even after a
/// `WrongTeam`/cleared-credential failure.
pub(crate) struct RejectedAuth {
    /// `user_id`, when known (empty ids collapse to `None`).
    principal: Option<String>,
    token_kind: AuthTokenKind,
    /// Full rejected bearer; the debounce key. Uses the whole token (not a
    /// suffix) so it matches the credential identity the permanent-failure
    /// verdict is scoped to; never logged.
    rejected_token_id: String,
}

impl RejectedAuth {
    pub(crate) fn capture(auth: Option<&EchoAgentAuth>) -> Self {
        Self {
            principal: auth.map(|a| a.user_id.clone()).filter(|id| !id.is_empty()),
            token_kind: TokenType::from_auth(auth).telemetry_kind(),
            rejected_token_id: auth.map(|a| a.key.clone()).unwrap_or_default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn principal_for_test(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn token_kind_for_test(&self) -> AuthTokenKind {
        self.token_kind
    }
}

/// Trigger + attribution for a user-facing recovery; set at construction iff the
/// source emits the KPI.
struct ManualAuthEmit {
    trigger: ManualAuthSurface,
    snapshot: RejectedAuth,
}

/// Per-process debounce + emit for the `manual_auth` KPI. Held by
/// `AuthManager` so all recoveries on one process share the dedup state.
/// All fields are `Default` under both cfgs, so one derive serves both.
#[derive(Default)]
pub(crate) struct ManualAuthTracker {
    /// Id of the rejected credential we last emitted for (single slot: only the
    /// most recent). Repeats on the same bearer debounce; a new credential has a
    /// new id and re-arms.
    last_token: parking_lot::Mutex<Option<String>>,
    /// Test-only: the last emitted event, so a test can assert what was
    /// emitted, not just that something fired.
    #[cfg(test)]
    last_emit: parking_lot::Mutex<Option<ManualAuth>>,
    /// Test-only: count of events that actually fired (post-debounce), so a
    /// concurrency test can assert the dedup mutex collapses N races to one.
    #[cfg(test)]
    emit_count: std::sync::atomic::AtomicU32,
}

impl ManualAuthTracker {
    /// Emit a terminal manual-auth event, debounced against the most-recent
    /// credential (single slot). No-op for transient failures
    /// (`manual_auth_reason` is `None`) and for API keys (a 401 there means
    /// rotate the key, not `/login`).
    pub(crate) fn record(
        &self,
        snapshot: &RejectedAuth,
        err: &AuthError,
        trigger: ManualAuthSurface,
    ) {
        if snapshot.token_kind == AuthTokenKind::ApiKey {
            return;
        }
        let Some(reason) = manual_auth_reason(err) else {
            return;
        };
        {
            // Hold the guard only for the check-and-set; drop it before
            // `log_event` (which spawns).
            let mut last = self.last_token.lock();
            if last.as_deref() == Some(snapshot.rejected_token_id.as_str()) {
                return;
            }
            *last = Some(snapshot.rejected_token_id.clone());
        }
        let event = ManualAuth {
            reason,
            trigger,
            token_kind: snapshot.token_kind,
            principal: snapshot.principal.clone(),
        };
        #[cfg(test)]
        {
            *self.last_emit.lock() = Some(event.clone());
            self.emit_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        echo_agent_telemetry::session_ctx::log_event(event);
    }

    #[cfg(test)]
    pub(crate) fn emit_count_for_test(&self) -> u32 {
        self.emit_count.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn last_token_for_test(&self) -> Option<String> {
        self.last_token.lock().clone()
    }

    #[cfg(test)]
    pub(crate) fn last_emit_for_test(&self) -> Option<ManualAuth> {
        self.last_emit.lock().clone()
    }
}

/// Which recovery step to attempt next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryStep {
    /// Re-read auth.json from disk (file-locked).
    ReloadFromDisk,
    /// Refresh via the authority (OIDC, external binary, etc.).
    RefreshFromAuthority,
    /// On devboxes: purge auth.json and mint fresh OIDC credentials.
    DevboxRecovery,
    /// All strategies exhausted.
    Done,
}

/// State machine that walks through recovery strategies after a 401.
pub(crate) struct UnauthorizedRecovery {
    auth_manager: Arc<AuthManager>,
    /// The token that was rejected by the server.
    rejected_token: String,
    /// Current step in the recovery sequence.
    step: RecoveryStep,
    /// Error from `RefreshFromAuthority`, propagated as fallback when
    /// devbox recovery doesn't apply.
    authority_error: Option<AuthError>,
    /// Whether the last authority failure was transient. Kept past the
    /// `authority_error` handoff so exhaustion preserves the
    /// transient/permanent axis (see the `Done` arm).
    authority_was_transient: bool,
    /// `Some` iff this recovery is user-facing — so a terminal failure emits.
    emit: Option<ManualAuthEmit>,
}

impl UnauthorizedRecovery {
    /// `rejected` is the credential the server rejected: its key drives recovery
    /// and (for user-facing sources) its identity is the KPI attribution.
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        rejected: Option<EchoAgentAuth>,
        source: RecoverySource,
    ) -> Self {
        let rejected_token = rejected.as_ref().map(|a| a.key.clone()).unwrap_or_default();
        let emit = source.trigger().map(|trigger| ManualAuthEmit {
            trigger,
            snapshot: RejectedAuth::capture(rejected.as_ref()),
        });
        Self {
            auth_manager,
            rejected_token,
            step: RecoveryStep::ReloadFromDisk,
            authority_error: None,
            authority_was_transient: false,
            emit,
        }
    }

    /// Attempt the next recovery step. Walks
    /// `ReloadFromDisk -> RefreshFromAuthority -> DevboxRecovery -> Done`.
    /// `token_type` span field is recorded lazily via
    /// `Span::is_disabled()` to avoid the lock when tracing is off.
    #[tracing::instrument(
        skip(self),
        fields(step = ?self.step, token_type = tracing::field::Empty),
    )]
    pub(crate) async fn next(&mut self) -> Result<EchoAgentAuth, AuthError> {
        let span = tracing::Span::current();
        if !span.is_disabled() {
            // Only acquire the inner-lock when tracing actually
            // collects the span. `token_type()` -> `inner.read()` is
            // ~free but it's still a lock, and recovery is on the
            // 401-recovery path; making the cost zero when tracing is
            // off matches the no-trace-no-cost contract.
            span.record(
                "token_type",
                tracing::field::debug(self.auth_manager.token_type()),
            );
        }
        let result = self.resolve_next().await;
        if let (Err(e), Some(emit)) = (&result, &self.emit) {
            self.auth_manager
                .record_manual_auth(&emit.snapshot, e, emit.trigger);
        }
        result
    }

    /// Walk the recovery steps and apply the team-pin policy gate.
    async fn resolve_next(&mut self) -> Result<EchoAgentAuth, AuthError> {
        // Team-pin gate: 401 recovery must not resurrect a wrong-team session
        // (disk adoption / refresh / devbox mint) for the relay to reconnect
        // with. Clear + reject on mismatch.
        let auth = self.next_step_loop().await?;
        if let Some(e) = self.auth_manager.cached_token_policy_error(&auth) {
            self.auth_manager.reject_and_clear(&e);
            return Err(e);
        }
        Ok(auth)
    }

    async fn next_step_loop(&mut self) -> Result<EchoAgentAuth, AuthError> {
        loop {
            match self.step {
                RecoveryStep::ReloadFromDisk => {
                    self.step = RecoveryStep::RefreshFromAuthority;
                    if let Some(auth) = self.try_reload_from_disk().await {
                        return Ok(auth);
                    }
                }
                RecoveryStep::RefreshFromAuthority => {
                    self.step = RecoveryStep::DevboxRecovery;
                    match self.try_refresh_from_authority().await {
                        Ok(auth) => return Ok(auth),
                        Err(e) => {
                            self.authority_was_transient =
                                matches!(e, AuthError::Refresh(RefreshTokenError::Transient(_)));
                            self.authority_error = Some(e);
                        }
                    }
                }
                RecoveryStep::DevboxRecovery => {
                    self.step = RecoveryStep::Done;
                    // preferred_method=api_key forbids automatic OIDC mint.
                    if !self
                        .auth_manager
                        .echo_agent_com_config()
                        .blocks_automatic_oidc()
                        && self.auth_manager.is_devbox_environment()
                        && let Ok(auth) = self
                            .auth_manager
                            .try_devbox_recovery(Some(&self.rejected_token))
                            .await
                    {
                        return Ok(auth);
                    }
                    return Err(self
                        .authority_error
                        .take()
                        .unwrap_or(AuthError::RecoveryExhausted));
                }
                RecoveryStep::Done => {
                    // Exhaustion after a *transient* authority failure stays
                    // transient: `RecoveryExhausted` here would count a network
                    // blip as a forced re-login (`manual_auth`) and cancel the
                    // relay instead of letting it reconnect.
                    return Err(if self.authority_was_transient {
                        AuthError::transient("recovery exhausted after transient refresh failure")
                    } else {
                        AuthError::RecoveryExhausted
                    });
                }
            }
        }
    }

    /// Re-read `auth.json` from disk. Accept the token only if it differs
    /// from the one that was rejected.
    async fn try_reload_from_disk(&self) -> Option<EchoAgentAuth> {
        let _lock = self
            .auth_manager
            .try_lock_auth_file_async(
                crate::auth::manager::AUTH_LOCK_TIMEOUT,
                crate::auth::manager::lock::Heartbeat::Skip,
            )
            .await
            .into_guard();
        if _lock.is_none() {
            tracing::warn!("auth recovery: proceeding without file lock");
        }

        let Some(disk_auth) = self.auth_manager.read_disk_auth() else {
            // Every ReloadFromDisk outcome must log (adopted / expired /
            // same-as-rejected / no entry): a silent arm hides which path
            // a recovery loop is taking. Debug level — the disk-state
            // *transition* is logged once by `read_disk_auth` itself.
            echo_agent_telemetry::unified_log::debug("auth recovery: no disk entry", None, None);
            return None;
        };
        if crate::auth::is_expired(&disk_auth) {
            tracing::debug!("auth recovery: disk token is expired, skipping");
            echo_agent_telemetry::unified_log::debug(
                "auth recovery: disk token expired",
                None,
                Some(serde_json::json!({
                    "disk_key_prefix": echo_agent_auth::bearer_suffix(&disk_auth.key),
                    "expires_at": disk_auth.expires_at.map(|e| e.to_rfc3339()),
                })),
            );
            return None;
        }
        if self.is_different_token(&disk_auth) {
            tracing::info!("auth recovery: disk has a different token, accepting");
            echo_agent_telemetry::unified_log::info(
                "auth recovery: adopted disk token",
                None,
                Some(serde_json::json!({
                    "adopted_key_prefix": echo_agent_auth::bearer_suffix(&disk_auth.key),
                    "expires_at": disk_auth.expires_at.map(|e| e.to_rfc3339()),
                })),
            );
            self.auth_manager.hot_swap(disk_auth.clone());
            Some(disk_auth)
        } else {
            tracing::debug!("auth recovery: disk token is same as rejected, skipping");
            echo_agent_telemetry::unified_log::debug(
                "auth recovery: disk token same as rejected",
                None,
                None,
            );
            None
        }
    }

    /// Return the live token instead of refreshing when its mint age is
    /// within ±[`FRESH_MINT_GUARD_SECS`]; anything outside (including a
    /// clock that stepped far back) falls through to a normal refresh.
    ///
    /// A 401 moments after a successful mint is a stale rejection (sent with
    /// the previous key and mis-attributed — see `is_stale_snapshot`) or
    /// validation lag on the new key — re-minting fixes neither, and a crash
    /// between the IdP grant and persisting the response orphans the
    /// replacement RT (forced re-login). Consumers retry with the returned
    /// token; a genuinely-bad one refreshes once the window passes. Lives
    /// here, not in `refresh_chain`, so paywall claims re-mints that call
    /// `refresh_chain(ServerRejected)` directly are unaffected.
    fn fresh_mint_guard(&self) -> Option<EchoAgentAuth> {
        let auth = self.auth_manager.current()?;
        let mint_age_seconds = auth.mint_age_seconds();
        if !(-FRESH_MINT_GUARD_SECS..FRESH_MINT_GUARD_SECS).contains(&mint_age_seconds) {
            return None;
        }
        tracing::info!(
            mint_age_seconds,
            "auth recovery: current token freshly minted, skipping refresh"
        );
        echo_agent_telemetry::unified_log::info(
            "auth recovery: fresh mint, refresh skipped",
            None,
            Some(serde_json::json!({
                "key_prefix": echo_agent_auth::bearer_suffix(&auth.key),
                "mint_age_seconds": mint_age_seconds,
                "guard_seconds": FRESH_MINT_GUARD_SECS,
                "expires_at": auth.expires_at.map(|e| e.to_rfc3339()),
            })),
        );
        Some(auth)
    }

    /// Dispatch to the correct refresh chain based on the current `TokenType`.
    ///
    /// Per-variant outcome:
    ///
    /// - **OidcSession / ExternalBinary**: full refresh chain via the
    ///   authority, unless the live token is inside the fresh-mint guard
    ///   window ([`Self::fresh_mint_guard`]).
    /// - **LegacySession / ApiKey**: no refresh authority for these
    ///   types. We've already tried `ReloadFromDisk` (the previous
    ///   recovery step), so the server's 401 stands. Surface
    ///   [`AuthError::ServerRejectedNoRecovery`] -- *not*
    ///   `TokenExpiredNoRefresh`, because the trigger here is the
    ///   server rejecting the token (it may not have aged past any
    ///   local TTL; ApiKey in particular has no expiry). Consumers
    ///   reading the variant can distinguish "ran past local TTL" from
    ///   "server actively rejected".
    /// - **None**: no credentials at all.
    async fn try_refresh_from_authority(&self) -> Result<EchoAgentAuth, AuthError> {
        let tt = self.auth_manager.token_type();
        match tt {
            TokenType::OidcSession | TokenType::ExternalBinary => {
                if let Some(auth) = self.fresh_mint_guard() {
                    return Ok(auth);
                }
                let result = self
                    .auth_manager
                    .refresh_chain(tt, crate::auth::manager::RefreshReason::ServerRejected)
                    .await;
                match &result {
                    Ok(auth) => {
                        echo_agent_telemetry::unified_log::info(
                            "auth recovery: refreshed from authority",
                            None,
                            Some(serde_json::json!({
                                "token_type": format!("{tt:?}"),
                                "new_key_prefix": echo_agent_auth::bearer_suffix(&auth.key),
                                "expires_at": auth.expires_at.map(|e| e.to_rfc3339()),
                            })),
                        );
                    }
                    Err(e) => {
                        echo_agent_telemetry::unified_log::warn(
                            "auth recovery: refresh from authority failed",
                            None,
                            Some(serde_json::json!({
                                "token_type": format!("{tt:?}"),
                                "error": format!("{e}"),
                            })),
                        );
                    }
                }
                result
            }
            TokenType::LegacySession | TokenType::ApiKey => {
                echo_agent_telemetry::unified_log::warn(
                    "auth recovery: no refresh authority for token type",
                    None,
                    Some(serde_json::json!({ "token_type": format!("{tt:?}") })),
                );
                Err(AuthError::ServerRejectedNoRecovery)
            }
            TokenType::None => Err(AuthError::NotLoggedIn),
        }
    }

    /// Check if a candidate token is different from the rejected one.
    fn is_different_token(&self, candidate: &EchoAgentAuth) -> bool {
        candidate.key != self.rejected_token
    }
}

#[cfg(test)]
mod tests {
    //! State-machine matrix tests for `UnauthorizedRecovery`.
    //!
    //! Coverage targets:
    //! - All 5 `TokenType` variants x dispatch in `try_refresh_from_authority`.
    //! - `try_reload_from_disk`: same/different/no token on disk.
    //! - `next()` exhaustion (Done -> RecoveryExhausted).
    //! - Fresh-mint guard: ±window bounds, ExternalBinary, verdict grace,
    //!   policy-hidden fall-through (fail closed).
    //!
    //! These tests use the same in-process `AuthManager` that production
    //! does and inject a counting refresher so we can observe whether the
    //! authority was consulted.
    use super::*;
    use crate::auth::config::EchoAgentComConfig;
    use crate::auth::error::{RefreshTokenError, RefreshTokenFailedReason};
    use crate::auth::model::{AuthMode, EchoAgentAuth};
    use crate::auth::refresh::{RefreshOutcome, TokenRefresher};
    use crate::auth::storage::{read_auth_json, write_auth_json};
    use chrono::{Duration, Utc};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// The rejected wire bearer these tests seed into the manager.
    fn rejected_cred() -> Option<EchoAgentAuth> {
        Some(EchoAgentAuth {
            key: "rejected-tok".into(),
            ..EchoAgentAuth::test_default()
        })
    }

    /// Refresher fake: returns Success with a fresh token on every call.
    struct OkRefresher {
        calls: Arc<AtomicU32>,
    }
    #[async_trait::async_trait]
    impl TokenRefresher for OkRefresher {
        async fn refresh(&self, _reason: crate::auth::manager::RefreshReason) -> RefreshOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            RefreshOutcome::Success(Box::new(EchoAgentAuth {
                key: "fresh-from-authority".into(),
                auth_mode: AuthMode::Oidc,
                refresh_token: Some("rt-new".into()),
                expires_at: Some(Utc::now() + Duration::hours(1)),
                ..EchoAgentAuth::test_default()
            }))
        }
    }

    /// Refresher fake: returns PermanentFailure (invalid_grant).
    struct FailRefresher {
        calls: Arc<AtomicU32>,
    }
    #[async_trait::async_trait]
    impl TokenRefresher for FailRefresher {
        async fn refresh(&self, _reason: crate::auth::manager::RefreshReason) -> RefreshOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            RefreshOutcome::permanent(RefreshTokenFailedReason::RefreshTokenRejected, None)
        }
    }

    fn mgr() -> (tempfile::TempDir, Arc<AuthManager>) {
        let dir = tempfile::tempdir().unwrap();
        let m = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
        (dir, m)
    }

    fn seed(mgr: &AuthManager, mode: AuthMode, refresh_token: Option<&str>) {
        let auth = EchoAgentAuth {
            key: "rejected-tok".into(),
            auth_mode: mode,
            refresh_token: refresh_token.map(str::to_string),
            // Past expiry so `current()` returns None and the refresh
            // chain actually has to do work.
            expires_at: Some(Utc::now() - Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        mgr.hot_swap(auth);
    }

    // -- TokenType dispatch matrix ----------------------------------------

    #[tokio::test]
    async fn dispatch_oidc_session_uses_refresh_chain() {
        let (_d, m) = mgr();
        seed(&m, AuthMode::Oidc, Some("rt"));
        let calls = Arc::new(AtomicU32::new(0));
        m.set_refresher(Arc::new(OkRefresher {
            calls: calls.clone(),
        }));

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        // ReloadFromDisk fails (no disk auth), then RefreshFromAuthority succeeds.
        let auth = rec.next().await.expect("recovery should succeed");
        assert_eq!(auth.key, "fresh-from-authority");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_external_binary_uses_refresh_chain() {
        let (_d, m) = mgr();
        seed(&m, AuthMode::External, None);
        let calls = Arc::new(AtomicU32::new(0));
        m.set_refresher(Arc::new(OkRefresher {
            calls: calls.clone(),
        }));

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let auth = rec.next().await.expect("external-binary recovery succeeds");
        assert_eq!(auth.key, "fresh-from-authority");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // -- Fresh-mint guard --------------------------------------------------

    /// Seed a *valid* (unexpired) in-memory token whose `create_time` lies
    /// `mint_age` in the past (negative = clock stepped back since mint).
    fn seed_valid(mgr: &AuthManager, mode: AuthMode, mint_age: Duration) {
        mgr.hot_swap(EchoAgentAuth {
            key: "rejected-tok".into(),
            auth_mode: mode,
            refresh_token: Some("rt".into()),
            create_time: Utc::now() - mint_age,
            expires_at: Some(Utc::now() + Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        });
    }

    /// Run one recovery against a counting refresher; return the outcome and
    /// how many times the authority was consulted.
    async fn recover_with_ok_refresher(
        m: &Arc<AuthManager>,
    ) -> (Result<EchoAgentAuth, AuthError>, u32) {
        let calls = Arc::new(AtomicU32::new(0));
        m.set_refresher(Arc::new(OkRefresher {
            calls: calls.clone(),
        }));
        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let result = rec.next().await;
        (result, calls.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn fresh_mint_guard_skips_idp_for_freshly_minted_token() {
        let (_d, m) = mgr();
        seed_valid(&m, AuthMode::Oidc, Duration::seconds(10));
        let (result, calls) = recover_with_ok_refresher(&m).await;
        assert_eq!(
            result.expect("guard returns the live token").key,
            "rejected-tok"
        );
        assert_eq!(calls, 0, "a 10s-old token must not be re-minted");
    }

    #[tokio::test]
    async fn fresh_mint_guard_applies_to_external_binary_tokens() {
        let (_d, m) = mgr();
        seed_valid(&m, AuthMode::External, Duration::seconds(10));
        let (result, calls) = recover_with_ok_refresher(&m).await;
        assert_eq!(
            result.expect("guard returns the live token").key,
            "rejected-tok"
        );
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn fresh_mint_guard_treats_small_negative_age_as_fresh() {
        // Clock stepped back slightly since mint (NTP nudge).
        let (_d, m) = mgr();
        seed_valid(&m, AuthMode::Oidc, Duration::seconds(-60));
        let (result, calls) = recover_with_ok_refresher(&m).await;
        assert_eq!(
            result.expect("guard returns the live token").key,
            "rejected-tok"
        );
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn fresh_mint_guard_refreshes_when_clock_stepped_far_back() {
        // A large backwards clock step must not wedge recovery for the whole
        // step: outside the ±window the guard stands down.
        let (_d, m) = mgr();
        seed_valid(&m, AuthMode::Oidc, Duration::hours(-1));
        let (result, calls) = recover_with_ok_refresher(&m).await;
        assert_eq!(
            result.expect("recovery should succeed").key,
            "fresh-from-authority"
        );
        assert_eq!(calls, 1, "far-negative mint age must reach the IdP");
    }

    #[tokio::test]
    async fn fresh_mint_guard_lets_old_token_refresh() {
        let (_d, m) = mgr();
        seed_valid(&m, AuthMode::Oidc, Duration::minutes(10));
        let (result, calls) = recover_with_ok_refresher(&m).await;
        assert_eq!(
            result.expect("recovery should succeed").key,
            "fresh-from-authority"
        );
        assert_eq!(
            calls, 1,
            "outside the guard window ServerRejected must reach the IdP"
        );
    }

    #[tokio::test]
    async fn fresh_mint_guard_wins_over_cached_permanent_failure() {
        // A fresh *valid* token is served even when a permanent-failure
        // verdict is cached for it — mirrors `auth()`'s wire-valid grace arm;
        // the verdict re-applies once the guard window passes.
        let (_d, m) = mgr();
        seed_valid(&m, AuthMode::Oidc, Duration::seconds(10));
        m.record_permanent_failure(
            "rejected-tok".into(),
            RefreshTokenFailedReason::RefreshTokenRejected.into(),
        );
        let (result, calls) = recover_with_ok_refresher(&m).await;
        assert_eq!(
            result
                .expect("guard precedes the verdict short-circuit")
                .key,
            "rejected-tok"
        );
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn fresh_mint_guard_never_returns_policy_hidden_token() {
        // Wrong-team fresh token: `current()` hides it (vet_cached), so the
        // guard must fall through to a normal refresh — fail closed.
        let dir = tempfile::tempdir().unwrap();
        let cfg = EchoAgentComConfig {
            force_login_team_uuid: Some(crate::auth::config::ForceLoginTeam::Single(
                "team-good".into(),
            )),
            ..EchoAgentComConfig::default()
        };
        let m = Arc::new(AuthManager::new(dir.path(), cfg));
        m.hot_swap(EchoAgentAuth {
            key: team_jwt("team-wrong"),
            auth_mode: AuthMode::Oidc,
            refresh_token: Some("rt".into()),
            create_time: Utc::now(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        });
        let calls = Arc::new(AtomicU32::new(0));
        m.set_refresher(Arc::new(OkRefresher {
            calls: calls.clone(),
        }));

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let result = rec.next().await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "hidden token must not satisfy the guard"
        );
        if let Ok(auth) = result {
            assert_ne!(
                auth.key,
                team_jwt("team-wrong"),
                "wrong-team token must never be returned"
            );
        }
    }

    #[tokio::test]
    async fn dispatch_legacy_session_returns_server_rejected_no_recovery() {
        let (_d, m) = mgr();
        // WebLogin (no refresh_token) -> LegacySession.
        seed(&m, AuthMode::WebLogin, None);

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let err = rec.next().await.unwrap_err();
        assert!(
            matches!(err, AuthError::ServerRejectedNoRecovery),
            "LegacySession recovery should surface ServerRejectedNoRecovery, got {err:?}",
        );
    }

    #[tokio::test]
    async fn dispatch_oidc_without_refresh_token_returns_server_rejected_no_recovery() {
        // Oidc without refresh_token classifies as LegacySession.
        let (_d, m) = mgr();
        seed(&m, AuthMode::Oidc, None);

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let err = rec.next().await.unwrap_err();
        assert!(matches!(err, AuthError::ServerRejectedNoRecovery));
    }

    #[tokio::test]
    async fn dispatch_api_key_returns_server_rejected_no_recovery() {
        let (_d, m) = mgr();
        seed(&m, AuthMode::ApiKey, None);

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let err = rec.next().await.unwrap_err();
        assert!(
            matches!(err, AuthError::ServerRejectedNoRecovery),
            "ApiKey recovery should surface ServerRejectedNoRecovery (not \
             TokenExpiredNoRefresh), got {err:?}",
        );
    }

    #[tokio::test]
    async fn dispatch_none_returns_not_logged_in() {
        let (_d, m) = mgr();
        // No seed — inner stays None → TokenType::None.
        // Single next() falls through ReloadFromDisk → RefreshFromAuthority.
        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let err = rec.next().await.unwrap_err();
        assert!(
            matches!(err, AuthError::NotLoggedIn),
            "None token type should surface NotLoggedIn, got {err:?}",
        );
    }

    // -- ReloadFromDisk matrix --------------------------------------------

    #[tokio::test]
    async fn reload_from_disk_picks_up_different_token() {
        let (dir, m) = mgr();
        seed(&m, AuthMode::Oidc, Some("rt"));

        // Sibling process wrote a different valid token to disk.
        let scope = m.echo_agent_com_config().auth_scope();
        let fresh = EchoAgentAuth {
            key: "fresh-from-disk".into(),
            auth_mode: AuthMode::Oidc,
            refresh_token: Some("rt-new".into()),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        let mut store = read_auth_json(&dir.path().join("auth.json")).unwrap_or_default();
        store.insert(scope, fresh);
        write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let auth = rec
            .next()
            .await
            .expect("recovery should pick up the disk token");
        assert_eq!(auth.key, "fresh-from-disk");
    }

    #[tokio::test]
    async fn reload_from_disk_skips_same_token_then_proceeds_to_authority() {
        let (dir, m) = mgr();
        seed(&m, AuthMode::Oidc, Some("rt"));

        // Disk has the SAME token that was rejected -- skip, fall through.
        let scope = m.echo_agent_com_config().auth_scope();
        let same = EchoAgentAuth {
            key: "rejected-tok".into(),
            auth_mode: AuthMode::Oidc,
            refresh_token: Some("rt".into()),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        let mut store = read_auth_json(&dir.path().join("auth.json")).unwrap_or_default();
        store.insert(scope, same);
        write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

        let calls = Arc::new(AtomicU32::new(0));
        m.set_refresher(Arc::new(OkRefresher {
            calls: calls.clone(),
        }));

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let auth = rec.next().await.expect("authority refresh succeeds");
        assert_eq!(auth.key, "fresh-from-authority");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "fall-through to authority must invoke the refresher exactly once",
        );
    }

    // -- Done state -------------------------------------------------------

    /// With no stored authority error (the first `next()` succeeded), driving
    /// past `Done` surfaces `RecoveryExhausted`. The transient-failure case is
    /// pinned by `exhaustion_after_transient_failure_stays_transient`.
    #[tokio::test]
    async fn next_after_done_returns_recovery_exhausted() {
        let (_d, m) = mgr();
        seed(&m, AuthMode::Oidc, Some("rt"));
        m.set_refresher(Arc::new(OkRefresher {
            calls: Arc::new(AtomicU32::new(0)),
        }));

        // Pin non-devbox so DevboxRecovery can't adopt the seeded token (CI runs
        // in K8s pods where is_devbox_environment() is true).
        m.set_devbox_env_for_test(false);

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let _ = rec.next().await.unwrap();
        let err = loop {
            if let Err(e) = rec.next().await {
                break e;
            }
        };
        assert!(
            matches!(err, AuthError::RecoveryExhausted),
            "Done state must surface RecoveryExhausted, got {err:?}",
        );
    }

    /// Exhaustion after a *transient* authority failure preserves the
    /// transient axis: surfacing `RecoveryExhausted` would count a network
    /// blip as a forced re-login (`manual_auth`) and make the relay cancel
    /// instead of reconnect.
    #[tokio::test]
    async fn exhaustion_after_transient_failure_stays_transient() {
        /// Refresher fake: transient failure on every call.
        struct TransientFailRefresher;
        #[async_trait::async_trait]
        impl TokenRefresher for TransientFailRefresher {
            async fn refresh(
                &self,
                _reason: crate::auth::manager::RefreshReason,
            ) -> RefreshOutcome {
                RefreshOutcome::transient("network blip")
            }
        }

        let (_d, m) = mgr();
        seed(&m, AuthMode::Oidc, Some("rt"));
        m.set_refresher(Arc::new(TransientFailRefresher));
        m.set_devbox_env_for_test(false);

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Turn);
        // First next(): the authority's transient error propagates as-is.
        let first = rec.next().await.unwrap_err();
        assert!(
            matches!(first, AuthError::Refresh(RefreshTokenError::Transient(_))),
            "authority transient must propagate, got {first:?}",
        );

        // Driving past exhaustion must stay transient too.
        let err = loop {
            if let Err(e) = rec.next().await {
                break e;
            }
        };
        assert!(
            matches!(err, AuthError::Refresh(RefreshTokenError::Transient(_))),
            "exhaustion after a transient failure must stay transient, got {err:?}",
        );
        assert_eq!(
            manual_auth_reason(&err),
            None,
            "a transient exhaustion must not map to a manual_auth reason",
        );
        assert!(
            !relay_should_cancel(&err),
            "the relay must reconnect (not cancel) on a transient exhaustion",
        );
        assert!(
            m.manual_auth_last_token().is_none(),
            "no manual_auth event may be recorded for a transient outage",
        );
    }

    // -- Permanent failure short-circuit (cross-check) ------------

    #[tokio::test]
    async fn refresh_authority_short_circuits_on_cached_permanent_failure() {
        let (_d, m) = mgr();
        seed(&m, AuthMode::Oidc, Some("rt"));
        // Pre-record a permanent failure scoped to the seeded credential.
        m.record_permanent_failure(
            "rejected-tok".into(),
            RefreshTokenFailedReason::RefreshTokenRejected.into(),
        );

        let calls = Arc::new(AtomicU32::new(0));
        m.set_refresher(Arc::new(FailRefresher {
            calls: calls.clone(),
        }));

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let err = rec.next().await.unwrap_err();
        assert!(matches!(
            err,
            AuthError::Refresh(RefreshTokenError::Permanent(_))
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "refresher must not be invoked when permanent_failure is cached",
        );
    }

    // -- ReloadFromDisk rejects expired disk tokens -------------------------

    /// Regression: disk holds a different but expired token. Recovery
    /// must skip it and fall through to RefreshFromAuthority, not
    /// return it for the caller to send on the wire (instant 401).
    #[tokio::test]
    async fn reload_from_disk_rejects_expired_different_token() {
        let (dir, m) = mgr();
        seed(&m, AuthMode::Oidc, Some("rt"));

        let scope = m.echo_agent_com_config().auth_scope();
        let expired_different = EchoAgentAuth {
            key: "different-but-expired".into(),
            auth_mode: AuthMode::Oidc,
            refresh_token: Some("rt-new".into()),
            expires_at: Some(Utc::now() - Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        let mut store = read_auth_json(&dir.path().join("auth.json")).unwrap_or_default();
        store.insert(scope, expired_different);
        write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

        let calls = Arc::new(AtomicU32::new(0));
        m.set_refresher(Arc::new(OkRefresher {
            calls: calls.clone(),
        }));

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let auth = rec.next().await.expect("should fall through to authority");
        assert_eq!(
            auth.key, "fresh-from-authority",
            "must skip the expired disk token and use the refresher"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // -- ManualAuthTracker debounce under concurrency ---------------------

    /// The dedup mutex's whole purpose: N concurrent recoveries on the *same*
    /// rejected credential collapse to a single emitted `manual_auth` event.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn manual_auth_record_dedups_concurrent_same_credential() {
        let tracker = Arc::new(ManualAuthTracker::default());
        let auth = EchoAgentAuth {
            key: "rejected".into(),
            auth_mode: AuthMode::Oidc,
            refresh_token: Some("rt".into()),
            user_id: "user-1".into(),
            ..EchoAgentAuth::test_default()
        };
        let snapshot = Arc::new(RejectedAuth::capture(Some(&auth)));
        let err = Arc::new(AuthError::permanent(
            RefreshTokenFailedReason::RefreshTokenRejected,
        ));

        let handles: Vec<_> = (0..16)
            .map(|_| {
                let (t, s, e) = (tracker.clone(), snapshot.clone(), err.clone());
                tokio::spawn(async move {
                    t.record(s.as_ref(), e.as_ref(), ManualAuthSurface::Turn);
                })
            })
            .collect();
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(
            tracker.emit_count_for_test(),
            1,
            "concurrent records on one rejected credential must emit exactly once",
        );
    }

    // -- force_login_team_uuid pin enforced on the 401-recovery path -------

    fn ensure_crypto_provider() {
        let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
    }

    fn team_jwt(principal_id: &str) -> String {
        ensure_crypto_provider();
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &serde_json::json!({
                "sub": "user-1",
                "principal_type": "Team",
                "principal_id": principal_id,
                "exp": 9999999999u64,
            }),
            &jsonwebtoken::EncodingKey::from_secret(b"test-secret"),
        )
        .unwrap()
    }

    /// A sibling writes a wrong-team token to disk; 401 recovery (relay path)
    /// must reject + clear it at `next()`, not hand it back as a bearer.
    #[tokio::test]
    async fn recovery_rejects_wrong_team_adopted_disk_token() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EchoAgentComConfig {
            force_login_team_uuid: Some(crate::auth::config::ForceLoginTeam::Single(
                "team-good".into(),
            )),
            ..EchoAgentComConfig::default()
        };
        let scope = cfg.auth_scope();
        let m = Arc::new(AuthManager::new(dir.path(), cfg));

        // In-memory: the rejected (expired) session that triggered recovery.
        seed(&m, AuthMode::Oidc, Some("rt"));

        // Disk: a different, non-expired, *wrong-team* token a sibling wrote.
        let mut store = read_auth_json(&dir.path().join("auth.json")).unwrap_or_default();
        store.insert(
            scope,
            EchoAgentAuth {
                key: team_jwt("team-wrong"),
                auth_mode: AuthMode::Oidc,
                refresh_token: Some("rt-sibling".into()),
                expires_at: Some(Utc::now() + Duration::hours(1)),
                ..EchoAgentAuth::test_default()
            },
        );
        write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

        let mut rec = m.unauthorized_recovery(rejected_cred(), RecoverySource::Background);
        let err = rec.next().await.unwrap_err();
        assert!(
            matches!(err, AuthError::PinnedTeamMismatch { .. }),
            "recovery must reject a wrong-team disk token, got {err:?}"
        );
    }
}
