//! Unit tests for [`super::AcpTerminalAdapter`]. Extracted from
//! `adapter.rs` so the implementation reads top-to-bottom; wired in
//! via `#[path = "adapter_tests.rs"] mod tests;` in adapter.rs.

use super::*;
use echo_agent_tools::notification::types::ToolNotificationHandle;

fn make_tracked_task(command: &str) -> TrackedTask {
    TrackedTask {
        command: command.to_string(),
        cwd: "/tmp".to_string(),
        output_file: PathBuf::from("/tmp/out.log"),
        ..Default::default()
    }
}

fn out(output: &str, exit_code: Option<i32>, signal: Option<String>) -> SnapshotOutput {
    SnapshotOutput {
        output: output.into(),
        truncated: false,
        exit_code,
        signal,
    }
}

#[test]
fn wrap_command_quotes_shell_metacharacters() {
    let cmd = wrap_command("echo 'hello world' && ls").unwrap();
    #[cfg(unix)]
    {
        let shell = crate::default_shell_path();
        assert!(
            cmd.starts_with(&format!("{shell} -lc")),
            "expected wrapped cmd to begin with `{shell} -lc`, got: {cmd}"
        );
    }
    #[cfg(not(unix))]
    assert_eq!(cmd, "echo 'hello world' && ls");
    assert!(cmd.contains("echo"));
}

#[test]
fn parse_exit_maps_code_signal_and_none() {
    let code = Some(acp::TerminalExitStatus::new().exit_code(Some(42)));
    assert_eq!(parse_exit(&code), (Some(42), None));
    let signal = Some(acp::TerminalExitStatus::new().signal(Some("SIGKILL".into())));
    assert_eq!(parse_exit(&signal), (None, Some("SIGKILL".into())));
    assert_eq!(parse_exit(&None), (None, None));
}

#[test]
fn to_snapshot_derives_completed_and_end_time() {
    let running = make_tracked_task("ls -la").to_snapshot("t-1", out("partial", None, None));
    assert!(!running.completed);
    assert!(running.end_time.is_none());

    // An exit code or a signal marks the snapshot complete and stamps end_time.
    let exited = make_tracked_task("fast").to_snapshot("t-2", out("", Some(1), None));
    assert!(exited.completed);
    assert!(exited.end_time.is_some());
    assert_eq!(exited.exit_code, Some(1));

    let signaled =
        make_tracked_task("killed").to_snapshot("t-3", out("", None, Some("SIGTERM".into())));
    assert!(signaled.completed);
    assert!(signaled.end_time.is_some());
}

/// Scripted client side of the terminal protocol: each `terminal/output`
/// serves the next snapshot; `wait_for_exit` resolves after the last one.
fn scripted_gateway(outputs: Vec<(String, bool)>) -> GatewaySender {
    use echo_agent_acp::AcpClientMessage;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut next = 0usize;
        let mut wait_reply: Option<
            tokio::sync::oneshot::Sender<
                echo_agent_acp::AcpResult<acp::WaitForTerminalExitResponse>,
            >,
        > = None;
        let mut exited = false;
        while let Some(msg) = rx.recv().await {
            match msg {
                AcpClientMessage::CreateTerminal(args) => {
                    let _ = args
                        .response_tx
                        .send(Ok(acp::CreateTerminalResponse::new("term-1")));
                }
                AcpClientMessage::WaitForTerminalExit(args) => {
                    wait_reply = Some(args.response_tx);
                }
                AcpClientMessage::TerminalOutput(args) => {
                    let idx = next.min(outputs.len() - 1);
                    let (text, truncated) = outputs[idx].clone();
                    let mut response = acp::TerminalOutputResponse::new(text, truncated);
                    if exited {
                        response = response
                            .exit_status(Some(acp::TerminalExitStatus::new().exit_code(Some(0))));
                    }
                    next += 1;
                    let _ = args.response_tx.send(Ok(response));
                    if next >= outputs.len()
                        && let Some(reply) = wait_reply.take()
                    {
                        exited = true;
                        let _ = reply.send(Ok(acp::WaitForTerminalExitResponse::new(
                            acp::TerminalExitStatus::new().exit_code(Some(0)),
                        )));
                    }
                }
                AcpClientMessage::ReleaseTerminal(args) => {
                    let _ = args
                        .response_tx
                        .send(Ok(acp::ReleaseTerminalResponse::new()));
                    break;
                }
                AcpClientMessage::KillTerminalCommand(args) => {
                    let _ = args.response_tx.send(Ok(acp::KillTerminalResponse::new()));
                }
                _ => {}
            }
        }
    });
    GatewaySender::new(tx)
}

