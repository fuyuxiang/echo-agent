pub(crate) mod api_key_probe;
pub(crate) mod attribution;
mod auth_provider;
mod config;
pub mod credential_provider;
#[path = "devbox_login_stub.rs"]
pub(crate) mod devbox_login;
pub(crate) mod device_code;
pub mod error;
mod external_auth;
mod flow;
mod jwt;
pub(crate) mod manager;
mod model;
pub mod oidc;
pub(crate) mod recovery;
pub(crate) mod refresh;
pub(crate) mod single_flight;
mod storage;
mod token_output;
pub(crate) mod token_type;
pub(crate) use api_key_probe::{
    DEFAULT_PROBE_TIMEOUT, first_party_env_key_allows_advertise, should_probe_first_party_env_key,
};
pub use auth_provider::{AuthProviderConfig, AuthProviderRef};
pub(crate) use auth_provider::{
    PROVIDER_TIMEOUT_CEILING_SECS, PROVIDER_TOKEN_EXPIRY_SKEW_SECS, ProviderRefreshOutcome,
};
#[cfg(test)]
pub(crate) use auth_provider::{test_backdate_provider_mint, test_counting_provider};
pub(crate) use config::LEGACY_AUTH_SCOPE;
pub use config::{
    ECHO_AGENT_OAUTH2_ISSUER, EchoAgentComConfig, ForceLoginTeam, OAuth2ProviderConfig,
    OidcAuthConfig, PreferredAuthMethod, echo_agent_oauth2_issuer, is_echo_agent_oauth2_issuer,
};
pub(crate) use config::{
    force_login_team_from_env, force_login_team_from_requirements, resolve_force_login_team,
};
pub(crate) use external_auth::{parse_output, refresh_with_command};
pub(crate) use flow::{
    AuthChannels, mint_session_noninteractive, run_auth_flow, run_auth_flow_with_stderr_bridge,
    try_noninteractive_auth_no_mint,
};
pub use flow::{
    AuthUrlInfo, AuthUrlMode, LoginTransportOverride, LogoutResult, ensure_authenticated,
    ensure_authenticated_or_noninteractive, ensure_authenticated_with_override, perform_logout,
    run_cli_login, run_cli_logout, try_ensure_fresh_auth,
};
pub use jwt::{is_jwt_expired_or_near, parse_jwt_expiration};
mod meta;
pub use error::{AuthError, RefreshTokenError, RefreshTokenFailedReason};
pub use manager::{AuthManager, shared_api_key_provider};
pub(crate) use manager::{AuthRemedy, SilentRefresh};
pub use meta::{AuthMeta, GateInfo};
pub use model::{AuthMode, EchoAgentAuth, lookup_auth};
pub(crate) use model::{TOKEN_TTL, UserInfo, default_coding_data_retention_opt_out, is_expired};
pub(crate) use refresh::DiagnosticUploader;
pub use storage::{clear_api_key, read_api_key, read_auth_json, store_api_key};
