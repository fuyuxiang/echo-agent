#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    #[test]
    fn interaction_resolved_dismisses_matching_permission() {
        // A peer answered a shared permission → this pane retracts its copy.
        let mut app = make_app_with_agent("sess-1");
        let (msg, _rx) = make_permission_message("sess-1");
        handle(msg, &mut app);
        assert_eq!(app.agents[&AgentId(0)].permission_queue.len(), 1);

        let changed = handle_session_notification(
            &interaction_resolved_ext("sess-1", "call-perm-1"),
            &mut app,
        );
        assert!(changed, "dismissing a visible permission must redraw");
        assert!(
            app.agents[&AgentId(0)].permission_queue.is_empty(),
            "the resolved permission must be removed from the queue"
        );
    }

    #[test]
    fn interaction_resolved_dismisses_matching_question() {
        use crate::views::question_view::QuestionViewState;
        let mut app = make_app_with_agent("sess-1");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            let stashed = agent.prompt.stash();
            agent.question_view = Some(QuestionViewState::new("call-q".into(), vec![], stashed));
        }

        let changed =
            handle_session_notification(&interaction_resolved_ext("sess-1", "call-q"), &mut app);
        assert!(changed, "dismissing a visible question must redraw");
        assert!(
            app.agents[&AgentId(0)].question_view.is_none(),
            "the resolved question must be cleared"
        );
    }

    #[test]
    fn interaction_resolved_dismisses_matching_plan_approval() {
        let mut app = make_app_with_agent("sess-1");
        let (ext, _rx) = make_exit_plan_ext_with_tool_call_id("call-plan", Some("# Plan"));
        assert!(handle_exit_plan_mode(ext, &mut app));
        assert!(app.agents[&AgentId(0)].plan_approval_view.is_some());

        let changed =
            handle_session_notification(&interaction_resolved_ext("sess-1", "call-plan"), &mut app);
        assert!(changed, "dismissing a visible plan approval must redraw");
        assert!(
            app.agents[&AgentId(0)].plan_approval_view.is_none(),
            "the resolved plan approval must be cleared"
        );
    }

    #[test]
    fn interaction_resolved_is_noop_for_unknown_tool_call_id() {
        let mut app = make_app_with_agent("sess-1");
        let (msg, _rx) = make_permission_message("sess-1");
        handle(msg, &mut app);

        let changed = handle_session_notification(
            &interaction_resolved_ext("sess-1", "some-other-call"),
            &mut app,
        );
        assert!(!changed, "an unknown tool_call_id must be a silent no-op");
        assert_eq!(
            app.agents[&AgentId(0)].permission_queue.len(),
            1,
            "an unrelated pending modal must be left intact"
        );
    }

    #[test]
    fn permission_for_inactive_agent_queues_on_owning_agent() {
        // The headline behavior change in handle_permission_request:
        // permissions for an inactive owning agent now QUEUE (not cancel)
        // so the user sees them on switching back.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        let (msg, mut rx) = make_permission_message("sess-A");
        let affected = handle(msg, &mut app);

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent_a.permission_queue.len(),
            1,
            "permission for inactive A must queue on A's permission_queue"
        );
        let agent_b = app.agents.get(&AgentId(1)).unwrap();
        assert_eq!(
            agent_b.permission_queue.len(),
            0,
            "active B's permission_queue must remain empty"
        );
        assert!(
            !affected,
            "permission queued on a non-active agent must not request a redraw"
        );
        // Permission is still pending; the response_tx must still be alive
        // (no auto-cancel was sent).
        assert!(
            rx.try_recv().is_err(),
            "permission must NOT have been answered yet (queued, not cancelled)"
        );
    }

    #[test]
    fn exec_vehicle_permission_enqueues_a_persisting_default_scope() {
        // Regression guard for the enqueue invariant: an exec-vehicle bash
        // prompt that offers the scoped "Always allow:" row must open on a
        // default scope that persists a grant — the full command, not a bare
        // `python3` prefix (which the ←/→ arrows could not repair).
        use std::sync::Arc;
        use echo_agent_workspace::permission::bash_command_splitting::BashCommandHighlights;

        let mut app = make_app_with_agent("sess-1");
        let highlights = BashCommandHighlights {
            prefix: vec![],
            highlighted_words: vec![
                "python3".to_owned(),
                "-u".to_owned(),
                "foo.py".to_owned(),
                "arg".to_owned(),
            ],
            suffix: vec![],
        };
        let meta = serde_json::to_value(&highlights).unwrap().as_object().cloned();

        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = acp::RequestPermissionRequest::new(
            acp::SessionId::new("sess-1"),
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from("call-perm-exec")),
                acp::ToolCallUpdateFields::default(),
            ),
            vec![
                acp::PermissionOption::new(
                    acp::PermissionOptionId::new(Arc::from("allow-once")),
                    "Allow once",
                    acp::PermissionOptionKind::AllowOnce,
                ),
                acp::PermissionOption::new(
                    acp::PermissionOptionId::new(Arc::from("allow-always-command")),
                    "Always allow",
                    acp::PermissionOptionKind::AllowAlways,
                ),
            ],
        )
        .meta(meta);
        let msg = AcpClientMessage::RequestPermission(echo_agent_acp::AcpArgs {
            request,
            response_tx: tx,
        });

        handle(msg, &mut app);

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let perm = agent.permission_queue.front().expect("permission queued");
        assert_eq!(
            perm.bash_selection_count, 4,
            "exec vehicle must open on the full-command scope"
        );
        assert!(
            echo_agent_workspace::permission::always_allow_scope_persists(
                perm.bash_highlights.as_ref().unwrap(),
                perm.bash_selection_count,
            ),
            "the enqueue default scope must persist a grant"
        );
    }

    #[test]
    fn ask_user_question_routes_to_background_session_not_active_view() {
        // Repro of the dashboard bug: a session started but not entered asks a
        // question. Active view is agent A (sess-A); the question is for the
        // BACKGROUND agent B (sess-B). It must land on B, not fail or land on A.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        assert_eq!(app.active_view, ActiveView::Agent(AgentId(0)));

        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let raw = serde_json::value::to_raw_value(&serde_json::json!({
            "sessionId": "sess-B",
            "toolCallId": "tc-bg",
            "questions": [],
            "mode": "default",
        }))
        .unwrap();
        let msg = AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
            request: acp::ExtRequest::new("echo.agent/ask_user_question", raw.into()),
            response_tx: tx,
        });

        let affected = handle(msg, &mut app);

        assert!(
            !affected,
            "a background-session question must not redraw the active view"
        );
        assert!(
            app.agents.get(&AgentId(1)).unwrap().question_view.is_some(),
            "question must be parked on the session that asked (background agent B)"
        );
        assert!(
            app.agents.get(&AgentId(0)).unwrap().question_view.is_none(),
            "question must NOT land on the unrelated active agent A"
        );
        assert!(
            rx.try_recv().is_err(),
            "response must NOT be sent yet (parked, waiting for user)"
        );
    }

    #[test]
    fn mcp_elicit_opens_elicitation_view_and_parks_response() {
        let mut app = make_app_with_agent("sess-A");
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let raw = serde_json::value::to_raw_value(&serde_json::json!({
            "sessionId": "sess-A",
            "toolCallId": "mcp-elicit-1",
            "serverName": "demo-mcp",
            "message": "Need your email",
            "mode": "form",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "email": { "type": "string", "format": "email" }
                },
                "required": ["email"]
            }
        }))
        .unwrap();
        let msg = AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
            request: acp::ExtRequest::new("echo.agent/mcp/elicit", raw.into()),
            response_tx: tx,
        });

        let affected = handle(msg, &mut app);
        assert!(affected, "active session elicitation should request redraw");
        let agent = app.agents.get(&AgentId(0)).unwrap();
        let ev = agent
            .elicitation_view
            .as_ref()
            .expect("elicitation_view open");
        assert_eq!(ev.server_name, "demo-mcp");
        assert_eq!(ev.tool_call_id, "mcp-elicit-1");
        assert!(
            rx.try_recv().is_err(),
            "response must wait for user Accept/Decline/Cancel"
        );
    }

    #[test]
    fn mcp_elicit_does_not_replace_url_waiting() {
        let mut app = make_app_with_agent("sess-A");
        let (tx1, mut rx1) = tokio::sync::oneshot::channel();
        let raw1 = serde_json::value::to_raw_value(&serde_json::json!({
            "sessionId": "sess-A",
            "toolCallId": "mcp-elicit-url",
            "serverName": "demo-mcp",
            "message": "Open login",
            "mode": "url",
            "url": "https://example.com/login",
            "elicitationId": "eid-1"
        }))
        .unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/mcp/elicit", raw1.into()),
                response_tx: tx1,
            }),
            &mut app,
        );
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            let ev = agent.elicitation_view.as_mut().unwrap();
            assert!(ev.send_response(
                echo_agent_tools::mcp_elicitation::McpElicitExtResponse::Accept { content: None },
            ));
            ev.begin_url_waiting();
        }
        assert!(rx1.try_recv().is_ok(), "URL accept must send ACP immediately");

        let (tx2, mut rx2) = tokio::sync::oneshot::channel();
        let raw2 = serde_json::value::to_raw_value(&serde_json::json!({
            "sessionId": "sess-A",
            "toolCallId": "mcp-elicit-form",
            "serverName": "demo-mcp",
            "message": "Need email",
            "mode": "form",
            "requestedSchema": {
                "type": "object",
                "properties": { "email": { "type": "string" } }
            }
        }))
        .unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/mcp/elicit", raw2.into()),
                response_tx: tx2,
            }),
            &mut app,
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let ev = agent.elicitation_view.as_ref().unwrap();
        assert!(ev.is_url_waiting());
        assert_eq!(ev.elicitation_id(), Some("eid-1"));
        assert!(agent.pending_elicitation.is_some());
        assert!(
            rx2.try_recv().is_err(),
            "the next elicit must wait until Waiting chrome is dismissed"
        );

        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        assert!(agent.dismiss_waiting_elicitation("eid-1", None));
        let ev = agent.elicitation_view.as_ref().expect("parked form shown");
        assert!(ev.form().is_some(), "promoted card is the parked form");
        assert_eq!(ev.tool_call_id, "mcp-elicit-form");
        assert!(rx2.try_recv().is_err());
    }

    /// The reverse layering of the test below: the elicitation opened FIRST
    /// (so it holds the true session draft), a question arrived on top, and
    /// then the elicitation is peer-resolved while the question still owns
    /// the composer. The draft must be handed to the question's stash — not
    /// written through the live composer, where the question's own close
    /// would restore its empty stash over it.
    #[test]
    fn peer_resolved_elicitation_hands_draft_to_open_question() {
        use crate::views::question_view::QuestionViewState;

        let mut app = make_app_with_agent("sess-A");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.prompt.set_text("my precious draft");
        }

        // Elicitation opens first and stashes the session draft.
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let raw = serde_json::value::to_raw_value(&serde_json::json!({
            "sessionId": "sess-A",
            "toolCallId": "mcp-elicit-1",
            "serverName": "demo-mcp",
            "message": "Need your email",
            "mode": "form",
            "requestedSchema": {
                "type": "object",
                "properties": { "email": { "type": "string" } }
            }
        }))
        .unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/mcp/elicit", raw.into()),
                response_tx: tx,
            }),
            &mut app,
        );

        // A question arrives on top and takes the (blank) composer.
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            assert_eq!(agent.prompt.text(), "", "elicitation displaced the draft");
            let stashed = agent.prompt.stash();
            agent.prompt.set_text("");
            agent.question_view = Some(QuestionViewState::new("call-q".into(), vec![], stashed));
        }

        // A peer resolves the elicitation while the question is open.
        handle_session_notification(&interaction_resolved_ext("sess-A", "mcp-elicit-1"), &mut app);
        assert!(app.agents[&AgentId(0)].elicitation_view.is_none());
        assert_eq!(
            app.agents[&AgentId(0)].prompt.text(),
            "",
            "the question still owns the composer; the draft must not write through"
        );

        // The question closes: the handed-over draft comes back.
        handle_session_notification(&interaction_resolved_ext("sess-A", "call-q"), &mut app);
        assert_eq!(
            app.agents[&AgentId(0)].prompt.text(),
            "my precious draft",
            "the question's close must restore the elicitation's session draft"
        );
    }

    #[test]
    fn elicitation_over_open_question_does_not_wipe_stashed_draft() {
        use crate::views::question_view::QuestionViewState;

        let mut app = make_app_with_agent("sess-A");
        {
            // A question card already displaced the user's draft: the real
            // text lives in its stash and the live composer is blank.
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.prompt.set_text("my precious draft");
            let stashed = agent.prompt.stash();
            agent.prompt.set_text("");
            agent.question_view =
                Some(QuestionViewState::new("call-q".into(), vec![], stashed));
        }

        let (tx, _rx) = tokio::sync::oneshot::channel();
        let raw = serde_json::value::to_raw_value(&serde_json::json!({
            "sessionId": "sess-A",
            "toolCallId": "mcp-elicit-1",
            "serverName": "demo-mcp",
            "message": "Need your email",
            "mode": "form",
            "requestedSchema": {
                "type": "object",
                "properties": { "email": { "type": "string" } }
            }
        }))
        .unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/mcp/elicit", raw.into()),
                response_tx: tx,
            }),
            &mut app,
        );
        {
            let agent = app.agents.get(&AgentId(0)).unwrap();
            let ev = agent.elicitation_view.as_ref().expect("elicitation open");
            assert!(
                ev.stashed_prompt.is_none(),
                "the question already owns the draft; the elicitation must not stash the blank composer"
            );
        }

        // The question resolves first and restores the draft…
        handle_session_notification(&interaction_resolved_ext("sess-A", "call-q"), &mut app);
        assert_eq!(
            app.agents[&AgentId(0)].prompt.text(),
            "my precious draft",
            "question close must put the draft back"
        );

        // …then the elicitation resolves and must NOT clobber it.
        handle_session_notification(&interaction_resolved_ext("sess-A", "mcp-elicit-1"), &mut app);
        assert!(app.agents[&AgentId(0)].elicitation_view.is_none());
        assert_eq!(
            app.agents[&AgentId(0)].prompt.text(),
            "my precious draft",
            "elicitation close must not restore an empty stash over the draft"
        );
    }

    #[test]
    fn parked_elicit_is_dropped_when_peer_resolves_it() {
        let mut app = make_app_with_agent("sess-A");
        let (tx1, _rx1) = tokio::sync::oneshot::channel();
        let raw1 = serde_json::value::to_raw_value(&serde_json::json!({
            "sessionId": "sess-A",
            "toolCallId": "mcp-elicit-url",
            "serverName": "demo-mcp",
            "message": "Open login",
            "mode": "url",
            "url": "https://example.com/login",
            "elicitationId": "eid-1"
        }))
        .unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/mcp/elicit", raw1.into()),
                response_tx: tx1,
            }),
            &mut app,
        );
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            let ev = agent.elicitation_view.as_mut().unwrap();
            assert!(ev.send_response(
                echo_agent_tools::mcp_elicitation::McpElicitExtResponse::Accept { content: None },
            ));
            ev.begin_url_waiting();
        }

        let (tx2, mut rx2) = tokio::sync::oneshot::channel();
        let raw2 = serde_json::value::to_raw_value(&serde_json::json!({
            "sessionId": "sess-A",
            "toolCallId": "mcp-elicit-form",
            "serverName": "demo-mcp",
            "message": "Need email",
            "mode": "form",
            "requestedSchema": {
                "type": "object",
                "properties": { "email": { "type": "string" } }
            }
        }))
        .unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/mcp/elicit", raw2.into()),
                response_tx: tx2,
            }),
            &mut app,
        );
        assert!(app.agents[&AgentId(0)].pending_elicitation.is_some());

        let changed = handle_session_notification(
            &interaction_resolved_ext("sess-A", "mcp-elicit-form"),
            &mut app,
        );
        assert!(changed);
        assert!(
            app.agents[&AgentId(0)].pending_elicitation.is_none(),
            "peer resolve must drop the parked form"
        );
        match rx2.try_recv() {
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {}
            other => panic!("parked oneshot must be dropped, got {other:?}"),
        }

        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        assert!(agent.dismiss_waiting_elicitation("eid-1", None));
        assert!(
            agent.elicitation_view.is_none(),
            "must not promote a peer-resolved parked form"
        );
    }

    #[test]
    fn elicit_complete_requires_matching_server_name() {
        let mut app = make_app_with_agent("sess-A");
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let raw = serde_json::value::to_raw_value(&serde_json::json!({
            "sessionId": "sess-A",
            "toolCallId": "mcp-elicit-url",
            "serverName": "demo-mcp",
            "message": "Open login",
            "mode": "url",
            "url": "https://example.com/login",
            "elicitationId": "eid-1"
        }))
        .unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/mcp/elicit", raw.into()),
                response_tx: tx,
            }),
            &mut app,
        );
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            let ev = agent.elicitation_view.as_mut().unwrap();
            assert!(ev.send_response(
                echo_agent_tools::mcp_elicitation::McpElicitExtResponse::Accept { content: None },
            ));
            ev.begin_url_waiting();
        }

        let complete = |server_name: &str| {
            serde_json::value::to_raw_value(&serde_json::json!({
                "sessionId": "sess-A",
                "elicitationId": "eid-1",
                "serverName": server_name,
            }))
            .unwrap()
        };

        // A different server guessing the id must not dismiss the card.
        let (tx_bad, _rx_bad) = tokio::sync::oneshot::channel();
        let changed = handle(
            AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
                request: acp::ExtNotification::new(
                    "echo.agent/mcp/elicit_complete",
                    complete("evil-mcp").into(),
                ),
                response_tx: tx_bad,
            }),
            &mut app,
        );
        assert!(!changed);
        assert!(
            app.agents[&AgentId(0)].elicitation_view.is_some(),
            "a mismatched serverName must not dismiss the waiting card"
        );

        // The emitting server's own complete dismisses it.
        let (tx_ok, _rx_ok) = tokio::sync::oneshot::channel();
        let changed = handle(
            AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
                request: acp::ExtNotification::new(
                    "echo.agent/mcp/elicit_complete",
                    complete("demo-mcp").into(),
                ),
                response_tx: tx_ok,
            }),
            &mut app,
        );
        assert!(changed);
        assert!(
            app.agents[&AgentId(0)].elicitation_view.is_none(),
            "the matching serverName must dismiss the waiting card"
        );
    }

    #[test]
    fn ask_user_question_unknown_session_parks_without_error() {
        // No local view for the session, and the active agent HAS a session_id
        // (so the race-window fallback does not fire). The reverse-request must
        // be left UNANSWERED (dropped) — NOT failed with an error, which would
        // render the tool red. Leader replay-on-attach handles it later.
        let mut app = make_app_with_agent("sess-A");

        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let raw = serde_json::value::to_raw_value(&serde_json::json!({
            "sessionId": "sess-unknown",
            "toolCallId": "tc-unknown",
            "questions": [],
            "mode": "default",
        }))
        .unwrap();
        let msg = AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
            request: acp::ExtRequest::new("echo.agent/ask_user_question", raw.into()),
            response_tx: tx,
        });

        let affected = handle(msg, &mut app);

        assert!(!affected);
        assert!(
            app.agents.get(&AgentId(0)).unwrap().question_view.is_none(),
            "must not attach the question to an unrelated active agent"
        );
        // A dropped oneshot sender yields `Closed`; `Empty` would mean still
        // held open, `Ok` would mean a (failing) response was sent.
        match rx.try_recv() {
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {}
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                panic!("response_tx must be dropped (parked), not held open")
            }
            Ok(_) => panic!("must NOT send any response — that would fail/resolve the tool"),
        }
    }

    #[test]
    fn permission_for_inactive_yolo_agent_auto_approves() {
        // YOLO mode is honored on the OWNING agent, not the active one,
        // so background turns aren't blocked waiting for a switch.
        let mut app = make_app_with_agent("sess-A");
        app.agents.get_mut(&AgentId(0)).unwrap().session.yolo_mode = true;
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        let (msg, rx) = make_permission_message("sess-A");
        let affected = handle(msg, &mut app);

        assert!(!affected, "YOLO auto-approve never needs a redraw");
        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent_a.permission_queue.len(),
            0,
            "YOLO must auto-approve in place of queueing"
        );
        let response = rx
            .blocking_recv()
            .expect("YOLO must have sent a response on response_tx");
        let resp = response.expect("YOLO response must be Ok");
        match resp.outcome {
            acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome {
                option_id,
                ..
            }) => {
                assert_eq!(option_id.0.as_ref(), "allow-once");
            }
            other => panic!("expected Selected, got {other:?}"),
        }
    }

    #[test]
    fn permission_for_unknown_session_id_is_cancelled() {
        // No agent owns the session and the active agent already has a
        // session_id (so the race-window fallback does not fire). The
        // permission must be cancelled rather than queued anywhere.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        // make_app_with_agent already activated AgentId(0); no switch needed.

        let (msg, rx) = make_permission_message("sess-unknown");
        let affected = handle(msg, &mut app);

        assert!(!affected);
        for id in [AgentId(0), AgentId(1)] {
            assert_eq!(
                app.agents.get(&id).unwrap().permission_queue.len(),
                0,
                "no agent should have queued the unknown-session permission",
            );
        }
        let response = rx
            .blocking_recv()
            .expect("cancel_permission must have sent a response");
        let resp = response.expect("response must be Ok");
        assert!(
            matches!(resp.outcome, acp::RequestPermissionOutcome::Cancelled),
            "unknown session_id permissions must be cancelled, got {:?}",
            resp.outcome,
        );
    }

    // ── Plan approval persistence tests ─────────────────────────

    #[test]
    fn close_viewer_preserves_plan_approval_state() {
        let mut app = make_app_with_agent("sess-A");

        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let ext_req = crate::views::plan_approval_view::ExitPlanModeExtRequest {
            session_id: "sess-A".into(),
            tool_call_id: "tc-persist".into(),
            plan_content: Some("# Plan\nDo stuff".into()),
        };
        let raw = serde_json::value::to_raw_value(&ext_req).unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/exit_plan_mode", raw.into()),
                response_tx: tx,
            }),
            &mut app,
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.plan_approval_view.is_some(), "approval should be set");

        // Close the viewer (simulates Esc / close button).
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.cancel_line_viewer();

        // Approval state must survive the close.
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent.plan_approval_view.is_some(),
            "plan_approval_view must persist after viewer close"
        );
        assert!(agent.line_viewer.is_none(), "viewer should be closed");

        // Response must NOT have been sent (still waiting for user).
        assert!(
            rx.try_recv().is_err(),
            "response must not be sent on viewer close"
        );
    }

    #[test]
    fn reopen_viewer_restores_approval_buttons() {
        let mut app = make_app_with_agent("sess-A");
        // Seed a CreatePlan tool so the source is Inline (plan content
        // is carried in the ext_method params, not read from disk).
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            seed_pending_tool(agent, "tc-reopen", "CreatePlan");
        }

        let (tx, _rx) = tokio::sync::oneshot::channel();
        let ext_req = crate::views::plan_approval_view::ExitPlanModeExtRequest {
            session_id: "sess-A".into(),
            tool_call_id: "tc-reopen".into(),
            plan_content: Some("# Plan\nStep 1".into()),
        };
        let raw = serde_json::value::to_raw_value(&ext_req).unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/exit_plan_mode", raw.into()),
                response_tx: tx,
            }),
            &mut app,
        );

        // Close viewer.
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.cancel_line_viewer();
        assert!(agent.line_viewer.is_none());

        // Reopen plan preview — inline content is in plan_approval_view.plan_content.
        agent.show_plan_preview();

        assert!(agent.line_viewer.is_some(), "viewer should reopen");
        assert!(
            agent.line_viewer.as_ref().unwrap().feedback_active(),
            "feedback_active must be true after reopen"
        );
    }

    #[test]
    fn approve_after_reopen_keeps_session_draft_and_sends_freeform() {
        let mut app = make_app_with_agent("sess-A");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.prompt.set_text("session draft mid-thinking");
            seed_pending_tool(agent, "tc-prompt", "CreatePlan");
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        let ext_req = crate::views::plan_approval_view::ExitPlanModeExtRequest {
            session_id: "sess-A".into(),
            tool_call_id: "tc-prompt".into(),
            plan_content: Some("# Plan\nDo things".into()),
        };
        let raw = serde_json::value::to_raw_value(&ext_req).unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/exit_plan_mode", raw.into()),
                response_tx: tx,
            }),
            &mut app,
        );

        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        assert_eq!(agent.prompt.text(), "session draft mid-thinking");
        assert_eq!(
            agent
                .plan_approval_view
                .as_ref()
                .map(|p| p.stashed_prompt.text.as_str()),
            Some("session draft mid-thinking"),
        );

        agent.cancel_line_viewer();
        agent.prompt.set_text("revision freeform notes");

        agent.reopen_plan_approval();
        assert_eq!(
            agent
                .plan_approval_view
                .as_ref()
                .map(|p| p.stashed_prompt.text.as_str()),
            Some("session draft mid-thinking"),
        );
        assert_eq!(agent.prompt.text(), "revision freeform notes");

        let outcome = agent.approve_plan();
        assert!(matches!(
            outcome,
            crate::app::app_view::InputOutcome::Action(
                crate::app::actions::Action::Interject { ref text, .. }
            ) if text.contains("revision freeform notes")
        ));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(agent.prompt.text(), "session draft mid-thinking");

        let response = rx.blocking_recv().expect("should have sent response");
        let raw = response.expect("should be Ok");
        let parsed: serde_json::Value = serde_json::from_str(raw.0.get()).unwrap();
        assert_eq!(parsed["outcome"], "approved");
    }

    /// Delivers a status snapshot the way the agent does, and reports whether
    /// the client repainted.
    fn notify_status(app: &mut crate::app::app_view::AppView, cwd: &str) -> bool {
        let notif = SessionNotification {
            session_id: acp::SessionId::new("sess-1"),
            update: EchoAgentSessionUpdate::SessionStatus(Box::new(
                crate::app::status_line::test_context(cwd),
            )),
            meta: None,
        };
        let raw = serde_json::value::to_raw_value(&notif).unwrap();
        let ext = acp::ExtNotification::new("echo.agent/session_notification", std::sync::Arc::from(raw));
        handle_session_notification(&ext, app)
    }

    /// Storing the snapshot paints nothing when no row is configured, and the
    /// agent pushes one at every turn end: reporting a change here would
    /// repaint the whole fleet once per turn for a row nobody draws.
    #[test]
    fn a_status_snapshot_does_not_repaint_a_client_with_no_status_line() {
        let mut app = make_app_with_agent("sess-1");
        assert!(
            !app.current_ui.status_line.reserves_a_row(),
            "disabled is the default"
        );

        assert!(!notify_status(&mut app, "/tmp"), "no row, no repaint");
        assert!(
            app.agents[&AgentId(0)].status_context.is_some(),
            "the payload is still stored for whenever a row is enabled"
        );
        assert!(app.status_line.display().is_none(), "and nothing is drawn");
    }

    /// The other half. An enabled row settles once it has drawn, and an idle
    /// session asks for no ticks, so the snapshot's own repaint is the only
    /// thing that moves the row until the next turn.
    #[test]
    fn a_status_snapshot_repaints_a_row_that_had_already_settled() {
        let mut app = make_app_with_agent("sess-1");
        app.current_ui.status_line =
            echo_agent_status_line::test_support::StatusLineConfigFixture::from_kind(
                echo_agent_status_line::StatusLineType::Builtin,
            )
            .with_items(vec![echo_agent_status_line::StatusLineItem::Cwd])
            .into_config();

        assert!(
            notify_status(&mut app, "/tmp/first"),
            "the first snapshot draws"
        );
        assert!(app.status_line.is_settled(), "a drawn row settles");
        assert_eq!(
            app.status_line_tick_demand(),
            crate::app::app_view::TickDemand::None,
            "an idle settled row asks for no ticks, so only the snapshot can move it"
        );

        // Inside the refresh floor the snapshot defers rather than repaints, so
        // what it must leave behind is a row still asking to be recomputed.
        notify_status(&mut app, "/tmp/second");
        assert_ne!(
            app.status_line_tick_demand(),
            crate::app::app_view::TickDemand::None,
            "the snapshot left the row settled and idle, so it will never redraw"
        );

        app.update_status_line_at(
            std::time::Instant::now() + crate::app::status_line::MIN_REFRESH_INTERVAL_MS,
        );
        assert!(
            app.status_line.take_changed(),
            "the deferred recompute never happened"
        );
    }

    #[test]
    fn exit_plan_mode_prefills_mid_thinking_draft_as_freeform() {
        let mut app = make_app_with_agent("sess-B");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.prompt.set_text("typed while thinking");
            seed_pending_tool(agent, "tc-draft", "CreatePlan");
        }

        let (tx, _rx) = tokio::sync::oneshot::channel();
        let ext_req = crate::views::plan_approval_view::ExitPlanModeExtRequest {
            session_id: "sess-B".into(),
            tool_call_id: "tc-draft".into(),
            plan_content: Some("# Plan\n".into()),
        };
        let raw = serde_json::value::to_raw_value(&ext_req).unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/exit_plan_mode", raw.into()),
                response_tx: tx,
            }),
            &mut app,
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.plan_approval_view.is_some());
        assert_eq!(agent.prompt.text(), "typed while thinking");
        assert_eq!(
            agent
                .plan_approval_view
                .as_ref()
                .map(|p| p.stashed_prompt.text.as_str()),
            Some("typed while thinking"),
        );
    }

    #[test]
    fn exit_plan_mode_with_permission_followup_keeps_real_session_draft() {
        let mut app = make_app_with_agent("sess-perm");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.permission_stashed_prompt =
                Some(crate::views::prompt_widget::StashedPrompt {
                    text: "real session draft".into(),
                    cursor: 0,
                    images: Vec::new(),
                    chip_elements: Vec::new(),
                    image_counter: 0,
                    image_undo_stash: Vec::new(),
                });
            // Non-empty queue means permission still owns the keyboard.
            agent.permission_queue.push_back(
                crate::app::agent_view::test_fixtures::make_followup_permission_state(),
            );
            agent.prompt.set_text("permission followup text");
            seed_pending_tool(agent, "tc-perm", "CreatePlan");
        }

        let (tx, _rx) = tokio::sync::oneshot::channel();
        let ext_req = crate::views::plan_approval_view::ExitPlanModeExtRequest {
            session_id: "sess-perm".into(),
            tool_call_id: "tc-perm".into(),
            plan_content: Some("# Plan\n".into()),
        };
        let raw = serde_json::value::to_raw_value(&ext_req).unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/exit_plan_mode", raw.into()),
                response_tx: tx,
            }),
            &mut app,
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent
                .plan_approval_view
                .as_ref()
                .map(|p| p.stashed_prompt.text.as_str()),
            Some("real session draft"),
        );
        assert_eq!(
            agent.prompt.text(),
            "",
            "live must stay empty while permission owns keys"
        );
        assert!(agent.permission_stashed_prompt.is_none());
        assert!(!agent.permission_queue.is_empty());
    }

    #[test]
    fn exit_plan_mode_prefills_image_chips_into_freeform() {
        let mut app = make_app_with_agent("sess-img");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.prompt.set_text("see ");
            let img = crate::prompt_images::PastedImage {
                element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0),
                display_number: 0,
                mime_type: "image/png".into(),
                dimensions: Some((100, 80)),
                byte_len: 2048,
                encoded_bytes: Some(vec![0u8; 16].into()),
                source_path: None,
                staged_temp_path: None,
                session_image_path: None,
                preview: crate::prompt_images::PromptImagePreview::default(),
            };
            agent.prompt.insert_image(img).expect("insert image");
            seed_pending_tool(agent, "tc-img", "CreatePlan");
        }

        let (tx, _rx) = tokio::sync::oneshot::channel();
        let ext_req = crate::views::plan_approval_view::ExitPlanModeExtRequest {
            session_id: "sess-img".into(),
            tool_call_id: "tc-img".into(),
            plan_content: Some("# Plan\n".into()),
        };
        let raw = serde_json::value::to_raw_value(&ext_req).unwrap();
        handle(
            AcpClientMessage::ExtMethod(echo_agent_acp::AcpArgs {
                request: acp::ExtRequest::new("echo.agent/exit_plan_mode", raw.into()),
                response_tx: tx,
            }),
            &mut app,
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent.prompt.text().contains("[Image #1]"),
            "freeform prefill must keep image chip text, got {:?}",
            agent.prompt.text()
        );
        assert_eq!(
            agent.prompt.images.len(),
            1,
            "freeform prefill must restore image payload"
        );
        assert_eq!(
            agent
                .plan_approval_view
                .as_ref()
                .map(|p| p.stashed_prompt.images.len()),
            Some(1),
            "session draft must retain its own image payload"
        );
    }

