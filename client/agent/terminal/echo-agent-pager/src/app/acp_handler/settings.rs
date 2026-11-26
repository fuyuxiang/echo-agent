use super::*;
use serde::Deserialize;

/// Handle `echo.agent/models/update` — model list changed (etag-triggered refresh).
pub(super) fn handle_models_update(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    if let Ok(model_state) = serde_json::from_str::<acp::SessionModelState>(notif.params.get()) {
        use crate::acp::model_state::ModelState;
        let new_models = ModelState::from(Some(model_state));
        tracing::info!(
            count = new_models.available.len(),
            "models updated via echo.agent/models/update"
        );

        app.models.update_catalog(new_models.available.clone());
        let stale = app
            .models
            .current
            .as_ref()
            .is_none_or(|id| !app.models.available.contains_key(id));
        if stale && let Some(id) = new_models.current {
            app.models.set_current(id, None);
        }

        for agent in app.agents.values_mut() {
            if let Some(ref current) = agent.session.models.current
                && !new_models.available.contains_key(current)
            {
                tracing::debug!(
                    current_model = %current.0,
                    available_count = new_models.available.len(),
                    "models update dropped this session's model from the catalog; keeping it displayed"
                );
            }
            agent
                .session
                .models
                .update_catalog(new_models.available.clone());
        }
        true
    } else {
        tracing::warn!("Failed to parse echo.agent/models/update");
        false
    }
}

