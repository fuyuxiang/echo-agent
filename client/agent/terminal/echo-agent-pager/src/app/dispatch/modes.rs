//! Plan, yolo, auto, and permission mode transitions and toasts.

use super::ctx::{NO_SESSION_NOTICE, with_active_agent};
use super::queue::{maybe_drain_queue, note_peek_page_flip};
use super::settings::ui::{refresh_open_settings_modals, save_success_toast};
use crate::app::actions::Effect;
use crate::app::app_view::{ActiveView, AppView};
use agent_client_protocol as acp;
use echo_agent_telemetry::session_ctx::log_event;

/// Show the current plan: if a plan file exists, open it in the preview
/// overlay popover. If no plan has been written yet, show a toast.
///
/// Delegates to `AgentView::show_plan_preview()` which reads the plan file
/// from `~/.echo-agent/sessions/<urlencoded_cwd>/<session_id>/plan.md`.
pub(super) fn dispatch_show_plan(app: &mut AppView) -> Vec<Effect> {
    with_active_agent(app, |agent| {
        if agent.plan_approval_view.is_some() {
            agent.reopen_plan_approval();
        } else {
            agent.show_plan_preview();
        }
    });
    vec![]
}

/// Enter plan mode via `/plan`.
///
/// When not in plan mode: emits `SetSessionMode` (or `SetModeThenPrompt`
/// if a description is provided). When already in plan mode: no-op with toast.
/// Use `/view-plan` to open the current saved plan preview.
///
/// When a description is present, the mode switch and prompt send must be
/// ordered: the mode switch ACP call must complete before the prompt is
/// dispatched. `SetModeThenPrompt` bundles both into a single spawned task
/// to guarantee this ordering.
pub(super) fn dispatch_enter_plan_mode(
    app: &mut AppView,
    description: Option<String>,
) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    let in_plan = agent.plan_mode_pending.unwrap_or(agent.plan_mode_active);
    if in_plan {
        app.show_toast("Already in plan mode. Use /view-plan to view the current plan.");
        return vec![];
    }

    let agent = app.agents.get_mut(&id).unwrap();
    let Some(session_id) = agent.session.session_id.clone() else {
        agent.show_toast(NO_SESSION_NOTICE);
        return vec![];
    };

    // Set optimistic pending state (same pattern as dispatch_cycle_mode).
    agent.plan_mode_pending = Some(true);
    tracing::info!("Plan mode entered via /plan slash command");

    let mode_id = acp::SessionModeId::new("plan");

    if let Some(desc) = description {
        // Enqueue and drain: maybe_drain_queue does all synchronous turn
        // setup (scrollback, start_turn, prompt_id) and returns a SendPrompt.
        // We combine it with the mode switch into a single sequential effect
        // so the mode switch completes before the prompt is sent.
        // The description is a plain prompt: capture composer-recognized
        // tokens like the normal submit path (offsets recomputed against
        // `desc` since the leading `/plan ` was stripped).
        let skill_token_ranges = agent
            .prompt
            .slash_controller
            .recognized_token_ranges(&desc, &agent.session.models);
        agent
            .session
            .enqueue_prompt_with_skill_tokens(desc, skill_token_ranges);
        let drain = maybe_drain_queue(agent);
        note_peek_page_flip(app, id, drain.page_flip_entry);
        let mut effects = Vec::with_capacity(1);
        for eff in drain.effects {
            match eff {
                Effect::SendPrompt {
                    agent_id,
                    text,
                    prompt_id,
                    skill_token_ranges,
                    ..
                } => {
                    effects.push(Effect::SetModeThenPrompt {
                        session_id: session_id.clone(),
                        mode_id: mode_id.clone(),
                        agent_id,
                        text,
                        prompt_id,
                        skill_token_ranges,
                    });
                }
                other => effects.push(other),
            }
        }
        // If drain was empty (not idle), just emit the mode switch — the
        // prompt stays queued and will drain naturally when the agent idles.
        if effects.is_empty() {
            effects.push(Effect::SetSessionMode {
                session_id,
                mode_id,
            });
        }
        effects
    } else {
        vec![Effect::SetSessionMode {
            session_id,
            mode_id,
        }]
    }
}

