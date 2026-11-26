//! RFC 8628 Device Authorization Grant -- CLI side.
//!
//! Two-phase API:
//!   1. `request_device_code()` -- POST to server, get code + URL
//!   2. `complete_device_code_login()` -- poll until approved, persist credentials
//!
//! Callers control what happens between the two phases (print to stderr,
//! show in TUI, display in IDE sidebar, etc.).

use std::sync::Arc;

use chrono::{Duration, Utc};
use serde::Deserialize;
use thiserror::Error;

use crate::auth::oidc::with_alpha_test_key;
use crate::auth::{AuthChannels, AuthManager, AuthMode, AuthUrlInfo, AuthUrlMode, EchoAgentAuth};

const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const DEFAULT_DEVICE_POLL_INTERVAL_SECS: i32 = 5;
const DEVICE_SLOW_DOWN_INCREMENT_SECS: u64 = 5;
const MIN_DEVICE_CODE_EXPIRY_FALLBACK_SECS: i64 = 10 * 60;

/// Only the 404 "no device endpoint" case is typed, because the login flow
/// matches on it to fall back to loopback. Every other device-code failure
/// stays a plain `anyhow` error: wrapping one in a `#[error(transparent)]`
/// variant hides the `reqwest::Error` the login funnel classifies, because
/// transparent forwards `source()` past the error it wraps.
#[derive(Debug, Error)]
pub(crate) enum DeviceCodeError {
    #[error(
        "Device-code login is not available for this deployment. \
         Try `echoagent login` or set ECHO_AGENT_API_KEY instead."
    )]
    NotEnabled,
}

// --- Public types ---

/// Low-cardinality client-surface hint sent to the OAuth2 provider as the
/// `x-echo-agent-client-surface` header so device-flow metrics can separate logins a
/// human can actually finish (`Ui`, `Cli`) from headless automation
/// (`Headless`) that mints a device code but can never reach the browser
/// consent page — the traffic that otherwise pollutes the device-flow
/// conversion denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientSurface {
    /// An interactive front-end (TUI / IDE) renders the URL + code to a human.
    Ui,
    /// CLI attached to an interactive terminal (stderr is a TTY).
    Cli,
    /// No interactive surface (CI, container, script): no human can complete.
    Headless,
}

impl ClientSurface {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::Cli => "cli",
            Self::Headless => "headless",
        }
    }
}

/// Classify the CLI (non-TUI) surface: a TTY on stderr means a human is
/// watching the printed URL + code; otherwise we're headless (CI/container/
/// script) and no one will complete the flow.
fn detect_cli_surface() -> ClientSurface {
    use std::io::IsTerminal as _;
    if std::io::stderr().is_terminal() {
        ClientSurface::Cli
    } else {
        ClientSurface::Headless
    }
}

/// Result of requesting a device code from the server.
/// Callers display `verification_uri` + `user_code` to the user,
/// then pass this struct to `complete_device_code_login`.
#[derive(Debug, Clone)]
pub(crate) struct DeviceCode {
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub user_code: String,
    device_code: String,
    interval: i32,
    expires_in: i64,
}

// --- Wire types (serde) ---

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: i64,
    interval: Option<i32>,
}

#[derive(Deserialize)]
struct TokenOk {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    #[expect(dead_code, reason = "field retained for protocol compatibility")]
    scope: Option<String>,
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenErr {
    error: String,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct IdTokenClaims {
    sub: Option<String>,
    email: Option<String>,
}

// --- Phase 1: Request device code ---

/// Request a device code + user code from the OAuth2 provider.
///
/// This is a single HTTP POST. The caller is responsible for displaying
/// `DeviceCode::verification_uri` and `DeviceCode::user_code` to the user
/// before calling `complete_device_code_login`.
pub(crate) async fn request_device_code(
    issuer: &str,
    client_id: &str,
    scopes: &[String],
    surface: ClientSurface,
) -> anyhow::Result<DeviceCode> {
    let client = crate::http::shared_client();
    let url = format!("{}/oauth2/device/code", issuer.trim_end_matches('/'));
    let scope_str = scopes.join(" ");

    let resp = with_alpha_test_key(
        client
            .post(&url)
            // Lets oauth2-provider segment device-flow success by client version.
            .header("x-echo-agent-client-version", echo_agent_version::VERSION)
            // Lets oauth2-provider separate human-completable logins from
            // headless automation in the device-flow funnel metrics.
            .header("x-echo-agent-client-surface", surface.as_str())
            .form(&[
                ("client_id", client_id),
                ("scope", scope_str.as_str()),
                ("referrer", "echo-agent-build"),
            ]),
        &url,
    )
    .send()
    .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 404 {
            anyhow::bail!(DeviceCodeError::NotEnabled);
        }
        anyhow::bail!("Device code request failed (HTTP {status}): {body}");
    }

    let server_resp: DeviceCodeResponse = resp.json().await?;

    // Defend against control characters from a malicious issuer.
    if !server_resp
        .user_code
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        anyhow::bail!("Server returned invalid user_code format (expected [A-Z0-9-])");
    }

