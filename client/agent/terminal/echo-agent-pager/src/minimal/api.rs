//! Read/render surface consumed by the `echo-agent-pager-minimal` crate.
//!
//! **If you don't work on the minimal (scrollback-native) render mode, you can
//! ignore this file.** It is the *single* seam through which `minimal` reaches
//! into this crate's view model. Its whole reason to exist is to keep every
//! other file's internals `pub(crate)`: minimal lives in a sibling crate, so
//! anything it touches would otherwise have to be widened to `pub` and scattered
//! across the core structs (`AgentView`, the `views::*` widgets, …). Instead we
//! keep those `pub(crate)` and expose exactly what minimal needs as thin `pub`
//! accessors/wrappers *here*.
//!
//! Note: this is the *minimal → pager* direction (minimal reading the pager).
//! The reverse direction (pager dispatching into minimal's renderer) is the
//! fn-pointer seam in [`crate::minimal_hook`], installed by the composition-root
//! binary.
//!
//! Conventions:
//! - Getters take `&AgentView` / `&PromptWidget` and return `Option<&T>` or a
//!   `Copy` value. Mutating access is a `*_mut` accessor or an explicit setter,
//!   added only where minimal actually mutates.
//! - `pub use` cannot re-export a `pub(crate)` item at wider visibility (E0365),
//!   so free helpers are re-exposed as thin `pub fn` wrappers, not re-exports.
//! - Purely-internal DTOs (`DropdownChrome`, `McpServersPickerRows`) are never
//!   named across the crate boundary — the wrappers return their extracted data
//!   (a `Rect`, a tuple of `Vec`s) so those types stay `pub(crate)`.

use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::acp::tracker::TurnActivity;
// Only the test-only setters below reference `AgentSession`.
#[cfg(any(test, feature = "test-support"))]
use crate::app::agent::AgentSession;
use crate::app::agent_view::{AgentView, McpInitProgress};
use crate::app::app_view::{ActiveView, AppView, SessionPickerEntry};
use crate::appearance::LayoutConfig;
use crate::scrollback::entry::{EntryId, ScrollbackEntry};
use crate::scrollback::state::ScrollbackState;
use crate::theme::Theme;
use crate::views::extensions_modal::{ExtensionsModalState, StatusFilter};
use crate::views::mcps_modal::{McpServerDisplayStatus, McpServerInfo};
use crate::views::modal::CancelTurnViewState;
use crate::views::picker::{PickerEntry, PickerField, PickerState};
use crate::views::plan_approval_view::PlanApprovalViewState;
use crate::views::prompt_widget::PromptWidget;
use crate::views::question_view::QuestionViewState;
use crate::views::rewind::RewindState;
use crate::views::session_picker::{SessionEntryData, SourceFilter};
use crate::views::suggestion_controller::SuggestionController;

/// The shared renderer's minimum `/btw` panel dimensions.
pub const MINIMAL_BTW_MIN_WIDTH: u16 = 12;
pub const MINIMAL_BTW_MIN_HEIGHT: u16 = 3;

/// Whether minimal can paint and expose input geometry for this panel size.
pub fn minimal_btw_size_is_paintable(width: u16, height: u16) -> bool {
    width >= MINIMAL_BTW_MIN_WIDTH && height >= MINIMAL_BTW_MIN_HEIGHT
}

/// Whether cached minimal input geometry represents a painted panel.
pub fn minimal_btw_geometry_is_paintable(area: Rect) -> bool {
    minimal_btw_size_is_paintable(area.width, area.height)
}

/// Clamp desired `/btw` rows to the available minimal live-region rows.
pub fn minimal_btw_visible_height(desired: u16, width: u16, available: u16) -> u16 {
    if desired == 0 || !minimal_btw_size_is_paintable(width, available) {
        0
    } else {
        desired.min(available)
    }
}

/// Typed result of the minimal-only `/btw` pre-router.
#[derive(Debug)]
pub enum MinimalBtwInput {
    /// Minimal consumed the event; the shared/fullscreen router must not run.
    Handled(Box<crate::app::app_view::InputOutcome>),
    /// Another minimal surface owns the event; delegate to its shared handler.
    Occluded,
    /// The plain live surface is active but `/btw` declined the event.
    Delegate,
}