/// Set plan mode (on / off). PAGER-owned + ACP-mediated, per-session.
///
/// Optimistic flow: captures effective state (`pending.or(active)`),
/// sets `plan_mode_pending`, refreshes modals, toasts, then emits
/// `Effect::SetSessionMode`. Shell confirms via `CurrentModeUpdate`.
///
/// No explicit rollback — `SetSessionMode` has no failure surface.
/// If the ACP transport drops, `plan_mode_pending` stays set until
/// the next `CurrentModeUpdate` or session restart.
///
/// Idempotent: same value toasts but skips the ACP round-trip.
pub(super) fn set_plan_mode(
    app: &mut AppView,
    kind: crate::app::actions::PlanModeKind,
) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    let Some(session_id) = agent.session.session_id.clone() else {
        agent.show_toast(NO_SESSION_NOTICE);
        return vec![];
    };

    // Effective state: prefer optimistic pending over confirmed
    // active. Mirrors `dispatch_cycle_mode`'s `in_plan` read so
    // rapid toggles don't double-send.
    let prev = agent.plan_mode_pending.unwrap_or(agent.plan_mode_active);
    let new = kind.to_bool();

    // Idempotent: toast but skip the ACP round-trip.
    if prev == new {
        app.show_toast(&plan_mode_toast(kind));
        return vec![];
    }

    // Optimistic mutation: pager-side pending flag, then UI feedback,
    // then effect. The shell's `CurrentModeUpdate` broadcast will
    // confirm + clear `plan_mode_pending` via `detect_plan_mode_change`.
    agent.plan_mode_pending = Some(new);
    refresh_open_settings_modals(app);
    app.show_toast(&plan_mode_toast(kind));

    tracing::info!(
        target: "settings",
        key = "plan_mode",
        value = new,
        "setting changed",
    );

    // OFF targets `SessionMode::Default`, not the user's prior mode.
    // If the user was in `Ask` (shell-injection only), that preference
    // is silently dropped. See `PLAN_MODE_CHOICES` in `settings/defs.rs`.
    let mode_id = acp::SessionModeId::new(if new {
        echo_agent_tools::types::SessionMode::Plan.as_id()
    } else {
        echo_agent_tools::types::SessionMode::Default.as_id()
    });

    vec![Effect::SetSessionMode {
        session_id,
        mode_id,
    }]
}

/// Format the `Plan mode` toast. Non-destructive in both directions
/// (unlike YOLO), so both ON and OFF use the uniform ✓ glyph.
/// Uses lowercase "on"/"off" via `save_success_toast`.
fn plan_mode_toast(kind: crate::app::actions::PlanModeKind) -> String {
    save_success_toast("Plan mode", kind.to_bool())
}

/// The single gate for client paths that ENABLE always-approve: `Some(reason)`
/// iff `enabling` and the pin (`app.yolo_policy_block`) is set. Every enabling
/// path routes through here (or [`refuse_if_yolo_locked`]) so new paths stay
/// gated by default; callers must NOT persist on a refusal.
pub(super) fn yolo_enable_blocked(app: &AppView, enabling: bool) -> Option<&'static str> {
    if enabling {
        app.yolo_policy_block
    } else {
        None
    }
}

/// `Vec<Effect>` wrapper for the persisting setters: on a refusal, toast and
/// return `Some(vec![])` (no persist); `None` means proceed.
fn refuse_if_yolo_locked(app: &mut AppView, enabling: bool) -> Option<Vec<Effect>> {
    let warning = yolo_enable_blocked(app, enabling)?;
    app.show_toast(warning);
    Some(vec![])
}

/// Canonical "auto wins only when yolo is off" precedence — the single source
/// of truth for the yolo-over-auto rule applied at every reconnect / seed / meta
/// site. Callers pass the already-resolved auto signal (a per-session flag or a
/// `permission_mode == Some("auto")` test).
pub(crate) fn effective_auto(yolo: bool, auto: bool) -> bool {
    !yolo && auto
}

/// When the auto gate is off, force the displayed permission mode off Auto and
/// clear every agent's per-session auto flag, so the UI / Shift+Tab cycle /
/// settings snapshot and each tab's badge never show Auto while the feature is
/// disabled. Shared by the startup reconcile and the mid-session kill-switch.
/// Clearing every agent (not just when the global mirror still reads "auto")
/// matters because `switch_to_agent` re-anchors the mirror to the active tab.
pub(crate) fn downgrade_displayed_auto_if_gated(app: &mut AppView) {
    if app.auto_mode_gate {
        return;
    }
    for agent in app.agents.values_mut() {
        agent.session.auto_mode = false;
    }
    if let Some(dashboard) = app.dashboard.as_mut()
        && dashboard.pending_mode == crate::views::dashboard::DashboardDispatchMode::Auto
    {
        dashboard.pending_mode = crate::views::dashboard::DashboardDispatchMode::Normal;
    }
    if app.current_ui.permission_mode.as_deref() == Some("auto") {
        app.current_ui.permission_mode = Some("ask".into());
    }
}

/// Whether a newly created session should start with the Auto display flag set:
/// the gate is on, the current UI mode is Auto, and yolo is not winning. Mirrors
/// the canonical `auto && !yolo` precedence used on the wire (`ClientCapabilities`
/// / `SessionFlags`). The `auto_mode_gate` check is defense-in-depth so a stale
/// `current_ui == "auto"` can never seed a new session into Auto when gated off.
pub(super) fn inherit_auto_mode(app: &AppView) -> bool {
    app.auto_mode_gate
        && effective_auto(
            app.default_yolo,
            app.current_ui.permission_mode.as_deref() == Some("auto"),
        )
}

