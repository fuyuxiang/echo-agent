use super::*;
use echo_agent_tools::computer::types::TaskKind;
use echo_agent_tools::types::TaskSnapshot;

/// Drive the admission handshake inline so receiver assertions observe the
/// bridge's command order without racing a detached proxy task.
async fn handle_notification_with_admission(
    config: &NotificationBridgeConfig,
    notification: ToolNotification,
    offsets: &mut HashMap<String, usize>,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
    accepted: bool,
) {
    let notification = handle_notification(config, notification, offsets);
    tokio::pin!(notification);

    let mut command = tokio::select! {
        _ = &mut notification => panic!("notification completed before requesting admission"),
        command = cmd_rx.recv() => command.expect("expected task-wake prompt"),
    };
    let SessionCommand::Prompt { admission, .. } = &mut command else {
        panic!("expected task-wake prompt");
    };
    admission
        .take()
        .expect("expected task-wake admission request")
        .respond_to
        .send(accepted)
        .expect("notification must still be awaiting admission");
    config
        .session_cmd_tx
        .send(command)
        .expect("test command receiver must remain open");
    notification.await;
}

fn make_test_config() -> (
    NotificationBridgeConfig,
    mpsc::UnboundedReceiver<SessionCommand>,
) {
    let (config, _gateway_rx, _persistence_rx, session_cmd_rx) = make_test_config_full();
    (config, session_cmd_rx)
}

#[allow(clippy::type_complexity)]
fn make_test_config_full() -> (
    NotificationBridgeConfig,
    mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
    mpsc::UnboundedReceiver<PersistenceMsg>,
    mpsc::UnboundedReceiver<SessionCommand>,
) {
    make_test_config_full_raw()
}

#[allow(clippy::type_complexity)]
fn make_test_config_full_raw() -> (
    NotificationBridgeConfig,
    mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
    mpsc::UnboundedReceiver<PersistenceMsg>,
    mpsc::UnboundedReceiver<SessionCommand>,
) {
    let (gateway_tx, gateway_rx) = mpsc::unbounded_channel();
    let gateway = echo_agent_acp::AcpAgentGatewaySender::new(gateway_tx);
    let (session_cmd_tx, session_cmd_rx) = mpsc::unbounded_channel();
    let (persistence_tx, persistence_rx) = mpsc::unbounded_channel();
    let config = NotificationBridgeConfig {
        gateway,
        session_id: acp::SessionId::new("test-session"),
        hunk_tracker_handle: HunkTrackerHandle::noop(),
        file_state_tracker: Arc::new(FileStateTracker::new()),
        prompt_index: Arc::new(TokioMutex::new(0)),
        cwd: PathBuf::from("/tmp"),
        gateway_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        persistence: PersistenceHandle::from_sender_for_test(persistence_tx),
        incremental_bash_output: false,
        plan_mode: Arc::new(parking_lot::Mutex::new(
            crate::session::plan_mode::PlanModeTracker::new(PathBuf::from("/tmp/test-session")),
        )),
        current_prompt_mode: Arc::new(parking_lot::Mutex::new(
            crate::session::plan_mode::PromptMode::Agent,
        )),
        turn_prompt_mode: Arc::new(parking_lot::Mutex::new(
            crate::session::plan_mode::PromptMode::Agent,
        )),
        session_cmd_tx,
        task_completion_reservations:
            echo_agent_tools::reminders::task_completion::TaskCompletionReservations::default(),
        task_wake_suppressed:
            echo_agent_tools::reminders::task_completion::TaskWakeSuppressed::default(),
        synthetic_trace_tx: Arc::new(std::sync::Mutex::new(None)),
        task_output_tool_name: Arc::new(std::sync::OnceLock::new()),
        read_tool_name: Arc::new(std::sync::OnceLock::new()),
        auto_wake_enabled: true,
        queue_exit_reminder_on_approved_exit: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        goal_loop_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    (config, gateway_rx, persistence_rx, session_cmd_rx)
}

fn make_task_snapshot(task_id: &str, kind: TaskKind) -> TaskSnapshot {
    TaskSnapshot {
        task_id: task_id.into(),
        command: "echo test".into(),
        display_command: None,
        cwd: String::new(),
        start_time: std::time::SystemTime::now(),
        end_time: Some(std::time::SystemTime::now()),
        output: String::new(),
        output_file: PathBuf::new(),
        truncated: false,
        exit_code: Some(0),
        signal: None,
        completed: true,
        kind,
        block_waited: false,
        explicitly_killed: false,
        kill_result_delivered: false,
        owner_session_id: None,
        description: None,
        is_backgrounded: false,
        output_total_bytes: 0,
    }
}

#[tokio::test]
async fn bash_task_completed_injects_bash_task_completed_source() {
    let (config, mut cmd_rx) = make_test_config();
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    let snapshot = make_task_snapshot("bg-123", TaskKind::Bash);
    let notification = ToolNotification::TaskCompleted(snapshot);
    let mut offsets = HashMap::new();

    handle_notification_with_admission(&config, notification, &mut offsets, &mut cmd_rx, true)
        .await;

    let command = cmd_rx.try_recv().expect("expected Prompt");
    match command {
        SessionCommand::Prompt {
            prompt_id,
            prompt_blocks,
            verbatim,
            ..
        } => {
            assert!(prompt_id.starts_with("task-completed-"));
            assert!(verbatim);
            let text = match &prompt_blocks[0] {
                acp::ContentBlock::Text(t) => &t.text,
                _ => panic!("expected text block"),
            };
            assert!(text.contains("bg-123"));
            assert!(text.contains("exit code: 0"));
            assert!(text.contains(r#"get_command_or_subagent_output("bg-123")"#));
            assert!(!text.contains(r#"get_task_output("bg-123")"#));
        }
        _ => panic!("expected Prompt"),
    }

    let cmd3 = cmd_rx
        .try_recv()
        .expect("expected DispatchNotificationHook for task_complete");
    match cmd3 {
        SessionCommand::DispatchNotificationHook {
            notification_type,
            message,
            ..
        } => {
            assert_eq!(notification_type, "task_complete");
            assert_eq!(
                message.as_deref(),
                Some("Background task completed: bg-123")
            );
        }
        _ => panic!("expected DispatchNotificationHook"),
    }
}

/// Gap 1: while a goal loop is active, a completed background bash task
/// must NOT fire the synthetic auto-wake prompt — an async "task completed"
/// wake mid-goal derails a weak model. It must also NOT be marked
/// reserved (so surface 2's `TaskCompletionReminder` is free to
/// drain it). The pager's `echo.agent/task_completed` notification still fires.
#[tokio::test]
async fn bash_task_completed_suppresses_auto_wake_during_goal_loop() {
    let (config, mut gateway_rx, _persistence_rx, mut cmd_rx) = make_test_config_full();
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    config
        .goal_loop_active
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let snapshot = make_task_snapshot("bg-goal", TaskKind::Bash);
    let mut offsets = HashMap::new();

    handle_notification(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
    )
    .await;

    // No synthetic prompt / CopyFile / InjectNotification while the goal
    // loop drives the turn — only the Notification hook dispatch.
    match cmd_rx
        .try_recv()
        .expect("expected DispatchNotificationHook for task_complete")
    {
        SessionCommand::DispatchNotificationHook {
            notification_type, ..
        } => assert_eq!(notification_type, "task_complete"),
        _ => panic!("unexpected session command"),
    }
    assert!(
        cmd_rx.try_recv().is_err(),
        "goal-loop-active bash completion must not inject auto-wake commands"
    );
    // Not marked reserved: surface 2 must be free to drain it.
    assert!(
        config.task_completion_reservations.snapshot().is_empty(),
        "goal-loop-active completion must not be marked reserved"
    );
    // The pager UI notification must still be emitted.
    let mut found_ext = false;
    while let Ok(msg) = gateway_rx.try_recv() {
        if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg
            && args.request.method.as_ref() == "echo.agent/task_completed"
        {
            found_ext = true;
        }
    }
    assert!(
        found_ext,
        "echo.agent/task_completed ExtNotification must still be sent for UI"
    );
}

/// Gap 1 (preserve non-goal behavior): with the goal loop inactive — the
/// default for a normal session — a completed bash task DOES fire the
/// synthetic auto-wake prompt AND is marked reserved so surface
/// 2 suppresses the duplicate reminder.
#[tokio::test]
async fn bash_task_completed_auto_wakes_and_reserves_without_goal_loop() {
    let (config, mut cmd_rx) = make_test_config();
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    let snapshot = make_task_snapshot("bg-normal", TaskKind::Bash);
    let mut offsets = HashMap::new();

    handle_notification_with_admission(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
        &mut cmd_rx,
        true,
    )
    .await;

    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(SessionCommand::Prompt { .. })
    ));
    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(SessionCommand::DispatchNotificationHook { .. })
    ));
    assert_eq!(
        config.task_completion_reservations.snapshot(),
        vec!["bg-normal".to_string()],
    );
}

fn task_completed_will_wake(
    gateway_rx: &mut mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
) -> Option<bool> {
    while let Ok(msg) = gateway_rx.try_recv() {
        if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg
            && args.request.method.as_ref() == "echo.agent/task_completed"
        {
            let v: serde_json::Value = serde_json::from_str(args.request.params.get()).ok()?;
            return v["update"]["will_wake"].as_bool();
        }
    }
    None
}

/// The completion notification carries the wake verdict — the pager keys
/// its between-turns status line on it (skip when a wake response
/// follows, emit when nothing else will mark the moment).
#[tokio::test]
async fn task_completed_notification_stamps_will_wake() {
    let (config, mut gateway_rx, _persistence_rx, mut cmd_rx) = make_test_config_full();
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    let (trace_tx, mut trace_rx) = mpsc::unbounded_channel();
    *config
        .synthetic_trace_tx
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(trace_tx);
    let mut offsets = HashMap::new();
    handle_notification_with_admission(
        &config,
        ToolNotification::TaskCompleted(make_task_snapshot("bg-wake", TaskKind::Bash)),
        &mut offsets,
        &mut cmd_rx,
        true,
    )
    .await;
    assert!(matches!(
        cmd_rx.recv().await,
        Some(SessionCommand::Prompt { .. })
    ));
    match cmd_rx.recv().await {
        Some(SessionCommand::CopyFile { respond_to }) => drop(respond_to),
        _ => panic!("trace copy must follow accepted prompt admission"),
    }
    assert_eq!(
        task_completed_will_wake(&mut gateway_rx),
        Some(true),
        "an auto-woken completion must stamp will_wake: true"
    );
    assert!(
        trace_rx.try_recv().is_ok(),
        "accepted admission must request a synthetic-turn trace"
    );

    let (config, mut gateway_rx, mut persistence_rx, mut cmd_rx) = make_test_config_full();
    let (trace_tx, mut trace_rx) = mpsc::unbounded_channel();
    *config
        .synthetic_trace_tx
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(trace_tx);
    let mut offsets = HashMap::new();
    handle_notification_with_admission(
        &config,
        ToolNotification::TaskCompleted(make_task_snapshot("bg-declined", TaskKind::Bash)),
        &mut offsets,
        &mut cmd_rx,
        false,
    )
    .await;
    assert_eq!(
        task_completed_will_wake(&mut gateway_rx),
        Some(false),
        "an actor-declined completion must stamp will_wake: false"
    );
    assert!(
        config.task_completion_reservations.contains("bg-declined"),
        "the actor owns reservation release after queuing the deferred fallback"
    );
    assert!(
        trace_rx.try_recv().is_err(),
        "declined admission must not request a synthetic-turn trace"
    );
    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(SessionCommand::Prompt { .. })
    ));
    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(SessionCommand::DispatchNotificationHook { .. })
    ));
    let mut persisted = false;
    while let Ok(message) = persistence_rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(update)) =
            message
            && matches!(
                &update.update,
                crate::extensions::notification::SessionUpdate::TaskCompleted { .. }
            )
        {
            persisted = true;
        }
    }
    assert!(
        persisted,
        "declined admission must still persist echo.agent/task_completed"
    );
}

