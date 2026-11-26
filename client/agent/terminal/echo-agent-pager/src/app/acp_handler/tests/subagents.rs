#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    #[test]
    fn replayed_subagent_finished_marks_orphan_terminal() {
        let mut app = make_app_with_agent("sess-1");
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .session
            .loading_replay = true;

        let spawned = subagent_ext_replay(
            "sess-1",
            serde_json::json!({
                "sessionUpdate": "subagent_spawned",
                "subagent_id": "sa-1",
                "parent_session_id": "sess-1",
                "child_session_id": "child-1",
                "subagent_type": "general-purpose",
                "description": "orphan review",
            }),
            "sess-1-1",
        );
        handle_ext_notification(&spawned, &mut app);

        let finished = subagent_ext_replay(
            "sess-1",
            serde_json::json!({
                "sessionUpdate": "subagent_finished",
                "subagent_id": "sa-1",
                "child_session_id": "child-1",
                "status": "cancelled",
                "error": "interrupted by process restart",
                "tool_calls": 0,
                "turns": 0,
                "duration_ms": 1000,
                "tokens_used": 0,
            }),
            "sess-1-2",
        );
        handle_ext_notification(&finished, &mut app);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let info = agent
            .subagent_sessions
            .get("child-1")
            .expect("subagent present after replay");
        assert!(
            info.finished,
            "orphan must be terminal after replayed subagent_finished"
        );
        assert_eq!(info.status.as_deref(), Some("cancelled"));
    }

    #[test]
    fn killing_a_stuck_orphan_records_the_shell_reported_status() {
        // The kill call carries the shell's terminal status: "cancelled" is the
        // default when nothing was live, "completed" a real terminal report.
        for status in ["cancelled", "completed"] {
            let mut app = make_app_with_agent("sess-1");
            app.agents
                .get_mut(&AgentId(0))
                .unwrap()
                .session
                .loading_replay = true;

            let spawned = subagent_ext_replay(
                "sess-1",
                serde_json::json!({
                    "sessionUpdate": "subagent_spawned",
                    "subagent_id": "sa-1",
                    "parent_session_id": "sess-1",
                    "child_session_id": "child-1",
                    "subagent_type": "general-purpose",
                    "description": "orphan review",
                }),
                "sess-1-1",
            );
            handle_ext_notification(&spawned, &mut app);

            {
                let agent = app.agents.get_mut(&AgentId(0)).unwrap();
                agent.session.loading_replay = false;
                let info = agent.subagent_sessions.get_mut("child-1").unwrap();
                assert!(!info.finished);
                info.pending_kill = true;
                info.kill_requested_at = Some(std::time::Instant::now());
            }

            let finalized = finalize_killed_subagent(
                &mut app,
                &acp::SessionId::new("sess-1".to_owned()),
                "sa-1",
                status,
            );
            assert!(finalized, "the stuck orphan row must be finalized");

            let agent = app.agents.get(&AgentId(0)).unwrap();
            let info = agent.subagent_sessions.get("child-1").unwrap();
            assert!(info.finished, "kill must finalize the stuck orphan");
            assert_eq!(info.status.as_deref(), Some(status));
            assert!(!info.pending_kill, "pending_kill must clear so it can't revert");
            assert!(info.kill_requested_at.is_none());
        }
    }

    #[test]
    fn killing_an_already_finished_subagent_keeps_its_real_status() {
        let mut app = make_app_with_agent("sess-1");
        let spawn = make_ext_session_notification(
            "sess-1",
            test_subagent_spawned("sess-1", "child-1"),
        );
        assert!(handle(spawn, &mut app));
        let finish = make_ext_session_notification(
            "sess-1",
            EchoAgentSessionUpdate::SubagentFinished {
                subagent_id: "child-1".into(),
                child_session_id: "child-1".into(),
                status: "failed".into(),
                error: Some("real failure".into()),
                tool_calls: 7,
                turns: 3,
                duration_ms: 9_876,
                tokens_used: 543,
                output: None,
                will_wake: false,
            },
        );
        assert!(handle(finish, &mut app));
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .subagent_sessions
            .get_mut("child-1")
            .unwrap()
            .pending_kill = true;

        assert!(finalize_killed_subagent(
            &mut app,
            &acp::SessionId::new("sess-1".to_owned()),
            "child-1",
            "cancelled",
        ));

        let info = &app.agents[&AgentId(0)].subagent_sessions["child-1"];
        assert!(info.finished);
        assert_eq!(
            info.status.as_deref(),
            Some("failed"),
            "retained terminal status must win over the kill-call default"
        );
        assert_eq!(info.error.as_deref(), Some("real failure"));
        assert_eq!(info.tool_calls, Some(7));
        assert_eq!(info.turns, Some(3));
        assert_eq!(info.duration_ms, Some(9_876));
        assert_eq!(info.tokens_used, Some(543));
        assert!(!info.pending_kill);
        let entry_id = info.scrollback_entry_id.unwrap();
        let entry = app.agents[&AgentId(0)].scrollback.get_by_id(entry_id).unwrap();
        let RenderBlock::Subagent(sb) = &entry.block else {
            panic!("expected subagent row");
        };
        assert!(
            matches!(sb.kind, SubagentBlockKind::Failed { .. }),
            "parent row must keep Failed, not repaint as Cancelled/Completed"
        );
    }

    #[test]
    fn refreshing_a_killed_background_child_adds_no_duplicate_row_or_footer() {
        use crate::app::agent_view::test_fixtures::count_turn_markers;

        let mut app = make_app_with_agent("sess-1");
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .session
            .tracker
            .task_tool_background
            .insert("child-bg".into(), true);
        assert!(handle(
            make_ext_session_notification("sess-1", test_subagent_spawned("sess-1", "child-bg")),
            &mut app,
        ));
        assert!(handle(
            make_ext_session_notification("sess-1", test_subagent_finished("child-bg")),
            &mut app,
        ));
        let terminal_rows = |app: &crate::app::app_view::AppView| {
            (0..app.agents[&AgentId(0)].scrollback.len())
                .filter(|&idx| {
                    matches!(
                        app.agents[&AgentId(0)].scrollback.entry(idx).map(|e| &e.block),
                        Some(RenderBlock::Subagent(sb))
                            if sb.child_session_id == "child-bg"
                                && !matches!(sb.kind, SubagentBlockKind::Started)
                    )
                })
                .count()
        };
        let footer_count = |app: &crate::app::app_view::AppView| {
            count_turn_markers(&app.agents[&AgentId(0)].subagent_views["child-bg"])
        };
        assert_eq!(terminal_rows(&app), 1, "first finish appends one terminal row");
        assert_eq!(footer_count(&app), 1, "first finish appends one footer");

        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .subagent_sessions
            .get_mut("child-bg")
            .unwrap()
            .pending_kill = true;
        assert!(finalize_killed_subagent(
            &mut app,
            &acp::SessionId::new("sess-1".to_owned()),
            "child-bg",
            "completed",
        ));

        assert_eq!(terminal_rows(&app), 1, "re-finalize must not add a second row");
        assert_eq!(footer_count(&app), 1, "re-finalize must not add a second footer");
        let info = &app.agents[&AgentId(0)].subagent_sessions["child-bg"];
        assert!(info.finished && info.is_background);
        assert_eq!(info.status.as_deref(), Some("completed"));
    }

    #[test]
    fn multi_turn_child_keeps_second_footer_on_re_finalize() {
        use crate::app::agent_view::test_fixtures::count_turn_markers;
        use crate::app::subagent::finalize_finished_child_view;

        let mut app = make_app_with_agent("sess-1");
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .session
            .tracker
            .task_tool_background
            .insert("child-multi".into(), true);
        assert!(handle(
            make_ext_session_notification(
                "sess-1",
                test_subagent_spawned("sess-1", "child-multi"),
            ),
            &mut app,
        ));
        // Seed an intermediate turn marker, then later content, then finalize.
        {
            let child = app
                .agents
                .get_mut(&AgentId(0))
                .unwrap()
                .subagent_views
                .get_mut("child-multi")
                .unwrap();
            child
                .scrollback
                .push_block(RenderBlock::session_event(SessionEvent::TurnCompleted {
                    elapsed: Some(std::time::Duration::from_secs(1)),
                }));
            child
                .scrollback
                .push_block(RenderBlock::system("turn-2 content"));
            assert_eq!(
                count_turn_markers(child),
                1,
                "precondition: one earlier-turn footer exists"
            );
            finalize_finished_child_view(child, std::time::Duration::from_secs(2));
            assert_eq!(
                count_turn_markers(child),
                2,
                "second-turn finalize must append its own trailing footer"
            );
            // Re-finalize with no new content must stay idempotent on the tail.
            finalize_finished_child_view(child, std::time::Duration::from_secs(3));
            assert_eq!(
                count_turn_markers(child),
                2,
                "re-finalize must not append a third footer"
            );
        }
    }

    #[test]
    fn spawning_many_subagents_starts_no_history_search_threads() {
        const SUBAGENTS: usize = 50;

        let mut app = make_app_with_agent("sess-parent");
        for i in 0..SUBAGENTS {
            let child_sid = format!("child-storm-{i}");
            handle(
                make_ext_session_notification_with_method(
                    "sess-parent",
                    "echo.agent/session/update",
                    test_subagent_spawned("sess-parent", &child_sid),
                ),
                &mut app,
            );
            handle(
                make_ext_session_notification_with_method(
                    "sess-parent",
                    "echo.agent/session/update",
                    test_subagent_finished(&child_sid),
                ),
                &mut app,
            );
        }

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent.subagent_views.len(),
            SUBAGENTS,
            "every spawn must have created a child view (the leak's unit)"
        );
        let daemons = agent
            .subagent_views
            .values()
            .filter(|v| v.prompt.history_search.daemon_built())
            .count();
        assert_eq!(
            daemons, 0,
            "subagent child views must never spawn history-search matcher threads"
        );
    }

    #[test]
    fn a_spawn_then_finish_updates_the_parent_row() {
        let mut app = make_app_with_agent("sess-parent");
        let child_sid = "child-sess-replay";

        let affected = handle(
            make_ext_session_notification_with_method(
                "sess-parent",
                "echo.agent/session/update",
                test_subagent_spawned("sess-parent", child_sid),
            ),
            &mut app,
        );
        assert!(
            affected,
            "SubagentSpawned on the active agent must request a redraw"
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let info = agent
            .subagent_sessions
            .get(child_sid)
            .expect("SubagentSpawned must register subagent_sessions");
        assert_eq!(info.description.as_ref(), "scan src/");
        assert_eq!(info.subagent_type.as_ref(), "explore");
        assert!(
            agent.subagent_views.contains_key(child_sid),
            "SubagentSpawned must create subagent_views eagerly"
        );
        let entry_id = info
            .scrollback_entry_id
            .expect("spawn must stash scrollback_entry_id on SubagentInfo");
        assert_eq!(agent.scrollback.len(), 1);
        let entry = agent.scrollback.get_by_id(entry_id).unwrap();
        let RenderBlock::Subagent(sb) = &entry.block else {
            panic!("SubagentSpawned must push a SubagentBlock to parent scrollback");
        };
        assert_eq!(sb.child_session_id, child_sid);
        assert!(matches!(sb.kind, SubagentBlockKind::Started));
        assert!(agent.scrollback.needs_animation());

        let affected = handle(
            make_ext_session_notification_with_method(
                "sess-parent",
                "echo.agent/session/update",
                test_subagent_finished(child_sid),
            ),
            &mut app,
        );
        assert!(
            affected,
            "SubagentFinished on the active agent must request a redraw"
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let info = agent.subagent_sessions.get(child_sid).unwrap();
        assert!(info.finished);
        assert_eq!(info.status.as_deref(), Some("completed"));
        assert_eq!(info.tool_calls, Some(2));
        assert_eq!(info.turns, Some(1));
        assert_eq!(info.duration_ms, Some(500));
        assert_eq!(info.scrollback_entry_id, Some(entry_id));

        let entry = agent.scrollback.get_by_id(entry_id).unwrap();
        let RenderBlock::Subagent(sb) = &entry.block else {
            panic!("finished subagent must keep the started scrollback entry");
        };
        match &sb.kind {
            SubagentBlockKind::Completed { elapsed } => {
                assert_eq!(*elapsed, std::time::Duration::from_millis(500));
            }
            other => {
                panic!("blocking subagent must mutate started block to Completed, got {other:?}")
            }
        }
        assert!(!entry.is_running, "finish_running must clear running flag");
        assert!(
            !agent.scrollback.needs_animation(),
            "finished subagent entry must not keep scrollback animation"
        );
    }

    #[test]
    fn late_unique_subagent_lifecycle_event_is_not_dropped() {
        let mut app = make_app_with_agent("sess-parent");
        let notification = |update: serde_json::Value, event_seq: u64| {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let payload = serde_json::json!({
                "sessionId": "sess-parent",
                "update": update,
                "_meta": { "eventId": format!("sess-parent-{event_seq}") },
            });
            let raw = serde_json::value::to_raw_value(&payload).unwrap();
            AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
                request: acp::ExtNotification::new("echo.agent/session_notification", raw.into()),
                response_tx: tx,
            })
        };
        let spawned = |child_sid: &str, event_seq: u64| {
            notification(
                serde_json::to_value(test_subagent_spawned("sess-parent", child_sid)).unwrap(),
                event_seq,
            )
        };
        let finished = |child_sid: &str, event_seq: u64| {
            notification(
                serde_json::to_value(test_subagent_finished(child_sid)).unwrap(),
                event_seq,
            )
        };

        for event_seq in 1..=7 {
            assert!(handle(
                spawned(&format!("child-{event_seq}"), event_seq),
                &mut app,
            ));
        }

        // A persisted active goal update can arrive ahead of a lower-ID spawn;
        // it advances the EchoAgent highwater without adding a scrollback block.
        assert!(handle(
            notification(
                serde_json::json!({
                    "sessionUpdate": "goal_updated",
                    "goal_id": "goal-1",
                    "objective": "track lifecycle events",
                    "status": "active",
                    "phase": "executing",
                    "tokens_used": 0,
                    "elapsed_ms": 0,
                    "total_deliverables": 0,
                    "completed_deliverables": 0,
                    "total_worker_rounds": 0,
                    "total_verify_rounds": 0,
                    "token_baseline": 0,
                    "finished_subagent_tokens": 0,
                }),
                100,
            ),
            &mut app,
        ));
        assert_eq!(
            (
                app.agents[&AgentId(0)].last_applied_echo_agent_event_seq,
                app.agents[&AgentId(0)].scrollback.len(),
            ),
            (Some(100), 7)
        );

        let _ = handle(finished("child-1", 50), &mut app);
        assert!(app.agents[&AgentId(0)].subagent_sessions["child-1"].finished);
        assert_eq!(
            app.agents[&AgentId(0)].last_applied_echo_agent_event_seq,
            Some(100),
            "a late lower-ID lifecycle event must not regress the scalar EchoAgent highwater"
        );

        // A restarted producer can reuse an eventId for a different child.
        let _ = handle(spawned("child-reused-id", 1), &mut app);
        assert!(
            app.agents[&AgentId(0)]
                .subagent_sessions
                .contains_key("child-reused-id"),
            "raw eventId reuse must not suppress a new child lifecycle"
        );

        let _ = handle(spawned("child-8", 8), &mut app);
        assert_eq!(
            (
                app.agents[&AgentId(0)].subagent_sessions.len(),
                app.agents[&AgentId(0)].scrollback.len(),
            ),
            (9, 9),
            "a unique late subagent lifecycle event must not be treated as a duplicate"
        );
        assert_eq!(
            app.agents[&AgentId(0)].last_seen_event_id.as_deref(),
            Some("sess-parent-100"),
            "applied late lifecycle must not move the reconnect cursor backwards"
        );

        let _ = handle(finished("child-2", 9), &mut app);
        let _ = handle(spawned("child-8", 8), &mut app);
        let _ = handle(finished("child-2", 9), &mut app);
        assert_eq!(
            (
                app.agents[&AgentId(0)].subagent_sessions.len(),
                app.agents[&AgentId(0)].scrollback.len(),
            ),
            (9, 9),
            "exact spawn and finish redeliveries must remain idempotent"
        );
        assert_eq!(
            app.agents[&AgentId(0)].last_seen_event_id.as_deref(),
            Some("sess-parent-100"),
            "dropped duplicates and late lower-ID applies must not move the reconnect cursor"
        );
    }

    #[test]
    fn finish_before_spawn_is_applied_after_later_cursor_progress() {
        let mut app = make_app_with_agent("sess-parent");
        let notification = |update: EchoAgentSessionUpdate, event_id: &str| {
            let payload = SessionNotification {
                session_id: acp::SessionId::new("sess-parent"),
                update,
                meta: Some(serde_json::json!({ "eventId": event_id })),
            };
            acp::ExtNotification::new(
                "echo.agent/session_notification",
                serde_json::value::to_raw_value(&payload).unwrap().into(),
            )
        };

        assert!(!handle_ext_notification(
            &notification(test_subagent_finished("child-reordered"), "sess-parent-2"),
            &mut app,
        ));
        assert!(app.agents[&AgentId(0)].subagent_sessions.is_empty());
        assert_eq!(app.agents[&AgentId(0)].deferred_subagent_finishes.len(), 1);
        assert_eq!(app.agents[&AgentId(0)].last_seen_event_id, None);

        assert!(handle_ext_notification(
            &notification(
                test_subagent_progress("sess-parent", "unrelated-child"),
                "sess-parent-3",
            ),
            &mut app,
        ));
        assert_eq!(
            app.agents[&AgentId(0)].last_seen_event_id.as_deref(),
            Some("sess-parent-3")
        );

        assert!(handle_ext_notification(
            &notification(
                test_subagent_spawned("sess-parent", "child-reordered"),
                "sess-parent-1",
            ),
            &mut app,
        ));

        let agent = &app.agents[&AgentId(0)];
        let info = &agent.subagent_sessions["child-reordered"];
        assert!(info.finished);
        assert_eq!(info.status.as_deref(), Some("completed"));
        assert!(agent.deferred_subagent_finishes.is_empty());
        assert_eq!(
            agent.last_seen_event_id.as_deref(),
            Some("sess-parent-3"),
            "applying a late lower-ID spawn/finish must keep the higher reconnect cursor"
        );
    }

    #[test]
    fn replaying_a_spawn_rebuilds_a_removed_row_and_keeps_the_finish() {
        let mut app = make_app_with_agent("sess-parent");
        let child_sid = "child-finished-before-rebuild";
        let spawn = subagent_ext_replay(
            "sess-parent",
            serde_json::to_value(test_subagent_spawned("sess-parent", child_sid)).unwrap(),
            "sess-parent-1",
        );
        let finish = subagent_ext_replay(
            "sess-parent",
            serde_json::to_value(test_subagent_finished(child_sid)).unwrap(),
            "sess-parent-2",
        );
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .session
            .loading_replay = true;

        assert!(handle_ext_notification(&spawn, &mut app));
        let first_entry_id = app.agents[&AgentId(0)].subagent_sessions[child_sid]
            .scrollback_entry_id
            .unwrap();
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .scrollback
            .remove_entry(first_entry_id);
        assert!(handle_ext_notification(&finish, &mut app));
        assert!(app.agents[&AgentId(0)].subagent_sessions[child_sid].finished);
        assert!(app.agents[&AgentId(0)].scrollback.is_empty());

        assert!(handle_ext_notification(&spawn, &mut app));

        let agent = &app.agents[&AgentId(0)];
        let info = &agent.subagent_sessions[child_sid];
        assert!(info.finished, "replay rebuild must retain the terminal state");
        assert_eq!(info.status.as_deref(), Some("completed"));
        let rebuilt_entry_id = info
            .scrollback_entry_id
            .expect("replay spawn must rebuild the missing row");
        assert_ne!(rebuilt_entry_id, first_entry_id);
        let rebuilt_entry = agent.scrollback.get_by_id(rebuilt_entry_id).unwrap();
        let RenderBlock::Subagent(block) = &rebuilt_entry.block else {
            panic!("replay spawn must rebuild a subagent row");
        };
        assert!(matches!(block.kind, SubagentBlockKind::Completed { .. }));
        assert!(!rebuilt_entry.is_running);
        assert!(!agent.scrollback.needs_animation());
    }

    #[test]
    fn a_terminal_rebuild_still_accepts_the_replay_updates_that_follow() {
        let mut app = make_app_with_agent("sess-parent");
        let child_sid = "child-late-terminal-rebuild";
        assert!(handle(
            make_ext_session_notification(
                "sess-parent",
                test_subagent_spawned("sess-parent", child_sid),
            ),
            &mut app,
        ));
        assert!(handle(
            make_ext_session_notification("sess-parent", test_subagent_finished(child_sid)),
            &mut app,
        ));
        let entry_id = app.agents[&AgentId(0)].subagent_sessions[child_sid]
            .scrollback_entry_id
            .unwrap();
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.scrollback.remove_entry(entry_id);
        agent.arm_late_replay_grace();

        let replay_spawn = subagent_ext_replay(
            "sess-parent",
            serde_json::to_value(test_subagent_spawned("sess-parent", child_sid)).unwrap(),
            "sess-parent-1",
        );
        assert!(handle_ext_notification(&replay_spawn, &mut app));
        assert!(
            app.agents[&AgentId(0)].late_replay_until.is_some(),
            "the retained finish must keep replay delivery semantics"
        );

        let replay_progress = subagent_ext_replay(
            "sess-parent",
            serde_json::to_value(test_subagent_progress("sess-parent", child_sid)).unwrap(),
            "sess-parent-2",
        );
        assert!(
            handle_ext_notification(&replay_progress, &mut app),
            "the next replay update must still apply during late grace"
        );
        let agent = &app.agents[&AgentId(0)];
        assert_eq!(agent.subagent_sessions[child_sid].turn_count, Some(1));
        assert_eq!(agent.last_seen_event_id.as_deref(), Some("sess-parent-2"));
    }

    #[test]
    fn a_duplicate_live_spawn_keeps_the_existing_child_view() {
        let mut app = make_app_with_agent("sess-parent");
        let spawn = make_ext_session_notification(
            "sess-parent",
            test_subagent_spawned("sess-parent", "child-live-duplicate"),
        );
        assert!(handle(spawn, &mut app));
        let entry_id = app.agents[&AgentId(0)].subagent_sessions["child-live-duplicate"]
            .scrollback_entry_id
            .unwrap();
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .scrollback
            .remove_entry(entry_id);
        let first_view = app.agents[&AgentId(0)].subagent_views["child-live-duplicate"]
            .as_ref() as *const AgentView;

        let duplicate = make_ext_session_notification(
            "sess-parent",
            test_subagent_spawned("sess-parent", "child-live-duplicate"),
        );
        assert!(!handle(duplicate, &mut app));

        let agent = &app.agents[&AgentId(0)];
        assert!(agent.scrollback.is_empty());
        assert_eq!(
            agent.subagent_views["child-live-duplicate"].as_ref() as *const AgentView,
            first_view,
            "live duplicate spawn must not replace the child view"
        );
        assert_eq!(
            agent.subagent_sessions["child-live-duplicate"].scrollback_entry_id,
            Some(entry_id),
            "live duplicate spawn must preserve retained domain state"
        );
    }

    #[test]
    fn a_duplicate_workflow_replay_spawn_keeps_the_existing_child_view() {
        let mut app = make_app_with_agent("sess-parent");
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .session
            .loading_replay = true;
        let spawn = || {
            subagent_ext_replay(
                "sess-parent",
                serde_json::to_value(test_subagent_spawned_for_workflow(
                    "sess-parent",
                    "workflow-child",
                    Some("workflow-run".to_string()),
                ))
                .unwrap(),
                "sess-parent-1",
            )
        };

        assert!(handle_ext_notification(&spawn(), &mut app));
        let first_view = app.agents[&AgentId(0)].subagent_views["workflow-child"]
            .as_ref() as *const AgentView;
        assert!(!handle_ext_notification(&spawn(), &mut app));

        let agent = &app.agents[&AgentId(0)];
        assert!(agent.scrollback.is_empty());
        assert_eq!(
            agent.subagent_views["workflow-child"].as_ref() as *const AgentView,
            first_view,
            "duplicate replay must not replace the workflow child's AgentView"
        );
        assert_eq!(
            agent.last_seen_event_id.as_deref(),
            Some("sess-parent-1"),
            "the duplicate replay must not consume the cursor again"
        );
    }

    #[test]
    fn a_subagents_activity_label_updates_live_and_clears_on_finish() {
        let mut app = make_app_with_agent("sess-parent");
        let child_sid = "child-activity";
        let _ = handle(
            make_ext_session_notification(
                "sess-parent",
                test_subagent_spawned("sess-parent", child_sid),
            ),
            &mut app,
        );

        // A live child message chunk resolves "Responding" and stamps both
        // the block and the info.
        let _ = handle(
            make_agent_chunk_with_event(child_sid, "child text", "p-child", None),
            &mut app,
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        let info = agent.subagent_sessions.get(child_sid).unwrap();
        assert_eq!(info.activity_label.as_deref(), Some("Responding"));
        let entry_id = info.scrollback_entry_id.unwrap();
        let entry = agent.scrollback.get_by_id(entry_id).unwrap();
        let RenderBlock::Subagent(sb) = &entry.block else {
            panic!("expected Subagent block");
        };
        assert_eq!(sb.activity_label, info.activity_label);

        // SubagentProgress recomputes from the child tracker and restamps.
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .subagent_sessions
            .get_mut(child_sid)
            .unwrap()
            .activity_label = None;
        let _ = handle(
            make_ext_session_notification(
                "sess-parent",
                test_subagent_progress("sess-parent", child_sid),
            ),
            &mut app,
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent
                .subagent_sessions
                .get(child_sid)
                .unwrap()
                .activity_label
                .as_deref(),
            Some("Responding")
        );

        let _ = handle(
            make_ext_session_notification("sess-parent", test_subagent_finished(child_sid)),
            &mut app,
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        let info = agent.subagent_sessions.get(child_sid).unwrap();
        assert!(
            info.activity_label.is_none(),
            "finish must clear the info label"
        );
        let entry = agent.scrollback.get_by_id(entry_id).unwrap();
        let RenderBlock::Subagent(sb) = &entry.block else {
            panic!("expected Subagent block");
        };
        assert!(
            sb.activity_label.is_none(),
            "finish must clear the block label"
        );
    }

    #[test]
    fn a_write_tool_call_labels_a_live_child_only() {
        enum Case {
            Live,
            ReloadingTranscript,
            Unregistered,
        }
        for (case, child_sid) in [
            (Case::Live, "child-writing-live"),
            (Case::ReloadingTranscript, "child-writing-reloading"),
            (Case::Unregistered, "child-writing-unregistered"),
        ] {
            let mut app = make_app_with_agent("sess-parent");
            let _ = handle(
                make_ext_session_notification(
                    "sess-parent",
                    test_subagent_spawned("sess-parent", child_sid),
                ),
                &mut app,
            );
            match case {
                Case::Live => {}
                Case::ReloadingTranscript => {
                    app.agents
                        .get_mut(&AgentId(0))
                        .unwrap()
                        .subagent_views
                        .get_mut(child_sid)
                        .unwrap()
                        .session
                        .loading_replay = true;
                }
                Case::Unregistered => {
                    app.agents
                        .get_mut(&AgentId(0))
                        .unwrap()
                        .subagent_sessions
                        .remove(child_sid);
                }
            }

            let changed = handle(
                make_ext_session_notification(
                    child_sid,
                    EchoAgentSessionUpdate::ToolCallDeltaChunk {
                        tool_call_id: Some("call_1".into()),
                        tool_index: 0,
                        name: Some("write".into()),
                        arguments_delta: None,
                    },
                ),
                &mut app,
            );

            let agent = app.agents.get(&AgentId(0)).unwrap();
            match case {
                Case::Live => {
                    assert!(changed, "a live child's write delta must redraw");
                    assert_eq!(
                        agent.subagent_sessions[child_sid].activity_label.as_deref(),
                        Some("Writing file…")
                    );
                }
                Case::ReloadingTranscript => {
                    assert!(!changed, "a delta must be ignored while the child reloads its transcript");
                    assert!(agent.subagent_sessions[child_sid].activity_label.is_none());
                    assert_eq!(
                        agent.subagent_views[child_sid].session.tracker.activity(),
                        None,
                        "the reloading tracker must not pick up the delta"
                    );
                }
                Case::Unregistered => {
                    assert!(!changed, "a delta for an unregistered child must not redraw");
                }
            }
        }
    }

    #[test]
    fn a_finished_subagents_label_is_not_restamped_by_late_events() {
        enum LateEvent {
            AcpChunk,
            ToolCallDelta,
        }
        for (event, child_sid) in [
            (LateEvent::AcpChunk, "child-late-acp"),
            (LateEvent::ToolCallDelta, "child-late-delta"),
        ] {
            let mut app = make_app_with_agent("sess-parent");
            let _ = handle(
                make_ext_session_notification(
                    "sess-parent",
                    test_subagent_spawned("sess-parent", child_sid),
                ),
                &mut app,
            );
            let _ = handle(
                make_ext_session_notification("sess-parent", test_subagent_finished(child_sid)),
                &mut app,
            );

            let changed = match event {
                LateEvent::AcpChunk => {
                    // Simulate the racing child rail still looking live.
                    app.agents
                        .get_mut(&AgentId(0))
                        .unwrap()
                        .subagent_views
                        .get_mut(child_sid)
                        .unwrap()
                        .session
                        .state = AgentState::TurnRunning;
                    handle(
                        make_agent_chunk_with_event(child_sid, "late text", "p-child", None),
                        &mut app,
                    )
                }
                LateEvent::ToolCallDelta => handle(
                    make_ext_session_notification(
                        child_sid,
                        EchoAgentSessionUpdate::ToolCallDeltaChunk {
                            tool_call_id: Some("call_1".into()),
                            tool_index: 0,
                            name: Some("write".into()),
                            arguments_delta: None,
                        },
                    ),
                    &mut app,
                ),
            };
            if matches!(event, LateEvent::ToolCallDelta) {
                assert!(!changed, "a delta after finish must not redraw");
            }

            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert!(
                agent.subagent_sessions[child_sid].activity_label.is_none(),
                "a finished subagent's label must stay cleared after a late event"
            );
        }
    }

    #[test]
    fn a_replay_tagged_spawn_outside_a_session_load_is_dropped() {
        with_replay_disk_home(|_| {
            let child_sid = "child-unexpected-replay";
            let mut app = make_app_with_agent("sess-parent");
            assert!(!app.agents[&AgentId(0)].session.loading_replay);
            write_child_updates_jsonl(
                replay_disk_test_home(),
                child_sid,
                &(child_tool_line(child_sid) + "\n"),
            );
            let spawned = subagent_ext_replay(
                "sess-parent",
                serde_json::json!({
                    "sessionUpdate": "subagent_spawned",
                    "subagent_id": child_sid,
                    "parent_session_id": "sess-parent",
                    "child_session_id": child_sid,
                    "subagent_type": "explore",
                    "description": "scan src/",
                }),
                "sess-parent-1",
            );
            handle_ext_notification(&spawned, &mut app);
            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert!(
                agent.subagent_sessions.is_empty(),
                "unexpected replay spawn must not register"
            );
            assert!(agent.subagent_views.is_empty());
        });
    }

    #[test]
    fn a_stray_replay_is_accepted_briefly_after_a_load_then_a_live_update_stops_it() {
        let replay = || crate::acp::meta::NotificationMeta {
            is_replay: true,
            ..crate::acp::meta::NotificationMeta::default()
        };
        let mut app = make_app_with_agent("sess-late");
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        assert!(!agent.session.loading_replay);
        agent.arm_late_replay_grace();

        assert!(
            !drop_unexpected_replay(agent, &replay(), "sess-late", "test"),
            "a stray replay must still apply during the post-load grace"
        );

        let live = crate::acp::meta::NotificationMeta::default();
        assert!(!drop_unexpected_replay(agent, &live, "sess-late", "test"));
        assert!(
            drop_unexpected_replay(agent, &replay(), "sess-late", "test"),
            "a live update ends the grace so later replays are dropped"
        );
    }

    #[test]
    fn subagent_spawned_during_resume_defers_child_replay_until_open() {
        with_replay_disk_home(|_| {
            let child_sid = "child-resume-defer";
            let mut app = make_app_with_agent("sess-parent");
            // Simulate resume: the parent agent is replaying its own session.
            app.agents
                .get_mut(&AgentId(0))
                .unwrap()
                .session
                .loading_replay = true;

            spawn_subagent_with_optional_updates(
                &mut app,
                child_sid,
                Some(&(child_tool_line(child_sid) + "\n")),
            );

            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                0,
                "resume spawn must NOT eagerly replay the child transcript"
            );
            assert!(
                agent
                    .subagent_sessions
                    .get(child_sid)
                    .is_some_and(|i| i.transcript.needs_replay()),
                "resume spawn must leave the transcript NeedsReplay for the first open"
            );

            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.open_subagent_fullscreen(child_sid.to_string());
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                1,
                "opening the subagent after resume must replay its transcript"
            );
            assert!(
                agent
                    .subagent_sessions
                    .get(child_sid)
                    .is_some_and(|i| !i.transcript.needs_replay()),
                "the first open must record the disk copy"
            );
        });
    }

    #[test]
    fn live_spawn_burst_defers_child_replay_until_open() {
        // The burst size that froze the TUI in the field.
        const BURST_CHILDREN: usize = 25;

        with_replay_disk_home(|_| {
            let mut app = make_app_with_agent("sess-parent");
            let child_sids: Vec<String> = (0..BURST_CHILDREN)
                .map(|i| format!("child-burst-{i}"))
                .collect();
            let reads_before = crate::app::subagent::test_support::transcript_reads();
            for sid in &child_sids {
                spawn_subagent_with_optional_updates(
                    &mut app,
                    sid,
                    Some(&(child_tool_line(sid) + "\n")),
                );
            }
            assert_eq!(
                crate::app::subagent::test_support::transcript_reads(),
                reads_before,
                "a spawn burst must not open a single child transcript"
            );

            let agent = app.agents.get(&AgentId(0)).unwrap();
            for sid in &child_sids {
                assert_eq!(
                    child_scrollback_tool_call_count(agent, sid),
                    0,
                    "a live spawn must not replay the on-disk transcript"
                );
                assert!(
                    agent
                        .subagent_sessions
                        .get(sid.as_str())
                        .is_some_and(|i| i.transcript.needs_replay()),
                    "a live spawn must leave the transcript NeedsReplay for the first open"
                );
            }

            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.open_subagent_fullscreen(child_sids[0].clone());
            assert_eq!(
                crate::app::subagent::test_support::transcript_reads(),
                reads_before + 1,
                "the first open must read exactly the opened child's transcript"
            );
            assert_eq!(child_scrollback_tool_call_count(agent, &child_sids[0]), 1);
            assert_eq!(
                child_scrollback_tool_call_count(agent, &child_sids[1]),
                0,
                "opening one child must not read its siblings"
            );
        });
    }

    #[test]
    fn resumed_child_keeps_inherited_history_when_a_live_block_arrives_first() {
        with_replay_disk_home(|home| {
            let child_sid = "child-resume-live-first";
            write_child_updates_jsonl(home, child_sid, &(child_tool_line(child_sid) + "\n"));

            let mut app = make_app_with_agent("sess-parent");
            let mut spawned = test_subagent_spawned("sess-parent", child_sid);
            let EchoAgentSessionUpdate::SubagentSpawned { resumed_from, .. } = &mut spawned else {
                unreachable!();
            };
            *resumed_from = Some("orig-child".into());
            let _ = handle(
                make_ext_session_notification_with_method(
                    "sess-parent",
                    "echo.agent/session/update",
                    spawned,
                ),
                &mut app,
            );
            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                0,
                "a resumed spawn must defer the read like any other"
            );

            // A background child streams its first live block before it is opened.
            app.agents
                .get_mut(&AgentId(0))
                .unwrap()
                .subagent_sessions
                .get_mut(child_sid)
                .unwrap()
                .is_background = true;
            let _ = handle(make_agent_chunk_message(child_sid, "working on it"), &mut app);

            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                1,
                "the inherited tool call must be read before the live block lands"
            );

            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.open_subagent_fullscreen(child_sid.to_string());
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                1,
                "opening the child must show the inherited history exactly once"
            );
        });
    }

    #[test]
    fn subagent_resume_finished_then_open_shows_full_transcript() {
        with_replay_disk_home(|_| {
            let child_sid = "child-resume-finished";
            let mut app = make_app_with_agent("sess-parent");
            app.agents
                .get_mut(&AgentId(0))
                .unwrap()
                .session
                .loading_replay = true;

            spawn_subagent_with_optional_updates(
                &mut app,
                child_sid,
                Some(&(child_tool_line(child_sid) + "\n")),
            );
            let _ = handle(
                make_ext_session_notification_with_method(
                    "sess-parent",
                    "echo.agent/session/update",
                    test_subagent_finished(child_sid),
                ),
                &mut app,
            );

            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                0,
                "resume must not eagerly load the finished subagent transcript"
            );
            assert!(
                agent
                    .subagent_sessions
                    .get(child_sid)
                    .is_some_and(|i| i.transcript.needs_replay()),
                "finished-during-resume must leave the transcript NeedsReplay"
            );
            assert!(
                matches!(
                    agent.subagent_views.get(child_sid).unwrap().session.state,
                    AgentState::Idle
                ),
                "finished subagent must be Idle after resume, not TurnRunning"
            );

            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.open_subagent_fullscreen(child_sid.to_string());
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                1,
                "opening a finished subagent after resume must show its transcript"
            );
            let child = agent.subagent_views.get(child_sid).unwrap();
            assert!(
                (0..child.scrollback.len()).any(|i| child
                    .scrollback
                    .entry(i)
                    .is_some_and(|e| matches!(e.block, RenderBlock::SessionEvent(_)))),
                "opened finished subagent must show a TurnCompleted footer"
            );
        });
    }

    #[test]
    fn an_open_resumed_child_that_finishes_without_streaming_hydrates_in_place() {
        with_replay_disk_home(|home| {
            let child_sid = "child-open-resume-finish";

            // A resumed child spawns live; its inherited transcript has not
            // flushed to disk yet.
            let mut app = make_app_with_agent("sess-parent");
            let mut spawned = test_subagent_spawned("sess-parent", child_sid);
            let EchoAgentSessionUpdate::SubagentSpawned { resumed_from, .. } = &mut spawned else {
                unreachable!();
            };
            *resumed_from = Some("orig-child".into());
            let _ = handle(
                make_ext_session_notification_with_method(
                    "sess-parent",
                    "echo.agent/session/update",
                    spawned,
                ),
                &mut app,
            );

            // Open it fullscreen before any transcript exists: the read finds
            // nothing, so the view stays prompt-only.
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.open_subagent_fullscreen(child_sid.to_string());
            assert_eq!(child_scrollback_tool_call_count(agent, child_sid), 0);

            // The inherited transcript flushes, then the child finishes while
            // still open, having streamed no live block.
            write_child_updates_jsonl(home, child_sid, &(child_tool_line(child_sid) + "\n"));
            let _ = handle(
                make_ext_session_notification_with_method(
                    "sess-parent",
                    "echo.agent/session/update",
                    test_subagent_finished(child_sid),
                ),
                &mut app,
            );

            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                1,
                "finishing must hydrate the open resumed child in place, not just stamp a footer"
            );
        });
    }

    /// Each scenario owns one parent plus a child driven through the real
    /// notification handler, and the child's on-disk transcript under the
    /// shared replay test home.
    mod eviction_and_rebuild {
        use super::*;
        use crate::app::subagent::ChildTranscript;

        struct Scenario {
            app: AppView,
            child_sid: &'static str,
        }

        impl Drop for Scenario {
            fn drop(&mut self) {
                crate::app::subagent::set_replay_echo_agent_home_for_tests(None);
            }
        }

        impl Scenario {
            /// Live-spawn `child_sid`, first writing `updates` to the child's
            /// `updates.jsonl` (`None` = nothing persisted).
            fn spawn(child_sid: &'static str, updates: Option<String>) -> Self {
                let home = replay_disk_test_home();
                crate::app::subagent::set_replay_echo_agent_home_for_tests(Some(home.to_path_buf()));
                let mut app = make_app_with_agent("sess-parent");
                spawn_subagent_with_optional_updates(&mut app, child_sid, updates.as_deref());
                Scenario { app, child_sid }
            }

            fn spawn_sibling(&mut self, child_sid: &str, updates: Option<String>) {
                spawn_subagent_with_optional_updates(&mut self.app, child_sid, updates.as_deref());
            }

            fn agent(&self) -> &AgentView {
                self.app.agents.get(&AgentId(0)).unwrap()
            }

            fn agent_mut(&mut self) -> &mut AgentView {
                self.app.agents.get_mut(&AgentId(0)).unwrap()
            }

            fn transcript(&self) -> ChildTranscript {
                self.agent().subagent_sessions[self.child_sid].transcript
            }

            fn set_transcript(&mut self, state: ChildTranscript) {
                let sid = self.child_sid;
                self.agent_mut()
                    .subagent_sessions
                    .get_mut(sid)
                    .unwrap()
                    .transcript = state;
            }

            fn set_background(&mut self) {
                let sid = self.child_sid;
                self.agent_mut()
                    .subagent_sessions
                    .get_mut(sid)
                    .unwrap()
                    .is_background = true;
            }

            /// Deliver the child's `SubagentFinished` through the real handler.
            fn finish(&mut self) {
                let _ = handle(
                    make_ext_session_notification_with_method(
                        "sess-parent",
                        "echo.agent/session/update",
                        test_subagent_finished(self.child_sid),
                    ),
                    &mut self.app,
                );
            }

            fn open(&mut self) {
                self.open_child(self.child_sid);
            }

            fn open_child(&mut self, child_sid: &str) {
                let sid = child_sid.to_string();
                self.agent_mut().open_subagent_fullscreen(sid);
            }

            fn close(&mut self) {
                self.agent_mut().close_subagent_fullscreen();
            }

            fn push_child_block(&mut self, block: RenderBlock) {
                let sid = self.child_sid;
                self.agent_mut()
                    .subagent_views
                    .get_mut(sid)
                    .unwrap()
                    .scrollback
                    .push_block(block);
            }

            fn tool_calls(&self) -> usize {
                self.tool_calls_for(self.child_sid)
            }

            fn tool_calls_for(&self, child_sid: &str) -> usize {
                child_scrollback_tool_call_count(self.agent(), child_sid)
            }

            fn session_events(&self) -> usize {
                child_scrollback_session_event_count(self.agent(), self.child_sid)
            }

            fn prompts_matching(&self, prompt: &str) -> usize {
                child_scrollback_matching_prompt_count(self.agent(), self.child_sid, prompt)
            }

            fn has_system_block(&self) -> bool {
                let child = self.agent().subagent_views.get(self.child_sid).unwrap();
                (0..child.scrollback.len()).any(|i| {
                    matches!(
                        child.scrollback.entry(i).map(|e| &e.block),
                        Some(RenderBlock::System(_))
                    )
                })
            }

            fn compaction_markers(&self) -> usize {
                let child = self.agent().subagent_views.get(self.child_sid).unwrap();
                (0..child.scrollback.len())
                    .filter(|i| {
                        matches!(
                            child.scrollback.entry(*i).map(|e| &e.block),
                            Some(RenderBlock::SessionEvent(b)) if matches!(
                                b.event,
                                SessionEvent::CompactionStarted { .. }
                                    | SessionEvent::CompactionCompleted { .. }
                            )
                        )
                    })
                    .count()
            }

            fn updates_path(&self) -> std::path::PathBuf {
                replay_disk_test_home()
                    .join("sessions")
                    .join(urlencoding::encode("/tmp").as_ref())
                    .join(self.child_sid)
                    .join("updates.jsonl")
            }

            /// Replace `updates.jsonl` with a directory so a rebuild read
            /// fails (`Err`, not the missing-file `Empty`).
            fn break_transcript(&self) {
                let path = self.updates_path();
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
            }

            fn replace_transcript(&self, content: &str) {
                let path = self.updates_path();
                if path.is_dir() {
                    std::fs::remove_dir(&path).unwrap();
                }
                std::fs::write(&path, content).unwrap();
            }

            /// Remove the child session dir so a rebuild resolves `Empty`.
            fn remove_session_dir(&self) {
                std::fs::remove_dir_all(self.updates_path().parent().unwrap()).unwrap();
            }
        }

        fn child_compaction_started_line(child_sid: &str) -> String {
            format!(
                r#"{{"method":"_echo.agent/session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"auto_compact_started","tokens_used":9000,"context_window":10000,"percentage":90,"reason":"threshold"}}}}}}"#
            )
        }

        fn child_compaction_completed_line(child_sid: &str) -> String {
            format!(
                r#"{{"method":"_echo.agent/session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"auto_compact_completed","tokens_after":100,"elapsed_ms":5}}}}}}"#
            )
        }

        #[test]
        fn a_finished_foreground_child_is_evicted_then_rebuilds_on_open() {
            enum Trigger {
                FinishWhileClosed,
                CloseWhileOpen,
                SwitchToSibling,
            }
            for (trigger, child_sid) in [
                (Trigger::FinishWhileClosed, "child-evict-finish"),
                (Trigger::CloseWhileOpen, "child-evict-close"),
                (Trigger::SwitchToSibling, "child-evict-switch"),
            ] {
                let mut s = Scenario::spawn(child_sid, Some(child_tool_line(child_sid) + "\n"));
                match trigger {
                    Trigger::FinishWhileClosed => {
                        s.open();
                        s.close();
                        s.finish();
                        let child = s.agent().subagent_views.get(child_sid).unwrap();
                        assert_eq!(child.scrollback.len(), 0, "finish must evict the transcript");
                        assert!(matches!(child.session.state, AgentState::Idle));
                    }
                    Trigger::CloseWhileOpen => {
                        s.open();
                        s.finish();
                        assert_eq!(s.tool_calls(), 1, "an open child is guarded at finish");
                        s.close();
                    }
                    Trigger::SwitchToSibling => {
                        let sibling = "child-evict-sibling";
                        s.spawn_sibling(sibling, Some(child_tool_line(sibling) + "\n"));
                        s.open();
                        s.finish();
                        s.open_child(sibling);
                    }
                }
                assert_eq!(s.tool_calls(), 0, "the finished child must be evicted");
                assert_eq!(
                    s.transcript(),
                    ChildTranscript::NeedsReplay,
                    "eviction must schedule the deferred on-open replay again"
                );

                s.open();
                assert_eq!(s.tool_calls(), 1, "opening the evicted child must rebuild it");
                assert_eq!(
                    s.session_events(),
                    1,
                    "the rebuilt transcript ends with the TurnCompleted footer"
                );
            }
        }

        #[test]
        fn an_evict_guard_keeps_the_transcript_in_place() {
            enum Guard {
                OpenAtFinish,
                RunningOnClose,
            }
            for (guard, child_sid, footer) in [
                (Guard::OpenAtFinish, "child-guard-finish", 1),
                (Guard::RunningOnClose, "child-guard-close", 0),
            ] {
                let mut s = Scenario::spawn(child_sid, Some(child_tool_line(child_sid) + "\n"));
                s.open();
                match guard {
                    Guard::OpenAtFinish => s.finish(),
                    Guard::RunningOnClose => s.close(),
                }
                assert_eq!(s.tool_calls(), 1, "the guarded transcript must survive");
                assert_eq!(s.session_events(), footer);
            }
        }

        #[test]
        fn an_on_disk_prompt_echo_dedups_against_the_injected_prompt_on_open() {
            // Same echo-dedup on open whether the view is freshly spawned or was
            // first evicted back to the task-prompt baseline.
            enum Entry {
                FreshSpawn,
                Evicted,
            }
            for (entry, child_sid) in [
                (Entry::FreshSpawn, "child-echo-fresh"),
                (Entry::Evicted, "child-echo-evicted"),
            ] {
                let task = "scan src/ for auth";
                write_subagent_meta_json(replay_disk_test_home(), "sess-parent", child_sid, task);
                let updates = format!(
                    "{}\n{}",
                    child_user_message_line(child_sid, task),
                    child_tool_line(child_sid)
                );
                let mut s = Scenario::spawn(child_sid, Some(updates));

                // Spawn injects the task prompt once and reads no transcript.
                assert_eq!(s.prompts_matching(task), 1, "spawn injects the task prompt once");
                assert_eq!(s.tool_calls(), 0, "spawn does not replay the transcript");

                if matches!(entry, Entry::Evicted) {
                    s.finish();
                    assert_eq!(s.prompts_matching(task), 1, "eviction keeps the task prompt");
                    assert_eq!(s.tool_calls(), 0);
                }

                s.open();
                assert_eq!(
                    s.prompts_matching(task),
                    1,
                    "the replayed on-disk echo must dedup against the injected prompt"
                );
                assert_eq!(s.tool_calls(), 1);
            }
        }

        #[test]
        fn late_block_after_evict_still_rebuilds_on_open() {
            let child_sid = "child-late-event";
            let mut s = Scenario::spawn(child_sid, Some(child_tool_line(child_sid) + "\n"));
            s.finish();
            // Late block sneaks into the evicted (prompt-less) scrollback.
            s.push_child_block(RenderBlock::system("late notice".to_string()));

            s.open();
            assert_eq!(
                s.tool_calls(),
                1,
                "a late block must not stop the on-open rebuild"
            );
        }

        #[test]
        fn a_nonemitting_rebuild_of_an_evicted_view_retries_until_disk_lands() {
            // An evicted view holds nothing to restore, so a non-emitting read
            // stays NeedsReplay and retries once real content lands. A read error
            // applies no footer; an Empty read (flush not landed yet) still stamps
            // the finished footer on the bare view.
            enum Outcome {
                ReadError,
                Empty,
            }
            for (outcome, child_sid, footer_before_flush) in [
                (Outcome::ReadError, "child-evict-io-retry", 0),
                (Outcome::Empty, "child-evict-flush-race", 1),
            ] {
                let mut s = Scenario::spawn(child_sid, Some(child_tool_line(child_sid) + "\n"));
                s.finish();

                match outcome {
                    Outcome::ReadError => s.break_transcript(),
                    Outcome::Empty => s.replace_transcript(""),
                }
                s.open();
                assert_eq!(s.tool_calls(), 0, "a non-emitting rebuild reads nothing");
                assert_eq!(
                    s.transcript(),
                    ChildTranscript::NeedsReplay,
                    "a non-emitting rebuild of an evicted view stays NeedsReplay"
                );
                assert_eq!(s.session_events(), footer_before_flush);

                s.replace_transcript(&(child_tool_line(child_sid) + "\n"));
                s.open();
                assert_eq!(s.tool_calls(), 1, "the retry rebuilds once disk content lands");
                assert_eq!(s.session_events(), 1, "the rebuilt view ends with one footer");
            }
        }

        #[test]
        fn partial_flush_rebuild_self_heals_on_next_open_cycle() {
            let child_sid = "child-partial-flush";
            // Only a prefix of the final transcript is on disk at finish time.
            let mut s = Scenario::spawn(child_sid, Some(child_tool_line(child_sid) + "\n"));
            s.finish();

            s.open();
            assert_eq!(s.tool_calls(), 1, "the first open rebuilds the prefix");
            assert_eq!(s.transcript(), ChildTranscript::DiskBacked);
            s.close();
            assert_eq!(
                s.transcript(),
                ChildTranscript::NeedsReplay,
                "closing a disk-backed view must schedule the replay again"
            );

            // The rest of the flush lands after the partial rebuild.
            let second_line = child_tool_line(child_sid)
                .replace(r#""toolCallId":"tc1""#, r#""toolCallId":"tc2""#);
            s.replace_transcript(&format!(
                "{}\n{}\n",
                child_tool_line(child_sid),
                second_line
            ));
            s.open();
            assert_eq!(
                s.tool_calls(),
                2,
                "the open after the late append must replay the completed file"
            );
        }

        #[test]
        fn finished_background_child_with_content_never_rebuilds() {
            let child_sid = "child-bg-no-rebuild";
            let mut s = Scenario::spawn(child_sid, Some(child_tool_line(child_sid) + "\n"));
            s.set_background();
            // Populated view whose transcript was never read back from disk.
            s.open();
            assert_eq!(s.tool_calls(), 1);
            s.close();
            s.set_transcript(ChildTranscript::NeedsReplay);

            s.finish();
            assert_eq!(s.tool_calls(), 1);

            s.open();
            assert_eq!(
                s.tool_calls(),
                1,
                "opening a populated background child must not append the disk transcript"
            );
        }

        #[test]
        fn a_nonemitting_rebuild_restores_the_populated_view() {
            // Content the replay never read back is restored when the rebuild
            // reads nothing: a read error stays retriable, an Empty read pins
            // the only copy MemoryOnly.
            enum Outcome {
                ReadError,
                Empty,
            }
            for (outcome, child_sid) in [
                (Outcome::ReadError, "child-restore-err"),
                (Outcome::Empty, "child-restore-empty"),
            ] {
                let mut s = Scenario::spawn(child_sid, Some(child_tool_line(child_sid) + "\n"));
                s.open();
                s.set_transcript(ChildTranscript::NeedsReplay);
                s.finish();
                assert_eq!(s.tool_calls(), 1);
                assert_eq!(s.session_events(), 1);

                match outcome {
                    Outcome::ReadError => s.break_transcript(),
                    Outcome::Empty => s.remove_session_dir(),
                }
                s.open();
                assert_eq!(
                    s.tool_calls(),
                    1,
                    "a non-emitting rebuild restores the populated view"
                );
                assert_eq!(s.session_events(), 1, "the restored view keeps its single footer");

                match outcome {
                    Outcome::ReadError => {
                        s.replace_transcript(&(child_tool_line(child_sid) + "\n"));
                        s.open();
                        assert_eq!(s.tool_calls(), 1, "the repaired disk rebuilds cleanly");
                        assert_eq!(
                            s.transcript(),
                            ChildTranscript::DiskBacked,
                            "a read error is retriable and records the disk copy once repaired"
                        );
                    }
                    Outcome::Empty => {
                        assert_eq!(
                            s.transcript(),
                            ChildTranscript::MemoryOnly,
                            "nothing on disk pins the restored copy as the only one"
                        );
                        s.close();
                        assert_eq!(s.tool_calls(), 1, "close must not drop the memory-only copy");
                        s.open();
                        assert_eq!(s.tool_calls(), 1);
                    }
                }
            }
        }

        #[test]
        fn finish_keeps_the_only_copy_when_disk_cannot_rebuild_it() {
            // Two ways disk cannot rebuild: nothing persisted, or a non-emitting file.
            for (child_sid, updates) in [
                ("child-no-disk-copy", None),
                (
                    "child-non-emitting-disk",
                    Some(child_compaction_started_line("child-non-emitting-disk") + "\n"),
                ),
            ] {
                let mut s = Scenario::spawn(child_sid, updates);
                // Live-streamed content that exists only in memory.
                s.push_child_block(RenderBlock::system("streamed output".to_string()));
                assert_eq!(s.transcript(), ChildTranscript::NeedsReplay);

                s.finish();
                assert_eq!(
                    s.transcript(),
                    ChildTranscript::NeedsReplay,
                    "a probe that proves nothing must keep retrying"
                );
                assert!(s.has_system_block(), "the only copy must survive the finish");

                s.open();
                assert_eq!(
                    s.transcript(),
                    ChildTranscript::MemoryOnly,
                    "the open's Empty rebuild records the restored copy as memory-only"
                );
                assert!(s.has_system_block());
                s.close();
                assert!(
                    s.has_system_block(),
                    "close must never drop the memory-only copy"
                );
            }
        }

        #[test]
        fn rebuild_after_evict_preserves_child_compaction_markers() {
            let child_sid = "child-echo-agent-marker";
            let updates = format!(
                "{}\n{}\n{}\n",
                child_tool_line(child_sid),
                child_compaction_started_line(child_sid),
                child_compaction_completed_line(child_sid),
            );
            let mut s = Scenario::spawn(child_sid, Some(updates));
            // Read the deferred transcript, then leave the child running.
            s.open();
            assert!(
                s.compaction_markers() >= 1,
                "the on-open replay must render the compaction marker"
            );
            assert_eq!(s.tool_calls(), 1);
            s.close();

            s.finish();
            assert_eq!(s.tool_calls(), 0, "finish must evict the transcript");

            s.open();
            assert_eq!(s.tool_calls(), 1);
            assert_eq!(
                s.compaction_markers(),
                2,
                "the rebuilt transcript must keep both compaction markers"
            );
        }
    }

    #[test]
    fn subagent_spawn_injects_meta_prompt_by_content_without_reading_disk() {
        // (meta.json task prompt, whether spawn injects it and arms echo dedup)
        let cases: &[(Option<&str>, bool)] = &[
            (Some("explore handlers only"), true),
            (Some("   "), false),
            (None, false),
        ];
        for (idx, (meta, injects)) in cases.iter().enumerate() {
            let child_sid = format!("child-inject-{idx}");
            with_replay_disk_home(|home| {
                let parent_sid = "sess-parent";
                if let Some(meta) = meta {
                    write_subagent_meta_json(home, parent_sid, &child_sid, meta);
                }
                let mut app = make_app_with_agent(parent_sid);
                spawn_subagent_with_optional_updates(&mut app, &child_sid, None);

                let agent = app.agents.get(&AgentId(0)).unwrap();
                let injected = usize::from(*injects);
                assert_eq!(
                    child_scrollback_matching_prompt_count(agent, &child_sid, meta.unwrap_or("")),
                    injected,
                    "prompt injection for {meta:?}"
                );
                assert_eq!(
                    agent.subagent_views.get(&child_sid).unwrap().scrollback.len(),
                    injected,
                    "scrollback holds only the injected prompt, if any, for {meta:?}"
                );
                assert_eq!(child_scrollback_tool_call_count(agent, &child_sid), 0);
                assert_eq!(
                    child_tracker_expects_user_echo(agent, &child_sid),
                    *injects,
                    "echo dedup for {meta:?}"
                );
                assert!(
                    agent
                        .subagent_sessions
                        .get(&child_sid)
                        .is_some_and(|i| i.transcript.needs_replay()),
                    "nothing read on spawn: the transcript stays NeedsReplay for {meta:?}"
                );
            });
        }
    }

    #[test]
    fn subagent_spawn_and_open_replay_is_idempotent() {
        with_replay_disk_home(|_| {
            let child_sid = "child-idempotent";
            let mut app = make_app_with_agent("sess-parent");
            spawn_subagent_with_optional_updates(
                &mut app,
                child_sid,
                Some(&(child_tool_line(child_sid) + "\n")),
            );

            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                0,
                "spawn must defer the replay to first open"
            );
            agent.open_subagent_fullscreen(child_sid.to_string());
            assert_eq!(child_scrollback_tool_call_count(agent, child_sid), 1);
            agent.open_subagent_fullscreen(child_sid.to_string());
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                1,
                "re-opening must not duplicate the replay once the disk copy is recorded"
            );
        });
    }

    #[test]
    fn subagent_spawn_live_foreign_cwd_is_never_read() {
        with_replay_disk_home(|home| {
            let child_sid = "child-foreign-cwd";
            write_child_updates_jsonl_under_cwd(
                home,
                "/other/cwd",
                child_sid,
                &(child_tool_line(child_sid) + "\n"),
            );

            let mut app = make_app_with_agent("sess-parent");
            spawn_subagent_with_optional_updates(&mut app, child_sid, None);

            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                0,
                "live spawn must not scan a foreign-cwd transcript"
            );
            assert!(
                agent
                    .subagent_sessions
                    .get(child_sid)
                    .is_some_and(|i| i.transcript.needs_replay()),
                "an Empty hinted miss stays NeedsReplay"
            );

            // The retry on open stays hinted-only for a live child, so the
            // foreign-cwd transcript is still not read.
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.open_subagent_fullscreen(child_sid.to_string());
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                0,
                "opening a live child must not scan a foreign-cwd transcript"
            );
        });
    }

    #[test]
    fn subagent_open_resumed_child_reads_foreign_cwd_transcript() {
        with_replay_disk_home(|home| {
            let child_sid = "child-resume-foreign";
            write_child_updates_jsonl_under_cwd(
                home,
                "/other/cwd",
                child_sid,
                &(child_tool_line(child_sid) + "\n"),
            );

            let mut app = make_app_with_agent("sess-parent");
            let mut spawned = test_subagent_spawned("sess-parent", child_sid);
            let EchoAgentSessionUpdate::SubagentSpawned { resumed_from, .. } = &mut spawned else {
                unreachable!();
            };
            *resumed_from = Some("orig-child".into());
            let _ = handle(
                make_ext_session_notification_with_method(
                    "sess-parent",
                    "echo.agent/session/update",
                    spawned,
                ),
                &mut app,
            );

            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                0,
                "a resumed spawn must defer the replay like any other"
            );
            assert!(
                agent
                    .subagent_sessions
                    .get(child_sid)
                    .is_some_and(|i| i.transcript.needs_replay()),
                "a resumed spawn must leave the transcript NeedsReplay"
            );

            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.open_subagent_fullscreen(child_sid.to_string());
            assert_eq!(
                child_scrollback_tool_call_count(agent, child_sid),
                1,
                "opening a resumed child must scan for the relocated transcript"
            );
        });
    }

    #[test]
    fn both_session_transports_produce_the_same_subagent_row() {
        let child_sid = "child-equiv";
        let (spawn_notif, finish_notif) =
            run_subagent_lifecycle_via_method("echo.agent/session_notification", child_sid);
        let (spawn_update, finish_update) =
            run_subagent_lifecycle_via_method("echo.agent/session/update", child_sid);

        assert_eq!(spawn_notif.description, spawn_update.description);
        assert_eq!(spawn_notif.subagent_type, spawn_update.subagent_type);
        assert_eq!(spawn_notif.has_child_view, spawn_update.has_child_view);
        assert_eq!(spawn_notif.scrollback_len, spawn_update.scrollback_len);
        assert_eq!(spawn_notif.child_session_id, child_sid);
        assert_eq!(spawn_update.child_session_id, child_sid);
        assert!(matches!(spawn_notif.block_kind, SubagentBlockKind::Started));
        assert!(matches!(
            spawn_update.block_kind,
            SubagentBlockKind::Started
        ));
        assert_eq!(
            spawn_notif.scrollback_entry_id,
            spawn_update.scrollback_entry_id
        );
        assert!(spawn_notif.scrollback_entry_id.is_some());

        assert!(finish_notif.finished);
        assert!(finish_update.finished);
        assert_eq!(finish_notif.status.as_deref(), Some("completed"));
        assert_eq!(finish_update.status.as_deref(), Some("completed"));
        assert_eq!(finish_notif.tool_calls, Some(2));
        assert_eq!(finish_update.tool_calls, Some(2));
        assert_eq!(finish_notif.turns, Some(1));
        assert_eq!(finish_update.turns, Some(1));
        assert_eq!(finish_notif.duration_ms, Some(500));
        assert_eq!(finish_update.duration_ms, Some(500));
        assert!(matches!(
            finish_notif.block_kind,
            SubagentBlockKind::Completed { .. }
        ));
        assert!(matches!(
            finish_update.block_kind,
            SubagentBlockKind::Completed { .. }
        ));
    }

    #[test]
    fn spawning_on_a_background_agent_registers_it_without_redrawing() {
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        let child_sid = "child-inactive";
        let affected = handle(
            make_ext_session_notification_with_method(
                "sess-A",
                "echo.agent/session/update",
                test_subagent_spawned("sess-A", child_sid),
            ),
            &mut app,
        );

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        let info = agent_a
            .subagent_sessions
            .get(child_sid)
            .expect("SubagentSpawned must register on inactive agent A");
        assert!(
            agent_a.subagent_views.contains_key(child_sid),
            "SubagentSpawned must create subagent_views on inactive agent A"
        );
        assert_eq!(agent_a.scrollback.len(), 1);
        let entry_id = info
            .scrollback_entry_id
            .expect("inactive spawn must stash scrollback_entry_id");
        let entry = agent_a.scrollback.get_by_id(entry_id).unwrap();
        let RenderBlock::Subagent(sb) = &entry.block else {
            panic!("inactive spawn must push SubagentBlock");
        };
        assert!(matches!(sb.kind, SubagentBlockKind::Started));
        assert!(
            !affected,
            "SubagentSpawned on inactive agent must not request a redraw"
        );

        let affected = handle(
            make_ext_session_notification_with_method(
                "sess-A",
                "echo.agent/session/update",
                test_subagent_finished(child_sid),
            ),
            &mut app,
        );
        assert!(
            !affected,
            "SubagentFinished on inactive agent must not request a redraw"
        );

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        let info = agent_a.subagent_sessions.get(child_sid).unwrap();
        assert!(info.finished);
        assert_eq!(info.status.as_deref(), Some("completed"));
        let entry = agent_a.scrollback.get_by_id(entry_id).unwrap();
        let RenderBlock::Subagent(sb) = &entry.block else {
            panic!("inactive finish must keep SubagentBlock");
        };
        assert!(matches!(sb.kind, SubagentBlockKind::Completed { .. }));
    }

    #[test]
    fn a_spawn_for_an_unknown_session_is_ignored() {
        let mut app = make_app_with_agent("sess-A");
        let affected = handle(
            make_ext_session_notification_with_method(
                "sess-unknown",
                "echo.agent/session/update",
                test_subagent_spawned("sess-unknown", "child-unknown"),
            ),
            &mut app,
        );

        assert!(!affected, "unknown session_id must not request a redraw");
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent.subagent_sessions.is_empty(),
            "SubagentSpawned for unknown session must not register subagent_sessions"
        );
        assert!(
            agent.scrollback.is_empty(),
            "SubagentSpawned for unknown session must not push scrollback"
        );
    }

    #[test]
    fn a_malformed_session_notification_is_ignored() {
        let mut app = make_app_with_agent("sess-A");
        let (tx, _rx) = tokio::sync::oneshot::channel();
        // Valid JSON but not a SessionNotification: parse must fail quietly.
        let raw =
            serde_json::value::to_raw_value(&serde_json::json!({"unexpected": true})).unwrap();
        let request = acp::ExtNotification::new("echo.agent/session/update", raw.into());
        let msg = AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
            request,
            response_tx: tx,
        });

        let affected = handle(msg, &mut app);

        assert!(
            !affected,
            "malformed echo.agent/session/update params must not redraw"
        );
        assert!(
            app.agents.get(&AgentId(0)).unwrap().scrollback.is_empty(),
            "malformed notification must not mutate scrollback"
        );
    }

    #[test]
    fn a_notification_reaches_its_target_agent_even_when_another_is_active() {
        // AutoCompactCompleted on the EchoAgent ext path resets the context bar
        // numerator via refresh_context_used. That side effect must run on
        // the matched agent regardless of which view is currently active.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        // Seed A with a stale context-used reading so we can prove the
        // notification reset it.
        {
            let agent_a = app.agents.get_mut(&AgentId(0)).unwrap();
            agent_a.apply_context_used(90_000, 131_072);
        }
        switch_active_to(&mut app, AgentId(1));

        let affected = handle(
            make_ext_session_notification(
                "sess-A",
                EchoAgentSessionUpdate::AutoCompactCompleted {
                    tokens_before: Some(90_000),
                    tokens_after: 25_000,
                    elapsed_ms: Some(300),
                    summary_preview: None,
                },
            ),
            &mut app,
        );

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent_a.context_state.as_ref().map(|c| c.used),
            Some(25_000),
            "AutoCompactCompleted must reset A's context_used even when B is active"
        );
        assert!(
            !affected,
            "ext notification routed to a non-active agent must not request a redraw"
        );
    }