/// Keep the active session's `auto_mode` display flag in lockstep with the
/// applied canonical permission mode. The canonical (`app.current_ui
/// .permission_mode`) is the single value every mode-change path finalizes —
/// the cycle, the settings setter, and the rollback all write it — so deriving
/// the flag from it (and clearing it under yolo, which wins) keeps the prompt
/// "auto" indicator correct regardless of which seam applied the mode.
pub(super) fn sync_active_auto_flag(app: &mut AppView) {
    let is_auto = app.current_ui.permission_mode.as_deref() == Some("auto");
    if let ActiveView::Agent(id) = app.active_view
        && let Some(agent) = app.agents.get_mut(&id)
    {
        agent.session.auto_mode = effective_auto(agent.session.is_yolo(), is_auto);
    }
    // Keep `/auto` feature-gate visibility in lockstep across slash surfaces.
    app.sync_permission_mode_slash_gate();
}

/// State-only `permission_mode` (YOLO) mutation; also called from rollback.
/// Flips to ON are refused while the pin is set.
pub(super) fn set_yolo_mode_inner(app: &mut AppView, new: bool) {
    if yolo_enable_blocked(app, new).is_some() {
        tracing::warn!("always-approve enable blocked by managed policy");
        return;
    }
    // Global mirrors update unconditionally (even if the user navigated
    // away from the agent mid-rollback). Per-agent state is gated below.
    app.default_yolo = new;
    app.permission_mode_from_soft_default = false;
    // Write-only mirror — see fn doc-comment.
    app.current_ui.permission_mode = Some(if new { "always-approve" } else { "ask" }.to_string());

    let ActiveView::Agent(id) = app.active_view else {
        return;
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return;
    };

    let previous_state = agent.session.is_yolo();

    // Drain ordering invariant: flag flip BEFORE the drain (see fn
    // doc-comment). Do NOT reorder these without re-reading the
    // contract.
    agent.session.yolo_mode = new;

    if new {
        // YOLO ON: auto-approve all queued permissions. Drain runs
        // even on idempotent re-dispatch. Prefers `AllowOnce`; falls
        // back to `Cancelled` (never `AllowAlways`).
        agent.last_permission_click = None;
        for perm in agent.permission_queue.drain(..) {
            if let Some(allow) = perm
                .options
                .iter()
                .find(|o| o.kind == acp::PermissionOptionKind::AllowOnce)
            {
                perm.request
                    .response_tx
                    .send(Ok(acp::RequestPermissionResponse::new(
                        acp::RequestPermissionOutcome::Selected(
                            acp::SelectedPermissionOutcome::new(allow.option_id.clone()),
                        ),
                    )))
                    .ok();
            } else {
                perm.request
                    .response_tx
                    .send(Ok(acp::RequestPermissionResponse::new(
                        acp::RequestPermissionOutcome::Cancelled,
                    )))
                    .ok();
            }
        }
        super::permissions::restore_permission_stashes(agent);
    }

    // Telemetry + tracing guarded on real state change only.
    if previous_state != new {
        echo_agent_telemetry::session_ctx::log_event(echo_agent_telemetry::events::YoloToggled {
            enabled: new,
            previous_state,
            trigger: echo_agent_telemetry::events::YoloTrigger::Pager,
        });
        tracing::info!(target: "settings", key = "permission_mode", value = new, "setting changed");
    }
}

/// Set YOLO (`permission_mode`). SHELL-owned, emits
/// `Effect::PersistPermissionMode` with rollback. The drain runs
/// unconditionally on YOLO=ON (even duplicate dispatches) because
/// a permission could arrive between dispatches.
fn capture_prev_permission_canonical(app: &AppView, prev_yolo: bool) -> &'static str {
    if prev_yolo {
        "always-approve"
    } else {
        match app.current_ui.permission_mode.as_deref() {
            Some("default") => "default",
            Some("auto") => "auto",
            _ => "ask",
        }
    }
}

pub(super) fn set_yolo_mode(app: &mut AppView, new: bool) -> Vec<Effect> {
    // Managed policy pins always-approve off — no state change, no persist.
    if let Some(blocked) = refuse_if_yolo_locked(app, new) {
        return blocked;
    }
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    // Capture LIVE yolo + plan state and session_id atomically for rollback.
    let (prev_yolo, session_id, effective_plan) = app
        .agents
        .get(&id)
        .map(|a| {
            (
                a.session.is_yolo(),
                a.session.session_id.clone(),
                a.plan_mode_pending.unwrap_or(a.plan_mode_active),
            )
        })
        .unwrap_or((false, None, false));
    let prev_canonical = capture_prev_permission_canonical(app, prev_yolo);

    set_yolo_mode_inner(app, new);

    // Refresh modal snapshots so the indicator reflects the new value.
    refresh_open_settings_modals(app);
    // Toggling yolo always lands on ask/always-approve (never auto); keep the
    // per-session auto display flag in sync (clears it).
    sync_active_auto_flag(app);

    // Toast on every save. YOLO ON gets a weightier visual; under an active
    // plan mode, say the plan edit gate stays binding — "all tool actions
    // auto-run" would overpromise while the shell rejects non-plan-file edits.
    if new && effective_plan {
        app.show_toast(YOLO_ON_UNDER_PLAN_TOAST);
    } else {
        app.show_toast(&yolo_toast(new));
    }

    // Forward write is always "ask" or "always-approve" (bool entry
    // point). Rollback uses `prev_canonical` with LIVE precedence.
    let canonical: &'static str = if new { "always-approve" } else { "ask" };
    vec![Effect::PersistPermissionMode {
        canonical,
        session_id,
        persist: crate::app::actions::PermissionModePersist::WithRollback(prev_canonical),
    }]
}

