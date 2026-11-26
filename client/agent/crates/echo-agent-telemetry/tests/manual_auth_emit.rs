#![allow(clippy::disallowed_methods)] // test clients hit localhost mocks
//! Wire test: `log_event(ManualAuth)` must POST to the product events endpoint as
//! `echo-agent-shell-manual_auth` with the `reason`/`trigger`/`token_kind`/`principal`
//! the `distinct(principal)` alert consumes. Mocks the observability backend
//! (real HTTP collector) so the emit->wire path is checked, not just the struct.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use echo_agent_telemetry::client;
use echo_agent_telemetry::config::{TelemetryConfig, TelemetryMode};
use echo_agent_telemetry::events::{
    AuthTokenKind, ManualAuth, ManualAuthReason, ManualAuthSurface,
};
use echo_agent_telemetry::process_info::{
    Entrypoint, Interactivity, LeaderMode, ProcessIdentity, ReleaseChannel, set_identity,
    set_release_channel,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_auth_posts_to_events_endpoint_as_echo_agent_shell_manual_auth() {
    let bodies: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = bodies.clone();
    let app = axum::Router::new().route(
        "/events",
        axum::routing::post(move |axum::Json(v): axum::Json<serde_json::Value>| {
            let captured = captured.clone();
            async move {
                captured.lock().unwrap().push(v);
                axum::http::StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/events", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    set_identity(ProcessIdentity {
        entrypoint: Entrypoint::Cli,
        leader: LeaderMode::Standalone,
        interactivity: Interactivity::Unattended,
    });
    set_release_channel(ReleaseChannel::Alpha);

    client::init(
        TelemetryConfig {
            events_url: Some(url),
            events_api_key: Some("test-key".into()),
            mixpanel_enabled: false,
            ..TelemetryConfig::default()
        },
        TelemetryMode::Enabled,
        Some("user-xyz".into()),
        None,
        None,
        None,
        "0.0.0-test".into(),
        None,
        reqwest::Client::new(),
    );

    echo_agent_telemetry::log_event(ManualAuth {
        reason: ManualAuthReason::RefreshTokenRejected,
        trigger: ManualAuthSurface::Turn,
        token_kind: AuthTokenKind::OidcSession,
        principal: Some("user-xyz".into()),
    });

    // The emit is fire-and-forget; poll the collector for the POST.
    let deadline = Instant::now() + Duration::from_secs(5);
    let event = loop {
        let found = bodies.lock().unwrap().iter().find_map(|b| {
            let e = b.get("events")?.get(0)?;
            (e.get("event_name")?.as_str()? == "echo-agent-shell-manual_auth").then(|| e.clone())
        });
        if let Some(e) = found {
            break e;
        }
        assert!(
            Instant::now() < deadline,
            "no echo-agent-shell-manual_auth POST received"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    let meta = event.get("event_metadata").expect("event_metadata present");
    assert_eq!(
        meta.get("reason").and_then(|v| v.as_str()),
        Some("refresh_token_rejected"),
    );
    assert_eq!(meta.get("trigger").and_then(|v| v.as_str()), Some("turn"));
    assert_eq!(
        meta.get("token_kind").and_then(|v| v.as_str()),
        Some("oidc_session"),
    );
    assert_eq!(
        meta.get("principal").and_then(|v| v.as_str()),
        Some("user-xyz"),
        "principal must be a queryable top-level metadata field for distinct() counting",
    );
    for (key, expected) in [
        ("entrypoint", serde_json::json!("cli")),
        ("is_leader_mode", serde_json::json!(false)),
        ("is_interactive", serde_json::json!(false)),
        ("release_channel", serde_json::json!("alpha")),
        (
            "dev_build",
            serde_json::json!(echo_agent_version::IS_DEV_BUILD),
        ),
        ("sessions_active", serde_json::json!(0)),
        ("subagents_active", serde_json::json!(0)),
        ("compaction_active", serde_json::json!(false)),
        ("mcp_servers_connected", serde_json::json!(0)),
        ("turns_active", serde_json::json!(0)),
        ("workflow_runs_active", serde_json::json!(0)),
    ] {
        assert_eq!(
            meta.get(key),
            Some(&expected),
            "identity and idle gauge values are wire contract: {key}",
        );
    }
    assert!(
        meta.get("uptime_secs").is_some(),
        "the resource fields must ride every product event",
    );
    assert!(
        ["linux", "macos", "windows"]
            .contains(&meta.get("os").and_then(|v| v.as_str()).unwrap_or_default()),
        "os must be a known platform",
    );
    assert!(
        ["x86_64", "aarch64"].contains(
            &meta
                .get("arch")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
        ),
        "arch must be a known architecture",
    );
    assert!(
        meta.get("cpu_cores")
            .is_some_and(|v| v.as_u64().is_some_and(|n| n >= 1)),
        "cpu_cores must be a positive count",
    );
    assert!(
        meta.get("is_ci").is_some_and(|v| v.is_boolean()),
        "is_ci must ride as a boolean",
    );
    for key in ["agent_id", "shell_version"] {
        assert!(
            meta.get(key).is_some(),
            "identity insert {key} must ride every event",
        );
    }
    for key in [
        "team_id",
        "deployment_id",
        "client_type",
        "client_version",
        "subscription_tier",
    ] {
        assert!(
            meta.get(key).is_none(),
            "ctx-gated insert {key} must stay absent under a bare api-key ctx",
        );
    }
    #[cfg(unix)]
    for key in [
        "cpu_time_ms",
        "child_cpu_time_ms",
        "cpu_user_ms",
        "cpu_system_ms",
    ] {
        assert!(
            meta.get(key).is_some_and(|v| v.as_u64().is_some()),
            "cumulative counter {key} must ride the event on unix",
        );
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert!(
        meta.get("rss_bytes")
            .is_some_and(|v| v.as_u64().is_some_and(|b| b > 0)),
        "a live process must carry a nonzero resident set",
    );

    let conditional: &[&str] = &[
        "cpu_share_percent",
        "cpu_window_ms",
        "child_cpu_share_percent",
        "footprint_bytes",
        "memory_limit_bytes",
        "session_id",
        "turn_number",
        #[cfg(not(unix))]
        "cpu_time_ms",
        #[cfg(not(unix))]
        "child_cpu_time_ms",
        #[cfg(not(unix))]
        "cpu_user_ms",
        #[cfg(not(unix))]
        "cpu_system_ms",
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        "rss_bytes",
    ];
    for key in echo_agent_telemetry::client::RESERVED_EVENT_KEYS {
        assert!(
            meta.get(*key).is_some() || conditional.contains(key),
            "reserved key {key} neither present nor known-conditional",
        );
    }

    server.abort();
}
