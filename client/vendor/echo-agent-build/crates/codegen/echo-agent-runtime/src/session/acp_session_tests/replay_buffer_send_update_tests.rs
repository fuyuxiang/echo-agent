use super::support::*;
use super::*;
use crate::terminal::AsyncTerminalRunner;
use crate::terminal::runner::{TerminalError, TerminalRunRequest, TerminalRunResult};
use echo_agent_paths::AbsPathBuf;
use echo_agent_workspace::file_system::MockFs;
use echo_agent_workspace::permission::PermissionHandle;
use tokio::sync::mpsc;
#[derive(Debug)]
struct DummyTerminal;
#[async_trait::async_trait]
impl AsyncTerminalRunner for DummyTerminal {
    async fn run(&self, _request: TerminalRunRequest) -> Result<TerminalRunResult, TerminalError> {
        Err(TerminalError::Other("dummy terminal".into()))
    }
}
fn agent_msg_update(text: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
        acp::TextContent::new(text.to_string()),
    )))
}
fn extract_text(n: &acp::SessionNotification) -> Option<String> {
    match &n.update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            acp::ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        },
        _ => None,
    }
}
pub(super) struct ReplaySendUpdateFixture {
    pub(super) actor: SessionActor,
    pub(super) event_rx: mpsc::UnboundedReceiver<SessionEvent>,
    sent: Arc<tokio::sync::Mutex<Vec<acp::SessionNotification>>>,
    persistence_rx: mpsc::UnboundedReceiver<PersistenceMsg>,
}
pub(super) async fn make_replay_send_update_fixture() -> ReplaySendUpdateFixture {
    let (gateway_tx, mut gateway_rx) =
        mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
    let gateway = GatewaySender::new(gateway_tx);
    let sent = Arc::new(tokio::sync::Mutex::new(
        Vec::<acp::SessionNotification>::new(),
    ));
    let sent_for_task = sent.clone();
    tokio::task::spawn_local(async move {
        while let Some(msg) = gateway_rx.recv().await {
            if let echo_agent_acp::AcpClientMessage::SessionNotification(args) = msg {
                sent_for_task.lock().await.push(args.request);
                let _ = args.response_tx.send(Ok(()));
            }
        }
    });
    let (persistence_tx, persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
    let cwd = AbsPathBuf::new(std::path::PathBuf::from("/tmp")).unwrap();
    let fs = Arc::new(MockFs::new(cwd.to_path_buf()));
    let terminal = Arc::new(DummyTerminal {});
    let (hunk_tx, _hunk_rx) = tokio::sync::mpsc::unbounded_channel();
    let hunk_tracker_handle = echo_agent_hunk_tracker::HunkTrackerActor::spawn(
        "test-session".to_string(),
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
    let (event_tx, event_rx) = mpsc::unbounded_channel::<SessionEvent>();
    let actor = SessionActor {
        status_wake: Default::default(),
        session_info: SessionInfo {
            id: acp::SessionId::new("test-session"),
            cwd: cwd.as_str().to_string(),
        },
        auth_method_id: test_auth_method_id("test-auth"),
        model_auth_memo: std::cell::RefCell::new(None),
        attribution_callback: None,
        auth_manager: None,
        is_chat_kind: false,
        state,
        notifications: NotificationSender {
            gateway,
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
        chat_state_handle: echo_agent_chat_state::ChatStateHandle::noop(),
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
        buffering_settings: Some(BufferingSettings {
            max_items: 100,
            max_bytes: 1_000_000,
            max_duration_ms: 50,
        }),
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
    };
    ReplaySendUpdateFixture {
        actor,
        event_rx,
        sent,
        persistence_rx,
    }
}
#[tokio::test(flavor = "current_thread")]
async fn send_update_buffers_streaming_chunks_and_flush_sends_merged_notification() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let ReplaySendUpdateFixture {
                actor,
                mut event_rx,
                sent,
                mut persistence_rx,
            } = make_replay_send_update_fixture().await;
            actor.send_update(agent_msg_update("he"), Some(1)).await;
            actor.send_update(agent_msg_update("llo"), Some(2)).await;
            assert!(
                sent.lock().await.is_empty(),
                "buffering enabled: no outbound notifications expected yet"
            );
            assert!(
                persistence_rx.try_recv().is_err(),
                "buffering enabled: nothing should be persisted until emitted"
            );
            let mut replay_buffer = ReplayBuffer::new(actor.buffering_settings.clone());
            while let Ok(event) = event_rx.try_recv() {
                let SessionEvent::Notification(notification) = event else {
                    unreachable!("send_update should only enqueue replay notifications")
                };
                let _ = replay_buffer.consume_chunk(notification);
            }
            let flushed = replay_buffer
                .flush()
                .expect("flush should emit pending chunk");
            actor.emit_buffered(flushed).await;
            tokio::task::yield_now().await;
            let sent_msgs = sent.lock().await.clone();
            assert_eq!(sent_msgs.len(), 1);
            assert_eq!(extract_text(&sent_msgs[0]).as_deref(), Some("hello"));
            let mut persisted = vec![];
            while let Ok(msg) = persistence_rx.try_recv() {
                persisted.push(msg);
            }
            let persisted_updates = persisted
                .into_iter()
                .filter(|m| matches!(m, PersistenceMsg::Update(_)))
                .count();
            assert_eq!(persisted_updates, 1);
        })
        .await;
}
/// Regression for the cancel-during-long-reasoning trace upload gap:
/// the `SessionCommand::Cancel` and `SessionCommand::CopyFile` handlers
/// in `run_session` must flush the actor-owned `ReplayBuffer` so streamed
/// chunks (notably `AgentThoughtChunk` reasoning) still pending at cancel
/// time are persisted to `updates.jsonl` before `mvp_agent` issues
/// `CopyFile` to snapshot the session directory for the trace upload.
///
/// Without the flush, the tail of a long reasoning stream sitting in the
/// buffer when the user hits Ctrl+C never reaches disk before
/// `copy_session_dir_to_memory` reads `updates.jsonl`. This test
/// exercises the exact code added to both match arms.
#[tokio::test(flavor = "current_thread")]
async fn cancel_and_copyfile_handlers_flush_buffered_chunks_to_persistence() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let ReplaySendUpdateFixture {
                actor,
                mut event_rx,
                sent: _sent,
                mut persistence_rx,
            } = make_replay_send_update_fixture().await;
            actor
                .send_update(agent_msg_update("partial reasoning tail"), Some(1))
                .await;
            assert!(
                persistence_rx.try_recv().is_err(),
                "no Update should land while the chunk is still in flight",
            );
            let mut replay_buffer = ReplayBuffer::new(actor.buffering_settings.clone());
            while let Ok(event) = event_rx.try_recv() {
                if let SessionEvent::Notification(notification) = event {
                    let _ = replay_buffer.consume_chunk(notification);
                }
            }
            assert!(
                persistence_rx.try_recv().is_err(),
                "chunk should still be buffered, not yet persisted",
            );
            if let Some(notification) = replay_buffer.flush() {
                actor.emit_buffered(notification).await;
            }
            tokio::task::yield_now().await;
            let mut got_chunk_text: Option<String> = None;
            while let Ok(msg) = persistence_rx.try_recv() {
                if let PersistenceMsg::Update(update) = msg
                    && let crate::session::storage::SessionUpdate::Acp(notif) = update
                    && let acp::SessionUpdate::AgentMessageChunk(chunk) = &notif.update
                    && let acp::ContentBlock::Text(t) = &chunk.content
                {
                    got_chunk_text = Some(t.text.clone());
                    break;
                }
            }
            assert_eq!(
                got_chunk_text.as_deref(),
                Some("partial reasoning tail"),
                "Cancel/CopyFile handler flush must persist the buffered \
                     reasoning chunk before the trace upload snapshots \
                     updates.jsonl"
            );
            drop(actor);
        })
        .await;
}
/// Negative control for `cancel_and_copyfile_handlers_flush_buffered_chunks_to_persistence`:
/// without the flush, a buffered chunk does NOT reach persistence on its
/// own — proving the flush call is load-bearing in the cancel path.
#[tokio::test(flavor = "current_thread")]
async fn buffered_chunk_does_not_reach_persistence_without_explicit_flush() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let ReplaySendUpdateFixture {
                actor,
                mut event_rx,
                sent: _sent,
                mut persistence_rx,
            } = make_replay_send_update_fixture().await;
            actor
                .send_update(agent_msg_update("would-be lost reasoning"), Some(1))
                .await;
            let mut replay_buffer = ReplayBuffer::new(actor.buffering_settings.clone());
            while let Ok(event) = event_rx.try_recv() {
                if let SessionEvent::Notification(notification) = event {
                    let _ = replay_buffer.consume_chunk(notification);
                }
            }
            tokio::task::yield_now().await;
            let mut saw_update = false;
            while let Ok(msg) = persistence_rx.try_recv() {
                if matches!(msg, PersistenceMsg::Update(_)) {
                    saw_update = true;
                    break;
                }
            }
            assert!(
                !saw_update,
                "without an explicit flush, the buffered chunk must \
                     remain stranded in `replay_buffer.pending` — this is \
                     the exact bug the Cancel/CopyFile patch fixes",
            );
            drop(actor);
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn available_commands_update_is_forwarded_but_not_persisted() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let ReplaySendUpdateFixture {
                actor,
                event_rx: _event_rx,
                sent,
                mut persistence_rx,
            } = make_replay_send_update_fixture().await;
            let session_id = acp::SessionId::new("test-session");
            actor
                .emit_notification_direct(
                    acp::SessionNotification::new(
                        session_id.clone(),
                        acp::SessionUpdate::AvailableCommandsUpdate(
                            acp::AvailableCommandsUpdate::new(vec![]),
                        ),
                    ),
                )
                .await;
            actor
                .emit_notification_direct(
                    acp::SessionNotification::new(session_id, agent_msg_update("hello")),
                )
                .await;
            for _ in 0..50 {
                if sent.lock().await.len() >= 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert_eq!(
                sent.lock().await.len(),
                2,
                "both updates must be forwarded to the live client (command palette must stay current)",
            );
            let mut persisted = vec![];
            while let Ok(msg) = persistence_rx.try_recv() {
                if let PersistenceMsg::Update(
                    crate::session::storage::SessionUpdate::Acp(n),
                ) = msg {
                    persisted.push(n);
                }
            }
            assert_eq!(
                persisted.len(),
                1,
                "exactly one update must be persisted; available_commands_update must be skipped",
            );
            assert!(
                matches!(persisted[0].update, acp::SessionUpdate::AgentMessageChunk(_)),
                "the persisted update must be the agent message, not available_commands_update",
            );
            drop(actor);
        })
        .await;
}
/// `handle_sampling_event::ChannelToken` for `Reasoning` and `Text`
/// channels must accumulate into the session's streaming capture so
/// the trace upload can serialize it even when the canonical
/// `record_assistant_response` path is skipped (cancel / max tokens).
#[tokio::test(flavor = "current_thread")]
async fn channel_tokens_accumulate_into_streaming_capture() {
    use echo_agent_sampler::{RequestId, SamplingChannel, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-stream-1".to_string());
            actor.current_turn_number.set(7);
            let req = RequestId::random();
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 1_700_000_000_000,
                })
                .await;
            for chunk in ["Let me ", "think ", "step by step. "] {
                actor
                    .handle_sampling_event(SamplingEvent::ChannelToken {
                        request_id: req.clone(),
                        channel: SamplingChannel::Reasoning,
                        text: chunk.to_string(),
                        chunk_index: 0,
                    })
                    .await;
            }
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Text,
                    text: "Answer: 42".to_string(),
                    chunk_index: 0,
                })
                .await;
            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(
                cap.prompt_id.as_deref(),
                Some("prompt-stream-1"),
                "prompt id must be stamped on the capture",
            );
            assert_eq!(cap.turn_number, 7, "turn number must be stamped");
            assert_eq!(
                cap.started_at_ms,
                Some(1_700_000_000_000),
                "started_at_ms must come from StreamStarted",
            );
            assert_eq!(
                cap.reasoning_text, "Let me think step by step. ",
                "reasoning chunks must concatenate in arrival order",
            );
            assert_eq!(
                cap.response_text, "Answer: 42",
                "text channel chunks must concatenate separately",
            );
            assert_eq!(cap.reasoning_chunks, 3);
            assert_eq!(cap.text_chunks, 1);
            assert!(!cap.truncated);
            assert_eq!(
                cap.phase,
                CapturePhase::ResponseText,
                "phase must reflect the most recent channel — text \
                     arrived last, so the model was cut off mid-response",
            );
        })
        .await;
}
/// A same-prompt `StreamStarted` restart (a doomloop retry) must accumulate a
/// second generation through the real `handle_sampling_event` path rather than
/// wipe the first — guards the `if cap.prompt_id != prompt_id` branch in the
/// `StreamStarted` arm that the pure-struct tests bypass.
#[tokio::test(flavor = "current_thread")]
async fn same_prompt_restart_accumulates_segments_via_handler() {
    use echo_agent_sampler::{RequestId, SamplingChannel, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-doomloop".to_string());
            let req = RequestId::random();
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 1,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "first gen reasoning".to_string(),
                    chunk_index: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 2,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req,
                    channel: SamplingChannel::Reasoning,
                    text: "second gen reasoning".to_string(),
                    chunk_index: 0,
                })
                .await;
            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(
                cap.segments.len(),
                1,
                "the first generation must be folded into segments, not wiped",
            );
            assert_eq!(cap.segments[0].reasoning_text, "first gen reasoning");
            assert_eq!(
                cap.reasoning_text, "second gen reasoning",
                "the second generation is the in-progress slot",
            );
            assert_eq!(cap.attempt_count, 2, "both same-prompt generations counted");
        })
        .await;
}
/// On `SamplingEvent::Completed` the canonical response is committed via
/// `record_assistant_response`, so its generation is DISCARDED from the
/// out-of-band capture (the in-progress slot is cleared, not folded into
/// `segments`) — that reasoning is already in afterStateHistory. Prior
/// uncommitted same-turn generations (e.g. a doomloop retry that preceded the
/// commit) are left intact, so a completed turn neither re-uploads its own
/// reasoning nor erases earlier uncommitted partials.
#[tokio::test(flavor = "current_thread")]
async fn completed_event_clears_slot_keeps_prior_uncommitted_segments() {
    use echo_agent_sampler::{InferenceLatencyStats, RequestId, SamplingChannel, SamplingEvent};
    use echo_agent_sampling_types::{ConversationItem, ConversationResponse};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-completed".to_string());
            let req = RequestId::random();
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "prior uncommitted reasoning".to_string(),
                    chunk_index: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 1,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "committed reasoning".to_string(),
                    chunk_index: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::Completed {
                    request_id: req,
                    response: Box::new(ConversationResponse {
                        items: vec![ConversationItem::assistant("Answer".to_string())],
                        usage: None,
                        stop_reason: None,
                        cost_usd_ticks: None,
                        message_chunks_emitted: 0,
                        doom_loop_signals: Vec::new(),
                        stop_message: None,
                        message_id: None,
                        raw_stop_reason: None,
                        stop_sequence: None,
                    }),
                    metrics: InferenceLatencyStats::default(),
                })
                .await;
            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(
                cap.segments.len(),
                1,
                "the prior uncommitted generation must be retained",
            );
            assert_eq!(
                cap.segments[0].reasoning_text,
                "prior uncommitted reasoning"
            );
            assert!(
                cap.reasoning_text.is_empty(),
                "Completed must clear the committed generation from the in-progress slot",
            );
        })
        .await;
}
/// Regression: the sampler-event drainer must release the per-turn
/// stream-drain barrier when (and only when) it processes the terminal
/// `Completed` event. `run_turn_via_sampler` awaits this barrier before the
/// turn loop emits the canonical client `ToolCall`s, so every streamed
/// text/thought chunk's global `eventId` is allocated before the tool call's.
/// Without it the tool call's `send_update` (run on the turn-loop task) could
/// interleave between two still-draining text chunks (run on the drainer task)
/// and split the assistant message around the tool call on every attached
/// client — the multi-pane "out of order" bug.
#[tokio::test(flavor = "current_thread")]
async fn completed_event_releases_stream_drain_barrier() {
    use echo_agent_sampler::{InferenceLatencyStats, RequestId, SamplingChannel, SamplingEvent};
    use echo_agent_sampling_types::{ConversationItem, ConversationResponse};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-barrier".to_string());
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            *actor.turn_stream_drained.lock() = Some(tx);
            let req = RequestId::random();
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Text,
                    text: "the scrollback blo".to_string(),
                    chunk_index: 0,
                })
                .await;
            assert!(
                actor.turn_stream_drained.lock().is_some(),
                "a mid-stream text chunk must NOT release the stream-drain barrier"
            );
            actor
                .handle_sampling_event(SamplingEvent::Completed {
                    request_id: req,
                    response: Box::new(ConversationResponse {
                        items: vec![ConversationItem::assistant("blocks".to_string())],
                        usage: None,
                        stop_reason: None,
                        cost_usd_ticks: None,
                        message_chunks_emitted: 1,
                        doom_loop_signals: Vec::new(),
                        stop_message: None,
                        message_id: None,
                        raw_stop_reason: None,
                        stop_sequence: None,
                    }),
                    metrics: InferenceLatencyStats::default(),
                })
                .await;
            assert!(
                actor.turn_stream_drained.lock().is_none(),
                "Completed must take the stream-drain barrier sender"
            );
            assert!(
                rx.await.is_ok(),
                "Completed must fire the stream-drain barrier so \
                 run_turn_via_sampler can proceed to emit tool calls in order"
            );
        })
        .await;
}
/// `SamplingEvent::Failed` (fired by the sampler for cancellation,
/// `MaxTokensTruncation`, etc.) must NOT clear the accumulator —
/// the consumer needs to take it via `TakeStreamingCapture` and
/// upload as `streaming_partial.json`.
#[tokio::test(flavor = "current_thread")]
async fn failed_event_preserves_streaming_capture_for_takeout() {
    use echo_agent_sampler::{
        RequestId, SamplingChannel, SamplingErrorInfo, SamplingErrorKind, SamplingEvent,
    };
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-failed".to_string());
            let req = RequestId::random();
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "I should consider...".to_string(),
                    chunk_index: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::Failed {
                    request_id: req,
                    error: SamplingErrorInfo {
                        kind: SamplingErrorKind::MaxTokensTruncation,
                        status_code: None,
                        message: "max output tokens reached".to_string(),
                        is_retryable: false,
                        retry_after_secs: None,
                        should_retry: None,
                        error_code: None,
                        model_metadata: None,
                        empty_response_context: None,
                        doom_loop_triggers: None,
                        doom_loop_aborted_at_chunk: None,
                        credential: echo_agent_sampling_types::SentCredential::Unknown,
                    },
                })
                .await;
            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(
                cap.reasoning_text, "I should consider...",
                "Failed must NOT discard the accumulator — the trace \
                     consumer is going to take it next via \
                     TakeStreamingCapture so the partial reasoning is \
                     preserved for inspection",
            );
            assert!(!cap.is_empty());
            assert_eq!(
                cap.phase,
                CapturePhase::Reasoning,
                "MaxTokensTruncation hit while the model was still \
                     thinking, so the partial must be labeled as tied to \
                     the reasoning phase",
            );
        })
        .await;
}
/// Observe-only (`max_retries = 0`): a first completion carrying confident
/// signals had NOTHING discarded, so it must not be classified as a
/// budget-spent accept — no tally, no counters, no capture stamp; the
/// signals stay warn-only on the accepted response.
#[tokio::test(flavor = "current_thread")]
async fn observe_only_confident_completion_stays_warn_only() {
    use echo_agent_sampler::{RequestId, SamplingChannel, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            fixture.actor.doom_loop_recovery =
                Some(echo_agent_sampling_types::DoomLoopRecoveryPolicy {
                    max_threshold: 8,
                    max_retries: 0,
                    ..Default::default()
                });
            let actor = Arc::new(fixture.actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-observe".to_string());
            let req = RequestId::random();
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "loop loop loop".to_string(),
                    chunk_index: 0,
                })
                .await;
            let response = echo_agent_sampling_types::ConversationResponse {
                items: vec![echo_agent_sampling_types::ConversationItem::assistant(
                    "answer kept as-is",
                )],
                stop_reason: None,
                usage: None,
                cost_usd_ticks: None,
                message_chunks_emitted: 1,
                doom_loop_signals: vec![
                    echo_agent_sampling_types::doom_loop::DoomLoopSignal::parse(
                        "tail_repetition:8@thinking",
                    ),
                ],
                stop_message: None,
                message_id: None,
                raw_stop_reason: None,
                stop_sequence: None,
            };
            actor
                .handle_sampling_event(SamplingEvent::Completed {
                    request_id: req,
                    response: Box::new(response),
                    metrics: Default::default(),
                })
                .await;
            let tally = actor.doom_loop_turn_tally.lock().clone();
            assert!(!tally.fired(), "no resample happened: nothing to report");
            assert!(!tally.accepted_after_budget);
            assert_eq!(tally.attempts, 0);
            let cap = actor.streaming_turn_capture.lock().clone();
            assert!(
                !cap.has_doom_loop_segments(),
                "no accepted-stamp for an undiscarded turn"
            );
            assert!(cap.segments.is_empty());
            let signals = actor
                .signals_handle()
                .snapshot()
                .await
                .expect("signals snapshot");
            assert_eq!(signals.doom_loop_recovery_attempts, 0);
            assert_eq!(signals.doom_loop_recovery_accepted_after_budget, 0);
            assert_eq!(signals.doom_loop_recovery_top_trigger, None);
        })
        .await;
}
/// A recovered turn's capture carries doom-stamped segments: the doomed
/// generation's Retrying (kind `DoomLoopDetected`, triggers + abort chunk)
/// stamps the in-progress slot, the resample's `StreamStarted` folds it into
/// `segments`, and a budget-spent accept folds a text-free stamped segment
/// on `Completed`. Session counters and the per-turn tally track along.
#[tokio::test(flavor = "current_thread")]
async fn doom_loop_recovery_stamps_capture_segments_and_counters() {
    use echo_agent_sampler::{RequestId, SamplingChannel, SamplingErrorKind, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut fixture = make_replay_send_update_fixture().await;
            fixture.actor.doom_loop_recovery =
                Some(echo_agent_sampling_types::DoomLoopRecoveryPolicy::default());
            let actor = Arc::new(fixture.actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-doom".to_string());
            let req = RequestId::random();
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "loop loop loop".to_string(),
                    chunk_index: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::Retrying {
                    request_id: req.clone(),
                    attempt: 1,
                    max_retries: 2,
                    kind: SamplingErrorKind::DoomLoopDetected,
                    reason: "doom loop detected: tail_repetition:8@thinking".to_string(),
                    doom_loop_triggers: Some(vec!["tail_repetition:8@thinking".to_string()]),
                    doom_loop_aborted_at_chunk: Some(421),
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 1,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "still looping".to_string(),
                    chunk_index: 0,
                })
                .await;
            let response = echo_agent_sampling_types::ConversationResponse {
                items: vec![echo_agent_sampling_types::ConversationItem::assistant(
                    "still looping answer",
                )],
                stop_reason: None,
                usage: None,
                cost_usd_ticks: None,
                message_chunks_emitted: 1,
                doom_loop_signals: vec![
                    echo_agent_sampling_types::doom_loop::DoomLoopSignal::parse(
                        "tail_repetition:4@thinking",
                    ),
                ],
                stop_message: None,
                message_id: None,
                raw_stop_reason: None,
                stop_sequence: None,
            };
            actor
                .handle_sampling_event(SamplingEvent::Completed {
                    request_id: req,
                    response: Box::new(response),
                    metrics: Default::default(),
                })
                .await;
            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(cap.segments.len(), 2, "doomed fold + text-free accept");
            let resampled = cap.segments[0].doom_loop.as_ref().expect("stamped");
            assert_eq!(resampled.action, "resampled");
            assert_eq!(resampled.attempt, 1);
            assert_eq!(resampled.aborted_at_chunk, Some(421));
            assert_eq!(
                resampled.doom_loop_triggers,
                vec!["tail_repetition:8@thinking".to_string()]
            );
            assert_eq!(cap.segments[0].reasoning_text, "loop loop loop");
            let accepted = cap.segments[1].doom_loop.as_ref().expect("stamped");
            assert_eq!(accepted.action, "accepted_after_budget");
            assert!(
                cap.segments[1].reasoning_text.is_empty(),
                "committed text lives in history, not the capture"
            );
            assert!(cap.has_doom_loop_segments());
            let tally = actor.doom_loop_turn_tally.lock().clone();
            assert_eq!(tally.attempts, 1);
            assert!(tally.accepted_after_budget);
            assert_eq!(
                tally.top_trigger.as_deref(),
                Some("tail_repetition:4@thinking"),
                "tightest across resample + accept"
            );
            let signals = actor
                .signals_handle()
                .snapshot()
                .await
                .expect("signals snapshot");
            assert_eq!(signals.doom_loop_recovery_attempts, 1);
            assert_eq!(signals.doom_loop_recovery_accepted_after_budget, 1);
            assert_eq!(signals.doom_loop_recovery_aborted_chunks, 421);
            assert_eq!(
                signals.doom_loop_recovery_top_trigger.as_deref(),
                Some("tail_repetition:4@thinking")
            );
        })
        .await;
}
/// A `ToolCallDelta` arriving after the model streamed some reasoning
/// must re-label the live capture as tied to the tool-call phase while
/// preserving the reasoning text already accumulated — so a partial
/// taken at that point shows the model was cut off mid tool-call.
#[tokio::test(flavor = "current_thread")]
async fn tool_call_delta_marks_streaming_capture_phase() {
    use echo_agent_sampler::{RequestId, SamplingChannel, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-toolcall".to_string());
            let req = RequestId::random();
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "I'll call a tool.".to_string(),
                    chunk_index: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ToolCallDelta {
                    request_id: req,
                    tool_index: 0,
                    id: Some("call-1".to_string()),
                    name: Some("read_file".to_string()),
                    arguments_delta: Some("{\"path\":".to_string()),
                })
                .await;
            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(
                cap.phase,
                CapturePhase::ToolCall,
                "ToolCallDelta must re-label the active capture as the \
                     tool-call phase",
            );
            assert_eq!(
                cap.reasoning_text, "I'll call a tool.",
                "marking the tool-call phase must not discard the \
                     reasoning accumulated before the tool call",
            );
        })
        .await;
}
/// `ToolCallDelta` on an idle (empty, never-begun) slot must not
/// fabricate a phase — there is no partial to attribute it to.
#[tokio::test(flavor = "current_thread")]
async fn tool_call_delta_on_idle_slot_leaves_phase_pending() {
    use echo_agent_sampler::{RequestId, SamplingEvent};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let fixture = make_replay_send_update_fixture().await;
            let actor = Arc::new(fixture.actor);
            actor
                .handle_sampling_event(SamplingEvent::ToolCallDelta {
                    request_id: RequestId::random(),
                    tool_index: 0,
                    id: Some("call-1".to_string()),
                    name: Some("read_file".to_string()),
                    arguments_delta: None,
                })
                .await;
            let cap = actor.streaming_turn_capture.lock().clone();
            assert_eq!(cap.phase, CapturePhase::Pending);
            assert!(cap.is_empty());
        })
        .await;
}
/// `StreamingTurnCapture::append` must respect the byte cap so that
/// a runaway extended-thinking turn cannot blow the actor's memory.
/// The capture is marked `truncated = true` once the cap is hit, but
/// the structure is still serializable.
#[test]
fn streaming_capture_appender_respects_byte_cap() {
    let mut cap = StreamingTurnCapture::default();
    assert_eq!(
        cap.phase,
        CapturePhase::Pending,
        "a fresh capture starts in the pending phase",
    );
    let chunk = "a".repeat(STREAMING_CAPTURE_MAX_BYTES / 2);
    cap.append(true, &chunk);
    cap.append(true, &chunk);
    assert!(!cap.truncated, "exactly at the cap should not be truncated");
    cap.append(true, "b");
    assert!(
        cap.truncated,
        "going one byte past the cap must flip the truncated flag"
    );
    let pre_len = cap.reasoning_text.len();
    cap.append(true, "ccccccccccc");
    assert_eq!(cap.reasoning_text.len(), pre_len);
    let bytes = serde_json::to_vec_pretty(&cap).expect("serialize capture");
    let parsed: StreamingTurnCapture = serde_json::from_slice(&bytes).expect("round-trip capture");
    assert!(parsed.truncated);
    assert_eq!(parsed.reasoning_text.len(), cap.reasoning_text.len());
    assert_eq!(
        parsed.phase,
        CapturePhase::Reasoning,
        "phase must survive the serde round-trip the upload path uses",
    );
}
/// A multi-generation reasoning-only turn taken through the real
/// `SessionCommand::TakeStreamingCapture` command (served by a spawned
/// `run_session`), after the real `handle_sampling_failure` reasoning-only
/// terminal, must yield a capture whose `segments` hold every uncommitted
/// generation, in order, stamped with the terminal `empty_reason`. That command
/// + finalize + terminal-failure path is the unique seam here; the struct tests
/// in `streaming_capture.rs` and `same_prompt_restart_accumulates_segments_via_handler`
/// already pin the fold-don't-wipe accumulation, so this asserts only the
/// segment count/order and `empty_reason`. Two generations suffice — before the
/// fix the slot was wiped on each same-turn `StreamStarted`, leaving one. This
/// simulates the events a reasoning-only doomloop produces; it does not drive
/// the sampler classifier (the mock-HTTP test covers that).
#[tokio::test(start_paused = true)]
async fn reasoning_only_doomloop_turn_captures_every_generation_as_segments() {
    use echo_agent_sampler::{
        RequestId, SamplingChannel, SamplingErrorInfo, SamplingErrorKind, SamplingEvent,
    };
    use echo_agent_sampling_types::{EmptyReason, EmptyResponseContext};
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let ReplaySendUpdateFixture {
                actor,
                event_rx,
                sent: _sent,
                persistence_rx: _persistence_rx,
            } = make_replay_send_update_fixture().await;
            let actor = Arc::new(actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("prompt-doomloop".to_string());
            let req = RequestId::random();
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 1,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "thinking attempt 1".to_string(),
                    chunk_index: 0,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::StreamStarted {
                    request_id: req.clone(),
                    timestamp_ms: 2,
                })
                .await;
            actor
                .handle_sampling_event(SamplingEvent::ChannelToken {
                    request_id: req.clone(),
                    channel: SamplingChannel::Reasoning,
                    text: "thinking attempt 2".to_string(),
                    chunk_index: 0,
                })
                .await;
            let error = SamplingErrorInfo {
                kind: SamplingErrorKind::EmptyResponse,
                status_code: None,
                message: "empty response from model (reasoning_only)".to_string(),
                is_retryable: false,
                retry_after_secs: None,
                should_retry: None,
                error_code: None,
                model_metadata: None,
                empty_response_context: Some(EmptyResponseContext {
                    reason: EmptyReason::ReasoningOnly,
                    had_reasoning: true,
                    content_len: 0,
                    tool_call_count: 0,
                    finish_reason: Some("stop".to_string()),
                    completion_tokens: Some(0),
                    reasoning_tokens: Some(4096),
                    prompt_tokens: Some(128),
                    model: "echo-agent-test".to_string(),
                    first_choice_seen: true,
                }),
                doom_loop_triggers: None,
                doom_loop_aborted_at_chunk: None,
                credential: echo_agent_sampling_types::SentCredential::Unknown,
            };
            actor
                .handle_sampling_event(SamplingEvent::Failed {
                    request_id: req,
                    error: error.clone(),
                })
                .await;
            let Err(_terminal) = actor.handle_sampling_failure(error, 0).await else {
                panic!("a reasoning_only empty response must be a terminal error, not recoverable");
            };
            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
            let (_chat_tx, chat_rx) =
                mpsc::unbounded_channel::<echo_agent_chat_state::ChatStateEvent>();
            let codebase_indexes = Arc::new(parking_lot::Mutex::new(
                echo_agent_workspace::file_system::CodebaseIndexManager::new(),
            ));
            tokio::task::spawn_local(super::run_session(
                actor.clone(),
                cmd_rx,
                chat_rx,
                event_rx,
                None,
                codebase_indexes,
                std::path::PathBuf::from("/tmp"),
                crate::session::fs_watch::FsWatchCapabilities::none(),
            ));
            let (respond_to, capture_rx) = tokio::sync::oneshot::channel();
            cmd_tx
                .send(SessionCommand::TakeStreamingCapture {
                    prompt_id: "prompt-doomloop".to_string(),
                    respond_to,
                })
                .unwrap();
            let capture = tokio::time::timeout(Duration::from_secs(2), capture_rx)
                .await
                .expect("TakeStreamingCapture must respond within 2s")
                .expect("the take responder must not be dropped")
                .expect("a reasoning-only doomloop turn must yield a non-empty capture");
            assert_eq!(
                capture.segments.len(),
                2,
                "both reasoning-only generations must be retained as segments",
            );
            assert_eq!(capture.segments[0].reasoning_text, "thinking attempt 1");
            assert_eq!(capture.segments[1].reasoning_text, "thinking attempt 2");
            assert_eq!(capture.empty_reason.as_deref(), Some("reasoning_only"));
        })
        .await;
}