/// Per-agent ownership of a minimal `/btw` panel and its in-flight response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MinimalBtwLifecycle {
    Active {
        request_id: Option<uuid::Uuid>,
        revision: uuid::Uuid,
    },
    Suspended {
        revision: uuid::Uuid,
    },
}

/// Complete minimal lifecycle moved aside while a painted modal handles input.
pub(crate) struct SuspendedMinimalBtwLifecycle {
    state: crate::views::btw_overlay::BtwOverlayState,
    request_id: Option<uuid::Uuid>,
    revision: uuid::Uuid,
    focused: bool,
}

// ── Consolidated minimal-mode state (AppView::minimal_state) ─────────────────
//
// Minimal's private per-session state, consolidated into a single field on the
// central `AppView` instead of several loose `pub` fields. Default-empty and
// inert outside `--minimal`.

/// In-progress incremental `/transcript` build (minimal mode).
///
/// The full-fidelity ANSI transcript is a layout + syntax-highlight pass over
/// the whole session; building it in one shot froze the event loop, and the
/// block model is `!Send` (syntect's resumable highlighter state lives inside
/// markdown blocks) so it cannot move to a worker. Instead the minimal draw
/// loop renders a **time-budgeted slice per frame**
/// (`echo-agent-pager-minimal::full_view::pump_transcript`) — the same
/// amortization the reference scrollback TUIs use for transcript-scale work —
/// and arms `pending_pager_path` when done.
pub struct TranscriptBuild {
    /// The agent whose conversation this build snapshots. The pump resolves
    /// entries against THIS agent — never the active view: `EntryId`s are
    /// per-`ScrollbackState` counters (every state starts at 1), so resolving
    /// the snapshot against whichever agent happens to be active after a
    /// session switch would silently stitch the transcript from another
    /// session's blocks. Keying by owner also keeps the build alive (and the
    /// pager opening) when the user tabs away mid-build.
    pub agent: crate::app::agent::AgentId,
    /// Snapshot of the entry IDs to render, in conversation order. IDs are
    /// re-resolved per slice, so entries removed mid-build (rewind / clear)
    /// are skipped instead of skewing positions.
    pub ids: Vec<EntryId>,
    /// Next index into `ids` to render.
    pub next: usize,
    /// Accumulated ANSI output.
    pub out: String,
}

/// Minimal-mode-only state held on [`AppView::minimal_state`].
#[derive(Default)]
pub(crate) struct MinimalState {
    /// Pin the todo panel visible (minimal reuses Ctrl+T for this).
    pub(crate) show_todos: bool,
    /// A welcome card is queued to commit into native scrollback next draw.
    pub(crate) welcome_pending: bool,
    /// Entry IDs queued by Ctrl+E / `/expand` to re-print fully expanded (K10).
    pub(crate) pending_expand: Vec<EntryId>,
    /// In-progress `/transcript` build, pumped one slice per frame.
    pub(crate) transcript: Option<TranscriptBuild>,
    /// `tool_call_id` of the plan already emitted into native scrollback. Minimal
    /// prints the whole plan as a normal committed conversation block (rather than
    /// rendering it under the prompt), so this de-dupes the per-frame push — and,
    /// because each revision is a fresh ExitPlanMode with a new id, still commits
    /// every revised plan as its own block.
    pub(crate) committed_plan_tool_call_id: Option<String>,
}

/// `AppView::minimal_state.show_todos`.
pub fn minimal_show_todos(app: &AppView) -> bool {
    app.minimal_state.show_todos
}

/// `AppView::minimal_state.welcome_pending`.
pub fn minimal_welcome_pending(app: &AppView) -> bool {
    app.minimal_state.welcome_pending
}

/// `AppView::minimal_state.welcome_pending` (write).
pub fn set_minimal_welcome_pending(app: &mut AppView, on: bool) {
    app.minimal_state.welcome_pending = on;
}

/// `AppView::minimal_state.pending_expand` (read).
pub fn minimal_pending_expand(app: &AppView) -> &[EntryId] {
    &app.minimal_state.pending_expand
}

/// Drain `AppView::minimal_state.pending_expand` (Ctrl+E / `/expand` queue).
pub fn take_minimal_pending_expand(app: &mut AppView) -> Vec<EntryId> {
    std::mem::take(&mut app.minimal_state.pending_expand)
}

