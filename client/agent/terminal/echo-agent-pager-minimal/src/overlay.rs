//! Minimal-mode inline-overlay host (design K11 / §6.8).
//!
//! Renders the single active *prompt-anchored* dropdown — `@` file search, `/`
//! slash command, or shell completion — directly above the prompt, growing the
//! pinned live viewport to make room and shrinking + re-anchoring it to the
//! bottom of the screen when the dropdown closes. (Modal overlays — permission,
//! question, plan, rewind — land in PR 10; the command palette / model picker
//! in a later PR.)
//!
//! ## Viewport sizing (the load-bearing part)
//!
//! The live viewport is **not bottom-pinned**: it sits
//! directly after the committed conversation. The welcome card is committed at
//! the top of a fresh screen (`welcome::maybe_commit_welcome` resets the
//! viewport to row 0 + clears), and from there each committed block pushes the
//! viewport down via `insert_before`; once it reaches the screen bottom, further
//! commits scroll into native scrollback.
//!
//! When idle, the viewport is sized to *exactly* its content (todo panel +
//! status + overlay + prompt) so the prompt sits right after the conversation
//! with no gap, and the rest of the screen below stays empty. To show a dropdown
//! / grow for the prompt, [`Terminal::set_viewport_height`] grows the viewport's
//! **bottom edge downward** into that empty space — so committed content is never
//! scrolled away and closing the overlay leaves **no blank band**. Only when the
//! growth would overflow the screen bottom does it scroll committed rows up. The
//! shrink path keeps the top fixed, so the prompt simply moves back up. No
//! explicit bottom re-anchoring is needed (or wanted).
//!
//! `set_viewport_height` early-returns when the height is unchanged, so steady
//! state is a no-op.
//!
//! [`Terminal::set_viewport_height`]: echo_agent_ratatui_inline::Terminal::set_viewport_height

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use echo_agent_pager::app::PagerTerminal;
use echo_agent_pager::app::agent_view::AgentView;
use echo_agent_pager::app::app_view::{ActiveView, AppView};
use echo_agent_pager::appearance::LayoutConfig;
use echo_agent_pager::minimal_api;
use echo_agent_pager::render::SafeBuf as _;
use echo_agent_pager::theme::Theme;
use echo_agent_pager::views::prompt_widget::{PromptBg, PromptInfo, PromptStyle, PromptWidget};
use echo_agent_pager::views::question_view::{feedback_input, inline_text_width};

/// Which prompt-anchored dropdown is currently shown.
///
/// Mirrors the coexistence order in `AgentView::draw`: `@` file search wins over
/// `/` slash, which wins over shell completion. Only one is ever visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    FileSearch,
    Slash,
    Completion,
}

/// The active dropdown and its capped item-row count, or `None` when no
/// prompt-anchored dropdown is open (or the open one has nothing to show).
///
/// `items_width` is the width the rows render at (minimal is flush-left, so
/// the prompt/viewport width); slash rows wrap, so their count is line-based.
fn active(prompt: &PromptWidget, items_width: u16) -> Option<(Kind, u16)> {
    use echo_agent_pager::views::completion_dropdown::MAX_VISIBLE_ROWS;
    use echo_agent_pager::views::file_search::dropdown::MAX_DROPDOWN_ROWS as FILE_MAX;
    use echo_agent_pager::views::slash_dropdown::desired_item_rows;

    // Precedence matches `AgentView::draw`: file search is checked first and,
    // when visible, suppresses the others even if it currently has 0 results.
    if prompt.file_search_visible() {
        let n = (prompt.file_search.result_count() as u16).min(FILE_MAX);
        return (n > 0).then_some((Kind::FileSearch, n));
    }
    if prompt.slash_open() {
        let n = desired_item_rows(&prompt.slash_snapshot().matches, items_width);
        return (n > 0).then_some((Kind::Slash, n));
    }
    if prompt.completion_dropdown_open() {
        let n = (minimal_api::prompt_suggestions(prompt).dropdown.items.len() as u16)
            .min(MAX_VISIBLE_ROWS);
        return (n > 0).then_some((Kind::Completion, n));
    }
    None
}

/// Extra rows the active dropdown needs above the prompt (top border + items +
/// bottom border), or 0 when none is open. The bottom border doubles as the gap
/// row directly above the prompt, matching `render_dropdown_chrome`.
pub fn overlay_rows(prompt: &PromptWidget, items_width: u16) -> u16 {
    active(prompt, items_width)
        .map(|(_, rows)| panel_rows(rows))
        .unwrap_or(0)
}

/// Panel height for `item_rows` rendered rows: top border + items + bottom
/// border. `item_rows` is assumed already capped and non-zero.
fn panel_rows(item_rows: u16) -> u16 {
    item_rows + 2
}

/// Moderate target height for a centered app-modal panel. Deliberately well
/// below a full screen so closing the modal leaves only a small blank band
/// (the band equals `target - base`); a screen-tall band was the "bunch of
/// blank space" dogfooding complaint.
const MINIMAL_APP_MODAL_ROWS: u16 = 18;

/// Target live-viewport height for a centered app-modal: a moderate,
/// bottom-anchored panel rather than the full screen. Never below the live
/// region's `base`, never above the screen `ceiling`.
fn app_modal_target(base: u16, ceiling: u16) -> u16 {
    MINIMAL_APP_MODAL_ROWS.clamp(base, ceiling)
}

/// Target live-viewport height for a prompt-replacing modal (permission /
/// question / rewind): the `modal_h` rows of the modal, one status row, the
/// uncommitted live `tail_h` rows **above** it, and the `sl_h` rows of the
/// configured `[ui.status_line]` row **below** it.
///
/// Reserving room for the tail is load-bearing: a tool blocked on a permission /
/// question is held uncommitted in the live tail (`is_pending_user_input`), so
/// the tail is where its diff / command preview is drawn. Sizing to `modal_h + 1`
/// alone collapses the tail to zero rows — the "Allow Edit to …?" prompt then
/// shows with no visible diff. Floored at `base` (and at 3) so some live region
/// always remains; capped at `ceiling` — when tail + modal overflow the screen,
/// `live::draw_tail` bottom-anchors and clips the top.
fn modal_target(tail_h: u16, modal_h: u16, sl_h: u16, base: u16, ceiling: u16) -> u16 {
    tail_h
        .saturating_add(modal_h)
        .saturating_add(1) // status row between the tail and the modal
        .saturating_add(sl_h)
        .max(base)
        .min(ceiling)
        .max(3)
}

