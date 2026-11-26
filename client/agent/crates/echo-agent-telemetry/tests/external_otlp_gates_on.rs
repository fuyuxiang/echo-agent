//! Wire test for the external OTEL stream with **both content gates ON** — the
//! higher-risk privacy path, where prompt text and tool parameters actually
//! leave the process. Asserts against an in-process OTLP collector that:
//!
//! - gated content (`prompt`, `tool_parameters`, `file_path`, verbatim
//!   `tool_name`/`mcp_server.name`) IS present when the gate is on,
//! - planted secret shapes are STILL scrubbed inside that gated content
//!   (gates loosen *which fields* export, never the secret scrub),
//! - identity attributes ride every record and metric once set,
//! - `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE=cumulative` and
//!   `OTEL_METRICS_INCLUDE_VERSION=1` take effect on the wire,
//! - the remote fleet kill switch stops emission in-process.
//!
//! Single sequential `#[test]` because the `EXTERNAL` registry is a
//! process-global `OnceLock`, so each init-config scenario is its own test
//! binary.

mod otlp_collector;

use echo_agent_telemetry::external::{self, ExternalOtelRemotePolicy, IdentityAttrs};
use otlp_collector as col;

// Secret shapes — MUST be scrubbed everywhere, even inside gated content.
const SECRET_KEY: &str = "sk-LEAKaaaaaaaaaaaaaaaa1234567890";
const SECRET_MODEL: &str = "echo-agent-4-sk-LEAKmodel1234567890abcd";
// Benign markers — with the gate ON these MUST appear on the wire (proving the
// gated field is actually exported, not just that the scrub ran).
const PROMPT_MARK: &str = "promptbodymarker";
const PARAM_MARK: &str = "parammarker";
const CLIENT_VERSION: &str = "9.9.9-cv";