/// Put drained expand IDs back at the FRONT of the queue (a terminal write
/// failed mid-drain): they retry next frame, ahead of any newly queued Ctrl+E.
pub fn requeue_minimal_pending_expand(app: &mut AppView, mut ids: Vec<EntryId>) {
    ids.extend(std::mem::take(&mut app.minimal_state.pending_expand));
    app.minimal_state.pending_expand = ids;
}

// ── Incremental /transcript build ────────────────────────────────────────────

/// Arm the incremental minimal `/transcript` build from the active agent's
/// conversation. No-op when a build is already running (the in-flight one
/// wins) — and pushes the "nothing to show" system block when the conversation
/// is empty. The minimal draw loop pumps the build a slice per frame and arms
/// `pending_pager_path` on completion.
pub fn request_minimal_transcript(app: &mut AppView) {
    if app.minimal_state.transcript.is_some() {
        return;
    }
    let ActiveView::Agent(id) = &app.active_view else {
        return;
    };
    let id = *id;
    let Some(agent) = app.agents.get_mut(&id) else {
        return;
    };
    let sb = &agent.scrollback;
    let ids: Vec<EntryId> = (0..sb.len())
        .filter_map(|i| sb.entry(i).map(|e| e.id))
        .collect();
    if ids.is_empty() {
        agent
            .scrollback
            .push_block(crate::scrollback::block::RenderBlock::system(
                "No conversation transcript to view yet",
            ));
        return;
    }
    app.minimal_state.transcript = Some(TranscriptBuild {
        agent: id,
        ids,
        next: 0,
        out: String::new(),
    });
}

/// Take the in-progress transcript build out of the state for one pump slice
/// (the pump needs `&AgentView` and the build simultaneously; taking avoids a
/// double `&mut AppView` borrow). Put it back via [`set_minimal_transcript`]
/// unless the slice finished it.
pub fn take_minimal_transcript(app: &mut AppView) -> Option<TranscriptBuild> {
    app.minimal_state.transcript.take()
}

/// Store the (still unfinished) transcript build back after a pump slice.
pub fn set_minimal_transcript(app: &mut AppView, build: Option<TranscriptBuild>) {
    app.minimal_state.transcript = build;
}

/// Progress of the in-flight transcript build (`rendered`, `total`), for the
/// status row. `None` when no build is running.
pub fn minimal_transcript_progress(app: &AppView) -> Option<(usize, usize)> {
    app.minimal_state
        .transcript
        .as_ref()
        .map(|b| (b.next, b.ids.len()))
}

/// `AppView::minimal_state.committed_plan_tool_call_id` (read).
pub fn minimal_committed_plan_id(app: &AppView) -> Option<&str> {
    app.minimal_state.committed_plan_tool_call_id.as_deref()
}

/// [`AppView::status_line_frame`].
pub fn status_line_frame(app: &AppView) -> crate::views::status_line::StatusLineFrame {
    app.status_line_frame()
}

/// [`crate::views::status_line::inner_width`].
pub fn status_line_inner_width(width: u16, padding: u16) -> Option<u16> {
    crate::views::status_line::inner_width(width, padding)
}

