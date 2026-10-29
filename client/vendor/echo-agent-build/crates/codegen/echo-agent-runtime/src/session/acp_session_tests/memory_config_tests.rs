use super::support::*;
use super::*;
use crate::session::memory::MemorySearchSource;
use echo_agent_paths::AbsPathBuf;
use echo_agent_workspace::file_system::MockFs;
use echo_agent_workspace::permission::PermissionHandle;
use tokio::sync::mpsc;
#[test]
fn first_turn_memory_visibility_matches_displayed_score() {
    assert_eq!(
        [true, true, false, false],
        [0.90, 0.006, 0.004, 0.0].map(super::SessionActor::is_first_turn_memory_score_visible),
    );
}
#[test]
fn initial_injection_backend_params_use_override_min_score() {
    let params = crate::session::memory::MemoryBackendParams {
        embedding_maintenance: Default::default(),
        session_id: "test-session".to_owned(),
        embed_config: None,
        embed_base_url: "http://localhost".to_owned(),
        embed_api_key: None,
        search_config: crate::config::MemorySearchConfig {
            min_score: 0.35,
            ..Default::default()
        },
        watcher: None,
        stale_claim_secs: 60,
        search_source: MemorySearchSource::Tool,
        observation_sink: crate::session::memory::noop_memory_observation_sink(),
        embedding_credentials: crate::session::memory::EndpointScopedCredentials::none(),
    };
    let initial_injection = crate::config::MemoryInitialInjectionConfig {
        enabled: true,
        min_score: Some(0.72),
    };
    let (adjusted, effective_min_score) =
        build_initial_injection_backend_params(&params, &initial_injection);
    assert_eq!(MemorySearchSource::Injection, adjusted.search_source);
    assert!((0.72 - adjusted.search_config.min_score).abs() < f32::EPSILON);
    assert!((0.72 - effective_min_score as f32).abs() < f32::EPSILON);
    assert!((0.35 - params.search_config.min_score).abs() < f32::EPSILON);
    assert_eq!(MemorySearchSource::Tool, params.search_source);
}
#[test]
fn initial_injection_backend_params_preserve_default_zero_min_score() {
    let params = crate::session::memory::MemoryBackendParams {
        embedding_maintenance: Default::default(),
        session_id: "test-session".to_owned(),
        embed_config: None,
        embed_base_url: "http://localhost".to_owned(),
        embed_api_key: None,
        search_config: crate::config::MemorySearchConfig {
            min_score: 0.41,
            ..Default::default()
        },
        watcher: None,
        stale_claim_secs: 60,
        search_source: MemorySearchSource::Tool,
        observation_sink: crate::session::memory::noop_memory_observation_sink(),
        embedding_credentials: crate::session::memory::EndpointScopedCredentials::none(),
    };
    let initial_injection = crate::config::MemoryInitialInjectionConfig {
        enabled: true,
        min_score: None,
    };
    let (adjusted, effective_min_score) =
        build_initial_injection_backend_params(&params, &initial_injection);
    assert_eq!(MemorySearchSource::Injection, adjusted.search_source);
    assert!((0.41 - adjusted.search_config.min_score).abs() < f32::EPSILON);
    assert!((0.0 - effective_min_score as f32).abs() < f32::EPSILON);
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
    let cwd = AbsPathBuf::new(cwd_path.clone()).unwrap();
    let fs = Arc::new(MockFs::new(cwd.to_path_buf()));
    let terminal = Arc::new(DummyTerminal {});
    let (hunk_tx, _) = tokio::sync::mpsc::unbounded_channel();
    let hunk_tracker_handle = echo_agent_hunk_tracker::HunkTrackerActor::spawn(
        "test-memory".to_string(),
        cwd.to_path_buf(),
        hunk_tx,
        echo_agent_hunk_tracker::TrackingMode::AgentOnly,
        tokio_util::sync::CancellationToken::new(),
    );
    let tool_context = ToolContext::new(cwd.clone(), None, None, fs, terminal, hunk_tracker_handle);
    let memory_storage = memory_config
        .as_ref()
        .filter(|mc| mc.enabled)
        .map(|_| crate::session::memory::MemoryStorage::new(&cwd_path, None));
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
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
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
    std::mem::forget(tmp);
    let memory_initial_injection_config = memory_config
        .as_ref()
        .map_or_else(Default::default, |mc| mc.initial_injection.clone());
    SessionActor {
        status_wake: Default::default(),
        session_info: SessionInfo {
            id: acp::SessionId::new("test-memory"),
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
        unattributed_background_usage: std::sync::atomic::AtomicBool::new(false),
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
        },
        session_start: std::time::Instant::now(),
        inference_idle_timeout: Duration::from_secs(300),
        max_retries: 3,
        rate_limit_waits: crate::session::acp_session::RateLimitWaitConfig::default(),
        max_turns: None,
        pending_interjections: InterjectionBuffer::new(),
        pending_skill_reminders: Mutex::new(Vec::new()),
        idle_flush_timeout: memory_config
            .as_ref()
            .and_then(|mc| mc.flush.idle_timeout_secs)
            .map(std::time::Duration::from_secs),
        dream_check_timeout: memory_config
            .as_ref()
            .filter(|mc| mc.dream.enabled)
            .and_then(|mc| mc.dream.check_interval_secs)
            .filter(|&s| s > 0)
            .map(std::time::Duration::from_secs),
        last_idle_flush_conversation_len: std::sync::atomic::AtomicUsize::new(0),
        event_tx,
        buffering_settings: None,
        client_identifier: None,
        origin_client: None,
        feedback_manager: Arc::new(FeedbackManager::local_only("test-memory")),
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
        image_describe_cache: Arc::new(crate::session::image_describe::ImageDescribeCache::new()),
        subagent_token_records: parking_lot::Mutex::new(HashMap::new()),
        workspace_ops: echo_agent_workspace::WorkspaceOps::for_test(),
        trace_config_template: std::cell::RefCell::new(None),
    }
}
#[tokio::test(flavor = "current_thread")]
async fn test_is_flushing_suppresses_auto_compact() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel();
            let (persistence_tx, _) = mpsc::unbounded_channel();
            let actor = create_test_actor(90_000, 100_000, 85, gateway_tx, persistence_tx).await;
            let result = actor.check_auto_compact_needed().await;
            assert!(result.is_some(), "should trigger at 90%");
            actor
                .memory
                .is_flushing
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let result = actor.check_auto_compact_needed().await;
            assert!(result.is_none(), "should suppress when is_flushing=true");
            actor
                .memory
                .is_flushing
                .store(false, std::sync::atomic::Ordering::Relaxed);
            let result = actor.check_auto_compact_needed().await;
            assert!(
                result.is_some(),
                "should trigger again after is_flushing=false"
            );
        })
        .await;
}
/// Test that `force_compact` triggers auto-compact even below threshold,
/// and is consumed (reset to false) after a single use.
#[tokio::test(flavor = "current_thread")]
async fn test_force_compact_triggers_below_threshold() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel();
            let (persistence_tx, _) = mpsc::unbounded_channel();
            let actor = create_test_actor(10_000, 100_000, 85, gateway_tx, persistence_tx).await;
            let result = actor.check_auto_compact_needed().await;
            assert!(result.is_none(), "should not trigger at 10%");
            actor
                .compaction
                .force_compact
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let result = actor.check_auto_compact_needed().await;
            assert!(
                result.is_some(),
                "force_compact should trigger at any usage"
            );
            let info = result.unwrap();
            assert_eq!(info.tokens_used, 10_000);
            assert!(
                !actor
                    .compaction
                    .force_compact
                    .load(std::sync::atomic::Ordering::Relaxed),
                "force_compact should be consumed after use"
            );
            let result = actor.check_auto_compact_needed().await;
            assert!(result.is_none(), "should not trigger after flag consumed");
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
#[allow(clippy::field_reassign_with_default)]
async fn test_flush_config_from_memory_config() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel();
            let (persistence_tx, _) = mpsc::unbounded_channel();
            let mut config = crate::config::MemoryConfig::default();
            config.enabled = true;
            config.pruning.keep_last_n_turns = 7;
            config.pruning.soft_trim_threshold = 9999;
            config.flush.soft_threshold_tokens = 12345;
            let actor = create_test_actor_with_memory(
                50_000,
                100_000,
                85,
                gateway_tx,
                persistence_tx,
                Some(config),
            )
            .await;
            assert_eq!(actor.memory.flush_config.soft_threshold_tokens, 12345);
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
#[allow(clippy::field_reassign_with_default)]
async fn test_memory_flush_enabled_from_config() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel();
            let (persistence_tx, _) = mpsc::unbounded_channel();
            let mut config = crate::config::MemoryConfig::default();
            config.enabled = true;
            config.flush.enabled = true;
            let actor = create_test_actor_with_memory(
                50_000,
                100_000,
                85,
                gateway_tx.clone(),
                persistence_tx,
                Some(config),
            )
            .await;
            assert!(
                actor.memory.flush_config.enabled,
                "flush_config.enabled should be true from MemoryConfig"
            );
            let (persistence_tx2, _) = mpsc::unbounded_channel();
            let mut config2 = crate::config::MemoryConfig::default();
            config2.enabled = true;
            config2.flush.enabled = false;
            let actor2 = create_test_actor_with_memory(
                50_000,
                100_000,
                85,
                gateway_tx,
                persistence_tx2,
                Some(config2),
            )
            .await;
            assert!(
                !actor2.memory.flush_config.enabled,
                "flush_config.enabled should be false when config says so"
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
#[allow(clippy::field_reassign_with_default)]
async fn test_memory_storage_created_when_enabled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel();
            let (persistence_tx, _) = mpsc::unbounded_channel();
            let mut config = crate::config::MemoryConfig::default();
            config.enabled = true;
            let actor = create_test_actor_with_memory(
                50_000,
                100_000,
                85,
                gateway_tx.clone(),
                persistence_tx,
                Some(config),
            )
            .await;
            assert!(
                actor.memory.is_enabled(),
                "memory_storage should be Some when enabled"
            );
            let (persistence_tx2, _) = mpsc::unbounded_channel();
            let actor2 = create_test_actor_with_memory(
                50_000,
                100_000,
                85,
                gateway_tx,
                persistence_tx2,
                None,
            )
            .await;
            assert!(
                !actor2.memory.is_enabled(),
                "memory_storage should be None when disabled"
            );
        })
        .await;
}
/// Actor with injection enabled and an FTS index matching the test query, so
/// `first_turn_memory_reminder()` can inject and cache related recall while
/// rebuilding live preferences on each request.
#[allow(clippy::field_reassign_with_default)]
async fn create_injection_ready_actor(
    initial_conversation: Vec<echo_agent_sampling_types::ConversationItem>,
) -> SessionActor {
    let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
    let (persistence_tx, _persistence_rx) = mpsc::unbounded_channel();
    let mut config = crate::config::MemoryConfig::default();
    config.enabled = true;
    config.initial_injection = crate::config::MemoryInitialInjectionConfig {
        enabled: true,
        min_score: None,
    };
    let mut actor =
        create_test_actor_with_memory(1_000, 100_000, 85, gateway_tx, persistence_tx, Some(config))
            .await;
    let tmp = tempfile::TempDir::new().unwrap();
    let global_dir = tmp.path().join("memory");
    let workspace_dir = global_dir.join("test_ws");
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let storage =
        crate::session::memory::MemoryStorage::with_paths(global_dir, workspace_dir.clone());
    crate::session::memory::index::init_sqlite_vec();
    let note = storage.workspace_memory_file();
    std::fs::write(
        &note,
        "# Conventions\n\nProject uses Rust for backend services.",
    )
    .unwrap();
    let mut idx = crate::session::memory::index::MemoryIndex::open_or_create(
        &workspace_dir.join("index.sqlite"),
        storage.clone(),
        crate::config::MemoryIndexConfig::default(),
        4,
    )
    .unwrap();
    idx.reindex_file(&note, "workspace").unwrap();
    drop(idx);
    actor.memory.storage = std::cell::RefCell::new(Some(storage));
    std::mem::forget(tmp);
    actor.memory.backend_params = Some(crate::session::memory::MemoryBackendParams {
        embedding_maintenance: Default::default(),
        session_id: "test-memory".to_owned(),
        embed_config: None,
        embed_base_url: "http://localhost".to_owned(),
        embed_api_key: None,
        search_config: crate::config::MemorySearchConfig::default(),
        watcher: None,
        stale_claim_secs: 60,
        search_source: MemorySearchSource::Tool,
        observation_sink: crate::session::memory::noop_memory_observation_sink(),
        embedding_credentials: crate::session::memory::EndpointScopedCredentials::none(),
    });
    actor
        .chat_state_handle
        .replace_conversation(initial_conversation);
    actor
}
/// Control: proves the harness setup is sufficient for injection, so the
/// companion test below isolates the idempotency guard.
#[tokio::test(flavor = "current_thread")]
async fn test_first_turn_reminder_injects_without_persisted_block() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = create_injection_ready_actor(vec![
                echo_agent_sampling_types::ConversationItem::system("You are a helpful assistant."),
                echo_agent_sampling_types::ConversationItem::user(
                    "tell me about rust backend services conventions",
                ),
            ])
            .await;
            let reminder = actor.first_turn_memory_reminder().await;
            let reminder = reminder.expect("first turn with matching index must inject");
            assert!(
                reminder.contains(echo_agent_chat_state::MEMORY_CONTEXT_OPEN_TAG),
                "reminder must be a tagged memory-context block, got: {reminder}"
            );
            assert!(
                actor
                    .memory
                    .context_injected
                    .load(std::sync::atomic::Ordering::Relaxed),
                "latch must be set after the injection decision runs"
            );
            assert_eq!(
                Some(reminder.to_owned()),
                actor.first_turn_memory_reminder().await
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn test_first_turn_reminder_skips_all_displayed_zero_results() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut actor = create_injection_ready_actor(vec![
                echo_agent_sampling_types::ConversationItem::system("You are a helpful assistant."),
                echo_agent_sampling_types::ConversationItem::user(
                    "tell me about rust backend services conventions",
                ),
            ])
            .await;
            actor
                .memory
                .backend_params
                .as_mut()
                .unwrap()
                .search_config
                .source_weights
                .insert("workspace".to_owned(), 0.0);
            assert!(
                !actor
                    .first_turn_memory_reminder()
                    .await
                    .unwrap()
                    .contains("Project uses Rust")
            );
            assert_eq!(
                0,
                actor
                    .memory
                    .injection_count
                    .load(std::sync::atomic::Ordering::Relaxed)
            );
            assert!(
                actor
                    .memory
                    .context_injected
                    .load(std::sync::atomic::Ordering::Relaxed)
            );
            assert!(
                !actor
                    .first_turn_memory_reminder()
                    .await
                    .unwrap()
                    .contains("Project uses Rust")
            );
        })
        .await;
}
/// A persisted block cannot override current records after resume.
#[tokio::test(flavor = "current_thread")]
async fn test_first_turn_reminder_refreshes_when_block_persisted() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let persisted_block =
                crate::session::helpers::memory_context::format_memory_reminder(&[
                    echo_agent_tools::types::memory_backend::MemorySearchResult {
                        chunk_id: "prev:0".into(),
                        path: "MEMORY.md".into(),
                        start_line: 0,
                        end_line: 5,
                        score: 0.98,
                        snippet: "Obsolete preference that must not be reused.".into(),
                        source: "workspace".into(),
                        created_at: None,
                    },
                ])
                .unwrap();
            let actor = create_injection_ready_actor(vec![
                echo_agent_sampling_types::ConversationItem::system(format!(
                    "You are a helpful assistant.\n\n{persisted_block}"
                )),
                echo_agent_sampling_types::ConversationItem::user(
                    "tell me about rust backend services conventions",
                ),
            ])
            .await;
            let reminder = actor.first_turn_memory_reminder().await.unwrap();
            assert!(!reminder.contains("Obsolete preference"));
            assert!(reminder.contains("Project uses Rust"));
            assert!(
                actor
                    .memory
                    .context_injected
                    .load(std::sync::atomic::Ordering::Relaxed),
                "the live context decision is recorded"
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
#[allow(clippy::field_reassign_with_default)]
async fn test_idle_flush_timeout_from_config() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = mpsc::unbounded_channel();
            let (persistence_tx, _) = mpsc::unbounded_channel();
            let mut config = crate::config::MemoryConfig::default();
            config.enabled = true;
            config.flush.idle_timeout_secs = Some(120);
            let actor = create_test_actor_with_memory(
                50_000,
                100_000,
                85,
                gateway_tx.clone(),
                persistence_tx,
                Some(config),
            )
            .await;
            assert_eq!(
                actor.idle_flush_timeout,
                Some(std::time::Duration::from_secs(120))
            );
            let (persistence_tx2, _) = mpsc::unbounded_channel();
            let mut config2 = crate::config::MemoryConfig::default();
            config2.enabled = true;
            config2.flush.idle_timeout_secs = None;
            let actor2 = create_test_actor_with_memory(
                50_000,
                100_000,
                85,
                gateway_tx,
                persistence_tx2,
                Some(config2),
            )
            .await;
            assert_eq!(actor2.idle_flush_timeout, None);
        })
        .await;
}
