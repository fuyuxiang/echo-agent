use super::*;

// ---------------------------------------------------------------------------
// Permission request handling
// ---------------------------------------------------------------------------

/// Route a permission request to the agent that owns its `session_id`, queue
/// it on that agent's view, and return whether the active view needs a redraw.
///
/// Permissions are routed by `session_id` so that requests for an inactive
/// agent still queue on the owning agent's view. When the user switches back
/// to that agent, the queued permission is visible and can be answered.
///
/// If no agent owns the `session_id` (e.g. session was just cleaned up), the
/// request is cancelled rather than left dangling.
///
/// YOLO mode is honored on the owning agent regardless of which agent is
/// currently active, so background turns aren't blocked waiting for an
/// always-yes answer the user has already given.
pub(super) fn handle_permission_request(
    perm: echo_agent_acp::AcpArgs<acp::RequestPermissionRequest>,
    app: &mut AppView,
) -> bool {
    // 1. Look up the owning agent by session_id (root or subagent view).
    let matched = match find_session_match(app, &perm.request.session_id) {
        Some(m) => m,
        None => {
            tracing::warn!(
                session_id = %perm.request.session_id.0,
                "Permission request for unknown session_id; cancelling"
            );
            cancel_permission(perm);
            return false;
        }
    };
    let owning_agent_id = matched.agent_id();
    let is_active = is_matched_agent_active(app, owning_agent_id);
    let Some(agent) = app.agents.get_mut(&owning_agent_id) else {
        cancel_permission(perm);
        return false;
    };

    // 2. YOLO mode: auto-approve immediately on the owning agent so background
    //    turns aren't blocked waiting for the user to switch back.
    //
    //    If no `AllowOnce` option exists, falls through to
    //    `enqueue_permission` even in YOLO mode (won't pick
    //    `AllowAlways` by default).
    if agent.session.is_yolo()
        && let Some(allow) = perm
            .request
            .options
            .iter()
            .find(|o| o.kind == acp::PermissionOptionKind::AllowOnce)
    {
        let option_id = allow.option_id.clone();
        perm.response_tx
            .send(Ok(acp::RequestPermissionResponse::new(
                acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                    option_id,
                )),
            )))
            .ok();
        return false; // no redraw needed
    }

    // 3. Fire notification so the user notices the pending approval.
    //    Rate-limit: only fire the bell/popup on the empty→non-empty
    //    transition to avoid stacking notifications during concurrent
    //    permission requests.
    if !app
        .notification_service
        .should_suppress_permission_notification()
    {
        app.notification_service.notify(NotificationEvent {
            kind: NotificationEventKind::ApprovalRequired,
            title: "EchoAgent".into(),
            body: NotificationEventKind::ApprovalRequired.as_str().into(),
            session_id: Some(perm.request.session_id.0.to_string()),
        });
        app.notification_service.mark_permission_notified();
    }

    // 4. Queue on the owning agent's view. Subagent provenance for display
    //    is still resolved via subagent_sessions in enqueue_permission().
    //    Redraw is only needed when the owning agent is currently visible.
    let needs_redraw = enqueue_permission(perm, agent);
    needs_redraw && is_active
}

