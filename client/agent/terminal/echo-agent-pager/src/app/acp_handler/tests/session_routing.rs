#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    #[test]
    fn acp_chunk_for_inactive_agent_lands_in_its_scrollback() {
        // Regression: switching away from a streaming agent must not
        // discard chunks bound for that agent. Before this fix, only
        // `TaskResult::PromptResponse` survived, so the user saw a bare
        // "Worked for X.Xs" with no body text.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        let affected = handle(make_agent_chunk_message("sess-A", "hello from A"), &mut app);

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent_message_text(agent_a),
            "hello from A",
            "chunk for inactive agent A must land in A's scrollback"
        );
        assert!(
            !affected,
            "chunk routed to a non-active agent must not request a redraw"
        );
        let agent_b = app.agents.get(&AgentId(1)).unwrap();
        assert!(
            agent_b.scrollback.is_empty(),
            "active agent B's scrollback must remain untouched"
        );
    }

    #[test]
    fn acp_chunk_for_active_agent_returns_affected_true() {
        // Baseline: chunk for the visible agent triggers a redraw.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        let affected = handle(make_agent_chunk_message("sess-B", "hello from B"), &mut app);

        assert!(affected, "chunk for active agent must request a redraw");
        let agent_b = app.agents.get(&AgentId(1)).unwrap();
        assert_eq!(agent_message_text(agent_b), "hello from B");
    }

    #[test]
    fn acp_chunk_for_subagent_routes_through_parent() {
        // Subagent (child) chunk must land in the parent's
        // `subagent_views[child_sid]` even when a different agent is
        // currently active.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        let child_sid = "sess-A-child";
        {
            let parent = app.agents.get_mut(&AgentId(0)).unwrap();
            parent
                .subagent_sessions
                .insert(child_sid.into(), make_subagent_info(child_sid));
            parent
                .subagent_views
                .insert(child_sid.into(), Box::new(make_agent(Some(child_sid))));
        }

        let affected = handle(
            make_agent_chunk_message(child_sid, "hello from subagent"),
            &mut app,
        );

        let parent = app.agents.get(&AgentId(0)).unwrap();
        let child_view = parent
            .subagent_views
            .get(child_sid)
            .expect("child view must still exist");
        assert_eq!(
            agent_message_text(child_view),
            "hello from subagent",
            "subagent chunk must land in subagent_views[child_sid]"
        );
        assert!(
            !affected,
            "subagent chunk for non-active parent must not request a redraw"
        );
    }

    #[test]
    fn acp_chunk_with_unknown_session_id_is_dropped_and_no_redraw() {
        // No agent owns the session_id and the active agent already has a
        // session_id assigned (so the race-window fallback does not fire).
        // The notification must be dropped silently.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        // make_app_with_agent already activated AgentId(0); no switch needed.

        let affected = handle(
            make_agent_chunk_message("sess-unknown", "stray text"),
            &mut app,
        );

        assert!(!affected, "unknown session_id must not request a redraw");
        assert!(
            app.agents.get(&AgentId(0)).unwrap().scrollback.is_empty(),
            "agent A must not have absorbed a notification for sess-unknown"
        );
        assert!(
            app.agents.get(&AgentId(1)).unwrap().scrollback.is_empty(),
            "agent B must not have absorbed a notification for sess-unknown"
        );
    }

    #[test]
    fn session_id_none_race_window_routes_to_active_agent() {
        // Pin the existing race-window semantics: notifications that arrive
        // before `TaskResult::SessionCreated` (active agent has no session_id
        // yet) must still land on the active agent.

        // Case 1: active agent A has session_id == None; everyone else has
        // a real id. Stray notification routes to A.
        {
            let mut app = make_app_with_agent("sess-A");
            app.agents.get_mut(&AgentId(0)).unwrap().session.session_id = None;
            insert_agent(&mut app, AgentId(1), Some("sess-B"));
            // make_app_with_agent already activated AgentId(0); no switch needed.

            let _ = handle(
                make_agent_chunk_message("not-yet-assigned", "racing chunk"),
                &mut app,
            );

            assert_eq!(
                agent_message_text(app.agents.get(&AgentId(0)).unwrap()),
                "racing chunk",
                "race-window fallback should land on active agent A"
            );
            assert!(
                app.agents.get(&AgentId(1)).unwrap().scrollback.is_empty(),
                "non-active agent B must not absorb the race chunk"
            );
        }

        // Case 2: both A and B have session_id == None; the active one wins.
        {
            let mut app = make_app_with_agent("sess-A");
            app.agents.get_mut(&AgentId(0)).unwrap().session.session_id = None;
            insert_agent(&mut app, AgentId(1), None);
            switch_active_to(&mut app, AgentId(1));

            let _ = handle(
                make_agent_chunk_message("not-yet-assigned", "racing chunk"),
                &mut app,
            );

            assert!(
                app.agents.get(&AgentId(0)).unwrap().scrollback.is_empty(),
                "non-active agent A must not absorb the race chunk"
            );
            assert_eq!(
                agent_message_text(app.agents.get(&AgentId(1)).unwrap()),
                "racing chunk",
                "race-window fallback must prefer the active agent (B)"
            );
        }
    }

    #[test]
    fn plan_update_for_inactive_agent_lands_in_its_todo() {
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));
        // Sanity: A's todo starts empty.
        assert_eq!(
            app.agents.get(&AgentId(0)).unwrap().todo.counts().total(),
            0,
        );

        let _ = handle(make_plan_message("sess-A", &["task1", "task2"]), &mut app);

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent_a.todo.counts().total(),
            2,
            "Plan update must mutate A's todo even when B is active"
        );
        let agent_b = app.agents.get(&AgentId(1)).unwrap();
        assert_eq!(
            agent_b.todo.counts().total(),
            0,
            "active agent B's todo must not absorb A's plan"
        );
    }

    #[test]
    fn commands_update_for_inactive_agent_bumps_its_generation() {
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));
        let initial_gen_a = app
            .agents
            .get(&AgentId(0))
            .unwrap()
            .session
            .available_commands_generation;

        let _ = handle(
            make_commands_update_message("sess-A", &["compact", "fork"]),
            &mut app,
        );

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent_a.session.available_commands.len(),
            2,
            "AvailableCommandsUpdate must replace A's commands list"
        );
        assert_eq!(
            agent_a.session.available_commands_generation,
            initial_gen_a + 1,
            "AvailableCommandsUpdate must bump A's generation counter"
        );
    }

    #[test]
    fn bg_task_stdout_for_inactive_agent_lands_in_its_bg_tasks() {
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        // Pre-register a bg task on A so route_bg_task_stdout has a target.
        let task_id = "task-A-1";
        let tool_call_id = "call-A-1";
        {
            let agent_a = app.agents.get_mut(&AgentId(0)).unwrap();
            agent_a.session.bg_tasks.insert(
                task_id.into(),
                BgTaskState {
                    task_id: task_id.into(),
                    tool_call_id: tool_call_id.into(),
                    command: "sleep 5".into(),
                    description: None,
                    cwd: "/tmp".into(),
                    output_file: "/tmp/out".into(),
                    status: BgTaskStatus::Running,
                    start_time: std::time::SystemTime::now(),
                    end_time: None,
                    exit_code: None,
                    signal: None,
                    stdout: String::new(),
                    stdout_line_count: 0,
                    truncated: false,
                    pending_kill: false,
                    kill_requested_at: None,
                    scrollback_entry_id: None,
                    is_monitor: false,
                    restored_from_replay: false,
                },
            );
            agent_a
                .session
                .bg_tool_call_to_task
                .insert(tool_call_id.into(), task_id.into());
        }

        let _ = handle(
            make_bash_stdout_message("sess-A", tool_call_id, "stdout-from-A"),
            &mut app,
        );

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent_a.session.bg_tasks.get(task_id).unwrap().stdout,
            "stdout-from-A",
            "Bash stdout must land in A's bg_tasks even when B is active"
        );
    }

    #[test]
    fn acp_chunks_for_two_agents_dont_cross_contaminate() {
        // Send chunks to both A and B in sequence; each landing in its own
        // scrollback proves the demux works in both directions regardless
        // of which agent is currently active.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        let _ = handle(make_agent_chunk_message("sess-A", "A only"), &mut app);
        let _ = handle(make_agent_chunk_message("sess-B", "B only"), &mut app);

        assert_eq!(
            agent_message_text(app.agents.get(&AgentId(0)).unwrap()),
            "A only",
        );
        assert_eq!(
            agent_message_text(app.agents.get(&AgentId(1)).unwrap()),
            "B only",
        );
    }