    validate_verification_uri(&server_resp.verification_uri)?;
    if let Some(ref verification_uri_complete) = server_resp.verification_uri_complete {
        validate_verification_uri(verification_uri_complete)?;
    }

    Ok(DeviceCode {
        verification_uri: server_resp.verification_uri,
        verification_uri_complete: server_resp.verification_uri_complete,
        user_code: server_resp.user_code,
        device_code: server_resp.device_code,
        interval: server_resp
            .interval
            .unwrap_or(DEFAULT_DEVICE_POLL_INTERVAL_SECS),
        expires_in: server_resp.expires_in,
    })
}

// --- Phase 2: Poll until approved ---

/// Poll the token endpoint until the user approves (or denies / expires).
///
/// On success, persists credentials to `~/.echo-agent/auth.json` and returns
/// the authenticated `EchoAgentAuth`.
///
/// Callers should have already displayed `device_code.verification_uri`
/// and `device_code.user_code` to the user before calling this.
pub(crate) async fn complete_device_code_login(
    issuer: &str,
    client_id: &str,
    device_code: DeviceCode,
    auth_manager: &Arc<AuthManager>,
    surface: ClientSurface,
) -> anyhow::Result<(EchoAgentAuth, bool)> {
    let client = crate::http::shared_client();
    let token_url = format!("{}/oauth2/token", issuer.trim_end_matches('/'));
    let mut poll_interval = std::time::Duration::from_secs(device_code.interval.max(1) as u64);
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(
            device_code
                .expires_in
                .max(MIN_DEVICE_CODE_EXPIRY_FALLBACK_SECS) as u64,
        );

    loop {
        // Sleep first: an immediate poll on a fresh code only returns
        // authorization_pending (and risks slow_down).
        tokio::time::sleep(poll_interval).await;

        if tokio::time::Instant::now() > deadline {
            anyhow::bail!("Device code expired. Run `echoagent login --device-auth` again.");
        }

        let resp = with_alpha_test_key(
            client
                .post(&token_url)
                .header("x-echo-agent-client-version", echo_agent_version::VERSION)
                .header("x-echo-agent-client-surface", surface.as_str())
                .form(&[
                    ("grant_type", DEVICE_GRANT_TYPE),
                    ("device_code", device_code.device_code.as_str()),
                    ("client_id", client_id),
                ]),
            &token_url,
        )
        .send()
        .await?;

        if resp.status().is_success() {
            let tokens: TokenOk = resp.json().await?;
            let auth = build_auth(&tokens, issuer, client_id, auth_manager).await?;
            return Ok((auth, true));
        }

        let err: TokenErr = resp.json().await?;
        let detail = err.error_description.as_deref().unwrap_or(&err.error);
        match err.error.as_str() {
            "authorization_pending" => {
                // User hasn't acted yet -- keep polling.
                continue;
            }
            "slow_down" => {
                poll_interval += std::time::Duration::from_secs(DEVICE_SLOW_DOWN_INCREMENT_SECS);
                continue;
            }
            "access_denied" => {
                tracing::warn!(description = detail, "device auth authorization denied");
                anyhow::bail!("Authorization denied. The user rejected the request.");
            }
            "expired_token" => {
                tracing::warn!(description = detail, "device auth token expired");
                anyhow::bail!("Device code expired. Run `echoagent login --device-auth` again.");
            }
            other => {
                tracing::warn!(
                    error = other,
                    description = detail,
                    "device auth token exchange failed"
                );
                anyhow::bail!("Token exchange error: {detail}");
            }
        }
    }
}

