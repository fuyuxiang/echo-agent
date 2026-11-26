//! Unit tests for [`super::oidc_refresher::OidcRefresher`]. Extracted
//! from `oidc_refresher.rs` so the implementation reads top-to-bottom;
//! wired in via `#[path = "oidc_refresher_tests.rs"] mod tests;`.

use super::*;
use crate::auth::{EchoAgentAuth, EchoAgentComConfig};
use chrono::{Duration, Utc};

// ── OIDC refresh E2E with mock IdP ─────────────────────────────────

/// Start a mock server that handles OIDC discovery, token refresh, and
/// the proxy /user endpoint (called by AuthManager::update).
async fn start_mock_oidc_and_proxy() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let base_for_discovery = base.clone();

    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = base_for_discovery.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(
                |body: axum::extract::Form<Vec<(String, String)>>| async move {
                    // Verify the request is a refresh_token grant.
                    let grant_type = body
                        .iter()
                        .find(|(k, _)| k == "grant_type")
                        .map(|(_, v)| v.as_str());
                    assert_eq!(
                        grant_type,
                        Some("refresh_token"),
                        "expected refresh_token grant"
                    );

                    axum::Json(serde_json::json!({
                        "access_token": "oidc-refreshed-token",
                        "refresh_token": "oidc-new-rt",
                        "expires_in": 3600,
                    }))
                },
            ),
        )
        .route(
            "/user",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "userId": "user-42",
                    "email": "test@corp.com",
                }))
            }),
        );

    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, handle)
}

fn write_auth_to_disk(dir: &std::path::Path, scope: &str, auth: &EchoAgentAuth) {
    let path = dir.join("auth.json");
    let mut map = crate::auth::read_auth_json(&path).unwrap_or_default();
    map.insert(scope.to_owned(), auth.clone());
    let json = serde_json::to_string_pretty(&map).unwrap();
    std::fs::write(&path, json).unwrap();
}

