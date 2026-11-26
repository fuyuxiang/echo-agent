use super::*;

/// Route a `ToolCallUpdate` stdout chunk to the central bg task store.
///
/// Returns `true` if the update was consumed (belongs to a bg task),
/// `false` if it should be passed to the normal tracker.
pub(super) fn route_bg_task_stdout(
    tcu: &acp::ToolCallUpdate,
    session: &mut super::super::agent::AgentSession,
) -> bool {
    let tc_id = tcu.tool_call_id.0.to_string();

    // Check if this tool_call_id maps to a bg task
    let task_id = match session.bg_tool_call_to_task.get(&tc_id) {
        Some(tid) => tid.clone(),
        None => return false,
    };

    // Extract stdout from the raw_output BashOutput
    if let Some(ref raw_output) = tcu.fields.raw_output {
        // The shell sends full cumulative output buffer — just overwrite.
        // Check for BashOutput type
        if raw_output.get("type").and_then(|v| v.as_str()) == Some("Bash") {
            // Try output_for_prompt first (pre-stripped string)
            let stdout =
                if let Some(s) = raw_output.get("output_for_prompt").and_then(|v| v.as_str()) {
                    s.to_string()
                } else if let Some(arr) = raw_output.get("output").and_then(|v| v.as_array()) {
                    // Decode output bytes (Vec<u8> serialized as JSON array)
                    let bytes: Vec<u8> = arr
                        .iter()
                        .filter_map(|v| v.as_u64().map(|n| n as u8))
                        .collect();
                    String::from_utf8_lossy(&bytes).into_owned()
                } else {
                    return true; // Consumed but no extractable output
                };

            // Capture the shell-side `truncated` flag — once true, it stays
            // true for the rest of the task (the rolling buffer can't
            // "un-truncate").
            let chunk_truncated = raw_output
                .get("truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            // Don't overwrite with empty stdout (shell clears buffer on completion).
            // `set_stdout` handles the BG_TASK_MAX_STDOUT trim, flips
            // `truncated` on TUI-side overflow, and refreshes the cached
            // `stdout_line_count` in one shot.
            if !stdout.is_empty()
                && let Some(bg_task) = session.bg_tasks.get_mut(&task_id)
            {
                bg_task.set_stdout(stdout);
                if chunk_truncated {
                    bg_task.truncated = true;
                }
            }
        }
    }

    true // Consumed — don't pass to tracker
}

/// Handle `echo.agent/task_backgrounded` — a bash command transitioned to background.
///
/// Creates a `BgTaskState` in the central store and sets up the
/// `tool_call_id → task_id` correlation for stdout routing.
///
/// If the tool already has an Execute block in scrollback (demotion),
/// the existing block is replaced in-place with a `BgTask` and the
/// entry's running state is cleared. Otherwise a fresh `BgTask` block
/// is pushed.
pub(super) fn handle_task_backgrounded(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    // Parse the SessionNotification envelope
    let Ok(session_notif) = serde_json::from_str::<SessionNotification>(notif.params.get()) else {
        tracing::warn!("Failed to parse echo.agent/task_backgrounded");
        return false;
    };

    // Extract TaskBackgrounded fields
    let (tool_call_id, task_id, command, cwd, output_file, monitor_description, notif_description) =
        match session_notif.update {
            EchoAgentSessionUpdate::TaskBackgrounded {
                tool_call_id,
                task_id,
                command,
                cwd,
                output_file,
                monitor_description,
                description,
            } => (
                tool_call_id,
                task_id,
                command,
                cwd,
                output_file,
                monitor_description,
                description,
            ),
            _ => return false,
        };

    // Replayed (`session/load`) restores are historical context, not new
    // activity: mark them so the tasks pane doesn't auto-open on resume.
    let meta = NotificationMeta::from_json(session_notif.meta.as_ref().and_then(|v| v.as_object()));
    let restored_from_replay = meta.is_replay;

    let (matched, is_active, agent) = match resolve_notif_agent(app, &session_notif.session_id) {
        Some(t) => t,
        None => return false,
    };

    tracing::info!(
        tool_call_id = %tool_call_id,
        task_id = %task_id,
        restored_from_replay,
        "Background task started"
    );

    let child_sid: &str = session_notif.session_id.0.as_ref();
    let Some((session, scrollback)) = resolve_target_view(agent, matched, child_sid) else {
        return false;
    };

    // Check if this is a demotion (foreground→background): the execute block
    // already exists in scrollback as a pending tool in the tracker.
    let demotion_eid = session.tracker.pending_tool_entry_id(&tool_call_id);

    // A monitor is identified by the structured `monitor_description` field
    // (current path). Fallback: reparented monitors (subagent session sharing)
    // and backends predating that field still bake a "[monitor] <desc>" prefix
    // into the command — detect it and strip the prefix so those render as a
    // "Monitor" row instead of a bash-highlighted "[monitor] …" under Tasks.
    let monitor_prefix = command
        .strip_prefix(crate::app::agent::MONITOR_PREFIX)
        .map(str::to_string);
    let is_monitor = monitor_description.is_some() || monitor_prefix.is_some();
    // Always drain the deferred-tool suppression key now that routing is being
    // set up — even when we end up preferring the wire `description`. This entry
    // also suppresses late stdout ToolCallUpdates (see tracker), so leaving it
    // behind would leak per bg task and keep dropping updates for the session.
    let deferred_description = session
        .tracker
        .bg_deferred_tools
        .remove(&tool_call_id)
        .flatten();
    // Prefer monitor label, then notification/tool description, then deferred
    // raw_input description (late is_background detection). Blank/whitespace
    // values count as absent so an empty wire `description` can't shadow a real
    // fallback. On demotion we also fall back to the Execute block's description.
    let non_blank = |d: Option<String>| d.filter(|s| !s.trim().is_empty());
    let mut description = non_blank(monitor_description)
        .or_else(|| non_blank(monitor_prefix))
        .or_else(|| non_blank(notif_description))
        .or_else(|| non_blank(deferred_description));

    // Completed-before-Backgrounded race: short bg shells can exit (and the
    // terminal poll emit `TaskCompleted`) before this notification is sent.
    // Never overwrite the recorded terminal state back to Running — the
    // completion already came and went, so it would stick forever.
    let completed_early = session
        .bg_tasks
        .get(&task_id)
        .is_some_and(|t| t.status != BgTaskStatus::Running);

    // Create central bg task state (description may still be filled from the
    // Execute block on demotion before we insert into the map).
    let mut bg_task = BgTaskState {
        task_id: task_id.clone(),
        tool_call_id: tool_call_id.clone(),
        command: command.clone(),
        description: None,
        cwd,
        output_file,
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
        is_monitor,
        restored_from_replay,
    };

    let entry_id = if let Some(eid) = demotion_eid {
        // Demotion: extract stdout and swap the block in a single mutable borrow.
        if let Some(entry) = scrollback.get_by_id_mut(eid) {
            if let RenderBlock::ToolCall(crate::scrollback::blocks::ToolCallBlock::Execute(exec)) =
                &mut entry.block
            {
                // Use `set_stdout` so the cached `stdout_line_count` and the
                // TUI-side trim/truncated flag invariants stay in sync.
                bg_task.set_stdout(exec.output.take().unwrap_or_default());
                if description.as_ref().is_none_or(|d| d.trim().is_empty()) {
                    description = exec.description.take();
                }
            }
            let block = crate::scrollback::blocks::BgTaskBlock::started(&command, &task_id)
                .with_description(description.clone());
            entry.block = RenderBlock::BgTask(block);
            entry.display_mode = crate::scrollback::types::DisplayMode::Collapsed;
            entry.display_mode_pinned = false;
            entry.invalidate_cache();
            scrollback.mark_height_dirty(eid);
            scrollback.finish_running(eid);
            session.tracker.remove_pending_tool(&tool_call_id);
            Some(eid)
        } else if completed_early {
            // Entry gone and the task already completed: the completion
            // block is already in scrollback — nothing left to render.
            session.tracker.remove_pending_tool(&tool_call_id);
            None
        } else {
            // Entry was removed between the tracker lookup and now (compaction,
            // clear, etc.). Create a fresh BgTask so the task has UI presence.
            session.tracker.remove_pending_tool(&tool_call_id);
            let block = crate::scrollback::blocks::BgTaskBlock::started(&command, &task_id)
                .with_description(description.clone());
            let fallback = scrollback.push_block(RenderBlock::BgTask(block));
            scrollback.set_last_running(true);
            Some(fallback)
        }
    } else if completed_early {
        // The completion block is already in scrollback; a fresh "Task
        // started" block would render out of order and animate forever.
        None
    } else {
        let block = crate::scrollback::blocks::BgTaskBlock::started(&command, &task_id)
            .with_description(description.clone());
        let eid = scrollback.push_block(RenderBlock::BgTask(block));
        scrollback.set_last_running(true);
        Some(eid)
    };

    bg_task.description = description;

    if completed_early {
        if let Some(existing) = session.bg_tasks.get_mut(&task_id) {
            existing.absorb_late_backgrounded(bg_task, entry_id);
        }
    } else {
        bg_task.scrollback_entry_id = entry_id;
        session.bg_tasks.insert(task_id.clone(), bg_task);
    }
    session
        .bg_tool_call_to_task
        .insert(tool_call_id.clone(), task_id.clone());

    is_active
}

/// Handle `echo.agent/monitor_event` — background task or monitor emitted new output.
pub(super) fn handle_monitor_event(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    let Ok(session_notif) = serde_json::from_str::<SessionNotification>(notif.params.get()) else {
        return false;
    };
    let (task_id, _description, event_text) = match session_notif.update {
        EchoAgentSessionUpdate::MonitorEvent {
            task_id,
            description,
            event_text,
        } => (task_id, description, event_text),
        _ => return false,
    };
    let (matched, is_active, agent) = match resolve_notif_agent(app, &session_notif.session_id) {
        Some(t) => t,
        None => return false,
    };

    let child_sid: &str = session_notif.session_id.0.as_ref();
    let session = if matches!(matched, SessionMatch::Child(_)) {
        match agent.subagent_views.get_mut(child_sid) {
            Some(child_view) => &mut child_view.session,
            None => return false,
        }
    } else {
        &mut agent.session
    };

    // Append the event text to the bg task's stdout buffer so the
    // block viewer shows it (same as bash output chunks for bg tasks).
    // `append_stdout` handles the trim, flips `truncated` on overflow,
    // and refreshes `stdout_line_count`.
    if let Some(task) = session.bg_tasks.get_mut(&task_id) {
        task.append_stdout(&event_text);
    }

    is_active
}

pub(super) fn handle_scheduled_task_created(
    notif: &acp::ExtNotification,
    app: &mut AppView,
) -> bool {
    let Ok(session_notif) = serde_json::from_str::<SessionNotification>(notif.params.get()) else {
        return false;
    };
    let (task_id, prompt, human_schedule, next_fire_at) = match session_notif.update {
        EchoAgentSessionUpdate::ScheduledTaskCreated {
            task_id,
            prompt,
            human_schedule,
            next_fire_at,
        } => (task_id, prompt, human_schedule, next_fire_at),
        _ => return false,
    };
    let matched = match find_session_match(app, &session_notif.session_id) {
        Some(m) => m,
        None => return false,
    };
    let agent_id = matched.agent_id();
    let is_active = is_matched_agent_active(app, agent_id);
    let agent = app
        .agents
        .get_mut(&agent_id)
        .expect("find_session_match returned an existing AgentId");

    // Remove provisional entries (created by /loop for instant UI feedback).
    agent
        .session
        .scheduled_tasks
        .retain(|k, _| !k.starts_with("provisional-"));

    match agent.session.scheduled_tasks.entry(task_id.clone()) {
        Entry::Occupied(mut e) => {
            let info = e.get_mut();
            info.prompt = prompt;
            info.human_schedule = human_schedule;
            info.next_fire_at = next_fire_at;
        }
        Entry::Vacant(e) => {
            e.insert(crate::app::agent::ScheduledTaskInfo {
                task_id,
                prompt,
                human_schedule,
                created_at: std::time::Instant::now(),
                next_fire_at,
                tag: "loop".into(),
                last_subagent_id: None,
            });
        }
    }

    is_active
}

pub(super) fn handle_scheduled_task_fired(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    let Ok(session_notif) = serde_json::from_str::<SessionNotification>(notif.params.get()) else {
        return false;
    };
    let (task_id, prompt, human_schedule, next_fire_at, subagent_id) = match session_notif.update {
        EchoAgentSessionUpdate::ScheduledTaskFired {
            task_id,
            prompt,
            human_schedule,
            next_fire_at,
            subagent_id,
        } => (task_id, prompt, human_schedule, next_fire_at, subagent_id),
        _ => return false,
    };
    let matched = match find_session_match(app, &session_notif.session_id) {
        Some(m) => m,
        None => return false,
    };
    let agent_id = matched.agent_id();
    let is_active = is_matched_agent_active(app, agent_id);
    let agent = app
        .agents
        .get_mut(&agent_id)
        .expect("find_session_match returned an existing AgentId");

    // Self-heal: if the task is unknown (e.g. a regression of the shell-side
    // re-announce on session restore), insert a fresh entry from the fire
    // payload so the tasks pane still shows the loop.
    match agent.session.scheduled_tasks.entry(task_id) {
        Entry::Occupied(mut e) => {
            let info = e.get_mut();
            info.next_fire_at = next_fire_at;
            if subagent_id.is_some() {
                info.last_subagent_id = subagent_id;
            }
        }
        Entry::Vacant(e) => {
            if next_fire_at.is_none() {
                return is_active;
            }
            let task_id = e.key().clone();
            e.insert(crate::app::agent::ScheduledTaskInfo {
                task_id,
                prompt,
                human_schedule,
                created_at: std::time::Instant::now(),
                next_fire_at,
                tag: "loop".into(),
                last_subagent_id: subagent_id,
            });
        }
    }
    is_active
}

pub(super) fn handle_scheduled_task_deleted(
    notif: &acp::ExtNotification,
    app: &mut AppView,
) -> bool {
    let Ok(session_notif) = serde_json::from_str::<SessionNotification>(notif.params.get()) else {
        return false;
    };
    let (task_id, reason) = match session_notif.update {
        EchoAgentSessionUpdate::ScheduledTaskDeleted { task_id, reason } => (task_id, reason),
        _ => return false,
    };
    let matched = match find_session_match(app, &session_notif.session_id) {
        Some(m) => m,
        None => return false,
    };
    let agent_id = matched.agent_id();
    let is_active = is_matched_agent_active(app, agent_id);
    let agent = app
        .agents
        .get_mut(&agent_id)
        .expect("find_session_match returned an existing AgentId");

    let info = agent.session.scheduled_tasks.remove(&task_id);

    // Expiry is the only removal nobody asked for, so it alone leaves a transcript record (the
    // tombstone replays on resume). Chip presence makes re-delivered tombstones a no-op; the
    // replay gate keeps a misrouted replay from duplicating history on a live transcript.
    let meta = NotificationMeta::from_json(session_notif.meta.as_ref().and_then(|v| v.as_object()));
    if reason == ScheduledTaskRemovedReason::Expired
        && (!meta.is_replay || agent.accepts_replayed_update())
        && let Some(info) = info
    {
        let entry_id = agent
            .scrollback
            .push_block(RenderBlock::system(expired_task_notice(&info)));
        // A reconnect reload may have rendered this notice live before the outage; recording the
        // staged entry lets the keep-stash finalize drop the duplicate. (Marking the reload
        // replay-seen instead would swap in a notice-only transcript and drop the stash.)
        if meta.is_replay {
            agent.note_replayed_expiry_notice(entry_id);
        }
    }
    is_active
}

/// One-line transcript notice for a scheduled task that hit its auto-expiry.
fn expired_task_notice(info: &crate::app::agent::ScheduledTaskInfo) -> String {
    let first_line = crate::app::status_blocks::first_nonempty_line(&info.prompt);
    let mut head: String = first_line.chars().take(60).collect();
    if head.len() < first_line.len() {
        head.push('\u{2026}');
    }
    format!(
        "Scheduled task expired: \"{head}\" ({}). Recurring tasks auto-expire after {} days; re-create it if still needed.",
        info.human_schedule,
        echo_agent_tools::implementations::echo_agent_build::scheduler::types::RECURRING_TASK_TTL_DAYS,
    )
}

pub(super) fn handle_scheduled_task_inject_prompt(
    notif: &acp::ExtNotification,
    app: &mut AppView,
) -> bool {
    let payload: serde_json::Value = match serde_json::from_str(notif.params.get()) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to parse echo.agent/scheduled_task_inject_prompt");
            return false;
        }
    };
    let Some(session_id) = payload["sessionId"].as_str() else {
        tracing::warn!("echo.agent/scheduled_task_inject_prompt: missing or non-string sessionId");
        return false;
    };
    let Some(prompt) = payload["prompt"].as_str().filter(|s| !s.is_empty()) else {
        tracing::warn!("echo.agent/scheduled_task_inject_prompt: missing or empty prompt");
        return false;
    };
    let task_id = payload["taskId"].as_str().unwrap_or("unknown");
    let human_schedule = payload["humanSchedule"].as_str().unwrap_or("unknown");
    tracing::debug!(task_id, human_schedule, "Enqueuing scheduled cron prompt");

    // Only the driver injects + runs the scheduled prompt. In leader mode the
    // `echo.agent/scheduled_task_inject_prompt` notification is routed by the leader
    // to the SINGLE session driver (see `is_scheduled_task_inject_prompt` in
    // leader/server.rs), so any client that receives it IS the driver and must
    // enqueue + run it — including a client that attached via `session/load`
    // (`attached_as_viewer == true`) but is the designated driver. We therefore
    // do NOT skip on `attached_as_viewer` here: that latched flag wrongly
    // suppressed cron on an attacher-driver, leaving the loop stuck with no
    // output. The other clients render the resulting turn from the broadcast
    // deltas. (The de-dup guards below still prevent a double enqueue.)
    let agent_id = {
        let agent = app.agents.values_mut().find(|a| {
            a.session
                .session_id
                .as_ref()
                .is_some_and(|sid| sid.0.as_ref() == session_id)
        });
        let Some(agent) = agent else {
            return false;
        };

        // Skip if this specific task is already running or queued.
        if agent.cron_task_id.as_deref() == Some(task_id) {
            tracing::debug!(task_id, "cron prompt skipped: task already running");
            return true;
        }
        let already_queued = agent
            .session
            .pending_prompts
            .iter()
            .any(|p| p.task_id.as_deref() == Some(task_id));
        if already_queued {
            tracing::debug!(task_id, "cron prompt already queued, skipping duplicate");
            return true;
        }

        let agent_id = agent.session.id;
        agent.session.enqueue_cron_prompt(
            prompt.to_string(),
            task_id.to_string(),
            human_schedule.to_string(),
        );
        agent_id
    };
    let effects = super::super::dispatch::maybe_drain_queue_and_note_peek(app, agent_id);
    app.pending_effects.extend(effects);

    true
}