/// Handle `echo.agent/settings/update` — remote settings refreshed on `/new`.
pub(super) fn handle_settings_update(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    let Ok(update) = serde_json::from_str::<PagerSettingsUpdate>(notif.params.get()) else {
        tracing::warn!("Failed to parse echo.agent/settings/update");
        return false;
    };

    // Reseed this process's remote-campaign cache. In leader mode no in-process
    // agent seeds the TUI process, and the bounded startup prefetch can miss —
    // without this reseed a remote campaign stays invisible to
    // `resolve_dismissable_campaigns`, so a `/model` pick never records its
    // dismissal and the leader re-nudges every new session. Idempotent in
    // embedded mode, where the in-process agent seeds the same cache.
    if let Some(campaigns) = update.campaigns.clone() {
        let rs = echo_agent_runtime::util::config::RemoteSettings {
            campaigns,
            ..Default::default()
        };
        echo_agent_runtime::util::config::set_remote_campaigns_from_settings(Some(&rs));
    }

    if let Some(v) = update.auto_permission_mode_enabled {
        // Keep the pager's auto-permission-mode gate live with the remote settings
        // remote tier (the leader caches it agent-side; the pager process needs
        // its own copy). Refresh the startup snapshot so the Shift+Tab cycle and
        // the settings modal both reflect a remote-only enablement/kill-switch
        // without a restart.
        echo_agent_runtime::util::config::cache_remote_auto_permission_mode_enabled(Some(v));
        app.auto_mode_gate =
            echo_agent_runtime::util::config::auto_permission_mode_enabled_from_disk();
        // Mid-session kill switch: when the gate just went off, drop displayed
        // Auto to Ask + clear every agent's per-session flag (shared with the
        // startup reconcile), AND tell live sessions to leave Auto. Clearing only
        // the display would let the agent keep classifier-approving while the UI
        // shows "Ask" — the emergency-off must actually disable enforcement.
        if !app.auto_mode_gate {
            // Sessions to notify: agents that HAD Auto on (capture before the
            // downgrade clears the flag) and have a live session id.
            let leaving_auto: Vec<acp::SessionId> = app
                .agents
                .values()
                .filter(|a| a.session.is_auto())
                .filter_map(|a| a.session.session_id.clone())
                .collect();
            super::super::dispatch::downgrade_displayed_auto_if_gated(app);
            notify_sessions_leave_auto(app, &leaving_auto);
        }
        // Reveal/hide `/auto` on every slash surface in lockstep with the gate
        // (covers both a mid-session kill-switch and re-enablement).
        app.sync_permission_mode_slash_gate();
    }

    // `permission_mode` is presence-aware (omit / null / string). While the
    // soft default still owns the mode, a push re-arms `default_yolo` + UI for
    // the next `/new`; once the user claims a mode (Shift+Tab / settings /
    // `/mode`) the latch is cleared and pushes leave it alone.
    if let Some(remote_opt) = update.permission_mode.as_ref()
        && app.permission_mode_from_soft_default
    {
        // One config read at the I/O boundary; the applier is deterministic.
        let root = echo_agent_runtime::config::load_effective_config()
            .unwrap_or_else(|_| broken_config_ask_fallback());
        apply_soft_default_permission_mode(app, root.get("ui"), remote_opt.as_deref());
    }

    if let Some(v) = update.show_resolved_model {
        app.show_resolved_model = v;
    }
    // Temporary client kill switch: ignore remote `sharing_enabled` until
    // session share links are restored. Presence is still observed so a
    // later re-enable can go back to `app.sharing_enabled = v`.
    if update.sharing_enabled.is_some() {
        app.sharing_enabled = false;
        for agent in app.agents.values_mut() {
            agent.set_sharing_enabled(false);
        }
    }
    // Env overrides win over live updates too, mirroring the startup
    // resolution in event_loop — otherwise the proxy's explicit `false`
    // (sent for kill-switch semantics) clobbers a local test override
    // moments after launch.
    if let Some(v) = update.privacy_notice_rollout {
        app.privacy_notice_rollout =
            echo_agent_config::env_bool("ECHO_AGENT_PRIVACY_NOTICE_ROLLOUT").unwrap_or(v);
    }
    if let Some(v) = update.privacy_banner_reshow_days {
        app.privacy_banner_reshow_days = Some(
            std::env::var("ECHO_AGENT_PRIVACY_BANNER_RESHOW_DAYS")
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(v),
        );
    }
    // Tier before voice: same payload may set "API Key" and voice_mode_enabled=false.
    // Always recompute is_api_key_auth from the tier so a later Free/EchoAgent
    // stamp does not leave API-key bypass / a hidden billing surface stuck.
    if let Some(v) = update.subscription_tier_display {
        let was_api_key = app.is_api_key_auth;
        let is_key = super::super::app_view::is_api_key_label(&v);
        app.is_api_key_auth = is_key;
        app.usage_visible = !is_key && app.team_name.is_none() && !app.has_external_auth_provider;
        app.sync_billing_surface_to_agents();
        app.subscription_tier = Some(v);
        app.apply_tier_restrictions();
        // Leaving API Key → free/X Basic without a voice field: drop force-on.
        // Paid tiers keep voice; remote settings may send voice_mode_enabled later.
        if was_api_key
            && !is_key
            && update.voice_mode_enabled.is_none()
            && app
                .subscription_tier
                .as_deref()
                .is_some_and(echo_agent_runtime::tier::is_restricted_tier_name)
        {
            app.voice_reset();
            app.voice_ui_active = false;
            app.apply_voice_mode_enabled(false);
        }
    }
    if let Some(remote_v) = update.voice_mode_enabled {
        let v = crate::app::resolve_voice_mode_live(Some(remote_v), app.is_api_key_auth);
        if !v {
            app.voice_reset();
            app.voice_ui_active = false;
        }
        app.apply_voice_mode_enabled(v);
    } else {
        app.ensure_voice_for_api_key();
    }
    // TODO: extract resolve_session_picker_grouped helper (duplicates event_loop.rs:143-160)
    // Respect env var > config > remote precedence (mirrors event_loop.rs startup).
    if let Some(remote_val) = update.session_picker_grouped {
        let resolved = std::env::var("ECHO_AGENT_SESSION_PICKER_GROUPED")
            .ok()
            .and_then(|v| match v.as_str() {
                "1" | "true" => Some(true),
                "0" | "false" => Some(false),
                _ => None,
            })
            .or_else(|| {
                echo_agent_runtime::config::load_effective_config()
                    .ok()
                    .and_then(|cfg| cfg.get("cli")?.get("session_picker_grouped")?.as_bool())
            })
            .unwrap_or(remote_val);
        app.session_picker_grouped = resolved;
    }
    if let Some(v) = update.subscription_watch_interval_secs {
        app.subscription_watch_interval_secs = Some(v);
    }

    // Gate update logic:
    // - allow_access == Some(true): explicitly granted → lift the gate
    // - gate_message.is_some(): server sent a new message → impose/update
    // - Neither condition met: don't touch the gate. In particular,
    //   allow_access=Some(false) without a gate_message must NOT clear the
    //   gate (gate_from_settings returns None when gate_message is absent,
    //   which would incorrectly lift an existing gate).

    // A fresh machine has no auth at startup, so the prefetch never runs and the startup seed sees
    // no settings. Welcome only: arming behind a session blocks new sessions on an unseen screen.
    if let Some(gate) = update.consent_gate.as_ref()
        && matches!(app.consent_state, crate::app::consent::ConsentState::Done)
        && matches!(app.active_view, crate::app::app_view::ActiveView::Welcome)
        && app.agents.is_empty()
    {
        crate::app::event_loop::seed_consent_state_from_gate(app, Some(gate));
    }

    if update.allow_access == Some(true) {
        let effs = app.lift_gate();
        app.pending_effects.extend(effs);
    } else if let Some(msg) = update.gate_message.as_ref()
        && !msg.is_empty()
    {
        // (An empty gate_message would only clear the gate message text, NOT
        // access, so it intentionally does not touch the gate here.)
        let effs = app.impose_gate(echo_agent_runtime::auth::GateInfo {
            message: msg.clone(),
            url: update.gate_url.clone(),
            label: update.gate_label.clone(),
        });
        app.pending_effects.extend(effs);
    }

    // Load config layers once for tips + group_tool_verbs +
    // collapsed_edit_blocks resolution. Loaded unconditionally: the UI flags
    // re-resolve on every update (see below), and updates are rare (post-auth
    // refresh, `/new`), so three small TOML reads are fine.
    let (requirements, user_config, managed_config) = (
        echo_agent_runtime::config::load_merged_requirements(),
        echo_agent_runtime::config::load_from_disk().ok(),
        echo_agent_runtime::config::load_managed_config().ok(),
    );

    // Local layers may beat remote — re-resolve the full chain into the render
    // cache (mirrors the event_loop.rs startup resolve). Runs on None too: the
    // shell always publishes this field from its live remote tier, so None
    // means remote settings cleared it (or an older shell that cannot deliver the
    // remote tier at all) — either way resolving without a remote value is
    // correct, and it reverts a previously cached remote enable back to the
    // local/default (off) resolution instead of leaving Some(true) stuck
    // until restart.
    let remote = echo_agent_runtime::util::config::RemoteSettings {
        group_tool_verbs: update.group_tool_verbs,
        ..Default::default()
    };
    let resolved = echo_agent_runtime::util::config::resolve_group_tool_verbs(
        requirements.as_ref(),
        user_config.as_ref(),
        managed_config.as_ref(),
        Some(&remote),
    )
    .value;
    // On a real flip, re-fold every live transcript (mirrors dispatch's
    // set_group_tool_verbs_inner); unchanged values keep `/new` cheap.
    // Stale expansion ids describe the old grouping shape — drop them so the
    // re-fold can't reopen a verb slot expanded or mark a coincident dense
    // group expanded (see `clear_group_expansion`).
    if resolved != crate::appearance::cache::load_group_tool_verbs() {
        crate::appearance::cache::set_group_tool_verbs(resolved);
        for agent in app.agents.values_mut() {
            agent.scrollback.clear_group_expansion();
            agent.scrollback.invalidate_heights();
            for child in agent.subagent_views.values_mut() {
                child.scrollback.clear_group_expansion();
                child.scrollback.invalidate_heights();
            }
        }
    }

    // Same None-reverts contract as group_tool_verbs above: re-resolve the
    // full local chain with the pushed remote tier so a cleared remote settings
    // field falls back to local/default instead of staying latched.
    let remote = echo_agent_runtime::util::config::RemoteSettings {
        collapsed_edit_blocks: update.collapsed_edit_blocks,
        ..Default::default()
    };
    let resolved = echo_agent_runtime::util::config::resolve_collapsed_edit_blocks(
        requirements.as_ref(),
        user_config.as_ref(),
        managed_config.as_ref(),
        Some(&remote),
    )
    .value;
    // On a real flip, re-materialize on-default Edit rows + repaint suffixes
    // in every live transcript (mirrors dispatch's
    // set_collapsed_edit_blocks_inner); unchanged values keep `/new` cheap.
    let prev = crate::appearance::cache::load_collapsed_edit_blocks();
    if resolved != prev {
        crate::appearance::cache::set_collapsed_edit_blocks(resolved);
        for agent in app.agents.values_mut() {
            agent
                .scrollback
                .apply_collapsed_edit_blocks_flip(prev, resolved);
            for child in agent.subagent_views.values_mut() {
                child
                    .scrollback
                    .apply_collapsed_edit_blocks_flip(prev, resolved);
            }
        }
    }

    // `scheduler_background_loops` is deliberately absent from this handler,
    // unlike the flags above. A live session's scheduled fires keep the mode
    // the shell pinned when the session's actor spawned, so applying a pushed
    // flip here would make `/loop` promise a runtime those fires never get.
    // The per-session value arrives on the `session/new` / `session/load`
    // response instead (`AgentView::scheduler_background_loops`).

    // Re-resolve tips from config layers + the updated remote tips.
    if let Some(remote_tips) = update.tips {
        use echo_agent_runtime::util::config::resolve_tips;

        app.tips = resolve_tips(
            requirements.as_ref(),
            user_config.as_ref(),
            managed_config.as_ref(),
            Some(&remote_tips),
        );
        if !app.tips.is_empty() {
            let echo_agent_home = echo_agent_tools::util::echo_agent_home::echo_agent_home();
            app.tip = echo_agent_runtime::util::tips::pick_and_advance(&app.tips, &echo_agent_home);
        } else {
            app.tip = None;
        }
    }

    // Re-resolve dropdown tags only when the update carries the field. Some(None) =
    // remote cleared (drop remote layer); Some(Some(map)) = set; outer None = field
    // absent (older shell) → keep the tags resolved at startup. Env + local
    // [slash_command_tags] always apply via resolve_slash_command_tags.
    if let Some(remote_tags) = update.slash_command_tags.as_ref() {
        use echo_agent_runtime::util::config::resolve_slash_command_tags;
        let effective_config = echo_agent_runtime::config::load_effective_config().ok();
        let empty_toml = toml::Value::Table(Default::default());
        let tags_config = effective_config.as_ref().unwrap_or(&empty_toml);
        *app.command_tags.borrow_mut() =
            resolve_slash_command_tags(tags_config, remote_tags.as_ref());
    }

    tracing::info!("settings updated via echo.agent/settings/update");
    true
}

