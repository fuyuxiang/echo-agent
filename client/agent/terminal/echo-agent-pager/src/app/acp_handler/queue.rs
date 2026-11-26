use super::*;

/// A server-authoritative running prompt that drained into the running slot
/// while the previous turn was still finishing locally (FIFO handoff
/// race). Stashed on [`AppView::pending_running_adoptions`] and consumed by the
/// `PromptResponse` handler after `finish_turn` clears `current_prompt_id`.
#[derive(Debug, Clone)]
pub(crate) struct PendingRunningAdoption {
    /// The `prompt_id` the leader reported as `running_prompt_id`.
    pub prompt_id: String,
    /// The queued prompt's text (for the turn-start shim's user block), if the
    /// pager knew about the prompt. `None` for prompts queued by other clients.
    pub text: Option<String>,
    /// Combined-turn display segments (len ≥ 2); shim paints one bubble each.
    pub combined_texts: Option<Vec<String>>,
    /// The adopted entry's `kind` (`"prompt"`/`"bash"`/`"verification"`/…),
    /// which selects the turn-start shim's display block + focus flag.
    pub kind: String,
    /// Set when a `running=None` broadcast spares this stash (one-shot: the
    /// next `running=None` tears it down).
    pub turn_ended: bool,
}

/// Wire payload of `echo.agent/session/prompt_complete`, emitted by
/// `MvpAgent::prompt()` on the shell after every turn.
///
/// `Serialize` is derived so tests construct payloads through the same type
/// they are parsed into (shape drift fails at compile time, not at runtime).
/// Unknown fields (e.g. `turnId`, future additions) are ignored; every field
/// except `sessionId` is optional for wire compatibility with older shells —
/// in particular `promptId` only exists on shells with the lost-response fix.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PromptCompletePayload {
    pub(super) session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) prompt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) agent_result: Option<String>,
    /// What triggered a cancelled turn's cancel (`"send_now"` suppresses the
    /// "Turn cancelled" marker); stamped top-level, absent on older shells.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cancel_trigger: Option<String>,
    /// Why a cancelled turn was cancelled (`"HookDenied"` picks the
    /// blocked-by-hook marker); stamped top-level, absent on older shells.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cancellation_category: Option<String>,
    /// `_meta` extension point — parsed defensively as a trigger fallback.
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub(super) meta: Option<serde_json::Value>,
}

impl PromptCompletePayload {
    /// The cancel trigger, wherever it was stamped: the top-level
    /// `cancelTrigger` field (the shell's emission), falling back to
    /// `_meta.cancelTrigger` (the envelope shape of the durable rail).
    /// `None` (older shells) means a normal cancel.
    pub(super) fn cancel_trigger(&self) -> Option<&str> {
        self.cancel_trigger.as_deref().or_else(|| {
            self.meta
                .as_ref()?
                .get(super::super::turn_completion::CANCEL_TRIGGER_KEY)?
                .as_str()
        })
    }

    /// The cancellation category (`"HookDenied"` picks the blocked-by-hook
    /// marker), wherever it was stamped: the top-level field (the shell's
    /// broadcast) with the `_meta` envelope shape as fallback. `None` (older
    /// shells / plain user cancels) keeps the user-cancel copy.
    pub(super) fn cancellation_category(&self) -> Option<&str> {
        self.cancellation_category.as_deref().or_else(|| {
            self.meta
                .as_ref()?
                .get(super::super::turn_completion::CANCELLATION_CATEGORY_KEY)?
                .as_str()
        })
    }
}