/// Grow / shrink the live viewport to fit its content + the active overlay.
///
/// The live region is **not** bottom-pinned: it sits
/// directly after the committed conversation. `set_viewport_height` grows it
/// *downward* into the empty space below when there's room — so an overlay never
/// scrolls committed content into scrollback and closing it leaves **no blank
/// band** — and only scrolls when the growth would overflow the screen bottom.
/// As blocks commit, `insert_before` pushes the viewport down naturally; once it
/// reaches the bottom, further commits scroll. A no-op when height is unchanged.
pub fn sync_viewport(app: &mut AppView, terminal: &mut PagerTerminal) {
    let term_h = terminal.last_known_area().height;
    if term_h < 3 {
        return;
    }
    let width = terminal.viewport_area().width;

    let target = compute_target(app, term_h, width);

    let cur = terminal.viewport_area();
    if cur.height == target {
        return;
    }

    // Content-anchored, NOT bottom-pinned. `sync_viewport` runs *before*
    // `commit_active` (see [`super::draw`]) and sizes to the POST-commit tail
    // ([`super::live::tail_height`]). When the conversation is short the prompt
    // sits right below the last block with blank space beneath it; once the
    // conversation fills the screen that "right after the content" position
    // *is* the bottom (content scrolls into native history as it grows, like a
    // shell). We deliberately do NOT force the viewport to the bottom edge —
    // that left a large blank gap under a short conversation (dogfood feedback).
    //
    // Two resize paths, gated on whether a commit is about to run:
    if will_commit(app) {
        // A commit follows. Only pre-set the viewport HEIGHT (to the post-commit
        // size) and keep the current top; `commit_active`'s `insert_before` then
        // does the rest — it prints the finalized block, scrolls the overflow
        // into native scrollback, clears the vacated rows, and repositions the
        // (correctly-sized) viewport to sit right after the block. We use
        // `set_viewport_area` rather than `set_viewport_height` here to skip the
        // latter's clear + grow-time scroll, which would be redundant work right
        // before `insert_before` performs its own clear/scroll. (This leaves the
        // stored `Viewport::Inline` height briefly out of sync with the area
        // height, but that is harmless: `set_viewport_height` judges grow/shrink
        // against the live area height, and it is resynced on the next frame.)
        terminal.set_viewport_area(Rect {
            height: target,
            ..cur
        });
    } else {
        // No commit this frame (overlay open/close, idle prompt edits).
        // `set_viewport_height` is top-fixed — which keeps the viewport anchored
        // right after the content — and scrolls committed rows up into native
        // scrollback on a grow (preserving them) while clearing the vacated rows
        // on a shrink (wiping stale overlay/dropdown content).
        let _ = terminal.set_viewport_height(target);
    }
}

/// Whether [`super::commit::commit_active`] will print at least one block into
/// native scrollback this frame (the shared [`super::commit::scan_frontier`]
/// projection), and no centered app-modal is holding commits. Gates the resize
/// path in [`sync_viewport`]: a commit's `insert_before` handles clearing +
/// repositioning itself, so the resize must NOT clear; otherwise
/// `set_viewport_height`'s clear is needed to wipe stale rows.
fn will_commit(app: &AppView) -> bool {
    let ActiveView::Agent(id) = &app.active_view else {
        return false;
    };
    let Some(agent) = app.agents.get(id) else {
        return false;
    };
    if app_modal_active(agent) {
        return false;
    }
    let turn_running = agent.session.state.is_turn_running();
    super::commit::scan_frontier(&agent.scrollback, turn_running).will_commit
}

