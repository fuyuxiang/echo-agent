#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    #[test]
    fn goal_updated_ignores_unknown_json_fields_via_serde() {
        // Serde-side half of the forward-compat story: a payload that
        // carries an extra JSON field absent on today's
        // `SessionUpdate::GoalUpdated` (no `deny_unknown_fields` on the
        // variant) must still deserialize and drive a full
        // `GoalDisplayState`. This guards against someone later adding
        // `#[serde(deny_unknown_fields)]` to the variant, which would
        // silently break wire compatibility with older shells.
        //
        // The complementary Rust-level half — that the destructure with
        // trailing `..` keeps absent additive `Option<T>` fields landing
        // as `None` in the mapped `GoalDisplayState` — is exercised by
        // `goal_updated_absent_optional_fields_deserialize_to_none`.
        let mut app = make_app_with_agent("sess-A");

        let raw_payload = serde_json::json!({
            "sessionId": "sess-A",
            "update": {
                "sessionUpdate": "goal_updated",
                "goal_id": "g-ext",
                "objective": "build forward-compat tolerance",
                "status": "active",
                "phase": "executing",
                "token_budget": 200_000,
                "tokens_used": 12_345,
                "elapsed_ms": 750,
                "total_deliverables": 2,
                "completed_deliverables": 1,
                "current_deliverable_idx": 1,
                "current_deliverable_title": "Wire compat",
                "current_subagent_role": "verifier",
                "total_worker_rounds": 5,
                "total_verify_rounds": 2,
                "token_baseline": 100,
                "finished_subagent_tokens": 99,
                "live_subagent_tokens": 4_321,
                "live_tokens_by_model": [["echo-agent-4", 6_000], ["echo-agent-3", 4_000]],
                "live_context_pct": 42,
                "live_turn_count": 7,
                "live_tool_call_count": 11,
                "last_event": "verify_started",
                "last_event_detail": "round 2 of 3",
                "last_event_timestamp": "2026-05-24T00:00:00Z",
                // Field absent on today's `SessionUpdate::GoalUpdated` — simulates
                // a future shell adding a new wire field. With trailing `..` in
                // the destructure and no `deny_unknown_fields` on the variant,
                // this must parse and the pager must still produce a
                // GoalDisplayState mapped from the known subset.
                "future_field_for_pr5": "ignored-by-todays-pager"
            }
        });
        let raw = serde_json::value::to_raw_value(&raw_payload).unwrap();
        let request = acp::ExtNotification::new("echo.agent/session_notification", raw.into());
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let msg = AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
            request,
            response_tx: tx,
        });

        let affected = handle(msg, &mut app);
        assert!(
            affected,
            "GoalUpdated for the active agent must request a redraw"
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let goal = agent
            .goal_state
            .as_ref()
            .expect("GoalUpdated should populate goal_state even with unknown wire fields");
        assert_eq!(goal.goal_id, "g-ext");
        assert_eq!(goal.objective, "build forward-compat tolerance");
        assert_eq!(goal.status, GoalDisplayStatus::Active);
        assert_eq!(goal.phase, GoalDisplayPhase::Executing);
        assert_eq!(goal.token_budget, Some(200_000));
        assert_eq!(goal.tokens_used, 12_345);
        assert_eq!(goal.elapsed_ms, 750);
        assert_eq!(goal.total_deliverables, 2);
        assert_eq!(goal.completed_deliverables, 1);
        assert_eq!(goal.current_deliverable_id, Some(1));
        assert_eq!(
            goal.current_deliverable_title.as_deref(),
            Some("Wire compat")
        );
        assert_eq!(goal.current_subagent_role.as_deref(), Some("verifier"));
        assert_eq!(goal.total_worker_rounds, 5);
        assert_eq!(goal.total_verify_rounds, 2);
        assert_eq!(goal.token_baseline, 100);
        assert_eq!(goal.finished_subagent_tokens, 99);
        assert_eq!(goal.live_subagent_tokens, Some(4_321));
        assert_eq!(
            goal.live_tokens_by_model,
            vec![("echo-agent-4".to_owned(), 6_000), ("echo-agent-3".to_owned(), 4_000)],
            "populated per-model breakdown must round-trip wire->display"
        );
        assert_eq!(goal.live_context_pct, Some(42));
        assert_eq!(goal.live_turn_count, Some(7));
        assert_eq!(goal.live_tool_call_count, Some(11));
        assert_eq!(goal.last_event.as_deref(), Some("verify_started"));
        assert_eq!(goal.last_event_detail.as_deref(), Some("round 2 of 3"));
        assert_eq!(
            goal.last_event_timestamp.as_deref(),
            Some("2026-05-24T00:00:00Z")
        );
        assert_eq!(goal.pause_message, None);
        // Classifier fields default to `None` / `false` when absent.
        assert_eq!(goal.classifier_runs_attempted, None);
        assert_eq!(goal.classifier_max_runs, None);
        assert_eq!(goal.last_classifier_verdict, None);
        assert_eq!(goal.last_classifier_details_path, None);
        assert!(!goal.verifying_completion);
        assert!(!goal.planning);
        assert!(
            goal.deliverables.is_empty(),
            "deliverables is wire-compat-only in the simplified goal model"
        );
    }

    #[test]
    fn goal_complete_transition_pushes_end_to_end_marker_once() {
        let mut app = make_app_with_agent("sess-A");

        let send = |app: &mut AppView, status: &str, elapsed_ms: u64| {
            let raw_payload = serde_json::json!({
                "sessionId": "sess-A",
                "update": {
                    "sessionUpdate": "goal_updated",
                    "goal_id": "g1",
                    "objective": "obj",
                    "status": status,
                    "phase": "executing",
                    "tokens_used": 0,
                    "elapsed_ms": elapsed_ms,
                    "total_deliverables": 0,
                    "completed_deliverables": 0,
                    "total_worker_rounds": 0,
                    "total_verify_rounds": 0,
                    "token_baseline": 0,
                    "finished_subagent_tokens": 0,
                }
            });
            let raw = serde_json::value::to_raw_value(&raw_payload).unwrap();
            let (tx, _rx) = tokio::sync::oneshot::channel();
            handle(
                AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
                    request: acp::ExtNotification::new("echo.agent/session_notification", raw.into()),
                    response_tx: tx,
                }),
                app,
            );
        };

        let goal_markers = |app: &AppView| -> Vec<std::time::Duration> {
            let sb = &app.agents.get(&AgentId(0)).unwrap().scrollback;
            (0..sb.len())
                .filter_map(|i| match sb.get(i).map(|e| &e.block) {
                    Some(RenderBlock::SessionEvent(b)) => match &b.event {
                        SessionEvent::GoalCompleted { elapsed } => Some(*elapsed),
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        };

        send(&mut app, "active", 1_000);
        assert!(goal_markers(&app).is_empty(), "no marker while Active");

        send(&mut app, "complete", 619_000);
        assert_eq!(
            goal_markers(&app),
            vec![std::time::Duration::from_millis(619_000)],
            "transition to Complete pushes one e2e marker with the goal's total time",
        );

        // A repeat Complete update (e.g. a late notification) must not
        // duplicate the marker.
        send(&mut app, "complete", 620_000);
        assert_eq!(
            goal_markers(&app).len(),
            1,
            "repeat Complete must not push a second marker",
        );
    }

    #[test]
    fn goal_elapsed_is_monotonic_across_updates() {
        // The displayed elapsed must never tick backward when a notification's
        // authoritative base is below the already-extrapolated value;
        // `elapsed_floor_ms` clamps it.
        let mut app = make_app_with_agent("sess-A");
        assert!(send_goal_update(&mut app, "g1", "active", 10_000));
        let a = app
            .agents
            .get(&AgentId(0))
            .unwrap()
            .goal_state
            .as_ref()
            .unwrap()
            .live_elapsed_ms();
        assert!(a >= 10_000);

        // Same goal, but a LOWER authoritative base (extrapolation outran the
        // shell's flush point).
        send_goal_update(&mut app, "g1", "active", 8_000);
        let b = app
            .agents
            .get(&AgentId(0))
            .unwrap()
            .goal_state
            .as_ref()
            .unwrap()
            .live_elapsed_ms();
        assert!(b >= a, "elapsed must not tick backward: {b} < {a}");
        assert!(b >= 10_000);
    }

    #[test]
    fn cleared_goal_is_not_resurrected_by_late_update() {
        // After a goal is cleared, a late in-flight GoalUpdated for the same
        // goal_id (queued before the clear) must be dropped so the "Done"
        // chip / modal stay cleared and don't resurrect.
        let mut app = make_app_with_agent("sess-A");
        send_goal_update(&mut app, "g1", "complete", 5_000);
        assert!(
            app.agents.get(&AgentId(0)).unwrap().goal_state.is_some(),
            "goal present after complete"
        );

        // Clear (the cleared event itself carries an empty goal_id).
        send_goal_update(&mut app, "", "cleared", 0);
        assert!(
            app.agents.get(&AgentId(0)).unwrap().goal_state.is_none(),
            "chip cleared on cleared status"
        );

        // A stale late update for the cleared goal must not resurrect it.
        let affected = send_goal_update(&mut app, "g1", "complete", 5_000);
        assert!(
            app.agents.get(&AgentId(0)).unwrap().goal_state.is_none(),
            "cleared goal must not resurrect"
        );
        assert!(!affected, "ignored stale update must not request a redraw");
    }

    #[test]
    fn new_goal_after_clear_is_not_suppressed() {
        // A genuinely new goal (different id) after a clear must start
        // normally — the cleared-id guard only drops the SAME id.
        let mut app = make_app_with_agent("sess-A");
        send_goal_update(&mut app, "g1", "active", 1_000);
        send_goal_update(&mut app, "", "cleared", 0);
        assert!(send_goal_update(&mut app, "g2", "active", 500));
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent.goal_state.as_ref().expect("new goal present").goal_id,
            "g2"
        );
    }

    #[test]
    fn goal_switch_resets_elapsed_floor() {
        // A NEW goal (different id) must start its own clock and NOT inherit
        // the prior goal's carried elapsed floor.
        let mut app = make_app_with_agent("sess-A");
        send_goal_update(&mut app, "g1", "active", 10_000);
        // Switch directly to a different goal with a small elapsed base.
        send_goal_update(&mut app, "g2", "active", 500);
        let elapsed = app
            .agents
            .get(&AgentId(0))
            .unwrap()
            .goal_state
            .as_ref()
            .unwrap()
            .live_elapsed_ms();
        assert!(
            elapsed < 5_000,
            "new goal must start from its own base, not the prior 10s floor: {elapsed}"
        );
    }

    #[test]
    fn goal_updated_resolves_details_path_existence_on_receipt() {
        // The handler resolves last_classifier_details_path's existence ONCE
        // on receipt into the cached bool (no per-frame stat).
        let mut app = make_app_with_agent("sess-A");

        // A real on-disk path → cached exists = true.
        let f = tempfile::NamedTempFile::new().unwrap();
        let real_path = f.path().to_string_lossy().into_owned();
        let mut update = goal_update_value("g1", "active", 0);
        update["last_classifier_details_path"] = serde_json::json!(real_path);
        dispatch_goal_update(&mut app, update);
        let g = app
            .agents
            .get(&AgentId(0))
            .unwrap()
            .goal_state
            .as_ref()
            .unwrap();
        assert!(
            g.last_classifier_details_exists,
            "existing details path must cache exists = true"
        );
        assert_eq!(
            g.last_classifier_details_path.as_deref(),
            Some(real_path.as_str())
        );

        // A missing path → cached exists = false (modal renders "(unavailable)").
        let mut update = goal_update_value("g1", "active", 0);
        update["last_classifier_details_path"] = serde_json::json!("/no/such/details-xyz.md");
        dispatch_goal_update(&mut app, update);
        let g = app
            .agents
            .get(&AgentId(0))
            .unwrap()
            .goal_state
            .as_ref()
            .unwrap();
        assert!(
            !g.last_classifier_details_exists,
            "missing details path must cache exists = false"
        );
    }

    #[test]
    fn goal_updated_absent_optional_fields_deserialize_to_none() {
        // Rust-level forward-compat half: every additive
        // `Option<T>` field on `SessionUpdate::GoalUpdated` is allowed to
        // be omitted from the wire payload and must surface as `None` in
        // the destructured arm — i.e. the pager keeps mapping the known
        // subset cleanly when the shell-side struct grows or when an
        // older shell omits newer optional fields. Drop a handful of
        // optional keys from the payload and assert they materialise as
        // `None` on the resulting `GoalDisplayState`.
        let mut app = make_app_with_agent("sess-A");

        let raw_payload = serde_json::json!({
            "sessionId": "sess-A",
            "update": {
                "sessionUpdate": "goal_updated",
                "goal_id": "g-min",
                "objective": "minimal payload",
                "status": "active",
                "phase": "idle",
                // token_budget omitted — Option<i64> must default to None.
                "tokens_used": 0,
                "elapsed_ms": 0,
                "total_deliverables": 0,
                "completed_deliverables": 0,
                // current_deliverable_idx omitted — Option<u32> -> None.
                // current_deliverable_title omitted — Option<String> -> None.
                // current_subagent_role omitted — Option<String> -> None.
                "total_worker_rounds": 0,
                "total_verify_rounds": 0,
                "token_baseline": 0,
                "finished_subagent_tokens": 0,
                // live_subagent_tokens omitted — Option<u64> -> None.
                // live_context_pct omitted — Option<u8> -> None.
                // live_turn_count omitted — Option<u32> -> None.
                // live_tool_call_count omitted — Option<u32> -> None.
                // last_event omitted — Option<String> -> None.
                // last_event_detail omitted — Option<String> -> None.
                // last_event_timestamp omitted — Option<String> -> None.
                // pause_message omitted — Option<String> -> None.
            }
        });
        let raw = serde_json::value::to_raw_value(&raw_payload).unwrap();
        let request = acp::ExtNotification::new("echo.agent/session_notification", raw.into());
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let msg = AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
            request,
            response_tx: tx,
        });

        let affected = handle(msg, &mut app);
        assert!(
            affected,
            "minimal GoalUpdated for the active agent must request a redraw"
        );

        let agent = app.agents.get(&AgentId(0)).unwrap();
        let goal = agent
            .goal_state
            .as_ref()
            .expect("GoalUpdated must populate goal_state even with all Option fields omitted");

        // Required fields landed as sent.
        assert_eq!(goal.goal_id, "g-min");
        assert_eq!(goal.objective, "minimal payload");
        assert_eq!(goal.status, GoalDisplayStatus::Active);
        assert_eq!(goal.phase, GoalDisplayPhase::Idle);
        assert_eq!(goal.tokens_used, 0);
        assert_eq!(goal.elapsed_ms, 0);
        assert_eq!(goal.total_deliverables, 0);
        assert_eq!(goal.completed_deliverables, 0);
        assert_eq!(goal.total_worker_rounds, 0);
        assert_eq!(goal.total_verify_rounds, 0);
        assert_eq!(goal.token_baseline, 0);
        assert_eq!(goal.finished_subagent_tokens, 0);

        // Every omitted Option<T> wire field must surface as None — this
        // is the property that keeps the destructure stable as the shell
        // grows additive optional fields.
        assert_eq!(goal.token_budget, None, "token_budget");
        assert_eq!(goal.current_deliverable_id, None, "current_deliverable_id");
        assert_eq!(
            goal.current_deliverable_title, None,
            "current_deliverable_title"
        );
        assert_eq!(goal.current_subagent_role, None, "current_subagent_role");
        assert_eq!(goal.live_subagent_tokens, None, "live_subagent_tokens");
        assert!(
            goal.live_tokens_by_model.is_empty(),
            "omitted live_tokens_by_model must default to empty via #[serde(default)]"
        );
        assert_eq!(goal.live_context_pct, None, "live_context_pct");
        assert_eq!(goal.live_turn_count, None, "live_turn_count");
        assert_eq!(goal.live_tool_call_count, None, "live_tool_call_count");
        assert_eq!(goal.last_event, None, "last_event");
        assert_eq!(goal.last_event_detail, None, "last_event_detail");
        assert_eq!(goal.last_event_timestamp, None, "last_event_timestamp");
        assert_eq!(goal.pause_message, None, "pause_message");
        assert_eq!(
            goal.classifier_runs_attempted, None,
            "classifier_runs_attempted"
        );
        assert_eq!(goal.classifier_max_runs, None, "classifier_max_runs");
        assert_eq!(
            goal.last_classifier_verdict, None,
            "last_classifier_verdict"
        );
        assert_eq!(
            goal.last_classifier_details_path, None,
            "last_classifier_details_path"
        );
        assert!(
            !goal.verifying_completion,
            "verifying_completion defaults to false"
        );
        assert!(!goal.planning, "planning defaults to false");
        assert!(
            goal.deliverables.is_empty(),
            "deliverables is wire-compat-only in the simplified goal model"
        );
    }


    fn workflow_update_value(
        run_id: &str,
        name: &str,
        status: &str,
        foreground: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "sessionUpdate": "workflow_updated",
            "run_id": run_id,
            "revision": 0,
            "name": name,
            "objective": "obj",
            "status": status,
            "foreground": foreground,
            "elapsed_ms": 1_000,
        })
    }

    fn send_workflow_update(
        app: &mut AppView,
        run_id: &str,
        name: &str,
        status: &str,
        foreground: bool,
    ) -> bool {
        dispatch_goal_update(app, workflow_update_value(run_id, name, status, foreground))
    }

    fn send_revisioned_workflow_update(
        app: &mut AppView,
        run_id: &str,
        name: &str,
        status: &str,
        foreground: bool,
        revision: u64,
    ) -> bool {
        let mut update = workflow_update_value(run_id, name, status, foreground);
        update["revision"] = serde_json::json!(revision);
        dispatch_goal_update(app, update)
    }

    #[test]
    fn workflow_updated_never_owns_goal_ui() {
        let mut app = make_app_with_agent("sess-A");
        send_workflow_update(&mut app, "wf_goal", "goal", "active", true);
        assert!(app.agents[&AgentId(0)].goal_state.is_none());
        assert_eq!(app.agents[&AgentId(0)].workflow_runs.len(), 1);
    }

    #[test]
    fn workflow_updates_bypass_global_echo_agent_highwater_and_use_run_revision() {
        let mut app = make_app_with_agent("sess-A");
        let id = AgentId(0);
        app.agents.get_mut(&id).unwrap().last_applied_echo_agent_event_seq = Some(100);

        let mut update = workflow_update_value("wf", "deep-research", "active", false);
        update["revision"] = serde_json::json!(1);
        let raw_payload = serde_json::json!({
            "sessionId": "sess-A",
            "update": update,
            "_meta": { "eventId": "sess-A-5" },
        });
        let raw = serde_json::value::to_raw_value(&raw_payload).unwrap();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let affected = handle(
            AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
                request: acp::ExtNotification::new("echo.agent/session_notification", raw.into()),
                response_tx: tx,
            }),
            &mut app,
        );

        assert!(affected, "per-run workflow revision must win over the global highwater");
        assert_eq!(app.agents[&id].workflow_runs[0].run_id, "wf");
        assert_eq!(app.agents[&id].last_applied_echo_agent_event_seq, Some(100));
        assert_eq!(app.agents[&id].last_seen_event_id.as_deref(), Some("sess-A-5"));
    }

    #[test]
    fn workflow_revisions_reject_regression_and_resurrection() {
        let mut app = make_app_with_agent("sess-A");
        send_revisioned_workflow_update(&mut app, "wf", "deep-research", "complete", false, 3);
        assert!(!send_revisioned_workflow_update(
            &mut app, "wf", "deep-research", "active", false, 2,
        ));
        assert_eq!(app.agents[&AgentId(0)].workflow_runs[0].status, "complete");
        send_revisioned_workflow_update(&mut app, "wf", "", "cleared", false, 4);
        assert!(!send_revisioned_workflow_update(
            &mut app, "wf", "deep-research", "complete", false, 3,
        ));
        assert!(app.agents[&AgentId(0)].workflow_runs.is_empty());
    }

    #[test]
    fn workflow_snapshot_derives_active_count_from_roster() {
        let mut app = make_app_with_agent("sess-A");
        let mut update = workflow_update_value("wf", "deep-research", "active", false);
        update["revision"] = serde_json::json!(1);
        update["active_agents"] = serde_json::json!(99);
        update["agents"] = serde_json::json!([
            { "agent_id": "a1", "label": "one", "state": "running" },
            { "agent_id": "a2", "label": "two", "state": "done" }
        ]);
        dispatch_goal_update(&mut app, update);
        assert_eq!(app.agents[&AgentId(0)].workflow_runs[0].active_agents, 1);
    }

    #[test]
    fn workflow_block_animates_while_running_and_finishes_on_terminal() {
        let mut app = make_app_with_agent("sess-A");
        send_workflow_update(&mut app, "wf_bg", "deep-research", "active", false);
        {
            let agent = app.agents.get(&AgentId(0)).unwrap();
            let eid = *agent.workflow_blocks.get("wf_bg").expect("live block id");
            let entry = agent.scrollback.get_by_id(eid).expect("entry");
            assert!(entry.is_running, "live workflow block must animate");
        }
        send_workflow_update(&mut app, "wf_bg", "deep-research", "cancelled", false);
        {
            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert!(
                agent.workflow_blocks.is_empty(),
                "terminal status must drop the run's block id"
            );
            let sb = &agent.scrollback;
            let entry = (0..sb.len())
                .filter_map(|i| sb.get(i))
                .find(|e| matches!(e.block, RenderBlock::Workflow(_)))
                .expect("workflow block stays in scrollback as history");
            assert!(
                !entry.is_running,
                "finish_running must clear the running flag on terminal status"
            );
            if let RenderBlock::Workflow(ref wb) = entry.block {
                assert!(
                    matches!(
                        wb.status,
                        crate::scrollback::blocks::WorkflowBlockStatus::Cancelled { .. }
                    ),
                    "wire cancelled maps to the Cancelled block status"
                );
            }
        }
    }

    fn count_workflow_blocks(agent: &AgentView) -> usize {
        let sb = &agent.scrollback;
        (0..sb.len())
            .filter_map(|i| sb.get(i))
            .filter(|e| matches!(e.block, RenderBlock::Workflow(_)))
            .count()
    }

    #[test]
    fn failed_reconnect_reload_restores_runs_and_avoids_duplicate_workflow_block() {
        let mut app = make_app_with_agent("sess-A");
        let id = AgentId(0);

        assert!(send_revisioned_workflow_update(&mut app, "wf", "deep-research", "active", false, 3));
        assert_eq!(app.agents[&id].workflow_runs.len(), 1);
        assert_eq!(count_workflow_blocks(&app.agents[&id]), 1);
        let block_id = *app.agents[&id].workflow_blocks.get("wf").expect("live block id");

        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.begin_session_reload(1);
            assert!(agent.workflow_runs.is_empty(), "staging clears the run list");
            assert!(agent.finish_session_reload(1, false));
        }

        assert_eq!(
            app.agents[&id].workflow_runs.len(),
            1,
            "run list restored after a failed reload"
        );
        assert_eq!(
            app.agents[&id].workflow_blocks.get("wf").copied(),
            Some(block_id),
            "block map restored, still pointing at the restored scrollback block"
        );

        assert!(
            !send_revisioned_workflow_update(&mut app, "wf", "deep-research", "active", false, 2),
            "restored revision highwater still rejects a regression"
        );
        assert!(send_revisioned_workflow_update(&mut app, "wf", "deep-research", "complete", false, 4));
        assert_eq!(
            count_workflow_blocks(&app.agents[&id]),
            1,
            "restored block map prevents a duplicate workflow history block"
        );
    }

    #[test]
    fn cleared_workflow_update_bypasses_revision_highwater() {
        let mut app = make_app_with_agent("sess-A");
        let id = AgentId(0);

        assert!(send_revisioned_workflow_update(&mut app, "wf", "deep-research", "active", false, 5));
        assert_eq!(app.agents[&id].workflow_runs.len(), 1);

        assert!(send_workflow_update(&mut app, "wf", "deep-research", "cleared", false));
        assert!(
            app.agents[&id].workflow_runs.is_empty(),
            "cleared removes the run despite a lower/default revision"
        );
        assert!(
            !app.agents[&id].workflow_blocks.contains_key("wf"),
            "cleared drops the live block id"
        );
        assert!(
            app.agents[&id].cleared_workflow_runs.contains("wf"),
            "clear tombstone recorded"
        );
    }

    #[test]
    fn first_seen_terminal_workflow_emits_history_block() {
        let mut app = make_app_with_agent("sess-A");
        assert!(send_workflow_update(&mut app, "wf_done", "deep-research", "complete", false));

        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(agent.workflow_runs.len(), 1, "run is tracked in the projection");
        assert_eq!(
            count_workflow_blocks(agent),
            1,
            "a first-seen terminal status must still emit a workflow history block"
        );
        let sb = &agent.scrollback;
        let entry = (0..sb.len())
            .filter_map(|i| sb.get(i))
            .find(|e| matches!(e.block, RenderBlock::Workflow(_)))
            .expect("workflow history block");
        assert!(!entry.is_running, "the history block is finished, not animating");
        if let RenderBlock::Workflow(ref wb) = entry.block {
            assert!(
                matches!(wb.status, crate::scrollback::blocks::WorkflowBlockStatus::Done { .. }),
                "wire complete maps to the Done block status"
            );
        }
        assert!(
            !agent.workflow_blocks.contains_key("wf_done"),
            "terminal status drops the live block id (kept only as history)"
        );
    }
