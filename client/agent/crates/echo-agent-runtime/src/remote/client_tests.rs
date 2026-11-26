use super::*;
use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use std::sync::{Arc, Mutex};
#[test]
fn login_config_response_parses_tristate() {
    let parse = |s: &str| {
        serde_json::from_str::<LoginConfigResponse>(s)
            .unwrap()
            .device_flow
    };
    assert_eq!(parse(r#"{"device_flow": true}"#), Some(true));
    assert_eq!(parse(r#"{"device_flow": false}"#), Some(false));
    assert_eq!(parse(r#"{"device_flow": null}"#), None);
    assert_eq!(parse("{}"), None, "absent flag must parse as unset");
}
#[test]
fn get_env_keys_parses_strings_and_rejects_non_strings() {
    use crate::agent::config::EnvKeys;
    let parse = |v: serde_json::Value| {
        let obj = serde_json::json!({ "env_key": v });
        get_env_keys(obj.as_object().unwrap(), "env_key")
    };
    assert_eq!(parse(serde_json::json!("A")), Some(EnvKeys::single("A")));
    assert_eq!(
        parse(serde_json::json!(["A", "B"])),
        Some(EnvKeys::new(["A", "B"]))
    );
    assert_eq!(parse(serde_json::json!(["A", 123])), None);
    assert_eq!(parse(serde_json::json!([])), None);
}
fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}
#[derive(Debug, Default, Clone)]
struct LoginConfigHeaders {
    authorization: Option<String>,
    user_id: Option<String>,
    email: Option<String>,
    agent_id: Option<String>,
    client_identifier: Option<String>,
    client_version: Option<String>,
}
#[derive(Clone)]
struct LoginConfigServerState {
    status_code: StatusCode,
    body: String,
    seen: Arc<Mutex<Vec<LoginConfigHeaders>>>,
}
/// Mock cli-chat-proxy serving `GET /v1/login-config` with a fixed status +
/// raw body, recording the request headers it saw.
async fn start_login_config_server(
    status_code: StatusCode,
    body: String,
) -> (
    String,
    Arc<Mutex<Vec<LoginConfigHeaders>>>,
    tokio::task::JoinHandle<()>,
) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = LoginConfigServerState {
        status_code,
        body,
        seen: seen.clone(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let app = Router::new()
        .route(
            "/v1/login-config",
            get(
                |State(state): State<LoginConfigServerState>, headers: HeaderMap| async move {
                    state.seen.lock().unwrap().push(LoginConfigHeaders {
                        authorization: header_str(&headers, "authorization"),
                        user_id: header_str(&headers, "x-userid"),
                        email: header_str(&headers, "x-email"),
                        agent_id: header_str(&headers, "x-echo-agent-agent-id"),
                        client_identifier: header_str(&headers, "x-echo-agent-client-identifier"),
                        client_version: header_str(&headers, "x-echo-agent-client-version"),
                    });
                    (state.status_code, state.body)
                },
            ),
        )
        .with_state(state);
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("{base}/v1"), seen, handle)
}
#[tokio::test]
async fn fetch_login_device_flow_parses_2xx_bodies() {
    for (body, expected) in [
        (r#"{"device_flow": true}"#, Some(true)),
        (r#"{"device_flow": false}"#, Some(false)),
        (r#"{"device_flow": null}"#, None),
        (r#"{}"#, None),
        (r#"{"other": 1}"#, None),
    ] {
        let (base, _seen, server) =
            start_login_config_server(StatusCode::OK, body.to_string()).await;
        let got = fetch_login_device_flow(&base).await;
        server.abort();
        assert_eq!(got, expected, "body {body:?}");
    }
}
#[tokio::test]
async fn fetch_login_device_flow_errors_return_none() {
    for (status, body) in [
        (StatusCode::NOT_FOUND, r#"{"device_flow": true}"#),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"device_flow": true}"#,
        ),
        (StatusCode::OK, "not json"),
    ] {
        let (base, _seen, server) = start_login_config_server(status, body.to_string()).await;
        let got = fetch_login_device_flow(&base).await;
        server.abort();
        assert_eq!(got, None, "status {status}, body {body:?}");
    }
}
#[tokio::test]
async fn fetch_login_device_flow_sends_only_unauthenticated_headers() {
    let (base, seen, server) =
        start_login_config_server(StatusCode::OK, r#"{"device_flow": true}"#.to_string()).await;
    let got = fetch_login_device_flow(&base).await;
    server.abort();
    assert_eq!(got, Some(true));
    let seen = seen.lock().unwrap();
    let h = seen
        .last()
        .expect("server should have received one request");
    assert!(
        h.agent_id.as_deref().is_some_and(|v| !v.is_empty()),
        "must send x-echo-agent-agent-id (the bucketing key)"
    );
    assert!(
        h.client_identifier.is_some(),
        "must send x-echo-agent-client-identifier"
    );
    assert!(
        h.client_version.is_some(),
        "must send x-echo-agent-client-version"
    );
    assert_eq!(h.authorization, None, "must not send Authorization");
    assert_eq!(h.user_id, None, "must not send x-userid");
    assert_eq!(h.email, None, "must not send x-email");
}
/// Mock cli-chat-proxy serving `GET /settings` with a fixed status + body.
async fn start_settings_server(
    status: StatusCode,
    body: String,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let app = Router::new().route(
        "/settings",
        get(move || {
            let body = body.clone();
            async move { (status, body) }
        }),
    );
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, handle)
}
/// `fetch_settings_blocking` maps each HTTP outcome to the [`SettingsFetch`]
/// variant the external-OTEL gate relies on; 401 is the only outcome that
/// yields `Rejected`, everything else non-2xx fails closed as `Retry`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_fetch_maps_status_to_outcome() {
    let auth = EchoAgentAuth::test_default();
    let cases: [(StatusCode, &str, &str); 6] = [
        (StatusCode::OK, "{}", "Fetched"),
        (StatusCode::UNAUTHORIZED, "{}", "Rejected"),
        (StatusCode::FORBIDDEN, "{}", "Retry"),
        (StatusCode::TOO_MANY_REQUESTS, "{}", "Retry"),
        (StatusCode::INTERNAL_SERVER_ERROR, "{}", "Retry"),
        (StatusCode::OK, "not json", "Retry"),
    ];
    for (status, body, expected) in cases {
        let (base, server) = start_settings_server(status, body.to_string()).await;
        let a = auth.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            fetch_settings_blocking_with_attempts(&base, &a, None, 1)
        })
        .await
        .unwrap();
        server.abort();
        let got = match outcome {
            SettingsFetch::Fetched(_) => "Fetched",
            SettingsFetch::Rejected => "Rejected",
            SettingsFetch::Retry => "Retry",
        };
        assert_eq!(got, expected, "status {status}, body {body:?}");
    }
}
#[derive(Debug, Default, Clone)]
struct SeenHeaders {
    authorization: Option<String>,
    token_auth: Option<String>,
    user_id: Option<String>,
    email: Option<String>,
    alpha_test_key: Option<String>,
    client_version: Option<String>,
}
#[derive(Clone)]
struct BundleServerState {
    body: serde_json::Value,
    status_code: StatusCode,
    seen_headers: Arc<Mutex<Vec<SeenHeaders>>>,
}
async fn start_bundle_server(
    status_code: StatusCode,
    body: serde_json::Value,
) -> (
    String,
    Arc<Mutex<Vec<SeenHeaders>>>,
    tokio::task::JoinHandle<()>,
) {
    let seen_headers = Arc::new(Mutex::new(Vec::new()));
    let state = BundleServerState {
        body,
        status_code,
        seen_headers: seen_headers.clone(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let app = Router::new()
        .route(
            "/v1/subagents/bundle",
            get(
                |State(state): State<BundleServerState>, headers: HeaderMap| async move {
                    state.seen_headers.lock().unwrap().push(SeenHeaders {
                        authorization: headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned),
                        token_auth: headers
                            .get("x-echo-agent-token-auth")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned),
                        user_id: headers
                            .get("x-userid")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned),
                        email: headers
                            .get("x-email")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned),
                        alpha_test_key: {
                            let _ = &headers;
                            None
                        },
                        client_version: headers
                            .get("x-echo-agent-client-version")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned),
                    });
                    (state.status_code, axum::Json(state.body))
                },
            ),
        )
        .route(
            "/forward/{tail}",
            get(
                |Path(_tail): Path<String>,
                 State(state): State<BundleServerState>,
                 headers: HeaderMap| async move {
                    state.seen_headers.lock().unwrap().push(SeenHeaders {
                        authorization: headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned),
                        token_auth: headers
                            .get("x-echo-agent-token-auth")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned),
                        user_id: headers
                            .get("x-userid")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned),
                        email: headers
                            .get("x-email")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned),
                        alpha_test_key: {
                            let _ = &headers;
                            None
                        },
                        client_version: headers
                            .get("x-echo-agent-client-version")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned),
                    });
                    (state.status_code, axum::Json(state.body))
                },
            ),
        )
        .with_state(state);
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("{base}/v1"), seen_headers, handle)
}
fn test_auth() -> EchoAgentAuth {
    EchoAgentAuth {
        key: "token".to_string(),
        auth_mode: crate::auth::AuthMode::Oidc,
        create_time: chrono::Utc::now(),
        user_id: "user-1".to_string(),
        email: Some("test@example.com".to_string()),
        first_name: None,
        last_name: None,
        profile_image_asset_id: None,
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
        coding_data_retention_opt_out: false,
        has_echo_agent_code_access: None,
        refresh_token: None,
        expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        oidc_issuer: None,
        oidc_client_id: None,
    }
}
fn test_auth_manager() -> Arc<crate::auth::AuthManager> {
    let dir = tempfile::tempdir().unwrap();
    let mgr = crate::auth::AuthManager::new(dir.path(), crate::auth::EchoAgentComConfig::default());
    mgr.hot_swap(test_auth());
    std::mem::forget(dir);
    Arc::new(mgr)
}
#[tokio::test(flavor = "current_thread")]
async fn fetch_subagent_bundle_success() {
    let body = serde_json::json!({
        "version": "bundle-v1",
        "personas": {"researcher": "persona"},
        "roles": {"reviewer": "role"},
        "agents": {"default": "agent"}
    });
    let (proxy_base_url, seen_headers, server) =
        start_bundle_server(axum::http::StatusCode::OK, body).await;
    let am = test_auth_manager();
    let bundle = fetch_subagent_bundle(&proxy_base_url, Some(&am), None, None)
        .await
        .unwrap();
    assert_eq!(bundle.version, "bundle-v1");
    assert_eq!(
        bundle.personas.get("researcher"),
        Some(&"persona".to_string())
    );
    assert_eq!(bundle.roles.get("reviewer"), Some(&"role".to_string()));
    assert_eq!(bundle.agents.get("default"), Some(&"agent".to_string()));
    let headers = seen_headers.lock().unwrap();
    let headers = headers.last().unwrap();
    assert_eq!(headers.authorization.as_deref(), Some("Bearer token"));
    assert_eq!(headers.token_auth.as_deref(), Some("echo-agent-cli"));
    assert_eq!(headers.user_id.as_deref(), Some("user-1"));
    assert_eq!(headers.email.as_deref(), Some("test@example.com"));
    assert_eq!(headers.alpha_test_key, None);
    assert!(headers.client_version.is_some());
    server.abort();
}
#[tokio::test(flavor = "current_thread")]
async fn fetch_subagent_bundle_uses_deployment_key_without_user_headers() {
    let body = serde_json::json!({
        "version": "bundle-v1",
        "personas": {},
        "roles": {},
        "agents": {}
    });
    let (proxy_base_url, seen_headers, server) =
        start_bundle_server(axum::http::StatusCode::OK, body).await;
    let am = test_auth_manager();
    let bundle = fetch_subagent_bundle(&proxy_base_url, Some(&am), Some("deploy-key"), None)
        .await
        .unwrap();
    assert_eq!(bundle.version, "bundle-v1");
    let headers = seen_headers.lock().unwrap();
    let headers = headers.last().unwrap();
    assert_eq!(headers.authorization.as_deref(), Some("Bearer deploy-key"));
    assert_eq!(headers.token_auth, None);
    assert_eq!(headers.user_id, None);
    assert_eq!(headers.email, None);
    server.abort();
}
#[tokio::test(flavor = "current_thread")]
async fn fetch_subagent_bundle_http_failure() {
    let (proxy_base_url, _seen_headers, server) = start_bundle_server(
        axum::http::StatusCode::UNAUTHORIZED,
        serde_json::json!({"error": "unauthorized"}),
    )
    .await;
    let am = test_auth_manager();
    let error = fetch_subagent_bundle(&proxy_base_url, Some(&am), None, None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        BackendError::RequestFailed { status: 401, .. }
    ));
    server.abort();
}
#[tokio::test(flavor = "current_thread")]
async fn fetch_subagent_bundle_parse_failure() {
    let (proxy_base_url, _seen_headers, server) = start_bundle_server(
        axum::http::StatusCode::OK,
        serde_json::json!({"version": 42}),
    )
    .await;
    let am = test_auth_manager();
    let error = fetch_subagent_bundle(&proxy_base_url, Some(&am), None, None)
        .await
        .unwrap_err();
    assert!(matches!(error, BackendError::Serialization(_)));
    server.abort();
}
#[test]
fn parse_openai_format_uses_id_field() {
    let value = serde_json::json!({
        "id": "echo-agent-3",
        "object": "model",
        "owned_by": "echoagent",
        "context_window": 131072
    });
    let result = parse_remote_model_value(&value, "https://api.echo-agent.invalid/v1").unwrap();
    assert_eq!(result.model, "echo-agent-3");
    assert_eq!(result.base_url, "https://api.echo-agent.invalid/v1");
    assert_eq!(result.name.as_deref(), Some("echo-agent-3"));
}
#[test]
fn parse_model_field_takes_priority_over_id() {
    let value = serde_json::json!({
        "id": "display-key",
        "model": "actual-model-id",
        "name": "Display Name",
        "context_window": 131072
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert_eq!(result.model, "actual-model-id");
    assert_eq!(result.name.as_deref(), Some("Display Name"));
}
#[test]
fn parse_reads_model_family() {
    let value = serde_json::json!({
        "model": "echo-agent-4.5",
        "context_window": 1_000_000,
        "model_family": "echoagent"
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert_eq!(result.model_family.as_deref(), Some("echoagent"));
    let value = serde_json::json!({
        "model": "acme-1",
        "contextWindow": 400_000,
        "modelFamily": "acme"
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert_eq!(result.model_family.as_deref(), Some("acme"));
    let value = serde_json::json!({"model": "x", "context_window": 256_000});
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert!(result.model_family.is_none());
}
#[test]
fn parse_reads_reasoning_effort_fields() {
    use echo_agent_sampling_types::ReasoningEffort;
    let value = serde_json::json!({
        "model": "echo-agent-4.5",
        "context_window": 1_000_000,
        "supports_reasoning_effort": true,
        "reasoning_effort": "high"
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert!(result.supports_reasoning_effort);
    assert_eq!(result.reasoning_effort, Some(ReasoningEffort::High));
    let value = serde_json::json!({
        "model": "echo-agent-4.5",
        "contextWindow": 1_000_000,
        "supportsReasoningEffort": true,
        "reasoningEffort": "xhigh"
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert!(result.supports_reasoning_effort);
    assert_eq!(result.reasoning_effort, Some(ReasoningEffort::Xhigh));
    let value = serde_json::json!({"model": "x", "context_window": 256_000});
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert!(!result.supports_reasoning_effort);
    assert!(result.reasoning_effort.is_none());
}
#[test]
fn parse_reads_reasoning_efforts_list() {
    use echo_agent_sampling_types::ReasoningEffort;
    let value = serde_json::json!({
        "model": "echo-agent-4.5",
        "context_window": 1_000_000,
        "reasoning_efforts": [
            { "id": "deep", "value": "xhigh", "label": "Deep" },
            { "value": "quantum" },
            "low",
        ]
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert_eq!(result.reasoning_efforts.len(), 2);
    assert_eq!(result.reasoning_efforts[0].id, "deep");
    assert_eq!(result.reasoning_efforts[0].value, ReasoningEffort::Xhigh);
    assert_eq!(result.reasoning_efforts[1].value, ReasoningEffort::Low);
    for value in [
        serde_json::json!({
            "model": "m", "context_window": 256_000,
            "reasoningEfforts": [{ "value": "high" }]
        }),
        serde_json::json!({
            "model": "m", "context_window": 256_000,
            "_meta": { "reasoningEfforts": [{ "value": "high" }] }
        }),
    ] {
        let result = parse_remote_model_value(&value, "https://default.url").unwrap();
        assert_eq!(result.reasoning_efforts.len(), 1);
        assert_eq!(result.reasoning_efforts[0].value, ReasoningEffort::High);
    }
    let value = serde_json::json!({"model": "x", "context_window": 256_000});
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert!(result.reasoning_efforts.is_empty());
}
#[test]
fn parse_reads_meta_fallback_fields() {
    let value = serde_json::json!({
        "_meta": {
            "model": "meta-model-id",
            "contextWindow": 131072,
            "agentType": "concise"
        }
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert_eq!(result.model, "meta-model-id");
    assert_eq!(
        result.context_window,
        std::num::NonZeroU64::new(131072).unwrap()
    );
    assert_eq!(result.agent_type, "concise");
}
#[test]
fn parse_remote_model_value_no_laziness_detector_block_yields_default() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert_eq!(
        result.laziness_detector,
        crate::agent::config::LazinessDetectorPerModelConfig::default()
    );
}
#[test]
fn parse_remote_model_value_parses_camelcase_key() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
        "lazinessDetector": {
            "enabled": true,
            "max_nudges_per_session": 2,
            "idle_threshold_ms": 12_000,
            "min_confidence": 0.75,
        },
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    let expected = crate::agent::config::LazinessDetectorPerModelConfig {
        enabled: true,
        max_nudges_per_session: 2,
        idle_threshold_ms: Some(12_000),
        min_confidence: Some(0.75),
        include_reasoning: None,
    };
    assert_eq!(result.laziness_detector, expected);
}
#[test]
fn parse_remote_model_value_parses_snake_case_laziness_detector() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
        "laziness_detector": {
            "enabled": true,
            "max_nudges_per_session": 3,
            "idle_threshold_ms": 8_000,
            "min_confidence": 0.6,
        },
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    let expected = crate::agent::config::LazinessDetectorPerModelConfig {
        enabled: true,
        max_nudges_per_session: 3,
        idle_threshold_ms: Some(8_000),
        min_confidence: Some(0.6),
        include_reasoning: None,
    };
    assert_eq!(result.laziness_detector, expected);
}
#[test]
fn parse_remote_model_value_parses_meta_laziness_detector() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
        "_meta": {
            "lazinessDetector": {
                "enabled": true,
                "max_nudges_per_session": 1,
                "idle_threshold_ms": 15_000,
                "min_confidence": 0.9,
            },
        },
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    let expected = crate::agent::config::LazinessDetectorPerModelConfig {
        enabled: true,
        max_nudges_per_session: 1,
        idle_threshold_ms: Some(15_000),
        min_confidence: Some(0.9),
        include_reasoning: None,
    };
    assert_eq!(result.laziness_detector, expected);
}
#[test]
fn parse_remote_model_value_partial_block_uses_field_defaults() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
        "lazinessDetector": {
            "enabled": true,
        },
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    let expected = crate::agent::config::LazinessDetectorPerModelConfig {
        enabled: true,
        max_nudges_per_session: 0,
        idle_threshold_ms: None,
        min_confidence: None,
        include_reasoning: None,
    };
    assert_eq!(result.laziness_detector, expected);
}
#[test]
fn parse_remote_model_value_malformed_block_falls_back_to_default() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
        "lazinessDetector": {
            "enabled": true,
            "max_nudges_per_session": "abc",
        },
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert_eq!(
        result.laziness_detector,
        crate::agent::config::LazinessDetectorPerModelConfig::default()
    );
}
#[test]
fn parse_remote_model_value_non_object_value_falls_back_to_default() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
        "lazinessDetector": "not-an-object",
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert_eq!(
        result.laziness_detector,
        crate::agent::config::LazinessDetectorPerModelConfig::default()
    );
}
#[test]
fn parse_remote_model_value_top_level_camelcase_wins_over_snake_case() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
        "lazinessDetector": {
            "enabled": true,
            "max_nudges_per_session": 7,
        },
        "laziness_detector": {
            "enabled": false,
            "max_nudges_per_session": 99,
        },
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    let expected = crate::agent::config::LazinessDetectorPerModelConfig {
        enabled: true,
        max_nudges_per_session: 7,
        idle_threshold_ms: None,
        min_confidence: None,
        include_reasoning: None,
    };
    assert_eq!(result.laziness_detector, expected);
}
/// `include_reasoning: false` parses cleanly under the per-model
/// `lazinessDetector` block (camelCase wrapper, snake_case inner —
/// matching the existing field-naming convention used for the
/// sibling `min_confidence`, `idle_threshold_ms`, etc.).
#[test]
fn parse_remote_model_value_parses_include_reasoning_under_camelcase_wrapper() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
        "lazinessDetector": {
            "enabled": true,
            "include_reasoning": false,
        },
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert_eq!(result.laziness_detector.include_reasoning, Some(false));
}
#[test]
fn parse_remote_model_value_parses_include_reasoning_under_snake_case_wrapper() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
        "laziness_detector": {
            "enabled": true,
            "include_reasoning": true,
        },
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert_eq!(result.laziness_detector.include_reasoning, Some(true));
}
#[test]
fn parse_remote_model_value_omitted_include_reasoning_defaults_to_none() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
        "lazinessDetector": {
            "enabled": true,
            "max_nudges_per_session": 2,
        },
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert_eq!(
        result.laziness_detector.include_reasoning, None,
        "absent include_reasoning defers to harness default via None",
    );
}
#[test]
fn parse_remote_model_value_top_level_wins_over_meta() {
    let value = serde_json::json!({
        "model": "echo-agent-4",
        "context_window": 256_000,
        "lazinessDetector": {
            "enabled": true,
            "max_nudges_per_session": 5,
        },
        "_meta": {
            "lazinessDetector": {
                "enabled": false,
                "max_nudges_per_session": 99,
            },
        },
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    let expected = crate::agent::config::LazinessDetectorPerModelConfig {
        enabled: true,
        max_nudges_per_session: 5,
        idle_threshold_ms: None,
        min_confidence: None,
        include_reasoning: None,
    };
    assert_eq!(result.laziness_detector, expected);
}
#[test]
fn parse_reads_show_model_fingerprint_field() {
    let value = serde_json::json!({
        "model": "echo-agent-build",
        "context_window": 256_000,
        "show_model_fingerprint": true
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert!(result.show_model_fingerprint);
    let value = serde_json::json!({
        "model": "echo-agent-build",
        "contextWindow": 256_000,
        "showModelFingerprint": true
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert!(result.show_model_fingerprint);
    let value = serde_json::json!({
        "model": "echo-agent-build",
        "context_window": 256_000,
        "_meta": { "showModelFingerprint": true }
    });
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert!(result.show_model_fingerprint);
    let value = serde_json::json!({"model": "x", "context_window": 256_000});
    let result = parse_remote_model_value(&value, "https://default.url").unwrap();
    assert!(!result.show_model_fingerprint);
}
#[test]
fn get_object_returns_none_for_non_object_values() {
    let value = serde_json::json!({
        "string": "hello",
        "number": 42,
        "bool": true,
        "array": [1, 2, 3],
        "null": null,
    });
    let obj = value.as_object().unwrap();
    assert!(get_object(obj, "string").is_none());
    assert!(get_object(obj, "number").is_none());
    assert!(get_object(obj, "bool").is_none());
    assert!(get_object(obj, "array").is_none());
    assert!(get_object(obj, "null").is_none());
    assert!(get_object(obj, "missing").is_none());
}
#[test]
fn get_object_returns_some_for_actual_object() {
    let value = serde_json::json!({
        "nested": { "a": 1, "b": "two" },
    });
    let obj = value.as_object().unwrap();
    let nested = get_object(obj, "nested").expect("nested key should resolve to object");
    assert!(nested.is_object());
    assert_eq!(nested["a"], serde_json::json!(1));
    assert_eq!(nested["b"], serde_json::json!("two"));
}
fn endpoints(
    proxy: &str,
    models_base_url: Option<&str>,
    models_list_url: Option<&str>,
) -> crate::agent::config::EndpointsConfig {
    crate::agent::config::EndpointsConfig {
        cli_chat_proxy_base_url: Some(proxy.to_owned()),
        models_base_url: models_base_url.map(|s| s.to_owned()),
        models_list_url: models_list_url.map(|s| s.to_owned()),
        ..Default::default()
    }
}
#[test]
fn inference_url_defaults_to_proxy() {
    let ep = endpoints("https://proxy.echo-agent.invalid/v1", None, None);
    assert_eq!(
        ep.resolve_inference_base_url(),
        "https://proxy.echo-agent.invalid/v1"
    );
}
#[test]
fn inference_url_uses_models_base_url() {
    let ep = endpoints(
        "https://proxy.echo-agent.invalid/v1",
        Some("https://enterprise.acme.com/v1"),
        None,
    );
    assert_eq!(
        ep.resolve_inference_base_url(),
        "https://enterprise.acme.com/v1"
    );
}
#[test]
fn inference_url_base_url_wins_over_proxy() {
    let ep = endpoints(
        "https://proxy.echo-agent.invalid/v1",
        Some("https://inference.acme.com/v1"),
        Some("https://registry.acme.com/api/models"),
    );
    assert_eq!(
        ep.resolve_inference_base_url(),
        "https://inference.acme.com/v1"
    );
}
#[test]
fn list_url_defaults_to_proxy_models() {
    let ep = endpoints("https://proxy.echo-agent.invalid/v1", None, None);
    assert_eq!(
        ep.resolve_models_list_url(),
        "https://proxy.echo-agent.invalid/v1/models"
    );
}
#[test]
fn list_url_derived_from_base_url() {
    let ep = endpoints(
        "https://proxy.echo-agent.invalid/v1",
        Some("https://api.echo-agent.invalid/v1"),
        None,
    );
    assert_eq!(
        ep.resolve_models_list_url(),
        "https://api.echo-agent.invalid/v1/models"
    );
}
#[test]
fn list_url_explicit_overrides_derivation() {
    let ep = endpoints(
        "https://proxy.echo-agent.invalid/v1",
        Some("https://inference.acme.com/v1"),
        Some("https://registry.acme.com/api/list-models"),
    );
    assert_eq!(
        ep.resolve_models_list_url(),
        "https://registry.acme.com/api/list-models"
    );
}
/// INVARIANT: the `/models` fetch URL + auth scheme match the auth mode —
/// Session/Deployment → cli-chat-proxy (Session auth), never the inference host;
/// ApiKey → `echo_agent_api_base_url` (ApiKey, public default when unset); a custom
/// models endpoint → that URL verbatim.
#[test]
#[serial_test::serial]
fn models_fetch_endpoint_matches_auth_mode() {
    use crate::agent::config::EndpointsConfig;
    use crate::agent::models::ModelFetchAuth;
    for k in [
        "ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL",
        "ECHO_AGENT_API_BASE_URL",
        "ECHO_AGENT_MODELS_LIST_URL",
    ] {
        unsafe { std::env::remove_var(k) };
    }
    let cfg = EndpointsConfig::from_config_value(
        &toml::from_str(
            r#"[endpoints]
                echo_agent_api_base_url = "https://inference.acme-corp.example/echoagent/v1""#,
        )
        .unwrap(),
    );
    let session = ListModelsEndpoint::from_endpoints(&cfg, ModelFetchAuth::Session);
    assert_eq!(
        session.url,
        "https://chat-proxy.echo-agent.invalid/v1/models"
    );
    assert_eq!(session.auth, EndpointAuth::Session);
    let deployment = ListModelsEndpoint::from_endpoints(&cfg, ModelFetchAuth::Deployment);
    assert_eq!(
        deployment.url,
        "https://chat-proxy.echo-agent.invalid/v1/models"
    );
    assert_eq!(deployment.auth, EndpointAuth::Session);
    let api = ListModelsEndpoint::from_endpoints(&cfg, ModelFetchAuth::ApiKey);
    assert_eq!(
        api.url,
        "https://inference.acme-corp.example/echoagent/v1/models"
    );
    assert_eq!(api.auth, EndpointAuth::ApiKey);
    let default = EndpointsConfig::from_config_value(&toml::Value::Table(Default::default()));
    assert_eq!(
        ListModelsEndpoint::from_endpoints(&default, ModelFetchAuth::ApiKey).url,
        "https://api.echo-agent.invalid/v1/models"
    );
    let custom = EndpointsConfig::from_config_value(
        &toml::from_str(
            r#"[endpoints]
                models_base_url = "https://models.acme.com/v1""#,
        )
        .unwrap(),
    );
    let ep = ListModelsEndpoint::from_endpoints(&custom, ModelFetchAuth::Session);
    assert_eq!(ep.url, "https://models.acme.com/v1/models");
    assert_eq!(ep.auth, EndpointAuth::ApiKey);
}
/// REGRESSION: `echoagent setup` must send the deployment key to
/// the proxy, never the inference endpoint.
#[test]
#[serial_test::serial]
fn deployment_config_url_uses_cli_chat_proxy_when_not_overridden() {
    use crate::agent::config::EndpointsConfig;
    for k in [
        "ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL",
        "ECHO_AGENT_MANAGED_CONFIG_URL",
        "ECHO_AGENT_API_BASE_URL",
    ] {
        unsafe { std::env::remove_var(k) };
    }
    unsafe { std::env::set_var("ECHO_AGENT_DEPLOYMENT_KEY", "echo-agent-token-ENTERPRISE") };
    let managed: toml::Value = toml::from_str(
        r#"[endpoints]
            deployment_key = "echo-agent-token-ENTERPRISE"
            echo_agent_api_base_url = "https://inference.acme-corp.example/echoagent/v1""#,
    )
    .unwrap();
    let url = EndpointsConfig::from_config_value(&managed).resolve_managed_config_url();
    assert_eq!(
        url,
        "https://chat-proxy.echo-agent.invalid/v1/deployment/config"
    );
    assert!(
        !url.contains("acme-corp"),
        "deployment key would be sent to the inference host: {url}"
    );
    let pinned: toml::Value = toml::from_str(
        r#"[endpoints]
            echo_agent_api_base_url = "https://inference.acme-corp.example/echoagent/v1"
            cli_chat_proxy_base_url = "https://proxy.acme-corp.example/v1""#,
    )
    .unwrap();
    assert_eq!(
        EndpointsConfig::from_config_value(&pinned).resolve_managed_config_url(),
        "https://proxy.acme-corp.example/v1/deployment/config"
    );
    unsafe { std::env::remove_var("ECHO_AGENT_DEPLOYMENT_KEY") };
}
#[derive(Clone)]
struct DualBundleServerState {
    archive_status: StatusCode,
    archive_bytes: Vec<u8>,
    legacy_status: StatusCode,
    legacy_body: serde_json::Value,
}
async fn start_dual_bundle_server(
    state: DualBundleServerState,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let app = Router::new()
        .route(
            "/v1/bundle/archive",
            get(|State(state): State<DualBundleServerState>| async move {
                (state.archive_status, state.archive_bytes)
            }),
        )
        .route(
            "/v1/subagents/bundle",
            get(|State(state): State<DualBundleServerState>| async move {
                (state.legacy_status, axum::Json(state.legacy_body))
            }),
        )
        .with_state(state);
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("{base}/v1"), handle)
}
#[tokio::test(flavor = "current_thread")]
async fn fetch_bundle_returns_archive_on_success() {
    let archive_bytes = b"fake-tar-gz-bytes".to_vec();
    let (proxy_base_url, server) = start_dual_bundle_server(DualBundleServerState {
        archive_status: StatusCode::OK,
        archive_bytes: archive_bytes.clone(),
        legacy_status: StatusCode::OK,
        legacy_body: serde_json::json!({
            "version": "v1", "personas": {}, "roles": {}, "agents": {}
        }),
    })
    .await;
    let am = test_auth_manager();
    let result = fetch_bundle(&proxy_base_url, Some(&am), None, None)
        .await
        .unwrap();
    match result {
        FetchedBundle::Archive(bytes) => assert_eq!(bytes, archive_bytes),
        FetchedBundle::Legacy(_) => panic!("expected Archive variant"),
    }
    server.abort();
}
#[tokio::test(flavor = "current_thread")]
async fn fetch_bundle_falls_back_on_archive_404() {
    let (proxy_base_url, server) = start_dual_bundle_server(DualBundleServerState {
        archive_status: StatusCode::NOT_FOUND,
        archive_bytes: Vec::new(),
        legacy_status: StatusCode::OK,
        legacy_body: serde_json::json!({
            "version": "v1",
            "personas": {"r": "p"},
            "roles": {},
            "agents": {}
        }),
    })
    .await;
    let am = test_auth_manager();
    let result = fetch_bundle(&proxy_base_url, Some(&am), None, None)
        .await
        .unwrap();
    match result {
        FetchedBundle::Legacy(bundle) => {
            assert_eq!(bundle.version, "v1");
            assert_eq!(bundle.personas.get("r"), Some(&"p".to_string()));
        }
        FetchedBundle::Archive(_) => panic!("expected Legacy variant"),
    }
    server.abort();
}
#[tokio::test(flavor = "current_thread")]
async fn fetch_bundle_falls_back_on_archive_503() {
    let (proxy_base_url, server) = start_dual_bundle_server(DualBundleServerState {
        archive_status: StatusCode::SERVICE_UNAVAILABLE,
        archive_bytes: Vec::new(),
        legacy_status: StatusCode::OK,
        legacy_body: serde_json::json!({
            "version": "v1", "personas": {}, "roles": {}, "agents": {}
        }),
    })
    .await;
    let am = test_auth_manager();
    let result = fetch_bundle(&proxy_base_url, Some(&am), None, None)
        .await
        .unwrap();
    match &result {
        FetchedBundle::Legacy(bundle) => assert_eq!(bundle.version, "v1"),
        FetchedBundle::Archive(_) => panic!("expected Legacy variant"),
    }
    server.abort();
}
/// `BackendClient::save_session_data` resolves auth from the attached
/// `AuthManager` and sends the token as `Bearer <key>` on the wire.
/// This is the writeback path used on every session flush.
#[tokio::test(flavor = "current_thread")]
async fn backend_client_resolves_auth_from_auth_manager() {
    let captured_auth = Arc::new(Mutex::new(None::<String>));
    let captured = captured_auth.clone();
    let app = Router::new().route(
        "/sessions/{id}/data",
        axum::routing::post(move |headers: HeaderMap| async move {
            *captured.lock().unwrap() = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            StatusCode::OK
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let am = test_auth_manager();
    let client = BackendClient::with_base_url(format!("http://{addr}")).with_auth_manager(am);
    client
        .save_session_data("test-session", &[], None)
        .await
        .unwrap();
    let sent = captured_auth
        .lock()
        .unwrap()
        .clone()
        .expect("server must receive Authorization header");
    assert_eq!(sent, "Bearer token", "must use token from AuthManager");
    server.abort();
}
#[tokio::test(flavor = "current_thread")]
async fn fetch_bundle_propagates_legacy_error_after_fallback() {
    let (proxy_base_url, server) = start_dual_bundle_server(DualBundleServerState {
        archive_status: StatusCode::NOT_FOUND,
        archive_bytes: Vec::new(),
        legacy_status: StatusCode::UNAUTHORIZED,
        legacy_body: serde_json::json!({"error": "unauthorized"}),
    })
    .await;
    let am = test_auth_manager();
    let error = fetch_bundle(&proxy_base_url, Some(&am), None, None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        BackendError::RequestFailed { status: 401, .. }
    ));
    server.abort();
}
/// Regression: reqwest .header() appends — duplicate
/// or overlapping headers cause Cloudflare to reject the request.
#[tokio::test(flavor = "current_thread")]
#[allow(clippy::disallowed_methods)]
async fn auth_headers_do_not_collide_with_json() {
    let client =
        BackendClient::with_base_url("http://localhost").with_auth_manager(test_auth_manager());
    let auth_headers = client.auth_header_map().await.unwrap();
    assert!(
        !auth_headers.contains_key("content-type"),
        "content-type in auth map would overwrite .json()"
    );
    let request = reqwest::Client::new()
        .put("http://localhost/sessions/test")
        .json(&serde_json::json!({"test": true}))
        .headers(auth_headers)
        .build()
        .unwrap();
    for name in request.headers().keys() {
        let count = request.headers().get_all(name).iter().count();
        assert_eq!(count, 1, "duplicate header {name}");
    }
}
