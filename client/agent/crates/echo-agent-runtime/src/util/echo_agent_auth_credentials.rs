use reqwest::RequestBuilder;
use std::sync::Arc;
/// Credentials for authenticating with echoagent backend services.
///
/// Two construction modes:
/// - `with_auth_manager(am)` — live mode. `resolve_async()` drives
///   `AuthManager::get_valid_token()` (memory -> disk -> OIDC refresh).
/// - `new(token)` — static mode. For one-shot callers that don't have
///   an `AuthManager` (visibility checks, bundle fetches, tests).
///
/// Deployment key (enterprise) sends bare `Bearer`, routed to management key auth.
/// User token (EchoAgent users) sends `Bearer` + `X-Echo-Agent-Token-Auth: echo-agent-cli`.
/// Deployment key takes precedence when both are present.
#[derive(Clone)]
pub struct EchoAgentAuthCredentials {
    pub user_token: Option<String>,
    pub deployment_key: Option<String>,
    pub alpha_test_key: Option<String>,
    /// Live auth source. When set, `resolve_async()` drives the full
    /// refresh chain; `resolve()` reads the in-memory cache.
    auth_manager: Option<Arc<crate::auth::AuthManager>>,
}
impl std::fmt::Debug for EchoAgentAuthCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EchoAgentAuthCredentials")
            .field(
                "user_token",
                &self.user_token.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "deployment_key",
                &self.deployment_key.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "mode",
                &if self.auth_manager.is_some() {
                    "live"
                } else {
                    "static"
                },
            )
            .finish()
    }
}
impl EchoAgentAuthCredentials {
    /// Static credentials from a snapshot token. No refresh capability.
    pub fn new(user_token: Option<String>) -> Self {
        Self {
            user_token,
            deployment_key: None,
            alpha_test_key: None,
            auth_manager: None,
        }
    }
    /// Live credentials backed by an `AuthManager`. `resolve_async()`
    /// drives memory -> disk -> OIDC refresh; `resolve()` reads the
    /// in-memory cache for sync contexts.
    pub fn with_auth_manager(mut self, am: Arc<crate::auth::AuthManager>) -> Self {
        self.auth_manager = Some(am);
        self
    }
    /// Return a reference to the internal `AuthManager`, if any.
    pub fn auth_manager(&self) -> Option<&Arc<crate::auth::AuthManager>> {
        self.auth_manager.as_ref()
    }
    /// Error hint for 401 responses, based on which credential was sent.
    pub fn auth_error_hint(&self) -> &'static str {
        if self.deployment_key.is_some() {
            "Your ECHO_AGENT_DEPLOYMENT_KEY is invalid or expired. Please contact a team admin."
        } else if self.user_token.is_some() {
            "Your auth token is invalid or expired. Run `echoagent login` to re-authenticate."
        } else {
            "Not authenticated."
        }
    }
    /// Return a snapshot with the live token from the internal `AuthManager`
    /// if available, falling back to the static `user_token`.
    ///
    /// Uses `current_or_expired()` instead of `current()` so that a token
    /// in the early-invalidation refresh window (expired for proactive
    /// refresh but still accepted by the server) is still returned.
    /// Without this, the `resolve_async()` error fallback returns
    /// credentials with no token, causing requests to be sent without
    /// an Authorization header.
    pub fn resolve(&self) -> EchoAgentAuthCredentials {
        if let Some(ref am) = self.auth_manager
            && let Some(auth) = am.current_or_expired()
        {
            let mut creds = self.clone();
            creds.user_token = Some(auth.key);
            creds
        } else {
            self.clone()
        }
    }
    /// Async resolve via the internal `AuthManager::get_valid_token()`
    /// (memory -> disk -> active OIDC refresh). Falls back to sync
    /// `resolve()` on error so transient refresh failures don't drop
    /// the bearer.
    pub async fn resolve_async(&self) -> EchoAgentAuthCredentials {
        let Some(ref am) = self.auth_manager else {
            return self.clone();
        };
        match am.get_valid_token().await {
            Ok(key) => {
                let mut creds = self.clone();
                creds.user_token = Some(key);
                creds
            }
            Err(e) => {
                tracing::warn!(error = %e, "resolve_credentials_async: active resolve failed, using cached");
                self.resolve()
            }
        }
    }
    pub fn apply(&self, builder: RequestBuilder, base_url: &str) -> RequestBuilder {
        let builder = if let Some(ref key) = self.deployment_key {
            builder.header("Authorization", format!("Bearer {}", key))
        } else if let Some(ref token) = self.user_token {
            builder
                .header("Authorization", format!("Bearer {}", token))
                .header(
                    obfstr::obfstr!("X-Echo-Agent-Token-Auth"),
                    obfstr::obfstr!("echo-agent-cli"),
                )
        } else {
            builder
        };
        let _ = base_url;
        builder
    }
}
impl echo_agent_auth::HttpAuth for EchoAgentAuthCredentials {
    fn apply(&self, builder: RequestBuilder, base_url: &str) -> RequestBuilder {
        EchoAgentAuthCredentials::apply(self, builder, base_url)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthManager, AuthMode, EchoAgentAuth, EchoAgentComConfig};
    use chrono::{Duration, Utc};
    use std::sync::Arc;
    fn make_manager_with_token(
        expires_at: chrono::DateTime<Utc>,
    ) -> (Arc<AuthManager>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
        let auth = EchoAgentAuth {
            key: "test-bearer-token".into(),
            auth_mode: AuthMode::External,
            expires_at: Some(expires_at),
            create_time: Utc::now(),
            ..EchoAgentAuth::test_default()
        };
        mgr.hot_swap(auth);
        (mgr, dir)
    }
    #[test]
    fn resolve_returns_token_when_not_expired() {
        let (mgr, _dir) = make_manager_with_token(Utc::now() + Duration::hours(1));
        let creds = EchoAgentAuthCredentials::new(None).with_auth_manager(mgr);
        let resolved = creds.resolve();
        assert_eq!(resolved.user_token.as_deref(), Some("test-bearer-token"));
    }
    #[test]
    fn resolve_returns_token_during_early_invalidation_window() {
        let (mgr, _dir) = make_manager_with_token(Utc::now() + Duration::minutes(3));
        let creds = EchoAgentAuthCredentials::new(None).with_auth_manager(mgr.clone());
        assert!(mgr.current().is_none());
        assert!(mgr.current_or_expired().is_some());
        assert_eq!(
            creds.resolve().user_token.as_deref(),
            Some("test-bearer-token")
        );
    }
    #[test]
    fn resolve_returns_static_token_when_no_auth_manager() {
        let creds = EchoAgentAuthCredentials::new(Some("static-token".into()));
        assert_eq!(creds.resolve().user_token.as_deref(), Some("static-token"));
    }
    #[test]
    fn resolve_returns_none_when_no_token_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
        let creds = EchoAgentAuthCredentials::new(None).with_auth_manager(mgr);
        assert!(creds.resolve().user_token.is_none());
    }
}