/// Stand-in effective config for a failed load: pins an explicit ask so a
/// broken config never soft-defaults into auto (mirrors the launch
/// resolver's fail-safe).
pub(super) fn broken_config_ask_fallback() -> toml::Value {
    let mut ui = toml::value::Table::new();
    ui.insert("permission_mode".into(), toml::Value::String("ask".into()));
    let mut root = toml::value::Table::new();
    root.insert("ui".into(), toml::Value::Table(ui));
    toml::Value::Table(root)
}

/// Re-arm the soft-defaulted launch mode from a pushed `permission_mode`
/// (TOML `[ui]` > remote > the interactive auto soft default, matching
/// `effective_auto_for_launch_interactive`), for the next `/new` only — live
/// sessions are untouched and nothing is persisted. `effective_ui` is
/// injected so the resolve is deterministic under test; on a failed config
/// load the caller substitutes [`broken_config_ask_fallback`]. Enforcement
/// gating reuses the app's startup snapshots (`yolo_policy_block`,
/// `auto_mode_gate`); the agent's permission manager re-clamps
/// authoritatively at decision time.
pub(super) fn apply_soft_default_permission_mode(
    app: &mut AppView,
    effective_ui: Option<&toml::Value>,
    remote: Option<&str>,
) {
    let mode = echo_agent_runtime::util::config::selected_permission_mode(effective_ui, remote)
        .unwrap_or(echo_agent_runtime::util::config::PermissionMode::Auto);
    app.default_yolo = mode.is_always_approve() && app.yolo_policy_block.is_none();
    let auto = mode.is_auto() && app.auto_mode_gate && !app.default_yolo;
    app.current_ui.permission_mode = Some(if auto {
        "auto".to_string()
    } else if app.default_yolo {
        "always-approve".to_string()
    } else {
        echo_agent_runtime::util::config::resolved_display_permission_mode(effective_ui, remote)
            .to_string()
    });
}