#[tokio::test]
async fn oidc_refresher_e2e_full_refresh_cycle() {
    let (base_url, server) = start_mock_oidc_and_proxy().await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base_url),
    );

    // Seed an expired OIDC token with all required fields.
    let expired = EchoAgentAuth {
        key: "old-expired-token".into(),
        create_time: Utc::now() - Duration::hours(2),
        user_id: "user-42".into(),
        email: Some("test@corp.com".into()),
        refresh_token: Some("old-refresh-token".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("test-client".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired);

    let refresher = OidcRefresher::new(mgr.clone());

    // ServerRejected so we bypass the cache check (token is expired anyway).
    let result = refresher.refresh(RefreshReason::ServerRejected).await;
    let new_auth = match result {
        RefreshOutcome::Success(auth) => auth,
        other => panic!("expected Success, got: {other:?}"),
    };
    assert_eq!(new_auth.key, "oidc-refreshed-token");
    assert_eq!(new_auth.refresh_token.as_deref(), Some("oidc-new-rt"));
    assert_eq!(new_auth.user_id, "user-42");
    assert_eq!(new_auth.oidc_issuer.as_deref(), Some(base_url.as_str()));
    assert!(new_auth.expires_at.is_some());

    server.abort();
}

#[tokio::test]
async fn oidc_refresher_e2e_proactive_returns_cached_when_valid() {
    let (base_url, server) = start_mock_oidc_and_proxy().await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base_url),
    );

    // Seed a valid (not expired) OIDC token.
    let valid = EchoAgentAuth {
        key: "still-valid-token".into(),
        user_id: "user-42".into(),
        email: Some("test@corp.com".into()),
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("test-client".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(valid);

    let refresher = OidcRefresher::new(mgr.clone());
    // PreRequest with a valid token: the refresher finds no expired_auth
    // (token is valid) and no disk token, so it returns TransientFailure.
    // The PreRequest fast-path is handled by refresh_chain (not the refresher).
    let result = refresher.refresh(RefreshReason::PreRequest).await;
    assert!(
        matches!(result, RefreshOutcome::TransientFailure { .. }),
        "PreRequest with valid token should return TransientFailure (refresh_chain handles fast-path)"
    );

    server.abort();
}

#[tokio::test]
async fn oidc_refresher_e2e_force_refreshes_locally_valid_token() {
    let (base_url, server) = start_mock_oidc_and_proxy().await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base_url),
    );

    // Seed a valid (not yet expired) OIDC token. force=true simulates
    // the reactive 401 path — server rejected the token even though it
    // looks locally valid (e.g. clock skew, server-side revocation).
    // The refresher should still attempt an OIDC refresh.
    let valid = EchoAgentAuth {
        key: "still-valid-token".into(),
        user_id: "user-42".into(),
        email: Some("test@corp.com".into()),
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("test-client".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(valid);

    let refresher = OidcRefresher::new(mgr.clone());
    // ServerRejected should refresh even though the token is locally valid.
    let result = refresher.refresh(RefreshReason::ServerRejected).await;
    let new_auth = match result {
        RefreshOutcome::Success(auth) => auth,
        other => panic!("expected Success, got: {other:?}"),
    };
    assert_eq!(
        new_auth.key, "oidc-refreshed-token",
        "ServerRejected should refresh even when token is locally valid"
    );

    server.abort();
}

// ── Near-expiry (5-minute buffer) refresh scenarios ──────────────

/// Regression test for token-expiry-window bug: when the token is within
/// the 5-minute early-invalidation buffer, current() returns None but
/// expired_auth() returns the token. The OidcRefresher must successfully
/// refresh it via the refresh_token grant — the exact path exercised by
/// initialize() in mvp_agent/mod.rs.
#[tokio::test]
async fn oidc_refresher_e2e_near_expiry_within_buffer_refreshes() {
    let (base_url, server) = start_mock_oidc_and_proxy().await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base_url),
    );

    // Token expires in 3 minutes — inside the 5-minute buffer.
    // current() will return None, but expired_auth() will return it.
    let near_expiry = EchoAgentAuth {
        key: "about-to-expire-token".into(),
        user_id: "user-42".into(),
        email: Some("test@corp.com".into()),
        refresh_token: Some("rt-still-valid".into()),
        expires_at: Some(Utc::now() + Duration::minutes(3)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("test-client".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(near_expiry);

    // Preconditions: confirm the bug scenario
    assert!(
        mgr.current().is_none(),
        "current() should be None within buffer"
    );
    assert!(
        mgr.is_expired(),
        "is_expired() should be true within buffer"
    );
    assert!(
        mgr.expired_auth().is_some(),
        "expired_auth() should return the token"
    );

    // Simulate the initialize() refresh path: get expired auth, call try_refresh
    mgr.set_refresher(std::sync::Arc::new(OidcRefresher::new(mgr.clone())));
    let refreshed = mgr.auth().await.ok();

    assert!(
        refreshed.is_some(),
        "OIDC refresh should succeed for near-expiry token"
    );
    let fresh = refreshed.unwrap();
    assert_eq!(fresh.key, "oidc-refreshed-token");
    assert_eq!(fresh.refresh_token.as_deref(), Some("oidc-new-rt"));

    // After refresh, current() should return the new valid token
    let current = mgr.current();
    assert!(
        current.is_some(),
        "current() should return new token after refresh"
    );
    assert_eq!(current.unwrap().key, "oidc-refreshed-token");

    server.abort();
}

/// Contract: on `invalid_grant`, `OidcRefresher` must report **which refresh
/// token it spent**, not just the access-token key.
///
/// `refresh_chain` uses `tried_refresh_token` to tell a lost rotation race
/// apart from a revoked session; an unattributed outcome silently disables
/// that check and turns any concurrent-refresh race into a machine-wide
/// logout. This is the shape assertion that the previous demotion tests
/// missed — they hand-built outcomes with `tried_key: None`, a shape this
/// refresher never emits, so they passed while production was unprotected.
#[tokio::test]
async fn oidc_refresher_attributes_the_refresh_token_it_spent_on_invalid_grant() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let base_for_discovery = base_url.clone();

    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = base_for_discovery.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(|| async {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({"error": "invalid_grant"})),
                )
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base_url),
    );
    mgr.hot_swap(EchoAgentAuth {
        key: "spent-access-token".into(),
        user_id: "user-42".into(),
        refresh_token: Some("rt-spent".into()),
        expires_at: Some(Utc::now() - Duration::minutes(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("test-client".into()),
        ..EchoAgentAuth::test_default()
    });

    let outcome = OidcRefresher::new(mgr.clone())
        .refresh(crate::auth::manager::RefreshReason::PreRequest)
        .await;

    match outcome {
        RefreshOutcome::PermanentFailure {
            tried_key,
            tried_refresh_token,
            ..
        } => {
            assert_eq!(
                tried_refresh_token.as_deref(),
                Some("rt-spent"),
                "the RT actually sent to the IdP must be reported so \
                 refresh_chain can detect a sibling rotation",
            );
            assert_eq!(tried_key.as_deref(), Some("spent-access-token"));
        }
        other => panic!("expected PermanentFailure, got: {other:?}"),
    }

    server.abort();
}

/// When the near-expiry token has a refresh_token but the IdP rejects
/// the refresh (e.g. refresh_token revoked), silent refresh must fail.
#[tokio::test]
async fn oidc_refresher_e2e_near_expiry_idp_rejects_refresh() {
    // Start a mock that rejects refresh requests with 401
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let base_for_discovery = base_url.clone();

    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = base_for_discovery.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(|| async {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({"error": "invalid_grant"})),
                )
            }),
        );

    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base_url),
    );

    let near_expiry = EchoAgentAuth {
        key: "about-to-expire-token".into(),
        user_id: "user-42".into(),
        refresh_token: Some("rt-revoked".into()),
        expires_at: Some(Utc::now() + Duration::minutes(3)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("test-client".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(near_expiry);

    // Permanent invalid_grant discards AT+RT (no grace re-serve of pre-refresh
    // snapshot). Grace remains for *transient* refresh failures only.
    mgr.set_refresher(std::sync::Arc::new(OidcRefresher::new(mgr.clone())));
    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(
            err,
            crate::auth::AuthError::Refresh(crate::auth::RefreshTokenError::Permanent(_))
        ),
        "permanent invalid_grant must not grace-serve the pre-refresh AT, got: {err:?}",
    );
    assert!(
        mgr.current_or_expired().is_none(),
        "permanent invalid_grant must clear credentials",
    );

    server.abort();
}

/// On `invalid_client` (client_id rotated, soft-deleted, or disabled) with a
/// hard-expired AT, permanent failure retains AT+RT (only invalid_grant discards).
#[tokio::test]
async fn oidc_refresher_e2e_invalid_client_retains_credentials() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let base_for_discovery = base_url.clone();

    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = base_for_discovery.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(|| async {
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({
                        "error": "invalid_client",
                        "error_description": "Unknown client"
                    })),
                )
            }),
        );

    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base_url),
    );

    let expired = EchoAgentAuth {
        key: "old-token".into(),
        create_time: Utc::now() - Duration::hours(2),
        user_id: "user-42".into(),
        refresh_token: Some("rt-valid".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("deleted-client-id".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired);

    mgr.set_refresher(std::sync::Arc::new(OidcRefresher::new(mgr.clone())));
    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(
            err,
            crate::auth::AuthError::Refresh(crate::auth::RefreshTokenError::Permanent(_))
        ),
        "refresh should fail permanently when client is unknown, got: {err:?}",
    );
    assert_eq!(
        mgr.current_or_expired()
            .and_then(|a| a.refresh_token)
            .as_deref(),
        Some("rt-valid"),
        "invalid_client must retain RT for TTL-gated retry after client rotation",
    );
    assert!(
        mgr.read_disk_auth().is_some() || mgr.current_or_expired().is_some(),
        "invalid_client must not clear credentials",
    );

    server.abort();
}