/// Derive the effective CWD and worktree flag for a child session.
///
/// Each field is derived independently: `child_cwd` controls the path,
/// `worktree_path` controls the worktree flag. Either can be present
/// without the other.
pub(super) fn derive_child_cwd(
    parent_cwd: &std::path::Path,
    info: Option<&crate::app::subagent::SubagentInfo>,
) -> (PathBuf, bool) {
    let cwd = info
        .and_then(|i| i.child_cwd.as_deref())
        .map(PathBuf::from)
        .unwrap_or_else(|| parent_cwd.to_path_buf());
    let is_worktree = info.is_some_and(|i| i.worktree_path.is_some());
    (cwd, is_worktree)
}

/// Updates the cached branch/worktree display on the matching agent so the
/// status bar can render without spawning `git` on every frame.
pub(super) fn handle_git_head_changed(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    let Ok(params) = serde_json::from_str::<echo_agent_workspace::session::git::GitHeadChanged>(
        notif.params.get(),
    ) else {
        return false;
    };

    // Find the agent by ACP session id (not local AgentId) and update its git display cache
    if let Some(agent) = app.agents.values_mut().find(|a| {
        a.session
            .session_id
            .as_ref()
            .is_some_and(|s| s.0.as_ref() == params.session_id.as_str())
    }) {
        // Refresh the shared per-cwd git cache so views keyed on this
        // directory (the header / top bar when it's the process cwd) pick
        // up the new branch without spawning subprocesses; the agent's own
        // fields below drive its status bar / dashboard row directly.
        crate::git_info::update_from_notification(
            &agent.session.cwd,
            params.branch.as_deref(),
            params.main_repo.clone(),
            params.is_worktree,
        );
        agent.current_branch = params.branch;
        agent.is_worktree = params.is_worktree;
        agent.main_repo = params.main_repo;
        return true;
    }

    // Fallback: check child subagent views.
    for agent in app.agents.values_mut() {
        if let Some(child_view) = agent.subagent_views.values_mut().find(|cv| {
            cv.session
                .session_id
                .as_ref()
                .is_some_and(|s| s.0.as_ref() == params.session_id.as_str())
        }) {
            crate::git_info::update_from_notification(
                &child_view.session.cwd,
                params.branch.as_deref(),
                params.main_repo.clone(),
                params.is_worktree,
            );
            child_view.current_branch = params.branch;
            child_view.is_worktree = params.is_worktree;
            child_view.main_repo = params.main_repo;
            return true;
        }
    }

    false
}