/// Tell live sessions to leave Auto on the mid-session kill-switch: fire the
/// `echo.agent/yolo_mode_changed` notification the agent maps to
/// `SetAutoMode { enabled: false }`, fire-and-forget over the shared ACP channel.
/// The notification is CLIENT-scoped (the agent applies it to every session of
/// the sending client), so one send covers all affected sessions. `yolo_mode` is
/// deliberately OMITTED — the agent skips the yolo branch when the key is absent,
/// so a sibling tab's always-approve is preserved; only auto is cleared.
pub(super) fn notify_sessions_leave_auto(app: &AppView, session_ids: &[acp::SessionId]) {
    if session_ids.is_empty() {
        return;
    }
    let params = serde_json::json!({
        "auto_mode": false,
        "permission_mode": "ask",
    });
    let notification = acp::ExtNotification::new(
        "echo.agent/yolo_mode_changed",
        serde_json::value::to_raw_value(&params)
            .expect("serialize yolo_mode_changed params")
            .into(),
    );
    let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
    let args = echo_agent_acp::AcpArgs {
        request: notification,
        response_tx,
    };
    let _ = app.acp_tx.send(args.into());
}

/// Handle `echo.agent/sessions/changed` — the leader broadcasts roster
/// upserts/removals to all clients (FleetView dashboard).
pub(super) fn handle_sessions_changed(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    let Ok(changed) = serde_json::from_str::<crate::app::roster::RosterChanged>(notif.params.get())
    else {
        tracing::warn!("Failed to parse echo.agent/sessions/changed");
        return false;
    };
    let mut affected = false;
    for entry in changed.upserted {
        app.upsert_roster_entry(entry);
        affected = true;
    }
    for sid in changed.removed {
        app.remove_roster_entry(&sid);
        affected = true;
    }
    affected
}

