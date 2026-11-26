use super::*;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use parking_lot::Mutex;

fn test_cfg() -> ProactiveRefreshConfig {
    ProactiveRefreshConfig {
        enabled: true,
        fraction: 0.6,
        jitter_fraction: 0.2,
        safety_margin: Duration::from_secs(120),
        min_refresh_interval: Duration::from_secs(60),
    }
}

fn identity(user_id: &str) -> AuthIdentity {
    AuthIdentity {
        user_id: user_id.to_owned(),
        principal_type: None,
        principal_id: None,
    }
}

fn secs_between(a: DateTime<Utc>, b: DateTime<Utc>) -> i64 {
    (b - a).num_seconds()
}

#[test]
fn steady_state_schedule_lands_in_jitter_window() {
    let now = Utc::now();
    let ttl = Duration::from_secs(3600);
    let exp = datetime_plus(now, ttl).unwrap();
    let cfg = test_cfg();

    let (at, jitter) = compute_success_refresh_at(now, Some(exp), Some(ttl), &cfg, 0.0);
    assert_eq!(secs_between(now, at), 2160);
    assert_eq!(jitter, 0.0);

    let (at, jitter) = compute_success_refresh_at(now, Some(exp), Some(ttl), &cfg, 1.0);
    assert_eq!(secs_between(now, at), 2880);
    assert!((jitter - 720.0).abs() < 0.001);

    let (at, jitter) = compute_success_refresh_at(now, Some(exp), Some(ttl), &cfg, -1.0);
    assert_eq!(secs_between(now, at), 1440);
    assert!((jitter + 720.0).abs() < 0.001);

    assert!(at <= datetime_minus(exp, cfg.safety_margin).unwrap());
    assert!(at >= datetime_plus(now, cfg.min_refresh_interval).unwrap());
}

#[test]
fn cold_start_schedule_uses_remaining_lifetime() {
    let now = Utc::now();
    let rem = Duration::from_secs(1000);
    let exp = datetime_plus(now, rem).unwrap();
    let cfg = test_cfg();

    let (at, jitter) = compute_success_refresh_at(now, Some(exp), None, &cfg, 0.0);
    assert_eq!(secs_between(now, at), 600);
    assert_eq!(jitter, 0.0);

    let (at, _) = compute_success_refresh_at(now, Some(exp), None, &cfg, 1.0);
    assert_eq!(secs_between(now, at), 800);

    let (at, _) = compute_success_refresh_at(now, Some(exp), None, &cfg, -1.0);
    assert_eq!(secs_between(now, at), 400);

    assert!(at <= datetime_minus(exp, cfg.safety_margin).unwrap());
    assert!(at >= datetime_plus(now, cfg.min_refresh_interval).unwrap());
}

#[test]
fn cold_start_expired_or_inside_safety_window_refreshes_now() {
    let now = Utc::now();
    let cfg = test_cfg();

    let expired = datetime_minus(now, Duration::from_secs(1)).unwrap();
    let (at, _) = compute_success_refresh_at(now, Some(expired), None, &cfg, 0.0);
    assert_eq!(at, now, "expired seed must not wait min_refresh_interval");

    let near = datetime_plus(now, Duration::from_secs(30)).unwrap();
    let (at, _) = compute_success_refresh_at(now, Some(near), None, &cfg, 1.0);
    assert_eq!(
        at, now,
        "seed inside the safety window must refresh immediately"
    );
}

#[test]
fn short_ttl_success_path_floored_to_min_interval() {
    let now = Utc::now();
    let ttl = Duration::from_secs(30);
    let exp = datetime_plus(now, ttl).unwrap();
    let cfg = test_cfg();
    let (at, _) = compute_success_refresh_at(now, Some(exp), Some(ttl), &cfg, 0.0);
    assert_eq!(secs_between(now, at), 60);
    let (again, _) = compute_success_refresh_at(at, Some(exp), Some(ttl), &cfg, 0.0);
    assert!(secs_between(at, again) >= 60);
}

