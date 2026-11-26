//! What it takes to get a session back to a usable credential, and the one
//! bounded unattended attempt the startup paths make before asking the user.

use std::sync::Arc;
use std::time::Duration;

use super::{AuthManager, RefreshReason};
use crate::auth::error::AuthError;
use crate::auth::model::EchoAgentAuth;
use crate::auth::token_type::TokenType;

/// The way back to a usable credential, as of right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthRemedy {
    /// A later unattended refresh can still succeed — in the field, almost
    /// always a launch seconds after wake, before the network is up.
    SelfHealing,
    /// Only an interactive run of the operator's auth provider can mint one.
    ProviderLogin { label: Option<String> },
    /// Only a user-driven login can.
    ManualLogin,
}

impl AuthRemedy {
    pub(crate) fn is_self_healing(&self) -> bool {
        matches!(self, Self::SelfHealing)
    }

    /// `error_type` for a turn that died on this credential.
    pub(crate) fn turn_error_type(&self) -> &'static str {
        match self {
            Self::SelfHealing => "auth_transient",
            Self::ProviderLogin { .. } | Self::ManualLogin => "auth",
        }
    }

    /// The same remedy, for a turn that has already spent its automatic
    /// retries. [`Self::SelfHealing`] cannot survive that: its whole message
    /// is "retry in a few seconds", which is exactly what just failed several
    /// times over. What is left is a plain re-authentication — no advice of
    /// our own, and classified so the client offers its own way back.
    pub(crate) fn after_retries_exhausted(self) -> Self {
        match self {
            Self::SelfHealing => Self::ManualLogin,
            provider_or_manual => provider_or_manual,
        }
    }

    /// What to tell the user beyond the failure itself.
    pub(crate) fn advice(&self) -> Option<String> {
        match self {
            Self::SelfHealing => Some(
                "Authentication is temporarily unavailable (often a network blip right \
                 after wake). Your session is still signed in and will recover \
                 automatically — retry in a few seconds; no need to run /login."
                    .to_owned(),
            ),
            Self::ProviderLogin { label } => {
                Some(crate::auth::error::provider_login_message(label.as_deref()).into_owned())
            }
            Self::ManualLogin => None,
        }
    }
}

/// Outcome of a bounded best-effort mint, for callers that must distinguish
/// "the deadline elapsed with the exchange still in flight" (spawn-don't-drop)
/// from a refresh that actually resolved: forcing a second mint after a
/// deadline only queues behind the detached exchange for up to another full
/// budget.
pub(crate) enum BoundedRefresh {
    /// The chain finished inside the budget with this result. Boxed like
    /// [`SilentRefresh::Renewed`]: `EchoAgentAuth` is large and the other variant
    /// is unit-sized.
    Resolved(Box<Result<EchoAgentAuth, AuthError>>),
    /// The spawned chain outlived the budget and continues in the background
    /// (persisting and hot-swapping any minted token when it lands).
    DeadlineElapsed,
}

/// What a [`AuthManager::silent_refresh`] attempt leaves the caller holding.
#[derive(Debug, Clone)]
pub(crate) enum SilentRefresh {
    /// The credential [`AuthManager::auth`] vouched for — the one the next
    /// request would carry.
    ///
    /// Carried, not re-read: `auth()` also succeeds on its grace arm, serving a
    /// token that is still wire-valid but inside the early-invalidation buffer,
    /// and [`AuthManager::current`] hides exactly that token. A caller that
    /// answered `Renewed` with `current()` would reject the session this
    /// outcome just accepted — and disagree with the `Failed(SelfHealing)` arm
    /// on the very same credential.
    Renewed(Box<EchoAgentAuth>),
    Failed(AuthRemedy),
}

impl AuthManager {
    /// Attempt one unattended refresh, bounded because the caller's response
    /// gates the client's first draw.
    ///
    /// Spawned rather than awaited inline: dropping the future at the deadline
    /// abandons an IdP exchange whose rotated refresh token the server may
    /// already have burned, which is how a suspend mid-refresh revoked whole
    /// token families in the field.
    pub(crate) async fn silent_refresh(self: &Arc<Self>) -> SilentRefresh {
        let manager = Arc::clone(self);
        let attempt = tokio::spawn(async move { manager.auth().await });
        let outcome =
            match tokio::time::timeout(crate::http::STARTUP_AUTH_REFRESH_TIMEOUT, attempt).await {
                Ok(Ok(Ok(auth))) => SilentRefresh::Renewed(Box::new(auth)),
                _ => SilentRefresh::Failed(self.auth_remedy()),
            };
        // The variant, not the outcome: the `Renewed` payload is a credential.
        let logged = match &outcome {
            SilentRefresh::Renewed(_) => "Renewed".to_owned(),
            SilentRefresh::Failed(remedy) => format!("Failed({remedy:?})"),
        };
        echo_agent_telemetry::unified_log::info(
            "auth: silent refresh",
            None,
            Some(serde_json::json!({ "outcome": logged })),
        );
        outcome
    }