#[tokio::test(start_paused = true)]
async fn stalled_admission_is_bounded_and_task_completion_still_emits() {
    let (config, mut gateway_rx, mut persistence_rx, mut cmd_rx) = make_test_config_full_raw();
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    let mut offsets = HashMap::new();
    let notification = handle_notification(
        &config,
        ToolNotification::TaskCompleted(make_task_snapshot("bg-stalled", TaskKind::Bash)),
        &mut offsets,
    );
    tokio::pin!(notification);

    tokio::select! {
        _ = &mut notification => panic!("admission should still be waiting"),
        command = cmd_rx.recv() => assert!(matches!(command, Some(SessionCommand::Prompt { .. }))),
    }
    tokio::time::advance(TASK_WAKE_ADMISSION_TIMEOUT + std::time::Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    notification.await;

    assert_eq!(task_completed_will_wake(&mut gateway_rx), Some(false));
    assert!(
        config.task_completion_reservations.contains("bg-stalled"),
        "a timed-out admission may still be handled and deferred by the actor"
    );
    let mut persisted_completion = false;
    while let Ok(message) = persistence_rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(update)) =
            message
            && matches!(
                &update.update,
                crate::extensions::notification::SessionUpdate::TaskCompleted { .. }
            )
        {
            persisted_completion = true;
        }
    }
    assert!(persisted_completion);
}

#[tokio::test(start_paused = true)]
async fn timed_out_monitor_admission_queues_one_fallback_and_late_actor_drops_prompt() {
    let (config, mut gateway_rx, _persistence_rx, mut cmd_rx) = make_test_config_full_raw();
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    let mut offsets = HashMap::new();
    let notification = handle_notification(
        &config,
        ToolNotification::TaskCompleted(make_task_snapshot("mon-timeout", TaskKind::Monitor)),
        &mut offsets,
    );
    tokio::pin!(notification);
    let prompt = tokio::select! {
        _ = &mut notification => panic!("admission should still be waiting"),
        command = cmd_rx.recv() => command.expect("prompt command"),
    };
    tokio::time::advance(TASK_WAKE_ADMISSION_TIMEOUT + std::time::Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    notification.await;

    let SessionCommand::Prompt {
        admission: Some(admission),
        respond_to,
        ..
    } = prompt
    else {
        panic!("expected task wake prompt");
    };
    assert!(matches!(
        admission.fallback.source,
        NotificationSource::MonitorCompleted { ref task_id } if task_id == "mon-timeout"
    ));
    assert!(admission.respond_to.send(true).is_err());
    let _ = respond_to.send(Ok(crate::session::commands::PromptTurnOk {
        stop_reason: acp::StopReason::Cancelled,
        total_tokens: 0,
        turn_snapshot: None,
        completion_kind: crate::session::commands::PromptCompletionKind::RemovedFromQueue,
        structured_output: None,
        usage: None,
        tool_overrides: None,
    }));

    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(SessionCommand::DispatchNotificationHook { .. })
    ));
    assert!(cmd_rx.try_recv().is_err());
    assert_eq!(task_completed_will_wake(&mut gateway_rx), Some(false));
    assert!(
        config.task_completion_reservations.contains("mon-timeout"),
        "the late actor fallback retains the reservation until user delivery"
    );
}

#[tokio::test]
async fn task_completed_stamps_will_wake_false_when_session_channel_closed() {
    let (config, mut gateway_rx, _persistence_rx, cmd_rx) = make_test_config_full_raw();
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    drop(cmd_rx);
    config
        .task_completion_reservations
        .reserve("bg-dead".into());
    let mut offsets = HashMap::new();
    handle_notification(
        &config,
        ToolNotification::TaskCompleted(make_task_snapshot("bg-dead", TaskKind::Bash)),
        &mut offsets,
    )
    .await;
    assert_eq!(
        task_completed_will_wake(&mut gateway_rx),
        Some(false),
        "a completion whose wake prompt could not be enqueued must stamp will_wake: false"
    );
    assert!(config.task_completion_reservations.contains("bg-dead"));
    config.task_completion_reservations.release("bg-dead");
    assert!(!config.task_completion_reservations.contains("bg-dead"));
}

