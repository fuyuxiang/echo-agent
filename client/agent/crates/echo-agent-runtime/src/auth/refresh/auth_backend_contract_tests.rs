//! End-to-end auth-backend contract tests: a mock IdP whose `/token` response
//! is forced per case, asserting the refresh outcome, the storm cap, and the
//! emitted `manual_auth` instrumentation on the live recovery path.

use super::*;
use crate::auth::error::RefreshTokenFailedReason;
use crate::auth::recovery::RecoverySource;
use crate::auth::{EchoAgentAuth, EchoAgentComConfig};
use chrono::{Duration, Utc};
use echo_agent_telemetry::events::{AuthTokenKind, ManualAuthReason};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Mock IdP: OIDC discovery + a `/token` endpoint returning a fixed
/// `(status, body)` and counting every hit, plus the `/user` endpoint
/// `AuthManager::update` calls after a successful refresh. `delay_ms` widens
/// the in-lock window so concurrent callers queue on `refresh_lock`.
async fn start_idp(
    token_status: u16,
    token_body: String,
    hits: Arc<AtomicU32>,
    delay_ms: u64,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let disco = base.clone();

    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let b = disco.clone();
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
                let hits = hits.clone();
                let body = token_body.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    if delay_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }
                    (
                        axum::http::StatusCode::from_u16(token_status).unwrap(),
                        body,
                    )
                }
            }),
        )
        .route(
            "/user",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({ "userId": "user-42", "email": "u@corp.com" }))
            }),
        );

    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, handle)
}

fn expired_oidc(base_url: &str) -> EchoAgentAuth {
    EchoAgentAuth {
        key: "expired-at".into(),
        create_time: Utc::now() - Duration::hours(2),
        user_id: "user-42".into(),
        auth_mode: crate::auth::model::AuthMode::Oidc,
        refresh_token: Some("rt-under-test".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some(base_url.to_owned()),
        oidc_client_id: Some("client-under-test".into()),
        ..EchoAgentAuth::test_default()
    }
}

#[derive(Debug)]
enum Expect {
    Success,
    Permanent(RefreshTokenFailedReason),
    Transient,
}

/// The IdP token-endpoint contract: each response shape maps to one outcome.
/// `invalid_grant`/`invalid_client` are the only permanent verdicts; status
/// blips and unrecognized codes stay transient (never permanent-lock).
#[tokio::test]
async fn auth_backend_contract_token_responses_map_to_outcomes() {
    use RefreshTokenFailedReason::{ClientRejected, RefreshTokenRejected};
    let cases: &[(&str, u16, &str, Expect)] = &[
        (
            "success",
            200,
            r#"{"access_token":"fresh","refresh_token":"fresh-rt","expires_in":3600}"#,
            Expect::Success,
        ),
        (
            "invalid_grant",
            400,
            r#"{"error":"invalid_grant"}"#,
            Expect::Permanent(RefreshTokenRejected),
        ),
        (
            "invalid_client",
            401,
            r#"{"error":"invalid_client"}"#,
            Expect::Permanent(ClientRejected),
        ),
        ("server_error_5xx", 503, "{}", Expect::Transient),
        ("rate_limited_429", 429, "{}", Expect::Transient),
        (
            "temporarily_unavailable",
            400,
            r#"{"error":"temporarily_unavailable"}"#,
            Expect::Transient,
        ),
        ("bare_4xx_no_body", 400, "", Expect::Transient),
        ("malformed_body", 400, "not json", Expect::Transient),
        // Proxy/WAF-mangled bodies must degrade to retry, never a false permanent
        // lock: a nested error object or a non-string `error` is not a recognized
        // top-level code, so it stays transient.
        (
            "nested_error_object",
            400,
            r#"{"error":{"code":"invalid_grant"}}"#,
            Expect::Transient,
        ),
        (
            "non_string_error",
            400,
            r#"{"error":123}"#,
            Expect::Transient,
        ),
    ];

    for (name, status, body, expect) in cases {
        let hits = Arc::new(AtomicU32::new(0));
        let (base_url, server) = start_idp(*status, body.to_string(), hits.clone(), 0).await;
        let dir = tempfile::tempdir().unwrap();
        let auth_manager = Arc::new(
            AuthManager::new(dir.path(), EchoAgentComConfig::default())
                .with_proxy_base_url(&base_url),
        );
        auth_manager.hot_swap(expired_oidc(&base_url));

        let refresher = OidcRefresher::new(auth_manager.clone());
        let result = refresher.refresh(RefreshReason::ServerRejected).await;

        match (expect, &result) {
            (Expect::Success, RefreshOutcome::Success(_)) => {}
            (Expect::Permanent(want), RefreshOutcome::PermanentFailure { error, .. }) => {
                assert_eq!(error.reason, *want, "{name}: wrong permanent reason");
            }
            (Expect::Transient, RefreshOutcome::TransientFailure { .. }) => {}
            (exp, got) => panic!("{name}: expected {exp:?}, got {got:?}"),
        }
        server.abort();
    }
}

/// A burst of concurrent 401s on the same revoked refresh token must hit the
/// IdP exactly once. The callers serialize on `refresh_lock`; the leader records
/// the verdict before releasing, so the in-lock re-check (`refresh_chain` step
/// 1b) short-circuits every follower. Delete step 1b and the count climbs to N.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_backend_contract_concurrent_401s_hit_idp_once() {
    let hits = Arc::new(AtomicU32::new(0));
    // 100ms /token delay so every caller passes the pre-lock check and queues
    // on refresh_lock before the leader records the verdict, exercising step 1b.
    let (base_url, server) = start_idp(
        400,
        r#"{"error":"invalid_grant"}"#.to_string(),
        hits.clone(),
        100,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let auth_manager = Arc::new(
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base_url),
    );
    auth_manager.hot_swap(expired_oidc(&base_url));
    auth_manager.set_refresher(Arc::new(OidcRefresher::new(auth_manager.clone())));

    let mut tasks = Vec::new();
    for _ in 0..6 {
        let auth_manager = auth_manager.clone();
        tasks.push(tokio::spawn(async move { auth_manager.auth().await }));
    }
    for t in tasks {
        let outcome = t.await.unwrap();
        assert!(
            matches!(
                outcome,
                Err(crate::auth::AuthError::Refresh(
                    crate::auth::RefreshTokenError::Permanent(_)
                ))
            ),
            "every concurrent caller must fail permanently on a revoked refresh token, got {outcome:?}",
        );
    }

    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "concurrent 401s on one dead credential must hit the IdP exactly once",
    );

    server.abort();
}