#[test]
fn clipped_jitter_reports_effective_displacement() {
    let now = Utc::now();
    let ttl = Duration::from_secs(30);
    let exp = datetime_plus(now, ttl).unwrap();
    let cfg = test_cfg();
    let (at0, j0) = compute_success_refresh_at(now, Some(exp), Some(ttl), &cfg, 0.0);
    let (at1, j1) = compute_success_refresh_at(now, Some(exp), Some(ttl), &cfg, 1.0);
    let (atn, jn) = compute_success_refresh_at(now, Some(exp), Some(ttl), &cfg, -1.0);
    assert_eq!(secs_between(now, at0), 60);
    assert_eq!(at0, at1);
    assert_eq!(at0, atn);
    // Nominal is exp − 0.4·ttl = now+18; floor is now+60 ⇒ +42s.
    assert!((j0 - 42.0).abs() < 0.1);
    assert!((j0 - j1).abs() < 0.001);
    assert!((j0 - jn).abs() < 0.001);
}

#[test]
fn jitter_is_non_degenerate_over_many_draws() {
    let now = Utc::now();
    let ttl = Duration::from_secs(3600);
    let exp = datetime_plus(now, ttl).unwrap();
    let cfg = test_cfg();
    let mut seen = std::collections::BTreeSet::new();
    for i in 0..40 {
        let unit = (i as f64) / 20.0 - 1.0;
        let (at, _) = compute_success_refresh_at(now, Some(exp), Some(ttl), &cfg, unit);
        seen.insert(at.timestamp_millis());
        assert!(at <= datetime_minus(exp, cfg.safety_margin).unwrap());
        assert!(at >= datetime_plus(now, cfg.min_refresh_interval).unwrap());
    }
    assert!(
        seen.len() > 10,
        "jitter draws collapsed: {} unique targets",
        seen.len()
    );
}

#[test]
fn retry_delay_not_floored_by_min_interval_and_bounded_by_expiry() {
    let now = Utc::now();
    let exp = datetime_plus(now, Duration::from_secs(5)).unwrap();
    let first = compute_retry_delay(0, now, Some(exp));
    assert_eq!(first, Duration::from_secs(1));
    assert!(first < Duration::from_secs(60));

    let capped_by_expiry = compute_retry_delay(10, now, Some(exp));
    assert_eq!(capped_by_expiry, Duration::from_secs(5));

    let past = datetime_plus(now, Duration::from_secs(10)).unwrap();
    assert_eq!(compute_retry_delay(0, past, Some(exp)), RETRY_CAP);
}

#[test]
fn retry_delay_grows_exponentially_to_cap() {
    let now = Utc::now();
    assert_eq!(compute_retry_delay(0, now, None), Duration::from_secs(1));
    assert_eq!(compute_retry_delay(1, now, None), Duration::from_secs(2));
    assert_eq!(compute_retry_delay(2, now, None), Duration::from_secs(4));
    assert_eq!(compute_retry_delay(5, now, None), RETRY_CAP);
}

#[test]
fn parse_retry_after_zero_or_past_is_absent() {
    assert_eq!(parse_retry_after_value("0"), None);
    assert_eq!(parse_retry_after_value("5"), Some(Duration::from_secs(5)));
    assert_eq!(
        parse_retry_after_value("Wed, 21 Oct 2015 07:28:00 GMT"),
        None
    );
    let future = (Utc::now() + chrono::TimeDelta::seconds(30)).to_rfc2822();
    let parsed = parse_retry_after_value(&future).expect("future HTTP-date");
    assert!(parsed > Duration::from_secs(20) && parsed <= Duration::from_secs(30));
}

#[test]
fn bound_retry_after_floors_zero_to_retry_base() {
    let inner = Inner {
        snapshot: arc_swap::ArcSwap::from_pointee(TokenSnapshot {
            access_token: Arc::from("tok"),
            expires_at: None,
            observed_ttl: None,
        }),
        issuer: "https://auth.example.com".into(),
        client_id: "client1".into(),
        identity: identity("user-9"),
        cfg: test_cfg(),
        on_refresh: None,
        persist: Arc::new(PersistGate {
            seq: std::sync::atomic::AtomicU64::new(0),
            lock: parking_lot::Mutex::new(()),
        }),
    };
    assert_eq!(bound_retry_after(Duration::ZERO, &inner), RETRY_BASE);
    assert_eq!(
        bound_retry_after(Duration::from_secs(5), &inner),
        Duration::from_secs(5)
    );
}