/// Resolve the target viewport height for the active agent's overlay state.
fn compute_target(app: &mut AppView, term_h: u16, width: u16) -> u16 {
    let minimal_live_rows = app.appearance.minimal_live_rows;
    let ceiling = term_h.saturating_sub(1).max(3);
    let base = minimal_live_rows.clamp(3, ceiling);
    // The `[ui.status_line]` row's cap must match `live::draw_live`'s, or the
    // viewport is sized for rows the paint drops.
    let sl_h = minimal_api::status_line_frame(app)
        .height()
        .min(ceiling.saturating_sub(2));
    // Ctrl+T "force-show" pin; effective visibility (auto-hide) is computed per
    // agent by `todo_panel_height`.
    let force_todos = minimal_api::minimal_show_todos(app);
    // Committed appearance (timestamps off) so the measured tail height matches
    // exactly what `draw_tail` renders.
    let commit_app = super::commit::committed_appearance(&app.appearance);
    // Theme + prompt style: built after the agent is known so bash/feedback/
    // remember chrome matches `draw_live` (same `prompt_style` inputs).
    // Minimal is flush-left (W-38): prompt-replacing modals span the live
    // region's full width (no outer horizontal padding), so measure their
    // height at that same width — it must match `live::draw_live`'s
    // `modal_area` or the viewport is sized for a different text wrap.
    let content_w = width as usize;

    let ActiveView::Agent(id) = &app.active_view else {
        // No agent yet: size for the in-region sign-in / folder-trust UI so the
        // trust question isn't clipped to the idle prompt height.
        let hint = super::auth::minimal_auth_hint(
            &app.auth_state,
            &app.trust_state,
            app.has_access(),
            app.is_zdr_blocked(),
        );
        let needed = super::auth::auth_hint_rows(&hint, width);
        return needed.max(base).min(ceiling);
    };
    let id = *id;
    // Snapshot mode/multiline before the mut agent borrow so `prompt_style` can
    // still read `app.appearance` (same inputs as `draw_live`).
    let (input_mode, multiline) = app
        .agents
        .get(&id)
        .map(|a| (a.prompt_input_mode, a.multiline_mode))
        .unwrap_or_default();
    let theme = echo_agent_pager::theme::Theme::current();
    let style = super::live::prompt_style(&app.appearance, input_mode, &theme, multiline);

    let Some(agent) = app.agents.get_mut(&id) else {
        return base;
    };

    // A below-prompt list panel (resume / mcps) renders as a simple list under
    // the input bar; size to its exact content height (capped) so the footer
    // sits directly under the last row and the list scrolls internally past the
    // screen. Checked before the app-modal branch since the session picker is
    // also an `active_modal`.
    if let Some(kind) = super::panel::active(agent) {
        return super::panel::panel_height(agent, kind, width, ceiling);
    }

    // A centered app-modal (command palette / settings / pickers) or the
    // extensions modal (hooks / plugins / marketplace / skills) reuses the
    // full-TUI popup renderer, which fills whatever area it's given. Grow to a
    // moderate, bottom-anchored height — NOT the full ceiling. Growing to the
    // ceiling and shrinking on close left a *screen-tall* blank band above the
    // prompt (committed rows scrolled into native scrollback can't be pulled
    // back). A capped panel keeps that band small while still giving the
    // list/editor room (its inner content scrolls).
    if app_modal_active(agent) || minimal_api::extensions_modal(agent).is_some() {
        return app_modal_target(base, ceiling);
    }

    // A prompt-replacing modal (permission / question / rewind) takes the bottom
    // region in place of the prompt. Size to fit the uncommitted live tail
    // ABOVE the modal too (+ the status row between them): a tool blocked on a
    // permission / question is held in the tail (`is_pending_user_input`, see
    // `commit::is_committable`), so the tail is exactly where its diff / command
    // preview lives — without reserving room for it the viewport collapses to
    // just the modal and the "Allow Edit to …?" prompt shows with no visible
    // diff (the mid-stream permission hold of design §6.8 / risk #3). Capped at
    // `ceiling`; when tail + modal overflow, `draw_tail` bottom-anchors and
    // clips the top.
    if let Some(modal) = active_modal(agent) {
        let modal_h = modal_height(modal, agent, term_h, content_w);
        let tail_h = super::live::tail_height(agent, width, &commit_app);
        return modal_target(tail_h, modal_h, sl_h, base, ceiling);
    }

    // Otherwise size to fit the prompt (it expands as you type) plus any
    // prompt-anchored dropdown.
    let overlay_h = overlay_rows(&agent.prompt, width);
    let cap = ceiling.saturating_sub(overlay_h + 1 + sl_h).max(1);
    let prompt_h = agent
        .prompt
        .desired_height(width, &style, false, cap)
        .max(1);

    // Size the viewport to exactly its content — tail (uncommitted streaming
    // output) + todo panel + /btw panel + status + overlay + prompt — so the
    // prompt sits directly after the conversation with no gap, whether idle or
    // mid-turn. When a turn is "thinking" the tail is empty, so the prompt
    // stays right under the content instead of floating below a fixed empty
    // region; as output streams the tail grows and the viewport grows downward
    // with it. The region is not bottom-pinned, so the rest of the screen below
    // stays empty (the app "owns" the window from the top down).
    let tail_h = super::live::tail_height(agent, width, &commit_app);
    let todos_h = super::todo::todo_panel_height(agent, force_todos);
    // Below the prompt sits either the dropdown overlay or the 1-row info bar
    // (model · context usage · turn time/tokens); reserve at least the info row
    // when no dropdown is open so it isn't clipped / doesn't scroll content.
    let below_h = overlay_h.max(1);
    // `/btw` is a non-blocking side panel above the status/prompt (same place
    // as the full TUI). Height is measured at full viewport width so wrap
    // matches `live::draw_live`. Only reserve rows the shared minimal paint
    // policy accepts, otherwise a narrow or short terminal leaves a blank strip.
    let raw_btw = if minimal_api::minimal_btw_surface_available(agent) {
        echo_agent_pager::views::btw_overlay::btw_panel_height(agent.btw_state.as_ref(), width)
    } else {
        0
    };
    let chrome = 1u16 // status row
        .saturating_add(below_h)
        .saturating_add(sl_h)
        .saturating_add(prompt_h);
    let available = ceiling.saturating_sub(chrome);
    let btw_h = minimal_api::minimal_btw_visible_height(raw_btw, width, available);
    content_target(tail_h, todos_h, btw_h, below_h, prompt_h, sl_h, ceiling)
}

/// Live-viewport height sized to exactly its content: tail + todo panel + /btw
/// panel + status row + overlay + prompt + the configured `[ui.status_line]`
/// row. Floored at 2 (status + prompt), capped at the screen.
fn content_target(
    tail_h: u16,
    todos_h: u16,
    btw_h: u16,
    overlay_h: u16,
    prompt_h: u16,
    sl_h: u16,
    ceiling: u16,
) -> u16 {
    tail_h
        .saturating_add(todos_h)
        .saturating_add(btw_h)
        .saturating_add(1) // status row
        .saturating_add(overlay_h)
        .saturating_add(prompt_h)
        .saturating_add(sl_h)
        .clamp(2, ceiling)
}

/// Render the active prompt-anchored dropdown into the band directly above
/// `prompt_area`, reusing the shared dropdown chrome + item renderers so the
/// look matches the full TUI. `viewport_area` is the whole live region (used for
/// panel width / horizontal padding). A no-op when no dropdown is open.
pub fn render(
    buf: &mut Buffer,
    viewport_area: Rect,
    prompt_area: Rect,
    prompt: &mut PromptWidget,
    layout_cfg: &LayoutConfig,
    compact: bool,
    theme: &Theme,
) {
    let Some((kind, item_rows)) = active(prompt, prompt_area.width) else {
        return;
    };

    // Sync the scroll window before measuring/rendering (mirrors `AgentView`).
    if kind == Kind::FileSearch {
        prompt.file_search.ensure_visible(item_rows as usize);
    }

    let item_count = match kind {
        Kind::FileSearch => prompt.file_search.result_count(),
        Kind::Slash => prompt.slash_snapshot().matches.len(),
        Kind::Completion => minimal_api::prompt_suggestions(prompt).dropdown.items.len(),
    };

    let Some(items_rect) = minimal_api::dropdown_chrome_items(
        buf,
        item_count,
        item_rows,
        None, // no inline prompt area; anchor straight below `prompt_area`
        prompt_area,
        viewport_area,
        layout_cfg,
        compact,
        true, // minimal: anchor the dropdown *below* the input bar
        theme,
    ) else {
        return;
    };

    match kind {
        Kind::FileSearch => {
            echo_agent_pager::views::file_search::dropdown::render_dropdown(
                buf,
                items_rect,
                &prompt.file_search,
                theme,
            );
        }
        Kind::Slash => {
            let snap = prompt.slash_snapshot();
            let hovered = prompt.slash_hovered();
            echo_agent_pager::views::slash_dropdown::render_dropdown(
                buf, items_rect, &snap, hovered, theme,
            );
        }
        Kind::Completion => {
            echo_agent_pager::views::completion_dropdown::render_dropdown(
                buf,
                items_rect,
                &minimal_api::prompt_suggestions(prompt).dropdown,
                theme,
            );
        }
    }
}