/// When the IdP would return `invalid_client` but disk auth.json already holds
/// a valid token with a different client_id (a sibling re-authenticated during
/// a client rotation), `auth()` adopts the sibling's disk token instead of
/// failing.
#[tokio::test]
async fn oidc_refresher_e2e_invalid_client_adopts_valid_sibling_disk_token() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let base_for_discovery = base_url.clone();

    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = base_for_discovery.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(|| async {
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    axum::Json(serde_json::json!({
                        "error": "invalid_client",
                        "error_description": "Unknown client"
                    })),
                )
            }),
        );

    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg).with_proxy_base_url(&base_url));

    // Pre-populate disk with auth that has a *different* client_id,
    // simulating another process having re-authenticated.
    let disk_auth = EchoAgentAuth {
        key: "disk-fresh-token".into(),
        user_id: "user-42".into(),
        refresh_token: Some("rt-disk".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("rotated-new-client-id".into()),
        ..EchoAgentAuth::test_default()
    };
    let mut store = std::collections::BTreeMap::new();
    store.insert(scope, disk_auth);
    let json = serde_json::to_string_pretty(&store).unwrap();
    std::fs::write(dir.path().join("auth.json"), json).unwrap();

    // In-memory auth has the OLD client_id that the server rejects.
    let expired = EchoAgentAuth {
        key: "old-token".into(),
        create_time: Utc::now() - Duration::hours(2),
        user_id: "user-42".into(),
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("deleted-client-id".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired);

    // auth() picks up the valid disk token via try_use_disk_token
    // (disk has a different, unexpired entry from a sibling process).
    // This is BETTER than the old try_refresh path which ignored
    // the valid disk token and hit the IdP.
    mgr.set_refresher(std::sync::Arc::new(OidcRefresher::new(mgr.clone())));
    let refreshed = mgr.auth().await;
    assert!(
        refreshed.is_ok(),
        "auth() should pick up the valid disk token, got: {refreshed:?}"
    );
    assert_eq!(
        refreshed.unwrap().oidc_client_id.as_deref(),
        Some("rotated-new-client-id"),
        "should use the sibling's rotated client_id from disk"
    );

    server.abort();
}

// The standalone `try_refresh_session_token` helper that previously
// lived in this module was removed when refresh was centralized in
// `AuthManager`. Its call sites now go through `AuthManager::auth()`
// / `AuthManager::unauthorized_recovery()`, both of which have their
// own coverage in `manager.rs`. The historical regression tests for
// the helper (`try_refresh_respects_auth_type`,
// `auth_type_must_be_session_token_after_session_key_set`) were
// dropped along with the function. The `resolve_credentials`
// invariant they also pinned remains covered by
// `agent::config::tests::{resolve_credentials_sets_auth_type,
// resolve_credentials_no_session_key_returns_api_key}`.

/// When another process has already refreshed and written a valid token
/// to auth.json, `refresh_chain` (via `auth()`) should pick it up from
/// disk instead of hitting the IdP.
#[tokio::test]
async fn oidc_refresh_picks_up_valid_disk_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg).with_proxy_base_url("http://127.0.0.1:1"));

    // Seed in-memory with an expired token (stale refresh_token).
    let expired = EchoAgentAuth {
        key: "old-expired-token".into(),
        create_time: Utc::now() - Duration::hours(2),
        user_id: "user-42".into(),
        refresh_token: Some("stale-rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some("https://idp.example.com".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired);

    // Simulate another process writing a valid token to disk.
    let fresh_on_disk = EchoAgentAuth {
        key: "fresh-from-other-process".into(),
        user_id: "user-42".into(),
        email: Some("user@test.com".into()),
        refresh_token: Some("new-rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        oidc_issuer: Some("https://idp.example.com".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    write_auth_to_disk(dir.path(), &scope, &fresh_on_disk);

    // Disk-token pickup is now refresh_chain's responsibility.
    // Go through auth() which calls refresh_chain.
    let result = mgr.auth().await;
    assert_eq!(
        result.unwrap().key,
        "fresh-from-other-process",
        "should use the valid token written by another process"
    );
    assert_eq!(
        mgr.current().unwrap().key,
        "fresh-from-other-process",
        "in-memory state should be updated"
    );
}

/// When the disk token is also expired but has a newer refresh_token,
/// the OIDC refresher should use the disk's RT for the IdP call.
#[tokio::test]
async fn oidc_refresh_uses_disk_refresh_token() {
    // Custom mock that captures the submitted refresh_token so we can
    // assert the disk RT was sent, not the stale in-memory one.
    let captured_rt = std::sync::Arc::new(parking_lot::Mutex::new(None::<String>));
    let captured_for_handler = captured_rt.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let base_for_disc = base_url.clone();
    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = base_for_disc.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(move |body: axum::extract::Form<Vec<(String, String)>>| {
                let captured = captured_for_handler.clone();
                async move {
                    let rt = body
                        .iter()
                        .find(|(k, _)| k == "refresh_token")
                        .map(|(_, v)| v.clone());
                    *captured.lock() = rt;
                    axum::Json(serde_json::json!({
                        "access_token": "oidc-refreshed-token",
                        "refresh_token": "oidc-new-rt",
                        "expires_in": 3600,
                    }))
                }
            }),
        )
        .route(
            "/user",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({ "userId": "user-42", "email": "test@corp.com" }))
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg).with_proxy_base_url(&base_url));

    mgr.hot_swap(EchoAgentAuth {
        key: "old-mem-token".into(),
        create_time: Utc::now() - Duration::hours(2),
        user_id: "user-42".into(),
        email: Some("test@corp.com".into()),
        refresh_token: Some("stale-rt-will-fail".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("test-client".into()),
        ..EchoAgentAuth::test_default()
    });

    write_auth_to_disk(
        dir.path(),
        &scope,
        &EchoAgentAuth {
            key: "old-disk-token".into(),
            create_time: Utc::now() - Duration::hours(2),
            user_id: "user-42".into(),
            email: Some("test@corp.com".into()),
            refresh_token: Some("disk-rt-valid".into()),
            expires_at: Some(Utc::now() - Duration::hours(1)),
            oidc_issuer: Some(base_url.clone()),
            oidc_client_id: Some("test-client".into()),
            ..EchoAgentAuth::test_default()
        },
    );

    let refresher = OidcRefresher::new(mgr.clone());
    let result = refresher.refresh(RefreshReason::ServerRejected).await;
    assert!(matches!(result, RefreshOutcome::Success(_)));

    assert_eq!(
        captured_rt.lock().as_deref(),
        Some("disk-rt-valid"),
        "must send the disk token's RT to the IdP, not the stale in-memory one"
    );

    server.abort();
}

/// When the lock file is held by another process (simulated), the
/// refresher should fall through and still attempt the refresh.
/// (Lock is now managed by refresh_chain, but the refresher itself
/// should still succeed without a lock.)
#[tokio::test]
async fn lock_timeout_falls_through_to_refresh() {
    let (base_url, server) = start_mock_oidc_and_proxy().await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg).with_proxy_base_url(&base_url));

    // Seed with expired token that has a valid refresh_token.
    let expired = EchoAgentAuth {
        key: "old-token".into(),
        create_time: Utc::now() - Duration::hours(2),
        user_id: "user-42".into(),
        email: Some("test@corp.com".into()),
        refresh_token: Some("rt-valid".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("test-client".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired);

    // Hold the lock file externally so the refresher times out.
    let lock_path = dir.path().join("auth.json.lock");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    use fs2::FileExt;
    lock_file.lock_exclusive().unwrap();

    // Use a very short timeout so the test doesn't wait 30s.
    let _lock = mgr
        .try_lock_auth_file_async(
            std::time::Duration::from_millis(100),
            crate::auth::manager::lock::Heartbeat::Skip,
        )
        .await
        .into_guard();
    assert!(_lock.is_none(), "lock should timeout");

    // The refresh should still succeed (refresher doesn't need the lock).
    let refresher = OidcRefresher::new(mgr.clone());
    let result = refresher.refresh(RefreshReason::ServerRejected).await;
    let new_auth = match result {
        RefreshOutcome::Success(auth) => auth,
        other => panic!("expected Success, got: {other:?}"),
    };
    assert_eq!(
        new_auth.key, "oidc-refreshed-token",
        "refresh should succeed even when lock times out"
    );

    lock_file.unlock().unwrap();
    server.abort();
}

// ── Disk-token retry on invalid_grant ──────────────────────

/// Mock IdP. `success_rts`: RT -> (access_token, new_rt).
/// `rotation_targets`: RT -> new disk RT written as a side effect
/// on `invalid_grant` (simulates sibling rotation). `attempts`
/// counts every POST so tests can assert one-shot.
async fn start_mock_oidc_with_disk_rotation(
    success_rts: std::collections::HashMap<&'static str, (&'static str, &'static str)>,
    rotation_targets: std::collections::HashMap<&'static str, &'static str>,
    sibling_writes_disk: Option<(std::path::PathBuf, String)>,
    attempts: Arc<std::sync::atomic::AtomicU32>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let base_for_discovery = base.clone();
    let attempts_for_handler = attempts.clone();

    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = base_for_discovery.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(move |body: axum::extract::Form<Vec<(String, String)>>| {
                let counter = attempts_for_handler.clone();
                let sibling_writes_disk = sibling_writes_disk.clone();
                let success_rts = success_rts.clone();
                let rotation_targets = rotation_targets.clone();
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    use axum::response::IntoResponse;
                    let rt = body
                        .iter()
                        .find(|(k, _)| k == "refresh_token")
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("");

                    if let Some((access, new_rt)) = success_rts.get(rt) {
                        return (
                            axum::http::StatusCode::OK,
                            axum::Json(serde_json::json!({
                                "access_token": access,
                                "refresh_token": new_rt,
                                "expires_in": 3600,
                            })),
                        )
                            .into_response();
                    }

                    if let Some(rotate_to) = rotation_targets.get(rt)
                        && let Some((ref path, ref scope)) = sibling_writes_disk
                    {
                        let mut map = crate::auth::read_auth_json(path).unwrap_or_default();
                        if let Some(entry) = map.get_mut(scope) {
                            entry.refresh_token = Some((*rotate_to).into());
                        }
                        let json = serde_json::to_string_pretty(&map).unwrap();
                        std::fs::write(path, json).unwrap();
                    }

                    (
                        axum::http::StatusCode::BAD_REQUEST,
                        axum::Json(serde_json::json!({
                            "error": "invalid_grant",
                            "error_description":
                                "Refresh token has been revoked",
                        })),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/user",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "userId": "user-42",
                    "email": "test@corp.com",
                }))
            }),
        );

    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, handle)
}