/// Whether minimal's Ctrl+O remap opens the full-transcript pager *right now*.
///
/// Minimal remaps Ctrl+O to `Action::OpenTranscriptPager` except when:
///
/// - Ctrl+O is bound to interject (Apple Terminal: the kitty keyboard protocol is
///   unavailable, so Ctrl+Enter doesn't arrive and Ctrl+I aliases to Tab, leaving
///   Ctrl+O as the only interject chord) AND an interject would actually consume
///   the press:
///   - editing a queued row (the interject key saves / interjects the edit), or
///   - a turn is running with a non-empty composer, or
///   - a turn is running with an empty composer **and** a visible queued
///     follow-up (prompt-path force-send of the top queue row; same as full TUI)
/// - a free-tier pinned upgrade CTA is live (`pinned_upgrade_cta_live`), so
///   Ctrl+O reaches ToggleYolo (open CTA) instead of the transcript
///
/// Otherwise the remap keeps the key for the transcript. When the remap yields,
/// `minimal_key_intercept` routes to the prompt path (interject, or ToggleYolo
/// on Apple Terminal when interject has nothing to send). The info-row hint
/// re-evaluates this every frame so the advertised key ("ctrl+o transcript" vs
/// `/transcript`) always matches what the press would do.
pub fn minimal_ctrl_o_opens_transcript(app: &AppView) -> bool {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
    let ActiveView::Agent(id) = &app.active_view else {
        return true;
    };
    let Some(agent) = app.agents.get(id) else {
        return true;
    };
    if !app
        .registry
        .matches_id(crate::actions::ActionId::InterjectPrompt, &ctrl_o)
    {
        // Not the interject chord: transcript unless a pinned upgrade CTA owns it.
        return !agent.pinned_upgrade_cta_live;
    }
    // Editing a queued row: the interject key saves (idle) or interjects
    // (running) the edited text — never steal it mid-edit.
    if matches!(
        agent.prompt_mode,
        crate::app::agent_view::PromptMode::EditingQueued { .. }
    ) {
        return false;
    }
    // Matches prompt-path send-now: non-empty composer text *or* a visible
    // queued follow-up (empty-composer force-send of the top row). Exclude the
    // in-flight shared-queue entry when it is the running turn (same rule as
    // `AgentView::visible_queue_is_empty`).
    let running = agent.session.current_prompt_id.as_deref();
    let has_queued_follow_up = !agent.session.pending_prompts.is_empty()
        || agent
            .shared_queue
            .iter()
            .any(|e| Some(e.id.as_str()) != running);
    let has_payload = !agent.prompt.text().trim().is_empty() || has_queued_follow_up;
    if crate::actions::ActionRegistry::interjection_possible(
        agent.session.state.is_turn_running(),
        has_payload,
    ) {
        return false;
    }
    !agent.pinned_upgrade_cta_live
}

/// `AppView::minimal_state.committed_plan_tool_call_id` (write).
pub fn set_minimal_committed_plan_id(app: &mut AppView, id: Option<String>) {
    app.minimal_state.committed_plan_tool_call_id = id;
}

// ── AgentView field accessors ────────────────────────────────────────────────

/// `AgentView::last_activity` (read).
pub fn last_activity(v: &AgentView) -> Option<&TurnActivity> {
    v.last_activity.as_ref()
}

/// `AgentView::last_activity` (write).
pub fn set_last_activity(v: &mut AgentView, val: Option<TurnActivity>) {
    v.last_activity = val;
}

/// `AgentView::extensions_modal`.
pub fn extensions_modal(v: &AgentView) -> Option<&ExtensionsModalState> {
    v.extensions_modal.as_ref()
}

/// `AgentView::extensions_modal` (mutable — minimal reuses the full-TUI modal
/// renderer, which takes `&mut ExtensionsModalState`, and updates render-stored
/// picker row state).
pub fn extensions_modal_mut(v: &mut AgentView) -> Option<&mut ExtensionsModalState> {
    v.extensions_modal.as_mut()
}

/// `AgentView::question_view`.
pub fn question_view(v: &AgentView) -> Option<&QuestionViewState> {
    v.question_view.as_ref()
}

/// `AgentView::question_view` (mutable — minimal clamps the scroll offset).
pub fn question_view_mut(v: &mut AgentView) -> Option<&mut QuestionViewState> {
    v.question_view.as_mut()
}

/// `AgentView::hovered_question_item`.
pub fn hovered_question_item(v: &AgentView) -> Option<usize> {
    v.hovered_question_item
}

/// `AgentView::hovered_permission_item`.
pub fn hovered_permission_item(v: &AgentView) -> Option<usize> {
    v.hovered_permission_item
}

/// `AgentView::plan_mode_active`.
pub fn plan_mode_active(v: &AgentView) -> bool {
    v.plan_mode_active
}

/// `AgentView::plan_mode_pending`.
pub fn plan_mode_pending(v: &AgentView) -> Option<bool> {
    v.plan_mode_pending
}

/// `AgentView::mcp_init_progress`.
pub fn mcp_init_progress(v: &AgentView) -> Option<&McpInitProgress> {
    v.mcp_init_progress.as_ref()
}

/// `AgentView::plan_approval_view`.
pub fn plan_approval_view(v: &AgentView) -> Option<&PlanApprovalViewState> {
    v.plan_approval_view.as_ref()
}

