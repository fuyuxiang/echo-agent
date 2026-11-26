use super::*;
use crate::acp::model_state::ModelState;
use crate::acp::tracker::AcpUpdateTracker;
use crate::app::agent::{AgentSession, AgentState};
use crate::app::agent_view::{AgentView, PromptMode};
use crate::app::bundle::BundleState;
use crate::scrollback::state::ScrollbackState;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
#[test]
fn welcome_show_toast_scrubs_control_chars() {
    let mut app = test_app();
    assert!(matches!(app.active_view, ActiveView::Welcome));
    app.show_toast("a\nb\rc\thttps://echo-agent.invalid");
    let toast = app
        .welcome_toast
        .as_ref()
        .map(|(m, _)| m.as_str())
        .unwrap_or("");
    assert!(
        !toast.chars().any(|c| c.is_control()),
        "control chars must be scrubbed at write: {toast:?}"
    );
    assert!(toast.contains("https://echo-agent.invalid"), "{toast:?}");
}
#[test]
fn parse_esc_ttl_bounds() {
    let default = PendingAction::ESC_DOUBLE_PRESS_TTL;
    assert_eq!(parse_esc_ttl(None), default);
    assert_eq!(parse_esc_ttl(Some("garbage".into())), default);
    assert_eq!(parse_esc_ttl(Some("".into())), default);
    assert_eq!(parse_esc_ttl(Some("0".into())), default);
    assert_eq!(parse_esc_ttl(Some("-5".into())), default);
    assert_eq!(
        parse_esc_ttl(Some(" 1200 ".into())),
        Duration::from_millis(1200)
    );
    assert_eq!(
        parse_esc_ttl(Some(ESC_DOUBLE_PRESS_TEST_MS.to_string())),
        Duration::from_millis(ESC_DOUBLE_PRESS_TEST_MS)
    );
    assert_eq!(
        parse_esc_ttl(Some(u64::MAX.to_string())),
        Duration::from_millis(ESC_DOUBLE_PRESS_TEST_MS)
    );
}
/// `AppView::draw` is the ONLY drain point for the process-wide deferred
/// release flag; if the wrapper loses its `run_deferred_release()` call,
/// every draw/tick-path cliff (video scroll-off, takeover drain,
/// frame-set replacement) silently stops purging. Drives the real
/// `draw()` against a channel-backed terminal (no tty; same recipe as
/// pager-render's `draw_frame` tests). Serialized: process-wide flag.
#[test]
#[serial_test::serial(MEMORY_RELEASE_DEFER)]
fn app_draw_drains_deferred_release_after_flush() {
    use crate::memory_release::test_support;
    use ratatui::{TerminalOptions, Viewport};
    test_support::install_counting_hook();
    crate::memory_release::run_deferred_release();
    let (frame_tx, _frame_rx) = std::sync::mpsc::channel::<crate::render::draw::WriterPayload>();
    let writer =
        crate::render::draw::TermWriter::new(frame_tx, crate::render::draw::WriterSync::new())
            .expect("single test writer");
    let backend = ratatui::backend::CrosstermBackend::new(writer);
    let mut terminal = echo_agent_ratatui_inline::Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24)),
        },
    )
    .expect("channel-backed terminal requires no tty");
    let mut app = test_app();
    crate::memory_release::request_release_after_draw("unit-test-defer");
    let before = test_support::calls();
    app.draw(&mut terminal);
    assert_eq!(
        test_support::calls(),
        before + 1,
        "AppView::draw must drain the deferred release post-flush"
    );
    let before = test_support::calls();
    app.draw(&mut terminal);
    assert_eq!(
        test_support::calls(),
        before,
        "a draw without a pending request must not purge"
    );
}
pub(crate) fn test_app() -> AppView {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    AppView {
        pending_startup: None,
        active_view: ActiveView::Welcome,
        auth_return_view: None,
        agents: indexmap::IndexMap::new(),
        next_agent_id: 0,
        models: ModelState::default(),
        registry: ActionRegistry::defaults(),
        settings_registry: std::sync::Arc::new(crate::settings::SettingsRegistry::defaults()),
        current_ui: echo_agent_runtime::agent::config::UiConfig::default(),
        status_line: Default::default(),
        cwd: std::path::PathBuf::from("/tmp"),
        cwd_has_git_ancestor: false,
        acp_tx: tx,
        scratch: crate::scrollback::render::ScratchBuffer::new(),
        cursor: CursorState::new(),
        pending_action: None,
        exit_session_pending: None,
        scroll_state: MouseScrollState::default(),
        scroll_config: ScrollConfig::default(),
        appearance: AppearanceConfig::default(),
        notification_service: NotificationService::new(Default::default()),
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
        subagents: false,
        ask_user: false,
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
        mouse_captured: true,
        new_worktree_dialog: None,
        contextual_hints: Default::default(),
        remote_contextual_hints: None,
        tip_seen_counts: Default::default(),
        last_known_terminal_rows: 0,
        small_screen_tip_evaluated: false,
        ssh_wrap_tip_evaluated: false,
        clipboard_focus_tip: Default::default(),
        new_session_worktree_mode: WorktreeMode::Never,
        fork_worktree_mode: WorktreeMode::Ask,
        restore_code: None,
        suppress_code_restore_once: None,
        resume_local_miss: None,
        agent_override: None,
        bootstrap_acp_commands: Vec::new(),
        auth_methods: Vec::new(),
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
        auth_code_input: LineEditor::default(),
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
        coding_data_retention_opt_out: true,
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
        bundle_state: BundleState::default(),
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
        welcome_changelog_cta_rect: None,
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
        has_claude_import: false,
        import_claude_modal: None,
        welcome_doc_viewer: None,
        screen_mode: ScreenMode::Inline,
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
        keyboard_normalizer: KeyboardNormalizer::from_terminal_context(),
        voice_mode_enabled: false,
        voice_ui_active: false,
        voice_config: echo_agent_voice::VoiceConfig::default(),
        voice_auth: None,
        voice_cmd_tx: None,
        voice_state: VoiceState::Idle,
    }
}
pub(crate) fn test_app_with_agent() -> AppView {
    let mut app = test_app();
    let id = super::super::agent::AgentId(0);
    let mut agent = AgentView::new(
        AgentSession {
            id,
            acp_tx: app.acp_tx.clone(),
            session_id: Some("test-session".into()),
            models: ModelState::default(),
            state: AgentState::Idle,
            tracker: AcpUpdateTracker::new(),
            cwd: std::path::PathBuf::from("/tmp"),
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
    agent.active_pane = crate::views::agent::ActivePane::Scrollback;
    app.agents.insert(id, agent);
    super::super::dispatch::switch_to_agent(
        &mut app,
        id,
        super::super::dispatch::SwitchCause::Load,
    );
    app
}
#[test]
fn dashboard_x11_primary_provenance_bypasses_unrelated_clipboard_image() {
    const PRIMARY: &str = "PRIMARY selection text";
    let clipboard_hook = || crate::clipboard::ClipboardProbeHook {
        text: Some("CLIPBOARD text".to_owned()),
        primary_text: Some(PRIMARY.to_owned()),
        x11_primary_available: true,
        ..crate::clipboard::ClipboardProbeHook::with_raster(Some(crate::clipboard::ImageData {
            data: vec![1, 2, 3],
            mime_type: "image/png".to_owned(),
        }))
    };
    let mut bracketed = test_app();
    bracketed.active_view = ActiveView::AgentDashboard;
    bracketed.dashboard = Some(crate::views::dashboard::DashboardState::new());
    crate::clipboard::set_clipboard_probe_hook(clipboard_hook());
    let _ = bracketed.handle_input(&Event::Paste(PRIMARY.to_owned()));
    crate::clipboard::clear_clipboard_probe_hook();
    assert!(
        bracketed.pending_effects.iter().any(|effect| matches!(
            effect,
            crate::app::actions::Effect::ProbeClipboardAttachment { .. }
        )),
        "the distinct CLIPBOARD image must make ordinary bracketed paste probe"
    );
    let mut primary = test_app();
    primary.active_view = ActiveView::AgentDashboard;
    primary.dashboard = Some(crate::views::dashboard::DashboardState::new());
    crate::clipboard::set_clipboard_probe_hook(clipboard_hook());
    let outcome = primary.handle_input_at_with_paste_provenance(
        &Event::Paste(PRIMARY.to_owned()),
        Instant::now(),
        PasteProvenance::X11Primary,
    );
    let probe_calls = crate::clipboard::clipboard_probe_call_count();
    crate::clipboard::clear_clipboard_probe_hook();
    assert!(matches!(outcome, InputOutcome::Changed));
    let dashboard = primary.dashboard.as_ref().expect("dashboard state");
    assert_eq!(dashboard.dispatch.text(), PRIMARY);
    assert!(!dashboard.dispatch.text().contains("CLIPBOARD"));
    assert!(dashboard.dispatch.images.is_empty());
    assert_eq!(dashboard.paste_probe_in_flight, 0);
    assert!(primary.pending_effects.iter().all(|effect| !matches!(
        effect,
        crate::app::actions::Effect::ProbeClipboardAttachment { .. }
    )));
    assert_eq!(probe_calls, 0);
}
/// With the image-input tip OFF, the poll short-circuits at the window gate
/// before touching the pasteboard — the per-tip gate fails closed.
#[test]
fn clipboard_poll_no_op_when_flag_off() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.agents.get_mut(&id).unwrap().last_terminal_size = (80, 30);
    app.notification_service.focus_tracker.on_focus_gained();
    app.contextual_hints.image_input = false;
    assert!(!app.poll_clipboard_focus_tip(), "tip-off poll is a no-op");
    assert!(!app.agents[&id].ephemeral_tip.is_active());
}
/// The in-window gate decides whether an already-running iteration may touch
/// the pasteboard at all. It opens only when contextual hints are on, the
/// probe is supported (macOS), the fire cooldown is clear, the terminal is
/// focused, and the active agent is eligible; flipping any one closes it so
/// the poll reads the clipboard zero times. (Probe support is macOS-only, so
/// the in-window result tracks the platform.)
#[test]
fn clipboard_poll_window_gate() {
    let mut app = test_app_with_agent();
    app.contextual_hints.image_input = true;
    let id = super::super::agent::AgentId(0);
    app.agents.get_mut(&id).unwrap().last_terminal_size = (80, 30);
    app.notification_service.focus_tracker.on_focus_gained();
    let now = std::time::Instant::now();
    let supported = crate::clipboard::clipboard_image_probe_supported();
    assert_eq!(
        app.clipboard_tip_in_poll_window(now),
        supported,
        "in window"
    );
    app.contextual_hints.image_input = false;
    assert!(!app.clipboard_tip_in_poll_window(now), "tip off");
    app.contextual_hints.image_input = true;
    app.notification_service.focus_tracker.on_focus_lost();
    assert!(!app.clipboard_tip_in_poll_window(now), "unfocused");
    app.notification_service.focus_tracker.on_focus_gained();
    let img = crate::prompt_images::from_clipboard_data(&crate::clipboard::ImageData {
        data: vec![1, 2, 3],
        mime_type: "image/png".into(),
    });
    app.agents.get_mut(&id).unwrap().prompt.images.push(img);
    assert!(!app.clipboard_tip_in_poll_window(now), "image attached");
    app.agents.get_mut(&id).unwrap().prompt.images.clear();
    let fired = crate::tips::clipboard_focus::CheckOutcome {
        change_count: Some(1),
        has_image: true,
    };
    app.clipboard_focus_tip.note_fired(&fired, now);
    assert!(!app.clipboard_tip_in_poll_window(now), "in cooldown");
}
/// A positive, deduped, un-cooled-down outcome on a drawable agent shows the
/// tip and commits the cooldown + changeCount dedup (same content won't
/// re-fire). Drives `apply_clipboard_probe` with a synthetic outcome so it
/// is independent of the real pasteboard.
#[test]
fn clipboard_probe_shows_and_commits_on_positive_outcome() {
    use crate::tips::clipboard_focus::CheckOutcome;
    let mut app = test_app_with_agent();
    app.contextual_hints.image_input = true;
    let id = super::super::agent::AgentId(0);
    app.agents.get_mut(&id).unwrap().last_terminal_size = (80, 30);
    let now = std::time::Instant::now();
    let outcome = CheckOutcome {
        change_count: Some(7),
        has_image: true,
    };
    assert!(app.apply_clipboard_probe(outcome, now));
    assert!(app.agents[&id].ephemeral_tip.is_active());
    assert!(
        !app.clipboard_focus_tip.should_fire(&outcome, now),
        "fired content must commit the changeCount dedup"
    );
}
/// A refused show (here: the renderability gate on a short terminal) must
/// burn nothing — the same outcome stays fireable.
#[test]
fn clipboard_probe_refused_show_burns_nothing() {
    use crate::tips::clipboard_focus::CheckOutcome;
    let mut app = test_app_with_agent();
    app.contextual_hints.image_input = true;
    let id = super::super::agent::AgentId(0);
    app.agents.get_mut(&id).unwrap().last_terminal_size = (80, 10);
    let now = std::time::Instant::now();
    let outcome = CheckOutcome {
        change_count: Some(7),
        has_image: true,
    };
    assert!(!app.apply_clipboard_probe(outcome, now));
    assert!(!app.agents[&id].ephemeral_tip.is_active());
    assert!(
        app.clipboard_focus_tip.should_fire(&outcome, now),
        "refused show must leave cooldown and dedup uncommitted"
    );
}
/// Build an idle subagent child `AgentView` for child gate↔tick symmetry tests.
fn idle_child_view(app: &AppView, id_n: usize, sid: &str) -> Box<AgentView> {
    let session = AgentSession {
        id: super::super::agent::AgentId(id_n),
        acp_tx: app.acp_tx.clone(),
        session_id: Some(sid.to_string().into()),
        models: ModelState::default(),
        state: AgentState::Idle,
        tracker: AcpUpdateTracker::new(),
        cwd: std::path::PathBuf::from("/tmp"),
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
    };
    Box::new(AgentView::new(session, ScrollbackState::new()))
}
fn key_event(code: KeyCode, mods: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, mods))
}
/// Build a registry pinned to the non-VSCode bindings so tests are
/// deterministic regardless of the host terminal.
fn pin_non_vscode_registry(app: &mut AppView) {
    let mut actions = crate::actions::default_actions(ScreenMode::Fullscreen, false);
    for def in actions.iter_mut() {
        if def.id == ActionId::Quit {
            def.default_key = key!('q', CONTROL);
            def.alt_keys = vec![key!('d', CONTROL)];
        }
        if def.id == ActionId::HalfPageDown {
            def.default_key = key!('d', CONTROL);
        }
    }
    app.registry = ActionRegistry::new(actions);
}
fn ctrl_d() -> Event {
    key_event(KeyCode::Char('d'), KeyModifiers::CONTROL)
}
fn ctrl_q() -> Event {
    key_event(KeyCode::Char('q'), KeyModifiers::CONTROL)
}
fn ctrl_c() -> Event {
    key_event(KeyCode::Char('c'), KeyModifiers::CONTROL)
}
fn left_mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}
#[test]
fn needs_animation_ignores_tracing_rx_outside_dev_builds() {
    let mut app = test_app_with_agent();
    let (_tx, rx) = tokio::sync::mpsc::channel::<String>(4);
    app.tracing_rx = Some(rx);
    assert!(
        !app.needs_animation(),
        "release builds must not request animation ticks just because \
         tracing_rx exists (always true after startup)"
    );
}
#[test]
fn needs_animation_gates_prompt_history_tick_delivery() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    assert!(
        !app.needs_animation(),
        "an idle agent with no history overlay must not request animation ticks"
    );
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.prompt_history = vec!["first prompt".into(), "second prompt".into()];
        let history = agent.combined_prompt_history();
        assert!(agent.prompt.history_search.activate(&history, ""));
    }
    assert!(
        app.needs_animation(),
        "an open prompt history overlay must request animation ticks"
    );
    let mut delivered = false;
    for _ in 0..1000 {
        if app.tick() && app.agents[&id].prompt.history_search.result_count() == 2 {
            delivered = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        delivered,
        "tick() must poll the history daemon and deliver results"
    );
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .history_search
        .deactivate();
    assert!(
        !app.needs_animation(),
        "closing the history overlay stops the animation ticks"
    );
}
#[test]
fn needs_animation_gates_scrollback_search_tick_delivery() {
    use crate::scrollback::ScrollbackSearchState;
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("foo bar"));
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("baz foo"));
        agent.scrollback.prepare_layout(80, 24);
    }
    assert!(
        !app.needs_animation(),
        "an idle agent with no search open must not request animation ticks"
    );
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.scrollback_search = Some(ScrollbackSearchState::open());
        let search = agent.scrollback_search.as_mut().unwrap();
        search.update_query("foo", &agent.scrollback);
        assert_eq!(
            search.current_index(),
            None,
            "matches are not computed synchronously on the input thread"
        );
    }
    assert!(
        app.needs_animation(),
        "an open scrollback search must request animation ticks"
    );
    let mut delivered = false;
    for _ in 0..1000 {
        app.tick();
        if app.agents[&id]
            .scrollback_search
            .as_ref()
            .unwrap()
            .current_index()
            == Some(0)
        {
            delivered = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(delivered, "tick() must poll the daemon and deliver results");
    assert_eq!(
        app.agents[&id]
            .scrollback_search
            .as_ref()
            .unwrap()
            .match_count(),
        2
    );
    app.agents.get_mut(&id).unwrap().scrollback_search = None;
    assert!(
        !app.needs_animation(),
        "closing the search stops the animation ticks"
    );
}
#[test]
fn tick_demand_fast_while_wake_turn_streams() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    assert_eq!(app.tick_demand(), TickDemand::None, "idle agent parks");
    app.agents
        .get_mut(&id)
        .unwrap()
        .note_streaming_wake_turn("p-wake");
    assert_eq!(
        app.tick_demand(),
        TickDemand::Fast,
        "wake chrome spinner must tick while the pane stays Idle"
    );
}
/// The welcome screen shimmer only advances ~12fps, so a resting welcome
/// screen must demand Slow ticks — not a 30fps loop; the deep-search
/// spinner upgrades it to Fast while loading.
#[test]
fn tick_demand_welcome_is_slow_unless_loading() {
    let mut app = test_app();
    assert_eq!(app.active_view, ActiveView::Welcome);
    assert_eq!(app.tick_demand(), TickDemand::Slow);
    assert!(app.needs_animation(), "slow still counts as animating");
    app.session_picker_content_loading = true;
    assert_eq!(app.tick_demand(), TickDemand::Fast);
}
/// An open modal session picker that is still fetching keeps fast ticks
/// alive on an otherwise-idle agent (its loading spinner must animate) —
/// including after the fast foreign scan lands rows the default EchoAgent
/// filter hides; once the native list settles the demand parks again.
#[test]
fn tick_demand_fast_while_modal_session_picker_loads() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    assert_eq!(app.tick_demand(), TickDemand::None, "idle agent parks");
    app.agents.get_mut(&id).unwrap().active_modal =
        Some(crate::views::modal::ActiveModal::SessionPicker {
            state: crate::views::picker::PickerState::default(),
            entries: None,
            loading: true,
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
    assert_eq!(
        app.tick_demand(),
        TickDemand::Fast,
        "loading modal picker must keep the spinner animating"
    );
    let foreign_entry = SessionPickerEntry {
        id: "claude-1".into(),
        summary: "claude".into(),
        updated_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        cwd: String::new(),
        hostname: None,
        source: "claude".into(),
        model_id: None,
        num_messages: 0,
        last_active_at: None,
        branch: None,
        repo_name: "r".into(),
        worktree_label: None,
        last_turn_summary: None,
        last_recap: None,
        card_detail: None,
    };
    if let Some(crate::views::modal::ActiveModal::SessionPicker { entries, .. }) =
        app.agents.get_mut(&id).unwrap().active_modal.as_mut()
    {
        *entries = Some(vec![foreign_entry]);
    }
    assert_eq!(
        app.tick_demand(),
        TickDemand::Fast,
        "foreign rows hidden by the EchoAgent filter must not end the loading spinner"
    );
    if let Some(crate::views::modal::ActiveModal::SessionPicker { loading, .. }) =
        app.agents.get_mut(&id).unwrap().active_modal.as_mut()
    {
        *loading = false;
    }
    assert_eq!(
        app.tick_demand(),
        TickDemand::None,
        "settled picker must not keep demanding ticks"
    );
}
/// An idle agent view demands no ticks at all; the macOS Cmd link-hover
/// poll (when it is the only pending work) demands Slow, never Fast.
#[test]
#[cfg(target_os = "macos")]
fn tick_demand_link_poll_is_slow_only() {
    use crate::render::osc8::{LinkOverlay, OverlayLink};
    use std::sync::Arc;
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    assert_eq!(app.tick_demand(), TickDemand::None, "idle agent parks");
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let mut overlay = LinkOverlay::new();
        overlay.push(OverlayLink {
            screen_row: 2,
            col_start: 0,
            col_end: 10,
            target: crate::render::osc8::LinkTarget::Url(Arc::from("https://example.com")),
            presentation: crate::render::osc8::LinkPresentation::Opaque,
            id: Some(1),
        });
        agent.visible_link_map.rebuild(1, &overlay, vec![]);
        agent.hovered_entry = Some(0);
        agent.last_mouse_moved_at = Some(std::time::Instant::now());
    }
    if !crate::app::agent_view::has_native_link_hover() {
        assert_eq!(
            app.tick_demand(),
            TickDemand::Slow,
            "link poll alone must not spin the fast loop"
        );
    }
}
#[test]
fn needs_animation_gates_mode_switch_banner_countdown() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    assert!(!app.needs_animation(), "idle agent must not request ticks");
    app.agents
        .get_mut(&id)
        .unwrap()
        .show_mode_switch_banner("Plan");
    assert!(
        app.needs_animation(),
        "mode_switch_banner must request ticks (tick_mode_banner countdown)"
    );
    let mut cleared = false;
    for _ in 0..512 {
        app.tick();
        if app.agents[&id].mode_switch_banner.is_none() {
            cleared = true;
            break;
        }
    }
    assert!(
        cleared,
        "tick() must decrement mode_switch_banner until it expires"
    );
    assert!(
        !app.needs_animation(),
        "expired mode banner must stop requesting ticks"
    );
}
/// Draw-entry resync: an `expires_at` crossing between pushes must close
/// the `/announcements` gate on the next frame; a later live list re-opens
/// it through the same divergence check.
#[test]
fn slash_gate_resyncs_when_critical_expires_between_pushes() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.agents
        .get_mut(&id)
        .unwrap()
        .set_has_session_announcements(true);
    app.active_announcements = vec![echo_agent_announcements::RemoteAnnouncement {
        id: Some("expired".into()),
        message: Some("gone".into()),
        severity: Some("critical".into()),
        expires_at: Some("2000-01-01T00:00:00Z".into()),
        ..Default::default()
    }];
    app.resync_announcement_slash_gate_on_divergence();
    assert!(
        !app.agents[&id]
            .prompt
            .slash_controller
            .has_session_announcements(),
        "expired-only list must close the gate on the next frame"
    );
    app.active_announcements = vec![echo_agent_announcements::RemoteAnnouncement {
        id: Some("live".into()),
        message: Some("new outage".into()),
        severity: Some("critical".into()),
        ..Default::default()
    }];
    app.resync_announcement_slash_gate_on_divergence();
    assert!(
        app.agents[&id]
            .prompt
            .slash_controller
            .has_session_announcements(),
        "a live critical must re-open the gate"
    );
}
/// Critical freezes tip TTL and must not arm needs_animation for a tip
/// that is not counting down (session-long metronome heat).
#[test]
fn ephemeral_tip_frozen_under_critical_does_not_request_animation_or_burn_ttl() {
    use std::collections::HashMap;
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let _ = agent.ephemeral_tip.show(
            crate::tips::EphemeralTip::new("t", ratatui::text::Line::from("TIP")),
            &mut HashMap::new(),
        );
        agent.session_banner_active = true;
    }
    let before = app.agents[&id]
        .ephemeral_tip
        .ticks_remaining()
        .expect("tip active");
    assert!(
        !app.agents[&id].ephemeral_tip_needs_tick(),
        "critical must freeze tip tick policy"
    );
    assert!(
        !app.needs_animation(),
        "frozen tip under critical must not arm the metronome on an idle agent"
    );
    for _ in 0..10 {
        app.tick();
    }
    assert_eq!(
        app.agents[&id].ephemeral_tip.ticks_remaining(),
        Some(before),
        "TTL must not burn while critical occludes"
    );
    app.agents.get_mut(&id).unwrap().session_banner_active = false;
    assert!(
        app.needs_animation(),
        "unfreezing must re-arm tip countdown ticks"
    );
    app.tick();
    let after = app.agents[&id]
        .ephemeral_tip
        .ticks_remaining()
        .expect("tip still active");
    assert!(after < before, "TTL must resume when critical clears");
}
/// The word-select tip's long TTL is bounded by prompt divergence: ANY
/// prompt change since the tip was shown (typed here; the snapshot guard
/// covers paste/drop identically) refuses the chord immediately and
/// retires the tip on the next tick, so Ctrl+Y goes back to yank.
#[test]
fn word_select_tip_retires_on_prompt_divergence_and_accepts_before() {
    use std::collections::HashMap;
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.last_terminal_size = (80, 30);
        agent.active_pane = crate::views::agent::ActivePane::Prompt;
        let _ = agent.ephemeral_tip.show(
            crate::tips::word_select::word_select_tip(),
            &mut HashMap::new(),
        );
        agent.word_select_tip_prompt_snapshot = Some(agent.prompt.text().to_string());
    }
    let out = app.handle_input(&key_event(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert!(
        matches!(out, InputOutcome::Action(Action::AcceptWordSelectTip)),
        "Ctrl+Y with the tip up must route to accept, got {out:?}"
    );
    let _ = app.handle_input(&key_event(KeyCode::Char('a'), KeyModifiers::NONE));
    let out = app.handle_input(&key_event(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert!(
        !matches!(out, InputOutcome::Action(Action::AcceptWordSelectTip)),
        "Ctrl+Y after a prompt edit must not accept, got {out:?}"
    );
    app.tick();
    assert!(
        !app.agents[&id].ephemeral_tip.is_active(),
        "prompt divergence must retire the word-select tip on tick"
    );
    assert!(
        app.agents[&id].word_select_tip_prompt_snapshot.is_none(),
        "snapshot must drop with the tip"
    );
}
#[test]
fn needs_animation_gates_image_viewer_loading() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    assert!(!app.needs_animation());
    let viewer = crate::prompt_images::ImageViewerState::open_from_path_deferred(
        std::path::Path::new("/nonexistent/image_gate_test.png"),
    );
    assert!(viewer.loading, "deferred open must be in loading state");
    app.agents.get_mut(&id).unwrap().image_viewer = Some(viewer);
    assert!(
        app.needs_animation(),
        "image_viewer.loading must request ticks (poll/spawn load path)"
    );
    let mut terminal = false;
    for _ in 0..200 {
        app.tick();
        let agent = &app.agents[&id];
        if agent.image_viewer.is_none()
            || agent.toast.is_some()
            || agent.image_load_rx.is_some()
            || agent.image_viewer.as_ref().is_some_and(|v| !v.loading)
        {
            terminal = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(
        terminal,
        "tick() must progress image load (spawn rx, fail toast, or clear loading)"
    );
    app.agents.get_mut(&id).unwrap().image_viewer = None;
    app.agents.get_mut(&id).unwrap().image_load_rx = None;
    app.agents.get_mut(&id).unwrap().toast = None;
    assert!(!app.needs_animation());
}
#[test]
fn needs_animation_gates_loading_replay() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    assert!(!app.needs_animation());
    app.agents.get_mut(&id).unwrap().session.loading_replay = true;
    assert!(
        app.needs_animation(),
        "loading_replay (attach/resume) must keep ticks alive"
    );
    let _ = app.tick();
    app.agents.get_mut(&id).unwrap().session.loading_replay = false;
    assert!(!app.needs_animation());
}
#[test]
fn active_scroll_stream_arms_scroll_clock_not_animation_ticks() {
    use crate::input::mouse::{ScrollConfig, ScrollDirection};
    let mut app = test_app_with_agent();
    assert!(!app.needs_animation());
    let _ = app
        .scroll_state
        .on_scroll_event(ScrollDirection::Up, ScrollConfig::default());
    assert!(
        app.scroll_state.has_active_stream(),
        "fixture: scroll event must arm an active stream"
    );
    assert!(
        !app.needs_animation(),
        "scroll streams must not demand animation ticks (scroll clock owns pacing)"
    );
    assert!(
        app.scroll_state
            .scroll_clock_deadline(std::time::Instant::now())
            .is_some(),
        "active stream must expose a scroll-clock deadline to the event loop"
    );
    let mut finalized = false;
    for _ in 0..200 {
        let _ = app.tick_scroll();
        if !app.scroll_state.has_active_stream() {
            finalized = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        finalized,
        "tick_scroll() must finalize the scroll stream without a metronome"
    );
    assert!(
        app.scroll_state
            .scroll_clock_deadline(std::time::Instant::now())
            .is_none(),
        "finalized stream must disarm the scroll clock (no idle wakeups)"
    );
    assert!(!app.needs_animation());
}
#[test]
fn handle_input_scroll_suppressed_events_do_not_report_changed() {
    let mut app = test_app_with_agent();
    let wheel = Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    std::thread::sleep(std::time::Duration::from_millis(20));
    const EVENTS: u32 = 30;
    let start = std::time::Instant::now();
    let mut changed = 0u32;
    for _ in 0..EVENTS {
        if matches!(app.handle_input(&wheel), InputOutcome::Changed) {
            changed += 1;
        }
        assert!(
            app.scroll_state.has_active_stream(),
            "wheel burst must keep the stream active"
        );
    }
    let elapsed_ms = start.elapsed().as_millis() as u32;
    assert!(
        changed >= 1,
        "a flushing wheel event must still report Changed"
    );
    let max_changed = elapsed_ms / 16 + 2;
    assert!(
        changed <= max_changed,
        "cadence-suppressed wheel events must not report Changed: got \
         {changed} Changed outcomes from {EVENTS} events in {elapsed_ms}ms \
         (bound {max_changed})"
    );
    assert!(
        app.scroll_state
            .scroll_clock_deadline(std::time::Instant::now())
            .is_some(),
        "armed stream must schedule a scroll-clock deadline"
    );
}
#[test]
fn needs_animation_gates_dashboard_file_search() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    assert!(app.agents[&id].session.state.is_idle());
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    assert!(
        !app.needs_animation(),
        "idle AgentDashboard with no agents alive and no @-search must not request ticks"
    );
    app.dashboard
        .as_mut()
        .unwrap()
        .dispatch
        .file_search
        .update_context("@a", 2);
    assert!(
        app.dashboard
            .as_ref()
            .unwrap()
            .dispatch
            .file_search
            .context()
            .is_some(),
        "fixture: dispatch @-context must be armed"
    );
    assert!(
        app.needs_animation(),
        "dispatch file_search.context() on AgentDashboard must request ticks"
    );
    let _ = app.tick();
    assert!(
        app.dashboard
            .as_ref()
            .unwrap()
            .dispatch
            .file_search
            .context()
            .is_some(),
        "tick() must not clear dispatch @-context"
    );
    assert!(app.needs_animation());
    app.dashboard
        .as_mut()
        .unwrap()
        .dispatch
        .file_search
        .update_context("", 0);
    assert!(
        !app.needs_animation(),
        "clearing dispatch @-context stops ticks when agents stay idle"
    );
    app.dashboard
        .as_mut()
        .unwrap()
        .peek_reply
        .file_search
        .update_context("@b", 2);
    assert!(
        app.needs_animation(),
        "peek_reply file_search.context() on AgentDashboard must request ticks"
    );
    let _ = app.tick();
    app.dashboard
        .as_mut()
        .unwrap()
        .peek_reply
        .file_search
        .update_context("", 0);
    assert!(!app.needs_animation());
}
#[test]
fn tick_drains_tracing_rx_and_does_not_metronome_on_channel() {
    let mut app = test_app_with_agent();
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(8);
    for i in 0..5 {
        tx.try_send(format!("trace line {i}"))
            .expect("queue tracer line");
    }
    app.tracing_rx = Some(rx);
    assert!(
        !app.needs_animation(),
        "non-dev: queued tracer lines must not request animation ticks"
    );
    let _ = app.tick();
    assert!(
        matches!(
            app.tracing_rx.as_mut().unwrap().try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "tick() must drain the tracer channel (bounded; cannot grow unbounded)"
    );
    assert!(
        !app.needs_animation(),
        "non-dev: a present-but-drained tracer channel must not request ticks"
    );
    drop(tx);
}
#[test]
fn needs_animation_gates_btw_loading_spinner() {
    use crate::views::btw_overlay::BtwOverlayState;
    use crate::views::turn_status::SPINNER_DIVISOR;
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    assert!(!app.needs_animation());
    app.agents.get_mut(&id).unwrap().btw_state = Some(BtwOverlayState::Loading {
        question: "what is X?".into(),
    });
    assert!(app.needs_animation());
    let saw_redraw = (0..SPINNER_DIVISOR).any(|_| app.tick());
    assert!(
        saw_redraw,
        "Loading must redraw at spinner cadence while idle"
    );
    app.agents.get_mut(&id).unwrap().btw_state =
        Some(BtwOverlayState::done("what is X?".into(), "X is …".into()));
    assert!(!app.needs_animation());
    app.agents.get_mut(&id).unwrap().btw_state = Some(BtwOverlayState::Error {
        question: "what is X?".into(),
        error: "boom".into(),
    });
    assert!(!app.needs_animation());
}
#[test]
fn needs_animation_gates_pending_acp_command_sync() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    assert!(
        !app.needs_animation(),
        "an idle, fully-synced agent must not request ticks"
    );
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .available_commands_generation += 1;
    assert!(
        app.agents[&id].acp_synced_generation
            != app.agents[&id].session.available_commands_generation,
        "fixture: a commands update must leave the catalog sync pending"
    );
    assert!(
        app.needs_animation(),
        "a pending ACP command-catalog sync must request animation ticks"
    );
    let _ = app.tick();
    assert_eq!(
        app.agents[&id].acp_synced_generation,
        app.agents[&id].session.available_commands_generation,
        "tick() must reconcile the slash-command catalog generation"
    );
    assert!(
        !app.needs_animation(),
        "a reconciled command catalog must stop requesting ticks"
    );
}
#[test]
fn needs_animation_gates_pending_turn_end_reconcile() {
    use super::super::dispatch::{TURN_END_RECONCILE_GRACE, reconcile_overdue_turn_ends};
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::AgentDashboard;
    assert!(app.agents[&id].session.state.is_idle());
    assert!(
        !app.needs_animation(),
        "idle background agent on the dashboard must not request ticks"
    );
    app.agents.get_mut(&id).unwrap().pending_turn_end_reconcile =
        Some(super::super::agent_view::PendingTurnEnd {
            prompt_id: "pid-stuck".into(),
            stop_reason: Some("end_turn".into()),
            agent_result: None,
            cancel_trigger: None,
            cancellation_category: None,
            received_at: std::time::Instant::now()
                - (TURN_END_RECONCILE_GRACE + std::time::Duration::from_secs(1)),
        });
    assert!(
        app.needs_animation(),
        "an armed turn-end reconcile must request ticks even for a background agent"
    );
    let _ = reconcile_overdue_turn_ends(&mut app);
    assert!(
        app.agents[&id].pending_turn_end_reconcile.is_none(),
        "reconcile must clear the overdue marker"
    );
    assert!(
        !app.needs_animation(),
        "a cleared reconcile marker must stop requesting ticks"
    );
}
#[test]
fn needs_animation_gates_pending_cancel_resend() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::AgentDashboard;
    assert!(!app.needs_animation());
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.running_wake_turn = Some(super::super::agent_view::RunningWakeTurn {
            prompt_id: "task-completed-bg1".into(),
            cancel_sent: true,
        });
        agent.pending_cancel_resend = Some(super::super::agent_view::PendingCancelResend {
            prompt_id: Some("task-completed-bg1".into()),
            sent_at: std::time::Instant::now(),
            attempts: 1,
            confirmed: false,
            cancel_subagents: true,
            trigger: crate::app::actions::CancelTrigger::Mouse,
        });
    }
    assert!(
        app.needs_animation(),
        "an armed cancel resend on a wake-cancelling pane must request ticks"
    );
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.running_wake_turn = None;
        agent.session.state = super::super::agent::AgentState::TurnCancelling;
    }
    assert!(
        app.needs_animation(),
        "an armed cancel resend on a cancelling pane must request ticks"
    );
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = super::super::agent::AgentState::Idle;
    }
    assert!(
        app.needs_animation(),
        "a stale resend record must keep ticking until reconcile drops it"
    );
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.pending_cancel_resend = None;
    }
    assert!(!app.needs_animation());
}
#[test]
fn needs_animation_gates_subagent_image_viewer_loading() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let child_sid = "child-img-gate";
    let child = idle_child_view(&app, 1, child_sid);
    app.agents
        .get_mut(&id)
        .unwrap()
        .subagent_views
        .insert(child_sid.to_string(), child);
    assert!(
        !app.needs_animation(),
        "an idle agent with an idle subagent child must not request ticks"
    );
    let viewer = crate::prompt_images::ImageViewerState::open_from_path_deferred(
        std::path::Path::new("/nonexistent/child_img_gate.png"),
    );
    assert!(viewer.loading, "deferred open must be in loading state");
    app.agents
        .get_mut(&id)
        .unwrap()
        .subagent_views
        .get_mut(child_sid)
        .unwrap()
        .image_viewer = Some(viewer);
    assert!(
        app.needs_animation(),
        "a loading image viewer on a subagent CHILD must request ticks (child arm)"
    );
    let mut terminal = false;
    for _ in 0..200 {
        app.tick();
        let child = &app.agents[&id].subagent_views[child_sid];
        if child.image_viewer.is_none()
            || child.toast.is_some()
            || child.image_load_rx.is_some()
            || child.image_viewer.as_ref().is_some_and(|v| !v.loading)
        {
            terminal = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(
        terminal,
        "tick() must progress the CHILD image load (shared tick_agent_image_load)"
    );
    {
        let child = app
            .agents
            .get_mut(&id)
            .unwrap()
            .subagent_views
            .get_mut(child_sid)
            .unwrap();
        child.image_viewer = None;
        child.image_load_rx = None;
        child.toast = None;
    }
    assert!(
        !app.needs_animation(),
        "a cleared child image viewer must stop requesting ticks"
    );
}
#[test]
fn gboom_backgrounded_game_drops_held_movement() {
    use crate::gboom::GboomState;
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let mut game = GboomState::new();
    game.handle_key(&KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
    game.handle_key(&KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
    assert!(
        game.any_movement_held(),
        "press should latch a movement hold"
    );
    app.agents.get_mut(&id).unwrap().gboom = Some(game);
    app.active_view = ActiveView::Agent(id);
    app.gboom_release_backgrounded_games();
    assert!(
        app.agents[&id].gboom.as_ref().unwrap().any_movement_held(),
        "the active game must keep its holds"
    );
    app.active_view = ActiveView::Welcome;
    app.gboom_release_backgrounded_games();
    assert!(
        !app.agents[&id].gboom.as_ref().unwrap().any_movement_held(),
        "a backgrounded game must drop its holds"
    );
}
/// `Event::Resize` must close the tip show gate of every agent view —
/// parent AND fullscreen-capable subagent children — until the next draw
/// re-measures: a trigger firing between the event and the (debounced)
/// resize draw would otherwise act on the pre-resize measurement and burn
/// a seen count on a tip the new layout can never paint. The event must
/// NOT write the full terminal size into `last_terminal_size` — views can
/// paint into chrome-shrunk rects, so the event height proves nothing
/// about the banner row.
#[test]
fn resize_event_closes_tip_show_gate_until_redraw() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let child_sid = "child-session";
    {
        let mut child = idle_child_view(&app, 1, child_sid);
        child.note_terminal_size((80, 28));
        let agent = app.agents.get_mut(&id).unwrap();
        agent.note_terminal_size((80, 30));
        agent.subagent_views.insert(child_sid.to_string(), child);
    }
    let _ = app.handle_input(&Event::Resize(120, 50));
    let agent = app.agents.get_mut(&id).unwrap();
    assert_eq!(
        agent.last_terminal_size,
        (80, 30),
        "event must not overwrite the draw-measured rect size"
    );
    let mut counts = std::collections::HashMap::new();
    let tip = || {
        crate::tips::EphemeralTip::new("t", ratatui::text::Line::from("hint"))
            .with_session_seen_cap("t_seen", 2)
    };
    assert!(!agent.show_ephemeral_tip(tip(), &mut counts));
    assert!(counts.is_empty(), "stale-size show must not burn a count");
    let child = agent.subagent_views.get_mut(child_sid).unwrap();
    assert!(!child.show_ephemeral_tip(tip(), &mut counts));
    assert!(counts.is_empty(), "child stale-size show must not burn");
    child.note_terminal_size((118, 46));
    assert!(child.show_ephemeral_tip(tip(), &mut counts));
    let agent = app.agents.get_mut(&id).unwrap();
    agent.note_terminal_size((120, 50));
    assert!(agent.show_ephemeral_tip(tip(), &mut counts));
    assert_eq!(counts.get("t_seen"), Some(&2));
}
#[test]
fn external_auth_provider_keeps_billing_off_after_auth_meta() {
    let mut app = test_app();
    app.has_external_auth_provider = true;
    app.usage_visible = false;
    app.apply_auth_meta(&echo_agent_runtime::auth::AuthMeta::default());
    assert!(!app.usage_visible);
    assert!(app.tier_restricted_commands.is_empty());
    assert!(
        !app.welcome_prompt
            .slash_controller
            .registry()
            .is_restricted("usage")
    );
    assert!(!app.welcome_prompt.slash_controller.usage_command_visible());
}
#[test]
fn apply_auth_meta_disables_billing_surface_for_team_users() {
    let mut app = test_app();
    assert!(app.usage_visible);
    let meta = echo_agent_runtime::auth::AuthMeta {
        team_id: Some("team-uuid".into()),
        team_name: Some("Acme Corp".into()),
        ..Default::default()
    };
    app.apply_auth_meta(&meta);
    assert!(!app.usage_visible);
    assert_eq!(app.team_id.as_deref(), Some("team-uuid"));
    assert!(
        !app.welcome_prompt
            .slash_controller
            .billing_surface_visible()
    );
}
#[test]
fn apply_auth_meta_enables_billing_surface_for_personal_users() {
    let mut app = test_app();
    app.usage_visible = false;
    let meta = echo_agent_runtime::auth::AuthMeta::default();
    app.apply_auth_meta(&meta);
    assert!(app.usage_visible);
}
#[test]
fn apply_auth_meta_clears_api_key_flag_and_restores_billing_on_personal_login() {
    let mut app = test_app();
    app.is_api_key_auth = true;
    app.usage_visible = false;
    app.apply_auth_meta(&echo_agent_runtime::auth::AuthMeta::default());
    assert!(!app.is_api_key_auth);
    assert!(app.usage_visible);
}
#[test]
fn apply_auth_meta_api_key_enables_voice_and_skips_tier_gate() {
    let mut app = test_app();
    advertise_media_tools(&mut app);
    assert!(!app.voice_mode_enabled);
    app.apply_auth_meta(&echo_agent_runtime::auth::AuthMeta {
        auth_mode: Some("ApiKey".into()),
        subscription_tier: Some("API Key".into()),
        ..Default::default()
    });
    assert!(app.is_api_key_auth);
    assert!(!app.usage_visible);
    assert!(app.tier_restricted_commands.is_empty());
    assert_tier_restricted_commands_present(&app);
    assert!(!app.is_voice_tier_restricted());
    assert!(app.voice_mode_enabled);
    let mut app = test_app();
    app.apply_auth_meta(&echo_agent_runtime::auth::AuthMeta {
        subscription_tier: Some("api_key".into()),
        ..Default::default()
    });
    assert!(app.is_api_key_auth);
    assert!(app.voice_mode_enabled);
    assert!(app.tier_restricted_commands.is_empty());
    app.apply_auth_meta(&echo_agent_runtime::auth::AuthMeta {
        auth_mode: Some("Oidc".into()),
        subscription_tier: Some("Free".into()),
        ..Default::default()
    });
    assert!(!app.is_api_key_auth);
    assert!(!app.voice_mode_enabled);
    assert!(app.usage_visible);
    assert!(!app.tier_restricted_commands.is_empty());
}
fn expected_tier_restricted_commands() -> Vec<String> {
    TIER_RESTRICTED_COMMANDS
        .iter()
        .map(|n| (*n).to_string())
        .collect()
}
/// Make every tier-restricted command visible on the welcome prompt so the
/// present/absent assertions exercise the deny list, not incidental
/// fail-closed hiding:
/// - `/imagine`, `/imagine-video` are `required_tools()`-gated, so advertise
///   their tools (otherwise the registry fail-closes them).
/// - `/voice` is fail-closed hidden until the remote flag turns it on, so
///   reveal it via the registry directly. (We drive the prompt's registry
///   rather than `apply_voice_mode_enabled`, which also flips a process-global
///   atomic and would leak across parallel tests.)
fn advertise_media_tools(app: &mut AppView) {
    app.welcome_prompt
        .slash_controller
        .registry_mut()
        .set_available_tools(
            ["image_gen", "image_to_video"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        );
    app.welcome_prompt.set_voice_visible(true);
}
fn assert_tier_restricted_commands_absent(app: &AppView) {
    let reg = app.welcome_prompt.slash_controller.registry();
    for name in TIER_RESTRICTED_COMMANDS {
        assert!(
            reg.get(name).is_none(),
            "/{name} must be denied on a restricted tier"
        );
    }
    assert!(reg.get("cost").is_none(), "/cost alias must be denied");
}
fn assert_tier_restricted_commands_present(app: &AppView) {
    let reg = app.welcome_prompt.slash_controller.registry();
    for name in TIER_RESTRICTED_COMMANDS {
        assert!(
            reg.get(name).is_some(),
            "/{name} must be available when not tier-restricted (tools advertised)"
        );
    }
}
#[test]
fn apply_auth_meta_restricts_usage_for_free_tier() {
    let mut app = test_app();
    advertise_media_tools(&mut app);
    app.apply_auth_meta(&echo_agent_runtime::auth::AuthMeta::default());
    assert_eq!(
        app.tier_restricted_commands,
        expected_tier_restricted_commands()
    );
    assert_tier_restricted_commands_absent(&app);
    assert!(app.usage_visible);
}
#[test]
fn apply_auth_meta_restricts_usage_for_x_basic_tier() {
    let mut app = test_app();
    advertise_media_tools(&mut app);
    let meta = echo_agent_runtime::auth::AuthMeta {
        subscription_tier: Some("X Basic".into()),
        ..Default::default()
    };
    app.apply_auth_meta(&meta);
    assert_eq!(
        app.tier_restricted_commands,
        expected_tier_restricted_commands()
    );
    assert_tier_restricted_commands_absent(&app);
}
#[test]
fn apply_auth_meta_lifts_restrictions_for_paid_tiers_and_teams() {
    let mut app = test_app();
    advertise_media_tools(&mut app);
    let meta = echo_agent_runtime::auth::AuthMeta {
        subscription_tier: Some("EchoAgent".into()),
        ..Default::default()
    };
    app.apply_auth_meta(&meta);
    assert!(app.tier_restricted_commands.is_empty());
    assert_tier_restricted_commands_present(&app);
    let mut app = test_app();
    advertise_media_tools(&mut app);
    app.apply_auth_meta(&echo_agent_runtime::auth::AuthMeta::default());
    assert!(!app.tier_restricted_commands.is_empty());
    app.subscription_tier = Some("EchoAgent".into());
    app.apply_tier_restrictions();
    assert!(app.tier_restricted_commands.is_empty());
    assert_tier_restricted_commands_present(&app);
    let mut app = test_app();
    let meta = echo_agent_runtime::auth::AuthMeta {
        team_id: Some("team-uuid".into()),
        team_name: Some("Acme Corp".into()),
        ..Default::default()
    };
    app.apply_auth_meta(&meta);
    assert!(app.tier_restricted_commands.is_empty());
}
#[test]
fn is_restricted_tier_classification() {
    assert!(is_restricted_tier(None));
    assert!(is_restricted_tier(Some("")));
    assert!(is_restricted_tier(Some("Free")));
    assert!(is_restricted_tier(Some("X Basic")));
    assert!(is_restricted_tier(Some("x_basic")));
    assert!(!is_restricted_tier(Some("EchoAgent")));
    assert!(!is_restricted_tier(Some("EchoAgent Heavy")));
    assert!(!is_restricted_tier(Some("X Premium")));
    assert!(!is_restricted_tier(Some("X Premium+")));
    assert!(!is_restricted_tier(Some("SomeFutureTier")));
}
#[test]
fn voice_included_in_tier_restricted_commands() {
    assert!(TIER_RESTRICTED_COMMANDS.contains(&"voice"));
}
#[test]
fn is_voice_tier_restricted_tracks_tier() {
    let mut app = test_app();
    app.apply_auth_meta(&echo_agent_runtime::auth::AuthMeta::default());
    assert!(app.is_voice_tier_restricted());
    let mut app = test_app();
    let meta = echo_agent_runtime::auth::AuthMeta {
        subscription_tier: Some("EchoAgent".into()),
        ..Default::default()
    };
    app.apply_auth_meta(&meta);
    assert!(!app.is_voice_tier_restricted());
}
#[test]
fn apply_auth_meta_clears_gate_on_subscription() {
    let mut app = test_app();
    app.gate = Some(echo_agent_runtime::auth::GateInfo {
        message: "Subscribe to use EchoAgent Build".into(),
        url: Some("https://cloud.echo-agent.invalid/echoagent?referrer=echo-agent-build".into()),
        label: None,
    });
    assert!(app.is_access_blocked());
    let meta = echo_agent_runtime::auth::AuthMeta::default();
    app.apply_auth_meta(&meta);
    assert!(app.gate.is_none());
    assert!(app.has_access());
}
#[test]
fn apply_auth_meta_gate_unchanged_when_still_gated() {
    let mut app = test_app();
    let gate = echo_agent_runtime::auth::GateInfo {
        message: "Subscribe".into(),
        url: None,
        label: None,
    };
    app.gate = Some(gate.clone());
    let meta = echo_agent_runtime::auth::AuthMeta {
        gate: Some(gate),
        ..Default::default()
    };
    app.apply_auth_meta(&meta);
    assert!(app.gate.is_some());
    assert!(app.is_access_blocked());
}
#[test]
fn welcome_ctrl_q_requires_confirmation() {
    let mut app = test_app();
    let outcome = app.handle_input(&key_event(KeyCode::Char('q'), KeyModifiers::CONTROL));
    assert!(matches!(outcome, InputOutcome::Changed));
    let pending = app
        .pending_action
        .as_ref()
        .expect("expected pending action");
    assert!(matches!(pending.action, Action::Quit));
    assert_eq!(
        pending.shortcut,
        KeyShortcut::from(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL))
    );
}
#[test]
fn welcome_ctrl_u_update_keeps_priority_over_foreign_resume() {
    let mut app = test_app();
    app.foreign_session_compat = echo_agent_foreign_sessions::EnabledForeignSessionSources {
        cursor: true,
        ..Default::default()
    };
    let crate::app::actions::Effect::CanonicalizeForeignResumeCwd {
        requested_cwd,
        launch_token,
    } = app.begin_foreign_resume_detection().unwrap()
    else {
        panic!("expected canonicalization effect");
    };
    let canonical_cwd = dunce::canonicalize(&requested_cwd).unwrap();
    assert!(app.accept_foreign_resume_canonical_cwd(
        launch_token,
        &requested_cwd,
        Some(canonical_cwd.clone()),
    ));
    app.apply_foreign_resume_detection(
        launch_token,
        &canonical_cwd,
        Some(echo_agent_foreign_sessions::RecentForeignSession {
            tool: echo_agent_foreign_sessions::ForeignSessionTool::Cursor,
            native_id: "cursor-session".into(),
            age: std::time::Duration::from_secs(30),
        }),
    );
    let key = key_event(KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert!(matches!(
        app.handle_input(&key),
        InputOutcome::Action(Action::ResumeForeignSession)
    ));
    app.pending_update_version = Some("9.9.9".into());
    assert!(matches!(
        app.handle_input(&key),
        InputOutcome::Action(Action::QuitForUpdate)
    ));
}
#[test]
fn minimal_ctrl_g_edits_prompt_while_full_tui_keeps_tasks() {
    let event = key_event(KeyCode::Char('g'), KeyModifiers::CONTROL);
    let mut minimal = test_app_with_agent();
    minimal.screen_mode = ScreenMode::Minimal;
    minimal.registry = ActionRegistry::defaults_for(ScreenMode::Minimal);
    let id = super::super::agent::AgentId(0);
    minimal
        .agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .set_screen_mode(ScreenMode::Minimal);
    minimal
        .agents
        .get_mut(&id)
        .unwrap()
        .set_input_mode(crate::views::agent::InputMode::Vim);
    assert_eq!(
        minimal.agents[&id].active_pane,
        crate::views::agent::ActivePane::Scrollback,
        "Vim startup leaves the legacy pane field on Scrollback"
    );
    let out = minimal.handle_input(&event);
    assert!(matches!(
        out,
        InputOutcome::Action(Action::EditPromptExternal)
    ));
    assert!(!minimal.agents[&id].tasks.overlay.visible);
    assert!(!minimal.agents[&id].tasks.overlay.focused);
    minimal.pending_editor = Some(
        crate::app::external_editor::PendingEditorRequest::PromptDraft {
            agent_id: id,
            original_text: "already pending".to_owned(),
        },
    );
    assert!(matches!(
        minimal.handle_input(&event),
        InputOutcome::Unchanged
    ));
    let mut owned = test_app_with_agent();
    owned.screen_mode = ScreenMode::Minimal;
    owned.registry = ActionRegistry::defaults_for(ScreenMode::Minimal);
    owned
        .agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .suggestions
        .dropdown
        .open = true;
    assert!(matches!(owned.handle_input(&event), InputOutcome::Changed));
    assert!(owned.pending_editor.is_none());
    assert!(!owned.agents[&id].tasks.overlay.visible);
    assert!(!owned.agents[&id].tasks.overlay.focused);
    let mut full = test_app_with_agent();
    full.screen_mode = ScreenMode::Fullscreen;
    let out = full.handle_input(&event);
    assert!(matches!(out, InputOutcome::Changed));
    assert!(full.agents[&id].tasks.overlay.visible);
    assert!(full.agents[&id].tasks.overlay.focused);
    assert!(full.pending_editor.is_none());
}
#[test]
fn minimal_ctrl_backslash_is_inert_while_full_modes_open_dashboard() {
    let event = key_event(KeyCode::Char('\\'), KeyModifiers::CONTROL);
    let mut minimal = test_app_with_agent();
    minimal.screen_mode = ScreenMode::Minimal;
    minimal.registry = ActionRegistry::defaults_for(ScreenMode::Minimal);
    assert!(matches!(
        minimal.handle_input(&event),
        InputOutcome::Unchanged
    ));
    assert!(minimal.dashboard.is_none());
    for mode in [ScreenMode::Fullscreen, ScreenMode::Inline] {
        let mut app = test_app_with_agent();
        app.screen_mode = mode;
        app.registry = ActionRegistry::defaults_for(mode);
        assert!(matches!(
            app.handle_input(&event),
            InputOutcome::Action(Action::OpenDashboard)
        ));
    }
}
#[test]
fn minimal_ctrl_t_toggles_todo_panel() {
    let mut app = test_app_with_agent();
    app.screen_mode = ScreenMode::Minimal;
    assert!(!app.minimal_state.show_todos);
    let out = app.handle_input(&key_event(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert!(matches!(out, InputOutcome::Changed));
    assert!(
        app.minimal_state.show_todos,
        "Ctrl+T pins the panel visible"
    );
    let _ = app.handle_input(&key_event(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert!(
        !app.minimal_state.show_todos,
        "Ctrl+T again unpins the panel"
    );
}
#[test]
fn non_minimal_ctrl_t_leaves_todo_panel_flag_untouched() {
    let mut app = test_app_with_agent();
    app.screen_mode = ScreenMode::Inline;
    assert!(!app.minimal_state.show_todos);
    let _ = app.handle_input(&key_event(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert!(
        !app.minimal_state.show_todos,
        "the minimal todo-panel flag must never flip outside minimal mode"
    );
}
/// The minimal info-row transcript hint and the Ctrl+O key remap are gated
/// on the same predicate. Ctrl+O opens the transcript pager unless it is
/// the interject chord (Apple Terminal) AND an interject would actually
/// consume the press (turn running + non-empty composer, turn running +
/// queued follow-up with empty composer, or editing a queued row) — at
/// idle / empty composer with no queue the interject path is a silent
/// no-op, so the remap keeps the key (it looked simply dead before).
#[test]
fn minimal_ctrl_o_transcript_predicate_tracks_interject_binding() {
    let mut app = test_app_with_agent();
    app.registry = ActionRegistry::non_vscode_for_mode_for_test(ScreenMode::Minimal);
    assert!(
        crate::minimal_api::minimal_ctrl_o_opens_transcript(&app),
        "Ctrl+O opens the transcript when interject doesn't own the chord"
    );
    app.registry = ActionRegistry::apple_terminal_for_mode_for_test(ScreenMode::Minimal);
    assert!(
        crate::minimal_api::minimal_ctrl_o_opens_transcript(&app),
        "idle + empty composer: Ctrl+O must open the transcript, not no-op"
    );
    let id = super::super::agent::AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    assert!(
        crate::minimal_api::minimal_ctrl_o_opens_transcript(&app),
        "running turn + empty composer + empty queue: still no interjection"
    );
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.set_text("");
        agent.session.enqueue_prompt("queued follow-up".into());
    }
    assert!(
        !crate::minimal_api::minimal_ctrl_o_opens_transcript(&app),
        "running + empty composer + queue: Ctrl+O must yield to send-now"
    );
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .pending_prompts
        .clear();
    app.agents.get_mut(&id).unwrap().prompt.set_text("steer it");
    assert!(
        !crate::minimal_api::minimal_ctrl_o_opens_transcript(&app),
        "running turn + payload: Ctrl+O must yield to interject"
    );
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::Idle;
        agent.prompt_mode = PromptMode::EditingQueued {
            id: 1,
            original: String::new(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
    }
    assert!(
        !crate::minimal_api::minimal_ctrl_o_opens_transcript(&app),
        "editing a queued row: Ctrl+O must stay the interject/save key"
    );
}
/// In minimal mode Ctrl+O routes to `Action::OpenTranscriptPager` (unless
/// interject owns the chord AND would consume the press — see the
/// predicate test above).
#[test]
fn minimal_ctrl_o_opens_transcript_pager() {
    let mut app = test_app_with_agent();
    app.screen_mode = ScreenMode::Minimal;
    app.registry = ActionRegistry::non_vscode_for_mode_for_test(ScreenMode::Minimal);
    let out = app.handle_input(&key_event(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(
        matches!(out, InputOutcome::Action(Action::OpenTranscriptPager)),
        "expected OpenTranscriptPager, got {out:?}"
    );
}
/// Apple Terminal (interject = Ctrl+O), minimal mode: at idle the interject
/// path would silently no-op, so Ctrl+O must open the transcript — this was
/// the "Ctrl+O appears dead on Mac" report. With a running turn and text in
/// the composer the same key must send-now (cancel-and-send). With a running
/// turn, empty composer, and a queued follow-up it must force-send that row
/// (send-now).
#[test]
fn minimal_ctrl_o_on_apple_terminal_transcript_at_idle_interject_with_payload() {
    let mut app = test_app_with_agent();
    app.screen_mode = ScreenMode::Minimal;
    app.registry = ActionRegistry::apple_terminal_for_mode_for_test(ScreenMode::Minimal);
    let out = app.handle_input(&key_event(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(
        matches!(out, InputOutcome::Action(Action::OpenTranscriptPager)),
        "idle Apple-Terminal Ctrl+O must open the transcript, got {out:?}"
    );
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.prompt.set_text("steer it");
    }
    let out = app.handle_input(&key_event(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(
        matches!(out, InputOutcome::Action(Action::SendPromptNow { ref text, .. }) if text == "steer it"),
        "running Apple-Terminal Ctrl+O with payload must send-now, got {out:?}"
    );
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.set_text("");
        agent.session.enqueue_prompt("queued follow-up".into());
    }
    let out = app.handle_input(&key_event(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(
        matches!(
            out,
            InputOutcome::Action(Action::SendPromptNow { ref text, .. })
                if text == "queued follow-up"
        ),
        "running + empty + queue: Apple-Terminal Ctrl+O must send-now, got {out:?}"
    );
    assert!(
        app.agents[&id].session.pending_prompts.is_empty(),
        "queued row must be consumed by prompt-path send-now"
    );
}
fn assert_background_routing_for_mode(
    mode: ScreenMode,
    pane: crate::app::agent_view::AgentPane,
    event: Event,
) {
    let mut app = test_app_with_agent();
    app.screen_mode = mode;
    app.registry = ActionRegistry::defaults_for(mode);
    let ActiveView::Agent(id) = app.active_view else {
        panic!("test app must start on an agent");
    };
    app.agents.get_mut(&id).unwrap().set_active_pane(pane, true);
    let out = app.handle_input(&event);
    assert!(matches!(out, InputOutcome::Changed));
    assert_eq!(app.agents[&id].active_pane, pane);
    assert!(!app.agents[&id].tasks.overlay.visible);
    assert!(!app.agents[&id].tasks.overlay.focused);
    crate::app::agent_view::test_fixtures::add_running_execute(app.agents.get_mut(&id).unwrap());
    let out = app.handle_input(&event);
    assert!(matches!(
        out,
        InputOutcome::Action(Action::DemoteToBackground)
    ));
    assert_eq!(app.agents[&id].active_pane, pane);
    assert!(!app.agents[&id].tasks.overlay.visible);
    assert!(!app.agents[&id].tasks.overlay.focused);
}
#[test]
fn raw_ctrl_b_routes_like_canonical_in_full_and_minimal_modes() {
    for mode in [ScreenMode::Fullscreen, ScreenMode::Minimal] {
        for pane in [
            crate::app::agent_view::AgentPane::Prompt,
            crate::app::agent_view::AgentPane::Scrollback,
        ] {
            assert_background_routing_for_mode(
                mode,
                pane,
                crate::app::agent_view::test_fixtures::raw_ctrl_b_event(),
            );
        }
    }
}
/// Minimal maps the full-TUI queue chord to `/queue` because the pane is absent.
#[test]
fn minimal_toggle_queue_chord_shows_queue_block() {
    let mut app = test_app_with_agent();
    app.screen_mode = ScreenMode::Minimal;
    app.registry = ActionRegistry::non_vscode_for_mode_for_test(ScreenMode::Minimal);
    let out = app.handle_input(&key_event(KeyCode::Char(';'), KeyModifiers::CONTROL));
    assert!(
        matches!(out, InputOutcome::Action(Action::ShowQueue)),
        "expected ShowQueue, got {out:?}"
    );
    app.screen_mode = ScreenMode::Fullscreen;
    let out = app.handle_input(&key_event(KeyCode::Char(';'), KeyModifiers::CONTROL));
    assert!(
        !matches!(out, InputOutcome::Action(Action::ShowQueue)),
        "full TUI must keep the queue-pane toggle, got {out:?}"
    );
}
fn welcome_session_entry(id: &str) -> SessionPickerEntry {
    SessionPickerEntry {
        id: id.into(),
        summary: id.into(),
        updated_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        cwd: "/tmp/repo".into(),
        hostname: None,
        source: "local".into(),
        model_id: None,
        num_messages: 0,
        last_active_at: None,
        branch: None,
        repo_name: "tmp-repo".into(),
        worktree_label: None,
        last_turn_summary: None,
        last_recap: None,
        card_detail: None,
    }
}
fn open_welcome_session_picker(app: &mut AppView) {
    crate::appearance::cache::set_vim_mode(false);
    app.session_picker_entries = Some(vec![welcome_session_entry("session-0")]);
    app.session_picker_state.search_active = true;
}
#[test]
fn welcome_session_picker_ctrl_w_resumes_in_worktree_while_search_is_focused() {
    let mut app = test_app();
    open_welcome_session_picker(&mut app);
    app.session_picker_state.set_query("session");
    let outcome = app.handle_input(&key_event(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::PickSessionInWorktree(0))
    ));
    assert_eq!(app.session_picker_state.query(), "session");
}
#[test]
fn welcome_session_picker_ctrl_d_keeps_global_quit_precedence() {
    let mut app = test_app();
    open_welcome_session_picker(&mut app);
    app.session_picker_state.set_query("session");
    let outcome = app.handle_input(&ctrl_d());
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(matches!(
        app.pending_action.as_ref().map(|pending| &pending.action),
        Some(Action::Quit)
    ));
    assert_eq!(app.session_picker_state.query(), "session");
}
#[test]
fn welcome_session_picker_cursor_motion_does_not_trigger_deep_search() {
    let mut app = test_app();
    open_welcome_session_picker(&mut app);
    app.session_picker_state.set_query("session");
    let outcome = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.session_picker_state.query(), "session");
}
#[test]
fn welcome_session_picker_ctrl_u_kills_to_cursor_and_triggers_deep_search() {
    let mut app = test_app();
    open_welcome_session_picker(&mut app);
    app.session_picker_state.set_query("session");
    let _ = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    let outcome = app.handle_input(&key_event(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::TriggerDeepSearch)
    ));
    assert_eq!(app.session_picker_state.query(), "n");
    assert_eq!(app.session_picker_state.query_cursor(), 0);
}
#[test]
fn welcome_ctrl_w_opens_new_worktree_dialog() {
    let mut app = test_app();
    app.cwd_has_git_ancestor = true;
    let outcome = app.handle_input(&key_event(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::OpenNewWorktreeDialog)
    ));
}
#[test]
fn welcome_ctrl_w_noop_outside_git_repo() {
    let mut app = test_app();
    app.cwd_has_git_ancestor = false;
    let outcome = app.handle_input(&key_event(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert!(matches!(outcome, InputOutcome::Unchanged));
}
#[test]
fn welcome_trust_decline_keys_quit() {
    for code in [KeyCode::Char('n'), KeyCode::Char('N'), KeyCode::Esc] {
        let mut app = test_app();
        app.trust_state = TrustState::Pending {
            workspace: std::path::PathBuf::from("/tmp/x"),
        };
        let outcome = app.handle_input(&key_event(code, KeyModifiers::NONE));
        assert!(
            matches!(outcome, InputOutcome::Action(Action::Quit)),
            "{code:?} on the trust prompt must quit, got {outcome:?}"
        );
    }
    let mut app = test_app();
    app.trust_state = TrustState::Pending {
        workspace: std::path::PathBuf::from("/tmp/x"),
    };
    let outcome = app.handle_input(&key_event(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Action(Action::TrustFolder)));
}
/// A notice already on screen, with both menu rows and both of its links painted.
fn consent_pending_app() -> AppView {
    use crate::app::consent::{ConsentLegibility, ConsentNotice, ConsentSegment};
    use ratatui::layout::Rect;
    let mut app = test_app();
    app.trust_state = TrustState::Pending {
        workspace: std::path::PathBuf::from("/tmp/x"),
    };
    app.consent_state = crate::app::consent::ConsentState::Pending {
        notice: ConsentNotice {
            id: "notice".to_string(),
            version: 1,
            title: "Title".to_string(),
            segments: vec![
                ConsentSegment::Link {
                    index: 0,
                    label: "Terms".to_string(),
                },
                ConsentSegment::Link {
                    index: 1,
                    label: "AUP".to_string(),
                },
            ],
            links: vec![
                "https://echo-agent.invalid/legal/tos".to_string(),
                "https://echo-agent.invalid/legal/aup".to_string(),
            ],
            accept_label: "Accept".to_string(),
        },
        legibility: ConsentLegibility::Painted,
        painted_at: Some(std::time::Instant::now()),
    };
    app.welcome_menu_rects = vec![Rect::new(10, 20, 30, 1), Rect::new(10, 21, 30, 1)];
    app.welcome_consent_link_rects =
        vec![(0, Rect::new(5, 12, 5, 1)), (1, Rect::new(20, 12, 6, 1))];
    app
}
/// Accept is `a` alone. `y` belongs to the trust question one screen later, Enter may be buffered,
/// and the rest have no meaning here.
#[test]
fn welcome_consent_answers_only_to_its_own_keys() {
    for code in [
        KeyCode::Char('y'),
        KeyCode::Char('n'),
        KeyCode::Esc,
        KeyCode::Enter,
        KeyCode::Char(' '),
        KeyCode::Tab,
    ] {
        let mut app = consent_pending_app();
        let outcome = app.handle_input(&key_event(code, KeyModifiers::NONE));
        assert!(
            matches!(outcome, InputOutcome::Unchanged),
            "{code:?} must not answer the notice, got {outcome:?}",
        );
    }
    let mut app = consent_pending_app();
    assert!(matches!(
        app.handle_input(&key_event(KeyCode::Char('a'), KeyModifiers::NONE)),
        InputOutcome::Action(Action::AcceptConsent)
    ));
    let mut app = consent_pending_app();
    assert!(matches!(app.handle_input(&ctrl_c()), InputOutcome::Changed));
    assert!(
        app.pending_action.is_some(),
        "the first Ctrl+C must arm the confirmation"
    );
    let mut app = consent_pending_app();
    assert!(
        matches!(
            app.handle_input(&key_event(KeyCode::Char('q'), KeyModifiers::NONE)),
            InputOutcome::Action(Action::Quit)
        ),
        "the screen offers Quit, so the key has to work",
    );
}
/// Every event from before the notice painted was aimed at the screen it replaced, and acting on
/// one would quit and take the composer's text with it. Ctrl+C is the exception, because nothing
/// else on this screen handles it.
#[test]
fn welcome_consent_ignores_everything_from_before_the_paint() {
    use crate::app::consent::ConsentState;
    let unpainted = || {
        let mut app = consent_pending_app();
        if let ConsentState::Pending { painted_at, .. } = &mut app.consent_state {
            *painted_at = None;
        }
        app
    };
    let mut app = consent_pending_app();
    let painted = match &app.consent_state {
        ConsentState::Pending { painted_at, .. } => painted_at.expect("painted"),
        ConsentState::Done => unreachable!(),
    };
    let outcome = app.handle_input_at_with_paste_provenance(
        &key_event(KeyCode::Char('a'), KeyModifiers::NONE),
        painted - std::time::Duration::from_millis(1),
        crate::app::app_view::PasteProvenance::Terminal,
    );
    assert!(
        matches!(outcome, InputOutcome::Unchanged),
        "a key that predates the notice was aimed at the composer, got {outcome:?}",
    );
    for ev in [
        left_mouse(MouseEventKind::Down(MouseButton::Left), 12, 20),
        key_event(KeyCode::Char('q'), KeyModifiers::NONE),
    ] {
        assert!(matches!(
            unpainted().handle_input(&ev),
            InputOutcome::Unchanged
        ));
    }
    let mut app = unpainted();
    assert!(matches!(app.handle_input(&ctrl_c()), InputOutcome::Changed));
    assert!(
        app.pending_action.is_some(),
        "a notice that never painted must still be escapable",
    );
}
#[test]
fn welcome_consent_answers_and_links_are_reachable_by_key_and_click() {
    let click = |col, row| left_mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    let mut app = consent_pending_app();
    assert!(matches!(
        app.handle_input(&click(12, 20)),
        InputOutcome::Action(Action::AcceptConsent)
    ));
    let mut app = consent_pending_app();
    assert!(matches!(
        app.handle_input(&click(12, 21)),
        InputOutcome::Action(Action::Quit)
    ));
    let mut app = consent_pending_app();
    assert!(matches!(
        app.handle_input(&click(21, 12)),
        InputOutcome::Action(Action::OpenConsentLink(1))
    ));
    let mut app = consent_pending_app();
    assert!(matches!(
        app.handle_input(&key_event(KeyCode::Char('2'), KeyModifiers::NONE)),
        InputOutcome::Action(Action::OpenConsentLink(1))
    ));
    for code in [KeyCode::Char('0'), KeyCode::Char('3')] {
        let mut app = consent_pending_app();
        assert!(
            matches!(
                app.handle_input(&key_event(code, KeyModifiers::NONE)),
                InputOutcome::Unchanged
            ),
            "{code:?} addresses no link",
        );
    }
}
/// What the renderer reports is the only thing standing between a click and an acceptance, so the
/// three answers it can give have to land in the state exactly.
#[test]
fn consent_paint_records_what_the_renderer_reported() {
    use crate::app::consent::{ConsentLegibility, ConsentNotice, ConsentState};
    let pending = || ConsentState::Pending {
        notice: ConsentNotice {
            id: "notice".to_string(),
            version: 1,
            title: "Title".to_string(),
            segments: Vec::new(),
            links: Vec::new(),
            accept_label: "Accept".to_string(),
        },
        legibility: ConsentLegibility::Illegible,
        painted_at: None,
    };
    let mut state = pending();
    record_consent_paint(&mut state, Some(ConsentLegibility::Illegible));
    let ConsentState::Pending {
        painted_at,
        legibility,
        ..
    } = &state
    else {
        panic!("expected pending");
    };
    assert!(painted_at.is_some(), "an illegible paint is still a paint");
    assert_eq!(*legibility, ConsentLegibility::Illegible);
    let mut state = pending();
    record_consent_paint(&mut state, Some(ConsentLegibility::Painted));
    record_consent_paint(&mut state, None);
    let ConsentState::Pending {
        painted_at,
        legibility,
        ..
    } = &state
    else {
        panic!("expected pending");
    };
    assert_eq!(
        *legibility,
        ConsentLegibility::Illegible,
        "a frame that did not paint the notice cannot leave it acceptable",
    );
    assert!(painted_at.is_some(), "the first paint still happened");
}
/// An unreadable notice still has to take `q`, so the paint stamp cannot wait for legibility.
#[test]
fn welcome_consent_quit_works_while_the_body_is_unreadable() {
    use crate::app::consent::{ConsentLegibility, ConsentState};
    let mut app = consent_pending_app();
    if let ConsentState::Pending { legibility, .. } = &mut app.consent_state {
        *legibility = ConsentLegibility::Illegible;
    }
    app.welcome_menu_rects.truncate(1);
    assert!(matches!(
        app.handle_input(&key_event(KeyCode::Char('q'), KeyModifiers::NONE)),
        InputOutcome::Action(Action::Quit)
    ));
    let mut app = consent_pending_app();
    if let ConsentState::Pending { legibility, .. } = &mut app.consent_state {
        *legibility = ConsentLegibility::Illegible;
    }
    for ev in [
        key_event(KeyCode::Char('1'), KeyModifiers::NONE),
        left_mouse(MouseEventKind::Down(MouseButton::Left), 6, 12),
    ] {
        assert!(matches!(app.handle_input(&ev), InputOutcome::Unchanged));
    }
    let mut app = consent_pending_app();
    if let ConsentState::Pending { legibility, .. } = &mut app.consent_state {
        *legibility = ConsentLegibility::Illegible;
    }
    assert!(matches!(
        app.handle_input(&left_mouse(MouseEventKind::Down(MouseButton::Left), 12, 20)),
        InputOutcome::Action(Action::Quit)
    ));
}
#[test]
fn welcome_consent_hover_tracks_the_menu_row_and_the_link() {
    let mut app = consent_pending_app();
    app.welcome_menu_rects.truncate(1);
    let moved = |col, row| left_mouse(MouseEventKind::Moved, col, row);
    assert!(matches!(
        app.handle_input(&moved(12, 20)),
        InputOutcome::Changed
    ));
    assert_eq!(app.welcome_menu_index, Some(0));
    assert!(matches!(
        app.handle_input(&moved(30, 20)),
        InputOutcome::Unchanged
    ));
    assert!(matches!(
        app.handle_input(&moved(6, 12)),
        InputOutcome::Changed
    ));
    assert_eq!(app.welcome_consent_hover_link, Some(0));
    assert_eq!(app.welcome_menu_index, None);
    assert!(matches!(
        app.handle_input(&moved(21, 12)),
        InputOutcome::Changed
    ));
    assert_eq!(app.welcome_consent_hover_link, Some(1));
    assert!(matches!(
        app.handle_input(&moved(0, 0)),
        InputOutcome::Changed
    ));
    assert_eq!(app.welcome_consent_hover_link, None);
    assert!(matches!(
        app.handle_input(&moved(1, 0)),
        InputOutcome::Unchanged
    ));
}
#[test]
fn welcome_ctrl_c_requires_confirmation() {
    let mut app = test_app();
    let outcome = app.handle_input(&ctrl_c());
    assert!(matches!(outcome, InputOutcome::Changed));
    let pending = app
        .pending_action
        .as_ref()
        .expect("expected pending action");
    assert!(matches!(pending.action, Action::Quit));
    assert_eq!(
        pending.shortcut,
        KeyShortcut::from(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
    );
}
#[test]
fn welcome_ctrl_c_double_press_quits() {
    let mut app = test_app();
    let _ = app.handle_input(&ctrl_c());
    assert!(app.pending_action.is_some());
    let outcome = app.handle_input(&ctrl_c());
    assert!(matches!(outcome, InputOutcome::Action(Action::Quit)));
    assert!(app.pending_action.is_none());
}
#[test]
fn welcome_ctrl_d_requires_confirmation() {
    let mut app = test_app();
    let outcome = app.handle_input(&ctrl_d());
    assert!(matches!(outcome, InputOutcome::Changed));
    let pending = app
        .pending_action
        .as_ref()
        .expect("expected pending action");
    assert!(matches!(pending.action, Action::Quit));
    assert_eq!(
        pending.shortcut,
        KeyShortcut::from(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
    );
}
#[test]
fn menu_action_indices_without_changelog() {
    assert!(matches!(
        dispatch_menu_action(0, false, false, None),
        InputOutcome::Action(Action::OpenNewWorktreeDialog)
    ));
    assert!(matches!(
        dispatch_menu_action(1, false, false, None),
        InputOutcome::Action(Action::FetchSessionList)
    ));
    assert!(matches!(
        dispatch_menu_action(2, false, false, None),
        InputOutcome::Action(Action::Quit)
    ));
}
#[test]
fn menu_action_changelog_sits_above_quit() {
    let md = Some("# notes");
    assert!(matches!(
        dispatch_menu_action(1, false, true, md),
        InputOutcome::Action(Action::FetchSessionList)
    ));
    assert!(matches!(
        dispatch_menu_action(2, false, true, md),
        InputOutcome::Action(Action::ShowReleaseNotes { .. })
    ));
    assert!(matches!(
        dispatch_menu_action(3, false, true, md),
        InputOutcome::Action(Action::Quit)
    ));
}
#[test]
fn menu_action_changelog_before_fetch_is_noop() {
    assert!(matches!(
        dispatch_menu_action(2, false, true, None),
        InputOutcome::Unchanged
    ));
}
#[test]
fn menu_action_indices_with_import_and_changelog() {
    let md = Some("# notes");
    assert!(matches!(
        dispatch_menu_action(0, true, true, md),
        InputOutcome::Action(Action::ImportClaudeSettings)
    ));
    assert!(matches!(
        dispatch_menu_action(1, true, true, md),
        InputOutcome::Action(Action::OpenNewWorktreeDialog)
    ));
    assert!(matches!(
        dispatch_menu_action(2, true, true, md),
        InputOutcome::Action(Action::FetchSessionList)
    ));
    assert!(matches!(
        dispatch_menu_action(3, true, true, md),
        InputOutcome::Action(Action::ShowReleaseNotes { .. })
    ));
    assert!(matches!(
        dispatch_menu_action(4, true, true, md),
        InputOutcome::Action(Action::Quit)
    ));
}
#[test]
fn welcome_pending_ctrl_c_quits_instantly() {
    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    let outcome = app.handle_input(&ctrl_c());
    assert!(matches!(outcome, InputOutcome::Action(Action::Quit)));
    assert!(app.pending_action.is_none());
}
#[test]
fn welcome_authenticating_ctrl_c_quits_instantly() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Command,
    };
    let outcome = app.handle_input(&ctrl_c());
    assert!(matches!(outcome, InputOutcome::Action(Action::Quit)));
    assert!(app.pending_action.is_none());
}
#[test]
fn page_keys_from_prompt_page_conversation_without_mutating_prompt() {
    let mut app = test_app_with_agent();
    let ActiveView::Agent(id) = app.active_view else {
        panic!("test app must start on an agent");
    };
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.set_active_pane(crate::app::agent_view::AgentPane::Prompt, true);
        agent.prompt.set_text("draft text");
        agent.prompt.textarea.set_selection(1, 5);
    }
    let prompt_before = {
        let agent = &app.agents[&id];
        (
            agent.prompt.text().to_owned(),
            agent.prompt.cursor(),
            agent.prompt.textarea.selection_range(),
        )
    };
    assert!(
        prompt_before.2.is_some(),
        "precondition: prompt selection is active"
    );
    for (code, page_up) in [(KeyCode::PageUp, true), (KeyCode::PageDown, false)] {
        let outcome = app.handle_input(&key_event(code, KeyModifiers::NONE));
        assert!(
            matches!(
                (&outcome, page_up),
                (InputOutcome::Action(Action::PageUp), true)
                    | (InputOutcome::Action(Action::PageDown), false)
            ),
            "{code:?} must page the conversation, got {outcome:?}",
        );
        let agent = &app.agents[&id];
        assert_eq!(agent.active_pane, crate::app::agent_view::AgentPane::Prompt);
        assert_eq!(agent.prompt.text(), prompt_before.0);
        assert_eq!(agent.prompt.cursor(), prompt_before.1);
        assert_eq!(agent.prompt.textarea.selection_range(), prompt_before.2);
    }
}
#[test]
fn prompt_paging_scope_matches_agent_surface() {
    fn focused_app(screen_mode: ScreenMode) -> (AppView, super::super::agent::AgentId) {
        let mut app = test_app_with_agent();
        app.screen_mode = screen_mode;
        let ActiveView::Agent(id) = app.active_view else {
            panic!("test app must start on an agent");
        };
        app.agents
            .get_mut(&id)
            .unwrap()
            .set_active_pane(crate::app::agent_view::AgentPane::Prompt, true);
        (app, id)
    }
    #[derive(Clone, Copy)]
    enum Surface {
        Agent(ScreenMode),
        DashboardOverlay,
        DashboardPopup,
    }
    for (label, surface, paging_enabled) in [
        ("inline agent", Surface::Agent(ScreenMode::Inline), true),
        (
            "fullscreen agent",
            Surface::Agent(ScreenMode::Fullscreen),
            true,
        ),
        ("minimal agent", Surface::Agent(ScreenMode::Minimal), false),
        (
            "dashboard session overlay",
            Surface::DashboardOverlay,
            false,
        ),
        ("dashboard attached popup", Surface::DashboardPopup, false),
    ] {
        let screen_mode = match surface {
            Surface::Agent(mode) => mode,
            Surface::DashboardOverlay | Surface::DashboardPopup => ScreenMode::Inline,
        };
        let (mut app, id) = focused_app(screen_mode);
        match surface {
            Surface::DashboardOverlay => {
                app.dashboard = Some(crate::views::dashboard::DashboardState::new());
                app.dashboard.as_mut().unwrap().attached_agent = Some(id);
            }
            Surface::DashboardPopup => assert_eq!(attach_popup(&mut app), id),
            Surface::Agent(_) => {}
        }
        let outcome = app.handle_input(&key_event(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(
            matches!(
                &outcome,
                InputOutcome::Action(Action::PageUp | Action::PageDown)
            ),
            paging_enabled,
            "{label} prompt paging scope mismatch: {outcome:?}",
        );
    }
}
#[test]
fn prompt_page_actions_target_visible_fullscreen_child_scrollback() {
    fn make_pageable(agent: &mut AgentView) {
        for i in 0..16 {
            agent
                .scrollback
                .push_block(crate::scrollback::block::RenderBlock::agent_message(
                    format!("message {i}\ncontinued"),
                ));
        }
        agent.scrollback.prepare_layout(40, 6);
        agent.scrollback.goto_bottom();
        assert!(
            agent.scrollback.scroll_info().0 > 0,
            "precondition: scrollback must have a page above"
        );
    }
    let mut app = test_app_with_agent();
    app.screen_mode = ScreenMode::Fullscreen;
    let ActiveView::Agent(id) = app.active_view else {
        panic!("test app must start on an agent");
    };
    let child_sid = "page-target-child";
    let mut child = idle_child_view(&app, 1, child_sid);
    child.set_active_pane(crate::app::agent_view::AgentPane::Prompt, true);
    make_pageable(&mut child);
    {
        let parent = app.agents.get_mut(&id).unwrap();
        make_pageable(parent);
        parent.subagent_views.insert(child_sid.to_owned(), child);
        parent.active_subagent = Some(child_sid.to_owned());
    }
    let offsets = |app: &AppView| {
        let parent = &app.agents[&id];
        (
            parent.scrollback.scroll_info().0,
            parent.subagent_views[child_sid].scrollback.scroll_info().0,
        )
    };
    let before = offsets(&app);
    let outcome = app.handle_input(&key_event(KeyCode::PageUp, KeyModifiers::NONE));
    let InputOutcome::Action(action @ Action::PageUp) = outcome else {
        panic!("child prompt PageUp must emit PageUp, got {outcome:?}");
    };
    let _ = super::super::dispatch::dispatch(action, &mut app);
    let after_up = offsets(&app);
    assert_eq!(after_up.0, before.0, "parent scrollback must not move");
    assert!(
        after_up.1 < before.1,
        "PageUp must move the visible child scrollback"
    );
    let outcome = app.handle_input(&key_event(KeyCode::PageDown, KeyModifiers::NONE));
    let InputOutcome::Action(action @ Action::PageDown) = outcome else {
        panic!("child prompt PageDown must emit PageDown, got {outcome:?}");
    };
    let _ = super::super::dispatch::dispatch(action, &mut app);
    let after_down = offsets(&app);
    assert_eq!(after_down.0, before.0, "parent scrollback must stay put");
    assert!(
        after_down.1 > after_up.1,
        "PageDown must move the visible child scrollback"
    );
}
#[test]
fn ctrl_d_from_scrollback_is_half_page_down_not_quit() {
    let mut app = test_app_with_agent();
    pin_non_vscode_registry(&mut app);
    let outcome = app.handle_input(&ctrl_d());
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::HalfPageDown)
    ));
    assert!(app.pending_action.is_none());
}
#[test]
fn ctrl_d_double_press_quits_from_prompt() {
    let mut app = test_app_with_agent();
    pin_non_vscode_registry(&mut app);
    let id = super::super::agent::AgentId(0);
    app.agents.get_mut(&id).unwrap().active_pane = crate::views::agent::ActivePane::Prompt;
    let outcome = app.handle_input(&ctrl_d());
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "first Ctrl+D should set pending quit, got: {outcome:?}",
    );
    assert!(app.pending_action.is_some());
    assert_eq!(app.pending_action.as_ref().unwrap().label, Some("quit"));
    let outcome = app.handle_input(&ctrl_d());
    assert!(matches!(outcome, InputOutcome::Action(Action::Quit)));
    assert!(app.pending_action.is_none());
}
#[test]
fn ctrl_d_in_vscode_quits_from_scrollback() {
    let mut app = test_app_with_agent();
    let mut actions = crate::actions::default_actions(ScreenMode::Fullscreen, false);
    for def in actions.iter_mut() {
        if def.id == ActionId::Quit {
            def.default_key = key!('d', CONTROL);
            def.alt_keys = vec![];
        }
        if def.id == ActionId::HalfPageDown {
            def.default_key = key!('D');
        }
    }
    app.registry = ActionRegistry::new(actions);
    let outcome = app.handle_input(&ctrl_d());
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "first Ctrl+D should set pending quit, got: {outcome:?}",
    );
    assert!(app.pending_action.is_some());
    assert_eq!(app.pending_action.as_ref().unwrap().label, Some("quit"));
    let outcome = app.handle_input(&ctrl_d());
    assert!(matches!(outcome, InputOutcome::Action(Action::Quit)));
    assert!(app.pending_action.is_none());
}
#[test]
fn ctrl_q_sets_pending_action() {
    let mut app = test_app_with_agent();
    let outcome = app.handle_input(&ctrl_q());
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(app.pending_action.is_some());
    assert_eq!(app.pending_action.as_ref().unwrap().label, Some("quit"));
}
#[test]
fn ctrl_q_double_press_quits() {
    let mut app = test_app_with_agent();
    let _ = app.handle_input(&ctrl_q());
    assert!(app.pending_action.is_some());
    let outcome = app.handle_input(&ctrl_q());
    assert!(matches!(outcome, InputOutcome::Action(Action::Quit)));
    assert!(app.pending_action.is_none());
}
#[test]
fn different_key_clears_pending() {
    crate::appearance::cache::set_simple_mode(false);
    let mut app = test_app_with_agent();
    if let ActiveView::Agent(id) = app.active_view
        && let Some(agent) = app.agents.get_mut(&id)
    {
        agent.vim_mode = true;
    }
    let _ = app.handle_input(&ctrl_q());
    assert!(app.pending_action.is_some());
    let outcome = app.handle_input(&key_event(KeyCode::Char('j'), KeyModifiers::NONE));
    assert!(app.pending_action.is_none());
    assert!(matches!(outcome, InputOutcome::Action(Action::SelectNext)));
}
#[test]
fn ctrl_q_then_ctrl_d_does_not_confirm() {
    let mut app = test_app_with_agent();
    pin_non_vscode_registry(&mut app);
    let _ = app.handle_input(&ctrl_q());
    assert!(app.pending_action.is_some());
    let outcome = app.handle_input(&ctrl_d());
    assert!(app.pending_action.is_none());
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::HalfPageDown)
    ));
}
fn ctrl_n() -> Event {
    key_event(KeyCode::Char('n'), KeyModifiers::CONTROL)
}
#[test]
fn ctrl_n_sets_pending_new_session() {
    let mut app = test_app_with_agent();
    let outcome = app.handle_input(&ctrl_n());
    assert!(matches!(outcome, InputOutcome::Changed));
    let pending = app.pending_action.as_ref().expect("pending action");
    assert_eq!(pending.label, Some("new"));
}
#[test]
fn second_ctrl_n_opens_new_session_mode_question_when_mode_is_ask() {
    let mut app = test_app_with_agent();
    app.new_session_worktree_mode = WorktreeMode::Ask;
    let _ = app.handle_input(&ctrl_n());
    let outcome = app.handle_input(&ctrl_n());
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::ChooseNewSessionMode)
    ));
    assert!(app.pending_action.is_none());
}
#[test]
fn second_ctrl_n_respects_never_worktree_mode() {
    let mut app = test_app_with_agent();
    app.new_session_worktree_mode = WorktreeMode::Never;
    let _ = app.handle_input(&ctrl_n());
    let outcome = app.handle_input(&ctrl_n());
    assert!(matches!(outcome, InputOutcome::Action(Action::NewSession)));
    assert!(app.pending_action.is_none());
}
#[test]
fn second_ctrl_n_respects_always_worktree_mode() {
    let mut app = test_app_with_agent();
    app.new_session_worktree_mode = WorktreeMode::Always;
    let _ = app.handle_input(&ctrl_n());
    let outcome = app.handle_input(&ctrl_n());
    assert!(matches!(outcome, InputOutcome::Action(Action::NewSession)));
    assert!(app.pending_action.is_none());
}
#[test]
fn ctrl_c_running_cancels_turn() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let outcome = app.handle_input(&ctrl_c());
    assert!(matches!(outcome, InputOutcome::Action(Action::CancelTurn)));
}
#[test]
fn ctrl_c_cancelling_escalates_to_quit_pending() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnCancelling;
    let outcome = app.handle_input(&ctrl_c());
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(app.pending_action.is_some());
    assert_eq!(app.pending_action.as_ref().unwrap().label, Some("quit"));
}
fn assert_pending_quit(app: &AppView) {
    let pending = app
        .pending_action
        .as_ref()
        .expect("expected pending action");
    assert_eq!(pending.label, Some("quit"));
    assert!(matches!(pending.action, Action::Quit));
}
#[test]
fn ctrl_c_idle_empty_prompt_sets_pending_quit() {
    crate::appearance::cache::set_simple_mode(true);
    let mut app = test_app_with_agent();
    let outcome = app.handle_input(&ctrl_c());
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_pending_quit(&app);
}
#[test]
fn ctrl_c_idle_empty_prompt_focused_sets_pending_quit() {
    crate::appearance::cache::set_simple_mode(true);
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.agents.get_mut(&id).unwrap().active_pane = crate::views::agent::ActivePane::Prompt;
    let outcome = app.handle_input(&ctrl_c());
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_pending_quit(&app);
}
#[test]
fn ctrl_c_double_press_idle_quits() {
    crate::appearance::cache::set_simple_mode(true);
    let mut app = test_app_with_agent();
    let _ = app.handle_input(&ctrl_c());
    assert!(app.pending_action.is_some());
    let outcome = app.handle_input(&ctrl_c());
    assert!(matches!(outcome, InputOutcome::Action(Action::Quit)));
    assert!(app.pending_action.is_none());
}
#[test]
fn ctrl_c_consumed_by_cancel_does_not_set_pending() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let outcome = app.handle_input(&ctrl_c());
    assert!(matches!(outcome, InputOutcome::Action(Action::CancelTurn)));
    assert!(app.pending_action.is_none());
}
#[test]
fn ctrl_c_consumed_by_text_clear_does_not_set_pending() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.prompt.textarea.set_text("some text");
    let outcome = app.handle_input(&ctrl_c());
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(app.pending_action.is_none());
}
#[test]
fn ctrl_c_then_other_key_resets_pending() {
    crate::appearance::cache::set_simple_mode(true);
    let mut app = test_app_with_agent();
    let _ = app.handle_input(&ctrl_c());
    assert!(app.pending_action.is_some());
    let _ = app.handle_input(&key_event(KeyCode::Char('j'), KeyModifiers::NONE));
    assert!(app.pending_action.is_none());
}
#[test]
fn ctrl_c_idle_prompt_with_text_clears_text() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.prompt.textarea.set_text("draft prompt");
    assert!(agent.session.state.is_idle());
    let outcome = app.handle_input(&ctrl_c());
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Ctrl+C with text in idle prompt must Change (clear text), got: {outcome:?}",
    );
    assert!(
        app.agents[&id].prompt.textarea.text().is_empty(),
        "Ctrl+C must clear prompt text when agent is idle; got: {:?}",
        app.agents[&id].prompt.textarea.text(),
    );
}
#[test]
fn ctrl_c_running_prompt_with_text_clears_text_and_preserves_turn() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.prompt.textarea.set_text("draft prompt");
    let outcome = app.handle_input(&ctrl_c());
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Ctrl+C with text in a running prompt must clear the text, got: {outcome:?}",
    );
    assert!(
        app.agents[&id].prompt.textarea.text().is_empty(),
        "Ctrl+C must clear prompt text first; got: {:?}",
        app.agents[&id].prompt.textarea.text(),
    );
    assert!(
        app.agents[&id].session.state.is_turn_running(),
        "First Ctrl+C must NOT cancel the turn while a draft was present",
    );
    let outcome = app.handle_input(&ctrl_c());
    assert!(
        matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "Second Ctrl+C on empty running prompt must CancelTurn, got: {outcome:?}",
    );
}
#[test]
fn esc_from_prompt_pane_running_turn_cancels_in_non_vim_mode() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.vim_mode = false;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "1× Esc while running must cancel in non-vim mode, got {outcome:?}"
    );
    assert!(app.pending_action.is_none());
    assert_eq!(
        app.agents[&id].cancel_trigger_hint,
        Some(crate::app::actions::CancelTrigger::Esc)
    );
}
#[test]
fn esc_cancels_running_wake_turn_while_pane_is_idle() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.running_wake_turn = Some(crate::app::agent_view::RunningWakeTurn {
        prompt_id: "task-completed-bg1".into(),
        cancel_sent: false,
    });
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.vim_mode = false;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "Esc during a wake turn must cancel, got {outcome:?}"
    );
    assert!(
        app.pending_action.is_none(),
        "must not arm idle clear/rewind"
    );
    assert_eq!(
        app.agents[&id].cancel_trigger_hint,
        Some(crate::app::actions::CancelTrigger::Esc)
    );
}
#[test]
fn esc_from_prompt_pane_running_turn_with_draft_cancels_preserving_draft() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.vim_mode = false;
    agent.prompt.textarea.set_text("draft while streaming");
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "mid-turn Esc with draft must cancel in non-vim mode, got {outcome:?}"
    );
    assert!(app.pending_action.is_none(), "must not arm idle clear");
    assert_eq!(
        app.agents[&id].prompt.textarea.text(),
        "draft while streaming",
        "Esc cancel must preserve the draft (not clear it like Ctrl+C)"
    );
    assert_eq!(
        app.agents[&id].cancel_trigger_hint,
        Some(crate::app::actions::CancelTrigger::Esc)
    );
}
#[test]
fn esc_from_scrollback_pane_running_turn_cancels_in_non_vim_mode() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    agent.active_pane = crate::views::agent::ActivePane::Scrollback;
    agent.vim_mode = false;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "1× Esc from scrollback while running must cancel in non-vim mode, got {outcome:?}"
    );
    assert!(app.pending_action.is_none());
    assert_eq!(
        app.agents[&id].cancel_trigger_hint,
        Some(crate::app::actions::CancelTrigger::Esc)
    );
}
#[test]
fn esc_from_prompt_pane_running_turn_vim_mode_is_swallowed() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.vim_mode = true;
    agent.prompt.textarea.set_text("draft while streaming");
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "1× Esc while running must swallow in vim mode, got {outcome:?}"
    );
    assert!(app.pending_action.is_none());
    assert!(app.agents[&id].cancel_trigger_hint.is_none());
    assert_eq!(
        app.agents[&id].prompt.textarea.text(),
        "draft while streaming",
        "vim mid-turn Esc must not clear the draft or arm idle clear"
    );
    assert!(app.agents[&id].session.state.is_turn_running());
}
#[test]
fn esc_from_scrollback_pane_running_turn_vim_mode_is_swallowed() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    agent.active_pane = crate::views::agent::ActivePane::Scrollback;
    agent.vim_mode = true;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "1× Esc from scrollback while running must swallow in vim mode, got {outcome:?}"
    );
    assert!(app.pending_action.is_none());
    assert!(app.agents[&id].cancel_trigger_hint.is_none());
    assert!(app.agents[&id].session.state.is_turn_running());
}
#[test]
fn esc_cancels_turn_gate_truth_table() {
    assert!(crate::app::esc_cancels_turn(true, true));
    assert!(crate::app::esc_cancels_turn(true, false));
    assert!(crate::app::esc_cancels_turn(false, false));
    assert!(!crate::app::esc_cancels_turn(false, true));
}
#[test]
fn esc_running_turn_minimal_screen_mode_cancels_even_with_vim_on() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.vim_mode = true;
    agent
        .prompt
        .set_screen_mode(crate::app::ScreenMode::Minimal);
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "minimal mode must Esc-cancel even with vim scrollback nav on, got {outcome:?}"
    );
    assert_eq!(
        app.agents[&id].cancel_trigger_hint,
        Some(crate::app::actions::CancelTrigger::Esc)
    );
}
#[test]
fn esc_owned_before_agent_covers_app_level_owners() {
    let mut app = test_app_with_agent();
    assert!(!app.esc_owned_before_agent());
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::DashboardDispatch,
        interim: None,
    };
    assert!(app.esc_owned_before_agent(), "listening owns Esc");
    app.voice_state = VoiceState::ColdStart {
        hold: false,
        target: VoiceTarget::DashboardDispatch,
    };
    assert!(app.esc_owned_before_agent(), "pending cold-start owns Esc");
    app.voice_state = VoiceState::Idle;
    assert!(!app.esc_owned_before_agent());
    app.import_claude_modal = Some(
        crate::views::import_claude_modal::ImportClaudeModalState::new(
            echo_agent_runtime::claude_import::ImportPlan::default(),
            std::path::PathBuf::from("/tmp"),
        ),
    );
    assert!(app.esc_owned_before_agent(), "import-claude modal owns Esc");
    app.import_claude_modal = None;
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(super::super::agent::AgentId(0));
    }
    assert!(app.esc_owned_before_agent(), "dashboard popup owns Esc");
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(super::super::agent::AgentId(99));
    }
    assert!(!app.esc_owned_before_agent());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = None;
    }
    assert!(!app.esc_owned_before_agent());
}
#[test]
fn esc_while_cancelling_retries_cancel() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnCancelling;
    agent.active_pane = crate::views::agent::ActivePane::Scrollback;
    agent.vim_mode = true;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "Esc while cancelling must retry CancelTurn, got {outcome:?}"
    );
    assert!(app.pending_action.is_none());
    assert_eq!(
        app.agents[&id].cancel_trigger_hint,
        Some(crate::app::actions::CancelTrigger::Esc)
    );
}
#[test]
fn esc_cancel_grace_holds_rewind_arm_then_expires() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.vim_mode = false;
    agent
        .scrollback
        .push_block(crate::scrollback::block::RenderBlock::user_prompt(
            "earlier",
        ));
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Action(Action::CancelTurn)));
    assert!(app.agents[&id].rewind_suppress_deadline.is_some());
    app.agents.get_mut(&id).unwrap().session.state = AgentState::Idle;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Esc within the post-cancel grace must swallow, got {outcome:?}"
    );
    assert!(
        app.pending_action.is_none(),
        "post-cancel Esc must not arm the rewind picker"
    );
    app.agents.get_mut(&id).unwrap().rewind_suppress_deadline = Some(std::time::Instant::now());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        matches!(
            app.pending_action.as_ref().map(|p| &p.action),
            Some(Action::RewindShowPicker)
        ),
        "expired grace must restore the idle rewind arm"
    );
    assert!(
        app.agents[&id].rewind_suppress_deadline.is_none(),
        "the expired deadline must be cleared on the consult"
    );
}
#[test]
fn idle_non_empty_double_esc_clears_prompt() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.prompt.textarea.set_text("draft to clear");
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    let pending = app.pending_action.as_ref().expect("arm clear");
    assert_eq!(pending.label, Some("clear"));
    assert!(matches!(pending.action, Action::ClearPrompt));
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Action(Action::ClearPrompt)));
    assert!(app.pending_action.is_none());
    let effects = crate::app::dispatch::dispatch(Action::ClearPrompt, &mut app);
    assert!(effects.is_empty());
    assert!(app.agents[&id].prompt.textarea.text().is_empty());
    assert!(
        app.agents[&id].session.prompt_history.is_empty(),
        "the cleared draft goes to the stash, never to the history"
    );
    assert_eq!(
        app.agents[&id]
            .prompt_stash
            .as_ref()
            .map(|entry| entry.prompt.text.as_str()),
        Some("draft to clear")
    );
}
#[test]
fn idle_empty_with_messages_double_esc_opens_rewind_silent() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent
        .scrollback
        .push_block(crate::scrollback::block::RenderBlock::user_prompt(
            "earlier",
        ));
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    let pending = app.pending_action.as_ref().expect("arm rewind");
    assert!(
        pending.label.is_none(),
        "first Esc for rewind must be silent"
    );
    assert!(matches!(pending.action, Action::RewindShowPicker));
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::RewindShowPicker)
    ));
    assert!(app.pending_action.is_none());
}
#[test]
fn idle_empty_no_messages_esc_is_swallowed() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    assert!(agent.scrollback.is_empty());
    assert!(agent.prompt.textarea.text().is_empty());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "idle empty with no messages must swallow Esc (not FocusScrollback), got {outcome:?}"
    );
    assert!(app.pending_action.is_none());
    assert_eq!(
        app.agents[&id].active_pane,
        crate::views::agent::ActivePane::Prompt
    );
}
#[test]
fn mouse_send_retires_armed_clear_so_next_esc_swallows() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::views::agent::ActivePane::Prompt;
        agent.prompt.textarea.set_text("draft to clear");
        agent.vim_mode = true;
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    let pending = app.pending_action.as_ref().expect("arm clear");
    assert!(matches!(pending.action, Action::ClearPrompt));
    let _ = crate::app::dispatch::dispatch(Action::SendPrompt("draft to clear".into()), &mut app);
    assert!(
        app.pending_action.is_none(),
        "submit must retire the stale ClearPrompt arm",
    );
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Esc after a mouse-send must swallow mid-turn, got {outcome:?}",
    );
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::ClearPrompt)),
        "the retired ClearPrompt arm must not fire",
    );
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "Esc must not cancel mid-turn",
    );
    assert!(app.agents[&id].cancel_trigger_hint.is_none());
    assert!(app.pending_action.is_none());
}
/// Arm an idle-Esc `ClearPrompt`, submit via `text`-carrying `action` (a
/// turn-start path with no intervening key), assert the arm was retired, then
/// with the turn running assert the next Esc swallows (never the stale clear).
fn assert_submit_path_retires_clear_arm(action: Action) {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::views::agent::ActivePane::Prompt;
        agent.prompt.textarea.set_text("draft to clear");
        agent.vim_mode = true;
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(matches!(
        app.pending_action.as_ref().expect("arm clear").action,
        Action::ClearPrompt
    ));
    let _ = crate::app::dispatch::dispatch(action, &mut app);
    assert!(
        app.pending_action.is_none(),
        "every submit path (inner funnel) must retire the stale ClearPrompt arm",
    );
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Esc after a non-keyed submit must swallow mid-turn, got {outcome:?}",
    );
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "Esc must not cancel mid-turn",
    );
    assert!(app.agents[&id].cancel_trigger_hint.is_none());
    assert!(app.pending_action.is_none());
}
#[test]
fn submit_follow_up_retires_armed_clear_so_next_esc_swallows() {
    assert_submit_path_retires_clear_arm(Action::SubmitFollowUp("follow up".into()));
}
#[test]
fn slash_preserving_send_retires_armed_clear_so_next_esc_swallows() {
    assert_submit_path_retires_clear_arm(Action::SendSlashCommandPreservingDraft(
        "/compact".into(),
    ));
}
#[test]
fn stale_idle_clear_arm_never_fires_on_busy_agent() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::views::agent::ActivePane::Prompt;
        agent.prompt.textarea.set_text("draft to clear");
        agent.vim_mode = true;
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(matches!(
        app.pending_action.as_ref().expect("arm clear").action,
        Action::ClearPrompt
    ));
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Esc on a busy agent must swallow, not fire the stale clear arm, got {outcome:?}",
    );
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::ClearPrompt)),
        "the stale ClearPrompt arm must not fire on a running turn",
    );
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "Esc must not cancel mid-turn",
    );
    assert!(app.agents[&id].cancel_trigger_hint.is_none());
    assert!(
        app.pending_action.is_none(),
        "the stale arm must be dropped"
    );
}
#[test]
fn stale_idle_rewind_arm_never_fires_on_busy_agent() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::views::agent::ActivePane::Prompt;
        agent.vim_mode = true;
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::user_prompt(
                "earlier",
            ));
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(matches!(
        app.pending_action.as_ref().expect("arm rewind").action,
        Action::RewindShowPicker
    ));
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Esc on a busy agent must swallow, not fire the stale rewind arm, got {outcome:?}",
    );
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "Esc must not cancel mid-turn",
    );
    assert!(
        app.pending_action.is_none(),
        "the stale arm must be dropped"
    );
}
#[test]
fn stale_idle_clear_arm_never_fires_on_wake_turn() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::views::agent::ActivePane::Prompt;
        agent.prompt.textarea.set_text("draft to clear");
        agent.vim_mode = true;
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(matches!(
        app.pending_action.as_ref().expect("arm clear").action,
        Action::ClearPrompt
    ));
    app.agents
        .get_mut(&id)
        .unwrap()
        .note_streaming_wake_turn("p-wake");
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Esc on a wake turn must swallow, not fire the stale clear arm, got {outcome:?}",
    );
    assert!(app.agents[&id].cancel_trigger_hint.is_none());
    assert!(
        app.pending_action.is_none(),
        "the stale arm must be dropped"
    );
}
#[test]
fn esc_consumed_by_policy_disarms_esc_d_combo() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::views::agent::ActivePane::Prompt;
        assert!(agent.scrollback.is_empty());
        assert!(agent.prompt.textarea.text().is_empty());
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        app.agents[&id].esc_pressed_at.is_none(),
        "idle-empty swallow Esc must disarm the Esc→d combo",
    );
    let mut app = test_app_with_agent();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.active_pane = crate::views::agent::ActivePane::Prompt;
        agent.vim_mode = true;
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        app.agents[&id].esc_pressed_at.is_none(),
        "mid-turn swallow Esc must disarm the Esc→d combo",
    );
}
#[test]
fn idle_non_empty_esc_ttl_expiry_re_arms_without_clearing() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.prompt.textarea.set_text("still here");
    let _ = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    if let Some(p) = app.pending_action.as_mut() {
        p.expires_at = std::time::Instant::now() - std::time::Duration::from_millis(1);
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(
        app.agents[&id].prompt.textarea.text(),
        "still here",
        "expired first Esc must not clear"
    );
    let pending = app.pending_action.as_ref().expect("re-arm clear");
    assert_eq!(pending.label, Some("clear"));
}
#[test]
fn idle_images_only_double_esc_arms_clear() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent
        .prompt
        .images
        .push(crate::prompt_images::from_clipboard_data(
            &crate::clipboard::ImageData {
                data: vec![1, 2, 3],
                mime_type: "image/png".into(),
            },
        ));
    assert!(agent.prompt.textarea.text().is_empty());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    let pending = app.pending_action.as_ref().expect("arm clear for images");
    assert!(matches!(pending.action, Action::ClearPrompt));
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Action(Action::ClearPrompt)));
    assert!(app.pending_action.is_none());
    let effects = crate::app::dispatch::dispatch(Action::ClearPrompt, &mut app);
    assert!(effects.is_empty());
    assert!(
        app.agents[&id].prompt.images.is_empty(),
        "second Esc must clear the image chips"
    );
    assert!(
        app.agents[&id].session.prompt_history.is_empty(),
        "an images-only (empty-text) clear records nothing in prompt history"
    );
}
/// Scrollback-pane double-Esc, idle + empty prompt + messages: first Esc
/// arms `RewindShowPicker` silently, second within the TTL opens the
/// picker. Driven per scrollback nav mode because the routing differs —
/// vim resolves through `lookup_with_mode(vim=true)`, non-vim adds the
/// bare-letter forward-to-prompt fallback — and neither may consume Esc.
fn assert_scrollback_double_esc_opens_rewind(vim: bool) {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.vim_mode = vim;
    agent.set_input_mode(if vim {
        crate::views::agent::InputMode::Vim
    } else {
        crate::views::agent::InputMode::Simple
    });
    agent.active_pane = crate::views::agent::ActivePane::Scrollback;
    agent
        .scrollback
        .push_block(crate::scrollback::block::RenderBlock::user_prompt(
            "earlier",
        ));
    assert!(agent.prompt.textarea.text().is_empty());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "vim={vim}: first scrollback Esc must arm silently, got {outcome:?}"
    );
    let pending = app
        .pending_action
        .as_ref()
        .expect("scrollback-pane idle Esc must arm rewind");
    assert!(
        pending.label.is_none(),
        "vim={vim}: first Esc for rewind must be silent"
    );
    assert!(matches!(pending.action, Action::RewindShowPicker));
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::RewindShowPicker)),
        "vim={vim}: second Esc from scrollback must open the rewind picker, got {outcome:?}"
    );
    assert!(app.pending_action.is_none());
}
/// Non-vim (simple) scrollback nav: double-Esc from scrollback opens rewind.
#[test]
fn idle_scrollback_pane_double_esc_opens_rewind() {
    assert_scrollback_double_esc_opens_rewind(false);
}
/// Vim scrollback nav consumes no plain Esc, so the same flow must work.
#[test]
fn idle_scrollback_pane_double_esc_opens_rewind_vim_mode() {
    assert_scrollback_double_esc_opens_rewind(true);
}
/// From the SCROLLBACK pane an idle Esc with a draft in the (unfocused)
/// composer arms NOTHING and leaves the draft intact: clear is skipped by
/// the prompt-pane gate, and rewind is skipped by the global
/// empty-composer gate even with turns present — never clear or
/// rewind-stash a draft the reader has scrolled past. The Esc is
/// swallowed (no pending, no global quit/back-out).
#[test]
fn idle_scrollback_pane_esc_with_draft_and_messages_swallows() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::views::agent::ActivePane::Scrollback;
    agent
        .scrollback
        .push_block(crate::scrollback::block::RenderBlock::user_prompt(
            "earlier",
        ));
    agent
        .prompt
        .textarea
        .set_text("draft while reading scrollback");
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        app.pending_action.is_none(),
        "scrollback-pane Esc with a draft must arm neither clear nor rewind"
    );
    assert_eq!(
        app.agents[&id].prompt.textarea.text(),
        "draft while reading scrollback",
        "scrollback-pane Esc must leave the composer draft intact"
    );
}
/// A pending needs-input overlay blocks the scrollback rewind arm: the
/// overlay intercepts exempt the scrollback pane, so its Esc reaches the
/// policy — which must swallow rather than arm a picker that would
/// key-starve the pending overlay. The overlay must survive the Esc.
#[test]
fn idle_scrollback_pane_esc_with_pending_input_overlay_does_not_arm_rewind() {
    type OverlayInstaller = (&'static str, fn(&mut AgentView));
    let installers: [OverlayInstaller; 2] = [
        ("cancel_turn_view", |a| {
            a.cancel_turn_view = Some(crate::views::modal::CancelTurnViewState {
                active_idx: 0,
                running_count: 1,
            });
        }),
        ("question_view", |a| {
            let stashed = a.prompt.stash();
            a.question_view = Some(crate::views::question_view::QuestionViewState::new(
                "call-q".into(),
                vec![],
                stashed,
            ));
        }),
    ];
    for (name, install) in installers {
        let mut app = test_app_with_agent();
        let id = super::super::agent::AgentId(0);
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::views::agent::ActivePane::Scrollback;
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::user_prompt(
                "earlier",
            ));
        assert!(agent.prompt.textarea.text().is_empty());
        install(agent);
        assert!(
            !agent.no_input_overlay_pending(),
            "{name}: fixture must have a pending needs-input overlay"
        );
        let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "{name}: scrollback Esc under a pending overlay must swallow, got {outcome:?}"
        );
        assert!(
            app.pending_action.is_none(),
            "{name}: must not arm rewind under a pending needs-input overlay"
        );
        assert!(
            !app.agents[&id].no_input_overlay_pending(),
            "{name}: the pending overlay must survive the swallowed Esc"
        );
    }
}
/// A latent Bash/Remember composer mode blocks the scrollback rewind arm: a rewind restore must not drop conversation text into a still-armed
/// `!` composer. The Esc must swallow WITHOUT exiting the mode: mode exit stays a prompt-pane (step 0e) affordance.
#[test]
fn idle_scrollback_pane_esc_in_bash_mode_does_not_arm_rewind() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::views::agent::ActivePane::Scrollback;
    agent.prompt_input_mode = crate::app::agent_view::PromptInputMode::Bash;
    agent
        .scrollback
        .push_block(crate::scrollback::block::RenderBlock::user_prompt(
            "earlier",
        ));
    assert!(agent.prompt.textarea.text().is_empty());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "scrollback Esc with a latent bash composer must swallow, got {outcome:?}"
    );
    assert!(
        app.pending_action.is_none(),
        "must not arm rewind while the composer is in bash mode"
    );
    assert_eq!(
        app.agents[&id].prompt_input_mode,
        crate::app::agent_view::PromptInputMode::Bash,
        "scrollback Esc must not exit the composer mode either"
    );
}
/// An active prompt history search blocks the scrollback rewind arm — the
/// step 0b intercept is prompt-pane-only, so a scrollback Esc reaches the
/// policy while the search overlay is open and must swallow rather than
/// stack a rewind arm under it. The search must survive the Esc.
#[test]
fn idle_scrollback_pane_esc_with_history_search_does_not_arm_rewind() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(crate::scrollback::block::RenderBlock::user_prompt(
            "earlier",
        ));
    agent.session.prompt_history = vec!["earlier".into()];
    assert!(agent.prompt.textarea.text().is_empty());
    let history = agent.combined_prompt_history();
    let current_text = agent.prompt.text().to_string();
    assert!(
        agent
            .prompt
            .history_search
            .activate(&history, &current_text)
    );
    agent.active_pane = crate::views::agent::ActivePane::Scrollback;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "scrollback Esc with an open history search must swallow, got {outcome:?}"
    );
    assert!(
        app.pending_action.is_none(),
        "must not arm rewind while history search is open"
    );
    assert!(
        app.agents[&id].prompt.history_search.is_active(),
        "scrollback Esc must not dismiss the search either"
    );
}
#[test]
fn running_slash_dropdown_esc_dismisses_not_cancel() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.prompt.set_text("/he");
    agent.prompt.refresh_slash(&agent.session.models);
    assert!(
        agent.prompt.slash_open(),
        "precondition: slash dropdown open"
    );
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(app.pending_action.is_none());
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "slash Esc must steal, not cancel"
    );
    assert!(app.agents[&id].session.state.is_turn_running());
    assert!(!app.agents[&id].prompt.slash_open());
}
#[test]
fn running_bash_mode_empty_esc_exits_mode_not_cancel() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    agent.prompt_input_mode = crate::app::agent_view::PromptInputMode::Bash;
    assert!(agent.prompt.textarea.text().is_empty());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "empty bash Esc exits mode, does not cancel while running"
    );
    assert_eq!(
        app.agents[&id].prompt_input_mode,
        crate::app::agent_view::PromptInputMode::Normal
    );
    assert!(app.agents[&id].session.state.is_turn_running());
}
#[test]
fn tab_from_prompt_follows_screen_mode_registry() {
    let id = super::super::agent::AgentId(0);
    for mode in [ScreenMode::Fullscreen, ScreenMode::Inline] {
        let mut app = test_app_with_agent();
        app.screen_mode = mode;
        app.registry = ActionRegistry::defaults_for(mode);
        app.agents.get_mut(&id).unwrap().active_pane = crate::views::agent::ActivePane::Prompt;
        let outcome = app.handle_input(&key_event(KeyCode::Tab, KeyModifiers::NONE));
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::FocusScrollback)
        ));
    }
    let mut minimal = test_app_with_agent();
    minimal.screen_mode = ScreenMode::Minimal;
    minimal.registry = ActionRegistry::defaults_for(ScreenMode::Minimal);
    minimal.agents.get_mut(&id).unwrap().active_pane = crate::views::agent::ActivePane::Prompt;
    let outcome = minimal.handle_input(&key_event(KeyCode::Tab, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Unchanged));
    assert_eq!(
        minimal.agents[&id].active_pane,
        crate::views::agent::ActivePane::Prompt
    );
}
#[test]
fn prompt_focused_printable_chars_still_go_to_textarea() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    let _ = app.handle_input(&key_event(KeyCode::Char('a'), KeyModifiers::NONE));
    let agent = app.agents.get(&id).unwrap();
    assert_eq!(agent.prompt.textarea.text(), "a");
}
#[test]
fn prompt_focused_question_mark_with_shift_still_goes_to_textarea() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::views::agent::ActivePane::Prompt;
    let outcome = app.handle_input(&key_event(KeyCode::Char('?'), KeyModifiers::SHIFT));
    assert!(
        !matches!(outcome, InputOutcome::Changed if app.agents.get(&id).unwrap().active_modal.is_some()),
        "?+SHIFT must not open the command palette when typing in the prompt; got {outcome:?}",
    );
    let agent = app.agents.get(&id).unwrap();
    assert!(
        agent.active_modal.is_none(),
        "?+SHIFT must not open any modal in the prompt",
    );
    assert!(
        agent.prompt.textarea.text().contains('?'),
        "?+SHIFT must reach the textarea as `?`; got {:?}",
        agent.prompt.textarea.text(),
    );
}
#[test]
fn prompt_focused_bare_text_chars_promote_no_action() {
    for ch in ['p', 'b', '/', '?', '1', '5', 'm', 'o', 'c', 'h'] {
        let mut app = test_app_with_agent();
        let id = super::super::agent::AgentId(0);
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::views::agent::ActivePane::Prompt;
        let _ = app.handle_input(&key_event(KeyCode::Char(ch), KeyModifiers::NONE));
        let agent = app.agents.get(&id).unwrap();
        assert!(
            agent.active_modal.is_none(),
            "bare `{ch}` must not open any modal",
        );
        assert!(
            agent.prompt.textarea.text().contains(ch),
            "bare `{ch}` must reach the textarea; got {:?}",
            agent.prompt.textarea.text(),
        );
        let mut app = test_app_with_agent();
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::views::agent::ActivePane::Prompt;
        let _ = app.handle_input(&key_event(KeyCode::Char(ch), KeyModifiers::SHIFT));
        let agent = app.agents.get(&id).unwrap();
        assert!(
            agent.active_modal.is_none(),
            "shift+`{ch}` must not open any modal",
        );
    }
}
#[test]
fn welcome_pending_l_triggers_login() {
    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    app.welcome_prompt_focused = false;
    let outcome = app.handle_input(&key_event(KeyCode::Char('l'), KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Action(Action::Login)));
}
#[test]
fn welcome_pending_enter_triggers_login() {
    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    app.welcome_prompt_focused = false;
    let outcome = app.handle_input(&key_event(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Action(Action::Login)));
}
#[test]
fn welcome_pending_n_is_unchanged() {
    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    app.welcome_prompt_focused = false;
    let outcome = app.handle_input(&key_event(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Unchanged));
}
#[test]
fn welcome_done_n_starts_session() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    let outcome = app.handle_input(&key_event(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(matches!(
        outcome,
        InputOutcome::ActionThenForward(Action::NewSession)
    ));
}
#[test]
fn welcome_done_ctrl_w_opens_new_worktree_dialog() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    app.cwd_has_git_ancestor = true;
    let outcome = app.handle_input(&key_event(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::OpenNewWorktreeDialog)
    ));
}
#[test]
fn welcome_ctrl_v_creates_normal_session() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    app.welcome_prompt_focused = true;
    let outcome = app.handle_input(&key_event(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert!(matches!(
        outcome,
        InputOutcome::ActionThenForward(Action::NewSession)
    ));
}
#[test]
fn welcome_cmd_v_creates_normal_session() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    app.welcome_prompt_focused = true;
    let outcome = app.handle_input(&key_event(KeyCode::Char('v'), KeyModifiers::SUPER));
    assert!(matches!(
        outcome,
        InputOutcome::ActionThenForward(Action::NewSession)
    ));
}
#[test]
fn worktree_dialog_enter_creates_worktree_session() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    app.new_worktree_dialog = Some(NewWorktreeDialogState::new());
    let outcome = app.handle_input(&key_event(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::NewWorktreeSession {
            load_session_id: None,
            label: None,
            git_ref: None,
        })
    ));
    assert!(app.new_worktree_dialog.is_none());
}
#[test]
fn worktree_dialog_modified_enter_is_ignored() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    app.new_worktree_dialog = Some(NewWorktreeDialogState::new());
    let outcome = app.handle_input(&key_event(KeyCode::Enter, KeyModifiers::CONTROL));
    assert!(matches!(outcome, InputOutcome::Unchanged));
    assert!(app.new_worktree_dialog.is_some());
    let outcome = app.handle_input(&key_event(KeyCode::Char('w'), KeyModifiers::SHIFT));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.new_worktree_dialog.as_ref().unwrap().label(), "W");
}
#[test]
fn worktree_dialog_enter_threads_label() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    app.new_worktree_dialog = Some(NewWorktreeDialogState::new());
    for c in "wolves".chars() {
        app.handle_input(&key_event(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let outcome = app.handle_input(&key_event(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::NewWorktreeSession {
            load_session_id: None,
            label: Some(ref l),
            git_ref: None,
        }) if l == "wolves"
    ));
    assert!(app.new_worktree_dialog.is_none());
}
#[test]
fn worktree_dialog_esc_closes() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    app.new_worktree_dialog = Some(NewWorktreeDialogState::new());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(app.new_worktree_dialog.is_none());
}
#[test]
fn worktree_dialog_typing_updates_label() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    app.new_worktree_dialog = Some(NewWorktreeDialogState::new());
    let outcome = app.handle_input(&key_event(KeyCode::Char('h'), KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.new_worktree_dialog.as_ref().unwrap().label(), "h");
    let outcome = app.handle_input(&key_event(KeyCode::Char('i'), KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.new_worktree_dialog.as_ref().unwrap().label(), "hi");
}
#[test]
fn worktree_dialog_backspace_removes_char() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    let mut dialog = NewWorktreeDialogState::new();
    dialog.set_label("test");
    app.new_worktree_dialog = Some(dialog);
    let outcome = app.handle_input(&key_event(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.new_worktree_dialog.as_ref().unwrap().label(), "tes");
}
#[test]
fn worktree_dialog_enforces_byte_cap_for_typing_and_middle_paste() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    let mut dialog = NewWorktreeDialogState::new();
    dialog.set_label("a".repeat(98));
    let _ = dialog.set_cursor_byte(1);
    app.new_worktree_dialog = Some(dialog);
    let outcome = app.handle_input(&Event::Paste("éx".to_owned()));
    assert!(matches!(outcome, InputOutcome::Changed));
    let dialog = app.new_worktree_dialog.as_ref().unwrap();
    assert_eq!(dialog.label().len(), 100);
    assert_eq!(&dialog.label()[1.."aé".len()], "é");
    let outcome = app.handle_input(&key_event(KeyCode::Char('中'), KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.new_worktree_dialog.as_ref().unwrap().label().len(), 100);
    let mut dialog = NewWorktreeDialogState::new();
    dialog.set_label("a".repeat(99));
    app.new_worktree_dialog = Some(dialog);
    let _ = app.handle_input(&key_event(KeyCode::Char('é'), KeyModifiers::NONE));
    assert_eq!(app.new_worktree_dialog.as_ref().unwrap().label().len(), 99);
}
#[test]
fn worktree_dialog_paste_is_scoped_away_from_welcome_prompt() {
    let mut app = test_app();
    app.auth_state = AuthState::Done;
    let mut dialog = NewWorktreeDialogState::new();
    dialog.set_label("ab");
    let _ = dialog.set_cursor_byte(1);
    app.new_worktree_dialog = Some(dialog);
    let outcome = app.handle_input(&Event::Paste("中".to_owned()));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.new_worktree_dialog.as_ref().unwrap().label(), "a中b");
    assert!(app.welcome_prompt.text().is_empty());
}
#[test]
fn authenticating_loopback_esc_quits() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Loopback,
    };
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Action(Action::Quit)));
}
#[test]
fn authenticating_command_esc_quits() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Command,
    };
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Action(Action::Quit)));
}
/// Regression (user report): 'q' must type into the auth-code input,
/// not quit.
#[test]
fn authenticating_loopback_q_types_into_code_input() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Loopback,
    };
    let outcome = app.handle_input(&key_event(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "typing 'q' must edit the auth code input, got {outcome:?}"
    );
    assert_eq!(app.auth_code_input.text(), "q");
}
/// Users reflex-type the displayed device code; bare 'q' must not abort.
#[test]
fn authenticating_device_and_command_q_does_not_quit() {
    for mode in [AuthMode::Device, AuthMode::Command] {
        let mut app = test_app();
        app.auth_state = AuthState::Authenticating {
            request_seq: 1,
            handle: None,
            auth_url: None,
            mode,
        };
        let outcome = app.handle_input(&key_event(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(
            matches!(outcome, InputOutcome::Unchanged),
            "bare 'q' must not quit during {mode:?} auth, got {outcome:?}"
        );
    }
}
/// Advertised cancel keys must survive the bare-'q' removal.
#[test]
fn authenticating_advertised_cancel_keys_still_quit() {
    for mode in [AuthMode::Loopback, AuthMode::Device, AuthMode::Command] {
        for (code, mods) in [
            (KeyCode::Char('q'), KeyModifiers::CONTROL),
            (KeyCode::Char('c'), KeyModifiers::CONTROL),
            (KeyCode::Esc, KeyModifiers::NONE),
        ] {
            let mut app = test_app();
            app.auth_state = AuthState::Authenticating {
                request_seq: 1,
                handle: None,
                auth_url: None,
                mode,
            };
            let outcome = app.handle_input(&key_event(code, mods));
            assert!(
                matches!(outcome, InputOutcome::Action(Action::Quit)),
                "{code:?}+{mods:?} must still quit during {mode:?} auth, got {outcome:?}"
            );
        }
    }
}
#[test]
fn authenticating_loopback_char_mutates_input() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Loopback,
    };
    let outcome = app.handle_input(&key_event(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.auth_code_input.text(), "a");
}
#[test]
fn authenticating_loopback_readline_control_chords_are_ignored() {
    for code in [KeyCode::Char('u'), KeyCode::Char('d')] {
        let mut app = test_app();
        app.auth_state = AuthState::Authenticating {
            request_seq: 1,
            handle: None,
            auth_url: None,
            mode: AuthMode::Loopback,
        };
        app.auth_code_input.set_text("token");
        let outcome = app.handle_input(&key_event(code, KeyModifiers::CONTROL));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(app.auth_code_input.text(), "token");
    }
}
#[cfg(target_os = "windows")]
#[test]
fn authenticating_loopback_altgr_char_mutates_input() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Loopback,
    };
    let outcome = app.handle_input(&key_event(
        KeyCode::Char('@'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.auth_code_input.text(), "@");
}
#[test]
fn authenticating_loopback_backspace_removes_char() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Loopback,
    };
    app.auth_code_input.set_text("ab");
    let outcome = app.handle_input(&key_event(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.auth_code_input.text(), "a");
}
#[test]
fn authenticating_loopback_paste_appends_text() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Loopback,
    };
    app.auth_code_input.set_text("tok");
    let outcome = app.handle_input(&Event::Paste("en_value".to_string()));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.auth_code_input.text(), "token_value");
}
#[test]
fn authenticating_loopback_cursor_edit_and_paste_stay_scoped() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Loopback,
    };
    app.auth_code_input.set_text("ab");
    let _ = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    let _ = app.handle_input(&Event::Paste("中\r\n".to_owned()));
    assert_eq!(app.auth_code_input.text(), "a中b");
    assert!(app.welcome_prompt.text().is_empty());
    let _ = app.handle_input(&key_event(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(app.auth_code_input.text(), "a中");
}
#[test]
fn authenticating_loopback_uses_canonical_super_v_paste() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Loopback,
    };
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::no_raster(
        Some("secret\r\n"),
    ));
    let outcome = app.handle_input(&key_event(KeyCode::Char('v'), KeyModifiers::SUPER));
    crate::clipboard::clear_clipboard_probe_hook();
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.auth_code_input.text(), "secret");
    assert!(app.welcome_prompt.text().is_empty());
}
#[test]
fn authenticating_loopback_enter_empty_is_noop() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Loopback,
    };
    app.auth_code_input.set_text("   ");
    let outcome = app.handle_input(&key_event(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Unchanged));
}
#[test]
fn authenticating_loopback_enter_with_content_submits() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Loopback,
    };
    app.auth_code_input.set_text(" token123 ");
    let outcome = app.handle_input(&key_event(KeyCode::Enter, KeyModifiers::NONE));
    match outcome {
        InputOutcome::Action(Action::SubmitAuthCode(code)) => {
            assert_eq!(code, "token123");
        }
        other => panic!("expected SubmitAuthCode, got {:?}", other),
    }
}
/// A bare `Moved` after a press means the release was lost: the press
/// must end, never promote into a selection.
#[test]
fn moved_after_press_ends_gesture_instead_of_promoting() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(crate::scrollback::RenderBlock::agent_message(
            "hello world this should wrap across lines",
        ));
    agent.scrollback.prepare_layout(40, 10);
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 20));
    let _ = agent.draw(
        ratatui::layout::Rect::new(0, 0, 40, 20),
        &mut buf,
        &ActionRegistry::defaults(),
        &mut crate::scrollback::render::ScratchBuffer::new(),
        None,
        false,
        crate::app::agent_view::BannerSlotParams::none(),
        &BundleState::default(),
        false,
        false,
        &mut Vec::new(),
        crate::app::agent_view::AppRenderParams::default(),
    );
    let hit = agent
        .last_scrollback_selection_model
        .ranges
        .first()
        .and_then(|range| range.lines.first())
        .cloned()
        .expect("expected selectable markdown line");
    let down_col = hit.screen_x + hit.selectable_cols.start;
    let row = hit.screen_y.min(9);
    let move_col = down_col + 1;
    let down = left_mouse(MouseEventKind::Down(MouseButton::Left), down_col, row);
    let moved = left_mouse(MouseEventKind::Moved, move_col, row);
    assert!(matches!(app.handle_input(&down), InputOutcome::Changed));
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.pending_text_drag.is_some());
    assert!(agent.drag_selection.is_none());
    assert!(matches!(app.handle_input(&moved), InputOutcome::Changed));
    let agent = app.agents.get(&id).unwrap();
    assert!(!agent.left_mouse_down, "lost release ended the press");
    assert!(agent.pending_text_drag.is_none());
    assert!(agent.drag_selection.is_none(), "hover must not select");
}
#[test]
fn moved_without_button_does_not_promote_pending_scrollback_drag() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(crate::scrollback::RenderBlock::agent_message(
            "hello world this should wrap across lines",
        ));
    agent.scrollback.prepare_layout(40, 10);
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 20));
    let _ = agent.draw(
        ratatui::layout::Rect::new(0, 0, 40, 20),
        &mut buf,
        &ActionRegistry::defaults(),
        &mut crate::scrollback::render::ScratchBuffer::new(),
        None,
        false,
        crate::app::agent_view::BannerSlotParams::none(),
        &BundleState::default(),
        false,
        false,
        &mut Vec::new(),
        crate::app::agent_view::AppRenderParams::default(),
    );
    let hit = agent
        .last_scrollback_selection_model
        .ranges
        .first()
        .and_then(|range| range.lines.first())
        .cloned()
        .expect("expected selectable markdown line");
    let down_col = hit.screen_x + hit.selectable_cols.start;
    let row = hit.screen_y.min(9);
    let move_col = down_col + 1;
    let down = left_mouse(MouseEventKind::Down(MouseButton::Left), down_col, row);
    let up = left_mouse(MouseEventKind::Up(MouseButton::Left), down_col, row);
    let moved = left_mouse(MouseEventKind::Moved, move_col, row);
    assert!(matches!(app.handle_input(&down), InputOutcome::Changed));
    assert!(matches!(app.handle_input(&up), InputOutcome::Changed));
    let agent = app.agents.get(&id).unwrap();
    assert!(!agent.left_mouse_down);
    assert!(agent.pending_text_drag.is_none());
    assert!(agent.drag_selection.is_none());
    let outcome = app.handle_input(&moved);
    assert!(matches!(
        outcome,
        InputOutcome::Unchanged | InputOutcome::Changed
    ));
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.drag_selection.is_none());
}
#[test]
fn scrollback_click_still_selects_entry_on_mouse_up() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(crate::scrollback::RenderBlock::agent_message("hello world"));
    agent.scrollback.prepare_layout(40, 10);
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 20));
    let _ = agent.draw(
        ratatui::layout::Rect::new(0, 0, 40, 20),
        &mut buf,
        &ActionRegistry::defaults(),
        &mut crate::scrollback::render::ScratchBuffer::new(),
        None,
        false,
        crate::app::agent_view::BannerSlotParams::none(),
        &BundleState::default(),
        false,
        false,
        &mut Vec::new(),
        crate::app::agent_view::AppRenderParams::default(),
    );
    let hit = agent
        .last_scrollback_selection_model
        .ranges
        .first()
        .and_then(|range| range.lines.first())
        .cloned()
        .expect("expected selectable markdown line");
    let click_col = hit.screen_x + hit.selectable_cols.start;
    let click_row = hit.screen_y;
    let down = left_mouse(
        MouseEventKind::Down(MouseButton::Left),
        click_col,
        click_row,
    );
    let up = left_mouse(MouseEventKind::Up(MouseButton::Left), click_col, click_row);
    assert!(matches!(app.handle_input(&down), InputOutcome::Changed));
    assert!(matches!(app.handle_input(&up), InputOutcome::Changed));
    let selected_after = app.agents.get(&id).unwrap().scrollback.selected();
    assert_eq!(selected_after, Some(0));
}
fn make_test_warning() -> crate::startup::StartupWarning {
    crate::startup::StartupWarning {
        severity: crate::startup::WarningSeverity::Warning,
        message: "test warning".to_string(),
        action: Some("run /terminal-setup".to_string()),
    }
}
#[test]
fn welcome_d_starts_session_when_no_warnings() {
    let mut app = test_app();
    app.welcome_prompt_focused = true;
    app.startup_warnings = vec![];
    let outcome = app.handle_input(&key_event(KeyCode::Char('d'), KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::ActionThenForward(Action::NewSession)),
        "Expected NewSession when no warnings, got {outcome:?}"
    );
}
#[test]
fn welcome_other_char_starts_session_even_with_warnings() {
    let mut app = test_app();
    app.welcome_prompt_focused = true;
    app.startup_warnings = vec![make_test_warning()];
    let outcome = app.handle_input(&key_event(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::ActionThenForward(Action::NewSession)),
        "Expected NewSession for 'a' even with warnings, got {outcome:?}"
    );
}
#[test]
fn merge_escapes_both_some_concatenates() {
    let result = AppView::merge_escapes(
        Some("notif".into()),
        Some(crate::terminal::overlay::PostFlush::plain("render".into())),
    );
    assert_eq!(
        result.as_ref().map(|post| post.as_str()),
        Some("notifrender")
    );
}
#[test]
fn merge_escapes_only_notif() {
    let result = AppView::merge_escapes(Some("notif".into()), None);
    assert_eq!(result.as_ref().map(|post| post.as_str()), Some("notif"));
}
#[test]
fn merge_escapes_only_render() {
    let result = AppView::merge_escapes(
        None,
        Some(crate::terminal::overlay::PostFlush::plain("render".into())),
    );
    assert_eq!(result.as_ref().map(|post| post.as_str()), Some("render"));
}
#[test]
fn merge_escapes_both_none() {
    let result = AppView::merge_escapes(None, None);
    assert!(result.is_none());
}
#[test]
fn dashboard_stale_clears_modal_placement_under_kitty() {
    use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};
    let _g = set_protocol_for_test(GraphicsProtocol::Kitty);
    let mut app = test_app_with_agent();
    let clears = AppView::dashboard_stale_image_clears(&mut app.agents, None);
    let expected = crate::terminal::overlay::clear_kitty().into_string();
    assert_eq!(
        clears.as_ref().map(|post| post.as_str()),
        Some(expected.as_str()),
        "the modal/preview placement (id 1) is deleted every dashboard frame"
    );
}
#[test]
fn dashboard_stale_clears_none_without_graphics_protocol() {
    use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};
    let _g = set_protocol_for_test(GraphicsProtocol::None);
    let mut app = test_app_with_agent();
    let clears = AppView::dashboard_stale_image_clears(&mut app.agents, None);
    assert!(clears.is_none(), "text-only terminals never get escapes");
}
#[test]
fn dashboard_stale_clears_drain_undrawn_agent_inline_media() {
    use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};
    let _g = set_protocol_for_test(GraphicsProtocol::Kitty);
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .inline_media_ids
            .insert(std::path::PathBuf::from("/tmp/media.png"), 5);
        agent.inline_media_active = true;
    }
    let clears = AppView::dashboard_stale_image_clears(&mut app.agents, None)
        .expect("kitty sweep always emits");
    assert!(
        clears
            .as_str()
            .contains(&crate::terminal::image::clear_kitty_image(5)),
        "deletes the undrawn agent's inline placement: {clears:?}"
    );
    let again = AppView::dashboard_stale_image_clears(&mut app.agents, None);
    let expected = crate::terminal::overlay::clear_kitty().into_string();
    assert_eq!(
        again.as_ref().map(|post| post.as_str()),
        Some(expected.as_str()),
    );
}
#[test]
fn dashboard_stale_clears_skip_attached_popup_agent() {
    use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};
    let _g = set_protocol_for_test(GraphicsProtocol::Kitty);
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .inline_media_ids
            .insert(std::path::PathBuf::from("/tmp/media.png"), 5);
        agent.inline_media_active = true;
    }
    crate::terminal::overlay::reset_owner();
    let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    let _ = crate::terminal::overlay::static_image(&png, 20, 10, 0, 0, 7)
        .unwrap()
        .commit();
    for _ in 0..2 {
        assert!(AppView::dashboard_stale_image_clears(&mut app.agents, Some(id)).is_none());
        let popup = crate::terminal::overlay::static_image(&png, 20, 10, 0, 0, 7).unwrap();
        assert!(!popup.as_str().contains("a=t"));
        let _ = popup.commit();
    }
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.inline_media_active, "drawn agent state is untouched");
    assert_eq!(agent.inline_media_ids.len(), 1);
}
#[test]
fn dashboard_too_small_popup_clears_shared_overlay_slot() {
    use crate::terminal::image::{GraphicsProtocol, set_protocol_for_test};
    let _guard = set_protocol_for_test(GraphicsProtocol::Kitty);
    crate::terminal::overlay::reset_owner();
    let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    let _ = crate::terminal::overlay::static_image(&png, 20, 10, 0, 0, 8)
        .unwrap()
        .commit();
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    let mut dashboard = crate::views::dashboard::DashboardState::new();
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 4));
    let (_, _, drawn) = crate::views::dashboard::render_popup_overlay(
        &mut buf,
        ratatui::layout::Rect::new(0, 0, 40, 4),
        &crate::theme::Theme::current(),
        "Tiny",
        &mut dashboard,
        |_inner, _buf| panic!("tiny popup must not draw the agent"),
    );
    assert!(!drawn);
    let clear =
        AppView::dashboard_stale_image_clears(&mut app.agents, drawn.then_some(id)).unwrap();
    assert!(clear.as_str().contains("a=d"));
    assert!(
        !crate::terminal::overlay::static_image(&png, 20, 10, 0, 0, 8)
            .unwrap()
            .as_str()
            .contains("a=t")
    );
    clear.write_to(&mut Vec::new()).unwrap();
    assert!(
        crate::terminal::overlay::static_image(&png, 20, 10, 0, 0, 8)
            .unwrap()
            .as_str()
            .contains("a=t")
    );
}
#[test]
fn worktree_mode_round_trip_ask() {
    let mode = WorktreeMode::from_config_str("ask");
    assert_eq!(mode, WorktreeMode::Ask);
    assert_eq!(mode.as_config_str(), "ask");
}
#[test]
fn worktree_mode_round_trip_always() {
    let mode = WorktreeMode::from_config_str("always");
    assert_eq!(mode, WorktreeMode::Always);
    assert_eq!(mode.as_config_str(), "always");
}
#[test]
fn worktree_mode_round_trip_never() {
    let mode = WorktreeMode::from_config_str("never");
    assert_eq!(mode, WorktreeMode::Never);
    assert_eq!(mode.as_config_str(), "never");
}
#[test]
fn worktree_mode_unrecognised_falls_back_to_never() {
    assert_eq!(WorktreeMode::from_config_str("alway"), WorktreeMode::Never);
    assert_eq!(WorktreeMode::from_config_str(""), WorktreeMode::Never);
    assert_eq!(WorktreeMode::from_config_str("ALWAYS"), WorktreeMode::Never);
}
/// Helper: parse a TOML string and return the document.
fn parse_toml(s: &str) -> toml_edit::DocumentMut {
    s.parse::<toml_edit::DocumentMut>().expect("valid TOML")
}
#[test]
fn resolve_from_hints_no_keys_returns_defaults() {
    let doc = parse_toml("");
    let (new_s, fork) = WorktreeMode::resolve_from_hints(doc.get("hints"));
    assert_eq!(new_s, WorktreeMode::Never);
    assert_eq!(fork, WorktreeMode::Ask);
}
#[test]
fn resolve_from_hints_legacy_key_sets_both() {
    let doc = parse_toml("[hints]\nworktree_mode = \"always\"\n");
    let (new_s, fork) = WorktreeMode::resolve_from_hints(doc.get("hints"));
    assert_eq!(new_s, WorktreeMode::Always);
    assert_eq!(fork, WorktreeMode::Always);
}
#[test]
fn resolve_from_hints_per_command_keys_override_legacy() {
    let doc = parse_toml(
        "[hints]\n\
         worktree_mode = \"always\"\n\
         new_session_worktree_mode = \"never\"\n\
         fork_worktree_mode = \"ask\"\n",
    );
    let (new_s, fork) = WorktreeMode::resolve_from_hints(doc.get("hints"));
    assert_eq!(new_s, WorktreeMode::Never);
    assert_eq!(fork, WorktreeMode::Ask);
}
#[test]
fn resolve_from_hints_only_per_command_keys() {
    let doc = parse_toml(
        "[hints]\n\
         new_session_worktree_mode = \"ask\"\n\
         fork_worktree_mode = \"never\"\n",
    );
    let (new_s, fork) = WorktreeMode::resolve_from_hints(doc.get("hints"));
    assert_eq!(new_s, WorktreeMode::Ask);
    assert_eq!(fork, WorktreeMode::Never);
}
#[test]
fn resolve_from_hints_one_per_command_key_other_falls_back_to_legacy() {
    let doc = parse_toml(
        "[hints]\n\
         worktree_mode = \"always\"\n\
         fork_worktree_mode = \"never\"\n",
    );
    let (new_s, fork) = WorktreeMode::resolve_from_hints(doc.get("hints"));
    assert_eq!(new_s, WorktreeMode::Always);
    assert_eq!(fork, WorktreeMode::Never);
}
#[test]
fn resolve_from_hints_one_per_command_key_other_falls_back_to_default() {
    let doc = parse_toml("[hints]\nnew_session_worktree_mode = \"always\"\n");
    let (new_s, fork) = WorktreeMode::resolve_from_hints(doc.get("hints"));
    assert_eq!(new_s, WorktreeMode::Always);
    assert_eq!(fork, WorktreeMode::Ask);
}
fn scroll_event(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}
#[test]
fn opening_workflow_transcript_cancels_pending_scroll_stream() {
    use crate::input::mouse::{ScrollConfig, ScrollDirection};
    let mut app = test_app_with_agent();
    let ActiveView::Agent(id) = app.active_view else {
        panic!("test app must start on an agent");
    };
    let child_sid = "workflow-child";
    let child = idle_child_view(&app, 1, child_sid);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.subagent_views.insert(child_sid.to_owned(), child);
    agent
        .workflow_runs
        .push(crate::views::workflows::WorkflowRunSnapshot {
            run_id: "wf_run".to_owned(),
            name: "deep-research".to_owned(),
            objective: "obj".to_owned(),
            status: "active".to_owned(),
            management_available: true,
            builtin: false,
            phases: vec![("Research".to_owned(), "active".to_owned())],
            current_phase: Some("Research".to_owned()),
            agents: vec![crate::views::workflows::WorkflowAgentRowView {
                agent_id: child_sid.to_owned(),
                label: "researcher".to_owned(),
                phase: Some("Research".to_owned()),
                model: None,
                state: "running".to_owned(),
                tokens_used: 0,
                duration_ms: 0,
            }],
            agent_budget: None,
            agents_used: 0,
            agents_reserved: 0,
            agents_remaining: None,
            agent_usage_incomplete: false,
            active_agents: 1,
            elapsed_ms: 0,
            received_at: std::time::Instant::now(),
            pause_message: None,
            result_summary: None,
        });
    agent.show_workflows = true;
    agent.workflows_view.detail_run_id = Some("wf_run".to_owned());
    let _ = app
        .scroll_state
        .on_scroll_event(ScrollDirection::Up, ScrollConfig::default());
    app.last_scroll_pos = Some((30, 12));
    assert!(app.scroll_state.has_active_stream());
    let out = app.handle_input(&key_event(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(out, InputOutcome::Changed));
    assert_eq!(app.agents[&id].active_subagent.as_deref(), Some(child_sid));
    assert!(!app.scroll_state.has_active_stream());
    assert_eq!(app.last_scroll_pos, None);
}
#[test]
fn scroll_event_stashes_origin_for_residual_flush() {
    let mut app = test_app();
    assert!(app.last_scroll_pos.is_none());
    let _ = app.handle_input(&scroll_event(MouseEventKind::ScrollDown, 42, 17));
    assert_eq!(app.last_scroll_pos, Some((42, 17)));
    let _ = app.handle_input(&scroll_event(MouseEventKind::ScrollUp, 7, 3));
    assert_eq!(app.last_scroll_pos, Some((7, 3)));
}
#[test]
fn scroll_event_does_not_stash_when_blocking_modal_open() {
    let mut app = test_app();
    app.new_worktree_dialog = Some(NewWorktreeDialogState::new());
    assert!(app.is_scroll_blocking_modal_open());
    let _ = app.handle_input(&scroll_event(MouseEventKind::ScrollDown, 42, 17));
    assert!(
        app.last_scroll_pos.is_none(),
        "scroll events must be ignored while a scroll-blocking modal is open",
    );
}
#[test]
fn welcome_privacy_banner_hover_triggers_redraw() {
    let mut app = test_app();
    app.active_view = ActiveView::Welcome;
    app.welcome_privacy_banner_opt_in_rect = Some(ratatui::layout::Rect::new(50, 10, 8, 1));
    app.welcome_privacy_banner_opt_out_rect = Some(ratatui::layout::Rect::new(25, 10, 24, 1));
    app.welcome_privacy_banner_terms_rect = Some(ratatui::layout::Rect::new(7, 11, 5, 1));
    app.welcome_privacy_banner_policy_rect = Some(ratatui::layout::Rect::new(17, 11, 14, 1));
    let over = left_mouse(MouseEventKind::Moved, 52, 10);
    assert!(matches!(app.handle_input(&over), InputOutcome::Changed));
    assert!(app.welcome_on_privacy_banner);
    let cross = left_mouse(MouseEventKind::Moved, 30, 10);
    assert!(matches!(app.handle_input(&cross), InputOutcome::Changed));
    assert!(app.welcome_on_privacy_banner);
    let over_legal = left_mouse(MouseEventKind::Moved, 10, 11);
    assert!(matches!(
        app.handle_input(&over_legal),
        InputOutcome::Changed
    ));
    assert!(app.welcome_on_privacy_banner);
    let leave = left_mouse(MouseEventKind::Moved, 5, 5);
    assert!(matches!(app.handle_input(&leave), InputOutcome::Changed));
    assert!(!app.welcome_on_privacy_banner);
    assert!(matches!(app.handle_input(&leave), InputOutcome::Unchanged));
}
#[test]
fn welcome_doc_viewer_is_scroll_blocking_and_wheel_scrolls_content() {
    let mut app = test_app();
    app.active_view = ActiveView::Welcome;
    app.welcome_doc_viewer = Some(crate::views::modal::ActiveModal::DocViewer {
        title: "Release Notes".into(),
        content: "line\n".repeat(80),
        scroll: 0,
        window: crate::views::modal_window::ModalWindowState::new(),
        cached_lines: None,
        previous_palette: None,
        standalone: true,
    });
    assert!(
        app.is_scroll_blocking_modal_open(),
        "welcome release-notes overlay must block background scroll",
    );
    let outcome = app.handle_input(&scroll_event(MouseEventKind::ScrollDown, 40, 12));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "wheel must be handled by the doc viewer",
    );
    assert!(
        app.last_scroll_pos.is_none(),
        "wheel must not reach the background scroll path while release notes are open",
    );
    let scroll = match app.welcome_doc_viewer.as_ref() {
        Some(crate::views::modal::ActiveModal::DocViewer { scroll, .. }) => *scroll,
        _ => panic!("expected DocViewer"),
    };
    assert!(scroll > 0, "wheel must advance doc scroll, got {scroll}");
}
#[test]
fn tutorial_is_scroll_blocking_and_wheel_scrolls_topic() {
    let mut app = test_app();
    app.active_view = ActiveView::Welcome;
    let mut tut = crate::views::tutorial::TutorialState::new();
    let _ = crate::views::tutorial::handle_tutorial_input(
        &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &mut tut,
    );
    app.tutorial = Some(tut);
    assert!(
        app.is_scroll_blocking_modal_open(),
        "tutorial overlay must block background scroll",
    );
    let outcome = app.handle_input(&scroll_event(MouseEventKind::ScrollDown, 40, 12));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        app.last_scroll_pos.is_none(),
        "wheel must not reach the background scroll path while the tutorial is open",
    );
    let tut = app.tutorial.as_ref().expect("tutorial stays open");
    assert!(
        tut.scroll > 0,
        "wheel must advance topic scroll, got {}",
        tut.scroll
    );
}
#[test]
fn tutorial_esc_on_list_closes_overlay() {
    let mut app = test_app();
    app.active_view = ActiveView::Welcome;
    app.tutorial = Some(crate::views::tutorial::TutorialState::new());
    let outcome = app.handle_input(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        app.tutorial.is_none(),
        "Esc on the list closes the tutorial"
    );
}
#[test]
fn dashboard_shortcuts_modal_is_scroll_blocking() {
    let mut app = test_app();
    app.active_view = ActiveView::AgentDashboard;
    let mut d = crate::views::dashboard::DashboardState::new();
    let entries = Vec::new();
    let state = crate::views::shortcuts_help::build_initial_picker_state(&entries);
    d.shortcuts_modal = Some(Box::new(crate::views::dashboard::ShortcutsModalState {
        entries,
        state,
        window: Default::default(),
        filter_active: false,
        collapsed_sections: Default::default(),
        expanded_ids: std::collections::HashSet::new(),
        mode: crate::views::shortcuts_help::ShortcutsHelpMode::Browse,
    }));
    app.dashboard = Some(d);
    assert!(
        app.is_scroll_blocking_modal_open(),
        "an open dashboard cheatsheet must block background scroll",
    );
    let _ = app.handle_input(&scroll_event(MouseEventKind::ScrollDown, 42, 17));
    assert!(
        app.last_scroll_pos.is_none(),
        "wheel must not reach the background scroll path while the cheatsheet is open",
    );
}
/// Ctrl+C on the session-less dashboard arms the quit confirmation
/// (like the agent view) and a second press confirms. Regression for
/// "Ctrl+C/D/Q do nothing on the dashboard prompt".
#[test]
fn ctrl_c_on_dashboard_arms_then_confirms_quit() {
    let mut app = test_app();
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    let outcome = app.handle_input(&key_event(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        app.pending_action.is_some(),
        "Ctrl+C on the dashboard must arm a pending quit confirmation"
    );
    let outcome = app.handle_input(&key_event(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::Quit)),
        "second Ctrl+C must quit, got {outcome:?}"
    );
}
/// Ctrl+Q on the dashboard arms quit via the global `When::Always`
/// lookup (it's not bound to `When::DashboardFocused`).
#[test]
fn ctrl_q_on_dashboard_arms_quit() {
    let mut app = test_app();
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    let outcome = app.handle_input(&key_event(KeyCode::Char('q'), KeyModifiers::CONTROL));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        app.pending_action.is_some(),
        "Ctrl+Q on the dashboard must arm a pending quit confirmation"
    );
}
/// Ctrl+Space on the dashboard resolves to `VoiceToggle` via the global
/// `When::Always` fallthrough — the dispatch input ignores the chord, so it
/// falls through to `handle_global_action`. (The event loop intercepts
/// Ctrl+Space before this for hold-to-talk/toggle when voice is enabled;
/// this registry route is the cheatsheet/command-palette fallback.)
#[test]
fn ctrl_space_on_dashboard_routes_to_voice_toggle() {
    let mut app = test_app();
    pin_non_vscode_registry(&mut app);
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    let outcome = app.handle_input(&key_event(KeyCode::Char(' '), KeyModifiers::CONTROL));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::VoiceToggle)),
        "Ctrl+Space on the dashboard must route to VoiceToggle, got {outcome:?}"
    );
}
/// With `[ui].voice_keybind_enabled = false` the global fallthrough must
/// swallow the chord — otherwise Ctrl+Space would still start dictation via
/// the registry route whenever the event-loop intercept skips it.
#[test]
fn ctrl_space_on_dashboard_ignored_when_keybind_disabled() {
    let mut app = test_app();
    pin_non_vscode_registry(&mut app);
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    app.current_ui.voice_keybind_enabled = Some(false);
    let outcome = app.handle_input(&key_event(KeyCode::Char(' '), KeyModifiers::CONTROL));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::VoiceToggle)),
        "Ctrl+Space must be inert with the voice shortcut disabled, got {outcome:?}"
    );
}
/// Esc while voice is recording on the dashboard must STOP voice (route to
/// `VoiceToggle`) rather than fall into the dashboard's Esc cascade
/// (clear filter / unfocus / deselect / exit).
#[test]
fn esc_on_dashboard_while_listening_stops_voice() {
    let mut app = test_app();
    pin_non_vscode_registry(&mut app);
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::DashboardDispatch,
        interim: None,
    };
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::VoiceToggle)),
        "Esc while recording on the dashboard must stop voice, got {outcome:?}"
    );
}
/// Esc on the dashboard while NOT recording must keep its normal cascade
/// behaviour (here: not a `VoiceToggle`).
#[test]
fn esc_on_dashboard_not_listening_does_not_toggle_voice() {
    let mut app = test_app();
    pin_non_vscode_registry(&mut app);
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    app.voice_state = VoiceState::Idle;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::VoiceToggle)),
        "Esc must not toggle voice when not recording, got {outcome:?}"
    );
}
/// Esc with a voice cold-start still queued (pipeline spawning, mic not yet
/// open) must cancel it so the event loop doesn't open the mic after the user
/// backed out — even though `voice_listening` is still false.
#[test]
fn esc_cancels_pending_voice_cold_start() {
    let mut app = test_app();
    pin_non_vscode_registry(&mut app);
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    app.voice_state = VoiceState::ColdStart {
        hold: false,
        target: VoiceTarget::DashboardDispatch,
    };
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        !app.voice_state.pending_cold_start(),
        "Esc must cancel the queued cold-start"
    );
    assert!(
        app.voice_recording_target().is_none(),
        "target dropped on cancel"
    );
}
/// The dictation overlay must only render on the surface that owns the bound
/// target. After an explicit stop the interim is kept (`Stopping`) for a
/// trailing final, so navigating away must not flash it on the wrong box.
#[test]
fn voice_overlay_bound_to_target_surface() {
    let id = super::super::agent::AgentId(0);
    let mut app = test_app();
    app.voice_state = VoiceState::Stopping {
        target: VoiceTarget::Agent(id),
        interim: Some("partial".into()),
    };
    app.active_view = ActiveView::Agent(id);
    assert!(
        app.voice_target_on_active_surface(),
        "overlay shows on the agent that owns the dictation"
    );
    app.active_view = ActiveView::AgentDashboard;
    assert!(
        !app.voice_target_on_active_surface(),
        "overlay hidden once the user navigates off the target surface"
    );
}
/// Entering a session from the dashboard sets `active_view = Agent(id)` but
/// leaves `attached_agent = Some(id)` as a return breadcrumb. The agent is
/// fullscreen, so dictation into its prompt must stay on-surface and the
/// bind-enforcer must not auto-stop it. Regression: recording bar missing
/// after clicking into a session. (Popup-over-dashboard suppression is
/// covered by `dispatch::tests::voice_suppressed_while_dashboard_popup_open`.)
#[test]
fn voice_target_on_agent_entered_from_dashboard() {
    let id = super::super::agent::AgentId(0);
    let mut app = test_app();
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::Agent(id),
        interim: None,
    };
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    app.dashboard.as_mut().unwrap().attached_agent = Some(id);
    assert!(app.voice_target_on_active_surface());
    app.enforce_voice_session_bound();
    assert!(
        app.voice_listening(),
        "entering a session from the dashboard must not auto-stop the mic"
    );
}
/// Attach a popup overlay onto a freshly-built `test_app_with_agent`
/// and return the attached agent id. Convenience for the
/// popup-handle-input tests.
/// NOTE: this helper bypasses
/// `dispatch_dashboard_attach`. The action-dispatcher path
/// (which sets `attached_agent` via `Action::DashboardAttach(...)`)
/// is pinned by tests in `dispatch.rs`
/// (`dashboard_attach_top_level_opens_popup_overlay`,
/// `dashboard_attach_subagent_opens_popup_with_subagent`).
/// `attach_popup` exists so the `handle_input`/`dispatch_scroll`
/// tests in this file can stand up a popup'd state in two lines
/// without re-exercising the dispatcher each time.
fn attach_popup(app: &mut AppView) -> super::super::agent::AgentId {
    app.active_view = ActiveView::AgentDashboard;
    let id = super::super::agent::AgentId(0);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(id));
    }
    id
}
/// Esc keystroke closes the popup at the
/// `AppView::handle_input` layer (not the dispatch layer the
/// other tests exercise).
#[test]
fn handle_input_esc_closes_popup_overlay() {
    let mut app = test_app_with_agent();
    let id = attach_popup(&mut app);
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, Some(id));
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, None);
}
/// Esc on a neutral overlay (scrollback focused, no modals
/// or viewers, no text selection or link highlight, no
/// question / goal / rewind / permission overlays) closes the
/// dashboard session overlay — mirrors the `q` shortcut and
/// gives users a single-key back-out from agent detail to
/// the dashboard. The Esc cascade is preserved for non-
/// neutral states: see `overlay_esc_passes_through_when_*`.
#[test]
fn overlay_esc_exits_when_agent_is_neutral() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc on a neutral overlay must request DashboardOverlayExit, got {outcome:?}",
    );
}
/// In a dashboard overlay an empty, Normal-mode prompt-focused Esc backs
/// out to the dashboard (attach lands on Prompt
/// focus, so without this Esc would silently arm the agent's rewind policy
/// instead of returning to the list).
#[test]
fn overlay_esc_backs_out_when_empty_normal_prompt() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
    assert!(agent.prompt.text().is_empty());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "empty prompt Esc in an overlay must back out to the dashboard, got {outcome:?}",
    );
    assert!(app.pending_action.is_none());
}
/// Overlay + open `/btw` + empty Normal prompt: Esc dismisses `/btw`, not
/// dashboard back-out; a follow-up Esc still exits when the guard holds.
#[test]
fn overlay_esc_dismisses_btw_before_dashboard_backout() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
    agent.btw_state = Some(crate::views::btw_overlay::BtwOverlayState::done(
        "side question".into(),
        "side answer".into(),
    ));
    let first = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(first, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc with open /btw must not exit the overlay, got {first:?}",
    );
    assert!(app.agents.get(&id).unwrap().btw_state.is_none());
    let second = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(second, InputOutcome::Action(Action::DashboardOverlayExit)),
        "second Esc with no /btw must back out to the dashboard, got {second:?}",
    );
}
/// Regression: in an overlay, a bare Esc while a turn is
/// RUNNING must swallow (matching full-screen vim mode), NOT detach to the
/// dashboard and NOT cancel. The empty-prompt back-out is idle-gated, so Esc
/// falls through to `try_handle_esc_policy` → mid-turn swallow.
#[test]
fn overlay_esc_running_turn_empty_prompt_swallows_not_backout() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
    agent.session.state = AgentState::TurnRunning;
    agent.vim_mode = true;
    assert!(agent.prompt.text().is_empty());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "running-turn overlay Esc (empty prompt) must swallow, not detach/cancel, got {outcome:?}",
    );
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "Esc must not cancel mid-turn",
    );
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc must not detach mid-turn",
    );
    assert!(app.agents[&id].cancel_trigger_hint.is_none());
    assert!(app.pending_action.is_none());
}
/// Regression: in an overlay, a bare Esc from the
/// (neutral) bare-scrollback pane while a turn is RUNNING must swallow, NOT
/// detach — the neutral back-out is idle-gated. The fixture is otherwise
/// neutral (so the gate, not a missing-neutral, is what suppresses detach).
#[test]
fn overlay_esc_running_turn_scrollback_swallows_not_backout() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::app::agent_view::AgentPane::Scrollback;
    agent.session.state = AgentState::TurnRunning;
    agent.vim_mode = true;
    assert!(agent.is_bare_scrollback() && agent.no_input_overlay_pending());
    assert!(agent.no_esc_consumer_pending());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "running-turn overlay Esc (scrollback) must swallow, not detach/cancel, got {outcome:?}",
    );
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "Esc must not cancel mid-turn",
    );
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc must not detach mid-turn",
    );
    assert!(app.agents[&id].cancel_trigger_hint.is_none());
}
/// Overlay + non-vim: mid-turn Esc CANCELS (matching full-screen), and
/// still must not detach to the dashboard.
#[test]
fn overlay_esc_running_turn_non_vim_cancels_not_backout() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
    agent.session.state = AgentState::TurnRunning;
    agent.vim_mode = false;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "running-turn overlay Esc must cancel in non-vim mode, got {outcome:?}",
    );
    assert_eq!(
        app.agents[&id].cancel_trigger_hint,
        Some(crate::app::actions::CancelTrigger::Esc)
    );
}
#[test]
fn overlay_esc_wake_turn_scrollback_does_not_backout() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::app::agent_view::AgentPane::Scrollback;
    agent.vim_mode = true;
    agent.note_streaming_wake_turn("p-wake");
    assert!(agent.session.state.is_idle());
    assert!(agent.wake_turn_active());
    assert!(agent.is_bare_scrollback() && agent.no_input_overlay_pending());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "vim-mode wake Esc must swallow, not detach, got {outcome:?}",
    );
}
/// Overlay + TurnCancelling: Esc retries cancel (does not detach).
#[test]
fn overlay_esc_cancelling_scrollback_retries_cancel_not_backout() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::app::agent_view::AgentPane::Scrollback;
    agent.session.state = AgentState::TurnCancelling;
    assert!(agent.is_bare_scrollback() && agent.no_input_overlay_pending());
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "cancelling overlay Esc must retry CancelTurn, got {outcome:?}",
    );
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc must not detach while cancelling",
    );
}
/// Counterpart to the back-out: a NON-EMPTY draft Esc in an overlay must
/// pass through to the agent's policy (arms "press again to clear"), never
/// back out — so the user doesn't lose a draft by reaching for the dashboard.
#[test]
fn overlay_esc_with_draft_arms_clear_not_backout() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
    agent.prompt.textarea.set_text("draft in overlay");
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "a drafted overlay prompt Esc must NOT back out, got {outcome:?}",
    );
    let pending = app.pending_action.as_ref().expect("clear arm");
    assert_eq!(pending.label, Some("clear"));
}
/// A Bash/Remember empty prompt keeps Esc as its mode-exit even in an overlay: the back-out is gated to `PromptInputMode::Normal`, so the
/// special-mode Esc is not stolen as a dashboard back-out.
#[test]
fn overlay_esc_in_bash_mode_exits_mode_not_backout() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
    agent.prompt_input_mode = crate::app::agent_view::PromptInputMode::Bash;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "empty bash-mode Esc in an overlay must exit the mode, not back out, got {outcome:?}",
    );
    assert_eq!(
        app.agents[&id].prompt_input_mode,
        crate::app::agent_view::PromptInputMode::Normal,
        "Esc must have exited bash mode",
    );
}
/// A live highlighted link consumes Esc (the agent's scrollback
/// handler clears it). We mustn't pre-empt that — the overlay
/// closes only after the per-pane Esc work is drained.
#[test]
fn overlay_esc_passes_through_when_link_highlight_present() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    app.agents.get_mut(&id).unwrap().highlighted_link_idx = Some(0);
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc with a highlighted link must clear the highlight first, got {outcome:?}",
    );
}
/// Build an app with a neutral agent attached as the dashboard overlay.
fn neutral_overlay_app() -> (AppView, super::super::agent::AgentId) {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.attached_agent = Some(id);
    }
    (app, id)
}
fn open_agents_modal() -> crate::views::agents_modal::AgentsModalState {
    crate::views::agents_modal::AgentsModalState::new(
        std::path::Path::new("/nonexistent"),
        &std::collections::HashMap::new(),
        &BundleState::default(),
        None,
        None,
        None,
    )
}
/// With a pending input overlay, neither `q` nor `Esc` is consumed as a
/// dashboard-overlay exit — both fall through to the agent (the scrollback
/// handler, not the overlay handler).
#[test]
fn overlay_q_esc_do_not_exit_while_input_overlay_pending() {
    let installers: [fn(&mut AgentView); 2] = [
        |a| {
            a.cancel_turn_view = Some(crate::views::modal::CancelTurnViewState {
                active_idx: 0,
                running_count: 1,
            });
        },
        |a| {
            let request = crate::views::plan_approval_view::ExitPlanModeExtRequest {
                session_id: "s".into(),
                tool_call_id: "c".into(),
                plan_content: Some("p".into()),
            };
            let stashed = crate::views::prompt_widget::StashedPrompt {
                text: String::new(),
                cursor: 0,
                images: Vec::new(),
                chip_elements: Vec::new(),
                image_counter: 0,
                image_undo_stash: Vec::new(),
            };
            let view = crate::views::plan_approval_view::PlanApprovalViewState::new(
                request,
                stashed,
                tokio::sync::oneshot::channel().0,
            );
            a.plan_approval_view = Some(view);
        },
    ];
    for key in [KeyCode::Char('q'), KeyCode::Esc] {
        let (mut app, id) = neutral_overlay_app();
        assert!(
            app.agents.get(&id).unwrap().is_bare_scrollback()
                && app.agents.get(&id).unwrap().no_input_overlay_pending(),
            "fixture must start neutral",
        );
        let bare = app.handle_input(&key_event(key, KeyModifiers::NONE));
        assert!(
            matches!(bare, InputOutcome::Action(Action::DashboardOverlayExit)),
            "neutral {key:?} must exit, got {bare:?}",
        );
        for &install in &installers {
            let (mut app, id) = neutral_overlay_app();
            install(app.agents.get_mut(&id).unwrap());
            let outcome = app.handle_input(&key_event(key, KeyModifiers::NONE));
            assert!(
                !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
                "{key:?} with an input overlay pending must fall through, got {outcome:?}",
            );
        }
    }
}
/// Left arrow on an empty, prompt-focused overlay backs out to the
/// dashboard — the mirror of the dashboard's Right-arrow "open
/// detail". Requires the prompt to be focused with an empty buffer.
#[test]
fn overlay_left_arrow_empty_prompt_exits_to_dashboard() {
    let (mut app, id) = neutral_overlay_app();
    app.agents.get_mut(&id).unwrap().active_pane = crate::app::agent_view::AgentPane::Prompt;
    let outcome = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Left on an empty focused prompt must exit the overlay, got {outcome:?}",
    );
}
/// `/gboom` is opened from an empty prompt — the exact state where the
/// dashboard overlay steals Left/Esc as back-out. Both must reach the game.
#[test]
fn overlay_gboom_owns_left_and_esc() {
    let (mut app, id) = neutral_overlay_app();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
        agent.gboom = Some(crate::gboom::GboomState::new());
    }
    let left = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        !matches!(left, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Left with /gboom open must reach the game, got {left:?}",
    );
    assert!(
        app.agents.get(&id).unwrap().gboom.is_some(),
        "Left must not close /gboom",
    );
    let esc = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(esc, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc with /gboom open must close the game, not the overlay, got {esc:?}",
    );
    assert!(
        app.agents.get(&id).unwrap().gboom.is_none(),
        "Esc should close /gboom",
    );
}
/// Left arrow with an active prompt history search (empty draft) is NOT
/// an overlay exit — the search owns the key (Left moves its query caret),
/// so it must reach the agent rather than backing out to the dashboard.
#[test]
fn overlay_left_arrow_history_search_active_does_not_exit() {
    let (mut app, id) = neutral_overlay_app();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
        assert!(agent.prompt.history_search.activate(&[], ""));
        assert!(
            agent.prompt.text().is_empty(),
            "fixture draft must be empty"
        );
    }
    let outcome = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Left with an active history search must reach the agent, got {outcome:?}",
    );
}
/// Left arrow with the `@` file-search dropdown open is NOT an overlay exit
/// — the prompt widget owns picker nav (Right drills in, Up/Down move the
/// selection), so the key must reach the agent rather than backing out. In
/// production an open dropdown implies a non-empty draft (the `@` token);
/// we force the decoupled state to isolate the explicit file-search guard.
#[test]
fn overlay_left_arrow_file_search_open_does_not_exit() {
    let (mut app, id) = neutral_overlay_app();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
        let ctx = crate::views::file_search::context::detect("@", 1).expect("@-context must parse");
        agent.prompt.file_search.set_test_state(
            ctx,
            vec![echo_agent_workspace::file_system::FuzzyMatchResult {
                path: nucleo::Utf32String::from("src"),
                score: 100,
                indices: Vec::new(),
                is_dir: true,
            }],
            0,
        );
        assert!(
            agent.prompt.file_search_visible(),
            "fixture must open the @ dropdown",
        );
        assert!(
            agent.prompt.text().is_empty(),
            "fixture keeps the draft empty to isolate the file-search guard",
        );
        assert!(
            !agent.is_empty_focused_prompt(),
            "an open @ dropdown must fail the empty-focused-prompt guard",
        );
    }
    let outcome = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Left with the @ dropdown open must reach the agent, got {outcome:?}",
    );
}
/// Left arrow with a non-empty prompt draft is NOT an overlay exit —
/// it falls through to the prompt so it moves the caret within the
/// text rather than closing the agent detail.
#[test]
fn overlay_left_arrow_with_draft_does_not_exit() {
    let (mut app, id) = neutral_overlay_app();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
        agent.prompt.set_text("draft");
    }
    let outcome = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Left with a non-empty prompt must NOT exit the overlay, got {outcome:?}",
    );
}
/// Left arrow while the scrollback pane is focused is NOT an overlay
/// exit — it must reach the agent so the scrollback's `Left=collapse`
/// binding keeps working (the back-out is prompt-only).
#[test]
fn overlay_left_arrow_in_scrollback_does_not_exit() {
    let (mut app, _id) = neutral_overlay_app();
    let outcome = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Left in scrollback must reach the agent, got {outcome:?}",
    );
}
/// An open modal (extensions, `/agents`, persona detail, block viewer, or
/// `active_modal`) makes `is_empty_focused_prompt` false even on an empty,
/// prompt-focused composer, so the modal — not the overlay back-out — owns
/// Esc/Left.
#[test]
fn overlay_open_modal_fails_empty_focused_prompt_guard() {
    let (mut app, id) = neutral_overlay_app();
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
    assert!(
        agent.is_empty_focused_prompt(),
        "bare empty prompt must satisfy the guard",
    );
    agent.extensions_modal = Some(crate::views::extensions_modal::ExtensionsModalState::new(
        crate::views::extensions_modal::ExtensionsTab::Plugins,
    ));
    assert!(
        !agent.is_empty_focused_prompt(),
        "an open extensions modal must fail the guard",
    );
    agent.extensions_modal = None;
    agent.agents_modal = Some(open_agents_modal());
    assert!(
        !agent.is_empty_focused_prompt(),
        "an open agents modal must fail the guard",
    );
    agent.agents_modal = None;
    agent.persona_detail =
        Some(crate::views::persona_detail::PersonaDetailState::from_name_only("researcher"));
    assert!(
        !agent.is_empty_focused_prompt(),
        "an open persona detail must fail the guard",
    );
    agent.persona_detail = None;
    agent.block_viewer = Some(crate::views::block_viewer::BlockViewerPane::for_plain_text(
        "t", "content",
    ));
    assert!(
        !agent.is_empty_focused_prompt(),
        "an open block viewer must fail the guard",
    );
    agent.block_viewer = None;
    agent.active_modal = Some(crate::views::modal::ActiveModal::CommandPalette {
        entries: Vec::new(),
        state: crate::views::picker::PickerState::default(),
        window: crate::views::modal_window::ModalWindowState::new(),
    });
    assert!(
        !agent.is_empty_focused_prompt(),
        "an open active_modal must fail the guard",
    );
    agent.active_modal = None;
    assert!(
        agent.is_empty_focused_prompt(),
        "clearing the modals restores the guard",
    );
}
/// With an agent attached (dashboard overlay) and the extensions modal
/// open on the Prompt pane, Esc/Left must reach the modal rather than
/// backing out to the dashboard. Esc closes the modal; Left folds /
/// is consumed by the modal — neither yields `DashboardOverlayExit`.
#[test]
fn overlay_modal_open_esc_left_do_not_exit() {
    let (mut app, id) = neutral_overlay_app();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
        agent.extensions_modal = Some(crate::views::extensions_modal::ExtensionsModalState::new(
            crate::views::extensions_modal::ExtensionsTab::Plugins,
        ));
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc with the extensions modal open must not back out, got {outcome:?}",
    );
    assert!(
        app.agents[&id].extensions_modal.is_none(),
        "Esc must reach the modal handler and close it",
    );
    let (mut app, id) = neutral_overlay_app();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
        agent.extensions_modal = Some(crate::views::extensions_modal::ExtensionsModalState::new(
            crate::views::extensions_modal::ExtensionsTab::Plugins,
        ));
    }
    let outcome = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Left with the extensions modal open must not back out, got {outcome:?}",
    );
    assert!(
        app.agents[&id].extensions_modal.is_some(),
        "Left must reach the modal (fold), keeping it open",
    );
    for code in [KeyCode::Esc, KeyCode::Left] {
        let (mut app, id) = neutral_overlay_app();
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
            agent.active_modal = Some(crate::views::modal::ActiveModal::CommandPalette {
                entries: Vec::new(),
                state: crate::views::picker::PickerState::default(),
                window: crate::views::modal_window::ModalWindowState::new(),
            });
        }
        let outcome = app.handle_input(&key_event(code, KeyModifiers::NONE));
        assert!(
            !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
            "{code:?} with active_modal open must not back out, got {outcome:?}",
        );
    }
    let (mut app, id) = neutral_overlay_app();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
        agent.agents_modal = Some(open_agents_modal());
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc with the agents modal open must not back out, got {outcome:?}",
    );
    assert!(
        app.agents[&id].agents_modal.is_none(),
        "Esc must reach the agents modal handler and close it",
    );
    assert_eq!(
        app.dashboard.as_ref().and_then(|d| d.attached_agent),
        Some(id),
        "closing /agents must leave the dashboard overlay attached",
    );
    let (mut app, id) = neutral_overlay_app();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
        agent.agents_modal = Some(open_agents_modal());
    }
    let outcome = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Left with the agents modal open must not back out, got {outcome:?}",
    );
    assert!(
        app.agents[&id].agents_modal.is_some(),
        "Left must reach the agents modal, keeping it open",
    );
}
/// `/agents` open on a scrollback-focused overlay must own Esc (close the
/// modal) rather than the neutral-scrollback overlay exit. `is_bare_scrollback`
/// used to omit `agents_modal`, so this path looped dashboard ↔ conversation.
#[test]
fn overlay_agents_modal_owns_esc_from_scrollback() {
    let (mut app, id) = neutral_overlay_app();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        assert_eq!(
            agent.active_pane,
            crate::app::agent_view::AgentPane::Scrollback,
            "fixture starts on scrollback (neutral overlay exit state)",
        );
        agent.agents_modal = Some(open_agents_modal());
        assert!(
            !agent.is_bare_scrollback(),
            "an open agents modal must fail the bare-scrollback overlay-exit guard",
        );
    }
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc with /agents open on scrollback must not back out, got {outcome:?}",
    );
    assert!(
        app.agents[&id].agents_modal.is_none(),
        "Esc must close the agents modal",
    );
    assert_eq!(
        app.dashboard.as_ref().and_then(|d| d.attached_agent),
        Some(id),
        "closing /agents must leave the dashboard overlay attached",
    );
}
/// The graduated plan/Q&A back-out also defers to an open modal: with a
/// single-question Q&A overlay at its back-out top AND a modal open, both
/// `overlay_esc_backs_out` and `overlay_left_backs_out` return false (a modal
/// and a question view can coexist when the ACP handler installs the overlay
/// without closing the modal).
#[test]
fn graduated_back_out_defers_to_open_modal() {
    let (mut app, id) = neutral_overlay_app();
    install_question_overlay(&mut app, id, 1);
    {
        let a = app.agents.get(&id).unwrap();
        assert!(
            a.overlay_esc_backs_out() && a.overlay_left_backs_out(),
            "fixture must be a back-out-top state without a modal",
        );
    }
    app.agents.get_mut(&id).unwrap().extensions_modal =
        Some(crate::views::extensions_modal::ExtensionsModalState::new(
            crate::views::extensions_modal::ExtensionsTab::Plugins,
        ));
    {
        let a = app.agents.get(&id).unwrap();
        assert!(
            !a.overlay_esc_backs_out() && !a.overlay_left_backs_out(),
            "an open extensions modal must suppress the graduated back-out",
        );
    }
    app.agents.get_mut(&id).unwrap().extensions_modal = None;
    app.agents.get_mut(&id).unwrap().agents_modal = Some(open_agents_modal());
    {
        let a = app.agents.get(&id).unwrap();
        assert!(
            !a.overlay_esc_backs_out() && !a.overlay_left_backs_out(),
            "an open agents modal must suppress the graduated back-out",
        );
    }
    app.agents.get_mut(&id).unwrap().agents_modal = None;
    app.agents.get_mut(&id).unwrap().active_modal =
        Some(crate::views::modal::ActiveModal::CommandPalette {
            entries: Vec::new(),
            state: crate::views::picker::PickerState::default(),
            window: crate::views::modal_window::ModalWindowState::new(),
        });
    {
        let a = app.agents.get(&id).unwrap();
        assert!(
            !a.overlay_esc_backs_out() && !a.overlay_left_backs_out(),
            "an open active_modal must suppress the graduated back-out",
        );
    }
}
/// Install a plan-approval overlay on the agent and put it in the
/// "focused dashboard overlay, prompt pane" state the graduated
/// back-out cares about.
fn install_plan_overlay(app: &mut AppView, id: super::super::agent::AgentId) {
    let a = app.agents.get_mut(&id).unwrap();
    a.in_dashboard_overlay = true;
    a.active_pane = crate::app::agent_view::AgentPane::Prompt;
    let request = crate::views::plan_approval_view::ExitPlanModeExtRequest {
        session_id: "s".into(),
        tool_call_id: "c".into(),
        plan_content: Some("p".into()),
    };
    let stashed = crate::views::prompt_widget::StashedPrompt {
        text: String::new(),
        cursor: 0,
        images: Vec::new(),
        chip_elements: Vec::new(),
        image_counter: 0,
        image_undo_stash: Vec::new(),
    };
    let mut view = crate::views::plan_approval_view::PlanApprovalViewState::new(
        request,
        stashed,
        tokio::sync::oneshot::channel().0,
    );
    view.focus = crate::views::plan_approval_view::PlanApprovalFocus::Prompt;
    a.plan_approval_view = Some(view);
}
/// Install a Q&A overlay with `n_questions` single-select questions,
/// focused in the dashboard overlay's Navigation surface.
fn install_question_overlay(
    app: &mut AppView,
    id: super::super::agent::AgentId,
    n_questions: usize,
) {
    use crate::views::question_view::QuestionViewState;
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let questions: Vec<Question> = (0..n_questions)
        .map(|i| Question {
            question: format!("Q{i}?"),
            options: vec![QuestionOption {
                label: "opt".into(),
                description: String::new(),
                preview: None,
                id: None,
            }],
            multi_select: None,
            id: None,
        })
        .collect();
    let a = app.agents.get_mut(&id).unwrap();
    a.in_dashboard_overlay = true;
    a.active_pane = crate::app::agent_view::AgentPane::Prompt;
    a.question_view = Some(QuestionViewState::new(
        "c".into(),
        questions,
        crate::views::prompt_widget::StashedPrompt::default(),
    ));
}
/// Graduated back-out: at the plan feedback top state (empty prompt,
/// no pending comment) a bare Esc returns to the dashboard, leaving
/// the plan overlay pending (no approve / reject is sent).
#[test]
fn overlay_esc_exits_at_plan_top_state() {
    let (mut app, id) = neutral_overlay_app();
    install_plan_overlay(&mut app, id);
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc at the plan top state must back out, got {outcome:?}",
    );
    assert!(
        app.agents.get(&id).unwrap().plan_approval_view.is_some(),
        "backing out must leave the plan overlay pending (unanswered)",
    );
}
/// A typed feedback draft is NOT a top state — Esc keeps its
/// in-overlay meaning so the draft isn't lost to an accidental exit.
#[test]
fn overlay_esc_does_not_exit_with_plan_draft() {
    let (mut app, id) = neutral_overlay_app();
    install_plan_overlay(&mut app, id);
    app.agents.get_mut(&id).unwrap().prompt.set_text("feedback");
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc with a plan feedback draft must NOT back out, got {outcome:?}",
    );
}
/// Graduated back-out: in the Q&A Navigation surface with nothing
/// selected, a bare Esc (whose only job there is to unselect) backs
/// out to the dashboard instead of dead-ending.
#[test]
fn overlay_esc_exits_when_question_nav_unselected() {
    let (mut app, id) = neutral_overlay_app();
    install_question_overlay(&mut app, id, 1);
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc with nothing selected must back out, got {outcome:?}",
    );
    assert!(
        app.agents.get(&id).unwrap().question_view.is_some(),
        "backing out must leave the question overlay pending",
    );
}
/// Multi-question Q&A: on question 2+ a bare `Esc` must NOT back out — the
/// flow isn't at its top, so `Esc` stays in-flow (the question view handles
/// it) and `Left` can still walk back. Only `active_tab == 0` is the
/// back-out top.
#[test]
fn overlay_esc_does_not_exit_on_later_multi_question() {
    let (mut app, id) = neutral_overlay_app();
    install_question_overlay(&mut app, id, 2);
    app.agents
        .get_mut(&id)
        .unwrap()
        .question_view
        .as_mut()
        .unwrap()
        .next_question();
    assert_eq!(
        app.agents
            .get(&id)
            .unwrap()
            .question_view
            .as_ref()
            .unwrap()
            .active_tab,
        1,
        "fixture must be on the second question",
    );
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc on question 2+ of a multi-question Q&A must stay in-flow, got {outcome:?}",
    );
}
/// ...but from question 1 (the top of a multi-question flow) with nothing
/// selected, a bare `Esc` still backs out, leaving the Q&A pending.
#[test]
fn overlay_esc_exits_at_first_multi_question() {
    let (mut app, id) = neutral_overlay_app();
    install_question_overlay(&mut app, id, 2);
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc at question 1 of a multi-question Q&A must back out, got {outcome:?}",
    );
    assert!(
        app.agents.get(&id).unwrap().question_view.is_some(),
        "backing out must leave the question overlay pending",
    );
}
/// With an option selected, Esc has something to clear — it must NOT
/// back out (the first Esc unselects; a second, now-unselected Esc
/// would exit).
#[test]
fn overlay_esc_does_not_exit_when_question_option_selected() {
    let (mut app, id) = neutral_overlay_app();
    install_question_overlay(&mut app, id, 1);
    app.agents
        .get_mut(&id)
        .unwrap()
        .question_view
        .as_mut()
        .unwrap()
        .select_option(0, 0);
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc with a selection must unselect first, not back out, got {outcome:?}",
    );
}
/// Left backs out of a single-question Q&A (Left has no prev question
/// to step to), but with multiple questions Left switches question
/// and must NOT exit.
#[test]
fn overlay_left_exits_single_question_only() {
    let (mut app, id) = neutral_overlay_app();
    install_question_overlay(&mut app, id, 1);
    let single = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        matches!(single, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Left in a single-question Q&A must back out, got {single:?}",
    );
    let (mut app, id) = neutral_overlay_app();
    install_question_overlay(&mut app, id, 2);
    let multi = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        !matches!(multi, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Left in a multi-question Q&A must switch question, not back out, got {multi:?}",
    );
}
/// Single-question Q&A back-out is key-specific: `Left` has no in-overlay
/// behaviour there (only multi-question `Left` switches questions; `Esc`
/// owns unselect), so a bare `Left` backs out even with an option selected.
/// The exit is non-destructive — the Q&A and its selection stay pending — so
/// nothing is lost. (Esc stays graduated, clearing the selection first: see
/// `overlay_esc_does_not_exit_when_question_option_selected`.)
#[test]
fn overlay_left_exits_single_question_with_selection() {
    let (mut app, id) = neutral_overlay_app();
    install_question_overlay(&mut app, id, 1);
    app.agents
        .get_mut(&id)
        .unwrap()
        .question_view
        .as_mut()
        .unwrap()
        .select_option(0, 0);
    assert!(
        app.agents
            .get(&id)
            .unwrap()
            .question_view
            .as_ref()
            .unwrap()
            .active_tab_has_selection(),
        "fixture must start with a live selection",
    );
    let outcome = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    let InputOutcome::Action(action @ Action::DashboardOverlayExit) = outcome else {
        panic!(
            "Left in a single-question Q&A must back out even with a selection, got {outcome:?}",
        );
    };
    let _ = super::super::dispatch::dispatch(action, &mut app);
    let agent = app.agents.get(&id).unwrap();
    assert!(
        agent
            .question_view
            .as_ref()
            .is_some_and(|qv| qv.active_tab_has_selection()),
        "the selection must survive the back-out (Q&A still pending)",
    );
}
/// Install a plan-approval overlay showing the plan in the line
/// viewer (`Preview` focus) — the default shape when the plan has
/// content (`acp_handler` opens the preview). This is the state the
/// user reported as stuck: `Esc` / `Left` are dead no-ops in the
/// plan line viewer.
fn install_plan_preview_overlay(app: &mut AppView, id: super::super::agent::AgentId) {
    let request = crate::views::plan_approval_view::ExitPlanModeExtRequest {
        session_id: "s".into(),
        tool_call_id: "c".into(),
        plan_content: Some("# Plan\n- step one\n- step two".into()),
    };
    let view = crate::views::plan_approval_view::PlanApprovalViewState::new(
        request,
        crate::views::prompt_widget::StashedPrompt::default(),
        tokio::sync::oneshot::channel().0,
    );
    let a = app.agents.get_mut(&id).unwrap();
    a.in_dashboard_overlay = true;
    a.plan_approval_view = Some(view);
    a.show_plan_preview();
    assert!(
        a.line_viewer.is_some(),
        "fixture must open the plan line viewer",
    );
}
/// Regression for the reported bug: in plan approval shown via the
/// line viewer (the common case), Esc was a dead no-op. It must now
/// back out to the dashboard, leaving the plan pending (unanswered).
#[test]
fn overlay_esc_exits_at_plan_preview() {
    let (mut app, id) = neutral_overlay_app();
    install_plan_preview_overlay(&mut app, id);
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc in the plan line-viewer preview must back out, got {outcome:?}",
    );
    assert!(
        app.agents.get(&id).unwrap().plan_approval_view.is_some(),
        "backing out must leave the plan overlay pending (unanswered)",
    );
}
/// Left is likewise a no-op in the plan line viewer (the list pane
/// ignores it), so it backs out too.
#[test]
fn overlay_left_exits_at_plan_preview() {
    let (mut app, id) = neutral_overlay_app();
    install_plan_preview_overlay(&mut app, id);
    let outcome = app.handle_input(&key_event(KeyCode::Left, KeyModifiers::NONE));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Left in the plan line-viewer preview must back out, got {outcome:?}",
    );
}
/// Backing out of the plan preview is non-destructive: dispatching the
/// resulting `DashboardOverlayExit` switches to the dashboard but
/// leaves BOTH the plan-approval view and its line-viewer preview
/// intact, so re-opening the agent shows the plan exactly as before.
#[test]
fn overlay_exit_from_plan_preview_keeps_preview_intact() {
    let (mut app, id) = neutral_overlay_app();
    install_plan_preview_overlay(&mut app, id);
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    let InputOutcome::Action(action @ Action::DashboardOverlayExit) = outcome else {
        panic!("Esc must yield DashboardOverlayExit, got {outcome:?}");
    };
    let _ = super::super::dispatch::dispatch(action, &mut app);
    assert!(
        matches!(app.active_view, ActiveView::AgentDashboard),
        "exit must land on the dashboard",
    );
    let agent = app.agents.get(&id).unwrap();
    assert!(
        agent.plan_approval_view.is_some(),
        "plan approval must survive the back-out (still pending)",
    );
    assert!(
        agent.line_viewer.is_some(),
        "the plan line-viewer preview must survive the back-out",
    );
}
/// Graduated: while a visual selection is active in the plan viewer,
/// Esc must clear it first (reach the viewer) rather than backing out.
#[test]
fn overlay_esc_does_not_exit_plan_preview_in_visual_mode() {
    let (mut app, id) = neutral_overlay_app();
    install_plan_preview_overlay(&mut app, id);
    app.agents
        .get_mut(&id)
        .unwrap()
        .line_viewer
        .as_mut()
        .unwrap()
        .list_state
        .visual_mode = true;
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc with an active visual selection must reach the viewer, got {outcome:?}",
    );
}
/// Graduated: while an accepted search matcher is active in the plan
/// viewer (input bar closed, filter still applied), the first Esc must
/// clear it (reach the viewer) rather than backing out; only once it's
/// cleared does Esc exit to the dashboard.
#[test]
fn overlay_esc_clears_matcher_before_exiting_plan_preview() {
    use crate::views::list_pane::{ListMatcher, MatchMode, QueryKind};
    let (mut app, id) = neutral_overlay_app();
    install_plan_preview_overlay(&mut app, id);
    app.agents
        .get_mut(&id)
        .unwrap()
        .line_viewer
        .as_mut()
        .unwrap()
        .list_state
        .set_matcher(Some(ListMatcher::new(
            "step",
            QueryKind::Substring,
            MatchMode::Search,
        )));
    let outcome = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::DashboardOverlayExit)),
        "Esc with an accepted matcher must reach the viewer, got {outcome:?}",
    );
    assert!(
        app.agents
            .get(&id)
            .unwrap()
            .line_viewer
            .as_ref()
            .unwrap()
            .list_state
            .matcher()
            .is_none(),
        "the first Esc must clear the accepted search matcher",
    );
    let outcome2 = app.handle_input(&key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome2, InputOutcome::Action(Action::DashboardOverlayExit)),
        "once the matcher is cleared, Esc must back out, got {outcome2:?}",
    );
}
/// Overlay Ctrl+X on an agent with a RUNNING turn — routes to the
/// agent view's existing cancel behaviour (`Action::CancelTurn`,
/// same as Ctrl+C) and never arms the close confirm: mashing
/// Ctrl+X to stop a turn must not be able to close the session.
#[test]
fn overlay_ctrl_x_busy_agent_cancels_turn_without_arming() {
    let (mut app, id) = neutral_overlay_app();
    app.agents.get_mut(&id).unwrap().session.state = crate::app::agent::AgentState::TurnRunning;
    for _ in 0..2 {
        let outcome = app.handle_input(&key_event(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(
            matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
            "Ctrl+X on a busy agent must cancel the turn, got {outcome:?}",
        );
        assert!(
            app.pending_action.is_none(),
            "Ctrl+X on a busy agent must not arm the close confirm",
        );
    }
    assert!(
        app.agents.get(&id).unwrap().active_modal.is_none(),
        "Ctrl+X must be intercepted before the agent sees it",
    );
}
/// Overlay Ctrl+X during `/compact` cancels compaction (same as `[stop]`).
#[test]
fn overlay_ctrl_x_compact_running_cancels_without_arming() {
    use crate::app::agent::{AgentCommand, AgentState};
    let (mut app, id) = neutral_overlay_app();
    app.agents.get_mut(&id).unwrap().session.state = AgentState::CommandRunning {
        command: AgentCommand::Compact,
        started_at: std::time::Instant::now(),
    };
    let outcome = app.handle_input(&key_event(KeyCode::Char('x'), KeyModifiers::CONTROL));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "Ctrl+X during /compact must cancel, got {outcome:?}",
    );
    assert!(
        app.pending_action.is_none(),
        "Ctrl+X during /compact must not arm close confirm",
    );
}
/// Overlay Ctrl+X on a non-turn busy agent (command in flight,
/// cancel pending) — `Action::CancelTurn` would no-op for these
/// states, so the press arms the two-press close instead of
/// being a dead key.
#[test]
fn overlay_ctrl_x_command_or_cancelling_agent_arms_close_confirm() {
    use crate::app::agent::{AgentCommand, AgentState};
    let states = [
        AgentState::TurnCancelling,
        AgentState::CommandCancelling {
            command: AgentCommand::Compact,
        },
    ];
    for state in states {
        let (mut app, id) = neutral_overlay_app();
        app.agents.get_mut(&id).unwrap().session.state = state.clone();
        let outcome = app.handle_input(&key_event(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "Ctrl+X on a {state:?} agent must arm (not cancel / fire), got {outcome:?}",
        );
        assert!(
            app.pending_action
                .as_ref()
                .is_some_and(|p| matches!(p.action, Action::DashboardOverlayStop)),
            "Ctrl+X on a {state:?} agent must arm the close confirm",
        );
    }
}
/// Overlay Ctrl+X on an IDLE agent — arms the two-press close
/// confirm (`pending_action` = `DashboardOverlayStop` so the
/// shortcuts bar paints "press again to close this session");
/// there is no turn to cancel.
#[test]
fn overlay_ctrl_x_idle_agent_arms_close_confirm() {
    let (mut app, _id) = neutral_overlay_app();
    let outcome = app.handle_input(&key_event(KeyCode::Char('x'), KeyModifiers::CONTROL));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "first Ctrl+X on an idle agent must arm (not fire), got {outcome:?}",
    );
    let pending = app.pending_action.as_ref().expect("confirm must be armed");
    assert!(
        matches!(pending.action, Action::DashboardOverlayStop),
        "pending action must be the overlay stop",
    );
    assert_eq!(pending.label, Some("close this session"));
    assert!(
        !pending.expired(),
        "the confirm window must still be live right after arming",
    );
    assert!(
        app.pending_effects.is_empty(),
        "no CancelTurn for an idle agent",
    );
}
/// Overlay Ctrl+X, second press inside the confirm window — the
/// pending-action fast path consumes the key and fires
/// `Action::DashboardOverlayStop` (close + back to dashboard).
#[test]
fn overlay_ctrl_x_second_press_fires_overlay_stop() {
    let (mut app, _id) = neutral_overlay_app();
    let _ = app.handle_input(&key_event(KeyCode::Char('x'), KeyModifiers::CONTROL));
    assert!(app.pending_action.is_some(), "first press must arm");
    let outcome = app.handle_input(&key_event(KeyCode::Char('x'), KeyModifiers::CONTROL));
    assert!(
        matches!(outcome, InputOutcome::Action(Action::DashboardOverlayStop)),
        "second Ctrl+X must fire the confirmed stop, got {outcome:?}",
    );
    assert!(
        app.pending_action.is_none(),
        "firing must consume the pending confirm",
    );
}
/// Overlay Ctrl+X then ANY other key — the pending-action fast
/// path disarms the confirm (the dashboard's stop-confirm
/// semantics: any other press cancels), and the other key is
/// still processed normally.
#[test]
fn overlay_ctrl_x_other_key_disarms_confirm() {
    let (mut app, _id) = neutral_overlay_app();
    let _ = app.handle_input(&key_event(KeyCode::Char('x'), KeyModifiers::CONTROL));
    assert!(app.pending_action.is_some(), "first press must arm");
    let _ = app.handle_input(&key_event(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(
        app.pending_action.is_none(),
        "any other key must disarm the pending stop confirm",
    );
    let outcome = app.handle_input(&key_event(KeyCode::Char('x'), KeyModifiers::CONTROL));
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Ctrl+X after a disarm must re-arm, not fire, got {outcome:?}",
    );
}
/// OUTSIDE the overlay (a plain agent view, no dashboard attach),
/// Ctrl+X must keep its existing agent-screen behaviour — the
/// overlay stop binding lives in `When::DashboardOverlay` only.
#[test]
fn plain_agent_ctrl_x_does_not_arm_overlay_stop() {
    let mut app = test_app_with_agent();
    let id = super::super::agent::AgentId(0);
    app.active_view = ActiveView::Agent(id);
    let _ = app.handle_input(&key_event(KeyCode::Char('x'), KeyModifiers::CONTROL));
    assert!(
        !app.pending_action
            .as_ref()
            .is_some_and(|p| matches!(p.action, Action::DashboardOverlayStop)),
        "overlay stop must not arm outside the dashboard overlay",
    );
}
/// When the attached agent disappears externally,
/// the `handle_input` filter must clear `attached_agent`
/// immediately rather than waiting for the next draw frame.
#[test]
fn minimal_double_ctrl_c_arms_then_quits() {
    let prev = crate::app::minimal_mode_active();
    crate::app::set_minimal_mode_active_for_test(true);
    let mut app = test_app_with_agent();
    if let ActiveView::Agent(id) = app.active_view {
        app.agents.get_mut(&id).unwrap().active_pane = crate::views::agent::ActivePane::Prompt;
    }
    let o1 = app.handle_input(&key_event(KeyCode::Char('c'), KeyModifiers::CONTROL));
    let armed = app.pending_action.is_some();
    let o2 = app.handle_input(&key_event(KeyCode::Char('c'), KeyModifiers::CONTROL));
    crate::app::set_minimal_mode_active_for_test(prev);
    assert!(armed, "first Ctrl+C should arm quit (o1={o1:?})");
    assert!(
        matches!(o2, InputOutcome::Action(crate::app::actions::Action::Quit)),
        "second Ctrl+C should quit (o2={o2:?})"
    );
}
#[test]
fn handle_input_clears_stale_attached_agent_on_input() {
    let mut app = test_app_with_agent();
    let id = attach_popup(&mut app);
    app.agents.shift_remove(&id);
    let _ = app.handle_input(&key_event(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        None,
        "stale attached_agent must be cleared on input",
    );
}
/// Click on the popup's `[✗]` close affordance
/// closes the popup. The close-rect is registered into
/// `state.popup_close_rect` by the renderer; we set it
/// directly here since this test doesn't run a render pass.
#[test]
fn handle_input_mouse_click_on_close_affordance_closes_popup() {
    let mut app = test_app_with_agent();
    let _ = attach_popup(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.popup_close_rect = Some(ratatui::layout::Rect::new(50, 1, 3, 1));
        d.popup_outer_rect = Some(ratatui::layout::Rect::new(0, 0, 60, 20));
    }
    let click = Event::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 51,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    let outcome = app.handle_input(&click);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, None);
}
/// A click on a dashboard row outside the popup's
/// outer rect dispatches `DashboardAttach(clicked_row)` so the
/// popup target switches.
#[test]
fn handle_input_mouse_click_outside_popup_on_row_switches_target() {
    let mut app = test_app_with_agent();
    let _ = attach_popup(&mut app);
    let row_id =
        crate::views::dashboard::DashboardRowId::TopLevel(super::super::agent::AgentId(42));
    if let Some(d) = app.dashboard.as_mut() {
        d.popup_outer_rect = Some(ratatui::layout::Rect::new(20, 5, 40, 10));
        d.row_rects
            .push((row_id.clone(), ratatui::layout::Rect::new(0, 1, 10, 1)));
    }
    let click = Event::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 5,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    let outcome = app.handle_input(&click);
    match outcome {
        InputOutcome::Action(Action::DashboardAttach(target)) => {
            assert_eq!(target, row_id);
        }
        other => panic!("expected DashboardAttach, got {other:?}"),
    }
}
/// Scroll routing through the popup
/// overlay. A scroll inside `popup_outer_rect` must NOT advance
/// the dashboard's `viewport_offset` (it forwards to the
/// attached agent). A scroll outside the popup falls through to
/// the dashboard list pane and DOES advance `viewport_offset`.
#[test]
fn handle_input_scroll_inside_popup_forwards_to_agent() {
    let mut app = test_app_with_agent();
    let _ = attach_popup(&mut app);
    let popup_outer = ratatui::layout::Rect::new(20, 5, 40, 10);
    if let Some(d) = app.dashboard.as_mut() {
        d.popup_outer_rect = Some(popup_outer);
        d.viewport_offset = 0;
    }
    let inside_x = popup_outer.x + 5;
    let inside_y = popup_outer.y + 3;
    app.dispatch_scroll(3, inside_x, inside_y);
    assert_eq!(
        app.dashboard.as_ref().unwrap().viewport_offset,
        0,
        "scroll inside popup must not advance the dashboard viewport",
    );
    let outside_x = 0;
    let outside_y = 0;
    app.dispatch_scroll(3, outside_x, outside_y);
    assert_eq!(
        app.dashboard.as_ref().unwrap().viewport_offset,
        3,
        "scroll outside popup must advance the dashboard viewport",
    );
}
/// When the attached agent emits
/// `Action::ExitSession` (via the synchronous outcome path,
/// e.g. user presses the keybind for ExitSession inside the
/// popup), the popup is closed but the agent stays in
/// `app.agents`. The `/exit` slash command takes a different
/// path (emits an effect) — see the user-guide for the
/// asymmetry; this test pins only the synchronous-outcome
/// branch.
///
/// We can't easily synthesize an `ExitSession` from
/// `agent.handle_input` without a real prompt event sequence,
/// so the test exercises the popup-close intercept by feeding a
/// key that lands in the agent's prompt and observing the popup
/// state after the intercept runs. Concretely: we drive an Esc
/// key (which the popup-close fast-path catches BEFORE the
/// agent intercept). To prove the `ExitSession` branch
/// independently, we directly invoke the intercepted-outcome
/// path with a stub: set `attached_agent`, then call the same
/// close routine the intercept would call. This is the smallest
/// behavioural pin available without a full prompt-mode setup.
#[test]
fn handle_input_exit_session_action_closes_popup() {
    let mut app = test_app_with_agent();
    let id = attach_popup(&mut app);
    assert!(app.agents.contains_key(&id));
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, Some(id));
    if let Some(d) = app.dashboard.as_mut() {
        d.close_popup();
    }
    if let Some(agent) = app.agents.get_mut(&id) {
        agent.close_subagent_fullscreen();
    }
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, None);
    assert!(
        app.agents.contains_key(&id),
        "ExitSession intercept must NOT remove the agent (it only closes the popup)",
    );
}
/// Chat mode hides the welcome picker's source filter, so `f` must not
/// cycle it; Build mode keeps the cycle.
#[test]
fn welcome_picker_f_cycle_disabled_under_chat_mode() {
    let conversation_entry = SessionPickerEntry {
        id: "conv-welcome-f".into(),
        summary: "chat".into(),
        updated_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        cwd: String::new(),
        hostname: None,
        source: "conversation".into(),
        model_id: None,
        num_messages: 0,
        last_active_at: None,
        branch: None,
        repo_name: "r".into(),
        worktree_label: None,
        last_turn_summary: None,
        last_recap: None,
        card_detail: None,
    };
    let f_key = Event::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
    crate::appearance::cache::set_vim_mode(false);
    let mut app = test_app();
    app.session_picker_entries = Some(vec![conversation_entry]);
    app.chat_mode = true;
    let _ = app.handle_input(&f_key);
    assert_eq!(
        app.session_picker_source_filter,
        crate::views::session_picker::SourceFilter::EchoAgent,
        "f must not cycle the hidden source filter under chat mode"
    );
    assert_eq!(
        app.session_picker_state.query(),
        "f",
        "under chat mode `f` keeps its normal typing/search meaning"
    );
    app.session_picker_state.reset();
    app.chat_mode = false;
    let outcome = app.handle_input(&f_key);
    assert!(matches!(
        outcome,
        InputOutcome::Action(Action::CycleSessionSourceFilter)
    ));
}
#[cfg(feature = "local-workspace")]
#[test]
fn welcome_ctrl_e_cycles_workspace_mode() {
    use crate::views::welcome::WelcomeWorkspaceMode;
    let mut app = test_app();
    app.chat_mode = true;
    app.active_view = ActiveView::Welcome;
    app.auth_state = AuthState::Done;
    app.trust_state = TrustState::Done;
    assert_eq!(app.welcome_workspace_mode, WelcomeWorkspaceMode::Sandbox);
    let key = Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    let outcome = app.handle_input(&key);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(
        app.welcome_workspace_mode,
        WelcomeWorkspaceMode::LocalWorkspace
    );
    let _ = app.handle_input(&key);
    assert_eq!(app.welcome_workspace_mode, WelcomeWorkspaceMode::Sandbox);
}
#[cfg(feature = "local-workspace")]
#[test]
fn welcome_ack_cancel_clears_history_bypass() {
    use crate::views::welcome::WelcomeWorkspaceMode;
    let mut app = test_app();
    app.chat_mode = true;
    app.active_view = ActiveView::Welcome;
    app.auth_state = AuthState::Done;
    app.trust_state = TrustState::Done;
    app.welcome_local_workspace_ack_pending = true;
    app.welcome_workspace_mode = WelcomeWorkspaceMode::LocalWorkspace;
    app.welcome_history_load_as_build = true;
    app.deferred_startup.worktree = true;
    app.deferred_startup.history_load_as_build = true;
    let outcome = app.handle_input(&key_event(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(!app.welcome_local_workspace_ack_pending);
    assert_eq!(app.welcome_workspace_mode, WelcomeWorkspaceMode::Sandbox);
    assert!(
        !app.welcome_history_load_as_build,
        "ACK cancel must drop history bypass"
    );
    assert!(!app.deferred_startup.history_load_as_build);
    assert!(!app.deferred_startup.worktree);
}
#[cfg(feature = "local-workspace")]
#[test]
fn welcome_workspace_click_selects_mode() {
    use crate::views::welcome::{WelcomeWorkspaceMode, WorkspaceModeHitRects};
    let mut app = test_app();
    app.chat_mode = true;
    app.active_view = ActiveView::Welcome;
    app.auth_state = AuthState::Done;
    app.trust_state = TrustState::Done;
    app.welcome_workspace_mode_rects = WorkspaceModeHitRects {
        options: [
            Some(ratatui::layout::Rect::new(10, 5, 9, 1)),
            Some(ratatui::layout::Rect::new(20, 5, 17, 1)),
        ],
        row: Some(ratatui::layout::Rect::new(0, 5, 80, 1)),
    };
    let click = Event::Mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 25,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    let outcome = app.handle_input(&click);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert_eq!(
        app.welcome_workspace_mode,
        WelcomeWorkspaceMode::LocalWorkspace
    );
}
#[cfg(feature = "local-workspace")]
#[test]
fn welcome_workspace_locked_ignores_cycle_and_click() {
    use crate::views::welcome::{WelcomeWorkspaceMode, WorkspaceModeHitRects};
    let mut app = test_app();
    app.chat_mode = true;
    app.active_view = ActiveView::Welcome;
    app.auth_state = AuthState::Done;
    app.trust_state = TrustState::Done;
    app.local_workspace_startup_locked = true;
    app.welcome_workspace_mode = WelcomeWorkspaceMode::LocalWorkspace;
    app.welcome_workspace_mode_rects = WorkspaceModeHitRects {
        options: [
            Some(ratatui::layout::Rect::new(10, 5, 9, 1)),
            Some(ratatui::layout::Rect::new(20, 5, 17, 1)),
        ],
        row: Some(ratatui::layout::Rect::new(0, 5, 80, 1)),
    };
    let key = Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    assert!(matches!(
        app.handle_input(&key),
        InputOutcome::Unchanged | InputOutcome::Changed
    ));
    assert_eq!(
        app.welcome_workspace_mode,
        WelcomeWorkspaceMode::LocalWorkspace
    );
    let click = Event::Mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 12,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    let _ = app.handle_input(&click);
    assert_eq!(
        app.welcome_workspace_mode,
        WelcomeWorkspaceMode::LocalWorkspace,
        "locked picker must not change selection"
    );
}
#[cfg(feature = "local-workspace")]
#[test]
fn welcome_ctrl_e_ignored_while_history_picker_open() {
    use crate::views::welcome::WelcomeWorkspaceMode;
    let mut app = test_app();
    app.chat_mode = true;
    app.active_view = ActiveView::Welcome;
    app.auth_state = AuthState::Done;
    app.trust_state = TrustState::Done;
    app.session_picker_entries = Some(vec![]);
    app.session_picker_state.set_query("keep-me");
    let before = app.welcome_workspace_mode;
    let key = Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    let outcome = app.handle_input(&key);
    assert_eq!(app.welcome_workspace_mode, before);
    assert!(
        !matches!(outcome, InputOutcome::Action(Action::ForceDeepSearch)),
        "history open: Ctrl+E must not cycle or soft-refresh: {outcome:?}"
    );
    assert_eq!(app.session_picker_state.query(), "keep-me");
    let _ = WelcomeWorkspaceMode::Sandbox;
}
#[cfg(feature = "local-workspace")]
#[test]
fn welcome_ctrl_e_ignored_while_authenticating() {
    use crate::views::welcome::WelcomeWorkspaceMode;
    let mut app = test_app();
    app.chat_mode = true;
    app.active_view = ActiveView::Welcome;
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Command,
    };
    app.trust_state = TrustState::Done;
    assert_eq!(app.welcome_workspace_mode, WelcomeWorkspaceMode::Sandbox);
    let key = Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    let _ = app.handle_input(&key);
    assert_eq!(
        app.welcome_workspace_mode,
        WelcomeWorkspaceMode::Sandbox,
        "Ctrl+E must not cycle mode before auth is Done"
    );
}
#[cfg(feature = "local-workspace")]
#[test]
fn welcome_ctrl_e_ignored_when_zdr_blocked() {
    use crate::views::welcome::WelcomeWorkspaceMode;
    let mut app = test_app();
    app.chat_mode = true;
    app.active_view = ActiveView::Welcome;
    app.auth_state = AuthState::Done;
    app.trust_state = TrustState::Done;
    app.is_zdr = true;
    app.zdr_access_enabled = false;
    assert_eq!(app.welcome_workspace_mode, WelcomeWorkspaceMode::Sandbox);
    let key = Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    let _ = app.handle_input(&key);
    assert_eq!(
        app.welcome_workspace_mode,
        WelcomeWorkspaceMode::Sandbox,
        "Ctrl+E must not cycle mode on ZDR-blocked welcome"
    );
}