// ─────────────────────────── modal overlays (PR10) ───────────────────────────
//
// Unlike the prompt-anchored dropdowns above, these modals *replace* the prompt:
// they occupy the bottom region and the user interacts with them directly. Keys
// already route to the shared permission / question / rewind handlers (minimal
// did not change input routing), so this is a render + sizing concern only.
//
// The permission, question, rewind-picker, and cancel-turn confirm modals are
// all hosted here. Plan approval is hosted separately via [`plan`] (its full-TUI
// surface is a fullscreen line-viewer + live prompt, so it gets its own minimal
// treatment rather than a `render_*` reuse).

/// A prompt-replacing modal overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modal {
    Permission,
    Question,
    Rewind,
    /// The "subagents are still running — stop them?" confirm shown when
    /// cancelling a turn with running subagents (`AgentView::cancel_turn_view`).
    Cancel,
    /// Plan approval (`AgentView::plan_approval_view`) — rendered compactly by
    /// [`super::plan`] in place of the full TUI's fullscreen line viewer.
    Plan,
}

/// The active prompt-replacing modal, in the full-TUI render precedence
/// (cancel-confirm > plan > permission > question > rewind), or `None`.
pub fn active_modal(agent: &AgentView) -> Option<Modal> {
    // The cancel-turn confirm is checked first to match the input router, which
    // intercepts keys for `cancel_turn_view` ahead of the question view
    // (`AgentView::handle_input`).
    if minimal_api::cancel_turn_view(agent).is_some() {
        return Some(Modal::Cancel);
    }
    // Plan approval routes through the line viewer (kept open in minimal) and is
    // mutually exclusive with permission/question in practice; check it next.
    if minimal_api::plan_approval_view(agent).is_some() {
        return Some(Modal::Plan);
    }
    if !agent.permission_queue.is_empty() {
        return Some(Modal::Permission);
    }
    if minimal_api::question_view(agent).is_some() {
        return Some(Modal::Question);
    }
    if minimal_api::rewind_state(agent).is_some() {
        return Some(Modal::Rewind);
    }
    None
}

/// Desired modal height in rows. `screen_h` must be the **full terminal height**
/// so the views' internal caps compute against the real screen, not the small
/// live region (design Issue 5).
pub fn modal_height(modal: Modal, agent: &mut AgentView, screen_h: u16, content_w: usize) -> u16 {
    match modal {
        Modal::Permission => agent
            .permission_queue
            .front()
            .map(|p| {
                echo_agent_pager::views::permission_view::permission_view_height(
                    p, screen_h, content_w,
                )
            })
            .unwrap_or(0),
        Modal::Question => {
            let input_mode = minimal_api::question_view(agent).is_some_and(|qv| {
                qv.focus == echo_agent_pager::views::question_view::QuestionFocus::InputMode
            });
            let editor_extra = if input_mode {
                question_editor_h(
                    agent,
                    content_w as u16,
                    question_editor_cap(screen_h),
                    &Theme::current(),
                )
                .saturating_sub(1)
            } else {
                0
            };
            minimal_api::question_view_mut(agent)
                .map(|qv| {
                    echo_agent_pager::views::question_view::question_view_height(
                        qv, screen_h, content_w,
                    )
                    .saturating_add(editor_extra)
                })
                .unwrap_or(0)
        }
        Modal::Rewind => minimal_api::rewind_state(agent)
            .map(|rw| echo_agent_pager::views::rewind::rewind_overlay_height(&rw.phase, screen_h))
            .unwrap_or(0),
        Modal::Cancel => {
            if minimal_api::cancel_turn_view(agent).is_some() {
                echo_agent_pager::views::modal::cancel_turn_panel_height(screen_h)
            } else {
                0
            }
        }
        Modal::Plan => super::plan::height(agent),
    }
}

/// Render the active modal into `area` (the live region's full width —
/// flush-left, W-38). `screen_h` is the full terminal height — the same
/// value `modal_height` sized the area with. Returns the text cursor when
/// the modal hosts an inline editor (permission follow-up / question input
/// mode), else `None`.
pub fn render_modal(
    buf: &mut Buffer,
    area: Rect,
    modal: Modal,
    agent: &mut AgentView,
    theme: &Theme,
    screen_h: u16,
) -> Option<(u16, u16)> {
    match modal {
        Modal::Permission => render_permission(buf, area, agent, theme),
        Modal::Question => render_question(buf, area, agent, theme, screen_h),
        Modal::Rewind => {
            if let Some(rw) = minimal_api::rewind_state(agent) {
                echo_agent_pager::views::rewind::render_rewind_overlay(buf, area, &rw.phase, true);
            }
            None
        }
        Modal::Cancel => {
            // The prompt is always focused in minimal, so the confirm is too.
            // `cancel_turn_view` (shared) and `cancel_turn_buttons_mut` (exclusive)
            // can't both be borrowed from the agent at once through the facade, so
            // render the hit-test rects into a local Vec and store them back after.
            let mut buttons: Vec<Rect> = Vec::new();
            let drawn = if let Some(ctv) = minimal_api::cancel_turn_view(agent) {
                echo_agent_pager::views::modal::render_cancel_turn_panel(
                    buf,
                    area,
                    ctv,
                    true,
                    &mut buttons,
                );
                true
            } else {
                false
            };
            if drawn {
                *minimal_api::cancel_turn_buttons_mut(agent) = buttons;
            }
            None
        }
        Modal::Plan => super::plan::render(buf, area, agent, theme),
    }
}

