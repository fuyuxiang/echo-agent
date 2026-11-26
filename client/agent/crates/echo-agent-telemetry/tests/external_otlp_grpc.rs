//! gRPC transport coverage for the external OTEL stream. This mirrors the
//! primary HTTP/protobuf wire test in `external_otlp.rs`, but must live in its
//! own integration-test binary because the external telemetry registry is a
//! process-global `OnceLock`.

mod otlp_collector;

use otlp_collector as col;

const CANARY_MODEL: &str = "sk-CANARYgrpcabcdefghij1234567890";
const CANARY_PROMPT: &str = "CANARY_GRPC_PROMPT_TEXT do not export";
const CANARY_MCP: &str = "canary-grpc-internal-mcp-server";

#[test]
fn external_stream_grpc_end_to_end() {
    let collected = col::Collected::default();
    let endpoint =
        col::start_collector_with_protocol(collected.clone(), col::CollectorProtocol::Grpc);

    let mut cfg = echo_agent_telemetry::external::ExternalOtelConfig::resolve_with(
        |name| match name {
            "ECHO_AGENT_EXTERNAL_OTEL" => Some("1".into()),
            "OTEL_LOGS_EXPORTER" | "OTEL_METRICS_EXPORTER" => Some("otlp".into()),
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some(endpoint.clone()),
            "OTEL_EXPORTER_OTLP_PROTOCOL" => Some("grpc".into()),
            "OTEL_METRIC_EXPORT_INTERVAL" => Some("200".into()),
            "OTEL_BLRP_SCHEDULE_DELAY" => Some("100".into()),
            _ => None,
        },
        None,
    )
    .expect("double opt-in must resolve");
    cfg.client = echo_agent_telemetry::external::config::ExternalClientInfo {
        service_version: "0.0.0-test".into(),
        client_version: "0.0.0-test".into(),
        app_entrypoint: "cli".into(),
    };

    echo_agent_telemetry::external::init(Some(cfg));
    assert!(echo_agent_telemetry::external::is_active());

    echo_agent_telemetry::log_event(echo_agent_telemetry::events::SessionNew {
        session_id: "sess-grpc-1".into(),
        client_identifier: None,
        client_version: None,
        is_git_repo: true,
        permission_mode: echo_agent_telemetry::enums::PermissionMode::Ask,
    });
    echo_agent_telemetry::log_event(echo_agent_telemetry::events::SessionHarness {
        session_id: "sess-grpc-1".into(),
        client_identifier: Some("echo-agent-pager".into()),
        model_id: "echo-agent-4".into(),
        agent_name: "echo-agent-build-plan".into(),
        permission_mode: echo_agent_telemetry::enums::PermissionMode::Ask,
        mcp_server_names: vec![CANARY_MCP.into()],
        plugin_names: vec![],
        skill_names: vec![],
        lsp_server_names: vec![],
        hook_names: vec![],
        agents_md_dir_names: vec![],
        memory_enabled: false,
        memory_retrieval_mode: echo_agent_telemetry::events::MemoryRetrievalMode::Disabled,
        is_git_repo: true,
        auto_update: None,
    });
    echo_agent_telemetry::log_event(echo_agent_telemetry::events::PromptSubmitted {
        prompt_length: CANARY_PROMPT.len(),
        model_id: "echo-agent-4".into(),
        client_identifier: None,
        screen_mode: None,
        prompt_text: Some(CANARY_PROMPT.into()),
    });
    echo_agent_telemetry::log_event(echo_agent_telemetry::events::ModelResponseReceived {
        model_id: CANARY_MODEL.into(),
        duration_ms: 5,
        stop_reason: Some("stop".into()),
        prompt_tokens: Some(11),
        completion_tokens: Some(7),
        reasoning_tokens: None,
        cached_prompt_tokens: None,
    });

    echo_agent_telemetry::external::flush();
    assert!(
        col::wait_until(std::time::Duration::from_secs(10), || {
            collected.logs_len() > 0 && collected.metrics_len() > 0
        }),
        "gRPC collector must receive both signals"
    );

    let event_names = col::event_names(&collected);
    for expected in [
        "echo_agent_code.session_start",
        "echo_agent_code.user_prompt",
        "echo_agent_code.api_request",
    ] {
        assert!(
            event_names.iter().any(|n| n == expected),
            "missing {expected} in {event_names:?}"
        );
    }

    let metrics = col::metric_points(&collected);
    assert!(
        metrics
            .iter()
            .any(|p| p.name == "echo_agent_code.session.count"),
        "missing session.count in {metrics:?}"
    );
    assert!(
        metrics
            .iter()
            .any(|p| p.name == "echo_agent_code.token.usage"),
        "missing token.usage in {metrics:?}"
    );
    for point in metrics {
        assert_eq!(
            point.temporality,
            col::TEMPORALITY_DELTA,
            "default temporality must be Delta over gRPC"
        );
    }

    let raw = collected.raw_text();
    assert!(!raw.contains("CANARY"), "canary reached the gRPC wire");
    assert!(
        !raw.contains(CANARY_MCP),
        "MCP server name reached the gRPC wire"
    );

    echo_agent_telemetry::external::shutdown();
}
