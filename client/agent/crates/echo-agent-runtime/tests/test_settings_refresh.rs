//! Integration test: MockInferenceServer `/v1/settings` endpoint and
//! remote settings settings refresh infrastructure.
//!
//! Tests the mock endpoint directly (no binary needed) and verifies
//! the `fetch_settings_blocking` client round-trips correctly with
//! runtime-mutated mock settings.
//!
//! Run locally:
//! ```bash
//! cargo test -p echo-agent-runtime --test test_settings_refresh
//! ```

use std::future::Future;

use echo_agent_runtime::util::config::RemoteSettings;
use echo_agent_test_support::*;

async fn with_local_set<F, Fut>(f: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::task::LocalSet::new().run_until(f()).await;
}

/// Verify the mock `/v1/settings` endpoint returns 404 when no settings
/// are configured (the default). This preserves backward compatibility:
/// existing tests that never call `set_settings` see a 404, and
/// `fetch_settings_blocking` returns `None`.
#[tokio::test]
async fn test_settings_endpoint_returns_404_when_unconfigured() {
    with_local_set(|| async {
        let server = MockInferenceServer::start()
            .await
            .expect("start mock server");

        let resp = reqwest::get(format!("{}/settings", server.url()))
            .await
            .expect("request failed");
        assert_eq!(resp.status(), 404);
    })
    .await;
}

/// Verify the mock `/v1/settings` endpoint returns configured settings
/// and that `set_settings` runtime mutation is reflected immediately.
#[tokio::test]
async fn test_settings_endpoint_returns_configured_settings() {
    with_local_set(|| async {
        let server = MockInferenceServer::start()
            .await
            .expect("start mock server");

        // Configure initial settings
        server.set_settings(RemoteSettings {
            tips: Some(vec!["tip_v1".into()]),
            leader_mode: Some(false),
            ..Default::default()
        });

        // Fetch and verify
        let resp = reqwest::get(format!("{}/settings", server.url()))
            .await
            .expect("request failed");
        assert_eq!(resp.status(), 200);
        let settings: RemoteSettings = resp.json().await.expect("parse failed");
        assert_eq!(settings.tips, Some(vec!["tip_v1".into()]));
        assert_eq!(settings.leader_mode, Some(false));
    })
    .await;
}

/// Verify that `set_settings` updates are visible to subsequent requests
/// (runtime mutation for multi-session test scenarios).
#[tokio::test]
async fn test_settings_endpoint_reflects_runtime_mutations() {
    with_local_set(|| async {
        let server = MockInferenceServer::start()
            .await
            .expect("start mock server");

        // Initial settings
        server.set_settings(RemoteSettings {
            tips: Some(vec!["tip_v1".into()]),
            ..Default::default()
        });
        let settings: RemoteSettings = reqwest::get(format!("{}/settings", server.url()))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(settings.tips, Some(vec!["tip_v1".into()]));

        // Mutate settings (simulating a remote feature flag change)
        server.set_settings(RemoteSettings {
            tips: Some(vec!["tip_v2".into()]),
            leader_mode: Some(true),
            ..Default::default()
        });

        // Subsequent request sees the updated values
        let settings: RemoteSettings = reqwest::get(format!("{}/settings", server.url()))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(settings.tips, Some(vec!["tip_v2".into()]));
        assert_eq!(settings.leader_mode, Some(true));
    })
    .await;
}