pub(super) fn handle_queue_changed(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    let Ok(changed) =
        serde_json::from_str::<crate::app::prompt_queue::QueueChanged>(notif.params.get())
    else {
        tracing::warn!("Failed to parse echo.agent/queue/changed");
        return false;
    };

    let running_prompt_id = changed.running_prompt_id.clone();
    let session_id = changed.session_id.clone();

    // Prefer running_* fields on the payload (authoritative; present when a
    // turn is promoting). Fall back to the local mirror for older shells.
    let running_entry = running_prompt_id.as_ref().and_then(|pid| {
        app.shared_prompt_queue(&session_id)
            .and_then(|q| q.iter().find(|e| &e.id == pid).cloned())
    });
    let running_text: Option<String> = changed
        .running_text
        .clone()
        .or_else(|| running_entry.as_ref().map(|e| e.text.clone()));
    let running_combined: Option<Vec<String>> = changed
        .running_combined_texts
        .clone()
        .filter(|v| v.len() >= 2)
        .or_else(|| {
            running_entry
                .as_ref()
                .and_then(|e| e.combined_texts.clone())
                .filter(|v| v.len() >= 2)
        });
    let running_kind: String = changed
        .running_kind
        .clone()
        .or_else(|| running_entry.as_ref().map(|e| e.kind.clone()))
        .unwrap_or_else(|| "prompt".to_string());

    // Resolve the owning agent before the queue is replaced.
    let sid = acp::SessionId::new(session_id.clone());
    let agent_id = match find_session_match(app, &sid) {
        Some(SessionMatch::Root(id)) => Some(id),
        _ => None,
    };

    let recv_entry_ids: Vec<&str> = changed.entries.iter().map(|e| e.id.as_str()).collect();
    // Raw (pre-merge) broadcast rows for the optimistic-echo reconcile: the
    // post-apply snapshot re-pins unconfirmed echoes, so only the broadcast
    // itself can prove a row landed shell-side.
    let raw_entries: Vec<(String, u64)> = changed
        .entries
        .iter()
        .map(|e| (e.id.clone(), e.version))
        .collect();
    let local_current_prompt_id = agent_id
        .and_then(|aid| app.agents.get(&aid))
        .and_then(|a| a.session.current_prompt_id.clone())
        .unwrap_or_default();
    tracing::debug!(
        target: "qtrace",
        pid = std::process::id(),
        event = "queue_changed_recv",
        session = %session_id,
        running_prompt_id = running_prompt_id.as_deref().unwrap_or(""),
        local_current_prompt_id = %local_current_prompt_id,
        entry_count = changed.entries.len(),
        entries = ?recv_entry_ids,
        "received echo.agent/queue/changed broadcast",
    );

    let rekeyed_echo_ids = app.apply_queue_changed(changed);

    // Mirror the reconciled shared queue into the owning agent so the queue
    // pane can render the union of local + server rows without needing
    // `AppView` access during draw / input handling.
    if let Some(aid) = agent_id {
        let snapshot = app
            .shared_prompt_queue(&session_id)
            .cloned()
            .unwrap_or_default();
        // Stashed adoption: its painted block is about to be consumed.
        let stashed_pid = app
            .pending_running_adoptions
            .get(&aid)
            .map(|p| p.prompt_id.clone());
        if let Some(agent) = app.agents.get_mut(&aid) {
            agent.shared_queue = snapshot;
            // A re-keyed echo's id is dead everywhere (only its content
            // matched the broadcast): drop it from the optimistic set and
            // any send-now parked on it — the row is visible under its new
            // id, so a fresh Enter sends it normally. A painted send-now
            // block moves to the new id (the message still runs there).
            for (old_id, new_id) in &rekeyed_echo_ids {
                agent.note_queue_echo_rekeyed(old_id, new_id);
            }

            // A painted-pending prompt the broadcast no longer lists — and
            // is neither running nor a stashed adoption — was removed and
            // will never adopt: retire its block. Unconfirmed optimistic ids
            // are exempt (their RPC is in flight; absence is expected).
            let removed_painted: Vec<String> = agent
                .send_now_painted_blocks
                .keys()
                .filter(|pid| {
                    running_prompt_id.as_deref() != Some(pid.as_str())
                        && stashed_pid.as_deref() != Some(pid.as_str())
                        && !raw_entries.iter().any(|(eid, _)| eid == *pid)
                        && !agent.optimistic_queue_ids.contains(*pid)
                        // An active-goal Send Now painted block awaiting its
                        // interjection claim: its row legitimately vanishes from
                        // the broadcast the instant the shell converts the Send
                        // Now into an interjection, so absence here is expected.
                        // Keep it in place for `handle_interjection` to convert;
                        // retiring it would drop and re-push it at the end.
                        && !agent.is_send_now_awaiting_interjection_claim(pid)
                })
                .cloned()
                .collect();
            for pid in &removed_painted {
                agent.retire_send_now_painted_block(pid);
            }

            // Cleanup hook: if the user is editing a server-origin row and
            // that row is no longer in the broadcast (started draining,
            // removed by another client, etc.), exit editing mode so the
            // composer isn't stranded on a ghost row. Don't dispatch any
            // follow-up Action — the broadcast already reconciled the
            // queue state for every other client.
            let stranded_server_id = match &agent.prompt_mode {
                super::super::agent_view::PromptMode::EditingQueued {
                    server_id: Some(sid),
                    ..
                } if !agent.shared_queue.iter().any(|e| &e.id == sid) => Some(sid.clone()),
                _ => None,
            };
            if let Some(sid) = stranded_server_id {
                tracing::debug!(
                    server_id = %sid,
                    "exiting EditingQueued: row is no longer in the shared queue"
                );
                agent.cancel_editing_queued_for_lost_row();
            }
        }
        // Resolve a queue-row send-now that was parked while its row was
        // still an optimistic echo: the broadcast just confirmed the row, so
        // fire the interject with the authoritative version (racing it
        // earlier would have no-opped shell-side and dropped the send-now).
        let fire = app.agents.get_mut(&aid).and_then(|agent| {
            agent.resolve_send_now_awaiting_confirm(&raw_entries, running_prompt_id.as_deref())
        });
        if let Some((id, expected_version)) = fire {
            if let Some(agent) = app.agents.get_mut(&aid) {
                // Same arming contract as `dispatch_queue_interject_shared`.
                super::super::dispatch::arm_send_now_and_paint(agent, &id, None);
            }
            crate::unified_log::info(
                "prompt.queue_send_now_confirmed",
                Some(&session_id),
                Some(serde_json::json!({ "prompt_id": id, "version": expected_version })),
            );
            app.pending_effects
                .push(crate::app::actions::Effect::QueueInterject {
                    session_id: sid.clone(),
                    id,
                    expected_version,
                    new_text: None,
                });
        }
    }

    // Wake-turn marker reconcile: `running_prompt_id` is authoritative. A
    // broadcast naming a wake prompt offers the stop affordance even before
    // its first delta; any other value retires a stale marker (recovery for
    // a lost wake terminal).
    if let Some(aid) = agent_id
        && let Some(agent) = app.agents.get_mut(&aid)
    {
        let running = running_prompt_id.as_deref();
        if agent
            .running_wake_turn
            .as_ref()
            .is_some_and(|wake| Some(wake.prompt_id.as_str()) != running)
        {
            agent.running_wake_turn = None;
        }
        if let Some(pid) = running
            && is_wake_prompt(pid)
        {
            // The broadcast is authoritative: the shell says this wake is
            // running, so an earlier terminal's record no longer applies.
            agent.finished_wake_prompts.remove(pid);
            agent.note_streaming_wake_turn(pid);
        }
    }

    // Adoption / turn-start correlation.
    //
    // Single-client idle path stays inert: the pager sets `current_prompt_id`
    // locally at `start_turn`, so when the confirming broadcast arrives with
    // `running_prompt_id == current_prompt_id`, the `Some(c) if c == pid` arm
    // makes this a no-op.
    match (running_prompt_id, agent_id) {
        // No turn running on the server — drop any stale pending adoption.
        // Exception (`turn_ended`, one-shot): a turn ending inside the handoff
        // window must leave the stash for the previous turn's PromptResponse,
        // regardless of buffer occupancy — this ext broadcast can overtake the
        // turn's `session/update`s (separate, reorderable channels).
        (None, Some(aid)) => {
            let retain = app
                .pending_running_adoptions
                .get(&aid)
                .is_some_and(|p| !p.turn_ended);
            if retain {
                if let Some(p) = app.pending_running_adoptions.get_mut(&aid) {
                    p.turn_ended = true;
                }
            } else if let Some(p) = app.pending_running_adoptions.remove(&aid)
                && let Some(agent) = app.agents.get_mut(&aid)
            {
                agent.discard_pending_adoption_updates(&p.prompt_id);
            }
        }
        // Non-adoptable running prompt (see `AgentView::should_adopt_running_prompt`):
        // either an actor-run synthetic turn with no `prompt_complete` /
        // `PromptResponse` exit (nothing would ever call `finish_turn`), or a turn
        // whose durable `TurnCompleted` already arrived in THIS load's replay
        // (terminal-in-replay — it already ended). Adopting either via
        // `apply_turn_start_shim` would `start_turn()` → `AgentState::TurnRunning`
        // and strand the pager on "Responding…"/"Waiting…" forever. The agent-aware
        // check is load-bearing here: `replayed_terminal_prompts` stays populated
        // after a load, so a later `queue/changed` re-reporting the already-ended
        // `running_prompt_id` must NOT re-adopt the turn the `SessionLoaded` /
        // reconnect adoption already correctly skipped. Skip the turn-start
        // adoption; a live synthetic turn's streaming content still renders via the
        // live-delta path in `handle` WITHOUT calling `start_turn`.
        (Some(pid), Some(aid))
            if app
                .agents
                .get(&aid)
                .is_some_and(|a| !a.should_adopt_running_prompt(&pid)) =>
        {
            tracing::debug!(
                target: "qtrace",
                pid = std::process::id(),
                prompt_id = %pid,
                "queue/changed: skipping turn-start adoption for non-adoptable running \
                 prompt (synthetic turn with no prompt_complete exit, or terminal-in-replay)",
            );
        }
        (Some(pid), Some(aid)) => {
            let current = app
                .agents
                .get(&aid)
                .and_then(|a| a.session.current_prompt_id.clone());
            match current {
                // Already tracking this running prompt — inert.
                Some(c) if c == pid => {}
                // Nothing running locally: adopt now + run the turn-start shim
                // (render the queued prompt's user block, set `TurnRunning`).
                None => {
                    let page_flip_entry = app.agents.get_mut(&aid).and_then(|agent| {
                        super::super::dispatch::apply_turn_start_shim(
                            agent,
                            pid,
                            running_text,
                            &running_kind,
                            running_combined,
                        )
                    });
                    super::super::dispatch::note_peek_page_flip(app, aid, page_flip_entry);
                }
                // A different prompt is still finishing locally (FIFO handoff
                // race — the next broadcast can arrive before the previous
                // turn's `PromptResponse`). Stash it; the `PromptResponse`
                // handler adopts it after `finish_turn` clears
                // `current_prompt_id`. Never corrupt the in-flight turn.
                Some(_) => {
                    // The leader emits this prompt's user-echo (no `promptId`,
                    // so the gate can't drop it) right after this broadcast but
                    // before the previous turn's `PromptResponse` runs the
                    // deferred shim. Arm the echo-skip now so it doesn't render
                    // a duplicate user block — but ONLY when THIS client will
                    // actually paint that block via the deferred shim.
                    //
                    // The deferred shim is run exclusively by the `PromptResponse`
                    // handler, which fires only for the client that DROVE the
                    // currently-finishing turn (`!attached_as_viewer`). A viewer
                    // of that turn ends it via `prompt_complete`, which clears
                    // (and removes) the stash without ever running the shim — so
                    // on a viewer the echo is the ONLY source of the user block
                    // and must not be swallowed.
                    //
                    // Key the guard on driver-vs-viewer of the *current* turn,
                    // NOT on who originated the draining prompt: a client can be
                    // `attached_as_viewer` on another client's turn yet
                    // immediate-send (self-originate) a queued prompt of its own.
                    // That client still won't run the shim, so an
                    // `is_self_originated`-based guard would wrongly swallow the
                    // echo and drop the block. Symmetric
                    // hazard: a driver adopting ANOTHER client's drained prompt
                    // DOES run the shim, so it must swallow the echo — which an
                    // origination-based guard would miss, double-rendering.
                    let drives_current_turn =
                        app.agents.get(&aid).is_some_and(|a| !a.attached_as_viewer);
                    let will_render_own_block = drives_current_turn
                        && super::super::dispatch::shim_renders_own_user_block(
                            &running_kind,
                            running_text.as_deref(),
                        );
                    if will_render_own_block && let Some(agent) = app.agents.get_mut(&aid) {
                        agent.session.tracker.expect_user_echo();
                    }
                    tracing::debug!(
                        target: "qtrace",
                        pid = std::process::id(),
                        event = "adoption_stashed",
                        prompt_id = %pid,
                        "stashing running-prompt adoption (FIFO handoff race)",
                    );
                    // A rebroadcast for the SAME running prompt (every queue
                    // edit/no-op rebroadcasts) must not clobber the stash: the
                    // first broadcast consumed the drained row from the mirror,
                    // so this pass re-derives `text: None` and the deferred
                    // shim would render no user block (and the echo-skip armed
                    // above already swallowed the shell's echo).
                    if app
                        .pending_running_adoptions
                        .get(&aid)
                        .is_some_and(|p| p.prompt_id == pid)
                    {
                        return true;
                    }
                    // A newer running prompt supersedes any earlier stash.
                    if let Some(prev) = app.pending_running_adoptions.insert(
                        aid,
                        PendingRunningAdoption {
                            prompt_id: pid.clone(),
                            text: running_text,
                            combined_texts: running_combined,
                            kind: running_kind,
                            turn_ended: false,
                        },
                    ) && let Some(agent) = app.agents.get_mut(&aid)
                    {
                        agent.discard_pending_adoption_updates(&prev.prompt_id);
                    }
                }
            }
        }
        _ => {}
    }
    true
}

