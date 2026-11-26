#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    /// Regression (resume sync): the on-disk replay stream re-emits persisted
    /// notifications through the generic `echo.agent/session/update` envelope. A
    /// background `monitor`/bash task (`TaskBackgrounded`) must restore into
    /// `bg_tasks` on a resumed / second terminal — not be dropped by the
    /// default match arm — so the idle "watching" status line and the Tasks pane
    /// match the originating terminal. (Before this routing only subagents
    /// survived resume.)
    #[test]
    fn ext_session_update_replay_restores_bg_task() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        assert!(app.agents[&id].session.bg_tasks.is_empty());

        let update = EchoAgentSessionUpdate::TaskBackgrounded {
            tool_call_id: "tc-mon".into(),
            task_id: "mon-1".into(),
            command: "tail -f deploy.log".into(),
            cwd: "/tmp".into(),
            output_file: "/tmp/mon-1.log".into(),
            monitor_description: Some("errors in deploy.log".into()),
            description: None,
        };
        handle(
            make_ext_session_notification_with_method("sess-1", "echo.agent/session/update", update),
            &mut app,
        );

        let task = app.agents[&id]
            .session
            .bg_tasks
            .get("mon-1")
            .expect("replayed TaskBackgrounded must restore bg_tasks on resume");
        assert!(task.is_monitor, "monitor_description must mark is_monitor");
        assert_eq!(task.status, BgTaskStatus::Running);
    }

    /// Companion for scheduled `/loop`s: a replayed `ScheduledTaskCreated` must
    /// restore `scheduled_tasks`, and a later `ScheduledTaskDeleted` must net it
    /// back out — so a resumed terminal's loop count matches instead of staying
    /// empty until the next live fire. (Pairs with the shell-side persistence of
    /// these notifications in `notification_bridge.rs`.)
    #[test]
    fn ext_session_update_replay_restores_then_removes_scheduled_task() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        assert!(app.agents[&id].session.scheduled_tasks.is_empty());

        handle(
            make_ext_session_notification_with_method(
                "sess-1",
                "echo.agent/session/update",
                EchoAgentSessionUpdate::ScheduledTaskCreated {
                    task_id: "loop-1".into(),
                    prompt: "check deploy".into(),
                    human_schedule: "every 5 minutes".into(),
                    next_fire_at: Some("2026-01-01T00:00:00Z".into()),
                },
            ),
            &mut app,
        );
        assert!(
            app.agents[&id]
                .session
                .scheduled_tasks
                .contains_key("loop-1"),
            "replayed ScheduledTaskCreated must restore scheduled_tasks on resume"
        );

        handle(
            make_ext_session_notification_with_method(
                "sess-1",
                "echo.agent/session/update",
                EchoAgentSessionUpdate::ScheduledTaskDeleted {
                    task_id: "loop-1".into(),
                    reason: Default::default(),
                },
            ),
            &mut app,
        );
        assert!(
            app.agents[&id].session.scheduled_tasks.is_empty(),
            "replayed ScheduledTaskDeleted must remove the loop on resume"
        );
    }

    /// Regression: demotion path (foreground Execute → BgTask) must call
    /// finish_running() so the entry is removed from the running set.
    /// Without this, the entry stays orphaned as "running" forever.
    #[test]
    fn task_backgrounded_demotion_clears_running_state() {
        let mut app = make_app_with_agent("sess-1");
        let tc_id = "call-abc-123";

        setup_pending_execute_tool(&mut app, tc_id);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(agent.scrollback.len(), 1);
        assert!(agent.scrollback.needs_animation());
        assert!(agent.session.tracker.pending_tool_entry_id(tc_id).is_some());

        let notif = make_task_backgrounded_notif("sess-1", tc_id, "task-001", "sleep 9999");
        let changed = handle_task_backgrounded(&notif, &mut app);
        assert!(changed);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            !agent.scrollback.needs_animation(),
            "entry must not be in the running set after demotion"
        );
        let entry = agent.scrollback.get(0).unwrap();
        assert!(
            matches!(entry.block, RenderBlock::BgTask(_)),
            "block should be demoted to BgTask"
        );
        assert!(!entry.is_running);
        assert!(agent.session.tracker.pending_tool_entry_id(tc_id).is_none());
    }

    /// Regression: late-detected is_background=true (raw_input arrives after the
    /// Execute block exists) followed by task_backgrounded must correctly demote
    /// the existing Execute block — not create a duplicate BgTask.
    #[test]
    fn task_backgrounded_late_detection_demotes_existing_entry() {
        let mut app = make_app_with_agent("sess-1");
        let tc_id = "call-late-bg-42";

        setup_pending_execute_tool(&mut app, tc_id);
        send_late_bg_detection(&mut app, tc_id);

        // Tool should be in BOTH pending_tools and bg_deferred_tools
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.session.tracker.pending_tool_entry_id(tc_id).is_some());
        assert!(agent.session.tracker.bg_deferred_tools.contains_key(tc_id));

        let notif = make_task_backgrounded_notif("sess-1", tc_id, "task-late-001", "sleep 9999");
        let changed = handle_task_backgrounded(&notif, &mut app);
        assert!(changed);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(agent.scrollback.len(), 1, "must not create duplicate");
        let entry = agent.scrollback.get(0).unwrap();
        assert!(matches!(entry.block, RenderBlock::BgTask(_)));
        assert!(!entry.is_running);
        assert!(!agent.scrollback.needs_animation());
        assert!(agent.session.tracker.pending_tool_entry_id(tc_id).is_none());
        assert!(!agent.session.tracker.bg_deferred_tools.contains_key(tc_id));
    }

    /// Regression: even when the wire notification carries its own
    /// `description`, the deferred-tool suppression key must still be drained
    /// (it is preferred but the entry otherwise leaks and keeps dropping late
    /// stdout updates for the rest of the session).
    #[test]
    fn task_backgrounded_with_wire_description_still_drains_deferred_tool() {
        let mut app = make_app_with_agent("sess-1");
        let tc_id = "call-late-bg-desc";

        setup_pending_execute_tool(&mut app, tc_id);
        send_late_bg_detection(&mut app, tc_id);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.session.tracker.bg_deferred_tools.contains_key(tc_id));

        let notif = SessionNotification {
            session_id: acp::SessionId::new("sess-1"),
            update: EchoAgentSessionUpdate::TaskBackgrounded {
                tool_call_id: tc_id.into(),
                task_id: "task-late-desc".into(),
                command: "sleep 9999".into(),
                cwd: "/tmp".into(),
                output_file: "/tmp/output.log".into(),
                monitor_description: None,
                description: Some("Wait a while".into()),
            },
            meta: None,
        };
        let raw = serde_json::value::to_raw_value(&notif).unwrap();
        let notif = acp::ExtNotification::new("echo.agent/task_backgrounded", raw.into());
        assert!(handle_task_backgrounded(&notif, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            !agent.session.tracker.bg_deferred_tools.contains_key(tc_id),
            "deferred-tool key must be drained even when wire description wins"
        );
    }

    /// Regression: a blank/whitespace wire `description` must not shadow a
    /// non-blank deferred (raw_input) description when there is no Execute
    /// block to recover from (fresh BgTask path). The label must come from the
    /// deferred description, not the blank wire value (which would otherwise
    /// fall back to the raw command).
    #[test]
    fn task_backgrounded_blank_wire_description_prefers_deferred() {
        let mut app = make_app_with_agent("sess-1");
        let tc_id = "call-blank-wire";

        // Simulate late is_background detection having stashed a real
        // description, with the placeholder entry dropped (no pending tool).
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent
                .session
                .tracker
                .bg_deferred_tools
                .insert(tc_id.to_string(), Some("deploy the server".to_string()));
        }
        assert!(
            app.agents
                .get(&AgentId(0))
                .unwrap()
                .session
                .tracker
                .pending_tool_entry_id(tc_id)
                .is_none(),
            "no Execute entry to recover from — exercises the merge chain"
        );

        let notif = SessionNotification {
            session_id: acp::SessionId::new("sess-1"),
            update: EchoAgentSessionUpdate::TaskBackgrounded {
                tool_call_id: tc_id.into(),
                task_id: "task-blank-wire".into(),
                command: "sleep 9999".into(),
                cwd: "/tmp".into(),
                output_file: "/tmp/output.log".into(),
                monitor_description: None,
                description: Some("   ".into()),
            },
            meta: None,
        };
        let raw = serde_json::value::to_raw_value(&notif).unwrap();
        let notif = acp::ExtNotification::new("echo.agent/task_backgrounded", raw.into());
        assert!(handle_task_backgrounded(&notif, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            !agent.session.tracker.bg_deferred_tools.contains_key(tc_id),
            "deferred-tool key drained"
        );
        assert_eq!(agent.scrollback.len(), 1);
        match &agent.scrollback.get(0).unwrap().block {
            RenderBlock::BgTask(bg) => assert_eq!(
                bg.description.as_deref(),
                Some("deploy the server"),
                "blank wire description must not shadow the deferred description"
            ),
            other => panic!("expected BgTask, got {other:?}"),
        }
    }

    #[test]
    fn task_backgrounded_routes_to_child_session() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");
        let notif =
            make_task_backgrounded_notif("child-sess", "tc-child-1", "task-child-1", "sleep 100");
        let changed = handle_task_backgrounded(&notif, &mut app);
        assert!(changed);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        // Parent scrollback must NOT have the bg task block.
        assert_eq!(agent.scrollback.len(), 0, "parent scrollback must be empty");
        assert!(
            agent.session.bg_tasks.is_empty(),
            "parent session must not have the bg task"
        );

        // Child view must have the bg task.
        let child = agent.subagent_views.get("child-sess").unwrap();
        assert_eq!(child.scrollback.len(), 1);
        assert!(child.session.bg_tasks.contains_key("task-child-1"));
        assert!(
            child
                .session
                .bg_tool_call_to_task
                .contains_key("tc-child-1")
        );
    }

    #[test]
    fn task_backgrounded_root_still_routes_to_parent() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");
        let notif =
            make_task_backgrounded_notif("parent-sess", "tc-root-1", "task-root-1", "ls -la");
        let changed = handle_task_backgrounded(&notif, &mut app);
        assert!(changed);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(agent.scrollback.len(), 1);
        assert!(agent.session.bg_tasks.contains_key("task-root-1"));

        let child = agent.subagent_views.get("child-sess").unwrap();
        assert_eq!(
            child.scrollback.len(),
            0,
            "child scrollback must not be affected"
        );
    }

    #[test]
    fn task_backgrounded_monitor_prefix_marks_is_monitor() {
        // Reparented monitor / older backend: the command carries the
        // "[monitor] <desc>" prefix but the notification has no
        // monitor_description. The pager must still mark it a monitor and use
        // the stripped text as the description so it renders as a Monitor row.
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");
        let notif = make_task_backgrounded_notif(
            "parent-sess",
            "tc-mon-1",
            "task-mon-1",
            "[monitor] event counter",
        );
        handle_task_backgrounded(&notif, &mut app);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let task = agent
            .session
            .bg_tasks
            .get("task-mon-1")
            .expect("bg task registered");
        assert!(
            task.is_monitor,
            "a `[monitor] ` command prefix should mark the task as a monitor"
        );
        assert_eq!(task.description.as_deref(), Some("event counter"));
    }

    /// Resume regression: the agent's cold-load reconciliation
    /// completes replay-restored dead tasks with `signal: "session_restart"`.
    /// That synthetic completion must finalize state QUIETLY — finish the
    /// replayed "Task started" entry and mark the task not-running — without
    /// pushing a fresh red "Task failed" block into the resumed scrollback.
    #[test]
    fn session_restart_completion_finalizes_without_failure_block() {
        let mut app = make_app_with_agent("sess-1");

        let replayed =
            make_replayed_task_backgrounded_notif("sess-1", "tc-r", "task-r", "tail -f x.log");
        handle_task_backgrounded(&replayed, &mut app);
        {
            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert_eq!(agent.scrollback.len(), 1, "replay restores started block");
            assert!(agent.scrollback.needs_animation(), "restored row runs");
        }

        let notif = make_task_completed_notif_with_signal(
            "sess-1",
            "task-r",
            "tail -f x.log",
            None,
            Some("session_restart"),
        );
        let changed = handle_task_completed(&notif, &mut app);
        assert!(changed);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent.scrollback.len(),
            1,
            "stale-on-load completion must not push a 'Task failed' block"
        );
        assert!(
            !agent.scrollback.needs_animation(),
            "the replayed started entry must be finished (no running accent)"
        );
        let task = &agent.session.bg_tasks["task-r"];
        assert_eq!(
            task.status,
            BgTaskStatus::Failed,
            "state still records the task as not running"
        );
    }

    /// Guard that the quiet path is NARROW: any other kill signal keeps the
    /// live behavior of pushing a completion/failure block.
    #[test]
    fn non_restart_signal_completion_still_pushes_failure_block() {
        let mut app = make_app_with_agent("sess-1");

        let bg = make_task_backgrounded_notif("sess-1", "tc-k", "task-k", "sleep 999");
        handle_task_backgrounded(&bg, &mut app);

        let notif = make_task_completed_notif_with_signal(
            "sess-1",
            "task-k",
            "sleep 999",
            None,
            Some("SIGKILL"),
        );
        handle_task_completed(&notif, &mut app);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent.scrollback.len(),
            2,
            "a real kill must still render started + failed blocks"
        );
    }

    #[test]
    fn task_completed_routes_to_child_session() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");

        // First, background a task on the child.
        let bg_notif =
            make_task_backgrounded_notif("child-sess", "tc-child-2", "task-child-2", "echo hi");
        handle_task_backgrounded(&bg_notif, &mut app);

        // Now complete it.
        let notif = make_task_completed_notif("child-sess", "task-child-2", "echo hi", Some(0));
        let changed = handle_task_completed(&notif, &mut app);
        assert!(changed);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        // Parent must NOT have a completion block.
        assert_eq!(
            agent.scrollback.len(),
            0,
            "parent scrollback must not have completion block"
        );

        // Child must have both the started and completed blocks.
        let child = agent.subagent_views.get("child-sess").unwrap();
        assert_eq!(child.scrollback.len(), 2, "child: started + completed");
        let bg = child.session.bg_tasks.get("task-child-2").unwrap();
        assert!(matches!(bg.status, BgTaskStatus::Done));
    }

    #[test]
    fn task_completed_root_still_routes_to_parent() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");

        // Background and complete a task on the parent.
        let bg_notif =
            make_task_backgrounded_notif("parent-sess", "tc-root-2", "task-root-2", "echo root");
        handle_task_backgrounded(&bg_notif, &mut app);
        let notif = make_task_completed_notif("parent-sess", "task-root-2", "echo root", Some(0));
        let changed = handle_task_completed(&notif, &mut app);
        assert!(changed);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent.scrollback.len(),
            2,
            "parent: started + completed blocks"
        );
        let bg = agent.session.bg_tasks.get("task-root-2").unwrap();
        assert!(matches!(bg.status, BgTaskStatus::Done));

        let child = agent.subagent_views.get("child-sess").unwrap();
        assert_eq!(
            child.scrollback.len(),
            0,
            "child scrollback must not be affected"
        );
    }

    #[test]
    fn task_completed_failure_routes_to_child_session() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");

        let bg_notif = make_task_backgrounded_notif("child-sess", "tc-fail", "task-fail", "exit 1");
        handle_task_backgrounded(&bg_notif, &mut app);

        let notif = make_task_completed_notif("child-sess", "task-fail", "exit 1", Some(1));
        let changed = handle_task_completed(&notif, &mut app);
        assert!(changed);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent.scrollback.len(),
            0,
            "parent scrollback must not have failure block"
        );

        let child = agent.subagent_views.get("child-sess").unwrap();
        assert_eq!(child.scrollback.len(), 2, "child: started + failed");
        let bg = child.session.bg_tasks.get("task-fail").unwrap();
        assert!(matches!(bg.status, BgTaskStatus::Failed));
        assert_eq!(bg.exit_code, Some(1));
    }

    #[test]
    fn monitor_event_routes_to_child_session() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");

        // Background a task on the child so monitor event has somewhere to land.
        let bg_notif =
            make_task_backgrounded_notif("child-sess", "tc-child-3", "task-child-3", "tail -f");
        handle_task_backgrounded(&bg_notif, &mut app);

        let notif = make_monitor_event_notif("child-sess", "task-child-3", "new log line");
        let changed = handle_monitor_event(&notif, &mut app);
        assert!(changed);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent.session.bg_tasks.is_empty(),
            "parent must not have the bg task"
        );

        let child = agent.subagent_views.get("child-sess").unwrap();
        let task = child.session.bg_tasks.get("task-child-3").unwrap();
        assert_eq!(task.stdout, "new log line");
    }

    #[test]
    fn monitor_event_root_still_routes_to_parent() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");

        // Background a task on the parent.
        let bg_notif =
            make_task_backgrounded_notif("parent-sess", "tc-root-3", "task-root-3", "tail -f");
        handle_task_backgrounded(&bg_notif, &mut app);

        let notif = make_monitor_event_notif("parent-sess", "task-root-3", "root event");
        let changed = handle_monitor_event(&notif, &mut app);
        assert!(changed);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let task = agent.session.bg_tasks.get("task-root-3").unwrap();
        assert_eq!(task.stdout, "root event");

        let child = agent.subagent_views.get("child-sess").unwrap();
        assert!(
            child.session.bg_tasks.is_empty(),
            "child must not have the bg task"
        );
    }

    #[test]
    fn task_backgrounded_child_inactive_returns_false_but_mutates_state() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");
        // Insert a second agent and switch to it so the first agent is inactive.
        let other = make_agent(Some("other-sess"));
        app.agents.insert(AgentId(1), other);
        crate::app::dispatch::switch_to_agent(
            &mut app,
            AgentId(1),
            crate::app::dispatch::SwitchCause::New,
        );
        assert!(matches!(app.active_view, ActiveView::Agent(AgentId(1))));

        let notif =
            make_task_backgrounded_notif("child-sess", "tc-bg-inact", "task-bg-inact", "sleep 1");
        let changed = handle_task_backgrounded(&notif, &mut app);
        // Active view was NOT affected — should return false.
        assert!(!changed);

        // But the bg task state must still land in the child view.
        let agent = app.agents.get(&AgentId(0)).unwrap();
        let child = agent.subagent_views.get("child-sess").unwrap();
        assert!(child.session.bg_tasks.contains_key("task-bg-inact"));
        assert_eq!(child.scrollback.len(), 1);
    }

    #[test]
    fn task_completed_child_inactive_returns_false_but_mutates_state() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");

        // Background a task on the child first.
        let bg_notif = make_task_backgrounded_notif(
            "child-sess",
            "tc-compl-inact",
            "task-compl-inact",
            "echo",
        );
        handle_task_backgrounded(&bg_notif, &mut app);

        // Now switch away.
        let other = make_agent(Some("other-sess"));
        app.agents.insert(AgentId(1), other);
        crate::app::dispatch::switch_to_agent(
            &mut app,
            AgentId(1),
            crate::app::dispatch::SwitchCause::New,
        );

        let notif = make_task_completed_notif("child-sess", "task-compl-inact", "echo", Some(0));
        let changed = handle_task_completed(&notif, &mut app);
        assert!(!changed);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let child = agent.subagent_views.get("child-sess").unwrap();
        let bg = child.session.bg_tasks.get("task-compl-inact").unwrap();
        assert!(matches!(bg.status, BgTaskStatus::Done));
        assert_eq!(child.scrollback.len(), 2);
    }

    #[test]
    fn task_backgrounded_unknown_session_returns_false() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");
        let notif =
            make_task_backgrounded_notif("unknown-sess", "tc-unknown", "task-unknown", "sleep 1");
        let changed = handle_task_backgrounded(&notif, &mut app);
        assert!(!changed);
    }

    #[test]
    fn task_completed_unknown_session_returns_false() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");
        let notif = make_task_completed_notif("unknown-sess", "task-unknown", "echo x", Some(0));
        let changed = handle_task_completed(&notif, &mut app);
        assert!(!changed);
    }

    #[test]
    fn monitor_event_unknown_session_returns_false() {
        let mut app = make_app_with_parent_and_child("parent-sess", "child-sess");
        let notif = make_monitor_event_notif("unknown-sess", "task-unknown", "event");
        let changed = handle_monitor_event(&notif, &mut app);
        assert!(!changed);
    }

    #[test]
    fn replayed_task_backgrounded_marks_restored_from_replay() {
        let mut app = make_app_with_agent("sess-1");

        let replayed =
            make_replayed_task_backgrounded_notif("sess-1", "tc-r", "task-r", "tail -f x.log");
        handle_task_backgrounded(&replayed, &mut app);
        let live = make_task_backgrounded_notif("sess-1", "tc-l", "task-l", "sleep 5");
        handle_task_backgrounded(&live, &mut app);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent.session.bg_tasks["task-r"].restored_from_replay,
            "isReplay-stamped TaskBackgrounded must mark restored_from_replay"
        );
        assert!(
            !agent.session.bg_tasks["task-l"].restored_from_replay,
            "live TaskBackgrounded must not mark restored_from_replay"
        );
    }

    /// Base snapshot for the Completed-before-Backgrounded race tests; tweak
    /// fields per test (the shared helpers hardcode output/description).
    fn race_snapshot(
        task_id: &str,
        command: &str,
        exit_code: Option<i32>,
    ) -> echo_agent_tools::types::TaskSnapshot {
        echo_agent_tools::types::TaskSnapshot {
            task_id: task_id.into(),
            command: command.into(),
            display_command: None,
            cwd: "/tmp".into(),
            start_time: std::time::SystemTime::now(),
            end_time: Some(std::time::SystemTime::now()),
            output: String::new(),
            output_file: "/tmp/out.log".into(),
            truncated: false,
            exit_code,
            signal: None,
            completed: true,
            kind: Default::default(),
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: true,
            output_total_bytes: 0,
        }
    }

    fn completed_notif_from_snapshot(
        session_id: &str,
        task_snapshot: echo_agent_tools::types::TaskSnapshot,
        replayed: bool,
    ) -> acp::ExtNotification {
        let notif = SessionNotification {
            session_id: acp::SessionId::new(session_id),
            update: EchoAgentSessionUpdate::TaskCompleted {
                task_snapshot,
                will_wake: false,
            },
            meta: replayed.then(crate::acp::meta::ReplayMetaStamp::replayed),
        };
        let raw = serde_json::value::to_raw_value(&notif).unwrap();
        acp::ExtNotification::new("echo.agent/task_completed", std::sync::Arc::from(raw))
    }

    /// Short bg shells can exit — and `TaskCompleted` arrive — before their
    /// `TaskBackgrounded`. The late `TaskBackgrounded` must not resurrect the
    /// finished task as Running or push a stray "Task started" block.
    #[test]
    fn completed_before_backgrounded_does_not_resurrect_running() {
        let mut app = make_app_with_agent("sess-1");

        // TaskCompleted first, for a task the pager has never seen.
        let mut snapshot = race_snapshot("task-race", "echo done", Some(0));
        snapshot.output = "task output line".into();
        let done = completed_notif_from_snapshot("sess-1", snapshot, false);
        assert!(handle_task_completed(&done, &mut app));
        {
            let agent = app.agents.get(&AgentId(0)).unwrap();
            let task = agent
                .session
                .bg_tasks
                .get("task-race")
                .expect("unknown TaskCompleted must record terminal state");
            assert_eq!(task.status, BgTaskStatus::Done);
            assert_eq!(task.stdout, "task output line");
            assert_eq!(agent.scrollback.len(), 1, "completed block rendered");
            assert!(
                task.scrollback_entry_id.is_some(),
                "tombstone anchored to the completion block (viewer actions need an entry)"
            );
        }

        // The late TaskBackgrounded (with a wire description) arrives.
        let notif = SessionNotification {
            session_id: acp::SessionId::new("sess-1"),
            update: EchoAgentSessionUpdate::TaskBackgrounded {
                tool_call_id: "tc-race".into(),
                task_id: "task-race".into(),
                command: "echo done".into(),
                cwd: "/tmp".into(),
                output_file: "/tmp/output.log".into(),
                monitor_description: None,
                description: Some("wait for build".into()),
            },
            meta: None,
        };
        let raw = serde_json::value::to_raw_value(&notif).unwrap();
        let late = acp::ExtNotification::new("echo.agent/task_backgrounded", std::sync::Arc::from(raw));
        assert!(handle_task_backgrounded(&late, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let task = &agent.session.bg_tasks["task-race"];
        assert_eq!(
            task.status,
            BgTaskStatus::Done,
            "late TaskBackgrounded must not overwrite a terminal status with Running"
        );
        assert_eq!(task.tool_call_id, "tc-race", "tool_call_id backfilled");
        assert_eq!(
            task.description.as_deref(),
            Some("wait for build"),
            "description backfilled from the late notification"
        );
        assert_eq!(task.stdout, "task output line", "snapshot stdout kept");
        assert_eq!(
            agent.session.bg_tool_call_to_task.get("tc-race"),
            Some(&"task-race".to_string())
        );
        assert_eq!(
            agent.scrollback.len(),
            1,
            "no stray 'Task started' block after the completion"
        );
        assert!(
            !agent.scrollback.needs_animation(),
            "nothing may animate as running for a finished task"
        );
    }

    /// Same race on the demotion path (foreground Execute auto-backgrounded):
    /// the pending Execute block is still demoted to a finished BgTask block
    /// and the terminal status survives.
    #[test]
    fn completed_before_backgrounded_demotion_finishes_execute_block() {
        let mut app = make_app_with_agent("sess-1");
        let tc_id = "call-race-demote";

        setup_pending_execute_tool(&mut app, tc_id);

        let done = make_task_completed_notif("sess-1", "task-demote", "sleep 9999", Some(1));
        assert!(handle_task_completed(&done, &mut app));
        {
            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert_eq!(
                agent.session.bg_tasks["task-demote"].status,
                BgTaskStatus::Failed
            );
            assert_eq!(agent.scrollback.len(), 2, "Execute block + failed block");
        }

        let late = make_task_backgrounded_notif("sess-1", tc_id, "task-demote", "sleep 9999");
        assert!(handle_task_backgrounded(&late, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let task = &agent.session.bg_tasks["task-demote"];
        assert_eq!(
            task.status,
            BgTaskStatus::Failed,
            "terminal status survives the late demotion"
        );
        assert_eq!(agent.scrollback.len(), 2, "no extra block from the demotion");
        let entry = agent.scrollback.get(0).unwrap();
        assert!(
            matches!(entry.block, RenderBlock::BgTask(_)),
            "Execute block demoted to BgTask"
        );
        assert!(
            !agent.scrollback.needs_animation(),
            "the demoted entry must be finished, not animating"
        );
        assert!(
            agent.session.tracker.pending_tool_entry_id(tc_id).is_none(),
            "pending tool drained"
        );
    }

    /// A completion tombstone prefers the snapshot's model-supplied
    /// description, so the race renders the same label as the normal order.
    #[test]
    fn unknown_completed_prefers_snapshot_description() {
        let mut app = make_app_with_agent("sess-1");

        let mut snapshot = race_snapshot("task-desc", "cargo build", Some(0));
        snapshot.description = Some("build the app".into());
        let done = completed_notif_from_snapshot("sess-1", snapshot, false);
        assert!(handle_task_completed(&done, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent.session.bg_tasks["task-desc"].description.as_deref(),
            Some("build the app")
        );
    }

    /// A monitor's completion tombstone keeps the Monitor rendering.
    #[test]
    fn unknown_completed_monitor_kind_marks_is_monitor() {
        let mut app = make_app_with_agent("sess-1");

        let mut snapshot = race_snapshot("task-mon", "tail -f x.log", Some(0));
        snapshot.kind = echo_agent_tools::computer::types::TaskKind::Monitor;
        let done = completed_notif_from_snapshot("sess-1", snapshot, false);
        assert!(handle_task_completed(&done, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.session.bg_tasks["task-mon"].is_monitor);
    }

    /// A tombstone from a replayed completion is historical context: it must
    /// not read as new activity (mirrors restored `TaskBackgrounded`s).
    #[test]
    fn replayed_unknown_completed_marks_tombstone_restored() {
        let mut app = make_app_with_agent("sess-1");

        let snapshot = race_snapshot("task-replay", "echo hi", Some(0));
        let done = completed_notif_from_snapshot("sess-1", snapshot, true);
        assert!(handle_task_completed(&done, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.session.bg_tasks["task-replay"].restored_from_replay);
    }

