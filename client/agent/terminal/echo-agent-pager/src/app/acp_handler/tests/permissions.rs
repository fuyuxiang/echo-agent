#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    /// The permission prompt must surface the payload an MCP call would
    /// send — both `UseTool` (meta-dispatch) and `MCPTool` (natively
    /// registered) raw_input shapes.
    #[test]
    fn mcp_args_lines_extracts_planned_tool_input() {
        for variant in ["UseTool", "MCPTool"] {
            let req = permission_req_with_raw_input(Some(serde_json::json!({
                "variant": variant,
                "tool_name": "jira__AddjiraComment",
                "tool_input": {"issue": "ABC-123", "body": "hello"},
            })));
            let lines = mcp_args_lines(&req);
            let joined = lines.join("\n");
            assert!(
                joined.contains("\"issue\": \"ABC-123\""),
                "{variant}: {joined}"
            );
            assert!(
                joined.contains("\"body\": \"hello\""),
                "{variant}: {joined}"
            );
        }
    }

    /// Non-MCP raw_input (bash, edit, gateway `{command}` shapes) must not
    /// grow a JSON dump — those prompts have dedicated displays.
    #[test]
    fn mcp_args_lines_empty_for_non_mcp_shapes() {
        for raw in [
            None,
            Some(serde_json::json!({"variant": "Bash", "command": "ls", "description": "d"})),
            Some(serde_json::json!({"command": "rm -rf /"})),
            Some(serde_json::json!({"file_path": "/tmp/x"})),
            Some(serde_json::json!("not-an-object")),
        ] {
            let req = permission_req_with_raw_input(raw.clone());
            assert!(
                mcp_args_lines(&req).is_empty(),
                "expected empty for {raw:?}"
            );
        }
    }

    /// A `tool_input` that is missing or JSON null renders nothing rather
    /// than a misleading `null`.
    #[test]
    fn mcp_args_lines_empty_for_missing_or_null_input() {
        for raw in [
            serde_json::json!({"variant": "UseTool", "tool_name": "t"}),
            serde_json::json!({"variant": "UseTool", "tool_name": "t", "tool_input": null}),
        ] {
            let req = permission_req_with_raw_input(Some(raw));
            assert!(mcp_args_lines(&req).is_empty());
        }
    }

    /// A pathological single-line value (e.g. an embedded base64 blob) is
    /// elided at `MCP_ARGS_MAX_LINE_CHARS` so per-frame wrap cost stays
    /// bounded. Uses a multi-byte char to pin char (not byte) slicing.
    #[test]
    fn mcp_args_lines_caps_line_length() {
        let req = permission_req_with_raw_input(Some(serde_json::json!({
            "variant": "UseTool",
            "tool_name": "t",
            "tool_input": {"blob": "é".repeat(MCP_ARGS_MAX_LINE_CHARS * 2)},
        })));
        let lines = mcp_args_lines(&req);
        let long = lines
            .iter()
            .find(|l| l.contains("é"))
            .expect("blob line present");
        assert_eq!(long.chars().count(), MCP_ARGS_MAX_LINE_CHARS + 1);
        assert!(long.ends_with('…'));
    }

    /// Pathologically large payloads are capped in storage with an explicit
    /// hidden-line count (the overlay clips further at render time).
    #[test]
    fn mcp_args_lines_caps_stored_lines() {
        let big: serde_json::Map<String, serde_json::Value> = (0..MCP_ARGS_MAX_LINES + 50)
            .map(|i| (format!("k{i:04}"), serde_json::Value::from(i)))
            .collect();
        let req = permission_req_with_raw_input(Some(serde_json::json!({
            "variant": "UseTool",
            "tool_name": "t",
            "tool_input": big,
        })));
        let lines = mcp_args_lines(&req);
        assert_eq!(lines.len(), MCP_ARGS_MAX_LINES + 1);
        let last = lines.last().unwrap();
        assert!(
            last.starts_with("… (+") && last.ends_with(" more lines)"),
            "unexpected tail: {last}"
        );
    }

    /// Manual recap with an uncommitted in-flight spinner: filled in place
    /// (no second block), animation stopped.
    #[test]
    fn recap_fills_uncommitted_spinner_in_place() {
        let mut agent = make_agent(Some("s1"));
        let spinner = agent
            .scrollback
            .push(crate::scrollback::entry::ScrollbackEntry::running(
                recap_block(""),
            ));
        agent.pending_recap_entry = Some(spinner);

        apply_recap_block(&mut agent, false, recap_block("THE RECAP"));

        assert_eq!(agent.scrollback.len(), 1, "filled in place, not appended");
        let entry = agent.scrollback.get_by_id(spinner).expect("entry kept");
        assert!(!entry.is_running, "spinner animation stopped");
        assert!(agent.pending_recap_entry.is_none());
    }

    /// Regression (minimal mode): the spinner was already committed into
    /// native scrollback (print-once) — an in-place fill would never reach the
    /// terminal. The stale committed entry is dropped from state and the recap
    /// appended as a fresh (uncommitted) block so the commit pass prints it.
    #[test]
    fn recap_reprints_fresh_block_when_spinner_already_committed() {
        let mut agent = make_agent(Some("s1"));
        let spinner = agent
            .scrollback
            .push(crate::scrollback::entry::ScrollbackEntry::running(
                recap_block(""),
            ));
        agent.pending_recap_entry = Some(spinner);
        // The minimal idle commit pass consumed the spinner.
        agent.scrollback.finish_running(spinner);
        agent.scrollback.mark_committed(0);
        agent.scrollback.set_commit_scan_cursor(1);
        assert!(agent.scrollback.is_committed(spinner));

        apply_recap_block(&mut agent, false, recap_block("THE RECAP"));

        assert_eq!(
            agent.scrollback.len(),
            1,
            "stale committed spinner dropped, fresh block appended"
        );
        let fresh = agent.scrollback.get(0).expect("fresh block");
        assert_ne!(fresh.id, spinner, "a NEW entry, not the committed one");
        assert!(
            !agent.scrollback.is_committed(fresh.id),
            "fresh block is uncommitted so the commit pass will print it"
        );
    }

    /// An automatic recap never consumes the manual loading slot — it always
    /// appends its own block and leaves the pending spinner alone.
    #[test]
    fn auto_recap_appends_and_leaves_manual_spinner_pending() {
        let mut agent = make_agent(Some("s1"));
        let spinner = agent
            .scrollback
            .push(crate::scrollback::entry::ScrollbackEntry::running(
                recap_block(""),
            ));
        agent.pending_recap_entry = Some(spinner);

        apply_recap_block(&mut agent, true, recap_block("AUTO RECAP"));

        assert_eq!(agent.scrollback.len(), 2, "auto recap appended");
        assert_eq!(agent.pending_recap_entry, Some(spinner));
    }

    #[test]
    fn late_auto_recap_dropped_when_agent_not_idle() {
        let idle = make_agent(Some("s1"));
        let mut busy = make_agent(Some("s1"));
        busy.session.state = crate::app::agent::AgentState::TurnRunning;

        assert!(should_drop_late_auto_recap(true, false, &busy));
        assert!(
            !should_drop_late_auto_recap(true, false, &idle),
            "idle agent: show auto recap"
        );
        assert!(
            !should_drop_late_auto_recap(false, false, &busy),
            "manual /recap always shown"
        );
        assert!(
            !should_drop_late_auto_recap(true, true, &busy),
            "history replay rebuilds scrollback even mid-turn"
        );
    }

    fn running_bg_task(is_monitor: bool) -> crate::app::agent::BgTaskState {
        crate::app::agent::BgTaskState {
            task_id: "t1".into(),
            tool_call_id: "c1".into(),
            command: "sleep 5".into(),
            description: None,
            cwd: "/tmp".into(),
            output_file: "/tmp/out".into(),
            status: crate::app::agent::BgTaskStatus::Running,
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
            is_monitor,
            restored_from_replay: false,
        }
    }

    #[test]
    fn recap_idle_allows_monitors_but_not_subagents_or_turn_wait() {
        let mut agent = make_agent(Some("s1"));
        agent.scrollback.push_block(
            crate::scrollback::block::RenderBlock::agent_message("done"),
        );
        assert!(
            !should_drop_late_auto_recap(true, false, &agent),
            "agent message + idle"
        );

        agent.session.bg_tasks.insert("mon".into(), running_bg_task(true));
        assert!(
            !should_drop_late_auto_recap(true, false, &agent),
            "running monitors must not block recap"
        );

        agent
            .session
            .bg_tasks
            .insert("bash".into(), running_bg_task(false));
        assert!(
            should_drop_late_auto_recap(true, false, &agent),
            "non-monitor bg task is not idle"
        );
        agent.session.bg_tasks.remove("bash");

        agent
            .subagent_sessions
            .insert("child".into(), make_subagent_info("child"));
        assert!(
            should_drop_late_auto_recap(true, false, &agent),
            "running subagent is not idle"
        );
        agent.subagent_sessions.get_mut("child").unwrap().finished = true;
        assert!(
            !should_drop_late_auto_recap(true, false, &agent),
            "finished subagent is idle again"
        );

        agent.session.enqueue_prompt("queued follow-up".into());
        assert!(
            should_drop_late_auto_recap(true, false, &agent),
            "queued prompt is a turn wait"
        );
        agent.session.pending_prompts.clear();

        agent.shared_queue = vec![crate::app::prompt_queue::QueueEntryWire {
            id: "sq-1".into(),
            version: 0,
            owner: None,
            last_editor: None,
            kind: "prompt".into(),
            text: "server follow-up".into(),
            position: 0,
            combined_texts: None,
        }];
        assert!(
            should_drop_late_auto_recap(true, false, &agent),
            "shared_queue follow-up is a turn wait"
        );
        agent.shared_queue.clear();

        agent.session.in_flight_prompt = Some(crate::app::agent::InFlightPrompt {
            text: "hi".into(),
            images: Vec::new(),
            scrollback_entry: crate::scrollback::entry::EntryId::new(1),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        assert!(
            should_drop_late_auto_recap(true, false, &agent),
            "in-flight prompt is a turn wait"
        );
        agent.session.in_flight_prompt = None;

        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::user_prompt(
                "so wait, what was the issue",
            ));
        assert!(
            should_drop_late_auto_recap(true, false, &agent),
            "trailing user prompt is waiting on a turn"
        );
        agent.scrollback.push_block(
            crate::scrollback::block::RenderBlock::bg_task("monitor logs", "mon-1"),
        );
        assert!(
            should_drop_late_auto_recap(true, false, &agent),
            "bg-task trailer after a waiting user prompt is still a turn wait"
        );
        agent.scrollback.push_block(crate::scrollback::block::RenderBlock::session_event(
            crate::scrollback::blocks::SessionEvent::TurnCancelled {
                elapsed: std::time::Duration::from_secs(1),
            },
        ));
        assert!(
            !should_drop_late_auto_recap(true, false, &agent),
            "turn terminal after a user prompt is idle"
        );

        let mut failed = make_agent(Some("s1"));
        failed.scrollback.push_block(
            crate::scrollback::block::RenderBlock::user_prompt("try again"),
        );
        failed.scrollback.push_block(
            crate::scrollback::block::RenderBlock::session_event(
                crate::scrollback::blocks::SessionEvent::ReAuthRequired,
            ),
        );
        assert!(
            !should_drop_late_auto_recap(true, false, &failed),
            "ReAuthRequired settles the turn without TurnFailed"
        );
        failed.scrollback.push_block(
            crate::scrollback::block::RenderBlock::session_event(
                crate::scrollback::blocks::SessionEvent::ContextTooLarge,
            ),
        );
        assert!(
            !should_drop_late_auto_recap(true, false, &failed),
            "ContextTooLarge settles the turn without TurnFailed"
        );
    }

    #[test]
    fn duplicate_live_auto_recap_dropped_after_existing_recap() {
        let mut agent = make_agent(Some("s1"));
        agent.scrollback.push_block(recap_block("first"));
        assert!(should_drop_duplicate_auto_recap(
            true,
            false,
            &agent.scrollback
        ));
        assert!(
            !should_drop_duplicate_auto_recap(true, true, &agent.scrollback),
            "replay must still paint stored recaps"
        );
        assert!(
            !should_drop_duplicate_auto_recap(false, false, &agent.scrollback),
            "manual /recap still allowed"
        );
    }

    #[test]
    fn duplicate_auto_recap_allowed_after_new_user_prompt() {
        let mut agent = make_agent(Some("s1"));
        agent.scrollback.push_block(recap_block("old"));
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::user_prompt(
                "next question",
            ));
        assert!(
            !should_drop_duplicate_auto_recap(true, false, &agent.scrollback),
            "new user turn re-arms auto recap"
        );
    }

    #[test]
    fn enqueue_while_scrollback_steals_focus_to_prompt() {
        use crate::app::agent_view::AgentPane;

        let mut app = make_app_with_agent("sess-1");
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .set_active_pane(AgentPane::Scrollback, true);

        let (msg, _rx) = make_permission_message("sess-1");
        handle(msg, &mut app);

        let agent = &app.agents[&AgentId(0)];
        assert_eq!(agent.permission_queue.len(), 1);
        assert_eq!(agent.active_pane, AgentPane::Prompt);
        assert_eq!(agent.permission_stashed_pane, Some(AgentPane::Scrollback));
    }

    #[test]
    fn enqueue_while_prompt_does_not_stash_pane() {
        use crate::app::agent_view::AgentPane;

        let mut app = make_app_with_agent("sess-1");
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .set_active_pane(AgentPane::Prompt, true);

        let (msg, _rx) = make_permission_message("sess-1");
        handle(msg, &mut app);

        let agent = &app.agents[&AgentId(0)];
        assert_eq!(agent.permission_queue.len(), 1);
        assert_eq!(agent.active_pane, AgentPane::Prompt);
        assert!(agent.permission_stashed_pane.is_none());
    }

    #[test]
    fn enqueue_while_queue_or_tasks_does_not_steal() {
        use crate::app::agent_view::AgentPane;

        for pane in [AgentPane::Queue, AgentPane::Tasks] {
            let mut app = make_app_with_agent("sess-1");
            app.agents
                .get_mut(&AgentId(0))
                .unwrap()
                .set_active_pane(pane, true);

            let (msg, _rx) = make_permission_message("sess-1");
            handle(msg, &mut app);

            let agent = &app.agents[&AgentId(0)];
            assert_eq!(agent.permission_queue.len(), 1, "pane={pane:?}");
            assert_eq!(agent.active_pane, pane);
            assert!(agent.permission_stashed_pane.is_none(), "pane={pane:?}");
        }
    }

    #[test]
    fn second_enqueue_does_not_resteal_if_user_returned_to_scrollback() {
        use crate::app::agent_view::AgentPane;

        let mut app = make_app_with_agent("sess-1");
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .set_active_pane(AgentPane::Scrollback, true);

        let (msg1, _rx1) = make_permission_message("sess-1");
        handle(msg1, &mut app);
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .set_active_pane(AgentPane::Scrollback, true);

        let (msg2, _rx2) = make_permission_message("sess-1");
        handle(msg2, &mut app);

        let agent = &app.agents[&AgentId(0)];
        assert_eq!(agent.permission_queue.len(), 2);
        assert_eq!(agent.active_pane, AgentPane::Scrollback);
        assert_eq!(agent.permission_stashed_pane, Some(AgentPane::Scrollback));
    }

    #[test]
    fn enqueue_while_scrollback_then_select_restores_scrollback() {
        use crate::app::actions::Action;
        use crate::app::agent_view::AgentPane;
        use crate::app::dispatch::dispatch;
        use std::sync::Arc;

        let mut app = make_app_with_agent("sess-1");
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .set_active_pane(AgentPane::Scrollback, true);

        let (msg, _rx) = make_permission_message("sess-1");
        handle(msg, &mut app);
        {
            let agent = &app.agents[&AgentId(0)];
            assert_eq!(agent.active_pane, AgentPane::Prompt);
            assert_eq!(agent.permission_stashed_pane, Some(AgentPane::Scrollback));
        }

        let _ = dispatch(
            Action::PermissionSelect(acp::PermissionOptionId::new(Arc::from("allow-once"))),
            &mut app,
        );

        let agent = &app.agents[&AgentId(0)];
        assert!(agent.permission_queue.is_empty());
        assert_eq!(agent.active_pane, AgentPane::Scrollback);
        assert!(agent.permission_stashed_pane.is_none());
    }