fn background_request(output_file: PathBuf) -> TerminalRunRequest {
    TerminalRunRequest {
        command: "watch-something".into(),
        working_directory: PathBuf::from("/tmp"),
        env: HashMap::new(),
        timeout: Duration::from_secs(60),
        output_byte_limit: 1024 * 1024,
        output_file,
        notification_handle: ToolNotificationHandle::noop(),
        tool_call_id: "call-1".into(),
        display_command: Some("[monitor] watch".into()),
        auto_background_on_timeout: false,
        foreground_block_budget: None,
        kind: TaskKind::Monitor,
        owner_session_id: Some("owner-1".into()),
        description: None,
    }
}

#[tokio::test(start_paused = true)]
async fn run_background_records_snapshots_and_threads_task_kind() {
    use echo_agent_tools::notification::types::ToolNotification;

    let dir = tempfile::tempdir().unwrap();
    let output_file = dir.path().join("terminal").join("monitor-call-1.log");

    let gateway = scripted_gateway(vec![
        ("line1\n".into(), false),
        ("line1\nline2\n".into(), false),
        ("line1\nline2\nline3\n".into(), false),
    ]);
    let adapter = AcpTerminalAdapter::new(gateway, acp::SessionId::new("sess-1"));

    let (handle, mut notifications) = ToolNotificationHandle::channel();
    let mut request = background_request(output_file.clone());
    request.notification_handle = handle;

    let bg = adapter.run_background(request).await.unwrap();
    assert_eq!(bg.task_id, "term-1");
    assert!(output_file.exists());

    let snapshot = adapter.get_task(&bg.task_id).await.unwrap();
    assert_eq!(snapshot.kind, TaskKind::Monitor);
    assert_eq!(snapshot.owner_session_id.as_deref(), Some("owner-1"));

    let completed = loop {
        match notifications.recv().await.expect("completion notification") {
            ToolNotification::TaskCompleted(snapshot) => break snapshot,
            _ => continue,
        }
    };
    assert_eq!(completed.kind, TaskKind::Monitor);
    assert_eq!(completed.owner_session_id.as_deref(), Some("owner-1"));
    assert_eq!(completed.exit_code, Some(0));

    assert_eq!(
        std::fs::read_to_string(&output_file).unwrap(),
        "line1\nline2\nline3\n"
    );
}

/// A gateway whose `terminal/output` never replies, so live polls fail and
/// `get_task` exercises its offline fallback.
fn output_unavailable_gateway() -> GatewaySender {
    use echo_agent_acp::AcpClientMessage;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let AcpClientMessage::ReleaseTerminal(args) = msg {
                let _ = args
                    .response_tx
                    .send(Ok(acp::ReleaseTerminalResponse::new()));
            }
        }
    });
    GatewaySender::new(tx)
}

fn insert_task(adapter: &AcpTerminalAdapter, task_id: &str, task: TrackedTask) {
    adapter
        .tasks
        .lock()
        .unwrap()
        .insert(task_id.to_string(), task);
}

#[tokio::test]
async fn get_task_completed_keeps_completion_buffer_over_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("done.log");
    tokio::fs::write(&log, "stale mirrored bytes")
        .await
        .unwrap();

    let adapter = AcpTerminalAdapter::new(output_unavailable_gateway(), acp::SessionId::new("s"));
    let mut task = TrackedTask {
        output_file: log,
        ..Default::default()
    };
    task.mark_completed(out("authoritative output", Some(0), None));
    insert_task(&adapter, "t-done", task);

    let snap = adapter.get_task("t-done").await.unwrap();
    assert_eq!(snap.output, "authoritative output");
    assert!(snap.completed);
}