#[test]
fn external_stream_gates_on_end_to_end() {
    let collected = col::Collected::default();
    let endpoint = col::start_collector(collected.clone());

    let mut cfg = external::ExternalOtelConfig::resolve_with(
        |name| match name {
            "ECHO_AGENT_EXTERNAL_OTEL" => Some("1".into()),
            "OTEL_LOGS_EXPORTER" | "OTEL_METRICS_EXPORTER" => Some("otlp".into()),
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some(endpoint.clone()),
            // Both content gates ON.
            "OTEL_LOG_USER_PROMPTS" | "OTEL_LOG_TOOL_DETAILS" => Some("1".into()),
            "OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE" => Some("cumulative".into()),
            "OTEL_METRICS_INCLUDE_VERSION" => Some("1".into()),
            "OTEL_METRIC_EXPORT_INTERVAL" => Some("200".into()),
            "OTEL_BLRP_SCHEDULE_DELAY" => Some("100".into()),
            _ => None,
        },
        None,
    )
    .expect("double opt-in must resolve");
    assert!(cfg.gates.log_user_prompts && cfg.gates.log_tool_details);
    cfg.client = external::config::ExternalClientInfo {
        service_version: "0.0.0-test".into(),
        client_version: CLIENT_VERSION.into(),
        app_entrypoint: "cli".into(),
    };

    external::init(Some(cfg));
    assert!(external::is_active(), "gates-on config must activate");

    // Identity attrs (plain ids — never tokens) ride every record + metric.
    external::set_identity(IdentityAttrs {
        user_id: Some("user-x".into()),
        organization_id: Some("org-acme".into()),
        team_id: Some("team-7".into()),
        deployment_id: Some("deploy-eu".into()),
    });

    // Product events disabled — pins the "external active while product telemetry off"
    // half of the independence matrix through the real funnel.
    assert!(!echo_agent_telemetry::is_enabled());

    echo_agent_telemetry::log_event(echo_agent_telemetry::events::SessionHarness {
        session_id: "sess-gates-on".into(),
        client_identifier: Some("echo-agent-pager".into()),
        model_id: "echo-agent-4".into(),
        agent_name: "echo-agent-build-plan".into(),
        permission_mode: echo_agent_telemetry::enums::PermissionMode::Ask,
        mcp_server_names: vec!["internal-mcp".into()],
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
        prompt_length: 100,
        model_id: "echo-agent-4".into(),
        client_identifier: None,
        screen_mode: None,
        prompt_text: Some(format!("refactor {PROMPT_MARK} with key {SECRET_KEY} now")),
    });
    echo_agent_telemetry::log_event(echo_agent_telemetry::events::ModelResponseReceived {
        model_id: SECRET_MODEL.into(),
        duration_ms: 5,
        stop_reason: Some("stop".into()),
        prompt_tokens: Some(11),
        completion_tokens: Some(7),
        reasoning_tokens: Some(3),
        cached_prompt_tokens: Some(9),
    });
    echo_agent_telemetry::log_event(echo_agent_telemetry::events::ToolCallCompleted {
        tool_name: "github__create_issue".into(),
        outcome: echo_agent_session_events::types::ToolOutcome::Success,
        duration_ms: 12,
        tool_result_size_bytes: None,
        file_path: Some("/tmp/projectdir/config.toml".into()),
        parameters: Some(serde_json::json!({
            "marker": PARAM_MARK,
            "token": SECRET_KEY,
            "deep": {"a": {"b": "c"}},
        })),
    });

    external::flush();
    assert!(
        col::wait_until(std::time::Duration::from_secs(10), || {
            !collected.logs.lock().unwrap().is_empty()
                && !collected.metrics.lock().unwrap().is_empty()
        }),
        "collector must receive both signals"
    );

    // ── Resource + scope ────────────────────────────────────────────────
    let records = col::log_records(&collected);
    let harness = col::find_event(&collected, "echo_agent_code.session_start")
        .expect("session_start must be present");
    assert_eq!(harness.scope_name, "ai.echoagent.echo_agent_code");
    assert_eq!(
        harness
            .resource
            .get("service.name")
            .and_then(|v| v.as_str()),
        Some("echo-agent-cli"),
        "service.name=echo-agent-cli is a wire commitment"
    );
    assert_eq!(
        harness
            .resource
            .get("echo_agent_code.schema.version")
            .and_then(|v| v.as_str()),
        Some("v1")
    );
    // External records carry no free-text body.
    assert!(
        records.iter().all(|r| !r.has_body),
        "no record may carry a body"
    );

    // ── Identity attrs on a record ──────────────────────────────────────
    assert_eq!(
        harness.attrs.get("user.id").and_then(|v| v.as_str()),
        Some("user-x")
    );
    assert_eq!(
        harness
            .attrs
            .get("organization.id")
            .and_then(|v| v.as_str()),
        Some("org-acme")
    );
    assert_eq!(
        harness.attrs.get("team.id").and_then(|v| v.as_str()),
        Some("team-7")
    );
    assert_eq!(
        harness.attrs.get("deployment.id").and_then(|v| v.as_str()),
        Some("deploy-eu")
    );

    // ── Prompt gate ON: text present, secret still scrubbed ─────────────
    let prompt =
        col::find_event(&collected, "echo_agent_code.user_prompt").expect("user_prompt present");
    let prompt_text = prompt
        .attrs
        .get("prompt")
        .and_then(|v| v.as_str())
        .expect("prompt attr present when OTEL_LOG_USER_PROMPTS=1");
    assert!(
        prompt_text.contains(PROMPT_MARK),
        "gated prompt body must export: {prompt_text:?}"
    );
    assert!(
        !prompt_text.contains(SECRET_KEY),
        "secret survived in prompt: {prompt_text:?}"
    );

    // ── Tool details gate ON: verbatim name + gated path/params, scrubbed ─
    let tool =
        col::find_event(&collected, "echo_agent_code.tool_result").expect("tool_result present");
    assert_eq!(
        tool.attrs.get("tool_name").and_then(|v| v.as_str()),
        Some("github__create_issue"),
        "details gate exposes the verbatim tool name"
    );
    assert_eq!(
        tool.attrs.get("file_extension").and_then(|v| v.as_str()),
        Some("toml"),
        "file_extension always exported"
    );
    assert!(
        tool.attrs.contains_key("file_path"),
        "full path exported under details gate"
    );
    let params = tool
        .attrs
        .get("tool_parameters")
        .and_then(|v| v.as_str())
        .expect("tool_parameters present under details gate");
    assert!(
        params.contains(PARAM_MARK),
        "gated params must export: {params:?}"
    );
    assert!(
        !params.contains(SECRET_KEY),
        "secret survived in params: {params:?}"
    );

    // ── Metrics: cumulative temporality + app.version + scrubbed model ──
    let tokens = col::find_metric(&collected, "echo_agent_code.token.usage");
    assert!(!tokens.is_empty(), "token.usage must export");
    for p in &tokens {
        assert_eq!(
            p.temporality,
            col::TEMPORALITY_CUMULATIVE,
            "cumulative requested"
        );
        assert_eq!(
            p.attrs.get("app.version").and_then(|v| v.as_str()),
            Some(CLIENT_VERSION),
            "OTEL_METRICS_INCLUDE_VERSION=1 attaches app.version"
        );
        assert_eq!(
            p.attrs.get("user.id").and_then(|v| v.as_str()),
            Some("user-x")
        );
        let model = p.attrs.get("model").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !model.contains("sk-LEAKmodel"),
            "metric model must be scrubbed: {model:?}"
        );
    }
    let sessions = col::find_metric(&collected, "echo_agent_code.session.count");
    // SessionHarness has no session.count metric; that comes from SessionNew —
    // not emitted here, so just confirm token.usage identity coverage above.
    let _ = sessions;

    // ── Canary scan at the raw HTTP layer (both signals) ────────────────
    let raw = collected.raw_text();
    assert!(!raw.contains(SECRET_KEY), "secret key reached the wire");
    assert!(
        !raw.contains("sk-LEAKmodel"),
        "secret model shape reached the wire"
    );

    // ── Remote fleet kill switch stops emission in-process ──────────────
    external::flush();
    col::wait_until(std::time::Duration::from_millis(500), || false);
    let logs_before = collected.logs_len();
    external::apply_remote_policy(ExternalOtelRemotePolicy {
        force_disable: true,
        lock_content_gates: false,
    });
    assert!(
        !external::is_active(),
        "kill switch must clear the emission gate"
    );
    echo_agent_telemetry::log_event(echo_agent_telemetry::events::PromptSubmitted {
        prompt_length: 1,
        model_id: "echo-agent-4".into(),
        client_identifier: None,
        screen_mode: None,
        prompt_text: Some("post-kill".into()),
    });
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(
        collected.logs_len(),
        logs_before,
        "no exports after the remote kill switch"
    );

    external::shutdown();
}
