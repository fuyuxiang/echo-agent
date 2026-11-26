use axum::{Json, Router, extract::State, routing::post};
use echo_agent_tools::computer::local::{LocalFs, LocalTerminalBackend};
use echo_agent_tools::computer::types::{AsyncFileSystem, TerminalBackend};
use echo_agent_tools::notification::ToolNotificationHandle;
use echo_agent_tools::registry::types::{SessionContext, ToolConfig, ToolServerConfig};
use serde_json::{Value, json};

#[tokio::test]
async fn web_search_uses_model_override_from_config_end_to_end() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    async fn handle_request(
        State(tx): State<tokio::sync::mpsc::UnboundedSender<Value>>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let _ = tx.send(body);
        Json(json!({
            "id": "resp_test",
            "object": "response",
            "created_at": 1234567890,
            "status": "completed",
            "model": "enterprise-search",
            "output": [{
                "type": "message",
                "id": "msg_1",
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "search result",
                    "annotations": []
                }]
            }]
        }))
    }
    let app = Router::new()
        .route("/responses", post(handle_request))
        .with_state(tx);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let raw_config: toml::Value = toml::from_str(&format!(
        r#"
            [models]
            web_search = "enterprise-search"

            [model.enterprise-search]
            model = "enterprise-search"
            base_url = "http://{addr}"
            api_key = "enterprise-key"
            context_window = 256000
            api_backend = "responses"
            "#,
    ))
    .unwrap();
    let web_search_model =
        crate::config::ModelOverrideConfig::resolve(None, None, &raw_config, None).web_search;
    let agent_cfg = crate::agent::config::Config::new_from_toml_cfg(&raw_config).unwrap();
    let models = crate::agent::config::resolve_model_list(&agent_cfg, None);
    let entry = models.get(web_search_model.as_str()).unwrap();
    let resolved = crate::agent::config::sampling_config_for_model(
        entry,
        crate::agent::config::resolve_credentials(entry, None),
        None,
        None,
        None,
        None,
    );
    let web_search_sampling = crate::tools::config::web_search_sampling_config(resolved);

    let builder = crate::tools::bridge::ToolBridge::get_builder();
    let config = ToolServerConfig {
        tools: vec![ToolConfig {
            id: "EchoAgentBuild:web_search".into(),
            params: None,
            name_override: None,
            params_name_overrides: None,
            description_override: None,
            behavior_version: None,
            kind: None,
        }],
        behavior_preset: None,
    };
    let fs: std::sync::Arc<dyn AsyncFileSystem> = std::sync::Arc::new(LocalFs);
    let terminal: std::sync::Arc<dyn TerminalBackend> =
        std::sync::Arc::new(LocalTerminalBackend::new());
    let ctx = SessionContext {
        backend: terminal,
        fs,
        cwd: std::env::temp_dir(),
        session_folder: std::env::temp_dir().join("echo-agent-web-search-e2e"),
        session_env: std::sync::Arc::new(std::collections::HashMap::new()),
        notification_handle: ToolNotificationHandle::noop(),
        owner_session_id: None,
        subagent: None,
        parent_scheduler_handle: None,
        skills: vec![],
        state_path: std::env::temp_dir().join("echo-agent-web-search-e2e/state.json"),
        memory_backend: None,
        web_search_config:
            echo_agent_tools::implementations::web_search::WebSearchConfig::Enabled {
                api_key: web_search_sampling.api_key.clone().unwrap(),
                base_url: web_search_sampling.base_url.clone(),
                model: web_search_sampling.model.clone(),
                extra_headers: web_search_sampling.extra_headers.clone(),
                // The optional extra access key is no longer carried on
                // `SamplerConfig`. The shell-level value flows in via
                // `Credentials` at session-spawn time; in this self-contained
                // test fixture there's no extra access key in scope.
                alpha_test_key: None,
                allowed_domains: None,
                excluded_domains: None,
            },
        web_fetch_config: Default::default(),
        lsp: None,
        image_gen_config: Default::default(),
        video_gen_config: Default::default(),
        app_builder_deployer_config: Default::default(),
        api_key_provider: None,
        auth_provider: None,
        attribution_callback: None,
        system_reminder_tag: echo_agent_tools::reminders::DEFAULT_REMINDER_TAG,
    };
    let bridge = crate::tools::bridge::ToolBridge::finalize_builder(builder, config, ctx)
        .await
        .expect("finalize_builder should succeed");
    let result = bridge
        .call(
            "web_search",
            json!({
                "query": "test query",
                "allowed_domains": ["example.com"]
            }),
            "web-search-e2e",
        )
        .await;
    assert!(
        result.is_ok(),
        "web_search should succeed: {:?}",
        result.err()
    );

    let request = rx.recv().await.expect("mock server should receive request");
    assert_eq!(
        request.get("model").and_then(|v| v.as_str()),
        Some(web_search_model.as_str())
    );

    server.abort();
}

#[tokio::test]
async fn web_search_errors_when_configured_model_cannot_be_resolved() {
    let builder = crate::tools::bridge::ToolBridge::get_builder();
    let config = ToolServerConfig {
        tools: vec![ToolConfig {
            id: "EchoAgentBuild:web_search".into(),
            params: None,
            name_override: None,
            params_name_overrides: None,
            description_override: None,
            behavior_version: None,
            kind: None,
        }],
        behavior_preset: None,
    };
    let fs: std::sync::Arc<dyn AsyncFileSystem> = std::sync::Arc::new(LocalFs);
    let terminal: std::sync::Arc<dyn TerminalBackend> =
        std::sync::Arc::new(LocalTerminalBackend::new());
    let ctx = SessionContext {
        backend: terminal,
        fs,
        cwd: std::env::temp_dir(),
        session_folder: std::env::temp_dir().join("echo-agent-web-search-disabled"),
        session_env: std::sync::Arc::new(std::collections::HashMap::new()),
        notification_handle: ToolNotificationHandle::noop(),
        owner_session_id: None,
        subagent: None,
        parent_scheduler_handle: None,
        skills: vec![],
        state_path: std::env::temp_dir().join("echo-agent-web-search-disabled/state.json"),
        memory_backend: None,
        web_search_config: echo_agent_tools::implementations::web_search::WebSearchConfig::Disabled,
        web_fetch_config: Default::default(),
        lsp: None,
        image_gen_config: Default::default(),
        video_gen_config: Default::default(),
        app_builder_deployer_config: Default::default(),
        api_key_provider: None,
        auth_provider: None,
        attribution_callback: None,
        system_reminder_tag: echo_agent_tools::reminders::DEFAULT_REMINDER_TAG,
    };
    let bridge = crate::tools::bridge::ToolBridge::finalize_builder(builder, config, ctx)
        .await
        .expect("finalize_builder should succeed");
    let result = bridge
        .call(
            "web_search",
            json!({
                "query": "test query"
            }),
            "web-search-disabled",
        )
        .await;
    assert!(result.is_err(), "web_search should fail when disabled");
}
