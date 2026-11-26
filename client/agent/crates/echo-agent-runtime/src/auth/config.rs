use super::model::TEAM_PRINCIPAL_TYPE;
use crate::env::{PROD_RELAY_WS_URL, PROD_WS_ORIGIN};
use serde::{Deserialize, Serialize};
fn default_oidc_scopes() -> Vec<String> {
    vec![
        "openid".into(),
        "profile".into(),
        "email".into(),
        "offline_access".into(),
        "api:access".into(),
    ]
}
/// Default scopes for the EchoAgent OAuth2 provider. Includes `echo-agent-cli:access`
/// which authorizes the token for API proxy requests.
fn default_oauth2_scopes() -> Vec<String> {
    vec![
        "openid".into(),
        "profile".into(),
        "email".into(),
        "offline_access".into(),
        "echo-agent-cli:access".into(),
        "api:access".into(),
        "conversations:read".into(),
        "conversations:write".into(),
        "workspaces:read".into(),
        "workspaces:write".into(),
    ]
}
fn default_team_oauth2_scopes() -> Vec<String> {
    vec![
        "profile".into(),
        "offline_access".into(),
        "echo-agent-cli:access".into(),
        "api:access".into(),
        "team:read".into(),
        "conversations:read".into(),
        "conversations:write".into(),
        "workspaces:read".into(),
        "workspaces:write".into(),
    ]
}
/// Pin automatic auth to one method via `[auth] preferred_method`. When set and
/// the method is unavailable, auth fails with no fallthrough; unset keeps
/// multi-method fallthrough. Config file only, not remote settings or env.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferredAuthMethod {
    /// `ECHO_AGENT_API_KEY` / auth.json `echoagent::api_key` / per-model BYOK (`echoagent.api_key`).
    ApiKey,
    /// OIDC / OAuth2 session (`cached_token`, interactive `cloud.echo-agent.invalid` / `oidc`,
    /// including devbox-minted OIDC).
    Oidc,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EchoAgentComConfig {
    pub echo_agent_ws_origin: String,
    pub echo_agent_ws_url: String,
    pub token_header: String,
    /// OIDC config for customer-provided IdPs. See [`OidcAuthConfig`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc: Option<OidcAuthConfig>,
    /// OAuth2 provider config. When set, preferred over the legacy relay flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth2: Option<OAuth2ProviderConfig>,
    /// External auth provider command (stdout = token, stderr = user UX, exit 0 = success).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_provider_command: Option<String>,
    /// Login button label (env: `ECHO_AGENT_AUTH_PROVIDER_LABEL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_provider_label: Option<String>,
    /// Token TTL in seconds for external auth providers that output bare
    /// tokens without `expires_in`. Synthesizes `expires_at` so proactive
    /// refresh works. Env: `ECHO_AGENT_AUTH_TOKEN_TTL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token_ttl: Option<u64>,
    /// Admin kill switch: when `Some(true)`, the `echoagent.api_key` auth method is
    /// neither advertised nor accepted, so `ECHO_AGENT_API_KEY`/per-model credentials
    /// can't bypass the deployment's IdP login. Env: `ECHO_AGENT_DISABLE_API_KEY_AUTH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_api_key_auth: Option<bool>,
    /// Restrict login to a specific team: the login token's team principal must
    /// equal this. Also settable via `ECHO_AGENT_FORCE_LOGIN_TEAM_ID`; see
    /// `resolve_force_login_team` for how the tiers resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_login_team_uuid: Option<ForceLoginTeam>,
    /// See [`PreferredAuthMethod`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_method: Option<PreferredAuthMethod>,
}
/// Team login restriction. TOML string or array; an empty array fails closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ForceLoginTeam {
    /// The only allowed team.
    Single(String),
    /// Allowed teams; empty = fail closed.
    AnyOf(Vec<String>),
}
/// Customer OIDC Identity Provider configuration (`[echo_agent_com_config.oidc]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcAuthConfig {
    pub issuer: String,
    pub client_id: String,
    #[serde(default = "default_oidc_scopes")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}