/// A scripted client for the kill/wait paths: `terminal/output` and
/// `terminal/kill` each answer from a fixed script, and every request's
/// method is recorded so tests can assert which round trips happened.
fn kill_wait_gateway(
    output_reply: Option<Option<u32>>,
    kill_ok: bool,
    sent: std::sync::Arc<Mutex<Vec<&'static str>>>,
) -> GatewaySender {
    use echo_agent_acp::AcpClientMessage;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                AcpClientMessage::TerminalOutput(args) => {
                    sent.lock().unwrap().push("output");
                    let reply = match output_reply {
                        None => Err(echo_agent_acp::acp_internal_error("unknown terminal")),
                        Some(exit_code) => {
                            let mut response =
                                acp::TerminalOutputResponse::new(String::new(), false);
                            if let Some(code) = exit_code {
                                response = response.exit_status(Some(
                                    acp::TerminalExitStatus::new().exit_code(Some(code)),
                                ));
                            }
                            Ok(response)
                        }
                    };
                    let _ = args.response_tx.send(reply);
                }
                AcpClientMessage::KillTerminalCommand(args) => {
                    sent.lock().unwrap().push("kill");
                    let reply = if kill_ok {
                        Ok(acp::KillTerminalResponse::new())
                    } else {
                        Err(echo_agent_acp::acp_internal_error("unknown terminal"))
                    };
                    let _ = args.response_tx.send(reply);
                }
                AcpClientMessage::WaitForTerminalExit(_) => {
                    sent.lock().unwrap().push("wait");
                }
                _ => {}
            }
        }
    });
    GatewaySender::new(tx)
}

/// A task id this adapter never started, against a client
/// whose kill is lenient, must answer `NotFound` — not ride the lenient
/// kill into a fabricated "terminated successfully".
#[tokio::test]
async fn kill_task_unknown_id_answers_not_found_despite_lenient_client_kill() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(None, true, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );

    let outcome = adapter.kill_task("never-existed").await;

    assert!(matches!(outcome, KillOutcome::NotFound));
    assert_eq!(*sent.lock().unwrap(), vec!["output"]);
}

/// A probe that dies at the transport level (the client dropped the response
/// channel without answering) proves nothing about the terminal's existence,
/// so the kill must proceed exactly as it did before the probe existed —
/// `NotFound` is reserved for a client that answered and disowned the id.
#[tokio::test]
async fn kill_task_unknown_id_probe_transport_failure_still_kills() {
    use echo_agent_acp::AcpClientMessage;

    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&sent);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                AcpClientMessage::TerminalOutput(args) => {
                    recorded.lock().unwrap().push("output");
                    drop(args.response_tx);
                }
                AcpClientMessage::KillTerminalCommand(args) => {
                    recorded.lock().unwrap().push("kill");
                    let _ = args.response_tx.send(Ok(acp::KillTerminalResponse::new()));
                }
                _ => {}
            }
        }
    });
    let adapter = AcpTerminalAdapter::new(GatewaySender::new(tx), acp::SessionId::new("s"));

    let outcome = adapter.kill_task("probe-disconnected").await;

    assert!(matches!(outcome, KillOutcome::Killed));
    assert_eq!(*sent.lock().unwrap(), vec!["output", "kill"]);
}

#[tokio::test]
async fn kill_task_tracked_running_kills_and_marks_explicitly_killed() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(None), true, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );
    insert_task(&adapter, "t-run", make_tracked_task("sleep 999"));

    let outcome = adapter.kill_task("t-run").await;

    assert!(matches!(outcome, KillOutcome::Killed));
    assert_eq!(*sent.lock().unwrap(), vec!["kill"]);
    let snap = adapter.get_task("t-run").await.expect("tracked task");
    assert!(snap.explicitly_killed);
    assert!(
        snap.kill_result_delivered,
        "bare kill_task is a model-tool kill"
    );
}

#[tokio::test]
async fn kill_task_with_source_client_ui_does_not_mark_delivered() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(None), true, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );
    insert_task(&adapter, "t-ui", make_tracked_task("sleep 999"));

    let outcome = adapter
        .kill_task_with_source("t-ui", KillSource::ClientUi)
        .await;

    assert!(matches!(outcome, KillOutcome::Killed));
    let snap = adapter.get_task("t-ui").await.expect("tracked task");
    assert!(snap.explicitly_killed);
    assert!(
        !snap.kill_result_delivered,
        "UI kill with no waiter must leave kill_result_delivered false"
    );
}

