//! Active-agent lookup and view-context helpers shared across dispatch modules.

use super::dashboard_telemetry::log_dashboard_opened;
use crate::app::agent::AgentId;
use crate::app::agent_view::AgentView;
use crate::app::app_view::{ActiveView, AppView, WelcomeAnnouncementState};
use crate::scrollback::state::ScrollbackState;
use agent_client_protocol as acp;

/// Refusal shown when a dispatch path can't proceed without a bound session. Every path in this
/// module tree that says exactly this uses it; the action-specific variants ("No active session
/// to delete") stay separate.
pub(super) const NO_SESSION_NOTICE: &str = "No active session";

/// The active agent's root session id, if any. Used to scope server-queue
/// edit Effects to the foregrounded session.
pub(super) fn active_agent_session_id(app: &AppView) -> Option<acp::SessionId> {
    let ActiveView::Agent(id) = app.active_view else {
        return None;
    };
    app.agents.get(&id)?.session.session_id.clone()
}

/// Apply a closure to the active agent (if any).
///
/// When a subagent view is active, resolves to the **child** view so
/// actions like SelectNext, GotoBottom, etc. target the visible view.
pub(super) fn with_active_agent(app: &mut AppView, f: impl FnOnce(&mut AgentView)) {
    if let ActiveView::Agent(id) = app.active_view
        && let Some(agent) = app.agents.get_mut(&id)
    {
        if let Some(child_sid) = agent.active_subagent.clone()
            && let Some(child) = agent.subagent_views.get_mut(&child_sid)
        {
            f(child);
            return;
        }
        f(agent);
    }
}

/// Open `url` via the system browser, falling back to a visible URL when the
/// browser cannot open (headless VM / missing opener). Prefer this over raw
/// `open_url_if_safe` for user-initiated billing/upgrade CTAs.
///
/// When no agent is active (welcome/gate screen), still attempts the open and
/// falls back to clipboard + toast.
pub(super) fn open_url_or_show(app: &mut AppView, url: &str) {
    if let Some(agent) = get_active_agent_mut(app) {
        agent.open_url_or_show(url);
        return;
    }

    use crate::app::link_opener::{OpenUrlResult, browser_unavailable_line, try_open_url};
    use crate::terminal::hyperlinks::SchemeFilter;

    match try_open_url(url, SchemeFilter::Standard) {
        OpenUrlResult::Opened | OpenUrlResult::RejectedScheme => {}
        OpenUrlResult::BrowserUnavailable => {
            let copied = crate::clipboard::SystemClipboard::try_set(url).reported_success();
            app.show_toast(&browser_unavailable_line(url, copied));
        }
    }
}

/// Get a shared reference to the active agent view (if any).
pub(super) fn get_active_agent(app: &AppView) -> Option<&AgentView> {
    if let ActiveView::Agent(id) = app.active_view
        && let Some(agent) = app.agents.get(&id)
    {
        if let Some(ref child_sid) = agent.active_subagent
            && let Some(child) = agent.subagent_views.get(child_sid)
        {
            return Some(child);
        }
        return Some(agent);
    }
    None
}

/// Get a mutable reference to the active agent view (if any).
pub(super) fn get_active_agent_mut(app: &mut AppView) -> Option<&mut AgentView> {
    if let ActiveView::Agent(id) = app.active_view
        && let Some(agent) = app.agents.get_mut(&id)
    {
        if let Some(child_sid) = agent.active_subagent.clone()
            && agent.subagent_views.contains_key(&child_sid)
        {
            return agent.subagent_views.get_mut(&child_sid).map(|b| &mut **b);
        }
        return Some(agent);
    }
    None
}

/// Child view when a fullscreen subagent overlay is open.
///
/// Unlike [`get_active_agent_mut`], never falls back to the parent.
/// Overlay cancel uses this so the overlay-open check and cancel target cannot disagree.
pub(super) fn active_subagent_view_mut(app: &mut AppView) -> Option<&mut AgentView> {
    let ActiveView::Agent(id) = app.active_view else {
        return None;
    };
    let agent = app.agents.get_mut(&id)?;
    let child_sid = agent.active_subagent.clone()?;
    agent.subagent_views.get_mut(&child_sid).map(|b| &mut **b)
}

/// Apply a closure to the active agent's scrollback (if any).
///
/// Resolves through `active_subagent` — see [`with_active_agent`].
pub(super) fn with_scrollback(app: &mut AppView, f: impl FnOnce(&mut ScrollbackState)) {
    with_active_agent(app, |agent| f(&mut agent.scrollback));
}