/// Whether the minimal `/btw` panel is the painted input owner.
///
/// This mirrors the shared router's surface cascade: everything that handles
/// input before `/btw`, plus the later prompt-replacing surfaces that minimal
/// paints in place of the panel, takes precedence here. Keeping the complete
/// owner predicate at the minimal facade gives paint and minimal input one
/// canonical answer without changing the fullscreen router.
pub fn minimal_btw_surface_available(v: &AgentView) -> bool {
    v.active_subagent.is_none()
        && v.image_viewer.is_none()
        && v.video_viewer.is_none()
        && v.gboom.is_none()
        && !(v.show_goal_detail && v.goal_state.is_some())
        && v.line_viewer.is_none()
        && v.extensions_modal.is_none()
        && v.persona_detail.is_none()
        && v.agents_modal.is_none()
        && v.block_viewer.is_none()
        && v.active_modal.is_none()
        && v.no_input_overlay_pending()
        && v.rewind_state.is_none()
}

/// Start a correlated minimal `/btw` loading panel on this agent.
pub fn start_minimal_btw(v: &mut AgentView, question: String) -> uuid::Uuid {
    let request_id = uuid::Uuid::new_v4();
    v.minimal_btw_lifecycle = Some(MinimalBtwLifecycle::Active {
        request_id: Some(request_id),
        revision: uuid::Uuid::new_v4(),
    });
    v.btw_state = Some(crate::views::btw_overlay::BtwOverlayState::Loading { question });
    v.btw_focused = false;
    request_id
}

/// Apply a minimal `/btw` response only when it still owns the loading panel.
pub fn finish_minimal_btw(
    v: &mut AgentView,
    request_id: uuid::Uuid,
    result: Result<String, String>,
) -> bool {
    let Some(MinimalBtwLifecycle::Active {
        request_id: Some(active_id),
        ..
    }) = v.minimal_btw_lifecycle
    else {
        return false;
    };
    if active_id != request_id {
        return false;
    }
    let Some(crate::views::btw_overlay::BtwOverlayState::Loading { question }) = v.btw_state.take()
    else {
        return false;
    };
    v.minimal_btw_lifecycle = Some(MinimalBtwLifecycle::Active {
        request_id: None,
        revision: uuid::Uuid::new_v4(),
    });
    match result {
        Ok(response) => {
            v.btw_state = Some(crate::views::btw_overlay::BtwOverlayState::done(
                question, response,
            ));
            v.btw_focused = true;
        }
        Err(error) => {
            v.btw_state =
                Some(crate::views::btw_overlay::BtwOverlayState::Error { question, error });
            v.btw_focused = false;
        }
    }
    true
}

/// Invalidate and clear the complete minimal `/btw` lifecycle.
pub fn clear_minimal_btw(v: &mut AgentView) {
    if v.minimal_btw_lifecycle.is_none() {
        return;
    }
    v.minimal_btw_lifecycle = None;
    v.btw_state = None;
    v.btw_focused = false;
    v.last_btw_area = Rect::default();
    v.last_btw_selection_model = Default::default();
    v.hit_btw_close.clear();
    clear_btw_drag_state(v);
}

/// Clear text-drag state only when it belongs to the minimal `/btw` surface.
///
/// Kept in this facade rather than widening the viewer module's private helper:
/// minimal already owns this lifecycle reset and is the only cross-module caller.
fn clear_btw_drag_state(v: &mut AgentView) {
    let is_btw = v
        .pending_text_drag
        .is_some_and(|p| p.anchor.entry_idx == crate::views::btw_overlay::BTW_OVERLAY_ENTRY_IDX)
        || v.drag_selection.as_ref().is_some_and(|d| {
            d.anchor.entry_idx == crate::views::btw_overlay::BTW_OVERLAY_ENTRY_IDX
        });
    if is_btw {
        v.pending_text_drag = None;
        v.drag_selection = None;
        v.drag_autoscroll = None;
        v.last_drag_mouse = None;
    }
}

/// Atomically suspend the complete lifecycle while another surface handles input.
pub(crate) fn suspend_minimal_btw(v: &mut AgentView) -> Option<SuspendedMinimalBtwLifecycle> {
    let MinimalBtwLifecycle::Active {
        request_id,
        revision,
    } = v.minimal_btw_lifecycle?
    else {
        return None;
    };
    let state = v.btw_state.take()?;
    v.minimal_btw_lifecycle = Some(MinimalBtwLifecycle::Suspended { revision });
    let focused = std::mem::replace(&mut v.btw_focused, false);
    Some(SuspendedMinimalBtwLifecycle {
        state,
        request_id,
        revision,
        focused,
    })
}

