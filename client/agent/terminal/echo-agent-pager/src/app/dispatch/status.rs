//! Session status, sharing, privacy, usage, and info dispatchers.

use agent_client_protocol as acp;

use super::ctx::get_active_agent;
use super::queue::push_and_page_flip;
use super::settings::ui::refresh_open_settings_modals;
use crate::app::actions::Effect;
use crate::app::agent::AgentId;
use crate::app::agent_view::AgentView;
use crate::app::app_view::{ActiveView, AppView};
use crate::notifications::{NotificationEvent, NotificationEventKind};
use crate::scrollback::block::RenderBlock;

/// Temporary kill switch: client share links are disabled.
pub(super) fn dispatch_share_session(app: &mut AppView) -> Vec<Effect> {
    app.show_toast("Session sharing is temporarily disabled");
    vec![]
}

/// Monotonic generation for usage-modal fetches. Each open stamps the modal
/// and its effects with a fresh value so a reply from a previous open (same
/// session, modal closed and reopened) can't overwrite newer results. `0` is
/// reserved for the minimal-mode paths, which never touch the modal.
static USAGE_FETCH_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_usage_fetch_nonce() -> u64 {
    USAGE_FETCH_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// The agent's open usage modal state, if any.
pub(super) fn usage_modal_state_mut(
    agent: &mut AgentView,
) -> Option<&mut crate::views::usage_modal::UsageInfoModalState> {
    match agent.active_modal.as_mut() {
        Some(crate::views::modal::ActiveModal::UsageInfo { state }) => Some(state),
        _ => None,
    }
}

/// Open (or re-tab) the usage/session-info modal and fire the fetch effects
/// that populate it. Full-TUI only — minimal mode keeps scrollback blocks.
pub(super) fn open_usage_info_modal(
    app: &mut AppView,
    tab: crate::views::usage_modal::UsageInfoTab,
) -> Vec<Effect> {
    use crate::views::modal::ActiveModal;
    use crate::views::usage_modal::{UsageInfoContext, UsageInfoModalState};

    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let usage_visible = app.usage_visible;
    let redirect_url = app.usage_billing_redirect_url.clone();
    let tier = app.subscription_tier.clone();
    let show_resolved_model = app.show_resolved_model;
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    let session_id = agent.session.session_id.clone();

    if let Some(state) = usage_modal_state_mut(agent) {
        state.set_tab(tab);
        return vec![];
    }

    let billing_reachable = usage_visible && !agent.chat_kind && redirect_url.is_none();
    let nonce = next_usage_fetch_nonce();
    let mut state = UsageInfoModalState::new(
        tab,
        UsageInfoContext {
            session_id: session_id.as_ref().map(|s| s.0.to_string()),
            usage_visible,
            chat_kind: agent.chat_kind,
            billing_redirect_url: redirect_url,
            subscription_tier: tier,
        },
    );
    state.fetch_nonce = nonce;

    let mut effects = Vec::new();
    if let Some(session_id) = session_id {
        effects.push(Effect::ShowContextInfo {
            agent_id: id,
            session_id: session_id.clone(),
            nonce,
        });
        effects.push(Effect::ShowSessionInfo {
            agent_id: id,
            session_id: session_id.clone(),
            show_resolved_model,
            nonce,
        });
        effects.push(Effect::FetchSessionUsage {
            agent_id: id,
            session_id,
            nonce,
        });
    }
    // Silent refresh of the cached billing mirrors the modal renders from.
    if billing_reachable {
        state.billing_loading = true;
        effects.push(Effect::FetchBilling {
            agent_id: id,
            silent: true,
            nonce,
        });
    }
    agent.active_modal = Some(ActiveModal::UsageInfo {
        state: Box::new(state),
    });
    effects
}

/// `/session-info` — open the usage modal on its "Session info" tab, or
/// fetch-and-show in scrollback in minimal mode.
pub(super) fn dispatch_show_session_info(app: &mut AppView) -> Vec<Effect> {
    if !app.screen_mode.is_minimal() {
        return open_usage_info_modal(app, crate::views::usage_modal::UsageInfoTab::SessionInfo);
    }
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    let Some(session_id) = agent.session.session_id.clone() else {
        // No active session — error should have been caught by slash command,
        // but guard here just in case.
        return vec![];
    };

    vec![Effect::ShowSessionInfo {
        agent_id: id,
        session_id,
        show_resolved_model: app.show_resolved_model,
        nonce: Default::default(),
    }]
}

/// State-only mutation for `coding_data_sharing`. SHELL-owned.
pub(super) fn set_coding_data_sharing_inner(app: &mut AppView, opted_in: bool) {
    app.coding_data_retention_opt_out = !opted_in;
}

/// Agent the coding-data ACP write is attributed to. Privacy is app-level,
/// so the id only routes the result back; `AgentId(0)` is the synthetic
/// stand-in for the welcome screen, where the banner is reachable before a
/// session exists.
fn coding_data_sharing_agent_id(app: &AppView) -> AgentId {
    match app.active_view {
        ActiveView::Agent(id) => id,
        _ => app.agents.keys().next().copied().unwrap_or(AgentId(0)),
    }
}

/// Claim the next write generation. Every `SetCodingDataSharing` must take
/// one so its reply can be matched against the newest write.
fn next_coding_data_write_seq(app: &mut AppView) -> u64 {
    app.coding_data_write_seq += 1;
    app.coding_data_write_seq
}

/// Is this reply from the newest write? Writes to this endpoint run
/// concurrently and can land out of order, so an older reply must not touch
/// state: its `rollback_to_opted_in` predates the newer write, and applying
/// it would silently undo whatever the user did since.
fn is_current_coding_data_write(app: &AppView, seq: u64, agent_id: AgentId) -> bool {
    if seq == app.coding_data_write_seq {
        return true;
    }
    tracing::debug!(
        target: "settings",
        key = "coding_data_sharing",
        ?agent_id,
        seq,
        current = app.coding_data_write_seq,
        "dropping superseded coding-data reply",
    );
    false
}

/// Take the parked /feedback trace upload iff it waits on exactly this
/// write generation.
fn take_pending_feedback_trace_upload(
    app: &mut AppView,
    seq: u64,
) -> Option<crate::app::app_view::PendingFeedbackTraceUpload> {
    if app
        .feedback_trace_upload_pending
        .as_ref()
        .is_some_and(|p| p.seq == seq)
    {
        return app.feedback_trace_upload_pending.take();
    }
    None
}

fn log_coding_data_consent_selected(
    source: echo_agent_telemetry::events::CodingDataConsentSource,
    opted_in: bool,
    previous_opted_in: bool,
) {
    use echo_agent_telemetry::events::{CodingDataConsentChoice, CodingDataConsentSelected};
    echo_agent_telemetry::session_ctx::log_event(CodingDataConsentSelected {
        source,
        choice: CodingDataConsentChoice::from_opted_in(opted_in),
        previous_choice: CodingDataConsentChoice::from_opted_in(previous_opted_in),
        changed: opted_in != previous_opted_in,
    });
}

/// What [`set_coding_data_sharing_tracked`] did, so callers sequencing work
/// on the write (e.g. a parked /feedback trace upload) can branch on a typed
/// outcome instead of pattern-matching the effect list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SharingWriteOutcome {
    /// A guard refused the change (ZDR / non-admin team member).
    Refused,
    /// The preference already matched; nothing to write.
    AlreadySet,
    /// A write was dispatched under this `coding_data_write_seq` generation.
    Claimed(u64),
}