    /// Bounded best-effort mint for RPC paths that must answer promptly.
    /// Thin wrapper over [`AuthManager::refresh_chain_bounded_outcome`] for
    /// callers that treat a deadline like any other retryable failure.
    pub(crate) async fn refresh_chain_bounded(
        self: &Arc<Self>,
        token_type: TokenType,
        reason: RefreshReason,
        budget: Duration,
    ) -> Result<EchoAgentAuth, AuthError> {
        match self
            .refresh_chain_bounded_outcome(token_type, reason, budget)
            .await
        {
            BoundedRefresh::Resolved(result) => *result,
            BoundedRefresh::DeadlineElapsed => Err(AuthError::transient(
                "bounded refresh deadline elapsed; refresh continues in background",
            )),
        }
    }

    /// Bounded best-effort mint, deadline distinguished (see
    /// [`BoundedRefresh`]).
    ///
    /// Spawned rather than awaited inline, like [`AuthManager::silent_refresh`]:
    /// dropping the future at the deadline abandons an IdP exchange whose
    /// rotated refresh token the server may already have burned. On deadline
    /// the spawned chain runs to completion (persisting and hot-swapping any
    /// minted token) while the caller gets [`BoundedRefresh::DeadlineElapsed`].
    pub(crate) async fn refresh_chain_bounded_outcome(
        self: &Arc<Self>,
        token_type: TokenType,
        reason: RefreshReason,
        budget: Duration,
    ) -> BoundedRefresh {
        let manager = Arc::clone(self);
        let attempt = tokio::spawn(async move { manager.refresh_chain(token_type, reason).await });
        let (result, outcome) = match tokio::time::timeout(budget, attempt).await {
            Ok(Ok(Ok(auth))) => (BoundedRefresh::Resolved(Box::new(Ok(auth))), "ok"),
            Ok(Ok(Err(err))) => (BoundedRefresh::Resolved(Box::new(Err(err))), "err"),
            Ok(Err(join_error)) => {
                // A JoinError here means the chain panicked (the handle is
                // never aborted), possibly after the IdP rotated the refresh
                // token but before persistence — an indeterminate credential
                // state. Non-retryable: a transient would invite an immediate
                // re-mint that could re-spend the rotated token.
                tracing::error!(
                    is_panic = join_error.is_panic(),
                    "bounded refresh task failed"
                );
                echo_agent_telemetry::unified_log::error(
                    "auth: bounded refresh task failed",
                    None,
                    Some(serde_json::json!({
                        "reason": format!("{reason:?}"),
                        "is_panic": join_error.is_panic(),
                    })),
                );
                // Record the verdict, not just the returned error: the ad-hoc
                // permanent below reaches only THIS caller, while any other
                // path (the spawned post-unblock retry's forced
                // `ServerRejected` chain included) would walk straight back
                // into `refresh_chain` and could re-spend the possibly-rotated
                // RT. A recorded verdict short-circuits every re-attempt at
                // step 1b for the TTL; `Other` is non-sticky, so a later
                // login / sibling adopt clears it, and a rotated key landing
                // on disk falls outside the verdict's key scope.
                if let Some(key) = self.attempted_verdict_key(reason) {
                    self.record_permanent_failure(
                        key,
                        crate::auth::error::RefreshTokenFailedReason::Other.into(),
                    );
                }
                (
                    BoundedRefresh::Resolved(Box::new(Err(AuthError::permanent(
                        crate::auth::error::RefreshTokenFailedReason::Other,
                    )))),
                    "join_error",
                )
            }
            Err(_) => (BoundedRefresh::DeadlineElapsed, "timeout"),
        };
        // The variant, not the outcome: the `Ok` payload is a credential.
        echo_agent_telemetry::unified_log::info(
            "auth: bounded refresh",
            None,
            Some(serde_json::json!({
                "reason": format!("{reason:?}"),
                "outcome": outcome,
            })),
        );
        result
    }

