mod otlp_collector;

use otlp_collector as col;

fn write_temp(contents: &str) -> (tempfile::NamedTempFile, String) {
    let file = tempfile::NamedTempFile::new().expect("temp file");
    std::fs::write(file.path(), contents).expect("write pem");
    let path = file.path().to_str().expect("utf-8 path").to_string();
    (file, path)
}

#[test]
fn external_stream_http_mtls_end_to_end() {
    col::init_test_tracing();

    // Pin aws-lc (same provider as the workspace `rustls` / gRPC mTLS tests).
    // The test binary links ring (reqwest) + aws-lc (tonic/rustls workspace),
    // so rustls will not auto-select a process default.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let tls = col::generate_tls_material();
    let (_ca_file, ca_path) = write_temp(&tls.ca_cert_pem);
    let (_cert_file, cert_path) = write_temp(&tls.client_cert_pem);
    let (_key_file, key_path) = write_temp(&tls.client_key_pem);

    let collected = col::Collected::default();
    let endpoint = col::start_http_mtls_collector(
        collected.clone(),
        tls.server_cert_pem.clone(),
        tls.server_key_pem.clone(),
        tls.ca_cert_pem.clone(),
    );
    assert!(endpoint.starts_with("https://"), "{endpoint}");

    let mut cfg = echo_agent_telemetry::external::ExternalOtelConfig::resolve_with(
        |name| match name {
            "ECHO_AGENT_EXTERNAL_OTEL" => Some("1".into()),
            "OTEL_LOGS_EXPORTER" | "OTEL_METRICS_EXPORTER" => Some("otlp".into()),
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some(endpoint.clone()),
            "OTEL_EXPORTER_OTLP_PROTOCOL" => Some("http/protobuf".into()),
            "OTEL_EXPORTER_OTLP_CERTIFICATE" => Some(ca_path.clone()),
            "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE" => Some(cert_path.clone()),
            "OTEL_EXPORTER_OTLP_CLIENT_KEY" => Some(key_path.clone()),
            "OTEL_METRIC_EXPORT_INTERVAL" => Some("200".into()),
            "OTEL_BLRP_SCHEDULE_DELAY" => Some("100".into()),
            _ => None,
        },
        None,
    )
    .expect("double opt-in must resolve");
    assert_eq!(
        cfg.logs_client_certificate.as_deref(),
        Some(cert_path.as_str())
    );
    assert_eq!(cfg.logs_client_key.as_deref(), Some(key_path.as_str()));
    assert_eq!(
        cfg.metrics_client_certificate.as_deref(),
        Some(cert_path.as_str())
    );
    assert_eq!(cfg.metrics_client_key.as_deref(), Some(key_path.as_str()));
    cfg.client = echo_agent_telemetry::external::config::ExternalClientInfo {
        service_version: "0.0.0-test".into(),
        client_version: "0.0.0-test".into(),
        app_entrypoint: "cli".into(),
    };

    echo_agent_telemetry::external::init(Some(cfg));
    assert!(
        echo_agent_telemetry::external::is_active(),
        "mTLS HTTP exporters must build and activate the stream \
         (crypto provider installed; client cert={cert_path}, key={key_path}, ca={ca_path}, endpoint={endpoint})"
    );

    echo_agent_telemetry::log_event(echo_agent_telemetry::events::SessionNew {
        session_id: "sess-http-mtls-1".into(),
        client_identifier: None,
        client_version: None,
        is_git_repo: true,
        permission_mode: echo_agent_telemetry::enums::PermissionMode::Ask,
    });
    echo_agent_telemetry::log_event(echo_agent_telemetry::events::SessionHarness {
        session_id: "sess-http-mtls-1".into(),
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
    assert!(
        col::wait_until(std::time::Duration::from_secs(10), || {
            collected.logs_len() > 0
        }),
        "log records must arrive over HTTP mTLS"
    );
    let names = col::event_names(&collected);
    assert!(
        names.iter().any(|n| n == "echo_agent_code.session_start"),
        "expected echo_agent_code.session_start in {names:?}"
    );

    assert!(
        col::wait_until(std::time::Duration::from_secs(10), || {
            collected.metrics_len() > 0
        }),
        "metric exports must arrive over HTTP mTLS"
    );

    echo_agent_telemetry::external::shutdown();
}