/// Set coding-data-sharing preference. SHELL-owned, auth-metadata-backed
/// (persists via ACP ext-request, NOT `~/.echo-agent/config.toml`).
pub(super) fn set_coding_data_sharing(
    app: &mut AppView,
    opted_in: bool,
    source: echo_agent_telemetry::events::CodingDataConsentSource,
) -> Vec<Effect> {
    set_coding_data_sharing_tracked(app, opted_in, source).0
}

pub(super) fn set_coding_data_sharing_tracked(
    app: &mut AppView,
    opted_in: bool,
    source: echo_agent_telemetry::events::CodingDataConsentSource,
) -> (Vec<Effect>, SharingWriteOutcome) {
    // ── Guard 1: Enterprise ZDR ──────────────────────────────────────
    if app.is_zdr {
        app.show_toast("\u{2717} Cannot change: Zero Data Retention enabled");
        return (vec![], SharingWriteOutcome::Refused);
    }
    // ── Guard 2: Non-admin team member ───────────────────────────────
    if app.team_name.is_some() {
        let is_admin = app
            .team_role
            .as_deref()
            .is_some_and(|r| r.eq_ignore_ascii_case("admin"));
        if !is_admin {
            app.show_toast("\u{2717} Data sharing is controlled by your team admin");
            return (vec![], SharingWriteOutcome::Refused);
        }
    }
    let agent_id = coding_data_sharing_agent_id(app);
    let prev = !app.coding_data_retention_opt_out;
    log_coding_data_consent_selected(source, opted_in, prev);

    // Opt-out always acks now. Unchanged opt-in acks only when idle:
    // an inflight write still owns that ack.
    let mut effects = Vec::new();
    if !opted_in || (prev == opted_in && !app.privacy_banner_opt_in_inflight) {
        effects.extend(ack_privacy_banner(app));
    }
    if prev == opted_in {
        return (effects, SharingWriteOutcome::AlreadySet);
    }

    if opted_in {
        app.privacy_banner_opt_in_inflight = true;
    }

    // Optimistic mutation. Success is silent; only the refusals above and
    // the failure handler toast.
    set_coding_data_sharing_inner(app, opted_in);
    refresh_open_settings_modals(app);

    tracing::info!(
        target: "settings",
        key = "coding_data_sharing",
        opted_in,
        "setting changed",
    );

    let seq = next_coding_data_write_seq(app);
    effects.push(Effect::SetCodingDataSharing {
        agent_id,
        opted_in,
        rollback_to_opted_in: prev,
        seq,
    });
    (effects, SharingWriteOutcome::Claimed(seq))
}