// ─────────────────────────── app-modals (PR13 / PR15) ────────────────────────
//
// A second family of overlays lives in `AgentView::active_modal` (the full-TUI
// `ActiveModal` enum) rather than the per-feature fields the [`Modal`]s above
// read: the command palette, keyboard-shortcuts help, settings editor, model /
// session / doc pickers, memory browser, etc. These are *centered popups* with
// their own rich renderers, so — unlike the bottom-anchored modals — minimal
// hosts them by growing the live viewport to (near) the whole screen and
// reusing the exact full-TUI renderer ([`AgentView::draw_active_modal`]).
// Committed scrollback scrolls up out of the way during the grow (and stays in
// native history); the popup centers in the grown region.
//
// Input already routes to the shared `handle_modal_key` path (minimal only
// swaps the *render* path), so hosting here is purely a render + sizing concern.

/// Whether an `AgentView::active_modal` (command palette / shortcuts help /
/// settings / pickers / …) is open. Minimal hosts these as centered overlays.
pub fn app_modal_active(agent: &AgentView) -> bool {
    agent.active_modal.is_some()
}

/// Render the active centered app-modal into `area`, reusing the exact full-TUI
/// [`AgentView::draw_active_modal`] dispatch so look + behavior match every
/// mode. Returns whether anything was drawn.
pub fn render_app_modal(
    buf: &mut Buffer,
    area: Rect,
    agent: &mut AgentView,
    compact: bool,
) -> bool {
    if agent.active_modal.is_none() {
        return false;
    }
    minimal_api::draw_active_modal(agent, area, buf, Theme::current(), compact);
    true
}

fn render_permission(
    buf: &mut Buffer,
    area: Rect,
    agent: &mut AgentView,
    theme: &Theme,
) -> Option<(u16, u16)> {
    let perm = agent.permission_queue.front()?;
    // Clone so the immutable borrow of `agent.prompt` ends before the mutable
    // `agent.prompt.draw` below.
    let followup = agent.prompt.text().to_string();
    let result = echo_agent_pager::views::permission_view::render_permission_view(
        buf,
        area,
        perm,
        &followup,
        agent.permission_pattern_edit.as_ref(),
        minimal_api::hovered_permission_item(agent),
        theme,
        true,
    );
    // Follow-up (reject-with-text) hosts an inline editor; draw it plainly
    // (minimal skips the full TUI's accent-bar/scrollbar chrome).
    let iarea = result.inline_prompt?;
    let style = inline_input_style(theme);
    let height = (area.y + area.height)
        .saturating_sub(iarea.y)
        .saturating_sub(1)
        .max(1);
    let rect = Rect {
        x: iarea.text_x,
        y: iarea.y,
        width: iarea.text_w,
        height,
    };
    agent
        .prompt
        .draw(buf, rect, None, &style, None, None)
        .cursor_pos
}

fn render_question(
    buf: &mut Buffer,
    area: Rect,
    agent: &mut AgentView,
    theme: &Theme,
    screen_h: u16,
) -> Option<(u16, u16)> {
    use echo_agent_pager::views::question_view::{QUESTION_VIEW_HPAD, QuestionFocus};

    let input_mode = minimal_api::question_view(agent)
        .map(|qv| qv.focus == QuestionFocus::InputMode)
        .unwrap_or(false);
    let input_h: u16 = if input_mode {
        question_editor_h(
            agent,
            area.width,
            question_editor_cap(screen_h).min(area.height.saturating_sub(1).max(1)),
            theme,
        )
    } else {
        0
    };
    let q_area = Rect {
        height: area.height.saturating_sub(input_h),
        ..area
    };
    let content_w = area.width.saturating_sub(QUESTION_VIEW_HPAD) as usize;

    // Clamp any stale scroll offset to the rendered viewport (mirrors
    // `AgentView::draw`). Computed in an inner scope so the immutable borrows
    // end before the `&mut clamp_scroll`.
    if let Some(qv) = minimal_api::question_view_mut(agent) {
        let vis = qv.questions.get(qv.active_tab).map(|question| {
            echo_agent_pager::views::question_view::visible_options_height(
                question,
                q_area.height,
                content_w,
                qv.focused_preview(),
                qv.fullscreen,
                qv.cached_desc_cap,
                qv.cached_preview_cap,
            )
        });
        if let Some(vis) = vis {
            qv.clamp_scroll(vis, content_w);
        }
    }

    if let Some(qv) = minimal_api::question_view(agent) {
        echo_agent_pager::views::question_view::render_question_view(
            buf,
            q_area,
            qv,
            minimal_api::hovered_question_item(agent),
            theme,
            true,
        );
    }

    if input_mode && is_feedback_pane(agent) {
        return render_feedback_editor(buf, area, agent, theme, input_h);
    }

    if input_mode {
        let row_y = area.y + area.height.saturating_sub(input_h);
        let is_multi = minimal_api::question_view(agent)
            .and_then(|qv| qv.questions.get(qv.active_tab))
            .and_then(|q| q.multi_select)
            .unwrap_or(false);
        let freeform_sel = minimal_api::question_view(agent)
            .and_then(|qv| qv.per_question_freeform_selected.get(qv.active_tab))
            .copied()
            .unwrap_or(false);
        let embed = echo_agent_pager::views::modal_window::embedded_row_style(theme, true);
        let fg = |normal| embed.map_or(normal, |e| e.fg(normal));
        let marker = if is_multi {
            (if freeform_sel { "[x]" } else { "[ ]" }).to_string()
        } else if freeform_sel {
            format!("({})", echo_agent_pager::glyphs::filled_dot())
        } else {
            "(\u{25cb})".to_string()
        };
        let marker_style = if freeform_sel {
            Style::default()
                .fg(fg(theme.text_primary))
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            Style::default().fg(fg(theme.gray))
        };
        let accent = Style::default().fg(fg(theme.accent_user));
        buf.set_span_safe(
            area.x + 3,
            row_y,
            &ratatui::text::Span::styled("z ", accent),
            2,
        );
        buf.set_span_safe(
            area.x + 5,
            row_y,
            &ratatui::text::Span::styled(format!("{marker} "), marker_style),
            4,
        );
        buf.set_span_safe(
            area.x + 9,
            row_y,
            &ratatui::text::Span::styled(echo_agent_pager::glyphs::prompt_arrow(), accent),
            2,
        );

        let style = inline_input_style(theme);
        let editor = Rect {
            x: area.x + 11,
            y: row_y,
            width: echo_agent_pager::views::question_view::inline_text_width(area.width),
            height: input_h,
        };
        return agent
            .prompt
            .draw(buf, editor, None, &style, None, None)
            .cursor_pos;
    }
    None
}

