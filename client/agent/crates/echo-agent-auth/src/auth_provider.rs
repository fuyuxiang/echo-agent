//! Credential dependency-inversion seam for outbound HTTP made by the
//! data-collector. Shell installs `ShellAuthCredentialProvider` wrapping
//! `AuthManager` + `TokenRefresher`; data-collector code holds an
//! `Arc<dyn AuthCredentialProvider>`.

use reqwest::RequestBuilder;

use crate::visibility::HttpAuth;

/// Snapshot of the currently effective credentials. Used by callers
/// that build their own header maps (the OTel OTLP exporter) or that
/// need the bearer prefix for 401-attribution telemetry.
#[derive(Clone, Debug, Default)]
pub struct CredentialSnapshot {
    /// Bearer token. `None` when no auth is configured (CI / `--api-key` headless).
    pub token: Option<String>,
    /// User identifier matching the bearer token's owner. `None` when no auth
    /// is configured or when the underlying provider has no concept of user
    /// identity (`StaticAuthCredentialProvider`). Read by the OTel layer to
    /// populate the `user.id` resource attribute.
    pub user_id: Option<String>,
    /// Team identifier from OAuth. `None` for personal accounts or when
    /// no auth is configured.
    pub team_id: Option<String>,
    /// `uuidv5(NAMESPACE_OID, deployment_key)`, set only for deployment-key auth.
    pub deployment_id: Option<String>,
    /// `uuidv5(NAMESPACE_OID, api_key)`, set only for `AuthMode::ApiKey`.
    pub api_key_id: Option<String>,
    /// Org id from the OIDC `organizationId` claim; `None` for personal / deployment-key auth.
    pub organization_id: Option<String>,
}

/// Source of truth for outbound auth on data-collector requests.
///
/// Supertrait of `HttpAuth` so a single impl satisfies both this trait
/// (refresh-aware snapshot + 401 recovery) and the visibility seam
/// (header construction). Callers add headers via `HttpAuth::apply`.
#[async_trait::async_trait]
pub trait AuthCredentialProvider: HttpAuth + Send + Sync + 'static {
    /// Return the current credential snapshot. Implementations should
    /// issue a cheap disk re-read (`AuthManager::refresh`) before
    /// snapshotting so callers see updates from sibling processes
    /// (`echo-agent-desktop`, `echoagent login`). The `token` field MUST mirror
    /// the bearer that `HttpAuth::apply` would send on the wire so
    /// 401-attribution prefixes match the actual request.
    fn snapshot(&self) -> CredentialSnapshot;

    /// Attempt to obtain a fresh token. Returns `true` if a different
    /// token was obtained -- caller should retry the failed request once.
    /// Returns `false` if no refresher is configured or refresh failed.
    async fn refresh_after_unauthorized(&self) -> bool;

    /// Whether `X-Echo-Agent-Token-Auth` should be sent with the bearer token.
    /// `false` for deployment keys (bare Bearer), `true` for user/OAuth tokens.
    /// See `EchoAgentAuthCredentials::apply()` for the wire format contract.
    fn needs_token_auth_header(&self) -> bool {
        true
    }

    /// Whether the provider holds a credential worth a real outbound attempt —
    /// an unexpired token (in memory or on disk), or a static key. Default
    /// `true` always attempts.
    fn has_usable_credential(&self) -> bool {
        true
    }
}

/// Static credential provider. Used by tests and by callers that pass a
/// raw `&str` token with no `AuthManager` available.
///
/// `apply()` delegates to the underlying `HttpAuth::apply()`.
/// `refresh_after_unauthorized()` always returns `false`.
///
/// `bearer` is the wire bearer the inner `HttpAuth` will send in the
/// `Authorization` header. Stored alongside the inner so `snapshot().token`
/// returns the same prefix that goes out on the wire (used by
/// 401-attribution telemetry). `None` when no bearer is configured.
pub struct StaticAuthCredentialProvider {
    inner: Box<dyn HttpAuth>,
    bearer: Option<String>,
}

impl StaticAuthCredentialProvider {
    /// Wrap `inner` so callers see it as an `AuthCredentialProvider`. Pass
    /// the bearer token that `inner.apply()` will send in the `Authorization`
    /// header so `snapshot().token` reflects the wire bearer truthfully.
    pub fn new(inner: Box<dyn HttpAuth>, bearer: Option<String>) -> Self {
        Self { inner, bearer }
    }
}

impl std::fmt::Debug for StaticAuthCredentialProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticAuthCredentialProvider")
            .field("has_bearer", &self.bearer.is_some())
            .finish()
    }
}

impl HttpAuth for StaticAuthCredentialProvider {
    fn apply(&self, builder: RequestBuilder, base_url: &str) -> RequestBuilder {
        self.inner.apply(builder, base_url)
    }
}

#[async_trait::async_trait]
impl AuthCredentialProvider for StaticAuthCredentialProvider {
    fn snapshot(&self) -> CredentialSnapshot {
        CredentialSnapshot {
            token: self.bearer.clone(),
            ..Default::default()
        }
    }

    async fn refresh_after_unauthorized(&self) -> bool {
        false
    }
}