/// Navigate the scrollback and clear any persistent text selection.
///
/// Used by navigation actions (j/k/g/G/PageUp/PageDown/Ctrl-D/Ctrl-U) where
/// scrolling away from the selected region should dismiss the highlight.
pub(super) fn navigate_clearing_selection(app: &mut AppView, f: impl FnOnce(&mut ScrollbackState)) {
    with_active_agent(app, |agent| {
        agent.persistent_text_selection = None;
        agent.table_selection_geometry = None;
        agent.selection_created_at = None;
        agent.highlighted_link_idx = None;
        f(&mut agent.scrollback);
    });
}

/// Synchronize the sleep inhibitor with the aggregate agent state.
///
/// Inhibits idle sleep when any agent is busy; releases when all are idle.
/// Called after every `AgentState` transition in dispatch.
pub(super) fn sync_sleep_inhibitor(app: &AppView) {
    let any_busy = app.agents.values().any(|a| !a.session.state.is_idle());
    if any_busy {
        app.notification_service.sleep_inhibitor.inhibit();
    } else {
        app.notification_service.sleep_inhibitor.release();
    }
}

pub(super) fn reseed_tip_for_new_session(app: &mut AppView) {
    if !matches!(app.active_view, ActiveView::Agent(_)) || app.tips.is_empty() {
        return;
    }
    let echo_agent_home = echo_agent_tools::util::echo_agent_home::echo_agent_home();
    app.tip = echo_agent_runtime::util::tips::pick_and_advance(&app.tips, &echo_agent_home);
}

/// Switch to the welcome screen, clearing ephemeral per-visit state. Use for
/// every return-to-welcome transition so a previously-expanded announcement
/// can't leak into the freshly-shown screen.
pub(super) fn show_welcome(app: &mut AppView) {
    app.active_view = ActiveView::Welcome;
    app.welcome_announcement = WelcomeAnnouncementState::default();
    // Drop stale welcome workspace one-shot / ACK so a later create/load
    // cannot inherit an override from a deferred or abandoned NewSession.
    #[cfg(feature = "local-workspace")]
    {
        app.welcome_session_local_workspace = None;
        app.welcome_local_workspace_ack_pending = false;
        // Abandoning a session/restore must not leak history bypass into the
        // next welcome LoadSession / SessionFlags.chat_mode batch.
        app.welcome_history_load_as_build = false;
    }
}

/// Restore the view a mid-session auth flow launched from, falling back to the
/// welcome screen (via `show_welcome`) when the original agent is gone. Shared by
/// cancel-login and AuthComplete so they can't diverge.
pub(super) fn restore_auth_return_view(app: &mut AppView, return_view: ActiveView) {
    match return_view {
        ActiveView::Agent(id) if app.agents.contains_key(&id) => {
            app.active_view = ActiveView::Agent(id)
        }
        ActiveView::AgentDashboard => {
            app.active_view = ActiveView::AgentDashboard;
            log_dashboard_opened(app);
        }
        _ => show_welcome(app),
    }
}

/// Why a switch from one [`ActiveView::Agent`] to another is happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwitchCause {
    /// Triggered by `/fork` (post-resolution) creating + switching to a
    /// child.
    Fork,
    /// Triggered by `/new` (fresh agent).
    New,
    /// Triggered by `/resume` (resuming a prior session) and the
    /// welcome-screen session picker.
    Load,
    /// Triggered by the agent picker (dashboard attach / switch).
    Picker,
    // `SwitchCause::Dashboard` was added
    // for the dashboard attach path but the earlier popup overlay
    // never reaches `switch_to_agent`, so the variant was dead. YAGNI —
    // any future caller can re-add it. The dashboard's attach path
    // sets `DashboardState::attached_agent` directly.
}

/// Surface a launch-blocked `--yolo` once on the first agent view (the TUI owns
/// the terminal, so stderr is gone); idempotent via `.take()`. Dashboard flows
/// that bypass [`switch_to_agent`] call it directly.
pub(super) fn surface_yolo_launch_block_notice(app: &mut AppView, target: AgentId) {
    if let Some(warning) = app.yolo_launch_block_notice.take()
        && let Some(agent) = app.agents.get_mut(&target)
    {
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::system(
                warning.to_string(),
            ));
        agent.show_toast(warning);
    }
    surface_screen_mode_switch_hint(app, target);
}

/// Surface a one-shot switch-back toast after a screen-mode relaunch (fullscreen only).
pub(super) fn surface_screen_mode_switch_hint(app: &mut AppView, target: AgentId) {
    if let Some(hint) = app.screen_mode_switch_hint.take()
        && !app.screen_mode.is_minimal()
        && let Some(agent) = app.agents.get_mut(&target)
    {
        agent.show_toast(hint);
    }
}

