use super::*;

/// Update the activity label on a subagent's collapsed scrollback block.
///
/// Skips the write (and cache invalidation) when the label hasn't changed,
/// so the per-delta common case ("Responding" stays "Responding") allocates
/// nothing.
pub(super) fn sync_activity_label(
    scrollback: &mut crate::scrollback::state::ScrollbackState,
    entry_id: Option<crate::scrollback::entry::EntryId>,
    activity_label: Option<&str>,
) {
    if let Some(eid) = entry_id
        && let Some(entry) = scrollback.get_by_id_mut(eid)
        && let RenderBlock::Subagent(ref mut sb) = entry.block
        && sb.activity_label.as_deref() != activity_label
    {
        sb.activity_label = activity_label.map(str::to_owned);
        entry.invalidate_cache();
    }
}

/// Fan a subagent's computed activity label out to both surfaces that show
/// it — the collapsed scrollback block and the [`SubagentInfo`] backing the
/// tasks pane / dashboard rows — so the two can't drift.
///
/// A finished row accepts only the `None` clear: buffered child-rail updates
/// race `SubagentFinished` and must not re-stamp it.
pub(super) fn sync_subagent_activity(
    parent: &mut AgentView,
    child_key: &str,
    activity_label: Option<String>,
) {
    let Some(info) = parent.subagent_sessions.get_mut(child_key) else {
        return;
    };
    if info.finished && activity_label.is_some() {
        return;
    }
    sync_activity_label(
        &mut parent.scrollback,
        info.scrollback_entry_id,
        activity_label.as_deref(),
    );
    info.activity_label = activity_label;
}

/// Resolve a subagent child view's live activity into the display label the
/// fan-out stamps ("Waiting" while the child is busy between activities).
pub(super) fn subagent_activity_label(child_view: &AgentView) -> Option<String> {
    match child_view.resolve_turn_activity() {
        Some(a) => Some(crate::app::subagent::format_activity_label(&a)),
        None if child_view.session.state.is_busy() => Some("Waiting".to_string()),
        None => None,
    }
}

/// Synthesize a finish for a stuck row when a kill found nothing live to stop
/// (else `pending_kill` times out → "running"). `status` is the real terminal
/// status for an already-finished orphan, else `"cancelled"`.
///
/// When the child had already finished, the retained terminal status wins over
/// the call's default (`cancelled`) so a failed child is not repainted as
/// cancelled while still carrying its failure text.
pub(crate) fn finalize_killed_subagent(
    app: &mut AppView,
    session_id: &acp::SessionId,
    subagent_id: &str,
    status: &str,
) -> bool {
    let Some(SessionMatch::Root(agent_id)) = find_session_match(app, session_id) else {
        return false;
    };
    let Some(agent) = app.agents.get(&agent_id) else {
        return false;
    };
    let Some(info) = agent
        .subagent_sessions
        .values()
        .find(|info| info.subagent_id.as_ref() == subagent_id)
    else {
        return false;
    };
    let child_session_id = info.child_session_id.to_string();
    let was_finished = info.finished;
    let (effective_status, error, tool_calls, turns, duration_ms, tokens_used) = if was_finished {
        (
            info.status.as_deref().unwrap_or(status).to_owned(),
            info.error.as_deref().map(str::to_owned),
            info.tool_calls.unwrap_or(0),
            info.turns.unwrap_or(0),
            info.duration_ms.unwrap_or(0),
            info.tokens_used.unwrap_or(0),
        )
    } else {
        (status.to_owned(), None, 0, 0, 0, 0)
    };

    let payload = SessionNotification {
        session_id: session_id.clone(),
        update: EchoAgentSessionUpdate::SubagentFinished {
            subagent_id: subagent_id.to_string(),
            child_session_id: child_session_id.clone(),
            status: effective_status,
            error,
            tool_calls,
            turns,
            duration_ms,
            tokens_used,
            output: None,
            will_wake: false,
        },
        meta: None,
    };
    let Ok(params) = serde_json::value::to_raw_value(&payload) else {
        return false;
    };

    let notif = acp::ExtNotification::new("echo.agent/session/update", params.into());
    handle_session_notification_with_origin(&notif, app, LifecycleOrigin::Reconciliation)
}