/// `prompt_complete` carries `sessionId`, `stopReason`, `agentResult`,
/// `turnId`, and (shells ≥ the lost-response fix) `promptId`; for viewers,
/// turns are serialized per session, so "finish the running viewer turn for
/// this session" is unambiguous even without the prompt id.
///
/// This is the one-release compat rail (kept until every leader emits the
/// durable [`EchoAgentSessionUpdate::TurnCompleted`]): it parses the payload and
/// delegates the turn-finalize to
/// [`finalize_turn_from_terminal`](super::super::turn_completion::finalize_turn_from_terminal),
/// which carries the driver-arm / viewer-finish behavior verbatim.
///
/// TODO(prompt_complete-deprecation): Legacy removal (gated): durable turn_completed is already consumed via finalize_turn_from_terminal; keep & re-point the lost-RPC reconcile to the durable rail before deleting.
pub(super) fn handle_prompt_complete(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    let Ok(payload) = serde_json::from_str::<PromptCompletePayload>(notif.params.get()) else {
        tracing::warn!("Failed to parse echo.agent/session/prompt_complete");
        return false;
    };
    let session_id = payload.session_id.as_str();

    let sid = acp::SessionId::new(session_id.to_string());
    let Some(SessionMatch::Root(id)) = find_session_match(app, &sid) else {
        return false;
    };
    let is_active = is_matched_agent_active(app, id);
    let Some(agent) = app.agents.get_mut(&id) else {
        return false;
    };

    // Finalize on the agent, then map the outcome to the return bool in the one
    // shared place both terminal rails use (returns it directly — arming reports
    // a change unconditionally so a background tab still wakes the reconcile tick).
    let outcome = super::super::turn_completion::finalize_turn_from_terminal(
        agent,
        session_id,
        super::super::turn_completion::TerminalSignal {
            prompt_id: payload.prompt_id.as_deref(),
            stop_reason: payload.stop_reason.as_deref(),
            agent_result: payload.agent_result.as_deref(),
            cancel_trigger: payload.cancel_trigger(),
            cancellation_category: payload.cancellation_category(),
        },
    );
    super::super::turn_completion::apply_terminal_outcome(outcome, app, id, is_active)
}
