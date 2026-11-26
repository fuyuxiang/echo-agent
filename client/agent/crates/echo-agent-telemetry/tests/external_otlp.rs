//! Integration test for the external OTEL stream against an in-process OTLP
//! collector: wire payloads, delta temporality, gates-off canary absence at
//! the wire layer, flush-on-shutdown ≤ 2 s, and post-shutdown silence.

mod otlp_collector;

use otlp_collector as col;

const CANARY_MODEL: &str = "sk-CANARYabcdefghij1234567890";
const CANARY_PROMPT: &str = "CANARY_PROMPT_TEXT do not export";
const CANARY_MCP: &str = "canary-internal-mcp-server";

#[test]
fn external_stream_end_to_end() {
    let collected = col::Collected::default();
    let endpoint = col::start_collector(collected.clone());

    // Resolve through the real config path (double opt-in, gates off).
    let mut cfg = echo_agent_telemetry::external::ExternalOtelConfig::resolve_with(
        |name| match name {
            "ECHO_AGENT_EXTERNAL_OTEL" => Some("1".into()),
            "OTEL_LOGS_EXPORTER" | "OTEL_METRICS_EXPORTER" => Some("otlp".into()),
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some(endpoint.clone()),
            // Keep intervals short so the test is fast; flush() forces anyway.
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

    // Emit through the same funnel production uses — with the product events client
    // never initialized (TelemetryMode effectively Disabled) and no auth at
    // all, pinning the Disabled half of the G7 independence matrix at the
    // funnel level: the external sink fires anyway.
    assert!(!echo_agent_telemetry::is_enabled());
    echo_agent_telemetry::log_event(echo_agent_telemetry::events::SessionNew {
        session_id: "sess-int-1".into(),
        client_identifier: None,
        client_version: None,
        is_git_repo: true,
        permission_mode: echo_agent_telemetry::enums::PermissionMode::Ask,
    });
    echo_agent_telemetry::log_event(echo_agent_telemetry::events::SessionHarness {
        session_id: "sess-int-1".into(),
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
    // Model-id canary for the metrics body (increment-time scrub).
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
        "collector must receive both signals"
    );

    // ── Logs payload ────────────────────────────────────────────────────
    let logs = col::decode_logs(&collected);
    let mut event_names: Vec<String> = vec![];
    let mut resource_service_name = None;
    for req in &logs {
        for rl in &req.resource_logs {
            if let Some(resource) = &rl.resource {
                for kv in &resource.attributes {
                    if kv.key == "service.name"
                        && let Some(v) = &kv.value
                    {
                        resource_service_name = Some(format!("{v:?}"));
                    }
                }
            }
            for sl in &rl.scope_logs {
                assert_eq!(
                    sl.scope.as_ref().map(|s| s.name.as_str()),
                    Some("ai.echoagent.echo_agent_code")
                );
                for record in &sl.log_records {
                    event_names.push(record.event_name.clone());
                }
            }
        }
    }
    assert!(
        resource_service_name
            .as_deref()
            .is_some_and(|s| s.contains("echo-agent-cli")),
        "service.name=echo-agent-cli is a wire commitment: {resource_service_name:?}"
    );
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
    // session_start arrives exactly once per emission (no double-send from
    // the funnel).
    assert_eq!(
        event_names
            .iter()
            .filter(|n| *n == "echo_agent_code.session_start")
            .count(),
        1
    );

    // ── Metrics payload: names + Delta temporality + session.count == 1 ──
    let metrics = col::decode_metrics(&collected);
    let mut metric_names = vec![];
    let mut session_count_total = 0u64;
    for req in &metrics {
        for rm in &req.resource_metrics {
            for sm in &rm.scope_metrics {
                for metric in &sm.metrics {
                    metric_names.push(metric.name.clone());
                    use opentelemetry_proto::tonic::metrics::v1::metric::Data;
                    if let Some(Data::Sum(sum)) = &metric.data {
                        assert_eq!(
                            sum.aggregation_temporality,
                            opentelemetry_proto::tonic::metrics::v1::AggregationTemporality::Delta
                                as i32,
                            "default temporality must be Delta (CC parity)"
                        );
                        if metric.name == "echo_agent_code.session.count" {
                            for dp in &sum.data_points {
                                if let Some(
                                    opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(v),
                                ) = dp.value
                                {
                                    session_count_total += v as u64;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        metric_names
            .iter()
            .any(|n| n == "echo_agent_code.session.count"),
        "missing session.count in {metric_names:?}"
    );
    assert!(
        metric_names
            .iter()
            .any(|n| n == "echo_agent_code.token.usage")
    );
    assert_eq!(
        session_count_total, 1,
        "session.count must increment exactly once per SessionNew"
    );

    // ── Canary absence at the HTTP layer (raw bytes, both signals) ──────
    let raw_logs = collected.raw_logs();
    let raw_metrics = collected.raw_metrics();
    for (label, raw) in [("logs", &raw_logs), ("metrics", &raw_metrics)] {
        let haystack = String::from_utf8_lossy(raw);
        assert!(
            !haystack.contains("CANARY"),
            "canary reached the {label} wire: gates are off / scrub failed"
        );
        assert!(
            !haystack.contains(CANARY_MCP),
            "MCP server name reached the {label} wire"
        );
    }
    // Prompt length exported, text not (already covered by the canary scan).

    // ── Shutdown: ≤ 2 s + post-shutdown silence ─────────────────────────
    let start = std::time::Instant::now();
    echo_agent_telemetry::external::shutdown();
    assert!(
        start.elapsed() <= std::time::Duration::from_millis(2500),
        "shutdown watchdog must bound exit at ~2s (took {:?})",
        start.elapsed()
    );
    assert!(!echo_agent_telemetry::external::is_active());

    let logs_before = collected.logs_len();
    echo_agent_telemetry::log_event(echo_agent_telemetry::events::PromptSubmitted {
        prompt_length: 1,
        model_id: "echo-agent-4".into(),
        client_identifier: None,
        screen_mode: None,
        prompt_text: None,
    });
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(
        collected.logs_len(),
        logs_before,
        "no exports after shutdown"
    );

    // Idempotent shutdown: second call is a no-op, not an error/panic.
    echo_agent_telemetry::external::shutdown();
}