/// Scrub an untrusted error string for toast display. Substitutes a
/// generic placeholder when the input exceeds 120 chars or contains
/// control / bidi-override characters (prevents escape-sequence
/// injection and visual spoofing). Full error stays in tracing logs.
pub(super) fn scrub_error_for_toast(error: &str) -> String {
    const MAX_TOAST_ERROR_LEN: usize = 120;
    if error.len() > MAX_TOAST_ERROR_LEN
        || error
            .chars()
            .any(crate::render::line_utils::is_unsafe_display_char)
    {
        "server error (see logs for details)".to_string()
    } else {
        error.to_string()
    }
}

/// `/context` and the context-bar click — open the usage modal on its
/// "Context usage" tab, or fetch-and-show in scrollback in minimal mode.
pub(super) fn dispatch_show_context_info(app: &mut AppView) -> Vec<Effect> {
    if !app.screen_mode.is_minimal() {
        return open_usage_info_modal(app, crate::views::usage_modal::UsageInfoTab::ContextUsage);
    }
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    let Some(session_id) = agent.session.session_id.clone() else {
        return vec![];
    };

    vec![Effect::ShowContextInfo {
        agent_id: id,
        session_id,
        nonce: Default::default(),
    }]
}

/// `/usage` — open the usage modal on its "Usage limit" tab. Minimal mode
/// keeps the scrollback flow: session token/cost, then consumer credits.
pub(super) fn dispatch_show_usage(app: &mut AppView) -> Vec<Effect> {
    if !app.screen_mode.is_minimal() {
        return open_usage_info_modal(app, crate::views::usage_modal::UsageInfoTab::UsageLimit);
    }
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let session_id = {
        let Some(agent) = app.agents.get_mut(&id) else {
            return vec![];
        };
        agent.session.session_id.clone()
    };
    match session_id {
        Some(session_id) => vec![Effect::FetchSessionUsage {
            agent_id: id,
            session_id,
            nonce: Default::default(),
        }],
        None => {
            if let Some(agent) = app.agents.get_mut(&id) {
                push_and_page_flip(
                    &mut agent.scrollback,
                    RenderBlock::system(
                        "Session usage is unavailable until the session starts.".to_string(),
                    ),
                );
            }
            append_consumer_billing_surface(app, id)
        }
    }
}