#[tokio::test]
async fn kill_task_with_source_teardown_marks_delivered() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(None), true, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );
    insert_task(&adapter, "t-td", make_tracked_task("sleep 999"));

    let outcome = adapter
        .kill_task_with_source("t-td", KillSource::Teardown)
        .await;

    assert!(matches!(outcome, KillOutcome::Killed));
    let snap = adapter.get_task("t-td").await.expect("tracked task");
    assert!(snap.explicitly_killed);
    assert!(snap.kill_result_delivered);
}

#[tokio::test]
async fn kill_task_with_source_client_ui_live_waiter_marks_delivered() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(None), true, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );
    let mut tracked = make_tracked_task("sleep 999");
    tracked.block_waited = true;
    tracked.live_waiters = 1;
    insert_task(&adapter, "t-wait", tracked);

    let outcome = adapter
        .kill_task_with_source("t-wait", KillSource::ClientUi)
        .await;

    assert!(matches!(outcome, KillOutcome::Killed));
    let snap = adapter.get_task("t-wait").await.expect("tracked task");
    assert!(
        snap.kill_result_delivered,
        "UI kill with a live waiter must mark delivered"
    );
}

#[tokio::test]
async fn kill_task_with_source_client_ui_stale_block_waited_does_not_mark_delivered() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(None), true, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );
    let mut tracked = make_tracked_task("sleep 999");
    tracked.block_waited = true;
    tracked.live_waiters = 0;
    insert_task(&adapter, "t-stale", tracked);

    let outcome = adapter
        .kill_task_with_source("t-stale", KillSource::ClientUi)
        .await;

    assert!(matches!(outcome, KillOutcome::Killed));
    let snap = adapter.get_task("t-stale").await.expect("tracked task");
    assert!(
        !snap.kill_result_delivered,
        "cancelled ACP wait must not count as delivered"
    );
    assert!(!snap.block_waited);
}

#[tokio::test]
async fn wait_for_completion_increments_live_waiters_and_drop_decrements() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(None), true, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );
    insert_task(&adapter, "t-inc", make_tracked_task("sleep 999"));

    {
        let wait = adapter.wait_for_completion("t-inc", Some(Duration::from_secs(30)));
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut wait)
                .await
                .is_err(),
            "wait must stay pending so Drop can be observed"
        );
        assert_eq!(
            adapter.tasks.lock().unwrap()["t-inc"].live_waiters,
            1,
            "wait_for_completion must increment live_waiters"
        );
    }
    assert_eq!(
        adapter.tasks.lock().unwrap()["t-inc"].live_waiters,
        0,
        "dropping the wait must decrement live_waiters"
    );

    let outcome = adapter
        .kill_task_with_source("t-inc", KillSource::ClientUi)
        .await;
    assert!(matches!(outcome, KillOutcome::Killed));
    let snap = adapter.get_task("t-inc").await.expect("tracked task");
    assert!(
        !snap.kill_result_delivered,
        "ClientUi kill after the waiter dropped must not count as delivered"
    );
}

#[tokio::test]
async fn wait_for_completion_live_waiter_makes_client_ui_kill_delivered() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(None), true, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );
    insert_task(&adapter, "t-live", make_tracked_task("sleep 999"));

    let wait = adapter.wait_for_completion("t-live", Some(Duration::from_secs(30)));
    tokio::pin!(wait);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut wait)
            .await
            .is_err(),
        "wait must stay pending"
    );
    assert_eq!(adapter.tasks.lock().unwrap()["t-live"].live_waiters, 1);

    let outcome = adapter
        .kill_task_with_source("t-live", KillSource::ClientUi)
        .await;
    assert!(matches!(outcome, KillOutcome::Killed));
    let snap = adapter.get_task("t-live").await.expect("tracked task");
    assert!(
        snap.kill_result_delivered,
        "ClientUi kill while wait_for_completion is live must mark delivered"
    );
}