/// Enqueue a permission request on an agent view.
///
/// Parses bash highlights, builds display content, stashes the prompt on
/// queue transition, and pushes the request onto the FIFO queue.
fn enqueue_permission(
    perm: echo_agent_acp::AcpArgs<acp::RequestPermissionRequest>,
    agent: &mut AgentView,
) -> bool {
    // 1. Parse bash highlights from request meta (imported from echo-agent-runtime).
    let bash_highlights: Option<BashCommandHighlights> = perm
        .request
        .meta
        .as_ref()
        .and_then(|meta| serde_json::from_value(serde_json::Value::Object(meta.clone())).ok());
    let bash_selection_count = bash_highlights
        .as_ref()
        .map(|h| echo_agent_workspace::permission::default_always_allow_scope(&h.highlighted_words))
        .unwrap_or(0);
    let bash_deny_selection_count = bash_highlights
        .as_ref()
        .map(|h| echo_agent_workspace::permission::default_always_deny_scope(&h.highlighted_words))
        .unwrap_or(0);

    // The request arrives over ACP from a possibly older agent, so a violated
    // enqueue invariant (allow row on a non-persisting default scope) is
    // warned, never asserted; the agent proves the invariant crate-locally.
    if let Some(h) = bash_highlights.as_ref() {
        let offers_allow_row = perm.request.options.iter().any(|o| {
            o.option_id.0.as_ref() == crate::views::permission_view::ALLOW_ALWAYS_COMMAND_OPTION_ID
        });
        if offers_allow_row
            && !echo_agent_workspace::permission::always_allow_scope_persists(
                h,
                bash_selection_count,
            )
        {
            tracing::warn!(
                scope = bash_selection_count,
                words = ?h.highlighted_words,
                "allow-always-command row offered but default scope does not persist"
            );
        }
    }

    // 1b. Parse MCP scope state from the `allow-always-mcp` option's meta.
    //     Mutually exclusive with the bash flow at the per-request level —
    //     the same prompt cannot carry both.
    let mcp_scope = perm
        .request
        .options
        .iter()
        .find(|o| o.option_id.0.as_ref() == "allow-always-mcp")
        .and_then(|opt| opt.meta.as_ref())
        .and_then(|m| {
            serde_json::from_value::<echo_agent_workspace::permission::McpToolPermission>(
                serde_json::Value::Object(m.clone()),
            )
            .ok()
        })
        .map(|perm| McpScopeState {
            tool_name: perm.tool_name,
            server_prefix: perm.server_prefix,
            selected: McpScope::Tool,
        });

    // 2. Build subagent provenance label.
    //    If session_id differs from the root session, look up subagent info.
    let subagent_label = resolve_subagent_label(agent, &perm.request.session_id);

    // 3. Build title and description from the tool call.
    let (title, description, bash_command_raw) = build_permission_display(
        &perm.request,
        bash_highlights.as_ref(),
        #[cfg(feature = "local-workspace")]
        matches!(
            agent.workspace_mode,
            crate::views::welcome::WelcomeWorkspaceMode::LocalWorkspace
        ),
        #[cfg(not(feature = "local-workspace"))]
        false,
    );

    // 4. Assign a monotonic ID.
    let perm_id = agent.next_perm_req_id;
    agent.next_perm_req_id += 1;

    // 5. Stash prompt on queue transition: empty -> non-empty.
    //    Do NOT stash again if the queue is already non-empty (that would
    //    capture followup text from the current permission as the "original"
    //    prompt, losing the user's real input).
    if agent.permission_queue.is_empty() && agent.permission_stashed_prompt.is_none() {
        agent.permission_stashed_prompt = Some(agent.prompt.stash());
        agent.prompt.set_text("");
    }

    // Permissions bypass the interceptor in Scrollback, so focus Prompt for the first queued request.
    if agent.permission_queue.is_empty()
        && agent.active_pane == AgentPane::Scrollback
        && agent.permission_stashed_pane.is_none()
    {
        agent.permission_stashed_pane = Some(AgentPane::Scrollback);
        agent.set_active_pane(AgentPane::Prompt, true);
    }

    // 6. Clone options before moving perm into the struct.
    let options = perm.request.options.clone();

    // 7. Cursor preselection (sticky last-used → configured default → the
    //    enable-always-approve row → index 0). See `permission_cursor`.
    let active_idx = crate::appearance::permission_cursor::resolve_initial_cursor(&options);

    // 8. Queue the request FIFO (do NOT replace/cancel existing requests).
    agent.permission_queue.push_back(PermissionViewState {
        request: perm,
        id: perm_id,
        focus: PermissionFocus::Options,
        options,
        active_idx,
        bash_highlights,
        bash_selection_count,
        bash_deny_selection_count,
        bash_command_raw,
        mcp_scope,
        title,
        description,
        args_expanded: false,
        desc_scroll: 0,
        subagent_label,
        options_area_height: 0,
        options_scroll_offset: 0,
    });

    // Stamp the agent's "last activity" anchor so
    // the dashboard's age column for `NeedsInput` rows reflects
    // "time since permission arrived" rather than "time since last
    // turn ended". The same field powers the dashboard relative-time label.
    agent.last_active_at = Some(std::time::Instant::now());

    true // needs redraw
}