pub(super) fn handle_announcements_update(notif: &acp::ExtNotification, app: &mut AppView) -> bool {
    let Ok(parsed) = serde_json::from_str::<echo_agent_announcements::AnnouncementsRefreshed>(
        notif.params.get(),
    ) else {
        return false;
    };

    if parsed.r#gen <= app.announcements_last_gen {
        return false;
    }

    // Re-merge config layers like startup (and the pre-unification settings
    // branch) did: the push carries the remote list only, and a wholesale
    // replace would drop requirements/user/managed announcements and let the
    // prune erase their persisted hide keys. Same disk reads the settings
    // branch performed; pushes are rare.
    let requirements = echo_agent_runtime::config::load_merged_requirements();
    let user_config = echo_agent_runtime::config::load_from_disk().ok();
    let managed_config = echo_agent_runtime::config::load_managed_config().ok();
    apply_announcements_update(
        app,
        parsed.r#gen,
        &parsed.announcements,
        requirements.as_ref(),
        user_config.as_ref(),
        managed_config.as_ref(),
    );
    true
}

/// Apply half of [`handle_announcements_update`], with config layers injected
/// so the merge/prune behavior is unit-testable without disk state.
/// `resolve_announcements` honors `ECHO_AGENT_ANNOUNCEMENTS_OVERRIDE` first, so a
/// backend push can't reintroduce announcements when the override is set.
pub(super) fn apply_announcements_update(
    app: &mut AppView,
    next_gen: u64,
    remote: &[echo_agent_announcements::RemoteAnnouncement],
    requirements: Option<&toml::Value>,
    user_config: Option<&toml::Value>,
    managed_config: Option<&toml::Value>,
) {
    let merged = echo_agent_runtime::util::config::resolve_announcements(
        requirements,
        user_config,
        managed_config,
        Some(remote),
    );
    let announcements = echo_agent_announcements::filter_expired(merged);

    app.announcement = match app.announcement.as_ref() {
        Some(current) => announcements
            .iter()
            .find(|a| *a == current)
            .cloned()
            .or_else(|| pick_random_announcement(&announcements)),
        None => pick_random_announcement(&announcements),
    };
    app.active_announcements = announcements;
    app.announcements_last_gen = next_gen;
    // Opportunistic per-ID prune on a real update (never per frame) so the hidden set cannot grow unboundedly.
    if echo_agent_announcements::prune_hidden_announcement_ids(
        &mut app.hidden_announcement_ids,
        &app.active_announcements,
    ) {
        app.pending_effects
            .push(Effect::PersistAnnouncementsHidden {
                hidden_ids: app.hidden_announcement_ids.clone(),
            });
    }
    app.sync_session_announcement_slash_gate();
}

pub(super) fn pick_random_announcement(
    announcements: &[echo_agent_announcements::RemoteAnnouncement],
) -> Option<echo_agent_announcements::RemoteAnnouncement> {
    if announcements.is_empty() {
        return None;
    }
    use rand::Rng;
    let idx = rand::rng().random_range(0..announcements.len());
    announcements.get(idx).cloned()
}