/// The instrumentation loop through the live recovery state machine: a dead
/// refresh token on each user-facing source (`Turn`, `Relay`) emits one
/// `manual_auth` event carrying the typed reason, the matching surface, and the
/// rejected principal; a refreshable token auto-refreshes and emits nothing.
#[tokio::test]
async fn auth_backend_contract_dead_token_emits_typed_manual_auth_event() {
    use echo_agent_telemetry::events::ManualAuthSurface;

    // A dead refresh token on each user-facing source emits the typed event
    // with the surface that produced it.
    for (source, want_surface) in [
        (RecoverySource::Turn, ManualAuthSurface::Turn),
        (RecoverySource::Relay, ManualAuthSurface::Relay),
    ] {
        let hits = Arc::new(AtomicU32::new(0));
        let (url, server) =
            start_idp(400, r#"{"error":"invalid_grant"}"#.to_string(), hits, 0).await;
        let dir = tempfile::tempdir().unwrap();
        let auth_manager = Arc::new(
            AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&url),
        );
        auth_manager.hot_swap(expired_oidc(&url));
        auth_manager.set_refresher(Arc::new(OidcRefresher::new(auth_manager.clone())));

        let err = auth_manager
            .unauthorized_recovery(auth_manager.current_or_expired(), source)
            .next()
            .await
            .expect_err("a dead refresh token must fail recovery");
        assert!(
            crate::auth::recovery::manual_auth_reason(&err).is_some(),
            "a dead refresh token must be a forced-relogin error, got {err:?}",
        );

        let event = auth_manager
            .manual_auth_last_emit()
            .expect("a user-facing dead-token recovery must emit a manual_auth event");
        assert_eq!(
            event.reason,
            ManualAuthReason::RefreshTokenRejected,
            "the emitted event must say *why*: the refresh token was rejected",
        );
        assert_eq!(
            event.trigger, want_surface,
            "emitted surface must match the source"
        );
        assert_eq!(
            event.principal.as_deref(),
            Some("user-42"),
            "the event must attribute the rejected principal",
        );
        assert_eq!(
            event.token_kind,
            AuthTokenKind::OidcSession,
            "an OIDC session must be reported as OidcSession",
        );
        server.abort();
    }

    // Refreshable token: recovery auto-refreshes and emits nothing.
    let ok_hits = Arc::new(AtomicU32::new(0));
    let (ok_url, ok_server) = start_idp(
        200,
        r#"{"access_token":"fresh","refresh_token":"fresh-rt","expires_in":3600}"#.to_string(),
        ok_hits,
        0,
    )
    .await;
    let ok_dir = tempfile::tempdir().unwrap();
    let ok_manager = Arc::new(
        AuthManager::new(ok_dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&ok_url),
    );
    ok_manager.hot_swap(expired_oidc(&ok_url));
    ok_manager.set_refresher(Arc::new(OidcRefresher::new(ok_manager.clone())));

    let refreshed = ok_manager
        .unauthorized_recovery(ok_manager.current_or_expired(), RecoverySource::Turn)
        .next()
        .await
        .expect("a refreshable token must auto-refresh");
    assert_eq!(
        refreshed.key, "fresh",
        "recovery must return the fresh token"
    );
    assert!(
        ok_manager.manual_auth_last_emit().is_none(),
        "a successful auto-refresh must NOT emit a manual_auth event",
    );
    ok_server.abort();
}