/// Gap 1 (adjacent branch): the goal-loop arm sits BEFORE the
/// `auto_wake_enabled == false` `InjectNotification` fallback, so an
/// auto-wake-DISABLED completion mid-goal must also be suppressed — it must
/// NOT fall through to the idle-gated `InjectNotification`. Guards against a
/// future reorder that would leak a mid-goal notification.
#[tokio::test]
async fn bash_task_completed_auto_wake_disabled_still_suppressed_during_goal_loop() {
    let (mut config, mut cmd_rx) = make_test_config();
    config.auto_wake_enabled = false;
    config
        .goal_loop_active
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let snapshot = make_task_snapshot("bg-disabled-goal", TaskKind::Bash);
    let mut offsets = HashMap::new();

    handle_notification(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
    )
    .await;

    // No InjectNotification during the goal loop, even with auto-wake disabled.
    match cmd_rx
        .try_recv()
        .expect("expected DispatchNotificationHook for task_complete")
    {
        SessionCommand::DispatchNotificationHook {
            notification_type, ..
        } => assert_eq!(notification_type, "task_complete"),
        _ => panic!("unexpected session command"),
    }
    assert!(
        cmd_rx.try_recv().is_err(),
        "goal-loop-active completion must not InjectNotification with auto-wake disabled"
    );
    assert!(config.task_completion_reservations.snapshot().is_empty());
}

/// Natural monitor exit (including exit code 0) must immediate-auto-wake
/// the same way bash does — not only via the idle-gated MonitorEvent path.
/// Also drops queued MonitorEvents so a second NotificationDrain turn is
/// not started for the same completion.
#[tokio::test]
async fn monitor_task_completed_auto_wakes_with_monitor_ended_message() {
    let (config, mut cmd_rx) = make_test_config();
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    let mut snapshot = make_task_snapshot("mon-456", TaskKind::Monitor);
    snapshot.display_command = Some("[monitor] watch deploy".into());
    snapshot.command = "tail -f deploy.log".into();
    snapshot.exit_code = Some(0);
    let mut offsets = HashMap::new();

    handle_notification_with_admission(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
        &mut cmd_rx,
        true,
    )
    .await;

    let cmd = cmd_rx.try_recv().expect("expected Prompt auto-wake");
    match cmd {
        SessionCommand::Prompt {
            prompt_id,
            prompt_blocks,
            verbatim,
            ..
        } => {
            assert_eq!(prompt_id, "task-completed-mon-456");
            assert!(verbatim);
            let text = match &prompt_blocks[0] {
                acp::ContentBlock::Text(t) => t.text.as_str(),
                _ => panic!("expected text block"),
            };
            assert!(
                text.contains("[monitor ended: exited (code 0)]"),
                "auto-wake must carry the terminal ended wording: {text}"
            );
            assert!(
                text.contains("watch deploy"),
                "auto-wake should include the monitor description: {text}"
            );
            assert!(
                text.contains("get_command_or_subagent_output(\"mon-456\")"),
                "auto-wake should point at the poll tool: {text}"
            );
        }
        _ => panic!("expected Prompt auto-wake for natural monitor exit"),
    }
    match cmd_rx
        .try_recv()
        .expect("accepted monitor wake must drop pipeline notifications")
    {
        SessionCommand::DropMonitorNotifications { task_id } => {
            assert_eq!(task_id, "mon-456");
        }
        _ => panic!("expected DropMonitorNotifications after accepted Prompt"),
    }
    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(SessionCommand::DispatchNotificationHook { .. })
    ));
    assert_eq!(
        config.task_completion_reservations.snapshot(),
        vec!["mon-456".to_string()],
    );
}

#[tokio::test]
async fn declined_quiet_monitor_wake_queues_canonical_deferred_completion() {
    let (config, _gateway_rx, mut persistence_rx, mut cmd_rx) = make_test_config_full();
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    let mut offsets = HashMap::new();

    handle_notification_with_admission(
        &config,
        ToolNotification::TaskCompleted(make_task_snapshot("mon-declined", TaskKind::Monitor)),
        &mut offsets,
        &mut cmd_rx,
        false,
    )
    .await;

    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(SessionCommand::Prompt { .. })
    ));
    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(SessionCommand::DispatchNotificationHook { .. })
    ));
    assert!(cmd_rx.try_recv().is_err());
    let mut persisted_completion = false;
    while let Ok(message) = persistence_rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(update)) =
            message
            && matches!(
                &update.update,
                crate::extensions::notification::SessionUpdate::TaskCompleted { .. }
            )
        {
            persisted_completion = true;
        }
    }
    assert!(persisted_completion);
    assert!(
        config.task_completion_reservations.contains("mon-declined"),
        "the actor owns reservation release after queuing the deferred fallback"
    );
}

/// After TaskCompleted auto-wake reserves the task, late pipeline
/// MonitorEvents must not inject another model-facing notification.
#[tokio::test]
async fn monitor_event_skipped_after_task_completed_auto_wake() {
    let (config, mut cmd_rx) = make_test_config();
    config
        .task_completion_reservations
        .reserve("mon-done".into());
    let mut offsets = HashMap::new();

    handle_notification(
        &config,
        ToolNotification::MonitorEvent(echo_agent_tools::notification::types::MonitorEvent {
            task_id: "mon-done".into(),
            description: "short exit".into(),
            event_text: "<monitor-event>done</monitor-event>".into(),
            raw_text: "done".into(),
            owner_session_id: Some("test-session".into()),
        }),
        &mut offsets,
    )
    .await;

    // No InjectNotification — only the TaskCompleted wake should talk to the model.
    assert!(
        cmd_rx.try_recv().is_err(),
        "post-auto-wake MonitorEvent must not InjectNotification"
    );
}

/// Model-tool kill of a monitor still skips auto-wake — the model already
/// got the kill_task tool result.
#[tokio::test]
async fn monitor_explicitly_killed_skips_auto_wake() {
    let (config, mut gateway_rx, _persistence_rx, mut cmd_rx) = make_test_config_full();
    let mut snapshot = make_task_snapshot("mon-killed", TaskKind::Monitor);
    snapshot.explicitly_killed = true;
    snapshot.kill_result_delivered = true;
    let mut offsets = HashMap::new();

    handle_notification(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
    )
    .await;

    match cmd_rx
        .try_recv()
        .expect("expected DispatchNotificationHook for task_complete")
    {
        SessionCommand::DispatchNotificationHook {
            notification_type, ..
        } => assert_eq!(notification_type, "task_complete"),
        _ => panic!("unexpected session command"),
    }
    assert!(
        cmd_rx.try_recv().is_err(),
        "model-tool-killed monitor must not auto-wake"
    );
    assert!(config.task_completion_reservations.snapshot().is_empty());
    assert_eq!(
        task_completed_will_wake(&mut gateway_rx),
        Some(false),
        "delivered monitor kill must stamp will_wake: false"
    );
}