pub(super) fn handle_task_completed(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    // The payload is a SessionNotification wrapping TaskCompleted { task_snapshot }
    let Ok(session_notif) = serde_json::from_str::<SessionNotification>(notif.params.get()) else {
        tracing::warn!("Failed to parse echo.agent/task_completed");
        return false;
    };

    let task_snapshot = match session_notif.update {
        EchoAgentSessionUpdate::TaskCompleted { task_snapshot, .. } => task_snapshot,
        _ => return false,
    };

    // Stamps `restored_from_replay` on tombstones inserted below.
    let meta = NotificationMeta::from_json(session_notif.meta.as_ref().and_then(|v| v.as_object()));

    let (matched, is_active, agent) = match resolve_notif_agent(app, &session_notif.session_id) {
        Some(t) => t,
        None => return false,
    };

    let task_id = &task_snapshot.task_id;
    let exit_code = task_snapshot.exit_code;
    let signal = task_snapshot.signal.clone();

    tracing::info!(
        task_id = %task_id,
        exit_code = ?exit_code,
        signal = ?signal,
        "Background task completed"
    );

    // Determine success once, reused for both bg_task status and scrollback block.
    let success = exit_code == Some(0) || (exit_code.is_none() && signal.is_none());

    // Synthetic completion emitted by the agent's cold-load reconciliation
    // (`reconcile_stale_background_tasks`): the task's process died with a
    // previous session lifetime — it did not fail NOW. Finalize pane/state
    // quietly instead of pushing a fresh red "Task failed" block into the
    // resumed scrollback (one per dead task — pure noise on every resume).
    let stale_on_load = signal.as_deref() == Some("session_restart");

    let child_sid: &str = session_notif.session_id.0.as_ref();
    let Some((session, scrollback)) = resolve_target_view(agent, matched, child_sid) else {
        return false;
    };

    // Compute elapsed duration from the bg task state (if we have it).
    // Prefer the human description for "Task completed/failed: …" labels
    // (same as "Task started"), falling back to the raw command only when
    // no description was supplied.
    let (command, elapsed, mut description, scrollback_entry_id) =
        if let Some(bg_task) = session.bg_tasks.get_mut(task_id) {
            bg_task.status = if success {
                BgTaskStatus::Done
            } else {
                BgTaskStatus::Failed
            };
            bg_task.exit_code = exit_code;
            bg_task.signal = signal.clone();
            bg_task.end_time = Some(std::time::SystemTime::now());
            bg_task.pending_kill = false;
            bg_task.kill_requested_at = None;
            (
                bg_task.command.clone(),
                bg_task.elapsed(),
                bg_task.description.clone(),
                bg_task.scrollback_entry_id,
            )
        } else {
            // Task we didn't know about — its `TaskBackgrounded` hasn't
            // arrived yet. Label from the model-supplied description, else
            // display_command when it differs from the raw command (monitors /
            // isolation-wrapped shells); treat equal values as non-labels.
            let command = task_snapshot.command.clone();
            let elapsed = task_snapshot
                .end_time
                .and_then(|end| end.duration_since(task_snapshot.start_time).ok())
                .unwrap_or_default();
            let description = task_snapshot
                .description
                .clone()
                .filter(|d| !d.trim().is_empty())
                .or_else(|| {
                    task_snapshot.display_command.clone().and_then(|d| {
                        // Bare label, matching the "Task started" path.
                        let d = d
                            .strip_prefix(crate::app::agent::MONITOR_PREFIX)
                            .map(str::to_string)
                            .unwrap_or(d);
                        let t = d.trim();
                        if t.is_empty() || t == command.trim() {
                            None
                        } else {
                            Some(d)
                        }
                    })
                });

            // Record the terminal state so the late `TaskBackgrounded` merges
            // into it instead of inserting a fresh Running entry.
            let status = if success {
                BgTaskStatus::Done
            } else {
                BgTaskStatus::Failed
            };
            let tombstone = BgTaskState::tombstone_from_snapshot(
                &task_snapshot,
                status,
                description.clone(),
                meta.is_replay,
            );
            session.bg_tasks.insert(task_id.clone(), tombstone);

            (command, elapsed, description, None)
        };

    // Finish the "Task started" scrollback entry (stops bullet animation).
    // Also sync description onto that block so the historical "Task started"
    // line shows the label if it was missing at background time.
    if let Some(entry_id) = scrollback_entry_id {
        if let Some(entry) = scrollback.get_by_id_mut(entry_id)
            && let RenderBlock::BgTask(bg) = &mut entry.block
        {
            if description.as_ref().is_none_or(|d| d.trim().is_empty()) {
                if let Some(d) = bg.description.clone().filter(|d| !d.trim().is_empty()) {
                    description = Some(d);
                }
            } else if bg.description.as_ref().is_none_or(|d| d.trim().is_empty()) {
                bg.description = description.clone();
                entry.invalidate_cache();
            }
        }
        scrollback.finish_running(entry_id);
    }

    // Keep central store in sync when we recovered a description from the
    // scrollback block (or display_command fallback).
    if let Some(bg_task) = session.bg_tasks.get_mut(task_id)
        && bg_task
            .description
            .as_ref()
            .is_none_or(|d| d.trim().is_empty())
        && let Some(ref d) = description
        && !d.trim().is_empty()
    {
        bg_task.description = Some(d.clone());
    }

    if stale_on_load {
        // The replayed "Task started" block above is finished (static
        // bullet); the pane row leaves the running filter via the status
        // update. No new scrollback block: nothing happened in THIS session.
        return is_active;
    }

    // Emit "Task completed/failed" scrollback block — uses description when
    // present so the label matches "Task started: <desc>" (not raw command).
    let block = if success {
        RenderBlock::bg_task_completed(&command, task_id, elapsed)
            .with_bg_task_description(description)
    } else {
        RenderBlock::bg_task_failed(&command, task_id, elapsed, exit_code, signal)
            .with_bg_task_description(description)
    };
    let completion_eid = scrollback.push_block(block);

    // Anchor tasks that have no "Task started" block (tombstones) to the
    // completion block, so block-viewer actions don't fall back to a bogus
    // EntryId(0) and immediately close.
    if let Some(bg_task) = session.bg_tasks.get_mut(task_id)
        && bg_task.scrollback_entry_id.is_none()
    {
        bg_task.scrollback_entry_id = Some(completion_eid);
    }

    is_active
}