/// OAuth2 provider configuration (`ECHO_AGENT_OAUTH2_ISSUER` / `ECHO_AGENT_OAUTH2_CLIENT_ID`).
///
/// Uses the standard OAuth 2.1 Auth Code + PKCE flow via [`OidcAuthConfig`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2ProviderConfig {
    pub issuer: String,
    pub client_id: String,
    #[serde(default = "default_oauth2_scopes")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_id: Option<String>,
    /// Client-supplied referrer for OAuth usage-attribution analytics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
}
pub const ECHO_AGENT_OAUTH2_ISSUER: &str = "https://auth.echo-agent.invalid";
/// Production accounts-app origin allowlist. Its own const so the frozen
/// contract test pins the production allowlist even when the non-production
/// feature adds staging/local origins.
const PROD_ACCOUNTS_APP_ORIGINS: &[&str] = &["https://accounts.echo-agent.invalid"];
/// Production build: accepts only the production accounts app.
pub(crate) fn allowed_accounts_app_origins() -> Vec<String> {
    PROD_ACCOUNTS_APP_ORIGINS
        .iter()
        .map(|o| o.to_string())
        .collect()
}
/// Build a CORS layer that accepts requests from the accounts-app deployments
/// listed in [`allowed_accounts_app_origins`] for the given HTTP method.
pub(crate) fn accounts_app_cors_layer(method: axum::http::Method) -> tower_http::cors::CorsLayer {
    tower_http::cors::CorsLayer::new()
        .allow_origin(tower_http::cors::AllowOrigin::list(
            allowed_accounts_app_origins()
                .iter()
                .filter_map(|origin| match origin.parse() {
                    Ok(value) => Some(value),
                    Err(_) => {
                        tracing::warn!(origin, "skipping malformed accounts-app CORS origin");
                        None
                    }
                }),
        ))
        .allow_methods([method])
}
/// Local-dev OAuth2 issuer (accounts-app running on localhost).
const ECHO_AGENT_OAUTH2_LOCAL_ISSUER: &str = "http://localhost:22255";
const DEFAULT_OAUTH2_REFERRER: &str = "echo-agent-build";
/// Returns `true` when `ECHO_AGENT_LOCAL_AUTH=1` is set,
/// indicating the local accounts-app should be used as the OAuth2 issuer.
pub(crate) fn use_local_auth() -> bool {
    std::env::var("ECHO_AGENT_LOCAL_AUTH")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}