/// Build a subagent provenance label for display.
///
/// Two tiers of provenance quality:
///
/// 1. **Tracked provenance** (`SubagentSpawned` was received): renders as
///    `Subagent "Find endpoints" (explore):` with description and type
///    from the tracked `SubagentInfo`. This is the trusted path.
///
/// 2. **Opaque non-root session**: the session_id does not match root and
///    is not in the tracked subagent map. Renders as
///    `Child session (untracked):` to signal reduced confidence.
///
/// Returns `None` for root session (no provenance needed).
fn resolve_subagent_label(agent: &AgentView, session_id: &acp::SessionId) -> Option<String> {
    let sid = session_id.0.as_ref();
    // Check if this is the root session (no provenance needed).
    if let Some(ref root_sid) = agent.session.session_id
        && root_sid.0.as_ref() == sid
    {
        return None;
    }
    // Tier 1: tracked subagent with full metadata.
    if let Some(info) = agent.subagent_sessions.get(sid) {
        return Some(format!(
            "Subagent \"{}\" ({}):",
            info.description, info.subagent_type
        ));
    }
    // Tier 2: non-root session with no tracked info.
    Some("Child session (untracked):".to_string())
}

/// Build title, description lines, and optional raw command for a permission request.
///
/// Deserializes `raw_input` into the shared [`BashToolInput`] from
/// `echo-agent-tools` for typed access to `command` and `description`.
/// Falls back to ACP-level `title`/`kind` fields when deserialization fails.
///
/// Returns `(title, description, bash_command_raw)`.
fn build_permission_display(
    req: &acp::RequestPermissionRequest,
    bash_highlights: Option<&BashCommandHighlights>,
    session_local_workspace: bool,
) -> (String, Vec<String>, Option<String>) {
    let is_bash = bash_highlights.is_some();

    let bash_input = req.tool_call.fields.raw_input.as_ref().and_then(|v| {
        serde_json::from_value::<echo_agent_tools::implementations::BashToolInput>(v.clone()).ok()
    });

    let raw_command = bash_input.as_ref().map(|b| b.command.clone()).or_else(|| {
        req.tool_call
            .fields
            .title
            .as_deref()
            .and_then(|t| t.strip_prefix("Execute `"))
            .and_then(|t| t.strip_suffix('`'))
            .map(|s| s.to_string())
    });

    let bash_description = bash_input.map(|b| b.description);

    let is_execute = is_bash
        || req.tool_call.fields.kind == Some(acp::ToolKind::Execute)
        || raw_command.is_some();

    let title = if is_execute {
        bash_description
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string())
            .unwrap_or_else(
                || match bash_highlights.and_then(|h| h.highlighted_words.first()) {
                    Some(bin) => format!("Allow `{bin}`?"),
                    None => "Allow Execute?".to_string(),
                },
            )
    } else if is_edit_permission(req) {
        let file_path = req
            .tool_call
            .fields
            .raw_input
            .as_ref()
            .and_then(|v| v.get("file_path"))
            .and_then(|v| v.as_str());
        if let Some(path) = file_path {
            format!("Allow Edit to {}?", path)
        } else if let Some(ref t) = req.tool_call.fields.title {
            format!(
                "Allow {}?",
                echo_agent_workspace::permission::mcp_pretty_name_if_qualified(t)
            )
        } else {
            "Allow Edit?".to_string()
        }
    } else if let Some(ref t) = req.tool_call.fields.title {
        format!(
            "Allow {}?",
            echo_agent_workspace::permission::mcp_pretty_name_if_qualified(t)
        )
    } else {
        match req.tool_call.fields.kind {
            Some(acp::ToolKind::Edit) => "Allow Edit?".to_string(),
            Some(acp::ToolKind::Execute) => "Allow Execute?".to_string(),
            Some(acp::ToolKind::Delete) => "Allow Delete?".to_string(),
            _ => "Allow?".to_string(),
        }
    };

    let title = qualify_permission_title_for_local_workspace(title, session_local_workspace);
    let description = permission_description_lines(req);
    let bash_cmd = if is_execute { raw_command } else { None };
    (title, description, bash_cmd)
}

/// Per-session HITL copy — not process-global CLI stamp.
fn qualify_permission_title_for_local_workspace(
    title: String,
    session_local_workspace: bool,
) -> String {
    if !session_local_workspace {
        return title;
    }
    if title.contains("on your machine") {
        return title;
    }
    if let Some(stripped) = title.strip_suffix('?') {
        return format!("{stripped} (on your machine)?");
    }
    format!("{title} (on your machine)")
}

/// Lines shown under the permission title: protected-edit note (if any), then
/// MCP planned-argument lines (empty for bash/edit).
fn permission_description_lines(req: &acp::RequestPermissionRequest) -> Vec<String> {
    let mut lines = mcp_args_lines(req);
    if is_edit_permission(req)
        && let Some(desc) = protected_edit_description(req)
    {
        lines.insert(0, desc);
    }
    lines
}

