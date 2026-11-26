//! Interactive login orchestration: callback HTTP server, browser
//! handoff, stdin paste fallback, race between the two.
//!
//! Cross-references [`super::protocol`] for OIDC mechanics and
//! [`super::super::AuthManager`] for credential persistence.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Query, State},
    http::{Method, StatusCode},
    response::Html,
    routing::get,
};
use tokio::net::TcpListener;

use super::super::config::{EchoAgentComConfig, OidcAuthConfig};
use super::super::{AuthManager, EchoAgentAuth};
use super::protocol::{
    OidcError, build_authorize_url, build_echo_agent_auth, discover, enforce_login_principal,
    exchange_code, extract_user_info, generate_pkce, login_principal_policy,
    peek_access_token_principal, peek_access_token_principal_id, validate_state,
};

/// Maximum time to wait for the browser OAuth callback (or manual paste of the code).
/// 10 minutes is long enough for users who step away briefly during login.
const AUTH_CALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Parse user-pasted input into `(code, state)`.
///
/// Accepts two formats:
///   1. Full callback URL: `http://127.0.0.1:PORT/callback?code=XXX&state=YYY`
///   2. Bare authorization code: `abc123`
fn parse_pasted_input(input: &str) -> Result<Callback, OidcError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(OidcError::InvalidPastedInput("empty input".into()));
    }

    if let Ok(url) = url::Url::parse(input) {
        let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if let Some(code) = params.get("code") {
            let state = params.get("state").cloned().unwrap_or_default();
            return Ok(Callback {
                code: code.clone(),
                state,
            });
        }
        if let Some(error) = params.get("error") {
            let desc = params.get("error_description").cloned().unwrap_or_default();
            return Err(OidcError::CallbackAuthFailed(if desc.is_empty() {
                error.clone()
            } else {
                format!("{error}: {desc}")
            }));
        }
        return Err(OidcError::InvalidPastedInput(
            "URL has no 'code' query parameter".into(),
        ));
    }

    Ok(Callback {
        code: input.to_owned(),
        state: String::new(),
    })
}

