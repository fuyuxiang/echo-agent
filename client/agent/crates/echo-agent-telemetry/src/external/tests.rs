//! Unit tests for the external stream: pinned allowlists, per-event schema
//! snapshots, canary leak tests, gate enforcement, and the tighten-only
//! remote policy. Everything asserting wire shape goes through the
//! in-memory exporters *behind the export-time validators*, so the tests pin
//! what actually leaves the process.

use super::config::ContentGates;
use super::schema::{self, AttrValue, ExternalKey, ExternalRecord, MetricIncrement};
use super::test_support::{TestStream, build, emit_event_into, emit_into};
use crate::events;
use opentelemetry::logs::AnyValue;

fn gates_off() -> ContentGates {
    ContentGates::default()
}

fn gates_all_on() -> ContentGates {
    ContentGates {
        log_user_prompts: true,
        log_tool_details: true,
    }
}

/// Exported (event_name, sorted attr key/value-debug pairs) for assertions.
fn exported_events(stream: &TestStream) -> Vec<(String, Vec<(String, String)>)> {
    stream
        .logs
        .get_emitted_logs()
        .expect("in-memory logs")
        .iter()
        .map(|log| {
            let record = &log.record;
            let mut attrs: Vec<(String, String)> = record
                .attributes_iter()
                .map(|(k, v)| {
                    let value = match v {
                        AnyValue::String(s) => s.as_str().to_owned(),
                        AnyValue::Int(i) => i.to_string(),
                        AnyValue::Boolean(b) => b.to_string(),
                        other => format!("{other:?}"),
                    };
                    (k.as_str().to_owned(), value)
                })
                .collect();
            attrs.sort();
            (record.event_name().unwrap_or("?").to_owned(), attrs)
        })
        .collect()
}

fn attr_keys(event: &(String, Vec<(String, String)>)) -> Vec<&str> {
    event.1.iter().map(|(k, _)| k.as_str()).collect()
}