/// Route a session-usage result (success or failure text) into the open
/// usage modal, or into scrollback in minimal mode. Stale results are dropped.
pub(super) fn handle_session_usage_result(
    app: &mut AppView,
    agent_id: AgentId,
    session_id: &acp::SessionId,
    text: String,
    nonce: u64,
) -> Vec<Effect> {
    if !app.screen_mode.is_minimal() {
        if let Some(agent) = app.agents.get_mut(&agent_id) {
            if agent.session.session_id.as_ref() != Some(session_id) {
                return vec![];
            }
            if let Some(state) = usage_modal_state_mut(agent)
                && state.fetch_nonce == nonce
            {
                state.session_usage_text = Some(text);
            }
        }
        return vec![];
    }
    commit_session_usage_block(app, agent_id, session_id, text)
}

/// Commit a session-usage block if still on `session_id`, then consumer credits.
pub(super) fn commit_session_usage_block(
    app: &mut AppView,
    agent_id: AgentId,
    session_id: &acp::SessionId,
    text: String,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    if agent.session.session_id.as_ref() != Some(session_id) {
        return vec![];
    }
    push_and_page_flip(&mut agent.scrollback, RenderBlock::system(text));
    append_consumer_billing_surface(app, agent_id)
}

/// Consumer credit follow-up for `/usage` (redirect or non-silent billing fetch).
pub(super) fn append_consumer_billing_surface(app: &mut AppView, agent_id: AgentId) -> Vec<Effect> {
    if !app.usage_visible {
        return vec![];
    }
    // Remote-settings kill switch (`echo_agent_build_usage_redirect_url`): link out
    // instead of fetching billing from the backend.
    if let Some(url) = app.usage_billing_redirect_url.clone() {
        if let Some(agent) = app.agents.get_mut(&agent_id) {
            agent.scrollback.push_block(RenderBlock::System(
                crate::scrollback::blocks::SystemMessageBlock::new(format!(
                    "Please check your usage on {url}"
                )),
            ));
        }
        return vec![];
    }
    if !app.agents.contains_key(&agent_id) {
        return vec![];
    }
    // Non-silent: the effect also pulls the auto top-up rule so the summary
    // renders usage, prepaid credits, and auto top-up together.
    vec![Effect::FetchBilling {
        agent_id,
        silent: false,
        nonce: Default::default(),
    }]
}

/// `/usage manage` — open consumer billing. No-op when the surface is hidden.
pub(super) fn dispatch_manage_billing(app: &mut AppView) -> Vec<Effect> {
    if !app.usage_visible {
        return vec![];
    }
    super::router::dispatch(
        crate::app::actions::Action::OpenUrl(
            "https://cloud.echo-agent.invalid/?_s=usage".to_string(),
        ),
        app,
    )
}

/// Commit a one-line "update available" notice into the active agent's
/// scrollback. Minimal mode has no welcome screen (the full TUI's update
/// surface), so the background update check's result is shown here instead
/// No-op when there is no active agent.
pub(crate) fn commit_minimal_update_notice(app: &mut AppView, latest_version: &str) {
    if let ActiveView::Agent(id) = app.active_view
        && let Some(agent) = app.agents.get_mut(&id)
    {
        agent.scrollback.push_block(RenderBlock::system(format!(
            "Update available: v{latest_version}. Restart to apply."
        )));
    }
}