/// Render a styled callback page shown in the browser after the OAuth redirect.
pub(crate) fn callback_page(title: &str, message: &str, is_success: bool) -> String {
    let icon = if is_success {
        // EchoAgent logo
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" fill="none" viewBox="0 0 33 33"><path fill="currentColor" d="m13.237 21.04 11.082-8.19c.543-.4 1.32-.244 1.578.38 1.363 3.288.754 7.241-1.957 9.955-2.71 2.714-6.482 3.31-9.93 1.954l-3.765 1.745c5.401 3.697 11.96 2.782 16.059-1.324 3.251-3.255 4.258-7.692 3.317-11.693l.008.009c-1.365-5.878.336-8.227 3.82-13.031q.123-.17.247-.345l-4.585 4.59v-.014L13.234 21.044M10.95 23.031c-3.877-3.707-3.208-9.446.1-12.755 2.446-2.449 6.454-3.448 9.952-1.979L24.76 6.56c-.677-.49-1.545-1.017-2.54-1.387A12.465 12.465 0 0 0 8.675 7.901c-3.519 3.523-4.625 8.94-2.725 13.561 1.42 3.454-.907 5.898-3.251 8.364-.83.874-1.664 1.749-2.335 2.674l10.583-9.466"/></svg>"#
    } else {
        // X circle
        r#"<svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" style="color:#ef4444"><circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/></svg>"#
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<meta name="color-scheme" content="light dark"/>
<title>{title}</title>
<style>
  *{{margin:0;padding:0;box-sizing:border-box}}
  body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;
    display:flex;align-items:center;justify-content:center;min-height:100vh;
    background:#0a0a0a;color:#e5e5e5}}
  .card{{text-align:center;display:flex;flex-direction:column;align-items:center;gap:16px;padding:48px}}
  h1{{font-size:18px;font-weight:600}}
  p{{font-size:14px;color:#a3a3a3}}
  @media(prefers-color-scheme:light){{
    body{{background:#fafafa;color:#171717}}
    p{{color:#525252}}
  }}
</style>
</head>
<body>
  <div class="card">
    {icon}
    <h1>{title}</h1>
    <p>{message}</p>
  </div>
</body>
</html>"#,
        title = title,
        icon = icon,
        message = message,
    )
}

/// Build the axum router for the OIDC loopback callback server.
fn build_callback_router(tx: tokio::sync::mpsc::Sender<CallbackResult>) -> Router {
    let cors =
        crate::auth::config::accounts_app_cors_layer(Method::GET).allow_private_network(true);

    Router::new()
        .route("/callback", get(handle_callback))
        .layer(cors)
        .with_state(tx)
}

async fn handle_callback(
    State(tx): State<tokio::sync::mpsc::Sender<CallbackResult>>,
    Query(params): Query<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
    let result = parse_callback_params(&params);
    let response = callback_response(&result);
    if let Err(e) = tx.try_send(result) {
        tracing::error!(?e, "OIDC: callback channel send failed; auth will time out");
    }
    response
}

fn parse_callback_params(params: &HashMap<String, String>) -> CallbackResult {
    if let Some(code) = params.get("code") {
        let state = params.get("state").cloned().unwrap_or_default();
        tracing::debug!(state = %state, "OIDC: received code via loopback callback");
        return Ok(Callback {
            code: code.clone(),
            state,
        });
    }
    let error = params.get("error").cloned().unwrap_or_default();
    let desc = params.get("error_description").cloned().unwrap_or_default();
    tracing::error!(error = %error, desc = %desc, "OIDC: IdP returned error");
    Err(if desc.is_empty() {
        error
    } else {
        format!("{error}: {desc}")
    })
}

fn callback_response(result: &CallbackResult) -> (StatusCode, Html<String>) {
    let (title, message) = match result {
        Ok(_) => (
            "Signed in",
            "You can close this window and return to EchoAgent Build.",
        ),
        Err(_) => ("Access denied", "Close this window and try again."),
    };
    (
        StatusCode::OK,
        Html(callback_page(title, message, result.is_ok())),
    )
}

/// Wait until stdin has data or `tx` is closed. Returns `false` if closed.
#[cfg(unix)]
fn wait_for_stdin_or_closed(
    stdin: &std::io::Stdin,
    tx: &tokio::sync::mpsc::Sender<CallbackResult>,
) -> bool {
    use std::os::unix::io::AsRawFd;
    let fd = stdin.as_raw_fd();
    loop {
        if tx.is_closed() {
            return false;
        }
        let ready = unsafe {
            let mut fds = std::mem::zeroed::<libc::pollfd>();
            fds.fd = fd;
            fds.events = libc::POLLIN;
            libc::poll(&mut fds, 1, 200)
        };
        if ready > 0 {
            return true;
        }
    }
}

fn spawn_stdin_reader(tx: tokio::sync::mpsc::Sender<CallbackResult>) {
    tokio::task::spawn_blocking(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        let mut buf = String::new();
        loop {
            #[cfg(unix)]
            if !wait_for_stdin_or_closed(&stdin, &tx) {
                tracing::debug!("OIDC: stdin reader exiting, channel closed");
                return;
            }
            #[cfg(not(unix))]
            if tx.is_closed() {
                tracing::debug!("OIDC: stdin reader exiting, channel closed");
                return;
            }

            buf.clear();
            let mut handle = stdin.lock();
            match handle.read_line(&mut buf) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }
            drop(handle);

            let trimmed = buf.trim().to_owned();
            if trimmed.is_empty() {
                continue;
            }
            match parse_pasted_input(&trimmed) {
                Ok(result) => {
                    tracing::debug!("OIDC: received code via stdin paste");
                    let _ = tx.blocking_send(Ok(result));
                    return;
                }
                Err(OidcError::InvalidPastedInput(msg)) => {
                    tracing::debug!(input = %msg, "OIDC: invalid stdin paste, retrying");
                    eprintln!("  Invalid input: {msg}. Try again:");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "OIDC: stdin paste returned auth error");
                    let _ = tx.blocking_send(Err(e.to_string()));
                    return;
                }
            }
        }
    });
}

/// Race loopback callback against manual paste from `code_rx`.
async fn race_callback_and_client_ui(
    listener: TcpListener,
    code_rx: &mut tokio::sync::mpsc::Receiver<String>,
) -> anyhow::Result<Callback> {
    tracing::debug!("OIDC: waiting for auth code (loopback + client paste)");
    let (tx, mut rx) = tokio::sync::mpsc::channel::<CallbackResult>(1);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let app = build_callback_router(tx.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    // Bridge client paste input into the callback channel.
    let client_tx = tx.clone();
    let client_bridge = async {
        while let Some(code) = code_rx.recv().await {
            match parse_pasted_input(&code) {
                Ok(result) => {
                    tracing::debug!("OIDC: received code via client paste");
                    let _ = client_tx.send(Ok(result)).await;
                    return;
                }
                Err(e) => {
                    tracing::debug!(error = %e, "OIDC: invalid client paste input");
                }
            }
        }
    };

    drop(tx);

    let result = tokio::select! {
        r = tokio::time::timeout(AUTH_CALLBACK_TIMEOUT, rx.recv()) => {
            r.map_err(|_| anyhow::Error::new(OidcError::CallbackTimeout))?
                .ok_or_else(|| anyhow::Error::new(OidcError::CallbackChannelClosed))?
        }
        _ = client_bridge => {
            rx.recv().await
                .ok_or_else(|| anyhow::Error::new(OidcError::CallbackChannelClosed))?
        }
    };

    let _ = shutdown_tx.send(());
    let _ = server.await;

    result.map_err(|e| anyhow::Error::new(OidcError::CallbackAuthFailed(e)))
}

/// Race loopback callback against stdin paste.
async fn race_callback_and_stdin(
    listener: TcpListener,
    enable_stdin: bool,
) -> anyhow::Result<Callback> {
    tracing::debug!(
        enable_stdin = enable_stdin,
        "OIDC: waiting for auth code (loopback + stdin)"
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel::<CallbackResult>(1);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let app = build_callback_router(tx.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    if enable_stdin {
        spawn_stdin_reader(tx.clone());
    }

    drop(tx);

    let result = tokio::time::timeout(AUTH_CALLBACK_TIMEOUT, rx.recv())
        .await
        .map_err(|_| {
            // "10 minutes" must match AUTH_CALLBACK_TIMEOUT above
            tracing::error!("auth: timed out after 10 minutes waiting for auth code");
            anyhow::Error::new(OidcError::CallbackTimeout)
        })?
        .ok_or_else(|| {
            tracing::error!(
                "OIDC: callback channel closed, no code received from loopback or stdin"
            );
            anyhow::Error::new(OidcError::CallbackChannelClosed)
        })?;

    let _ = shutdown_tx.send(());
    let _ = server.await;

    result.map_err(|e| anyhow::Error::new(OidcError::CallbackAuthFailed(e)))
}

/// Run the full OIDC login flow: discovery → PKCE → browser → callback → token exchange → persist.
pub async fn run_login_flow(
    config: &EchoAgentComConfig,
    auth_manager: &Arc<AuthManager>,
    channels: Option<super::super::flow::AuthChannels>,
) -> anyhow::Result<(EchoAgentAuth, bool)> {
    let oidc = config
        .oidc
        .as_ref()
        .ok_or_else(|| anyhow::Error::new(OidcError::NotConfigured))?;
    run_login_flow_with_config(oidc, auth_manager, channels).await
}

/// Run the OIDC login flow with an explicit [`OidcAuthConfig`].
///
/// Also used by the OAuth2 provider path via [`OAuth2ProviderConfig::as_oidc`].
///
/// The flow races two input paths:
///   - **Path A**: A loopback HTTP server on `127.0.0.1` that receives the IdP redirect.
///   - **Path B**: Stdin paste — the user manually pastes the callback URL or bare auth code.
///
/// Path B is essential for remote VMs where the browser runs on a different machine
/// and the `127.0.0.1` redirect cannot reach the CLI process.
/// * `channels` — `Some`: pushes the auth URL to the TUI and receives pasted codes.
///   `None`: prints to stderr / reads stdin (CLI mode).
pub async fn run_login_flow_with_config(
    oidc: &OidcAuthConfig,
    auth_manager: &Arc<AuthManager>,
    channels: Option<super::super::flow::AuthChannels>,
) -> anyhow::Result<(EchoAgentAuth, bool)> {
    tracing::info!(issuer = %oidc.issuer, client_id = %oidc.client_id, "OIDC: starting login flow");

    // Ensure jsonwebtoken CryptoProvider is installed (required for JWT validation).
    jsonwebtoken::crypto::CryptoProvider::install_default(
        &jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER,
    )
    .ok();

    let discovery = discover(&oidc.issuer).await?;
    let pkce = generate_pkce();
    let state = uuid::Uuid::now_v7().to_string();
    let nonce = uuid::Uuid::now_v7().to_string();

    // In local-dev mode, use a fixed callback port so the redirect_uri is stable
    // and can be pre-registered with the local OAuth2 provider. In production the
    // OS picks a random available port.
    let callback_port: u16 = if super::super::config::use_local_auth() {
        56121
    } else {
        0
    };
    let listener = TcpListener::bind(("127.0.0.1", callback_port))
        .await
        .map_err(|e| anyhow::Error::new(OidcError::BindLoopback(e.to_string())))?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{}/callback", port);
    let oauth2 = auth_manager.echo_agent_com_config().oauth2.as_ref();
    let auth_url = build_authorize_url(
        oidc,
        oauth2,
        &discovery,
        &redirect_uri,
        &pkce,
        &state,
        &nonce,
    );
    tracing::debug!(port = port, redirect_uri = %redirect_uri, "OIDC: callback server bound");

    let (url_tx, code_rx) = match channels {
        Some(ch) => (ch.url_tx, Some(ch.code_rx)),
        None => (None, None),
    };
    let has_client_ui = code_rx.is_some();

    if has_client_ui {
        // Client provides its own auth UI; just open the browser.
        if let Err(e) = webbrowser::open(&auth_url) {
            tracing::debug!(error = %e, "OIDC: failed to open browser");
        }
    } else {
        // No client UI — print to stderr.
        eprintln!();
        let provider_label = if oidc.issuer == super::super::config::ECHO_AGENT_OAUTH2_ISSUER {
            "EchoAgent".to_owned()
        } else {
            oidc.issuer.clone()
        };
        eprintln!("Signing in with {}...", provider_label);
        eprintln!();
        if let Err(e) = webbrowser::open(&auth_url) {
            tracing::debug!(error = %e, "OIDC: failed to open browser");
        }
        eprintln!("Open this URL to sign in:");
        eprintln!("  {}", auth_url);
    }

    let use_stdin = !has_client_ui && std::io::stdin().is_terminal();
    if use_stdin {
        eprintln!();
        eprintln!("Paste the URL here if it doesn't connect:");
    }

    // Push auth URL to the TUI via oneshot.
    if let Some(tx) = url_tx {
        let _ = tx.send(super::super::flow::AuthUrlInfo {
            url: auth_url.clone(),
            mode: super::super::flow::AuthUrlMode::Loopback,
        });
    }

    let Callback {
        code,
        state: received_state,
    } = if let Some(mut rx) = code_rx {
        // Client UI: race loopback against manual paste via code_rx.
        race_callback_and_client_ui(listener, &mut rx).await?
    } else {
        // No client UI: race loopback against stdin paste.
        race_callback_and_stdin(listener, use_stdin).await?
    };

    // Validate state (skip for bare code paste where state is empty)
    if !received_state.is_empty() {
        validate_state(&state, &received_state)?;
    }

    let tokens = exchange_code(
        &discovery.token_endpoint,
        &code,
        &redirect_uri,
        &oidc.client_id,
        &pkce.code_verifier,
    )
    .await?;
    tracing::info!(
        has_refresh = tokens.refresh_token.is_some(),
        expires_in = ?tokens.expires_in,
        "OIDC: token exchange complete"
    );

    // Resolve the actual principal chosen on the consent screen.
    //
    // The shell's config may not have principal_type set (personal login),
    // but the user might pick "Team" on the consent screen. The server
    // encodes the chosen principal in the access token JWT. If the config
    // doesn't specify a principal, peek at the token to discover it.
    let token_principal = peek_access_token_principal(&tokens.access_token);

    // The authorize URL only pre-selects; verify the token's principal here.
    // Match the principal id even if `principal_type` is absent.
    let principal_policy = login_principal_policy(auth_manager.echo_agent_com_config());
    enforce_login_principal(
        principal_policy.as_ref(),
        peek_access_token_principal_id(&tokens.access_token).as_deref(),
    )?;

    let (resolved_principal_type, resolved_principal_id, resolved_team_id) = {
        let cfg_pt = oauth2.and_then(|cfg| cfg.principal_type.clone());
        let cfg_pid = oauth2.and_then(|cfg| cfg.principal_id.clone());
        if cfg_pt.is_some() {
            (cfg_pt, cfg_pid, None)
        } else if let Some((pt, pid, tid)) = token_principal {
            tracing::info!(
                principal_type = %pt,
                principal_id = %pid,
                team_id = ?tid,
                "OIDC: resolved principal from access token"
            );
            (Some(pt), Some(pid), tid)
        } else {
            (cfg_pt, cfg_pid, None)
        }
    };

    let user_info = extract_user_info(
        tokens.id_token.as_deref(),
        &discovery,
        &oidc.issuer,
        &oidc.client_id,
        &nonce,
        resolved_principal_type.as_deref(),
        resolved_principal_id.as_deref(),
        resolved_team_id,
    )
    .await?;
    tracing::debug!(user_id = %user_info.user_id, "OIDC: extracted user info");

    let mut auth = build_echo_agent_auth(tokens, user_info, &oidc.issuer, &oidc.client_id);
    auth_manager.enrich_auth_inline(&mut auth).await;
    let auth = auth_manager
        .update(auth)
        .await
        .map_err(|e| anyhow::Error::new(OidcError::SaveAuth(e.to_string())))?;
    tracing::info!(user_id = %auth.user_id, "OIDC: login complete, credentials saved");

    Ok((auth, true))
}

/// Successful OIDC callback payload.
#[derive(Debug, PartialEq, Eq)]
struct Callback {
    code: String,
    state: String,
}

/// Result from the OIDC callback: either a [`Callback`] or an IdP error message.
type CallbackResult = Result<Callback, String>;

#[cfg(test)]
mod tests {
    use super::super::test_helpers::*;
    use super::*;

    /// End-to-end test: mock IdP + full login flow with code arriving via loopback.
    /// Exercises discovery → PKCE → race_callback_and_stdin → token exchange → user info → persist.
    #[tokio::test]
    async fn full_login_flow_via_race() {
        ensure_crypto_provider();
        let (issuer, idp_server) = start_mock_idp().await;
        let temp_dir = tempfile::tempdir().unwrap();
        // Dead proxy port: inline `/user` enrichment fails fast in tests.
        let dead_proxy = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://127.0.0.1:{}", l.local_addr().unwrap().port())
        };
        let auth_manager = Arc::new(
            AuthManager::new(temp_dir.path(), EchoAgentComConfig::default())
                .with_proxy_base_url(&dead_proxy),
        );

        let oidc_cfg = OidcAuthConfig {
            issuer: issuer.clone(),
            client_id: TEST_CLIENT_ID.into(),
            scopes: vec!["openid".into(), "email".into()],
            audience: None,
        };
        let discovery = discover(&oidc_cfg.issuer).await.unwrap();
        let pkce = generate_pkce();
        let state = "test-state".to_string();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let _auth_url = build_authorize_url(
            &oidc_cfg,
            None,
            &discovery,
            &redirect_uri,
            &pkce,
            &state,
            &test_nonce(),
        );

        // Simulate browser callback via race_callback_and_stdin
        let Callback {
            code,
            state: received_state,
        } = tokio::join!(race_callback_and_stdin(listener, false), async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            reqwest::get(format!(
                "http://127.0.0.1:{port}/callback?code=mock-auth-code&state={state}"
            ))
            .await
            .unwrap();
        })
        .0
        .unwrap();

        assert_eq!(code, "mock-auth-code");
        assert_eq!(received_state, state);

        let tokens = exchange_code(
            &discovery.token_endpoint,
            &code,
            &redirect_uri,
            &oidc_cfg.client_id,
            &pkce.code_verifier,
        )
        .await
        .unwrap();
        assert_eq!(tokens.access_token, "mock-access-token");

        let user_info = extract_user_info(
            tokens.id_token.as_deref(),
            &discovery,
            &oidc_cfg.issuer,
            &oidc_cfg.client_id,
            &test_nonce(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let auth = build_echo_agent_auth(tokens, user_info, &oidc_cfg.issuer, &oidc_cfg.client_id);
        let auth = auth_manager.update(auth).await.unwrap();

        assert_eq!(auth.key, "mock-access-token");
        assert_eq!(auth.refresh_token.as_deref(), Some("mock-refresh-token"));
        assert_eq!(auth.user_id, "user-42");
        assert_eq!(auth.email.as_deref(), Some("test@corp.com"));
        assert!(auth.principal_type.is_none());
        assert!(auth.principal_id.is_none());
        assert!(auth.expires_at.is_some());
        assert_eq!(auth.oidc_issuer.as_deref(), Some(issuer.as_str()));

        let auth_json = std::fs::read_to_string(temp_dir.path().join("auth.json")).unwrap();
        assert!(auth_json.contains("mock-access-token"));
        assert!(auth_json.contains("user-42"));

        idp_server.abort();
    }
    /// Parser matrix: full callback URL, bare code, error URL, empty.
    /// Each case is one bug class:
    ///   - full URL: regression in URL extraction
    ///   - bare code: paste-friendly fallback
    ///   - error URL: surfaces IdP error to user
    ///   - empty: input validation
    #[test]
    fn parse_pasted_input_matrix() {
        // (input, expected: Ok((code, state)) | Err substring)
        let ok_cases: &[(&str, &str, &str)] = &[
            (
                "http://127.0.0.1:54321/callback?code=abc123&state=xyz789",
                "abc123",
                "xyz789",
            ),
            ("abc123def456", "abc123def456", ""),
        ];
        for (input, code, state) in ok_cases {
            let cb =
                parse_pasted_input(input).unwrap_or_else(|e| panic!("parse {input:?} failed: {e}"));
            assert_eq!(cb.code, *code, "code for {input:?}");
            assert_eq!(cb.state, *state, "state for {input:?}");
        }

        let err_cases: &[(&str, &str)] = &[
            (
                "http://127.0.0.1:54321/callback?error=access_denied&error_description=User+denied",
                "access_denied",
            ),
            ("", ""),
            ("   ", ""),
        ];
        for (input, expected_substr) in err_cases {
            let err = parse_pasted_input(input).unwrap_err();
            if !expected_substr.is_empty() {
                assert!(
                    err.to_string().contains(expected_substr),
                    "input {input:?} -> unexpected err: {err}"
                );
            }
        }
    }
}