#[tokio::test]
async fn ui_killed_monitor_auto_wakes_and_tells_model_not_to_restart() {
    let (config, mut gateway_rx, _persistence_rx, mut cmd_rx) = make_test_config_full();
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    let mut snapshot = make_task_snapshot("mon-ui-killed", TaskKind::Monitor);
    snapshot.explicitly_killed = true;
    snapshot.kill_result_delivered = false;
    snapshot.display_command = Some("[monitor] watch deploy".into());
    let mut offsets = HashMap::new();

    handle_notification_with_admission(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
        &mut cmd_rx,
        true,
    )
    .await;

    let command = cmd_rx.try_recv().expect("expected Prompt");
    match command {
        SessionCommand::Prompt { prompt_blocks, .. } => {
            let text = match &prompt_blocks[0] {
                acp::ContentBlock::Text(t) => &t.text,
                _ => panic!("expected text block"),
            };
            assert!(
                text.contains("\nThis task was killed by the user — do not restart it.\n"),
                "UI-killed monitor wake must put the do-not-restart notice on its own line: {text}"
            );
        }
        _ => panic!("expected Prompt"),
    }
    assert_eq!(
        task_completed_will_wake(&mut gateway_rx),
        Some(true),
        "UI/Stop monitor kill with no delivered result must stamp will_wake: true"
    );
}

/// Goal-loop suppression applies to monitor completions too.
#[tokio::test]
async fn monitor_task_completed_suppressed_during_goal_loop() {
    let (config, mut cmd_rx) = make_test_config();
    config
        .goal_loop_active
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let snapshot = make_task_snapshot("mon-goal", TaskKind::Monitor);
    let mut offsets = HashMap::new();

    handle_notification(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
    )
    .await;

    match cmd_rx
        .try_recv()
        .expect("expected DispatchNotificationHook for task_complete")
    {
        SessionCommand::DispatchNotificationHook {
            notification_type, ..
        } => assert_eq!(notification_type, "task_complete"),
        _ => panic!("unexpected session command"),
    }
    assert!(
        cmd_rx.try_recv().is_err(),
        "goal-loop-active monitor completion must not auto-wake"
    );
    assert!(config.task_completion_reservations.snapshot().is_empty());
}

