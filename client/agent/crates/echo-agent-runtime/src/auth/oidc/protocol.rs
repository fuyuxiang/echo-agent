//! Pure OIDC protocol mechanics: PKCE, discovery, token exchange,
//! refresh_tokens, JWT validation, principal extraction.
//!
//! No `AuthManager` mutation here. The login orchestration is in
//! [`super::login`]; refresh primitives are in [`super::refresh`].
use super::super::config::{
    EchoAgentComConfig, ForceLoginTeam, OAuth2ProviderConfig, OidcAuthConfig,
};
use super::super::{AuthMode, EchoAgentAuth};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{Duration, Utc};
use parking_lot::RwLock;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration as StdDuration, Instant};
#[derive(Debug, Clone, thiserror::Error)]
pub(super) enum OidcError {
    #[error("OIDC not configured")]
    NotConfigured,
    #[error("failed to bind OIDC loopback server: {0}")]
    BindLoopback(String),
    #[error("failed to save OIDC auth: {0}")]
    SaveAuth(String),
    #[error("OIDC discovery failed: HTTP {status} from {url}")]
    DiscoveryHttp { status: u16, url: String },
    /// Keep the "10 minutes" text in sync with `AUTH_CALLBACK_TIMEOUT` in `login.rs`.
    #[error("Login timed out after 10 minutes. Please try again.")]
    CallbackTimeout,
    #[error("OIDC callback channel closed unexpectedly")]
    CallbackChannelClosed,
    #[error("OIDC authentication failed: {0}")]
    CallbackAuthFailed(String),
    #[error("failed to parse pasted input: {0}")]
    InvalidPastedInput(String),
    #[error("OIDC token exchange failed: HTTP {status} — {body}")]
    TokenExchangeHttp { status: u16, body: String },
    #[error("OIDC token refresh failed: HTTP {status} — {body}")]
    TokenRefreshHttp { status: u16, body: String },
    #[error("OIDC authentication failed: state mismatch")]
    StateMismatch,
    #[error("OIDC id_token uses unsupported algorithm: {0}")]
    UnsupportedAlg(String),
    #[error("OIDC id_token alg {alg} is not in discovery supported list")]
    AlgNotInDiscoverySupportedList { alg: String },
    #[error("OIDC id_token missing kid header")]
    IdTokenMissingKid,
    #[error("OIDC discovery missing jwks_uri")]
    DiscoveryMissingJwksUri,
    #[error("OIDC JWK not found for kid={kid}")]
    JwkNotFound { kid: String },
    #[error("OIDC id_token issuer mismatch")]
    IssuerMismatch,
    #[error("OIDC id_token audience mismatch")]
    AudienceMismatch,
    #[error("OIDC id_token nonce mismatch")]
    NonceMismatch,
    #[error("OIDC token response missing id_token")]
    MissingIdToken,
    #[error("OIDC id_token validation failed: {0}")]
    IdTokenValidationFailed(String),
    #[error(
        "This deployment requires logging into {expected}; your login returned {}",
        actual.as_deref().unwrap_or("no team principal")
    )]
    PinnedPrincipalMismatch {
        /// Pre-formatted requirement, e.g. `team <id>` or `one of teams: a, b`.
        expected: String,
        actual: Option<String>,
    },
    #[error(
        "Login is blocked by your administrator: force_login_team_uuid is an empty \
         list, so no team is permitted to sign in"
    )]
    ForceLoginNoPrincipalsAllowed,
}
const ALLOWED_ID_TOKEN_ALGS: &[jsonwebtoken::Algorithm] = &[
    jsonwebtoken::Algorithm::RS256,
    jsonwebtoken::Algorithm::RS384,
    jsonwebtoken::Algorithm::RS512,
    jsonwebtoken::Algorithm::PS256,
    jsonwebtoken::Algorithm::PS384,
    jsonwebtoken::Algorithm::PS512,
    jsonwebtoken::Algorithm::ES256,
    jsonwebtoken::Algorithm::ES384,
    jsonwebtoken::Algorithm::EdDSA,
];
/// Optionally attach an extra access header when the optional non-production
/// feature is enabled and the request targets a matching first-party host.
pub(crate) fn with_alpha_test_key(
    builder: reqwest::RequestBuilder,
    url: &str,
) -> reqwest::RequestBuilder {
    let _ = url;
    builder
}
pub(crate) fn is_configured(config: &EchoAgentComConfig) -> bool {
    config.oidc.is_some()
}
/// Peek at the unverified access token JWT to extract the `principal_type`
/// and `principal_id` chosen during the consent screen.
///
/// When the user picks "Team" on the consent screen, the server strips
/// user-only scopes (`openid`, `email`) and issues the token with
/// `principal_type=Team`. The shell's config doesn't know which principal
/// the user picked, so we peek at the token to find out.
///
/// Returns `(principal_type, principal_id)` or `None` if the token is not
/// a JWT or the claims can't be extracted.
pub(crate) fn peek_access_token_principal(
    access_token: &str,
) -> Option<(String, String, Option<String>)> {
    #[derive(serde::Deserialize)]
    struct MinimalClaims {
        #[serde(default, alias = "principalType")]
        principal_type: Option<String>,
        #[serde(default, alias = "principalId")]
        principal_id: Option<String>,
        #[serde(default)]
        team_id: Option<String>,
    }
    let token_data =
        jsonwebtoken::dangerous::insecure_decode::<MinimalClaims>(access_token).ok()?;
    let pt = token_data.claims.principal_type?;
    let pid = token_data.claims.principal_id?;
    if pt.is_empty() || pid.is_empty() {
        return None;
    }
    let tid = token_data.claims.team_id.filter(|s| !s.is_empty());
    Some((pt, pid, tid))
}
/// Extract just the `principal_id` claim for `force_login_team_uuid` matching,
/// regardless of whether `principal_type` is present. A token can carry the
/// team id in `principal_id` without a `principal_type`; the pin must still
/// match it. Matching the id alone is safe because a user id never collides
/// with a team uuid (distinct id spaces), and the server re-validates the
/// signed token anyway. Returns `None` only when no non-empty `principal_id`
/// is present (which `enforce_login_principal` treats as fail-closed).
pub(crate) fn peek_access_token_principal_id(access_token: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct PrincipalIdClaim {
        #[serde(default, alias = "principalId")]
        principal_id: Option<String>,
    }
    jsonwebtoken::dangerous::insecure_decode::<PrincipalIdClaim>(access_token)
        .ok()?
        .claims
        .principal_id
        .filter(|s| !s.is_empty())
}
/// Resolved allowed-team set from the dedicated `force_login_team_uuid` lockdown
/// knob, or `None` (unrestricted). The legacy `oauth2.principal_id` is
/// intentionally NOT an enforcement gate — it only pre-selects the team on the
/// consent page — so deployments that set it for pre-selection keep letting
/// users pick a team (no surprise login failures on upgrade). Pure for testing.
pub(crate) fn resolve_login_principal_policy(
    force_login_team_uuid: Option<&ForceLoginTeam>,
) -> Option<ForceLoginTeam> {
    force_login_team_uuid.cloned()
}
pub(crate) fn login_principal_policy(cfg: &EchoAgentComConfig) -> Option<ForceLoginTeam> {
    resolve_login_principal_policy(cfg.force_login_team_uuid.as_ref())
}
/// Reject a token whose principal isn't allowed, BEFORE persisting (no partial
/// state). A restriction also rejects a token with no principal (else picking
/// "personal" on the consent page defeats it); an empty `AnyOf` fails closed.
///
/// The `actual` principal comes from the access-token claim
/// (`peek_access_token_principal`, an unverified `insecure_decode`). This
/// client-side check is fail-fast UX / defense-in-depth — NOT the security
/// boundary: the server re-validates the signed token on every API call and
/// is authoritative, so a locally tampered token still cannot reach the API.
pub(crate) fn enforce_login_principal(
    policy: Option<&ForceLoginTeam>,
    actual: Option<&str>,
) -> anyhow::Result<()> {
    let allowed: &[String] = match policy {
        None => return Ok(()),
        Some(ForceLoginTeam::Single(id)) => std::slice::from_ref(id),
        Some(ForceLoginTeam::AnyOf(ids)) if ids.is_empty() => {
            tracing::warn!("OIDC: force_login_team_uuid is an empty list; failing closed");
            return Err(anyhow::Error::new(OidcError::ForceLoginNoPrincipalsAllowed));
        }
        Some(ForceLoginTeam::AnyOf(ids)) => ids,
    };
    if let Some(actual) = actual
        && allowed.iter().any(|a| a == actual)
    {
        return Ok(());
    }
    let expected = if allowed.len() == 1 {
        format!("team {}", allowed[0])
    } else {
        format!("one of teams: {}", allowed.join(", "))
    };
    tracing::warn!(
        expected = %expected,
        actual = ?actual,
        "OIDC: login principal does not satisfy required policy; rejecting"
    );
    Err(anyhow::Error::new(OidcError::PinnedPrincipalMismatch {
        expected,
        actual: actual.map(str::to_owned),
    }))
}
#[derive(Debug)]
pub(super) struct OidcUserInfo {
    pub(super) user_id: String,
    pub(super) email: Option<String>,
    pub(super) first_name: Option<String>,
    pub(super) last_name: Option<String>,
    pub(super) profile_image_asset_id: Option<String>,
    pub(super) principal_type: Option<String>,
    pub(super) principal_id: Option<String>,
    pub(super) team_id: Option<String>,
    pub(super) team_name: Option<String>,
    pub(super) team_role: Option<String>,
    pub(super) organization_id: Option<String>,
    pub(super) organization_name: Option<String>,
    pub(super) organization_role: Option<String>,
    pub(super) user_blocked_reason: Option<String>,
    pub(super) team_blocked_reasons: Vec<String>,
    pub(super) coding_data_retention_opt_out: bool,
}
pub(super) fn build_echo_agent_auth(
    tokens: TokenResponse,
    user_info: OidcUserInfo,
    issuer: &str,
    client_id: &str,
) -> EchoAgentAuth {
    let now = Utc::now();
    EchoAgentAuth {
        key: tokens.access_token,
        auth_mode: AuthMode::Oidc,
        create_time: now,
        user_id: user_info.user_id,
        email: user_info.email,
        first_name: user_info.first_name,
        last_name: user_info.last_name,
        profile_image_asset_id: user_info.profile_image_asset_id,
        principal_type: user_info.principal_type,
        principal_id: user_info.principal_id,
        team_id: user_info.team_id,
        team_name: user_info.team_name,
        team_role: user_info.team_role,
        organization_id: user_info.organization_id,
        organization_name: user_info.organization_name,
        organization_role: user_info.organization_role,
        user_blocked_reason: user_info.user_blocked_reason,
        team_blocked_reasons: user_info.team_blocked_reasons,
        coding_data_retention_opt_out: user_info.coding_data_retention_opt_out,
        has_echo_agent_code_access: None,
        refresh_token: tokens.refresh_token,
        expires_at: tokens.expires_in.map(|s| now + Duration::seconds(s as i64)),
        oidc_issuer: Some(issuer.to_owned()),
        oidc_client_id: Some(client_id.to_owned()),
    }
}
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Discovery {
    pub(super) authorization_endpoint: String,
    pub(super) token_endpoint: String,
    #[serde(default)]
    pub(super) jwks_uri: Option<String>,
    #[serde(default)]
    pub(super) id_token_signing_alg_values_supported: Option<Vec<String>>,
}
/// RFC 8414 says discovery clients SHOULD cache. 1h is short enough
/// that an endpoint move propagates within an agent session, long
/// enough that a discovery-endpoint outage no longer blocks token
/// refresh once the doc is cached.
const DISCOVERY_CACHE_TTL: StdDuration = StdDuration::from_secs(3600);
/// Per-issuer cache of `(Discovery, fetched_at)`. Process-global
/// because the discovery doc is identity-free; multiple AuthManagers
/// pointed at the same IdP share one entry.
static DISCOVERY_CACHE: LazyLock<RwLock<HashMap<String, (Discovery, Instant)>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
pub(super) async fn discover(issuer: &str) -> anyhow::Result<Discovery> {
    let issuer_key = issuer.trim_end_matches('/').to_owned();
    if let Some((doc, at)) = DISCOVERY_CACHE.read().get(&issuer_key)
        && at.elapsed() < DISCOVERY_CACHE_TTL
    {
        return Ok(doc.clone());
    }
    use backon::Retryable;
    let key = issuer_key.clone();
    let doc = (|| {
        let key = key.clone();
        async move { discover_once(&key).await }
    })
    .retry(discovery_retry_policy())
    .await?;
    DISCOVERY_CACHE
        .write()
        .insert(issuer_key, (doc.clone(), Instant::now()));
    Ok(doc)
}
fn discovery_retry_policy() -> backon::ExponentialBuilder {
    backon::ExponentialBuilder::default()
        .with_max_times(2)
        .with_min_delay(StdDuration::from_millis(500))
        .with_max_delay(StdDuration::from_secs(2))
        .with_jitter()
}
async fn discover_once(issuer_key: &str) -> anyhow::Result<Discovery> {
    let url = format!("{issuer_key}/.well-known/openid-configuration");
    tracing::debug!(url = %url, "OIDC: fetching discovery document");
    let resp = with_alpha_test_key(
        crate::http::shared_client()
            .get(&url)
            .timeout(StdDuration::from_secs(10)),
        &url,
    )
    .send()
    .await?;
    if !resp.status().is_success() {
        return Err(anyhow::Error::new(OidcError::DiscoveryHttp {
            status: resp.status().as_u16(),
            url,
        }));
    }
    let doc: Discovery = resp.json().await?;
    tracing::debug!(
        authorization_endpoint = %doc.authorization_endpoint,
        token_endpoint = %doc.token_endpoint,
        jwks_uri = ?doc.jwks_uri,
        id_token_algs = ?doc.id_token_signing_alg_values_supported,
        "OIDC: discovery complete"
    );
    Ok(doc)
}
pub(super) struct Pkce {
    pub(super) code_verifier: String,
    pub(super) code_challenge: String,
}
pub(super) fn generate_pkce() -> Pkce {
    let random_bytes: [u8; 32] = rand::random();
    let code_verifier = URL_SAFE_NO_PAD.encode(random_bytes);
    let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    Pkce {
        code_verifier,
        code_challenge,
    }
}
pub(super) fn build_authorize_url(
    config: &OidcAuthConfig,
    oauth2: Option<&OAuth2ProviderConfig>,
    discovery: &Discovery,
    redirect_uri: &str,
    pkce: &Pkce,
    state: &str,
    nonce: &str,
) -> String {
    let scopes = config.scopes.join(" ");
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}\
         &code_challenge={}&code_challenge_method=S256&state={}&nonce={}",
        discovery.authorization_endpoint,
        urlencoding::encode(&config.client_id),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(&scopes),
        urlencoding::encode(&pkce.code_challenge),
        urlencoding::encode(state),
        urlencoding::encode(nonce),
    );
    if let Some(ref audience) = config.audience {
        url.push_str(&format!("&audience={}", urlencoding::encode(audience)));
    }
    if let Some(oauth2) = oauth2 {
        if let Some(ref principal_type) = oauth2.principal_type {
            url.push_str(&format!(
                "&principal_type={}",
                urlencoding::encode(principal_type)
            ));
        }
        if let Some(ref principal_id) = oauth2.principal_id {
            url.push_str(&format!(
                "&principal_id={}",
                urlencoding::encode(principal_id)
            ));
        }
    }
    let referrer = oauth2
        .and_then(|o| o.referrer.as_deref())
        .filter(|r| !r.is_empty())
        .unwrap_or("echo-agent-build");
    url.push_str(&format!("&referrer={}", urlencoding::encode(referrer)));
    url
}
#[derive(Debug, Deserialize)]
pub(super) struct TokenResponse {
    pub(super) access_token: String,
    #[serde(default)]
    pub(super) refresh_token: Option<String>,
    #[serde(default)]
    pub(super) id_token: Option<String>,
    #[serde(default)]
    pub(super) expires_in: Option<u64>,
}
pub(super) async fn exchange_code(
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    code_verifier: &str,
) -> anyhow::Result<TokenResponse> {
    tracing::debug!(token_endpoint = %token_endpoint, "OIDC: exchanging code for tokens");
    let resp = with_alpha_test_key(
        crate::http::shared_client()
            .post(token_endpoint)
            .header("x-echo-agent-client-version", echo_agent_version::VERSION)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", client_id),
                ("code_verifier", code_verifier),
            ])
            .timeout(std::time::Duration::from_secs(15)),
        token_endpoint,
    )
    .send()
    .await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::Error::new(OidcError::TokenExchangeHttp {
            status,
            body,
        }));
    }
    Ok(resp.json().await?)
}
/// Retry gate for `refresh_tokens`. Defers to `classify_terminal` (the single
/// source of truth): only a recognized terminal code (`invalid_grant`,
/// `invalid_client`) stops retries. Everything else (5xx, 429, bare 4xx, or an
/// unrecognized/RFC-transient code) is retried.
fn is_transient_refresh_error(err: &anyhow::Error) -> bool {
    let Some(OidcError::TokenRefreshHttp { status, body }) = err.downcast_ref::<OidcError>() else {
        return true;
    };
    if *status >= 500 || *status == 429 {
        return true;
    }
    let error_code = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_owned));
    error_code
        .as_deref()
        .and_then(super::refresh::classify_terminal)
        .is_none()
}
/// Up to 3 attempts (1 + 2 retries), 200ms-2s jittered exponential
/// backoff. Bounded so a hard outage still surfaces to the user
/// promptly via the existing `RefreshOutcome::TransientFailure` path.
fn refresh_retry_policy() -> backon::ExponentialBuilder {
    backon::ExponentialBuilder::default()
        .with_max_times(2)
        .with_min_delay(StdDuration::from_millis(200))
        .with_max_delay(StdDuration::from_secs(2))
        .with_jitter()
}
pub(super) async fn refresh_tokens(
    token_endpoint: &str,
    refresh_token: &str,
    client_id: &str,
    principal_type: Option<&str>,
    principal_id: Option<&str>,
) -> anyhow::Result<TokenResponse> {
    use backon::Retryable;
    tracing::debug!(
        token_endpoint = %token_endpoint,
        principal_type = ?principal_type,
        principal_id = ?principal_id,
        "OIDC: refreshing token"
    );
    let probe = super::refresh::SuspendProbe::start();
    (|| {
        refresh_tokens_once(
            token_endpoint,
            refresh_token,
            client_id,
            principal_type,
            principal_id,
        )
    })
    .retry(refresh_retry_policy())
    .when(move |err: &anyhow::Error| {
        if !is_transient_refresh_error(err) {
            return false;
        }
        if probe.straddled_past_grace() {
            crate::unified_log::warn(
                "auth.refresh.retry_suppressed_suspend",
                None,
                Some(serde_json::json!({
                    "suspended_ms": probe.suspended_ms(),
                    "error": err.to_string(),
                })),
            );
            return false;
        }
        true
    })
    .await
}
/// One unretried POST to `token_endpoint`. Errors carry the typed
/// `OidcError::TokenRefreshHttp` so the retry classifier can read the
/// status code and OAuth2 `error` field without re-parsing.
async fn refresh_tokens_once(
    token_endpoint: &str,
    refresh_token: &str,
    client_id: &str,
    principal_type: Option<&str>,
    principal_id: Option<&str>,
) -> anyhow::Result<TokenResponse> {
    let mut params = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    if let Some(pt) = principal_type {
        params.push(("principal_type", pt));
    }
    if let Some(pid) = principal_id {
        params.push(("principal_id", pid));
    }
    let resp = with_alpha_test_key(
        crate::http::shared_client()
            .post(token_endpoint)
            .form(&params)
            .timeout(StdDuration::from_secs(15)),
        token_endpoint,
    )
    .send()
    .await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        let error_code = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(str::to_owned));
        tracing::warn!(
            http_status = status,
            oauth2_error = ?error_code,
            rt_prefix = echo_agent_auth::bearer_suffix(refresh_token),
            client_id = %client_id,
            principal_type = ?principal_type,
            "OIDC: token refresh HTTP error"
        );
        return Err(anyhow::Error::new(OidcError::TokenRefreshHttp {
            status,
            body,
        }));
    }
    Ok(resp.json().await?)
}
#[derive(Debug, Deserialize)]
pub(super) struct IdTokenClaims {
    #[serde(default)]
    pub(super) sub: Option<String>,
    #[serde(default)]
    pub(super) email: Option<String>,
    #[serde(default)]
    pub(super) iss: Option<String>,
    #[serde(default)]
    pub(super) aud: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) nonce: Option<String>,
    #[serde(default, alias = "given_name")]
    pub(super) first_name: Option<String>,
    #[serde(default, alias = "family_name")]
    pub(super) last_name: Option<String>,
    #[serde(default)]
    pub(super) picture: Option<String>,
}
pub(super) fn aud_matches(aud: &serde_json::Value, expected: &str) -> bool {
    match aud {
        serde_json::Value::String(s) => s == expected,
        serde_json::Value::Array(values) => values
            .iter()
            .any(|v| matches!(v, serde_json::Value::String(s) if s == expected)),
        _ => false,
    }
}
pub(super) fn validate_state(expected: &str, received: &str) -> anyhow::Result<()> {
    if received != expected {
        tracing::warn!(expected = %expected, received = %received, "OIDC: state mismatch");
        return Err(anyhow::Error::new(OidcError::StateMismatch));
    }
    Ok(())
}
/// Explicit JWA name mapping — avoids coupling to `jsonwebtoken::Algorithm`'s `Debug` repr.
pub(super) fn alg_to_jwa_name(alg: jsonwebtoken::Algorithm) -> &'static str {
    match alg {
        jsonwebtoken::Algorithm::RS256 => "RS256",
        jsonwebtoken::Algorithm::RS384 => "RS384",
        jsonwebtoken::Algorithm::RS512 => "RS512",
        jsonwebtoken::Algorithm::PS256 => "PS256",
        jsonwebtoken::Algorithm::PS384 => "PS384",
        jsonwebtoken::Algorithm::PS512 => "PS512",
        jsonwebtoken::Algorithm::ES256 => "ES256",
        jsonwebtoken::Algorithm::ES384 => "ES384",
        jsonwebtoken::Algorithm::EdDSA => "EdDSA",
        other => match other {
            jsonwebtoken::Algorithm::HS256 => "HS256",
            jsonwebtoken::Algorithm::HS384 => "HS384",
            jsonwebtoken::Algorithm::HS512 => "HS512",
            _ => "unknown",
        },
    }
}
pub(super) fn ensure_alg_allowed(
    alg: jsonwebtoken::Algorithm,
    discovery_supported_algs: Option<&[String]>,
) -> anyhow::Result<()> {
    let alg_name = alg_to_jwa_name(alg);
    if !ALLOWED_ID_TOKEN_ALGS.contains(&alg) {
        return Err(anyhow::Error::new(OidcError::UnsupportedAlg(
            alg_name.to_owned(),
        )));
    }
    if let Some(supported) = discovery_supported_algs
        && !supported.iter().any(|a| a == alg_name)
    {
        return Err(anyhow::Error::new(
            OidcError::AlgNotInDiscoverySupportedList {
                alg: alg_name.to_owned(),
            },
        ));
    }
    Ok(())
}
pub(super) async fn validate_and_extract_user_info(
    token: &str,
    discovery: &Discovery,
    expected_issuer: &str,
    expected_client_id: &str,
    expected_nonce: &str,
) -> anyhow::Result<OidcUserInfo> {
    let header = jsonwebtoken::decode_header(token)?;
    let kid = header
        .kid
        .ok_or_else(|| anyhow::Error::new(OidcError::IdTokenMissingKid))?;
    let jwks_uri = discovery
        .jwks_uri
        .as_ref()
        .ok_or_else(|| anyhow::Error::new(OidcError::DiscoveryMissingJwksUri))?;
    let jwks: jsonwebtoken::jwk::JwkSet = with_alpha_test_key(
        crate::http::shared_client()
            .get(jwks_uri)
            .timeout(std::time::Duration::from_secs(10)),
        jwks_uri,
    )
    .send()
    .await?
    .error_for_status()?
    .json()
    .await?;
    let jwk = jwks
        .find(&kid)
        .ok_or_else(|| anyhow::Error::new(OidcError::JwkNotFound { kid: kid.clone() }))?;
    let decoding_key = jsonwebtoken::DecodingKey::from_jwk(jwk)?;
    let alg = header.alg;
    ensure_alg_allowed(
        alg,
        discovery.id_token_signing_alg_values_supported.as_deref(),
    )?;
    let mut validation = jsonwebtoken::Validation::new(alg);
    validation.set_issuer(&[expected_issuer]);
    validation.set_audience(&[expected_client_id]);
    validation.validate_exp = true;
    validation.validate_aud = true;
    validation.required_spec_claims = ["sub", "iss", "aud", "exp"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
    let token_data = jsonwebtoken::decode::<IdTokenClaims>(token, &decoding_key, &validation)?;
    if token_data.claims.iss.as_deref() != Some(expected_issuer) {
        return Err(anyhow::Error::new(OidcError::IssuerMismatch));
    }
    if let Some(ref aud) = token_data.claims.aud
        && !aud_matches(aud, expected_client_id)
    {
        return Err(anyhow::Error::new(OidcError::AudienceMismatch));
    }
    if token_data.claims.nonce.as_deref() != Some(expected_nonce) {
        return Err(anyhow::Error::new(OidcError::NonceMismatch));
    }
    Ok(OidcUserInfo {
        user_id: token_data
            .claims
            .sub
            .unwrap_or_else(|| "unknown".to_string()),
        email: token_data.claims.email,
        first_name: token_data.claims.first_name,
        last_name: token_data.claims.last_name,
        profile_image_asset_id: token_data.claims.picture,
        principal_type: None,
        principal_id: None,
        team_id: None,
        team_name: None,
        team_role: None,
        organization_id: None,
        organization_name: None,
        organization_role: None,
        user_blocked_reason: None,
        team_blocked_reasons: vec![],
        coding_data_retention_opt_out: crate::auth::default_coding_data_retention_opt_out(),
    })
}
pub(super) async fn extract_user_info(
    id_token: Option<&str>,
    discovery: &Discovery,
    expected_issuer: &str,
    expected_client_id: &str,
    expected_nonce: &str,
    principal_type: Option<&str>,
    principal_id: Option<&str>,
    fallback_team_id: Option<String>,
) -> anyhow::Result<OidcUserInfo> {
    if principal_type == Some(crate::auth::model::TEAM_PRINCIPAL_TYPE) {
        let team_user_id = principal_id.unwrap_or("unknown").to_owned();
        return Ok(OidcUserInfo {
            user_id: team_user_id,
            email: None,
            first_name: None,
            last_name: None,
            profile_image_asset_id: None,
            principal_type: Some(crate::auth::model::TEAM_PRINCIPAL_TYPE.to_string()),
            principal_id: principal_id.map(ToOwned::to_owned),
            team_id: principal_id.map(ToOwned::to_owned).or(fallback_team_id),
            team_name: None,
            team_role: None,
            organization_id: None,
            organization_name: None,
            organization_role: None,
            user_blocked_reason: None,
            team_blocked_reasons: vec![],
            coding_data_retention_opt_out: crate::auth::default_coding_data_retention_opt_out(),
        });
    }
    let token = id_token.ok_or_else(|| anyhow::Error::new(OidcError::MissingIdToken))?;
    validate_and_extract_user_info(
        token,
        discovery,
        expected_issuer,
        expected_client_id,
        expected_nonce,
    )
    .await
    .map(|mut user_info| {
        user_info.principal_type = principal_type.map(ToOwned::to_owned);
        user_info.principal_id = principal_id.map(ToOwned::to_owned);
        if user_info.team_id.is_none() {
            user_info.team_id = fallback_team_id;
        }
        user_info
    })
    .map_err(|e| anyhow::Error::new(OidcError::IdTokenValidationFailed(e.to_string())))
}
#[cfg(test)]
mod tests {
    use super::super::test_helpers::*;
    use super::*;
    #[test]
    fn pkce_s256_challenge_matches_verifier() {
        let pkce = generate_pkce();
        assert_eq!(pkce.code_verifier.len(), 43);
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.code_verifier.as_bytes()));
        assert_eq!(pkce.code_challenge, expected);
    }
    #[test]
    fn authorize_url_includes_required_oidc_params() {
        let config = OidcAuthConfig {
            issuer: "https://example.okta.com".into(),
            client_id: TEST_CLIENT_ID.into(),
            scopes: vec!["openid".into(), "profile".into()],
            audience: Some("api://echoagent".into()),
        };
        let discovery = Discovery {
            authorization_endpoint: "https://example.okta.com/authorize".into(),
            token_endpoint: "https://example.okta.com/token".into(),
            jwks_uri: None,
            id_token_signing_alg_values_supported: None,
        };
        let pkce = Pkce {
            code_verifier: "v".into(),
            code_challenge: "c".into(),
        };
        let nonce = test_nonce();
        let nonce_q = format!("nonce={nonce}");
        let url = build_authorize_url(
            &config,
            None,
            &discovery,
            "http://127.0.0.1:9999/callback",
            &pkce,
            "state123",
            &nonce,
        );
        for required in [
            "response_type=code",
            "client_id=test-client-id",
            "code_challenge=c",
            "code_challenge_method=S256",
            "state=state123",
            nonce_q.as_str(),
            "scope=openid",
            "audience=api",
            "referrer=echo-agent-build",
        ] {
            assert!(url.contains(required), "missing param: {required}");
        }
        assert_eq!(
            url.matches("referrer=").count(),
            1,
            "expected exactly one referrer param, got: {url}"
        );
    }
    #[test]
    fn authorize_url_includes_team_principal_params() {
        let config = OidcAuthConfig {
            issuer: "https://auth.echo-agent.invalid".into(),
            client_id: TEST_CLIENT_ID.into(),
            scopes: vec!["offline_access".into(), "echo-agent-cli:access".into()],
            audience: None,
        };
        let oauth2 = OAuth2ProviderConfig {
            issuer: "https://auth.echo-agent.invalid".into(),
            client_id: TEST_CLIENT_ID.into(),
            scopes: vec!["offline_access".into(), "echo-agent-cli:access".into()],
            principal_type: Some("Team".into()),
            principal_id: Some("team-123".into()),
            referrer: Some("echo-agent-build".into()),
        };
        let discovery = Discovery {
            authorization_endpoint: "https://auth.echo-agent.invalid/authorize".into(),
            token_endpoint: "https://auth.echo-agent.invalid/token".into(),
            jwks_uri: None,
            id_token_signing_alg_values_supported: None,
        };
        let pkce = Pkce {
            code_verifier: "v".into(),
            code_challenge: "c".into(),
        };
        let url = build_authorize_url(
            &config,
            Some(&oauth2),
            &discovery,
            "http://127.0.0.1:9999/callback",
            &pkce,
            "state123",
            &test_nonce(),
        );
        assert!(url.contains("principal_type=Team"));
        assert!(url.contains("principal_id=team-123"));
        assert!(url.contains("referrer=echo-agent-build"));
        assert_eq!(
            url.matches("referrer=").count(),
            1,
            "expected exactly one referrer param, got: {url}"
        );
    }
    #[test]
    fn authorize_url_uses_oauth2_referrer_override_once() {
        let config = OidcAuthConfig {
            issuer: "https://auth.echo-agent.invalid".into(),
            client_id: TEST_CLIENT_ID.into(),
            scopes: vec!["offline_access".into(), "echo-agent-cli:access".into()],
            audience: None,
        };
        let oauth2 = OAuth2ProviderConfig {
            issuer: "https://auth.echo-agent.invalid".into(),
            client_id: TEST_CLIENT_ID.into(),
            scopes: vec!["offline_access".into(), "echo-agent-cli:access".into()],
            principal_type: None,
            principal_id: None,
            referrer: Some("echo-agent-desktop".into()),
        };
        let discovery = Discovery {
            authorization_endpoint: "https://auth.echo-agent.invalid/authorize".into(),
            token_endpoint: "https://auth.echo-agent.invalid/token".into(),
            jwks_uri: None,
            id_token_signing_alg_values_supported: None,
        };
        let pkce = Pkce {
            code_verifier: "v".into(),
            code_challenge: "c".into(),
        };
        let url = build_authorize_url(
            &config,
            Some(&oauth2),
            &discovery,
            "http://127.0.0.1:9999/callback",
            &pkce,
            "state123",
            &test_nonce(),
        );
        assert!(url.contains("referrer=echo-agent-desktop"));
        assert!(!url.contains("referrer=echo-agent-build"));
        assert_eq!(
            url.matches("referrer=").count(),
            1,
            "expected exactly one referrer param, got: {url}"
        );
    }
    #[tokio::test]
    async fn extract_user_info_allows_team_without_id_token() {
        let discovery = Discovery {
            authorization_endpoint: "https://example.okta.com/authorize".into(),
            token_endpoint: "https://example.okta.com/token".into(),
            jwks_uri: Some("https://example.okta.com/jwks".into()),
            id_token_signing_alg_values_supported: Some(vec!["RS256".into()]),
        };
        let user_info = extract_user_info(
            None,
            &discovery,
            "https://example.okta.com",
            "test-client",
            &test_nonce(),
            Some("Team"),
            Some("team-123"),
            None,
        )
        .await
        .expect("team login should not require id_token");
        assert_eq!(
            user_info.user_id, "team-123",
            "team user_id should be the principal_id"
        );
        assert_eq!(user_info.principal_type.as_deref(), Some("Team"));
        assert_eq!(user_info.principal_id.as_deref(), Some("team-123"));
        assert_eq!(user_info.team_id.as_deref(), Some("team-123"));
        assert!(user_info.email.is_none());
    }
    #[test]
    fn validate_state_rejects_mismatch() {
        let err = validate_state("expected-state", "wrong-state").unwrap_err();
        assert!(
            err.to_string().contains("state mismatch"),
            "unexpected error: {err}"
        );
    }
    #[tokio::test]
    async fn id_token_validation_fails_on_nonce_mismatch() {
        ensure_crypto_provider();
        let (issuer, id_token, discovery, handle) = mock_idp_token().await;
        let err = extract_user_info(
            Some(&id_token),
            &discovery,
            &issuer,
            TEST_CLIENT_ID,
            "wrong-nonce",
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("nonce mismatch"),
            "unexpected error: {err}"
        );
        handle.abort();
    }
    #[test]
    fn rejects_unsupported_id_token_alg() {
        let err = ensure_alg_allowed(jsonwebtoken::Algorithm::HS256, Some(&["RS256".to_string()]))
            .unwrap_err();
        assert!(
            err.to_string().contains("unsupported algorithm"),
            "unexpected error: {err}"
        );
    }
    #[tokio::test]
    async fn id_token_validation_fails_on_audience_mismatch() {
        ensure_crypto_provider();
        let (issuer, id_token, discovery, handle) = mock_idp_token().await;
        let err = extract_user_info(
            Some(&id_token),
            &discovery,
            &issuer,
            "wrong-client",
            &test_nonce(),
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("audience mismatch")
                || err.to_string().contains("InvalidAudience"),
            "unexpected error: {err}"
        );
        handle.abort();
    }
    /// JWT principal-extraction matrix:
    ///   - team JWT: extracts (Team, team_id, None)
    ///   - non-JWT garbage / empty: returns None
    ///   - JWT without principal_type/_id: returns None
    #[test]
    fn peek_access_token_principal_matrix() {
        ensure_crypto_provider();
        fn make_jwt(claims: serde_json::Value) -> String {
            let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
            jsonwebtoken::encode(
                &header,
                &claims,
                &jsonwebtoken::EncodingKey::from_secret(b"test-secret"),
            )
            .unwrap()
        }
        let team_jwt = make_jwt(serde_json::json!({
            "sub": "user-42",
            "iss": "https://auth.echo-agent.invalid",
            "aud": "test-client",
            "exp": 9999999999u64,
            "iat": 1000000000u64,
            "scope": "offline_access echo-agent-cli:access api:access",
            "principal_type": "Team",
            "principal_id": "team-abc-123",
            "client_id": "test-client",
            "jti": "token-1",
        }));
        let (pt, pid, tid) = peek_access_token_principal(&team_jwt).expect("team principal");
        assert_eq!(pt, "Team");
        assert_eq!(pid, "team-abc-123");
        assert_eq!(tid, None);
        assert!(peek_access_token_principal("not-a-jwt-token").is_none());
        assert!(peek_access_token_principal("").is_none());
        let no_principal = make_jwt(serde_json::json!({
            "sub": "user-42",
            "iss": "https://auth.echo-agent.invalid",
            "aud": "test-client",
            "exp": 9999999999u64,
            "iat": 1000000000u64,
        }));
        assert!(peek_access_token_principal(&no_principal).is_none());
    }
    /// `peek_access_token_principal_id` extracts the id even when
    /// `principal_type` is absent, where the stricter
    /// `peek_access_token_principal` returns `None`.
    #[test]
    fn peek_access_token_principal_id_does_not_require_type() {
        ensure_crypto_provider();
        fn make_jwt(claims: serde_json::Value) -> String {
            jsonwebtoken::encode(
                &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
                &claims,
                &jsonwebtoken::EncodingKey::from_secret(b"test-secret"),
            )
            .unwrap()
        }
        let id_only = make_jwt(serde_json::json!({ "principal_id": "team-abc", "sub": "u" }));
        assert_eq!(
            peek_access_token_principal_id(&id_only).as_deref(),
            Some("team-abc"),
        );
        assert!(
            peek_access_token_principal(&id_only).is_none(),
            "the strict peek still needs principal_type",
        );
        let none = make_jwt(serde_json::json!({ "sub": "u" }));
        assert!(peek_access_token_principal_id(&none).is_none());
        assert!(peek_access_token_principal_id("not-a-jwt").is_none());
    }
    /// Enforcement matrix: None passes; Single/AnyOf require a match (and reject
    /// a no-principal token); empty AnyOf fails closed.
    #[test]
    fn enforce_login_principal_matrix() {
        assert!(enforce_login_principal(None, None).is_ok());
        assert!(enforce_login_principal(None, Some("team-abc")).is_ok());
        let single = ForceLoginTeam::Single("team-abc".into());
        assert!(enforce_login_principal(Some(&single), Some("team-abc")).is_ok());
        let err = enforce_login_principal(Some(&single), Some("team-other")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "This deployment requires logging into team team-abc; \
             your login returned team-other",
        );
        let err = enforce_login_principal(Some(&single), None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "This deployment requires logging into team team-abc; \
             your login returned no team principal",
        );
        let any_of = ForceLoginTeam::AnyOf(vec!["team-a".into(), "team-b".into()]);
        assert!(enforce_login_principal(Some(&any_of), Some("team-b")).is_ok());
        let err = enforce_login_principal(Some(&any_of), Some("team-c")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "This deployment requires logging into one of teams: team-a, team-b; \
             your login returned team-c",
        );
        let err = enforce_login_principal(Some(&ForceLoginTeam::AnyOf(vec![])), Some("team-a"))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Login is blocked by your administrator: force_login_team_uuid is an empty \
             list, so no team is permitted to sign in",
        );
    }
    /// Only the dedicated `force_login_team_uuid` knob produces an enforcement
    /// policy; the legacy `oauth2.principal_id` is pre-select-only and never an
    /// enforcement gate (regression guard for the upgrade-behavior concern).
    #[test]
    fn resolve_login_principal_policy_uses_force_login_team_only() {
        assert_eq!(resolve_login_principal_policy(None), None);
        assert_eq!(
            resolve_login_principal_policy(Some(&ForceLoginTeam::Single("team-locked".into()))),
            Some(ForceLoginTeam::Single("team-locked".into())),
        );
        assert_eq!(
            resolve_login_principal_policy(Some(&ForceLoginTeam::AnyOf(vec![
                "a".into(),
                "b".into()
            ]))),
            Some(ForceLoginTeam::AnyOf(vec!["a".into(), "b".into()])),
        );
    }
    /// Discovery is cached for `DISCOVERY_CACHE_TTL`: the second call
    /// to `discover()` for the same issuer hits the cache and does not
    /// fetch over HTTP. Without this, every refresh pays a discovery
    /// round-trip and a discovery-endpoint blip blocks token refresh.
    #[tokio::test]
    async fn discover_uses_cache_within_ttl() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let hits = std::sync::Arc::new(AtomicU32::new(0));
        let hits_for_handler = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (issuer, issuer_path) = mock_issuer(&listener);
        let issuer_for_handler = issuer.clone();
        let app = axum::Router::new().route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = issuer_for_handler.clone();
                let counter = hits_for_handler.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                    }))
                }
            }),
        );
        let app = axum::Router::new().nest(&issuer_path, app);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let _ = discover(&issuer).await.unwrap();
        let _ = discover(&issuer).await.unwrap();
        let _ = discover(&issuer).await.unwrap();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "discover() must hit the network exactly once across 3 calls (cache TTL = 1h)"
        );
        server.abort();
    }
    /// `refresh_tokens` retries on a transient 503 and succeeds on the
    /// next attempt. Without backon, a single IdP blip during refresh
    /// surfaces to the user as a chat failure.
    #[tokio::test]
    async fn refresh_tokens_retries_on_transient_5xx() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let hits = std::sync::Arc::new(AtomicU32::new(0));
        let hits_for_handler = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new().route(
            "/token",
            axum::routing::post(move || {
                let counter = hits_for_handler.clone();
                async move {
                    let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    if n == 1 {
                        (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            "upstream busy".to_string(),
                        )
                    } else {
                        (
                            axum::http::StatusCode::OK,
                            r#"{"access_token":"new-at","expires_in":3600}"#.to_string(),
                        )
                    }
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let token_endpoint = format!("http://127.0.0.1:{port}/token");
        let resp = refresh_tokens(&token_endpoint, "rt", "client", None, None)
            .await
            .expect("transient 5xx must be retried until success");
        assert_eq!(resp.access_token, "new-at");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "first attempt fails 503, second succeeds — exactly 2 hits"
        );
        server.abort();
    }
    /// Terminal OAuth2 errors (`invalid_grant`, `invalid_client`) MUST
    /// NOT be retried -- retrying a revoked grant just wastes time and
    /// risks rate-limit. Verifies `is_transient_refresh_error` correctly
    /// classifies typed 4xx as terminal.
    #[tokio::test]
    async fn refresh_tokens_does_not_retry_terminal_invalid_grant() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let hits = std::sync::Arc::new(AtomicU32::new(0));
        let hits_for_handler = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new().route(
            "/token",
            axum::routing::post(move || {
                let counter = hits_for_handler.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    (
                        axum::http::StatusCode::BAD_REQUEST,
                        r#"{"error":"invalid_grant","error_description":"refresh token revoked"}"#
                            .to_string(),
                    )
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let token_endpoint = format!("http://127.0.0.1:{port}/token");
        let err = refresh_tokens(&token_endpoint, "rt", "client", None, None)
            .await
            .expect_err("invalid_grant is terminal");
        assert!(
            err.to_string().contains("400") || err.to_string().contains("invalid_grant"),
            "error must surface the IdP rejection, got: {err}"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "terminal OAuth2 error must NOT be retried (exactly 1 hit)"
        );
        server.abort();
    }
    /// A 4xx carrying an OAuth2 code that is NOT a recognized terminal one
    /// (e.g. RFC 6749 `temporarily_unavailable`) must be retried, not given up
    /// on. The retry gate defers to `classify_terminal`, so only the recognized
    /// terminal codes stop retries; everything else is transient.
    #[tokio::test]
    async fn refresh_tokens_retries_on_coded_transient_error() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let hits = std::sync::Arc::new(AtomicU32::new(0));
        let hits_for_handler = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new().route(
            "/token",
            axum::routing::post(move || {
                let counter = hits_for_handler.clone();
                async move {
                    let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    if n == 1 {
                        (
                            axum::http::StatusCode::BAD_REQUEST,
                            r#"{"error":"temporarily_unavailable"}"#.to_string(),
                        )
                    } else {
                        (
                            axum::http::StatusCode::OK,
                            r#"{"access_token":"new-at","expires_in":3600}"#.to_string(),
                        )
                    }
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let token_endpoint = format!("http://127.0.0.1:{port}/token");
        let resp = refresh_tokens(&token_endpoint, "rt", "client", None, None)
            .await
            .expect("a non-terminal coded 4xx must be retried until success");
        assert_eq!(resp.access_token, "new-at");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "temporarily_unavailable must be retried (1 fail + 1 success = 2 hits)"
        );
        server.abort();
    }
    #[test]
    fn callback_timeout_error_is_user_friendly() {
        let err: anyhow::Error = OidcError::CallbackTimeout.into();
        let msg = err.to_string();
        assert!(
            msg.contains("Login timed out after 10 minutes"),
            "expected friendly timeout message, got: {msg}"
        );
        assert!(
            msg.contains("Please try again"),
            "expected 'Please try again' call to action, got: {msg}"
        );
        assert!(
            !msg.contains("OIDC"),
            "should not leak internal 'OIDC' terminology to users, got: {msg}"
        );
        assert!(
            !msg.contains("300s"),
            "should not mention raw seconds, got: {msg}"
        );
    }
}
