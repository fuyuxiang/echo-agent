//! Tests for the dispatch module tree: shared fixtures and per-domain test modules.
mod auth;
mod billing;
mod cta_e2e;
mod dashboard;
mod jump;
mod modes;
mod notes;
mod permissions;
mod prompt;
mod queue_release;
mod rewind;
mod router;
mod session;
mod settings;
mod status;
mod status_line;
mod task_result;
mod transcript;
mod turn;
mod voice;
use super::billing::{
    CreditLimitUpsellMode, credit_limit_upsell_mode, is_max_tier, open_credit_limit_upsell,
    open_free_usage_upsell,
};
use super::cta::{
    CTA_MCP_ABSENT_MAX_ATTEMPTS, CTA_MCP_POLL_MAX_ATTEMPTS, cta_impression_plugin_name,
    cta_install_error_category, cta_install_relative_path, plugin_cta_phase_for,
};
use super::ctx::{find_agent_by_session_id, get_active_agent, get_active_agent_mut};
use super::dashboard::{
    apply_pending_dispatch_config, dispatch_dashboard_attach, dispatch_dashboard_begin_rename,
    dispatch_dashboard_commit_rename, dispatch_dashboard_confirm_worktree,
    dispatch_dashboard_create_new_agent_with_detail, dispatch_dashboard_delete,
    dispatch_dashboard_dispatch, dispatch_dashboard_dispatch_slash,
    dispatch_dashboard_overlay_cycle, dispatch_dashboard_overlay_exit,
    dispatch_dashboard_overlay_stop, dispatch_dashboard_peek_reply,
    dispatch_dashboard_permission_followup, dispatch_dashboard_permission_select,
    dispatch_dashboard_question_answer, dispatch_dashboard_stop,
    dispatch_dashboard_toggle_auto_approve, dispatch_exit_dashboard, dispatch_open_dashboard,
    ensure_dashboard_state, resolve_location_input,
};
use super::modes::{
    YOLO_ON_UNDER_PLAN_TOAST, active_agent_plan_nudge_state, dispatch_cycle_mode_and_sync,
    downgrade_displayed_auto_if_gated, permission_mode_toast,
};
use super::permissions::drain_permission_queue;
use super::prompt::{dispatch_doctor, dispatch_send_prompt, dispatch_send_prompt_inner};
use super::session::fork::build_child_fork_marker;
use super::session::lifecycle::{dispatch_new_session_inner, drain_startup_actions, finish_trust};
use super::session::load::{dispatch_load_session_with_restore, reanchor_grouped_selection};
use super::session::modal::{
    dispatch_rename_session, dispatch_reset_session_title, dispatch_sessions_confirm_close,
};
use super::settings::setters::set_default_model_inner;
use super::settings::ui::{action_for_reset, apply_setting_rollback};
use super::status::scrub_error_for_toast;
use super::task_result::dispatch_task_result;
use super::*;
use crate::acp::model_state::ModelState;
use crate::acp::tracker::AcpUpdateTracker;
use crate::app::actions::{Action, Effect, SubagentKillOutcome, SwitchModelError, TaskResult};
use crate::app::agent::{AgentId, AgentSession, AgentState};
use crate::app::agent_view::{ActivePane, AgentView, PromptMode};
use crate::app::app_view::{
    ActiveView, AppView, AuthMode, AuthState, TrustState, VoiceState, VoiceTarget,
    WelcomeAnnouncementState,
};
use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::{SessionEvent, ToolCallBlock};
use crate::scrollback::state::ScrollbackState;
use agent_client_protocol as acp;
use indexmap::IndexMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
fn test_app() -> AppView {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    AppView {
        pending_startup: None,
        active_view: ActiveView::Welcome,
        auth_return_view: None,
        agents: IndexMap::new(),
        next_agent_id: 0,
        models: ModelState::default(),
        registry: crate::actions::ActionRegistry::defaults(),
        settings_registry: std::sync::Arc::new(crate::settings::SettingsRegistry::defaults()),
        current_ui: echo_agent_runtime::agent::config::UiConfig::default(),
        cwd: PathBuf::from("/tmp"),
        cwd_has_git_ancestor: false,
        acp_tx: tx,
        scratch: crate::scrollback::render::ScratchBuffer::new(),
        cursor: crate::render::draw::CursorState::new(),
        pending_action: None,
        exit_session_pending: None,
        scroll_state: crate::input::mouse::MouseScrollState::default(),
        scroll_config: crate::input::mouse::ScrollConfig::default(),
        appearance: crate::appearance::AppearanceConfig::default(),
        notification_service: crate::notifications::NotificationService::new(Default::default()),
        status_line: Default::default(),
        pending_notification_escapes: None,
        deferred_notification: None,
        tracing_rx: None,
        active_announcements: vec![],
        hidden_announcement_ids: Default::default(),
        announcements_last_gen: 0,
        announcement: None,
        changelog_markdown: None,
        changelog_bullets: Vec::new(),
        tips: Vec::new(),
        tip: None,
        cli_model_override: None,
        cli_effort_token: None,
        default_yolo: false,
        permission_mode_from_soft_default: true,
        auto_mode_gate: true,
        yolo_policy_block: None,
        yolo_launch_block_notice: None,
        screen_mode_switch_hint: None,
        require_plan_approval: false,
        plan_mode: false,
        chat_mode: false,
        #[cfg(feature = "local-workspace")]
        welcome_workspace_mode: crate::views::welcome::WelcomeWorkspaceMode::Sandbox,
        #[cfg(feature = "local-workspace")]
        local_workspace_startup_locked: false,
        #[cfg(feature = "local-workspace")]
        welcome_session_local_workspace: None,
        #[cfg(feature = "local-workspace")]
        welcome_local_workspace_ack_pending: false,
        #[cfg(feature = "local-workspace")]
        welcome_history_load_as_build: false,
        subagents: false,
        ask_user: false,
        mouse_captured: true,
        new_worktree_dialog: None,
        contextual_hints: Default::default(),
        remote_contextual_hints: None,
        tip_seen_counts: Default::default(),
        last_known_terminal_rows: 0,
        small_screen_tip_evaluated: false,
        ssh_wrap_tip_evaluated: false,
        clipboard_focus_tip: Default::default(),
        new_session_worktree_mode: crate::app::app_view::WorktreeMode::Never,
        fork_worktree_mode: crate::app::app_view::WorktreeMode::Ask,
        restore_code: None,
        suppress_code_restore_once: None,
        resume_local_miss: None,
        agent_override: None,
        bootstrap_acp_commands: Vec::new(),
        auth_methods: vec![acp::AuthMethod::Agent(acp::AuthMethodAgent::new(
            acp::AuthMethodId::new("cloud.echo-agent.invalid"),
            "EchoAgent".to_string(),
        ))],
        auth_state: AuthState::Done,
        trust_state: TrustState::Done,
        consent_state: crate::app::consent::ConsentState::Done,
        account_email: None,
        welcome_consent_link_rects: Vec::new(),
        welcome_consent_hover_link: None,
        consent_answered: None,
        login_label: None,
        login_method_id: None,
        auth_start_mode: AuthMode::Pending,
        auth_code_input: Default::default(),
        next_auth_request_seq: 1,
        auth_url_poll_handle: None,
        deferred_startup: Default::default(),
        auth_use_oauth: false,
        auth_clipboard_delivery: None,
        auth_clipboard_feedback_generation: 0,
        team_id: None,
        team_name: None,
        is_zdr: false,
        team_role: None,
        coding_data_retention_opt_out: false,
        privacy_notice_rollout: false,
        privacy_banner_reshow_days: None,
        privacy_banner_acked: None,
        privacy_banner_opt_in_inflight: false,
        coding_data_write_seq: 0,
        show_tips: None,
        auto_update: None,
        ask_user_question_timeout_enabled: None,
        zdr_access_enabled: false,
        usage_billing_redirect_url: None,
        access_gate_shown_logged: false,
        announcement_cta_impressions_logged: Default::default(),
        gate: None,
        subscription_tier: None,
        paywall_check_started: None,
        last_subscription_check_at: None,
        subscription_watch_interval_secs: None,
        pending_gate_verification: None,
        gate_verify_gen: 0,
        bundle_state: crate::app::bundle::BundleState::default(),
        scroll_debug_hud: crate::views::scroll_debug_hud::ScrollDebugHud::new(),
        fps_hud: crate::views::fps_hud::FpsHud::new(),
        welcome_prompt: crate::views::prompt_widget::PromptWidget::new(),
        slash_mru: std::rc::Rc::new(std::cell::RefCell::new(
            crate::slash::mru::SlashMru::new_in_memory(),
        )),
        command_tags: std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new())),
        welcome_prompt_focused: false,
        welcome_tip_typing_dismissed: false,
        welcome_menu_index: None,
        welcome_menu_rects: Vec::new(),
        welcome_show_changelog_action: false,
        welcome_import_banner_rect: None,
        last_mouse_pos: None,
        last_scroll_pos: None,
        last_cache_evict_at: None,
        welcome_prompt_rect: None,
        welcome_auth_url_rect: None,
        welcome_on_auth_url: false,
        welcome_on_changelog_cta: false,
        welcome_announcement: WelcomeAnnouncementState::default(),
        welcome_auth_fallback_rect: None,
        welcome_refresh_rect: None,
        welcome_gate_url_rect: None,
        welcome_changelog_cta_rect: None,
        welcome_upgrade_cta_rect: None,
        welcome_privacy_banner_opt_in_rect: None,
        welcome_privacy_banner_opt_out_rect: None,
        welcome_privacy_banner_terms_rect: None,
        welcome_privacy_banner_policy_rect: None,
        #[cfg(feature = "local-workspace")]
        welcome_workspace_mode_rects: Default::default(),
        #[cfg(feature = "local-workspace")]
        welcome_on_workspace_mode: false,
        welcome_toast: None,
        welcome_on_privacy_banner: false,
        welcome_on_upgrade_cta: false,
        auth_show_raw_url: false,
        native_select_hold: false,
        session_picker_entries: None,
        session_picker_loading: false,
        session_picker_state: crate::views::picker::PickerState::with_mode(
            crate::views::picker::PickerMode::FullScreen,
        ),
        session_picker_source_filter: crate::views::session_picker::SourceFilter::default(),
        session_picker_relaxed_notified_for: None,
        session_picker_content_results: None,
        session_picker_content_loading: false,
        session_picker_deep_search_seq: 0,
        session_picker_list_seq: 0,
        foreign_session_compat: Default::default(),
        foreign_session_scan_seq: 0,
        foreign_scan_coordinator: Default::default(),
        session_picker_lanes: Default::default(),
        session_picker_detail_generation: 0,
        session_picker_entries_query: None,
        session_picker_pending_delete: None,
        welcome_tick: 0,
        welcome_shimmer_frame: 0,
        startup_warnings: Vec::new(),
        is_api_key_auth: false,
        pending_update_version: None,
        foreign_resume_launch_generation: 0,
        foreign_resume_launch: None,
        quit_for_update: false,
        relaunch: None,
        import_claude_modal: None,
        welcome_doc_viewer: None,
        screen_mode: crate::app::ScreenMode::Inline,
        pending_screen_mode_switch: None,
        pending_effects: Vec::new(),
        pending_editor: None,
        pending_pager_path: None,
        pending_pager_ansi: false,
        minimal_state: crate::minimal_api::MinimalState::default(),
        reconnect_pending: false,
        show_resolved_model: true,
        sharing_enabled: false,
        plugin_cta_enabled: false,
        plugin_cta_marketplace: None,
        workspace_dashboard_enabled: false,
        usage_visible: true,
        has_external_auth_provider: false,
        tier_restricted_commands: Vec::new(),
        leader_mode: true,
        credit_balance: None,
        auto_topup: None,
        billing_poll_wanted: false,
        leader_roster: Vec::new(),
        dashboard_local_sessions: Vec::new(),
        dashboard_sessions_loading: false,
        shared_prompt_queues: std::collections::HashMap::new(),
        optimistic_prompt_echoes: std::collections::HashMap::new(),
        pending_running_adoptions: std::collections::HashMap::new(),
        session_picker_grouped: false,
        scheduler_background_loops_seed: true,
        cancel_rewind_enabled: true,
        session_recap_available: false,
        shell_feedback_trace_offer: false,
        feedback_trace_choice_latched: false,
        feedback_trace_upload_pending: None,
        tutorial: None,
        dashboard: None,
        dashboard_return: None,
        dashboard_persisted: None,
        keyboard_normalizer: crate::input::KeyboardNormalizer::from_terminal_context(),
        has_claude_import: false,
        voice_mode_enabled: false,
        voice_ui_active: false,
        voice_config: echo_agent_voice::VoiceConfig::default(),
        voice_auth: None,
        voice_cmd_tx: None,
        voice_state: VoiceState::Idle,
    }
}
/// Build a default `AgentSession` for
/// tests. Centralises the fixture so new fields on `AgentSession`
/// don't break every test that constructs one by hand. The
/// `acp_tx` is cloned from the test `AppView`; the
/// `deferred_model_switch` is pulled from the `AppView`'s CLI
/// overrides for parity with `dispatch_new_session_inner`.
fn make_test_agent_session(app: &AppView, id: AgentId, sid: &str) -> AgentSession {
    AgentSession {
        id,
        acp_tx: app.acp_tx.clone(),
        session_id: Some(sid.to_string().into()),
        models: ModelState::default(),
        state: AgentState::Idle,
        tracker: AcpUpdateTracker::new(),
        cwd: PathBuf::from("/tmp"),
        is_worktree: false,
        forked_from: None,
        pending_prompts: std::collections::VecDeque::new(),
        next_queue_id: 0,
        yolo_mode: false,
        auto_mode: false,
        prompt_history: Vec::new(),
        prompt_history_loading: false,
        loading_replay: false,
        restore_degree: None,
        rate_limited: false,
        model_incompatible: false,
        credit_limit_blocked: false,
        free_usage_blocked: false,
        available_commands: Vec::new(),
        available_commands_generation: 0,
        available_tools: None,
        model_switch_pending: false,
        user_model_preference: None,
        deferred_model_switch: app.deferred_model_switch_from_cli(),
        bg_tasks: std::collections::BTreeMap::new(),
        bg_tool_call_to_task: std::collections::HashMap::new(),
        scheduled_tasks: std::collections::HashMap::new(),
        in_flight_prompt: None,
        compact_held_prompt: None,
        current_prompt_id: None,
        created_via_new: false,
    }
}
pub(super) fn test_app_with_agent() -> AppView {
    let mut app = test_app();
    let id = AgentId(0);
    let session = make_test_agent_session(&app, id, "test-session");
    let mut agent = AgentView::new(session, ScrollbackState::new());
    agent.active_pane = ActivePane::Scrollback;
    app.agents.insert(id, agent);
    app.next_agent_id = 1;
    switch_to_agent(&mut app, id, SwitchCause::New);
    app
}
/// Give a test agent a generated title so the dashboard renders it.
///
/// The dashboard hides empty (no-real-turn) sessions
/// (`views::dashboard::row::is_empty_top_level`); nav/render tests that
/// rely on their placeholder agents being visible call this to opt in.
fn mark_agent_nonempty(app: &mut AppView, id: AgentId) {
    if let Some(a) = app.agents.get_mut(&id) {
        a.generated_session_title = Some(format!("Session {}", id.0));
    }
}
/// Push a plain prompt directly onto the LOCAL drip-feed queue
/// (`pending_prompts`), bypassing the server-authoritative
/// immediate-send routing. Used by tests that exercise the local
/// `maybe_drain_queue` / editing / `DrainQueue` machinery, which is still
/// the path for image/skill/bash/editing prompts and idle drains.
pub(super) fn enqueue_local(app: &mut AppView, id: AgentId, text: &str) {
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .enqueue_prompt(text.to_string());
}
fn make_test_subagent(child_sid: &str, sa_id: &str) -> crate::app::subagent::SubagentInfo {
    crate::app::subagent::SubagentInfo {
        subagent_id: Arc::from(sa_id),
        child_session_id: Arc::from(child_sid),
        description: Arc::from("test subagent"),
        subagent_type: Arc::from("general-purpose"),
        persona: None,
        role: None,
        model: None,
        context_source: None,
        resumed_from: None,
        capability_mode: None,
        workflow_run_id: None,
        context_normalized: false,
        parent_prompt_id: None,
        started_at: std::time::Instant::now(),
        last_progress_at: std::time::Instant::now(),
        finished: false,
        status: None,
        error: None,
        duration_ms: None,
        tool_calls: None,
        turns: None,
        turn_count: None,
        tool_call_count: None,
        tokens_used: None,
        context_window_tokens: None,
        context_usage_pct: None,
        tools_used: Vec::new(),
        error_count: None,
        activity_label: None,
        is_background: false,
        pending_kill: false,
        kill_requested_at: None,
        scrollback_entry_id: None,
        prompt: None,
        child_cwd: None,
        worktree_path: None,
        transcript: Default::default(),
    }
}
fn cta_entry(name: &str, status: &str) -> echo_agent_hooks_plugins_types::MarketplacePluginEntry {
    echo_agent_hooks_plugins_types::MarketplacePluginEntry {
        name: name.into(),
        version: None,
        description: None,
        category: None,
        author: None,
        tags: Vec::new(),
        keywords: Vec::new(),
        domains: Vec::new(),
        homepage: None,
        relative_path: format!("plugins/{name}"),
        skill_count: 0,
        has_hooks: false,
        has_agents: false,
        has_mcp: false,
        install_status: status.into(),
        installed_version: None,
        components: None,
        remote_url: None,
        remote_ref: None,
        remote_sha: None,
        remote_subdir: None,
    }
}
fn cta_outcome(
    status: echo_agent_hooks_plugins_types::OutcomeStatus,
    message: &str,
) -> echo_agent_hooks_plugins_types::ActionOutcome {
    echo_agent_hooks_plugins_types::ActionOutcome {
        status,
        message: message.into(),
        requires_reload: false,
        requires_restart: false,
    }
}
fn cta_mcp_server(
    name: &str,
    plugin: Option<&str>,
    status: crate::views::mcps_modal::McpServerDisplayStatus,
) -> crate::views::mcps_modal::McpServerInfo {
    use crate::views::mcps_modal::{McpServerDisplayStatus, McpWireSource};
    crate::views::mcps_modal::McpServerInfo {
        name: name.into(),
        display_name: None,
        status,
        tool_count: 0,
        auth_required: matches!(status, McpServerDisplayStatus::NeedsAuth),
        setup_required: false,
        setup: None,
        setup_values: std::collections::HashMap::new(),
        tools: vec![],
        enabled: true,
        source: plugin
            .map(|p| format!("plugin: {p}"))
            .unwrap_or_else(|| "local".into()),
        wire_source: McpWireSource::Local,
        plugin_name: plugin.map(str::to_string),
        is_managed_gateway: false,
    }
}
fn arm_reconcile(
    app: &mut AppView,
    id: AgentId,
    prompt_id: &str,
    stop_reason: &str,
    age: std::time::Duration,
) {
    arm_reconcile_with_trigger(app, id, prompt_id, stop_reason, None, age);
}
/// [`arm_reconcile`] with an explicit `_meta.cancelTrigger`.
fn arm_reconcile_with_trigger(
    app: &mut AppView,
    id: AgentId,
    prompt_id: &str,
    stop_reason: &str,
    cancel_trigger: Option<&str>,
    age: std::time::Duration,
) {
    arm_reconcile_with_meta(app, id, prompt_id, stop_reason, cancel_trigger, None, age);
}
/// [`arm_reconcile`] with explicit `_meta.cancelTrigger` /
/// `_meta.cancellationCategory`.
#[allow(clippy::too_many_arguments)]
fn arm_reconcile_with_meta(
    app: &mut AppView,
    id: AgentId,
    prompt_id: &str,
    stop_reason: &str,
    cancel_trigger: Option<&str>,
    cancellation_category: Option<&str>,
    age: std::time::Duration,
) {
    app.agents.get_mut(&id).unwrap().pending_turn_end_reconcile =
        Some(crate::app::agent_view::PendingTurnEnd {
            prompt_id: prompt_id.into(),
            stop_reason: Some(stop_reason.into()),
            agent_result: None,
            cancel_trigger: cancel_trigger.map(str::to_string),
            cancellation_category: cancellation_category.map(str::to_string),
            received_at: std::time::Instant::now() - age,
        });
}
pub(super) fn end_turn() -> Action {
    Action::TaskComplete(TaskResult::PromptResponse {
        agent_id: AgentId(0),
        result: Ok(acp::PromptResponse::new(acp::StopReason::EndTurn)),
        http_status: None,
        prompt_id: None,
    })
}
/// Plant a Build session under the process `echo_agent_home()` (OnceLock-cached;
/// do not rely on setting `ECHO_AGENT_HOME` mid-process). Caller must remove `sess_dir`.
fn plant_local_build_session(cwd: &std::path::Path, session_id: &str) -> std::path::PathBuf {
    let home = echo_agent_runtime::util::echo_agent_home::echo_agent_home();
    let encoded =
        echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&cwd.to_string_lossy());
    let sess_dir = home.join("sessions").join(encoded).join(session_id);
    std::fs::create_dir_all(&sess_dir).expect("plant session dir");
    std::fs::write(sess_dir.join("summary.json"), b"{}").expect("plant summary");
    sess_dir
}
/// Extract the in-flight auth request sequence, panicking if the auth
/// state is not `Authenticating`.
fn authenticating_seq(app: &AppView) -> u64 {
    match app.auth_state {
        AuthState::Authenticating { request_seq, .. } => request_seq,
        ref other => panic!("expected Authenticating, got {other:?}"),
    }
}
/// Extract text from the last system message in an agent's scrollback.
pub(super) fn last_system_text(app: &AppView, id: AgentId) -> String {
    system_text_from_end(app, id, 0)
}
/// Like [`last_system_text`] but takes an offset from the end.
/// `offset = 0` is the last entry, `offset = 1` is second-to-last, etc.
fn system_text_from_end(app: &AppView, id: AgentId, offset: usize) -> String {
    let sb = &app.agents[&id].scrollback;
    let idx = sb.len() - 1 - offset;
    let entry = sb.get(idx).expect("scrollback index out of bounds");
    match &entry.block {
        RenderBlock::System(sys) => sys.text.clone(),
        other => panic!("expected System block at index {idx}, got {other:?}"),
    }
}
/// Insert a placeholder agent at `id` so `switch_to_agent` recognises
/// it (the helper's defensive check uses `app.agents.contains_key`).
/// `session_id` and `active_pane` are populated to mirror the
/// existing `test_app_with_agent` setup; these tests do not read
/// either field.
fn insert_placeholder_agent(app: &mut AppView, id: AgentId) {
    let mut agent = AgentView::new(
        AgentSession {
            id,
            acp_tx: app.acp_tx.clone(),
            session_id: Some("placeholder".into()),
            models: ModelState::default(),
            state: AgentState::Idle,
            tracker: AcpUpdateTracker::new(),
            cwd: PathBuf::from("/tmp"),
            is_worktree: false,
            forked_from: None,
            pending_prompts: std::collections::VecDeque::new(),
            next_queue_id: 0,
            yolo_mode: false,
            auto_mode: false,
            prompt_history: Vec::new(),
            prompt_history_loading: false,
            loading_replay: false,
            restore_degree: None,
            rate_limited: false,
            model_incompatible: false,
            credit_limit_blocked: false,
            free_usage_blocked: false,
            available_commands: Vec::new(),
            available_commands_generation: 0,
            available_tools: None,
            model_switch_pending: false,
            user_model_preference: None,
            deferred_model_switch: None,
            bg_tasks: std::collections::BTreeMap::new(),
            bg_tool_call_to_task: std::collections::HashMap::new(),
            scheduled_tasks: std::collections::HashMap::new(),
            in_flight_prompt: None,
            compact_held_prompt: None,
            current_prompt_id: None,
            created_via_new: false,
        },
        ScrollbackState::new(),
    );
    agent.active_pane = ActivePane::Scrollback;
    app.agents.insert(id, agent);
}
/// Build an app with three agents (ids 0, 1, 2) and `active_view` set
/// to agent 0.
pub(super) fn three_agent_app() -> AppView {
    let mut app = test_app_with_agent();
    insert_placeholder_agent(&mut app, AgentId(1));
    insert_placeholder_agent(&mut app, AgentId(2));
    app
}
use crate::slash::commands::fork::ForkArgs;
fn fork_args(worktree_override: Option<bool>, directive: Option<&str>) -> ForkArgs {
    ForkArgs {
        worktree_override,
        directive: directive.map(String::from),
    }
}
/// Build a single-agent app for the `/fork` dispatcher tests.
///
/// Sets `current_branch` to `Some("main")` so the agent appears to be
/// inside a git repo. This is required because `dispatch_fork` skips
/// the worktree question when `current_branch` is `None` (non-git cwd).
fn fork_test_app() -> AppView {
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().current_branch = Some("main".into());
    app
}
/// Build a minimal `AcpArgs<acp::ExtRequest>` for an
/// `echo.agent/ask_user_question` ext-method request. Returns the args
/// plus the receiver half of the response oneshot so the test can
/// assert the handler completes the ACP roundtrip.
fn make_ask_user_question_args(
    tool_call_id: &str,
) -> (
    echo_agent_acp::AcpArgs<acp::ExtRequest>,
    tokio::sync::oneshot::Receiver<echo_agent_acp::AcpResult<acp::ExtResponse>>,
) {
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        AskUserQuestionExtRequest, Question, QuestionOption,
    };
    let req = AskUserQuestionExtRequest {
        session_id: "test-session".into(),
        tool_call_id: tool_call_id.into(),
        mode: echo_agent_tools::implementations::echo_agent_build::ask_user_question::AskUserQuestionMode::Default,
        timeout_secs: None,
        questions: vec![Question {
                question: "ACP-driven question".into(),
                options: vec![QuestionOption {
                    label: "ok".into(),
                    description: "ok".into(),
                    preview: None,
                    id: None,
                }],
                multi_select: Some(false),
                            id: None,
            }],
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let ext = acp::ExtRequest::new(
        "echo.agent/ask_user_question",
        serde_json::value::to_raw_value(&req)
            .expect("serialize AskUserQuestionExtRequest")
            .into(),
    );
    (
        echo_agent_acp::AcpArgs {
            request: ext,
            response_tx: tx,
        },
        rx,
    )
}
fn set_forked_from(app: &mut AppView, child: AgentId, parent: AgentId) {
    if let Some(agent) = app.agents.get_mut(&child) {
        agent.session.forked_from = Some(parent);
    }
}
fn make_bg_task(task_id: &str) -> crate::app::agent::BgTaskState {
    crate::app::agent::BgTaskState {
        task_id: task_id.into(),
        tool_call_id: String::new(),
        command: "sleep 99".into(),
        description: None,
        cwd: String::new(),
        output_file: String::new(),
        status: crate::app::agent::BgTaskStatus::Running,
        start_time: std::time::SystemTime::now(),
        end_time: None,
        exit_code: None,
        signal: None,
        stdout: String::new(),
        stdout_line_count: 0,
        truncated: false,
        pending_kill: false,
        kill_requested_at: None,
        scrollback_entry_id: None,
        is_monitor: false,
        restored_from_replay: false,
    }
}
/// Set up a two-agent app: agent 0 is active with "sess-A",
/// agent 1 is inactive with "sess-B" and a bg task.
fn two_agent_app_with_bg_task() -> AppView {
    let mut app = test_app_with_agent();
    app.agents[&AgentId(0)].session.session_id = Some(acp::SessionId::new("sess-A"));
    let id1 = AgentId(1);
    let mut agent1 = AgentView::new(
        AgentSession {
            id: id1,
            acp_tx: app.acp_tx.clone(),
            session_id: Some(acp::SessionId::new("sess-B")),
            models: ModelState::default(),
            state: AgentState::Idle,
            tracker: AcpUpdateTracker::new(),
            cwd: PathBuf::from("/tmp"),
            is_worktree: false,
            forked_from: None,
            pending_prompts: std::collections::VecDeque::new(),
            next_queue_id: 0,
            yolo_mode: false,
            auto_mode: false,
            prompt_history: Vec::new(),
            prompt_history_loading: false,
            loading_replay: false,
            restore_degree: None,
            rate_limited: false,
            model_incompatible: false,
            credit_limit_blocked: false,
            free_usage_blocked: false,
            available_commands: Vec::new(),
            available_commands_generation: 0,
            available_tools: None,
            model_switch_pending: false,
            user_model_preference: None,
            deferred_model_switch: None,
            bg_tasks: std::collections::BTreeMap::new(),
            bg_tool_call_to_task: std::collections::HashMap::new(),
            scheduled_tasks: std::collections::HashMap::new(),
            in_flight_prompt: None,
            compact_held_prompt: None,
            current_prompt_id: None,
            created_via_new: false,
        },
        ScrollbackState::new(),
    );
    let mut task = make_bg_task("task-B-1");
    task.pending_kill = true;
    task.kill_requested_at = Some(std::time::Instant::now());
    agent1.session.bg_tasks.insert("task-B-1".into(), task);
    app.agents.insert(id1, agent1);
    app.next_agent_id = 2;
    assert!(matches!(app.active_view, ActiveView::Agent(AgentId(0))));
    app
}
/// Test helper: open Settings then OpenResetConfirm for `key`.
/// Extracted so individual tests don't have to repeat the
/// open-then-open ritual.
fn setup_reset_confirm_open(app: &mut AppView, key: crate::settings::SettingKey) {
    use crate::views::modal::ActiveModal;
    let _ = dispatch(Action::OpenSettings, app);
    let _ = dispatch(Action::OpenResetConfirm { key }, app);
    let agent = app.agents.get(&AgentId(0)).expect("agent must exist");
    assert!(
        matches!(
            agent.active_modal,
            Some(ActiveModal::ResetSettingsConfirm { .. })
        ),
        "setup_reset_confirm_open: ResetSettingsConfirm must be active",
    );
}
fn make_picker_entry(id: &str, cwd: &str) -> crate::app::app_view::SessionPickerEntry {
    crate::app::app_view::SessionPickerEntry {
        id: id.into(),
        summary: id.into(),
        updated_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        cwd: cwd.into(),
        hostname: None,
        source: "local".into(),
        model_id: None,
        num_messages: 0,
        last_active_at: None,
        branch: None,
        repo_name: "repo".into(),
        worktree_label: None,
        last_turn_summary: None,
        last_recap: None,
        card_detail: None,
    }
}
fn make_conversation_entry(id: &str) -> crate::app::app_view::SessionPickerEntry {
    let mut e = make_picker_entry(id, "");
    e.source = "conversation".into();
    e
}
/// Open a SessionPicker modal on the active agent seeded with `entries`.
fn open_session_picker_with(
    app: &mut AppView,
    entries: Vec<crate::app::app_view::SessionPickerEntry>,
) {
    use crate::views::modal::ActiveModal;
    let agent = get_active_agent_mut(app).expect("active agent");
    agent.active_modal = Some(ActiveModal::SessionPicker {
        state: crate::views::picker::PickerState::default(),
        entries: Some(entries),
        loading: false,
        lanes: Default::default(),
        previous_palette: None,
        window: crate::views::modal_window::ModalWindowState::new(),
        content_results: None,
        content_loading: false,
        deep_search_seq: 0,
        entries_query: None,
        source_filter: crate::views::session_picker::SourceFilter::default(),
        pending_delete: None,
    });
}
/// Toast strings match the expected format and contain on/off
/// status.
fn read_toast(app: &AppView) -> String {
    let agent = app.agents.get(&AgentId(0)).expect("agent must exist");
    agent
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("toast should be set")
}
/// Helper: enqueue a single permission containing the new
/// "enable-always-approve" option (AllowOnce kind, position 0 —
/// default-selected by the real `enqueue_permission` helper),
/// a regular "opt-allow-once" (AllowOnce kind, position 1), and
/// a "opt-reject-once" (RejectOnce, position 2). Mirrors the
/// option list the shell builds for TUI/Pager/Desktop.
/// Returns the response receiver for the injected permission.
fn enqueue_permission_with_enable_always_approve(
    app: &mut AppView,
) -> tokio::sync::oneshot::Receiver<acp::Result<acp::RequestPermissionResponse>> {
    use crate::views::permission_view::{PermissionFocus, PermissionViewState};
    use std::sync::Arc;
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let request = acp::RequestPermissionRequest::new(
        acp::SessionId::new(Arc::from("test-sess")),
        acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from("tc-enable-aa-1")),
            acp::ToolCallUpdateFields::default(),
        ),
        vec![
            acp::PermissionOption::new(
                acp::PermissionOptionId::new(Arc::from(
                    echo_agent_workspace::permission::ENABLE_ALWAYS_APPROVE_OPTION_ID,
                )),
                "Yes, and don't ask again for anything",
                acp::PermissionOptionKind::AllowOnce,
            ),
            acp::PermissionOption::new(
                acp::PermissionOptionId::new(Arc::from("opt-allow-once")),
                "Yes, proceed",
                acp::PermissionOptionKind::AllowOnce,
            ),
            acp::PermissionOption::new(
                acp::PermissionOptionId::new(Arc::from("opt-reject-once")),
                "No",
                acp::PermissionOptionKind::RejectOnce,
            ),
        ],
    );
    let options = request.options.clone();
    agent.permission_queue.push_back(PermissionViewState {
        request: echo_agent_acp::AcpArgs {
            request,
            response_tx,
        },
        id: 1,
        focus: PermissionFocus::Options,
        options,
        active_idx: 0,
        bash_highlights: None,
        bash_selection_count: 0,
        bash_deny_selection_count: 0,
        bash_command_raw: None,
        mcp_scope: None,
        title: "test-enable-always-approve".to_string(),
        description: vec![],
        args_expanded: false,
        desc_scroll: 0,
        subagent_label: None,
        options_area_height: 0,
        options_scroll_offset: 0,
    });
    response_rx
}
const POLICY_WARNING: &str =
    echo_agent_workspace::permission::resolution::YOLO_PIN_REASON_REQUIREMENTS;