    /// Classify the current credential's way back.
    ///
    /// The provider arm deliberately ignores the recorded verdict: real
    /// interactive-only binaries block until something kills them, so their
    /// run routinely ends with nothing recorded at all.
    pub(crate) fn auth_remedy(&self) -> AuthRemedy {
        let provider_mints_sessions = self.is_external_provider_refresh_authority();
        let user_must_act = self.requires_manual_reauth()
            || (provider_mints_sessions && self.current_wire_valid().is_none());
        match (user_must_act, provider_mints_sessions) {
            (false, _) => AuthRemedy::SelfHealing,
            (true, true) => AuthRemedy::ProviderLogin {
                label: self.echo_agent_com_config().auth_provider_label.clone(),
            },
            (true, false) => AuthRemedy::ManualLogin,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::model::AuthMode;
    use crate::auth::refresh::{RefreshOutcome, TokenRefresher};
    use crate::auth::{EchoAgentComConfig, error::RefreshTokenFailedReason};
    use chrono::{Duration, Utc};

    fn external_provider_config() -> EchoAgentComConfig {
        EchoAgentComConfig {
            auth_provider_command: Some("acme-auth".to_owned()),
            auth_provider_label: Some("Acme SSO".to_owned()),
            ..EchoAgentComConfig::default()
        }
    }

    fn external_credential(expires_at: chrono::DateTime<Utc>) -> EchoAgentAuth {
        EchoAgentAuth {
            key: "external".into(),
            auth_mode: AuthMode::External,
            expires_at: Some(expires_at),
            ..EchoAgentAuth::test_default()
        }
    }

    /// Wired as production wires it, so nothing here passes on the
    /// "no refresh authority" arm.
    fn provider_manager(dir: &std::path::Path, credential: EchoAgentAuth) -> Arc<AuthManager> {
        let config = external_provider_config();
        let command = config.auth_provider_command.clone();
        let manager = Arc::new(AuthManager::new(dir, config));
        manager.hot_swap(credential);
        manager.configure_refresher(command, None);
        manager
    }

    /// The verdict-free arm.
    #[test]
    fn hard_expired_external_credential_needs_the_provider_without_a_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let manager = provider_manager(
            dir.path(),
            external_credential(Utc::now() - Duration::hours(1)),
        );
        assert!(!manager.has_permanent_failure());
        assert_eq!(
            manager.auth_remedy(),
            AuthRemedy::ProviderLogin {
                label: Some("Acme SSO".to_owned())
            }
        );
    }

    /// A bare-token credential the backend rejects: it never expires locally,
    /// so only the verdict from the failed run says the user has to act.
    #[test]
    fn wire_valid_external_credential_needs_the_provider_once_its_run_failed() {
        let dir = tempfile::tempdir().unwrap();
        let manager = provider_manager(
            dir.path(),
            external_credential(Utc::now() + Duration::hours(1)),
        );
        assert_eq!(manager.auth_remedy(), AuthRemedy::SelfHealing);

        manager.record_permanent_failure(
            "external".to_owned(),
            RefreshTokenFailedReason::ProviderInteractiveRequired.into(),
        );
        assert_eq!(
            manager.auth_remedy(),
            AuthRemedy::ProviderLogin {
                label: Some("Acme SSO".to_owned())
            }
        );
    }

    /// Inside the early-invalidation buffer the proxy still accepts the token,
    /// so a failed refresh must not cost the user a login.
    #[test]
    fn buffer_window_external_credential_is_self_healing() {
        let dir = tempfile::tempdir().unwrap();
        let manager = provider_manager(
            dir.path(),
            external_credential(Utc::now() + Duration::minutes(1)),
        );
        assert!(manager.current().is_none(), "buffer window hides the token");
        assert_eq!(manager.auth_remedy(), AuthRemedy::SelfHealing);
    }

    /// A provider command configured alongside OIDC must not capture OIDC's
    /// own refresh path.
    #[test]
    fn expired_oidc_credential_with_a_refresh_token_is_self_healing() {
        let dir = tempfile::tempdir().unwrap();
        let manager = provider_manager(
            dir.path(),
            EchoAgentAuth {
                key: "expired-oidc".into(),
                auth_mode: AuthMode::Oidc,
                refresh_token: Some("rt-live".into()),
                expires_at: Some(Utc::now() - Duration::hours(1)),
                ..EchoAgentAuth::test_default()
            },
        );
        assert_eq!(manager.auth_remedy(), AuthRemedy::SelfHealing);
    }

    /// With no provider command there is no binary to escalate to.
    #[test]
    fn expired_credential_without_a_provider_command_needs_a_manual_login() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
        manager.hot_swap(external_credential(Utc::now() - Duration::hours(1)));
        manager.configure_refresher(None, None);
        assert_eq!(manager.auth_remedy(), AuthRemedy::SelfHealing);

        manager.record_permanent_failure(
            "external".to_owned(),
            RefreshTokenFailedReason::ProviderInteractiveRequired.into(),
        );
        assert_eq!(manager.auth_remedy(), AuthRemedy::ManualLogin);
    }

