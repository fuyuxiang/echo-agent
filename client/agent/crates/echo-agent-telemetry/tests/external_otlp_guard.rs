//! Wire test for the **no-double-send invariant** (the credential-leak guard).
//!
//! If the internal trace firehose resolved its endpoint/headers from the
//! deprecated `OTEL_EXPORTER_OTLP_*` fallback, the shell sets
//! `internal_pipeline_consumed_otel_vars = true`, and `external::init` MUST
//! refuse to activate — otherwise the same standard vars could point both the
//! internally-authed firehose and the customer collector at one endpoint,
//! leaking EchoAgent credentials. Here we prove the refusal end-to-end: even with a
//! fully valid double opt-in pointed at a live collector, nothing is exported.

mod otlp_collector;

use echo_agent_telemetry::external;
use otlp_collector as col;

#[test]
fn refuses_to_activate_when_internal_consumed_standard_vars() {
    let collected = col::Collected::default();
    let endpoint = col::start_collector(collected.clone());

    let mut cfg = external::ExternalOtelConfig::resolve_with(
        |name| match name {
            "ECHO_AGENT_EXTERNAL_OTEL" => Some("1".into()),
            "OTEL_LOGS_EXPORTER" | "OTEL_METRICS_EXPORTER" => Some("otlp".into()),
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some(endpoint.clone()),
            "OTEL_METRIC_EXPORT_INTERVAL" => Some("100".into()),
            "OTEL_BLRP_SCHEDULE_DELAY" => Some("100".into()),
            _ => None,
        },
        None,
    )
    .expect("config resolves (the refusal happens at init, not resolution)");
    cfg.client = external::config::ExternalClientInfo {
        service_version: "0.0.0-test".into(),
        client_version: "0.0.0-test".into(),
        app_entrypoint: "cli".into(),
    };
    // The flag the shell sets when the internal firehose consumed the standard
    // OTEL_* vars via the deprecated fallback.
    cfg.internal_pipeline_consumed_otel_vars = true;

    external::init(Some(cfg));
    assert!(
        !external::is_active(),
        "external stream MUST refuse to activate to prevent credential leakage"
    );

    // Emit through the real funnel; with the stream inert this must be a no-op.
    echo_agent_telemetry::log_event(echo_agent_telemetry::events::SessionNew {
        session_id: "sess-guard".into(),
        client_identifier: None,
        client_version: None,
        is_git_repo: true,
        permission_mode: echo_agent_telemetry::enums::PermissionMode::Ask,
    });
    external::flush();

    // Give any (erroneously constructed) exporter ample time to phone home.
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(
        collected.logs_len(),
        0,
        "no logs may be exported when refused"
    );
    assert_eq!(
        collected.metrics_len(),
        0,
        "no metrics may be exported when refused"
    );

    external::shutdown();
}