fn provider_params(issuer: String, expires_at: Option<DateTime<Utc>>) -> ProactiveOidcParams {
    ProactiveOidcParams {
        access_token: "stale-access".into(),
        refresh_token: "refresh-tok".into(),
        issuer,
        client_id: "client1".into(),
        identity: identity("user-9"),
        expires_at,
        refresh: ProactiveRefreshConfig {
            enabled: true,
            fraction: 0.6,
            jitter_fraction: 0.0,
            safety_margin: Duration::from_secs(2),
            min_refresh_interval: Duration::ZERO,
        },
        on_refresh: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn current_never_blocks_or_touches_network() {
    let provider = ProactiveOidcAuthProvider::new(provider_params(
        "http://127.0.0.1:1".into(),
        Some(Utc::now() - chrono::TimeDelta::hours(1)),
    ));
    let start = Instant::now();
    let cred = provider.current();
    assert!(
        start.elapsed() < Duration::from_millis(50),
        "current() stalled for {:?}",
        start.elapsed()
    );
    match cred {
        AuthCredential::Bearer { token } => assert_eq!(token, "stale-access"),
        other => panic!("expected Bearer, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn principal_key_is_stable_across_rotation() {
    let provider = ProactiveOidcAuthProvider::new(provider_params(
        "https://auth.example.com".into(),
        Some(Utc::now() + chrono::TimeDelta::hours(1)),
    ));
    let k1 = provider.principal_key();
    provider.swap_access_token("rotated-access");
    let k2 = provider.principal_key();
    assert_eq!(k1, k2);
    assert_ne!(k1, AuthCredential::bearer("rotated-access").principal_key());
    assert_ne!(k1, AuthCredential::bearer("stale-access").principal_key());
    match provider.current() {
        AuthCredential::Bearer { token } => assert_eq!(token, "rotated-access"),
        other => panic!("expected Bearer, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn principal_key_includes_empty_user_id() {
    let mut params = provider_params("https://auth.example.com".into(), None);
    params.identity.user_id.clear();
    let empty = ProactiveOidcAuthProvider::new(params).principal_key();
    let with_user =
        ProactiveOidcAuthProvider::new(provider_params("https://auth.example.com".into(), None))
            .principal_key();
    assert_ne!(
        empty, with_user,
        "empty user_id must still be part of the fingerprint"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn disabled_flag_does_not_refresh_or_spawn() {
    let _metrics = lock_metrics();
    let skipped_before = refresh_count(OUTCOME_SKIPPED_DISABLED);
    let ok_before = refresh_count(OUTCOME_OK);
    let hits = Arc::new(AtomicU32::new(0));
    let base = spawn_mock_idp(
        serde_json::json!({
            "access_token": "fresh-access",
            "refresh_token": "fresh-refresh",
            "expires_in": 5
        }),
        axum::http::StatusCode::OK,
        hits.clone(),
    )
    .await;

    let mut params = provider_params(base, Some(Utc::now() + chrono::TimeDelta::seconds(2)));
    params.refresh.enabled = false;
    params.refresh.min_refresh_interval = Duration::ZERO;
    let provider = ProactiveOidcAuthProvider::new(params);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "disabled must not hit the IdP"
    );
    assert_eq!(refresh_count(OUTCOME_SKIPPED_DISABLED), skipped_before + 1);
    assert_eq!(refresh_count(OUTCOME_OK), ok_before);
    match provider.current() {
        AuthCredential::Bearer { token } => assert_eq!(token, "stale-access"),
        other => panic!("expected stale Bearer, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn debug_does_not_leak_tokens() {
    let provider =
        ProactiveOidcAuthProvider::new(provider_params("https://auth.example.com".into(), None));
    let debug = format!("{provider:?}");
    assert!(!debug.contains("stale-access"));
    assert!(!debug.contains("refresh-tok"));
}

#[tokio::test(flavor = "current_thread")]
async fn identity_surfaces_principal_fields() {
    let mut params = provider_params("https://auth.example.com".into(), None);
    params.identity = AuthIdentity {
        user_id: "user-1".into(),
        principal_type: Some("Team".into()),
        principal_id: Some("team-9".into()),
    };
    let provider = ProactiveOidcAuthProvider::new(params);
    let id = provider.identity().expect("identity present");
    assert_eq!(id.user_id, "user-1");
    assert_eq!(id.principal_type.as_deref(), Some("Team"));
    assert_eq!(id.principal_id.as_deref(), Some("team-9"));
}

async fn spawn_mock_idp(
    token_body: serde_json::Value,
    status: axum::http::StatusCode,
    hits: Arc<AtomicU32>,
) -> String {
    use axum::Router;
    use axum::routing::{get, post};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let token_endpoint = format!("{base}/token");
    let app =
        Router::new()
            .route(
                "/.well-known/openid-configuration",
                get({
                    let token_endpoint = token_endpoint.clone();
                    move || {
                        let token_endpoint = token_endpoint.clone();
                        async move {
                            axum::Json(serde_json::json!({ "token_endpoint": token_endpoint }))
                        }
                    }
                }),
            )
            .route(
                "/token",
                post({
                    let hits = hits.clone();
                    let token_body = token_body.clone();
                    move || {
                        let hits = hits.clone();
                        let token_body = token_body.clone();
                        async move {
                            hits.fetch_add(1, Ordering::SeqCst);
                            (status, axum::Json(token_body))
                        }
                    }
                }),
            );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    base
}

fn write_auth_json(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("auth.json");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(
        br#"{
            "oidc": {
                "key": "stale-access",
                "user_id": "u1",
                "refresh_token": "refresh-tok",
                "oidc_issuer": "https://auth.example.com",
                "oidc_client_id": "client1"
            }
        }"#,
    )
    .unwrap();
    path
}

async fn wait_auth_json_field(
    path: &std::path::Path,
    field: &str,
    expected: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw)
            && value["oidc"][field].as_str() == Some(expected)
        {
            return value;
        }
        if Instant::now() >= deadline {
            panic!("auth.json {field} did not become {expected:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_auth_json_changed(
    path: &std::path::Path,
    field: &str,
    not_eq: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(raw) = std::fs::read_to_string(path)
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw)
            && value["oidc"][field].as_str() != Some(not_eq)
        {
            return value;
        }
        if Instant::now() >= deadline {
            panic!("auth.json {field} stayed {not_eq:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn background_refresh_updates_snapshot_against_mock_idp() {
    let _metrics = lock_metrics();
    let ok_before = refresh_count(OUTCOME_OK);
    let lead_before = lead_sample_count();
    let lead_sum_before = lead_sample_sum();
    let duration_before = duration_sample_count();
    let jitter_before = jitter_sample_count();

    let hits = Arc::new(AtomicU32::new(0));
    let base = spawn_mock_idp(
        serde_json::json!({
            "access_token": "fresh-access",
            "refresh_token": "fresh-refresh",
            "expires_in": 5
        }),
        axum::http::StatusCode::OK,
        hits.clone(),
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let auth_path = write_auth_json(dir.path());
    let persist_path = auth_path.clone();
    let mut params = provider_params(base, Some(Utc::now() + chrono::TimeDelta::seconds(5)));
    params.on_refresh = Some(Arc::new(move |event: &RefreshEvent| {
        crate::hub_auth::write_refreshed_token(&persist_path, "oidc", event).unwrap();
    }));
    let provider = ProactiveOidcAuthProvider::new(params);

    match provider.current() {
        AuthCredential::Bearer { token } => assert_eq!(token, "stale-access"),
        other => panic!("expected Bearer, got {other:?}"),
    }

    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match provider.current() {
            AuthCredential::Bearer { token } if token == "fresh-access" => break,
            _ if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            _ => panic!("background refresh did not update the snapshot"),
        }
    }

    let updated = wait_auth_json_field(&auth_path, "refresh_token", "fresh-refresh").await;
    assert_eq!(updated["oidc"]["key"], "fresh-access");
    assert_eq!(updated["oidc"]["refresh_token"], "fresh-refresh");
    assert!(hits.load(Ordering::SeqCst) >= 1);
    assert!(refresh_count(OUTCOME_OK) > ok_before);
    let new_leads = lead_sample_count() - lead_before;
    assert_eq!(new_leads, 1);
    let lead = (lead_sample_sum() - lead_sum_before) / new_leads as f64;
    // Old remaining after a ~3s wait from a 5s token — not the new expires_in=5.
    assert!(
        (0.0..4.5).contains(&lead),
        "lead={lead} looks like the new TTL rather than old remaining"
    );
    assert!(duration_sample_count() > duration_before);
    assert!(jitter_sample_count() >= jitter_before);
}

fn lead_cumulative_le(bound: f64) -> u64 {
    prometheus::gather()
        .iter()
        .find(|mf| mf.name() == "echo_agent_workspace_oidc_refresh_lead_seconds")
        .into_iter()
        .flat_map(|mf| mf.get_metric())
        .flat_map(|m| m.get_histogram().get_bucket())
        .find(|b| b.upper_bound() == bound)
        .map_or(0, |b| b.cumulative_count())
}

#[test]
fn lead_histogram_separates_negative_from_small_positive() {
    let _metrics = lock_metrics();
    let _ = &*REFRESH_LEAD;
    let le0_before = lead_cumulative_le(0.0);
    let le10_before = lead_cumulative_le(10.0);
    let count_before = lead_sample_count();

    REFRESH_LEAD.observe(-5.0);
    REFRESH_LEAD.observe(5.0);

    assert_eq!(
        lead_sample_count() - count_before,
        2,
        "both observations must be recorded"
    );
    assert_eq!(
        lead_cumulative_le(0.0) - le0_before,
        1,
        "only the negative lead may land in le +0"
    );
    assert_eq!(
        lead_cumulative_le(10.0) - le10_before,
        2,
        "both observations land in le +10; a missing zero bucket would hide the negative"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn failed_refresh_retries_faster_than_min_interval_then_exhausts() {
    let _metrics = lock_metrics();
    let retry_before = refresh_count(OUTCOME_FAILED_RETRY);
    let exhausted_before = refresh_count(OUTCOME_FAILED_EXHAUSTED);

    let hits = Arc::new(AtomicU32::new(0));
    let times = Arc::new(Mutex::new(Vec::<Instant>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let token_endpoint = format!("{base}/token");
    let times_h = times.clone();
    let hits_h = hits.clone();
    let app =
        axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                axum::routing::get({
                    let token_endpoint = token_endpoint.clone();
                    move || {
                        let token_endpoint = token_endpoint.clone();
                        async move {
                            axum::Json(serde_json::json!({ "token_endpoint": token_endpoint }))
                        }
                    }
                }),
            )
            .route(
                "/token",
                axum::routing::post(move || {
                    let times = times_h.clone();
                    let hits = hits_h.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        times.lock().push(Instant::now());
                        (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            axum::Json(serde_json::json!({"error": "temporarily_unavailable"})),
                        )
                    }
                }),
            );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;

    let provider = ProactiveOidcAuthProvider::new(provider_params(
        base,
        Some(Utc::now() + chrono::TimeDelta::seconds(3)),
    ));

    let deadline = Instant::now() + Duration::from_secs(8);
    while hits.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let recorded = times.lock().clone();
    assert!(
        recorded.len() >= 2,
        "expected at least two retries, got {}",
        recorded.len()
    );
    let gap = recorded[1].saturating_duration_since(recorded[0]);
    assert!(
        gap < Duration::from_secs(60),
        "failure retry was floored to success-path interval: {gap:?}"
    );

    let exhaust_deadline = Instant::now() + Duration::from_secs(6);
    while refresh_count(OUTCOME_FAILED_EXHAUSTED) == exhausted_before
        && Instant::now() < exhaust_deadline
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        refresh_count(OUTCOME_FAILED_EXHAUSTED) > exhausted_before,
        "failed_exhausted was not recorded"
    );
    assert!(refresh_count(OUTCOME_FAILED_RETRY) >= retry_before);
    match provider.current() {
        AuthCredential::Bearer { token } => assert_eq!(token, "stale-access"),
        other => panic!("expected stale Bearer, got {other:?}"),
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn huge_expires_in_is_a_refresh_failure() {
    let _metrics = lock_metrics();
    let retry_before = refresh_count(OUTCOME_FAILED_RETRY);
    let exhausted_before = refresh_count(OUTCOME_FAILED_EXHAUSTED);
    let ok_before = refresh_count(OUTCOME_OK);

    let hits = Arc::new(AtomicU32::new(0));
    let base = spawn_mock_idp(
        serde_json::json!({
            "access_token": "fresh-access",
            "refresh_token": "fresh-refresh",
            "expires_in": u64::MAX
        }),
        axum::http::StatusCode::OK,
        hits.clone(),
    )
    .await;

    let provider = ProactiveOidcAuthProvider::new(provider_params(
        base,
        Some(Utc::now() + chrono::TimeDelta::seconds(4)),
    ));

    let deadline = Instant::now() + Duration::from_secs(8);
    while hits.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        hits.load(Ordering::SeqCst) >= 1,
        "refresh never reached IdP"
    );

    let fail_deadline = Instant::now() + Duration::from_secs(3);
    while refresh_count(OUTCOME_FAILED_RETRY) == retry_before
        && refresh_count(OUTCOME_FAILED_EXHAUSTED) == exhausted_before
        && Instant::now() < fail_deadline
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        refresh_count(OUTCOME_FAILED_RETRY) > retry_before
            || refresh_count(OUTCOME_FAILED_EXHAUSTED) > exhausted_before,
        "out-of-range expires_in was not treated as a refresh failure"
    );
    assert_eq!(refresh_count(OUTCOME_OK), ok_before);
    match provider.current() {
        AuthCredential::Bearer { token } => assert_eq!(token, "stale-access"),
        other => panic!("expected stale Bearer, got {other:?}"),
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn invalid_expires_in_keeps_rotated_refresh_token() {
    let _metrics = lock_metrics();
    let presented = Arc::new(Mutex::new(Vec::<String>::new()));
    let spent_old = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let token_endpoint = format!("{base}/token");
    let presented_h = presented.clone();
    let spent_old_h = spent_old.clone();
    let app =
        axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                axum::routing::get({
                    let token_endpoint = token_endpoint.clone();
                    move || {
                        let token_endpoint = token_endpoint.clone();
                        async move {
                            axum::Json(serde_json::json!({ "token_endpoint": token_endpoint }))
                        }
                    }
                }),
            )
            .route(
                "/token",
                axum::routing::post(move |body: String| {
                    let presented = presented_h.clone();
                    let spent_old = spent_old_h.clone();
                    async move {
                        let rt = body
                            .split('&')
                            .find_map(|part| part.strip_prefix("refresh_token="))
                            .unwrap_or_default()
                            .to_owned();
                        presented.lock().push(rt.clone());
                        match rt.as_str() {
                            "refresh-tok" => {
                                if spent_old.swap(true, Ordering::SeqCst) {
                                    (
                                        axum::http::StatusCode::BAD_REQUEST,
                                        axum::Json(serde_json::json!({"error": "invalid_grant"})),
                                    )
                                } else {
                                    (
                                        axum::http::StatusCode::OK,
                                        axum::Json(serde_json::json!({
                                            "access_token": "interim-access",
                                            "refresh_token": "rotated-rt",
                                            "expires_in": u64::MAX
                                        })),
                                    )
                                }
                            }
                            "rotated-rt" => (
                                axum::http::StatusCode::OK,
                                axum::Json(serde_json::json!({
                                    "access_token": "fresh-2",
                                    "refresh_token": "rt-2",
                                    "expires_in": 3600
                                })),
                            ),
                            _ => (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::Json(serde_json::json!({"error": "invalid_grant"})),
                            ),
                        }
                    }
                }),
            );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;

    let dir = tempfile::tempdir().unwrap();
    let auth_path = write_auth_json(dir.path());
    let persist_path = auth_path.clone();
    let mut params = provider_params(base, Some(Utc::now() + chrono::TimeDelta::seconds(3)));
    params.on_refresh = Some(Arc::new(move |event: &RefreshEvent| {
        crate::hub_auth::write_refreshed_token(&persist_path, "oidc", event).unwrap();
    }));
    let provider = ProactiveOidcAuthProvider::new(params);

    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match provider.current() {
            AuthCredential::Bearer { token } if token == "fresh-2" => break,
            _ if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            _ => panic!(
                "did not recover with rotated refresh token; presented={:?}",
                presented.lock().clone()
            ),
        }
    }

    let seen = presented.lock().clone();
    assert_eq!(
        seen.iter().filter(|t| t.as_str() == "refresh-tok").count(),
        1,
        "old refresh token reused after rotation: {seen:?}"
    );
    assert!(
        seen.iter().any(|t| t == "rotated-rt"),
        "rotated refresh token was never presented: {seen:?}"
    );
    let updated = wait_auth_json_changed(&auth_path, "refresh_token", "refresh-tok").await;
    assert_ne!(updated["oidc"]["refresh_token"], "refresh-tok");
}

#[tokio::test]
async fn drop_aborts_background_task() {
    let hits = Arc::new(AtomicU32::new(0));
    let base = spawn_mock_idp(
        serde_json::json!({"error": "no"}),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        hits.clone(),
    )
    .await;
    let params = provider_params(base, Some(Utc::now() + chrono::TimeDelta::seconds(2)));
    let provider = ProactiveOidcAuthProvider::new(params);

    let deadline = Instant::now() + Duration::from_secs(5);
    while hits.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        hits.load(Ordering::SeqCst) >= 1,
        "task never issued a refresh"
    );
    let at_drop = hits.load(Ordering::SeqCst);
    drop(provider);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = hits.load(Ordering::SeqCst);
    assert!(
        after <= at_drop + 1,
        "task kept refreshing after drop: {at_drop} → {after}"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        after,
        "task continued after abort window"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn expired_seed_refreshes_without_min_interval_delay() {
    let _metrics = lock_metrics();
    let hits = Arc::new(AtomicU32::new(0));
    let base = spawn_mock_idp(
        serde_json::json!({
            "access_token": "fresh-access",
            "refresh_token": "fresh-refresh",
            "expires_in": 3600
        }),
        axum::http::StatusCode::OK,
        hits.clone(),
    )
    .await;

    let mut params = provider_params(base, Some(Utc::now() - chrono::TimeDelta::seconds(1)));
    params.refresh = ProactiveRefreshConfig {
        enabled: true,
        ..ProactiveRefreshConfig::default()
    };
    let provider = ProactiveOidcAuthProvider::new(params);

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match provider.current() {
            AuthCredential::Bearer { token } if token == "fresh-access" => break,
            _ if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            _ => panic!("expired seed was not refreshed promptly (waited 3s; default min is 60s)"),
        }
    }
    assert!(hits.load(Ordering::SeqCst) >= 1);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn invalid_grant_stops_the_loop() {
    let _metrics = lock_metrics();
    let terminal_before = refresh_count(OUTCOME_FAILED_TERMINAL);
    let hits = Arc::new(AtomicU32::new(0));
    let base = spawn_mock_idp(
        serde_json::json!({"error": "invalid_grant"}),
        axum::http::StatusCode::BAD_REQUEST,
        hits.clone(),
    )
    .await;

    let mut params = provider_params(base, Some(Utc::now() - chrono::TimeDelta::seconds(1)));
    params.refresh = ProactiveRefreshConfig {
        enabled: true,
        ..ProactiveRefreshConfig::default()
    };
    let _provider = ProactiveOidcAuthProvider::new(params);

    let deadline = Instant::now() + Duration::from_secs(3);
    while hits.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        hits.load(Ordering::SeqCst) >= 1,
        "terminal 400 never reached the IdP"
    );
    let at_first = hits.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        at_first,
        "invalid_grant must stop the loop, not retry at RETRY_CAP"
    );
    assert!(refresh_count(OUTCOME_FAILED_TERMINAL) > terminal_before);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn rate_limit_429_retries_and_honors_retry_after() {
    let _metrics = lock_metrics();
    let terminal_before = refresh_count(OUTCOME_FAILED_TERMINAL);
    let retry_before = refresh_count(OUTCOME_FAILED_RETRY);
    let hits = Arc::new(AtomicU32::new(0));
    let base = spawn_rate_limited_idp(hits.clone(), "1").await;

    let mut params = provider_params(base, Some(Utc::now() + chrono::TimeDelta::seconds(30)));
    params.refresh = ProactiveRefreshConfig {
        enabled: true,
        min_refresh_interval: Duration::from_secs(60),
        ..ProactiveRefreshConfig::default()
    };
    let _provider = ProactiveOidcAuthProvider::new(params);

    let deadline = Instant::now() + Duration::from_secs(4);
    while hits.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        hits.load(Ordering::SeqCst) >= 2,
        "429 must retry, not stop the loop (hits={})",
        hits.load(Ordering::SeqCst)
    );
    assert_eq!(
        refresh_count(OUTCOME_FAILED_TERMINAL),
        terminal_before,
        "429 must not be classified as terminal"
    );
    assert!(refresh_count(OUTCOME_FAILED_RETRY) > retry_before);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn rate_limit_429_zero_retry_after_uses_backoff() {
    let _metrics = lock_metrics();
    let hits = Arc::new(AtomicU32::new(0));
    let base = spawn_rate_limited_idp(hits.clone(), "0").await;

    let mut params = provider_params(base, Some(Utc::now() + chrono::TimeDelta::seconds(30)));
    params.refresh = ProactiveRefreshConfig {
        enabled: true,
        min_refresh_interval: Duration::from_secs(60),
        ..ProactiveRefreshConfig::default()
    };
    let _provider = ProactiveOidcAuthProvider::new(params);

    tokio::time::sleep(Duration::from_millis(700)).await;
    let n = hits.load(Ordering::SeqCst);
    assert!(n >= 1, "first 429 attempt should still run (hits={n})");
    assert!(
        n <= 2,
        "Retry-After: 0 must not hot-loop the IdP (hits={n})"
    );
}

#[tokio::test]
async fn stale_persist_does_not_clobber_newer_token() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = write_auth_json(dir.path());
    let mut params = provider_params("https://auth.example.com".into(), None);
    params.refresh.enabled = false;
    // Production wiring: persist_on_refresh writes synchronously so the
    // PersistGate seq check covers the actual auth.json write.
    params.on_refresh = Some(crate::hub_auth::persist_on_refresh(
        auth_path.clone(),
        "oidc".into(),
    ));
    let provider = ProactiveOidcAuthProvider::new(params);

    provider.persist_for_test(
        "old-access",
        Some("old-rt".into()),
        None,
        Duration::from_millis(150),
    );
    provider.persist_for_test("new-access", Some("new-rt".into()), None, Duration::ZERO);

    let updated = wait_auth_json_field(&auth_path, "refresh_token", "new-rt").await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&auth_path).unwrap()).unwrap();
    assert_eq!(after["oidc"]["key"], "new-access");
    assert_eq!(after["oidc"]["refresh_token"], "new-rt");
    assert_eq!(updated["oidc"]["key"], "new-access");
}

async fn spawn_rate_limited_idp(hits: Arc<AtomicU32>, retry_after: &'static str) -> String {
    use axum::Router;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::routing::{get, post};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let token_endpoint = format!("{base}/token");
    let app =
        Router::new()
            .route(
                "/.well-known/openid-configuration",
                get({
                    let token_endpoint = token_endpoint.clone();
                    move || {
                        let token_endpoint = token_endpoint.clone();
                        async move {
                            axum::Json(serde_json::json!({ "token_endpoint": token_endpoint }))
                        }
                    }
                }),
            )
            .route(
                "/token",
                post(move || {
                    let hits = hits.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        let mut headers = HeaderMap::new();
                        headers.insert(
                            axum::http::header::RETRY_AFTER,
                            HeaderValue::from_static(retry_after),
                        );
                        (
                            StatusCode::TOO_MANY_REQUESTS,
                            headers,
                            axum::Json(serde_json::json!({"error": "slow_down"})),
                        )
                    }
                }),
            );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    base
}