    /// A refresh that fails over a token still inside the early-invalidation
    /// buffer is a *success* for [`AuthManager::silent_refresh`]: `auth()`
    /// serves the cached bearer the proxy still accepts. `current()` hides that
    /// token, so `Renewed` must carry the credential — a caller re-reading
    /// `current()` here would reject a session that `Failed(SelfHealing)`, on
    /// this very credential, would have accepted via `current_or_expired()`.
    #[tokio::test]
    async fn renewed_carries_the_wire_valid_bearer_the_buffer_hides() {
        struct OfflineRefresher;
        #[async_trait::async_trait]
        impl TokenRefresher for OfflineRefresher {
            async fn refresh(
                &self,
                _reason: crate::auth::manager::RefreshReason,
            ) -> RefreshOutcome {
                RefreshOutcome::transient("network unreachable")
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
        // CI runs in pods where `is_devbox_environment()` is true; a mint would
        // resolve the credential for the wrong reason.
        manager.set_devbox_env_for_test(false);
        // A minute from real expiry: inside the 5-min buffer, still on the wire.
        manager.hot_swap(EchoAgentAuth {
            key: "wire-valid".into(),
            auth_mode: AuthMode::Oidc,
            refresh_token: Some("rt-live".into()),
            expires_at: Some(Utc::now() + Duration::minutes(1)),
            ..EchoAgentAuth::test_default()
        });
        manager.set_refresher(Arc::new(OfflineRefresher));
        assert!(manager.current().is_none(), "the buffer hides the token");
        assert!(manager.is_expired(), "and reports the session expired");

        let SilentRefresh::Renewed(auth) = manager.silent_refresh().await else {
            panic!("auth()'s grace arm serves the wire-valid bearer");
        };
        assert_eq!(auth.key, "wire-valid");
        assert!(
            manager.current().is_none(),
            "current() still hides it — which is why the outcome carries it",
        );
        assert!(
            manager.auth_remedy().is_self_healing(),
            "and the other arm would have accepted the same credential",
        );
    }

    /// A panicked mint is an indeterminate credential state: the bounded
    /// wrapper must both return a permanent error AND record the verdict, so
    /// later paths (the spawned post-unblock retry's forced `ServerRejected`
    /// chain included) short-circuit at step 1b instead of walking back into
    /// the IdP and re-spending a possibly-rotated refresh token.
    #[tokio::test]
    async fn panicked_bounded_refresh_records_the_verdict() {
        struct PanickingRefresher;
        #[async_trait::async_trait]
        impl TokenRefresher for PanickingRefresher {
            async fn refresh(
                &self,
                _reason: crate::auth::manager::RefreshReason,
            ) -> RefreshOutcome {
                panic!("mint died mid-exchange");
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
        manager.hot_swap(EchoAgentAuth {
            key: "expired-oidc".into(),
            auth_mode: AuthMode::Oidc,
            refresh_token: Some("rt-live".into()),
            expires_at: Some(Utc::now() - Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        });
        manager.set_refresher(Arc::new(PanickingRefresher));
        assert!(!manager.has_permanent_failure());

        let err = manager
            .refresh_chain_bounded(
                TokenType::OidcSession,
                RefreshReason::ServerRejected,
                std::time::Duration::from_secs(5),
            )
            .await
            .expect_err("a panicked mint is a failure");
        assert!(
            matches!(
                err,
                AuthError::Refresh(crate::auth::error::RefreshTokenError::Permanent(_))
            ),
            "non-retryable for the caller, got: {err:?}"
        );
        assert!(
            manager.has_permanent_failure(),
            "and recorded, so a follow-up chain short-circuits at step 1b \
             instead of re-spending the possibly-rotated refresh token"
        );
    }

    #[test]
    fn turn_surface_matches_the_remedy() {
        assert_eq!(AuthRemedy::SelfHealing.turn_error_type(), "auth_transient");
        assert!(
            AuthRemedy::SelfHealing
                .advice()
                .is_some_and(|a| a.contains("no need to run /login"))
        );
        assert_eq!(AuthRemedy::ManualLogin.turn_error_type(), "auth");
        assert_eq!(
            AuthRemedy::ManualLogin.advice(),
            None,
            "the client's own banner already tells the user to run /login"
        );
        let provider = AuthRemedy::ProviderLogin {
            label: Some("Acme SSO".to_owned()),
        };
        assert_eq!(provider.turn_error_type(), "auth");
        let advice = provider.advice().expect("provider advice");
        assert!(advice.contains("Acme SSO") && advice.contains("/login"));
        assert!(!advice.contains("no need to run /login"));
    }
}