/// `/queue` — commit a read-only list of the queued prompts as a system block.
/// The text is built by [`crate::app::status_blocks::queue_block_text`]; this
/// just resolves the active agent and pushes it. Works in every render mode; the
/// primary inspection surface in minimal, which has no interactive `QueuePane`.
pub(super) fn dispatch_show_queue(app: &mut AppView) -> Vec<Effect> {
    if let ActiveView::Agent(id) = app.active_view
        && let Some(agent) = app.agents.get_mut(&id)
    {
        let text = crate::app::status_blocks::queue_block_text(agent);
        agent.scrollback.push_block(RenderBlock::system(text));
    }
    vec![]
}

/// `/tasks` — commit a read-only list of background tasks, subagents, and
/// scheduled (`/loop`) tasks as a system block. The text is built by
/// [`crate::app::status_blocks::tasks_block_text`]; this just resolves the
/// active agent and pushes it. Works in every render mode; the primary snapshot
/// surface in minimal, which has no interactive `TasksPane`.
pub(super) fn dispatch_show_tasks(app: &mut AppView) -> Vec<Effect> {
    if let ActiveView::Agent(id) = app.active_view
        && let Some(agent) = app.agents.get_mut(&id)
    {
        let text = crate::app::status_blocks::tasks_block_text(agent);
        agent.scrollback.push_block(RenderBlock::system(text));
    }
    vec![]
}

/// Open the hidden `/gboom` easter egg as a modal over the active agent
/// view. Requires a graphics-capable terminal (kitty protocol or iTerm2);
/// otherwise a toast explains why nothing happened. On session-less
/// surfaces (dashboard, welcome) this is a silent no-op.
///
/// Targets the top-level agent view (where the prompt lives), not a
/// focused subagent view: the modal's tick/draw plumbing runs on the
/// top-level view, mirroring the video viewer.
pub(super) fn dispatch_open_gboom(app: &mut AppView) -> Vec<Effect> {
    use crate::terminal::image::{GraphicsProtocol, detect_graphics_protocol};
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    if detect_graphics_protocol() == GraphicsProtocol::None {
        agent.show_toast(
            "No demons here: GBOOM needs a graphics-capable terminal \
             (kitty, Ghostty, WezTerm, iTerm2)",
        );
        return vec![];
    }
    // Close other media modals: they share the kitty placement id. Drop the
    // image viewer's in-flight loader too (its close path clears both —
    // a leaked rx would mis-feed the next image viewer's poll loop).
    agent.image_viewer = None;
    agent.image_load_rx = None;
    agent.video_viewer = None;
    agent.gboom = Some(crate::gboom::GboomState::new());
    vec![]
}

/// Emit a `SessionReady` notification for the given agent.
///
/// Takes `&NotificationService` separately from `&AgentView` to avoid
/// borrow-checker conflicts when `agent` is borrowed from `app.agents`.
pub(super) fn notify_session_ready(
    notification_service: &crate::notifications::NotificationService,
    agent: &AgentView,
) {
    notification_service.notify(NotificationEvent {
        kind: NotificationEventKind::SessionReady,
        title: "EchoAgent".into(),
        body: NotificationEventKind::SessionReady.as_str().into(),
        session_id: agent.session.session_id.as_ref().map(|s| s.0.to_string()),
    });
}

// TaskResult handlers.

