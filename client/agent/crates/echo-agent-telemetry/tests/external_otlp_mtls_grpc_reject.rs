mod otlp_collector;

use otlp_collector as col;

#[test]
fn external_stream_grpc_mtls_fails_without_client_identity() {
    col::init_test_tracing();

    let tls = col::generate_tls_material();
    let ca_file = tempfile::NamedTempFile::new().expect("CA temp file");
    std::fs::write(ca_file.path(), &tls.ca_cert_pem).expect("write CA pem");
    let ca_path = ca_file.path().to_str().expect("utf-8 CA path").to_string();

    let collected = col::Collected::default();
    let endpoint = col::start_grpc_mtls_collector(
        collected.clone(),
        tls.server_cert_pem.clone(),
        tls.server_key_pem.clone(),
        tls.ca_cert_pem.clone(),
    );

    let mut cfg = echo_agent_telemetry::external::ExternalOtelConfig::resolve_with(
        |name| match name {
            "ECHO_AGENT_EXTERNAL_OTEL" => Some("1".into()),
            "OTEL_LOGS_EXPORTER" | "OTEL_METRICS_EXPORTER" => Some("otlp".into()),
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some(endpoint.clone()),
            "OTEL_EXPORTER_OTLP_PROTOCOL" => Some("grpc".into()),
            "OTEL_EXPORTER_OTLP_CERTIFICATE" => Some(ca_path.clone()),
            "OTEL_METRIC_EXPORT_INTERVAL" => Some("200".into()),
            "OTEL_BLRP_SCHEDULE_DELAY" => Some("100".into()),
            _ => None,
        },
        None,
    )
    .expect("config must resolve without client identity");
    assert!(cfg.logs_client_certificate.is_none());
    cfg.client = echo_agent_telemetry::external::config::ExternalClientInfo {
        service_version: "0.0.0-test".into(),
        client_version: "0.0.0-test".into(),
        app_entrypoint: "cli".into(),
    };

    echo_agent_telemetry::external::init(Some(cfg));
    assert!(
        echo_agent_telemetry::external::is_active(),
        "stream must build and activate so zero collector records mean rejection, \
         not a construction failure"
    );

    echo_agent_telemetry::log_event(echo_agent_telemetry::events::SessionHarness {
        session_id: "sess-grpc-mtls-no-client".into(),
        client_identifier: Some("echo-agent-pager".into()),
        model_id: "echo-agent-4".into(),
        agent_name: "echo-agent-build-plan".into(),
        permission_mode: echo_agent_telemetry::enums::PermissionMode::Ask,
        mcp_server_names: vec![],
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
    echo_agent_telemetry::external::flush();

    std::thread::sleep(std::time::Duration::from_millis(800));
    let health = echo_agent_telemetry::external::export_health()
        .expect("active stream must expose export health");
    assert!(
        health.export_failures > 0,
        "mTLS rejection must record at least one export failure; health={health:?}"
    );
    assert_eq!(
        collected.logs_len(),
        0,
        "mTLS-required collector must reject clients without identity"
    );

    echo_agent_telemetry::external::shutdown();
}
