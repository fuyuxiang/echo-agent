#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    #[test]
    fn handle_updates_total_tokens_used_for_active_agent() {
        let mut app = make_app_with_agent("sess-1");
        assert!(app.agents.get(&AgentId(0)).unwrap().context_state.is_none());

        let _ = handle(make_token_notification_message("sess-1", 12_345), &mut app);

        assert_eq!(
            app.agents
                .get(&AgentId(0))
                .unwrap()
                .context_state
                .as_ref()
                .map(|c| c.used),
            Some(12_345),
        );
    }

    #[test]
    fn handle_routes_tokens_to_root_when_session_id_not_yet_set() {
        // Regression: a notification racing ahead of TaskResult::SessionCreated
        // (session_id still None) must update the active agent, not be dropped
        // into the empty subagent_views path.
        let mut app = make_app_with_agent("sess-1");
        app.agents.get_mut(&AgentId(0)).unwrap().session.session_id = None;

        let _ = handle(make_token_notification_message("sess-1", 12_345), &mut app);

        assert_eq!(
            app.agents
                .get(&AgentId(0))
                .unwrap()
                .context_state
                .as_ref()
                .map(|c| c.used),
            Some(12_345),
        );
    }

    #[test]
    fn duplicate_event_id_is_dropped_and_highwater_advances() {
        // Each session/update carries a monotonic eventId; live + replay of the
        // same event share it. A client that receives an event twice must render
        // it once (this is what eliminates the driver-side duplication when a
        // second client opens the same session). Per-session events arrive in
        // increasing order, so the pager keeps a highwater and drops anything
        // `<=` it. Updates without an eventId still apply (back-compat).
        let mut app = make_app_with_agent("sess-dedup");
        let id = AgentId(0);

        // First event applies (active agent → affected==true) and sets highwater.
        let a1 = handle(
            make_agent_chunk_with_event("sess-dedup", "hello", "p1", Some("sess-dedup-5")),
            &mut app,
        );
        assert!(a1, "first event must apply");
        assert_eq!(app.agents[&id].last_applied_event_seq, Some(5));

        // Exact duplicate eventId → dropped (not affected), highwater unchanged.
        let a2 = handle(
            make_agent_chunk_with_event("sess-dedup", "hello", "p1", Some("sess-dedup-5")),
            &mut app,
        );
        assert!(!a2, "a duplicate eventId must be dropped");
        assert_eq!(app.agents[&id].last_applied_event_seq, Some(5));

        // Stale lower eventId → dropped.
        let a3 = handle(
            make_agent_chunk_with_event("sess-dedup", "hello", "p1", Some("sess-dedup-3")),
            &mut app,
        );
        assert!(!a3, "a lower (already-passed) eventId must be dropped");
        assert_eq!(app.agents[&id].last_applied_event_seq, Some(5));

        // New higher eventId → applies, highwater advances.
        let a4 = handle(
            make_agent_chunk_with_event("sess-dedup", "world", "p1", Some("sess-dedup-9")),
            &mut app,
        );
        assert!(a4, "a new (higher) eventId must apply");
        assert_eq!(app.agents[&id].last_applied_event_seq, Some(9));

        // No eventId (older shell) → always applies; highwater untouched.
        let a5 = handle(
            make_agent_chunk_with_event("sess-dedup", "again", "p1", None),
            &mut app,
        );
        assert!(
            a5,
            "an update without an eventId must still apply (back-compat)"
        );
        assert_eq!(app.agents[&id].last_applied_event_seq, Some(9));
    }

    /// Regression: the per-process `eventId` counter resets each resume,
    /// so replayed history isn't monotonic — replay must bypass the dedup highwater.
    #[test]
    fn replayed_history_with_event_id_resets_does_not_break_resume() {
        let mut app = make_app_with_agent("sess-resume");
        let id = AgentId(0);
        // Replay arrives inside a `session/load` window.
        app.agents.get_mut(&id).unwrap().session.loading_replay = true;

        // eventIds climb (5, 9) then reset below the peak (2, 4): resumed twice.
        for (text, eid) in [
            ("r1-a", "sess-resume-5"),
            ("r1-b", "sess-resume-9"),
            ("r2-a", "sess-resume-2"),
            ("r2-b", "sess-resume-4"),
        ] {
            handle(
                make_agent_chunk_meta("sess-resume", text, "p1", Some(eid), true),
                &mut app,
            );
        }
        assert_eq!(
            app.agents[&id].last_applied_event_seq, None,
            "replay must not seed the dedup highwater"
        );
        assert_eq!(
            app.agents[&id].last_seen_event_id.as_deref(),
            Some("sess-resume-9"),
            "reconnect cursor is forward-only: lower post-reset ids must not regress the highwater"
        );
        // SessionLoaded completes the window.
        app.agents.get_mut(&id).unwrap().session.loading_replay = false;

        assert!(
            handle(
                make_agent_chunk_with_event("sess-resume", "new turn", "p2", Some("sess-resume-1")),
                &mut app,
            ),
            "a live update after resume must render even with a reset-low eventId"
        );
        assert_eq!(app.agents[&id].last_applied_event_seq, Some(1));

        assert!(
            !handle(
                make_agent_chunk_with_event("sess-resume", "dup", "p2", Some("sess-resume-1")),
                &mut app,
            ),
            "a duplicate live eventId is still deduped"
        );
    }

    /// Full-replay reconnect: the pre-outage transcript stays stashed during
    /// the window and the replayed transcript replaces it wholesale on
    /// success.
    #[test]
    fn reconnect_reload_full_replay_replaces_transcript_on_success() {
        let mut app = make_app_with_agent("sess-rc");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent
                .scrollback
                .push_block(RenderBlock::system("pre-outage content"));
            agent.last_seen_event_id = Some("sess-rc-3".into());
            agent.begin_session_reload(1);
            assert!(agent.session.loading_replay);
            assert_eq!(
                agent.scrollback.len(),
                1,
                "staging starts with only the reload placeholder; old content is stashed"
            );
            assert!(scrollback_has_system_text(
                agent,
                "Reloading session after reconnect"
            ));
        }

        assert!(!handle(
            replay_chunk("sess-rc", "h1", "sess-rc-1"),
            &mut app
        ));
        assert!(!handle(
            replay_chunk("sess-rc", "h2", "sess-rc-2"),
            &mut app
        ));

        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.finish_session_reload(1, true));
        assert!(!agent.session.loading_replay);
        assert!(!agent.scrollback.in_batch());
        assert!(
            !agent.scrollback.is_empty(),
            "the replayed transcript is now live"
        );
        assert!(
            !scrollback_has_system_text(agent, "pre-outage content"),
            "full replay drops the pre-outage stash"
        );
        assert!(
            !scrollback_has_system_text(agent, "Reloading session after reconnect"),
            "the reload placeholder is removed at finalize"
        );
        assert_eq!(
            agent.last_seen_event_id.as_deref(),
            Some("sess-rc-3"),
            "forward-only cursor keeps the pre-outage highwater when replayed ids are lower"
        );
        assert!(matches!(agent.session.state, AgentState::Idle));
    }

    /// Failed reconnect reload: the partial replay is discarded and the
    /// pre-outage transcript (plus cursor/highwaters) is restored — the view
    /// must never end up permanently blank.
    ///
    /// Both highwaters are advanced IN-WINDOW (live lines land in staging)
    /// before the failure: a seed-only check would pass even with the restore
    /// deleted, and a stale post-discard highwater silently dedup-drops the
    /// next reload's cursor-tail re-deliveries of the discarded blocks.
    #[test]
    fn reconnect_reload_failure_restores_pre_outage_transcript() {
        let mut app = make_app_with_agent("sess-rc");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent
                .scrollback
                .push_block(RenderBlock::system("pre-outage content"));
            agent.last_seen_event_id = Some("sess-rc-3".into());
            agent.last_applied_event_seq = Some(3);
            agent.last_applied_echo_agent_event_seq = Some(5);
            agent.begin_session_reload(1);
        }

        // Partial replay lands before the load fails...
        assert!(!handle(
            replay_chunk("sess-rc", "h1", "sess-rc-1"),
            &mut app
        ));
        // ...along with live post-cursor traffic on BOTH streams, advancing
        // both highwaters inside the doomed staging.
        let _ = handle(
            make_agent_chunk_with_event("sess-rc", "live tail", "p9", Some("sess-rc-40")),
            &mut app,
        );
        let _ = handle_ext_notification(&echo_agent_model_switch_notif("sess-rc", "sess-rc-30"), &mut app);
        assert_eq!(
            app.agents[&id].last_applied_event_seq,
            Some(40),
            "the live ACP tail advanced its highwater in-window"
        );
        assert_eq!(
            app.agents[&id].last_applied_echo_agent_event_seq,
            Some(30),
            "the live EchoAgent line advanced its highwater in-window"
        );
        // Highest EntryId the discarded staging handed out (its last entry).
        let staged_max_id = {
            let agent = app.agents.get_mut(&id).unwrap();
            let len = agent.scrollback.len();
            agent.scrollback.get(len - 1).unwrap().id
        };

        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.finish_session_reload(1, false));
        assert!(!agent.session.loading_replay);
        assert!(
            scrollback_has_system_text(agent, "pre-outage content"),
            "failure must restore the stashed transcript"
        );
        assert_eq!(agent.scrollback.len(), 1, "partial replay was discarded");
        assert_eq!(
            agent.last_seen_event_id.as_deref(),
            Some("sess-rc-3"),
            "the cursor reverts with the transcript so a later reload doesn't skip events"
        );
        assert_eq!(agent.last_applied_event_seq, Some(3));
        assert_eq!(
            agent.last_applied_echo_agent_event_seq,
            Some(5),
            "the EchoAgent highwater reverts with the transcript — left at 30 it would \
             dedup-drop the next reload's re-delivery of the discarded blocks"
        );
        let next = agent.scrollback.push_block(RenderBlock::system("after"));
        assert!(
            next.value() > staged_max_id.value(),
            "the restored stash must not reuse ids the discarded staging allocated"
        );
    }

    /// Cursor-resolved reconnect: the agent replays nothing (only a live
    /// post-cursor tail). The pre-outage transcript is kept and the tail is
    /// appended below it.
    #[test]
    fn reconnect_reload_cursor_tail_appends_to_kept_transcript() {
        let mut app = make_app_with_agent("sess-rc");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent
                .scrollback
                .push_block(RenderBlock::system("pre-outage content"));
            // A turn was in flight at disconnect: its entry is still running.
            agent.scrollback.set_last_running(true);
            agent.last_seen_event_id = Some("sess-rc-3".into());
            agent.last_applied_event_seq = Some(3);
            agent.begin_session_reload(1);
        }

        // Post-cursor tail arrives as LIVE updates (no isReplay).
        assert!(!handle(
            make_agent_chunk_with_event("sess-rc", "tail", "p2", Some("sess-rc-4")),
            &mut app,
        ));

        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.finish_session_reload(1, true));
        assert!(
            scrollback_has_system_text(agent, "pre-outage content"),
            "cursor-resolved reload keeps the existing transcript"
        );
        assert_eq!(
            agent.scrollback.len(),
            2,
            "the live tail is appended below the kept transcript"
        );
        assert_eq!(
            agent.last_seen_event_id.as_deref(),
            Some("sess-rc-4"),
            "the cursor advances to the applied tail"
        );
        assert_eq!(agent.last_applied_event_seq, Some(4));
        assert!(
            !agent.scrollback.needs_animation(),
            "running entries from the pre-outage turn are finished on merge"
        );

        // Live streaming continues against the merged transcript: finalize
        // force-idled the turn (open streams are deliberately closed — "tools
        // were lost"), so the next delta opens exactly one new entry below
        // the tail and keeps advancing the dedup highwater.
        let len_before = app.agents[&id].scrollback.len();
        assert!(handle(
            make_agent_chunk_with_event("sess-rc", "next turn", "p2", Some("sess-rc-5")),
            &mut app,
        ));
        let agent = app.agents.get_mut(&id).unwrap();
        assert_eq!(agent.scrollback.len(), len_before + 1);
        assert_eq!(agent.last_applied_event_seq, Some(5));
    }

    /// A reconnect superseding an unfinished reload window keeps exactly one
    /// pre-outage stash: the first window's partial replay is discarded, not
    /// stacked, and batch state cannot leak across windows.
    #[test]
    fn superseded_reload_keeps_original_transcript() {
        let mut app = make_app_with_agent("sess-rc");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent
                .scrollback
                .push_block(RenderBlock::system("pre-outage content"));
            agent.begin_session_reload(1);
        }
        assert!(!handle(
            replay_chunk("sess-rc", "h1", "sess-rc-1"),
            &mut app
        ));

        {
            let agent = app.agents.get_mut(&id).unwrap();
            // Second reconnect before the first window finalized (the event
            // loop normally finalizes first; this exercises the defensive
            // path in begin_session_reload).
            agent.begin_session_reload(2);
            assert_eq!(
                agent.scrollback.len(),
                1,
                "gen-1 partial replay is discarded; gen-2 staging holds only the placeholder"
            );
            // Finalizing the dead gen-1 window is rejected.
            assert!(!agent.finish_session_reload(1, true));
            assert!(agent.session.loading_replay);
        }

        // Gen-2 load fails → the ORIGINAL transcript comes back.
        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.finish_session_reload(2, false));
        assert!(scrollback_has_system_text(agent, "pre-outage content"));
        assert_eq!(agent.scrollback.len(), 1);
        assert!(!agent.scrollback.in_batch());
    }

    /// A reconnect that interrupts an in-flight fresh-view load closes out
    /// the load's batch and placeholder before stashing — neither may leak
    /// into the stash (an unbalanced batch would defer rebuilds forever; the
    /// placeholder would linger mid-transcript on a failure restore).
    #[test]
    fn reload_window_supersedes_interrupted_fresh_view_load() {
        let mut app = make_app_with_agent("sess-rc");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent
                .scrollback
                .push_block(RenderBlock::system("pre-outage content"));
            // In-flight fresh-view load: open batch + placeholder + replay flag.
            agent.scrollback.begin_batch();
            let pid = agent
                .scrollback
                .push_block(RenderBlock::system("Loading session sess-rc..."));
            agent.loading_placeholder_id = Some(pid);
            agent.session.loading_replay = true;

            agent.begin_session_reload(1);
            assert_eq!(
                agent.scrollback.len(),
                1,
                "staging holds only the reload placeholder"
            );
        }

        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.finish_session_reload(1, false));
        assert!(
            !agent.scrollback.in_batch(),
            "no batch leaked into the stash"
        );
        assert!(scrollback_has_system_text(agent, "pre-outage content"));
        assert!(
            !scrollback_has_system_text(agent, "Loading session sess-rc"),
            "the interrupted load's placeholder was removed before stashing"
        );
        assert!(agent.loading_placeholder_id.is_none());
    }

    #[test]
    fn full_replay_rebuilds_subagent_row_from_stashed_domain_state() {
        let mut app = make_app_with_agent("sess-subagent-reload");
        let id = AgentId(0);
        let _ = handle(
            make_ext_session_notification_with_method(
                "sess-subagent-reload",
                "echo.agent/session/update",
                test_subagent_spawned("sess-subagent-reload", "child-replay"),
            ),
            &mut app,
        );
        app.agents.get_mut(&id).unwrap().begin_session_reload(1);

        let replay = subagent_ext_replay(
            "sess-subagent-reload",
            serde_json::to_value(test_subagent_spawned(
                "sess-subagent-reload",
                "child-replay",
            ))
            .unwrap(),
            "sess-subagent-reload-1",
        );
        assert!(handle_ext_notification(&replay, &mut app));

        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.finish_session_reload(1, true));
        assert_eq!(agent.scrollback.len(), 1);
        let info = &agent.subagent_sessions["child-replay"];
        assert!(
            info.scrollback_entry_id
                .is_some_and(|entry_id| agent.scrollback.get_by_id(entry_id).is_some()),
            "full replay must rebuild the retained subagent row"
        );
    }

    #[test]
    fn late_grace_replay_rebuilds_subagent_row_after_full_reload_finalize() {
        let mut app = make_app_with_agent("sess-subagent-late-replay");
        let id = AgentId(0);
        let _ = handle(
            make_ext_session_notification_with_method(
                "sess-subagent-late-replay",
                "echo.agent/session/update",
                test_subagent_spawned("sess-subagent-late-replay", "child-late-replay"),
            ),
            &mut app,
        );
        app.agents.get_mut(&id).unwrap().begin_session_reload(1);

        assert!(!handle(
            replay_chunk(
                "sess-subagent-late-replay",
                "replayed transcript",
                "sess-subagent-late-replay-1",
            ),
            &mut app,
        ));
        assert!(app.agents.get_mut(&id).unwrap().finish_session_reload(1, true));

        let replay = subagent_ext_replay(
            "sess-subagent-late-replay",
            serde_json::to_value(test_subagent_spawned(
                "sess-subagent-late-replay",
                "child-late-replay",
            ))
            .unwrap(),
            "sess-subagent-late-replay-2",
        );
        assert!(handle_ext_notification(&replay, &mut app));

        let agent = &app.agents[&id];
        assert_eq!(agent.scrollback.len(), 2);
        let info = &agent.subagent_sessions["child-late-replay"];
        assert!(
            info.scrollback_entry_id
                .is_some_and(|entry_id| agent.scrollback.get_by_id(entry_id).is_some()),
            "late-grace replay must rebuild a subagent row discarded with the stash"
        );
    }

    /// An applied Plan update advances the reconnect cursor like any other
    /// applied arm — leaving it behind would make the tail re-send it.
    #[test]
    fn applied_plan_update_advances_reconnect_cursor() {
        let mut app = make_app_with_agent("sess-plan");
        let id = AgentId(0);

        let _ = handle(plan_update_msg("sess-plan", &[], None, false), &mut app);

        assert_eq!(
            app.agents[&id].last_seen_event_id.as_deref(),
            None,
            "no eventId on the update — cursor untouched"
        );

        let _ = handle(
            plan_update_msg("sess-plan", &[], Some("sess-plan-6"), false),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].last_seen_event_id.as_deref(),
            Some("sess-plan-6")
        );
        assert_eq!(app.agents[&id].last_applied_event_seq, Some(6));
    }

    /// Todo-pane stash semantics across the three reload outcomes.
    #[test]
    fn reload_todo_stash_restores_on_failure() {
        let mut app = make_app_with_agent("sess-todo");
        let id = AgentId(0);
        let _ = handle(
            plan_update_msg("sess-todo", &["old-task"], Some("sess-todo-1"), false),
            &mut app,
        );
        app.agents.get_mut(&id).unwrap().begin_session_reload(1);

        // Replayed Plan overwrites the (fresh) live pane during the window.
        let _ = handle(
            plan_update_msg("sess-todo", &["replayed-task"], Some("sess-todo-2"), true),
            &mut app,
        );
        assert_eq!(todo_contents(&app, id), vec!["replayed-task"]);

        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.finish_session_reload(1, false));
        assert_eq!(
            todo_contents(&app, id),
            vec!["old-task"],
            "failure restores the pre-outage todos with the transcript"
        );
    }

    #[test]
    fn reload_todo_merge_keeps_staging_when_plan_applied_in_window() {
        let mut app = make_app_with_agent("sess-todo");
        let id = AgentId(0);
        let _ = handle(
            plan_update_msg("sess-todo", &["old-task"], Some("sess-todo-1"), false),
            &mut app,
        );
        app.agents.get_mut(&id).unwrap().begin_session_reload(1);

        // A LIVE tail Plan applied in-window is newer than the stash.
        let _ = handle(
            plan_update_msg("sess-todo", &["tail-task"], Some("sess-todo-2"), false),
            &mut app,
        );

        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.finish_session_reload(1, true));
        assert_eq!(
            todo_contents(&app, id),
            vec!["tail-task"],
            "the cursor-merge keeps the in-window plan"
        );
    }

    #[test]
    fn reload_todo_merge_restores_stash_when_no_plan_applied() {
        let mut app = make_app_with_agent("sess-todo");
        let id = AgentId(0);
        let _ = handle(
            plan_update_msg("sess-todo", &["old-task"], Some("sess-todo-1"), false),
            &mut app,
        );
        app.agents.get_mut(&id).unwrap().begin_session_reload(1);

        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.finish_session_reload(1, true));
        assert_eq!(
            todo_contents(&app, id),
            vec!["old-task"],
            "a cursor-merge without an in-window plan keeps the pre-outage todos"
        );
    }

    /// EchoAgent updates dedup on their OWN per-session `eventId` highwater: a
    /// re-delivered live copy (cursor-tail overlap when stamp order and file
    /// order diverge, leader fan-out) is dropped instead of re-applied — the
    /// EchoAgent arms have no other dedup. Replay stays exempt.
    #[test]
    fn echo_agent_session_update_dedup_drops_already_applied_event() {
        let mut app = make_app_with_agent("sess-xdup");
        let id = AgentId(0);

        assert!(handle_ext_notification(
            &echo_agent_model_switch_notif("sess-xdup", "sess-xdup-10"),
            &mut app
        ));
        assert_eq!(app.agents[&id].scrollback.len(), 1);
        assert_eq!(app.agents[&id].last_applied_echo_agent_event_seq, Some(10));

        // Exact re-delivery: dropped, nothing re-applied, cursor unchanged.
        assert!(!handle_ext_notification(
            &echo_agent_model_switch_notif("sess-xdup", "sess-xdup-10"),
            &mut app
        ));
        assert_eq!(
            app.agents[&id].scrollback.len(),
            1,
            "a duplicate EchoAgent event must not push a second block"
        );
        assert_eq!(
            app.agents[&id].last_seen_event_id.as_deref(),
            Some("sess-xdup-10")
        );

        // A newer event still applies.
        assert!(handle_ext_notification(
            &echo_agent_model_switch_notif("sess-xdup", "sess-xdup-11"),
            &mut app
        ));
        assert_eq!(app.agents[&id].scrollback.len(), 2);
        assert_eq!(app.agents[&id].last_applied_echo_agent_event_seq, Some(11));

        // Lower-stale re-delivery (an already-applied lower id re-sent by
        // the cursor tail, e.g. goal mode) is dropped too — `<=`, not just
        // equality.
        assert!(!handle_ext_notification(
            &echo_agent_model_switch_notif("sess-xdup", "sess-xdup-9"),
            &mut app
        ));
        assert_eq!(
            app.agents[&id].scrollback.len(),
            2,
            "a stale lower-id EchoAgent event must not push a block"
        );
        assert_eq!(app.agents[&id].last_applied_echo_agent_event_seq, Some(11));
    }

    /// An unhandled EchoAgent kind (the default `_` arm) leaves no trace, so it must
    /// NOT advance the reconnect cursor or the dedup highwater — a cursor
    /// reconnect must still re-deliver it. An applied kind advances both.
    #[test]
    fn unhandled_echo_agent_update_does_not_advance_cursor_or_highwater() {
        let mut app = make_app_with_agent("sess-ig");
        let id = AgentId(0);

        assert!(!handle_ext_notification(
            &echo_agent_unhandled_notif("sess-ig", "sess-ig-7"),
            &mut app
        ));
        assert_eq!(
            app.agents[&id].last_seen_event_id, None,
            "an unhandled EchoAgent update must not advance the reconnect cursor"
        );
        assert_eq!(
            app.agents[&id].last_applied_echo_agent_event_seq, None,
            "an unhandled EchoAgent update must not advance the dedup highwater"
        );

        // An applied kind (ModelAutoSwitched) advances both.
        assert!(handle_ext_notification(
            &echo_agent_model_switch_notif("sess-ig", "sess-ig-8"),
            &mut app
        ));
        assert_eq!(
            app.agents[&id].last_seen_event_id.as_deref(),
            Some("sess-ig-8")
        );
        assert_eq!(app.agents[&id].last_applied_echo_agent_event_seq, Some(8));
    }

    /// Split-highwater regression: a fresh direct-emitted EchoAgent id must NOT
    /// make a queued lower-id ACP chunk look stale. EchoAgent lines bypass the
    /// agent's FIFO pipeline, so this ordering happens routinely (goal mode,
    /// subagent progress while the parent streams) — a shared highwater
    /// would silently drop the late chunk (live-text loss).
    #[test]
    fn direct_echo_agent_event_does_not_shoot_down_delayed_acp_chunk() {
        let mut app = make_app_with_agent("sess-split");
        let id = AgentId(0);

        // Direct EchoAgent emission stamped N+1 arrives first.
        assert!(handle_ext_notification(
            &echo_agent_model_switch_notif("sess-split", "sess-split-21"),
            &mut app
        ));
        assert_eq!(app.agents[&id].last_applied_echo_agent_event_seq, Some(21));

        // The delayed ACP chunk stamped N arrives after — it must render.
        let len_before = app.agents[&id].scrollback.len();
        assert!(
            handle(
                make_agent_chunk_with_event(
                    "sess-split",
                    "delayed text",
                    "p1",
                    Some("sess-split-20")
                ),
                &mut app,
            ),
            "the lower-id ACP chunk must apply"
        );
        assert_eq!(
            app.agents[&id].scrollback.len(),
            len_before + 1,
            "the chunk's text must render — a shared highwater would have dropped it"
        );
        assert_eq!(
            app.agents[&id].last_applied_event_seq,
            Some(20),
            "the ACP highwater is seeded by the ACP stream only"
        );
        assert_eq!(
            app.agents[&id].last_applied_echo_agent_event_seq,
            Some(21),
            "…and the ACP apply must not clobber the EchoAgent highwater either"
        );
    }

    /// The bg-task stdout arm advances the reconnect cursor like the other
    /// applied arms — a lagging cursor re-delivers the chunk, and after a
    /// full-replay swap the highwater (deliberately unseeded by replay)
    /// cannot absorb it.
    #[test]
    fn applied_bg_stdout_update_advances_reconnect_cursor() {
        let mut app = make_app_with_agent("sess-bg");
        let id = AgentId(0);
        app.agents
            .get_mut(&id)
            .unwrap()
            .session
            .bg_tool_call_to_task
            .insert("call-bg".into(), "task-1".into());

        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = acp::SessionNotification::new(
            acp::SessionId::new("sess-bg"),
            acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                acp::ToolCallId::new("call-bg"),
                acp::ToolCallUpdateFields::new().raw_output(Some(serde_json::json!({
                    "type": "Bash",
                    "output_for_prompt": "hi",
                }))),
            )),
        )
        .meta(
            serde_json::json!({ "eventId": "sess-bg-6" })
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
            app.agents[&id].last_seen_event_id.as_deref(),
            Some("sess-bg-6"),
            "the bg-stdout arm must advance the cursor"
        );
    }

    /// Symptom-2 guard: a replay update with no `session/load` in flight
    /// (leader broadcast fallthrough, or a replay landing after its reload
    /// already timed out) must be dropped, never appended.
    #[test]
    fn unexpected_replay_update_is_dropped() {
        let mut app = make_app_with_agent("sess-rc");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent
                .scrollback
                .push_block(RenderBlock::system("live content"));
        }

        assert!(
            !handle(
                replay_chunk("sess-rc", "stale history", "sess-rc-9"),
                &mut app
            ),
            "unexpected replay must not redraw"
        );

        let agent = app.agents.get_mut(&id).unwrap();
        assert_eq!(
            agent.scrollback.len(),
            1,
            "unexpected replay must not append to the transcript"
        );
        assert!(
            agent.last_seen_event_id.is_none(),
            "a dropped replay must not advance the reconnect cursor"
        );
    }

    /// The reconnect cursor only follows APPLIED updates: a deduped duplicate
    /// or a stale-turn drop must not advance it.
    #[test]
    fn dropped_updates_do_not_advance_reconnect_cursor() {
        let mut app = make_app_with_agent("sess-cur");
        let id = AgentId(0);

        assert!(handle(
            make_agent_chunk_with_event("sess-cur", "a", "p1", Some("sess-cur-5")),
            &mut app,
        ));
        assert_eq!(
            app.agents[&id].last_seen_event_id.as_deref(),
            Some("sess-cur-5")
        );

        // Duplicate (deduped) — cursor unchanged.
        assert!(!handle(
            make_agent_chunk_with_event("sess-cur", "a", "p1", Some("sess-cur-5")),
            &mut app,
        ));
        assert_eq!(
            app.agents[&id].last_seen_event_id.as_deref(),
            Some("sess-cur-5")
        );

        // Stale-turn drop: a non-viewer's self-originated, non-current prompt
        // id is dropped by the promptId gate — cursor unchanged.
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.note_self_originated_prompt("p-stale");
            agent.session.current_prompt_id = Some("p1".into());
        }
        let _ = handle(
            make_agent_chunk_with_event("sess-cur", "stale", "p-stale", Some("sess-cur-9")),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].last_seen_event_id.as_deref(),
            Some("sess-cur-5"),
            "a promptId-gated drop must not advance the cursor"
        );
    }

    /// EchoAgent extension session updates: replay-stamped ones are gated like ACP
    /// updates, and applied ones advance the reconnect cursor.
    #[test]
    fn echo_agent_session_update_replay_gating_and_cursor() {
        fn model_switch_notif(meta: Option<serde_json::Value>) -> acp::ExtNotification {
            let payload = SessionNotification {
                session_id: acp::SessionId::new("sess-echo-agent"),
                update: EchoAgentSessionUpdate::ModelAutoSwitched {
                    previous_model_id: "m-old".into(),
                    new_model_id: "m-new".into(),
                    reason: "gone".into(),
                },
                meta,
            };
            acp::ExtNotification::new(
                "echo.agent/session/update",
                std::sync::Arc::from(serde_json::value::to_raw_value(&payload).unwrap()),
            )
        }

        let mut app = make_app_with_agent("sess-echo-agent");
        let id = AgentId(0);

        // Replay-stamped with no load in flight → dropped, nothing pushed.
        let replay_meta = serde_json::json!({ "isReplay": true, "eventId": "sess-echo-agent-7" });
        assert!(!handle_ext_notification(
            &model_switch_notif(Some(replay_meta.clone())),
            &mut app
        ));
        {
            let agent = app.agents.get_mut(&id).unwrap();
            assert!(agent.scrollback.is_empty(), "unexpected EchoAgent replay dropped");
            assert!(agent.last_seen_event_id.is_none());
        }

        // Same update inside a reload window → applied and marks the window
        // as full-replay (finishing keeps the staged state, drops the stash).
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent
                .scrollback
                .push_block(RenderBlock::system("pre-outage content"));
            agent.begin_session_reload(1);
        }
        assert!(handle_ext_notification(
            &model_switch_notif(Some(replay_meta)),
            &mut app
        ));
        let agent = app.agents.get_mut(&id).unwrap();
        assert_eq!(
            agent.last_seen_event_id.as_deref(),
            Some("sess-echo-agent-7"),
            "applied EchoAgent updates advance the reconnect cursor"
        );
        assert!(agent.finish_session_reload(1, true));
        assert!(
            !scrollback_has_system_text(agent, "pre-outage content"),
            "an EchoAgent replay line counts as replay for the swap decision"
        );
        assert_eq!(
            agent.scrollback.len(),
            1,
            "the staged EchoAgent block is the new transcript"
        );
    }

    /// Characterization (leader-relaunch orphan rows): a reconnect reload whose
    /// replay contains `SubagentSpawned` with NO `SubagentFinished` (the
    /// subagent died with the old leader, or is still running on the surviving
    /// one) leaves the row `finished == false` after the success swap — the
    /// window finalize force-idles only the ROOT transcript, nothing resolves
    /// or expires subagent rows. Pager-side child tracking itself stays
    /// functional: a live child update delivered after the swap still renders
    /// into the child view, so a post-reconnect freeze would be leader route
    /// loss (see the `leader::server` child-route backfill tests), not pager
    /// state.
    #[test]
    fn reload_replayed_spawn_without_finished_keeps_unresolved_running_row() {
        let mut app = make_app_with_agent("sess-sub");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent
                .scrollback
                .push_block(RenderBlock::system("pre-outage content"));
            agent.begin_session_reload(1);
        }

        // Replayed spawn, no finish (mirrors a mid-subagent reconnect replay).
        let payload = SessionNotification {
            session_id: acp::SessionId::new("sess-sub"),
            update: test_subagent_spawned("sess-sub", "child-sub"),
            meta: Some(serde_json::json!({ "isReplay": true, "eventId": "sess-sub-3" })),
        };
        let notif = acp::ExtNotification::new(
            "echo.agent/session_notification",
            serde_json::value::to_raw_value(&payload).unwrap().into(),
        );
        assert!(handle_ext_notification(&notif, &mut app));

        let agent = app.agents.get_mut(&id).unwrap();
        assert!(agent.finish_session_reload(1, true));
        assert!(matches!(agent.session.state, AgentState::Idle));

        let info = agent
            .subagent_sessions
            .get("child-sub")
            .expect("replayed spawn registers the subagent row");
        assert!(
            !info.finished,
            "no Finished in the replay → the row stays running indefinitely \
             (current behavior: nothing resolves it after the swap)"
        );
        assert!(
            agent.subagent_views.contains_key("child-sub"),
            "the child view exists and is tracked"
        );

        // A live child delta after the swap still renders into the child view:
        // pager-side routing is intact when the leader delivers it.
        let child_len_before = app.agents[&id].subagent_views["child-sub"].scrollback.len();
        let _ = handle(
            make_agent_chunk_with_event("child-sub", "child live text", "p-child", None),
            &mut app,
        );
        assert!(
            app.agents[&id].subagent_views["child-sub"].scrollback.len() > child_len_before,
            "a delivered live child update must render into the child view"
        );
    }

    #[test]
    fn deduped_stale_event_does_not_regress_context_used() {
        // Regression: the context bar must not drop when a stale, already-passed
        // replay delta arrives after a fresher live one. In leader / reconnect /
        // replay-live-overlap, a historical delta (LOWER eventId, LOWER
        // totalTokens) is deduped for rendering — but `refresh_context_used`
        // must respect the dedup too, otherwise the bar regresses below the real
        // usage (the reported "resume shows lower context" bug).
        let mut app = make_app_with_agent("sess-ctx");
        let id = AgentId(0);

        // Fresh live delta: high eventId, high token count.
        let _ = handle(
            make_token_notification_with_event("sess-ctx", 500_000, "sess-ctx-20"),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].context_state.as_ref().map(|c| c.used),
            Some(500_000),
        );
        assert_eq!(app.agents[&id].last_applied_event_seq, Some(20));

        // Stale historical replay delta: lower eventId (deduped), lower tokens.
        let _ = handle(
            make_token_notification_with_event("sess-ctx", 120_000, "sess-ctx-7"),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].context_state.as_ref().map(|c| c.used),
            Some(500_000),
            "a deduped stale delta must not regress context_used to its lower value"
        );
        // Highwater unchanged by the deduped event.
        assert_eq!(app.agents[&id].last_applied_event_seq, Some(20));
    }

    /// The reconnect adoption path (`finalize_reload_and_maybe_adopt`) must skip a
    /// running prompt whose terminal arrived in the reconnect replay — mirrors the
    /// `SessionLoaded` terminal-in-replay test for the other adoption site.
    #[test]
    fn reconnect_finalize_reload_skips_adoption_when_terminal_in_replay() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        // Open a reconnect reload window (enters the replay window, clean set).
        app.agents.get_mut(&id).unwrap().begin_session_reload(1);

        // The running turn's terminal arrives in the reconnect replay → recorded.
        let _ = handle_ext_notification(
            &echo_agent_turn_completed_notif("sess-1", "p-run", "end_turn", true),
            &mut app,
        );
        assert!(app.agents[&id].replayed_terminal_prompts.contains("p-run"));

        let finalized = app
            .agents
            .get_mut(&id)
            .unwrap()
            .finalize_reload_and_maybe_adopt(1, true, Some("p-run".to_string()));
        assert!(finalized, "the reconnect reload window must finalize");
        let agent = &app.agents[&id];
        assert!(
            agent.session.current_prompt_id.is_none(),
            "a terminal-in-replay prompt must NOT be adopted on reconnect"
        );
        assert!(agent.session.state.is_idle());
    }

    /// Apply-only cursor rule (EchoAgent path): a `ModelChanged` the catalog can't
    /// resolve is ignored, so it must NOT advance the reconnect cursor or the
    /// dedup highwater — a later reconnect (catalog now has the model) must
    /// still replay it. An applied follower switch advances both. Mirrors the
    /// ACP path's `advance_reconnect_cursor`.
    #[test]
    fn ignored_model_changed_does_not_advance_cursor_applied_one_does() {
        let mut app = make_app_with_agent("sess-1");
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            seed_models(agent, "echo-agent-3", &["echo-agent-3", "echo-agent-4"]);
        }

        // Unknown model → ignored → both markers untouched.
        assert!(!handle_ext_notification(
            &model_changed_ext_with_event("sess-1", "echo-agent-99-unknown", "sess-1-7"),
            &mut app
        ));
        assert_eq!(
            app.agents[&id].last_seen_event_id, None,
            "an ignored ModelChanged must not advance the reconnect cursor"
        );
        assert_eq!(
            app.agents[&id].last_applied_echo_agent_event_seq, None,
            "an ignored ModelChanged must not advance the dedup highwater"
        );

        // Known model → applied → both markers advance.
        assert!(handle_ext_notification(
            &model_changed_ext_with_event("sess-1", "echo-agent-4", "sess-1-8"),
            &mut app
        ));
        assert_eq!(
            app.agents[&id].last_seen_event_id.as_deref(),
            Some("sess-1-8"),
            "an applied ModelChanged advances the reconnect cursor"
        );
        assert_eq!(app.agents[&id].last_applied_echo_agent_event_seq, Some(8));
    }

    #[test]
    fn fresh_higher_event_still_updates_context_used() {
        // Counterpart: a genuinely newer delta (higher eventId) must still
        // advance the context bar — the dedup gate only blocks stale events.
        let mut app = make_app_with_agent("sess-ctx2");
        let id = AgentId(0);

        let _ = handle(
            make_token_notification_with_event("sess-ctx2", 100_000, "sess-ctx2-3"),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].context_state.as_ref().map(|c| c.used),
            Some(100_000),
        );

        let _ = handle(
            make_token_notification_with_event("sess-ctx2", 250_000, "sess-ctx2-8"),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].context_state.as_ref().map(|c| c.used),
            Some(250_000),
            "a newer (higher eventId) delta must update context_used"
        );
        assert_eq!(app.agents[&id].last_applied_event_seq, Some(8));
    }