fn agent_toast(app: &AppView) -> Option<String> {
    app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
}
/// Use the `theme_cache::test_lock` to serialize tests that touch
/// the in-memory theme state (single mutable global). Mirrors the
/// pattern used by `theme::cache::tests`.
fn with_theme_test_env(f: impl FnOnce()) {
    let _guard = crate::theme::cache::test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::theme::cache::reset_for_test();
    crate::theme::cache::seed_auto_theme_defaults_for_test();
    crate::theme::cache::set(crate::theme::ThemeKind::EchoAgentNight);
    crate::theme::system_appearance::clear_mock();
    f();
    crate::theme::system_appearance::clear_mock();
    crate::theme::cache::reset_for_test();
}
fn agent_scrollback_len(app: &AppView) -> usize {
    app.agents.get(&AgentId(0)).unwrap().scrollback.len()
}
use crate::scrollback::blocks::UserPromptBlock;
/// Helper: open the dashboard against an existing `app`.
fn open_dashboard(app: &mut AppView) {
    let _ = dispatch_open_dashboard(app);
}
/// Display-order list of selectable row ids — the same order
/// `dashboard_neighbor_row` and the renderer walk. Test-only mirror
/// of the row build in `dispatch_dashboard_select`.
fn dashboard_row_order(app: &AppView) -> Vec<crate::views::dashboard::DashboardRowId> {
    let d = app.dashboard.as_ref().unwrap();
    let home = crate::views::dashboard::render::cached_home();
    let roster: &[crate::app::roster::RosterEntry] = if app.leader_mode {
        &app.leader_roster
    } else {
        &app.dashboard_local_sessions
    };
    let rows = crate::views::dashboard::build_rows_with_roster(
        &app.agents,
        &d.pinned,
        &d.reorder,
        None,
        d.grouping,
        &d.filter,
        home,
        roster,
    );
    crate::views::dashboard::render::focusables(
        &rows,
        d.grouping,
        &d.filter,
        &d.collapsed_sections,
        d.idle_show_all,
        d.search_mode,
    )
    .into_iter()
    .filter_map(|f| match f {
        crate::views::dashboard::Focusable::Row(id) => Some(id),
        crate::views::dashboard::Focusable::Section(_)
        | crate::views::dashboard::Focusable::IdleOverflow => None,
    })
    .collect()
}
/// Build a synthetic `PermissionViewState` with the given id and
/// options. Pushes it to the agent's permission_queue.
///
/// Returns the response receiver so tests can verify
/// the response was actually `send`'d through the oneshot. The
/// previous version dropped the receiver (`_rx`), which let
/// "happy-path" tests assert the queue was popped but masked
/// regressions where the pop happened without the corresponding
/// send.
fn push_synthetic_permission(
    agent: &mut crate::app::agent_view::AgentView,
    id: usize,
    options: Vec<(&str, &str)>,
) -> tokio::sync::oneshot::Receiver<Result<acp::RequestPermissionResponse, acp::Error>> {
    use crate::views::permission_view::{PermissionFocus, PermissionViewState};
    let (tx, rx) =
        tokio::sync::oneshot::channel::<Result<acp::RequestPermissionResponse, acp::Error>>();
    let request = echo_agent_acp::AcpArgs {
        request: acp::RequestPermissionRequest::new(
            acp::SessionId::new(std::sync::Arc::from("sess-1")),
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new(std::sync::Arc::from("tc-1")),
                acp::ToolCallUpdateFields::default(),
            ),
            options
                .iter()
                .map(|(oid, name)| {
                    acp::PermissionOption::new(
                        acp::PermissionOptionId::new(std::sync::Arc::from(*oid)),
                        name.to_string(),
                        if *oid == "reject" {
                            acp::PermissionOptionKind::RejectOnce
                        } else {
                            acp::PermissionOptionKind::AllowOnce
                        },
                    )
                })
                .collect(),
        ),
        response_tx: tx,
    };
    let opts = request.request.options.clone();
    let state = PermissionViewState {
        request,
        id,
        focus: PermissionFocus::Options,
        options: opts,
        active_idx: 0,
        bash_highlights: None,
        bash_selection_count: 0,
        bash_deny_selection_count: 0,
        bash_command_raw: None,
        mcp_scope: None,
        title: "Test permission".to_string(),
        description: Vec::new(),
        args_expanded: false,
        desc_scroll: 0,
        subagent_label: None,
        options_area_height: 0,
        options_scroll_offset: 0,
    };
    agent.permission_queue.push_back(state);
    rx
}
const MOUSE_OFF_STICKY: &str = crate::app::MOUSE_OFF_HINT_SCROLLBACK;
fn reset_mouse_capture_enabled(on: bool) {
    crate::app::MOUSE_CAPTURE_ENABLED.store(on, std::sync::atomic::Ordering::Release);
}
fn mouse_capture_is_enabled() -> bool {
    crate::app::MOUSE_CAPTURE_ENABLED.load(std::sync::atomic::Ordering::Acquire)
}
/// Build a minimal `CreditBalance` for billing dispatch tests.
fn test_bal(usage_pct: f64) -> crate::views::credit_bar::CreditBalance {
    crate::views::credit_bar::CreditBalance {
        usage_pct,
        effective_usage_pct: usage_pct,
        period_end_display: None,
        pay_as_you_go: false,
        on_demand_cap_cents: None,
        on_demand_used_cents: None,
        prepaid_balance_cents: None,
        period_type: None,
        is_unified_billing_user: None,
    }
}
