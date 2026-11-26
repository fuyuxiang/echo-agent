use super::support::{create_test_actor, test_agent_with_user_message_template};
use super::{
    date_rollover_reminder, laziness_injection_active, resolve_reminder_policy, todo_gate_active,
};
use crate::session::persistence::PersistenceMsg;
use crate::util::config::RemoteSettings;
use echo_agent_core::AgentDefinition;
use echo_agent_core::prompt::context::{PromptAudience, TemplateOverride};
use echo_agent_core::system_reminder::{
    DEFAULT_TODO_GATE_MAX_FIRES, ReminderPolicy, TodoGateConfig,
};
/// Helper: a `RemoteSettings` whose only non-default fields are the
/// TodoGate knobs we want to vary. Mirrors `Default::default()` for
/// everything else so the test stays robust to unrelated additions.
fn remote_with_todo_gate(enabled: Option<bool>, cap: Option<u32>) -> RemoteSettings {
    RemoteSettings {
        todo_gate_enabled: enabled,
        todo_gate_max_fires_per_prompt: cap,
        ..RemoteSettings::default()
    }
}
#[test]
fn remote_none_preserves_built_in_defaults() {
    let policy = resolve_reminder_policy(None, false);
    assert_eq!(
        policy.todo_gate,
        TodoGateConfig {
            enabled: false,
            max_fires_per_prompt: DEFAULT_TODO_GATE_MAX_FIRES,
        },
    );
    assert!(policy.enabled);
    assert!(policy.todo_nudge.enabled);
}
#[test]
fn remote_disable_matches_default_path() {
    let remote = remote_with_todo_gate(Some(false), None);
    let policy = resolve_reminder_policy(Some(&remote), false);
    assert_eq!(
        policy.todo_gate,
        TodoGateConfig {
            enabled: false,
            max_fires_per_prompt: DEFAULT_TODO_GATE_MAX_FIRES,
        },
    );
}
#[test]
fn remote_enable_true_overrides_default() {
    let remote = remote_with_todo_gate(Some(true), None);
    let policy = resolve_reminder_policy(Some(&remote), false);
    assert_eq!(
        policy.todo_gate,
        TodoGateConfig {
            enabled: true,
            max_fires_per_prompt: DEFAULT_TODO_GATE_MAX_FIRES,
        },
    );
}
#[test]
fn remote_cap_override_applies_without_enabling_gate() {
    let remote = remote_with_todo_gate(None, Some(5));
    let policy = resolve_reminder_policy(Some(&remote), false);
    assert_eq!(
        policy.todo_gate,
        TodoGateConfig {
            enabled: false,
            max_fires_per_prompt: 5,
        },
    );
}
#[test]
fn cli_todo_gate_overrides_remote_enable_false() {
    let remote = remote_with_todo_gate(Some(false), Some(7));
    let policy = resolve_reminder_policy(Some(&remote), true);
    assert_eq!(
        policy.todo_gate,
        TodoGateConfig {
            enabled: true,
            // Cap stays whatever remote said; CLI only flips `enabled`.
            max_fires_per_prompt: 7,
        },
    );
}
#[test]
fn remote_settings_deserializes_without_todo_gate_fields() {
    let legacy_json = "{}";
    let settings: RemoteSettings = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(settings.todo_gate_enabled, None);
    assert_eq!(settings.todo_gate_max_fires_per_prompt, None);
    let policy = resolve_reminder_policy(Some(&settings), false);
    assert_eq!(
        policy.todo_gate,
        TodoGateConfig {
            enabled: false,
            max_fires_per_prompt: DEFAULT_TODO_GATE_MAX_FIRES,
        },
    );
}
#[test]
fn remote_settings_accepts_explicit_null_todo_gate_fields() {
    let json = r#"{
            "todo_gate_enabled": null,
            "todo_gate_max_fires_per_prompt": null
        }"#;
    let settings: RemoteSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.todo_gate_enabled, None);
    assert_eq!(settings.todo_gate_max_fires_per_prompt, None);
}
#[test]
fn remote_settings_preserves_false_and_zero_todo_gate_fields() {
    let json = r#"{
            "todo_gate_enabled": false,
            "todo_gate_max_fires_per_prompt": 0
        }"#;
    let settings: RemoteSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.todo_gate_enabled, Some(false));
    assert_eq!(settings.todo_gate_max_fires_per_prompt, Some(0));
}
fn def_with_template(tpl: TemplateOverride) -> AgentDefinition {
    let mut def = AgentDefinition::default_echo_agent_build();
    def.system_prompt = tpl;
    def
}
fn policy_with_gate(enabled: bool) -> ReminderPolicy {
    let mut p = ReminderPolicy::default();
    p.todo_gate.enabled = enabled;
    p
}
use crate::session::goal_tracker::GoalStatus;
#[test]
fn laziness_injection_active_predicate_matrix() {
    let def = def_with_template(TemplateOverride::None);
    let policy_on = policy_with_gate(true);
    for (goal_harness_enabled, goal_status, expect) in [
        (false, None, false),
        (false, Some(GoalStatus::Active), false),
        (true, None, false),
        (true, Some(GoalStatus::Active), true),
        (true, Some(GoalStatus::Complete), false),
        (true, Some(GoalStatus::UserPaused), false),
    ] {
        assert_eq!(
            laziness_injection_active(goal_harness_enabled, goal_status),
            expect,
            "goal_harness_enabled={goal_harness_enabled} status={goal_status:?}",
        );
        assert!(
            !todo_gate_active(
                &policy_on,
                PromptAudience::Primary,
                &def,
                goal_harness_enabled,
                goal_status,
            ),
            "todo gate must be suppressed during the active goal loop",
        );
    }
}
#[test]
fn todo_gate_active_predicate_matrix() {
    let def = def_with_template(TemplateOverride::None);
    let policy_off = policy_with_gate(false);
    let policy_on = policy_with_gate(true);
    for (policy, audience, goal_harness_enabled, goal_status, expect) in [
        (&policy_off, PromptAudience::Primary, true, None, false),
        (&policy_off, PromptAudience::Subagent, true, None, false),
        (
            &policy_off,
            PromptAudience::Primary,
            true,
            Some(GoalStatus::Active),
            false,
        ),
        (
            &policy_on,
            PromptAudience::Primary,
            true,
            Some(GoalStatus::Active),
            false,
        ),
        (
            &policy_on,
            PromptAudience::Subagent,
            true,
            Some(GoalStatus::Active),
            false,
        ),
        (&policy_on, PromptAudience::Primary, false, None, false),
        (
            &policy_on,
            PromptAudience::Primary,
            false,
            Some(GoalStatus::Active),
            false,
        ),
        (&policy_on, PromptAudience::Primary, true, None, false),
    ] {
        assert_eq!(
            todo_gate_active(policy, audience, &def, goal_harness_enabled, goal_status),
            expect,
            "gate.enabled={} audience={audience:?} goal_harness_enabled={goal_harness_enabled} status={goal_status:?}",
            policy.todo_gate.enabled
        );
    }
    for status in [
        GoalStatus::Complete,
        GoalStatus::UserPaused,
        GoalStatus::BackOffPaused,
        GoalStatus::InfraPaused,
        GoalStatus::Blocked,
        GoalStatus::BudgetLimited,
    ] {
        assert!(
            !todo_gate_active(
                &policy_on,
                PromptAudience::Primary,
                &def,
                true,
                Some(status)
            ),
            "non-active status {status:?} must not enable gate"
        );
    }
    let mut templates = vec![
        TemplateOverride::None,
        TemplateOverride::Codex,
        TemplateOverride::Custom("custom".into()),
    ];
    for tpl in templates {
        let def = def_with_template(tpl);
        for audience in [PromptAudience::Primary, PromptAudience::Subagent] {
            assert!(
                !todo_gate_active(&policy_on, audience, &def, true, None),
                "built-in template without active goal must not enable gate"
            );
        }
    }
}
use chrono::NaiveDate;
fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid test date")
}
#[test]
fn date_rollover_reminder_silent_when_same_day() {
    let today = ymd(2026, 4, 24);
    assert!(date_rollover_reminder(today, today).is_none());
}
#[test]
fn date_rollover_reminder_fires_when_day_advances() {
    let last = ymd(2026, 4, 24);
    let today = ymd(2026, 4, 25);
    let msg = date_rollover_reminder(today, last).expect("rollover should fire");
    assert!(
        msg.contains("2026-04-25"),
        "must announce the new date: {msg}"
    );
    assert!(
        !msg.contains("2026-04-24"),
        "must not echo the stale date: {msg}"
    );
}
#[test]
fn date_rollover_reminder_fires_across_month_and_year_boundaries() {
    assert!(date_rollover_reminder(ymd(2026, 5, 1), ymd(2026, 4, 30)).is_some());
    assert!(date_rollover_reminder(ymd(2027, 1, 1), ymd(2026, 12, 31)).is_some());
}
#[test]
fn date_rollover_reminder_silent_when_clock_moves_backward() {
    let last = ymd(2026, 4, 25);
    let today = ymd(2026, 4, 24);
    assert!(date_rollover_reminder(today, last).is_none());
}
#[tokio::test(flavor = "current_thread")]
async fn same_session_rolls_over_once_when_local_date_advances() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(50_000, 256_000, 85, gateway_tx, persistence_tx).await;
            let today = chrono::Local::now().date_naive();
            assert_eq!(actor.last_announced_local_date.get(), today);
            actor.maybe_inject_date_rollover_reminder().await;
            assert_eq!(
                actor.chat_state_handle.get_conversation_len().await,
                0,
                "same-day turn must not inject a rollover reminder"
            );
            let yesterday = today.pred_opt().expect("today is never the min date");
            actor.last_announced_local_date.set(yesterday);
            actor.maybe_inject_date_rollover_reminder().await;
            let conv = actor.chat_state_handle.get_conversation().await;
            assert_eq!(conv.len(), 1, "rollover must inject exactly one reminder");
            let text = conv[0].text_content();
            assert!(
                text.contains("<system-reminder>"),
                "rollover reminder must be wrapped in system-reminder tags: {text}"
            );
            assert!(
                text.contains("The local date has changed since this session started"),
                "rollover reminder must announce the date change: {text}"
            );
            assert!(
                text.contains(&today.to_string()),
                "rollover reminder must carry today's date {today}: {text}"
            );
            assert_eq!(actor.last_announced_local_date.get(), today);
            actor.maybe_inject_date_rollover_reminder().await;
            assert_eq!(
                actor.chat_state_handle.get_conversation_len().await,
                1,
                "rollover must not re-fire on a later same-day turn"
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn rollover_reminder_follows_the_custom_template_date_intent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(50_000, 256_000, 85, gateway_tx, persistence_tx).await;
            let today = chrono::Local::now().date_naive();
            let yesterday = today.pred_opt().expect("today is never the min date");
            *actor.agent.borrow_mut() = test_agent_with_user_message_template(
                echo_agent_core::prompt::user_message::UserMessageTemplate::Custom(
                    "Workspace: ${{ workspace_path }}".to_string(),
                ),
            )
            .await;
            actor.last_announced_local_date.set(yesterday);
            actor.maybe_inject_date_rollover_reminder().await;
            assert_eq!(
                actor.chat_state_handle.get_conversation_len().await,
                0,
                "a date-free custom template must suppress the rollover reminder"
            );
            *actor.agent.borrow_mut() = test_agent_with_user_message_template(
                echo_agent_core::prompt::user_message::UserMessageTemplate::Custom(
                    "Today is ${{ today_local }}".to_string(),
                ),
            )
            .await;
            actor.last_announced_local_date.set(yesterday);
            actor.maybe_inject_date_rollover_reminder().await;
            let conv = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                conv.len(),
                1,
                "a today_local-bearing custom template must keep the rollover reminder"
            );
            assert!(
                conv[0]
                    .text_content()
                    .contains("The local date has changed since this session started"),
                "the kept reminder must be the date-rollover reminder: {}",
                conv[0].text_content()
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn rollover_reminder_fires_when_fallback_stamps_a_date_free_template() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(50_000, 256_000, 85, gateway_tx, persistence_tx).await;
            let today = chrono::Local::now().date_naive();
            let yesterday = today.pred_opt().expect("today is never the min date");
            *actor.agent.borrow_mut() = test_agent_with_user_message_template(
                echo_agent_core::prompt::user_message::UserMessageTemplate::Custom(
                    "Workspace: ${{ workspace_path }}".to_string(),
                ),
            )
            .await;
            actor.last_announced_local_date.set(yesterday);
            actor.maybe_inject_date_rollover_reminder().await;
            assert_eq!(
                actor.chat_state_handle.get_conversation_len().await,
                0,
                "a date-free template without a fallback-stamped date must stay silent"
            );
            actor.prefix_carries_fallback_date.set(true);
            actor.last_announced_local_date.set(yesterday);
            actor.maybe_inject_date_rollover_reminder().await;
            let conv = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                conv.len(),
                1,
                "a fallback-stamped date must roll over even under a date-free template"
            );
            assert!(
                conv[0]
                    .text_content()
                    .contains("The local date has changed since this session started"),
                "the injected reminder must be the date-rollover reminder: {}",
                conv[0].text_content()
            );
        })
        .await;
}
