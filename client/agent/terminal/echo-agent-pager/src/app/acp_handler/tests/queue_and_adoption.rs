#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    /// The pager reconciles the authoritative shared prompt queue from the
    /// `echo.agent/queue/changed` broadcast, and an empty broadcast clears it.
    #[test]
    fn queue_changed_reconciles_shared_queue() {
        let mut app = make_app_with_agent("sess-1");

        assert!(handle_ext_notification(
            &queue_changed_ext("sess-1", &["p1", "p2"]),
            &mut app
        ));
        let q = app.shared_prompt_queue("sess-1").expect("queue present");
        assert_eq!(
            q.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            vec!["p1", "p2"]
        );

        // A later broadcast fully replaces the previous queue.
        assert!(handle_ext_notification(
            &queue_changed_ext("sess-1", &["p2"]),
            &mut app
        ));
        let q = app.shared_prompt_queue("sess-1").expect("queue present");
        assert_eq!(
            q.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            vec!["p2"]
        );

        // An empty broadcast clears the entry.
        assert!(handle_ext_notification(
            &queue_changed_ext("sess-1", &[]),
            &mut app
        ));
        assert!(app.shared_prompt_queue("sess-1").is_none());
    }

    /// When a server-origin row being edited disappears from a later
    /// broadcast (drained / removed by another client), the queue handler
    /// exits `EditingQueued` so the composer isn't stranded.
    #[test]
    fn queue_changed_exits_editing_when_server_row_disappears() {
        use crate::app::agent_view::PromptMode;

        let mut app = make_app_with_agent("sess-1");

        // Establish the shared queue with p1 present, then put the agent in
        // EditingQueued{server_id: Some("p1")}.
        assert!(handle_ext_notification(
            &queue_changed_ext("sess-1", &["p1"]),
            &mut app
        ));
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.prompt_mode = PromptMode::EditingQueued {
                id: 42,
                original: "p1 text".into(),
                server_id: Some("p1".into()),
                kind: crate::app::agent::QueueEntryKind::Prompt,
            };
        }

        // A rebroadcast where p1 is gone (drained or removed elsewhere)
        // must cancel the editor.
        assert!(handle_ext_notification(
            &queue_changed_ext("sess-1", &[]),
            &mut app
        ));
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            matches!(agent.prompt_mode, PromptMode::Normal),
            "expected EditingQueued to be cleared, got {:?}",
            agent.prompt_mode
        );
    }

    /// When a server-origin row being edited is STILL in the broadcast,
    /// the editor is preserved (the row's contents may have changed —
    /// last-writer-wins handles that on the next submit).
    #[test]
    fn queue_changed_preserves_editing_when_server_row_still_present() {
        use crate::app::agent_view::PromptMode;

        let mut app = make_app_with_agent("sess-1");

        assert!(handle_ext_notification(
            &queue_changed_ext("sess-1", &["p1", "p2"]),
            &mut app
        ));
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.prompt_mode = PromptMode::EditingQueued {
                id: 42,
                original: "p1 text".into(),
                server_id: Some("p1".into()),
                kind: crate::app::agent::QueueEntryKind::Prompt,
            };
        }

        // Reorder/edit broadcast keeps p1 present.
        assert!(handle_ext_notification(
            &queue_changed_ext("sess-1", &["p1"]),
            &mut app
        ));
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            matches!(
                agent.prompt_mode,
                PromptMode::EditingQueued {
                    server_id: Some(_),
                    ..
                }
            ),
            "expected EditingQueued to persist while p1 is still in the queue, got {:?}",
            agent.prompt_mode
        );
    }

    /// `queue_changed_ext` with per-entry versions/kinds and a running id —
    /// for the optimistic-echo send-now race tests, which assert the
    /// interject fires with the row's authoritative (broadcast) version.
    fn queue_changed_versioned(
        session_id: &str,
        entries: &[(&str, u64, &str)],
        running: Option<&str>,
    ) -> acp::ExtNotification {
        let entries: Vec<serde_json::Value> = entries
            .iter()
            .enumerate()
            .map(|(i, (id, version, kind))| {
                serde_json::json!({
                    "id": id,
                    "version": version,
                    "owner": "A",
                    "kind": kind,
                    "text": format!("text {id}"),
                    "position": i,
                })
            })
            .collect();
        let mut params = serde_json::json!({ "sessionId": session_id, "entries": entries });
        if let Some(r) = running {
            params["runningPromptId"] = serde_json::json!(r);
        }
        acp::ExtNotification::new(
            "echo.agent/queue/changed",
            std::sync::Arc::from(serde_json::value::to_raw_value(&params).unwrap()),
        )
    }

    /// Arm the rapid double-Enter race: a bash command sent mid-turn (its
    /// server row still an optimistic echo) followed immediately by Enter on
    /// the empty composer. Returns the echo's prompt id after asserting the
    /// send-now was PARKED (not fired) against the unconfirmed row.
    fn park_send_now_on_optimistic_bash_row(app: &mut AppView) -> String {
        use crate::app::actions::{Action, Effect};
        use crate::app::app_view::InputOutcome;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.session.state = crate::app::agent::AgentState::TurnRunning;
            agent.session.current_prompt_id = Some("running-turn".into());
            agent.set_active_pane(crate::app::agent_view::ActivePane::Prompt, true);
        }
        // Enter #1: bash typed mid-turn goes server-authoritative.
        let effects =
            crate::app::dispatch::dispatch(Action::SendBashCommand("echo hi".into()), app);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::SendBashCommand { .. })),
            "mid-turn bash must send server-authoritatively; effects = {effects:?}"
        );
        let echo_id = app.agents[&AgentId(0)]
            .optimistic_queue_ids
            .iter()
            .next()
            .cloned()
            .expect("the echo id must be tracked as optimistic");

        // Enter #2 immediately (empty composer): the send-now must PARK —
        // firing the interject now would overtake the in-flight prompt RPC
        // shell-side and no-op, silently dropping the send-now.
        let outcome = app
            .agents
            .get_mut(&AgentId(0))
            .unwrap()
            .handle_prompt_key_for_test(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "the send-now against an unconfirmed row must park, got {outcome:?}"
        );
        assert_eq!(
            app.agents[&AgentId(0)].send_now_awaiting_confirm.as_deref(),
            Some(echo_id.as_str())
        );
        echo_id
    }

    /// Rapid double-Enter on a queued bash command: the parked send-now fires
    /// exactly when the confirming broadcast lands, carrying the row's
    /// authoritative version (firing it early no-opped
    /// shell-side, dropped the send-now, and the armed cancel expectation hid
    /// the still-queued row — the command looked like it disappeared).
    #[test]
    fn parked_send_now_fires_on_confirming_broadcast() {
        use crate::app::actions::Effect;

        let mut app = make_app_with_agent("sess-1");
        let echo_id = park_send_now_on_optimistic_bash_row(&mut app);
        assert!(
            app.pending_effects.is_empty(),
            "nothing may fire before the row is confirmed"
        );

        // The shell's confirming broadcast lists the row (version 3).
        assert!(handle_ext_notification(
            &queue_changed_versioned(
                "sess-1",
                &[(echo_id.as_str(), 3, "bash")],
                Some("running-turn"),
            ),
            &mut app,
        ));
        let agent = &app.agents[&AgentId(0)];
        assert!(agent.send_now_awaiting_confirm.is_none());
        assert!(
            agent.optimistic_queue_ids.is_empty(),
            "the broadcast confirms the echo"
        );
        assert_eq!(
            agent.expect_send_now_cancel.as_deref(),
            Some(echo_id.as_str()),
            "the fired send-now arms the cancel expectation"
        );
        assert!(
            app.pending_effects.iter().any(|e| matches!(
                e,
                Effect::QueueInterject { id, expected_version, new_text: None, .. }
                    if *id == echo_id && *expected_version == 3
            )),
            "the parked send-now must fire with the authoritative version; effects = {:?}",
            app.pending_effects
        );
    }

    /// The natural drain wins the race: the parked row is confirmed directly
    /// as the RUNNING turn — nothing to promote, the park just clears.
    #[test]
    fn parked_send_now_clears_when_row_confirmed_running() {
        use crate::app::actions::Effect;

        let mut app = make_app_with_agent("sess-1");
        let echo_id = park_send_now_on_optimistic_bash_row(&mut app);

        assert!(handle_ext_notification(
            &queue_changed_versioned("sess-1", &[], Some(echo_id.as_str())),
            &mut app,
        ));
        let agent = &app.agents[&AgentId(0)];
        assert!(agent.send_now_awaiting_confirm.is_none());
        assert!(
            !app.pending_effects
                .iter()
                .any(|e| matches!(e, Effect::QueueInterject { .. })),
            "an already-running row must not be interjected; effects = {:?}",
            app.pending_effects
        );
    }

    /// A broadcast that does not list the parked row (its RPC still in
    /// flight) keeps the park armed; the later confirming broadcast fires it.
    #[test]
    fn parked_send_now_survives_unrelated_broadcast() {
        use crate::app::actions::Effect;

        let mut app = make_app_with_agent("sess-1");
        let echo_id = park_send_now_on_optimistic_bash_row(&mut app);

        // Unrelated broadcast (another client's row): the park must survive.
        assert!(handle_ext_notification(
            &queue_changed_versioned(
                "sess-1",
                &[("other-row", 1, "prompt")],
                Some("running-turn")
            ),
            &mut app,
        ));
        assert_eq!(
            app.agents[&AgentId(0)].send_now_awaiting_confirm.as_deref(),
            Some(echo_id.as_str()),
            "an unconfirmed park must survive unrelated broadcasts"
        );
        assert!(
            !app.pending_effects
                .iter()
                .any(|e| matches!(e, Effect::QueueInterject { .. }))
        );

        // The row's own confirmation still fires it.
        assert!(handle_ext_notification(
            &queue_changed_versioned(
                "sess-1",
                &[("other-row", 1, "prompt"), (echo_id.as_str(), 1, "bash")],
                Some("running-turn"),
            ),
            &mut app,
        ));
        assert!(app.agents[&AgentId(0)].send_now_awaiting_confirm.is_none());
        assert!(
            app.pending_effects.iter().any(|e| matches!(
                e,
                Effect::QueueInterject { id, expected_version, .. }
                    if *id == echo_id && *expected_version == 1
            ))
        );
    }

    /// Correlation wiring: `handle_queue_changed` adopts `current_prompt_id` from
    /// `runningPromptId` only when it was `None`, never overriding an already-set one.
    #[test]
    fn queue_changed_adopts_running_prompt_id_only_when_unset() {
        // Shell and pager share the echo-agent-prompt-queue type, so serializing the payload as the
        // shell emits it pins the wire shape the handler consumes, not cross-crate compat.
        let shell_payload = echo_agent_runtime::session::prompt_queue::QueueChanged {
            session_id: "sess-1".to_string(),
            entries: Vec::new(),
            running_prompt_id: Some("prompt-running".to_string()),
            running_text: None,
            running_kind: None,
            running_combined_texts: None,
        };
        let raw = serde_json::value::to_raw_value(&shell_payload).unwrap();
        let json_str = raw.get().to_string();
        assert!(json_str.contains("runningPromptId"));
        let mirror: crate::app::prompt_queue::QueueChanged =
            serde_json::from_str(&json_str).unwrap();
        assert_eq!(mirror.running_prompt_id.as_deref(), Some("prompt-running"));

        let notif = acp::ExtNotification::new("echo.agent/queue/changed", raw.into());

        // Case 1: current_prompt_id is None -> adopt it.
        let mut app = make_app_with_agent("sess-1");
        assert!(
            app.agents
                .get(&AgentId(0))
                .unwrap()
                .session
                .current_prompt_id
                .is_none()
        );
        assert!(handle_queue_changed(&notif, &mut app));
        assert_eq!(
            app.agents
                .get(&AgentId(0))
                .unwrap()
                .session
                .current_prompt_id
                .as_deref(),
            Some("prompt-running"),
            "adopts running_prompt_id when current_prompt_id was None"
        );

        // Case 2: current_prompt_id already set (the single-client drip-feed
        // case) -> must NOT be overridden, behaviorally inert.
        let mut app = make_app_with_agent("sess-1");
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .session
            .current_prompt_id = Some("local-already".to_string());
        assert!(handle_queue_changed(&notif, &mut app));
        assert_eq!(
            app.agents
                .get(&AgentId(0))
                .unwrap()
                .session
                .current_prompt_id
                .as_deref(),
            Some("local-already"),
            "never overrides an already-set current_prompt_id"
        );
    }

    /// When nothing is running locally (`current_prompt_id == None`), a
    /// `running_prompt_id` broadcast is adopted directly and the turn-start
    /// shim renders the queued prompt's user block + sets `TurnRunning`.
    #[test]
    fn running_prompt_adopted_directly_with_turn_start_shim_when_idle() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        // A prompt the pager sent server-authoritatively, echoed in the queue.
        app.push_optimistic_prompt_echo("sess-1", "p2", "second prompt", "prompt");
        let scroll_before = app.agents[&id].scrollback.len();
        assert!(app.agents[&id].session.current_prompt_id.is_none());

        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], Some("p2")),
            &mut app
        ));

        let agent = app.agents.get(&id).unwrap();
        assert_eq!(agent.session.current_prompt_id.as_deref(), Some("p2"));
        assert!(
            agent.session.state.is_turn_running(),
            "turn-start shim must set TurnRunning"
        );
        assert!(
            agent.scrollback.len() > scroll_before,
            "shim must render the queued prompt's user block"
        );
        // The running item left the shared queue.
        assert!(app.shared_prompt_queue("sess-1").is_none());
    }

    #[test]
    fn running_combined_texts_paints_one_bubble_per_segment() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        let before = app.agents[&id].scrollback.len();
        assert!(handle_queue_changed(
            &queue_changed_running_ex(
                "sess-1",
                &[],
                Some("p-combo"),
                Some("first\n\nsecond"),
                Some("prompt"),
                Some(&["first", "second"]),
            ),
            &mut app,
        ));
        let agent = app.agents.get(&id).unwrap();
        assert_eq!(agent.session.current_prompt_id.as_deref(), Some("p-combo"));
        assert_eq!(agent.scrollback.len(), before + 2);
        let texts: Vec<_> = (0..agent.scrollback.len())
            .filter_map(|i| agent.scrollback.entry(i))
            .filter_map(|e| match &e.block {
                RenderBlock::UserPrompt(ub) => Some(ub.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["first", "second"]);
    }

    #[test]
    fn running_text_without_segments_paints_single_bubble() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        let before = app.agents[&id].scrollback.len();
        assert!(handle_queue_changed(
            &queue_changed_running_ex(
                "sess-1",
                &[],
                Some("p-join"),
                Some("first\n\nsecond"),
                Some("prompt"),
                None,
            ),
            &mut app,
        ));
        let agent = app.agents.get(&id).unwrap();
        assert_eq!(agent.scrollback.len(), before + 1);
        match &agent.scrollback.entry(before).unwrap().block {
            RenderBlock::UserPrompt(ub) => assert_eq!(ub.text, "first\n\nsecond"),
            other => panic!("expected user prompt, got {other:?}"),
        }
    }

    /// Regression: when the shell promotes
    /// a server-initiated / auto-wake prompt (synthetic id `task-completed-…`,
    /// injected when a background task finishes) to the running turn, it
    /// broadcasts `echo.agent/queue/changed` with `runningPromptId` = that synthetic
    /// id. The pager must NOT adopt it via the turn-start shim: those turns run
    /// inside the actor and emit no `prompt_complete` / `PromptResponse`, so
    /// `start_turn()` here would strand the pager on "Responding…" forever
    /// (the exact reported bug — a background task finishing left the spinner
    /// running indefinitely). It must stay Idle (content still renders via the
    /// live-delta path, which no longer claims `current_prompt_id` for a driver).
    #[test]
    fn queue_changed_does_not_adopt_server_initiated_running_prompt() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        assert!(app.agents[&id].session.current_prompt_id.is_none());

        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], Some("task-completed-bg-123")),
            &mut app
        ));

        let agent = app.agents.get(&id).unwrap();
        assert!(
            matches!(agent.session.state, AgentState::Idle),
            "auto-wake running prompt must NOT enter TurnRunning (no prompt_complete \
             would ever finish it → permanent stuck 'Responding…' spinner)"
        );
        assert!(
            agent.session.current_prompt_id.is_none(),
            "the shim must not run, so current_prompt_id stays unset here"
        );
        assert!(
            !app.pending_running_adoptions.contains_key(&id),
            "a server-initiated running prompt must not be stashed for later adoption"
        );
    }

    /// Cron (`scheduler-fired-…`) is a synthetic id but is client-driven via
    /// `MvpAgent::prompt()` and DOES emit `prompt_complete`, so the queue-changed
    /// adoption must STILL fire for it (the exit exists, so it won't strand).
    /// This is the counterpart to the auto-wake skip above.
    #[test]
    fn queue_changed_adopts_scheduler_fired_running_prompt() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        assert!(app.agents[&id].session.current_prompt_id.is_none());

        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], Some("scheduler-fired-task-1")),
            &mut app
        ));

        let agent = app.agents.get(&id).unwrap();
        assert_eq!(
            agent.session.current_prompt_id.as_deref(),
            Some("scheduler-fired-task-1"),
            "cron turn is client-driven (has a prompt_complete exit) → adopt it"
        );
        assert!(
            agent.session.state.is_turn_running(),
            "cron turn must enter TurnRunning so its running chrome shows"
        );
    }

    /// Regression (queue shim terminal-in-replay): the
    /// terminal-aware adoption must hold on the `queue/changed` turn-start path,
    /// not only the load paths. After a load records a finished turn in
    /// `replayed_terminal_prompts`, a later `queue/changed` re-reporting that
    /// same (already-ended) `runningPromptId` must NOT re-adopt it. The pid is
    /// user-driven (so it passes the pid-only synthetic guard) — only the
    /// agent-aware `should_adopt_running_prompt`, which consults
    /// `replayed_terminal_prompts`, stops the viewer re-stranding on "Waiting…",
    /// mirroring the `SessionLoaded` / reconnect adoption.
    #[test]
    fn queue_changed_does_not_readopt_terminal_in_replay_running_prompt() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);

        // A load's replay records the running turn's durable terminal; the load
        // then completes (loading_replay clears, the terminal set persists until
        // the next replay window).
        app.agents.get_mut(&id).unwrap().session.loading_replay = true;
        let _ = handle_ext_notification(
            &echo_agent_turn_completed_notif("sess-1", "p-run", "end_turn", true),
            &mut app,
        );
        app.agents.get_mut(&id).unwrap().session.loading_replay = false;
        assert!(app.agents[&id].replayed_terminal_prompts.contains("p-run"));
        assert!(app.agents[&id].session.current_prompt_id.is_none());

        // The leader re-reports the already-ended pid as running (stale slot).
        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], Some("p-run")),
            &mut app
        ));

        let agent = app.agents.get(&id).unwrap();
        assert!(
            agent.session.current_prompt_id.is_none(),
            "a terminal-in-replay running prompt must NOT be re-adopted by the queue shim"
        );
        assert!(
            matches!(agent.session.state, AgentState::Idle),
            "re-adopting an already-ended turn would re-strand the viewer on Waiting…"
        );
        assert!(
            !app.pending_running_adoptions.contains_key(&id),
            "and it must not be stashed for later adoption"
        );
    }

    /// No regression: a running prompt NOT in the replay terminal set is still
    /// adopted by the queue shim (the agent-aware guard is precise — it blocks
    /// only ended turns, not normal queue-handoff adoption).
    #[test]
    fn queue_changed_still_adopts_running_prompt_not_terminal_in_replay() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        // A DIFFERENT turn's terminal is recorded in replay.
        app.agents.get_mut(&id).unwrap().session.loading_replay = true;
        let _ = handle_ext_notification(
            &echo_agent_turn_completed_notif("sess-1", "p-old", "end_turn", true),
            &mut app,
        );
        app.agents.get_mut(&id).unwrap().session.loading_replay = false;

        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], Some("p-run")),
            &mut app
        ));

        let agent = app.agents.get(&id).unwrap();
        assert_eq!(
            agent.session.current_prompt_id.as_deref(),
            Some("p-run"),
            "a running prompt with no terminal-in-replay must still be adopted"
        );
        assert!(agent.session.state.is_turn_running());
    }

    /// Regression (live-delta path): the auto-wake turn streams its reply as a
    /// synthetic-promptId `session/update`. On a driver (not a viewer) the
    /// content must render, but the turn must NOT be claimed — no role flip, no
    /// `current_prompt_id`, no `TurnRunning` — otherwise it strands the
    /// turn-status and poisons the slot so later real turns' PromptResponses get
    /// discarded. (Pairs with the `handle_queue_changed` skip above: together
    /// they keep synthetic auto-wake turns out of the running slot on BOTH the
    /// queue-broadcast and the streaming-delta paths.)
    #[test]
    fn synthetic_auto_wake_delta_renders_without_claiming_turn() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);

        let _ = handle(
            make_agent_chunk_message_with_prompt(
                "sess-1",
                "background task finished on its own",
                "task-completed-bg1",
                false,
            ),
            &mut app,
        );

        let agent = app.agents.get(&id).unwrap();
        assert_eq!(
            agent_message_text(agent),
            "background task finished on its own",
            "auto-wake content must still render"
        );
        assert!(matches!(agent.session.state, AgentState::Idle));
        assert!(agent.session.current_prompt_id.is_none());
        assert!(!agent.attached_as_viewer);
    }

    /// FIFO handoff: the leader's running broadcast for the next prompt
    /// can arrive BEFORE the previous turn's PromptResponse. The pager must NOT
    /// corrupt the in-flight turn — it stashes the adoption and applies it once
    /// PromptResponse clears `current_prompt_id`.
    #[test]
    fn running_prompt_stashed_during_handoff_then_adopted_on_prompt_response() {
        use crate::app::dispatch::dispatch;
        use crate::app::actions::{Action, Effect, TaskResult};

        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);

        // p1: idle submit → drains locally, turn running.
        let effects = dispatch(Action::SendPrompt("first".into()), &mut app);
        let pid_first = match &effects[0] {
            Effect::SendPrompt { prompt_id, .. } => prompt_id.clone(),
            other => panic!("expected SendPrompt, got {other:?}"),
        };
        assert_eq!(
            app.agents[&id].session.current_prompt_id.as_deref(),
            Some(pid_first.as_str())
        );

        // p2: running submit → immediate server send + optimistic echo.
        let effects = dispatch(Action::SendPrompt("second".into()), &mut app);
        let pid_second = match &effects[0] {
            Effect::SendPrompt {
                text, prompt_id, ..
            } => {
                assert_eq!(text, "second");
                prompt_id.clone()
            }
            other => panic!("expected immediate SendPrompt, got {other:?}"),
        };
        assert_eq!(app.agents[&id].session.queue_len(), 0);

        // Leader drains p2 and broadcasts running=p2 BEFORE p1's PromptResponse.
        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], Some(&pid_second)),
            &mut app
        ));
        // The in-flight turn (p1) is NOT corrupted; p2 is stashed.
        assert_eq!(
            app.agents[&id].session.current_prompt_id.as_deref(),
            Some(pid_first.as_str()),
            "must not adopt while a different prompt is still running"
        );
        assert!(app.pending_running_adoptions.contains_key(&id));

        // p1's PromptResponse → finish_turn clears current → adopt p2 (shim).
        let scroll_before = app.agents[&id].scrollback.len();
        let effects = dispatch(
            Action::TaskComplete(TaskResult::PromptResponse {
                agent_id: id,
                result: Ok(acp::PromptResponse::new(acp::StopReason::EndTurn)),
                http_status: None,
                prompt_id: None,
            }),
            &mut app,
        );
        assert!(app.agents[&id].session.state.is_turn_running());
        assert_eq!(
            app.agents[&id].session.current_prompt_id.as_deref(),
            Some(pid_second.as_str()),
            "current_prompt_id handed off to the next running prompt"
        );
        assert!(app.pending_running_adoptions.is_empty());
        assert!(
            app.agents[&id].scrollback.len() > scroll_before,
            "shim renders p2's user block on adoption"
        );
        // No re-send — p2 was already sent at enqueue time.
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SendPrompt { .. })),
            "must not re-send the already-sent prompt"
        );
    }

    /// Regression: in the FIFO handoff the leader's user-echo (no `promptId`)
    /// arrives before the deferred turn-start shim arms `expect_user_echo`, so
    /// without arming it at stash time the 2nd queued prompt renders twice.
    #[test]
    fn fifo_handoff_user_echo_not_duplicated() {
        use crate::app::dispatch::dispatch;
        use crate::app::actions::{Action, Effect, TaskResult};

        // A user-echo as the leader emits it: a text block, meta with no `promptId`.
        fn user_echo(app: &mut AppView, text: &str) {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let request = acp::SessionNotification::new(
                acp::SessionId::new("sess-1"),
                acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new(text)),
                )),
            )
            .meta(
                serde_json::json!({ "agentTimestampMs": 1 })
                    .as_object()
                    .cloned(),
            );
            handle(
                AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
                    request,
                    response_tx: tx,
                }),
                app,
            );
        }

        fn user_block_count(app: &AppView, id: AgentId, text: &str) -> usize {
            let agent = app.agents.get(&id).unwrap();
            agent
                .scrollback
                .entries_in_range(0..agent.scrollback.len())
                .iter()
                .filter(|e| matches!(&e.block, RenderBlock::UserPrompt(b) if b.text == text))
                .count()
        }

        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);

        // p1 from idle; its echo consumes the armed skip, so the 2nd handoff
        // starts with skip == false (the condition that triggered the dup).
        let _ = dispatch(Action::SendPrompt("first".into()), &mut app);
        user_echo(&mut app, "first");
        assert_eq!(user_block_count(&app, id, "first"), 1);

        // p2 submitted while running → immediate server send.
        let effects = dispatch(Action::SendPrompt("second".into()), &mut app);
        let pid_second = match &effects[0] {
            Effect::SendPrompt { prompt_id, .. } => prompt_id.clone(),
            other => panic!("expected immediate SendPrompt, got {other:?}"),
        };

        // Leader broadcasts running=p2 before p1's PromptResponse, then p2's echo.
        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], Some(&pid_second)),
            &mut app
        ));
        user_echo(&mut app, "second");

        assert_eq!(
            user_block_count(&app, id, "second"),
            0,
            "FIFO-handoff user-echo must be swallowed, not rendered as a duplicate"
        );

        // p1's PromptResponse → finish_turn + the deferred shim renders p2 once.
        dispatch(
            Action::TaskComplete(TaskResult::PromptResponse {
                agent_id: id,
                result: Ok(acp::PromptResponse::new(acp::StopReason::EndTurn)),
                http_status: None,
                prompt_id: None,
            }),
            &mut app,
        );

        assert_eq!(
            user_block_count(&app, id, "second"),
            1,
            "exactly one p2 user block after handoff (no duplicate)"
        );
        assert_eq!(
            app.agents[&id].session.current_prompt_id.as_deref(),
            Some(pid_second.as_str()),
        );
    }

    /// Regression (viewer scrollback): when a queued prompt drains via the
    /// FIFO-handoff race on a VIEWER (a client watching another client's turn),
    /// this client never runs the deferred turn-start shim for it — a viewer's
    /// turn-end is `prompt_complete`, which does not apply the stashed adoption
    /// (only the driver's `PromptResponse` does). Arming `expect_user_echo` at
    /// stash time would then swallow the leader's authoritative user-echo and the
    /// drained prompt's user block would vanish from the viewer's scrollback
    /// entirely. The stash branch must arm the echo-skip ONLY when this client
    /// drives the current turn (`!attached_as_viewer`); a viewer must let the
    /// server echo render the block. Counterpart to the driver-side
    /// `fifo_handoff_user_echo_not_duplicated` above.
    #[test]
    fn viewer_drained_prompt_renders_user_block_from_echo() {
        // Live user-echo as the leader emits it: a text block, no `promptId`.
        fn user_echo(app: &mut AppView, text: &str) {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let request = acp::SessionNotification::new(
                acp::SessionId::new("sess-1"),
                acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new(text)),
                )),
            )
            .meta(
                serde_json::json!({ "agentTimestampMs": 1 })
                    .as_object()
                    .cloned(),
            );
            handle(
                AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
                    request,
                    response_tx: tx,
                }),
                app,
            );
        }

        fn user_block_count(app: &AppView, id: AgentId, text: &str) -> usize {
            let agent = app.agents.get(&id).unwrap();
            agent
                .scrollback
                .entries_in_range(0..agent.scrollback.len())
                .iter()
                .filter(|e| matches!(&e.block, RenderBlock::UserPrompt(b) if b.text == text))
                .count()
        }

        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);

        // This client is a VIEWER: it watches another client's running turn
        // (`p1`) and has NOT originated the queued prompt `p2`.
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.attached_as_viewer = true;
            agent.session.current_prompt_id = Some("p1".to_string());
        }

        // Viewer receives the shared queue: `p1` running, `p2` queued. The
        // broadcast carries `p2`'s text ("text p2") into the shared queue.
        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &["p2"], Some("p1")),
            &mut app
        ));

        // `p1` finishes and the leader promotes `p2`: running=p2, p2 removed
        // from the queue. The viewer's `current_prompt_id` is still `p1` (its
        // `prompt_complete` for p1 has not been processed yet) → FIFO-handoff
        // STASH branch, where the old code unconditionally armed the echo-skip.
        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], Some("p2")),
            &mut app
        ));

        // The leader emits p2's authoritative user-echo (no `promptId`).
        user_echo(&mut app, "text p2");

        // The viewer must render p2's user block from that echo exactly once.
        // Before the fix the stash branch armed `expect_user_echo`, swallowing
        // the echo, so the count was 0 and the prompt vanished from scrollback.
        assert_eq!(
            user_block_count(&app, id, "text p2"),
            1,
            "viewer must render a stash-drained prompt's user block from the server echo"
        );
    }

    /// Regression: a client can be `attached_as_viewer`
    /// on ANOTHER client's turn yet immediate-send (self-originate) a queued
    /// prompt of its own. When that prompt drains via the FIFO-handoff race, the
    /// viewer still won't run the deferred shim (it ends the viewed turn via
    /// `prompt_complete`, which clears the stash), so the echo-skip guard must
    /// NOT key on origination — keying on `is_self_originated` here armed the
    /// skip and dropped the block. Keying on `!attached_as_viewer` keeps the echo
    /// and renders the block. This sets `current_prompt_id` to another client's
    /// turn AND marks the drained prompt self-originated, the exact combination
    /// the prior guard mishandled.
    #[test]
    fn viewer_self_originated_drained_prompt_renders_from_echo() {
        fn user_echo(app: &mut AppView, text: &str) {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let request = acp::SessionNotification::new(
                acp::SessionId::new("sess-1"),
                acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new(text)),
                )),
            )
            .meta(
                serde_json::json!({ "agentTimestampMs": 1 })
                    .as_object()
                    .cloned(),
            );
            handle(
                AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
                    request,
                    response_tx: tx,
                }),
                app,
            );
        }
        fn user_block_count(app: &AppView, id: AgentId, text: &str) -> usize {
            let agent = app.agents.get(&id).unwrap();
            agent
                .scrollback
                .entries_in_range(0..agent.scrollback.len())
                .iter()
                .filter(|e| matches!(&e.block, RenderBlock::UserPrompt(b) if b.text == text))
                .count()
        }

        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);

        // VIEWER of another client's turn `p1`, but it DID originate `p2` (typed
        // it via immediate-send while still viewing). The old guard keyed on
        // origination and wrongly armed the echo-skip.
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.attached_as_viewer = true;
            agent.session.current_prompt_id = Some("p1".to_string());
            agent.note_self_originated_prompt("p2");
        }

        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &["p2"], Some("p1")),
            &mut app
        ));
        // FIFO handoff: p2 promoted while current is still the viewed `p1`.
        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], Some("p2")),
            &mut app
        ));
        user_echo(&mut app, "text p2");

        assert_eq!(
            user_block_count(&app, id, "text p2"),
            1,
            "self-originated prompt drained on a viewer must still render its user block from the echo"
        );
    }

    /// Ordering: the `running_prompt_id` broadcast must be
    /// adopted BEFORE the turn's first `session/update`, otherwise the gate
    /// drops chunks whose `promptId != current_prompt_id`. This proves that
    /// once adoption set `current_prompt_id`, a chunk carrying that `promptId`
    /// renders into scrollback.
    #[test]
    fn adopted_running_prompt_lets_subsequent_chunk_through_gate() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        app.push_optimistic_prompt_echo("sess-1", "p2", "hello", "prompt");

        // Broadcast adopts p2 (idle → shim sets current_prompt_id = p2).
        handle_queue_changed(&queue_changed_running("sess-1", &[], Some("p2")), &mut app);
        assert_eq!(
            app.agents[&id].session.current_prompt_id.as_deref(),
            Some("p2")
        );

        // A streaming chunk stamped with promptId=p2 now passes the gate.
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = acp::SessionNotification::new(
            acp::SessionId::new("sess-1"),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("streamed body"),
            ))),
        )
        .meta(serde_json::json!({ "promptId": "p2" }).as_object().cloned());
        let msg = AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
            request,
            response_tx: tx,
        });
        handle(msg, &mut app);

        assert_eq!(
            agent_message_text(app.agents.get(&id).unwrap()),
            "streamed body",
            "chunk for the adopted running prompt must render (not dropped by the gate)"
        );
    }

    /// Multi-client mid-turn load (leader mode): a client that opens a session
    /// while ANOTHER client is driving an in-flight turn subscribes AFTER the
    /// turn-start `queue/changed`, so it never adopts via `handle_queue_changed`
    /// and its `current_prompt_id` stays `None`. The server conveys the running
    /// prompt id in the `SessionLoaded` response meta; the loader must adopt it
    /// (set `current_prompt_id` + enter `TurnRunning`) WITHOUT re-rendering the
    /// user block (replay already rendered it), so subsequent live
    /// `session/update` deltas for that prompt pass the gate and render live.
    #[test]
    fn session_loaded_with_running_prompt_id_adopts_and_passes_live_gate() {
        use crate::app::dispatch::dispatch;
        use crate::app::actions::{Action, TaskResult};

        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        // A viewer that loaded the session mid-turn has no local prompt id.
        assert!(app.agents[&id].session.current_prompt_id.is_none());
        let scroll_before = app.agents[&id].scrollback.len();

        dispatch(
            Action::TaskComplete(TaskResult::SessionLoaded {
                agent_id: id,
                session_id: acp::SessionId::new("sess-1"),
                models: None,
                code_restored: false,
                restore_summary: None,
                restore_degree: None,
                running_prompt_id: Some("p-run".to_string()),
                scheduler_background_loops: None,
            }),
            &mut app,
        );

        // Adopted the in-flight prompt id + entered the turn-running state.
        assert_eq!(
            app.agents[&id].session.current_prompt_id.as_deref(),
            Some("p-run"),
            "loader must adopt the server-conveyed running prompt id"
        );
        assert!(
            app.agents[&id].session.state.is_turn_running(),
            "loading mid-turn must enter TurnRunning so the spinner shows"
        );
        // No user-prompt block pushed: the in-flight turn's user prompt arrived
        // via the `session/load` replay; adoption must not duplicate it.
        let user_blocks = app.agents[&id]
            .scrollback
            .entries_in_range(0..app.agents[&id].scrollback.len())
            .iter()
            .filter(|e| matches!(&e.block, RenderBlock::UserPrompt(_)))
            .count();
        assert_eq!(
            user_blocks, 0,
            "adoption-on-load must NOT render a duplicate user-prompt block"
        );
        assert_eq!(
            app.agents[&id].scrollback.len(),
            scroll_before,
            "adoption-on-load must not grow the scrollback"
        );

        // A live (non-replay) chunk stamped with the adopted prompt id now
        // passes the gate and renders (previously dropped → the viewer froze).
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = acp::SessionNotification::new(
            acp::SessionId::new("sess-1"),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("live delta from driver"),
            ))),
        )
        .meta(
            serde_json::json!({ "promptId": "p-run" })
                .as_object()
                .cloned(),
        );
        handle(
            AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
                request,
                response_tx: tx,
            }),
            &mut app,
        );

        assert_eq!(
            agent_message_text(app.agents.get(&id).unwrap()),
            "live delta from driver",
            "live delta for the adopted in-flight prompt must render, not be dropped by the gate"
        );
    }

    /// The adoption predicate: synthetic non-scheduler turns (auto-wake /
    /// subagent-completion / notification-drain / goal turns) emit no
    /// `prompt_complete`, so a re-attach must NOT adopt them (adoption would
    /// strand the viewer in `TurnRunning`). Scheduler-fired (`/loop`) turns are
    /// synthetic but client-driven with a real `prompt_complete`, and plain user
    /// turns are always adoptable.
    #[test]
    fn should_adopt_running_prompt_skips_synthetic_non_scheduler() {
        assert!(should_adopt_running_prompt("p-user"));
        assert!(should_adopt_running_prompt(
            "scheduler-fired-019e51a3-abcd-1234"
        ));
        assert!(!should_adopt_running_prompt("task-completed-abc-123"));
        assert!(!should_adopt_running_prompt("subagent-completed-xyz-789"));
        assert!(!should_adopt_running_prompt(
            "notifications-019e0000-0000-7000-8000-0000000000aa"
        ));
        assert!(!should_adopt_running_prompt("goal-summary-019e2d3e"));
        assert!(!should_adopt_running_prompt(
            "goal-classifier-nudge-019e2d3e"
        ));
    }

    /// A `SessionLoaded` conveying a SYNTHETIC non-scheduler `running_prompt_id`
    /// (auto-wake / subagent-completion) must NOT be adopted: those actor-run
    /// turns emit no `prompt_complete`, so adopting one would strand the viewer
    /// in `TurnRunning` forever. The agent stays `Idle`, and the preserve arg is
    /// gated on the same predicate so the running turn's buffered follow-up chips
    /// are NOT preserved (adoption — their only flusher — is skipped, so
    /// preserving would orphan them). The ADOPTABLE-id preserve contrast is
    /// covered by `reload_preserves_running_turn_follow_ups_and_renders_on_adoption`.
    #[test]
    fn session_loaded_with_synthetic_running_prompt_id_stays_idle() {
        use crate::app::dispatch::dispatch;
        use crate::app::actions::{Action, TaskResult};

        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);

        // Seed the running turn's buffered follow-up chips keyed by the synthetic
        // prompt id, as if they had arrived on the ext channel during replay.
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.follow_up_pending.insert(
                "task-completed-abc-123".to_string(),
                crate::app::agent_view::FollowUps {
                    response_id: "resp-syn".into(),
                    suggestions: vec!["go".into()],
                },
            );
            agent
                .follow_up_pending_order
                .push_back("task-completed-abc-123".to_string());
        }

        dispatch(
            Action::TaskComplete(TaskResult::SessionLoaded {
                agent_id: id,
                session_id: acp::SessionId::new("sess-1"),
                models: None,
                code_restored: false,
                restore_summary: None,
                restore_degree: None,
                running_prompt_id: Some("task-completed-abc-123".to_string()),
                scheduler_background_loops: None,
            }),
            &mut app,
        );

        assert!(
            app.agents[&id].session.current_prompt_id.is_none(),
            "synthetic non-scheduler running prompt must not be adopted on load"
        );
        assert!(
            app.agents[&id].session.state.is_idle(),
            "adopting a synthetic actor-run turn would strand the viewer in TurnRunning"
        );
        assert!(
            app.agents[&id].follow_up_pending.is_empty(),
            "a synthetic id's buffered follow-ups must be dropped, not preserved"
        );
    }

    /// A `SessionLoaded` conveying a SCHEDULER-FIRED (`/loop`) `running_prompt_id`
    /// IS adopted: cron turns are synthetic but client-driven and DO emit a
    /// `prompt_complete`, so the viewer can safely enter `TurnRunning` (the
    /// dashboard shows a running `/loop` session as Working).
    #[test]
    fn session_loaded_with_scheduler_fired_running_prompt_id_adopts() {
        use crate::app::dispatch::dispatch;
        use crate::app::actions::{Action, TaskResult};

        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        let pid = "scheduler-fired-019e51a3-abcd-1234";

        dispatch(
            Action::TaskComplete(TaskResult::SessionLoaded {
                agent_id: id,
                session_id: acp::SessionId::new("sess-1"),
                models: None,
                code_restored: false,
                restore_summary: None,
                restore_degree: None,
                running_prompt_id: Some(pid.to_string()),
                scheduler_background_loops: None,
            }),
            &mut app,
        );

        assert_eq!(
            app.agents[&id].session.current_prompt_id.as_deref(),
            Some(pid),
            "scheduler-fired turns must still be adopted on load"
        );
        assert!(
            app.agents[&id].session.state.is_turn_running(),
            "an adopted /loop turn must enter TurnRunning so the dashboard shows Working"
        );
    }

    /// The session-load running-turn adopt is a production turn-start
    /// clear site — adopting an in-flight turn on load drops prior chips.
    #[test]
    fn session_loaded_adopting_running_turn_clears_follow_up_chips() {
        use crate::app::dispatch::dispatch;
        use crate::app::actions::{Action, TaskResult};

        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        app.agents
            .get_mut(&id)
            .unwrap()
            .apply_follow_ups("resp-1".into(), vec!["a".into()]);
        dispatch(
            Action::TaskComplete(TaskResult::SessionLoaded {
                agent_id: id,
                session_id: acp::SessionId::new("sess-1"),
                models: None,
                code_restored: false,
                restore_summary: None,
                restore_degree: None,
                running_prompt_id: Some("p-run".to_string()),
                scheduler_background_loops: None,
            }),
            &mut app,
        );
        assert!(
            app.agents[&id].follow_ups.is_none(),
            "adopting a running turn on session-load must clear prior chips"
        );
    }

    /// Idle reload (no running prompt) must ALSO drop prior chips and fully
    /// reset the seen map/generation — follow-ups are streaming-only and never
    /// persist across a reload, so stale state must not survive.
    #[test]
    fn session_loaded_idle_clears_follow_up_chips_and_seen() {
        use crate::app::dispatch::dispatch;
        use crate::app::actions::{Action, TaskResult};

        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.apply_follow_ups("resp-1".into(), vec!["a".into()]);
            agent.follow_up_chips = vec![ratatui::layout::Rect::new(0, 0, 5, 1)];
        }
        dispatch(
            Action::TaskComplete(TaskResult::SessionLoaded {
                agent_id: id,
                session_id: acp::SessionId::new("sess-1"),
                models: None,
                code_restored: false,
                restore_summary: None,
                restore_degree: None,
                running_prompt_id: None,
                scheduler_background_loops: None,
            }),
            &mut app,
        );
        let agent = &app.agents[&id];
        assert!(agent.follow_ups.is_none(), "idle reload must clear chips");
        assert!(
            agent.follow_up_chips.is_empty(),
            "idle reload must clear hit rects"
        );
        assert!(
            agent.follow_up_seen.is_empty(),
            "idle reload must reset the seen map"
        );
        assert_eq!(
            agent.follow_up_next_gen, 0,
            "idle reload must reset the generation"
        );
    }

    /// Multi-client load-window gap (leader mode): a viewer
    /// (`attached_as_viewer`) that subscribed mid-turn receives the driver's
    /// live (non-replay) deltas WHILE its `session/load` replay is still in
    /// flight — so `current_prompt_id == None` and `loading_replay == true`.
    /// Before the fix the gate dropped every such delta (the viewer froze at
    /// its load snapshot). Now the viewer ADOPTS the incoming prompt id and the
    /// delta renders into scrollback, recovering progress streamed during the
    /// load window.
    #[test]
    fn viewer_adopts_live_delta_during_load_window_instead_of_dropping() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        // Model a viewer mid-load: opened via `session/load`, replay in flight,
        // no turn of its own.
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.attached_as_viewer = true;
            agent.session.loading_replay = true;
            agent.session.current_prompt_id = None;
        }

        // A live (non-replay) chunk from the driver's user-initiated turn,
        // stamped with a normal (non-synthetic) promptId the viewer has never
        // seen.
        assert!(
            !is_server_initiated_prompt("p-driver"),
            "test fixture must use a non-synthetic prompt id"
        );
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = acp::SessionNotification::new(
            acp::SessionId::new("sess-1"),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("in-window driver delta"),
            ))),
        )
        .meta(
            serde_json::json!({ "promptId": "p-driver" })
                .as_object()
                .cloned(),
        );
        let affected = handle(
            AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
                request,
                response_tx: tx,
            }),
            &mut app,
        );

        // Adopted the driver's prompt id (so subsequent deltas keep matching).
        assert_eq!(
            app.agents[&id].session.current_prompt_id.as_deref(),
            Some("p-driver"),
            "viewer must adopt the driver's prompt id rather than drop the delta"
        );
        // The body rendered into scrollback (not dropped).
        assert_eq!(
            agent_message_text(app.agents.get(&id).unwrap()),
            "in-window driver delta",
            "viewer must render the live delta received during its load window"
        );
        // Redraw is suppressed while `loading_replay` is true (the content is in
        // scrollback and the post-`SessionLoaded` redraw paints it).
        assert!(
            !affected,
            "redraw is suppressed during loading_replay even though the delta applied"
        );
    }

    /// Driver-rewind safety: the viewer relaxation must NOT regress the
    /// locally-created driver's stale-chunk drop. A non-viewer
    /// (`attached_as_viewer == false`) whose own turn just finished/cancelled
    /// (`current_prompt_id == None`) must still DROP a late delta carrying the
    /// aborted turn's promptId — otherwise rewound content would resurface.
    #[test]
    fn driver_post_rewind_still_drops_stale_delta() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        // A locally-created driver (NOT a viewer) that just finished its turn:
        // finish_turn cleared current_prompt_id back to None. The aborted turn
        // was driven by THIS client, so its prompt id is self-originated — that
        // is what makes the gate treat the late chunk as ours (drop) rather
        // than as another client's turn (adopt).
        {
            let agent = app.agents.get_mut(&id).unwrap();
            assert!(
                !agent.attached_as_viewer,
                "a locally-created driver is never a viewer"
            );
            agent.note_self_originated_prompt("p-aborted");
            agent.session.loading_replay = false;
            agent.session.current_prompt_id = None;
        }

        // A late/stale chunk from the just-aborted turn arrives.
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = acp::SessionNotification::new(
            acp::SessionId::new("sess-1"),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("stale rewound body"),
            ))),
        )
        .meta(
            serde_json::json!({ "promptId": "p-aborted" })
                .as_object()
                .cloned(),
        );
        handle(
            AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
                request,
                response_tx: tx,
            }),
            &mut app,
        );

        // Dropped: no adoption, nothing rendered.
        assert!(
            app.agents[&id].session.current_prompt_id.is_none(),
            "driver post-rewind must NOT adopt a stale prompt id"
        );
        assert_eq!(
            agent_message_text(app.agents.get(&id).unwrap()),
            "",
            "driver post-rewind must drop the stale delta (no scrollback render)"
        );
    }

    /// Multi-client mirroring after this pane has driven a turn (the sticky-flag
    /// bug). A pane that already sent a prompt has `attached_as_viewer == false`
    /// and a self-originated prompt id. When ANOTHER pane then drives a normal
    /// turn, the live deltas carry a foreign (non-server-initiated) prompt id
    /// this pane never originated. With the old one-way latch the gate dropped
    /// them and the pane rendered nothing; now the gate re-derives viewer status
    /// from prompt-id ownership, so the foreign turn is adopted + rendered and
    /// the flag flips back to true.
    #[test]
    fn pane_that_drove_a_turn_still_mirrors_another_clients_turn() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        // This pane drove its own turn "p-mine" earlier, then went idle.
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.note_self_originated_prompt("p-mine");
            agent.attached_as_viewer = false;
            agent.session.loading_replay = false;
            agent.session.current_prompt_id = None;
        }

        // Another pane now drives a normal turn; its live delta arrives here
        // with a foreign, non-synthetic prompt id.
        assert!(
            !is_server_initiated_prompt("p-other"),
            "test fixture must use a non-synthetic prompt id"
        );
        let _ = handle(
            make_agent_chunk_message_with_prompt("sess-1", "other pane output", "p-other", false),
            &mut app,
        );

        let agent = app.agents.get(&id).unwrap();
        assert_eq!(
            agent.session.current_prompt_id.as_deref(),
            Some("p-other"),
            "the foreign turn must be adopted so subsequent deltas keep matching"
        );
        assert_eq!(
            agent_message_text(agent),
            "other pane output",
            "a pane that already drove a turn must still render another client's turn"
        );
        assert!(
            agent.attached_as_viewer,
            "viewing another client's turn must re-arm the viewer flag (no one-way latch)"
        );
    }

    /// The entry `kind` survives serialization, and on adoption a `bash` entry drives the bash
    /// turn-start shim (`bash_turn = true`, no user block).
    #[test]
    fn bash_kind_round_trips_and_adoption_sets_bash_turn() {
        // Shell and pager share the echo-agent-prompt-queue type; pin kind through a serde cycle.
        let shell = echo_agent_runtime::session::prompt_queue::QueueChanged {
            session_id: "sess-1".to_string(),
            entries: vec![echo_agent_runtime::session::prompt_queue::QueueEntryWire {
                id: "b1".to_string(),
                version: 0,
                owner: None,
                last_editor: None,
                kind: "bash".to_string(),
                text: "ls -la".to_string(),
                position: 0,
                combined_texts: None,
            }],
            running_prompt_id: None,
            running_text: None,
            running_kind: None,
            running_combined_texts: None,
        };
        let json = serde_json::to_string(&shell).unwrap();
        let mirror: crate::app::prompt_queue::QueueChanged = serde_json::from_str(&json).unwrap();
        assert_eq!(mirror.entries[0].kind, "bash");

        // Adoption: seed the shared queue with the bash entry, then the leader
        // reports it running → bash turn-start shim fires.
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        app.push_optimistic_prompt_echo("sess-1", "b1", "ls -la", "bash");
        let scroll_before = app.agents[&id].scrollback.len();

        handle_queue_changed(&queue_changed_running("sess-1", &[], Some("b1")), &mut app);

        let agent = app.agents.get(&id).unwrap();
        assert!(agent.bash_turn, "bash adoption must set bash_turn");
        assert_eq!(agent.session.current_prompt_id.as_deref(), Some("b1"));
        assert!(agent.session.state.is_turn_running());
        // Bash pushes NO user/display block.
        assert_eq!(agent.scrollback.len(), scroll_before);
    }

    /// The gate buffers an instant promoted turn's stream and the adoption flushes it.
    #[test]
    fn fifo_handoff_buffers_instant_bash_turn_and_flushes_on_adoption() {
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);

        send_tool_call_update(&mut app, "b1", "bash-mode-1", Some("sess-1-7"));
        {
            let agent = app.agents.get(&id).unwrap();
            assert_eq!(
                agent.pending_adoption_updates.len(),
                1,
                "the promoted turn's update must be buffered, not dropped"
            );
            assert_eq!(agent.session.current_prompt_id.as_deref(), Some("p1"));
            assert_eq!(
                agent.last_seen_event_id, None,
                "buffered-but-unapplied events must not advance the reconnect cursor"
            );
        }

        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], None),
            &mut app
        ));
        assert!(
            app.pending_running_adoptions.contains_key(&id),
            "turn-end broadcast must not tear down an adoption with buffered updates"
        );

        prompt_response(&mut app, "p1");
        let agent = app.agents.get(&id).unwrap();
        assert_eq!(agent.session.current_prompt_id.as_deref(), Some("b1"));
        assert!(agent.bash_turn, "adopted bash turn must set bash_turn");
        assert!(agent.pending_adoption_updates.is_empty(), "buffer flushed");
        assert_eq!(
            tool_call_block_count(agent),
            1,
            "the buffered execute block must render on adoption"
        );
        assert_eq!(
            agent.last_seen_event_id.as_deref(),
            Some("sess-1-7"),
            "the flush applies the update and advances the cursor"
        );

        prompt_response(&mut app, "b1");
        assert!(app.agents[&id].session.state.is_idle());
    }

    /// The stash survives one `running=None`; a second tears stash + buffer down.
    #[test]
    fn turn_end_retention_is_one_shot() {
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);
        send_tool_call_update(&mut app, "b1", "bash-mode-1", None);

        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], None),
            &mut app
        ));
        assert!(
            app.pending_running_adoptions.contains_key(&id),
            "first running=None (the turn's own end) must retain the stash"
        );

        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], None),
            &mut app
        ));
        assert!(
            !app.pending_running_adoptions.contains_key(&id),
            "second running=None must tear the stash down"
        );
        assert!(
            app.agents[&id].pending_adoption_updates.is_empty(),
            "the torn-down stash's buffered updates must be discarded"
        );

        prompt_response(&mut app, "p1");
        assert!(app.agents[&id].session.state.is_idle());
        assert!(app.agents[&id].session.current_prompt_id.is_none());
    }

    /// The stashed turn's own PromptResponse spends its only exit: discard, not restore.
    #[test]
    fn stashed_turns_own_response_discards_stash_and_buffer() {
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);
        send_tool_call_update(&mut app, "b1", "bash-mode-1", None);

        // b1's response lands first, while p1 is still current.
        prompt_response(&mut app, "b1");
        {
            let agent = app.agents.get(&id).unwrap();
            assert_eq!(
                agent.session.current_prompt_id.as_deref(),
                Some("p1"),
                "the running turn must be untouched"
            );
            assert!(agent.session.state.is_turn_running());
            assert!(
                !app.pending_running_adoptions.contains_key(&id),
                "the spent stash must not be restored"
            );
            assert!(
                agent.pending_adoption_updates.is_empty(),
                "the spent stash's buffered updates must be discarded"
            );
        }

        prompt_response(&mut app, "p1");
        assert!(app.agents[&id].session.state.is_idle());
        assert!(app.agents[&id].session.current_prompt_id.is_none());
    }

    /// A newer promoted prompt supersedes the stash and discards the old pid's buffer.
    #[test]
    fn superseded_adoption_discards_old_pids_buffer() {
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);
        send_tool_call_update(&mut app, "b1", "bash-mode-1", None);

        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.note_self_originated_prompt("b2");
        }
        app.push_optimistic_prompt_echo("sess-1", "b2", "printf 2", "bash");
        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], Some("b2")),
            &mut app
        ));
        assert!(
            app.agents[&id]
                .pending_adoption_updates
                .iter()
                .all(|(pid, _, _)| pid != "b1"),
            "superseded b1's buffered updates must be discarded at supersede time"
        );
        send_tool_call_update(&mut app, "b2", "bash-mode-2", None);

        prompt_response(&mut app, "p1");
        let agent = app.agents.get(&id).unwrap();
        assert_eq!(agent.session.current_prompt_id.as_deref(), Some("b2"));
        assert_eq!(
            tool_call_block_count(agent),
            1,
            "exactly the superseding turn's buffered block renders"
        );
        assert!(agent.pending_adoption_updates.is_empty());
    }

    /// A reconnect reload discards the buffer (the replay re-delivers it).
    #[test]
    fn session_reload_clears_pending_adoption_buffer() {
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);
        send_tool_call_update(&mut app, "b1", "bash-mode-1", None);
        assert_eq!(app.agents[&id].pending_adoption_updates.len(), 1);

        app.agents.get_mut(&id).unwrap().begin_session_reload(1);
        assert!(app.agents[&id].pending_adoption_updates.is_empty());
    }

    /// Retention must not depend on the buffer being non-empty (channel reorder).
    #[test]
    fn turn_end_broadcast_overtaking_updates_keeps_stash() {
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);

        // b1's running=None lands before any of its updates.
        assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], None),
            &mut app
        ));
        assert!(
            app.pending_running_adoptions.contains_key(&id),
            "empty-buffer retention: the overtaking turn-end must spare the stash"
        );

        send_tool_call_update(&mut app, "b1", "bash-mode-1", None);
        assert_eq!(app.agents[&id].pending_adoption_updates.len(), 1);

        prompt_response(&mut app, "p1");
        let agent = app.agents.get(&id).unwrap();
        assert_eq!(agent.session.current_prompt_id.as_deref(), Some("b1"));
        assert_eq!(tool_call_block_count(agent), 1, "late update must render");

        prompt_response(&mut app, "b1");
        assert!(app.agents[&id].session.state.is_idle());
    }

    /// The flush never moves the shared reconnect cursor backwards.
    #[test]
    fn flush_does_not_regress_reconnect_cursor() {
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);
        send_tool_call_update(&mut app, "b1", "bash-mode-1", Some("sess-1-7"));
        app.agents.get_mut(&id).unwrap().last_seen_event_id = Some("sess-1-9".to_string());

        prompt_response(&mut app, "p1");
        let agent = app.agents.get(&id).unwrap();
        assert_eq!(tool_call_block_count(agent), 1, "buffered update applies");
        assert_eq!(
            agent.last_seen_event_id.as_deref(),
            Some("sess-1-9"),
            "flush must not regress the cursor past a later applied event"
        );
    }

    /// A stash matching the finishing response's pid is discarded, not re-adopted.
    #[test]
    fn own_pid_stash_is_not_readopted_by_its_response() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.session.current_prompt_id = Some("p1".to_string());
            agent.session.state = AgentState::TurnRunning;
        }
        app.pending_running_adoptions.insert(
            id,
            crate::app::acp_handler::PendingRunningAdoption {
                prompt_id: "p1".to_string(),
                text: Some("first".to_string()),
                combined_texts: None,
                kind: "prompt".to_string(),
                turn_ended: false,
            },
        );

        prompt_response(&mut app, "p1");
        let agent = app.agents.get(&id).unwrap();
        assert!(
            agent.session.state.is_idle(),
            "must not re-enter TurnRunning"
        );
        assert!(agent.session.current_prompt_id.is_none());
        assert!(!app.pending_running_adoptions.contains_key(&id));
    }

    /// Overflow drops the newest entry; the flush renders the kept prefix.
    #[test]
    fn buffer_cap_drops_newest_and_flushes_prefix() {
        use crate::app::agent_view::MAX_PENDING_ADOPTION_UPDATES;
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);

        for i in 0..(MAX_PENDING_ADOPTION_UPDATES + 1) {
            let tool = format!("bash-mode-{i}");
            let event = format!("sess-1-{}", i + 1);
            send_tool_call_update(&mut app, "b1", &tool, Some(&event));
        }
        assert_eq!(
            app.agents[&id].pending_adoption_updates.len(),
            MAX_PENDING_ADOPTION_UPDATES,
            "overflow entry must be dropped, not evict the prefix"
        );

        prompt_response(&mut app, "p1");
        let agent = app.agents.get(&id).unwrap();
        assert_eq!(tool_call_block_count(agent), MAX_PENDING_ADOPTION_UPDATES);
        assert_eq!(
            agent.last_seen_event_id.as_deref(),
            Some(format!("sess-1-{MAX_PENDING_ADOPTION_UPDATES}").as_str()),
            "cursor ends at the last KEPT entry"
        );
    }

    /// A viewer's `prompt_complete` finalize discards stash + buffer.
    #[test]
    fn viewer_finalize_discards_adoption_buffer() {
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);
        send_tool_call_update(&mut app, "b1", "bash-mode-1", None);
        app.agents.get_mut(&id).unwrap().attached_as_viewer = true;

        let params = serde_json::json!({
            "sessionId": "sess-1",
            "stopReason": "end_turn",
            "promptId": "p1",
        });
        let notif = acp::ExtNotification::new(
            "echo.agent/session/prompt_complete",
            serde_json::value::to_raw_value(&params).unwrap().into(),
        );
        handle_prompt_complete(&notif, &mut app);

        let agent = app.agents.get(&id).unwrap();
        assert!(agent.session.state.is_idle(), "viewer turn finalized");
        assert!(!app.pending_running_adoptions.contains_key(&id));
        assert!(
            agent.pending_adoption_updates.is_empty(),
            "viewer finalize must discard the buffered updates"
        );
    }

    /// The credit-limit early return discards the popped adoption's buffer.
    #[test]
    fn credit_limit_response_discards_adoption_buffer() {
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);
        send_tool_call_update(&mut app, "b1", "bash-mode-1", None);
        app.agents
            .get_mut(&id)
            .unwrap()
            .session
            .credit_limit_blocked = true;

        prompt_response(&mut app, "p1");
        let agent = app.agents.get(&id).unwrap();
        assert!(!app.pending_running_adoptions.contains_key(&id));
        assert!(agent.pending_adoption_updates.is_empty());
    }

    /// A stash whose pid replayed a durable terminal is discarded, never adopted.
    #[test]
    fn terminal_in_replay_stash_is_discarded_not_adopted() {
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);
        send_tool_call_update(&mut app, "b1", "bash-mode-1", None);
        app.agents
            .get_mut(&id)
            .unwrap()
            .replayed_terminal_prompts
            .insert("b1".to_string());

        prompt_response(&mut app, "p1");
        let agent = app.agents.get(&id).unwrap();
        assert!(
            agent.session.state.is_idle(),
            "no adoption of a terminal pid"
        );
        assert!(agent.session.current_prompt_id.is_none());
        assert!(agent.pending_adoption_updates.is_empty());
        assert_eq!(tool_call_block_count(agent), 0, "nothing flushed");
    }

    /// The `reconnect_pending` early return discards the dropped adoption's buffer.
    #[test]
    fn reconnect_pending_response_discards_adoption_buffer() {
        let mut app = app_with_running_p1_and_stashed_b1();
        let id = AgentId(0);
        send_tool_call_update(&mut app, "b1", "bash-mode-1", Some("sess-1-7"));

        app.reconnect_pending = true;
        prompt_response(&mut app, "p1");

        let agent = app.agents.get(&id).unwrap();
        assert!(
            !app.pending_running_adoptions.contains_key(&id),
            "the adoption is dropped on the reconnect teardown"
        );
        assert!(
            agent.pending_adoption_updates.is_empty(),
            "its buffered updates must be discarded, not orphaned"
        );
        assert_eq!(
            agent.last_seen_event_id, None,
            "the cursor never advanced for the unapplied updates — replay heals"
        );
        assert_eq!(
            tool_call_block_count(agent),
            0,
            "nothing was applied out-of-band"
        );
    }

    /// `adopt_running_prompt` must not leave `start_turn`'s user-echo skip
    /// armed: the adopted turn pushed no local user block, so the armed skip
    /// would survive and silently eat the NEXT turn's `UserMessageChunk`
    /// broadcast (the driver's next message missing from the viewer).
    #[test]
    fn adopt_running_prompt_does_not_eat_next_turns_user_echo() {
        let mut app = make_app_with_agent("sess-adopt");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.adopt_running_prompt("p-driver".into());
            // The adopted turn ends.
            agent.session.finish_turn(&mut agent.scrollback);
        }
        let len_before = app.agents[&id].scrollback.len();

        // The driver's NEXT turn begins with its user-message broadcast.
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = acp::SessionNotification::new(
            acp::SessionId::new("sess-adopt"),
            acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("next question"),
            ))),
        )
        .meta(
            serde_json::json!({ "promptId": "p-next", "isReplay": false })
                .as_object()
                .cloned(),
        );
        let _ = handle(
            AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
                request,
                response_tx: tx,
            }),
            &mut app,
        );

        assert_eq!(
            app.agents[&id].scrollback.len(),
            len_before + 1,
            "the next turn's user message must render, not be echo-swallowed"
        );
    }

    /// A FAILED reconnect reload restores the stashed tracker; if a local send
    /// just before the outage armed the user-echo skip (echo chunk not yet
    /// received), the restored skip must be cleared at finalize — otherwise it
    /// silently swallows the NEXT turn's `UserMessageChunk`.
    #[test]
    fn failed_reload_clears_armed_user_echo_skip() {
        let mut app = make_app_with_agent("sess-echo");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            // Local send just before the outage: user block pushed + echo armed.
            agent
                .scrollback
                .push_block(RenderBlock::user_prompt("pre-outage prompt"));
            agent.session.tracker.expect_user_echo();
            // Reconnect opens the reload window (stashes the armed tracker)...
            agent.begin_session_reload(1);
            // ...and the reload FAILS, restoring the stash.
            assert!(agent.finish_session_reload(1, false));
            assert!(
                !agent.session.tracker.expects_user_echo(),
                "a failed reload must not leave the user-echo skip armed"
            );
        }
        let len_before = app.agents[&id].scrollback.len();

        // The next turn's user-message broadcast must render, not be swallowed.
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = acp::SessionNotification::new(
            acp::SessionId::new("sess-echo"),
            acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("next question"),
            ))),
        )
        .meta(
            serde_json::json!({ "promptId": "p-next", "isReplay": false })
                .as_object()
                .cloned(),
        );
        let _ = handle(
            AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
                request,
                response_tx: tx,
            }),
            &mut app,
        );

        assert_eq!(
            app.agents[&id].scrollback.len(),
            len_before + 1,
            "the next turn's user message must render after a failed reload"
        );
    }

    /// The bind helper resets per-session cursor/highwater only when the
    /// bound id actually changes: `SessionLoaded` re-binds the SAME id after
    /// every load, so an unconditional reset would wipe the cursor the load's
    /// replay just established.
    #[test]
    fn bind_session_id_resets_cursor_only_on_id_change() {
        let mut agent = make_agent(Some("sess-a"));
        agent.last_seen_event_id = Some("sess-a-7".into());
        agent.last_applied_event_seq = Some(7);
        agent.last_applied_echo_agent_event_seq = Some(8);
        agent.deferred_subagent_finishes.insert(
            "child-stale".into(),
            crate::app::agent_view::DeferredSubagentFinish {
                notification: echo_agent_runtime::extensions::notification::SessionNotification {
                    session_id: acp::SessionId::new("sess-a"),
                    update: test_subagent_finished("child-stale"),
                    meta: None,
                },
                inserted_at: std::time::Instant::now(),
            },
        );

        let epoch = agent.session_binding_epoch;
        agent.bind_session_id(acp::SessionId::new("sess-a"));
        assert_eq!(agent.session_binding_epoch, epoch);
        assert_eq!(agent.last_seen_event_id.as_deref(), Some("sess-a-7"));
        assert_eq!(agent.last_applied_event_seq, Some(7));
        assert_eq!(agent.last_applied_echo_agent_event_seq, Some(8));
        assert_eq!(agent.deferred_subagent_finishes.len(), 1);

        agent.bind_session_id(acp::SessionId::new("sess-b"));
        assert_eq!(agent.session_binding_epoch, epoch.wrapping_add(1));
        assert_eq!(
            agent.session.session_id.as_ref().map(|s| s.0.as_ref()),
            Some("sess-b")
        );
        assert!(
            agent.last_seen_event_id.is_none(),
            "another session's cursor must not survive a rebind"
        );
        assert!(agent.last_applied_event_seq.is_none());
        assert!(agent.last_applied_echo_agent_event_seq.is_none());
        assert!(
            agent.deferred_subagent_finishes.is_empty(),
            "deferred finishes are meaningless against another session"
        );
    }

    #[test]
    fn session_binding_epoch_counts_bind_and_unbind_transitions() {
        let mut agent = make_agent(None);
        assert_eq!(agent.session_binding_epoch, 0);
        agent.bind_session_id(acp::SessionId::new("a"));
        assert_eq!(agent.session_binding_epoch, 1);
        agent.unbind_session_id();
        assert_eq!(agent.session_binding_epoch, 2);
        agent.bind_session_id(acp::SessionId::new("a"));
        assert_eq!(agent.session_binding_epoch, 3);
    }

    #[test]
    fn viewer_adopting_live_delta_enters_turn_running_and_timer_is_monotonic() {
        // A viewer (attached_as_viewer) watching the driver's turn starts Idle.
        // Adopting the first live delta must flip it to TurnRunning and stamp
        // `turn_started_at` so the turn-in-progress chrome (status line, elapsed
        // timer, cancel/interject footer hints — all gated on TurnRunning)
        // renders. A second chunk must NOT reset `turn_started_at`.
        let mut app = make_app_with_agent("sess-view");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.attached_as_viewer = true;
            assert!(matches!(agent.session.state, AgentState::Idle));
            assert!(agent.turn_started_at.is_none());
        }

        let _ = handle(
            make_agent_chunk_message_with_prompt(
                "sess-view",
                "driver chunk 1",
                "pid-driver",
                false,
            ),
            &mut app,
        );

        let first_started_at = {
            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert!(
                matches!(agent.session.state, AgentState::TurnRunning),
                "viewer must enter TurnRunning when adopting a live driver delta"
            );
            assert_eq!(
                agent.session.current_prompt_id.as_deref(),
                Some("pid-driver")
            );
            agent
                .turn_started_at
                .expect("turn_started_at must be stamped on Idle->TurnRunning")
        };

        // Second chunk for the same turn must not reset the elapsed-timer anchor.
        let _ = handle(
            make_agent_chunk_message_with_prompt(
                "sess-view",
                "driver chunk 2",
                "pid-driver",
                false,
            ),
            &mut app,
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(matches!(agent.session.state, AgentState::TurnRunning));
        assert_eq!(
            agent.turn_started_at,
            Some(first_started_at),
            "turn_started_at must be monotonic across chunks (not reset per chunk)"
        );
    }

    #[test]
    fn viewer_replay_delta_does_not_enter_turn_running() {
        // A replayed historical-load delta (also attached_as_viewer) must NOT
        // flash TurnRunning — only LIVE deltas put a viewer into the running
        // state. The promptId is still adopted so later chunks match.
        let mut app = make_app_with_agent("sess-view");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.attached_as_viewer = true;
            agent.session.loading_replay = true;
        }

        let _ = handle(
            make_agent_chunk_message_with_prompt("sess-view", "old chunk", "pid-old", true),
            &mut app,
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            matches!(agent.session.state, AgentState::Idle),
            "a replay delta must not enter TurnRunning"
        );
        assert!(agent.turn_started_at.is_none());
    }

    #[test]
    fn viewer_mid_turn_reattach_shows_running_chrome_after_replay_window() {
        // Reproduces the MID-TURN reattach ordering that left a viewer stuck
        // Idle (no "Responding…"/cancel chrome) even though the driver's turn
        // was live:
        //   (1) the viewer subscribes on its load request, so it receives a
        //       LIVE delta DURING its replay window (loading_replay = true). It
        //       adopts the driver's promptId, but the TurnRunning transition is
        //       (correctly) suppressed while replaying.
        //   (2) SessionLoaded clears loading_replay — modeled here WITHOUT a
        //       runningPromptId conveyance (the worst case).
        //   (3) subsequent LIVE deltas carry the SAME (already-adopted) promptId,
        //       so they MATCH current_prompt_id and skip the adopt block.
        // Before the fix, step (3) never flipped the viewer to TurnRunning (the
        // TurnRunning entry lived inside the mismatch-only adopt block), so the
        // chrome never appeared. It must now.
        let mut app = make_app_with_agent("sess-view");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.attached_as_viewer = true;
            agent.session.loading_replay = true;
            assert!(matches!(agent.session.state, AgentState::Idle));
        }

        // (1) live delta during the replay window: adopts promptId, stays Idle.
        let _ = handle(
            make_agent_chunk_message_with_prompt("sess-view", "responding 1", "pid-driver", false),
            &mut app,
        );
        {
            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert_eq!(
                agent.session.current_prompt_id.as_deref(),
                Some("pid-driver"),
                "the in-flight turn id is adopted even during the replay window"
            );
            assert!(
                matches!(agent.session.state, AgentState::Idle),
                "during loading_replay the viewer must not flash TurnRunning yet"
            );
        }

        // (2) SessionLoaded completes (no runningPromptId conveyed).
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .session
            .loading_replay = false;

        // (3) a post-load live delta MATCHING the already-adopted promptId — this
        //     skips the mismatch-only adopt block, so the fix must flip the
        //     viewer to TurnRunning here.
        let _ = handle(
            make_agent_chunk_message_with_prompt("sess-view", "responding 2", "pid-driver", false),
            &mut app,
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            matches!(agent.session.state, AgentState::TurnRunning),
            "viewer must show TurnRunning chrome for a matching post-load live \
             delta (mid-turn reattach)"
        );
        assert!(
            agent.turn_started_at.is_some(),
            "elapsed-timer anchor must be stamped so the counter renders"
        );
        assert_eq!(
            agent.session.tracker.activity(),
            Some(crate::acp::tracker::TurnActivity::Responding),
            "activity must be Responding so the status line reads 'Responding…'"
        );
    }

    #[test]
    fn viewer_does_not_enter_turn_running_for_server_initiated_turn() {
        // A server-initiated / auto-wake turn (synthetic prompt id, e.g. a
        // background subagent or task completion: `task-completed-…`) runs inside
        // the actor and emits NO `echo.agent/session/prompt_complete`. If a viewer
        // entered TurnRunning for it, nothing would ever finish the turn and the
        // viewer would be stuck "Responding…" forever — exactly the bug where one
        // dashboard showed "Worked for" while the other was stuck responding.
        // The driver also declines to show chrome for these (its server-initiated
        // adopt path never calls start_turn), so the viewer must mirror that:
        // adopt the id (so content renders) but stay Idle (no running chrome).
        let mut app = make_app_with_agent("sess-view");
        app.agents.get_mut(&AgentId(0)).unwrap().attached_as_viewer = true;

        let _ = handle(
            make_agent_chunk_message_with_prompt(
                "sess-view",
                "auto-wake output",
                "task-completed-abc",
                false,
            ),
            &mut app,
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent.session.current_prompt_id.as_deref(),
            Some("task-completed-abc"),
            "the synthetic turn id is still adopted so its content renders"
        );
        assert!(
            matches!(agent.session.state, AgentState::Idle),
            "a viewer must NOT enter TurnRunning for a server-initiated turn (no \
             prompt_complete would ever finish it → permanent stuck spinner)"
        );
        assert!(agent.turn_started_at.is_none());
    }

    #[test]
    fn viewer_enters_turn_running_for_scheduler_fired_cron_turn() {
        // A `/loop` (scheduled-task) turn has a synthetic `scheduler-fired-…`
        // prompt id, but UNLIKE auto-wake turns it is client-driven via
        // `MvpAgent::prompt()` and DOES emit `echo.agent/session/prompt_complete`. So a
        // viewer MUST enter TurnRunning for it — otherwise the dashboard's
        // locally-tracked row for a running `/loop` session never shows Working.
        let mut app = make_app_with_agent("sess-view");
        app.agents.get_mut(&AgentId(0)).unwrap().attached_as_viewer = true;

        let _ = handle(
            make_agent_chunk_message_with_prompt(
                "sess-view",
                "cron output",
                "scheduler-fired-task-1",
                false,
            ),
            &mut app,
        );

        {
            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert_eq!(
                agent.session.current_prompt_id.as_deref(),
                Some("scheduler-fired-task-1")
            );
            assert!(
                matches!(agent.session.state, AgentState::TurnRunning),
                "a viewer MUST enter TurnRunning for a scheduler-fired cron turn \
                 so the dashboard shows it as Working"
            );
            assert!(agent.turn_started_at.is_some());
        }

        // The cron's prompt_complete must still finish it — no permanent spinner.
        let _ = handle_ext_notification(&prompt_complete_ext("sess-view"), &mut app);
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            matches!(agent.session.state, AgentState::Idle),
            "cron prompt_complete must finish the viewer turn (no strand)"
        );
        assert!(agent.session.current_prompt_id.is_none());
    }

    #[test]
    fn viewer_prompt_complete_finishes_turn() {
        // A viewer in TurnRunning receives echo.agent/session/prompt_complete for its
        // session -> finish_turn: state Idle, current_prompt_id cleared.
        let mut app = make_app_with_agent("sess-view");
        app.agents.get_mut(&AgentId(0)).unwrap().attached_as_viewer = true;
        let _ = handle(
            make_agent_chunk_message_with_prompt("sess-view", "chunk", "pid-driver", false),
            &mut app,
        );
        assert!(matches!(
            app.agents.get(&AgentId(0)).unwrap().session.state,
            AgentState::TurnRunning
        ));

        let affected = handle_ext_notification(&prompt_complete_ext("sess-view"), &mut app);
        assert!(affected, "finishing the active viewer turn should redraw");

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            matches!(agent.session.state, AgentState::Idle),
            "prompt_complete must drop a viewer back to Idle"
        );
        assert!(
            agent.session.current_prompt_id.is_none(),
            "prompt_complete must clear current_prompt_id"
        );
        assert!(agent.turn_started_at.is_none());
    }

    #[test]
    fn viewer_prompt_complete_pushes_turn_completed_marker() {
        // Regression (leader/dashboard mode): the "Worked for X" marker
        // is generated from the driver's PromptResponse RPC, which a viewer
        // never receives. Before this fix the driver pane showed
        // "Worked for …" while the viewer pane (same session) showed
        // nothing. The viewer must surface the equivalent marker on the
        // broadcast prompt_complete.
        let mut app = make_app_with_agent("sess-view");
        app.agents.get_mut(&AgentId(0)).unwrap().attached_as_viewer = true;
        let _ = handle(
            make_agent_chunk_message_with_prompt("sess-view", "chunk", "pid-driver", false),
            &mut app,
        );
        assert!(matches!(
            app.agents.get(&AgentId(0)).unwrap().session.state,
            AgentState::TurnRunning
        ));

        let affected = handle_ext_notification(&prompt_complete_ext("sess-view"), &mut app);
        assert!(affected, "finishing the active viewer turn should redraw");

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            matches!(
                last_session_event(&agent.scrollback),
                Some(SessionEvent::TurnCompleted { .. })
            ),
            "a viewer must push a 'Worked for' marker on prompt_complete"
        );
    }

    #[test]
    fn viewer_turn_completed_elapsed_matches_authoritative_turn_start() {
        // The viewer back-dates its turn anchor from the authoritative
        // `turnStartMs` (not the local first-delta time), so its elapsed — and
        // therefore the "Worked for X" marker — matches the driver's
        // instead of reading near-zero.
        let mut app = make_app_with_agent("sess-view");
        app.agents.get_mut(&AgentId(0)).unwrap().attached_as_viewer = true;

        // First live delta: the turn started ~5s ago on the driver.
        let _ = handle(
            make_viewer_chunk_with_turn_start("sess-view", "pid-driver", 5_000),
            &mut app,
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let elapsed = agent
            .turn_elapsed()
            .expect("viewer in TurnRunning must have an anchor");
        assert!(
            elapsed.as_secs() >= 4,
            "viewer elapsed must reflect the back-dated turn start (~5s), got {elapsed:?}"
        );

        let _ = handle_ext_notification(&prompt_complete_ext("sess-view"), &mut app);
        let agent = app.agents.get(&AgentId(0)).unwrap();
        match last_session_event(&agent.scrollback) {
            Some(SessionEvent::TurnCompleted {
                elapsed: Some(elapsed),
            }) => assert!(
                elapsed.as_secs() >= 4,
                "completion marker must reflect the back-dated start, got {elapsed:?}"
            ),
            other => panic!("expected TurnCompleted marker, got {other:?}"),
        }
    }

    #[test]
    fn viewer_prompt_complete_cancelled_pushes_turn_cancelled_marker() {
        let mut app = make_app_with_agent("sess-view");
        app.agents.get_mut(&AgentId(0)).unwrap().attached_as_viewer = true;
        let _ = handle(
            make_agent_chunk_message_with_prompt("sess-view", "chunk", "pid-driver", false),
            &mut app,
        );

        let _ = handle_ext_notification(
            &prompt_complete_ext_with_reason("sess-view", "cancelled", None),
            &mut app,
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            matches!(
                last_session_event(&agent.scrollback),
                Some(SessionEvent::TurnCancelled { .. })
            ),
            "a cancelled stopReason must map to a 'Turn cancelled' marker"
        );
    }

    #[test]
    fn viewer_prompt_complete_error_pushes_turn_failed_marker() {
        let mut app = make_app_with_agent("sess-view");
        app.agents.get_mut(&AgentId(0)).unwrap().attached_as_viewer = true;
        let _ = handle(
            make_agent_chunk_message_with_prompt("sess-view", "chunk", "pid-driver", false),
            &mut app,
        );

        let _ = handle_ext_notification(
            &prompt_complete_ext_with_reason("sess-view", "error", Some("boom")),
            &mut app,
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        match last_session_event(&agent.scrollback) {
            Some(SessionEvent::TurnFailed { error, .. }) => {
                assert_eq!(
                    error, "Request failed: boom. Try sending again.",
                    "agentResult must propagate into the formatted marker"
                )
            }
            other => panic!("expected TurnFailed marker, got {other:?}"),
        }
    }

    #[test]
    fn viewer_prompt_complete_rate_limit_pushes_no_marker() {
        // Rate limits surface a dedicated UX on the driver and aren't actionable
        // from a viewer — the viewer still finishes its turn but pushes no
        // "Turn failed" line.
        let mut app = make_app_with_agent("sess-view");
        app.agents.get_mut(&AgentId(0)).unwrap().attached_as_viewer = true;
        let _ = handle(
            make_agent_chunk_message_with_prompt("sess-view", "chunk", "pid-driver", false),
            &mut app,
        );

        let _ = handle_ext_notification(
            &prompt_complete_ext_with_reason("sess-view", "rate_limit", None),
            &mut app,
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            matches!(agent.session.state, AgentState::Idle),
            "the viewer turn must still finish on a rate-limit stop"
        );
        assert!(
            last_session_event(&agent.scrollback).is_none(),
            "rate-limit must not push a 'Turn failed' marker on a viewer"
        );
    }

    #[test]
    fn viewer_stop_hooks_after_marker_attach_to_it() {
        // Viewer order: the durable TurnCompleted pushes the marker first;
        // the batch arriving right after merges into it.
        let mut app = make_app_with_agent("sess-view-hooks");
        app.agents.get_mut(&AgentId(0)).unwrap().attached_as_viewer = true;
        let _ = handle(
            make_agent_chunk_message_with_prompt("sess-view-hooks", "chunk", "pid-v", false),
            &mut app,
        );
        let _ = handle_ext_notification(
            &echo_agent_turn_completed_notif("sess-view-hooks", "pid-v", "end_turn", false),
            &mut app,
        );
        assert_eq!(
            last_marker_stop_hook_groups(&app.agents[&AgentId(0)].scrollback),
            Some(0),
            "marker starts without hooks"
        );

        let _ = handle_ext_notification(
            &echo_agent_hook_execution_notif("sess-view-hooks", "stop", false),
            &mut app,
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            last_marker_stop_hook_groups(&agent.scrollback),
            Some(1),
            "the batch must merge into the existing marker"
        );
        assert_eq!(
            count_lifecycle_blocks(&agent.scrollback),
            0,
            "no standalone stop block on the live viewer path"
        );

        // A second, differently-named batch of the same turn (stop_failure +
        // stop on error turns) merges too…
        let _ = handle_ext_notification(
            &echo_agent_hook_execution_notif("sess-view-hooks", "stop_failure", false),
            &mut app,
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            last_marker_stop_hook_groups(&agent.scrollback),
            Some(2),
            "a second batch with a new event name must also merge"
        );
        assert_eq!(count_lifecycle_blocks(&agent.scrollback), 0);

        // …but a same-name repeat (e.g. the session-end `stop` batch) does
        // not belong to this marker and stays standalone.
        let _ = handle_ext_notification(
            &echo_agent_hook_execution_notif("sess-view-hooks", "stop", false),
            &mut app,
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(last_marker_stop_hook_groups(&agent.scrollback), Some(2));
        assert_eq!(
            count_lifecycle_blocks(&agent.scrollback),
            1,
            "a repeated same-name batch falls back to the standalone block"
        );
    }

    /// No regression: a running prompt whose terminal did NOT arrive in replay is
    /// still adopted on load (the set is precise — it blocks only ended turns).
    #[test]
    fn session_loaded_adopts_when_prompt_not_terminal_in_replay() {
        use crate::app::dispatch::dispatch;
        use crate::app::actions::{Action, TaskResult};

        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        app.agents.get_mut(&id).unwrap().session.loading_replay = true;
        // A DIFFERENT turn's terminal arrives in replay.
        let _ = handle_ext_notification(
            &echo_agent_turn_completed_notif("sess-1", "p-old", "end_turn", true),
            &mut app,
        );

        dispatch(
            Action::TaskComplete(TaskResult::SessionLoaded {
                agent_id: id,
                session_id: acp::SessionId::new("sess-1"),
                models: None,
                code_restored: false,
                restore_summary: None,
                restore_degree: None,
                running_prompt_id: Some("p-run".to_string()),
                scheduler_background_loops: None,
            }),
            &mut app,
        );

        let agent = &app.agents[&id];
        assert_eq!(
            agent.session.current_prompt_id.as_deref(),
            Some("p-run"),
            "a running prompt with no terminal-in-replay must still be adopted"
        );
        assert!(
            agent.session.state.is_turn_running(),
            "adopting an in-flight turn on load must enter TurnRunning"
        );
    }

    #[test]
    fn viewer_turn_anchor_backdates_from_turn_start_ms() {
        use std::time::Duration;
        // A turnStartMs ~10s in the past anchors ~10s ago.
        let ts = chrono::Utc::now().timestamp_millis() - 10_000;
        let elapsed = viewer_turn_anchor(Some(ts)).elapsed();
        assert!(
            elapsed >= Duration::from_secs(9) && elapsed <= Duration::from_secs(20),
            "anchor must be ~10s in the past, got {elapsed:?}"
        );
        // No turnStartMs (older shell) anchors at ~now.
        assert!(
            viewer_turn_anchor(None).elapsed() < Duration::from_secs(1),
            "no turnStartMs must anchor at ~now"
        );
        // A future turnStartMs (clock skew) clamps to now rather than panicking.
        let future = chrono::Utc::now().timestamp_millis() + 60_000;
        assert!(
            viewer_turn_anchor(Some(future)).elapsed() < Duration::from_secs(1),
            "a future turnStartMs must clamp to now"
        );
    }

    /// A broadcast dropping a painted-pending row retires its block; rows
    /// draining to running keep theirs.
    #[test]
    fn queue_changed_removal_retires_painted_block() {
        let mut app = make_app_with_agent("sess-1");
        // Confirmed row p1, painted at send-now dispatch.
        assert!(handle_ext_notification(
            &queue_changed_ext("sess-1", &["p1"]),
            &mut app
        ));
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.session.state = AgentState::TurnRunning;
            agent.session.current_prompt_id = Some("p-running".into());
            crate::app::dispatch::arm_send_now_and_paint(agent, "p1", None);
            assert!(agent.send_now_painted_blocks.contains_key("p1"));
        }
        // Another client removes p1 (broadcast without it, not running).
        assert!(handle_ext_notification(
            &queue_changed_ext("sess-1", &[]),
            &mut app
        ));
        let agent = &app.agents[&AgentId(0)];
        assert!(
            agent.send_now_painted_blocks.is_empty(),
            "removed row's painted entry must retire"
        );
        // p1 drains to running instead: block survives (exempt as running).
        let mut app = make_app_with_agent("sess-1");
        assert!(handle_ext_notification(
            &queue_changed_ext("sess-1", &["p1"]),
            &mut app
        ));
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.session.state = AgentState::TurnRunning;
            agent.session.current_prompt_id = Some("p-running".into());
            agent.note_self_originated_prompt("p1");
            crate::app::dispatch::arm_send_now_and_paint(agent, "p1", None);
        }
        assert!(handle_ext_notification(
            &queue_changed_running("sess-1", &[], Some("p1")),
            &mut app
        ));
        assert!(
            app.agents[&AgentId(0)]
                .send_now_painted_blocks
                .contains_key("p1"),
            "a row draining to running keeps its painted block for the adoption"
        );
    }