/// Returns the active EchoAgent OAuth2 issuer: the local-dev issuer when
/// `ECHO_AGENT_LOCAL_AUTH=1` is set, otherwise the production issuer.
pub fn echo_agent_oauth2_issuer() -> &'static str {
    if use_local_auth() {
        ECHO_AGENT_OAUTH2_LOCAL_ISSUER
    } else {
        ECHO_AGENT_OAUTH2_ISSUER
    }
}
/// Whether `issuer` is a recognised EchoAgent OAuth2 issuer (production or local-dev).
/// Use this instead of comparing to [`ECHO_AGENT_OAUTH2_ISSUER`] so local-dev counts as
/// first-party EchoAgent auth.
pub fn is_echo_agent_oauth2_issuer(issuer: &str) -> bool {
    issuer == ECHO_AGENT_OAUTH2_ISSUER || issuer == ECHO_AGENT_OAUTH2_LOCAL_ISSUER
}
/// auth.json scope key used by the pre-OIDC `echoagent login --legacy` flow.
/// Matches the key format produced by the original `accounts.echo-agent.invalid` relay auth.
pub(crate) const LEGACY_AUTH_SCOPE: &str = "https://accounts.echo-agent.invalid/sign-in";
impl EchoAgentComConfig {
    /// Whether `echoagent.api_key` auth is disabled. Pinning a team
    /// (`force_login_team_uuid`) implies this: team membership can't be verified
    /// from a bare API key. The `ECHO_AGENT_DISABLE_API_KEY_AUTH` env lockdown is
    /// OR-ed in live, so a lower-trust user `config.toml` cannot turn it back
    /// off; `requirements.toml` already wins by layer precedence.
    pub(crate) fn api_key_auth_disabled(&self) -> bool {
        self.disable_api_key_auth == Some(true)
            || self.force_login_team_uuid.is_some()
            || env_lockdown_forced()
    }
    /// When `preferred_method = api_key`, automatic OIDC paths (devbox mint,
    /// interactive browser login, external auth provider) must not run: the pin
    /// is fail-closed. Explicit `echoagent login --devbox`/`--api-key` bypass this.
    pub(crate) fn blocks_automatic_oidc(&self) -> bool {
        matches!(self.preferred_method, Some(PreferredAuthMethod::ApiKey))
    }
    /// The auth.json scope key for this config.
    pub fn auth_scope(&self) -> String {
        if let Some(ref oidc) = self.oidc {
            format!("{}::{}", oidc.issuer.trim_end_matches('/'), oidc.client_id)
        } else if let Some(ref oauth2) = self.oauth2 {
            oauth2.auth_scope()
        } else {
            unreachable!("oauth2 config is always present (EchoAgent default or env override)")
        }
    }
}
impl OAuth2ProviderConfig {
    pub fn is_team_principal(&self) -> bool {
        self.principal_type.as_deref() == Some(TEAM_PRINCIPAL_TYPE)
    }
    pub fn from_env() -> Option<Self> {
        let issuer = std::env::var("ECHO_AGENT_OAUTH2_ISSUER").ok()?;
        let client_id = std::env::var("ECHO_AGENT_OAUTH2_CLIENT_ID").ok()?;
        let principal_type = std::env::var("ECHO_AGENT_OAUTH2_PRINCIPAL_TYPE").ok();
        let principal_id = std::env::var("ECHO_AGENT_OAUTH2_PRINCIPAL_ID").ok();
        let default_scopes = match principal_type.as_deref() {
            Some(TEAM_PRINCIPAL_TYPE) => default_team_oauth2_scopes(),
            _ => default_oauth2_scopes(),
        };
        Some(Self {
            issuer,
            client_id,
            scopes: std::env::var("ECHO_AGENT_OAUTH2_SCOPES")
                .map(|s| s.split(',').map(|s| s.trim().to_owned()).collect())
                .unwrap_or(default_scopes),
            principal_type,
            principal_id,
            referrer: Some(
                std::env::var("ECHO_AGENT_OAUTH2_REFERRER")
                    .unwrap_or_else(|_| DEFAULT_OAUTH2_REFERRER.to_owned()),
            ),
        })
    }
    /// Convert to [`OidcAuthConfig`] to reuse the OIDC login flow.
    pub(crate) fn as_oidc(&self) -> OidcAuthConfig {
        OidcAuthConfig {
            issuer: self.issuer.clone(),
            client_id: self.client_id.clone(),
            scopes: self.scopes.clone(),
            audience: None,
        }
    }
    pub(crate) fn base_auth_scope(&self) -> String {
        format!("{}::{}", self.issuer.trim_end_matches('/'), self.client_id)
    }
    pub fn auth_scope(&self) -> String {
        self.base_auth_scope()
    }
}
impl Default for EchoAgentComConfig {
    fn default() -> Self {
        let oidc = OidcAuthConfig::from_env();
        let oauth2 = if oidc.is_some() {
            None
        } else {
            Some(
                OAuth2ProviderConfig::from_env().unwrap_or_else(|| OAuth2ProviderConfig {
                    issuer: echo_agent_oauth2_issuer().to_owned(),
                    client_id: obfstr::obfstr!("b1a00492-073a-47ea-816f-4c329264a828").to_owned(),
                    scopes: default_oauth2_scopes(),
                    principal_type: None,
                    principal_id: None,
                    referrer: Some(DEFAULT_OAUTH2_REFERRER.to_owned()),
                }),
            )
        };
        Self {
            echo_agent_ws_origin: std::env::var("ECHO_AGENT_WS_ORIGIN")
                .unwrap_or_else(|_| PROD_WS_ORIGIN.to_owned()),
            echo_agent_ws_url: std::env::var("ECHO_AGENT_WS_URL")
                .unwrap_or_else(|_| PROD_RELAY_WS_URL.to_owned()),
            token_header: "echo-agent-cli".to_owned(),
            oidc,
            oauth2,
            auth_provider_command: if echo_agent_config::controlled_discovery_home().is_some() { None } else { std::env::var("ECHO_AGENT_AUTH_PROVIDER_COMMAND").ok() },
            auth_provider_label: std::env::var("ECHO_AGENT_AUTH_PROVIDER_LABEL").ok(),
            auth_token_ttl: std::env::var("ECHO_AGENT_AUTH_TOKEN_TTL")
                .ok()
                .and_then(|v| v.parse().ok()),
            disable_api_key_auth: std::env::var("ECHO_AGENT_DISABLE_API_KEY_AUTH")
                .ok()
                .map(|v| env_flag_enabled(&v)),
            force_login_team_uuid: None,
            preferred_method: None,
        }
    }
}
/// Parse a boolean env-var value for echoagent's on/off flags. Bare presence enables
/// the flag, but falsy spellings (`0`, `false`, `off`, `no`, empty) count as
/// disabled, so `ECHO_AGENT_DISABLE_API_KEY_AUTH=false` does NOT enable the flag.
fn env_flag_enabled(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "off" | "no"
    )
}
/// True when the admin has set `ECHO_AGENT_DISABLE_API_KEY_AUTH` to a truthy value in
/// the process environment. Read live (call-time) and OR-ed into
/// `api_key_auth_disabled()` so the env lockdown is non-overridable by a
/// user-layer `config.toml`.
fn env_lockdown_forced() -> bool {
    std::env::var("ECHO_AGENT_DISABLE_API_KEY_AUTH")
        .ok()
        .is_some_and(|v| env_flag_enabled(&v))
}
/// Env var for the login-team pin. Named `..._TEAM_ID` (the user-facing "team
/// id") while the config key stays `force_login_team_uuid` for backward
/// compatibility; the two intentionally differ, so do not rename either.
const FORCE_LOGIN_TEAM_ID_ENV: &str = "ECHO_AGENT_FORCE_LOGIN_TEAM_ID";
/// The `ECHO_AGENT_FORCE_LOGIN_TEAM_ID` env override; the env tier in
/// [`resolve_force_login_team`].
pub(crate) fn force_login_team_from_env() -> Option<ForceLoginTeam> {
    let raw = std::env::var(FORCE_LOGIN_TEAM_ID_ENV).ok()?;
    parse_force_login_team(&raw)
}
/// The `force_login_team_uuid` pin from the merged `requirements.toml` / MDM
/// layers; the non-overridable tier in [`resolve_force_login_team`]. Read live
/// so the clamp holds on config-load paths that build `EchoAgentComConfig` without a
/// separate `apply_requirements` pass.
pub(crate) fn force_login_team_from_requirements() -> Option<ForceLoginTeam> {
    force_login_team_from_requirements_value(&crate::config::load_merged_requirements()?)
}
/// Extract the `force_login_team_uuid` pin from a merged requirements value,
/// reading the `[echo_agent_com_config]` key and its `[auth]` alias. A present but
/// unparseable value fails closed (an empty any-of, which blocks login), so a
/// malformed pin on the highest-trust tier cannot silently drop the restriction.
fn force_login_team_from_requirements_value(requirements: &toml::Value) -> Option<ForceLoginTeam> {
    let value = requirements
        .get("echo_agent_com_config")
        .and_then(|section| section.get("force_login_team_uuid"))
        .or_else(|| {
            requirements
                .get("auth")
                .and_then(|section| section.get("force_login_team_uuid"))
        })?;
    match value.clone().try_into() {
        Ok(team) => Some(team),
        Err(_) => {
            tracing::warn!(
                "force_login_team_uuid in requirements.toml is malformed; failing closed"
            );
            Some(ForceLoginTeam::AnyOf(vec![]))
        }
    }
}
/// Resolve the effective login-team pin by tier, highest precedence first:
/// `requirements.toml` / MDM > `ECHO_AGENT_FORCE_LOGIN_TEAM_ID` env > merged
/// user/managed `config.toml`. `requirements` is the non-overridable pin; the
/// env override wins over user config but is clamped by it.
pub(crate) fn resolve_force_login_team(
    requirements: Option<ForceLoginTeam>,
    env: Option<ForceLoginTeam>,
    config: Option<ForceLoginTeam>,
) -> Option<ForceLoginTeam> {
    requirements.or(env).or(config)
}
/// Parse a `ECHO_AGENT_FORCE_LOGIN_TEAM_ID` value into a [`ForceLoginTeam`]: a bare
/// value is a single team, a JSON array is an any-of set (each element trimmed),
/// and an empty or whitespace-only value yields `None`. A value that looks like
/// a JSON array but does not parse fails closed (an empty any-of, which blocks
/// login), so a typo in the array cannot silently drop the restriction.
fn parse_force_login_team(raw: &str) -> Option<ForceLoginTeam> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('[') {
        match serde_json::from_str::<Vec<String>>(trimmed) {
            Ok(teams) => Some(ForceLoginTeam::AnyOf(
                teams.into_iter().map(|t| t.trim().to_owned()).collect(),
            )),
            Err(_) => {
                tracing::warn!(
                    "ECHO_AGENT_FORCE_LOGIN_TEAM_ID is not a valid JSON array; failing closed"
                );
                Some(ForceLoginTeam::AnyOf(vec![]))
            }
        }
    } else {
        Some(ForceLoginTeam::Single(trimmed.to_owned()))
    }
}
impl OidcAuthConfig {
    pub fn from_env() -> Option<Self> {
        let issuer = std::env::var("ECHO_AGENT_OIDC_ISSUER").ok()?;
        let client_id = std::env::var("ECHO_AGENT_OIDC_CLIENT_ID").ok()?;
        Some(Self {
            issuer,
            client_id,
            scopes: std::env::var("ECHO_AGENT_OIDC_SCOPES")
                .map(|s| s.split(',').map(|s| s.trim().to_owned()).collect())
                .unwrap_or_else(|_| default_oidc_scopes()),
            audience: std::env::var("ECHO_AGENT_OIDC_AUDIENCE").ok(),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn team_auth_scope_is_base_scope() {
        let cfg = OAuth2ProviderConfig {
            issuer: "https://auth.echo-agent.invalid".into(),
            client_id: "client-123".into(),
            scopes: default_team_oauth2_scopes(),
            principal_type: Some("Team".into()),
            principal_id: Some("team-abc".into()),
            referrer: Some("echo-agent-build".into()),
        };
        assert_eq!(
            cfg.auth_scope(),
            "https://auth.echo-agent.invalid::client-123"
        );
    }
    #[test]
    fn env_flag_enabled_treats_falsy_spellings_as_off() {
        for off in ["", " ", "0", "false", "FALSE", "off", "No", "  false  "] {
            assert!(!env_flag_enabled(off), "{off:?} should be off");
        }
        for on in ["1", "true", "yes", "on", "enabled"] {
            assert!(env_flag_enabled(on), "{on:?} should be on");
        }
    }
    #[test]
    fn personal_auth_scope_is_base_scope() {
        let cfg = OAuth2ProviderConfig {
            issuer: "https://auth.echo-agent.invalid".into(),
            client_id: "client-123".into(),
            scopes: default_oauth2_scopes(),
            principal_type: None,
            principal_id: None,
            referrer: Some("echo-agent-build".into()),
        };
        assert_eq!(
            cfg.auth_scope(),
            "https://auth.echo-agent.invalid::client-123"
        );
    }
    /// FROZEN loopback contract: the accounts-app origins the CLI's loopback
    /// callback server accepts cross-origin requests from. The consent page
    /// (served from accounts.echo-agent.invalid) delivers the code via `fetch(..., cors)`, so
    /// removing an origin breaks loopback delivery for already-installed CLIs.
    /// Keep in sync with the oauth2-provider / accounts-app deployments.
    /// Non-production / local-dev origins are opt-in only.
    #[test]
    fn allowed_accounts_app_origins_are_frozen() {
        assert_eq!(
            PROD_ACCOUNTS_APP_ORIGINS,
            &["https://accounts.echo-agent.invalid"]
        );
        assert_eq!(allowed_accounts_app_origins(), PROD_ACCOUNTS_APP_ORIGINS);
    }
    /// FROZEN client contract: the 10 scopes the EchoAgent OAuth2 client requests.
    /// The server must keep accepting all of them; existing tokens carry
    /// exactly this set.
    #[test]
    fn default_oauth2_scopes_are_frozen() {
        let scopes = default_oauth2_scopes();
        let scopes: Vec<&str> = scopes.iter().map(String::as_str).collect();
        assert_eq!(
            scopes,
            [
                "openid",
                "profile",
                "email",
                "offline_access",
                "echo-agent-cli:access",
                "api:access",
                "conversations:read",
                "conversations:write",
                "workspaces:read",
                "workspaces:write",
            ]
        );
    }
    #[test]
    fn preferred_method_deserializes_from_toml() {
        let cfg: EchoAgentComConfig = toml::from_str(
            r#"
            preferred_method = "api_key"
            "#,
        )
        .expect("parse");
        assert_eq!(cfg.preferred_method, Some(PreferredAuthMethod::ApiKey));
        let cfg: EchoAgentComConfig = toml::from_str(
            r#"
            preferred_method = "oidc"
            "#,
        )
        .expect("parse");
        assert_eq!(cfg.preferred_method, Some(PreferredAuthMethod::Oidc));
        let cfg: EchoAgentComConfig = toml::from_str("").expect("parse empty");
        assert_eq!(cfg.preferred_method, None);
    }
    /// Every `ECHO_AGENT_FORCE_LOGIN_TEAM_ID` shape: bare value, arrays, empty-array,
    /// malformed, and empty/whitespace.
    #[test]
    fn parse_force_login_team_handles_all_shapes() {
        assert_eq!(
            parse_force_login_team("  team-abc  "),
            Some(ForceLoginTeam::Single("team-abc".into())),
        );
        assert_eq!(
            parse_force_login_team(r#"["  team-a "]"#),
            Some(ForceLoginTeam::AnyOf(vec!["team-a".into()])),
        );
        assert_eq!(
            parse_force_login_team(r#"["team-a", " team-b "]"#),
            Some(ForceLoginTeam::AnyOf(vec![
                "team-a".into(),
                "team-b".into()
            ])),
        );
        assert_eq!(
            parse_force_login_team("[]"),
            Some(ForceLoginTeam::AnyOf(vec![])),
        );
        assert_eq!(
            parse_force_login_team(r#"["team-a", "team-b"#),
            Some(ForceLoginTeam::AnyOf(vec![])),
        );
        assert_eq!(parse_force_login_team(""), None);
        assert_eq!(parse_force_login_team("   "), None);
    }
    /// Precedence by tier: requirements > env > user/managed config.
    #[test]
    fn resolve_force_login_team_precedence() {
        let req = || Some(ForceLoginTeam::Single("req-team".into()));
        let env = || Some(ForceLoginTeam::Single("env-team".into()));
        let cfg = || Some(ForceLoginTeam::Single("cfg-team".into()));
        assert_eq!(resolve_force_login_team(req(), env(), cfg()), req());
        assert_eq!(resolve_force_login_team(req(), None, cfg()), req());
        assert_eq!(resolve_force_login_team(req(), env(), None), req());
        assert_eq!(resolve_force_login_team(None, env(), cfg()), env());
        assert_eq!(resolve_force_login_team(None, env(), None), env());
        assert_eq!(resolve_force_login_team(None, None, cfg()), cfg());
        assert_eq!(resolve_force_login_team(None, None, None), None);
    }
    /// Requirements extraction from the `[echo_agent_com_config]` key and its `[auth]`
    /// alias. A present but malformed value fails closed (empty any-of), never
    /// `None`; an absent field is `None`.
    #[test]
    fn force_login_team_from_requirements_value_extracts_and_fails_closed() {
        fn pin(toml_str: &str) -> Option<ForceLoginTeam> {
            force_login_team_from_requirements_value(&toml::from_str(toml_str).expect("parse"))
        }
        assert_eq!(
            pin("[echo_agent_com_config]\nforce_login_team_uuid = \"team-a\"\n"),
            Some(ForceLoginTeam::Single("team-a".into())),
        );
        assert_eq!(
            pin("[auth]\nforce_login_team_uuid = [\"team-a\", \"team-b\"]\n"),
            Some(ForceLoginTeam::AnyOf(vec![
                "team-a".into(),
                "team-b".into()
            ])),
        );
        assert_eq!(
            pin("[echo_agent_com_config]\nforce_login_team_uuid = 123\n"),
            Some(ForceLoginTeam::AnyOf(vec![])),
        );
        assert_eq!(pin("[echo_agent_com_config]\n"), None);
        assert_eq!(pin(""), None);
    }
}