pub(super) fn handle_coding_data_sharing_updated(
    app: &mut AppView,
    agent_id: AgentId,
    opted_in: bool,
    seq: u64,
) -> Vec<Effect> {
    // Taken even for superseded replies: uploading on stale consent would be
    // wrong.
    let parked_upload = take_pending_feedback_trace_upload(app, seq);
    if !is_current_coding_data_write(app, seq, agent_id) {
        // A dropped parked upload persisted nothing, so undo the in-session
        // latch (same as the failure path): "nothing happened" must always
        // leave the card offerable again.
        if parked_upload.is_some() {
            app.feedback_trace_choice_latched = false;
        }
        return vec![];
    }
    // Re-anchor mirror to server-confirmed value (defense-in-depth against
    // server reshaping the boolean). `agent_id` discarded — privacy is
    // app-level, not per-agent.
    set_coding_data_sharing_inner(app, opted_in);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "coding_data_sharing",
        ?agent_id,
        opted_in,
        "ACP update confirmed; mirror re-anchored",
    );
    let mut effects = vec![];
    // Defer opt-in ack until this write lands; a failed write must not dismiss.
    if app.privacy_banner_opt_in_inflight {
        app.privacy_banner_opt_in_inflight = false;
        if opted_in {
            effects.extend(ack_privacy_banner(app));
        }
    }
    // The opt-in landed: release the parked upload and the deferred consent
    // persist.
    if let Some(pending) = parked_upload {
        if opted_in {
            effects.push(Effect::UploadFeedbackTrace {
                agent_id: pending.agent_id,
                session_id: pending.session_id,
            });
            effects.push(super::notes::persist_trace_upload_consent());
        } else {
            // The write round-tripped but the server-confirmed state is
            // still opted out: nothing uploaded or persisted, so undo the
            // latch like the failure path does.
            app.feedback_trace_choice_latched = false;
        }
    }
    effects
}

pub(super) fn handle_coding_data_sharing_failed(
    app: &mut AppView,
    agent_id: AgentId,
    error: String,
    rollback_to_opted_in: bool,
    seq: u64,
) -> Vec<Effect> {
    // The opt-in never landed: drop the parked upload (the storage proxy
    // would still refuse it) and undo the in-session latch — nothing was
    // persisted, so a later /feedback may offer the card again.
    if take_pending_feedback_trace_upload(app, seq).is_some() {
        app.feedback_trace_choice_latched = false;
    }
    // A superseded failure must not revert: `rollback_to_opted_in` predates
    // the newer write, so applying it would undo a change the user made
    // after this one was sent. It must not toast either — nothing the user
    // is looking at failed.
    if !is_current_coding_data_write(app, seq, agent_id) {
        return vec![];
    }
    // Revert optimistic mutation: inner → refresh → toast. `agent_id`
    // discarded — privacy is global.
    set_coding_data_sharing_inner(app, rollback_to_opted_in);
    refresh_open_settings_modals(app);
    // Scrub long/unsafe error strings before toasting.
    let scrubbed = scrub_error_for_toast(&error);
    app.show_toast(&format!(
        "\u{2717} Couldn't update coding data sharing: {scrubbed}"
    ));
    tracing::warn!(
        target: "settings",
        key = "coding_data_sharing",
        ?agent_id,
        rollback_to_opted_in,
        %error,
        "ACP update failed; reverted optimistic mutation",
    );
    // Opt-in failure: no ack; clear inflight so the banner stays.
    app.privacy_banner_opt_in_inflight = false;
    vec![]
}

/// Stamp `[privacy].privacy_banner_acked` (in-memory + disk).
/// No-op when the notice is not rolled out: a Settings pick must not
/// hide a notice the user has not been shown.
pub(in crate::app::dispatch) fn ack_privacy_banner(app: &mut AppView) -> Vec<Effect> {
    if !app.privacy_notice_rollout {
        return vec![];
    }
    let acked_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    app.privacy_banner_acked = Some(acked_at.clone());
    vec![Effect::PersistPrivacyBannerAcked { acked_at }]
}

/// `[Opt in]`: opt in via the settings path; ack only after ACP success, so
/// a failed round trip leaves the banner up instead of recording a change
/// that did not happen.
pub(in crate::app::dispatch) fn dispatch_privacy_banner_opt_in(app: &mut AppView) -> Vec<Effect> {
    if app.privacy_banner_opt_in_inflight || !app.privacy_banner_should_show() {
        return vec![];
    }
    set_coding_data_sharing(
        app,
        true,
        echo_agent_telemetry::events::CodingDataConsentSource::PrivacyBanner,
    )
}

