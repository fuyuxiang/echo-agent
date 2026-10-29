use super::support::*;
use super::*;
use tokio::sync::mpsc;
/// Test that `last_api_request_at` is recorded and used for idle detection.
///
/// The `maybe_refresh_model_metadata_on_resume` method checks this timestamp
/// to decide whether to proactively refresh model metadata from cli-chat-proxy.
/// This test verifies the timestamp recording and idle detection logic.
#[tokio::test(flavor = "current_thread")]
async fn test_last_api_request_at_idle_detection() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel();
            let (persistence_tx, _) = mpsc::unbounded_channel();
            let actor = create_test_actor(50_000, 100_000, 85, gateway_tx, persistence_tx).await;
            let initial = actor
                .last_api_request_at
                .load(std::sync::atomic::Ordering::Relaxed);
            assert_eq!(initial, 0, "last_api_request_at should be 0 initially");
            actor.record_api_request_time();
            let recorded = actor
                .last_api_request_at
                .load(std::sync::atomic::Ordering::Relaxed);
            assert!(
                recorded > 0,
                "last_api_request_at should be set after recording"
            );
            let now_ms = chrono::Utc::now().timestamp_millis();
            let diff = (now_ms - recorded).abs();
            assert!(
                diff < 1000,
                "recorded timestamp should be within 1 second of now"
            );
            let idle_secs = (now_ms - recorded) / 1000;
            assert!(
                idle_secs < SessionActor::IDLE_REFRESH_THRESHOLD_SECS,
                "should be within idle threshold immediately after recording"
            );
        })
        .await;
}
/// End-to-end test for `maybe_refresh_model_metadata_on_resume`.
///
/// Simulates a session idle for >10 minutes, then verifies the function
/// fetches `/models-v2`, parses the response, and updates `context_window`
/// and `max_completion_tokens` in the sampling config.
#[tokio::test(flavor = "current_thread")]
async fn test_e2e_idle_resume_refreshes_model_metadata() {
    use axum::routing::get;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let app = axum::Router::new().route(
                "/v1/models-v2",
                get(|| async {
                    axum::Json(serde_json::json!({
                        "data": [{
                            "model": "test-model",
                            "name": "Test Model",
                            "context_window": 300_000,
                            "max_completion_tokens": 16384,
                            "base_url": "http://localhost/v1"
                        }]
                    }))
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let mock_url = format!("http://{}/v1", addr);
            tokio::task::spawn_local(async move {
                axum::serve(listener, app).await.unwrap();
            });
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let (gateway_tx, _) = mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = mpsc::unbounded_channel::<PersistenceMsg>();
            let cwd = echo_agent_paths::AbsPathBuf::new(std::path::PathBuf::from("/tmp")).unwrap();
            let fs = Arc::new(echo_agent_workspace::file_system::MockFs::new(
                cwd.to_path_buf(),
            ));
            let terminal = Arc::new(DummyTerminal {});
            let (hunk_tx, _) = tokio::sync::mpsc::unbounded_channel();
            let hunk_tracker_handle = echo_agent_hunk_tracker::HunkTrackerActor::spawn(
                "test-idle-resume".to_string(),
                cwd.to_path_buf(),
                hunk_tx,
                echo_agent_hunk_tracker::TrackingMode::AgentOnly,
                tokio_util::sync::CancellationToken::new(),
            );
            let tool_context =
                ToolContext::new(cwd.clone(), None, None, fs, terminal, hunk_tracker_handle);
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
            let (chat_event_tx, _) = tokio::sync::mpsc::unbounded_channel();
            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
            let chat_state_handle = echo_agent_chat_state::ChatStateActor::spawn(
                vec![],
                echo_agent_sampling_types::SamplingConfig {
                    base_url: mock_url,
                    model: "test-model".to_string(),
                    max_completion_tokens: Some(8192),
                    temperature: None,
                    top_p: None,
                    api_backend: Default::default(),
                    extra_headers: Default::default(),
                    query_params: Default::default(),
                    env_http_headers: Default::default(),
                    context_window: std::num::NonZeroU64::new(200_000).unwrap(),
                    reasoning_effort: None,
                    stream_tool_calls: None,
                },
                Box::new(echo_agent_chat_state::NullChatPersistence),
                chat_event_tx,
                tokio_util::sync::CancellationToken::new(),
            );
            chat_state_handle.update_credentials(echo_agent_chat_state::types::Credentials {
                api_key: Some("test-key".to_string()),
                auth_type: Default::default(),
                alpha_test_key: None,
                client_version: None,
            });
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let actor = SessionActor {
                status_wake: Default::default(),
                session_info: SessionInfo {
                    id: acp::SessionId::new("test-idle-resume"),
                    cwd: cwd.as_str().to_string(),
                },
                attribution_callback: None,
                auth_method_id: test_auth_method_id("cached_token"),
                model_auth_memo: std::cell::RefCell::new(None),
                auth_manager: {
                    let dir = tempfile::tempdir().unwrap();
                    let mgr = std::sync::Arc::new(crate::auth::AuthManager::new(
                        dir.path(),
                        crate::auth::EchoAgentComConfig::default(),
                    ));
                    mgr.hot_swap(crate::auth::EchoAgentAuth {
                        auth_mode: crate::auth::AuthMode::Oidc,
                        refresh_token: Some("rt".into()),
                        expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                        ..crate::auth::EchoAgentAuth::test_default()
                    });
                    std::mem::forget(dir);
                    Some(mgr)
                },
                is_chat_kind: false,
                state,
                notifications: NotificationSender {
                    gateway: GatewaySender::new(gateway_tx),
                    gateway_enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                    persistence_tx,
                    disk_full: crate::session::notifications::idle_disk_full_rx(),
                },
                permissions: echo_agent_workspace::permission::PermissionHandle::allow_all(),
                tool_context,
                deny_read_globs: Vec::new(),
                mcp_state: Arc::new(TokioMutex::new(McpState::new(vec![]))),
                mcp_strategy: std::cell::Cell::new(McpInitStrategy::Blocking),
                delivery_tools: std::cell::RefCell::new(Vec::new()),
                attach_non_interactive: std::rc::Rc::new(std::cell::Cell::new(false)),
                chat_state_handle,
                unattributed_background_usage: std::sync::atomic::AtomicBool::new(false),
                current_prompt_id: std::sync::Arc::new(std::sync::Mutex::new(None)),
                pending_interactions: std::sync::Arc::new(std::sync::Mutex::new(
                    std::collections::HashMap::new(),
                )),
                current_prompt_mode: Arc::new(parking_lot::Mutex::new(PromptMode::Agent)),
                turn_start_prompt_mode: parking_lot::Mutex::new(PromptMode::Agent),
                desktop_document_turn: std::cell::Cell::new(false),
                turn_prompt_mode: Arc::new(parking_lot::Mutex::new(PromptMode::Agent)),
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
                    threshold_percent: std::cell::Cell::new(85),
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
                inference_idle_timeout: Duration::from_secs(300),
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
                queue_exit_reminder_on_approved_exit: Arc::new(std::sync::atomic::AtomicBool::new(
                    false,
                )),
                active_skill: parking_lot::Mutex::new(None),
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
                workflow_manager: crate::session::workflow::manager::WorkflowManager::test_bundle()
                    .0,
                workflow_launch_tx: tokio::sync::mpsc::unbounded_channel().0,
                goal_classifier_enabled: false,
                goal_planner_enabled: false,
                goal_summary_enabled: false,
                goal_verifier_skeptic_count: 1,
                goal_role_models: Default::default(),
                goal_use_current_model_only: false,
                goal_classifier_max_runs:
                    crate::session::goal_classifier::GOAL_CLASSIFIER_MAX_RUNS_DEFAULT,
                goal_strategist_every: 5,
                goal_reverify_after: crate::session::acp_session::GOAL_REVERIFY_AFTER_DEFAULT,
                goal_plan_reconciled: std::sync::atomic::AtomicBool::new(false),
                pending_classifier_completions: parking_lot::Mutex::new(
                    std::collections::VecDeque::new(),
                ),
                goal_classifier_in_flight: std::sync::atomic::AtomicBool::new(false),
                managed_mcp_handle: Default::default(),
                initial_client_mcp_servers: vec![],
                tool_metadata_snapshot: Arc::new(std::sync::Mutex::new(Default::default())),
                mcp_announced_servers: Mutex::new(HashMap::new()),
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
                streaming_turn_capture: parking_lot::Mutex::new(StreamingTurnCapture::default()),
                turn_stream_drained: parking_lot::Mutex::new(None),
                pending_image_strip: parking_lot::Mutex::new(None),
                sampler_handle: echo_agent_sampler::SamplerHandle::noop(),
                sampling_gate: None,
                rebuild_spec: crate::session::agent_rebuild::test_rebuild_spec_default(),
                image_description_model: crate::test_support::TEST_MODEL.to_owned(),
                image_describe_cache: Arc::new(
                    crate::session::image_describe::ImageDescribeCache::new(),
                ),
                subagent_token_records: parking_lot::Mutex::new(HashMap::new()),
                workspace_ops: echo_agent_workspace::WorkspaceOps::for_test(),
                trace_config_template: std::cell::RefCell::new(None),
            };
            let eleven_minutes_ago_ms = chrono::Utc::now().timestamp_millis() - (11 * 60 * 1000);
            actor
                .last_api_request_at
                .store(eleven_minutes_ago_ms, std::sync::atomic::Ordering::Relaxed);
            let cfg_before = actor.chat_state_handle.get_sampling_config().await.unwrap();
            assert_eq!(
                cfg_before.context_window,
                std::num::NonZeroU64::new(200_000).unwrap()
            );
            assert_eq!(cfg_before.max_completion_tokens, Some(8192));
            actor.maybe_refresh_model_metadata_on_resume().await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let cfg_after = actor.chat_state_handle.get_sampling_config().await.unwrap();
            assert_eq!(
                cfg_after.context_window,
                std::num::NonZeroU64::new(300_000).unwrap(),
                "context_window should be updated to 300K from /models-v2"
            );
            assert_eq!(
                cfg_after.max_completion_tokens,
                Some(16384),
                "max_completion_tokens should be updated to 16384 from /models-v2"
            );
        })
        .await;
}
/// Verify `maybe_refresh_model_metadata_on_resume` is a no-op when idle < 10 min.
#[tokio::test(flavor = "current_thread")]
async fn test_idle_resume_noop_when_not_idle_enough() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(50_000, 200_000, 85, gateway_tx, persistence_tx).await;
            let five_minutes_ago_ms = chrono::Utc::now().timestamp_millis() - (5 * 60 * 1000);
            actor
                .last_api_request_at
                .store(five_minutes_ago_ms, std::sync::atomic::Ordering::Relaxed);
            let cfg_before = actor.chat_state_handle.get_sampling_config().await.unwrap();
            actor.maybe_refresh_model_metadata_on_resume().await;
            let cfg_after = actor.chat_state_handle.get_sampling_config().await.unwrap();
            assert_eq!(
                cfg_before.context_window, cfg_after.context_window,
                "config should not change when idle < 10 min"
            );
        })
        .await;
}