/// A waiter dropped while the kill RPC is in flight must not count as
/// delivered: ACP has no oneshot, so we re-sample `live_waiters` after
/// the await (local backend uses `reply.send().is_ok()`).
#[tokio::test]
async fn client_ui_kill_does_not_mark_delivered_if_waiter_drops_during_kill() {
    use echo_agent_acp::AcpClientMessage;

    let (release_kill, hold_kill) = tokio::sync::oneshot::channel::<()>();
    let kill_seen = std::sync::Arc::new(tokio::sync::Notify::new());
    let kill_seen_gateway = Arc::clone(&kill_seen);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut hold_kill = Some(hold_kill);
        while let Some(msg) = rx.recv().await {
            if let AcpClientMessage::KillTerminalCommand(args) = msg {
                kill_seen_gateway.notify_one();
                if let Some(hold) = hold_kill.take() {
                    let _ = hold.await;
                }
                let _ = args.response_tx.send(Ok(acp::KillTerminalResponse::new()));
            }
        }
    });
    let adapter = AcpTerminalAdapter::new(GatewaySender::new(tx), acp::SessionId::new("s"));
    insert_task(&adapter, "t-race", make_tracked_task("sleep 999"));

    let mut wait = Box::pin(adapter.wait_for_completion("t-race", Some(Duration::from_secs(30))));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), wait.as_mut())
            .await
            .is_err(),
        "wait must stay pending so it is live when kill starts"
    );
    assert_eq!(adapter.tasks.lock().unwrap()["t-race"].live_waiters, 1);

    let kill = adapter.kill_task_with_source("t-race", KillSource::ClientUi);
    tokio::pin!(kill);
    tokio::select! {
        biased;
        () = kill_seen.notified() => {}
        _ = &mut kill => panic!("kill finished before the waiter was dropped"),
    }
    drop(wait);
    {
        let task = &adapter.tasks.lock().unwrap()["t-race"];
        assert_eq!(task.live_waiters, 0);
        assert!(
            !task.block_waited,
            "cancelled waiter must clear block_waited before the kill RPC returns, or an exit-watcher TaskCompleted in that gap still suppresses wake"
        );
        assert!(
            !task.kill_result_delivered,
            "ClientUi has not finished the RPC yet"
        );
    }
    let _ = release_kill.send(());
    let outcome = kill.await;
    assert!(matches!(outcome, KillOutcome::Killed));
    let snap = adapter.get_task("t-race").await.expect("tracked task");
    assert!(
        !snap.kill_result_delivered,
        "ClientUi kill must not count a waiter that dropped during the RPC"
    );
    assert!(
        !snap.is_auto_wake_suppressed(),
        "UI kill after a cancelled wait must still wake"
    );
}

/// ModelTool must mark delivered *before* the kill RPC returns, so an
/// exit-watcher TaskCompleted in that window still suppresses auto-wake.
#[tokio::test]
async fn model_tool_kill_marks_delivered_before_kill_rpc_returns() {
    use echo_agent_acp::AcpClientMessage;

    let (release_kill, hold_kill) = tokio::sync::oneshot::channel::<()>();
    let kill_seen = std::sync::Arc::new(tokio::sync::Notify::new());
    let kill_seen_gateway = Arc::clone(&kill_seen);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut hold_kill = Some(hold_kill);
        while let Some(msg) = rx.recv().await {
            if let AcpClientMessage::KillTerminalCommand(args) = msg {
                kill_seen_gateway.notify_one();
                if let Some(hold) = hold_kill.take() {
                    let _ = hold.await;
                }
                let _ = args.response_tx.send(Ok(acp::KillTerminalResponse::new()));
            }
        }
    });
    let adapter = AcpTerminalAdapter::new(GatewaySender::new(tx), acp::SessionId::new("s"));
    insert_task(&adapter, "t-model", make_tracked_task("sleep 999"));

    let kill = adapter.kill_task_with_source("t-model", KillSource::ModelTool);
    tokio::pin!(kill);
    tokio::select! {
        biased;
        () = kill_seen.notified() => {}
        _ = &mut kill => panic!("kill finished before mid-RPC snapshot"),
    }
    {
        let tasks = adapter.tasks.lock().unwrap();
        let task = &tasks["t-model"];
        assert!(task.explicitly_killed);
        assert!(
            task.kill_result_delivered,
            "ModelTool must suppress auto-wake while the kill RPC is in flight"
        );
        let snap = task.to_snapshot("t-model", out("", None, None));
        assert!(snap.is_auto_wake_suppressed());
    }
    let _ = release_kill.send(());
    assert!(matches!(kill.await, KillOutcome::Killed));
}