/// `[Opt out]`: ack now — waiting on ACP would re-ask a decline.
pub(in crate::app::dispatch) fn dispatch_privacy_banner_opt_out(app: &mut AppView) -> Vec<Effect> {
    if app.privacy_banner_opt_in_inflight || !app.privacy_banner_should_show() {
        return vec![];
    }
    set_coding_data_sharing(
        app,
        false,
        echo_agent_telemetry::events::CodingDataConsentSource::PrivacyBanner,
    )
}

pub(super) fn handle_context_info_complete(
    app: &mut AppView,
    agent_id: AgentId,
    session_id: &acp::SessionId,
    info: Box<echo_agent_runtime::session::SessionInfoResponse>,
    nonce: u64,
) -> Vec<Effect> {
    let minimal = app.screen_mode.is_minimal();
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        if agent.session.session_id.as_ref() != Some(session_id) {
            return vec![];
        }
        // A reply from a previous modal open must not touch anything — not
        // even the agent's context mirrors, which a fresher reply already set.
        if let Some(state) = usage_modal_state_mut(agent)
            && state.fetch_nonce != nonce
        {
            return vec![];
        }
        let model = info.data.model.as_deref().unwrap_or("unknown").to_string();
        let snapshot = info.data.context;
        agent.apply_full_context_info(snapshot.clone());
        if let Some(state) = usage_modal_state_mut(agent) {
            state.context = Some(crate::scrollback::blocks::ContextInfoBlock::new(
                snapshot, model,
            ));
            state.context_error = None;
        } else if minimal {
            push_and_page_flip(
                &mut agent.scrollback,
                crate::scrollback::block::RenderBlock::context_info(snapshot, model),
            );
        }
        // Full mode with the modal closed: result arrived after dismissal — drop.
    }
    vec![]
}

// Action handlers.

pub(super) fn dispatch_copy_session_id(app: &mut AppView, index: usize) -> Vec<Effect> {
    use crate::views::modal::ActiveModal;
    // Try agent modal first, then fall back to app fields (welcome screen).
    let id = get_active_agent(app)
        .and_then(|agent| {
            if let Some(ActiveModal::SessionPicker {
                entries: Some(ref e),
                ..
            }) = agent.active_modal
            {
                e.get(index).map(|entry| entry.id.clone())
            } else {
                None
            }
        })
        .or_else(|| {
            app.session_picker_entries
                .as_ref()
                .and_then(|s| s.get(index))
                .map(|e| e.id.clone())
        });
    if let Some(id) = id {
        let delivery = crate::clipboard::copy_text_or_file(&id);
        app.show_toast(delivery.toast_message().as_ref());
    }
    vec![]
}

/// Open the onboarding tutorial overlay (top-level modal — works over both
/// the welcome screen and an agent session). Toggles: dispatching while
/// open closes instead of stacking.
pub(super) fn dispatch_open_tutorial(app: &mut AppView) -> Vec<Effect> {
    // Minimal mode has no modal host: the overlay would render nothing
    // while the app-level intercept swallowed all input.
    if app.screen_mode.is_minimal() {
        return vec![];
    }
    if app.tutorial.is_some() {
        app.tutorial = None;
        return vec![];
    }
    app.tutorial = Some(crate::views::tutorial::TutorialState::new());
    vec![]
}

pub(super) fn dispatch_show_release_notes(
    app: &mut AppView,
    title: String,
    content: String,
) -> Vec<Effect> {
    match app.active_view {
        ActiveView::Agent(id) => {
            if let Some(agent) = app.agents.get_mut(&id) {
                agent.active_modal = Some(crate::views::modal::ActiveModal::DocViewer {
                    title,
                    content,
                    scroll: 0,
                    window: crate::views::modal_window::ModalWindowState::new(),
                    cached_lines: None,
                    previous_palette: None,
                    standalone: true,
                });
            }
        }
        ActiveView::Welcome => {
            app.welcome_doc_viewer = Some(crate::views::modal::ActiveModal::DocViewer {
                title,
                content,
                scroll: 0,
                window: crate::views::modal_window::ModalWindowState::new(),
                cached_lines: None,
                previous_palette: None,
                standalone: true,
            });
        }
        _ => {}
    }
    vec![]
}