fn protected_edit_description(req: &acp::RequestPermissionRequest) -> Option<String> {
    let meta = req.meta.as_ref()?;
    let protected: echo_agent_workspace::permission::ProtectedEditPermission =
        serde_json::from_value(serde_json::Value::Object(meta.clone())).ok()?;
    protected.description.filter(|s| !s.is_empty())
}

/// Maximum stored lines for the MCP planned-arguments display. The overlay
/// clips further (options always stay visible); this only bounds memory for
/// pathologically large inputs.
pub(super) const MCP_ARGS_MAX_LINES: usize = 200;

/// Maximum stored characters per argument line. Bounds the per-frame
/// wrap/render cost for pathological single-line values (e.g. an embedded
/// base64 blob); anything longer is elided with a marker.
pub(super) const MCP_ARGS_MAX_LINE_CHARS: usize = 2000;

/// Pretty-printed JSON lines of the arguments an MCP tool call plans to
/// send, taken from the serialized `ToolInput` the shell puts in
/// `tool_call.fields.raw_input` (`{"variant": "UseTool"|"MCPTool",
/// "tool_name": …, "tool_input": …}`). The prompt otherwise names the tool
/// without what it would do — the payload is what an approval (especially
/// an always-approve) is judged on.
///
/// Empty for non-MCP requests (bash/edit have dedicated displays) and for
/// MCP requests without a JSON `tool_input`.
pub(super) fn mcp_args_lines(req: &acp::RequestPermissionRequest) -> Vec<String> {
    let Some(raw) = req.tool_call.fields.raw_input.as_ref() else {
        return Vec::new();
    };
    // Match by serde tag rather than deserializing the whole enum, so args
    // still display when the shell adds variants this build doesn't know.
    let is_mcp = matches!(
        raw.get("variant").and_then(|v| v.as_str()),
        Some("UseTool") | Some("MCPTool")
    );
    if !is_mcp {
        return Vec::new();
    }
    let args = match raw.get("tool_input") {
        Some(serde_json::Value::Null) | None => return Vec::new(),
        Some(args) => args,
    };
    let pretty = match serde_json::to_string_pretty(args) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let mut lines: Vec<String> = pretty
        .lines()
        .map(|l| match l.char_indices().nth(MCP_ARGS_MAX_LINE_CHARS) {
            Some((byte_idx, _)) => format!("{}…", &l[..byte_idx]),
            None => l.to_owned(),
        })
        .collect();
    if lines.len() > MCP_ARGS_MAX_LINES {
        let hidden = lines.len() - MCP_ARGS_MAX_LINES;
        lines.truncate(MCP_ARGS_MAX_LINES);
        lines.push(format!("… (+{hidden} more lines)"));
    }
    lines
}

/// Check if this is an edit permission by looking at option names.
///
/// The shell's edit options include "allow all edits" in the AllowAlways
/// option name. This is reliable even when tool_call.fields.kind is None.
fn is_edit_permission(req: &acp::RequestPermissionRequest) -> bool {
    req.options.iter().any(|o| {
        o.kind == acp::PermissionOptionKind::AllowAlways && o.name.to_lowercase().contains("edit")
    })
}

/// Cancel a permission request by sending `Cancelled` on the response channel.
fn cancel_permission(perm: echo_agent_acp::AcpArgs<acp::RequestPermissionRequest>) {
    perm.response_tx
        .send(Ok(acp::RequestPermissionResponse::new(
            acp::RequestPermissionOutcome::Cancelled,
        )))
        .ok();
}

/// Drop live auto recap that would land between turns. Manual `/recap` and
/// replay always apply.
pub(super) fn should_drop_late_auto_recap(
    auto: bool,
    is_replay: bool,
    agent: &crate::app::agent_view::AgentView,
) -> bool {
    auto && !is_replay && !cli_is_idle_for_recap(agent)
}

/// Recap must not paint in the gap before the next turn starts.
fn cli_is_idle_for_recap(agent: &crate::app::agent_view::AgentView) -> bool {
    use crate::app::agent::BgTaskStatus;

    if !agent.session.state.is_idle() {
        return false;
    }
    if agent.session.in_flight_prompt.is_some() || agent.has_held_user_queue() {
        return false;
    }
    if agent.subagent_sessions.values().any(|s| !s.finished) {
        return false;
    }
    if agent
        .session
        .bg_tasks
        .values()
        .any(|t| t.status == BgTaskStatus::Running && !t.is_monitor)
    {
        return false;
    }
    if scrollback_waiting_on_user_turn(&agent.scrollback) {
        return false;
    }
    true
}