/// Set permission mode by typed kind. Entry point from the settings
/// modal. Mirrors `set_yolo_mode` but preserves the canonical string
/// (the inner collapses "default" onto "ask"; this setter restores
/// the distinction by overriding `app.current_ui.permission_mode`
/// after the inner call). Rollback uses LIVE-precedence canonical.
pub(super) fn set_permission_mode(
    app: &mut AppView,
    kind: crate::app::actions::PermissionModeKind,
) -> Vec<Effect> {
    // Feature gate: a commit to Auto is inert when the auto permission-mode
    // feature is disabled. Reading `app.auto_mode_gate` here (the same source
    // the Shift+Tab cycle uses) keeps the settings modal and the cycle in
    // lockstep — both degrade Auto → Ask when the gate is off.
    let kind =
        if matches!(kind, crate::app::actions::PermissionModeKind::Auto) && !app.auto_mode_gate {
            crate::app::actions::PermissionModeKind::Ask
        } else {
            kind
        };
    // Managed policy pins always-approve off — keep the modal on live state.
    if let Some(blocked) = refuse_if_yolo_locked(app, kind.is_always_approve()) {
        refresh_open_settings_modals(app);
        return blocked;
    }
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    // Capture LIVE yolo + plan state and session_id atomically for rollback.
    let (prev_yolo, session_id, effective_plan) = app
        .agents
        .get(&id)
        .map(|a| {
            (
                a.session.is_yolo(),
                a.session.session_id.clone(),
                a.plan_mode_pending.unwrap_or(a.plan_mode_active),
            )
        })
        .unwrap_or((false, None, false));
    let prev_canonical = capture_prev_permission_canonical(app, prev_yolo);

    // State mutation via shared inner. We overwrite the canonical
    // below for the Default case. Inner clears the soft-default latch.
    set_yolo_mode_inner(app, kind.is_always_approve());

    // Restore the "default" distinction the inner's bool-projection
    // collapses. No-op for `AlwaysApprove` and `Ask`.
    app.current_ui.permission_mode = Some(kind.as_canonical().to_string());

    // Refresh modal so its snapshot reflects the overridden canonical.
    refresh_open_settings_modals(app);
    // Keep the per-session auto display flag in sync with the applied canonical
    // (`kind` was already degraded to Ask when the gate is off, so a remaining
    // Auto here means the gate passed).
    sync_active_auto_flag(app);

    // Toast on every save (plan-aware for AlwaysApprove, mirroring
    // `set_yolo_mode` — the plan edit gate stays binding under yolo).
    if kind.is_always_approve() && effective_plan {
        app.show_toast(YOLO_ON_UNDER_PLAN_TOAST);
    } else {
        app.show_toast(&permission_mode_toast(kind));
    }

    vec![Effect::PersistPermissionMode {
        canonical: kind.as_canonical(),
        session_id,
        persist: crate::app::actions::PermissionModePersist::WithRollback(prev_canonical),
    }]
}

/// Build the toast for a `permission_mode` commit. `AlwaysApprove`
/// reuses `yolo_toast(true)` (destructive). `Ask` and `Default` get
/// dedicated "Permission mode: ..." toasts matching the picker brand.
pub(super) fn permission_mode_toast(kind: crate::app::actions::PermissionModeKind) -> String {
    use crate::app::actions::PermissionModeKind;
    match kind {
        PermissionModeKind::AlwaysApprove => yolo_toast(true),
        PermissionModeKind::Auto => "\u{2713} Permission mode: Auto (classifier)".to_string(),
        PermissionModeKind::Ask => "\u{2713} Permission mode: Ask".to_string(),
        PermissionModeKind::Default => "\u{2713} Permission mode: Default".to_string(),
    }
}

/// YOLO-ON toast when plan mode is active: always-approve arms the permission
/// fast path, but the shell's plan-mode gate still rejects non-plan-file
/// edits, so the standard "all tool actions auto-run" would overpromise.
pub(super) const YOLO_ON_UNDER_PLAN_TOAST: &str =
    "\u{26A0} Always-approve ON: plan mode still blocks file edits until you exit plan mode";