/// Whether the open question pane is the bare `/feedback` report box.
fn is_feedback_pane(agent: &AgentView) -> bool {
    minimal_api::question_view(agent).is_some_and(|qv| qv.is_feedback())
}

/// The bare `/feedback` report box, painted over the bottom `input_h` rows of the question card.
fn render_feedback_editor(
    buf: &mut Buffer,
    area: Rect,
    agent: &mut AgentView,
    theme: &Theme,
    input_h: u16,
) -> Option<(u16, u16)> {
    // The box lives inside `area`: the inline viewport can be shorter than the height request, and drawing past it indexes outside the frame.
    let box_h = input_h.min(area.height);
    if box_h == 0 {
        return None;
    }
    let input_area = Rect {
        x: area.x + 3,
        y: area.y + area.height.saturating_sub(box_h),
        width: feedback_input::width(area.width),
        height: box_h,
    };

    // Carry the card's surface and accent bar down the report rows.
    buf.set_style(
        Rect {
            x: area.x,
            y: input_area.y,
            width: area.width,
            height: box_h,
        },
        Style::default().bg(theme.bg_light),
    );
    for y in input_area.y..input_area.y.saturating_add(box_h) {
        if let Some(cell) = buf.cell_mut((area.x, y)) {
            cell.set_symbol(echo_agent_pager::glyphs::accent_bar());
            cell.set_style(Style::default().fg(theme.accent_user).bg(theme.bg_light));
        }
    }

    // Drop the outline when the box is too squeezed for it, so the text stays visible.
    let outlined = box_h >= feedback_input::MIN_HEIGHT;
    let style = if outlined {
        feedback_input::style(theme)
    } else {
        feedback_input::flat_style(theme)
    };
    agent
        .prompt
        .draw(
            buf,
            input_area,
            None,
            &style,
            outlined.then_some(&PromptInfo::default()),
            None,
        )
        .cursor_pos
}

/// Height cap for the inline question editor — the full TUI's policy
/// (`agent_view/render.rs`, `inline_prompt_max`).
fn question_editor_cap(screen_h: u16) -> u16 {
    ((screen_h as u32) / 3).clamp(3, 15) as u16
}

/// Desired height of the inline question editor (InputMode), bounded by `cap`.
fn question_editor_h(agent: &AgentView, area_w: u16, cap: u16, theme: &Theme) -> u16 {
    if is_feedback_pane(agent) {
        feedback_editor_h(agent, area_w, cap, theme)
    } else {
        freeform_editor_h(agent, area_w, cap, theme)
    }
}

/// The bare `/feedback` report box: its rules plus at least one text row, growing with the report.
/// Unlike the full TUI it reserves no rows up front, because `cap` is all the room the inline viewport has.
fn feedback_editor_h(agent: &AgentView, area_w: u16, cap: u16, theme: &Theme) -> u16 {
    let min_h = feedback_input::MIN_HEIGHT.min(cap.max(1));
    agent
        .prompt
        .desired_height(
            feedback_input::width(area_w),
            &feedback_input::style(theme),
            true,
            cap.max(min_h),
        )
        .max(min_h)
}

/// The one-line freeform answer row, growing with what the user types.
fn freeform_editor_h(agent: &AgentView, area_w: u16, cap: u16, theme: &Theme) -> u16 {
    agent
        .prompt
        .desired_height(
            inline_text_width(area_w),
            &inline_input_style(theme),
            false,
            cap.max(1),
        )
        .max(1)
}