/// Sibling-rotation race: tried RT -> invalid_grant + sibling rotates disk;
/// retry with disk RT must succeed without surfacing failure.
#[tokio::test]
async fn refresher_retries_with_disk_token_after_invalid_grant() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let auth_path = dir.path().join("auth.json");

    let attempts = Arc::new(AtomicU32::new(0));
    let success_rts = std::collections::HashMap::from([(
        "rt-fresh-from-sibling",
        ("fresh-access-token", "rt-newest"),
    )]);
    let rotation_targets = std::collections::HashMap::from([("rt-stale", "rt-fresh-from-sibling")]);
    let (base_url, server) = start_mock_oidc_with_disk_rotation(
        success_rts,
        rotation_targets,
        Some((auth_path.clone(), scope.clone())),
        attempts.clone(),
    )
    .await;

    let mgr = Arc::new(AuthManager::new(dir.path(), cfg).with_proxy_base_url(&base_url));

    // Disk and memory both have rt-stale; mock rotates disk on
    // the first invalid_grant so the retry sees the fresh RT.
    let stale = EchoAgentAuth {
        key: "stale-access-token".into(),
        create_time: Utc::now() - Duration::hours(2),
        user_id: "user-42".into(),
        refresh_token: Some("rt-stale".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("test-client".into()),
        ..EchoAgentAuth::test_default()
    };
    write_auth_to_disk(dir.path(), &scope, &stale);
    mgr.hot_swap(stale);

    let refresher = OidcRefresher::new(mgr.clone());
    let outcome = refresher.refresh(RefreshReason::PreRequest).await;

    match outcome {
        RefreshOutcome::Success(new_auth) => {
            assert_eq!(
                new_auth.key, "fresh-access-token",
                "retry should return the access_token issued for the disk RT"
            );
            assert_eq!(
                new_auth.refresh_token.as_deref(),
                Some("rt-newest"),
                "retry should carry the newly-issued refresh_token forward"
            );
        }
        other => panic!("expected Success after disk-token retry, got: {other:?}"),
    }
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "exactly two IdP calls: stale RT then disk RT"
    );

    server.abort();
}