/// Build the YOLO toast — ⚠ on ON (destructive), ✓ on OFF (safe default).
fn yolo_toast(new: bool) -> String {
    if new {
        // Warning glyph + consequence — only post-commit feedback.
        "\u{26A0} Always-approve ON: all tool actions auto-run".to_string()
    } else {
        // OFF restores safe default — uniform ✓ glyph.
        save_success_toast("Always-approve", false)
    }
}

/// Toggle YOLO mode (Ctrl+O keybinding path). Delegates to the
/// registry-driven `set_yolo_mode` so permission-queue draining,
/// telemetry, and persistence all flow through a single code path.
pub(super) fn dispatch_toggle_yolo(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get(&id) else {
        return vec![];
    };
    let new = !agent.session.yolo_mode;
    set_yolo_mode(app, new)
}

/// Shift+Tab mode cycle from the agent chat view: the shared cycle body plus
/// plan-nudge acceptance telemetry (the nudge advertises this chord). The
/// dashboard peek calls [`dispatch_cycle_mode_and_sync`] instead, so a peeked
/// agent — whose prompt the user is not looking at — never attributes an accept
/// and never collapses Auto/Always-Approve for the nudge jump.
pub(super) fn dispatch_cycle_mode(app: &mut AppView) -> Vec<Effect> {
    // Capture the pre-cycle nudge visibility + plan state so only a transition
    // into Plan taken while the nudge is on screen attributes as an acceptance;
    // a disabled/absent nudge never emits.
    let (nudge_showing, in_plan_before) = active_agent_plan_nudge_state(app);
    // Tip copy promises one Shift+Tab → Plan; collapse Auto/Always-Approve to
    // ask first so the ring's Normal→Plan arm is the sole Plan entry.
    let mut effects = collapse_to_ask_for_nudge_jump(app).unwrap_or_default();
    effects.extend(dispatch_cycle_mode_and_sync(app));
    // Re-read only `in_plan`, via the same mut agent handle used to retire the
    // nudge: entering Plan with the nudge up is an acceptance.
    if nudge_showing
        && !in_plan_before
        && let ActiveView::Agent(id) = app.active_view
        && let Some(agent) = app.agents.get_mut(&id)
        && agent.plan_mode_pending.unwrap_or(agent.plan_mode_active)
    {
        log_event(echo_agent_telemetry::events::ContextualTip {
            tip: echo_agent_telemetry::events::ContextualTipKind::PlanMode,
            action: echo_agent_telemetry::events::ContextualTipAction::Accepted,
        });
        // Retire the now-stale nudge so one impression maps to at most one
        // acceptance — a full mode loop back to Plan within the ~3s TTL would
        // otherwise re-emit — unifying with the undo/image tips' clear-on-accept.
        agent
            .ephemeral_tip
            .clear(crate::tips::plan_nudge::PLAN_NUDGE_KEY);
    }
    effects
}

/// When the plan nudge is showing and the active agent is in Auto or
/// Always-Approve, collapse permission to ask (no banner / no Plan effects)
/// so the subsequent ring step is Normal→Plan. Returns `None` when the ring
/// should run alone (Normal, absent nudge, already-in-plan, or no session).
/// Agent-view only — peek never calls this.
fn collapse_to_ask_for_nudge_jump(app: &mut AppView) -> Option<Vec<Effect>> {
    let ActiveView::Agent(id) = app.active_view else {
        return None;
    };
    let agent = app.agents.get(&id)?;
    if agent.ephemeral_tip.current_key() != Some(crate::tips::plan_nudge::PLAN_NUDGE_KEY) {
        return None;
    }
    let in_plan = agent.plan_mode_pending.unwrap_or(agent.plan_mode_active);
    if in_plan {
        return None;
    }
    let in_yolo = agent.session.is_yolo();
    let in_auto = agent.session.is_auto();
    // Normal → Plan is already a single ring step; only collapse Auto / yolo.
    if !in_yolo && !in_auto {
        return None;
    }
    let session_id = agent.session.session_id.clone()?;

    if in_yolo {
        set_yolo_mode_inner(app, false);
    }
    app.current_ui.permission_mode = Some("ask".into());
    sync_active_auto_flag(app);
    tracing::info!("Mode cycle: collapse to ask for plan nudge jump");
    Some(vec![Effect::PersistPermissionMode {
        canonical: "ask",
        session_id: Some(session_id),
        persist: crate::app::actions::PermissionModePersist::BestEffort,
    }])
}

/// The Shift+Tab cycle body shared by the agent view and the dashboard peek:
/// apply the mode, then keep the per-session `auto_mode` display flag in sync
/// with the freshly written canonical mode — covering every arm (including the
/// pre-session and policy-pin early returns) without per-arm edits. Deliberately
/// telemetry-free: the dashboard peek reuses it so it can't attribute a
/// plan-nudge acceptance for an agent the user isn't viewing.
pub(super) fn dispatch_cycle_mode_and_sync(app: &mut AppView) -> Vec<Effect> {
    app.permission_mode_from_soft_default = false;
    let effects = dispatch_cycle_mode_inner(app);
    sync_active_auto_flag(app);
    effects
}

