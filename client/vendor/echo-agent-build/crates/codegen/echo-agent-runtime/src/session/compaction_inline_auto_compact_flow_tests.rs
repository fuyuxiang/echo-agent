use super::super::support::*;
use super::super::*;
use super::{AutoCompactTriggerInfo, SuppressReason};
use crate::session::acp_session::McpReminderMode;
use crate::terminal::AsyncTerminalRunner;
use crate::terminal::runner::{TerminalError, TerminalRunRequest, TerminalRunResult};
use echo_agent_paths::AbsPathBuf;
use echo_agent_workspace::file_system::MockFs;
use echo_agent_workspace::permission::PermissionHandle;
use std::sync::OnceLock;
use tokio::sync::mpsc;
#[derive(Debug)]
struct DummyTerminal;
#[async_trait::async_trait]
impl AsyncTerminalRunner for DummyTerminal {
    async fn run(&self, _request: TerminalRunRequest) -> Result<TerminalRunResult, TerminalError> {
        Err(TerminalError::Other("dummy terminal".into()))
    }
}
/// Create a minimal SessionActor for testing auto-compact logic.
async fn create_test_actor(
    total_tokens: u64,
    context_window: u64,
    threshold_percent: u8,
    gateway_tx: mpsc::UnboundedSender<echo_agent_acp::AcpClientMessage>,
    persistence_tx: mpsc::UnboundedSender<PersistenceMsg>,
) -> SessionActor {
    let cwd = AbsPathBuf::new(std::path::PathBuf::from("/tmp")).unwrap();
    let fs = Arc::new(MockFs::new(cwd.to_path_buf()));
    let terminal = Arc::new(DummyTerminal {});
    let (hunk_tx, _hunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let hunk_tracker_handle = echo_agent_hunk_tracker::HunkTrackerActor::spawn(
        "test-auto-compact".to_string(),
        cwd.to_path_buf(),
        hunk_tx,
        echo_agent_hunk_tracker::TrackingMode::AgentOnly,
        tokio_util::sync::CancellationToken::new(),
    );
    let tool_context = ToolContext::new(cwd.clone(), None, None, fs, terminal, hunk_tracker_handle);
    let state = TokioMutex::new(State {
        running_task: None,
        pending_inputs: VecDeque::new(),
        edit_holds: HashMap::new(),
        pending_notifications: Vec::new(),
        notifications_suppressed: false,
        rewindable: false,
        front_message_committed: false,
        nudges_used_this_session: 0,
    });
    let (chat_event_tx, _chat_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (event_tx, _event_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::session::replay_events::SessionEvent>();
    let chat_state_handle = echo_agent_chat_state::ChatStateActor::spawn(
        vec![],
        echo_agent_sampling_types::SamplingConfig {
            base_url: "http://localhost".to_string(),
            model: "test".to_string(),
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: Default::default(),
            extra_headers: Default::default(),
            query_params: Default::default(),
            env_http_headers: Default::default(),
            context_window: std::num::NonZeroU64::new(context_window)
                .expect("test context_window must be non-zero"),
            reasoning_effort: None,
            stream_tool_calls: None,
        },
        Box::new(echo_agent_chat_state::NullChatPersistence),
        chat_event_tx,
        tokio_util::sync::CancellationToken::new(),
    );
    chat_state_handle.record_token_usage(total_tokens);
    SessionActor {
        status_wake: Default::default(),
        unattributed_background_usage: std::sync::atomic::AtomicBool::new(false),
        session_info: SessionInfo {
            id: acp::SessionId::new("test-auto-compact"),
            cwd: cwd.as_str().to_string(),
        },
        auth_method_id: test_auth_method_id("test-auth"),
        model_auth_memo: std::cell::RefCell::new(None),
        attribution_callback: None,
        auth_manager: None,
        is_chat_kind: false,
        state,
        notifications: NotificationSender {
            gateway: GatewaySender::new(gateway_tx),
            gateway_enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            persistence_tx,
            disk_full: crate::session::notifications::idle_disk_full_rx(),
        },
        permissions: PermissionHandle::allow_all(),
        tool_context,
        deny_read_globs: Vec::new(),
        mcp_state: Arc::new(TokioMutex::new(McpState::new(vec![]))),
        mcp_strategy: std::cell::Cell::new(McpInitStrategy::Blocking),
        delivery_tools: std::cell::RefCell::new(Vec::new()),
        attach_non_interactive: std::rc::Rc::new(std::cell::Cell::new(false)),
        chat_state_handle,
        current_prompt_id: std::sync::Arc::new(std::sync::Mutex::new(None)),
        pending_interactions: std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )),
        telemetry_enabled: false,
        supports_backend_search: std::cell::Cell::new(false),
        tool_overrides: std::cell::RefCell::new(None),
        resolved_tool_overrides: std::sync::Arc::new(arc_swap::ArcSwapOption::empty()),
        compactions_remaining: std::cell::Cell::new(None),
        compaction_at_tokens: std::cell::Cell::new(None),
        doom_loop_recovery: None,
        doom_loop_turn_tally: Default::default(),
        file_state_tracker: Arc::new(FileStateTracker::new()),
        rewind_pending_prompt: std::sync::Mutex::new(None),
        startup_hints: StartupHints::default(),
        forked_tool_override: None,
        compaction: crate::session::compaction_config::CompactionConfig {
            threshold_percent: std::cell::Cell::new(threshold_percent),
            force_compact: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            context_window_override: None,
            count: std::sync::atomic::AtomicU64::new(0),
            auto_compact_suppressed: std::sync::atomic::AtomicU8::new(0),
            previous_model: std::cell::Cell::new(None),
            compaction_mode: echo_agent_chat_state::CompactionMode::Transcript,
            verbatim_input: true,
            tool_choice: crate::util::config::CompactionToolChoice::Auto,
            prefire: crate::session::compaction_config::PrefireState::default(),
            prefix_released: std::sync::atomic::AtomicBool::new(false),
            cancel: Default::default(),
        },
        memory: crate::session::memory_state::SessionMemory {
            jobs: Default::default(),
            flush_config: crate::config::MemoryFlushConfig::default(),
            is_flushing: std::sync::atomic::AtomicBool::new(false),
            last_flush_compaction: std::sync::atomic::AtomicU64::new(0),
            storage: std::cell::RefCell::new(None),
            save_on_end: true,
            backend_params: None,
            initial_injection_config: Default::default(),
            context_injected: std::sync::atomic::AtomicBool::new(false),
            cached_context: Default::default(),
            flush_count: std::sync::atomic::AtomicU64::new(0),
            last_flush_content: std::cell::RefCell::new(None),
            flush_success_count: std::sync::atomic::AtomicU64::new(0),
            flush_error_count: std::sync::atomic::AtomicU64::new(0),
            search_counter: std::cell::RefCell::new(None),
            injection_count: std::sync::atomic::AtomicU64::new(0),
            compaction_recovery_count: std::sync::atomic::AtomicU64::new(0),
            chunks_added: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            dream_config: Default::default(),
            dream_count: std::sync::atomic::AtomicU64::new(0),
            dream_success_count: std::sync::atomic::AtomicU64::new(0),
            dream_error_count: std::sync::atomic::AtomicU64::new(0),
        },
        session_start: std::time::Instant::now(),
        inference_idle_timeout: std::time::Duration::from_secs(300),
        max_retries: 3,
        rate_limit_waits: crate::session::acp_session::RateLimitWaitConfig::default(),
        max_turns: None,
        pending_interjections: InterjectionBuffer::new(),
        pending_skill_reminders: Mutex::new(Vec::new()),
        idle_flush_timeout: None,
        dream_check_timeout: None,
        last_idle_flush_conversation_len: std::sync::atomic::AtomicUsize::new(0),
        event_tx,
        buffering_settings: None,
        client_identifier: None,
        origin_client: None,
        feedback_manager: Arc::new(FeedbackManager::local_only("test-session")),
        upload_queue: Arc::new(OnceLock::new()),
        sync_loop_cancel: None,
        agent: std::cell::RefCell::new(test_agent_default().await),
        last_reported_branch: std::sync::Arc::new(parking_lot::Mutex::new(None)),
        git_head_enabled: false,
        status_line_enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        models_manager: Default::default(),
        display_cwd: std::sync::OnceLock::new(),
        active_agent_type: parking_lot::Mutex::new(None),
        queue_exit_reminder_on_approved_exit: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        active_skill: parking_lot::Mutex::new(None),
        current_prompt_mode: Arc::new(parking_lot::Mutex::new(PromptMode::Agent)),
        turn_start_prompt_mode: parking_lot::Mutex::new(PromptMode::Agent),
        desktop_document_turn: std::cell::Cell::new(false),
        turn_prompt_mode: Arc::new(parking_lot::Mutex::new(PromptMode::Agent)),
        plan_mode: Arc::new(parking_lot::Mutex::new(
            crate::session::plan_mode::PlanModeTracker::new(std::path::PathBuf::from(
                "/tmp/test-session",
            )),
        )),
        goal_enabled: false,
        background_workflows_enabled: false,
        goal_harness_enabled: std::sync::atomic::AtomicBool::new(false),
        goal_harness_availability_reconciled: std::sync::atomic::AtomicBool::new(false),
        goal_tracker: Arc::new(parking_lot::Mutex::new(
            crate::session::goal_tracker::GoalTracker::new(std::path::PathBuf::from(
                "/tmp/test-session",
            )),
        )),
        goal_turn_task_ids: parking_lot::Mutex::new(std::collections::HashSet::new()),
        goal_continuation_streak: std::sync::atomic::AtomicU32::new(0),
        goal_blocked_streak: std::sync::atomic::AtomicU32::new(0),
        goal_update_rx: std::cell::RefCell::new(None),
        goal_update_tx: tokio::sync::mpsc::unbounded_channel().0,
        workflow_manager: crate::session::workflow::manager::WorkflowManager::test_bundle().0,
        workflow_launch_tx: tokio::sync::mpsc::unbounded_channel().0,
        goal_classifier_enabled: false,
        goal_planner_enabled: false,
        goal_summary_enabled: false,
        goal_verifier_skeptic_count: 1,
        goal_role_models: Default::default(),
        goal_use_current_model_only: false,
        goal_classifier_max_runs: crate::session::goal_classifier::GOAL_CLASSIFIER_MAX_RUNS_DEFAULT,
        goal_strategist_every: 5,
        goal_reverify_after: crate::session::acp_session::GOAL_REVERIFY_AFTER_DEFAULT,
        goal_plan_reconciled: std::sync::atomic::AtomicBool::new(false),
        pending_classifier_completions: parking_lot::Mutex::new(std::collections::VecDeque::new()),
        goal_classifier_in_flight: std::sync::atomic::AtomicBool::new(false),
        managed_mcp_handle: Default::default(),
        initial_client_mcp_servers: vec![],
        tool_metadata_snapshot: Arc::new(std::sync::Mutex::new(Default::default())),
        mcp_announced_servers: parking_lot::Mutex::new(std::collections::HashMap::new()),
        mcp_reminder_mode: McpReminderMode::Delta,
        mcp_reminder_dirty: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        mcp_connecting_reminder_injected: std::cell::Cell::new(false),
        mcp_handshakes_done: Arc::new(tokio::sync::Notify::new()),
        user_input_generation: std::sync::atomic::AtomicU64::new(0),
        laziness_debug_log: None,
        last_live_orphan_reconcile: std::cell::Cell::new(None),
        deferred_prefix: TaskSlot::new(),
        extension_registry: echo_agent_lifecycle::LocalExtensionRegistry::default(),
        last_announced_local_date: std::cell::Cell::new(chrono::Local::now().date_naive()),
        prefix_carries_fallback_date: std::cell::Cell::new(false),
        last_search_prompt_index: std::sync::atomic::AtomicI64::new(-1),
        last_api_request_at: std::sync::atomic::AtomicI64::new(0),
        hook_registry: std::cell::RefCell::new(None),
        turn_report: Default::default(),
        turn_abort: Default::default(),
        turn_end_tx: Default::default(),
        client_hooks: Default::default(),
        hook_resolved_workspace_root: String::new(),
        vcs_kind: echo_agent_workspace::session::git::VcsKind::Git,
        hook_load_errors: std::cell::RefCell::new(Vec::new()),
        plugin_registry: std::cell::RefCell::new(None),
        plugin_registry_handle: None,
        events: crate::session::events::EventTracker::new(std::path::Path::new("/tmp")),
        observability_bridge: noop_observability_bridge(),
        current_turn_number: std::cell::Cell::new(0),
        last_recap_main_turn: std::cell::Cell::new(0),
        recap_in_flight: std::cell::Cell::new(false),
        recap_epoch: std::cell::Cell::new(0),
        turn_summary_task: std::cell::RefCell::new(None),
        turn_summary_generation: std::cell::Cell::new(0),
        title_refresh_task: std::cell::RefCell::new(None),
        title_refresh_generation: std::cell::Cell::new(0),
        next_title_refresh_idx: std::cell::Cell::new(0),
        turn_summary_enabled: false,
        title_refresh_enabled: false,
        session_turn_active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        streaming_turn_capture: parking_lot::Mutex::new(
            crate::session::acp_session::StreamingTurnCapture::default(),
        ),
        turn_stream_drained: parking_lot::Mutex::new(None),
        pending_image_strip: parking_lot::Mutex::new(None),
        sampler_handle: echo_agent_sampler::SamplerHandle::noop(),
        sampling_gate: None,
        rebuild_spec: crate::session::agent_rebuild::test_rebuild_spec_default(),
        image_description_model: crate::test_support::TEST_MODEL.to_owned(),
        image_describe_cache: Arc::new(crate::session::image_describe::ImageDescribeCache::new()),
        subagent_token_records: parking_lot::Mutex::new(std::collections::HashMap::new()),
        workspace_ops: echo_agent_workspace::WorkspaceOps::for_test(),
        trace_config_template: std::cell::RefCell::new(None),
    }
}
/// Test check_auto_compact_needed uses state values.
#[tokio::test(flavor = "current_thread")]
async fn test_check_auto_compact_needed_uses_state() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(90_000, 100_000, 85, gateway_tx, persistence_tx).await;
            let result = actor.check_auto_compact_needed().await;
            assert!(result.is_some(), "Should trigger at 90%");
            let info = result.unwrap();
            assert_eq!(info.percentage, 90);
        })
        .await;
}
/// Test that overriding context_window on the sampling config changes
/// auto-compact behavior. Forked sessions must use the new model's
/// context window, not the source session's. Without this, auto-compact
/// fires at the wrong threshold.
#[tokio::test(flavor = "current_thread")]
async fn test_context_window_override_affects_auto_compact() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(86_000, 100_000, 85, gateway_tx, persistence_tx).await;
            let result = actor.check_auto_compact_needed().await;
            assert!(result.is_some(), "Should trigger at 86% of 100K window");
            if let Some(mut cfg) = actor.chat_state_handle.get_sampling_config().await {
                cfg.model = "larger-model".to_string();
                cfg.context_window = std::num::NonZeroU64::new(200_000).unwrap();
                actor.chat_state_handle.update_sampling_config(cfg);
            }
            let result = actor.check_auto_compact_needed().await;
            assert!(
                result.is_none(),
                "Should NOT trigger at 43% of 200K window after context_window override"
            );
        })
        .await;
}
/// Test the reverse direction: overriding to a smaller context window
/// should make auto-compact trigger sooner.
#[tokio::test(flavor = "current_thread")]
async fn test_context_window_override_to_smaller_triggers_compact() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(86_000, 200_000, 85, gateway_tx, persistence_tx).await;
            let result = actor.check_auto_compact_needed().await;
            assert!(result.is_none(), "Should NOT trigger at 43% of 200K window");
            if let Some(mut cfg) = actor.chat_state_handle.get_sampling_config().await {
                cfg.model = "smaller-model".to_string();
                cfg.context_window = std::num::NonZeroU64::new(100_000).unwrap();
                actor.chat_state_handle.update_sampling_config(cfg);
            }
            let result = actor.check_auto_compact_needed().await;
            assert!(
                result.is_some(),
                "Should trigger at 86% of 100K window after context_window override"
            );
        })
        .await;
}
/// Suppression gates both AUTO paths; the reset scope depends on the reason:
/// `other` clears next turn, `credit_block` holds until a successful model call,
/// `size` is sticky until a full reset (success / rewind / model switch).
#[tokio::test(flavor = "current_thread")]
async fn suppression_gates_and_reset_is_reason_scoped() {
    use crate::session::compaction_config::{SUPPRESS_NONE, SUPPRESS_TURN, SUPPRESS_UNTIL_SUCCESS};
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let actor = create_test_actor(214_000, 200_000, 85, gateway_tx, persistence_tx).await;
            let err = api_error_with_context_window(200_000);
            assert!(actor.check_auto_compact_needed().await.is_some());
            assert!(actor.should_compact_on_error(&err).await);
            actor
                .suppress_auto_compaction(SuppressReason::Other, 1_000, 200_000)
                .await;
            assert!(actor.check_auto_compact_needed().await.is_none());
            assert!(!actor.should_compact_on_error(&err).await);
            let _ = actor.compaction.auto_compact_suppressed.compare_exchange(
                SUPPRESS_TURN,
                SUPPRESS_NONE,
                Relaxed,
                Relaxed,
            );
            assert!(actor.check_auto_compact_needed().await.is_some());
            actor
                .suppress_auto_compaction(SuppressReason::CreditBlock, 1_000, 200_000)
                .await;
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_UNTIL_SUCCESS
            );
            assert!(actor.check_auto_compact_needed().await.is_none());
            assert!(!actor.should_compact_on_error(&err).await);
            let _ = actor.compaction.auto_compact_suppressed.compare_exchange(
                SUPPRESS_TURN,
                SUPPRESS_NONE,
                Relaxed,
                Relaxed,
            );
            assert!(
                actor.check_auto_compact_needed().await.is_none(),
                "credit-block suppression must survive the per-turn reset"
            );
            let _ = actor.compaction.auto_compact_suppressed.compare_exchange(
                SUPPRESS_UNTIL_SUCCESS,
                SUPPRESS_NONE,
                Relaxed,
                Relaxed,
            );
            assert!(actor.check_auto_compact_needed().await.is_some());
            actor
                .suppress_auto_compaction(SuppressReason::Size, 1_000, 200_000)
                .await;
            assert!(actor.check_auto_compact_needed().await.is_none());
            let _ = actor.compaction.auto_compact_suppressed.compare_exchange(
                SUPPRESS_TURN,
                SUPPRESS_NONE,
                Relaxed,
                Relaxed,
            );
            assert!(
                actor.check_auto_compact_needed().await.is_none(),
                "sticky suppression must survive the per-turn reset"
            );
            actor
                .compaction
                .auto_compact_suppressed
                .store(SUPPRESS_NONE, Relaxed);
            assert!(actor.check_auto_compact_needed().await.is_some());
        })
        .await;
}
/// A model switch clears suppression the switch (or the fresh budget-driven
/// trigger) can resolve — sticky size/schema and a stale per-turn `other` — so
/// the gates re-evaluate against the new window. Account-state credit/auth is
/// covered by `model_switch_keeps_account_state_suppression`.
#[tokio::test(flavor = "current_thread")]
async fn model_switch_clears_sticky_suppression() {
    use crate::session::compaction_config::{PreviousModelInfo, SUPPRESS_NONE};
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let actor =
                Arc::new(create_test_actor(50_000, 200_000, 85, gateway_tx, persistence_tx).await);
            for reason in [SuppressReason::Size, SuppressReason::Other] {
                actor.suppress_auto_compaction(reason, 1_000, 200_000).await;
                assert_ne!(
                    actor.compaction.auto_compact_suppressed.load(Relaxed),
                    SUPPRESS_NONE,
                    "{reason:?} should set suppression"
                );
                actor.compaction.previous_model.set(Some(PreviousModelInfo {
                    model_slug: "old-small-model".to_string(),
                    context_window: 100_000,
                }));
                actor
                    .maybe_compact_on_model_switch()
                    .await
                    .expect("non-auth model-switch path must not abort");
                assert_eq!(
                    actor.compaction.auto_compact_suppressed.load(Relaxed),
                    SUPPRESS_NONE,
                    "model switch must clear {reason:?} suppression so the gates re-evaluate"
                );
            }
        })
        .await;
}
/// Model switch must not clear credit/auth suppress or compact under it.
#[tokio::test(flavor = "current_thread")]
async fn model_switch_keeps_account_state_suppression() {
    use crate::session::compaction_config::{
        PreviousModelInfo, SUPPRESS_AUTH, SUPPRESS_UNTIL_SUCCESS,
    };
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let actor =
                Arc::new(create_test_actor(214_000, 200_000, 85, gateway_tx, persistence_tx).await);
            for (reason, expected) in [
                (SuppressReason::CreditBlock, SUPPRESS_UNTIL_SUCCESS),
                (SuppressReason::Auth, SUPPRESS_AUTH),
            ] {
                actor.suppress_auto_compaction(reason, 1_000, 200_000).await;
                assert_eq!(
                    actor.compaction.auto_compact_suppressed.load(Relaxed),
                    expected,
                    "{reason:?} suppress state"
                );
                actor.compaction.previous_model.set(Some(PreviousModelInfo {
                    model_slug: "old-big-model".to_string(),
                    context_window: 400_000,
                }));
                actor
                    .maybe_compact_on_model_switch()
                    .await
                    .expect("suppressed model-switch path must not abort");
                assert_eq!(
                    actor.compaction.auto_compact_suppressed.load(Relaxed),
                    expected,
                    "model switch must NOT clear {reason:?} suppression"
                );
                actor
                    .compaction
                    .auto_compact_suppressed
                    .store(crate::session::compaction_config::SUPPRESS_NONE, Relaxed);
            }
        })
        .await;
}
/// Auth suppress clears on credential recovery, not on a model 200.
#[tokio::test(flavor = "current_thread")]
async fn auth_suppress_clears_on_credential_recovery() {
    use crate::session::compaction_config::{SUPPRESS_AUTH, SUPPRESS_NONE};
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let actor = create_test_actor(180_000, 200_000, 85, gateway_tx, persistence_tx).await;
            actor
                .suppress_auto_compaction(SuppressReason::Auth, 1_000, 200_000)
                .await;
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_AUTH
            );
            assert!(actor.check_auto_compact_needed().await.is_none());
            actor.clear_auth_compact_suppression();
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_NONE
            );
            assert!(actor.check_auto_compact_needed().await.is_some());
        })
        .await;
}
/// Auth recovery must not clear credit suppress.
#[tokio::test(flavor = "current_thread")]
async fn clear_auth_suppress_leaves_credit_suppress() {
    use crate::session::compaction_config::SUPPRESS_UNTIL_SUCCESS;
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let actor = create_test_actor(180_000, 200_000, 85, gateway_tx, persistence_tx).await;
            actor
                .suppress_auto_compaction(SuppressReason::CreditBlock, 1_000, 200_000)
                .await;
            actor.clear_auth_compact_suppression();
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_UNTIL_SUCCESS,
                "credential recovery must not clear a credit-block suppress"
            );
        })
        .await;
}
/// After /login, clearing auth suppress must re-arm pre-sampling compact
/// before the next sample (ordering that prepare_sampler-after-gate broke).
#[tokio::test(flavor = "current_thread")]
async fn clear_auth_suppress_rearms_pre_sampling_compact_gate() {
    use crate::session::compaction_config::SUPPRESS_AUTH;
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let actor = create_test_actor(180_000, 200_000, 85, gateway_tx, persistence_tx).await;
            actor
                .suppress_auto_compaction(SuppressReason::Auth, 1_000, 200_000)
                .await;
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_AUTH
            );
            assert!(
                actor.check_auto_compact_needed().await.is_none(),
                "auth suppress must block pre-sampling compact"
            );
            actor.clear_auth_compact_suppression();
            assert!(
                actor.check_auto_compact_needed().await.is_some(),
                "after credential recovery, pre-sampling compact must re-arm"
            );
        })
        .await;
}
#[test]
fn is_auth_compact_error_classifies_401_messages() {
    let auth =
        acp::Error::internal_error().data("compact failed: API error (status 401 Unauthorized)");
    assert!(SessionActor::is_auth_compact_error(&auth));
    let credit = acp::Error::internal_error().data("compact failed: out of credits");
    assert!(!SessionActor::is_auth_compact_error(&credit));
    let size = acp::Error::internal_error()
        .data("compact failed: The prompt is too long for this model's context window.");
    assert!(!SessionActor::is_auth_compact_error(&size));
}
#[tokio::test(flavor = "current_thread")]
async fn surface_compact_auth_failure_emits_reauthable_retry_state() {
    use crate::extensions::notification::SessionUpdate as EchoAgentSessionUpdate;
    use crate::session::storage::SessionUpdate;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel();
            let actor = create_test_actor(10_000, 200_000, 85, gateway_tx, persistence_tx).await;
            let err = acp::Error::internal_error()
                .data("compact failed: API error (status 401 Unauthorized)");
            let out = actor.surface_compact_auth_failure(err).await;
            assert_eq!(out.code, acp::Error::auth_required().code);
            let mut saw_retry_auth = false;
            while let Ok(msg) = persistence_rx.try_recv() {
                if let PersistenceMsg::Update(SessionUpdate::EchoAgent(notif)) = msg
                    && let EchoAgentSessionUpdate::RetryState(
                        crate::extensions::notification::RetryState::Failed {
                            error_type,
                            message,
                        },
                    ) = &notif.update
                {
                    assert_eq!(error_type, "auth");
                    assert!(
                        message.contains("Unauthorized (401)") || message.contains("401"),
                        "message={message}"
                    );
                    saw_retry_auth = true;
                }
            }
            assert!(
                saw_retry_auth,
                "expected RetryState::Failed auth notification"
            );
        })
        .await;
}
/// The per-turn suppression notification is tailored to the failure reason.
#[tokio::test(flavor = "current_thread")]
async fn suppression_notification_is_reason_specific() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            async fn notification_for(reason: SuppressReason) -> String {
                let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
                let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel();
                let actor =
                    create_test_actor(10_000, 200_000, 85, gateway_tx, persistence_tx).await;
                actor.suppress_auto_compaction(reason, 1_000, 200_000).await;
                let mut text = None;
                while let Ok(msg) = persistence_rx.try_recv() {
                    if let PersistenceMsg::Update(
                        crate::session::storage::SessionUpdate::EchoAgent(notif),
                    ) = msg
                        && let crate::extensions::notification::SessionUpdate::AutoCompactFailed {
                            error,
                        } = &notif.update
                    {
                        text = Some(error.clone());
                    }
                }
                text.expect("expected an AutoCompactFailed notification")
            }
            let credit = notification_for(SuppressReason::CreditBlock).await;
            assert!(credit.contains("spending limit"), "credit_block: {credit}");
            let auth = notification_for(SuppressReason::Auth).await;
            assert!(auth.contains("/login"), "auth: {auth}");
            let size = notification_for(SuppressReason::Size).await;
            assert!(size.contains("too large to compact"), "size: {size}");
            let schema = notification_for(SuppressReason::Schema).await;
            assert!(schema.contains("can't be summarized"), "schema: {schema}");
            let other = notification_for(SuppressReason::Other).await;
            assert!(other.contains("/new"), "other: {other}");
        })
        .await;
}
/// Mock LLM endpoint answering every request with a deterministic 400.
async fn spawn_deterministic_400_server() -> String {
    spawn_status_body_server(
        400,
        r#"{"error":{"type":"invalid_request_error","message":"bad schema"}}"#,
    )
    .await
}
/// Mock LLM that answers every request with 401.
async fn spawn_deterministic_401_server() -> String {
    spawn_status_body_server(
        401,
        r#"{"error":{"type":"authentication_error","message":"Unauthorized (401)"}}"#,
    )
    .await
}
async fn spawn_status_body_server(status: u16, body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let status_line = match status {
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        other => panic!("add status line for {other}"),
    };
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}")
}
fn switch_target_config(model: &str, base_url: String) -> echo_agent_sampler::SamplerConfig {
    echo_agent_sampler::SamplerConfig {
        api_key: Some("test-key".to_string()),
        base_url,
        model: model.to_string(),
        context_window: 256_000,
        api_backend: crate::sampling::ApiBackend::Responses,
        ..Default::default()
    }
}
/// Family switch → compact with the new model over the lossy view: the
/// request must contain nothing but plain `{role, content}` text messages.
#[tokio::test(flavor = "current_thread")]
async fn family_switch_compacts_lossy_with_new_model() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let actor =
                Arc::new(create_test_actor(10_000, 200_000, 85, gateway_tx, persistence_tx).await);
            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("sys"),
                ConversationItem::user("hello"),
                ConversationItem::Reasoning(echo_agent_sampling_types::rs::ReasoningItem {
                    id: "tco_res-uuid_call-uuid-0".to_string(),
                    summary: vec![],
                    content: None,
                    encrypted_content: Some("tco_SEALEDCIPHERTEXT".to_string()),
                    status: None,
                }),
                ConversationItem::assistant_tool_calls(vec![echo_agent_sampling_types::ToolCall {
                    id: std::sync::Arc::<str>::from("call_echo_agent_minted_id"),
                    name: "run_terminal_command".to_string(),
                    arguments: std::sync::Arc::<str>::from(r#"{"command":"ls"}"#),
                }]),
                ConversationItem::ToolResult(echo_agent_sampling_types::ToolResultItem {
                    tool_call_id: "call_echo_agent_minted_id".to_string(),
                    content: std::sync::Arc::<str>::from("file listing"),
                    images: Vec::new(),
                }),
                ConversationItem::assistant("done"),
            ]);
            let server = echo_agent_test_support::MockInferenceServer::start()
                .await
                .expect("mock inference server");
            actor
                .handle_set_session_model(
                    switch_target_config("new-model", server.url()),
                    false,
                    true,
                    false,
                    true,
                    85,
                )
                .await
                .expect("compact failure is log-only; the switch must succeed");
            let requests = server.requests();
            assert!(
                !requests.is_empty(),
                "family switch must fire a compaction sample"
            );
            let body = requests[0].body.as_ref().unwrap();
            assert_eq!(
                body["model"], "new-model",
                "summarizer must be the NEW model"
            );
            for message in body["input"].as_array().unwrap() {
                let keys: Vec<&String> = message.as_object().unwrap().keys().collect();
                assert!(
                    keys.iter()
                        .all(|k| *k == "type" || *k == "role" || *k == "content"),
                    "lossy view must send plain text messages, got keys {keys:?} in {message}"
                );
                assert_eq!(message["type"], "message", "non-message item: {message}");
                assert!(
                    message["content"].is_string(),
                    "non-text content in {message}"
                );
            }
        })
        .await;
}
/// 401 auto-compact: SUPPRESS_AUTH + reauthable RetryState (abort for /login).
#[tokio::test(flavor = "current_thread")]
async fn e2e_auto_compact_401_suppresses_auth_and_surfaces_reauth() {
    use crate::extensions::notification::SessionUpdate as EchoAgentSessionUpdate;
    use crate::session::compaction_config::SUPPRESS_AUTH;
    use crate::session::storage::SessionUpdate;
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel();
            let actor =
                Arc::new(create_test_actor(180_000, 200_000, 85, gateway_tx, persistence_tx).await);
            let base_url = spawn_deterministic_401_server().await;
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = base_url;
            actor.chat_state_handle.update_sampling_config(cfg);
            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("sys"),
                ConversationItem::user("hello"),
                ConversationItem::assistant("hi"),
                ConversationItem::user("compact me"),
            ]);
            let err = actor
                .run_compact_only(
                    AutoCompactTriggerInfo {
                        tokens_used: 180_000,
                        context_window: 200_000,
                        percentage: 90,
                    },
                    false,
                )
                .await
                .expect_err("401 mock must fail auto-compact");
            assert!(
                SessionActor::is_auth_compact_error(&err),
                "401 compact failure must classify as auth: {err:?}"
            );
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_AUTH,
                "auth compact failure must use SUPPRESS_AUTH (cleared on re-login)"
            );
            let surfaced = actor.surface_compact_auth_failure(err).await;
            assert_eq!(surfaced.code, acp::Error::auth_required().code);
            let mut saw_retry_auth = false;
            let mut saw_auto_failed = false;
            while let Ok(msg) = persistence_rx.try_recv() {
                if let PersistenceMsg::Update(SessionUpdate::EchoAgent(notif)) = msg {
                    match &notif.update {
                        EchoAgentSessionUpdate::RetryState(
                            crate::extensions::notification::RetryState::Failed {
                                error_type,
                                message,
                            },
                        ) => {
                            assert_eq!(error_type, "auth");
                            assert!(
                                message.contains("Unauthorized") || message.contains("401"),
                                "message={message}"
                            );
                            saw_retry_auth = true;
                        }
                        EchoAgentSessionUpdate::AutoCompactFailed { error } => {
                            assert!(
                                error.contains("/login") || error.contains("authentication"),
                                "auto-failed={error}"
                            );
                            saw_auto_failed = true;
                        }
                        _ => {}
                    }
                }
            }
            assert!(saw_auto_failed, "expected AutoCompactFailed notification");
            assert!(
                saw_retry_auth,
                "expected RetryState::Failed auth so pager can stash + reauth"
            );
            actor.clear_auth_compact_suppression();
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                crate::session::compaction_config::SUPPRESS_NONE
            );
        })
        .await;
}
/// Model-switch compact 401 must surface reauth (same path as pre-sampling).
#[tokio::test(flavor = "current_thread")]
async fn e2e_model_switch_compact_401_surfaces_reauth() {
    use crate::extensions::notification::SessionUpdate as EchoAgentSessionUpdate;
    use crate::session::compaction_config::{PreviousModelInfo, SUPPRESS_AUTH};
    use crate::session::storage::SessionUpdate;
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel();
            let actor =
                Arc::new(create_test_actor(214_000, 200_000, 85, gateway_tx, persistence_tx).await);
            let base_url = spawn_deterministic_401_server().await;
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = base_url;
            actor.chat_state_handle.update_sampling_config(cfg);
            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("sys"),
                ConversationItem::user("hello"),
                ConversationItem::assistant("hi"),
                ConversationItem::user("compact me"),
            ]);
            actor.chat_state_handle.record_token_usage(214_000);
            actor.compaction.previous_model.set(Some(PreviousModelInfo {
                model_slug: "old-big-model".to_string(),
                context_window: 400_000,
            }));
            let err = actor
                .maybe_compact_on_model_switch()
                .await
                .expect_err("model-switch 401 compact must abort for reauth");
            assert_eq!(err.code, acp::Error::auth_required().code);
            assert!(
                SessionActor::is_auth_compact_error(&err)
                    || err.message.to_ascii_lowercase().contains("unauthorized")
                    || format!("{err:?}").contains("401"),
                "surfaced error should be reauthable auth: {err:?}"
            );
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_AUTH,
                "auth compact failure must use SUPPRESS_AUTH"
            );
            let mut saw_retry_auth = false;
            while let Ok(msg) = persistence_rx.try_recv() {
                if let PersistenceMsg::Update(SessionUpdate::EchoAgent(notif)) = msg
                    && let EchoAgentSessionUpdate::RetryState(
                        crate::extensions::notification::RetryState::Failed {
                            error_type,
                            message,
                        },
                    ) = &notif.update
                {
                    assert_eq!(error_type, "auth");
                    assert!(
                        message.contains("Unauthorized") || message.contains("401"),
                        "message={message}"
                    );
                    saw_retry_auth = true;
                }
            }
            assert!(
                saw_retry_auth,
                "expected RetryState::Failed auth so pager can stash + reauth"
            );
        })
        .await;
}
/// Non-auth model-switch compact failures stay log-only (turn continues).
#[tokio::test(flavor = "current_thread")]
async fn e2e_model_switch_compact_non_auth_failure_does_not_abort() {
    use crate::session::compaction_config::{PreviousModelInfo, SUPPRESS_NONE};
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let actor =
                Arc::new(create_test_actor(214_000, 200_000, 85, gateway_tx, persistence_tx).await);
            let base_url = spawn_deterministic_400_server().await;
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = base_url;
            actor.chat_state_handle.update_sampling_config(cfg);
            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("sys"),
                ConversationItem::user("hello"),
            ]);
            actor.chat_state_handle.record_token_usage(214_000);
            actor.compaction.previous_model.set(Some(PreviousModelInfo {
                model_slug: "old-big-model".to_string(),
                context_window: 400_000,
            }));
            actor
                .maybe_compact_on_model_switch()
                .await
                .expect("non-auth model-switch compact failure must not abort the turn");
            assert_ne!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_NONE,
                "schema/other compact failure must suppress after attempt"
            );
        })
        .await;
}
/// After clearing auth suppress, a shrink switch can re-evaluate and compact.
#[tokio::test(flavor = "current_thread")]
async fn clear_auth_suppress_allows_model_switch_compact_reeval() {
    use crate::session::compaction_config::{PreviousModelInfo, SUPPRESS_AUTH, SUPPRESS_NONE};
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let actor =
                Arc::new(create_test_actor(214_000, 200_000, 85, gateway_tx, persistence_tx).await);
            actor
                .suppress_auto_compaction(SuppressReason::Auth, 1_000, 200_000)
                .await;
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_AUTH
            );
            actor.compaction.previous_model.set(Some(PreviousModelInfo {
                model_slug: "old-big-model".to_string(),
                context_window: 400_000,
            }));
            actor
                .maybe_compact_on_model_switch()
                .await
                .expect("suppressed switch must not abort");
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_AUTH
            );
            actor.clear_auth_compact_suppression();
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_NONE
            );
            actor.compaction.previous_model.set(Some(PreviousModelInfo {
                model_slug: "old-big-model".to_string(),
                context_window: 400_000,
            }));
            let base_url = spawn_deterministic_400_server().await;
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = base_url;
            actor.chat_state_handle.update_sampling_config(cfg);
            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("sys"),
                ConversationItem::user("hello"),
            ]);
            actor.chat_state_handle.record_token_usage(214_000);
            actor
                .maybe_compact_on_model_switch()
                .await
                .expect("post-clear switch compact re-eval must not abort on non-auth");
            assert_ne!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_NONE,
                "post-clear switch must re-evaluate and attempt compact"
            );
        })
        .await;
}
/// A deterministic failure suppresses auto-compaction only on the AUTO
/// path — never for a bare manual `/compact`.
#[tokio::test(flavor = "current_thread")]
async fn bare_manual_compact_failure_does_not_suppress_auto() {
    use crate::session::compaction_config::SUPPRESS_NONE;
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let actor =
                Arc::new(create_test_actor(50_000, 200_000, 85, gateway_tx, persistence_tx).await);
            let base_url = spawn_deterministic_400_server().await;
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = base_url;
            actor.chat_state_handle.update_sampling_config(cfg);
            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("sys"),
                ConversationItem::user("hello"),
            ]);
            let result = actor.run_compact(None).await;
            assert!(result.is_err(), "mock 400 must fail the compaction");
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_NONE,
                "manual /compact (even without args) must never set auto-compact suppression"
            );
            let result = actor
                .run_compact_only(
                    AutoCompactTriggerInfo {
                        tokens_used: 180_000,
                        context_window: 200_000,
                        percentage: 90,
                    },
                    false,
                )
                .await;
            assert!(result.is_err(), "mock 400 must fail the compaction");
            assert_ne!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_NONE,
                "the same deterministic failure on the AUTO path must suppress"
            );
        })
        .await;
}
/// A forked session whose whole-transcript inherited prefix alone exceeds
/// the auto-compact threshold releases the prefix on compaction (so the
/// conversation can actually shrink below the threshold) and keeps the
/// release sticky across further compactions (no unbounded compaction loop).
#[tokio::test(flavor = "current_thread")]
async fn forked_prefix_released_under_pressure_and_stays_released() {
    use crate::session::compaction_config::SUPPRESS_NONE;
    use echo_agent_test_support::MockInferenceServer;
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
            let filler = "x".repeat(8_000);
            let mut conv = vec![ConversationItem::system("small system prompt")];
            for i in 0..9 {
                conv.push(ConversationItem::user(format!("u{i} {filler}")));
                conv.push(ConversationItem::assistant(format!("a{i} {filler}")));
            }
            conv.push(ConversationItem::user("final query"));
            let prefix_len = conv.len();
            let mut actor = create_test_actor(0, 40_000, 80, gateway_tx, persistence_tx).await;
            actor.startup_hints.inherited_prefix_len = Some(prefix_len);
            let actor = Arc::new(actor);
            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("Summary of prior work. ".repeat(30));
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            actor.chat_state_handle.update_sampling_config(cfg);
            actor.chat_state_handle.replace_conversation(conv);
            let threshold_tokens = 40_000u64 * 80 / 100;
            let before = actor.chat_state_handle.get_total_tokens().await;
            assert!(
                before > threshold_tokens,
                "seed must exceed threshold: {before} <= {threshold_tokens}"
            );
            let result = actor.run_compact(None).await;
            assert!(result.is_ok(), "compaction should succeed: {result:?}");
            assert!(
                actor.compaction.prefix_released.load(Relaxed),
                "prefix must be released under pressure"
            );
            let after = actor.chat_state_handle.get_total_tokens().await;
            assert!(
                after < threshold_tokens,
                "released history must drop below threshold: {after} >= {threshold_tokens}"
            );
            assert!(
                actor.chat_state_handle.get_conversation_len().await < prefix_len,
                "conversation must shrink below the pinned prefix floor"
            );
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_NONE,
                "a shrunk conversation must not suppress AUTO"
            );
            let result = actor.run_compact(None).await;
            assert!(
                result.is_ok(),
                "second compaction should succeed: {result:?}"
            );
            assert!(
                actor.compaction.prefix_released.load(Relaxed),
                "release must stay sticky across compactions"
            );
            let after2 = actor.chat_state_handle.get_total_tokens().await;
            assert!(
                after2 < threshold_tokens,
                "sticky release must keep the session under threshold: {after2}"
            );
        })
        .await;
}
/// When even the released (summarized) history still exceeds the threshold
/// -- the pathological case where the system prompt alone is over budget --
/// a forked session sets sticky suppression (WITHOUT a user-facing failure
/// event) instead of clearing it, so AUTO is not immediately re-armed while the
/// compaction itself still reports success.
#[tokio::test(flavor = "current_thread")]
async fn forked_release_still_over_threshold_suppresses_auto() {
    use crate::session::compaction_config::SUPPRESS_STICKY;
    use echo_agent_test_support::MockInferenceServer;
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel();
            let huge_system = "s".repeat(150_000);
            let conv = vec![
                ConversationItem::system(huge_system),
                ConversationItem::user("q"),
                ConversationItem::assistant("a"),
                ConversationItem::user("final query"),
            ];
            let prefix_len = conv.len();
            let mut actor = create_test_actor(0, 40_000, 80, gateway_tx, persistence_tx).await;
            actor.startup_hints.inherited_prefix_len = Some(prefix_len);
            let actor = Arc::new(actor);
            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("Summary. ".repeat(70));
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            actor.chat_state_handle.update_sampling_config(cfg);
            actor.chat_state_handle.replace_conversation(conv);
            let threshold_tokens = 40_000u64 * 80 / 100;
            let before = actor.chat_state_handle.get_total_tokens().await;
            assert!(
                before > threshold_tokens,
                "seed must exceed threshold: {before}"
            );
            let result = actor.run_compact(None).await;
            assert!(result.is_ok(), "compaction should succeed: {result:?}");
            assert!(
                actor.compaction.prefix_released.load(Relaxed),
                "prefix must be released under pressure"
            );
            assert_eq!(
                actor.compaction.auto_compact_suppressed.load(Relaxed),
                SUPPRESS_STICKY,
                "an over-threshold released history must set sticky suppression"
            );
            let mut saw_failure = false;
            while let Ok(msg) = persistence_rx.try_recv() {
                if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(
                    notif,
                )) = msg
                    && matches!(
                        &notif.update,
                        crate::extensions::notification::SessionUpdate::AutoCompactFailed { .. }
                    )
                {
                    saw_failure = true;
                }
            }
            assert!(
                !saw_failure,
                "successful compaction must not emit AutoCompactFailed"
            );
        })
        .await;
}
/// `classify_suppress_reason` maps each deterministic-failure shape to its
/// fixed [`SuppressReason`].
#[test]
fn classify_suppress_reason_maps_error_text() {
    let classify = SessionActor::classify_suppress_reason;
    assert_eq!(
        classify("caller does not have permission … spending-limit reached"),
        SuppressReason::CreditBlock
    );
    assert_eq!(
        classify("you have run out of credits"),
        SuppressReason::CreditBlock
    );
    assert_eq!(
        classify(
            "API error (status 402 Payment Required): EchoAgent Build usage balance exhausted"
        ),
        SuppressReason::CreditBlock
    );
    assert_eq!(
        classify("EchoAgent Build usage limit reached"),
        SuppressReason::CreditBlock
    );
    assert_eq!(
        classify("This model's maximum prompt length is 500000"),
        SuppressReason::Size
    );
    assert_eq!(
        classify("compact failed: The prompt is too long for this model's context window."),
        SuppressReason::Size
    );
    assert_eq!(
        classify("provider error: context_length_exceeded"),
        SuppressReason::Size
    );
    assert_eq!(
        classify("API error (status 401 Unauthorized)"),
        SuppressReason::Auth
    );
    assert_eq!(
        classify("provider returned invalid_request_error: messages.3"),
        SuppressReason::Schema
    );
    assert_eq!(
        classify("upstream 500 internal error"),
        SuppressReason::Other
    );
}
/// `SuppressReason::as_str` is the stable telemetry wire value — BQ/OTLP and
/// dashboards key off these exact strings. Lock them so a rename can't break monitoring.
#[test]
fn suppress_reason_as_str_is_stable() {
    assert_eq!(SuppressReason::CreditBlock.as_str(), "credit_block");
    assert_eq!(SuppressReason::Size.as_str(), "size");
    assert_eq!(SuppressReason::Auth.as_str(), "auth");
    assert_eq!(SuppressReason::Schema.as_str(), "schema");
    assert_eq!(SuppressReason::Other.as_str(), "other");
}
mod preserve_prefix {
    use super::super::preserve_inherited_prefix;
    use super::super::project_preserved_reseed_tokens;
    use echo_agent_sampling_types::conversation::ConversationItem;
    #[test]
    fn splices_inherited_with_compacted_suffix() {
        let conversation = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("parent q1"),
            ConversationItem::assistant("parent a1"),
            ConversationItem::user("child q1"),
        ];
        let compacted = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("summary"),
        ];
        let items = preserve_inherited_prefix(&conversation, compacted, 3).expect("Ok");
        assert_eq!(items.len(), 4);
        assert!(matches!(items[0], ConversationItem::System(_)));
    }
    /// Invariant: a head-only prefix lets compaction shrink the conversation;
    /// a whole-transcript prefix does not (that pinned floor is the loop).
    #[test]
    fn head_only_shrinks_full_transcript_does_not() {
        let mut conversation = vec![ConversationItem::system("sys")];
        for i in 0..8 {
            conversation.push(ConversationItem::user(format!("u{i}")));
            conversation.push(ConversationItem::assistant(format!("a{i}")));
        }
        let compacted = vec![
            ConversationItem::system("sys"),
            ConversationItem::assistant("summary"),
        ];
        let fixed = preserve_inherited_prefix(&conversation, compacted.clone(), 1).expect("Ok");
        assert!(fixed.len() < conversation.len(), "head-only shrinks");
        let buggy =
            preserve_inherited_prefix(&conversation, compacted, conversation.len()).expect("Ok");
        assert!(
            buggy.len() >= conversation.len(),
            "full prefix never shrinks"
        );
    }
    /// The reseed projection calibrates the bytes/4 estimate to real tokens
    /// (ratio != 1) and caps at the pre-compaction total, so the release
    /// decision reflects what the trigger applies next turn.
    #[test]
    fn project_preserved_reseed_tokens_calibrates_and_caps() {
        assert_eq!(
            project_preserved_reseed_tokens(30_000, 100_000, 50_000),
            60_000
        );
        assert_eq!(
            project_preserved_reseed_tokens(40_000, 70_000, 35_000),
            70_000
        );
        assert_eq!(
            project_preserved_reseed_tokens(20_000, 40_000, 40_000),
            20_000
        );
        assert_eq!(project_preserved_reseed_tokens(10, 5, 0), 5);
    }
    /// Both prefix and re-injected suffix may carry AGENTS.md; the splice must
    /// leave exactly one (else the model sees project instructions twice).
    #[test]
    fn does_not_duplicate_agents_md() {
        let conversation = vec![
            ConversationItem::system("sys"),
            ConversationItem::project_instructions("AGENTS.md"),
            ConversationItem::user("work"),
        ];
        let compacted = vec![
            ConversationItem::system("sys"),
            ConversationItem::project_instructions("AGENTS.md"),
            ConversationItem::user("summary"),
        ];
        let items = preserve_inherited_prefix(&conversation, compacted, 2).expect("Ok");
        let pi = items
            .iter()
            .filter(|i| super::super::is_project_instructions(i))
            .count();
        assert_eq!(pi, 1, "exactly one project-instructions item, not two");
    }
    #[test]
    fn keeps_reinjected_agents_md_when_prefix_lacks_it() {
        let conversation = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("work"),
        ];
        let compacted = vec![
            ConversationItem::system("sys"),
            ConversationItem::project_instructions("AGENTS.md"),
            ConversationItem::user("summary"),
        ];
        let items = preserve_inherited_prefix(&conversation, compacted, 1).expect("Ok");
        let pi = items
            .iter()
            .filter(|i| super::super::is_project_instructions(i))
            .count();
        assert_eq!(
            pi, 1,
            "re-injected AGENTS.md preserved when prefix lacks one"
        );
    }
}
#[allow(clippy::field_reassign_with_default)]
async fn create_test_actor_with_memory(
    total_tokens: u64,
    context_window: u64,
    threshold_percent: u8,
    gateway_tx: mpsc::UnboundedSender<echo_agent_acp::AcpClientMessage>,
    persistence_tx: mpsc::UnboundedSender<PersistenceMsg>,
    memory_config: Option<crate::config::MemoryConfig>,
) -> SessionActor {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd_path = tmp.path().to_path_buf();
    let memory_storage = memory_config
        .as_ref()
        .filter(|mc| mc.enabled)
        .map(|_| crate::session::memory::MemoryStorage::new(&cwd_path, None));
    std::mem::forget(tmp);
    let memory_initial_injection_config = memory_config
        .as_ref()
        .map_or_else(Default::default, |mc| mc.initial_injection.clone());
    let mut actor = create_test_actor(
        total_tokens,
        context_window,
        threshold_percent,
        gateway_tx,
        persistence_tx,
    )
    .await;
    actor.memory = crate::session::memory_state::SessionMemory {
        jobs: Default::default(),
        flush_config: memory_config
            .as_ref()
            .map_or_else(Default::default, |mc| mc.flush.clone()),
        is_flushing: std::sync::atomic::AtomicBool::new(false),
        last_flush_compaction: std::sync::atomic::AtomicU64::new(0),
        storage: std::cell::RefCell::new(memory_storage),
        save_on_end: true,
        backend_params: None,
        initial_injection_config: memory_initial_injection_config,
        context_injected: std::sync::atomic::AtomicBool::new(false),
        cached_context: Default::default(),
        flush_count: std::sync::atomic::AtomicU64::new(0),
        last_flush_content: std::cell::RefCell::new(None),
        flush_success_count: std::sync::atomic::AtomicU64::new(0),
        flush_error_count: std::sync::atomic::AtomicU64::new(0),
        search_counter: std::cell::RefCell::new(None),
        injection_count: std::sync::atomic::AtomicU64::new(0),
        compaction_recovery_count: std::sync::atomic::AtomicU64::new(0),
        chunks_added: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        dream_config: Default::default(),
        dream_count: std::sync::atomic::AtomicU64::new(0),
        dream_success_count: std::sync::atomic::AtomicU64::new(0),
        dream_error_count: std::sync::atomic::AtomicU64::new(0),
    };
    actor.idle_flush_timeout = memory_config
        .as_ref()
        .and_then(|mc| mc.flush.idle_timeout_secs)
        .map(std::time::Duration::from_secs);
    actor.dream_check_timeout = memory_config
        .as_ref()
        .filter(|mc| mc.dream.enabled)
        .and_then(|mc| mc.dream.check_interval_secs)
        .filter(|&s| s > 0)
        .map(std::time::Duration::from_secs);
    actor
}
/// Verify that `last_idle_flush_conversation_len` is reset after
/// compaction shrinks the conversation. Without this reset the
/// interval flush guard (`current_len > last_len`) stays false
/// because the compacted conversation is shorter than the stored
/// pre-compaction length.
#[tokio::test(flavor = "current_thread")]
#[allow(clippy::field_reassign_with_default)]
async fn test_idle_flush_conversation_len_reset_after_compaction() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel();
            let (persistence_tx, _) = mpsc::unbounded_channel();
            let mut config = crate::config::MemoryConfig::default();
            config.enabled = true;
            config.flush.idle_timeout_secs = Some(60);
            let actor = create_test_actor_with_memory(
                50_000,
                100_000,
                85,
                gateway_tx,
                persistence_tx,
                Some(config),
            )
            .await;
            for _ in 0..80 {
                actor
                    .chat_state_handle
                    .push_user_message(ConversationItem::user("hello".to_string()));
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            actor
                .last_idle_flush_conversation_len
                .store(80, std::sync::atomic::Ordering::Relaxed);
            {
                let current_len = actor.chat_state_handle.get_conversation_len().await;
                let last_len = actor
                    .last_idle_flush_conversation_len
                    .load(std::sync::atomic::Ordering::Relaxed);
                assert_eq!(current_len, 80);
                assert!(
                    current_len <= last_len,
                    "guard should block: no new messages"
                );
            }
            {
                let compacted = vec![ConversationItem::user("compacted summary".to_string())];
                let new_len = compacted.len();
                actor
                    .chat_state_handle
                    .replace_conversation_for_compaction(compacted);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                actor
                    .last_idle_flush_conversation_len
                    .store(new_len, std::sync::atomic::Ordering::Relaxed);
            }
            {
                actor
                    .chat_state_handle
                    .push_user_message(ConversationItem::user("new message".to_string()));
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                let current_len = actor.chat_state_handle.get_conversation_len().await;
                let last_len = actor
                    .last_idle_flush_conversation_len
                    .load(std::sync::atomic::Ordering::Relaxed);
                assert_eq!(current_len, 2, "summary + new message");
                assert_eq!(last_len, 1, "reset to post-compaction length");
                assert!(
                    current_len > last_len,
                    "guard should allow flush after compaction + new message"
                );
            }
        })
        .await;
}
fn api_error_with_context_window(context_window: u64) -> echo_agent_sampler::SamplingErrorInfo {
    echo_agent_sampler::SamplingErrorInfo {
        kind: echo_agent_sampler::SamplingErrorKind::Api,
        status_code: Some(400),
        message: "prompt is too long".to_string(),
        is_retryable: false,
        retry_after_secs: None,
        should_retry: None,
        error_code: None,
        model_metadata: Some(crate::sampling::ResponseModelMetadata {
            context_window: Some(context_window),
            max_completion_tokens: None,
            models_etag: None,
        }),
        empty_response_context: None,
        doom_loop_triggers: None,
        doom_loop_aborted_at_chunk: None,
        credential: echo_agent_sampling_types::SentCredential::Unknown,
    }
}
/// Primary scenario: remote settings shrinks the context window mid-session.
/// The shell's last-known token count (214K) exceeds the new limit (200K) —
/// should_compact_on_error must return true so the session can recover.
#[tokio::test(flavor = "current_thread")]
async fn test_compact_on_error_triggers_when_tokens_exceed_new_window() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(214_000, 1_000_000, 85, gateway_tx, persistence_tx).await;
            let err = api_error_with_context_window(200_000);
            assert!(actor.should_compact_on_error(&err).await);
        })
        .await;
}
/// When tracked tokens are within the new limit, the error was not a context
/// overflow — do not compact.
#[tokio::test(flavor = "current_thread")]
async fn test_compact_on_error_no_trigger_when_tokens_within_new_window() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(150_000, 1_000_000, 85, gateway_tx, persistence_tx).await;
            let err = api_error_with_context_window(200_000);
            assert!(!actor.should_compact_on_error(&err).await);
        })
        .await;
}
/// If the proxy hasn't been updated yet, model_metadata is None — must be
/// a no-op for backwards compatibility.
#[tokio::test(flavor = "current_thread")]
async fn test_compact_on_error_noop_without_model_metadata() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(500_000, 200_000, 85, gateway_tx, persistence_tx).await;
            let err = echo_agent_sampler::SamplingErrorInfo {
                kind: echo_agent_sampler::SamplingErrorKind::Api,
                status_code: Some(400),
                message: "prompt is too long".to_string(),
                is_retryable: false,
                retry_after_secs: None,
                should_retry: None,
                error_code: None,
                model_metadata: None,
                empty_response_context: None,
                doom_loop_triggers: None,
                doom_loop_aborted_at_chunk: None,
                credential: echo_agent_sampling_types::SentCredential::Unknown,
            };
            assert!(!actor.should_compact_on_error(&err).await);
        })
        .await;
}
/// Pre-sampling check uses estimated tokens (includes tool-result delta).
#[tokio::test(flavor = "current_thread")]
async fn test_pre_sampling_uses_estimated_tokens() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(80_000, 100_000, 85, gateway_tx, persistence_tx).await;
            let result = actor.check_auto_compact_needed().await;
            assert!(result.is_none(), "80% should not trigger at 85% threshold");
            actor.chat_state_handle.record_token_usage(90_000);
            let result = actor.check_auto_compact_needed().await;
            assert!(result.is_some(), "90% should trigger");
            assert_eq!(result.unwrap().percentage, 90);
        })
        .await;
}
/// Model-switch compaction fires when switching to a smaller context window.
#[tokio::test(flavor = "current_thread")]
async fn test_model_switch_compaction_triggers_on_downgrade() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(86_000, 100_000, 85, gateway_tx, persistence_tx).await;
            actor.compaction.previous_model.set(Some(
                crate::session::compaction_config::PreviousModelInfo {
                    model_slug: "large-model".to_string(),
                    context_window: 200_000,
                },
            ));
            let prev = actor.compaction.previous_model.take();
            assert!(prev.is_some());
            let prev = prev.unwrap();
            assert_eq!(prev.context_window, 200_000);
            let cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            assert!(prev.context_window > cfg.context_window.get());
            let total = actor.chat_state_handle.get_estimated_total_tokens().await;
            let trigger = actor.should_auto_compact(total, cfg.context_window);
            assert!(trigger.is_some(), "86% > 85% threshold, should trigger");
            actor.compaction.previous_model.set(Some(
                crate::session::compaction_config::PreviousModelInfo {
                    model_slug: "small-model".to_string(),
                    context_window: 50_000,
                },
            ));
            let prev = actor.compaction.previous_model.take().unwrap();
            assert!(prev.context_window <= cfg.context_window.get());
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn get_transcript_path_returns_some_when_file_exists() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor =
                create_test_actor(50_000, 200_000, 85, gateway_tx, persistence_tx).await;
            actor.compaction.compaction_mode = echo_agent_chat_state::CompactionMode::Transcript;
            let session_dir = crate::session::persistence::session_dir(&actor.session_info);
            std::fs::create_dir_all(&session_dir).unwrap();
            let updates_path = session_dir.join("updates.jsonl");
            std::fs::write(&updates_path, "{}\n").unwrap();
            let result = actor.get_transcript_path();
            assert!(result.is_some(), "file exists → Some");
            assert!(
                result.as_ref().unwrap().ends_with("updates.jsonl"),
                "path should end with updates.jsonl, got: {:?}",
                result,
            );
            let hint = actor.transcript_hint().expect("transcript hint present");
            assert!(hint.contains("read the full transcript"));
            assert!(hint.ends_with("updates.jsonl"));
            actor.compaction.compaction_mode = echo_agent_chat_state::CompactionMode::Summary;
            assert!(actor.transcript_hint().is_none());
            let _ = std::fs::remove_file(&updates_path);
            let _ = std::fs::remove_dir_all(&session_dir);
        })
        .await;
}