/// Device-code login shared by the TUI and CLI.
///
/// With `channels` (TUI) the verification URL goes to `url_tx` and the browser
/// opens automatically; on failure the copyable URL is the fallback. Without
/// `channels` (CLI) the URL + code are printed to stderr via `prompt_and_poll`.
/// `code_rx` is unused here. The caller reports success (`✓ Signed in`).
///
/// Takes `channels` by `&mut`, consuming it only after the device code is
/// obtained, so callers can reuse it for a loopback fallback on `NotEnabled`.
pub(crate) async fn run_device_code_login_channels(
    issuer: &str,
    client_id: &str,
    scopes: &[String],
    auth_manager: &Arc<AuthManager>,
    channels: &mut Option<AuthChannels>,
) -> anyhow::Result<(EchoAgentAuth, bool)> {
    // A front-end (TUI/IDE) listening on `url_tx` renders the URL to a human, so
    // it's `Ui`. Without one we're on the CLI: a TTY means a human can act
    // (`Cli`), no TTY means headless automation (`Headless`) that will never
    // complete. Computed before `take()` so the `request_device_code` call
    // already carries the surface.
    let surface = if channels.is_some() {
        ClientSurface::Ui
    } else {
        detect_cli_surface()
    };

    let device_code = request_device_code(issuer, client_id, scopes, surface).await?;

    let Some(channels) = channels.take() else {
        // CLI: print the URL + code to stderr.
        return prompt_and_poll(issuer, client_id, device_code, auth_manager, surface).await;
    };

    // TUI: push the URL through the channel BEFORE opening the browser, so
    // `echo.agent/auth/get_url` isn't blocked on a slow/hanging browser launch
    // (e.g. SSH/headless). When the issuer omits `verification_uri_complete`,
    // embed the code so the welcome screen can still show it (anti-phishing).
    let display_uri = match device_code.verification_uri_complete.as_deref() {
        Some(uri) => uri.to_owned(),
        None => {
            let sep = if device_code.verification_uri.contains('?') {
                '&'
            } else {
                '?'
            };
            format!(
                "{}{}user_code={}",
                device_code.verification_uri, sep, device_code.user_code
            )
        }
    };
    if let Some(tx) = channels.url_tx {
        let _ = tx.send(AuthUrlInfo {
            url: display_uri.clone(),
            mode: AuthUrlMode::Device,
        });
    }
    open_browser_detached(&display_uri).await;
    complete_device_code_login(issuer, client_id, device_code, auth_manager, surface).await
}

/// Display the device code to stderr and poll until approved.
async fn prompt_and_poll(
    issuer: &str,
    client_id: &str,
    device_code: DeviceCode,
    auth_manager: &Arc<AuthManager>,
    surface: ClientSurface,
) -> anyhow::Result<(EchoAgentAuth, bool)> {
    let display_uri = device_code
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&device_code.verification_uri);

    eprintln!();
    eprintln!("To sign in, open this URL in your browser:");
    eprintln!();
    eprintln!("  {}", display_uri);
    eprintln!();

    if !open_browser_detached(display_uri).await {
        eprintln!("  (Could not open browser automatically — open the URL above manually.)");
        eprintln!();
    }

    // Show the code to confirm it matches the browser (anti-phishing): a complete
    // URL pre-fills it (just confirm), otherwise the user types it.
    if device_code.verification_uri_complete.is_some() {
        eprintln!("Confirm this code in your browser:");
    } else {
        eprintln!("Then enter this code:");
    }
    eprintln!();
    eprintln!("  {}", device_code.user_code);
    eprintln!();
    eprintln!(
        "\x1b[90mOnly continue with a code you requested. \
         Don't share it with anyone.\x1b[0m"
    );
    eprintln!();
    eprintln!("Waiting for authorization...");

    // The caller prints the `✓ Signed in` confirmation (it also owns the
    // external-provider / devbox early-return paths that never reach here).
    complete_device_code_login(issuer, client_id, device_code, auth_manager, surface).await
}

