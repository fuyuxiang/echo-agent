use super::*;
use crate::capability::CapabilityMode;
use crate::handle::tests::{
    background_capable_cfg, make_confining_handle, make_handle, start_background_sleep,
};
use echo_agent_tool_protocol::turn_hook;
use echo_agent_tools::implementations::echo_agent_build::scheduler::types::{
    ScheduledTask, SchedulerState,
};
use echo_agent_tools::types::resources::State;
use std::sync::Arc;
async fn next_item(
    stream: &mut ToolStream<TypedToolOutput>,
) -> Option<echo_agent_tool_runtime::ToolStreamItem<TypedToolOutput>> {
    use std::task::Context;
    std::future::poll_fn(|cx: &mut Context<'_>| stream.as_mut().poll_next(cx)).await
}
fn turn_hook_frame(session: &str, req: &turn_hook::TurnHookRequest) -> HookFrame {
    HookFrame::custom_request(
        SessionId::new(session).unwrap(),
        "hk-test".to_owned(),
        turn_hook::TURN_HOOK_KIND.to_owned(),
        serde_json::to_value(req).unwrap(),
    )
}
#[tokio::test]
async fn handle_hook_request_turn_hook_returns_reply() {
    let handler = WorkspaceRpcHandler::new(make_handle());
    let req = turn_hook::TurnHookRequest::Before(turn_hook::BeforeTurnPayload {
        turn_number: 1,
        model_id: "echo-agent-3".to_owned(),
        yolo_mode: false,
        conversation_message_count: 0,
        session_relationship: "primary".to_owned(),
        schema_version: "1.0".to_owned(),
    });
    let value = handler
        .handle_hook_request(
            SessionId::new("main").unwrap(),
            turn_hook_frame("main", &req),
        )
        .await
        .expect("turn hook claimed");
    let reply: turn_hook::HookReply = serde_json::from_value(value).unwrap();
    assert_eq!(reply, turn_hook::HookReply::default());
}
#[tokio::test]
async fn handle_hook_request_ignores_non_turn_hook_kind() {
    let handler = WorkspaceRpcHandler::new(make_handle());
    let frame = HookFrame::custom_request(
        SessionId::new("main").unwrap(),
        "hk-x".to_owned(),
        "some_other_kind".to_owned(),
        serde_json::json!({}),
    );
    assert!(
        handler
            .handle_hook_request(SessionId::new("main").unwrap(), frame)
            .await
            .is_none()
    );
}
#[tokio::test]
async fn handle_hook_request_unbound_session_is_noop() {
    let handler = WorkspaceRpcHandler::new(make_handle());
    let req = turn_hook::TurnHookRequest::Before(turn_hook::BeforeTurnPayload {
        turn_number: 1,
        model_id: "echo-agent-3".to_owned(),
        yolo_mode: false,
        conversation_message_count: 0,
        session_relationship: "primary".to_owned(),
        schema_version: "1.0".to_owned(),
    });
    let value = handler
        .handle_hook_request(
            SessionId::new("never-bound").unwrap(),
            turn_hook_frame("never-bound", &req),
        )
        .await
        .expect("fail-open no-op reply");
    let reply: turn_hook::HookReply = serde_json::from_value(value).unwrap();
    assert_eq!(reply, turn_hook::HookReply::default());
}
#[tokio::test]
async fn dispatch_workspace_info_reports_server_version() {
    use echo_agent_workspace_types::rpc::workspace::{WorkspaceInfo, WorkspaceInfoReq};
    let handler = WorkspaceRpcHandler::new(make_handle());
    let value = handler
        .dispatch(
            <WorkspaceInfoReq as WorkspaceRpc>::METHOD,
            serde_json::json!({}),
            None,
        )
        .await
        .expect("workspace.info dispatch");
    let info: WorkspaceInfo = serde_json::from_value(value).expect("typed WorkspaceInfo");
    assert_eq!(Some(echo_agent_version::VERSION.to_owned()), info.version);
}
#[tokio::test]
async fn dispatch_unknown_method_returns_unknown_method_error() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.nonexistent", Value::Null, None)
        .await;
    match result {
        Err(WorkspaceError::UnknownMethod(method)) => {
            assert_eq!(method, "workspace.nonexistent");
        }
        other => panic!("expected UnknownMethod, got {other:?}"),
    }
}
#[tokio::test]
async fn handle_evict_unbind_does_not_unmount() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let handle = make_handle();
    handle
        .create_session_with_cwd("evict-conv", None)
        .expect("create");
    handle
        .session("evict-conv")
        .expect("session")
        .set_path_virtualization(
            crate::path_virtualization::PathVirtualization::try_from_session_root(
                "/workspace/evict-conv",
            )
            .expect("valid"),
        );
    let mounts = Arc::new(AtomicUsize::new(0));
    let unbinds = Arc::new(AtomicUsize::new(0));
    let mounts_c = mounts.clone();
    let unbinds_c = unbinds.clone();
    handle.set_bind_mount_hook(
        crate::path_virtualization::BindMountHook::probe_then_mount(
            |_| false,
            move |_| {
                mounts_c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .with_on_unbind(move |_, _| {
            unbinds_c.fetch_add(1, Ordering::SeqCst);
        }),
    );
    WorkspaceRpcHandler::new(handle)
        .handle_evict(ToolServerEvictParams {
            session_id: SessionId::new("evict-conv").unwrap(),
            reason: "test".into(),
            grace_period_ms: 50,
        })
        .await;
    assert_eq!(
        mounts.load(Ordering::SeqCst),
        0,
        "evict/prune must not mount or unmount"
    );
    assert_eq!(unbinds.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn handle_evict_triggers_two_phase_drain() {
    use echo_agent_tool_protocol::ToolServerLifecycleStatus;
    let handle = make_handle();
    let tracker = handle.activity_tracker().clone();
    let handler = WorkspaceRpcHandler::new(handle);
    assert!(!tracker.is_draining(), "not draining before evict");
    handler
        .handle_evict(ToolServerEvictParams {
            session_id: SessionId::new("main").expect("valid session id"),
            reason: "preemption".into(),
            grace_period_ms: 200,
        })
        .await;
    let snap = tracker.snapshot();
    assert_eq!(
        snap.status,
        ToolServerLifecycleStatus::ShuttingDown,
        "an evicted workspace must end in terminal ShuttingDown, not lingering Draining"
    );
    assert!(
        snap.drain_started_ms.is_some(),
        "evict drain must stamp drain_started_ms"
    );
}
#[tokio::test]
async fn handle_evict_shuts_down_terminal_backend_explicitly() {
    let handle = make_handle();
    let session = handle.session("main").expect("main session exists");
    let retained_backend = session.terminal_backend().clone();
    let retained_toolset = session.toolset();
    drop(session);
    let handler = WorkspaceRpcHandler::new(handle);
    handler
        .handle_evict(ToolServerEvictParams {
            session_id: SessionId::new("main").expect("valid session id"),
            reason: "preemption".into(),
            grace_period_ms: 100,
        })
        .await;
    crate::handle::tests::assert_backend_stops(&retained_backend).await;
    drop(retained_toolset);
}
#[tokio::test]
async fn list_background_tasks_rpc_stays_truthful_across_rebinds() {
    use crate::capability::CapabilityMode;
    use crate::handle::RebindOutcome;
    use crate::handle::tests::{background_capable_cfg, start_background_sleep};
    use crate::session::tool_config::test_support::tc;
    use echo_agent_tools::registry::types::ToolServerConfig;
    use echo_agent_workspace_types::rpc::workspace::ListBackgroundTasksResponse;
    let handle = make_handle();
    let cfg = background_capable_cfg();
    let session = handle
        .create_session_with_config(
            "bg-rpc",
            None,
            Some(cfg.clone()),
            CapabilityMode::All,
            None,
            false,
        )
        .expect("create background-capable session");
    session.set_bind_tool_config_fingerprint(serde_json::to_value(&cfg).ok());
    let out_dir = tempfile::tempdir().expect("temp dir");
    let bg = start_background_sleep(&session, out_dir.path(), "bg-rpc-task").await;
    let handler = WorkspaceRpcHandler::new(handle.clone());
    async fn list_tasks(
        handler: &WorkspaceRpcHandler,
    ) -> Vec<echo_agent_workspace_types::rpc::workspace::BackgroundTaskSummaryWire> {
        let value = handler
            .dispatch(
                "workspace.list_background_tasks",
                serde_json::json!({"session_id": "bg-rpc"}),
                Some("bg-rpc"),
            )
            .await
            .expect("list_background_tasks rpc");
        serde_json::from_value::<ListBackgroundTasksResponse>(value)
            .expect("decode response")
            .tasks
    }
    let tasks = list_tasks(&handler).await;
    assert_eq!(tasks.len(), 1, "the running task must be listed");
    assert_eq!(tasks[0].task_id, bg.task_id);
    assert_eq!(
        tasks[0].tool_name.as_deref(),
        Some("run_terminal_cmd"),
        "the creator tool is named from the live toolset"
    );
    let (_, outcome) = handle
        .rebind_existing_hub_session("bg-rpc", Some(cfg.clone()), serde_json::to_value(&cfg).ok())
        .await
        .expect("session exists");
    assert_eq!(outcome, RebindOutcome::Reused);
    let tasks = list_tasks(&handler).await;
    assert_eq!(tasks.len(), 1, "the task must survive a reused rebind");
    let read_only = ToolServerConfig {
        tools: vec![tc(
            "EchoAgentBuild:read_file",
            Some(echo_agent_tools::types::tool::ToolKind::Read),
        )],
        behavior_preset: None,
    };
    let (_, outcome) = handle
        .rebind_existing_hub_session(
            "bg-rpc",
            Some(read_only.clone()),
            serde_json::to_value(&read_only).ok(),
        )
        .await
        .expect("session exists");
    assert_eq!(outcome, RebindOutcome::Reresolved);
    let tasks = list_tasks(&handler).await;
    assert_eq!(tasks.len(), 1, "the task must survive the toolset swap");
    assert_eq!(tasks[0].task_id, bg.task_id);
    assert_eq!(
        tasks[0].tool_name, None,
        "the swapped-in toolset has no execute tool to name"
    );
    session.terminal_backend().kill_task(&bg.task_id).await;
    let tasks = list_tasks(&handler).await;
    assert!(
        tasks.is_empty(),
        "a killed task must leave the outstanding list: {tasks:?}"
    );
}
#[tokio::test]
async fn tasks_snapshot_rpc_lists_outstanding_background_tasks() {
    let handle = make_handle();
    let cfg = background_capable_cfg();
    let session = handle
        .create_session_with_config(
            "snap-rpc",
            None,
            Some(cfg.clone()),
            CapabilityMode::All,
            None,
            false,
        )
        .expect("create background-capable session");
    session.set_bind_tool_config_fingerprint(serde_json::to_value(&cfg).ok());
    let out_dir = tempfile::tempdir().expect("temp dir");
    let bg = start_background_sleep(&session, out_dir.path(), "snap-rpc-task").await;
    let handler = WorkspaceRpcHandler::new(handle.clone());
    async fn snapshot(handler: &WorkspaceRpcHandler) -> TasksSnapshotResponse {
        let value = handler
            .dispatch(
                "workspace.tasks_snapshot",
                serde_json::json!({"session_id": "snap-rpc"}),
                Some("snap-rpc"),
            )
            .await
            .expect("tasks_snapshot rpc");
        serde_json::from_value(value).expect("decode response")
    }
    let snap = snapshot(&handler).await;
    assert_eq!(
        snap.background_tasks.len(),
        1,
        "the running task must be listed"
    );
    let task = &snap.background_tasks[0];
    assert_eq!(task.task_id, bg.task_id);
    assert_eq!(task.kind, "bash");
    assert!(
        task.description.is_none(),
        "start_background_sleep does not set description: {:?}",
        task.description
    );
    assert!(
        DateTime::parse_from_rfc3339(&task.started_at).is_ok(),
        "started_at must be RFC3339: {}",
        task.started_at
    );
    assert!(
        snap.scheduled_tasks.is_empty(),
        "no scheduler resource in this toolset: {:?}",
        snap.scheduled_tasks
    );
    {
        use crate::handle::tests::terminal_run_request;
        let mut req = terminal_run_request("sleep 30", out_dir.path(), "snap-desc-task");
        req.description = Some("build frontend".into());
        let desc_bg = session
            .terminal_backend()
            .run_background(req)
            .await
            .expect("start described background task");
        let snap = snapshot(&handler).await;
        let described = snap
            .background_tasks
            .iter()
            .find(|t| t.task_id == desc_bg.task_id)
            .expect("described task in snapshot");
        assert_eq!(described.description.as_deref(), Some("build frontend"));
        session.terminal_backend().kill_task(&desc_bg.task_id).await;
    }
    session.terminal_backend().kill_task(&bg.task_id).await;
    let snap = snapshot(&handler).await;
    assert!(
        snap.background_tasks.is_empty(),
        "a killed task must leave the snapshot: {:?}",
        snap.background_tasks
    );
    {
        let toolset = session.toolset();
        let mut resources = toolset.resources.lock().await;
        let state = resources.get_or_default::<State<SchedulerState>>();
        let mut task = ScheduledTask::new(300, "check CI".into(), true, false);
        task.id = "loop-1".into();
        state.tasks.push(task);
    }
    let snap = snapshot(&handler).await;
    assert_eq!(snap.scheduled_tasks.len(), 1);
    let loop_task = &snap.scheduled_tasks[0];
    assert_eq!(loop_task.task_id, "loop-1");
    assert_eq!(loop_task.prompt, "check CI");
    assert_eq!(loop_task.human_schedule, "every 5 minutes");
    assert!(loop_task.recurring);
    assert!(
        DateTime::parse_from_rfc3339(&loop_task.next_fire_at).is_ok(),
        "next_fire_at must be RFC3339: {}",
        loop_task.next_fire_at
    );
}
/// Unknown ids report deleted:false. This session asks for no notifications, so nothing acknowledges a removal and a live loop still errors.
/// `hub_session_deletes_a_live_scheduled_task` covers the session that does ask.
#[tokio::test]
async fn delete_scheduled_task_rpc_reports_honestly() {
    use echo_agent_workspace_types::rpc::workspace::DeleteScheduledTaskResponse;
    let handle = make_handle();
    let cfg = background_capable_cfg();
    let session = handle
        .create_session_with_config(
            "del-rpc",
            None,
            Some(cfg.clone()),
            CapabilityMode::All,
            None,
            false,
        )
        .expect("create background-capable session");
    session.set_bind_tool_config_fingerprint(serde_json::to_value(&cfg).ok());
    seed_scheduled_task(session.toolset().as_ref(), "loop-del-1").await;
    let handler = WorkspaceRpcHandler::new(handle.clone());
    async fn delete(
        handler: &WorkspaceRpcHandler,
        task_id: &str,
    ) -> Result<DeleteScheduledTaskResponse, WorkspaceError> {
        handler
            .dispatch(
                "workspace.delete_scheduled_task",
                serde_json::json!({"session_id": "del-rpc", "task_id": task_id}),
                Some("del-rpc"),
            )
            .await
            .map(|value| serde_json::from_value(value).expect("decode delete response"))
    }
    let missing = delete(&handler, "no-such-loop").await.expect("unknown id");
    assert_eq!(missing.task_id, "no-such-loop");
    assert!(!missing.deleted, "an unknown id must report false");
    let live = delete(&handler, "loop-del-1").await;
    let err = live.expect_err("a live loop must error until the durable gate is satisfied");
    assert!(
        err.to_string().contains("durab"),
        "expected the durability refusal, got: {err}"
    );
    let snap_value = handler
        .dispatch(
            "workspace.tasks_snapshot",
            serde_json::json!({"session_id": "del-rpc"}),
            Some("del-rpc"),
        )
        .await
        .expect("tasks_snapshot after refusal");
    let snap: TasksSnapshotResponse = serde_json::from_value(snap_value).expect("decode snapshot");
    assert_eq!(
        snap.scheduled_tasks.len(),
        1,
        "a refused delete must leave the loop scheduled"
    );
}
/// Populate the real scheduler state; the production actor serves the RPC.
async fn seed_scheduled_task(toolset: &FinalizedToolset, id: &str) {
    let mut resources = toolset.resources.lock().await;
    let state = resources.get_or_default::<State<SchedulerState>>();
    let mut task = ScheduledTask::new(300, "check CI".into(), true, false);
    task.id = id.into();
    state.tasks.push(task);
}
#[tokio::test]
async fn kill_task_rpc_terminates_outstanding_background_task() {
    use echo_agent_workspace_types::rpc::workspace::{KillTaskOutcome, KillTaskResponse};
    let handle = make_handle();
    let cfg = background_capable_cfg();
    let session = handle
        .create_session_with_config(
            "kill-rpc",
            None,
            Some(cfg.clone()),
            CapabilityMode::All,
            None,
            false,
        )
        .expect("create background-capable session");
    session.set_bind_tool_config_fingerprint(serde_json::to_value(&cfg).ok());
    let out_dir = tempfile::tempdir().expect("temp dir");
    let bg = start_background_sleep(&session, out_dir.path(), "kill-rpc-task").await;
    let handler = WorkspaceRpcHandler::new(handle.clone());
    async fn kill(handler: &WorkspaceRpcHandler, task_id: &str) -> KillTaskResponse {
        let value = handler
            .dispatch(
                "workspace.kill_task",
                serde_json::json!({"session_id": "kill-rpc", "task_id": task_id}),
                Some("kill-rpc"),
            )
            .await
            .expect("kill_task rpc");
        serde_json::from_value(value).expect("decode kill response")
    }
    let killed = kill(&handler, &bg.task_id).await;
    assert_eq!(killed.task_id, bg.task_id);
    assert_eq!(killed.outcome, KillTaskOutcome::Killed);
    let missing = kill(&handler, "no-such-task").await;
    assert_eq!(missing.outcome, KillTaskOutcome::NotFound);
    let snap_value = handler
        .dispatch(
            "workspace.tasks_snapshot",
            serde_json::json!({"session_id": "kill-rpc"}),
            Some("kill-rpc"),
        )
        .await
        .expect("tasks_snapshot after kill");
    let snap: TasksSnapshotResponse = serde_json::from_value(snap_value).expect("decode snapshot");
    assert!(
        snap.background_tasks.is_empty(),
        "killed task must leave the snapshot: {:?}",
        snap.background_tasks
    );
}
#[tokio::test]
async fn tasks_snapshot_excludes_foreground_and_completed_processes() {
    use crate::handle::tests::terminal_run_request;
    use std::time::{Duration, Instant};
    let handle = make_handle();
    let cfg = background_capable_cfg();
    let session = handle
        .create_session_with_config(
            "snap-fg-rpc",
            None,
            Some(cfg.clone()),
            CapabilityMode::All,
            None,
            false,
        )
        .expect("create background-capable session");
    session.set_bind_tool_config_fingerprint(serde_json::to_value(&cfg).ok());
    let out_dir = tempfile::tempdir().expect("temp dir");
    let handler = WorkspaceRpcHandler::new(handle.clone());
    async fn snapshot(handler: &WorkspaceRpcHandler) -> TasksSnapshotResponse {
        let value = handler
            .dispatch(
                "workspace.tasks_snapshot",
                serde_json::json!({"session_id": "snap-fg-rpc"}),
                Some("snap-fg-rpc"),
            )
            .await
            .expect("tasks_snapshot rpc");
        serde_json::from_value(value).expect("decode response")
    }
    let backend = session.terminal_backend().clone();
    let fg_req = terminal_run_request("sleep 30", out_dir.path(), "snap-fg-task");
    let fg_join = tokio::spawn(async move { backend.run(fg_req).await });
    let poll_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let listed = session.terminal_backend().list_tasks().await;
        if listed.iter().any(|t| !t.completed && !t.is_backgrounded) {
            break;
        }
        assert!(
            Instant::now() < poll_deadline,
            "timeout waiting for incomplete FG in list_tasks: {listed:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let snap = snapshot(&handler).await;
    assert!(
        snap.background_tasks.is_empty(),
        "in-flight FG must not appear in tasks_snapshot: {:?}",
        snap.background_tasks
    );
    assert!(
        session
            .terminal_backend()
            .background_foreground_command("snap-fg-task")
            .await,
        "expected FG process snap-fg-task to background"
    );
    let snap = snapshot(&handler).await;
    assert!(
        snap.background_tasks
            .iter()
            .any(|t| t.task_id == "snap-fg-task"),
        "backgrounded former FG must appear: {:?}",
        snap.background_tasks
    );
    assert_eq!(
        snap.background_tasks.len(),
        1,
        "only the transitioned FG so far: {:?}",
        snap.background_tasks
    );
    let bg = start_background_sleep(&session, out_dir.path(), "snap-bg-task").await;
    let snap = snapshot(&handler).await;
    assert_eq!(
        snap.background_tasks.len(),
        2,
        "transitioned FG + incomplete BG must appear: {:?}",
        snap.background_tasks
    );
    assert!(
        snap.background_tasks
            .iter()
            .any(|t| t.task_id == bg.task_id),
        "run_background task missing: {:?}",
        snap.background_tasks
    );
    let short = session
        .terminal_backend()
        .run_background(terminal_run_request(
            "true",
            out_dir.path(),
            "snap-done-task",
        ))
        .await
        .expect("start short background task");
    let done = session
        .terminal_backend()
        .wait_for_completion(&short.task_id, Some(Duration::from_secs(5)))
        .await
        .expect("short background task should complete");
    assert!(done.completed, "short task must complete: {done:?}");
    let listed = session.terminal_backend().list_tasks().await;
    assert!(
        listed
            .iter()
            .any(|t| t.task_id == short.task_id && t.completed && t.is_backgrounded),
        "precondition: completed BG must still be in list_tasks: {listed:?}"
    );
    let snap = snapshot(&handler).await;
    assert!(
        snap.background_tasks
            .iter()
            .all(|t| t.task_id != short.task_id),
        "completed BG must not appear: {:?}",
        snap.background_tasks
    );
    assert_eq!(
        snap.background_tasks.len(),
        2,
        "still-running BG tasks remain: {:?}",
        snap.background_tasks
    );
    assert!(
        snap.background_tasks
            .iter()
            .any(|t| t.task_id == bg.task_id),
        "run_background task should still be present: {:?}",
        snap.background_tasks
    );
    assert!(
        snap.background_tasks
            .iter()
            .any(|t| t.task_id == "snap-fg-task"),
        "transitioned FG should still be present: {:?}",
        snap.background_tasks
    );
    session.terminal_backend().kill_task(&bg.task_id).await;
    session.terminal_backend().kill_task("snap-fg-task").await;
    let _ = fg_join.await;
}
#[tokio::test]
async fn handle_evict_keeps_queue_when_other_sessions_live() {
    let handle = make_handle();
    handle
        .create_session("other")
        .expect("create second session");
    let tracker = handle.activity_tracker().clone();
    let handler = WorkspaceRpcHandler::new(handle);
    handler
        .handle_evict(ToolServerEvictParams {
            session_id: SessionId::new("ghost").expect("valid session id"),
            reason: "idle_timeout".into(),
            grace_period_ms: 200,
        })
        .await;
    assert!(
        !tracker.is_draining(),
        "evict of an absent id with live sessions must not global-drain"
    );
}
#[tokio::test]
async fn handle_evict_nonlast_removes_session_and_preserves_survivors() {
    let handle = make_handle();
    handle
        .create_session("other")
        .expect("create second session");
    let tracker = handle.activity_tracker().clone();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    handler
        .handle_evict(ToolServerEvictParams {
            session_id: SessionId::new("other").expect("valid session id"),
            reason: "idle_timeout".into(),
            grace_period_ms: 200,
        })
        .await;
    assert!(
        handle.session("other").is_none(),
        "the evicted session must be removed from the map"
    );
    assert!(
        handle.session("main").is_some(),
        "a surviving session must be kept"
    );
    assert!(
        !tracker.is_draining(),
        "evicting a non-last session must not global-drain the shared queue"
    );
}
#[tokio::test]
async fn bind_rejected_after_evict_drain() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    handler
        .handle_evict(ToolServerEvictParams {
            session_id: SessionId::new("main").expect("valid session id"),
            reason: "preemption".into(),
            grace_period_ms: 100,
        })
        .await;
    assert!(matches!(
        handle.create_session("late"),
        Err(WorkspaceError::ShuttingDown)
    ));
}
#[tokio::test]
async fn repeat_evict_does_not_redrain() {
    use echo_agent_tool_protocol::ToolServerLifecycleStatus;
    let handle = make_handle();
    let tracker = handle.activity_tracker().clone();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = || ToolServerEvictParams {
        session_id: SessionId::new("main").expect("valid session id"),
        reason: "preemption".into(),
        grace_period_ms: 100,
    };
    handler.handle_evict(params()).await;
    assert_eq!(
        tracker.snapshot().status,
        ToolServerLifecycleStatus::ShuttingDown
    );
    handler.handle_evict(params()).await;
    assert_eq!(
        tracker.snapshot().status,
        ToolServerLifecycleStatus::ShuttingDown,
        "a repeat evict must not downgrade terminal ShuttingDown to Draining"
    );
}
#[tokio::test]
async fn dispatch_tool_definitions_returns_known_tools() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({"session_id": "main"});
    let result = handler
        .dispatch("workspace.tool_definitions", params, None)
        .await;
    let value = result.expect("should succeed");
    let arr = value.as_array().expect("should be array");
    assert!(!arr.is_empty(), "main session should have tools");
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|d| {
            d.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
        })
        .collect();
    assert!(
        names.contains(&"read_file"),
        "should contain read_file: {names:?}"
    );
}
#[tokio::test]
async fn dispatch_tool_definitions_unknown_session() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({"session_id": "ghost"});
    let result = handler
        .dispatch("workspace.tool_definitions", params, None)
        .await;
    assert!(matches!(result, Err(WorkspaceError::SessionNotFound(_))));
}
#[tokio::test]
async fn dispatch_get_all_hunks_returns_array() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.get_all_hunks", Value::Null, Some("main"))
        .await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_array());
}
#[tokio::test]
async fn dispatch_get_session_summary_returns_object_or_null() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.get_session_summary", Value::Null, Some("main"))
        .await;
    let value = result.expect("should succeed");
    assert!(
        value.is_object() || value.is_null(),
        "expected object or null, got {value}"
    );
}
#[tokio::test]
async fn dispatch_discover_skills_returns_array() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.discover_skills", Value::Null, None)
        .await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_array());
}
#[tokio::test]
async fn dispatch_load_envrc_returns_object() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.load_envrc", Value::Null, None)
        .await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_object());
}
#[tokio::test]
async fn dispatch_drop_session_self_succeeds() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    let params = serde_json::json!({"caller_session_id": "main", "session_id": "main"});
    let result = handler
        .dispatch("workspace.drop_session", params, None)
        .await;
    assert!(result.is_ok(), "dropping own session should succeed");
    assert!(handle.session("main").is_none(), "session should be gone");
}
#[tokio::test]
async fn dispatch_update_tool_config_missing_params() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.update_tool_config", serde_json::json!({}), None)
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::HubError(ref msg)) if msg.contains("missing")),
        "got {result:?}"
    );
}
fn caller_mismatch_count(method: &str, kind: &str) -> u64 {
    WORKSPACE_RPC_CALLER_MISMATCH_TOTAL
        .with_label_values(&[method, kind])
        .get()
}
fn baseline_config_value() -> Value {
    serde_json::to_value(crate::session::tool_config::test_support::baseline_config())
        .expect("baseline config serializes")
}
#[tokio::test]
async fn dispatch_update_tool_config_envelope_overrides_param() {
    let mismatch_before = caller_mismatch_count("update_tool_config", "param_mismatch");
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({
        "caller_session_id": "spoofed",
        "session_id": "main",
        "new_config": baseline_config_value(),
    });
    let result = handler
        .dispatch("workspace.update_tool_config", params, Some("main"))
        .await;
    assert!(
        result.is_ok(),
        "envelope caller == target must authorize: {result:?}"
    );
    assert!(
        caller_mismatch_count("update_tool_config", "param_mismatch") > mismatch_before,
        "the param/envelope disagreement must be counted"
    );
}
#[tokio::test]
async fn dispatch_update_tool_config_envelope_cross_session_unauthorized() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    let params = serde_json::json!({
        "caller_session_id": "main",
        "session_id": "main",
        "new_config": baseline_config_value(),
    });
    let result = handler
        .dispatch("workspace.update_tool_config", params, Some("other"))
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::Unauthorized { .. })),
        "got {result:?}"
    );
    assert!(
        handle.session("main").is_some(),
        "the target session must be untouched"
    );
}
#[tokio::test]
async fn dispatch_update_tool_config_param_fallback_without_envelope() {
    let absent_before = caller_mismatch_count("update_tool_config", "envelope_absent");
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({
        "caller_session_id": "main",
        "session_id": "main",
        "new_config": baseline_config_value(),
    });
    let result = handler
        .dispatch("workspace.update_tool_config", params, None)
        .await;
    assert!(result.is_ok(), "param fallback must authorize: {result:?}");
    assert!(
        caller_mismatch_count("update_tool_config", "envelope_absent") > absent_before,
        "the envelope-absent fallback must be counted"
    );
}
#[tokio::test]
async fn dispatch_update_tool_config_envelope_only_without_param() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({
        "session_id": "main",
        "new_config": baseline_config_value(),
    });
    let result = handler
        .dispatch("workspace.update_tool_config", params, Some("main"))
        .await;
    assert!(
        result.is_ok(),
        "envelope-only identity must authorize: {result:?}"
    );
}
#[test]
fn resolve_mutation_caller_clean_arms_count_nothing() {
    const METHOD: &str = "test_clean_arms";
    let mismatch_before = caller_mismatch_count(METHOD, "param_mismatch");
    let absent_before = caller_mismatch_count(METHOD, "envelope_absent");
    let caller =
        resolve_mutation_caller(METHOD, Some("sess"), None).expect("envelope-only must resolve");
    assert_eq!(caller, "sess");
    let caller = resolve_mutation_caller(METHOD, Some("sess"), Some("sess"))
        .expect("matching param must resolve");
    assert_eq!(caller, "sess");
    assert_eq!(
        caller_mismatch_count(METHOD, "param_mismatch"),
        mismatch_before,
        "clean arms must not count a mismatch"
    );
    assert_eq!(
        caller_mismatch_count(METHOD, "envelope_absent"),
        absent_before,
        "clean arms must not count an envelope-absent fallback"
    );
}
#[tokio::test]
async fn dispatch_drop_session_envelope_overrides_param() {
    let mutation_before = WORKSPACE_RPC_MUTATION_TOTAL
        .with_label_values(&["drop_session", "ok"])
        .get();
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    let params = serde_json::json!({"caller_session_id": "spoofed", "session_id": "main"});
    let result = handler
        .dispatch("workspace.drop_session", params, Some("main"))
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert!(handle.session("main").is_none(), "session should be gone");
    assert!(
        WORKSPACE_RPC_MUTATION_TOTAL
            .with_label_values(&["drop_session", "ok"])
            .get()
            > mutation_before,
        "the mutation audit counter must advance"
    );
}
#[tokio::test]
async fn dispatch_drop_session_envelope_cross_session_unauthorized() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    let params = serde_json::json!({"caller_session_id": "main", "session_id": "main"});
    let result = handler
        .dispatch("workspace.drop_session", params, Some("observer-ish"))
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::Unauthorized { .. })),
        "got {result:?}"
    );
    assert!(
        handle.session("main").is_some(),
        "the target session must survive"
    );
}
#[tokio::test]
async fn dispatch_configure_mcp_on_demand_create_enables_system_notifications() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    let _ = handler
        .dispatch(
            "workspace.configure_mcp",
            serde_json::json!({"mcp_servers": []}),
            Some("mcp-fresh"),
        )
        .await;
    let session = handle
        .session("mcp-fresh")
        .expect("session created on demand");
    assert!(
        session.system_notifications(),
        "the on-demand created session must forward system notifications"
    );
}
#[tokio::test]
async fn dispatch_hunk_action_unknown_action() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({
        "action": {"hunk_id": "test-id", "action": "dance"}
    });
    let result = handler
        .dispatch("workspace.hunk_action", params, None)
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::HubError(_))),
        "expected HubError for invalid action enum, got {result:?}"
    );
}
#[tokio::test]
async fn dispatch_hunk_action_malformed_json() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({
        "action": "not-an-object"
    });
    let result = handler
        .dispatch("workspace.hunk_action", params, None)
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::HubError(_))),
        "expected HubError for malformed action, got {result:?}"
    );
}
#[tokio::test]
async fn dispatch_hunk_action_missing_action_field() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({});
    let result = handler
        .dispatch("workspace.hunk_action", params, None)
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::HubError(ref msg)) if msg.contains("missing field")),
        "got {result:?}"
    );
}
#[tokio::test]
async fn dispatch_hunk_file_action_missing_path() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({"action": "accept"});
    let result = handler
        .dispatch("workspace.hunk_file_action", params, None)
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::HubError(ref msg)) if msg.contains("missing field")),
        "got {result:?}"
    );
}
#[tokio::test]
async fn dispatch_hunk_turn_action_missing_prompt_index() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({"action": "accept"});
    let result = handler
        .dispatch("workspace.hunk_turn_action", params, None)
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::HubError(ref msg)) if msg.contains("missing field")),
        "got {result:?}"
    );
}
#[tokio::test]
async fn dispatch_hunk_all_action_invalid_action() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({"action": "explode"});
    let result = handler
        .dispatch("workspace.hunk_all_action", params, None)
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::HubError(_))),
        "expected HubError for invalid action enum, got {result:?}"
    );
}
#[tokio::test]
async fn dispatch_hunk_get_all_file_contents_returns_array() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch(
            "workspace.hunk_get_all_file_contents",
            Value::Null,
            Some("main"),
        )
        .await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_array());
}
#[tokio::test]
async fn dispatch_hunk_get_staged_files_returns_array() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.hunk_get_staged_files", Value::Null, Some("main"))
        .await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_array());
}
#[tokio::test]
async fn dispatch_fuzzy_open_returns_search_id() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch(
            "workspace.fuzzy_open",
            serde_json::json!({"hidden": false}),
            None,
        )
        .await;
    let value = result.expect("should succeed");
    assert!(
        value.as_str().is_some_and(|s| !s.is_empty()),
        "response should be a non-empty search_id string: {value}"
    );
}
#[tokio::test]
async fn dispatch_fuzzy_close_unknown_id() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch(
            "workspace.fuzzy_close",
            serde_json::json!({"search_id": "nonexistent"}),
            None,
        )
        .await;
    let value = result.expect("should succeed");
    assert!(!value.as_bool().expect("response should be a bool"));
}
#[tokio::test]
async fn dispatch_fuzzy_change_missing_search_id() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch(
            "workspace.fuzzy_change",
            serde_json::json!({"query": "test"}),
            None,
        )
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::HubError(ref msg)) if msg.contains("missing field")),
        "got {result:?}"
    );
}
#[tokio::test]
async fn dispatch_fuzzy_search_missing_search_id() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.fuzzy_search", serde_json::json!({}), None)
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::HubError(ref msg)) if msg.contains("missing search_id")),
        "got {result:?}"
    );
}
#[tokio::test]
async fn dispatch_fuzzy_open_then_close_roundtrip() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let open_result = handler
        .dispatch(
            "workspace.fuzzy_open",
            serde_json::json!({"hidden": false}),
            None,
        )
        .await
        .expect("open should succeed");
    let search_id = open_result
        .as_str()
        .expect("open response should be a search_id string")
        .to_owned();
    let close_result = handler
        .dispatch(
            "workspace.fuzzy_close",
            serde_json::json!({"search_id": search_id}),
            None,
        )
        .await
        .expect("close should succeed");
    assert!(
        close_result
            .as_bool()
            .expect("close response should be a bool")
    );
    let close_again = handler
        .dispatch(
            "workspace.fuzzy_close",
            serde_json::json!({"search_id": search_id}),
            None,
        )
        .await
        .expect("close again should succeed");
    assert!(
        !close_again
            .as_bool()
            .expect("close-again response should be a bool")
    );
}
#[tokio::test]
async fn handle_call_wraps_in_envelope_with_value() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let mut ctx = ToolCallContext::default();
    ctx.extensions
        .insert(echo_agent_tool_runtime::SessionContext("main".to_owned()));
    let args = serde_json::json!({
        "method": "workspace.get_session_summary",
        "params": {}
    });
    let mut stream = handler.handle_call(ctx, args).await;
    let item = next_item(&mut stream).await.expect("should have terminal");
    match item {
        echo_agent_tool_runtime::ToolStreamItem::Terminal(Ok(typed)) => {
            let ok_val = typed
                .value
                .get("ok")
                .expect("envelope should have 'ok' key");
            assert!(
                ok_val.is_object() || ok_val.is_null(),
                "ok value should be object or null, got {ok_val}"
            );
        }
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
}
#[tokio::test]
async fn handle_call_error_envelope() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let ctx = ToolCallContext::default();
    let args = serde_json::json!({
        "method": "workspace.nonexistent",
        "params": {}
    });
    let mut stream = handler.handle_call(ctx, args).await;
    let item = next_item(&mut stream).await.expect("should have terminal");
    match item {
        echo_agent_tool_runtime::ToolStreamItem::Terminal(Ok(typed)) => {
            assert!(
                typed.value.get("err").is_some(),
                "envelope should have 'err' key: {}",
                typed.value
            );
            let err = typed.value.get("err").unwrap();
            assert!(err.get("code").is_some());
            assert!(err.get("message").is_some());
        }
        other => panic!("expected Terminal(Ok(envelope)), got {other:?}"),
    }
}
#[tokio::test]
async fn handle_call_records_rpc_metrics_and_collapses_unknown_method() {
    let handler = WorkspaceRpcHandler::new(make_handle());
    let ok_before = WORKSPACE_RPC_REQUESTS_TOTAL
        .with_label_values(&["workspace.get_session_summary", "ok"])
        .get();
    let dur_samples_before = WORKSPACE_RPC_DURATION_SECONDS
        .with_label_values(&["workspace.get_session_summary"])
        .get_sample_count();
    let mut ctx = ToolCallContext::default();
    ctx.extensions
        .insert(echo_agent_tool_runtime::SessionContext("main".to_owned()));
    let mut stream = handler
        .handle_call(
            ctx,
            serde_json::json!({"method": "workspace.get_session_summary", "params": {}}),
        )
        .await;
    let _ = next_item(&mut stream).await;
    assert!(
        WORKSPACE_RPC_REQUESTS_TOTAL
            .with_label_values(&["workspace.get_session_summary", "ok"])
            .get()
            > ok_before,
        "a known ok RPC must increment its per-method ok counter"
    );
    assert!(
        WORKSPACE_RPC_DURATION_SECONDS
            .with_label_values(&["workspace.get_session_summary"])
            .get_sample_count()
            > dur_samples_before,
        "the dispatch must observe the per-method duration histogram"
    );
    const BOGUS: &str = "workspace.__test_bogus_method_zzz";
    let unknown_before = WORKSPACE_RPC_REQUESTS_TOTAL
        .with_label_values(&[UNKNOWN_METHOD_LABEL, "error"])
        .get();
    let kind_before = WORKSPACE_RPC_ERRORS_TOTAL
        .with_label_values(&[UNKNOWN_METHOD_LABEL, "unknown_method"])
        .get();
    let mut stream = handler
        .handle_call(
            ToolCallContext::default(),
            serde_json::json!({"method": BOGUS, "params": {}}),
        )
        .await;
    let _ = next_item(&mut stream).await;
    assert!(
        WORKSPACE_RPC_REQUESTS_TOTAL
            .with_label_values(&[UNKNOWN_METHOD_LABEL, "error"])
            .get()
            > unknown_before,
        "an unrecognized method must increment the collapsed unknown/error counter"
    );
    assert!(
        WORKSPACE_RPC_ERRORS_TOTAL
            .with_label_values(&[UNKNOWN_METHOD_LABEL, "unknown_method"])
            .get()
            > kind_before,
        "a failed dispatch must also record its error_kind on the errors counter"
    );
    let has_bogus_series = prometheus::gather()
        .iter()
        .filter(|mf| mf.name() == "echo_agent_workspace_rpc_requests_total")
        .flat_map(|mf| mf.get_metric())
        .any(|m| {
            m.get_label()
                .iter()
                .any(|l| l.name() == "method" && l.value() == BOGUS)
        });
    assert!(
        !has_bogus_series,
        "the raw bad method must collapse to `unknown`, never its own series"
    );
}
#[tokio::test]
async fn dispatch_git_stage_non_git_dir_returns_error() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.git_stage", serde_json::json!({}), None)
        .await;
    assert!(result.is_err(), "non-git dir should error");
}
#[tokio::test]
async fn dispatch_git_commit_missing_message_returns_error() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.git_commit", serde_json::json!({}), None)
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::HubError(ref msg)) if msg.contains("missing field"))
    );
}
#[tokio::test]
async fn dispatch_git_checkout_missing_branch_returns_error() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.git_checkout", serde_json::json!({}), None)
        .await;
    assert!(
        matches!(result, Err(WorkspaceError::HubError(ref msg)) if msg.contains("missing field"))
    );
}
#[tokio::test]
async fn dispatch_git_stage_content_missing_fields() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let result = handler
        .dispatch("workspace.git_stage_content", serde_json::json!({}), None)
        .await;
    assert!(matches!(result, Err(WorkspaceError::HubError(ref msg)) if msg.contains("missing")));
}
#[tokio::test]
async fn handle_hook_before_turn_sets_turn_state() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    let payload = turn_hook::BeforeTurnPayload {
        turn_number: 1,
        model_id: "echo-agent-3".to_string(),
        yolo_mode: false,
        conversation_message_count: 0,
        session_relationship: "primary".to_string(),
        schema_version: "1.0".to_string(),
    };
    let frame = HookFrame {
        session_id: SessionId::new("main").unwrap(),
        tool_id: None,
        call_id: None,
        hook_id: None,
        event: HookEvent::Custom {
            kind: turn_hook::BEFORE_TURN_KIND.to_string(),
            payload: serde_json::to_value(&payload).unwrap(),
        },
        trace_context: None,
    };
    handler
        .handle_hook(SessionId::new("main").unwrap(), frame)
        .await;
    let tracker = handle.activity_tracker();
    assert!(
        tracker.known_sessions().contains(&"main".to_string()),
        "before_turn hook should create a session entry in the activity tracker"
    );
}
#[tokio::test]
async fn handle_hook_after_turn_does_not_panic() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    handle.activity_tracker().turn_started("main", 1);
    let payload = turn_hook::AfterTurnPayload {
        turn_number: 1,
        outcome: turn_hook::TurnHookOutcome::Completed,
        duration_ms: 500,
        tool_call_count: 3,
        model_id: "echo-agent-3".to_string(),
        written_repo_paths: Vec::new(),
        cancellation_category: None,
        cancellation_context: None,
    };
    let frame = HookFrame {
        session_id: SessionId::new("main").unwrap(),
        tool_id: None,
        call_id: None,
        hook_id: None,
        event: HookEvent::Custom {
            kind: turn_hook::AFTER_TURN_KIND.to_string(),
            payload: serde_json::to_value(&payload).unwrap(),
        },
        trace_context: None,
    };
    handler
        .handle_hook(SessionId::new("main").unwrap(), frame)
        .await;
}
#[tokio::test]
async fn handle_hook_malformed_payload_does_not_panic() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let frame = HookFrame {
        session_id: SessionId::new("main").unwrap(),
        tool_id: None,
        call_id: None,
        hook_id: None,
        event: HookEvent::Custom {
            kind: turn_hook::BEFORE_TURN_KIND.to_string(),
            payload: serde_json::json!({"garbage": true}),
        },
        trace_context: None,
    };
    handler
        .handle_hook(SessionId::new("main").unwrap(), frame)
        .await;
}
#[tokio::test]
async fn handle_hook_unrecognized_custom_kind_does_not_panic() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let frame = HookFrame {
        session_id: SessionId::new("main").unwrap(),
        tool_id: None,
        call_id: None,
        hook_id: None,
        event: HookEvent::Custom {
            kind: "unknown_kind".to_string(),
            payload: serde_json::json!({}),
        },
        trace_context: None,
    };
    handler
        .handle_hook(SessionId::new("main").unwrap(), frame)
        .await;
}
#[tokio::test]
async fn handle_hook_cancel_marks_call_completed() {
    use echo_agent_tool_protocol::ToolCallId;
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    let tracker = handle.activity_tracker();
    tracker.tool_call_started("call-42", "read_file", Some("main"));
    assert_eq!(tracker.snapshot().active_tool_calls, 1);
    let frame = HookFrame::cancel(
        SessionId::new("main").unwrap(),
        ToolId::new("read_file").unwrap(),
        ToolCallId::new("call-42").unwrap(),
    );
    handler
        .handle_hook(SessionId::new("main").unwrap(), frame)
        .await;
    assert_eq!(
        tracker.snapshot().active_tool_calls,
        0,
        "cancel hook should mark the call as completed"
    );
}
#[tokio::test]
async fn handle_hook_cancel_without_call_id_cancels_all_session_calls() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    let tracker = handle.activity_tracker();
    tracker.tool_call_started("call-a", "grep", Some("main"));
    tracker.tool_call_started("call-b", "read_file", Some("main"));
    tracker.tool_call_started("call-c", "write", Some("other"));
    assert_eq!(tracker.snapshot().active_tool_calls, 3);
    let frame = HookFrame {
        session_id: SessionId::new("main").unwrap(),
        tool_id: None,
        call_id: None,
        hook_id: None,
        event: HookEvent::Cancel,
        trace_context: None,
    };
    handler
        .handle_hook(SessionId::new("main").unwrap(), frame)
        .await;
    assert_eq!(
        tracker.snapshot_session("main").active_tool_calls,
        0,
        "session-wide cancel should complete all calls for the session"
    );
    assert_eq!(
        tracker.snapshot_session("other").active_tool_calls,
        1,
        "cancel must not affect calls in other sessions"
    );
    assert_eq!(tracker.snapshot().active_tool_calls, 1);
}
#[tokio::test]
async fn handle_hook_session_ended_clears_turn_active() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle.clone());
    let tracker = handle.activity_tracker();
    tracker.turn_started("main", 1);
    assert!(tracker.is_turn_active("main"));
    let frame = HookFrame::session_ended(SessionId::new("main").unwrap());
    handler
        .handle_hook(SessionId::new("main").unwrap(), frame)
        .await;
    assert!(
        !tracker.is_turn_active("main"),
        "session_ended hook should clear turn_active"
    );
}
use crate::workspace_ops::{GetFilesRes, PutFilesRes};
fn test_sha256(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(data))
}
#[tokio::test]
async fn dispatch_put_files_writes_and_returns_hash() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({
        "files": [{"path": "test_file.txt", "content": "hello world"}]
    });
    let result = handler
        .dispatch("workspace.put_files", params, None)
        .await
        .expect("dispatch should succeed");
    let res: PutFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(res.results[0].ok, "write should succeed");
    let expected_hash = test_sha256(b"hello world");
    assert_eq!(
        res.results[0].hash.as_deref(),
        Some(expected_hash.as_str()),
        "hash should be SHA-256 of written content"
    );
    assert!(res.results[0].error.is_none(), "no error expected");
    let on_disk = std::fs::read_to_string(root.join("test_file.txt")).unwrap();
    assert_eq!(on_disk, "hello world");
}
#[tokio::test]
async fn dispatch_put_get_files_resolve_against_bound_session_cwd() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    std::fs::create_dir(root.join("artifacts")).unwrap();
    handle
        .create_session_with_cwd("cwd-session", Some(root.join("artifacts")))
        .unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({
        "files": [{"path": "out.txt", "content": "rebased"}]
    });
    let result = handler
        .dispatch("workspace.put_files", params, Some("cwd-session"))
        .await
        .expect("dispatch should succeed");
    let res: PutFilesRes = serde_json::from_value(result).unwrap();
    assert!(res.results[0].ok, "{:?}", res.results[0].error);
    let on_disk = std::fs::read_to_string(root.join("artifacts").join("out.txt")).unwrap();
    assert_eq!(on_disk, "rebased");
    let params = serde_json::json!({ "files": [{"path": "out.txt"}] });
    let result = handler
        .dispatch("workspace.get_files", params.clone(), Some("cwd-session"))
        .await
        .expect("dispatch should succeed");
    let res: GetFilesRes = serde_json::from_value(result).unwrap();
    assert!(res.results[0].exists);
    assert_eq!(res.results[0].content.as_deref(), Some("rebased"));
    let result = handler
        .dispatch("workspace.get_files", params, None)
        .await
        .expect("dispatch should succeed");
    let res: GetFilesRes = serde_json::from_value(result).unwrap();
    assert!(!res.results[0].exists);
}
#[tokio::test]
async fn dispatch_put_files_rejects_path_traversal() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({
        "files": [{"path": "../escape.txt", "content": "evil"}]
    });
    let result = handler
        .dispatch("workspace.put_files", params, None)
        .await
        .expect("dispatch itself should succeed");
    let res: PutFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(!res.results[0].ok, "path traversal should be rejected");
    assert!(
        res.results[0]
            .error
            .as_ref()
            .unwrap()
            .contains("escapes workspace root"),
        "error should mention escape: {:?}",
        res.results[0].error
    );
}
#[tokio::test]
async fn dispatch_resolve_file_references_rejects_outside_root_when_confined() {
    let handle = make_confining_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let secret = std::env::temp_dir().join("h1_3885911_outside_secret.txt");
    std::fs::write(&secret, "OUTSIDE_SECRET").unwrap();
    let params = serde_json::json!({
        "refs": [secret.to_string_lossy(), "../escape.txt"]
    });
    let result = handler
        .dispatch("workspace.resolve_file_references", params, None)
        .await
        .expect("dispatch itself should succeed");
    let arr = result.as_array().expect("results array");
    assert_eq!(arr.len(), 2);
    for entry in arr {
        assert_eq!(entry["exists"], serde_json::Value::Bool(false));
        assert_eq!(entry["content"], serde_json::Value::Null);
        assert!(
            entry["error"]
                .as_str()
                .unwrap_or_default()
                .contains("escapes workspace root"),
            "escape should be rejected, not read: {entry:?}"
        );
    }
    std::fs::remove_file(&secret).ok();
}
#[tokio::test]
async fn dispatch_resolve_file_references_confines_to_session_base() {
    let handle = make_confining_handle();
    let root = handle.root_cwd().unwrap();
    std::fs::create_dir(root.join("artifacts")).unwrap();
    std::fs::write(root.join("rooted.txt"), "ROOT_ONLY").unwrap();
    handle
        .create_session_with_cwd("cwd-session", Some(root.join("artifacts")))
        .unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({ "refs": ["../rooted.txt"] });
    let result = handler
        .dispatch(
            "workspace.resolve_file_references",
            params,
            Some("cwd-session"),
        )
        .await
        .expect("dispatch itself should succeed");
    let arr = result.as_array().expect("results array");
    assert_eq!(arr[0]["exists"], serde_json::Value::Bool(false));
    assert_eq!(arr[0]["content"], serde_json::Value::Null);
    assert!(
        arr[0]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("escapes workspace root"),
        "escape above the base should be rejected: {:?}",
        arr[0]
    );
}
#[tokio::test]
async fn dispatch_resolve_file_references_uses_bound_session_base() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    std::fs::create_dir(root.join("artifacts")).unwrap();
    std::fs::write(root.join("artifacts").join("out.txt"), "rebased").unwrap();
    handle
        .create_session_with_cwd("cwd-session", Some(root.join("artifacts")))
        .unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({ "refs": ["out.txt"] });
    let result = handler
        .dispatch(
            "workspace.resolve_file_references",
            params,
            Some("cwd-session"),
        )
        .await
        .expect("dispatch should succeed");
    let arr = result.as_array().expect("results array");
    assert_eq!(arr[0]["exists"], serde_json::Value::Bool(true));
    assert_eq!(arr[0]["content"], serde_json::json!("rebased"));
}
#[tokio::test]
async fn handle_hook_pause_resume_are_noops() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    for event in [HookEvent::Pause, HookEvent::Resume] {
        let frame = HookFrame {
            session_id: SessionId::new("main").unwrap(),
            tool_id: None,
            call_id: None,
            hook_id: None,
            event,
            trace_context: None,
        };
        handler
            .handle_hook(SessionId::new("main").unwrap(), frame)
            .await;
    }
}
#[tokio::test]
async fn dispatch_put_files_rejects_absolute_outside_root() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({
        "files": [{"path": "/etc/passwd", "content": "evil"}]
    });
    let result = handler
        .dispatch("workspace.put_files", params, None)
        .await
        .expect("dispatch itself should succeed");
    let res: PutFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(
        !res.results[0].ok,
        "absolute path outside root should be rejected"
    );
    assert!(
        res.results[0]
            .error
            .as_ref()
            .unwrap()
            .contains("escapes workspace root"),
        "error should mention escape: {:?}",
        res.results[0].error
    );
}
#[tokio::test]
async fn dispatch_put_files_accepts_absolute_within_root() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let abs = root.join("sub/abs.txt");
    let params = serde_json::json!({
        "files": [{"path": abs.to_str().expect("utf-8 path"), "content": "hello"}]
    });
    let result = handler
        .dispatch("workspace.put_files", params, None)
        .await
        .expect("dispatch itself should succeed");
    let res: PutFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(
        res.results[0].ok,
        "absolute path within root should be accepted: {:?}",
        res.results[0].error
    );
    assert_eq!(
        std::fs::read_to_string(root.join("sub/abs.txt")).unwrap(),
        "hello"
    );
}
#[tokio::test]
#[cfg(unix)]
async fn dispatch_put_files_rejects_symlink_escape() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let outside = tempfile::tempdir().expect("create outside dir");
    std::os::unix::fs::symlink(outside.path(), root.join("escape_link")).expect("create symlink");
    let params = serde_json::json!({
        "files": [{"path": "escape_link/evil.txt", "content": "pwned"}]
    });
    let result = handler
        .dispatch("workspace.put_files", params, None)
        .await
        .expect("dispatch itself should succeed");
    let res: PutFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(!res.results[0].ok, "symlink escape should be rejected");
    assert!(
        res.results[0]
            .error
            .as_ref()
            .unwrap()
            .contains("symlink escape"),
        "error should mention symlink: {:?}",
        res.results[0].error
    );
    assert!(
        !outside.path().join("evil.txt").exists(),
        "file must not be created outside workspace"
    );
}
#[tokio::test]
async fn dispatch_put_files_partial_failure() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({
        "files": [
            {"path": "good.txt", "content": "valid content"},
            {"path": "../bad.txt", "content": "should fail"},
        ]
    });
    let result = handler
        .dispatch("workspace.put_files", params, None)
        .await
        .expect("dispatch should succeed");
    let res: PutFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 2);
    assert!(res.results[0].ok, "first file should succeed");
    assert!(res.results[0].hash.is_some(), "first file should have hash");
    assert!(!res.results[1].ok, "second file should fail");
    assert!(
        res.results[1].error.is_some(),
        "second file should have error"
    );
    assert!(
        res.results[1].hash.is_none(),
        "failed file should have no hash"
    );
    let on_disk = std::fs::read_to_string(root.join("good.txt")).unwrap();
    assert_eq!(on_disk, "valid content");
}
#[tokio::test]
async fn dispatch_get_files_reads_existing_file() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let content = "read me back";
    std::fs::write(root.join("readable.txt"), content).unwrap();
    let params = serde_json::json!({
        "files": [{"path": "readable.txt"}]
    });
    let result = handler
        .dispatch("workspace.get_files", params, None)
        .await
        .expect("dispatch should succeed");
    let res: GetFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(res.results[0].exists, "file should exist");
    assert_eq!(
        res.results[0].content.as_deref(),
        Some(content),
        "content should match what was written"
    );
    let expected_hash = test_sha256(content.as_bytes());
    assert_eq!(
        res.results[0].hash.as_deref(),
        Some(expected_hash.as_str()),
        "hash should be SHA-256 of file content"
    );
    assert!(!res.results[0].matched);
    assert_eq!(
        res.results[0].size,
        Some(content.len() as u64),
        "size should match content length"
    );
    assert!(res.results[0].error.is_none());
}
#[tokio::test]
async fn dispatch_get_files_nonexistent_returns_not_exists() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let params = serde_json::json!({
        "files": [{"path": "does_not_exist.txt"}]
    });
    let result = handler
        .dispatch("workspace.get_files", params, None)
        .await
        .expect("dispatch should succeed");
    let res: GetFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(!res.results[0].exists, "file should not exist");
    assert!(res.results[0].content.is_none());
    assert!(res.results[0].hash.is_none());
    assert!(!res.results[0].matched);
    assert!(
        res.results[0].error.is_none(),
        "missing file is not an error"
    );
}
#[tokio::test]
async fn dispatch_get_files_io_error_returns_exists_true() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    std::fs::create_dir_all(root.join("a_directory")).unwrap();
    let params = serde_json::json!({
        "files": [{"path": "a_directory"}]
    });
    let result = handler
        .dispatch("workspace.get_files", params, None)
        .await
        .expect("dispatch should succeed");
    let res: GetFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(res.results[0].exists, "directory exists on disk");
    assert!(
        res.results[0].error.is_some(),
        "reading a directory as file should fail: {:?}",
        res.results[0]
    );
    assert!(res.results[0].content.is_none(), "no content on error");
}
#[tokio::test]
async fn dispatch_get_files_non_utf8_returns_error_with_hash() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let binary_content: &[u8] = b"\xff\xfe\x00\x01";
    std::fs::write(root.join("binary.bin"), binary_content).unwrap();
    let params = serde_json::json!({
        "files": [{"path": "binary.bin"}]
    });
    let result = handler
        .dispatch("workspace.get_files", params, None)
        .await
        .expect("dispatch should succeed");
    let res: GetFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(res.results[0].exists, "file should exist");
    assert!(
        res.results[0].content.is_none(),
        "non-UTF-8 content should be None"
    );
    let expected_hash = test_sha256(binary_content);
    assert_eq!(
        res.results[0].hash.as_deref(),
        Some(expected_hash.as_str()),
        "hash should be SHA-256 of file content even for non-UTF-8 files"
    );
    assert!(
        res.results[0]
            .error
            .as_ref()
            .unwrap()
            .contains("not valid UTF-8"),
        "error should mention UTF-8: {:?}",
        res.results[0].error
    );
    assert_eq!(
        res.results[0].size,
        Some(4),
        "size should still be reported"
    );
}
#[tokio::test]
async fn dispatch_get_files_cache_hit() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let content = "cacheable content";
    std::fs::write(root.join("cached.txt"), content).unwrap();
    let expected_hash = test_sha256(content.as_bytes());
    let params = serde_json::json!({
        "files": [{"path": "cached.txt", "if_none_match": expected_hash}]
    });
    let result = handler
        .dispatch("workspace.get_files", params, None)
        .await
        .expect("dispatch should succeed");
    let res: GetFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(res.results[0].exists);
    assert!(res.results[0].matched, "should be a cache hit");
    assert!(
        res.results[0].content.is_none(),
        "content should be omitted on cache hit"
    );
    assert_eq!(
        res.results[0].hash.as_deref(),
        Some(expected_hash.as_str()),
        "hash should still be returned"
    );
    assert!(res.results[0].error.is_none());
}
#[tokio::test]
async fn dispatch_get_files_cache_miss() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let content = "fresh content";
    std::fs::write(root.join("stale.txt"), content).unwrap();
    let params = serde_json::json!({
        "files": [{"path": "stale.txt", "if_none_match": "0000000000000000000000000000000000000000000000000000000000000000"}]
    });
    let result = handler
        .dispatch("workspace.get_files", params, None)
        .await
        .expect("dispatch should succeed");
    let res: GetFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(res.results[0].exists);
    assert!(!res.results[0].matched, "should be a cache miss");
    assert_eq!(
        res.results[0].content.as_deref(),
        Some(content),
        "content should be returned on miss"
    );
    let expected_hash = test_sha256(content.as_bytes());
    assert_eq!(
        res.results[0].hash.as_deref(),
        Some(expected_hash.as_str()),
        "current hash should be returned"
    );
}
#[tokio::test]
async fn dispatch_put_then_get_round_trip() {
    let handle = make_handle();
    let handler = WorkspaceRpcHandler::new(handle);
    let content = "round trip content";
    let put_params = serde_json::json!({
        "files": [{"path": "round_trip.txt", "content": content}]
    });
    let put_result = handler
        .dispatch("workspace.put_files", put_params, None)
        .await
        .expect("put should succeed");
    let put_res: PutFilesRes = serde_json::from_value(put_result).unwrap();
    assert!(put_res.results[0].ok);
    let put_hash = put_res.results[0].hash.clone().unwrap();
    let get_params = serde_json::json!({
        "files": [{"path": "round_trip.txt"}]
    });
    let get_result = handler
        .dispatch("workspace.get_files", get_params, None)
        .await
        .expect("get should succeed");
    let get_res: GetFilesRes = serde_json::from_value(get_result).unwrap();
    assert!(get_res.results[0].exists);
    assert_eq!(
        get_res.results[0].content.as_deref(),
        Some(content),
        "content should match what was written"
    );
    assert_eq!(
        get_res.results[0].hash.as_deref(),
        Some(put_hash.as_str()),
        "get hash should match put hash"
    );
}
#[tokio::test]
async fn dispatch_put_files_append_mode() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let params1 = serde_json::json!({
        "files": [{"path": "chunked.txt", "content": "hello", "append": false}]
    });
    let res1 = handler
        .dispatch("workspace.put_files", params1, None)
        .await
        .expect("first chunk should succeed");
    let put1: PutFilesRes = serde_json::from_value(res1).unwrap();
    assert!(put1.results[0].ok);
    let chunk1_hash = put1.results[0].hash.clone().unwrap();
    assert_eq!(
        chunk1_hash,
        test_sha256(b"hello"),
        "hash should be of the appended chunk, not full file"
    );
    let params2 = serde_json::json!({
        "files": [{"path": "chunked.txt", "content": " world", "append": true}]
    });
    let res2 = handler
        .dispatch("workspace.put_files", params2, None)
        .await
        .expect("second chunk should succeed");
    let put2: PutFilesRes = serde_json::from_value(res2).unwrap();
    assert!(put2.results[0].ok);
    let chunk2_hash = put2.results[0].hash.clone().unwrap();
    assert_eq!(
        chunk2_hash,
        test_sha256(b" world"),
        "hash should be of the appended chunk only"
    );
    let on_disk = std::fs::read_to_string(root.join("chunked.txt")).unwrap();
    assert_eq!(on_disk, "hello world");
}
#[tokio::test]
async fn dispatch_get_files_byte_range() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let content = "0123456789";
    std::fs::write(root.join("range.txt"), content).unwrap();
    let params = serde_json::json!({
        "files": [{"path": "range.txt", "offset": 3, "length": 4}]
    });
    let result = handler
        .dispatch("workspace.get_files", params, None)
        .await
        .expect("dispatch should succeed");
    let res: GetFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(res.results[0].exists);
    assert_eq!(
        res.results[0].content.as_deref(),
        Some("3456"),
        "should return only the requested byte range"
    );
    let full_hash = test_sha256(content.as_bytes());
    assert_eq!(
        res.results[0].hash.as_deref(),
        Some(full_hash.as_str()),
        "hash should be of the full file, not the chunk"
    );
    assert!(!res.results[0].matched);
    assert_eq!(
        res.results[0].size,
        Some(content.len() as u64),
        "size should be full file size"
    );
}
#[tokio::test]
async fn dispatch_get_files_byte_range_cache_hit() {
    let handle = make_handle();
    let root = handle.root_cwd().unwrap();
    let handler = WorkspaceRpcHandler::new(handle);
    let content = "abcdefghij";
    std::fs::write(root.join("range_cache.txt"), content).unwrap();
    let full_hash = test_sha256(content.as_bytes());
    let params = serde_json::json!({
        "files": [{
            "path": "range_cache.txt",
            "offset": 2,
            "length": 3,
            "if_none_match": full_hash,
        }]
    });
    let result = handler
        .dispatch("workspace.get_files", params, None)
        .await
        .expect("dispatch should succeed");
    let res: GetFilesRes = serde_json::from_value(result).unwrap();
    assert_eq!(res.results.len(), 1);
    assert!(res.results[0].exists);
    assert!(res.results[0].matched, "should be a cache hit");
    assert!(
        res.results[0].content.is_none(),
        "content should be omitted on cache hit"
    );
    assert_eq!(
        res.results[0].hash.as_deref(),
        Some(full_hash.as_str()),
        "hash should still be returned"
    );
    assert_eq!(res.results[0].size, Some(10));
}
#[tokio::test]
async fn dispatch_knows_every_typed_method() {
    use crate::file_system::{
        ContentSearchRequest, FsDeleteFileReq, FsExistsReq, FsListReq, FsReadFileReq,
        FsWriteFileReq,
    };
    use crate::workspace_ops::*;
    use crate::worktree::{ApplyWorktreeRequest, CreateWorktreeRequest, RemoveWorktreeRequest};
    use echo_agent_workspace_types::rpc::git::{GitBranchInfoReq, GitMetadataReq};
    use echo_agent_workspace_types::rpc::search::FuzzyStatusReq;
    use echo_agent_workspace_types::rpc::skills::DiscoverPluginsReq;
    use echo_agent_workspace_types::rpc::workspace::{
        ConfigureMcpReq, DropSessionReq, InstallPluginReq, LoadEnvrcReq, LoadPermissionsReq,
        LoadProjectConfigReq, RefreshPluginsReq, ResolveFileReferencesReq, ToolDefinitionsReq,
        UpdateToolConfigReq,
    };
    use echo_agent_workspace_types::rpc::worktree::WorktreeCreateSyncReq;
    let handler = WorkspaceRpcHandler::new(make_handle());
    let methods = [
        <WorkspaceInfoReq as WorkspaceRpc>::METHOD,
        <ReposListReq as WorkspaceRpc>::METHOD,
        <GitStatusReq as WorkspaceRpc>::METHOD,
        <DiscoverSkillsReq as WorkspaceRpc>::METHOD,
        <DiscoverAgentsMdReq as WorkspaceRpc>::METHOD,
        <GitStatusExtReq as WorkspaceRpc>::METHOD,
        <GitFilesReq as WorkspaceRpc>::METHOD,
        <GitDiffReq as WorkspaceRpc>::METHOD,
        <GitStageReq as WorkspaceRpc>::METHOD,
        <GitStageContentReq as WorkspaceRpc>::METHOD,
        <GitUnstageReq as WorkspaceRpc>::METHOD,
        <GitDiscardReq as WorkspaceRpc>::METHOD,
        <GitCommitReq as WorkspaceRpc>::METHOD,
        <GitSyncBaseReq as WorkspaceRpc>::METHOD,
        <GitCheckoutReq as WorkspaceRpc>::METHOD,
        <GitEnsureBindingReq as WorkspaceRpc>::METHOD,
        <GitMergeToMainReq as WorkspaceRpc>::METHOD,
        <GitPushReq as WorkspaceRpc>::METHOD,
        <GitStashReq as WorkspaceRpc>::METHOD,
        <GitInfoReq as WorkspaceRpc>::METHOD,
        <GitBranchesReq as WorkspaceRpc>::METHOD,
        <GitResolveRootReq as WorkspaceRpc>::METHOD,
        <GitCurrentCommitReq as WorkspaceRpc>::METHOD,
        <DetectVcsKindReq as WorkspaceRpc>::METHOD,
        <GitCheckoutCommitReq as WorkspaceRpc>::METHOD,
        <GitBranchInfoReq as WorkspaceRpc>::METHOD,
        <GitMetadataReq as WorkspaceRpc>::METHOD,
        <PutFilesReq as WorkspaceRpc>::METHOD,
        <GetFilesReq as WorkspaceRpc>::METHOD,
        <FsListReq as WorkspaceRpc>::METHOD,
        <FsExistsReq as WorkspaceRpc>::METHOD,
        <FsReadFileReq as WorkspaceRpc>::METHOD,
        <FsWriteFileReq as WorkspaceRpc>::METHOD,
        <FsDeleteFileReq as WorkspaceRpc>::METHOD,
        <HunkSingleActionReq as WorkspaceRpc>::METHOD,
        <HunkFileActionReq as WorkspaceRpc>::METHOD,
        <HunkTurnActionReq as WorkspaceRpc>::METHOD,
        <HunkAllActionReq as WorkspaceRpc>::METHOD,
        <HunkGetAllFileContentsReq as WorkspaceRpc>::METHOD,
        <HunkGetSessionSummaryReq as WorkspaceRpc>::METHOD,
        <HunkGetAllHunksReq as WorkspaceRpc>::METHOD,
        <HunkGetStagedFilesReq as WorkspaceRpc>::METHOD,
        <HunkGetFilteredHunksReq as WorkspaceRpc>::METHOD,
        <HunkGetFileSummariesReq as WorkspaceRpc>::METHOD,
        <CodeGotoDefinitionReq as WorkspaceRpc>::METHOD,
        <CodeGotoReferencesReq as WorkspaceRpc>::METHOD,
        <CodeFindDefinitionsReq as WorkspaceRpc>::METHOD,
        <CodeFindReferencesReq as WorkspaceRpc>::METHOD,
        <CodeIndexStatusReq as WorkspaceRpc>::METHOD,
        <ContentSearchRequest as WorkspaceRpc>::METHOD,
        <FuzzyOpenReq as WorkspaceRpc>::METHOD,
        <FuzzyChangeReq as WorkspaceRpc>::METHOD,
        <FuzzyCloseReq as WorkspaceRpc>::METHOD,
        <FuzzyStatusReq as WorkspaceRpc>::METHOD,
        <CreateWorktreeRequest as WorkspaceRpc>::METHOD,
        <WorktreeCreateSyncReq as WorkspaceRpc>::METHOD,
        <RemoveWorktreeRequest as WorkspaceRpc>::METHOD,
        <ApplyWorktreeRequest as WorkspaceRpc>::METHOD,
        <WorktreeListReq as WorkspaceRpc>::METHOD,
        <WorktreeShowReq as WorkspaceRpc>::METHOD,
        <WorktreeDetachReq as WorkspaceRpc>::METHOD,
        <WorktreeSalvageReq as WorkspaceRpc>::METHOD,
        <WorktreeCleanArtifactsReq as WorkspaceRpc>::METHOD,
        <WorktreeDbPathReq as WorkspaceRpc>::METHOD,
        <WorktreeDbStatsReq as WorkspaceRpc>::METHOD,
        <PrepareWorktreeFromWorktreeReq as WorkspaceRpc>::METHOD,
        <CreateWorktreeFromWorktreeSyncReq as WorkspaceRpc>::METHOD,
        <BeginPromptReq as WorkspaceRpc>::METHOD,
        <EndPromptReq as WorkspaceRpc>::METHOD,
        <GetRewindPointsReq as WorkspaceRpc>::METHOD,
        <RewindToReq as WorkspaceRpc>::METHOD,
        <echo_agent_workspace_types::rpc::presence::PresenceNoteReq as WorkspaceRpc>::METHOD,
        <HookRegistryReq as WorkspaceRpc>::METHOD,
        <LoadProjectConfigReq as WorkspaceRpc>::METHOD,
        <LoadPermissionsReq as WorkspaceRpc>::METHOD,
        <LoadEnvrcReq as WorkspaceRpc>::METHOD,
        <ToolDefinitionsReq as WorkspaceRpc>::METHOD,
        <ResolveFileReferencesReq as WorkspaceRpc>::METHOD,
        <UpdateToolConfigReq as WorkspaceRpc>::METHOD,
        <DropSessionReq as WorkspaceRpc>::METHOD,
        <ConfigureMcpReq as WorkspaceRpc>::METHOD,
        <InstallPluginReq as WorkspaceRpc>::METHOD,
        <RefreshPluginsReq as WorkspaceRpc>::METHOD,
        <DiscoverPluginsReq as WorkspaceRpc>::METHOD,
        <ExportGithubReq as WorkspaceRpc>::METHOD,
    ];
    let skipped_global_db_mutators = [
        <WorktreeGcReq as WorkspaceRpc>::METHOD,
        <WorktreeDbRebuildReq as WorkspaceRpc>::METHOD,
    ];
    assert_eq!(skipped_global_db_mutators.len(), 2);
    for method in methods {
        let result = handler.dispatch(method, serde_json::json!({}), None).await;
        if let Err(e) = &result {
            assert!(
                !e.to_string().contains("unknown workspace method"),
                "dispatch does not know {method}: {e}"
            );
        }
    }
}
#[tokio::test]
async fn dispatch_stamps_client_rpc_activity_for_mutations_only() {
    use crate::file_system::{FsListReq, FsWriteFileReq};
    use echo_agent_tool_protocol::IdleWithholdReason;
    use echo_agent_workspace_types::rpc::workspace::DropSessionReq;
    let handler = WorkspaceRpcHandler::new(make_handle());
    let tracker = handler.workspace.activity_tracker().clone();
    assert_eq!(tracker.snapshot().withhold_reason, None);
    let _ = handler
        .dispatch(
            <FsListReq as WorkspaceRpc>::METHOD,
            serde_json::json!({}),
            None,
        )
        .await;
    assert_eq!(
        tracker.snapshot().withhold_reason,
        None,
        "a read never stamps"
    );
    let _ = handler
        .dispatch(
            <DropSessionReq as WorkspaceRpc>::METHOD,
            serde_json::json!({}),
            None,
        )
        .await;
    assert_eq!(
        tracker.snapshot().withhold_reason,
        None,
        "drop_session mutates but must not hold the sandbox alive"
    );
    let _ = handler
        .dispatch(
            <FsWriteFileReq as WorkspaceRpc>::METHOD,
            serde_json::json!({}),
            None,
        )
        .await;
    assert_eq!(
        tracker.snapshot().withhold_reason,
        Some(IdleWithholdReason::ClientRpc),
        "a mutation stamps"
    );
}
#[tokio::test]
async fn presence_note_stamps_only_visible_notes_for_live_sessions() {
    use echo_agent_tool_protocol::IdleWithholdReason;
    use echo_agent_workspace_types::rpc::presence::PresenceNoteReq;
    let handle = crate::handle::tests::make_handle_with_status_config(crate::StatusConfig {
        presence_keepalive_enabled: true,
        ..crate::StatusConfig::default()
    });
    handle.create_session("sess-1").expect("create session");
    let tracker = handle.activity_tracker().clone();
    let handler = WorkspaceRpcHandler::new(handle);
    let method = <PresenceNoteReq as WorkspaceRpc>::METHOD;
    let err = handler
        .dispatch(
            method,
            serde_json::json!({"session_id": "ghost", "visible": true, "seq": 1}),
            None,
        )
        .await
        .expect_err("unknown session is an error");
    assert!(matches!(err, WorkspaceError::SessionNotFound(_)));
    assert_eq!(
        tracker.snapshot().withhold_reason,
        None,
        "an unknown session must not stamp"
    );
    handler
        .dispatch(
            method,
            serde_json::json!({"session_id": "sess-1", "visible": false, "seq": 2}),
            None,
        )
        .await
        .expect("hidden note is acked");
    assert_eq!(
        tracker.snapshot().withhold_reason,
        None,
        "a hidden note stamps nothing"
    );
    handler
        .dispatch(
            method,
            serde_json::json!({"session_id": "sess-1", "visible": true, "seq": 3}),
            None,
        )
        .await
        .expect("visible note is acked");
    assert_eq!(
        tracker.snapshot().withhold_reason,
        Some(IdleWithholdReason::ClientPresence),
        "a visible note stamps the presence tier"
    );
}
#[tokio::test]
async fn presence_note_is_inert_while_dark() {
    use echo_agent_workspace_types::rpc::presence::PresenceNoteReq;
    let handle = make_handle();
    handle.create_session("sess-1").expect("create session");
    let tracker = handle.activity_tracker().clone();
    let handler = WorkspaceRpcHandler::new(handle);
    handler
        .dispatch(
            <PresenceNoteReq as WorkspaceRpc>::METHOD,
            serde_json::json!({"session_id": "sess-1", "visible": true, "seq": 1}),
            None,
        )
        .await
        .expect("visible note is acked even while dark");
    assert_eq!(
        tracker.snapshot().withhold_reason,
        None,
        "dark default: the note must not withhold idle"
    );
}