/// Restore only if delegated handling left the same suspension marker intact.
pub(crate) fn restore_minimal_btw(v: &mut AgentView, suspended: SuspendedMinimalBtwLifecycle) {
    if v.minimal_btw_lifecycle
        != Some(MinimalBtwLifecycle::Suspended {
            revision: suspended.revision,
        })
    {
        return;
    }
    v.btw_state = Some(suspended.state);
    v.btw_focused = suspended.focused;
    v.minimal_btw_lifecycle = Some(MinimalBtwLifecycle::Active {
        request_id: suspended.request_id,
        revision: suspended.revision,
    });
}

/// `AgentView::btw_focused` — whether Up/Down/PgUp/PgDn scroll the `/btw`
/// panel (set when a Done answer arrives; cleared when the user returns to
/// the prompt). Minimal paints the focus ring / ↑↓ hint from this flag.
pub fn btw_focused(v: &AgentView) -> bool {
    v.btw_focused
}

/// `AgentView::cancel_turn_view`.
pub fn cancel_turn_view(v: &AgentView) -> Option<&CancelTurnViewState> {
    v.cancel_turn_view.as_ref()
}

/// `AgentView::cancel_turn_buttons` (mutable — the renderer fills the hit-test
/// rects).
pub fn cancel_turn_buttons_mut(v: &mut AgentView) -> &mut Vec<Rect> {
    &mut v.cancel_turn_buttons
}

/// `AgentView::rewind_state`.
pub fn rewind_state(v: &AgentView) -> Option<&RewindState> {
    v.rewind_state.as_ref()
}

// ── AgentView method wrappers ────────────────────────────────────────────────

/// [`AgentView::resolve_turn_activity`].
pub fn resolve_turn_activity(v: &AgentView) -> Option<TurnActivity> {
    v.resolve_turn_activity()
}

/// [`AgentView::renders_parked`].
pub fn renders_parked(v: &AgentView) -> bool {
    v.renders_parked()
}

/// [`AgentView::watchers`] — idle-surviving background work (running
/// commands / monitors / loops / subagents) for the shared turn-status
/// widget's "… still running" cue.
pub fn watchers(v: &AgentView) -> crate::views::turn_status::Watchers {
    v.watchers()
}

/// [`AgentView::held_queue_count`].
pub fn held_queue_count(v: &AgentView) -> usize {
    v.held_queue_count()
}

/// [`AgentView::held_queue_top_sendable`].
pub fn held_queue_top_sendable(v: &AgentView) -> bool {
    v.held_queue_top_sendable()
}

/// [`AgentView::sync_pending_user_input_marks`].
pub fn sync_pending_user_input_marks(v: &mut AgentView) {
    v.sync_pending_user_input_marks();
}

/// Scrollback entry id of the tool row for `tool_call_id`, while the tracker
/// still has that tool pending. `None` once it has been reaped, or if it never
/// reached scrollback.
pub fn pending_tool_entry_id(v: &AgentView, tool_call_id: &str) -> Option<EntryId> {
    v.session.tracker.pending_tool_entry_id(tool_call_id)
}

/// [`AgentView::draw_active_modal`] — minimal reuses the full-TUI modal renderer.
pub fn draw_active_modal(
    v: &mut AgentView,
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    compact: bool,
) {
    v.draw_active_modal(area, buf, theme, compact);
}

/// [`AgentView::drain_blocked`].
pub fn drain_blocked(v: &AgentView) -> bool {
    v.drain_blocked()
}

// ── PromptWidget accessors ───────────────────────────────────────────────────

/// `PromptWidget::suggestions`.
pub fn prompt_suggestions(pw: &PromptWidget) -> &SuggestionController {
    &pw.suggestions
}

// ── Dropdown chrome ──────────────────────────────────────────────────────────

/// Lay out the inline dropdown chrome and return the item area rect
/// (`DropdownChrome::items`), or `None` when it doesn't fit. Wraps
/// [`crate::app::agent_view::render_dropdown_chrome`]; the `DropdownChrome` DTO
/// itself stays crate-internal.
#[allow(clippy::too_many_arguments)]
pub fn dropdown_chrome_items(
    buf: &mut Buffer,
    item_count: usize,
    item_rows: u16,
    inline_prompt_area: Option<Rect>,
    layout_prompt: Rect,
    area: Rect,
    layout_cfg: &LayoutConfig,
    compact: bool,
    below: bool,
    theme: &Theme,
) -> Option<Rect> {
    crate::app::agent_view::render_dropdown_chrome(
        buf,
        item_count,
        item_rows,
        inline_prompt_area,
        layout_prompt,
        area,
        layout_cfg,
        compact,
        below,
        theme,
    )
    .map(|chrome| chrome.items)
}

