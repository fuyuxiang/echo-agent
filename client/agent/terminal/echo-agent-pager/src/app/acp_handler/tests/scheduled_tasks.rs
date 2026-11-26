#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    #[test]
    fn inject_prompt_happy_path_enqueues_and_drains() {
        let mut app = make_app_with_agent("sess-1");
        let payload = serde_json::json!({
            "sessionId": "sess-1",
            "taskId": "task-42",
            "prompt": "/pr-babysit check",
            "humanSchedule": "every 5m",
        });
        let notif = make_inject_notif(&payload);

        let result = handle_scheduled_task_inject_prompt(&notif, &mut app);
        assert!(result);

        // Agent should now be in TurnRunning (drain happened, prompt was sent).
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.session.state.is_turn_running());
        assert!(agent.session.pending_prompts.is_empty());

        // Scrollback should have a cron prompt block.
        assert!(!agent.scrollback.is_empty());

        // pending_effects should contain a SendPromptBlocks with system-reminder framing,
        // displayText/displayAsCron meta, and a scheduler-fired- prompt_id prefix.
        match &app.pending_effects[0] {
            Effect::SendPromptBlocks {
                blocks, prompt_id, ..
            } => {
                let text = match &blocks[0] {
                    acp::ContentBlock::Text(t) => &t.text,
                    _ => panic!("expected Text block"),
                };
                assert!(
                    text.contains("<system-reminder>"),
                    "missing system-reminder framing"
                );
                assert!(text.contains("task-42"), "missing task_id");
                assert!(text.contains("every 5m"), "missing schedule");
                assert!(text.contains("/pr-babysit check"), "missing prompt");
                assert!(
                    prompt_id.starts_with("scheduler-fired-"),
                    "cron prompt_id must start with 'scheduler-fired-' for data pipeline tagging, got: {prompt_id}"
                );
            }
            other => panic!("expected SendPromptBlocks, got {other:?}"),
        }
    }

    #[test]
    fn inject_prompt_drives_even_when_attached_as_viewer() {
        // The leader routes `echo.agent/scheduled_task_inject_prompt` to the SINGLE
        // session driver, so any client that receives it IS the driver and must
        // enqueue + run it — even one that attached via `session/load`
        // (`attached_as_viewer == true`). Previously this handler latched on
        // `attached_as_viewer` and skipped, which stranded the cron loop with no
        // output whenever the designated driver was an attacher (the sticky-flag
        // bug). Pin the corrected behavior: the inject drives the turn.
        let mut app = make_app_with_agent("sess-1");
        app.agents.get_mut(&AgentId(0)).unwrap().attached_as_viewer = true;

        let payload = serde_json::json!({
            "sessionId": "sess-1",
            "taskId": "task-42",
            "prompt": "echo hello",
            "humanSchedule": "every 1m",
        });
        let result = handle_scheduled_task_inject_prompt(&make_inject_notif(&payload), &mut app);
        assert!(result);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent.session.state.is_turn_running(),
            "the designated driver must start the cron turn even when it attached as a viewer"
        );
        assert!(
            agent.session.pending_prompts.is_empty(),
            "the cron prompt must drain (not linger queued) on the driver"
        );
        assert!(
            matches!(
                app.pending_effects.first(),
                Some(Effect::SendPromptBlocks { .. })
            ),
            "the driver must emit a SendPromptBlocks effect for the cron turn"
        );
    }

    #[test]
    fn inject_prompt_malformed_json_returns_false() {
        let mut app = make_app_with_agent("sess-1");
        let raw = serde_json::value::to_raw_value(&"not a json object").unwrap();
        let notif = acp::ExtNotification::new("echo.agent/scheduled_task_inject_prompt", raw.into());

        // The JSON is valid (a string), but sessionId/prompt fields won't exist.
        let result = handle_scheduled_task_inject_prompt(&notif, &mut app);
        assert!(!result);
    }

    #[test]
    fn inject_prompt_empty_prompt_returns_false() {
        let mut app = make_app_with_agent("sess-1");
        let payload = serde_json::json!({
            "sessionId": "sess-1",
            "prompt": "",
        });
        let notif = make_inject_notif(&payload);

        let result = handle_scheduled_task_inject_prompt(&notif, &mut app);
        assert!(!result);
        // Nothing should be enqueued.
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.session.pending_prompts.is_empty());
    }

    #[test]
    fn inject_prompt_no_matching_agent_returns_false() {
        let mut app = make_app_with_agent("sess-1");
        let payload = serde_json::json!({
            "sessionId": "sess-other",
            "prompt": "do something",
        });
        let notif = make_inject_notif(&payload);

        let result = handle_scheduled_task_inject_prompt(&notif, &mut app);
        assert!(!result);
        // Agent should still be idle, nothing enqueued.
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.session.state.is_idle());
    }

    #[test]
    fn inject_prompt_busy_agent_enqueues_without_draining() {
        let mut app = make_app_with_agent("sess-1");
        // Make the agent busy.
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.session.state = AgentState::TurnRunning;

        let payload = serde_json::json!({
            "sessionId": "sess-1",
            "prompt": "/pr-babysit check",
        });
        let notif = make_inject_notif(&payload);

        let result = handle_scheduled_task_inject_prompt(&notif, &mut app);
        assert!(result);

        // Prompt should be queued but not drained (agent was busy).
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(agent.session.pending_prompts.len(), 1);
        assert_eq!(
            agent.session.pending_prompts[0].kind,
            crate::app::agent::QueueEntryKind::Cron
        );
        // No effects produced (drain was a no-op since agent was busy).
        assert!(app.pending_effects.is_empty());
    }

    #[test]
    fn inject_prompt_skips_duplicate_while_queued() {
        let mut app = make_app_with_agent("sess-1");
        // Busy with an unrelated turn so the cron prompt queues instead of draining.
        app.agents.get_mut(&AgentId(0)).unwrap().session.state = AgentState::TurnRunning;

        let payload = serde_json::json!({
            "sessionId": "sess-1",
            "taskId": "loop-7",
            "prompt": "check deploy",
            "humanSchedule": "every 1m",
        });

        assert!(handle_scheduled_task_inject_prompt(
            &make_inject_notif(&payload),
            &mut app
        ));
        assert_eq!(
            app.agents
                .get(&AgentId(0))
                .unwrap()
                .session
                .pending_prompts
                .len(),
            1
        );

        // A re-fire of the same task while it is still queued must not pile up.
        assert!(handle_scheduled_task_inject_prompt(
            &make_inject_notif(&payload),
            &mut app
        ));
        assert_eq!(
            app.agents
                .get(&AgentId(0))
                .unwrap()
                .session
                .pending_prompts
                .len(),
            1,
            "a re-fire of the same loop must dedupe in the queue, not accumulate"
        );
    }

    #[test]
    fn inject_prompt_skips_duplicate_while_running() {
        let mut app = make_app_with_agent("sess-1");
        let payload = serde_json::json!({
            "sessionId": "sess-1",
            "taskId": "loop-7",
            "prompt": "check deploy",
            "humanSchedule": "every 1m",
        });

        // First fire on an idle agent drains into a running cron turn.
        assert!(handle_scheduled_task_inject_prompt(
            &make_inject_notif(&payload),
            &mut app
        ));
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.session.state.is_turn_running());
        assert!(agent.session.pending_prompts.is_empty());

        // A re-fire while that same loop turn is running must be skipped, not queued.
        assert!(handle_scheduled_task_inject_prompt(
            &make_inject_notif(&payload),
            &mut app
        ));
        assert!(
            app.agents
                .get(&AgentId(0))
                .unwrap()
                .session
                .pending_prompts
                .is_empty(),
            "a re-fire while the loop turn runs must not enqueue a duplicate"
        );
    }

    #[test]
    fn inject_prompt_missing_session_id_returns_false() {
        let mut app = make_app_with_agent("sess-1");
        let payload = serde_json::json!({
            "prompt": "do something",
        });
        let notif = make_inject_notif(&payload);

        let result = handle_scheduled_task_inject_prompt(&notif, &mut app);
        assert!(!result);
    }

    #[test]
    fn fired_known_task_updates_next_fire_at_only() {
        let mut app = make_app_with_agent("sess-1");
        let original_created_at = Instant::now() - std::time::Duration::from_secs(60);
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.session.scheduled_tasks.insert(
                "task-1".into(),
                crate::app::agent::ScheduledTaskInfo {
                    task_id: "task-1".into(),
                    prompt: "original prompt".into(),
                    human_schedule: "every 5 minutes".into(),
                    created_at: original_created_at,
                    next_fire_at: Some("2026-01-01T00:00:00Z".into()),
                    tag: "loop".into(),
                    last_subagent_id: None,
                },
            );
        }

        let notif = make_fired_notif(
            "sess-1",
            "task-1",
            // Different field values to verify they are NOT copied over.
            "DIFFERENT",
            "every 1 hour",
            Some("2026-02-02T02:02:02Z"),
        );
        assert!(handle_scheduled_task_fired(&notif, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let info = agent.session.scheduled_tasks.get("task-1").unwrap();
        assert_eq!(info.prompt, "original prompt", "prompt must not change");
        assert_eq!(
            info.human_schedule, "every 5 minutes",
            "human_schedule must not change"
        );
        assert_eq!(
            info.created_at, original_created_at,
            "created_at must not change"
        );
        assert_eq!(
            info.next_fire_at.as_deref(),
            Some("2026-02-02T02:02:02Z"),
            "next_fire_at must be updated"
        );
        assert_eq!(
            agent.session.scheduled_tasks.len(),
            1,
            "no extra entry should be inserted"
        );
    }

    #[test]
    fn fired_unknown_task_inserts_entry_from_payload() {
        let mut app = make_app_with_agent("sess-1");
        let before = Instant::now();
        let notif = make_fired_notif(
            "sess-1",
            "task-new",
            "scheduled prompt",
            "every 10 minutes",
            Some("2026-03-03T03:03:03Z"),
        );
        assert!(handle_scheduled_task_fired(&notif, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(agent.session.scheduled_tasks.len(), 1);
        let info = agent.session.scheduled_tasks.get("task-new").unwrap();
        assert_eq!(info.task_id, "task-new");
        assert_eq!(info.prompt, "scheduled prompt");
        assert_eq!(info.human_schedule, "every 10 minutes");
        assert_eq!(info.next_fire_at.as_deref(), Some("2026-03-03T03:03:03Z"));
        assert!(
            info.created_at >= before && info.created_at <= Instant::now(),
            "created_at should be set to roughly now"
        );
    }

    #[test]
    fn fired_unknown_task_with_none_next_fire_skips_insert() {
        // Mirrors handle_missed_tasks output: a missed one-shot fires with
        // next_fire_at: None and is immediately removed. The pane should not
        // flicker an entry that the Removed will instantly drop.
        let mut app = make_app_with_agent("sess-1");
        let notif = make_fired_notif(
            "sess-1",
            "missed-1",
            "missed prompt",
            "every 1 minute",
            None,
        );
        assert!(handle_scheduled_task_fired(&notif, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent.session.scheduled_tasks.is_empty(),
            "no entry should be inserted for a Vacant + None fire"
        );
    }

    #[test]
    fn fired_known_task_with_none_next_fire_clears_field() {
        // The Vacant short-circuit on next_fire_at: None must NOT apply to
        // Occupied — clearing an existing countdown is correct behaviour.
        let mut app = make_app_with_agent("sess-1");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.session.scheduled_tasks.insert(
                "task-1".into(),
                crate::app::agent::ScheduledTaskInfo {
                    task_id: "task-1".into(),
                    prompt: "p".into(),
                    human_schedule: "every 1 minute".into(),
                    created_at: Instant::now(),
                    next_fire_at: Some("2026-01-01T00:00:00Z".into()),
                    tag: "loop".into(),
                    last_subagent_id: None,
                },
            );
        }
        let notif = make_fired_notif("sess-1", "task-1", "p", "every 1 minute", None);
        assert!(handle_scheduled_task_fired(&notif, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let info = agent.session.scheduled_tasks.get("task-1").unwrap();
        assert!(info.next_fire_at.is_none());
    }

    #[test]
    fn fired_updates_correct_agent_when_active_view_differs() {
        let mut app = make_app_two_agents();
        // Seed a known task on agent 0.
        {
            let agent0 = app.agents.get_mut(&AgentId(0)).unwrap();
            agent0.session.scheduled_tasks.insert(
                "task-owner".into(),
                crate::app::agent::ScheduledTaskInfo {
                    task_id: "task-owner".into(),
                    prompt: "check PR".into(),
                    human_schedule: "every 5m".into(),
                    created_at: Instant::now(),
                    next_fire_at: Some("2026-01-01T00:00:00Z".into()),
                    tag: "loop".into(),
                    last_subagent_id: None,
                },
            );
        }

        // Fire notification targets agent 0's session, but active_view
        // points to agent 1. Return value is "needs redraw" — false is
        // correct when the mutated agent is not the active view.
        let notif = make_fired_notif(
            "sess-owner",
            "task-owner",
            "check PR",
            "every 5m",
            Some("2026-06-01T12:00:00Z"),
        );
        let needs_redraw = handle_scheduled_task_fired(&notif, &mut app);
        assert!(
            !needs_redraw,
            "non-active agent mutation should not trigger redraw"
        );

        // Agent 0's next_fire_at must be updated.
        let agent0 = app.agents.get(&AgentId(0)).unwrap();
        let info = agent0.session.scheduled_tasks.get("task-owner").unwrap();
        assert_eq!(
            info.next_fire_at.as_deref(),
            Some("2026-06-01T12:00:00Z"),
            "next_fire_at must update on the owning agent, not the active one"
        );

        // Agent 1 must be completely untouched.
        let agent1 = app.agents.get(&AgentId(1)).unwrap();
        assert!(
            agent1.session.scheduled_tasks.is_empty(),
            "non-owning agent must not receive the update"
        );
    }

    #[test]
    fn fired_with_subagent_id_links_chip_and_survives_foreground_fire() {
        let mut app = make_app_with_agent("sess-1");

        let notif = make_fired_notif_with_subagent("sess-1", "task-bg", "sub-abc");
        assert!(handle_scheduled_task_fired(&notif, &mut app));
        {
            let agent = app.agents.get(&AgentId(0)).unwrap();
            let info = agent.session.scheduled_tasks.get("task-bg").unwrap();
            assert_eq!(info.last_subagent_id.as_deref(), Some("sub-abc"));
        }

        let notif = make_fired_notif_with_subagent("sess-1", "task-bg", "sub-def");
        assert!(handle_scheduled_task_fired(&notif, &mut app));
        {
            let agent = app.agents.get(&AgentId(0)).unwrap();
            let info = agent.session.scheduled_tasks.get("task-bg").unwrap();
            assert_eq!(info.last_subagent_id.as_deref(), Some("sub-def"));
        }

        let notif = make_fired_notif(
            "sess-1",
            "task-bg",
            "p",
            "every 1 minute",
            Some("2026-03-03T03:03:03Z"),
        );
        assert!(handle_scheduled_task_fired(&notif, &mut app));
        let agent = app.agents.get(&AgentId(0)).unwrap();
        let info = agent.session.scheduled_tasks.get("task-bg").unwrap();
        assert_eq!(info.last_subagent_id.as_deref(), Some("sub-def"));
    }

    #[test]
    fn created_upserts_existing_chip_preserving_identity_and_linkage() {
        let mut app = make_app_with_agent("sess-1");
        let original_created_at = Instant::now() - std::time::Duration::from_secs(60);
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.session.scheduled_tasks.insert(
                "task-up".into(),
                crate::app::agent::ScheduledTaskInfo {
                    task_id: "task-up".into(),
                    prompt: "old prompt".into(),
                    human_schedule: "every 5 minutes".into(),
                    created_at: original_created_at,
                    next_fire_at: Some("2026-01-01T00:00:00Z".into()),
                    tag: "loop".into(),
                    last_subagent_id: Some("sub-abc".into()),
                },
            );
        }

        let notif = make_created_ext_notif(
            "sess-1",
            "task-up",
            "new prompt",
            "every 10 minutes",
            Some("2026-02-02T02:02:02Z"),
        );
        assert!(handle_scheduled_task_created(&notif, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(agent.session.scheduled_tasks.len(), 1, "no duplicate chip");
        let info = agent.session.scheduled_tasks.get("task-up").unwrap();
        assert_eq!(info.prompt, "new prompt");
        assert_eq!(info.human_schedule, "every 10 minutes");
        assert_eq!(info.next_fire_at.as_deref(), Some("2026-02-02T02:02:02Z"));
        assert_eq!(
            info.created_at, original_created_at,
            "chip identity (countdown anchor) preserved"
        );
        assert_eq!(
            info.last_subagent_id.as_deref(),
            Some("sub-abc"),
            "click-through linkage preserved across an update"
        );
    }

    #[test]
    fn created_updates_correct_agent_when_active_view_differs() {
        let mut app = make_app_two_agents();
        let notif = make_created_ext_notif(
            "sess-owner",
            "task-new",
            "check PR",
            "every 5m",
            Some("2026-06-01T12:00:00Z"),
        );
        let needs_redraw = handle_scheduled_task_created(&notif, &mut app);
        assert!(
            !needs_redraw,
            "non-active agent mutation should not trigger redraw"
        );

        let agent0 = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent0.session.scheduled_tasks.contains_key("task-new"),
            "task must be created on the owning agent"
        );

        let agent1 = app.agents.get(&AgentId(1)).unwrap();
        assert!(
            agent1.session.scheduled_tasks.is_empty(),
            "non-owning agent must not receive the task"
        );
    }

    #[test]
    fn deleted_removes_from_correct_agent_when_active_view_differs() {
        let mut app = make_app_two_agents();
        // Seed task on agent 0.
        {
            let agent0 = app.agents.get_mut(&AgentId(0)).unwrap();
            agent0.session.scheduled_tasks.insert(
                "task-rm".into(),
                crate::app::agent::ScheduledTaskInfo {
                    task_id: "task-rm".into(),
                    prompt: "check PR".into(),
                    human_schedule: "every 5m".into(),
                    created_at: Instant::now(),
                    next_fire_at: Some("2026-01-01T00:00:00Z".into()),
                    tag: "loop".into(),
                    last_subagent_id: None,
                },
            );
        }

        let notif = make_deleted_ext_notif("sess-owner", "task-rm");
        let needs_redraw = handle_scheduled_task_deleted(&notif, &mut app);
        assert!(
            !needs_redraw,
            "non-active agent mutation should not trigger redraw"
        );

        let agent0 = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent0.session.scheduled_tasks.is_empty(),
            "task must be removed from the owning agent"
        );
    }

    fn seed_loop(app: &mut AppView, task_id: &str) {
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .session
            .scheduled_tasks
            .insert(
                task_id.into(),
                crate::app::agent::ScheduledTaskInfo {
                    task_id: task_id.into(),
                    prompt: "babysit the WSD pipeline overnight\nsecond line".into(),
                    human_schedule: "every 30 minutes".into(),
                    created_at: Instant::now(),
                    next_fire_at: Some("2026-01-01T00:00:00Z".into()),
                    tag: "loop".into(),
                    last_subagent_id: None,
                },
            );
    }

    /// An auto-expired task must leave a transcript record: it is the one removal nobody asked
    /// for. The tombstone replays on resume, so this line is also what a returning user sees after
    /// a restart.
    #[test]
    fn deleted_with_expired_reason_pushes_transcript_notice() {
        use echo_agent_tools::notification::ScheduledTaskRemovedReason;
        let mut app = make_app_with_agent("sess-1");
        seed_loop(&mut app, "loop-exp");

        let notif = make_deleted_ext_notif_with_reason(
            "sess-1",
            "loop-exp",
            ScheduledTaskRemovedReason::Expired,
            false,
        );
        assert!(handle_scheduled_task_deleted(&notif, &mut app));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.session.scheduled_tasks.is_empty(), "chip removed");
        assert_eq!(agent.scrollback.len(), 1);
        match &agent.scrollback.get(0).unwrap().block {
            RenderBlock::System(block) => {
                assert!(
                    block.text.contains("Scheduled task expired"),
                    "{}",
                    block.text
                );
                assert!(
                    block.text.contains("babysit the WSD pipeline overnight"),
                    "first prompt line identifies the task: {}",
                    block.text
                );
                assert!(!block.text.contains("second line"), "single-line notice");
                assert!(block.text.contains("every 30 minutes"), "{}", block.text);
            }
            other => panic!("expected System block, got {other:?}"),
        }

        // Re-delivering the same tombstone (reconnect tail replaying an event
        // already applied live) must not stack a second notice: the chip is
        // already gone, so the push is skipped.
        assert!(handle_scheduled_task_deleted(&notif, &mut app));
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(agent.scrollback.len(), 1, "duplicate delivery is a no-op");
    }

    /// A replayed expiry tombstone renders only while the agent accepts replay: inside an open
    /// `session/load` window (`loading_replay`) or the post-load `late_replay_until` grace, where
    /// legitimate replay tails still arrive after `SessionLoaded`. A misrouted replay against a
    /// live transcript (neither open) must remove the chip but never duplicate history.
    #[test]
    fn replayed_expiry_notice_requires_replay_window() {
        use echo_agent_tools::notification::ScheduledTaskRemovedReason;
        #[derive(Debug)]
        enum ReplayPhase {
            None,
            LoadWindow,
            LateGrace,
        }
        for (phase, expected_lines) in [
            (ReplayPhase::None, 0),
            (ReplayPhase::LoadWindow, 1),
            (ReplayPhase::LateGrace, 1),
        ] {
            let mut app = make_app_with_agent("sess-1");
            seed_loop(&mut app, "loop-exp");
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            match phase {
                ReplayPhase::None => {}
                ReplayPhase::LoadWindow => agent.session.loading_replay = true,
                ReplayPhase::LateGrace => agent.arm_late_replay_grace(),
            }

            let notif = make_deleted_ext_notif_with_reason(
                "sess-1",
                "loop-exp",
                ScheduledTaskRemovedReason::Expired,
                true,
            );
            assert!(handle_scheduled_task_deleted(&notif, &mut app));

            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert!(agent.session.scheduled_tasks.is_empty(), "chip removed");
            assert_eq!(agent.scrollback.len(), expected_lines, "phase {phase:?}");
        }
    }

    /// Reconnect after a live expiry: the reload replays `ScheduledTaskCreated` (restoring the
    /// chip) and then the expiry tombstone, which re-stages the notice. The keep-stash finalize
    /// outcome must drop the staged copy instead of appending it below the line the stash
    /// already rendered live before the outage.
    #[test]
    fn reconnect_replay_of_create_then_expiry_does_not_duplicate_notice() {
        use echo_agent_tools::notification::ScheduledTaskRemovedReason;
        let mut app = make_app_with_agent("sess-1");
        seed_loop(&mut app, "loop-exp");

        // Live expiry before the reconnect: notice rendered once.
        let live = make_deleted_ext_notif_with_reason(
            "sess-1",
            "loop-exp",
            ScheduledTaskRemovedReason::Expired,
            false,
        );
        assert!(handle_scheduled_task_deleted(&live, &mut app));

        // Reconnect: stash the transcript and replay the persisted scheduler events.
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .begin_session_reload(1);
        let created = make_created_ext_notif(
            "sess-1",
            "loop-exp",
            "babysit the WSD pipeline overnight",
            "every 30 minutes",
            Some("2026-01-01T00:00:00Z"),
        );
        assert!(handle_scheduled_task_created(&created, &mut app));
        let replayed = make_deleted_ext_notif_with_reason(
            "sess-1",
            "loop-exp",
            ScheduledTaskRemovedReason::Expired,
            true,
        );
        assert!(handle_scheduled_task_deleted(&replayed, &mut app));
        assert!(app
            .agents
            .get_mut(&AgentId(0))
            .unwrap()
            .finish_session_reload(1, true));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.session.scheduled_tasks.is_empty(), "chip removed");
        let notices = (0..agent.scrollback.len())
            .filter(|i| match &agent.scrollback.get(*i).unwrap().block {
                RenderBlock::System(block) => block.text.contains("Scheduled task expired"),
                _ => false,
            })
            .count();
        assert_eq!(notices, 1, "reconnect replay must not duplicate the notice");
    }

    /// Two distinct loops can share a prompt and schedule, producing byte-identical expiry
    /// notices. The reconnect dedupe must drop only as many staged copies as the stash already
    /// shows: one for the task expired live, keeping the second task's notice.
    #[test]
    fn reconnect_dedupe_keeps_notice_for_second_task_with_identical_copy() {
        use echo_agent_tools::notification::ScheduledTaskRemovedReason;
        let mut app = make_app_with_agent("sess-1");
        seed_loop(&mut app, "loop-a");

        // Task A expires live: one notice in the pre-outage transcript.
        let live = make_deleted_ext_notif_with_reason(
            "sess-1",
            "loop-a",
            ScheduledTaskRemovedReason::Expired,
            false,
        );
        assert!(handle_scheduled_task_deleted(&live, &mut app));

        // Reconnect: the replay restores and expires BOTH tasks; B's notice is new information.
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .begin_session_reload(1);
        for task_id in ["loop-a", "loop-b"] {
            let created = make_created_ext_notif(
                "sess-1",
                task_id,
                "babysit the WSD pipeline overnight\nsecond line",
                "every 30 minutes",
                Some("2026-01-01T00:00:00Z"),
            );
            assert!(handle_scheduled_task_created(&created, &mut app));
            let replayed = make_deleted_ext_notif_with_reason(
                "sess-1",
                task_id,
                ScheduledTaskRemovedReason::Expired,
                true,
            );
            assert!(handle_scheduled_task_deleted(&replayed, &mut app));
        }
        assert!(app
            .agents
            .get_mut(&AgentId(0))
            .unwrap()
            .finish_session_reload(1, true));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let notices = (0..agent.scrollback.len())
            .filter(|i| match &agent.scrollback.get(*i).unwrap().block {
                RenderBlock::System(block) => block.text.contains("Scheduled task expired"),
                _ => false,
            })
            .count();
        assert_eq!(
            notices, 2,
            "only the live-rendered copy dedupes; the second task's notice must survive"
        );
    }

    /// Every non-expiry removal is user- or lifecycle-driven and already visible elsewhere; it
    /// must stay silent in the transcript.
    #[test]
    fn deleted_with_other_or_unknown_reasons_is_silent() {
        use echo_agent_tools::notification::ScheduledTaskRemovedReason::*;
        for reason in [Unknown, Completed, Deleted, Shutdown] {
            let mut app = make_app_with_agent("sess-1");
            seed_loop(&mut app, "loop-quiet");

            let notif = make_deleted_ext_notif_with_reason("sess-1", "loop-quiet", reason, false);
            assert!(handle_scheduled_task_deleted(&notif, &mut app));

            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert!(agent.session.scheduled_tasks.is_empty(), "chip removed");
            assert_eq!(
                agent.scrollback.len(),
                0,
                "no transcript line for {reason:?}"
            );
        }
    }