/// Deserialization type for the `echo.agent/settings/update` notification payload.
///
/// This is intentionally a separate struct from `SettingsUpdateNotification` in
/// `echo-agent-runtime/src/agent/mvp_agent.rs`. The shell side derives `Serialize`
/// and owns the canonical field set from `RemoteSettings`; this pager side
/// derives `Deserialize` and selectively consumes only the fields relevant to
/// the TUI. Keeping them separate avoids coupling the pager to shell internals
/// and lets each side evolve independently (e.g. adding a shell-only field
/// doesn't require a pager change). All fields are `Option` with
/// `#[serde(default)]` so that partial updates and forward-compatible additions
/// are handled gracefully.
///
/// **Keep in sync** with field names/types in `SettingsUpdateNotification` at
/// `echo-agent-runtime/src/agent/mvp_agent.rs` when adding fields that both sides
/// need.
#[derive(serde::Deserialize)]
pub(super) struct PagerSettingsUpdate {
    #[serde(default)]
    show_resolved_model: Option<bool>,
    #[serde(default)]
    sharing_enabled: Option<bool>,
    #[serde(default)]
    privacy_notice_rollout: Option<bool>,
    #[serde(default)]
    privacy_banner_reshow_days: Option<u64>,
    #[serde(default)]
    voice_mode_enabled: Option<bool>,
    #[serde(default)]
    session_picker_grouped: Option<bool>,
    #[serde(default)]
    tips: Option<Vec<String>>,
    /// Free-form per-command slash-dropdown tags (canonical name → tag).
    /// Presence-aware and tolerant: omit = no update (older shell), `null` =
    /// remote cleared, map = set, malformed = warn + treat as absent so a
    /// bad value never fails the whole `PagerSettingsUpdate` parse.
    #[serde(default, deserialize_with = "deserialize_settings_update_tags")]
    slash_command_tags: Option<Option<std::collections::BTreeMap<String, String>>>,
    // `announcements` is deliberately NOT consumed here: every shell writer of
    // remote_settings also emits gen-ordered `echo.agent/announcements/update`
    // (emit_announcements_if_changed), and a gen-less apply on this path could
    // clobber a newer push. Single ingest path: handle_announcements_update.
    /// Remote campaigns snapshot. `Some` whenever the shell has settings
    /// (empty = campaigns withdrawn); `None`/omitted (settings-less push,
    /// older shell) must leave this process's campaign cache untouched.
    #[serde(default)]
    campaigns: Option<Vec<echo_agent_runtime::util::config::CampaignOverride>>,
    #[serde(default)]
    gate_message: Option<String>,
    #[serde(default)]
    gate_url: Option<String>,
    #[serde(default)]
    gate_label: Option<String>,
    #[serde(default)]
    allow_access: Option<bool>,
    #[serde(default)]
    subscription_tier_display: Option<String>,
    #[serde(default)]
    auto_permission_mode_enabled: Option<bool>,
    /// Soft-default permission mode. Presence-aware: omit = no update,
    /// `null` = recompute with remote=None, string = that soft-default.
    /// Omission happens with older shells that predate the field (they can
    /// never clear a mode they don't know about) — that version skew is why
    /// this is tri-state instead of a plain `Option`.
    #[serde(default, deserialize_with = "deserialize_presence_aware_string")]
    permission_mode: Option<Option<String>>,
    #[serde(default)]
    group_tool_verbs: Option<bool>,
    #[serde(default)]
    collapsed_edit_blocks: Option<bool>,
    #[serde(default)]
    subscription_watch_interval_secs: Option<u64>,
    /// Tolerant for the same reason as the settings response it mirrors: a malformed gate must not
    /// discard the tier, permission mode and campaigns that arrive with it.
    #[serde(
        default,
        deserialize_with = "echo_agent_runtime::util::config::deserialize_tolerant"
    )]
    consent_gate: Option<echo_agent_runtime::util::config::ConsentGate>,
}

/// Presence-aware string: omit → `None` (`#[serde(default)]`), null →
/// `Some(None)`, string → `Some(Some(_))`.
fn deserialize_presence_aware_string<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(deserializer)?))
}