fn attr(event: &(String, Vec<(String, String)>), key: &str) -> Option<String> {
    event
        .1
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

/// All exported metric (name, sorted data-point attr keys) pairs.
fn exported_metric_names(stream: &TestStream) -> Vec<String> {
    stream
        .metrics
        .get_finished_metrics()
        .expect("in-memory metrics")
        .iter()
        .flat_map(|rm| {
            rm.scope_metrics()
                .flat_map(|s| s.metrics().map(|m| m.name().to_owned()))
                .collect::<Vec<_>>()
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Pinned allowlists
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn external_allowed_keys_are_pinned() {
    // Keep this an independent copy — don't reference ALL_KEYS or
    // ExternalKey::as_str, or the assert becomes a tautology and stops gating
    // schema changes. Adding a key exports a new field: confirm it carries no
    // user content, then update this pin.
    let expected: &[&str] = &[
        "session.id",
        "turn_number",
        "prompt.id",
        "event.sequence",
        "user.id",
        "organization.id",
        "team.id",
        "deployment.id",
        "model",
        "permission_mode",
        "mcp_server_count",
        "plugin_count",
        "skill_count",
        "hook_count",
        "memory_enabled",
        "is_git_repo",
        "client_identifier",
        "duration_secs",
        "turn_count",
        "tool_call_count",
        "compaction_count",
        "prompt_length",
        "prompt",
        "screen_mode",
        "outcome",
        "duration_ms",
        "error_category",
        "cancellation_category",
        "stop_reason",
        "input_tokens",
        "output_tokens",
        "reasoning_tokens",
        "cache_read_tokens",
        "status_code",
        "tool_name",
        "success",
        "file_extension",
        "tool_parameters",
        "file_path",
        "decision",
        "access_kind",
        "source",
        "status",
        "transport_type",
        "tool_count",
        "error_type",
        "mcp_server.name",
        "to_mode",
        "trigger",
        "skill_source",
        "skill.name",
        "install_kind",
        "plugin_scope",
        "plugin_name",
        "plugin_version",
        "compaction_trigger",
        "compaction_outcome",
        "tokens_before",
        "tokens_after",
        "phase",
        "subagent_type",
        "auth_method",
        "from_model",
        "to_model",
        "error_code",
        "tip",
        "action",
    ];
    let actual: Vec<&str> = schema::ALL_KEYS.iter().map(|k| k.as_str()).collect();
    assert_eq!(
        actual, expected,
        "EXTERNAL_ALLOWED_KEYS changed: a new key is a wire-schema change — confirm it carries \
         no user content, then update this pin."
    );
    // Notably absent: the internal allowlist's file-path family.
    for forbidden in ["path", "cwd", "repo_path", "worktree", "gcs_path"] {
        assert!(
            !schema::external_allowed_keys().contains(forbidden),
            "{forbidden} must never be externally allowlisted"
        );
    }
}

#[test]
fn metric_attr_keys_are_pinned() {
    let expected: &[&str] = &[
        "type",
        "model",
        "outcome",
        "tool_name",
        "decision",
        "access_kind",
        "permission_mode",
        "error_category",
        "phase",
        "stuck_in",
        "auth_mode",
        "session.id",
        "app.version",
        "user.id",
        "organization.id",
        "team.id",
        "deployment.id",
    ];
    assert_eq!(
        schema::METRIC_ALLOWED_ATTR_KEYS,
        expected,
        "metric attribute keys changed — wire-schema change"
    );
    assert!(
        !schema::METRIC_ALLOWED_ATTR_KEYS.contains(&"prompt.id"),
        "prompt.id is events-only (unbounded cardinality on metrics)"
    );
}

#[test]
fn event_names_are_pinned() {
    use schema::ExternalEventName as E;
    let expected: &[(E, &str)] = &[
        (E::SessionStart, "echo_agent_code.session_start"),
        (E::SessionEnd, "echo_agent_code.session_end"),
        (E::UserPrompt, "echo_agent_code.user_prompt"),
        (E::TurnCompleted, "echo_agent_code.turn_completed"),
        (E::ApiRequest, "echo_agent_code.api_request"),
        (E::ApiError, "echo_agent_code.api_error"),
        (E::ToolResult, "echo_agent_code.tool_result"),
        (E::ToolDecision, "echo_agent_code.tool_decision"),
        (
            E::McpServerConnection,
            "echo_agent_code.mcp_server_connection",
        ),
        (
            E::PermissionModeChanged,
            "echo_agent_code.permission_mode_changed",
        ),
        (E::SkillActivated, "echo_agent_code.skill_activated"),
        (E::PluginLoaded, "echo_agent_code.plugin_loaded"),
        (E::Compaction, "echo_agent_code.compaction"),
        (E::Subagent, "echo_agent_code.subagent"),
        (E::Auth, "echo_agent_code.auth"),
        (E::InternalError, "echo_agent_code.internal_error"),
        (E::ModelSwitched, "echo_agent_code.model_switched"),
        (E::ContextualTip, "echo_agent_code.contextual_tip"),
    ];
    assert_eq!(expected.len(), <E as strum::EnumCount>::COUNT);
    for (variant, name) in expected {
        assert_eq!(variant.as_str(), *name, "event name is a wire commitment");
    }
}

#[test]
fn client_identifier_allowlist_is_pinned() {
    let expected: &[&str] = &[
        "echo-agent-pager",
        "echo-agent-tui",
        "echo-agent-shell",
        "echo-agent-web",
        "echo-agent-desktop",
        "echo-agent-code-extension",
        "echo-agent-agent-sdk",
        "nebula",
        "zed",
    ];
    assert_eq!(schema::KNOWN_CLIENT_IDENTIFIERS, expected);
    assert_eq!(
        schema::sanitize_client_identifier("echo-agent-pager"),
        "echo-agent-pager"
    );
    assert_eq!(
        schema::sanitize_client_identifier("Evil Corp Internal Tool v2"),
        "other",
        "unknown client identifiers are externally controlled free text and must collapse"
    );
}

#[test]
fn screen_mode_allowlist_is_pinned() {
    let expected: &[&str] = &["fullscreen", "inline", "minimal", "headless"];
    assert_eq!(schema::KNOWN_SCREEN_MODES, expected);
    assert_eq!(schema::sanitize_screen_mode("minimal"), "minimal");
    assert_eq!(
        schema::sanitize_screen_mode("my-custom-fork-mode"),
        "other",
        "unknown screen modes are externally controlled free text and must collapse"
    );
}

#[test]
fn tool_name_sanitization() {
    assert_eq!(schema::sanitize_tool_name("read_file"), "read_file");
    assert_eq!(schema::sanitize_tool_name("memory_search"), "memory_search");
    assert_eq!(schema::sanitize_tool_name("memory_get"), "memory_get");
    assert_eq!(
        schema::sanitize_tool_name("nebula__post_message"),
        "mcp_tool"
    );
    assert_eq!(
        schema::sanitize_tool_name("SuperSecretProjectTool"),
        "custom_tool",
        "unknown tool names must not pass verbatim"
    );
}

#[test]
fn file_extension_reduction() {
    assert_eq!(
        schema::file_extension("/Users/alice/proj/main.rs"),
        Some("rs".into())
    );
    assert_eq!(schema::file_extension("src/App.TSX"), Some("tsx".into()));
    assert_eq!(schema::file_extension("Makefile"), None);
    // 10-char cap.
    assert_eq!(
        schema::file_extension("x.aaaaaaaaaaaaaaaa"),
        Some("aaaaaaaaaa".into())
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Schema snapshots (gates off / on) through the wire-view harness
// ─────────────────────────────────────────────────────────────────────────────

fn sentinel_session_harness() -> events::SessionHarness {
    events::SessionHarness {
        session_id: "sess-1".into(),
        client_identifier: Some("echo-agent-pager".into()),
        model_id: "echo-agent-4".into(),
        agent_name: "echo-agent-build-plan".into(),
        permission_mode: crate::enums::PermissionMode::Ask,
        mcp_server_names: vec!["secret-server".into(), "other".into()],
        plugin_names: vec!["p1".into()],
        skill_names: vec!["s1".into(), "s2".into(), "s3".into()],
        lsp_server_names: vec![],
        hook_names: vec!["h1".into()],
        agents_md_dir_names: vec!["proj".into()],
        memory_enabled: true,
        memory_retrieval_mode: events::MemoryRetrievalMode::Hybrid,
        is_git_repo: true,
        auto_update: None,
    }
}

#[test]
fn session_start_snapshot_counts_not_names() {
    let stream = build(gates_off());
    emit_event_into(&stream, &sentinel_session_harness());
    let events = exported_events(&stream);
    assert_eq!(events.len(), 1);
    let ev = &events[0];
    assert_eq!(ev.0, "echo_agent_code.session_start");
    let mut keys = attr_keys(ev);
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "client_identifier",
            "event.sequence",
            "hook_count",
            "is_git_repo",
            "mcp_server_count",
            "memory_enabled",
            "model",
            "permission_mode",
            "plugin_count",
            "session.id",
            "skill_count",
        ]
    );
    assert_eq!(attr(ev, "mcp_server_count").as_deref(), Some("2"));
    assert_eq!(attr(ev, "skill_count").as_deref(), Some("3"));
    assert_eq!(attr(ev, "session.id").as_deref(), Some("sess-1"));
    // Names (MCP/plugin/skill/hook) never appear, only counts.
    let blob = format!("{events:?}");
    assert!(!blob.contains("secret-server"), "MCP name leaked: {blob}");
}

#[test]
fn session_new_increments_session_count_only() {
    let stream = build(gates_off());
    emit_event_into(
        &stream,
        &events::SessionNew {
            session_id: "sess-2".into(),
            client_identifier: None,
            client_version: None,
            is_git_repo: false,
            permission_mode: crate::enums::PermissionMode::Ask,
        },
    );
    assert!(exported_events(&stream).is_empty(), "metric-only mapping");
    let names = exported_metric_names(&stream);
    assert_eq!(names, vec!["echo_agent_code.session.count".to_owned()]);
}

#[test]
fn agent_connect_timeout_emits_phase_histogram_and_timeout_counter() {
    let stream = build(gates_off());
    let mut phase_durations_ms = std::collections::BTreeMap::new();
    phase_durations_ms.insert("config_load".into(), 12);
    phase_durations_ms.insert("model_catalog".into(), 28_700);
    emit_event_into(
        &stream,
        &events::AgentConnect {
            connect_target: crate::startup::AgentKind::Embedded,
            outcome: crate::startup::StartupOutcome::Timeout,
            stuck_in: Some("model_catalog".into()),
            phases: "config_load=12ms, model_catalog>=28.7s".into(),
            phase_durations_ms,
            elapsed_ms: 30_000,
            timeout_secs: Some(30),
            embedded_fallback: false,
            auth_mode: crate::startup::AuthMode::Deployment,
        },
    );
    assert!(exported_events(&stream).is_empty());
    let mut names = exported_metric_names(&stream);
    names.sort();
    assert_eq!(
        names,
        vec![
            "echo_agent_code.startup.phase_duration".to_owned(),
            "echo_agent_code.startup.timeout".to_owned(),
        ]
    );
}

#[test]
fn startup_completed_records_the_total_histogram_only() {
    let stream = build(gates_off());
    emit_event_into(
        &stream,
        &events::StartupCompleted {
            total_ms: 1_234,
            outcome: crate::startup::StartupOutcome::Ok,
            phases: "config_load=12ms, session_create=800ms".into(),
            auth_mode: crate::startup::AuthMode::Team,
            prefetch_wait_ms: Some(210),
            session_load_ms: Some(120),
            session_replay_ms: None,
            session_git_scan_ms: Some(40),
            session_spawn_ms: Some(300),
            time_to_first_frame_ms: Some(650),
        },
    );
    assert!(exported_events(&stream).is_empty());
    assert_eq!(
        exported_metric_names(&stream),
        vec!["echo_agent_code.startup.total".to_owned()]
    );
}

#[test]
fn api_request_snapshot_and_token_usage() {
    let stream = build(gates_off());
    emit_event_into(
        &stream,
        &events::ModelResponseReceived {
            model_id: "echo-agent-4".into(),
            duration_ms: 1200,
            stop_reason: Some("stop".into()),
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            reasoning_tokens: Some(25),
            cached_prompt_tokens: None,
        },
    );
    let events = exported_events(&stream);
    let ev = &events[0];
    assert_eq!(ev.0, "echo_agent_code.api_request");
    assert_eq!(attr(ev, "input_tokens").as_deref(), Some("100"));
    assert_eq!(attr(ev, "output_tokens").as_deref(), Some("50"));
    assert_eq!(attr(ev, "reasoning_tokens").as_deref(), Some("25"));
    assert_eq!(attr(ev, "cache_read_tokens"), None);
    assert_eq!(
        exported_metric_names(&stream),
        vec!["echo_agent_code.token.usage"]
    );
}

/// One failed turn ⇒ exactly one `error.count` increment, even though the
/// failure emits `ApiError` (and possibly `RateLimitHit`) *alongside*
/// `TurnCompleted{Error}`. `TurnCompleted{Error}` is the single increment
/// source; the api_error log events carry no metric (Bugbot regression:
/// double-counted errors at customer collectors).
#[test]
fn one_failed_turn_increments_error_count_exactly_once() {
    let stream = build(gates_off());
    // The turn-error path emits all three for a rate-limited failure.
    emit_event_into(
        &stream,
        &events::RateLimitHit {
            model_id: "echo-agent-4".into(),
            attempts: 3,
        },
    );
    emit_event_into(
        &stream,
        &events::ApiError {
            error_category: "rate_limit".into(),
            model_id: "echo-agent-4".into(),
            status_code: Some(429),
            duration_ms: Some(10),
        },
    );
    emit_event_into(
        &stream,
        &events::TurnCompleted {
            outcome: events::Outcome::Error,
            duration_ms: 10,
            tool_call_count: 0,
            model_id: "echo-agent-4".into(),
            cancellation_category: None,
            error_category: Some("rate_limit".into()),
        },
    );
    // Both api_error events exported as log records…
    let names: Vec<String> = exported_events(&stream)
        .iter()
        .map(|e| e.0.clone())
        .collect();
    assert_eq!(
        names
            .iter()
            .filter(|n| *n == "echo_agent_code.api_error")
            .count(),
        2
    );
    // …but error.count incremented exactly once.
    let total: u64 = stream
        .metrics
        .get_finished_metrics()
        .unwrap()
        .iter()
        .flat_map(|rm| rm.scope_metrics())
        .flat_map(|s| s.metrics())
        .filter(|m| m.name() == "echo_agent_code.error.count")
        .map(|m| match m.data() {
            opentelemetry_sdk::metrics::data::AggregatedMetrics::U64(
                opentelemetry_sdk::metrics::data::MetricData::Sum(sum),
            ) => sum.data_points().map(|p| p.value()).sum::<u64>(),
            _ => 0,
        })
        .sum();
    assert_eq!(total, 1, "one failed turn must count exactly one error");
}

#[test]
fn turn_error_increments_error_count() {
    let stream = build(gates_off());
    emit_event_into(
        &stream,
        &events::TurnCompleted {
            outcome: events::Outcome::Error,
            duration_ms: 10,
            tool_call_count: 0,
            model_id: "echo-agent-4".into(),
            cancellation_category: None,
            error_category: Some("server_error".into()),
        },
    );
    let mut names = exported_metric_names(&stream);
    names.sort();
    assert_eq!(
        names,
        vec!["echo_agent_code.error.count", "echo_agent_code.turn.count"]
    );
}

#[test]
fn tool_result_gates_off_collapses_and_reduces() {
    let stream = build(gates_off());
    emit_event_into(
        &stream,
        &events::ToolCallCompleted {
            tool_name: "nebula__post_message".into(),
            outcome: echo_agent_session_events::types::ToolOutcome::Success,
            duration_ms: 42,
            tool_result_size_bytes: None,
            file_path: Some("/Users/alice/secret-project/main.rs".into()),
            parameters: Some(serde_json::json!({"text": "CANARY_TOOL_ARGS"})),
        },
    );
    let events = exported_events(&stream);
    let ev = &events[0];
    assert_eq!(ev.0, "echo_agent_code.tool_result");
    assert_eq!(attr(ev, "tool_name").as_deref(), Some("mcp_tool"));
    assert_eq!(attr(ev, "file_extension").as_deref(), Some("rs"));
    assert_eq!(attr(ev, "file_path"), None, "full path is details-gated");
    assert_eq!(
        attr(ev, "tool_parameters"),
        None,
        "params are details-gated"
    );
    let blob = format!("{events:?}");
    assert!(
        !blob.contains("CANARY_TOOL_ARGS"),
        "tool args leaked: {blob}"
    );
    assert!(!blob.contains("secret-project"), "path leaked: {blob}");
}

#[test]
fn tool_result_details_gate_exposes_verbatim_scrubbed() {
    let stream = build(gates_all_on());
    // Use the *real* home dir: `redact_user_paths` collapses the current
    // user's home (env-derived), not arbitrary foreign paths.
    let home = dirs::home_dir()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/home/testuser".into());
    let path = format!("{home}/proj/main.rs");
    emit_event_into(
        &stream,
        &events::ToolCallCompleted {
            tool_name: "nebula__post_message".into(),
            outcome: echo_agent_session_events::types::ToolOutcome::Success,
            duration_ms: 42,
            tool_result_size_bytes: None,
            file_path: Some(path.clone()),
            parameters: Some(serde_json::json!({"key": "sk-CANARYabcdefghij1234567890"})),
        },
    );
    let events = exported_events(&stream);
    let ev = &events[0];
    assert_eq!(
        attr(ev, "tool_name").as_deref(),
        Some("nebula__post_message"),
        "details gate exposes the verbatim name"
    );
    let exported_path = attr(ev, "file_path").expect("details gate exposes the path");
    // Secrets are STILL scrubbed inside gated content; the home dir collapses.
    let blob = format!("{events:?}");
    assert!(
        !blob.contains("CANARY"),
        "secret inside gated params leaked: {blob}"
    );
    assert!(
        !exported_path.contains(&home),
        "home dir not collapsed in gated path: {exported_path}"
    );
    assert!(exported_path.contains("proj/main.rs"));
}

#[test]
fn user_prompt_gates_off_drops_text() {
    let stream = build(gates_off());
    emit_event_into(
        &stream,
        &events::PromptSubmitted {
            prompt_length: 26,
            model_id: "echo-agent-4".into(),
            client_identifier: None,
            screen_mode: Some("minimal".into()),
            prompt_text: Some("CANARY_PROMPT secret user text".into()),
        },
    );
    let events = exported_events(&stream);
    let ev = &events[0];
    assert_eq!(ev.0, "echo_agent_code.user_prompt");
    assert_eq!(attr(ev, "prompt_length").as_deref(), Some("26"));
    // screen_mode is ungated session metadata, not prompt content.
    assert_eq!(attr(ev, "screen_mode").as_deref(), Some("minimal"));
    assert_eq!(attr(ev, "prompt"), None);
    let blob = format!("{events:?}");
    assert!(
        !blob.contains("CANARY_PROMPT"),
        "prompt text leaked: {blob}"
    );
}

/// `screen_mode` is externally controlled free text (ACP `_meta.screenMode`);
/// unknown values must collapse to `"other"` on the wire, and an absent value
/// must emit no attribute at all.
#[test]
fn user_prompt_screen_mode_sanitized_and_optional() {
    let stream = build(gates_off());
    emit_event_into(
        &stream,
        &events::PromptSubmitted {
            prompt_length: 5,
            model_id: "echo-agent-4".into(),
            client_identifier: None,
            screen_mode: Some("Evil Free Text".into()),
            prompt_text: None,
        },
    );
    emit_event_into(
        &stream,
        &events::PromptSubmitted {
            prompt_length: 5,
            model_id: "echo-agent-4".into(),
            client_identifier: None,
            screen_mode: None,
            prompt_text: None,
        },
    );
    let events = exported_events(&stream);
    assert_eq!(attr(&events[0], "screen_mode").as_deref(), Some("other"));
    assert_eq!(attr(&events[1], "screen_mode"), None);
}

#[test]
fn user_prompt_gate_on_exports_scrubbed_text() {
    let stream = build(gates_all_on());
    emit_event_into(
        &stream,
        &events::PromptSubmitted {
            prompt_length: 10,
            model_id: "echo-agent-4".into(),
            client_identifier: None,
            screen_mode: None,
            prompt_text: Some("fix the bug; token sk-CANARYabcdefghij1234567890".into()),
        },
    );
    let events = exported_events(&stream);
    let ev = &events[0];
    let prompt = attr(ev, "prompt").expect("gate on ⇒ prompt exported");
    assert!(prompt.contains("fix the bug"));
    assert!(
        !prompt.contains("CANARY"),
        "secret inside prompt not scrubbed"
    );
}

#[test]
fn mcp_connection_collapses_server_name_by_default() {
    let stream = build(gates_off());
    emit_event_into(
        &stream,
        &events::McpServerFailed {
            server_name: "corp-internal-jira".into(),
            error_type: events::McpErrorType::Timeout,
            duration_ms: 1000,
            timeout_sec: 30,
        },
    );
    let events = exported_events(&stream);
    let ev = &events[0];
    assert_eq!(ev.0, "echo_agent_code.mcp_server_connection");
    assert_eq!(attr(ev, "status").as_deref(), Some("failed"));
    assert_eq!(attr(ev, "mcp_server.name").as_deref(), Some("mcp_server"));
    assert_eq!(attr(ev, "error_type").as_deref(), Some("timeout"));
    assert!(!format!("{events:?}").contains("corp-internal-jira"));
}

#[test]
fn tool_decision_snapshot() {
    let stream = build(gates_off());
    emit_event_into(
        &stream,
        &events::PermissionDecisionPayload {
            tool_name: "run_terminal_cmd".into(),
            access_kind: events::AccessKind::Bash,
            decision: events::PermissionOutcome::Deny,
            wait_ms: 1500,
            permission_mode: crate::enums::PermissionMode::Ask,
            source: Some("user_reject".into()),
            subagent_session_id: None,
            subagent_type: None,
            manager_prompt_attempted: Some(true),
            prompt_outcome: Some(events::PermissionPromptOutcome::Reject),
            prompt_outcome_detail: Some(events::PermissionPromptOutcomeDetail::RejectOnce),
            remember_tool_approvals: Some(true),
            decision_reason: Some(events::PermissionDecisionReason::AutoDenialLimit),
            classifier_source: Some(events::PermissionClassifierSource::Llm),
            classifier_verdict: Some(events::PermissionClassifierVerdict::Block),
            security_findings: Some(vec![events::PermissionSecurityFinding::DangerousCommand]),
            classifier_latency_ms: Some(42),
            auto_denials_consecutive: Some(3),
            auto_denials_total: Some(3),
        },
    );
    let events = exported_events(&stream);
    let ev = &events[0];
    assert_eq!(ev.0, "echo_agent_code.tool_decision");
    assert_eq!(attr(ev, "tool_name").as_deref(), Some("run_terminal_cmd"));
    assert_eq!(attr(ev, "decision").as_deref(), Some("deny"));
    assert_eq!(attr(ev, "access_kind").as_deref(), Some("bash"));
    assert_eq!(attr(ev, "permission_mode").as_deref(), Some("ask"));
    assert_eq!(attr(ev, "source").as_deref(), Some("user_reject"));
    assert_eq!(
        exported_metric_names(&stream),
        vec!["echo_agent_code.tool.decision"]
    );
    for key in [
        "manager_prompt_attempted",
        "prompt_outcome",
        "prompt_outcome_detail",
        "remember_tool_approvals",
        "decision_reason",
        "classifier_source",
        "classifier_verdict",
        "security_findings",
        "classifier_latency_ms",
        "auto_denials_consecutive",
        "auto_denials_total",
    ] {
        assert_eq!(attr(ev, key), None, "external record must not export {key}");
    }
    let dbg = format!("{events:?}");
    assert!(
        !dbg.contains("dangerous_command") && !dbg.contains("auto_denial_limit"),
        "no manager finding/reason value may appear in the external record"
    );
}

#[test]
fn skill_activated_name_gated() {
    let stream = build(gates_off());
    emit_event_into(
        &stream,
        &events::SkillDispatched {
            skill_name: "internal-deploy-runbook".into(),
            plugin_source: None,
            trigger: events::SkillTrigger::SlashCommand,
        },
    );
    let events = exported_events(&stream);
    let ev = &events[0];
    assert_eq!(attr(ev, "skill_source").as_deref(), Some("local"));
    assert_eq!(attr(ev, "trigger").as_deref(), Some("slash_command"));
    assert_eq!(attr(ev, "skill.name"), None);
    assert!(!format!("{events:?}").contains("internal-deploy-runbook"));
}

#[test]
fn skill_activated_exports_every_trigger() {
    for (trigger, label) in [
        (events::SkillTrigger::SlashCommand, "slash_command"),
        (events::SkillTrigger::SkillMdRead, "skill_md_read"),
        (events::SkillTrigger::SkillTool, "skill_tool"),
    ] {
        let stream = build(gates_off());
        emit_event_into(
            &stream,
            &events::SkillDispatched {
                skill_name: "pdf".into(),
                plugin_source: None,
                trigger,
            },
        );
        let events = exported_events(&stream);
        assert_eq!(attr(&events[0], "trigger").as_deref(), Some(label));
    }
}

#[test]
fn contextual_tip_maps_every_tip_and_action() {
    use events::{ContextualTipAction as A, ContextualTipKind as K};
    let cases = [
        (K::Undo, A::Shown, "undo", "shown"),
        (K::Undo, A::Accepted, "undo", "accepted"),
        (K::PlanMode, A::Shown, "plan_mode", "shown"),
        (K::PlanMode, A::Accepted, "plan_mode", "accepted"),
        (K::ImageInput, A::Shown, "image_input", "shown"),
        (K::ImageInput, A::Accepted, "image_input", "accepted"),
        (K::SendNow, A::Shown, "send_now", "shown"),
        (K::SendNow, A::Accepted, "send_now", "accepted"),
        (K::SmallScreen, A::Shown, "small_screen", "shown"),
        (K::SmallScreen, A::Accepted, "small_screen", "accepted"),
        (K::WordSelect, A::Shown, "word_select", "shown"),
        (K::WordSelect, A::Accepted, "word_select", "accepted"),
        (K::SshWrap, A::Shown, "ssh_wrap", "shown"),
        (K::SshWrap, A::Accepted, "ssh_wrap", "accepted"),
    ];
    for (tip, action, tip_label, action_label) in cases {
        let stream = build(gates_off());
        emit_event_into(&stream, &events::ContextualTip { tip, action });
        let events = exported_events(&stream);
        let ev = &events[0];
        assert_eq!(ev.0, "echo_agent_code.contextual_tip");
        assert_eq!(attr(ev, "tip").as_deref(), Some(tip_label));
        assert_eq!(attr(ev, "action").as_deref(), Some(action_label));
    }
}

#[test]
fn unmapped_events_produce_nothing() {
    use crate::events::TelemetryEvent as _;
    // ~70 events without an `external = …` arm cost nothing and export nothing.
    let ev = events::SlashCommandUsed {
        command: "secret command".into(),
        args_provided: true,
    };
    assert!(ev.external_record().is_none());
}

/// Workspace-origin exclusion: events emitted exclusively via
/// `EmitterOrigin::Workspace` (`log_session_event_with_origin`) must not carry
/// an external mapping — the fan-out hook deliberately lives only in the
/// Shell-origin wrappers. The workspace-only surface today is the
/// echo-agent-workspace sampler events, which live outside this crate and have
/// no `telemetry_event!` binding here; this pin guards the in-crate set.
#[test]
fn workspace_only_events_have_no_external_mapping() {
    use crate::events::TelemetryEvent as _;
    // Trace-upload lifecycle events are session-metrics/internal-only.
    assert!(
        crate::session_metrics::TraceUploadAttempted {
            session_id: String::new(),
            turn_number: 0,
            upload_method: "proxy".into(),
        }
        .external_record()
        .is_none()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Emit-path behavior: ctx injection, sequence, identity, truncation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn mapping_supplied_session_id_wins_and_sequence_increments() {
    let stream = build(gates_off());
    emit_event_into(&stream, &sentinel_session_harness());
    emit_event_into(&stream, &sentinel_session_harness());
    let events = exported_events(&stream);
    assert_eq!(events.len(), 2);
    assert_eq!(attr(&events[0], "event.sequence").as_deref(), Some("0"));
    assert_eq!(attr(&events[1], "event.sequence").as_deref(), Some("1"));
    assert_eq!(attr(&events[0], "session.id").as_deref(), Some("sess-1"));
}

#[test]
fn identity_attrs_attached_when_set_and_blank_ids_never_export() {
    let stream = build(gates_off());
    super::set_identity_on(
        &stream.ext,
        super::IdentityAttrs {
            user_id: Some("user-42".into()),
            organization_id: Some(String::new()), // blank: must not export
            team_id: None,
            deployment_id: Some("dep-7".into()),
        },
    );
    emit_event_into(&stream, &sentinel_session_harness());
    let events = exported_events(&stream);
    let ev = &events[0];
    assert_eq!(attr(ev, "user.id").as_deref(), Some("user-42"));
    assert_eq!(attr(ev, "deployment.id").as_deref(), Some("dep-7"));
    assert_eq!(attr(ev, "organization.id"), None, "blank ids never export");
    assert_eq!(attr(ev, "team.id"), None);
}

#[test]
fn long_attr_values_truncated() {
    let stream = build(gates_off());
    let long_model = "m".repeat(1000);
    emit_into(
        &stream,
        ExternalRecord {
            event: Some(schema::ExternalEventName::ApiRequest),
            attrs: vec![(ExternalKey::Model, AttrValue::Str(long_model))],
            gated: vec![],
            metrics: vec![],
        },
    );
    let events = exported_events(&stream);
    let model = attr(&events[0], "model").unwrap();
    assert!(
        model.len() < 200,
        "value not truncated: {} chars",
        model.len()
    );
    assert!(model.ends_with("…[truncated]"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Export-time validators (fail-closed)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn validating_metric_exporter_drops_export_on_bad_attr_key() {
    use opentelemetry::metrics::MeterProvider as _;
    let stream = build(gates_off());
    // Bypass emit.rs: increment with an attribute key outside the pinned set.
    let meter = stream
        .ext
        .meter_provider
        .as_ref()
        .unwrap()
        .meter(schema::SCOPE_NAME);
    let rogue = meter.u64_counter("echo_agent_code.session.count").build();
    rogue.add(
        1,
        &[opentelemetry::KeyValue::new("prompt", "CANARY_METRIC_LEAK")],
    );
    stream
        .ext
        .meter_provider
        .as_ref()
        .unwrap()
        .force_flush()
        .unwrap();
    let exported = stream.metrics.get_finished_metrics().unwrap();
    let blob = format!("{exported:?}");
    assert!(
        !blob.contains("CANARY_METRIC_LEAK"),
        "metric export with rogue attr must be dropped entirely: {blob}"
    );
    assert!(
        stream
            .ext
            .health
            .metric_exports_dropped
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 1
    );
}

#[test]
fn redacting_log_exporter_drops_record_with_closed_gate_key() {
    // A bug that attaches a gated key with the gate off must be caught at the
    // exporter even though emit.rs should never produce it.
    let stream = build(gates_off());
    use opentelemetry::logs::{LogRecord as _, Logger as _};
    let logger = stream.ext.logger.as_ref().unwrap();
    let mut record = logger.create_log_record();
    record.set_event_name("echo_agent_code.user_prompt");
    record.add_attribute("prompt", "CANARY_GATED_LEAK");
    logger.emit(record);
    stream
        .ext
        .logger_provider
        .as_ref()
        .unwrap()
        .force_flush()
        .unwrap();
    let blob = format!("{:?}", stream.logs.get_emitted_logs().unwrap());
    assert!(
        !blob.contains("CANARY_GATED_LEAK"),
        "closed-gate key must be dropped by the exporter: {blob}"
    );
}

#[test]
fn redacting_log_exporter_drops_record_with_unknown_key() {
    let stream = build(gates_off());
    use opentelemetry::logs::{LogRecord as _, Logger as _};
    let logger = stream.ext.logger.as_ref().unwrap();
    let mut record = logger.create_log_record();
    record.set_event_name("echo_agent_code.api_request");
    record.add_attribute("command", "echo CANARY_UNKNOWN_KEY");
    logger.emit(record);
    stream
        .ext
        .logger_provider
        .as_ref()
        .unwrap()
        .force_flush()
        .unwrap();
    let blob = format!("{:?}", stream.logs.get_emitted_logs().unwrap());
    assert!(
        !blob.contains("CANARY_UNKNOWN_KEY"),
        "unknown key leaked: {blob}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Remote policy: tighten-only
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn remote_force_disable_stops_emission() {
    let stream = build(gates_off());
    super::apply_remote_policy_on(
        &stream.ext,
        super::ExternalOtelRemotePolicy {
            force_disable: true,
            lock_content_gates: false,
        },
    );
    assert!(
        !stream.ext.active.load(std::sync::atomic::Ordering::Relaxed),
        "force_disable must clear the emission gate"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn remote_gate_lock_forces_gates_off_and_never_on() {
    let stream = build(gates_all_on());
    super::apply_remote_policy_on(
        &stream.ext,
        super::ExternalOtelRemotePolicy {
            force_disable: false,
            lock_content_gates: true,
        },
    );
    assert_eq!(*stream.ext.gates.read(), ContentGates::default());
    // The policy carries no loosen/enable direction by construction: applying
    // a default policy to an off-gates stream changes nothing.
    let stream2 = build(gates_off());
    super::apply_remote_policy_on(&stream2.ext, super::ExternalOtelRemotePolicy::default());
    assert_eq!(*stream2.ext.gates.read(), ContentGates::default());
    assert!(
        stream2
            .ext
            .active
            .load(std::sync::atomic::Ordering::Relaxed)
    );
}

/// All tests that mutate `SETTINGS_RESOLVED` must hold this lock.
static GATE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn settings_gate_suppresses_until_resolved() {
    let _serial = GATE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    struct RestoreGate;
    impl Drop for RestoreGate {
        fn drop(&mut self) {
            super::mark_external_otel_settings_resolved();
        }
    }
    let _restore = RestoreGate;

    // Baseline: default open.
    super::mark_external_otel_settings_resolved();
    assert!(super::is_settings_gate_open(), "gate defaults open");

    // Leader closes it at the start of its auth/network phase.
    super::suppress_external_otel_until_settings();
    assert!(
        !super::is_settings_gate_open(),
        "gate must be closed until settings resolve"
    );
    assert!(
        !super::is_active(),
        "is_active must be false while the settings gate is closed"
    );

    // Settings response arrives (policy evaluated) → reopen.
    super::mark_external_otel_settings_resolved();
    assert!(
        super::is_settings_gate_open(),
        "gate must reopen after settings are resolved"
    );
}

#[test]
fn settings_gate_opens_when_the_bounded_window_expires() {
    let _serial = GATE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    struct RestoreGate(std::time::Duration);
    impl Drop for RestoreGate {
        fn drop(&mut self) {
            super::set_settings_gate_max_wait(self.0);
            super::mark_external_otel_settings_resolved();
        }
    }
    let _restore = RestoreGate(super::settings_gate_max_wait());

    super::set_settings_gate_max_wait(std::time::Duration::from_secs(600));
    super::suppress_external_otel_until_settings();
    assert!(
        !super::is_settings_gate_open(),
        "inside the window the gate stays fail-closed"
    );

    super::set_settings_gate_max_wait(std::time::Duration::ZERO);
    assert!(
        super::is_settings_gate_open(),
        "an expired window must resolve the gate open onto local policy"
    );

    super::set_settings_gate_max_wait(std::time::Duration::from_secs(600));
    super::suppress_external_otel_until_settings();
    assert!(
        !super::is_settings_gate_open(),
        "re-closing must restart the window, not stay open"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Metric increment derivation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn metric_increments_pass_model_through_scrub_and_attach_session_id() {
    let stream = build(gates_off());
    emit_into(
        &stream,
        ExternalRecord {
            event: None,
            attrs: vec![(ExternalKey::SessionId, AttrValue::Str("sess-9".into()))],
            gated: vec![],
            metrics: vec![MetricIncrement::TokenUsage {
                token_type: "input",
                model: "sk-CANARYabcdefghij1234567890".into(),
                count: 7,
            }],
        },
    );
    let exported = stream.metrics.get_finished_metrics().unwrap();
    let blob = format!("{exported:?}");
    assert!(blob.contains("echo_agent_code.token.usage"));
    assert!(
        blob.contains("sess-9"),
        "session.id missing from metric: {blob}"
    );
    assert!(
        !blob.contains("CANARY"),
        "model-id metric attribute must pass the secret scrub: {blob}"
    );
}