#[tokio::test]
async fn kill_task_tracked_completed_answers_already_exited_without_round_trips() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(None, false, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );
    let mut task = make_tracked_task("true");
    task.mark_completed(out("", Some(0), None));
    insert_task(&adapter, "t-done", task);

    let outcome = adapter.kill_task("t-done").await;

    assert!(matches!(outcome, KillOutcome::AlreadyExited));
    assert!(sent.lock().unwrap().is_empty());
    assert!(!adapter.tasks.lock().unwrap()["t-done"].explicitly_killed);
}

/// A resumed session rebuilds the adapter with an empty map while the
/// client still holds live terminals: an untracked id whose probe answers
/// without an exit status is genuinely running, and the kill proceeds.
#[tokio::test]
async fn kill_task_untracked_live_terminal_still_kills() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(None), true, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );

    let outcome = adapter.kill_task("resumed-live").await;

    assert!(matches!(outcome, KillOutcome::Killed));
    assert_eq!(*sent.lock().unwrap(), vec!["output", "kill"]);
}

#[tokio::test]
async fn kill_task_untracked_exited_terminal_answers_already_exited() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(Some(0)), true, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );

    let outcome = adapter.kill_task("resumed-exited").await;

    assert!(matches!(outcome, KillOutcome::AlreadyExited));
    assert_eq!(*sent.lock().unwrap(), vec!["output"]);
}

/// A blocking wait on a task the exit watcher already
/// stamped complete answers immediately from the tracked snapshot — it must
/// not send a gateway wait for the released terminal and burn the full
/// requested budget polling it.
#[tokio::test]
async fn wait_for_completion_answers_a_completed_task_immediately() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(None, false, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );
    let mut task = make_tracked_task("true");
    task.mark_completed(out("finished output", Some(0), None));
    insert_task(&adapter, "t-done", task);

    let snapshot = adapter
        .wait_for_completion("t-done", Some(Duration::from_secs(600)))
        .await
        .expect("completed task answers a snapshot");

    assert!(snapshot.completed);
    assert_eq!(snapshot.output, "finished output");
    assert!(snapshot.block_waited);
    assert_eq!(*sent.lock().unwrap(), vec!["output"]);
    assert!(
        !sent.lock().unwrap().contains(&"wait"),
        "already-completed wait must not send WaitForTerminalExit"
    );
}

#[tokio::test]
async fn wait_for_completion_answers_a_killed_task_immediately() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(None, false, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );
    let mut task = make_tracked_task("sleep 999");
    task.explicitly_killed = true;
    task.mark_completed(out("", None, Some("SIGTERM".into())));
    insert_task(&adapter, "t-killed", task);

    let started = std::time::Instant::now();
    let snapshot = adapter
        .wait_for_completion("t-killed", Some(Duration::from_secs(600)))
        .await
        .expect("killed task answers a snapshot");

    assert!(snapshot.completed);
    assert!(snapshot.explicitly_killed);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(*sent.lock().unwrap(), vec!["output"]);
}

/// An id this adapter is not tracking is not-found-as-running: probe once
/// and return. Must not send WaitForTerminalExit and burn a 600s budget.
#[tokio::test]
async fn wait_for_completion_untracked_exited_terminal_returns_immediately() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(Some(0)), false, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );

    let started = std::time::Instant::now();
    let snapshot = adapter
        .wait_for_completion("resumed-exited", Some(Duration::from_secs(600)))
        .await
        .expect("exited untracked terminal");

    assert!(snapshot.completed);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(*sent.lock().unwrap(), vec!["output"]);
}

#[tokio::test]
async fn wait_for_completion_unknown_id_returns_immediately() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(None, false, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );

    let started = std::time::Instant::now();
    let snapshot = adapter
        .wait_for_completion("never-existed", Some(Duration::from_secs(600)))
        .await;

    assert!(snapshot.is_none());
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(*sent.lock().unwrap(), vec!["output"]);
}

#[tokio::test]
async fn wait_for_completion_still_running_stays_pending() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(None), false, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );
    insert_task(&adapter, "t-run", make_tracked_task("sleep 999"));

    let wait = adapter.wait_for_completion("t-run", Some(Duration::from_secs(600)));
    tokio::pin!(wait);
    assert!(
        tokio::time::timeout(Duration::from_millis(80), &mut wait)
            .await
            .is_err(),
        "a still-running tracked task must not take the already-terminal path"
    );
}