/// Open `url` in the browser off-thread: `webbrowser::open` is synchronous and
/// would stall the single-threaded TUI loop. Returns `true` on success so the
/// caller can decide how to notify the user (eprintln on CLI, nothing on TUI
/// where the URL is already rendered in the widget).
async fn open_browser_detached(url: &str) -> bool {
    let url = url.to_owned();
    match tokio::task::spawn_blocking(move || webbrowser::open(&url)).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::info!(error = %e, "device auth: could not open browser automatically");
            false
        }
        Err(e) => {
            tracing::info!(error = %e, "device auth: browser-open task failed");
            false
        }
    }
}

// --- Internal helpers ---

/// No id_token signature verification -- token arrives over a direct HTTPS
/// channel (no browser redirect), and is only used for display info (email).
async fn build_auth(
    tokens: &TokenOk,
    issuer: &str,
    client_id: &str,
    auth_manager: &Arc<AuthManager>,
) -> anyhow::Result<EchoAgentAuth> {
    let (user_id, email) = if let Some(ref id_token) = tokens.id_token {
        decode_jwt_claims(id_token)
    } else {
        (String::new(), None)
    };

    let (principal_type, principal_id, token_team_id) =
        match crate::auth::oidc::peek_access_token_principal(&tokens.access_token) {
            Some((pt, pid, tid)) => (Some(pt), Some(pid), tid),
            None => (None, None, None),
        };

    // Device flow has no pre-selection; verify the token's principal here.
    // Match the principal id even if `principal_type` is absent.
    let principal_policy =
        crate::auth::oidc::login_principal_policy(auth_manager.echo_agent_com_config());
    crate::auth::oidc::enforce_login_principal(
        principal_policy.as_ref(),
        crate::auth::oidc::peek_access_token_principal_id(&tokens.access_token).as_deref(),
    )?;

    let (user_id, email, team_id, organization_id) =
        match (principal_type.as_deref(), principal_id.as_deref()) {
            (Some(pt), Some(principal_id)) if pt == crate::auth::model::TEAM_PRINCIPAL_TYPE => (
                principal_id.to_owned(),
                None,
                Some(principal_id.to_owned()),
                None,
            ),
            (Some("Organization"), Some(principal_id)) => (
                principal_id.to_owned(),
                None,
                None,
                Some(principal_id.to_owned()),
            ),
            _ => (user_id, email, token_team_id, None),
        };

    let now = Utc::now();
    let mut auth = EchoAgentAuth {
        key: tokens.access_token.clone(),
        auth_mode: AuthMode::Oidc,
        create_time: now,
        user_id,
        email,
        first_name: None,
        last_name: None,
        profile_image_asset_id: None,
        principal_type,
        principal_id,
        organization_id,
        organization_name: None,
        organization_role: None,
        team_id,
        team_name: None,
        team_role: None,
        user_blocked_reason: None,
        team_blocked_reasons: vec![],
        coding_data_retention_opt_out: crate::auth::default_coding_data_retention_opt_out(),
        has_echo_agent_code_access: None,
        refresh_token: tokens.refresh_token.clone(),
        expires_at: tokens.expires_in.map(|s| now + Duration::seconds(s)),
        oidc_issuer: Some(issuer.to_owned()),
        oidc_client_id: Some(client_id.to_owned()),
    };

    auth_manager.enrich_auth_inline(&mut auth).await;

    auth_manager
        .update(auth)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to save credentials: {e}"))
}

/// Decode JWT payload without signature verification.
/// Returns (sub, Option<email>).
fn decode_jwt_claims(jwt: &str) -> (String, Option<String>) {
    use base64::Engine;
    let parts: Vec<&str> = jwt.splitn(3, '.').collect();
    if parts.len() < 2 {
        return (String::new(), None);
    }
    let payload = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]) {
        Ok(bytes) => bytes,
        Err(_) => return (String::new(), None),
    };
    let claims: IdTokenClaims = match serde_json::from_slice(&payload) {
        Ok(claims) => claims,
        Err(_) => return (String::new(), None),
    };
    (claims.sub.unwrap_or_default(), claims.email)
}