// ── MCP picker rows ──────────────────────────────────────────────────────────

/// Build the MCP-servers picker rows, returning `(labels, group_keys,
/// data_indices)`. Wraps [`crate::views::extensions_modal::build_mcp_servers_picker_rows`];
/// the `McpServersPickerRows` DTO stays crate-internal.
pub fn build_mcp_picker_rows(
    servers: &[McpServerInfo],
    query: &str,
    filter: StatusFilter,
    collapsed_sections: &HashSet<String>,
    tools_expanded: &HashSet<usize>,
) -> (Vec<String>, Vec<Option<String>>, Vec<Option<usize>>) {
    let rows = crate::views::extensions_modal::build_mcp_servers_picker_rows(
        servers,
        query,
        filter,
        collapsed_sections,
        tools_expanded,
    );
    (rows.labels, rows.group_keys, rows.data_indices)
}

/// [`crate::views::extensions_modal::mcp_section_children_hidden`].
pub fn mcp_section_children_hidden(
    collapsed_sections: &HashSet<String>,
    section_key: &str,
    searching: bool,
) -> bool {
    crate::views::extensions_modal::mcp_section_children_hidden(
        collapsed_sections,
        section_key,
        searching,
    )
}

/// [`McpServerDisplayStatus::theme_color`].
pub fn mcp_status_theme_color(status: &McpServerDisplayStatus, theme: &Theme) -> Color {
    status.theme_color(theme)
}

/// [`McpServerDisplayStatus::label`].
pub fn mcp_status_label(status: &McpServerDisplayStatus) -> &'static str {
    status.label()
}

// ── Session picker builders ──────────────────────────────────────────────────

/// Render a search bar from a [`PickerState`] using its grapheme-safe viewport.
pub fn render_picker_search_bar(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    state: &PickerState,
    show_hint: bool,
    bg: Option<Color>,
) {
    crate::views::picker::render_picker_search_bar(
        buf,
        area.x,
        area.y,
        area.width,
        theme,
        state,
        state.search_active,
        show_hint,
        bg,
    );
}

/// [`crate::views::session_picker::repo_name_from_cwd`].
pub fn repo_name_from_cwd(cwd: &str) -> String {
    crate::views::session_picker::repo_name_from_cwd(cwd)
}

/// [`crate::views::session_picker::filter_session_entries`].
pub fn filter_session_entries(
    entries: Option<&[SessionPickerEntry]>,
    query: &str,
    source_filter: SourceFilter,
) -> Vec<usize> {
    crate::views::session_picker::filter_session_entries(entries, query, source_filter)
}

/// [`crate::views::session_picker::build_session_entry_data`].
pub fn build_session_entry_data(
    entries_data: &[SessionPickerEntry],
    filtered_indices: &[usize],
    state: &PickerState,
    content_width: u16,
) -> Vec<SessionEntryData> {
    crate::views::session_picker::build_session_entry_data(
        entries_data,
        filtered_indices,
        state,
        content_width,
    )
}

/// [`crate::views::session_picker::hidden_external_hint`].
pub fn hidden_external_hint(
    entries: Option<&[SessionPickerEntry]>,
    source_filter: SourceFilter,
) -> Option<String> {
    crate::views::session_picker::hidden_external_hint(entries, source_filter)
}

/// [`crate::views::session_picker::build_grouped_picker_entries`].
pub fn build_grouped_picker_entries<'a>(
    entries_data: &'a [SessionPickerEntry],
    filtered_indices: &[usize],
    built: &'a [SessionEntryData],
    fields_vecs: &'a [Vec<PickerField<'a>>],
    state: &PickerState,
    current_repo: Option<&str>,
) -> (Vec<PickerEntry<'a>>, Vec<bool>) {
    crate::views::session_picker::build_grouped_picker_entries(
        entries_data,
        filtered_indices,
        built,
        fields_vecs,
        state,
        current_repo,
    )
}

// ── Welcome logo ─────────────────────────────────────────────────────────────

/// [`crate::views::welcome::logo::compact_logo_line_count`].
pub fn compact_logo_line_count() -> u16 {
    crate::views::welcome::logo::compact_logo_line_count()
}