/// invalid_grant -> retry uses sibling-rotated disk RT -> invalid_client.
/// Both ATs expired: PermanentFailure is recorded (not demoted).
#[tokio::test]
async fn refresher_disk_retry_invalid_client_with_different_client_id_preserves_disk() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let auth_path = dir.path().join("auth.json");

    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_for_handler = attempts.clone();
    let auth_path_for_handler = auth_path.clone();
    let scope_for_handler = scope.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let base_for_discovery = base_url.clone();

    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = base_for_discovery.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(move |body: axum::extract::Form<Vec<(String, String)>>| {
                let attempts = attempts_for_handler.clone();
                let auth_path = auth_path_for_handler.clone();
                let scope = scope_for_handler.clone();
                async move {
                    use axum::response::IntoResponse;
                    let n = attempts.fetch_add(1, Ordering::SeqCst);
                    let rt = body
                        .iter()
                        .find(|(k, _)| k == "refresh_token")
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("");
                    if n == 0 {
                        // First call: invalid_grant + sibling rotates disk.
                        assert_eq!(rt, "rt-stale");
                        let mut map = crate::auth::read_auth_json(&auth_path).unwrap_or_default();
                        if let Some(entry) = map.get_mut(&scope) {
                            entry.refresh_token = Some("rt-sibling".into());
                            entry.oidc_client_id = Some("rotated-new-client-id".into());
                            entry.key = "sibling-fresh-access".into();
                        }
                        let json = serde_json::to_string_pretty(&map).unwrap();
                        std::fs::write(&auth_path, json).unwrap();
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            axum::Json(serde_json::json!({
                                "error": "invalid_grant",
                                "error_description": "RT revoked",
                            })),
                        )
                            .into_response();
                    }
                    // Retry uses the sibling's RT -> invalid_client.
                    assert_eq!(rt, "rt-sibling");
                    (
                        axum::http::StatusCode::UNAUTHORIZED,
                        axum::Json(serde_json::json!({
                            "error": "invalid_client",
                            "error_description": "Unknown client",
                        })),
                    )
                        .into_response()
                }
            }),
        );

    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mgr = Arc::new(AuthManager::new(dir.path(), cfg).with_proxy_base_url(&base_url));

    let stale = EchoAgentAuth {
        key: "stale-access".into(),
        create_time: Utc::now() - Duration::hours(2),
        user_id: "user-42".into(),
        refresh_token: Some("rt-stale".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("client-stale".into()),
        ..EchoAgentAuth::test_default()
    };
    write_auth_to_disk(dir.path(), &scope, &stale);
    mgr.hot_swap(stale);

    let refresher: Arc<dyn crate::auth::refresh::TokenRefresher> =
        Arc::new(OidcRefresher::new(mgr.clone()));
    mgr.set_refresher(refresher);

    let result = mgr
        .refresh_chain(
            crate::auth::token_type::TokenType::OidcSession,
            RefreshReason::ServerRejected,
        )
        .await;

    match result {
        Err(crate::auth::AuthError::Refresh(crate::auth::RefreshTokenError::Permanent(_))) => {}
        other => panic!("expected PermanentFailure, got: {other:?}"),
    }

    // Disk-retry already tried the sibling RT and got invalid_client —
    // permanent is recorded, but ClientRejected retains credentials.
    assert!(
        mgr.current_or_expired().is_some() || mgr.read_disk_auth().is_some(),
        "invalid_client permanent must retain credentials (only invalid_grant discards)"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2, "no recursion");

    server.abort();
}