/// Re-anchor the global permission-mode mirror to the active agent.
pub(super) fn sync_active_permission_mode_mirror(app: &mut AppView) {
    let ActiveView::Agent(target) = app.active_view else {
        return;
    };
    let Some(agent) = app.agents.get(&target) else {
        return;
    };
    let (is_yolo, is_auto) = (agent.session.is_yolo(), agent.session.is_auto());
    let reanchor = if is_yolo {
        Some("always-approve")
    } else if is_auto && app.auto_mode_gate {
        // Gate-aware: never re-anchor the global mirror to "auto" when the
        // feature gate is off, even if a stale per-session `auto_mode`
        // survived (defense-in-depth with the settings kill-switch fan-out).
        Some("auto")
    } else if matches!(
        app.current_ui.permission_mode.as_deref(),
        Some("always-approve") | Some("auto")
    ) {
        // Non-yolo/non-auto agent: clear a stale yolo/auto mirror left by a
        // different agent; preserve an existing ask/default distinction.
        Some("ask")
    } else {
        None
    };
    if let Some(canonical) = reanchor {
        app.current_ui.permission_mode = Some(canonical.to_string());
    }
}

/// Switch the active agent — the primary funnel for assigning `ActiveView::Agent`
/// (new, resume, picker, fork); also fires [`surface_yolo_launch_block_notice`].
///
/// For [`SwitchCause::New`] and [`SwitchCause::Fork`], also follows dashboard
/// overlay attach when it named the prior top-level agent (Left / Esc / Ctrl+\
/// back-out only works while attach matches the active agent). Load and Picker
/// keep their own attach rules. Uses the top-level `ActiveView` id, never a
/// subagent placeholder from [`get_active_agent`].
///
/// No-op if `target` is unknown or already active. Dashboard-first flows that
/// assign `Agent` directly must call the notice themselves.
pub(crate) fn switch_to_agent(app: &mut AppView, target: AgentId, cause: SwitchCause) {
    // Structural backstop for the auth + folder-trust session gate. This is the
    // single funnel every FRESH-agent creator routes through (New/Load/Fork —
    // `Picker` switches to an already-created, post-gate agent), so asserting the
    // gate here makes "no session is created while `TrustState::Pending`" a
    // property of the flow rather than of each call site: any future creator
    // that forgets the deferring chokepoint gate trips this in debug/tests. The
    // deferring chokepoints (`dispatch_new_session`/`_worktree_session`/
    // `_load_session_inner`) stash+return BEFORE reaching here, so this never
    // fires on the reachable gated paths.
    // `cause` is also used for New/Fork overlay follow (and remains live for
    // the debug_assert even when that assert compiles out in release).
    debug_assert!(
        matches!(cause, SwitchCause::Picker) || app.session_startup_allowed(),
        "session creation via {cause:?} requires the startup gate open (auth + folder trust)"
    );
    if !app.agents.contains_key(&target) {
        return;
    }
    if matches!(app.active_view, ActiveView::Agent(current) if current == target) {
        return;
    }
    // Capture before mutating active_view (subagent views are not top-level ids).
    let previous_top_level = match app.active_view {
        ActiveView::Agent(id) => Some(id),
        _ => None,
    };
    app.active_view = ActiveView::Agent(target);
    // Re-anchor the global permission-mode mirror to the now-active agent so the
    // cycle's `sync_active_auto_flag` (which derives from the global) can't copy a
    // different agent's stale Auto/Always-Approve onto this one. Per-session
    // yolo/auto are the source of truth; the global is a write-only mirror.
    sync_active_permission_mode_mirror(app);
    // Seed the auto feature gate on the (possibly new) active agent's slash
    // registry.
    app.sync_permission_mode_slash_gate();
    surface_yolo_launch_block_notice(app, target);

    if matches!(cause, SwitchCause::New | SwitchCause::Fork)
        && let Some(previous) = previous_top_level
        && let Some(d) = app.dashboard.as_mut()
    {
        d.repoint_attach_if_on(previous, target);
    }
}

pub(super) fn find_agent_id_by_session_id(
    agents: &indexmap::IndexMap<AgentId, AgentView>,
    session_id: &str,
) -> Option<AgentId> {
    agents.iter().find_map(|(id, a)| {
        a.session
            .session_id
            .as_ref()
            .is_some_and(|sid| &*sid.0 == session_id)
            .then_some(*id)
    })
}

/// Root session match (for async kill-result routing off the active view).
pub(super) fn find_agent_by_session_id<'a>(
    agents: &'a mut indexmap::IndexMap<AgentId, AgentView>,
    session_id: &str,
) -> Option<&'a mut AgentView> {
    let id = find_agent_id_by_session_id(agents, session_id)?;
    agents.get_mut(&id)
}
