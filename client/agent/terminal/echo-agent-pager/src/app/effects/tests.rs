#![cfg_attr(rustfmt, rustfmt::skip)]
use super::*;
use echo_agent_runtime::extensions::billing::{BillingConfig, Cent, UsagePeriod};
/// The invalid-params server detail survives `attach_prompt_usage`
/// wrapping `error.data` as `{message, promptUsage}`.
#[test]
fn format_acp_error_reads_detail_from_wrapped_data() {
    let bare = acp::Error::invalid_params().data("model does not support tools");
    assert_eq!(format_acp_error(&bare, false), "model does not support tools");
    let wrapped = acp::Error::invalid_params()
        .data(
            serde_json::json!({
            "message": "model does not support tools",
            "promptUsage": { "inputTokens": 12, "outputTokens": 0, "numTurns": 1 }
        }),
        );
    assert_eq!(format_acp_error(&wrapped, false), "model does not support tools");
}
#[test]
fn format_acp_error_formats_http_500_dump() {
    let err = acp::Error::internal_error()
        .data(
            serde_json::json!({
            "message": "API error (status 500 Internal Server Error): {\"error\":\"upstream exploded\"}",
            "http_status": 500
        }),
        );
    assert_eq!(
            format_acp_error(&err, false),
            "Server error (500): Something went wrong on our side. Wait a minute and send again."
        );
}
#[test]
fn format_acp_error_rate_limit_surfaces_detail_or_fallback() {
    use echo_agent_runtime::sampling::error::{
        FREE_USAGE_USER_MESSAGE, RATE_LIMITED_ERROR_CODE,
        RATE_LIMITED_USER_MESSAGE_API_KEY, RATE_LIMITED_USER_MESSAGE_OAUTH,
    };
    let cap_body = "The service is temporarily at capacity. Please retry your request shortly.";
    let capacity = acp::Error::new(RATE_LIMITED_ERROR_CODE, "Rate limited")
        .data(format!(
            "API error (status 429 Too Many Requests): {cap_body}"
        ));
    assert_eq!(format_acp_error(&capacity, false), cap_body);
    assert_eq!(format_acp_error(&capacity, true), cap_body);
    let rpm_body = "You are sending requests too quickly. Please slow down, or upgrade to a EchoAgent subscription for higher limits: https://cloud.echo-agent.invalid/echoagent";
    let rpm = acp::Error::new(RATE_LIMITED_ERROR_CODE, "Rate limited")
        .data(format!("API error (status 429 Too Many Requests): {rpm_body}"));
    assert!(format_acp_error(&rpm, false).contains("cloud.echo-agent.invalid/echoagent"));
    assert_eq!(format_acp_error(&rpm, true), RATE_LIMITED_USER_MESSAGE_API_KEY);
    let empty = acp::Error::new(RATE_LIMITED_ERROR_CODE, "Rate limited");
    assert_eq!(format_acp_error(&empty, false), RATE_LIMITED_USER_MESSAGE_OAUTH);
    assert_eq!(format_acp_error(&empty, true), RATE_LIMITED_USER_MESSAGE_API_KEY);
    let free = acp::Error::new(RATE_LIMITED_ERROR_CODE, "Rate limited")
        .data(
            "API error (status 429 Too Many Requests): \
             subscription:free-usage-exhausted: You have used all your free usage.",
        );
    assert_eq!(format_acp_error(&free, false), FREE_USAGE_USER_MESSAGE);
    assert_eq!(format_acp_error(&free, true), FREE_USAGE_USER_MESSAGE);
    let free_wrapped = acp::Error::new(RATE_LIMITED_ERROR_CODE, "Rate limited")
        .data(
            serde_json::json!({
                "message": "API error (status 429 Too Many Requests): \
                    subscription:free-usage-exhausted: You have used all your free usage.",
                "promptUsage": { "inputTokens": 12, "outputTokens": 0, "numTurns": 1 }
            }),
        );
    assert_eq!(
            format_acp_error(&free_wrapped, false),
            FREE_USAGE_USER_MESSAGE
        );
}
/// Non-empty token ranges ride the wire block meta as `skillTokenRanges`
/// byte pairs; the text itself is untouched.
#[test]
fn plain_prompt_block_stamps_skill_token_ranges_meta() {
    let block = plain_prompt_content_block("great /pr-workflow go".into(), &[6..18]);
    let acp::ContentBlock::Text(tb) = block else {
        panic!("expected text block");
    };
    assert_eq!(tb.text, "great /pr-workflow go");
    let meta = tb.meta.expect("meta stamped when ranges non-empty");
    assert_eq!(meta["skillTokenRanges"], serde_json::json!([[6, 18]]));
}
/// Empty ranges keep `meta: None` — the legacy wire shape is unchanged.
#[test]
fn plain_prompt_block_no_meta_when_ranges_empty() {
    let block = plain_prompt_content_block("hello".into(), &[]);
    let acp::ContentBlock::Text(tb) = block else {
        panic!("expected text block");
    };
    assert_eq!(tb.text, "hello");
    assert!(tb.meta.is_none());
}
/// With a screen mode, `_meta` carries both `promptId` and `screenMode`
/// (the shell threads the latter into `prompt_submitted.screen_mode`).
#[test]
fn prompt_request_meta_stamps_screen_mode() {
    let meta = prompt_request_meta("p-1", Some("minimal"));
    assert_eq!(
            meta,
            serde_json::json!({ "promptId": "p-1", "screenMode": "minimal" })
        );
}
/// Without a screen mode (`SessionFlags::default()` in tests), the key is
/// omitted — the legacy `{"promptId": …}` wire shape stays byte-identical.
#[test]
fn prompt_request_meta_omits_screen_mode_when_unset() {
    let meta = prompt_request_meta("p-2", None);
    assert_eq!(meta, serde_json::json!({ "promptId": "p-2" }));
}
/// Text-only interjections must omit the `content` key entirely — the
/// legacy `echo.agent/interject` wire shape stays byte-identical.
#[test]
fn interject_params_omit_content_when_no_blocks() {
    let sid = acp::SessionId::new("s1");
    let params = build_interject_params(&sid, "steer", "i1", None);
    let obj = params.as_object().unwrap();
    assert!(!obj.contains_key("content"), "content key must be absent");
    assert_eq!(obj["sessionId"], "s1");
    assert_eq!(obj["text"], "steer");
    assert_eq!(obj["interjectionId"], "i1");
    assert_eq!(obj.len(), 3, "no extra keys on the legacy shape");
}
#[test]
fn picker_keeps_conversation_with_empty_cwd_and_missing_updated_at() {
    let payload = serde_json::json!({
            "sessions": [{
                "sessionId": "conv_abc",
                "cwd": "",
                "summary": "Compare GPU vendors",
                "source": "conversation",
                "_meta": { "echo.agent/session": { "kind": "chat" } }
            }]
        });
    let entries = parse_session_picker_entries(&payload);
    assert_eq!(entries.len(), 1, "conversation must not vanish");
    assert_eq!(entries[0].id, "conv_abc");
    assert_eq!(entries[0].cwd, "");
    assert_eq!(entries[0].source, "conversation");
}
#[test]
fn picker_keeps_old_conversation_past_cutoff() {
    let payload = serde_json::json!({
            "sessions": [{
                "sessionId": "conv_old",
                "cwd": "",
                "summary": "Ancient chat",
                "source": "conversation",
                "updatedAt": "2020-01-01T00:00:00Z",
                "_meta": { "echo.agent/session": { "kind": "chat" } }
            }]
        });
    let entries = parse_session_picker_entries(&payload);
    assert_eq!(entries.len(), 1, "old conversation must still render");
    assert_eq!(entries[0].source, "conversation");
}
#[test]
fn picker_drops_local_with_missing_updated_at() {
    let payload = serde_json::json!({
            "sessions": [{
                "sessionId": "local_no_ts",
                "cwd": "/Users/me/echoagent",
                "summary": "no timestamp",
                "source": "local"
            }]
        });
    let entries = parse_session_picker_entries(&payload);
    assert!(
            entries.is_empty(),
            "local rows still require a parseable updatedAt"
        );
}
/// Untitled cloud.echo-agent.invalid chats must stay listed, rendered as "Untitled".
#[test]
fn picker_keeps_untitled_conversation_as_untitled() {
    let payload = serde_json::json!({
            "sessions": [{
                "sessionId": "conv_untitled",
                "cwd": "",
                "summary": "",
                "source": "conversation",
                "updatedAt": "2026-07-01T00:00:00Z",
                "_meta": { "echo.agent/session": { "kind": "chat" } }
            }]
        });
    let entries = parse_session_picker_entries(&payload);
    assert_eq!(entries.len(), 1, "untitled conversation must not vanish");
    assert_eq!(entries[0].summary, "Untitled");
    assert_eq!(entries[0].source, "conversation");
}
/// The recap and last-turn summary ride the session-list wire and land on
/// the picker entry so the expanded card can show them.
#[test]
fn picker_parses_last_recap_and_last_turn_summary() {
    let recent = chrono::Utc::now().to_rfc3339();
    let payload = serde_json::json!({
            "sessions": [{
                "sessionId": "s_recap",
                "cwd": "/Users/me/echoagent",
                "summary": "Auth refactor",
                "source": "local",
                "updatedAt": recent,
                "lastTurnSummary": "Wired retries into billing",
                "lastRecap": "Where we left off: auth refactor across the API"
            }]
        });
    let entries = parse_session_picker_entries(&payload);
    assert_eq!(entries.len(), 1);
    assert_eq!(
            entries[0].last_turn_summary.as_deref(),
            Some("Wired retries into billing")
        );
    assert_eq!(
            entries[0].last_recap.as_deref(),
            Some("Where we left off: auth refactor across the API")
        );
}
/// Canary: the empty-summary drop still applies to Build rows.
#[test]
fn picker_still_drops_build_row_with_empty_summary() {
    let payload = serde_json::json!({
            "sessions": [{
                "sessionId": "local_empty",
                "cwd": "/nonexistent/effects-test",
                "summary": "",
                "source": "local",
                "updatedAt": "2026-07-01T00:00:00Z"
            }]
        });
    let entries = parse_session_picker_entries(&payload);
    assert!(entries.is_empty(), "empty-summary Build rows stay dropped");
}
#[test]
fn session_list_partial_parses_reasons() {
    let payload = |reason: &str| {
        serde_json::json!({
                "sessions": [],
                "_meta": { "echo.agent/partial": { "conversations": true, "reason": reason } }
            })
    };
    assert_eq!(
            parse_session_list_partial(&payload("no_oauth")),
            Some(ConversationsPartial::NoOauth)
        );
    assert_eq!(
            parse_session_list_partial(&payload("timeout")),
            Some(ConversationsPartial::Timeout)
        );
    assert_eq!(
            parse_session_list_partial(&payload("error")),
            Some(ConversationsPartial::Error)
        );
    assert_eq!(
            parse_session_list_partial(&payload("something_new")),
            Some(ConversationsPartial::Error)
        );
}
#[test]
fn session_list_partial_absent_for_healthy_or_meta_less_responses() {
    let healthy = serde_json::json!({
            "sessions": [],
            "_meta": { "echo.agent/partial": { "conversations": false } }
        });
    assert_eq!(parse_session_list_partial(&healthy), None);
    let legacy = serde_json::json!({ "sessions": [] });
    assert_eq!(parse_session_list_partial(&legacy), None);
}
/// The agent serializes `ExtMethodResult<KillTaskResponse>`: the outcome
/// lives at `result.outcome`. Probing the top level (the pre-fix code)
/// was why the tasks-pane ✗ never removed stale (`not_found`) rows after
/// a session resume.
#[test]
fn parse_kill_outcome_reads_result_envelope() {
    use echo_agent_tools::types::KillOutcome;
    let resp = r#"{"result":{"taskId":"t-1","outcome":"not_found"}}"#;
    assert_eq!(parse_kill_outcome(resp), Some(KillOutcome::NotFound));
    let resp = r#"{"result":{"taskId":"t-1","outcome":"killed"}}"#;
    assert_eq!(parse_kill_outcome(resp), Some(KillOutcome::Killed));
    let resp = r#"{"result":{"taskId":"t-1","outcome":"already_exited"}}"#;
    assert_eq!(parse_kill_outcome(resp), Some(KillOutcome::AlreadyExited));
}
/// Round-trip through the agent's own serializer: what
/// `extensions::task::respond()` produces must parse back to the same
/// typed outcome (guards against the two sides drifting apart).
#[test]
fn parse_kill_outcome_round_trips_agent_serialization() {
    use echo_agent_runtime::extensions::task::KillTaskResponse;
    use echo_agent_runtime::session::result::ExtMethodResult;
    use echo_agent_tools::types::KillOutcome;
    let wire = serde_json::to_string(
            &ExtMethodResult::success(KillTaskResponse {
                task_id: "t-1".into(),
                outcome: KillOutcome::NotFound,
            }),
        )
        .unwrap();
    assert_eq!(parse_kill_outcome(&wire), Some(KillOutcome::NotFound));
}
/// Error envelopes and malformed payloads yield `None` (clear pending
/// state, keep the row).
#[test]
fn parse_kill_outcome_none_for_error_or_malformed() {
    assert_eq!(
            parse_kill_outcome(r#"{"result":null,"error":"session not found"}"#),
            None
        );
    assert_eq!(parse_kill_outcome("not json"), None);
    assert_eq!(parse_kill_outcome("{}"), None);
    assert_eq!(
            parse_kill_outcome(r#"{"result":{"taskId":"t-1","outcome":"exploded"}}"#),
            None
        );
}
/// Typed `outcome`: `Cancelled` → `StoppedLive`; `AlreadyFinished` /
/// `NotFound` → `NothingLive` (carrying the real status when known).
#[test]
fn parse_subagent_kill_outcome_reads_typed_outcome() {
    assert!(matches!(
            parse_subagent_kill_outcome(
                r#"{"result":{"subagentId":"sa-1","cancelled":true,"outcome":{"kind":"cancelled"}}}"#
            ),
            SubagentKillOutcome::StoppedLive
        ));
    assert!(matches!(
            parse_subagent_kill_outcome(
                r#"{"result":{"subagentId":"sa-1","cancelled":false,"outcome":{"kind":"already_finished","status":"completed"}}}"#
            ),
            SubagentKillOutcome::NothingLive { status: Some(s) } if s == "completed"
        ));
    assert!(matches!(
            parse_subagent_kill_outcome(
                r#"{"result":{"subagentId":"sa-1","cancelled":false,"outcome":{"kind":"not_found"}}}"#
            ),
            SubagentKillOutcome::NothingLive { status: None }
        ));
}
/// An older shell sends no `outcome`; the parser falls back to the legacy
/// `cancelled` bool (true → `StoppedLive`, false → `NothingLive`).
#[test]
fn parse_subagent_kill_outcome_falls_back_to_legacy_bool() {
    assert!(matches!(
            parse_subagent_kill_outcome(r#"{"result":{"subagentId":"sa-1","cancelled":true}}"#),
            SubagentKillOutcome::StoppedLive
        ));
    assert!(matches!(
            parse_subagent_kill_outcome(r#"{"result":{"subagentId":"sa-1","cancelled":false}}"#),
            SubagentKillOutcome::NothingLive { status: None }
        ));
}
/// An unknown future `kind` deserializes to `Unknown` (via `#[serde(other)]`)
/// and falls back to the always-present `cancelled` bool — not `RpcFailed`,
/// which would leave the row stuck.
#[test]
fn parse_subagent_kill_outcome_unknown_kind_falls_back_to_legacy_bool() {
    assert!(matches!(
            parse_subagent_kill_outcome(
                r#"{"result":{"subagentId":"sa-1","cancelled":true,"outcome":{"kind":"some_future_kind"}}}"#
            ),
            SubagentKillOutcome::StoppedLive
        ));
    assert!(matches!(
            parse_subagent_kill_outcome(
                r#"{"result":{"subagentId":"sa-1","cancelled":false,"outcome":{"kind":"some_future_kind"}}}"#
            ),
            SubagentKillOutcome::NothingLive { status: None }
        ));
}
/// Round-trip through the agent's own serializer guards the two sides
/// against drifting apart.
#[test]
fn parse_subagent_kill_outcome_round_trips_agent_serialization() {
    use echo_agent_runtime::extensions::task::{
        CancelSubagentResponse, SubagentCancelOutcomeDto,
    };
    let wire = serde_json::to_string(
            &ExtMethodResult::success(CancelSubagentResponse {
                subagent_id: "sa-1".into(),
                cancelled: false,
                outcome: Some(SubagentCancelOutcomeDto::AlreadyFinished {
                    status: "failed".into(),
                }),
            }),
        )
        .unwrap();
    assert!(matches!(
            parse_subagent_kill_outcome(&wire),
            SubagentKillOutcome::NothingLive { status: Some(s) } if s == "failed"
        ));
}
/// A top-level payload (no `result` envelope), error envelopes, and
/// malformed payloads are a failed RPC (`RpcFailed`) — the caller must NOT
/// finalize a possibly-live row.
#[test]
fn parse_subagent_kill_outcome_rpc_failed_for_error_or_malformed() {
    assert!(matches!(
            parse_subagent_kill_outcome(r#"{"cancelled":true}"#),
            SubagentKillOutcome::RpcFailed
        ));
    assert!(matches!(
            parse_subagent_kill_outcome(r#"{"result":null,"error":"session not found"}"#),
            SubagentKillOutcome::RpcFailed
        ));
    assert!(matches!(
            parse_subagent_kill_outcome("not json"),
            SubagentKillOutcome::RpcFailed
        ));
    assert!(matches!(
            parse_subagent_kill_outcome("{}"),
            SubagentKillOutcome::RpcFailed
        ));
}
/// Image-bearing interjections carry the blocks as a `content` array.
#[test]
fn interject_params_carry_content_when_blocks_present() {
    let sid = acp::SessionId::new("s1");
    let blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(
            "look at [Image #1]",
        ))];
    let params = build_interject_params(
        &sid,
        "look at [Image #1]",
        "i1",
        Some(blocks.as_slice()),
    );
    let content = params["content"].as_array().expect("content array");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["text"], "look at [Image #1]");
}
/// A billing config with every field unset, for use as a base in
/// `credit_balance_from_config` tests via struct-update syntax.
fn empty_billing_config() -> BillingConfig {
    BillingConfig {
        credit_usage_percent: None,
        current_period: None,
        monthly_limit: None,
        used: None,
        on_demand_cap: None,
        on_demand_used: None,
        prepaid_balance: None,
        is_unified_billing_user: None,
        billing_period_start: None,
        billing_period_end: None,
        history: vec![],
    }
}
#[test]
fn credit_balance_prefers_credit_usage_percent_over_limit_used() {
    let c = BillingConfig {
        credit_usage_percent: Some(42.0),
        monthly_limit: Some(Cent { val: 10_000 }),
        used: Some(Cent { val: 9_000 }),
        ..empty_billing_config()
    };
    assert_eq!(credit_balance_from_config(c).usage_pct, 42.0);
}
#[test]
fn credit_balance_forwards_is_unified_billing_user() {
    let c = BillingConfig {
        is_unified_billing_user: Some(true),
        ..empty_billing_config()
    };
    assert_eq!(
            credit_balance_from_config(c).is_unified_billing_user,
            Some(true)
        );
    assert_eq!(
            credit_balance_from_config(empty_billing_config()).is_unified_billing_user,
            None
        );
}
#[test]
fn credit_balance_falls_back_to_limit_used_when_percent_absent() {
    let c = BillingConfig {
        monthly_limit: Some(Cent { val: 10_000 }),
        used: Some(Cent { val: 2_500 }),
        ..empty_billing_config()
    };
    assert_eq!(credit_balance_from_config(c).usage_pct, 25.0);
}
/// Match production: RFC 3339 → user's local wall-clock (no zone label).
fn expected_period_end_display(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .expect("test fixture is valid RFC 3339")
        .with_timezone(&chrono::Local)
        .format("%B %-d, %H:%M")
        .to_string()
}
#[test]
fn credit_balance_prefers_current_period_end_over_billing_period_end() {
    let end = "2026-06-08T20:00:00Z";
    let c = BillingConfig {
        credit_usage_percent: Some(10.0),
        current_period: Some(UsagePeriod {
            period_type: Some("USAGE_PERIOD_TYPE_WEEKLY".into()),
            start: Some("2026-06-01T00:00:00Z".into()),
            end: Some(end.into()),
        }),
        billing_period_end: Some("2026-07-01T20:00:00Z".into()),
        ..empty_billing_config()
    };
    assert_eq!(
            credit_balance_from_config(c).period_end_display.as_deref(),
            Some(expected_period_end_display(end).as_str())
        );
}
#[test]
fn credit_balance_period_end_uses_local_timezone() {
    let winter = "2026-01-15T20:00:00Z";
    let summer = "2026-07-15T20:00:00Z";
    let winter_cfg = BillingConfig {
        billing_period_end: Some(winter.into()),
        ..empty_billing_config()
    };
    let summer_cfg = BillingConfig {
        billing_period_end: Some(summer.into()),
        ..empty_billing_config()
    };
    assert_eq!(
            credit_balance_from_config(winter_cfg)
                .period_end_display
                .as_deref(),
            Some(expected_period_end_display(winter).as_str())
        );
    assert_eq!(
            credit_balance_from_config(summer_cfg)
                .period_end_display
                .as_deref(),
            Some(expected_period_end_display(summer).as_str())
        );
    assert_ne!(
            expected_period_end_display(winter),
            expected_period_end_display(summer)
        );
}
#[test]
fn credit_balance_falls_back_to_billing_period_end() {
    let end = "2026-07-01T20:00:00Z";
    let c = BillingConfig {
        billing_period_end: Some(end.into()),
        ..empty_billing_config()
    };
    assert_eq!(
            credit_balance_from_config(c).period_end_display.as_deref(),
            Some(expected_period_end_display(end).as_str())
        );
}
#[test]
fn credit_balance_period_end_falls_back_when_current_period_has_no_end() {
    let end = "2026-07-01T20:00:00Z";
    let c = BillingConfig {
        current_period: Some(UsagePeriod {
            period_type: None,
            start: Some("2026-06-01T00:00:00Z".into()),
            end: None,
        }),
        billing_period_end: Some(end.into()),
        ..empty_billing_config()
    };
    assert_eq!(
            credit_balance_from_config(c).period_end_display.as_deref(),
            Some(expected_period_end_display(end).as_str())
        );
}
#[test]
fn credit_balance_period_end_none_when_unavailable() {
    assert!(
            credit_balance_from_config(empty_billing_config())
                .period_end_display
                .is_none()
        );
}
#[test]
fn credit_balance_clamps_new_percent_above_100() {
    let c = BillingConfig {
        credit_usage_percent: Some(150.0),
        ..empty_billing_config()
    };
    assert_eq!(credit_balance_from_config(c).usage_pct, 100.0);
}
#[test]
fn credit_balance_clamps_legacy_used_above_limit() {
    let c = BillingConfig {
        monthly_limit: Some(Cent { val: 1_000 }),
        used: Some(Cent { val: 2_500 }),
        ..empty_billing_config()
    };
    assert_eq!(credit_balance_from_config(c).usage_pct, 100.0);
}
#[test]
fn credit_balance_effective_equals_usage_when_no_on_demand() {
    let c = BillingConfig {
        credit_usage_percent: Some(40.0),
        ..empty_billing_config()
    };
    let bal = credit_balance_from_config(c);
    assert!(!bal.pay_as_you_go);
    assert_eq!(bal.on_demand_cap_cents, None);
    assert_eq!(bal.effective_usage_pct, 40.0);
}
#[test]
fn credit_balance_effective_uses_on_demand_ratio_when_included_exhausted() {
    let c = BillingConfig {
        credit_usage_percent: Some(100.0),
        on_demand_cap: Some(Cent { val: 5_000 }),
        on_demand_used: Some(Cent { val: 1_000 }),
        ..empty_billing_config()
    };
    let bal = credit_balance_from_config(c);
    assert!(bal.pay_as_you_go);
    assert_eq!(bal.usage_pct, 100.0);
    assert_eq!(bal.effective_usage_pct, 20.0);
    assert_eq!(bal.on_demand_cap_cents, Some(5_000));
    assert_eq!(bal.on_demand_used_cents, Some(1_000));
}
#[test]
fn parse_auto_topup_present_rule_resolves() {
    let v = serde_json::json!({
            "rule": {"enabled": true, "topupAmount": {"val": 2000}, "maxAmountPerMonth": {"val": 10000}}
        });
    match parse_auto_topup_response(&v) {
        crate::views::credit_bar::AutoTopupFetch::Resolved(at) => {
            assert!(at.enabled);
            assert_eq!(at.topup_amount_cents, Some(2000));
            assert_eq!(at.max_amount_cents, Some(10000));
        }
        other => panic!("expected Resolved, got {other:?}"),
    }
}
#[test]
fn parse_auto_topup_empty_body_resolves_to_disabled() {
    for v in [serde_json::json!({}), serde_json::json!({ "rule": null })] {
        match parse_auto_topup_response(&v) {
            crate::views::credit_bar::AutoTopupFetch::Resolved(at) => {
                assert!(!at.enabled);
            }
            other => panic!("expected Resolved(disabled), got {other:?}"),
        }
    }
}
#[test]
fn parse_auto_topup_rule_without_enabled_is_disabled() {
    let v = serde_json::json!({ "rule": {"topupAmount": {"val": 500}} });
    match parse_auto_topup_response(&v) {
        crate::views::credit_bar::AutoTopupFetch::Resolved(at) => {
            assert!(!at.enabled);
            assert_eq!(at.topup_amount_cents, Some(500));
        }
        other => panic!("expected Resolved(disabled), got {other:?}"),
    }
}
#[test]
fn parse_auto_topup_malformed_body_is_unchanged() {
    for v in [serde_json::json!(null), serde_json::json!(42)] {
        match parse_auto_topup_response(&v) {
            crate::views::credit_bar::AutoTopupFetch::Unchanged => {}
            other => panic!("expected Unchanged, got {other:?}"),
        }
    }
}
#[test]
fn credit_balance_effective_tracks_included_for_new_shape_under_100() {
    let c = BillingConfig {
        credit_usage_percent: Some(95.0),
        on_demand_cap: Some(Cent { val: 5_000 }),
        on_demand_used: Some(Cent { val: 0 }),
        ..empty_billing_config()
    };
    let bal = credit_balance_from_config(c);
    assert!(bal.pay_as_you_go);
    assert_eq!(bal.effective_usage_pct, 95.0);
}
#[test]
fn credit_balance_effective_blends_budget_for_legacy_shape_under_100() {
    let c = BillingConfig {
        monthly_limit: Some(Cent { val: 10_000 }),
        used: Some(Cent { val: 5_000 }),
        on_demand_cap: Some(Cent { val: 10_000 }),
        on_demand_used: Some(Cent { val: 0 }),
        ..empty_billing_config()
    };
    let bal = credit_balance_from_config(c);
    assert!(bal.pay_as_you_go);
    assert_eq!(bal.usage_pct, 50.0);
    assert_eq!(bal.effective_usage_pct, 25.0);
}
#[test]
fn parse_worktree_restore_payload_full() {
    use echo_agent_workspace::session::git::RestoreDegree;
    let value = serde_json::json!({
            "codeRestored": true,
            "restoreSummary": "checked out abc12345, staged: true, unstaged: false, untracked: 3",
            "restoreDegree": "full",
        });
    let (restored, summary, degree) = parse_worktree_restore_payload(&value);
    assert!(restored);
    assert_eq!(degree, Some(RestoreDegree::Full));
    assert!(summary.unwrap().contains("staged: true"));
}
#[test]
fn parse_worktree_restore_payload_head_only() {
    use echo_agent_workspace::session::git::RestoreDegree;
    let value = serde_json::json!({
            "codeRestored": true,
            "restoreSummary": "checked out abc (session registry disabled — staged/unstaged/untracked not restored)",
            "restoreDegree": "head_only",
        });
    let (_, _, degree) = parse_worktree_restore_payload(&value);
    assert_eq!(degree, Some(RestoreDegree::HeadOnly));
}
#[test]
fn parse_worktree_restore_payload_missing_fields() {
    let value = serde_json::json!({ "codeRestored": false });
    let (restored, summary, degree) = parse_worktree_restore_payload(&value);
    assert!(!restored);
    assert!(summary.is_none());
    assert!(degree.is_none());
}
/// A typo / unknown variant must parse as `None` rather than
/// silently round-tripping a bogus value.
#[test]
fn parse_worktree_restore_payload_rejects_unknown_degree() {
    let value = serde_json::json!({
            "codeRestored": true,
            "restoreSummary": "x",
            "restoreDegree": "full_",
        });
    let (_, _, degree) = parse_worktree_restore_payload(&value);
    assert!(degree.is_none(), "typo must produce None");
}
#[test]
fn parse_session_load_restore_meta_full_shape() {
    use echo_agent_workspace::session::git::RestoreDegree;
    let meta = serde_json::json!({
            "codeRestore": {
                "restored": true,
                "summary": "checked out abc12345",
                "degree": "head_only",
            }
        });
    let (restored, summary, degree) = parse_session_load_restore_meta(meta.as_object());
    assert!(restored);
    assert_eq!(summary.as_deref(), Some("checked out abc12345"));
    assert_eq!(degree, Some(RestoreDegree::HeadOnly));
}
#[test]
fn parse_session_load_restore_meta_absent_returns_false() {
    let (restored, summary, degree) = parse_session_load_restore_meta(None);
    assert!(!restored);
    assert!(summary.is_none());
    assert!(degree.is_none());
}
#[test]
fn parse_session_load_restore_meta_no_coderestore_key() {
    let meta = serde_json::json!({ "other": 1 });
    let (restored, summary, degree) = parse_session_load_restore_meta(meta.as_object());
    assert!(!restored);
    assert!(summary.is_none());
    assert!(degree.is_none());
}
/// Parser must reject unknown degree strings in the meta path.
#[test]
fn parse_session_load_restore_meta_rejects_unknown_degree() {
    let meta = serde_json::json!({
            "codeRestore": { "restored": true, "summary": "x", "degree": "weird" }
        });
    let (_, _, degree) = parse_session_load_restore_meta(meta.as_object());
    assert!(degree.is_none());
}
/// Unknown keys return a descriptive error.
#[tokio::test]
async fn persist_setting_unknown_key_returns_err() {
    use crate::settings::SettingValue;
    let result = persist_setting("not-a-real-setting", SettingValue::Bool(true)).await;
    match result {
        Err(msg) => {
            assert!(
                msg.contains("unknown setting key"),
                "expected error to mention unknown setting key, got: {msg}",
            )
        }
        Ok(()) => panic!("expected Err for unknown key"),
    }
}
/// Type-mismatch returns Err (not panic) for spawned-task safety.
#[tokio::test]
async fn persist_setting_type_mismatch_errors_compact_mode() {
    use crate::settings::SettingValue;
    let r = persist_setting("compact_mode", SettingValue::String("nope".into())).await;
    let err = r.expect_err("compact_mode with String payload must return Err");
    assert!(
            err.contains("persist_setting(compact_mode) expected Bool"),
            "error message must mention key + expected kind, got: {err}",
        );
}
/// Type-mismatch for `show_timestamps`.
#[tokio::test]
async fn persist_setting_type_mismatch_errors_show_timestamps() {
    use crate::settings::SettingValue;
    let r = persist_setting("show_timestamps", SettingValue::String("nope".into()))
        .await;
    let err = r.expect_err("show_timestamps with String payload must return Err");
    assert!(
            err.contains("persist_setting(show_timestamps) expected Bool"),
            "error message must mention key + expected kind, got: {err}",
        );
}
/// Type-mismatch for `show_timeline`.
#[tokio::test]
async fn persist_setting_type_mismatch_errors_show_timeline() {
    use crate::settings::SettingValue;
    let r = persist_setting("show_timeline", SettingValue::String("nope".into())).await;
    let err = r.expect_err("show_timeline with String payload must return Err");
    assert!(
            err.contains("persist_setting(show_timeline) expected Bool"),
            "error message must mention key + expected kind, got: {err}",
        );
}
#[tokio::test]
async fn persist_setting_type_mismatch_errors_page_flip_on_send() {
    use crate::settings::SettingValue;
    let r = persist_setting("page_flip_on_send", SettingValue::String("nope".into()))
        .await;
    let err = r.expect_err("page_flip_on_send with String payload must return Err");
    assert!(
            err.contains("persist_setting(page_flip_on_send) expected Bool"),
            "got: {err}",
        );
}
#[tokio::test]
async fn persist_setting_type_mismatch_errors_confirm_before_rewind() {
    use crate::settings::SettingValue;
    let r = persist_setting("confirm_before_rewind", SettingValue::String("nope".into()))
        .await;
    let err = r.expect_err("confirm_before_rewind with String payload must return Err");
    assert!(
            err.contains("persist_setting(confirm_before_rewind) expected Bool"),
            "got: {err}",
        );
}
#[tokio::test]
async fn persist_setting_type_mismatch_errors_combine_queued_prompts() {
    use crate::settings::SettingValue;
    let r = persist_setting(
            "combine_queued_prompts",
            SettingValue::String("nope".into()),
        )
        .await;
    let err = r.expect_err("combine_queued_prompts with String payload must return Err");
    assert!(
            err.contains("persist_setting(combine_queued_prompts) expected Bool"),
            "got: {err}",
        );
}
/// Type-mismatch for `simple_mode`.
#[tokio::test]
async fn persist_setting_type_mismatch_errors_simple_mode() {
    use crate::settings::SettingValue;
    let r = persist_setting("simple_mode", SettingValue::Int(42)).await;
    let err = r.expect_err("simple_mode with Int payload must return Err");
    assert!(
            err.contains("persist_setting(simple_mode) expected Bool"),
            "error message must mention key + expected kind, got: {err}",
        );
}
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
/// Spawn a fake ACP agent that counts `echo.agent/yolo_mode_changed`
/// notifications. Exits when the channel closes.
fn spawn_fake_acp_agent(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpAgentMessage>,
) -> Arc<AtomicUsize> {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let echo_agent_acp::AcpAgentMessage::ExtNotification(args) = msg {
                if args.request.method.as_ref() == "echo.agent/yolo_mode_changed" {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                }
                let _ = args.response_tx.send(Ok(()));
            }
        }
    });
    counter
}
/// Redirect `ECHO_AGENT_HOME` to a tempdir for test isolation.
fn setup_echo_agent_home_in_tempdir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir creation");
    unsafe {
        std::env::set_var("ECHO_AGENT_HOME", tmp.path());
    }
    tmp
}
fn register_session_in(root: &std::path::Path, id: &str) -> acp::SessionId {
    use echo_agent_active_sessions::{ActiveSession, register_in};
    let session_id = acp::SessionId::new(id);
    register_in(
            root,
            ActiveSession {
                session_id: session_id.clone(),
                pid: std::process::id(),
                cwd: "/tmp/test".into(),
                opened_at: chrono::Utc::now(),
            },
        )
        .expect("register");
    session_id
}
/// Lock-free: the helper removes the registry entry (the normal path).
#[test]
fn unregister_best_effort_removes_entry_when_lock_free() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sid = register_session_in(dir.path(), "s1");
    unregister_active_session_best_effort_in(dir.path(), &sid);
    assert!(
            echo_agent_active_sessions::list_in(dir.path())
                .expect("list")
                .is_empty(),
            "lock-free unregister must remove the entry",
        );
}
/// Contended: the quit path must skip the shared flock rather than block.
/// The unregister runs on a worker joined against a deadline so a blocking
/// regression fails fast here instead of deadlocking the test binary.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unregister_best_effort_is_nonblocking_under_lock_contention() {
    use std::os::unix::io::AsRawFd;
    use std::sync::mpsc;
    use std::time::Duration;
    let dir = tempfile::tempdir().expect("tempdir");
    let sid = register_session_in(dir.path(), "s1");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join("active_sessions.lock"))
        .expect("open lock");
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let (tx, rx) = mpsc::channel();
    let root = dir.path().to_path_buf();
    let worker = std::thread::spawn(move || {
        unregister_active_session_best_effort_in(&root, &sid);
        let _ = tx.send(());
    });
    let returned = rx.recv_timeout(Duration::from_secs(2)).is_ok();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    worker.join().expect("worker thread");
    assert!(
            returned,
            "contended unregister blocked on the shared flock instead of skipping",
        );
    assert_eq!(
            echo_agent_active_sessions::list_in(dir.path())
                .expect("list")
                .len(),
            1,
            "contended unregister must leave the entry for collect_crashed",
        );
}
/// A real I/O error (uncreatable registry root) is swallowed: the
/// best-effort helper logs and returns instead of panicking.
#[test]
fn unregister_best_effort_swallows_io_error() {
    let file = tempfile::NamedTempFile::new().expect("tempfile");
    let bad_root = file.path().join("not-a-dir");
    unregister_active_session_best_effort_in(&bad_root, &acp::SessionId::new("s1"));
}
/// BestEffort path fires exactly one ACP notification regardless
/// of disk outcome.
#[tokio::test]
async fn persist_permission_mode_acp_notification_fires_once_on_best_effort() {
    use agent_client_protocol as acp;
    let _guard = setup_echo_agent_home_in_tempdir();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let counter = spawn_fake_acp_agent(rx);
    let session_id = Some(acp::SessionId::new(Arc::from("test-session")));
    let result = persist_permission_mode_and_notify(
            "always-approve",
            session_id,
            PermissionModePersist::BestEffort,
            tx,
        )
        .await;
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "ACP `echo.agent/yolo_mode_changed` notification must fire exactly once \
             on BestEffort path (regardless of disk outcome)",
        );
    assert!(
            matches!(
                result,
                TaskResult::SettingPersisted { .. }
                    | TaskResult::SettingPersistFailedBestEffort { .. },
            ),
            "BestEffort path must return SettingPersisted (Ok) or \
             SettingPersistFailedBestEffort (Err), got {result:?}",
        );
}
/// WithRollback: notification count matches disk outcome
/// (1 on Ok, 0 on Err).
#[tokio::test]
async fn persist_permission_mode_acp_notification_gated_on_disk_for_with_rollback() {
    use agent_client_protocol as acp;
    let _guard = setup_echo_agent_home_in_tempdir();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let counter = spawn_fake_acp_agent(rx);
    let session_id = Some(acp::SessionId::new(Arc::from("test-session")));
    let result = persist_permission_mode_and_notify(
            "always-approve",
            session_id,
            PermissionModePersist::WithRollback("ask"),
            tx,
        )
        .await;
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let count = counter.load(Ordering::SeqCst);
    match result {
        TaskResult::SettingPersisted { .. } => {
            assert_eq!(
                    count, 1,
                    "WithRollback + disk Ok must fire ACP notification exactly once",
                );
        }
        TaskResult::SettingPersistFailed { .. } => {
            assert_eq!(
                    count, 0,
                    "WithRollback + disk Err must SUPPRESS the ACP notification \
                     (Issue 3 — keeps agent and pager state consistent on rollback)",
                );
        }
        other => {
            panic!("expected SettingPersisted or SettingPersistFailed, got {other:?}")
        }
    }
}
/// `session_id: None` suppresses ACP notification unconditionally.
#[tokio::test]
async fn persist_permission_mode_no_session_id_suppresses_acp() {
    let _guard = setup_echo_agent_home_in_tempdir();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let counter = spawn_fake_acp_agent(rx);
    let _result = persist_permission_mode_and_notify(
            "always-approve",
            None,
            PermissionModePersist::BestEffort,
            tx,
        )
        .await;
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "session_id=None must suppress the ACP notification — sessionless \
             agents have no ACP channel to notify",
        );
}
/// BestEffort + disk failure must NOT return `SettingPersisted`.
#[tokio::test]
async fn persist_permission_mode_best_effort_failure_returns_dedicated_variant() {
    let _guard = setup_echo_agent_home_in_tempdir();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _counter = spawn_fake_acp_agent(rx);
    let result = persist_permission_mode_and_notify(
            "always-approve",
            None,
            PermissionModePersist::BestEffort,
            tx,
        )
        .await;
    match result {
        TaskResult::SettingPersisted { key, value } => {
            assert_eq!(key, "permission_mode");
            assert_eq!(value, crate::settings::SettingValue::Enum("always-approve"));
        }
        TaskResult::SettingPersistFailedBestEffort { key, error: _ } => {
            assert_eq!(
                    key, "permission_mode",
                    "BestEffort failure MUST report key=permission_mode and \
                     NOT lie about success via SettingPersisted",
                );
        }
        other => {
            panic!(
                "BestEffort must return SettingPersisted (Ok) or \
                 SettingPersistFailedBestEffort (Err), got {other:?} — \
                 a regression to `SettingPersisted` on failure is Round-2 Issue 2",
            )
        }
    }
}
/// (Err, WithRollback) → SUPPRESS for all canonicals.
#[test]
fn should_send_yolo_acp_with_rollback_suppresses_on_err() {
    let result: Result<(), String> = Err("simulated disk failure".to_string());
    assert!(
            !should_send_yolo_acp_notification(&result, PermissionModePersist::WithRollback("ask")),
            "WithRollback + Err MUST suppress the ACP notification",
        );
    assert!(
            !should_send_yolo_acp_notification(
                &result,
                PermissionModePersist::WithRollback("always-approve")
            ),
            "WithRollback + Err MUST suppress regardless of the prior canonical",
        );
    assert!(
            !should_send_yolo_acp_notification(
                &result,
                PermissionModePersist::WithRollback("default")
            ),
            "WithRollback + Err MUST suppress for the 'default' prior canonical too",
        );
}
/// (Ok, WithRollback) → FIRE for all canonicals.
#[test]
fn should_send_yolo_acp_with_rollback_fires_on_ok() {
    let ok: Result<(), String> = Ok(());
    assert!(
            should_send_yolo_acp_notification(&ok, PermissionModePersist::WithRollback("ask")),
            "WithRollback + Ok must fire the ACP notification (happy path)",
        );
    assert!(
            should_send_yolo_acp_notification(
                &ok,
                PermissionModePersist::WithRollback("always-approve")
            ),
            "WithRollback + Ok fires regardless of the prior canonical",
        );
    assert!(
            should_send_yolo_acp_notification(&ok, PermissionModePersist::WithRollback("default")),
            "WithRollback + Ok fires for 'default' prior canonical too",
        );
}
#[test]
fn should_send_yolo_acp_best_effort_fires_on_both_outcomes() {
    let ok: Result<(), String> = Ok(());
    let err: Result<(), String> = Err("simulated".to_string());
    assert!(
            should_send_yolo_acp_notification(&ok, PermissionModePersist::BestEffort),
            "BestEffort + Ok must notify",
        );
    assert!(
            should_send_yolo_acp_notification(&err, PermissionModePersist::BestEffort),
            "BestEffort + Err must STILL notify (cycle_mode contract \
             — the cycle_mode state machine doesn't have a clean \
             single-field rollback)",
        );
}
#[test]
fn route_permission_mode_result_ok_returns_persisted() {
    let result = route_permission_mode_result(
        Ok(()),
        PermissionModePersist::WithRollback("ask"),
        "always-approve",
    );
    match result {
        TaskResult::SettingPersisted { key, value } => {
            assert_eq!(key, "permission_mode");
            assert_eq!(value, crate::settings::SettingValue::Enum("always-approve"));
        }
        other => panic!("Ok must return SettingPersisted, got {other:?}"),
    }
}
#[test]
fn route_permission_mode_result_err_with_rollback_off_routes_to_failed() {
    let result = route_permission_mode_result(
        Err("simulated".to_string()),
        PermissionModePersist::WithRollback("ask"),
        "always-approve",
    );
    match result {
        TaskResult::SettingPersistFailed { key, rollback_value, error } => {
            assert_eq!(key, "permission_mode");
            assert_eq!(rollback_value, crate::settings::SettingValue::Enum("ask"));
            assert_eq!(error, "simulated");
        }
        other => {
            panic!("WithRollback + Err must return SettingPersistFailed, got {other:?}")
        }
    }
}
#[test]
fn route_permission_mode_result_err_with_rollback_on_routes_to_failed() {
    let result = route_permission_mode_result(
        Err("simulated".to_string()),
        PermissionModePersist::WithRollback("always-approve"),
        "ask",
    );
    match result {
        TaskResult::SettingPersistFailed { key, rollback_value, error } => {
            assert_eq!(key, "permission_mode");
            assert_eq!(
                    rollback_value,
                    crate::settings::SettingValue::Enum("always-approve"),
                    "prev_canonical='always-approve' must route to canonical \
                     'always-approve' for rollback",
                );
            assert_eq!(error, "simulated");
        }
        other => {
            panic!("WithRollback + Err must return SettingPersistFailed, got {other:?}")
        }
    }
}
/// Rollback preserves "default" canonical (not collapsed to "ask").
#[test]
fn route_permission_mode_result_err_with_rollback_default_routes_to_failed() {
    let result = route_permission_mode_result(
        Err("simulated".to_string()),
        PermissionModePersist::WithRollback("default"),
        "always-approve",
    );
    match result {
        TaskResult::SettingPersistFailed { key, rollback_value, error } => {
            assert_eq!(key, "permission_mode");
            assert_eq!(
                    rollback_value,
                    crate::settings::SettingValue::Enum("default"),
                    "PR 11: prev_canonical='default' must roll back to canonical 'default', \
                     NOT collapse onto 'ask' through a bool projection",
                );
            assert_eq!(error, "simulated");
        }
        other => {
            panic!("WithRollback + Err must return SettingPersistFailed, got {other:?}")
        }
    }
}
/// Ok path preserves "default" canonical verbatim.
#[test]
fn route_permission_mode_result_ok_preserves_default_canonical() {
    let result = route_permission_mode_result(
        Ok(()),
        PermissionModePersist::WithRollback("ask"),
        "default",
    );
    match result {
        TaskResult::SettingPersisted { key, value } => {
            assert_eq!(key, "permission_mode");
            assert_eq!(
                    value,
                    crate::settings::SettingValue::Enum("default"),
                    "PR 11: 'default' canonical must survive the route fn intact",
                );
        }
        other => panic!("Ok must return SettingPersisted, got {other:?}"),
    }
}
/// `(Err, BestEffort)` must NOT return `SettingPersisted`.
#[test]
fn route_permission_mode_result_err_best_effort_routes_to_dedicated_variant() {
    let result = route_permission_mode_result(
        Err("simulated".to_string()),
        PermissionModePersist::BestEffort,
        "always-approve",
    );
    match result {
        TaskResult::SettingPersistFailedBestEffort { key, error } => {
            assert_eq!(key, "permission_mode");
            assert_eq!(error, "simulated");
        }
        TaskResult::SettingPersisted { .. } => {
            panic!(
                "BestEffort + Err MUST NOT return SettingPersisted — that would lie about \
                 success on disk failure (Round-2 Issue 2 regression)",
            )
        }
        other => {
            panic!("BestEffort + Err must return SettingPersistFailedBestEffort, got {other:?}",)
        }
    }
}
#[test]
fn marketplace_outcome_succeeded_only_accepts_success_status() {
    use echo_agent_hooks_plugins_types::{ActionOutcome, OutcomeStatus};
    let success = ActionOutcome {
        status: OutcomeStatus::Success,
        message: "updated".into(),
        requires_reload: true,
        requires_restart: false,
    };
    let failed = ActionOutcome {
        status: OutcomeStatus::InternalError,
        message: "failed".into(),
        requires_reload: false,
        requires_restart: false,
    };
    assert!(marketplace_outcome_succeeded(&success));
    assert!(!marketplace_outcome_succeeded(&failed));
}
#[tokio::test]
async fn check_marketplace_updates_dispatches_update_and_skips_failed_notifications() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use echo_agent_acp::AcpAgentMessage;
    use echo_agent_hooks_plugins_types::{ActionOutcome, MarketplaceAction, OutcomeStatus};
    let action_calls = Arc::new(AtomicUsize::new(0));
    let saw_update = Arc::new(AtomicBool::new(false));
    let saw_wrong_action = Arc::new(AtomicBool::new(false));
    let saw_success_notification = Arc::new(AtomicBool::new(false));
    let action_calls_for_task = action_calls.clone();
    let saw_update_for_task = saw_update.clone();
    let saw_wrong_action_for_task = saw_wrong_action.clone();
    let saw_success_notification_for_task = saw_success_notification.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let AcpAgentMessage::ExtMethod(args) = msg {
                match args.request.method.as_ref() {
                    "echo.agent/marketplace/list" => {
                        let response = serde_json::json!({
                                "result": {
                                    "sources": [{
                                        "sourceName": "test-source",
                                        "sourceKind": "git",
                                        "sourceUrlOrPath": "https://example.com/plugins.git",
                                        "plugins": [{
                                            "name": "test-plugin",
                                            "version": "2.0.0",
                                            "description": null,
                                            "category": null,
                                            "author": null,
                                            "tags": [],
                                            "relativePath": "plugins/test-plugin",
                                            "skillCount": 0,
                                            "hasHooks": false,
                                            "hasAgents": false,
                                            "hasMcp": false,
                                            "installStatus": "update_available",
                                            "installedVersion": "1.0.0"
                                        }],
                                        "error": null
                                    }]
                                }
                            });
                        let raw = serde_json::value::RawValue::from_string(
                                response.to_string(),
                            )
                            .expect("serialize marketplace list response");
                        let _ = args
                            .response_tx
                            .send(Ok(acp::ExtResponse::new(Arc::from(raw))));
                    }
                    "echo.agent/marketplace/action" => {
                        action_calls_for_task.fetch_add(1, Ordering::SeqCst);
                        let req: echo_agent_hooks_plugins_types::MarketplaceActionRequest = serde_json::from_str(
                                args.request.params.get(),
                            )
                            .expect("parse marketplace action request");
                        match req.action {
                            MarketplaceAction::Update {
                                source_url_or_path,
                                plugin_relative_path,
                            } if source_url_or_path == "https://example.com/plugins.git"
                                && plugin_relative_path == "plugins/test-plugin" => {
                                saw_update_for_task.store(true, Ordering::SeqCst);
                            }
                            _ => {
                                saw_wrong_action_for_task.store(true, Ordering::SeqCst);
                            }
                        }
                        let outcome = ActionOutcome {
                            status: OutcomeStatus::InternalError,
                            message: "update failed".into(),
                            requires_reload: false,
                            requires_restart: false,
                        };
                        let response = serde_json::json!({ "result": outcome });
                        let raw = serde_json::value::RawValue::from_string(
                                response.to_string(),
                            )
                            .expect("serialize marketplace action response");
                        let _ = args
                            .response_tx
                            .send(Ok(acp::ExtResponse::new(Arc::from(raw))));
                    }
                    "echo.agent/plugins/notify-updates" => {
                        saw_success_notification_for_task.store(true, Ordering::SeqCst);
                        let raw = serde_json::value::RawValue::from_string("{}".into())
                            .expect("serialize notify response");
                        let _ = args
                            .response_tx
                            .send(Ok(acp::ExtResponse::new(Arc::from(raw))));
                    }
                    _ => {
                        let raw = serde_json::value::RawValue::from_string("{}".into())
                            .expect("serialize fallback response");
                        let _ = args
                            .response_tx
                            .send(Ok(acp::ExtResponse::new(Arc::from(raw))));
                    }
                }
            }
        }
    });
    let mut tasks = JoinSet::new();
    let (progress_tx, _progress_rx) = tokio::sync::mpsc::unbounded_channel();
    execute(
        Effect::CheckMarketplaceUpdates {
            agent_id: AgentId(7),
            session_id: acp::SessionId::new(Arc::from("test-session")),
        },
        &mut tasks,
        &tx,
        Path::new("."),
        &SessionFlags::default(),
        &progress_tx,
    );
    let result = tasks
        .join_next()
        .await
        .expect("task should complete")
        .expect("task should not panic");
    match result {
        TaskResult::MarketplaceUpdatesAvailable { agent_id, updates } => {
            assert_eq!(agent_id, AgentId(7));
            assert!(updates.is_empty());
        }
        other => panic!("expected MarketplaceUpdatesAvailable, got {other:?}"),
    }
    assert_eq!(action_calls.load(Ordering::SeqCst), 1);
    assert!(saw_update.load(Ordering::SeqCst));
    assert!(!saw_wrong_action.load(Ordering::SeqCst));
    assert!(!saw_success_notification.load(Ordering::SeqCst));
}
#[tokio::test]
async fn foreign_scan_task_echoes_sequence_without_enabled_sources() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (progress_tx, _progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = JoinSet::new();
    let app_coordinator = crate::app::ForeignScanCoordinator::default();
    app_coordinator.begin_request(41);
    execute(
        Effect::ScanForeignSessions {
            cwd: PathBuf::from("/path/that/must/not/be-read"),
            compat: echo_agent_foreign_sessions::EnabledForeignSessionSources::default(),
            echo_agent_home: PathBuf::from("/path/that/must/not/be-read"),
            coordinator: app_coordinator.clone(),
            seq: 41,
        },
        &mut tasks,
        &tx,
        Path::new("."),
        &SessionFlags::default(),
        &progress_tx,
    );
    match tasks.join_next().await.expect("task").expect("no panic") {
        TaskResult::ForeignSessionsScanned { entries, seq } => {
            assert!(entries.is_empty());
            assert_eq!(seq, 41);
        }
        other => panic!("expected ForeignSessionsScanned, got {other:?}"),
    }
    drop(app_coordinator);
}
#[tokio::test]
async fn foreign_resume_detection_runs_as_task_result() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (progress_tx, _progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = JoinSet::new();
    let (quit, _) = execute(
        Effect::CanonicalizeForeignResumeCwd {
            requested_cwd: PathBuf::from("/path/that/does-not-exist"),
            launch_token: 7,
        },
        &mut tasks,
        &tx,
        Path::new("."),
        &SessionFlags::default(),
        &progress_tx,
    );
    assert!(!quit);
    match tasks.join_next().await.expect("task").expect("no panic") {
        TaskResult::ForeignResumeCwdCanonicalized {
            canonical_cwd,
            launch_token,
            ..
        } => {
            assert!(canonical_cwd.is_none());
            assert_eq!(launch_token, 7);
        }
        other => panic!("expected ForeignResumeCwdCanonicalized, got {other:?}"),
    }
    let canonical_cwd = dunce::canonicalize(tempfile::tempdir().unwrap().path())
        .unwrap();
    let (quit, _) = execute(
        Effect::DetectForeignResumeHint {
            canonical_cwd: canonical_cwd.clone(),
            compat: echo_agent_foreign_sessions::EnabledForeignSessionSources::default(),
            echo_agent_home: PathBuf::from("/path/that/must/not-be-read"),
            launch_token: 8,
        },
        &mut tasks,
        &tx,
        Path::new("."),
        &SessionFlags::default(),
        &progress_tx,
    );
    assert!(!quit);
    match tasks.join_next().await.expect("task").expect("no panic") {
        TaskResult::ForeignResumeHintDetected {
            canonical_cwd: result_cwd,
            launch_token,
            hint,
        } => {
            assert_eq!(result_cwd, canonical_cwd);
            assert_eq!(launch_token, 8);
            assert!(hint.is_none());
        }
        other => panic!("expected ForeignResumeHintDetected, got {other:?}"),
    }
}
/// `FetchSessionList` wire shape: search sends `query` (no `allowRelax`);
/// browse opts into `allowRelax` and parses `echo.agent/listScope`; all
/// outcomes echo `seq`/`query`.
#[tokio::test]
async fn fetch_session_list_pushes_query_and_echoes_seq() {
    use std::sync::{Arc, Mutex};
    use echo_agent_acp::AcpAgentMessage;
    let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let captured_for_task = captured.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let AcpAgentMessage::ExtMethod(args) = msg {
                assert_eq!(args.request.method.as_ref(), "echo.agent/session/list");
                let params: serde_json::Value = serde_json::from_str(
                        args.request.params.get(),
                    )
                    .expect("params JSON");
                let fail = params.get("query").and_then(|q| q.as_str())
                    == Some("fail-me");
                let browse = params.get("query").is_none();
                captured_for_task.lock().unwrap().push(params);
                let body = if fail {
                    serde_json::json!({ "error": "boom" })
                } else if browse {
                    serde_json::json!({
                            "result": {
                                "sessions": [],
                                "_meta": { "echo.agent/listScope": "repo" },
                            }
                        })
                } else {
                    serde_json::json!({ "result": { "sessions": [] } })
                };
                let raw = serde_json::value::RawValue::from_string(body.to_string())
                    .expect("serialize list response");
                let _ = args.response_tx.send(Ok(acp::ExtResponse::new(Arc::from(raw))));
            }
        }
    });
    let (progress_tx, _progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let run = |effect: Effect| {
        let mut tasks = JoinSet::new();
        execute(
            effect,
            &mut tasks,
            &tx,
            Path::new("."),
            &SessionFlags::default(),
            &progress_tx,
        );
        tasks
    };
    let mut tasks = run(Effect::FetchSessionList {
        query: Some("hit".into()),
        seq: 7,
        kind_filter: None,
    });
    match tasks.join_next().await.expect("task").expect("no panic") {
        TaskResult::SessionListLoaded { sessions, scope, seq, query, .. } => {
            assert!(sessions.is_empty());
            assert_eq!(seq, 7, "seq must be echoed, not reconstructed");
            assert_eq!(query.as_deref(), Some("hit"), "query must be echoed");
            assert!(
                    !scope.is_relaxed(),
                    "search responses carry no relaxed scope"
                );
        }
        other => panic!("expected SessionListLoaded, got {other:?}"),
    }
    let mut tasks = run(Effect::FetchSessionList {
        query: None,
        seq: 8,
        kind_filter: None,
    });
    match tasks.join_next().await.expect("task").expect("no panic") {
        TaskResult::SessionListLoaded { scope, seq, query, .. } => {
            assert_eq!(seq, 8);
            assert_eq!(query, None);
            assert!(
                    scope.is_relaxed(),
                    "_meta[\"echo.agent/listScope\"] must parse into the task result"
                );
        }
        other => panic!("expected SessionListLoaded, got {other:?}"),
    }
    let mut tasks = run(Effect::FetchSessionList {
        query: Some("fail-me".into()),
        seq: 9,
        kind_filter: None,
    });
    match tasks.join_next().await.expect("task").expect("no panic") {
        TaskResult::SessionListFailed { error, seq, query } => {
            assert_eq!(error, "boom");
            assert_eq!(seq, 9);
            assert_eq!(
                    query.as_deref(),
                    Some("fail-me"),
                    "failure must echo the query (gates the indicator clear)"
                );
        }
        other => panic!("expected SessionListFailed, got {other:?}"),
    }
    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 3);
    assert_eq!(captured[0]["query"], "hit");
    assert_eq!(captured[0]["limit"], 30);
    assert!(captured[0]["cwd"].is_string());
    assert!(
            captured[0].get("allowRelax").is_none(),
            "search fetches must not opt into relaxing: {:?}",
            captured[0]
        );
    assert!(
            captured[1].get("query").is_none(),
            "plain fetch must not send a query key: {:?}",
            captured[1]
        );
    assert_eq!(
            captured[1]["allowRelax"], true,
            "browse fetches opt into relaxing"
        );
    assert_eq!(captured[2]["query"], "fail-me");
}
#[tokio::test]
async fn fetch_session_list_sends_kind_facet_filter() {
    use std::sync::{Arc, Mutex};
    use echo_agent_acp::AcpAgentMessage;
    let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let captured_for_task = captured.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let AcpAgentMessage::ExtMethod(args) = msg {
                let params: serde_json::Value = serde_json::from_str(
                        args.request.params.get(),
                    )
                    .expect("params JSON");
                captured_for_task.lock().unwrap().push(params);
                let body = serde_json::json!({ "result": { "sessions": [] } });
                let raw = serde_json::value::RawValue::from_string(body.to_string())
                    .expect("ser");
                let _ = args.response_tx.send(Ok(acp::ExtResponse::new(Arc::from(raw))));
            }
        }
    });
    let (progress_tx, _progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = JoinSet::new();
    execute(
        Effect::FetchSessionList {
            query: None,
            seq: 1,
            kind_filter: Some(vec!["build".into()]),
        },
        &mut tasks,
        &tx,
        Path::new("."),
        &SessionFlags::default(),
        &progress_tx,
    );
    let _ = tasks.join_next().await;
    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(
            captured[0]["_meta"]["echo.agent/facetFilters"]["kind"],
            serde_json::json!(["build"])
        );
}
#[tokio::test]
async fn fetch_workflows_list_sends_session_id() {
    use std::sync::{Arc, Mutex};
    use echo_agent_acp::AcpAgentMessage;
    let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let captured_for_task = captured.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let AcpAgentMessage::ExtMethod(args) = msg {
                assert_eq!(args.request.method.as_ref(), "echo.agent/workflows/list");
                let params: serde_json::Value = serde_json::from_str(
                        args.request.params.get(),
                    )
                    .expect("params JSON");
                captured_for_task.lock().unwrap().push(params);
                let body = serde_json::json!({ "result": { "workflows": [] } });
                let raw = serde_json::value::RawValue::from_string(body.to_string())
                    .expect("serialize workflows response");
                let _ = args.response_tx.send(Ok(acp::ExtResponse::new(Arc::from(raw))));
            }
        }
    });
    let session_id = acp::SessionId::new(Arc::from("test-session"));
    let mut tasks = JoinSet::new();
    let (progress_tx, _progress_rx) = tokio::sync::mpsc::unbounded_channel();
    execute(
        Effect::FetchWorkflowsList {
            agent_id: AgentId(3),
            session_id: session_id.clone(),
        },
        &mut tasks,
        &tx,
        Path::new("."),
        &SessionFlags::default(),
        &progress_tx,
    );
    match tasks.join_next().await.expect("task").expect("no panic") {
        TaskResult::WorkflowsListLoaded {
            agent_id,
            session_id: result_session_id,
            result,
        } => {
            assert_eq!(agent_id, AgentId(3));
            assert_eq!(result_session_id, session_id);
            assert!(result.expect("workflows load").is_empty());
        }
        other => panic!("expected WorkflowsListLoaded, got {other:?}"),
    }
    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0]["sessionId"], "test-session");
    assert!(captured[0].get("cwd").is_none());
}
/// The debounce arm must echo `query` and `seq` exactly. Awaits the real
/// 250 ms debounce (tokio's paused clock needs `test-util`, not enabled
/// in this crate).
#[tokio::test]
async fn debounce_session_search_echoes_query_and_seq() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (progress_tx, _progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = JoinSet::new();
    execute(
        Effect::DebounceSessionSearch {
            query: "abc".into(),
            seq: 9,
        },
        &mut tasks,
        &tx,
        Path::new("."),
        &SessionFlags::default(),
        &progress_tx,
    );
    match tasks.join_next().await.expect("task").expect("no panic") {
        TaskResult::SessionSearchDebounceExpired { query, seq } => {
            assert_eq!(query, "abc");
            assert_eq!(seq, 9);
        }
        other => panic!("expected SessionSearchDebounceExpired, got {other:?}"),
    }
}
/// Verify that every profile name produced by `SessionFlags::agent_profile()`
/// is a valid `BuiltinAgentName` that the shell can resolve.
#[test]
fn agent_profile_names_are_valid_builtins() {
    use std::str::FromStr;
    use echo_agent_core::config::BuiltinAgentName;
    let test_cases: &[(SessionFlags, &str)] = &[
        (
            SessionFlags {
                plan_mode: true,
                subagents: true,
                ask_user: false,
                ..Default::default()
            },
            "echo-agent-build-plan",
        ),
        (
            SessionFlags {
                plan_mode: true,
                subagents: false,
                ask_user: false,
                ..Default::default()
            },
            "echo-agent-build-plan-no-subagents",
        ),
        (
            SessionFlags {
                plan_mode: true,
                subagents: true,
                ask_user: true,
                ..Default::default()
            },
            "echo-agent-build-plan",
        ),
        (
            SessionFlags {
                plan_mode: true,
                subagents: false,
                ask_user: true,
                ..Default::default()
            },
            "echo-agent-build-plan-no-subagents",
        ),
        (
            SessionFlags {
                plan_mode: false,
                subagents: false,
                ask_user: true,
                ..Default::default()
            },
            "echo-agent-build-ask-user",
        ),
        (
            SessionFlags {
                plan_mode: false,
                subagents: true,
                ask_user: true,
                ..Default::default()
            },
            "echo-agent-build-ask-user",
        ),
    ];
    for (flags, expected_name) in test_cases {
        let profile = flags.agent_profile();
        assert_eq!(
                profile,
                Some(*expected_name),
                "flags {flags:?} should produce profile {expected_name:?}"
            );
        let builtin = BuiltinAgentName::from_str(expected_name);
        assert!(
                builtin.is_ok(),
                "profile name {expected_name:?} is not a valid BuiltinAgentName: {:?}",
                builtin.err()
            );
    }
}
/// Default flags produce no agent profile (uses echo-agent-build default).
#[test]
fn default_flags_produce_no_profile() {
    let flags = SessionFlags::default();
    assert_eq!(flags.agent_profile(), None);
}
/// --subagents alone produces no profile (echo-agent-build already has TaskTool).
#[test]
fn subagents_without_plan_produces_no_profile() {
    let flags = SessionFlags {
        plan_mode: false,
        subagents: true,
        ask_user: false,
        ..Default::default()
    };
    assert_eq!(flags.agent_profile(), None);
}
/// Neutralize `ECHO_AGENT_AGENT` for the profile-matrix tests below: agent-driven
/// dev shells export it, which flips `to_meta` into the defer-to-shell
/// escape hatch and drops `agentProfile` — the tests would then assert the
/// wrong branch. Empty string counts as unset (`!s.trim().is_empty()`).
/// Callers must be `#[serial_test::serial(ECHO_AGENT_AGENT)]` (process-global env).
fn without_echo_agent_agent() -> crate::test_util::EnvVarGuard {
    crate::test_util::EnvVarGuard::set("ECHO_AGENT_AGENT", "")
}
/// At the runtime defaults (every `--no-*` flag false → every
/// `SessionFlags` bool true via `!args.no_*`), `to_meta()` reflects the
/// full plan profile and no separate `askUserQuestion` toggle.
#[serial_test::serial(ECHO_AGENT_AGENT)]
#[test]
fn runtime_default_flags_produce_plan_meta() {
    let _env = without_echo_agent_agent();
    let flags = SessionFlags {
        plan_mode: true,
        subagents: true,
        ask_user: true,
        ..Default::default()
    };
    let meta = flags.to_meta().unwrap();
    assert_eq!(meta["agentProfile"], "echo-agent-build-plan");
    assert!(meta.get("askUserQuestion").is_none());
    assert_eq!(meta["yoloMode"], false);
}
/// --plan alone produces meta with `agentProfile` only and a
/// `askUserQuestion: false` since `ask_user` is off here.
#[serial_test::serial(ECHO_AGENT_AGENT)]
#[test]
fn plan_only_meta() {
    let _env = without_echo_agent_agent();
    let flags = SessionFlags {
        plan_mode: true,
        subagents: false,
        ask_user: false,
        ..Default::default()
    };
    let meta = flags.to_meta().unwrap();
    assert_eq!(meta["agentProfile"], "echo-agent-build-plan-no-subagents");
    assert_eq!(meta["askUserQuestion"], false);
    assert_eq!(meta["yoloMode"], false);
}
/// --plan --subagents selects the full plan profile.
#[serial_test::serial(ECHO_AGENT_AGENT)]
#[test]
fn plan_with_subagents_meta() {
    let _env = without_echo_agent_agent();
    let flags = SessionFlags {
        plan_mode: true,
        subagents: true,
        ask_user: false,
        ..Default::default()
    };
    let meta = flags.to_meta().unwrap();
    assert_eq!(meta["agentProfile"], "echo-agent-build-plan");
    assert_eq!(meta["askUserQuestion"], false);
    assert_eq!(meta["yoloMode"], false);
}
/// --ask-user alone selects the echo-agent-build-ask-user profile.
#[serial_test::serial(ECHO_AGENT_AGENT)]
#[test]
fn ask_user_alone_meta() {
    let _env = without_echo_agent_agent();
    let flags = SessionFlags {
        plan_mode: false,
        subagents: false,
        ask_user: true,
        ..Default::default()
    };
    let meta = flags.to_meta().unwrap();
    assert_eq!(meta["agentProfile"], "echo-agent-build-ask-user");
    assert!(meta.get("askUserQuestion").is_none());
    assert_eq!(meta["yoloMode"], false);
}
/// --plan --ask-user: plan already includes ask-user; profile is plan.
#[serial_test::serial(ECHO_AGENT_AGENT)]
#[test]
fn plan_with_ask_user_uses_plan_profile() {
    let _env = without_echo_agent_agent();
    let flags = SessionFlags {
        plan_mode: true,
        subagents: false,
        ask_user: true,
        ..Default::default()
    };
    let meta = flags.to_meta().unwrap();
    assert_eq!(meta["agentProfile"], "echo-agent-build-plan-no-subagents");
    assert!(meta.get("askUserQuestion").is_none());
    assert_eq!(meta["yoloMode"], false);
}
/// --no-plan --no-subagents --no-ask-user picks the default profile but
/// must still emit `askUserQuestion: false` so the shell can strip the
/// tool at the builder. Mirrors the runtime: `subagents` toggle alone
/// does not need an `agentProfile` (default `echo-agent-build` already has it).
#[test]
fn subagents_alone_emits_only_ask_user_question_disable() {
    let flags = SessionFlags {
        plan_mode: false,
        subagents: true,
        ask_user: false,
        ..Default::default()
    };
    let meta = flags.to_meta().expect("askUserQuestion=false must produce meta");
    assert!(meta.get("agentProfile").is_none());
    assert_eq!(meta["askUserQuestion"], false);
}
/// All three flags on at the runtime default produce echo-agent-build-plan
/// and no `askUserQuestion` field.
#[serial_test::serial(ECHO_AGENT_AGENT)]
#[test]
fn all_flags_meta() {
    let _env = without_echo_agent_agent();
    let flags = SessionFlags {
        plan_mode: true,
        subagents: true,
        ask_user: true,
        ..Default::default()
    };
    let meta = flags.to_meta().unwrap();
    assert_eq!(meta["agentProfile"], "echo-agent-build-plan");
    assert!(meta.get("askUserQuestion").is_none());
    assert_eq!(meta["yoloMode"], false);
}
/// `--no-ask-user` is the user-discovered bug — the flag must surface
/// as `_meta.askUserQuestion = false` regardless of which profile (if
/// any) the other flags select.
#[test]
fn to_meta_emits_ask_user_question_false_when_disabled() {
    for plan in [false, true] {
        for subagents in [false, true] {
            let flags = SessionFlags {
                plan_mode: plan,
                subagents,
                ask_user: false,
                ..Default::default()
            };
            let meta = flags
                .to_meta()
                .unwrap_or_else(|| {
                    panic!(
                        "ask_user=false must always emit meta (plan={plan}, subagents={subagents})"
                    )
                });
            assert_eq!(
                    meta["askUserQuestion"], false,
                    "askUserQuestion must be false (plan={plan}, subagents={subagents}); meta={meta:?}"
                );
        }
    }
}
/// Symmetric positive control: when `ask_user` is enabled the field is
/// omitted entirely (the shell defaults to enabled when the key is
/// absent — see `parse_ask_user_question_from_meta`).
#[test]
fn to_meta_omits_ask_user_question_when_enabled() {
    for plan in [false, true] {
        for subagents in [false, true] {
            let flags = SessionFlags {
                plan_mode: plan,
                subagents,
                ask_user: true,
                ..Default::default()
            };
            if let Some(meta) = flags.to_meta() {
                assert!(
                        meta.get("askUserQuestion").is_none(),
                        "askUserQuestion must be absent when enabled (plan={plan}, subagents={subagents}); meta={meta:?}"
                    );
            }
        }
    }
}
#[test]
fn to_meta_emits_auto_mode_when_enabled() {
    let flags = SessionFlags {
        auto_mode: true,
        yolo_mode: false,
        ..Default::default()
    };
    let meta = flags.to_meta().expect("auto_mode must emit meta");
    assert_eq!(meta["autoMode"], true);
    assert_eq!(
            meta["yoloMode"], false,
            "yoloMode must be explicitly false, not omitted (absent key falls \
             back to the shell's connect-time default / leader injection)"
        );
}
#[test]
fn create_permission_override_replaces_global_permission_seeds() {
    let flags = SessionFlags {
        yolo_mode: true,
        auto_mode: false,
        ..Default::default()
    };
    let mut meta = flags.to_meta();
    apply_permission_mode_override(&mut meta, Some(PermissionModeKind::Auto));
    let meta = meta.expect("permission metadata");
    assert_eq!(meta["yoloMode"], false);
    assert_eq!(meta["autoMode"], true);
}
/// yoloMode must ride the meta explicitly for BOTH polarities — absent
/// key ≠ off (see the emit-site comment in `to_meta`). Pins the
/// pre-session Always-Approve → Normal cycle not creating a yolo session.
#[test]
fn to_meta_always_emits_yolo_mode_explicitly() {
    for yolo in [false, true] {
        let flags = SessionFlags {
            yolo_mode: yolo,
            ..Default::default()
        };
        let meta = flags.to_meta().expect("permission seeds must always emit meta");
        assert_eq!(
                meta["yoloMode"],
                serde_json::json!(yolo),
                "yoloMode must be explicit (yolo={yolo}); meta={meta:?}"
            );
    }
}
#[test]
fn to_meta_chat_mode_stamps_kind_and_omits_agent_profile() {
    let flags = SessionFlags {
        chat_mode: true,
        plan_mode: true,
        subagents: true,
        ask_user: true,
        ..Default::default()
    };
    let meta = flags.to_meta().expect("chat_mode must emit meta");
    assert_eq!(meta["echo.agent/session"]["kind"], "chat");
    assert!(
            meta.get("agentProfile").is_none(),
            "K12: chat mode must omit Build agentProfile"
        );
    assert_chat_meta_has_no_workspace_bind_keys(
        &serde_json::Value::Object(meta.clone()),
    );
}
/// Load meta merge: explicit `chat_kind` alone (no process-wide chat_mode)
/// stamps kind and strips agentProfile — conversation resume acceptance.
#[test]
fn load_meta_chat_kind_alone_stamps_kind_and_strips_profile() {
    let flags = SessionFlags {
        chat_mode: false,
        plan_mode: true,
        subagents: true,
        ask_user: true,
        ..Default::default()
    };
    let mut meta = flags.to_meta();
    let chat_kind = true;
    if chat_kind || flags.chat_mode {
        apply_chat_kind_meta(&mut meta);
        scrub_chat_workspace_bind_meta(&mut meta);
    }
    let meta = meta.expect("chat_kind must produce meta");
    assert_eq!(meta["echo.agent/session"]["kind"], "chat");
    assert!(
            meta.get("agentProfile").is_none(),
            "entry chat_kind must strip Build agentProfile"
        );
    assert_chat_meta_has_no_workspace_bind_keys(
        &serde_json::Value::Object(meta.clone()),
    );
}
/// Chat create/load meta must never include client workspace-bind keys
/// (`envId`, Direct hub id, gateway attach), even if cloud fields are
/// present on the effect — backend owns workspace for `kind=chat`.
fn assert_chat_meta_has_no_workspace_bind_keys(meta: &serde_json::Value) {
    for key in CHAT_FORBIDDEN_WORKSPACE_BIND_KEYS {
        assert!(
                meta.get(*key).is_none(),
                "chat meta must not include workspace-bind key {key:?}: {meta}"
            );
    }
    assert!(
            meta.get("echo.agent/cloud_existing_workspace").is_none(),
            "chat meta without attach must not include existing workspace: {meta}"
        );
}
#[test]
fn chat_create_meta_never_includes_workspace_bind_keys_when_cloud_fields_set() {
    let flags = SessionFlags {
        chat_mode: true,
        ..Default::default()
    };
    let mut meta = flags.to_meta();
    apply_chat_kind_meta(&mut meta);
    scrub_chat_workspace_bind_meta(&mut meta);
    let meta = meta.expect("chat create must emit meta");
    assert_eq!(meta["echo.agent/session"]["kind"], "chat");
    assert_chat_meta_has_no_workspace_bind_keys(
        &serde_json::Value::Object(meta.clone()),
    );
}
#[test]
fn chat_load_meta_never_includes_workspace_bind_keys() {
    let flags = SessionFlags::default();
    let mut meta = flags.to_meta();
    apply_chat_kind_meta(&mut meta);
    {
        let obj = meta.get_or_insert_with(acp::Meta::new);
        obj.insert("envId".into(), serde_json::json!("env-poison"));
        obj.insert("echo.agent/cloud_server_id".into(), serde_json::json!("srv-poison"));
        obj.insert(
            "echo.agent/cloud_existing_workspace".into(),
            serde_json::json!({
                    "server_id": "srv-poison",
                    "cwd": "/ws",
                }),
        );
    }
    scrub_chat_workspace_bind_meta(&mut meta);
    let meta = meta.expect("chat load must emit meta");
    assert_eq!(meta["echo.agent/session"]["kind"], "chat");
    assert_chat_meta_has_no_workspace_bind_keys(
        &serde_json::Value::Object(meta.clone()),
    );
}
/// Attach stamp keeps existing workspace + local intent; envId / Direct hub stay stripped.
#[cfg(feature = "local-workspace")]
#[test]
fn scrub_chat_workspace_matrix_attach_exception() {
    use crate::app::session_startup::{LocalWorkspaceConfig, LocalWorkspaceMode};
    let mut meta = Some(acp::Meta::new());
    {
        let obj = meta.as_mut().unwrap();
        obj.insert("envId".into(), serde_json::json!("env-x"));
        obj.insert("echo.agent/cloud_server_id".into(), serde_json::json!("hub-x"));
        obj.insert(
            "echo.agent/cloud_existing_workspace".into(),
            serde_json::json!({"server_id": "srv-x", "cwd": "/ws"}),
        );
    }
    scrub_chat_workspace_bind_meta(&mut meta);
    let scrubbed = meta.as_ref().unwrap();
    assert!(scrubbed.get("envId").is_none());
    assert!(scrubbed.get("echo.agent/cloud_server_id").is_none());
    assert!(scrubbed.get("echo.agent/cloud_existing_workspace").is_none());
    let mut meta = Some(acp::Meta::new());
    apply_local_workspace_meta(
        &mut meta,
        &LocalWorkspaceConfig {
            mode: LocalWorkspaceMode::Attach,
            cwd: Some(std::path::PathBuf::from("/tmp/repo")),
            server_id: Some("srv-dogfood".into()),
        },
    );
    {
        let obj = meta.as_mut().unwrap();
        obj.insert("envId".into(), serde_json::json!("env-must-go"));
        obj.insert("echo.agent/cloud_server_id".into(), serde_json::json!("hub-must-go"));
    }
    scrub_chat_workspace_bind_meta(&mut meta);
    let scrubbed = meta.as_ref().unwrap();
    assert!(scrubbed.get("envId").is_none(), "envId must stay scrubbed");
    assert!(
            scrubbed.get("echo.agent/cloud_server_id").is_none(),
            "Direct hub must stay scrubbed"
        );
    assert_eq!(
            scrubbed["echo.agent/cloud_existing_workspace"]["server_id"],
            "srv-dogfood"
        );
    assert_eq!(scrubbed["echo.agent/local_workspace"]["mode"], "attach");
    assert_eq!(scrubbed["echo.agent/local_workspace"]["server_id"], "srv-dogfood");
    assert_eq!(scrubbed["echo.agent/local_workspace"]["cwd"], "/tmp/repo");
}
#[cfg(feature = "local-workspace")]
#[test]
fn to_meta_chat_attach_stamps_local_and_existing() {
    use crate::app::session_startup::{LocalWorkspaceConfig, LocalWorkspaceMode};
    let flags = SessionFlags {
        chat_mode: true,
        local_workspace: Some(LocalWorkspaceConfig {
            mode: LocalWorkspaceMode::Attach,
            cwd: Some(std::path::PathBuf::from("/tmp/repo")),
            server_id: Some("srv-1".into()),
        }),
        ..Default::default()
    };
    let meta = flags.to_meta().expect("meta");
    assert_eq!(meta["echo.agent/session"]["kind"], "chat");
    assert_eq!(meta["echo.agent/local_workspace"]["mode"], "attach");
    assert_eq!(meta["echo.agent/cloud_existing_workspace"]["server_id"], "srv-1");
    assert!(meta.get("envId").is_none());
    assert!(meta.get("echo.agent/cloud_server_id").is_none());
}
#[cfg(feature = "local-workspace")]
#[test]
fn to_meta_chat_own_stamps_intent_without_existing() {
    use crate::app::session_startup::{LocalWorkspaceConfig, LocalWorkspaceMode};
    let flags = SessionFlags {
        chat_mode: true,
        local_workspace: Some(LocalWorkspaceConfig {
            mode: LocalWorkspaceMode::Own,
            cwd: Some(std::path::PathBuf::from("/tmp/repo-own")),
            server_id: None,
        }),
        ..Default::default()
    };
    let meta = flags.to_meta().expect("meta");
    assert_eq!(meta["echo.agent/local_workspace"]["mode"], "own");
    assert_eq!(meta["echo.agent/local_workspace"]["cwd"], "/tmp/repo-own");
    assert!(meta["echo.agent/local_workspace"].get("server_id").is_none());
    assert!(
            meta.get("echo.agent/cloud_existing_workspace").is_none(),
            "own must not stamp existing; shell mints server_id"
        );
    assert!(meta.get("envId").is_none());
}
#[cfg(feature = "local-workspace")]
#[test]
fn mid_session_add_params_scrub_envid() {
    use crate::app::session_startup::{LocalWorkspaceConfig, LocalWorkspaceMode};
    let params = mid_session_add_local_workspace_params(
        "sess-1",
        &LocalWorkspaceConfig {
            mode: LocalWorkspaceMode::Attach,
            cwd: Some(std::path::PathBuf::from("/tmp/repo")),
            server_id: Some("srv-add".into()),
        },
    );
    assert_eq!(params["sessionId"], "sess-1");
    assert_eq!(params["meta"]["echo.agent/local_workspace"]["mode"], "attach");
    assert_eq!(
            params["meta"]["echo.agent/cloud_existing_workspace"]["server_id"],
            "srv-add"
        );
    assert!(params["meta"].get("envId").is_none());
}
#[cfg(feature = "local-workspace")]
#[test]
fn reject_non_fs_only_advertised_tools_matrix() {
    let fs_only = ["workspace.fs_list", "workspace.fs_read_file", "workspace.put_files"];
    assert!(reject_non_fs_only_advertised_tools(Some(&fs_only[..])).is_ok());
    assert!(
            reject_non_fs_only_advertised_tools(None)
                .unwrap_err()
                .contains("uncheckable")
        );
    assert!(
            reject_non_fs_only_advertised_tools(Some(&[][..]))
                .unwrap_err()
                .contains("empty")
        );
    let with_exec = ["workspace.fs_list", "workspace.bash", "terminal.exec"];
    let err = reject_non_fs_only_advertised_tools(Some(&with_exec[..])).unwrap_err();
    assert!(err.contains("FS-only"), "{err}");
    assert!(err.contains("workspace.bash"), "{err}");
    assert!(err.contains("terminal.exec"), "{err}");
}
#[cfg(feature = "local-workspace")]
#[test]
fn finalize_chat_session_meta_stamps_attach_on_worktree_path() {
    use crate::app::session_startup::{LocalWorkspaceConfig, LocalWorkspaceMode};
    let flags = SessionFlags {
        chat_mode: false,
        local_workspace: Some(LocalWorkspaceConfig {
            mode: LocalWorkspaceMode::Attach,
            cwd: Some(std::path::PathBuf::from("/tmp/repo")),
            server_id: Some("srv-wt".into()),
        }),
        ..Default::default()
    };
    let mut meta = flags.to_meta();
    finalize_chat_session_meta(&mut meta, true, &flags);
    let meta = meta.expect("meta");
    assert_eq!(meta["echo.agent/session"]["kind"], "chat");
    assert_eq!(meta["echo.agent/local_workspace"]["mode"], "attach");
    assert_eq!(meta["echo.agent/cloud_existing_workspace"]["server_id"], "srv-wt");
    assert!(meta.get("envId").is_none());
}
#[test]
fn to_meta_yolo_suppresses_auto_mode() {
    let flags = SessionFlags {
        auto_mode: true,
        yolo_mode: true,
        ..Default::default()
    };
    let meta = flags.to_meta().expect("yolo must emit meta");
    assert_eq!(meta["yoloMode"], true);
    assert_eq!(
            meta["autoMode"], false,
            "yolo wins; autoMode must be explicitly false (not omitted)"
        );
}
/// Verify that each resolved profile name produces a valid
/// `AgentDefinition` whose name matches the expected kebab-case string.
#[test]
fn agent_profile_definitions_have_correct_names() {
    use std::str::FromStr;
    use echo_agent_core::config::BuiltinAgentName;
    for name in [
        "echo-agent-build-plan",
        "echo-agent-build-plan-no-subagents",
        "echo-agent-build-ask-user",
    ] {
        let builtin = BuiltinAgentName::from_str(name).unwrap();
        let def = builtin.definition();
        assert_eq!(
                def.name, name,
                "definition name should match the kebab-case profile name"
            );
    }
}
fn make_session_info(
    model: &str,
    resolved: Option<&str>,
    used: u64,
    total: u64,
) -> echo_agent_runtime::session::SessionInfoResponse {
    use echo_agent_runtime::session::acp_types::{ContextInfo, SessionInfoData};
    echo_agent_runtime::session::SessionInfoResponse {
        session_id: "test-session-id".into(),
        cwd: "/tmp/test".into(),
        data: SessionInfoData {
            agent_name: None,
            model: Some(model.into()),
            model_display_name: None,
            resolved_model_id: resolved.map(Into::into),
            model_fingerprint: None,
            show_model_fingerprint: false,
            api_backend: None,
            conversation_id: None,
            turns: 0,
            turn_index: 0,
            context: ContextInfo {
                used,
                total,
                auto_compact_threshold_percent: 85,
                ..Default::default()
            },
        },
    }
}
#[test]
fn format_session_info_session_auth_ignores_api_key_env() {
    let info = make_session_info("auto", None, 1000, 10000);
    let text = format_session_info(&info, None, false, false, true);
    assert!(text.contains("Auth method: OAuth"), "{text}");
    assert!(!text.contains("Manage account and credits"), "{text}");
    assert!(!text.contains("Also present: ECHO_AGENT_API_KEY"), "{text}");
    assert!(!text.contains("console.echo-agent.invalid"), "{text}");
    assert!(!text.contains("echoagent login"), "{text}");
}
#[test]
fn format_session_info_api_key_without_env() {
    let info = make_session_info("auto", None, 1000, 10000);
    let text = format_session_info(&info, None, false, true, false);
    assert!(text.contains("Auth method: API key\n"), "{text}");
    assert!(!text.contains("ECHO_AGENT_API_KEY"), "{text}");
    assert!(!text.contains("Manage account and credits"), "{text}");
    assert!(
            text.contains("Run `echoagent login` to use your EchoAgent subscription instead."),
            "{text}"
        );
    assert!(!text.contains("cloud.echo-agent.invalid"), "{text}");
}
#[test]
fn format_session_info_api_key_auth_suggests_echo_agent_login() {
    let info = make_session_info("auto", None, 1000, 10000);
    let text = format_session_info(&info, None, false, true, true);
    assert!(text.contains("Auth method: API key (ECHO_AGENT_API_KEY)"), "{text}");
    assert!(!text.contains("Manage account and credits"), "{text}");
    assert!(
            text.contains("Run `echoagent login` to use your EchoAgent subscription instead."),
            "{text}"
        );
    assert!(!text.contains("Also present: ECHO_AGENT_API_KEY"), "{text}");
    assert!(!text.contains("console.echo-agent.invalid"), "{text}");
    assert!(!text.contains("cloud.echo-agent.invalid"), "{text}");
}
#[test]
fn format_session_info_session_only_shows_oauth() {
    let info = make_session_info("auto", None, 1000, 10000);
    let text = format_session_info(&info, None, false, false, false);
    assert!(text.contains("Auth method: OAuth"), "{text}");
    assert!(!text.contains("Manage account and credits"), "{text}");
    assert!(!text.contains("Also present: ECHO_AGENT_API_KEY"), "{text}");
    assert!(!text.contains("console.echo-agent.invalid"), "{text}");
    assert!(!text.contains("echoagent login"), "{text}");
}
#[test]
fn format_session_info_shows_conversation_id_when_present() {
    let mut info = make_session_info("auto", None, 1000, 10000);
    info.data.conversation_id = Some("conv_abc123".into());
    let text = format_session_info(&info, None, false, false, false);
    assert!(text.contains("Conversation ID: conv_abc123"));
    assert!(text.contains("Session ID: test-session-id"));
}
#[test]
fn format_session_info_shows_resolved_when_enabled_and_different() {
    let info = make_session_info("echo-agent-4.5", Some("echo-agent-4.3"), 1000, 10000);
    let text = format_session_info(&info, None, true, false, false);
    assert!(text.contains("Model: echo-agent-4.5 (echo-agent-4.3)"));
}
#[test]
fn format_session_info_hides_resolved_when_disabled() {
    let info = make_session_info("echo-agent-4.5", Some("echo-agent-4.3"), 1000, 10000);
    let text = format_session_info(&info, None, false, false, false);
    assert!(text.contains("Model: echo-agent-4.5"));
    assert!(!text.contains("echo-agent-4.3"));
}
/// The (cwd, id)-derived summary path resolves and `generated_title` wins.
#[tokio::test]
async fn lookup_session_title_loads_single_summary_by_cwd() {
    let root = tempfile::tempdir().unwrap();
    let cwd = "/workspace";
    let dir = root
        .path()
        .join("sessions")
        .join(echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(cwd))
        .join("sess-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
            dir.join("summary.json"),
            serde_json::json!({
                "info": { "id": "sess-1", "cwd": cwd },
                "session_summary": "raw summary",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
                "num_messages": 1,
                "current_model_id": "m",
                "generated_title": "Renamed title"
            })
                .to_string(),
        )
        .unwrap();
    let id = acp::SessionId::new("sess-1");
    let title = lookup_session_title_in(root.path().to_path_buf(), &id, cwd).await;
    assert_eq!(title.as_deref(), Some("Renamed title"));
}
#[test]
fn format_session_info_no_parens_when_resolved_matches_requested() {
    let info = make_session_info("echo-agent-4.5", Some("echo-agent-4.5"), 1000, 10000);
    let text = format_session_info(&info, None, true, false, false);
    assert!(text.contains("Model: echo-agent-4.5"));
    assert!(!text.contains("(echo-agent-4.5)"));
}
#[test]
fn format_session_info_shows_model_hash_when_catalog_flag_set() {
    let mut info = make_session_info("v9", None, 1000, 10000);
    info.data.model_fingerprint = Some("abc123".into());
    info.data.show_model_fingerprint = true;
    let text = format_session_info(&info, None, false, false, false);
    assert!(text.contains("Model Hash: abc123"));
}
#[test]
fn format_session_info_hides_model_hash_for_noncoding_without_flag() {
    let mut info = make_session_info("v9", None, 1000, 10000);
    info.data.model_fingerprint = Some("abc123".into());
    info.data.show_model_fingerprint = false;
    let text = format_session_info(&info, None, false, false, false);
    assert!(!text.contains("Model Hash"));
}
#[test]
fn format_session_info_shows_model_hash_for_coding_slug_without_flag() {
    let mut info = make_session_info("echo-agent-build", None, 1000, 10000);
    info.data.model_fingerprint = Some("abc123".into());
    info.data.show_model_fingerprint = false;
    let text = format_session_info(&info, None, false, false, false);
    assert!(text.contains("Model Hash: abc123"));
}
#[test]
fn session_picker_summary_strips_skill_xml() {
    use echo_agent_tools::implementations::skills::skill::extract_skill_display_text;
    let summary = "<command-name>pr-babysit</command-name>\n\
                        <command-message>/pr-babysit</command-message>\n\
                        <command-args>check</command-args>"
        .to_string();
    let display = extract_skill_display_text(&summary).unwrap_or(summary);
    assert_eq!(display, "/pr-babysit check");
}
#[test]
fn session_picker_summary_preserves_normal_text() {
    use echo_agent_tools::implementations::skills::skill::extract_skill_display_text;
    let summary = "Fix authentication bug in login flow".to_string();
    let display = extract_skill_display_text(&summary).unwrap_or(summary);
    assert_eq!(display, "Fix authentication bug in login flow");
}
#[test]
fn sanitize_user_error_strips_auth_prefixes() {
    assert_eq!(
            sanitize_user_error(
                "Authentication required: Login timed out after 10 minutes. Please try again."
            ),
            "Login timed out after 10 minutes. Please try again."
        );
    assert_eq!(
            sanitize_user_error("Authentication failed: something went wrong"),
            "something went wrong"
        );
    assert_eq!(
            sanitize_user_error("Login timed out after 10 minutes. Please try again."),
            "Login timed out after 10 minutes. Please try again."
        );
}
#[test]
fn sanitize_user_error_collapses_disk_full() {
    assert_eq!(
            sanitize_user_error(
                "couldn't create worktree: Internal error: \"hub error: Worktree creation failed: not enough free disk space\""
            ),
            "No space left on device"
        );
    assert_eq!(
            sanitize_user_error(
                "couldn't create worktree: failed to copy index: No space left on device (os error 28)"
            ),
            "No space left on device"
        );
    assert_eq!(
            sanitize_user_error("Internal error: \"Disk quota exceeded or out of space.\""),
            "No space left on device"
        );
    assert_eq!(
            sanitize_user_error("couldn't create worktree: failed to get HEAD commit from source"),
            "couldn't create worktree: failed to get HEAD commit from source"
        );
}
/// Production ordering of the deferred worktree resume failure: the
/// detail is sanitized FIRST, then composed — sanitizing the composed
/// message would collapse a disk-full chain whole and erase the title
/// hint for a deferred local-miss target.
#[test]
fn worktree_resume_failure_sanitizes_detail_before_hint() {
    let raw = "failed to copy index: No space left on device (os error 28)";
    let msg = worktree_resume_failure_message(
        Some("typo title"),
        &sanitize_user_error(raw),
    );
    assert_eq!(
            msg,
            format!(
                "couldn't resume worktree session: No space left on device; {}",
                crate::app::session_title_resolve::title_miss_hint("typo title")
            )
        );
    let id_msg = worktree_resume_failure_message(None, &sanitize_user_error(raw));
    assert_eq!(id_msg, "couldn't resume worktree session: No space left on device");
}
/// A resume-picker entry converts to a **dormant** dashboard roster row
/// (the non-leader idle source) preserving title, cwd, model, worktree
/// flag, origin, and last-change time.
#[test]
fn session_picker_entry_maps_to_dormant_roster_row() {
    use crate::app::app_view::SessionPickerEntry;
    use crate::app::roster::RosterActivity;
    let updated = chrono::Utc::now();
    let entry = SessionPickerEntry {
        id: "sess-1".to_string(),
        summary: "Wire up dashboard".to_string(),
        updated_at: updated,
        created_at: updated,
        cwd: "/repo/app".to_string(),
        hostname: Some("box".to_string()),
        source: "local".to_string(),
        model_id: Some("echo-agent-4".to_string()),
        num_messages: 3,
        last_active_at: Some(updated),
        branch: None,
        repo_name: "repo-app".to_string(),
        worktree_label: Some("wt".to_string()),
        last_turn_summary: Some("Fixed the parser".to_string()),
        last_recap: None,
        card_detail: None,
    };
    let roster = session_picker_entry_to_roster(&entry);
    assert_eq!(roster.session_id, "sess-1");
    assert_eq!(roster.title.as_deref(), Some("Wire up dashboard"));
    assert_eq!(roster.cwd, "/repo/app");
    assert!(roster.is_worktree, "worktree_label present → is_worktree");
    assert_eq!(roster.model_id.as_deref(), Some("echo-agent-4"));
    assert_eq!(roster.activity, RosterActivity::Dormant);
    assert_eq!(
            roster.last_turn_summary.as_deref(),
            Some("Fixed the parser")
        );
    assert!(!roster.resident);
    assert_eq!(roster.last_change_unix_ms, updated.timestamp_millis());
    assert_eq!(roster.origin.kind, "local");
    assert_eq!(roster.origin.host.as_deref(), Some("box"));
}
#[test]
fn rewind_execute_params_sends_conversation_only_with_force() {
    let params = rewind_execute_params("sess-1", 3);
    assert_eq!(params["sessionId"], "sess-1");
    assert_eq!(params["targetPromptIndex"], 3);
    assert_eq!(params["force"], true);
    assert_eq!(params["mode"], REWIND_MODE_WIRE);
    assert_eq!(params["mode"], "conversation_only");
}