/// Presence-aware + tolerant tags map for live settings updates.
/// Only invoked when the field is present (`#[serde(default)]` covers omit).
/// - JSON null → `Some(None)` (explicit remote clear)
/// - valid object → `Some(Some(map))`
/// - malformed → warn + `Ok(None)` (leave tags alone; do not fail the struct)
fn deserialize_settings_update_tags<'de, D>(
    deserializer: D,
) -> Result<Option<Option<std::collections::BTreeMap<String, String>>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(Some(None)),
        v => match serde_json::from_value::<std::collections::BTreeMap<String, String>>(v) {
            Ok(m) => Ok(Some(Some(m))),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "malformed slash_command_tags in settings update; leaving tags unchanged"
                );
                Ok(None)
            }
        },
    }
}

#[cfg(test)]
mod presence_aware_dto_tests {
    use super::*;

    #[derive(Deserialize)]
    struct Probe {
        #[serde(default, deserialize_with = "deserialize_presence_aware_string")]
        permission_mode: Option<Option<String>>,
    }

    #[test]
    fn permission_mode_dto_distinguishes_omit_from_null() {
        let omit: Probe = serde_json::from_value(serde_json::json!({
            "show_resolved_model": true,
        }))
        .unwrap();
        assert_eq!(omit.permission_mode, None, "omit must be None (no update)");

        let null_v: Probe = serde_json::from_value(serde_json::json!({
            "permission_mode": null,
        }))
        .unwrap();
        assert_eq!(
            null_v.permission_mode,
            Some(None),
            "explicit null must be Some(None)"
        );

        let some_v: Probe = serde_json::from_value(serde_json::json!({
            "permission_mode": "always-approve",
        }))
        .unwrap();
        assert_eq!(
            some_v.permission_mode,
            Some(Some("always-approve".into())),
            "string must be Some(Some(_))"
        );
    }

    #[test]
    fn malformed_consent_gate_does_not_discard_the_rest_of_the_update() {
        let update: PagerSettingsUpdate = serde_json::from_value(serde_json::json!({
            "consent_gate": {"version": "not-a-number"},
            "tips": ["still applied"],
        }))
        .expect("a malformed gate must not fail the whole update");

        assert!(update.consent_gate.is_none());
        assert_eq!(
            update.tips.as_deref(),
            Some(&["still applied".to_string()][..]),
        );
    }

    #[test]
    fn slash_command_tags_dto_absent_null_map_and_malformed() {
        // 1. field absent → outer None (leave tags alone)
        let absent: PagerSettingsUpdate = serde_json::from_value(serde_json::json!({
            "tips": ["hello"],
        }))
        .expect("absent slash_command_tags must not fail parse");
        assert_eq!(absent.slash_command_tags, None, "omit must be None");
        assert_eq!(absent.tips.as_deref(), Some(&["hello".to_string()][..]));

        // 2. explicit null → Some(None) (remote cleared)
        let null_v: PagerSettingsUpdate = serde_json::from_value(serde_json::json!({
            "slash_command_tags": null,
        }))
        .expect("null slash_command_tags must parse");
        assert_eq!(
            null_v.slash_command_tags,
            Some(None),
            "explicit null must be Some(None)"
        );

        // 3. valid map → Some(Some(map))
        let map_v: PagerSettingsUpdate = serde_json::from_value(serde_json::json!({
            "slash_command_tags": {"workflows": "new"},
        }))
        .expect("valid slash_command_tags map must parse");
        let tags = map_v
            .slash_command_tags
            .as_ref()
            .and_then(|inner| inner.as_ref())
            .expect("expected Some(Some(map))");
        assert_eq!(tags.get("workflows").map(String::as_str), Some("new"));
        assert_eq!(tags.len(), 1);

        // 4. malformed must NOT fail the whole struct; sibling fields still apply
        let bad: PagerSettingsUpdate = serde_json::from_value(serde_json::json!({
            "slash_command_tags": ["oops"],
            "tips": ["still-applied"],
            "permission_mode": "always-approve",
        }))
        .expect("malformed slash_command_tags must not fail PagerSettingsUpdate parse");
        assert_eq!(
            bad.slash_command_tags, None,
            "malformed tags treated as absent"
        );
        assert_eq!(
            bad.tips.as_deref(),
            Some(&["still-applied".to_string()][..]),
            "sibling tips must still parse"
        );
        assert_eq!(
            bad.permission_mode,
            Some(Some("always-approve".into())),
            "sibling permission_mode must still parse"
        );
    }
}