fn validate_verification_uri(uri: &str) -> anyhow::Result<()> {
    if uri.chars().any(|c| c.is_ascii_control()) {
        anyhow::bail!("Server returned invalid verification URI");
    }

    let parsed = url::Url::parse(uri)
        .map_err(|_| anyhow::anyhow!("Server returned invalid verification URI"))?;

    match parsed.scheme() {
        "https" => Ok(()),
        "http" if matches!(parsed.host_str(), Some("localhost") | Some("127.0.0.1")) => Ok(()),
        _ => anyhow::bail!("Server returned unsupported verification URI scheme"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use super::{AuthManager, build_auth, validate_verification_uri};
    use crate::auth::{AuthMode, EchoAgentComConfig};

    #[test]
    fn validate_verification_uri_rejects_unsupported_scheme() {
        let err = validate_verification_uri("javascript:alert(1)").unwrap_err();
        assert_eq!(
            "Server returned unsupported verification URI scheme",
            err.to_string()
        );
    }

    fn auth_manager_with_echo_agent_home(
        echo_agent_home: &std::path::Path,
        proxy_base_url: &str,
    ) -> Arc<AuthManager> {
        Arc::new(
            AuthManager::new(echo_agent_home, EchoAgentComConfig::default())
                .with_proxy_base_url(proxy_base_url),
        )
    }

    #[test]
    fn build_auth_persists_credentials_without_proxy_fetch() {
        let temp_dir = tempfile::tempdir().unwrap();
        let echo_agent_home = temp_dir.path().join(".echo-agent");
        std::fs::create_dir_all(&echo_agent_home).unwrap();
        let auth_manager =
            auth_manager_with_echo_agent_home(&echo_agent_home, "http://127.0.0.1:9");
        let tokens = super::TokenOk {
            access_token: "access-token".to_string(),
            refresh_token: Some("refresh-token".to_string()),
            expires_in: Some(900),
            scope: Some("openid email offline_access echo-agent-cli:access".to_string()),
            id_token: Some(
                "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c2VyLTEyMyIsImVtYWlsIjoiZGV2aWNlLWF1dGhAbG9jYWwudGVzdCJ9.sig".to_string(),
            ),
        };

        let auth = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(build_auth(
                &tokens,
                "http://localhost:22255",
                "client-id",
                &auth_manager,
            ))
            .unwrap();

        assert_eq!("access-token", auth.key);
        assert_eq!(AuthMode::Oidc, auth.auth_mode);
        assert_eq!("user-123", auth.user_id);
        assert_eq!(Some("device-auth@local.test".to_string()), auth.email);
        assert_eq!(Some("refresh-token".to_string()), auth.refresh_token);
        assert_eq!(Some("http://localhost:22255".to_string()), auth.oidc_issuer);
        assert_eq!(Some("client-id".to_string()), auth.oidc_client_id);
        assert!(auth_manager.current().is_some());
    }

    /// jsonwebtoken needs a process-level CryptoProvider; tests that encode
    /// JWTs can't rely on another test having installed it first.
    fn ensure_crypto_provider() {
        let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
    }

    #[test]
    fn build_auth_seeds_team_metadata_from_access_token() {
        ensure_crypto_provider();
        let temp_dir = tempfile::tempdir().unwrap();
        let echo_agent_home = temp_dir.path().join(".echo-agent");
        std::fs::create_dir_all(&echo_agent_home).unwrap();
        let auth_manager =
            auth_manager_with_echo_agent_home(&echo_agent_home, "http://127.0.0.1:9");
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
        let claims = serde_json::json!({
            "sub": "user-42",
            "iss": "https://auth.echo-agent.invalid",
            "aud": "client-id",
            "exp": 9999999999u64,
            "iat": 1000000000u64,
            "scope": "offline_access echo-agent-cli:access team:read",
            "principal_type": "Team",
            "principal_id": "team-123",
            "client_id": "client-id",
            "jti": "token-1",
        });
        let tokens = super::TokenOk {
            access_token: jsonwebtoken::encode(
                &header,
                &claims,
                &jsonwebtoken::EncodingKey::from_secret(b"test-secret"),
            )
            .unwrap(),
            refresh_token: Some("refresh-token".to_owned()),
            expires_in: Some(900),
            scope: Some("offline_access echo-agent-cli:access team:read".to_owned()),
            id_token: None,
        };

        let auth = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(build_auth(
                &tokens,
                "http://localhost:22255",
                "client-id",
                &auth_manager,
            ))
            .unwrap();

        assert_eq!("team-123", auth.user_id);
        assert_eq!(Some("Team".to_owned()), auth.principal_type);
        assert_eq!(Some("team-123".to_owned()), auth.principal_id);
        assert_eq!(Some("team-123".to_owned()), auth.team_id);
        assert_eq!(None, auth.organization_id);
        assert_eq!(None, auth.email);
    }

    /// Team access token carrying `principal_id` (signature irrelevant — only
    /// the principal claims are peeked).
    fn team_access_token(principal_id: &str) -> super::TokenOk {
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
        let claims = serde_json::json!({
            "sub": "user-42",
            "exp": 9999999999u64,
            "principal_type": "Team",
            "principal_id": principal_id,
        });
        super::TokenOk {
            access_token: jsonwebtoken::encode(
                &header,
                &claims,
                &jsonwebtoken::EncodingKey::from_secret(b"test-secret"),
            )
            .unwrap(),
            refresh_token: Some("refresh-token".to_owned()),
            expires_in: Some(900),
            scope: None,
            id_token: None,
        }
    }

    /// `build_auth` with `token_principal` must fail with `expected_err` and
    /// persist nothing.
    fn assert_build_auth_rejected(
        cfg: EchoAgentComConfig,
        token_principal: &str,
        expected_err: &str,
    ) {
        ensure_crypto_provider();
        let temp_dir = tempfile::tempdir().unwrap();
        let echo_agent_home = temp_dir.path().join(".echo-agent");
        std::fs::create_dir_all(&echo_agent_home).unwrap();
        let auth_manager = Arc::new(
            AuthManager::new(&echo_agent_home, cfg).with_proxy_base_url("http://127.0.0.1:9"),
        );

        let err = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(build_auth(
                &team_access_token(token_principal),
                "http://localhost:22255",
                "client-id",
                &auth_manager,
            ))
            .unwrap_err();

        assert_eq!(err.to_string(), expected_err);
        assert!(
            auth_manager.current().is_none(),
            "rejected login must not persist credentials",
        );
        assert!(
            !echo_agent_home.join("auth.json").exists(),
            "rejected login must not write auth.json",
        );
    }

    /// The legacy `oauth2.principal_id` only pre-selects a team; it must not
    /// enforce a pin (only `force_login_team_uuid` does), so a different team's
    /// token is accepted.
    #[test]
    fn build_auth_does_not_enforce_legacy_oauth2_principal_id() {
        ensure_crypto_provider();
        let cfg = EchoAgentComConfig {
            oauth2: Some(crate::auth::OAuth2ProviderConfig {
                issuer: "http://localhost:22255".into(),
                client_id: "client-id".into(),
                scopes: vec!["offline_access".into()],
                principal_type: Some("Team".into()),
                principal_id: Some("team-required".into()),
                referrer: None,
            }),
            ..EchoAgentComConfig::default()
        };
        let temp_dir = tempfile::tempdir().unwrap();
        let echo_agent_home = temp_dir.path().join(".echo-agent");
        std::fs::create_dir_all(&echo_agent_home).unwrap();
        let auth_manager = Arc::new(
            AuthManager::new(&echo_agent_home, cfg).with_proxy_base_url("http://127.0.0.1:9"),
        );

        let auth = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(build_auth(
                &team_access_token("team-other"),
                "http://localhost:22255",
                "client-id",
                &auth_manager,
            ))
            .expect("legacy oauth2.principal_id must not enforce a pin");
        assert_eq!(
            auth.team_id.as_deref(),
            Some("team-other"),
            "the token's own team is used; the legacy pre-select id does not gate it",
        );
    }

    /// Persistence-seam enforcement via a `force_login_team_uuid` list.
    #[test]
    fn build_auth_rejects_token_outside_force_login_team_list() {
        let cfg = EchoAgentComConfig {
            force_login_team_uuid: Some(crate::auth::ForceLoginTeam::AnyOf(vec![
                "team-a".into(),
                "team-b".into(),
            ])),
            ..EchoAgentComConfig::default()
        };
        assert_build_auth_rejected(
            cfg,
            "team-other",
            "This deployment requires logging into one of teams: team-a, team-b; \
             your login returned team-other",
        );
    }

    // ── complete_device_code_login poll loop ────────────────────────────────

    /// Spawn a mock `/oauth2/token` server that serves `responses` in order,
    /// repeating the last entry. Returns the issuer base URL.
    async fn spawn_token_server(
        responses: Vec<(u16, serde_json::Value)>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let counter = Arc::new(AtomicUsize::new(0));
        let responses = Arc::new(responses);
        let app = axum::Router::new().route(
            "/oauth2/token",
            axum::routing::post(move || {
                let counter = counter.clone();
                let responses = responses.clone();
                async move {
                    let idx = counter
                        .fetch_add(1, Ordering::SeqCst)
                        .min(responses.len() - 1);
                    let (status, body) = &responses[idx];
                    (
                        axum::http::StatusCode::from_u16(*status).unwrap(),
                        axum::Json(body.clone()),
                    )
                }
            }),
        );
        let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (issuer, handle)
    }

    fn device_code_for_test(interval: i32, expires_in: i64) -> super::DeviceCode {
        super::DeviceCode {
            verification_uri: "https://example.test/device".into(),
            verification_uri_complete: Some(
                "https://example.test/device?user_code=ABCD-EFGH".into(),
            ),
            user_code: "ABCD-EFGH".into(),
            device_code: "dev-code-123".into(),
            interval,
            expires_in,
        }
    }

    // Real time (not `start_paused`: the shared client's 30s connect_timeout
    // fires under auto-advance). Deadline-expiry isn't tested — the deadline is
    // floored at 10 min (MIN_DEVICE_CODE_EXPIRY_FALLBACK_SECS).
    async fn run_poll(
        responses: Vec<(u16, serde_json::Value)>,
    ) -> anyhow::Result<(super::EchoAgentAuth, bool)> {
        let (issuer, server) = spawn_token_server(responses).await;
        let temp_dir = tempfile::tempdir().unwrap();
        let auth_manager = auth_manager_with_echo_agent_home(temp_dir.path(), "http://127.0.0.1:9");
        let device_code = device_code_for_test(1, 900);
        let result = super::complete_device_code_login(
            &issuer,
            "client-id",
            device_code,
            &auth_manager,
            super::ClientSurface::Cli,
        )
        .await;
        server.abort();
        result
    }

    fn success_body() -> serde_json::Value {
        serde_json::json!({
            "access_token": "mock-access-token",
            "refresh_token": "mock-refresh-token",
            "expires_in": 900,
            "scope": "openid",
        })
    }

    #[tokio::test]
    async fn poll_succeeds_on_first_poll() {
        let (auth, is_new) = run_poll(vec![(200, success_body())])
            .await
            .expect("should resolve to a token");
        assert_eq!(auth.key, "mock-access-token");
        assert!(is_new);
    }

    #[tokio::test]
    async fn poll_succeeds_after_pending() {
        let (auth, _) = run_poll(vec![
            (400, serde_json::json!({ "error": "authorization_pending" })),
            (200, success_body()),
        ])
        .await
        .expect("should resolve to a token after pending");
        assert_eq!(auth.key, "mock-access-token");
    }

    #[tokio::test]
    async fn poll_handles_slow_down_then_succeeds() {
        // slow_down must be tolerated (interval bumped) without erroring.
        let (auth, _) = run_poll(vec![
            (400, serde_json::json!({ "error": "slow_down" })),
            (200, success_body()),
        ])
        .await
        .expect("slow_down should be retried, not fatal");
        assert_eq!(auth.key, "mock-access-token");
    }

    #[tokio::test]
    async fn poll_maps_access_denied_to_error() {
        let err = run_poll(vec![(400, serde_json::json!({ "error": "access_denied" }))])
            .await
            .expect_err("access_denied must be an error");
        assert!(err.to_string().contains("denied"), "got: {err}");
    }

    #[tokio::test]
    async fn poll_maps_expired_token_to_error() {
        let err = run_poll(vec![(400, serde_json::json!({ "error": "expired_token" }))])
            .await
            .expect_err("expired_token must be an error");
        assert!(err.to_string().contains("expired"), "got: {err}");
    }
}