/// Walk past monitor/bg-task/lifecycle trailers. Settled-turn markers
/// (including failure paths that skip `TurnFailed`) end the wait.
fn scrollback_waiting_on_user_turn(scrollback: &crate::scrollback::state::ScrollbackState) -> bool {
    use crate::scrollback::block::RenderBlock;

    for idx in (0..scrollback.len()).rev() {
        let Some(entry) = scrollback.get(idx) else {
            continue;
        };
        if entry.block.is_user_prompt() {
            return true;
        }
        if matches!(
            &entry.block,
            RenderBlock::AgentMessage(_) | RenderBlock::Thinking(_) | RenderBlock::ToolCall(_)
        ) {
            return false;
        }
        if let RenderBlock::SessionEvent(b) = &entry.block
            && session_event_settles_turn(&b.event)
        {
            return false;
        }
    }
    false
}

/// Retry/auth/overflow paths suppress `TurnFailed` and leave these markers.
fn session_event_settles_turn(event: &crate::scrollback::blocks::SessionEvent) -> bool {
    use crate::scrollback::blocks::SessionEvent;

    event.is_turn_terminal()
        || matches!(
            event,
            SessionEvent::ReAuthRequired
                | SessionEvent::ContextTooLarge
                | SessionEvent::DiskFull
                | SessionEvent::CompactionFailed { .. }
                | SessionEvent::RetryFailed { .. }
        )
}

/// Live auto recap when scrollback already has a recap after the last user
/// prompt. Replay still rebuilds history as stored.
pub(super) fn should_drop_duplicate_auto_recap(
    auto: bool,
    is_replay: bool,
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    auto && !is_replay && scrollback_has_recap_since_last_user(scrollback)
}

fn scrollback_has_recap_since_last_user(
    scrollback: &crate::scrollback::state::ScrollbackState,
) -> bool {
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::blocks::SessionEvent;

    let mut recap_since_user = false;
    for (_, entry) in scrollback.iter_entries() {
        if entry.block.is_user_prompt() {
            recap_since_user = false;
            continue;
        }
        if let RenderBlock::SessionEvent(b) = &entry.block
            && matches!(b.event, SessionEvent::Recap { .. })
        {
            recap_since_user = true;
        }
    }
    recap_since_user
}

/// Land a `SessionRecap` block: fill a manual `/recap`'s in-flight loading
/// spinner in place (and stop its animation) when one is showing, otherwise
/// append a fresh block. An automatic recap never consumes the manual loading
/// slot (`auto`) — it would orphan the in-flight manual response into a second
/// block — so it always appends.
///
/// Minimal (scrollback-native) mode may have already printed the loading
/// spinner into the terminal's native scrollback (the idle commit pass consumes
/// it print-once). Filling that entry in place would mutate state the terminal
/// never re-reads — the recap text would exist only in `/transcript`, never on
/// screen. Re-print instead: drop the stale committed entry from state (its
/// printed copy can't be un-printed, matching the K10 re-print semantics) and
/// append the real recap as a fresh block so the commit pass emits it.
/// `is_committed` is always false outside minimal, so the fill-in-place path is
/// unchanged for the alt-screen / inline modes.
pub(super) fn apply_recap_block(agent: &mut AgentView, auto: bool, recap_block: RenderBlock) {
    let fill_id = if auto {
        None
    } else {
        agent
            .pending_recap_entry
            .take()
            .filter(|&id| agent.scrollback.get_by_id(id).is_some())
    };
    match fill_id {
        Some(id) if agent.scrollback.is_committed(id) => {
            agent.scrollback.remove_entry(id);
            agent.scrollback.push_block(recap_block);
        }
        Some(id) => {
            // Existence just confirmed; scope the `&mut` borrow so it
            // ends before `finish_running` re-borrows the scrollback.
            if let Some(entry) = agent.scrollback.get_by_id_mut(id) {
                entry.block = recap_block;
            }
            agent.scrollback.finish_running(id);
        }
        None => {
            agent.scrollback.push_block(recap_block);
        }
    }
}

#[cfg(all(test, feature = "local-workspace"))]
mod tests {
    use super::*;

    #[test]
    fn permission_title_qualifies_for_local_workspace() {
        assert_eq!(
            qualify_permission_title_for_local_workspace("Allow Edit?".into(), false),
            "Allow Edit?"
        );
        assert_eq!(
            qualify_permission_title_for_local_workspace("Allow Edit?".into(), true),
            "Allow Edit (on your machine)?"
        );
    }
}