/// Both RTs revoked: retry is strictly one-shot (no third call).
#[tokio::test]
async fn refresher_disk_retry_is_one_shot() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let auth_path = dir.path().join("auth.json");

    let attempts = Arc::new(AtomicU32::new(0));
    // Empty success_rts; disk rotates after the first attempt
    // so the retry fires but also fails -- exhausts cleanly.
    let success_rts: std::collections::HashMap<&str, (&str, &str)> =
        std::collections::HashMap::new();
    let rotation_targets = std::collections::HashMap::from([("rt-stale", "rt-also-revoked")]);
    let (base_url, server) = start_mock_oidc_with_disk_rotation(
        success_rts,
        rotation_targets,
        Some((auth_path.clone(), scope.clone())),
        attempts.clone(),
    )
    .await;

    let mgr = Arc::new(AuthManager::new(dir.path(), cfg).with_proxy_base_url(&base_url));

    let stale = EchoAgentAuth {
        key: "stale-access-token".into(),
        create_time: Utc::now() - Duration::hours(2),
        user_id: "user-42".into(),
        refresh_token: Some("rt-stale".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some(base_url.clone()),
        oidc_client_id: Some("test-client".into()),
        ..EchoAgentAuth::test_default()
    };
    write_auth_to_disk(dir.path(), &scope, &stale);
    mgr.hot_swap(stale);

    let refresher = OidcRefresher::new(mgr.clone());
    let outcome = refresher.refresh(RefreshReason::PreRequest).await;

    match outcome {
        RefreshOutcome::PermanentFailure { error, .. } => {
            assert_eq!(error.reason, RefreshTokenFailedReason::RefreshTokenRejected);
        }
        other => panic!("expected PermanentFailure after exhausted retry, got: {other:?}"),
    }

    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "exactly two IdP calls — disk-token retry must NOT recurse"
    );

    // This test calls the refresher directly (not refresh_chain); disk is
    // unchanged here — refresh_chain is responsible for permanent clear.
    assert!(
        mgr.read_disk_auth().is_some(),
        "refresher must not touch disk; clearing is refresh_chain's responsibility"
    );

    server.abort();
}