/// Consecutive transient failures self-heal up to a bound, then escalate to a
/// non-sticky `Other` permanent failure (which ages out via the TTL). A
/// regression here would turn recoverable blips into a permanent `/login`.
#[tokio::test]
async fn auth_backend_contract_transient_failures_escalate_to_non_sticky_permanent() {
    let hits = Arc::new(AtomicU32::new(0));
    // Persistent 503: every refresh attempt is transient.
    let (base_url, server) = start_idp(503, "{}".to_string(), hits, 0).await;
    let dir = tempfile::tempdir().unwrap();
    let auth_manager = Arc::new(
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base_url),
    );
    auth_manager.hot_swap(expired_oidc(&base_url));

    // One refresher instance: it owns the consecutive-failure counter.
    // Budget is above try_recover_unauthorized's per-recovery attempts so a
    // single 401 recovery cannot alone escalate; exhaust the full budget here.
    let refresher = OidcRefresher::new(auth_manager.clone());
    let mut outcomes = Vec::new();
    for _ in 0..5 {
        outcomes.push(refresher.refresh(RefreshReason::ServerRejected).await);
    }

    assert!(
        matches!(outcomes[0], RefreshOutcome::TransientFailure { .. }),
        "first blip is transient, not a lockout: {:?}",
        outcomes[0],
    );
    assert!(
        matches!(outcomes[3], RefreshOutcome::TransientFailure { .. }),
        "4th blip still under escalation budget: {:?}",
        outcomes[3],
    );
    match &outcomes[4] {
        RefreshOutcome::PermanentFailure { error, .. } => {
            assert_eq!(
                error.reason,
                RefreshTokenFailedReason::Other,
                "escalation must use the generic Other reason",
            );
            assert!(
                !error.reason.is_sticky(),
                "an escalated transient must age out, not strand the user forever",
            );
        }
        other => panic!("repeated transients must escalate to a permanent Other, got {other:?}"),
    }

    server.abort();
}

/// Two `AuthManager`s sharing one auth.json stand in for two CLI processes: the
/// auth.json flock must serialize their refreshes so the shared refresh token is
/// spent at the IdP exactly once. The loser adopts the rotated token from disk
/// instead of racing a second exchange (which the IdP could revoke as reuse).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_backend_contract_two_instances_share_one_idp_call() {
    let hits = Arc::new(AtomicU32::new(0));
    let (url, server) = start_idp(
        200,
        r#"{"access_token":"fresh","refresh_token":"fresh-rt","expires_in":3600}"#.to_string(),
        hits.clone(),
        100,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();

    // Distinct managers, same on-disk auth.json (separate flock OFDs => they
    // genuinely contend, like two processes).
    let new_instance = || {
        let m = Arc::new(
            AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&url),
        );
        m.hot_swap(expired_oidc(&url));
        m.set_refresher(Arc::new(OidcRefresher::new(m.clone())));
        m
    };
    let a = new_instance();
    let b = new_instance();

    let (ra, rb) = tokio::join!(a.auth(), b.auth());

    assert_eq!(ra.expect("instance A must obtain a token").key, "fresh");
    assert_eq!(rb.expect("instance B must obtain a token").key, "fresh");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "two instances sharing auth.json must spend the refresh token at the IdP only once",
    );

    server.abort();
}