/// Verify `fetch_settings_blocking` round-trips through the mock server.
/// This is the actual client function used by `refresh_remote_settings`.
#[tokio::test]
async fn test_fetch_settings_blocking_round_trip() {
    with_local_set(|| async {
        let server = MockInferenceServer::start()
            .await
            .expect("start mock server");

        // Without settings configured: returns None (404 from mock)
        let auth = echo_agent_runtime::auth::EchoAgentAuth {
            key: "test-key".into(),
            ..Default::default()
        };
        let result = tokio::task::spawn_blocking({
            let url = server.url().to_string();
            let auth = auth.clone();
            move || {
                echo_agent_runtime::remote::fetch_settings_blocking(&url, &auth, None).into_option()
            }
        })
        .await
        .unwrap();
        assert!(
            result.is_none(),
            "Expected None when settings not configured"
        );

        // With settings configured: returns Some(settings)
        server.set_settings(RemoteSettings {
            tips: Some(vec!["fetched_tip".into()]),
            ..Default::default()
        });
        let result = tokio::task::spawn_blocking({
            let url = server.url().to_string();
            let auth = auth.clone();
            move || {
                echo_agent_runtime::remote::fetch_settings_blocking(&url, &auth, None).into_option()
            }
        })
        .await
        .unwrap();
        let settings = result.expect("Expected Some when settings are configured");
        assert_eq!(settings.tips, Some(vec!["fetched_tip".into()]));
    })
    .await;
}

/// Verify the `doom_loop_recovery` settings object survives the
/// `/v1/settings` round-trip, that its absence deserializes to `None` (old
/// servers), and that a partial object keeps its unset fields `None`.
#[tokio::test]
async fn test_doom_loop_recovery_settings_round_trip() {
    use echo_agent_runtime::util::config::DoomLoopRecoverySettings;

    with_local_set(|| async {
        let server = MockInferenceServer::start()
            .await
            .expect("start mock server");

        // Absent from the payload ⇒ None on the client.
        server.set_settings(RemoteSettings::default());
        let settings: RemoteSettings = reqwest::get(format!("{}/settings", server.url()))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(settings.doom_loop_recovery, None);

        server.set_settings(RemoteSettings {
            doom_loop_recovery: Some(DoomLoopRecoverySettings {
                enabled: Some(true),
                max_threshold: Some(16),
                max_retries: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        });
        let settings: RemoteSettings = reqwest::get(format!("{}/settings", server.url()))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let recovery = settings.doom_loop_recovery.expect("object round-trips");
        assert_eq!(recovery.enabled, Some(true));
        assert_eq!(recovery.max_threshold, Some(16));
        assert_eq!(recovery.max_retries, Some(1));
        assert_eq!(recovery.window_tokens, None);

        // Partial object: only the set field comes through; the rest stay
        // None so the resolver falls through per-field.
        server.set_settings(RemoteSettings {
            doom_loop_recovery: Some(DoomLoopRecoverySettings {
                max_threshold: Some(32),
                ..Default::default()
            }),
            ..Default::default()
        });
        let settings: RemoteSettings = reqwest::get(format!("{}/settings", server.url()))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let recovery = settings.doom_loop_recovery.expect("object round-trips");
        assert_eq!(recovery.enabled, None);
        assert_eq!(recovery.max_threshold, Some(32));
        assert_eq!(recovery.max_retries, None);
        assert_eq!(recovery.window_tokens, None);
    })
    .await;
}

/// Verify that the mock server's request log correctly tracks
/// GET /v1/settings requests for assertion in multi-session tests.
#[tokio::test]
async fn test_settings_requests_appear_in_request_log() {
    with_local_set(|| async {
        let server = MockInferenceServer::start()
            .await
            .expect("start mock server");
        server.set_settings(RemoteSettings::default());

        assert_eq!(server.request_count(), 0);

        // First request
        let _ = reqwest::get(format!("{}/settings", server.url()))
            .await
            .unwrap();
        let settings_reqs: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|r| r.method == "GET" && r.path.contains("/settings"))
            .collect();
        assert_eq!(settings_reqs.len(), 1, "Expected 1 settings request");

        // Second request (simulating /new refresh)
        let _ = reqwest::get(format!("{}/settings", server.url()))
            .await
            .unwrap();
        let settings_reqs: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|r| r.method == "GET" && r.path.contains("/settings"))
            .collect();
        assert_eq!(settings_reqs.len(), 2, "Expected 2 settings requests");
    })
    .await;
}