// ── Sleep-gate E2E (real OidcRefresher + mock IdP) ─────────────────

/// Mock IdP that counts `/token` POSTs so a test can prove a deferred refresh
/// suppressed the network call rather than just changing the return value.
async fn start_counting_mock_oidc(
    token_hits: Arc<std::sync::atomic::AtomicU32>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let base_for_discovery = base.clone();

    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = base_for_discovery.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(move || {
                let hits = token_hits.clone();
                async move {
                    hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(serde_json::json!({
                        "access_token": "oidc-refreshed-token",
                        "refresh_token": "oidc-new-rt",
                        "expires_in": 3600,
                    }))
                }
            }),
        )
        .route(
            "/user",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({ "userId": "user-42", "email": "test@corp.com" }))
            }),
        );

    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, handle)
}

fn expired_oidc_for(base_url: &str) -> EchoAgentAuth {
    EchoAgentAuth {
        key: "old-expired-token".into(),
        create_time: Utc::now() - Duration::hours(2),
        user_id: "user-42".into(),
        email: Some("test@corp.com".into()),
        refresh_token: Some("rt-valid".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some(base_url.to_owned()),
        oidc_client_id: Some("test-client".into()),
        ..EchoAgentAuth::test_default()
    }
}

/// While sleep is imminent, `auth()` defers and never reaches the IdP; after
/// wake it recovers via a real OIDC refresh. Exercises the production
/// `OidcRefresher` against a mock IdP, not a stub.
#[tokio::test]
async fn sleep_gate_e2e_defers_then_recovers_on_wake() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let token_hits = Arc::new(AtomicU32::new(0));
    let (base_url, server) = start_counting_mock_oidc(token_hits.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base_url),
    );
    mgr.hot_swap(expired_oidc_for(&base_url));
    mgr.set_refresher(Arc::new(OidcRefresher::new(mgr.clone())));

    mgr.set_system_sleep_imminent(true);
    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(
            err,
            crate::auth::AuthError::Refresh(crate::auth::RefreshTokenError::Transient(_))
        ),
        "gated refresh must return a transient refresh error, got {err:?}"
    );
    assert_eq!(
        token_hits.load(Ordering::SeqCst),
        0,
        "a deferred refresh must not reach the IdP token endpoint"
    );

    mgr.set_system_sleep_imminent(false);
    let fresh = mgr.auth().await.expect("refresh must succeed after wake");
    assert_eq!(fresh.key, "oidc-refreshed-token");
    assert_eq!(
        token_hits.load(Ordering::SeqCst),
        1,
        "exactly one IdP token call once the gate clears"
    );

    server.abort();
}