/// Chromeless prompt style for a modal's inline editor (no prefix / accent /
/// borders — the modal supplies its own framing).
fn inline_input_style(theme: &Theme) -> PromptStyle {
    PromptStyle {
        focused: true,
        show_prefix: false,
        vpad_top: 0,
        compact: false,
        chrome: false,
        chrome_pad_left: 0,
        chrome_pad_right: 0,
        bg: PromptBg::Panel(theme.bg_visual),
        accent_color_override: None,
        border_color_override: None,
        prefix_override: None,
        placeholder_when_focused: false,
        placeholder_override: None,
        show_accent_line: false,
        show_borders: false,
        title: None,
        image_preview: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_pager::views::suggestion_controller::CompletionItemParsed;

    fn completion_item() -> CompletionItemParsed {
        // Functional update: only the semantic fields; new optional item
        // fields (e.g. the replace-range pair) default without breaking
        // this out-of-crate literal again.
        CompletionItemParsed {
            display: "item".into(),
            insert_text: "item".into(),
            ..Default::default()
        }
    }

    /// Build an agent with an active single-select question in InputMode and
    /// the given freeform text loaded into the prompt.
    fn question_input_agent(text: &str) -> AgentView {
        use echo_agent_pager::views::prompt_widget::StashedPrompt;
        use echo_agent_pager::views::question_view::{Question, QuestionOption, QuestionViewState};

        let mut agent = minimal_api::test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        let mut qv = QuestionViewState::new(
            "tc-1".into(),
            vec![Question {
                question: "Pick one?".into(),
                options: vec![QuestionOption {
                    label: "Alpha".into(),
                    description: String::new(),
                    preview: None,
                    id: None,
                }],
                multi_select: Some(false),
                id: None,
            }],
            StashedPrompt::default(),
        );
        qv.per_question_cursor[0] = qv.questions[0].options.len();
        let _stashed = qv.activate_freeform_input();
        minimal_api::set_question_view(&mut agent, Some(qv));
        agent.prompt.set_text(text);
        agent
    }

    /// Regression: the editor height was hardcoded to 1 row (only the last
    /// typed line was visible) and InputMode dissolved the freeform row into
    /// a bare text line without its `z (·) ❯` prefix.
    #[test]
    fn question_input_mode_editor_grows_and_keeps_row_prefix() {
        let screen_h = 40u16;
        let content_w = 80usize;

        let mut single = question_input_agent("one line");
        let base_h = modal_height(Modal::Question, &mut single, screen_h, content_w);

        let mut multi = question_input_agent("line one\nline two\nline three");
        let multi_h = modal_height(Modal::Question, &mut multi, screen_h, content_w);
        assert_eq!(
            multi_h,
            base_h + 2,
            "editor must reserve one extra row per extra text line"
        );

        // Render and audit the editor rows.
        let theme = Theme::terminal_default();
        let area = Rect::new(0, 0, content_w as u16, multi_h);
        let mut buf = Buffer::empty(area);
        let cursor = render_question(&mut buf, area, &mut multi, &theme, screen_h);
        assert!(cursor.is_some(), "InputMode returns the editor cursor");

        // Editor occupies the last 3 rows; its first row carries the prefix.
        let editor_top = multi_h - 3;
        let row_text: String = (0..area.width)
            .map(|x| {
                buf.cell((x, editor_top))
                    .map(|c| c.symbol().to_string())
                    .unwrap_or_default()
            })
            .collect();
        assert!(
            row_text.contains('z') && row_text.contains("(\u{25cf})"),
            "editor row must keep the freeform-row `z (\u{25cf})` prefix, got {row_text:?}"
        );
        assert!(
            row_text.contains("line one"),
            "first text line renders on the first editor row, got {row_text:?}"
        );
        let all_text: String = (0..area.height)
            .flat_map(|y| {
                (0..area.width).map(move |x| (x, y)).map(|pos| {
                    buf.cell(pos)
                        .map(|c| c.symbol().to_string())
                        .unwrap_or_default()
                })
            })
            .collect();
        for line in ["line one", "line two", "line three"] {
            assert!(all_text.contains(line), "{line:?} must be visible");
        }
    }

    /// Regression (Bugbot): the renderer used to cap the editor only by
    /// `area.height - 1`, so with text longer than the reserved cap it drew
    /// editor rows over the question list.
    #[test]
    fn question_editor_render_cap_matches_reserved_cap() {
        let screen_h = 40u16; // cap = 13
        let content_w = 80usize;
        let cap = question_editor_cap(screen_h);
        assert_eq!(cap, 13);

        let long_text = (1..=30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut agent = question_input_agent(&long_text);
        let one_line = modal_height(
            Modal::Question,
            &mut question_input_agent("x"),
            screen_h,
            content_w,
        );
        let capped = modal_height(Modal::Question, &mut agent, screen_h, content_w);
        assert_eq!(
            capped,
            one_line + (cap - 1),
            "reserved rows must stop at the shared cap"
        );

        let theme = Theme::terminal_default();
        let area = Rect::new(0, 0, content_w as u16, capped);
        let mut buf = Buffer::empty(area);
        render_question(&mut buf, area, &mut agent, &theme, screen_h);

        let all_text: String = (0..area.height)
            .flat_map(|y| {
                (0..area.width).map(move |x| (x, y)).map(|pos| {
                    buf.cell(pos)
                        .map(|c| c.symbol().to_string())
                        .unwrap_or_default()
                })
            })
            .collect();
        assert!(
            all_text.contains("Pick one?"),
            "question chrome must stay visible above the capped editor"
        );
        // The editor's first row (prefix row) sits exactly `cap` rows from
        // the bottom.
        let editor_top = capped - cap;
        let row_text: String = (0..area.width)
            .map(|x| {
                buf.cell((x, editor_top))
                    .map(|c| c.symbol().to_string())
                    .unwrap_or_default()
            })
            .collect();
        assert!(
            row_text.contains('z') && row_text.contains("line 1"),
            "editor top row must be the prefix row at the capped offset, got {row_text:?}"
        );
    }

    #[test]
    fn panel_rows_adds_borders() {
        assert_eq!(panel_rows(1), 3);
        assert_eq!(panel_rows(8), 10);
    }

    #[test]
    fn fresh_prompt_has_no_overlay() {
        let pw = PromptWidget::new();
        assert_eq!(overlay_rows(&pw, 80), 0);
        assert!(active(&pw, 80).is_none());
    }

    #[test]
    fn open_completion_dropdown_reports_rows() {
        let mut pw = PromptWidget::new();
        // Open the shell-completion dropdown with three items. The completion
        // dropdown's fields are crate-visible, so we can drive it directly
        // without a full prompt key sequence.
        minimal_api::prompt_suggestions_mut(&mut pw).dropdown.open = true;
        minimal_api::prompt_suggestions_mut(&mut pw).dropdown.items =
            vec![completion_item(), completion_item(), completion_item()];
        assert_eq!(active(&pw, 80), Some((Kind::Completion, 3)));
        assert_eq!(overlay_rows(&pw, 80), 5); // 3 items + 2 borders
    }

    /// Regression (PR review): the dropdown sizes itself to its uncapped rows,
    /// and its below-anchor guard only checks the rect it is given — so
    /// `draw_live` must exclude the `[ui.status_line]` band from that rect, or
    /// a height-clamped panel paints into rows the status line repaints.
    #[test]
    fn dropdown_never_paints_into_the_status_line_band() {
        let mut pw = PromptWidget::new();
        minimal_api::prompt_suggestions_mut(&mut pw).dropdown.open = true;
        minimal_api::prompt_suggestions_mut(&mut pw).dropdown.items =
            vec![completion_item(), completion_item(), completion_item()];
        let full = Rect::new(0, 0, 80, 12);
        let prompt_area = Rect::new(0, 6, 80, 1);
        let layout_cfg = LayoutConfig::default();
        let theme = Theme::terminal_default();
        let sl_h = 2u16;
        let band_painted = |buf: &Buffer| {
            (full.height - sl_h..full.height).any(|y| {
                (0..full.width).any(|x| buf.cell((x, y)).is_some_and(|c| c.symbol() != " "))
            })
        };

        // The 5-row panel below the prompt reaches the last viewport row, so
        // given the full rect it paints into the band.
        let mut buf = Buffer::empty(full);
        render(
            &mut buf,
            full,
            prompt_area,
            &mut pw,
            &layout_cfg,
            false,
            &theme,
        );
        assert!(band_painted(&buf), "fixture must reach into the band");

        // With the band excluded the guard drops the panel instead.
        let mut buf = Buffer::empty(full);
        let overlay_area = Rect {
            height: full.height - sl_h,
            ..full
        };
        render(
            &mut buf,
            overlay_area,
            prompt_area,
            &mut pw,
            &layout_cfg,
            false,
            &theme,
        );
        assert!(!band_painted(&buf), "the status-line band must stay clean");
    }

    #[test]
    fn completion_dropdown_caps_item_rows() {
        use echo_agent_pager::views::completion_dropdown::MAX_VISIBLE_ROWS;
        let mut pw = PromptWidget::new();
        minimal_api::prompt_suggestions_mut(&mut pw).dropdown.open = true;
        minimal_api::prompt_suggestions_mut(&mut pw).dropdown.items =
            (0..(MAX_VISIBLE_ROWS as usize + 5))
                .map(|_| completion_item())
                .collect();
        assert_eq!(overlay_rows(&pw, 80), MAX_VISIBLE_ROWS + 2);
    }

    #[test]
    fn empty_open_dropdown_reports_nothing() {
        let mut pw = PromptWidget::new();
        minimal_api::prompt_suggestions_mut(&mut pw).dropdown.open = true; // open but no items
        assert_eq!(overlay_rows(&pw, 80), 0);
        assert!(active(&pw, 80).is_none());
    }

    #[test]
    fn content_target_fits_content_with_no_gap() {
        // Viewport = tail + todos + btw + status(1) + overlay + prompt — no base
        // floor, so the prompt sits right after the conversation. Idle (tail 0,
        // empty prompt) is just status + prompt.
        assert_eq!(content_target(0, 0, 0, 0, 1, 0, 40), 2); // status + 1-row prompt
        assert_eq!(content_target(0, 3, 0, 0, 1, 0, 40), 5); // + 3 todo rows
        assert_eq!(content_target(0, 3, 0, 5, 2, 0, 40), 11); // + overlay(5) + 2-row prompt
        // /btw Loading is 3 rows; Done/Error grow with the content.
        assert_eq!(content_target(0, 0, 3, 0, 1, 0, 40), 5); // + btw(3)
        // Production idle always reserves ≥1 below the prompt (info bar).
        assert_eq!(content_target(0, 0, 3, 1, 1, 0, 40), 6); // btw+status+info+prompt
        // todos + btw stack without collapsing either.
        assert_eq!(content_target(0, 3, 3, 0, 1, 0, 40), 8);
        // The streaming tail grows the viewport (no fixed empty gap while
        // "thinking": tail 0 → just status + prompt).
        assert_eq!(content_target(6, 0, 0, 0, 1, 0, 40), 8); // tail(6) + status + prompt
        assert_eq!(content_target(0, 0, 0, 1, 1, 1, 40), 4); // + status_line row
        assert_eq!(content_target(0, 0, 0, 1, 1, 3, 40), 6); // 3-line script output
        // Floored at 2 (status + prompt) and capped at the screen ceiling.
        assert_eq!(content_target(0, 0, 0, 0, 0, 0, 40), 2);
        assert_eq!(content_target(50, 0, 0, 0, 0, 0, 20), 20);
    }

    #[test]
    fn app_modal_target_is_capped_not_ceiling() {
        // Tall screen: a moderate bottom panel, never the whole screen — so the
        // band left on close (target - base) stays small.
        assert_eq!(app_modal_target(10, 49), MINIMAL_APP_MODAL_ROWS);
        // Short screen: clamps down to what fits.
        assert_eq!(app_modal_target(10, 14), 14);
        // A larger live region wins (never shrink the modal below base).
        assert_eq!(app_modal_target(24, 49), 24);
    }

    #[test]
    fn modal_target_reserves_room_for_the_pending_tail() {
        // Regression: an edit awaiting permission is held uncommitted in the
        // tail, so the permission modal must grow to show that diff ABOVE it,
        // not collapse to just the prompt. tail(10) + modal(8) + status(1) = 19,
        // which fits under the ceiling.
        assert_eq!(modal_target(10, 8, 0, 6, 40), 19);
        // No uncommitted tail (e.g. a bash permission with nothing streamed):
        // modal + status only — identical to the pre-fix behavior, no regression.
        assert_eq!(modal_target(0, 8, 0, 6, 40), 9);
        // A tall pending diff + modal overflow the screen → capped at the
        // ceiling; `draw_tail` then bottom-anchors and clips the top of the diff.
        assert_eq!(modal_target(100, 8, 0, 6, 40), 40);
        // Tiny modal, no tail → floored at `base` so some live region remains.
        assert_eq!(modal_target(0, 1, 0, 6, 40), 6);
        assert_eq!(modal_target(10, 8, 2, 6, 40), 21); // + 2 status_line rows
    }

    #[test]
    fn content_target_clamps_to_screen() {
        // Content taller than the screen clamps to the ceiling (then the tail
        // scrolls / clips); a tiny terminal still yields at least the floor.
        assert_eq!(content_target(30, 0, 0, 0, 1, 0, 24), 24);
        assert_eq!(content_target(5, 0, 0, 0, 1, 0, 2), 2);
        // A tall /btw Done answer still clamps rather than overflowing.
        assert_eq!(content_target(0, 0, 20, 0, 1, 0, 10), 10);
    }

    #[test]
    fn btw_height_policy_matches_draw_live_boundaries() {
        let visible = minimal_api::minimal_btw_visible_height;
        assert_eq!(visible(3, 80, 40), 3);
        assert_eq!(visible(12, 80, 40), 12);
        assert_eq!(visible(20, 80, 10), 10);
        assert_eq!(visible(0, 80, 40), 0);
        // Width 11/12 and available rows 2/3 are the production boundary.
        assert_eq!(visible(3, 11, 40), 0);
        assert_eq!(visible(3, 12, 40), 3);
        assert_eq!(visible(3, 80, 2), 0);
        assert_eq!(visible(3, 80, 3), 3);
        assert_eq!(content_target(0, 0, visible(3, 11, 40), 1, 1, 0, 40), 3);
        assert_eq!(content_target(0, 0, visible(3, 80, 2), 1, 1, 0, 5), 3);
    }
}