#[tokio::test]
async fn scheduled_task_created_is_persisted() {
    // A `/loop` create must be persisted (like TaskBackgrounded) so a
    // second terminal that resumes the session restores the loop from
    // replay — otherwise it stays invisible until the loop next fires.
    let (config, _gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();
    let notification = ToolNotification::ScheduledTaskCreated(
        echo_agent_tools::notification::types::ScheduledTaskCreated {
            task_id: "loop-1".into(),
            prompt: "check deploy".into(),
            human_schedule: "every 5 minutes".into(),
            next_fire_at: Some("2026-01-01T00:00:00Z".into()),
            generation: "generation-a".into(),
            revision: 1,
        },
    );
    let mut offsets = HashMap::new();

    handle_notification(&config, notification, &mut offsets).await;

    let msg = persistence_rx
        .try_recv()
        .expect("scheduled_task_created must be persisted");
    match msg {
        PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(notif)) => {
            assert!(matches!(
                &notif.update,
                crate::extensions::notification::SessionUpdate::ScheduledTaskCreated { .. }
            ));
            let meta = notif.meta.as_ref().expect("scheduler metadata");
            assert_eq!(meta["echo.agent/schedulerGeneration"], "generation-a");
            assert_eq!(meta["echo.agent/schedulerRevision"], 1);
            assert!(
                notif
                    .meta
                    .as_ref()
                    .and_then(|m| m.get("eventId"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|id| id.starts_with("test-session-")),
                "persisted EchoAgent bridge lines must carry an eventId"
            );
        }
        _ => panic!("expected PersistenceMsg::Update(EchoAgent(ScheduledTaskCreated))"),
    }
}

/// InProgress bash chunks stream live to the TUI but are not persisted —
/// Completed/Failed tool results (emitted on the tool-result path, not this
/// 100ms ticker) remain the replay source of truth.
#[tokio::test]
async fn bash_output_chunk_forwards_live_without_persisting() {
    let (config, mut gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();
    let notification =
        ToolNotification::BashOutputChunk(echo_agent_tools::notification::types::BashOutputChunk {
            base: echo_agent_tools::notification::types::BashNotificationBase {
                tool_call_id: "call-1".into(),
                command: "echo hi".into(),
                output: b"hi\n".to_vec(),
                total_bytes: 3,
                truncated: false,
                cwd: PathBuf::from("/tmp"),
            },
        });
    let mut offsets = HashMap::new();

    handle_notification(&config, notification, &mut offsets).await;

    assert!(
        persistence_rx.try_recv().is_err(),
        "InProgress bash chunks must not be persisted"
    );

    match gateway_rx.try_recv().expect("chunk must be broadcast") {
        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
            assert!(
                args.request
                    .meta
                    .as_ref()
                    .and_then(|m| m.get("eventId"))
                    .is_none(),
                "live-only InProgress must not mint a reconnect cursor eventId"
            );
            match &args.request.update {
                acp::SessionUpdate::ToolCallUpdate(u) => {
                    assert_eq!(u.fields.status, Some(acp::ToolCallStatus::InProgress));
                }
                other => panic!("expected ToolCallUpdate, got {other:?}"),
            }
        }
        other => panic!("expected SessionNotification, got {other:?}"),
    }
}

#[tokio::test]
async fn live_in_progress_event_id_is_not_a_prepare_replay_cursor() {
    let (config, mut gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();
    handle_notification(
        &config,
        ToolNotification::BashOutputChunk(echo_agent_tools::notification::types::BashOutputChunk {
            base: echo_agent_tools::notification::types::BashNotificationBase {
                tool_call_id: "call-cursor".into(),
                command: "echo hi".into(),
                output: b"hi\n".to_vec(),
                total_bytes: 3,
                truncated: false,
                cwd: PathBuf::from("/tmp"),
            },
        }),
        &mut HashMap::new(),
    )
    .await;
    assert!(persistence_rx.try_recv().is_err());
    let live_id = match gateway_rx.try_recv().unwrap() {
        echo_agent_acp::AcpClientMessage::SessionNotification(args) => args
            .request
            .meta
            .as_ref()
            .and_then(|m| m.get("eventId"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        other => panic!("expected SessionNotification, got {other:?}"),
    };
    assert!(live_id.is_none());
    let persisted = crate::session::storage::prepare_replay_lines(
        r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hi"}},"_meta":{"eventId":"s-1"}}}"#,
        live_id.as_deref().or(Some("ghost-live-id")),
    );
    assert!(
        persisted.mark_replay,
        "a non-persisted live id must not resolve as a reconnect cursor"
    );
}

#[tokio::test]
async fn bash_output_chunk_skips_persist_when_gateway_closed() {
    let (config, mut gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();
    config
        .gateway_enabled
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let notification =
        ToolNotification::BashOutputChunk(echo_agent_tools::notification::types::BashOutputChunk {
            base: echo_agent_tools::notification::types::BashNotificationBase {
                tool_call_id: "call-closed".into(),
                command: "echo hi".into(),
                output: b"hi\n".to_vec(),
                total_bytes: 3,
                truncated: false,
                cwd: PathBuf::from("/tmp"),
            },
        });
    let mut offsets = HashMap::new();
    handle_notification(&config, notification, &mut offsets).await;
    assert!(
        persistence_rx.try_recv().is_err(),
        "closed gateway must not persist InProgress bash either"
    );
    assert!(
        gateway_rx.try_recv().is_err(),
        "closed gateway must not forward"
    );
}

#[tokio::test]
async fn scheduled_task_removed_is_persisted() {
    // The deletion must also persist so replay nets out a removed loop
    // instead of resurrecting it from a persisted `created` line.
    let (config, _gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();
    let removed = echo_agent_tools::notification::ScheduledTaskRemoved::new(
        "loop-1".into(),
        echo_agent_tools::notification::ScheduledTaskRemovedReason::Expired,
        "generation-a".into(),
        2,
    );

    handle_scheduled_task_removed(&config, removed, None)
        .await
        .unwrap();

    let msg = persistence_rx
        .try_recv()
        .expect("scheduled_task_removed must be persisted");
    match msg {
        PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(notif)) => {
            assert!(matches!(
                &notif.update,
                crate::extensions::notification::SessionUpdate::ScheduledTaskDeleted {
                    reason: echo_agent_tools::notification::ScheduledTaskRemovedReason::Expired,
                    ..
                }
            ));
            assert!(
                echo_agent_persisted_event_id(&notif).is_some(),
                "the persisted deletion line must be stamped"
            );
            let meta = notif.meta.as_ref().expect("scheduler metadata");
            assert_eq!(meta["echo.agent/schedulerGeneration"], "generation-a");
            assert_eq!(meta["echo.agent/schedulerRevision"], 2);
        }
        _ => panic!("expected PersistenceMsg::Update(EchoAgent(ScheduledTaskDeleted))"),
    }
}

#[tokio::test]
async fn acknowledged_scheduler_removal_appends_before_ack_and_broadcast() {
    let (config, mut gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();
    let removed = echo_agent_tools::notification::ScheduledTaskRemoved::new(
        "loop-ack".into(),
        echo_agent_tools::notification::ScheduledTaskRemovedReason::Deleted,
        "generation-a".into(),
        17,
    );
    let (acknowledgement, mut receipt) = tokio::sync::oneshot::channel::<Result<(), String>>();
    let persistence = async {
        let PersistenceMsg::AppendUpdateDurablyAndAck {
            update: crate::session::storage::SessionUpdate::EchoAgent(notification),
            respond_to,
        } = persistence_rx.recv().await.expect("durable append")
        else {
            panic!("expected durable scheduler tombstone");
        };
        assert_eq!(
            notification.meta.unwrap()["echo.agent/schedulerRevision"],
            17
        );
        assert!(gateway_rx.try_recv().is_err());
        assert!(matches!(
            receipt.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        respond_to.send(Ok(())).unwrap();
        receipt.await.unwrap().unwrap();
    };

    let (result, ()) = tokio::join!(
        handle_scheduled_task_removed(&config, removed, Some(acknowledgement)),
        persistence,
    );
    result.unwrap();
    assert!(matches!(
        gateway_rx.try_recv(),
        Ok(echo_agent_acp::AcpClientMessage::ExtNotification(_))
    ));
}

fn echo_agent_persisted_event_id(
    notif: &crate::extensions::notification::SessionNotification,
) -> Option<String> {
    notif
        .meta
        .as_ref()
        .and_then(|m| m.get("eventId"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Per-site stamp pins for the bridge emitters not covered by the
/// representative chokepoint tests: deleting any one `stamp_event_id`
/// call must fail a test (an id-less persisted line silently disables
/// incremental reconnect for the session).
#[tokio::test]
async fn task_backgrounded_persisted_line_is_stamped() {
    let (config, _gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();
    let notification = ToolNotification::BashExecutionBackgrounded(
        echo_agent_tools::notification::types::BashExecutionBackgrounded {
            base: echo_agent_tools::notification::types::BashNotificationBase {
                tool_call_id: "call-bg".into(),
                command: "sleep 100".into(),
                output: Vec::new(),
                total_bytes: 0,
                truncated: false,
                cwd: PathBuf::from("/tmp"),
            },
            output_file: PathBuf::from("/tmp/out.log"),
            task_id: "task-bg".into(),
            monitor_description: None,
            description: None,
        },
    );
    let mut offsets = HashMap::new();

    handle_notification(&config, notification, &mut offsets).await;

    match persistence_rx.try_recv().expect("must persist") {
        PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(notif)) => {
            assert!(echo_agent_persisted_event_id(&notif).is_some());
        }
        _ => panic!("expected EchoAgent update"),
    }
}

#[tokio::test]
async fn task_completed_persisted_line_is_stamped() {
    let (config, _gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();
    // Monitor kind: persists without the bash auto-wake side effects.
    let snapshot = make_task_snapshot("mon-1", TaskKind::Monitor);
    let mut offsets = HashMap::new();

    handle_notification(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
    )
    .await;

    match persistence_rx.try_recv().expect("must persist") {
        PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(notif)) => {
            assert!(echo_agent_persisted_event_id(&notif).is_some());
        }
        _ => panic!("expected EchoAgent update"),
    }
}

#[tokio::test]
async fn current_mode_update_persisted_line_is_stamped() {
    let (config, _gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();

    emit_current_mode_update(&config, echo_agent_tools::types::SessionMode::Plan).await;

    match persistence_rx.try_recv().expect("must persist") {
        PersistenceMsg::Update(crate::session::storage::SessionUpdate::Acp(notif)) => {
            assert!(matches!(
                notif.update,
                acp::SessionUpdate::CurrentModeUpdate(_)
            ));
            assert!(
                notif
                    .meta
                    .as_ref()
                    .and_then(|m| m.get("eventId"))
                    .and_then(|v| v.as_str())
                    .is_some(),
                "the persisted mode line must be stamped"
            );
        }
        _ => panic!("expected Acp update"),
    }
}

#[test]
fn durable_append_mapping_respects_commit_disposition() {
    assert!(
        durable_append_landed(Err(DurableAppendError::Committed(std::io::Error::other(
            "summary failed"
        ),)))
        .is_ok()
    );
    for failure in [
        DurableAppendError::NotCommitted(std::io::Error::other("append failed")),
        DurableAppendError::AcknowledgementLost(std::io::Error::other("lost")),
    ] {
        assert!(durable_append_landed(Err(failure)).is_err());
    }
}

#[tokio::test]
async fn scheduled_task_fired_is_not_persisted() {
    // `_fired` recurs on every interval; persisting it would grow the
    // updates log without bound. Loops are restored from create/delete, so
    // the fire stays gateway-only (the pager self-heals the entry on a live
    // fire if needed).
    let (config, mut gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();
    let notification = ToolNotification::ScheduledTaskFired(
        echo_agent_tools::notification::types::ScheduledTaskFired {
            task_id: "loop-1".into(),
            prompt: "check deploy".into(),
            human_schedule: "every 5 minutes".into(),
            next_fire_at: Some("2026-01-01T00:00:00Z".into()),
            subagent_id: Some("subagent-1".into()),
            generation: "generation-a".into(),
            revision: 3,
        },
    );
    let mut offsets = HashMap::new();

    handle_notification(&config, notification, &mut offsets).await;

    assert!(
        persistence_rx.try_recv().is_err(),
        "scheduled_task_fired must NOT be persisted (recurring \u{2192} unbounded log growth)"
    );
    let fired = gateway_rx
        .try_recv()
        .expect("scheduled fire must be broadcast");
    let echo_agent_acp::AcpClientMessage::ExtNotification(fired) = fired else {
        panic!("expected scheduler fire notification");
    };
    let value: serde_json::Value = serde_json::from_str(fired.request.params.get()).unwrap();
    assert_eq!(
        value["_meta"]["echo.agent/schedulerGeneration"],
        "generation-a"
    );
    assert_eq!(value["_meta"]["echo.agent/schedulerRevision"], 3);
}

fn make_monitor_event_notification(task_id: &str, owner: Option<&str>) -> ToolNotification {
    ToolNotification::MonitorEvent(echo_agent_tools::notification::types::MonitorEvent {
        task_id: task_id.into(),
        description: "errors in deploy.log".into(),
        event_text: format!("<monitor-event task_id=\"{task_id}\">boom</monitor-event>"),
        raw_text: "boom".into(),
        owner_session_id: owner.map(str::to_string),
    })
}

#[tokio::test]
async fn cross_session_monitor_event_is_dropped() {
    // The bridge belongs to "test-session"; the event is owned by a
    // different session. In leader mode (one agent process, many sessions)
    // this is the cross-session leak: without the owner guard the foreign
    // monitor would inject a `<monitor-event>` reminder into this session's
    // conversation. Assert it is fully dropped — no conversation injection
    // and no pager forward.
    let (config, mut gateway_rx, _persistence_rx, mut cmd_rx) = make_test_config_full();
    let notification = make_monitor_event_notification("mon-foreign", Some("other-session"));
    let mut offsets = HashMap::new();

    handle_notification(&config, notification, &mut offsets).await;

    assert!(
        cmd_rx.try_recv().is_err(),
        "cross-session monitor event must not be injected into this session"
    );
    while let Ok(msg) = gateway_rx.try_recv() {
        if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg {
            assert_ne!(
                args.request.method.as_ref(),
                "echo.agent/monitor_event",
                "cross-session monitor event must not be forwarded to the pager"
            );
        }
    }
}

#[tokio::test]
async fn same_session_monitor_event_is_injected() {
    // Owner matches the bridge's own session id ("test-session") -> deliver.
    let (config, mut cmd_rx) = make_test_config();
    let notification = make_monitor_event_notification("mon-own", Some("test-session"));
    let mut offsets = HashMap::new();

    handle_notification(&config, notification, &mut offsets).await;

    match cmd_rx
        .try_recv()
        .expect("own-session monitor event must be injected")
    {
        SessionCommand::InjectNotification { source, .. } => match source {
            NotificationSource::MonitorEvent { task_id } => assert_eq!(task_id, "mon-own"),
            _ => panic!("expected MonitorEvent notification source"),
        },
        _ => panic!("expected InjectNotification"),
    }
}

#[tokio::test]
async fn legacy_monitor_event_without_owner_is_injected() {
    // Legacy / non-echo-agent-build backends record no owner; such events must
    // pass through unchanged for backwards compatibility.
    let (config, mut cmd_rx) = make_test_config();
    let notification = make_monitor_event_notification("mon-legacy", None);
    let mut offsets = HashMap::new();

    handle_notification(&config, notification, &mut offsets).await;

    assert!(
        matches!(
            cmd_rx
                .try_recv()
                .expect("legacy (no-owner) monitor event must be injected"),
            SessionCommand::InjectNotification {
                source: NotificationSource::MonitorEvent { .. },
                ..
            }
        ),
        "legacy monitor event should be injected as a MonitorEvent notification"
    );
}

#[tokio::test]
async fn block_waited_task_skips_auto_wake_prompt() {
    let (config, mut gateway_rx, _persistence_rx, mut cmd_rx) = make_test_config_full();
    let mut snapshot = make_task_snapshot("bg-waited", TaskKind::Bash);
    snapshot.block_waited = true;
    let notification = ToolNotification::TaskCompleted(snapshot);
    let mut offsets = HashMap::new();

    handle_notification(&config, notification, &mut offsets).await;

    // block_waited tasks must NOT inject a synthetic prompt — the
    // blocking caller already received the result directly.
    match cmd_rx
        .try_recv()
        .expect("expected DispatchNotificationHook for task_complete")
    {
        SessionCommand::DispatchNotificationHook {
            notification_type, ..
        } => assert_eq!(notification_type, "task_complete"),
        _ => panic!("unexpected session command"),
    }
    assert!(
        cmd_rx.try_recv().is_err(),
        "block_waited completion should not send Prompt or InjectNotification"
    );

    // The echo.agent/task_completed ExtNotification for UI updates must still be sent.
    let mut found_ext = false;
    while let Ok(msg) = gateway_rx.try_recv() {
        if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg
            && args.request.method.as_ref() == "echo.agent/task_completed"
        {
            found_ext = true;
        }
    }
    assert!(
        found_ext,
        "echo.agent/task_completed ExtNotification must still be sent for UI"
    );
}

#[tokio::test]
async fn explicitly_killed_task_skips_auto_wake_prompt() {
    let (config, mut gateway_rx, _persistence_rx, mut cmd_rx) = make_test_config_full();
    let mut snapshot = make_task_snapshot("bg-killed", TaskKind::Bash);
    snapshot.explicitly_killed = true;
    snapshot.kill_result_delivered = true;
    let notification = ToolNotification::TaskCompleted(snapshot);
    let mut offsets = HashMap::new();

    handle_notification(&config, notification, &mut offsets).await;

    // Model-tool / delivered kill skips auto-wake; UI kill is
    // `ui_killed_task_auto_wakes_and_tells_model_not_to_restart`.
    match cmd_rx
        .try_recv()
        .expect("expected DispatchNotificationHook for task_complete")
    {
        SessionCommand::DispatchNotificationHook {
            notification_type, ..
        } => assert_eq!(notification_type, "task_complete"),
        _ => panic!("unexpected session command"),
    }
    assert!(
        cmd_rx.try_recv().is_err(),
        "delivered kill completion should not send Prompt or InjectNotification"
    );
    assert_eq!(
        task_completed_will_wake(&mut gateway_rx),
        Some(false),
        "model-tool/delivered kill must stamp will_wake: false"
    );
}

#[tokio::test]
async fn teardown_killed_task_skips_auto_wake_prompt() {
    let (config, mut gateway_rx, _persistence_rx, mut cmd_rx) = make_test_config_full();
    let mut snapshot = make_task_snapshot("bg-teardown", TaskKind::Bash);
    snapshot.explicitly_killed = true;
    snapshot.kill_result_delivered = true;
    let mut offsets = HashMap::new();

    handle_notification(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
    )
    .await;

    match cmd_rx
        .try_recv()
        .expect("expected DispatchNotificationHook for task_complete")
    {
        SessionCommand::DispatchNotificationHook {
            notification_type, ..
        } => assert_eq!(notification_type, "task_complete"),
        _ => panic!("unexpected session command"),
    }
    assert!(
        cmd_rx.try_recv().is_err(),
        "teardown kill with no waiter must not enqueue Prompt"
    );
    assert_eq!(
        task_completed_will_wake(&mut gateway_rx),
        Some(false),
        "teardown kill must stamp will_wake: false"
    );
}

#[tokio::test]
async fn ui_killed_task_auto_wakes_and_tells_model_not_to_restart() {
    let (config, mut gateway_rx, _persistence_rx, mut cmd_rx) = make_test_config_full();
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    let mut snapshot = make_task_snapshot("bg-ui-killed", TaskKind::Bash);
    snapshot.explicitly_killed = true;
    snapshot.kill_result_delivered = false;
    let mut offsets = HashMap::new();

    handle_notification_with_admission(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
        &mut cmd_rx,
        true,
    )
    .await;

    let command = cmd_rx.try_recv().expect("expected Prompt");
    match command {
        SessionCommand::Prompt { prompt_blocks, .. } => {
            let text = match &prompt_blocks[0] {
                acp::ContentBlock::Text(t) => &t.text,
                _ => panic!("expected text block"),
            };
            assert!(
                text.contains("killed by the user — do not restart it"),
                "UI-kill wake must tell the model not to relaunch: {text}"
            );
        }
        _ => panic!("expected Prompt"),
    }
    assert_eq!(
        task_completed_will_wake(&mut gateway_rx),
        Some(true),
        "UI/Stop kill with no delivered result must stamp will_wake: true"
    );
}

#[tokio::test]
async fn ui_killed_task_gate_armed_defers_to_fallback() {
    let (config, mut gateway_rx, _persistence_rx, mut cmd_rx) = make_test_config_full();
    config.task_wake_suppressed.set(true);
    let mut snapshot = make_task_snapshot("bg-ui-kill-gated", TaskKind::Bash);
    snapshot.explicitly_killed = true;
    snapshot.kill_result_delivered = false;
    let mut offsets = HashMap::new();

    handle_notification_with_admission(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
        &mut cmd_rx,
        false,
    )
    .await;

    assert!(
        matches!(cmd_rx.try_recv(), Ok(SessionCommand::Prompt { .. })),
        "gate-armed UI kill must still request admission, not the suppress arm"
    );
    assert_eq!(
        task_completed_will_wake(&mut gateway_rx),
        Some(false),
        "declined admission must stamp will_wake: false"
    );
    assert!(
        config
            .task_completion_reservations
            .contains("bg-ui-kill-gated"),
        "deferred UI-kill fallback retains the reservation"
    );
}

#[tokio::test]
async fn bash_task_completed_falls_back_when_auto_wake_disabled() {
    let (mut config, mut cmd_rx) = make_test_config();
    config.auto_wake_enabled = false;
    config
        .task_output_tool_name
        .set(Some("get_command_or_subagent_output".to_string()))
        .expect("slot is fresh in this test fixture");
    let snapshot = make_task_snapshot("bg-disabled", TaskKind::Bash);
    let notification = ToolNotification::TaskCompleted(snapshot);
    let mut offsets = HashMap::new();

    handle_notification(&config, notification, &mut offsets).await;

    // With auto-wake disabled, should use InjectNotification (not Prompt).
    let cmd = cmd_rx.try_recv().expect("expected InjectNotification");
    match cmd {
        SessionCommand::InjectNotification {
            prompt_id,
            prompt_blocks,
            priority,
            source,
            ..
        } => {
            assert!(prompt_id.starts_with("bash-completed-"));
            assert_eq!(priority, NotificationPriority::Later);
            assert!(matches!(
                source,
                NotificationSource::BashTaskCompleted { ref task_id } if task_id == "bg-disabled"
            ));
            let text = match &prompt_blocks[0] {
                acp::ContentBlock::Text(t) => &t.text,
                _ => panic!("expected text block"),
            };
            assert!(text.contains(r#"get_command_or_subagent_output("bg-disabled")"#));
            assert!(!text.contains(r#"get_task_output("bg-disabled")"#));
            assert!(!text.contains("response:"));
        }
        _ => panic!("expected InjectNotification"),
    }

    let hook_cmd = cmd_rx
        .try_recv()
        .expect("expected DispatchNotificationHook for task_complete");
    match hook_cmd {
        SessionCommand::DispatchNotificationHook {
            notification_type,
            message,
            ..
        } => {
            assert_eq!(notification_type, "task_complete");
            assert_eq!(
                message.as_deref(),
                Some("Background task completed: bg-disabled")
            );
        }
        _ => panic!("expected DispatchNotificationHook"),
    }
}

#[tokio::test]
async fn bash_completion_uses_single_task_id_clone() {
    let (config, mut cmd_rx) = make_test_config();
    let snapshot = make_task_snapshot("unique-id-789", TaskKind::Bash);
    let notification = ToolNotification::TaskCompleted(snapshot);
    let mut offsets = HashMap::new();

    handle_notification_with_admission(&config, notification, &mut offsets, &mut cmd_rx, true)
        .await;

    let cmd = cmd_rx.try_recv().unwrap();
    if let SessionCommand::Prompt { prompt_id, .. } = cmd {
        assert_eq!(prompt_id, "task-completed-unique-id-789");
    } else {
        panic!("expected Prompt");
    }
}

fn extract_current_mode_id(notification: &acp::SessionNotification) -> Option<&str> {
    match &notification.update {
        acp::SessionUpdate::CurrentModeUpdate(cmu) => Some(cmu.current_mode_id.0.as_ref()),
        _ => None,
    }
}

/// Regression: `PlanModeExited` must emit `CurrentModeUpdate("default")`
/// onto both the gateway and the persistence stream. Without this,
/// agent-driven plan approvals leave the TUI stuck in plan mode.
#[tokio::test]
async fn plan_mode_exited_emits_current_mode_update_default() {
    let (config, mut gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();

    // Pre-condition: agent path requires plan mode to be Active first
    // so `deactivate_approved` actually flips state and triggers the emit.
    {
        let mut tracker = config.plan_mode.lock();
        assert!(tracker.activate_from_tool());
    }
    *config.current_prompt_mode.lock() = crate::session::plan_mode::PromptMode::Plan;
    *config.turn_prompt_mode.lock() = crate::session::plan_mode::PromptMode::Plan;

    let notification =
        ToolNotification::PlanModeExited(echo_agent_tools::notification::types::PlanModeExited {
            tool_call_id: "tc-exit-1".into(),
            plan_content: Some("- step 1".into()),
            plan_file_path: "/tmp/test-session/plan.md".into(),
        });

    let mut offsets = HashMap::new();
    handle_notification(&config, notification, &mut offsets).await;

    // Gateway: one CurrentModeUpdate("default").
    let mut gateway_modes = Vec::new();
    while let Ok(msg) = gateway_rx.try_recv() {
        if let echo_agent_acp::AcpClientMessage::SessionNotification(args) = msg
            && let Some(id) = extract_current_mode_id(&args.request)
        {
            gateway_modes.push(id.to_string());
        }
    }
    assert_eq!(
        gateway_modes,
        vec!["default".to_string()],
        "PlanModeExited should emit exactly one CurrentModeUpdate(default) to the gateway"
    );

    // Persistence: same notification persisted so replay re-applies the exit.
    let mut persisted_modes = Vec::new();
    while let Ok(msg) = persistence_rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::Acp(notif)) = msg
            && let Some(id) = extract_current_mode_id(&notif)
        {
            persisted_modes.push(id.to_string());
        }
    }
    assert_eq!(
        persisted_modes,
        vec!["default".to_string()],
        "PlanModeExited should persist exactly one CurrentModeUpdate(default)"
    );

    // Session-level prompt mode was reset.
    assert!(matches!(
        *config.current_prompt_mode.lock(),
        crate::session::plan_mode::PromptMode::Agent
    ));
}

/// Default (echoagent) polarity: the exit_plan_mode tool result is the model's
/// only exit signal, so an approved `PlanModeExited` must NOT arm the
/// deferred exit reminder — in memory or in the persisted snapshot.
/// Sibling of `plan_mode_exited_arms_exit_reminder_when_gated`.
#[tokio::test]
async fn plan_mode_exited_does_not_arm_exit_reminder_by_default() {
    let (config, _gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();

    {
        let mut tracker = config.plan_mode.lock();
        assert!(tracker.activate_from_tool());
    }

    let notification =
        ToolNotification::PlanModeExited(echo_agent_tools::notification::types::PlanModeExited {
            tool_call_id: "tc-exit-echo-agent".into(),
            plan_content: Some("- step 1".into()),
            plan_file_path: "/tmp/test-session/plan.md".into(),
        });

    let mut offsets = HashMap::new();
    handle_notification(&config, notification, &mut offsets).await;

    assert!(
        !config.plan_mode.lock().has_pending_exit_reminder(),
        "approved exit must not arm the deferred exit reminder"
    );
    let mut persisted_plan_snapshots = Vec::new();
    while let Ok(msg) = persistence_rx.try_recv() {
        if let PersistenceMsg::PlanModeState(snapshot) = msg {
            persisted_plan_snapshots.push(snapshot);
        }
    }
    assert!(
        !persisted_plan_snapshots.is_empty()
            && persisted_plan_snapshots
                .iter()
                .all(|s| !s.pending_exit_reminder),
        "persisted plan-mode snapshot must not carry the exit reminder"
    );
}

/// Gated counterpart: when `queue_exit_reminder_on_approved_exit` is
/// set, an approved `PlanModeExited` must arm the next-turn exit
/// reminder and persist it.
#[tokio::test]
async fn plan_mode_exited_arms_exit_reminder_when_gated() {
    let (config, _gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();
    config
        .queue_exit_reminder_on_approved_exit
        .store(true, std::sync::atomic::Ordering::Relaxed);

    {
        let mut tracker = config.plan_mode.lock();
        assert!(tracker.activate_from_tool());
    }

    let notification =
        ToolNotification::PlanModeExited(echo_agent_tools::notification::types::PlanModeExited {
            tool_call_id: "tc-exit-gated".into(),
            plan_content: Some("- step 1".into()),
            plan_file_path: "/tmp/test-session/plan.md".into(),
        });

    let mut offsets = HashMap::new();
    handle_notification(&config, notification, &mut offsets).await;

    assert!(
        config.plan_mode.lock().has_pending_exit_reminder(),
        "gated approved exit must arm the next-turn exit reminder"
    );
    let mut persisted_plan_snapshots = Vec::new();
    while let Ok(msg) = persistence_rx.try_recv() {
        if let PersistenceMsg::PlanModeState(snapshot) = msg {
            persisted_plan_snapshots.push(snapshot);
        }
    }
    assert!(
        !persisted_plan_snapshots.is_empty()
            && persisted_plan_snapshots
                .iter()
                .all(|s| s.pending_exit_reminder),
        "persisted plan-mode snapshot must carry the armed exit reminder"
    );
}

/// Symmetric to the exit test: `PlanModeEntered` emits
/// `CurrentModeUpdate("plan")`.
#[tokio::test]
async fn plan_mode_entered_emits_current_mode_update_plan() {
    let (config, mut gateway_rx, mut persistence_rx, _cmd_rx) = make_test_config_full();

    let notification =
        ToolNotification::PlanModeEntered(echo_agent_tools::notification::types::PlanModeEntered {
            tool_call_id: "tc-enter-1".into(),
        });

    let mut offsets = HashMap::new();
    handle_notification(&config, notification, &mut offsets).await;

    let mut gateway_modes = Vec::new();
    while let Ok(msg) = gateway_rx.try_recv() {
        if let echo_agent_acp::AcpClientMessage::SessionNotification(args) = msg
            && let Some(id) = extract_current_mode_id(&args.request)
        {
            gateway_modes.push(id.to_string());
        }
    }
    assert_eq!(gateway_modes, vec!["plan".to_string()]);

    let mut persisted_modes = Vec::new();
    while let Ok(msg) = persistence_rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::Acp(notif)) = msg
            && let Some(id) = extract_current_mode_id(&notif)
        {
            persisted_modes.push(id.to_string());
        }
    }
    assert_eq!(persisted_modes, vec!["plan".to_string()]);
}

/// Build a completed-bash `TaskSnapshot` whose `output` is large enough
/// to trip the inline-completion truncation cap, with a concrete
/// `output_file` path so the disk-pointer footer is exercised end-to-end.
fn make_large_bash_snapshot(task_id: &str, output_file: PathBuf) -> TaskSnapshot {
    TaskSnapshot {
        task_id: task_id.into(),
        command: "yes hello | head -c 20000".into(),
        display_command: None,
        cwd: String::new(),
        start_time: std::time::SystemTime::now(),
        end_time: Some(std::time::SystemTime::now()),
        output: "h".repeat(20_000),
        output_file,
        truncated: true,
        exit_code: Some(0),
        signal: None,
        completed: true,
        kind: TaskKind::Bash,
        block_waited: false,
        explicitly_killed: false,
        kill_result_delivered: false,
        owner_session_id: None,
        description: None,
        is_backgrounded: false,
        output_total_bytes: 0,
    }
}

/// Extract the auto-wake prompt text emitted on the session command channel.
fn auto_wake_prompt_text(cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>) -> String {
    let cmd = cmd_rx.try_recv().expect("expected Prompt");
    match cmd {
        SessionCommand::Prompt { prompt_blocks, .. } => match &prompt_blocks[0] {
            acp::ContentBlock::Text(t) => t.text.clone(),
            _ => panic!("expected text block"),
        },
        _ => panic!("expected Prompt"),
    }
}

/// Extract the InjectNotification prompt text emitted on the session
/// command channel (auto-wake-disabled fallback path).
fn inject_notification_prompt_text(cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>) -> String {
    let cmd = cmd_rx.try_recv().expect("expected InjectNotification");
    match cmd {
        SessionCommand::InjectNotification { prompt_blocks, .. } => match &prompt_blocks[0] {
            acp::ContentBlock::Text(t) => t.text.clone(),
            _ => panic!("expected text block"),
        },
        _ => panic!("expected InjectNotification"),
    }
}

/// Bash completion with a large output and no polling tool (compat-harness
/// toolset) renders the truncation marker AND the disk-pointer footer
/// pointing the model at `output_file` via the resolved Read tool name.
/// Covers BOTH the auto-wake branch and the auto-wake-disabled fallback
/// so the truncation + footer behaviour stays consistent across both
/// completion-injection paths.
#[tokio::test]
async fn bash_completion_renders_disk_pointer_footer_in_both_branches() {
    let output_file = PathBuf::from("/tmp/bg-disk-pointer.log");

    let (config_auto, mut cmd_rx_auto) = make_test_config();
    config_auto
        .read_tool_name
        .set(Some("read_file".to_string()))
        .expect("fresh slot");
    let snapshot = make_large_bash_snapshot("bg-disk-1", output_file.clone());
    let mut offsets = HashMap::new();
    handle_notification_with_admission(
        &config_auto,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
        &mut cmd_rx_auto,
        true,
    )
    .await;
    let prompt = auto_wake_prompt_text(&mut cmd_rx_auto);
    assert!(
        prompt.contains("[Output truncated"),
        "auto-wake: expected truncation marker, got: {prompt}"
    );
    let expected_footer = format!(
        "Use read_file on {} for full content",
        output_file.display()
    );
    assert!(
        prompt.contains(&expected_footer),
        "auto-wake: expected disk-pointer footer `{expected_footer}`, got: {prompt}"
    );
    assert!(
        prompt.contains("bg-disk-1"),
        "auto-wake: prompt must reference task id"
    );

    let (mut config_no_wake, mut cmd_rx_no_wake) = make_test_config();
    config_no_wake.auto_wake_enabled = false;
    config_no_wake
        .read_tool_name
        .set(Some("read_file".to_string()))
        .expect("fresh slot");
    let snapshot = make_large_bash_snapshot("bg-disk-2", output_file.clone());
    let mut offsets = HashMap::new();
    handle_notification(
        &config_no_wake,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
    )
    .await;
    let prompt = inject_notification_prompt_text(&mut cmd_rx_no_wake);
    assert!(
        prompt.contains("[Output truncated"),
        "auto-wake-disabled: expected truncation marker, got: {prompt}"
    );
    let expected_footer = format!(
        "Use read_file on {} for full content",
        output_file.display()
    );
    assert!(
        prompt.contains(&expected_footer),
        "auto-wake-disabled: expected disk-pointer footer `{expected_footer}`, got: {prompt}"
    );
    assert!(
        prompt.contains("bg-disk-2"),
        "auto-wake-disabled: prompt must reference task id"
    );
}

/// Completions must go through the size limit, and the copy persisted
/// for replay must be the copy that was sent. The limit itself is
/// tested in `task_completed_frame`.
#[tokio::test]
async fn task_completed_notification_is_frame_bounded() {
    let (mut config, mut gateway_rx, mut persistence_rx, mut cmd_rx) = make_test_config_full_raw();
    config.auto_wake_enabled = false;

    let mut snapshot = make_task_snapshot("bg-output-clamp", TaskKind::Bash);
    snapshot.output = "Z".repeat(2 * 1024 * 1024);
    snapshot.output_file = PathBuf::from("/tmp/bg-output-clamp.log");
    snapshot.is_backgrounded = true;

    let mut offsets = HashMap::new();
    handle_notification(
        &config,
        ToolNotification::TaskCompleted(snapshot),
        &mut offsets,
    )
    .await;
    while cmd_rx.try_recv().is_ok() {}

    let mut params = None;
    while let Ok(msg) = gateway_rx.try_recv() {
        if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg
            && args.request.method.as_ref() == "echo.agent/task_completed"
        {
            params = Some(args.request.params.get().to_string());
        }
    }
    let params = params.expect("expected an echo.agent/task_completed notification");
    assert!(
        params.len() <= task_completed_frame::FRAME_MAX_BYTES,
        "params is {} bytes",
        params.len()
    );
    assert!(params.contains("/tmp/bg-output-clamp.log"));

    let mut persisted = None;
    while let Ok(msg) = persistence_rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(saved)) =
            msg
        {
            persisted = Some(serde_json::to_value(&*saved).unwrap());
        }
    }
    assert_eq!(
        persisted.expect("the completion must be persisted"),
        serde_json::from_str::<serde_json::Value>(&params).unwrap(),
    );
}