/// A refresh already in flight when sleep becomes imminent runs to completion
/// and persists its rotated token (no abort), proven through the real
/// `OidcRefresher` by holding the mock `/token` open until after the gate is
/// raised. The refresh token has already reached the IdP at that point, so
/// aborting would discard the rotated successor — the failure we guard against.
#[tokio::test]
async fn sleep_gate_e2e_in_flight_refresh_completes_across_imminent_sleep() {
    let idp_hit = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let idp_hit_h = idp_hit.clone();
    let release_h = release.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let base_for_discovery = base_url.clone();
    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = base_for_discovery.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "authorization_endpoint": format!("{b}/authorize"),
                        "token_endpoint": format!("{b}/token"),
                    }))
                }
            }),
        )
        .route(
            "/token",
            axum::routing::post(move || {
                let idp_hit = idp_hit_h.clone();
                let release = release_h.clone();
                async move {
                    // Signal that the RT has reached the IdP, then block until
                    // released — this span is the in-flight window.
                    idp_hit.notify_one();
                    release.notified().await;
                    axum::Json(serde_json::json!({
                        "access_token": "oidc-refreshed-token",
                        "refresh_token": "oidc-new-rt",
                        "expires_in": 3600,
                    }))
                }
            }),
        )
        .route(
            "/user",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({ "userId": "user-42", "email": "test@corp.com" }))
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base_url),
    );
    mgr.hot_swap(expired_oidc_for(&base_url));
    mgr.set_refresher(Arc::new(OidcRefresher::new(mgr.clone())));

    let m = mgr.clone();
    let handle = tokio::spawn(async move { m.auth().await });

    idp_hit.notified().await;

    // `set_system_sleep_imminent` now holds the OS sleep ack until the in-flight
    // refresh drains. Drive it off the runtime (as the real OS power-listener
    // thread does) so the runtime can complete the refresh while it waits.
    let sleeper = mgr.clone();
    let ack = std::thread::spawn(move || sleeper.set_system_sleep_imminent(true));
    release.notify_one();

    let fresh = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("auth() must return")
        .unwrap()
        .expect("in-flight refresh must complete across imminent sleep");
    ack.join().expect("ack thread panicked");
    assert_eq!(fresh.key, "oidc-refreshed-token");
    assert_eq!(
        mgr.current().map(|a| a.key),
        Some("oidc-refreshed-token".to_owned()),
        "the rotated token must be persisted, not discarded"
    );

    server.abort();
}

// ── Transient-blip budget is per-credential ─────────────────────────

/// Minimal `AuthSnapshot` for exercising `record_transient_failure` in
/// isolation (it never reads credential state).
struct EmptySnapshot;
impl AuthSnapshot for EmptySnapshot {
    fn current(&self) -> Option<EchoAgentAuth> {
        None
    }
    fn expired_auth(&self) -> Option<EchoAgentAuth> {
        None
    }
    fn read_disk_auth(&self) -> Option<EchoAgentAuth> {
        None
    }
    fn is_expired(&self) -> bool {
        false
    }
}

/// A fresh credential (e.g. after re-login on this long-lived refresher) must
/// get the full blip budget instead of inheriting a dead credential's count,
/// so a valid token is never escalated to a permanent failure early.
#[test]
fn transient_blip_budget_is_scoped_to_the_credential() {
    let refresher = OidcRefresher::new(Arc::new(EmptySnapshot));
    let key_a = Some("cred-a".to_owned());

    // Accrue blips up to just under the escalation threshold on credential A.
    for _ in 0..MAX_CONSECUTIVE_TRANSIENT_FAILURES - 1 {
        assert!(matches!(
            refresher.record_transient_failure("blip".into(), key_a.clone(), false),
            RefreshOutcome::TransientFailure { .. }
        ));
    }

    // Credential B's first blip must stay transient, not escalate to permanent.
    assert!(
        matches!(
            refresher.record_transient_failure("blip".into(), Some("cred-b".to_owned()), false),
            RefreshOutcome::TransientFailure { .. }
        ),
        "a fresh credential must not inherit a prior credential's blip count",
    );
}

/// Network-unreachable failures (DNS/connect/timeout — the post-wake offline
/// window) must never consume the escalation budget: no amount of them may
/// produce a `PermanentFailure` ("Run `echoagent login`") verdict, because they
/// prove nothing about the credential. Counted failures accrued before or
/// after are unaffected (the budget is neither consumed nor reset).
#[test]
fn network_unreachable_blips_never_escalate() {
    let refresher = OidcRefresher::new(Arc::new(EmptySnapshot));
    let key = Some("cred-a".to_owned());

    // Far more unreachable blips than the budget: all stay transient.
    for _ in 0..MAX_CONSECUTIVE_TRANSIENT_FAILURES * 3 {
        assert!(
            matches!(
                refresher.record_transient_failure("wifi down".into(), key.clone(), true),
                RefreshOutcome::TransientFailure { .. }
            ),
            "a network-unreachable failure must never escalate to permanent",
        );
    }

    // The budget was not consumed: counted (IdP-reaching) blips still get the
    // full threshold before escalating.
    for _ in 0..MAX_CONSECUTIVE_TRANSIENT_FAILURES - 1 {
        assert!(matches!(
            refresher.record_transient_failure("5xx".into(), key.clone(), false),
            RefreshOutcome::TransientFailure { .. }
        ));
    }
    assert!(
        matches!(
            refresher.record_transient_failure("5xx".into(), key.clone(), false),
            RefreshOutcome::PermanentFailure { .. }
        ),
        "counted blips must still escalate at the threshold",
    );
}