#[tokio::test]
async fn wait_for_completion_untracked_live_terminal_stays_pending() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let adapter = AcpTerminalAdapter::new(
        kill_wait_gateway(Some(None), false, Arc::clone(&sent)),
        acp::SessionId::new("s"),
    );

    let wait = adapter.wait_for_completion("resumed-live", Some(Duration::from_secs(2)));
    tokio::pin!(wait);
    assert!(
        tokio::time::timeout(Duration::from_millis(80), &mut wait)
            .await
            .is_err(),
        "an untracked live terminal must send WaitForTerminalExit, not return after one probe"
    );
    assert!(
        sent.lock().unwrap().contains(&"wait"),
        "untracked live wait must send WaitForTerminalExit; sent {:?}",
        sent.lock().unwrap()
    );
}

/// A probe that dies at the transport level (the client dropped the response
/// channel without answering) proves nothing about the terminal's existence.
/// Same as kill: do not treat it as not-found and skip WaitForTerminalExit.
#[tokio::test]
async fn wait_for_completion_untracked_probe_transport_failure_still_waits() {
    use echo_agent_acp::AcpClientMessage;

    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&sent);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                AcpClientMessage::TerminalOutput(args) => {
                    recorded.lock().unwrap().push("output");
                    drop(args.response_tx);
                }
                AcpClientMessage::WaitForTerminalExit(_) => {
                    recorded.lock().unwrap().push("wait");
                }
                _ => {}
            }
        }
    });
    let adapter = AcpTerminalAdapter::new(GatewaySender::new(tx), acp::SessionId::new("s"));

    let wait = adapter.wait_for_completion("probe-disconnected", Some(Duration::from_secs(2)));
    tokio::pin!(wait);
    assert!(
        tokio::time::timeout(Duration::from_millis(80), &mut wait)
            .await
            .is_err(),
        "a transport-failed untracked probe must not abort the wait"
    );
    assert_eq!(*sent.lock().unwrap(), vec!["output", "wait"]);
}

/// The wait can lose a race with the exit watcher: the task completes and
/// its terminal is released between the entry check and the gateway send.
/// A stamped completion is an answer — the fallback must not poll the
/// released terminal until the deadline.
#[tokio::test]
async fn wait_for_completion_race_with_exit_watcher_skips_the_polling_fallback() {
    use echo_agent_acp::AcpClientMessage;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let adapter = AcpTerminalAdapter::new(GatewaySender::new(tx), acp::SessionId::new("s"));
    insert_task(&adapter, "t-race", make_tracked_task("sleep 999"));

    let tasks = Arc::clone(&adapter.tasks);
    let output_polls = std::sync::Arc::new(Mutex::new(0usize));
    let polls = Arc::clone(&output_polls);
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                AcpClientMessage::WaitForTerminalExit(args) => {
                    tasks
                        .lock()
                        .unwrap()
                        .get_mut("t-race")
                        .unwrap()
                        .mark_completed(SnapshotOutput {
                            output: "raced to completion".into(),
                            truncated: false,
                            exit_code: Some(0),
                            signal: None,
                        });
                    let _ = args
                        .response_tx
                        .send(Err(echo_agent_acp::acp_internal_error("terminal released")));
                }
                AcpClientMessage::TerminalOutput(args) => {
                    *polls.lock().unwrap() += 1;
                    let _ = args
                        .response_tx
                        .send(Err(echo_agent_acp::acp_internal_error("unknown terminal")));
                }
                _ => {}
            }
        }
    });

    let snapshot = adapter
        .wait_for_completion("t-race", Some(Duration::from_secs(600)))
        .await
        .expect("raced task answers its stamped snapshot");

    assert!(snapshot.completed);
    assert_eq!(snapshot.output, "raced to completion");
    assert_eq!(*output_polls.lock().unwrap(), 1);
}

#[tokio::test]
async fn get_task_running_fills_output_from_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("run.log");
    tokio::fs::write(&log, "live streamed bytes").await.unwrap();

    let adapter = AcpTerminalAdapter::new(output_unavailable_gateway(), acp::SessionId::new("s"));
    insert_task(
        &adapter,
        "t-run",
        TrackedTask {
            output_file: log,
            output_byte_limit: 1024,
            ..Default::default()
        },
    );

    let snap = adapter.get_task("t-run").await.unwrap();
    assert_eq!(snap.output, "live streamed bytes");
    assert!(!snap.completed);
    assert!(!snap.truncated);
}