/// [`crate::views::welcome::logo::render_compact_logo`].
pub fn render_compact_logo(area: Rect, buf: &mut Buffer, theme: &Theme) {
    crate::views::welcome::logo::render_compact_logo(area, buf, theme);
}

// ── Scrollback committed frontier (minimal-mode commit bookkeeping) ──────────
//
// The `committed` marker lives on `ScrollbackEntry` so it survives
// `shift_remove`/`remove_from`; the scan cursor + expand ring
// live on `ScrollbackState`. Only minimal drives these — they are `pub(crate)`
// in `scrollback/*` and reached exclusively through the wrappers below.

/// Whether `entry` was already emitted to the terminal's native scrollback.
///
/// The committed frontier lives as an `EntryId` set on [`ScrollbackState`]
/// (survives entry reordering for free), so this looks the entry up by id.
pub fn is_committed(sb: &ScrollbackState, entry: &ScrollbackEntry) -> bool {
    sb.is_committed(entry.id)
}

/// [`ScrollbackState::commit_scan_cursor`].
pub fn commit_scan_cursor(sb: &ScrollbackState) -> usize {
    sb.commit_scan_cursor()
}

/// [`ScrollbackState::set_commit_scan_cursor`].
pub fn set_commit_scan_cursor(sb: &mut ScrollbackState, cursor: usize) {
    sb.set_commit_scan_cursor(cursor);
}

/// [`ScrollbackState::mark_committed`].
pub fn mark_committed(sb: &mut ScrollbackState, index: usize) {
    sb.mark_committed(index);
}

/// [`ScrollbackState::record_committed_for_expand`].
pub fn record_committed_for_expand(sb: &mut ScrollbackState, id: EntryId) {
    sb.record_committed_for_expand(id);
}

// ── Test-only surface (minimal's unit tests, via the test-only helpers) ──

/// [`crate::app::agent_view::test_agent_view`].
#[cfg(any(test, feature = "test-support"))]
pub fn test_agent_view(session_id: Option<&str>, cwd: std::path::PathBuf) -> AgentView {
    crate::app::agent_view::test_agent_view(session_id, cwd)
}

/// Test-only setter for `AgentView::extensions_modal`.
#[cfg(any(test, feature = "test-support"))]
pub fn set_extensions_modal(v: &mut AgentView, val: Option<ExtensionsModalState>) {
    v.extensions_modal = val;
}

/// Test-only setter for `AgentView::question_view`.
#[cfg(any(test, feature = "test-support"))]
pub fn set_question_view(v: &mut AgentView, val: Option<QuestionViewState>) {
    v.question_view = val;
}

/// Test-only setter for `AgentView::plan_mode_active`.
#[cfg(any(test, feature = "test-support"))]
pub fn set_plan_mode_active(v: &mut AgentView, on: bool) {
    v.plan_mode_active = on;
}

/// Test-only setter for `AgentView::plan_mode_pending`.
#[cfg(any(test, feature = "test-support"))]
pub fn set_plan_mode_pending(v: &mut AgentView, val: Option<bool>) {
    v.plan_mode_pending = val;
}

/// Test-only mutable access to `PromptWidget::suggestions`.
#[cfg(any(test, feature = "test-support"))]
pub fn prompt_suggestions_mut(pw: &mut PromptWidget) -> &mut SuggestionController {
    &mut pw.suggestions
}

/// Test-only setter for `AgentSession`'s yolo mode.
#[cfg(any(test, feature = "test-support"))]
pub fn set_yolo_mode_for_test(session: &mut AgentSession, on: bool) {
    session.set_yolo_mode_for_test(on);
}

/// Test-only setter for `AgentSession`'s auto mode.
#[cfg(any(test, feature = "test-support"))]
pub fn set_auto_mode_for_test(session: &mut AgentSession, on: bool) {
    session.set_auto_mode_for_test(on);
}

/// Test-only setter for the thread-local `show_thinking_blocks` appearance
/// toggle. Thinking blocks render zero rows when this is off (the default), so
/// minimal's commit-height tests must force it on to exercise a thinking
/// block's committed height instead of getting an order-dependent 0.
#[cfg(any(test, feature = "test-support"))]
pub fn set_show_thinking_blocks(enabled: bool) {
    crate::appearance::cache::set_show_thinking_blocks(enabled);
}
