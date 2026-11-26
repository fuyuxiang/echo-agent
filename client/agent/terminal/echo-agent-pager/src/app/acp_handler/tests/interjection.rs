#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    #[test]
    fn interjection_broadcast_mid_park_adds_no_marker() {
        use crate::app::agent_view::test_fixtures::{count_turn_markers, simulate_task_output_wait};

        let mut app = make_app_with_agent("sess-park");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.session.state = AgentState::TurnRunning;
            agent.session.current_prompt_id = Some("p1".into());
            insert_running_task(agent, "t10", "sleep 10");
            insert_running_task(agent, "t15", "sleep 15");
            simulate_task_output_wait(agent, "t15");
            assert!(agent.is_parked_on_sendable_wait());
            assert_eq!(count_turn_markers(agent), 0, "the park writes no row");
        }

        assert!(handle_ext_notification(
            &interjection_broadcast("sess-park", "queued follow-up"),
            &mut app,
        ));

        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            assert_eq!(
                last_interjection_text(&agent.scrollback).as_deref(),
                Some("queued follow-up"),
            );
            assert_eq!(
                count_turn_markers(agent),
                0,
                "no 'Worked for …' marker around the interjection"
            );
        }

        handle_ext_notification(
            &make_task_completed_notif("sess-park", "t10", "sleep 10", Some(0)),
            &mut app,
        );
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        assert_eq!(
            count_turn_markers(agent),
            0,
            "no 'Worked for …' tick under the interjection"
        );
        assert!(agent.renders_parked(), "the parked chrome stays on");
    }

    #[test]
    fn parked_completions_push_chips_without_markers() {
        use crate::app::agent_view::test_fixtures::{count_turn_markers, simulate_task_output_wait};

        let mut app = make_app_with_agent("sess-park");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.session.state = AgentState::TurnRunning;
            agent.session.current_prompt_id = Some("p1".into());
            insert_running_task(agent, "t10", "sleep 10");
            insert_running_task(agent, "t15", "sleep 15");
            insert_running_task(agent, "t20", "sleep 20");
            simulate_task_output_wait(agent, "t20");
            assert!(agent.renders_parked());
        }

        handle_ext_notification(
            &make_task_completed_notif("sess-park", "t10", "sleep 10", Some(0)),
            &mut app,
        );
        // Duplicate completion for the same task: not a Running→Done edge.
        handle_ext_notification(
            &make_task_completed_notif("sess-park", "t10", "sleep 10", Some(0)),
            &mut app,
        );
        handle_ext_notification(
            &make_task_completed_notif("sess-park", "t15", "sleep 15", Some(0)),
            &mut app,
        );
        handle_ext_notification(
            &make_task_completed_notif("sess-park", "t20", "sleep 20", Some(0)),
            &mut app,
        );

        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        assert_eq!(
            count_turn_markers(agent),
            0,
            "completions during a park never write a marker"
        );
        assert!(
            work_status_lines(&agent.scrollback).is_empty(),
            "no work-only status lines in the transcript"
        );
    }

    #[test]
    fn consecutive_subagent_finishes_stay_markerless() {
        use crate::app::agent_view::test_fixtures::count_turn_markers;

        let mut app = make_app_with_agent("sess-park");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            park_on_subagents(agent, &["child-1", "child-2", "child-3"]);
        }

        for child in ["child-1", "child-1", "child-2", "child-3"] {
            handle(
                make_ext_session_notification("sess-park", test_subagent_finished(child)),
                &mut app,
            );
        }
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        assert_eq!(
            count_turn_markers(agent),
            0,
            "subagent finishes never write a marker mid-park"
        );
    }

    #[test]
    fn repark_after_parent_output_stays_markerless() {
        use crate::acp::meta::NotificationMeta;
        use crate::app::agent_view::test_fixtures::{
            complete_task_output_wait_call, count_turn_markers, simulate_task_output_wait_call,
        };

        let mut app = make_app_with_agent("sess-park");
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.session.current_prompt_id = Some("p1".into());
        insert_running_task(agent, "t10", "sleep 10");

        simulate_task_output_wait_call(agent, "wait-1", "t10", 30_000);
        assert!(agent.renders_parked());
        assert_eq!(count_turn_markers(agent), 0);

        complete_task_output_wait_call(agent, "wait-1");
        assert!(agent.session.tracker.handle_update(
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                acp::ContentBlock::Text(acp::TextContent::new("between-parks content")),
            )),
            &NotificationMeta::default(),
            &mut agent.scrollback,
        ));

        simulate_task_output_wait_call(agent, "wait-2", "t10", 30_000);
        assert!(agent.renders_parked(), "the re-park renders parked again");
        assert_eq!(count_turn_markers(agent), 0, "and still writes no marker");
    }

    #[test]
    fn interjection_notification_pushes_block_to_matching_session() {
        // Multi-client fix: an interjection typed in one pane is broadcast by
        // the shell as echo.agent/session/interjection; EVERY attached pane (incl.
        // the originator, which no longer pushes a local block) renders it.
        let mut app = make_app_with_agent("sess-view");
        let affected =
            handle_ext_notification(&interjection_ext("sess-view", "also add tests"), &mut app);
        assert!(affected, "rendering into the active agent should redraw");

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            last_interjection_text(&agent.scrollback).as_deref(),
            Some("also add tests"),
            "the interjection block must be pushed from the broadcast"
        );
    }

    #[test]
    fn interjection_notification_for_unknown_session_is_ignored() {
        let mut app = make_app_with_agent("sess-view");
        let affected = handle_ext_notification(&interjection_ext("sess-other", "stray"), &mut app);
        assert!(!affected, "an unmatched session must be a no-op");

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            last_interjection_text(&agent.scrollback).is_none(),
            "no interjection block must be pushed for an unknown session"
        );
    }

    #[test]
    fn interjection_notification_renders_for_a_viewer() {
        // A viewer (attached_as_viewer) watching another client's session must
        // also render interjections broadcast for that session.
        let mut app = make_app_with_agent("sess-view");
        app.agents.get_mut(&AgentId(0)).unwrap().attached_as_viewer = true;
        let affected =
            handle_ext_notification(&interjection_ext("sess-view", "viewer sees this"), &mut app);
        assert!(affected);
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            last_interjection_text(&agent.scrollback).as_deref(),
            Some("viewer sees this"),
            "a viewer must render interjections broadcast for its session"
        );
    }

    #[test]
    fn interjection_notification_dedups_originators_own_echo() {
        // The originator rendered an optimistic block in dispatch_interject and
        // recorded the id; its own broadcast echo must be dropped (no dup) and
        // the id forgotten.
        let mut app = make_app_with_agent("sess-view");
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .self_interjection_ids
            .insert("ij-1".to_string());

        let affected = handle_ext_notification(
            &interjection_ext_with_id("sess-view", "my own", Some("ij-1")),
            &mut app,
        );
        assert!(
            !affected,
            "an originator's own echo must be a no-op (already rendered locally)"
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            last_interjection_text(&agent.scrollback).is_none(),
            "the echo must not push a duplicate block"
        );
        assert!(
            !agent.self_interjection_ids.contains("ij-1"),
            "the id must be forgotten after dedup"
        );
    }

    #[test]
    fn goal_send_now_notification_claims_optimistic_prompt_block() {
        let mut app = make_app_with_agent("sess-view");
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.note_self_originated_prompt("prompt-1");
        let entry_id = agent
            .scrollback
            .push_block(RenderBlock::user_prompt("steer the goal".to_string()));
        agent
            .send_now_painted_blocks
            .insert("prompt-1".to_string(), (entry_id, false));

        let affected = handle_ext_notification(
            &interjection_ext_with_id("sess-view", "steer the goal", Some("prompt-1")),
            &mut app,
        );
        assert!(!affected, "the optimistic block already represents the message");
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(!agent.send_now_painted_blocks.contains_key("prompt-1"));
        assert!(agent.expect_send_now_cancel.is_none());
        assert_eq!(last_interjection_text(&agent.scrollback).as_deref(), Some("steer the goal"));
    }

    #[test]
    fn interjection_notification_with_foreign_id_renders() {
        // A broadcast carrying an id this client did NOT mint (another pane's
        // interjection) must render — only the originator dedups by its own id.
        let mut app = make_app_with_agent("sess-view");
        let affected = handle_ext_notification(
            &interjection_ext_with_id("sess-view", "from another pane", Some("other-id")),
            &mut app,
        );
        assert!(affected);
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            last_interjection_text(&agent.scrollback).as_deref(),
            Some("from another pane"),
            "an interjection from another pane must render"
        );
    }