/// The active agent's `(plan nudge visible, optimistically in plan mode)`, or
/// `(false, false)` with no active agent. Lets [`dispatch_cycle_mode`] attribute
/// a shift+tab that turns plan mode on while the nudge shows as an acceptance.
pub(super) fn active_agent_plan_nudge_state(app: &AppView) -> (bool, bool) {
    let ActiveView::Agent(id) = app.active_view else {
        return (false, false);
    };
    match app.agents.get(&id) {
        Some(agent) => (
            agent.ephemeral_tip.current_key() == Some(crate::tips::plan_nudge::PLAN_NUDGE_KEY),
            agent.plan_mode_pending.unwrap_or(agent.plan_mode_active),
        ),
        None => (false, false),
    }
}

/// Cycle session mode: Normal → Plan → Always-Approve → Normal.
///
/// Uses `plan_mode_pending` (optimistic) when available, falling back to
/// `plan_mode_active` (confirmed by ACP). This prevents double-sends when
/// the user presses Shift+Tab faster than the ACP round-trip.
fn dispatch_cycle_mode_inner(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    // Capture the pin before borrowing `agent`: the "→ Always-Approve" arms are
    // yolo-enabling, but a `yolo_enable_blocked(app, _)` call would conflict with
    // the live `&mut agent`. This is the same predicate (enabling = true here).
    let yolo_locked = app.yolo_policy_block;
    // Feature gate (default ON): when the auto permission mode is disabled, the
    // Shift+Tab cycle skips Auto entirely (legacy Normal→Plan→Always-Approve→
    // Normal), so Auto is never reachable from the cycle. Resolved once at
    // startup into `app.auto_mode_gate`.
    let auto_gate = app.auto_mode_gate;
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    // Per-session (symmetric with the `in_yolo` reads below), not the global UI
    // mirror, so the cycle and the prompt "auto" indicator agree per agent.
    let in_auto = agent.session.is_auto();
    let Some(session_id) = agent.session.session_id.clone() else {
        // No session yet (Shift+Tab forwarded from the welcome screen or a
        // fresh tab): cycle the mode locally and stash the ACP push in
        // `deferred_session_mode` — consumed by the `SessionCreated`
        // handlers, same mechanism as the dashboard's staged plan mode.
        // Cycle: Normal → Plan → Auto → Always-Approve → Normal (Auto skipped
        // when always-approve is the only remaining arm under a yolo pin).
        // Each arm yields the canonical permission mode to persist (`None`
        // when it is untouched, i.e. Normal → Plan); see the push below.
        let in_plan = agent.plan_mode_pending.unwrap_or(agent.plan_mode_active);
        let in_yolo = agent.session.is_yolo();
        let persist_canonical: Option<&'static str> = match (in_plan, in_auto, in_yolo) {
            // Normal → Plan
            (false, false, false) => {
                agent.plan_mode_pending = Some(true);
                agent.deferred_session_mode = Some(echo_agent_tools::types::SessionMode::Plan);
                agent.show_mode_switch_banner("Plan");
                tracing::info!("Mode cycle (pre-session): Normal → Plan");
                None
            }
            // Plan → Auto (or Plan → Always-Approve when the auto feature is
            // gated off, matching the legacy Normal→Plan→Always-Approve cycle).
            (true, false, false) => {
                agent.plan_mode_pending = Some(false);
                agent.deferred_session_mode = None;
                if auto_gate {
                    // Clear any launch-seeded yolo so the created session isn't
                    // started in yolo while the UI shows Auto (SessionFlags reads
                    // default_yolo at CreateSession).
                    agent.session.yolo_mode = false;
                    app.default_yolo = false;
                    app.current_ui.permission_mode = Some("auto".into());
                    agent.show_mode_switch_banner("Auto");
                    tracing::info!("Mode cycle (pre-session): Plan → Auto");
                    Some("auto")
                } else if let Some(warning) = yolo_locked {
                    app.current_ui.permission_mode = Some("ask".into());
                    agent.session.yolo_mode = false;
                    app.default_yolo = false;
                    agent.show_toast(warning);
                    agent.show_mode_switch_banner("Normal");
                    tracing::info!("Mode cycle (pre-session): Plan → Normal (auto gated, policy)");
                    Some("ask")
                } else {
                    agent.session.yolo_mode = true;
                    app.default_yolo = true;
                    app.current_ui.permission_mode = Some("always-approve".into());
                    agent.show_mode_switch_banner("Always-Approve");
                    tracing::info!("Mode cycle (pre-session): Plan → Always-Approve (auto gated)");
                    Some("always-approve")
                }
            }
            // Auto → Always-Approve (or Normal if pinned)
            (false, true, false) => {
                if let Some(warning) = yolo_locked {
                    app.current_ui.permission_mode = Some("ask".into());
                    agent.session.yolo_mode = false;
                    app.default_yolo = false;
                    agent.show_toast(warning);
                    agent.show_mode_switch_banner("Normal");
                    tracing::info!("Mode cycle (pre-session): Auto → Normal (policy)");
                    Some("ask")
                } else {
                    agent.session.yolo_mode = true;
                    app.default_yolo = true;
                    app.current_ui.permission_mode = Some("always-approve".into());
                    agent.show_mode_switch_banner("Always-Approve");
                    tracing::info!("Mode cycle (pre-session): Auto → Always-Approve");
                    Some("always-approve")
                }
            }
            // Always-Approve → Normal
            (false, _, true) => {
                agent.session.yolo_mode = false;
                app.default_yolo = false;
                app.current_ui.permission_mode = Some("ask".into());
                agent.show_mode_switch_banner("Normal");
                tracing::info!("Mode cycle (pre-session): Always-Approve → Normal");
                Some("ask")
            }
            // Plan + Auto → Auto (exit plan, keep the classifier), matching the
            // with-session `(true, true, false, …)` arm. Every other plan+weird
            // state (notably Plan+yolo) resets to Normal, matching the
            // with-session catch-all — both paths MUST agree on the same input.
            // Clear stale yolo so enforcement matches the displayed mode.
            (true, _, _) => {
                agent.plan_mode_pending = Some(false);
                agent.deferred_session_mode = None;
                agent.session.yolo_mode = false;
                app.default_yolo = false;
                if auto_gate && in_auto && !in_yolo {
                    app.current_ui.permission_mode = Some("auto".into());
                    agent.show_mode_switch_banner("Auto");
                    tracing::info!("Mode cycle (pre-session): Plan+Auto → Auto");
                    Some("auto")
                } else {
                    app.current_ui.permission_mode = Some("ask".into());
                    agent.show_mode_switch_banner("Normal");
                    tracing::info!("Mode cycle (pre-session): Plan(*) → Normal");
                    Some("ask")
                }
            }
        };
        refresh_open_settings_modals(app);
        let mut effects = Vec::new();
        // Persist the displayed mode for the next launch; the pending CreateSession snapshots this mutation before execution.
        if let Some(canonical) = persist_canonical {
            effects.push(Effect::PersistPermissionMode {
                canonical,
                session_id: None,
                persist: crate::app::actions::PermissionModePersist::BestEffort,
            });
        }
        return effects;
    };

    // Effective plan state: prefer optimistic pending over confirmed active.
    let in_plan = agent.plan_mode_pending.unwrap_or(agent.plan_mode_active);
    let in_yolo = agent.session.is_yolo();

    match (in_plan, in_auto, in_yolo) {
        // Normal → Plan
        (false, false, false) => {
            agent.plan_mode_pending = Some(true);
            agent.show_mode_switch_banner("Plan");
            refresh_open_settings_modals(app);
            tracing::info!("Mode cycle: Normal → Plan");
            vec![Effect::SetSessionMode {
                session_id,
                mode_id: acp::SessionModeId::new(
                    echo_agent_tools::types::SessionMode::Plan.as_id(),
                ),
            }]
        }
        // Plan → Auto (classifier mode; exit plan, not always-approve).
        // When the auto feature is gated off, Plan → Always-Approve (skip Auto),
        // matching the legacy cycle and respecting the yolo policy pin.
        (true, false, false) => {
            agent.plan_mode_pending = Some(false);
            if !auto_gate {
                if let Some(warning) = yolo_locked {
                    set_yolo_mode_inner(app, false);
                    app.current_ui.permission_mode = Some("ask".into());
                    refresh_open_settings_modals(app);
                    if let Some(a) = app.agents.get_mut(&id) {
                        a.show_toast(warning);
                        a.show_mode_switch_banner("Normal");
                    }
                    tracing::info!(
                        "Mode cycle: Plan → Normal (auto gated, always-approve blocked by policy)"
                    );
                    // Exit Plan on the agent too; a policy pin must not strand the session in Plan.
                    return vec![
                        Effect::SetSessionMode {
                            session_id: session_id.clone(),
                            mode_id: acp::SessionModeId::new(
                                echo_agent_tools::types::SessionMode::Default.as_id(),
                            ),
                        },
                        Effect::PersistPermissionMode {
                            canonical: "ask",
                            session_id: Some(session_id),
                            persist: crate::app::actions::PermissionModePersist::BestEffort,
                        },
                    ];
                }
                set_yolo_mode_inner(app, true);
                app.current_ui.permission_mode = Some("always-approve".into());
                refresh_open_settings_modals(app);
                if let Some(a) = app.agents.get_mut(&id) {
                    a.show_mode_switch_banner("Always-Approve");
                }
                tracing::info!("Mode cycle: Plan → Always-Approve (auto gated)");
                return vec![
                    Effect::SetSessionMode {
                        session_id: session_id.clone(),
                        mode_id: acp::SessionModeId::new(
                            echo_agent_tools::types::SessionMode::Default.as_id(),
                        ),
                    },
                    Effect::PersistPermissionMode {
                        canonical: "always-approve",
                        session_id: Some(session_id),
                        persist: crate::app::actions::PermissionModePersist::BestEffort,
                    },
                ];
            }
            set_yolo_mode_inner(app, false);
            app.current_ui.permission_mode = Some("auto".into());
            refresh_open_settings_modals(app);
            if let Some(a) = app.agents.get_mut(&id) {
                a.show_mode_switch_banner("Auto");
            }
            tracing::info!("Mode cycle: Plan → Auto");
            vec![
                Effect::SetSessionMode {
                    session_id: session_id.clone(),
                    mode_id: acp::SessionModeId::new(
                        echo_agent_tools::types::SessionMode::Default.as_id(),
                    ),
                },
                Effect::PersistPermissionMode {
                    canonical: "auto",
                    session_id: Some(session_id),
                    persist: crate::app::actions::PermissionModePersist::BestEffort,
                },
            ]
        }
        // Auto → Always-Approve (or Normal when policy pins yolo off)
        (false, true, false) => {
            if let Some(warning) = yolo_locked {
                set_yolo_mode_inner(app, false);
                app.current_ui.permission_mode = Some("ask".into());
                refresh_open_settings_modals(app);
                if let Some(a) = app.agents.get_mut(&id) {
                    a.show_toast(warning);
                    a.show_mode_switch_banner("Normal");
                }
                tracing::info!("Mode cycle: Auto → Normal (always-approve blocked by policy)");
                return vec![Effect::PersistPermissionMode {
                    canonical: "ask",
                    session_id: Some(session_id),
                    persist: crate::app::actions::PermissionModePersist::BestEffort,
                }];
            }
            set_yolo_mode_inner(app, true);
            app.current_ui.permission_mode = Some("always-approve".into());
            refresh_open_settings_modals(app);
            if let Some(a) = app.agents.get_mut(&id) {
                a.show_mode_switch_banner("Always-Approve");
            }
            tracing::info!("Mode cycle: Auto → Always-Approve");
            vec![Effect::PersistPermissionMode {
                canonical: "always-approve",
                session_id: Some(session_id),
                persist: crate::app::actions::PermissionModePersist::BestEffort,
            }]
        }
        // Always-Approve → Normal
        (false, _, true) => {
            set_yolo_mode_inner(app, false);
            app.current_ui.permission_mode = Some("ask".into());
            refresh_open_settings_modals(app);
            if let Some(a) = app.agents.get_mut(&id) {
                a.show_mode_switch_banner("Normal");
            }
            tracing::info!("Mode cycle: Always-Approve → Normal");
            vec![Effect::PersistPermissionMode {
                canonical: "ask",
                session_id: Some(session_id),
                persist: crate::app::actions::PermissionModePersist::BestEffort,
            }]
        }

        // Plan + Auto → Auto: exit plan but keep the classifier. Without this
        // explicit arm the state falls to `_` and would reset to Normal/ask.
        (true, true, false) => {
            agent.plan_mode_pending = Some(false);
            app.current_ui.permission_mode = Some("auto".into());
            refresh_open_settings_modals(app);
            if let Some(a) = app.agents.get_mut(&id) {
                a.show_mode_switch_banner("Auto");
            }
            tracing::info!("Mode cycle: Plan+Auto → Auto (exit plan, keep classifier)");
            vec![
                Effect::SetSessionMode {
                    session_id: session_id.clone(),
                    mode_id: acp::SessionModeId::new(
                        echo_agent_tools::types::SessionMode::Default.as_id(),
                    ),
                },
                Effect::PersistPermissionMode {
                    canonical: "auto",
                    session_id: Some(session_id),
                    persist: crate::app::actions::PermissionModePersist::BestEffort,
                },
            ]
        }

        // Any other combination → reset to Normal.
        // YOLO inner only called when actually in YOLO (avoids
        // spurious telemetry).
        _ => {
            agent.plan_mode_pending = Some(false);
            // NLL releases the `agent` borrow after the assignment
            // above; `set_yolo_mode_inner(app, …)` can reborrow below.
            if in_yolo {
                set_yolo_mode_inner(app, false);
            }
            app.current_ui.permission_mode = Some("ask".into());
            refresh_open_settings_modals(app);
            if let Some(a) = app.agents.get_mut(&id) {
                a.show_mode_switch_banner("Normal");
            }
            tracing::info!("Mode cycle: mixed state → Normal");
            let mut effects = vec![];
            if in_plan {
                effects.push(Effect::SetSessionMode {
                    session_id: session_id.clone(),
                    mode_id: acp::SessionModeId::new(
                        echo_agent_tools::types::SessionMode::Default.as_id(),
                    ),
                });
            }
            if in_yolo || in_auto {
                effects.push(Effect::PersistPermissionMode {
                    canonical: "ask",
                    session_id: Some(session_id),
                    persist: crate::app::actions::PermissionModePersist::BestEffort,
                });
            }
            effects
        }
    }
}
