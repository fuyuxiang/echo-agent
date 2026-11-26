//! Dashboard rendering.

use indexmap::IndexMap;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use super::layout::{MIN_DASHBOARD_WIDTH, compute_layout};
use super::row::{DashboardRow, RowBadge, build_rows_with_roster};
use super::state::{
    DashboardRowId, DashboardState, Filter, Focusable, Grouping, LocationPickerState, RenameDraft,
    RowState, SectionKey,
};
use crate::app::agent::AgentId;
use crate::app::agent_view::AgentView;
use crate::render::line_utils::{truncate_line, truncate_str};
use crate::theme::Theme;
use crate::util::format_time_ago;

/// Show each spinner frame for this many animation ticks. The frames
/// themselves come from [`crate::glyphs::dot_spinner_frames`] so they
/// degrade to an ASCII pulse on legacy Windows consoles.
const SPINNER_DIVISOR: u64 = 4;
/// How many ticks each phase of the `NeedsInput` bullet blink lasts. At the
/// ~30 Hz dashboard tick this toggles roughly every 0.33 s (≈1.5 Hz blink).
const NEEDS_INPUT_BLINK_DIVISOR: u64 = 10;

// Row markers use the filled (◆) / hollow (◇) diamonds from `crate::glyphs`
// (with CP437 fallbacks on legacy consoles). The dashboard uses
// diamonds instead of circles to differentiate this view's vocabulary from
// sibling activity views (which use circles). Filled marks the non-working states that
// need a strong visual presence (needs-input, completed, failed, blocked);
// hollow marks idle rows.

fn ensure_peek_viewport_lifecycle(
    state: &mut DashboardState,
    agents: &mut IndexMap<AgentId, AgentView>,
) {
    if state.attached_agent.is_some() {
        return;
    }
    // Peek row → begin/keep lease; else restore agent viewport.
    let Some(row) = state.peek.as_ref().map(|p| p.row.clone()) else {
        state.restore_peek_viewport(agents);
        return;
    };
    if state
        .peek_viewport
        .as_ref()
        .is_some_and(|lease| lease.row == row)
    {
        return;
    }
    if super::state::scrollback_available_for_row(&row, agents) {
        state.begin_peek_viewport(row, agents);
    } else {
        state.restore_peek_viewport(agents);
    }
}

// The thin left vertical bar marking the active/selected row
// (`crate::glyphs::selection_bar()`, with a `│` CP437 fallback on legacy
// consoles) is painted on *every* content line of a selected row so it
// spans the row's full visual height.

/// Per-row visual height (cells) when each row renders as a
/// title-row + secondary-row + 1-cell breathing gap.
const ROW_HEIGHT: u16 = 3;
/// Per-group-header visual height (label row + 1-cell breathing gap).
const GROUP_HEADER_HEIGHT: u16 = 2;

/// Promo upgrade CTA for the dashboard header, resolved through the shared
/// slot gate by the producer (`app_view`).
#[derive(Clone, Copy)]
pub struct HeaderUpgradeCta<'a> {
    /// The `[label]` button text.
    pub label: &'a str,
    /// Non-dismissible promo → the `Ctrl+O` override applies.
    pub pinned: bool,
    /// The promo's trimmed `cta.caption` accessor value; pinned-gated at paint.
    pub caption: Option<&'a str>,
}

/// Render the dashboard. Mirrors `agent_view::draw` — returns the
/// cursor position to place after the frame is committed.
///
/// Edge case 8 contract: when a permission request fires on an agent
/// while the dashboard is open, this renderer DOES NOT auto-popup the
/// `PermissionView` modal. Instead, the row's state flips to
/// `NeedsInput` (blinking yellow bullet + yellow `Pending:` subtitle). The user
/// must press Space to peek (which routes the permission question +
/// options into the peek panel) and then a number key to answer. This
/// is intentional: the dashboard is the top-level view, and we don't
/// want random subagent permission requests to obscure it.
#[allow(clippy::too_many_arguments)]
pub fn render_dashboard(
    buf: &mut Buffer,
    area: Rect,
    state: &mut DashboardState,
    agents: &mut IndexMap<AgentId, AgentView>,
    registry: &crate::actions::ActionRegistry,
    // App-level double-press confirmation hint (e.g. "press again to
    // quit" for Ctrl+Q / Ctrl+C / Ctrl+D). Threaded to the footer so the
    // session-less dashboard shows the same feedback the agent view does.
    pending_hint: Option<crate::views::shortcuts_bar::PendingHint>,
    // Leader-mode session roster (FleetView). Empty in non-leader mode,
    // which naturally gates the appended roster-only rows.
    roster: &[crate::app::roster::RosterEntry],
    // Whether the local on-disk session roster is still being fetched
    // (non-leader mode). When true and there's nothing to show yet, the
    // empty body reads "Loading sessions…" instead of the "no agents
    // yet" hint so a fresh open doesn't flash an empty-looking screen.
    dashboard_sessions_loading: bool,
    // Promo upgrade CTA to paint in the header after the location label
    // (`None` = no CTA); field meanings live on [`HeaderUpgradeCta`].
    upgrade_cta: Option<HeaderUpgradeCta<'_>>,
) -> Option<(u16, u16)> {
    // Cache whether a pinned (non-dismissible) promo CTA is live so the key
    // handler can steal Ctrl+O for it; the dispatch re-resolves the gate.
    state.pinned_upgrade_cta_live = upgrade_cta.is_some_and(|cta| cta.pinned);
    // Re-anchor selection BEFORE we build the rows so that the
    // visible set drives selection clamping.
    let theme = Theme::current();
    // `spinner_tick` is bumped in `AppView::tick()`,
    // not here, so the spinner advances even when no redraw was
    // triggered by other state changes.
    state.last_area = area;

    // Paint the full area with the theme's base background BEFORE
    // any sub-renderer runs. Mirrors `welcome::render` and
    // `PromptWidget::draw` — without this, cells that no sub-renderer
    // touches in a given frame (e.g. blank rows between the last list
    // row and the dispatch input, or trailing whitespace past short
    // row content) retain stale paint from the previous frame and the
    // dashboard looks like it doesn't cover the full panel.
    buf.set_style(area, ratatui::style::Style::default().bg(theme.bg_base));

    let home = cached_home();
    // The dashboard is not anchored to a specific agent; we treat every
    // row equally for highlighting. `is_active` survives for legacy
    // comparators that want to know what view the user came from (None
    // in fresh dashboard renders).
    let active: Option<AgentId> = None;
    let rows = build_rows_with_roster(
        agents,
        &state.pinned,
        &state.reorder,
        active,
        state.grouping,
        &state.filter,
        home,
        roster,
    );
    // Chat-conversation roster rows can't be deleted from the dashboard
    // yet — record them so the `[✗]` and Ctrl+X arm both skip them.
    state.conversation_row_ids = roster
        .iter()
        .filter(|e| e.origin.kind == "conversation")
        .map(|e| e.session_id.clone())
        .collect();
    state.reanchor_selection(&rows);

    // DO NOT GC pinned/reorder at render time. The old
    // code built the alive-set from the *post-filter* row list, so a
    // user-typed filter that hid a pinned row would (a) remove the
    // pin from the in-memory set, (b) be persisted on the next pin /
    // grouping toggle, (c) silently destroy the user's state.
    //
    // GC now runs only at open time (see `dispatch_open_dashboard`),
    // where it has access to the raw `app.agents` snapshot.

    // When an agent is attached, the popup overlay
    // covers the bottom portion of the screen with the agent's full
    // view (scrollback + status bar + prompt + shortcuts). Rendering
    // the dashboard's own dispatch input + footer below the popup
    // produced TWO stacked input bars, which the user explicitly
    // flagged as broken UX. In popup mode we paint ONLY a compact
    // dashboard banner at the top (rows in a bordered panel), and
    // the popup fills the rest. The dashboard's header, dispatch
    // input, footer, and bottom margin are all skipped — the
    // attached agent carries its own equivalents.
    if state.attached_agent.is_some() {
        state.peek_close_rect = None;
        state.slash_dropdown_items_area = None;
        state.slash_dropdown_hit = Default::default();
        state.file_search_dropdown_items_area = None;
        state.dispatch_rect = None;
        let popup = popup_rect(area);
        let banner_h = popup.y.saturating_sub(area.y);
        let banner_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: banner_h,
        };
        render_dashboard_banner(buf, banner_area, &theme, &rows, state);
        // No cursor for the dashboard itself — the popup's agent
        // owns the cursor.
        return None;
    }

    // Peek: list-first allocation (see layout::allocate_peek /
    // docs/internal/33-dashboard-peek-responsive-layout.md). Provisional
    // layout gives dispatch width for reply wrapping before we decide
    // whether peek fits.
    let mut layout = compute_layout(area, false);
    let fixed = super::layout::chrome_overhead(area);
    let reply_text_w = layout.dispatch.width.saturating_sub(6);

    match state.selected.clone() {
        Some(sel) => match super::peek::compute_peek_fields(&sel, agents) {
            Some(fields) => {
                let question = fields.question.is_some();
                let peek_min = if question {
                    super::layout::PEEK_MIN_BOX_QUESTION
                } else {
                    super::layout::PEEK_MIN_BOX_LIVE_TAIL
                };
                let content_rows = if question {
                    1 + fields.options.len().min(9) as u16
                } else {
                    let reply_rows = super::peek::reply_row_count(
                        &state.peek_reply,
                        reply_text_w,
                        super::peek::MAX_REPLY_ROWS,
                    );
                    let max_content = super::layout::max_peek_content_rows(area);
                    // Middle content width ≈ dispatch box minus borders + insets.
                    let middle_w = layout.dispatch.width.saturating_sub(4);
                    let (body_measured, pin_user) =
                        super::state::scrollback_mut_for_row(&sel, agents)
                            .map(|sb| {
                                (
                                    super::peek_tail::densified_body_line_count(sb, middle_w),
                                    super::peek_tail::scrollback_has_last_user(sb),
                                )
                            })
                            .unwrap_or((0, false));
                    super::layout::peek_live_tail_desired_content(
                        max_content,
                        reply_rows,
                        body_measured,
                        pin_user,
                    )
                    .content_rows
                };
                let alloc =
                    super::layout::allocate_peek(area.height, fixed, content_rows, peek_min);
                if alloc.show_peek {
                    state.set_peek_reply_target_cwd(peeked_agent_cwd(&sel, agents));
                    let badge = super::peek::peek_model_and_mode(&sel, agents);
                    match state.peek.as_mut() {
                        Some(p) => {
                            if p.apply_fields(sel, fields) {
                                state.clear_peek_reply();
                            }
                        }
                        None => state.set_peek(Some(super::peek::PeekPanelState::new(sel, fields))),
                    }
                    if let Some(p) = state.peek.as_mut() {
                        p.model_name = badge.model;
                        p.auto_approve = badge.yolo;
                        p.auto = badge.auto;
                        p.plan_mode = badge.plan;
                    }
                    layout = super::layout::compute_layout_with_peek_box(area, alloc.peek_box_h);
                } else {
                    state.set_peek_reply_target_cwd(None);
                    state.set_peek(None);
                }
            }
            None => {
                state.set_peek_reply_target_cwd(None);
                state.set_peek(None);
            }
        },
        None => {
            state.set_peek_reply_target_cwd(None);
            state.set_peek(None);
        }
    }

    ensure_peek_viewport_lifecycle(state, agents);

    if state.peek.is_none() && area.height > 8 && !state.dispatch.text().is_empty() {
        let rows = dispatch_text_rows(state, layout.dispatch.width, area.height);
        if rows > 1 {
            layout = super::layout::compute_layout_with_dispatch(area, false, rows);
        }
    }

    // Header.
    render_header(buf, layout.header, &theme, &rows, state, upgrade_cta);

    // Body: key off visible rows (local agents + roster), not the local map alone.
    if rows.is_empty() {
        if state.filter.is_active() {
            render_no_match(buf, layout.list, &theme, &state.filter);
        } else {
            render_empty_state(buf, layout.list, &theme, dashboard_sessions_loading);
        }
    } else if area.width < MIN_DASHBOARD_WIDTH {
        render_narrow_rows(buf, layout.list, &theme, &rows, state);
    } else {
        render_rows(buf, layout.list, &theme, &rows, state);
    }

    // Compute the contextual placeholder hint + footer mode once
    // here so both sub-renderers stay pure functions of the
    // selected-row state.
    let selected_state = state
        .selected
        .as_ref()
        .and_then(|sel| rows.iter().find(|r| r.id == *sel).map(|r| r.state));
    let peek_active = state.peek.is_some();

    // Peek REPLACES the dispatch input when active
    // (single rounded box at the same screen position, instead
    // of a separate panel floating above it). When peek is
    // closed, the dispatch input renders normally.
    let dispatch_cursor = if peek_active {
        // Peek has no in-box close button; `render_peek_panel` returns
        // the `❯ reply` caret position so the terminal cursor parks in
        // the live reply input, plus the reply row's rect (recorded for
        // click-to-focus / drag-selection mouse routing).
        state.peek_close_rect = None;
        // Split borrows: the panel is read from `state.peek` while the
        // reply widget (`state.peek_reply`) is drawn mutably — disjoint
        // fields, destructured so the borrow checker can see it.
        // Capture voice state before the disjoint destructure so the peek
        // reply can show the record badge + interim transcript (the peek box
        // replaces the dispatch box, which would otherwise own the overlay).
        let voice_listening = state.voice_listening;
        let voice_interim = state.voice_interim.clone();
        let multiline = state.multiline_mode;
        let peeked_row = state.peek.as_ref().map(|p| p.row.clone());
        let question_pending = state.peek.as_ref().is_some_and(|p| p.question.is_some());
        let (empty_hint, has_scrollback) = match peeked_row.as_ref() {
            Some(DashboardRowId::Subagent {
                parent,
                child_session_id,
            }) => {
                let parent_ok = agents
                    .get(parent)
                    .is_some_and(|p| p.subagent_sessions.contains_key(child_session_id));
                let loaded = agents
                    .get(parent)
                    .is_some_and(|p| p.subagent_views.contains_key(child_session_id));
                if parent_ok && !loaded {
                    (Some("Subagent not loaded"), false)
                } else {
                    (None, loaded)
                }
            }
            Some(row) => (
                None,
                super::state::scrollback_available_for_row(row, agents),
            ),
            None => (None, false),
        };
        let render = if let Some(panel) = state.peek.as_ref() {
            let live_tail = if !question_pending && has_scrollback {
                peeked_row
                    .as_ref()
                    .and_then(|row| super::state::scrollback_mut_for_row(row, agents))
                    .map(|scrollback| super::peek::PeekLiveTailArgs { scrollback })
            } else {
                None
            };
            super::peek::render_peek_panel(
                buf,
                layout.dispatch,
                panel,
                &mut state.peek_reply,
                &theme,
                voice_listening,
                voice_interim.as_deref(),
                multiline,
                Some(layout.list).filter(|r| r.area() > 0),
                live_tail,
                empty_hint,
            )
        } else {
            Default::default()
        };
        state.peek_reply_rect = render.reply_rect;
        let cursor = render.caret;
        // The reply is a full PromptWidget, so its `@` file-context
        // picker paints ABOVE the peek box (same chrome as the dispatch
        // box's). Slash completion stays inert for the reply.
        state.slash_dropdown_items_area = None;
        state.slash_dropdown_hit = Default::default();
        if state.peek_reply.file_search_visible() {
            state.file_search_dropdown_items_area = render_file_search_dropdown_for(
                buf,
                area,
                layout.dispatch,
                &theme,
                &mut state.peek_reply.file_search,
            );
        } else {
            state.file_search_dropdown_items_area = None;
        }
        // Peek replaces the input box — no input to click-to-focus.
        state.dispatch_rect = None;
        cursor
    } else {
        state.peek_close_rect = None;
        state.peek_reply_rect = None;
        // Record the box rect so a click anywhere on it focuses the
        // input (see `handle_mouse`).
        state.dispatch_rect = Some(layout.dispatch);
        let cursor = render_dispatch(
            buf,
            layout.dispatch,
            &theme,
            state,
            Some(layout.list).filter(|r| r.area() > 0),
        );
        // Completion dropdowns paint ABOVE the dispatch box. The `@`
        // file-search picker and the `/` slash dropdown never render
        // together — file search wins while the user is mid-`@token`,
        // otherwise the slash dropdown shows.
        if state.dispatch.file_search_visible() {
            render_file_search_dropdown(buf, area, layout.dispatch, &theme, state);
            state.slash_dropdown_items_area = None;
            state.slash_dropdown_hit = Default::default();
        } else {
            render_slash_dropdown(buf, area, layout.dispatch, &theme, state);
            state.file_search_dropdown_items_area = None;
        }
        cursor
    };

    // Footer.
    render_footer(
        buf,
        layout.footer,
        &theme,
        state,
        registry,
        selected_state,
        peek_active,
        pending_hint,
    );

    // Cheatsheet modal paints LAST so it overlays everything — the
    // row list, the dispatch widget, the footer hints. Modal lives
    // on `DashboardState` (mirrors `agent_view`'s `active_modal`);
    // when None, nothing to paint and the regular cursor logic
    // below proceeds. When Some, we suppress the dispatch cursor
    // because input is routed to the modal until it closes.
    if let Some(modal) = state.shortcuts_modal.as_mut() {
        crate::views::shortcuts_help::render_modal(
            buf,
            area,
            &modal.entries,
            &mut modal.state,
            &mut modal.window,
            modal.filter_active,
            &modal.collapsed_sections,
            &modal.expanded_ids,
            &modal.mode,
            &theme,
            /* compact */ false,
        );
        return None;
    }

    // The location picker overlays everything too (mutually exclusive
    // with the shortcuts modal in practice). When open, input is routed
    // to it, so the dispatch cursor is suppressed.
    if let Some(modal) = state.location_picker.as_mut() {
        render_location_picker(buf, area, &theme, modal);
        return None;
    }

    // The worktree-label dialog overlays the dashboard while the user names
    // the worktree for a dashboard-dispatched agent. Input is routed to it,
    // so the dispatch cursor is suppressed.
    if let Some(dialog) = state.worktree_dialog.as_ref() {
        crate::views::new_worktree_dialog::render_new_worktree_dialog(area, buf, dialog);
        return None;
    }

    // An active rename replaces the dispatch caret with its row-local editor caret.
    if let Some(pos) = rename_cursor_pos(state, &rows) {
        return Some(pos);
    }
    dispatch_cursor
}

const RENAME_PREFIX: &str = "rename: ";

fn rename_editor_view(draft: &RenameDraft, width: u16) -> (&str, u16) {
    let prefix_width = UnicodeWidthStr::width(RENAME_PREFIX) as u16;
    let editor_width = width.saturating_sub(prefix_width);
    let viewport = draft.viewport(editor_width as usize);
    let visible = &draft.text()[viewport.visible_byte_range];
    let cursor_offset = prefix_width
        .saturating_add(viewport.cursor_display_column as u16)
        .min(width.saturating_sub(1));
    (visible, cursor_offset)
}

fn render_rename_editor(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    style: Style,
    draft: &RenameDraft,
) {
    if width == 0 {
        return;
    }
    let prefix_width = UnicodeWidthStr::width(RENAME_PREFIX) as u16;
    buf.set_span(
        x,
        y,
        &Span::styled(RENAME_PREFIX, style),
        prefix_width.min(width),
    );
    let (visible, _) = rename_editor_view(draft, width);
    if !visible.is_empty() && prefix_width < width {
        buf.set_span(
            x + prefix_width,
            y,
            &Span::styled(visible, style),
            width - prefix_width,
        );
    }
}

/// Return the in-flight rename caret when its row is visible.
fn rename_cursor_pos(state: &DashboardState, rows: &[DashboardRow]) -> Option<(u16, u16)> {
    let rn = state.rename.as_ref()?;
    let (_, rect) = state.row_rects.iter().find(|(id, _)| *id == rn.row)?;
    let row = rows.iter().find(|r| r.id == rn.row);
    let (marker_width, indent_width, icon_width) = row
        .map(|r| {
            (
                UnicodeWidthStr::width(crate::glyphs::selection_bar()) as u16,
                (r.indent as u16) * 2,
                UnicodeWidthStr::width(state_icon(r.state, state.spinner_tick)) as u16,
            )
        })
        .unwrap_or((1, 0, 1));
    let chrome_width = marker_width + 1 + indent_width + icon_width + 1;
    let content_x = rect.x.saturating_add(chrome_width);
    let content_width = rect.x.saturating_add(rect.width).saturating_sub(content_x);
    let (_, cursor_offset) = rename_editor_view(rn, content_width);
    let cursor_x = content_x
        .saturating_add(cursor_offset)
        .min(rect.x.saturating_add(rect.width.saturating_sub(1)));
    // Mirror `render_row`'s vertical centering so the caret lands on
    // the title line (narrow-mode single-line rects yield offset 0).
    let title_y = rect.y + row.map_or(0, |r| row_content_offset(rect.height, r));
    Some((cursor_x, title_y))
}

/// Render the compact dashboard "banner" used when an agent is
/// attached as a popup. Replaces the previous
/// stacked-input-bars layout (where the dashboard's dispatch +
/// footer rendered visibly BELOW the popup). The banner is a
/// bordered panel containing the row list summary; the popup
/// renders directly below it carrying the focused agent's full
/// view (scrollback + prompt + shortcuts), so the user sees a
/// single coherent surface with the agent's prompt as the only
/// input bar.
///
/// Visual:
///
/// ```text
/// ╭──── Dashboard · 3 agents · 1 working ─────────────────────╮
/// │ Working (1)                                                │
/// │   ⸬ session A      Responding                       4.4s   │
/// │ Idle (2)                                                   │
/// │   ○ session B                                       1m12s  │
/// │   ○ session C                                       2m30s  │
/// ╰────────────────────────────────────────────────────────────╯
/// ```
///
/// The border + title are drawn via ratatui's `Block` widget so the
/// visual chrome matches other bordered panels (subagent fullscreen,
/// popup overlays). Rows are clipped to fit inside the bordered
/// area; the user can see additional rows by closing the popup
/// (Esc) to reach the full dashboard.
fn render_dashboard_banner(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    rows: &[DashboardRow],
    state: &mut DashboardState,
) {
    use ratatui::text::Line;
    use ratatui::widgets::{Block, Borders, Widget};

    state.row_rects.clear();
    state.row_delete_rects.clear();
    state.section_rects.clear();
    if area.area() == 0 || area.height < 3 {
        return;
    }

    // Build the title chip: `Dashboard · N agents · M working`.
    let mut total = 0usize;
    let mut working = 0usize;
    let mut needs_input = 0usize;
    for r in rows.iter().filter(|r| r.indent == 0) {
        total += 1;
        if r.state == RowState::Working {
            working += 1;
        }
        if r.state == RowState::NeedsInput {
            needs_input += 1;
        }
    }
    let agent_word = if total == 1 { "agent" } else { "agents" };
    let mut title_parts: Vec<String> = vec!["Dashboard".to_string()];
    title_parts.push(format!("{total} {agent_word}"));
    if working > 0 {
        title_parts.push(format!("{working} working"));
    }
    if needs_input > 0 {
        title_parts.push(format!("{needs_input} awaiting"));
    }
    let title = format!(" {} ", title_parts.join(" · "));

    // Draw the bordered frame with the title centred on the top edge.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(theme.selection_border)
                .bg(theme.bg_base),
        )
        .title(
            Line::from(title).style(
                Style::default()
                    .fg(theme.text_primary)
                    .bg(theme.bg_base)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    let inner = block.inner(area);
    block.render(area, buf);

    // Render rows inside the bordered area. Clip to the inner height
    // so we never overrun the border.
    if inner.area() == 0 {
        return;
    }
    if rows.is_empty() {
        let hint = " No sessions yet. Esc to dispatch one. ";
        let trunc = truncate_str(hint, inner.width as usize);
        buf.set_string(
            inner.x,
            inner.y,
            trunc,
            Style::default().fg(theme.gray_dim).bg(theme.bg_base),
        );
        return;
    }

    if inner.width < MIN_DASHBOARD_WIDTH {
        render_narrow_rows(buf, inner, theme, rows, state);
    } else {
        render_rows(buf, inner, theme, rows, state);
    }
}

/// Render the dashboard header row:
///
/// ```text
///   main worktree ~/wt/wt1 (worktree of ~/proj)   ◆ 2 awaiting · ⋮ 3 working · ◇ 1 idle │ [+ New Agent]
/// ```
///
/// Left: the current location — git branch + worktree badge + cwd, the
/// same line the welcome top bar paints (see
/// `views::welcome::location_line`), rendered the same way as the session
/// status bar — so the dashboard shows where a dispatched session will
/// run. Truncated with `…` against the chips.
/// Right: state-count chips with state-coloured diamond/spinner glyphs
/// (matching the row icon vocabulary so the same glyph that marks a
/// working row also marks the "working" chip). Counts are top-level
/// rows only — subagents inherit their parent's group and would
/// inflate the tallies if counted directly. Chips with zero count are
/// suppressed.
///
/// Reintroduces a `[+ New Agent]` button on the right edge of
/// the header. The button is the default cursor target whenever
/// no row is selected — Up-arrow from the first row, Esc deselect,
/// and dashboard-open-without-prior-agent all land here. While
/// focused, Enter on an empty prompt creates a session and opens
/// detail view; click does the same. The focused colour bumps to
/// the green `accent_success` (mirroring the affirmative "create"
/// affordance) so the cursor's location is obvious without a
/// separate marker.
fn render_header(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    rows: &[DashboardRow],
    state: &mut DashboardState,
    upgrade_cta: Option<HeaderUpgradeCta<'_>>,
) {
    use ratatui::text::{Line, Span};

    use crate::views::agent_status::AgentStatusBar;

    // Clear the click rects at the start of every frame; the paint
    // blocks below repopulate them when the header has room. `set(None)`
    // preserves the `hovered` flag (driven by mouse-move events) so it
    // survives the per-frame rect reset.
    state.new_agent_button_hit.set(None);
    state.location_hit.set(None);
    state.upgrade_cta_hit.set(None);

    if area.area() == 0 {
        return;
    }
    buf.set_style(area, Style::default().bg(theme.bg_base));

    // Paint the `[+ New Agent]` button on the far right FIRST,
    // before the status chips. We then shrink the chip rendering
    // area so the chips don't overlap the button. Right-margin
    // is 1 cell so the button doesn't kiss the panel edge.
    // Worktree mode armed (and the cwd is a git repo, so it can actually
    // take effect) → the button creates the next session in a fresh git
    // worktree, so label it `[+ New Worktree]` to match. Otherwise it's the
    // plain new-session button. Width is derived from the chosen label so the
    // positioning + hit rect below stay correct for either string.
    let button_label: &str = if state.dispatch_worktree && state.cwd_has_git_ancestor {
        "[+ New Worktree]"
    } else {
        "[+ New Agent]"
    };
    let button_w = UnicodeWidthStr::width(button_label) as u16;
    let right_margin: u16 = 1;
    let mut chip_area = area;
    if area.width >= button_w + right_margin {
        let button_x = area.x + area.width - right_margin - button_w;
        // Focused → light green `accent_success` (the affirmative
        // "create a new session" affordance), so the focus is obvious.
        // Hovered (mouse over, not focused) → brighter `text_primary`
        // foreground so the button stands out under the cursor. Only
        // the text colour changes on hover (no background fill).
        // Otherwise dim gray.
        let hovered = state.new_agent_button_hit.hovered;
        let fg = if state.new_agent_button_focused {
            theme.accent_success
        } else if hovered {
            theme.text_primary
        } else {
            theme.gray
        };
        let style = Style::default()
            .fg(fg)
            .bg(theme.bg_base)
            .add_modifier(Modifier::BOLD);
        buf.set_string(button_x, area.y, button_label, style);
        state.new_agent_button_hit.set(Some(Rect {
            x: button_x,
            y: area.y,
            width: button_w,
            height: 1,
        }));
        // Leave one breathing cell between the rightmost chip and
        // the button's `[`.
        let chip_budget = area.width.saturating_sub(button_w + right_margin + 1);
        chip_area = Rect {
            x: area.x,
            y: area.y,
            width: chip_budget,
            height: area.height,
        };
    }

    // Count top-level rows per state. Subagents inherit their
    // parent's group so we explicitly skip `indent > 0` rows.
    let mut awaiting = 0usize;
    let mut working = 0usize;
    let mut idle = 0usize;
    let mut done = 0usize;
    let mut failed = 0usize;
    let mut blocked = 0usize;
    for r in rows.iter().filter(|r| r.indent == 0) {
        match r.state {
            RowState::NeedsInput => awaiting += 1,
            RowState::Working => working += 1,
            RowState::Idle => idle += 1,
            // Inactive (roster-only) sessions get no header chip — the
            // chips surface actionable local state; the section header
            // already carries the inactive count.
            RowState::Inactive => {}
            RowState::Completed => done += 1,
            RowState::Failed => failed += 1,
            RowState::Blocked => blocked += 1,
        }
    }

    // Build right-aligned chips. Order mirrors `RowState::group_priority`
    // so the most-actionable state (awaiting your input) appears
    // leftmost in the chip group, where the eye lands first.
    let mut status = AgentStatusBar::new(theme);
    let chip = |glyph: &str, color: Color, count: usize, label: &'static str| {
        Line::from(vec![
            Span::styled(
                glyph.to_string(),
                Style::default().fg(color).bg(theme.bg_base),
            ),
            Span::styled(
                format!(" {count} {label}"),
                Style::default().fg(theme.gray).bg(theme.bg_base),
            ),
        ])
    };
    if awaiting > 0 {
        // Yellow bullet to match the per-row awaiting bullet (no blink in the
        // compact header chip).
        status.push(
            "awaiting",
            chip(
                crate::glyphs::diamond_filled(),
                theme.warning,
                awaiting,
                "awaiting",
            ),
        );
    }
    if working > 0 {
        // Use the spinner glyph at the current tick so the chip's
        // working glyph matches the spinner painted on individual
        // rows (visual consistency).
        let frames = crate::glyphs::dot_spinner_frames();
        let spin = frames[(state.spinner_tick / SPINNER_DIVISOR) as usize % frames.len()];
        status.push(
            "working",
            chip(spin, theme.accent_running, working, "working"),
        );
    }
    if blocked > 0 {
        status.push(
            "blocked",
            chip(
                crate::glyphs::diamond_filled(),
                theme.warning,
                blocked,
                "blocked",
            ),
        );
    }
    if idle > 0 {
        status.push(
            "idle",
            chip(
                crate::glyphs::diamond_hollow(),
                theme.gray_dim,
                idle,
                "idle",
            ),
        );
    }
    if done > 0 {
        status.push(
            "done",
            chip(
                crate::glyphs::diamond_filled(),
                theme.accent_success,
                done,
                "done",
            ),
        );
    }
    if failed > 0 {
        status.push(
            "failed",
            chip(
                crate::glyphs::diamond_filled(),
                theme.accent_error,
                failed,
                "failed",
            ),
        );
    }
    // Chips render right-aligned within `chip_area` so they sit
    // immediately to the left of the `[+ New Agent]` button. Capture
    // the per-chip rects so the left label's width budget stops short
    // of the leftmost chip instead of painting over it.
    let chip_rects = status.render(buf, chip_area);

    // Paint the current location — git branch + cwd (with worktree
    // label) — on the left, mirroring the session surfaces (welcome
    // top bar / agent status bar) so the dashboard shows WHERE a
    // dispatched session will run. Replaces the old bare "Agents"
    // label.
    //
    // Width budget: from `area.x` up to the leftmost chip's leading
    // ` │ ` separator (3 cells, painted by `AgentStatusBar::render`
    // before its first item), or the full chip area when no chips
    // rendered. `truncate_line` appends `…` when the path is cut.
    let full_label_budget = chip_rects
        .values()
        .map(|r| r.x)
        .min()
        .map(|min_x| min_x.saturating_sub(3).saturating_sub(area.x))
        .unwrap_or(chip_area.width) as usize;
    // Reserve the upgrade CTA (lead space + `[label]` + pinned-only `cta.caption`)
    // so the location label truncates first (reservation-first); the shared
    // painter then clamps to the space left, so it can't overpaint chips.
    // Caption gates on pinned only: the Ctrl+O CTA chord is handled before the peek-permission key handler, so it opens the CTA (not YOLO) even while a peek prompt is pending.
    let upgrade_caption = upgrade_cta.and_then(|cta| cta.pinned.then_some(cta.caption).flatten());
    let upgrade_reserve = upgrade_cta.map_or(0usize, |cta| {
        1 + crate::views::announcements::upgrade_cta_reserve(cta.label, upgrade_caption) as usize
    });
    let label_budget = full_label_budget.saturating_sub(upgrade_reserve);
    let mut location = crate::views::welcome::location_line_at(theme, &state.cwd);
    // 1-cell left inset, matching the old ` Agents` label.
    location
        .spans
        .insert(0, Span::styled(" ", Style::default().bg(theme.bg_base)));
    let mut location = truncate_line(location, label_budget);
    // Underline on hover so the label reads as a click target (opens the
    // location picker). Underline only visible text: whitespace-only spans
    // (the leading inset, the git↔path separator) stay bare, and the git
    // span (`{icon} {branch}`) keeps its branch icon + the space after it
    // un-underlined — only the branch name is underlined. Hover is
    // mouse-driven on the prior frame.
    if state.location_hit.hovered {
        let icon = crate::git_info::branch_icon();
        location.spans = underline_location_on_hover(std::mem::take(&mut location.spans), icon);
    }
    let location_w = location.width() as u16;
    buf.set_line(area.x, area.y, &location, location_w);

    // Record the painted label as a click target so the mouse handler
    // can open the location picker. Width is clamped to the label budget
    // so the hit area never extends under the chips / `[+ New Agent]`.
    let hit_w = location_w.min(label_budget as u16);
    if hit_w > 0 {
        state.location_hit.set(Some(Rect {
            x: area.x,
            y: area.y,
            width: hit_w,
            height: 1,
        }));
    }

    // Upgrade CTA painted right after the location label (free-tier upsell),
    // clamped to the space left before the chips — a lead space then the shared
    // clamping button painter. Pointer click → Dashboard, Ctrl+O → Keyboard.
    if let Some(HeaderUpgradeCta { label, .. }) = upgrade_cta {
        let avail = full_label_budget.saturating_sub(location_w as usize);
        if avail > 1 {
            let cta_x = area.x + location_w;
            buf.set_span(
                cta_x,
                area.y,
                &Span::styled(" ", Style::default().bg(theme.bg_base)),
                1,
            );
            let painted = crate::views::announcements::render_cta_button(
                buf,
                theme,
                cta_x + 1,
                area.y,
                (avail - 1) as u16,
                label,
                upgrade_caption,
                state.upgrade_cta_hit.hovered,
            );
            state.upgrade_cta_hit.set(painted);
        }
    }
}

/// Apply the header location label's hover underline: underline only the
/// visible text. Whitespace-only spans (the leading inset, the git↔path
/// separator) stay bare, and the git span (`{icon} {branch}`) keeps the
/// branch `icon` plus the space after it un-underlined — only the branch
/// name is underlined. Total width is preserved (the git span is split,
/// not resized).
fn underline_location_on_hover(spans: Vec<Span<'static>>, icon: &str) -> Vec<Span<'static>> {
    let underline = |content: &str, style: Style| -> Style {
        if content.chars().any(|c| !c.is_whitespace()) {
            style.add_modifier(Modifier::UNDERLINED)
        } else {
            style
        }
    };
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 1);
    for span in spans {
        let style = span.style;
        let content = span.content.into_owned();
        // The git span is `{icon} {branch}`: emit the icon + its trailing
        // space bare, and underline only the branch name.
        if content.starts_with(icon)
            && let Some(space) = content.find(' ')
        {
            let (icon_part, branch) = content.split_at(space + 1);
            out.push(Span::styled(icon_part.to_owned(), style));
            if !branch.is_empty() {
                let s = underline(branch, style);
                out.push(Span::styled(branch.to_owned(), s));
            }
            continue;
        }
        let s = underline(&content, style);
        out.push(Span::styled(content, s));
    }
    out
}

/// Render the location picker modal over the dashboard. Content-row hit
/// areas from the render are stashed on the modal for the mouse handler.
fn render_location_picker(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    modal: &mut LocationPickerState,
) {
    use crate::views::modal_window::{
        ModalSizing, ModalWindowConfig, Shortcut, push_vim_nav_search_hint, render_modal_window,
    };
    use crate::views::picker::{
        PickerEntry, PickerRow, render_divider, render_picker_content,
        render_picker_search_bar_with_label,
    };

    let mut shortcuts = vec![
        Shortcut {
            label: "\u{2191}\u{2193} nav",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Tab complete",
            clickable: false,
            id: 1,
        },
        Shortcut {
            label: "Enter select",
            clickable: false,
            id: 2,
        },
        Shortcut {
            label: "Esc close",
            clickable: false,
            id: 3,
        },
    ];
    // Surface `i search` in the footer when vim nav mode is active (input-default
    // picker, but Esc drops to nav under vim).
    push_vim_nav_search_hint(&mut shortcuts, modal.picker.search_active);
    let config = ModalWindowConfig {
        title: "Change directory",
        tabs: None,
        shortcuts: &shortcuts,
        sizing: ModalSizing::medium(),
        fold_info: None,
    };
    let Some(content) = render_modal_window(buf, area, &mut modal.window, &config, theme) else {
        modal.content_hits = None;
        return;
    };

    let mut content_area = content.content;

    // The effective candidate list — computed once and reused for both the
    // worktree-eligibility check and the rows below.
    let visible = modal.visible_candidates();
    // Worktrees require a git repo. The directory a selection would land in
    // (the highlighted row, else the base cwd) decides whether the worktree
    // toggle is meaningful; in a non-repo it's hidden and dispatch proceeds
    // normally.
    let target_dir = visible
        .get(modal.picker.selected)
        .map(|c| c.path.clone())
        .unwrap_or_else(|| modal.base_cwd.clone());
    let show_worktree = modal.target_is_repo(&target_dir);

    // Path input line — a visible, editable field (cursor always shown)
    // so the user can see / type an absolute, `~`, or relative path.
    // Doubles as the live filter for the candidate list below.
    modal.worktree_hit.set(None);
    if content_area.height >= 2 {
        // Reserve room at the right end of the path row for the worktree
        // toggle button, but only when the modal is wide enough to keep a
        // usable path field; otherwise the field spans the full width and
        // the button is hidden.
        let wt_text = if modal.worktree_mode {
            "[worktree:on]"
        } else {
            "[worktree:off]"
        };
        let wt_w = wt_text.len() as u16; // ASCII → byte len == display width
        const WT_GAP: u16 = 1;
        const MIN_PATH_W: u16 = 16;
        let (path_w, wt_rect) = if show_worktree && content_area.width >= wt_w + WT_GAP + MIN_PATH_W
        {
            (
                content_area.width - wt_w - WT_GAP,
                Some(Rect {
                    x: content_area.x + content_area.width - wt_w,
                    y: content_area.y,
                    width: wt_w,
                    height: 1,
                }),
            )
        } else {
            (content_area.width, None)
        };
        render_picker_search_bar_with_label(
            buf,
            content_area.x,
            content_area.y,
            path_w,
            theme,
            " path: ",
            &modal.picker,
            /* active */ false,
            /* show_hint */ false,
            Some(theme.bg_base),
        );
        modal.worktree_hit.set(wt_rect);
        if let Some(r) = wt_rect {
            // Dim label by default; brighten the text on hover, like other
            // clickable buttons. When armed, the "on" word is green in either
            // state so the active state reads at a glance.
            let label_fg = if modal.worktree_hit.hovered {
                theme.text_primary
            } else {
                theme.gray
            };
            buf.set_string(
                r.x,
                r.y,
                wt_text,
                Style::default().fg(label_fg).bg(theme.bg_base),
            );
            if modal.worktree_mode
                && let Some(on_at) = wt_text.find("on")
            {
                buf.set_string(
                    r.x + on_at as u16,
                    r.y,
                    "on",
                    Style::default().fg(theme.accent_success).bg(theme.bg_base),
                );
            }
        }
        // Second header row: the inline error (red) when present, else a
        // divider separating the input from the list.
        if let Some(err) = modal.error.as_deref() {
            let err_line = truncate_str(err, content_area.width as usize);
            buf.set_string(
                content_area.x,
                content_area.y + 1,
                &err_line,
                Style::default().fg(theme.accent_error).bg(theme.bg_base),
            );
        } else {
            render_divider(
                buf,
                content_area.x,
                content_area.y + 1,
                content_area.width,
                theme,
                Some(theme.bg_base),
            );
        }
        content_area.y += 2;
        content_area.height = content_area.height.saturating_sub(2);
    }

    // Build entries from the effective list (`visible`, computed above) —
    // recents (fuzzy-filtered) or live directory suggestions
    // (prefix-matched), depending on the query.
    // Worktree badge per row: just `worktree` when the directory name
    // already is the worktree name, else `worktree: <name>`. Built into a
    // parallel Vec so the `&str` badges outlive the entries.
    let badges: Vec<String> = visible
        .iter()
        .map(|c| match &c.worktree {
            Some(name) if name == &c.label => "worktree".to_string(),
            Some(name) => format!("worktree: {name}"),
            None => String::new(),
        })
        .collect();
    // Truncation priority: the directory name (label) is shown in full
    // whenever it fits; the path (right label) is truncated first. The
    // shared `render_picker_row` does the opposite (right label keeps its
    // full width, label truncates), so we pre-truncate the path here to
    // leave room for the full label + badge. Constants mirror
    // `render_picker_row`'s layout (fold prefix 2, gap 2, trailing 1); the
    // `-1` conservatively reserves a scrollbar column.
    let details: Vec<String> = {
        const PREFIX: u16 = 2;
        const GAP: u16 = 2;
        const TRAILING: u16 = 1;
        let row_w = content_area.width.saturating_sub(1);
        visible
            .iter()
            .zip(&badges)
            .map(|(c, badge)| {
                let badge_w = if badge.is_empty() {
                    0
                } else {
                    badge.width() as u16 + 1
                };
                let reserved = PREFIX + c.label.width() as u16 + badge_w + GAP + TRAILING;
                let budget = row_w.saturating_sub(reserved) as usize;
                truncate_str(&c.detail, budget)
            })
            .collect()
    };
    let entries: Vec<PickerEntry<'_>> = visible
        .iter()
        .zip(&badges)
        .zip(&details)
        .enumerate()
        .map(|(vis, ((c, badge), detail))| {
            PickerEntry::Row(PickerRow {
                label: c.label.as_str(),
                right_label: detail.as_str(),
                selected: vis == modal.picker.selected,
                expanded: false,
                fields: &[],
                description_lines: &[],
                summary_lines: &[],
                dimmed: false,
                indent: 0,
                badge: badge.as_str(),
                badge_color: (!badge.is_empty()).then_some(theme.accent_user),
                collapsible: false,
                underline_last_desc: false,
            })
        })
        .collect();

    let hits = render_picker_content(
        buf,
        content_area,
        theme,
        &mut modal.picker,
        &entries,
        /* non_selectable */ &[],
        /* non_selectable_clickable */ &[],
        Some(theme.bg_base),
        /* loading */ false,
    );
    modal.content_hits = Some(hits);
}

/// One line in the dashboard's vertical stack — either a state-group
/// header or a content row.
///
/// Reintroduces explicit state group headers (`──
/// Needs input (2) ──`). The per-row dot + state colour alone didn't
/// communicate group boundaries clearly enough; users couldn't tell at
/// a glance how many sessions were awaiting input vs working vs idle
/// vs done. Headers are emitted on every top-level state transition
/// (subagent rows inherit their parent's group and never trigger a
/// header) when `grouping == Grouping::State` and the filter isn't
/// already pinned to a single state.
///
/// Headers do NOT register `row_rects` — they're not selectable,
/// hoverable, or clickable.
enum DashboardLine<'a> {
    /// Cross-cutting "Pinned" section header (with count), emitted above the
    /// pinned block when grouping is ON.
    PinnedHeader {
        count: usize,
    },
    /// Textless horizontal rule. Used when grouping is OFF to separate the
    /// pinned block from the rest without a labelled header.
    Divider,
    Header {
        state: RowState,
        count: usize,
    },
    Row(&'a DashboardRow),
    /// The Idle group's "N more" overflow toggle row,
    /// emitted at the bottom of a capped Idle group. `hidden` is the
    /// number of folded agents; `expanded` reflects
    /// [`super::state::DashboardState::idle_show_all`] so the label can
    /// flip to "show fewer".
    IdleOverflow {
        hidden: usize,
        expanded: bool,
    },
}

/// Walk `rows` and intersperse `DashboardLine::Header` entries at
/// every top-level state transition. Returns a flat sequence that the
/// renderer iterates over directly; viewport clamping operates on
/// THIS sequence (not on `rows`) so the header rows count toward the
/// visible window's height budget.
///
/// Headers are suppressed when:
///   - `grouping == Grouping::Directory` — the cwd is the grouping
///     primitive in that mode and state headers would land
///     out-of-band relative to the cwd sections.
///   - `filter == Filter::State(_)` — the filtered view already only
///     contains a single state, so the header is redundant chrome.
fn build_dashboard_lines<'a>(
    rows: &'a [DashboardRow],
    grouping: Grouping,
    filter: &Filter,
    collapsed: &std::collections::HashSet<SectionKey>,
    idle_show_all: bool,
    search_active: bool,
) -> Vec<DashboardLine<'a>> {
    let groups_on = matches!(grouping, Grouping::State);
    let emit_state_headers = groups_on && !matches!(filter, Filter::State(_));

    // Pinned top-level agents are sorted to the front (see `sort_rows`), so
    // they form a contiguous prefix of clusters. Split that prefix off as a
    // dedicated "Pinned" section above the state / directory groups, so a
    // pinned (say) idle agent reads as pinned rather than landing under an
    // "Idle" header.
    let mut pinned_end = 0usize;
    let mut pinned_count = 0usize;
    {
        let mut i = 0usize;
        while i < rows.len() && rows[i].indent == 0 && rows[i].pinned {
            pinned_count += 1;
            i += 1;
            // Glue the pinned parent's subagents into the section.
            while i < rows.len() && rows[i].indent != 0 {
                i += 1;
            }
            pinned_end = i;
        }
    }

    let mut out: Vec<DashboardLine<'a>> = Vec::with_capacity(rows.len() + 6);
    if pinned_count > 0 {
        // Grouping ON → a labelled "Pinned N" header above the block.
        // Grouping OFF (Ctrl+G) → no header; a textless divider separates the
        // pinned block from the rest (only when there's a rest to separate).
        if groups_on {
            out.push(DashboardLine::PinnedHeader {
                count: pinned_count,
            });
        }
        // A collapsed "Pinned" section keeps its header but hides the
        // pinned rows. Collapse only applies when grouping is ON (the
        // header is the toggle affordance; the grouping-OFF divider has
        // none).
        let pinned_collapsed = groups_on && collapsed.contains(&SectionKey::Pinned);
        if !pinned_collapsed {
            out.extend(rows[..pinned_end].iter().map(DashboardLine::Row));
        }
        if !groups_on && pinned_end < rows.len() {
            out.push(DashboardLine::Divider);
        }
    }

    let rest = &rows[pinned_end..];
    if !emit_state_headers {
        out.extend(rest.iter().map(DashboardLine::Row));
        return out;
    }
    let mut last_top_state: Option<RowState> = None;
    // Whether the section currently being emitted is collapsed; when so
    // its rows (and their subagents) are skipped but the header stays.
    let mut current_collapsed = false;
    // Idle-overflow cap bookkeeping for the group currently being emitted.
    // `idle_limit` is `Some(visible_top_level_limit)` only while inside a
    // capped Idle group; `idle_top_seen` counts emitted top-level Idle
    // rows; `idle_capping` latches once the limit is passed so the
    // over-cap rows AND their subagents are skipped. `pending_overflow`
    // holds the `(hidden, expanded)` line to emit at the group's end
    // (after its rows, before the next header). Capping is suppressed
    // whenever the user is filtering OR in search mode — when you're
    // looking for something, every match shows. `search_active` is needed
    // in addition to the filter check because entering search mode clears
    // the filter to `None` (the live query rebuilds it per keystroke), so
    // an empty search query would otherwise leave old idle agents folded.
    let idle_cap_active = matches!(filter, Filter::None) && !search_active;
    let now = std::time::SystemTime::now();
    let mut idle_limit: Option<usize> = None;
    let mut idle_top_seen = 0usize;
    let mut idle_capping = false;
    let mut pending_overflow: Option<(usize, bool)> = None;
    for (i, row) in rest.iter().enumerate() {
        if row.indent == 0 && Some(row.state) != last_top_state {
            // Emit the overflow row of the group we're leaving before the
            // new header, so it lands at the bottom of the Idle group.
            if let Some((hidden, expanded)) = pending_overflow.take() {
                out.push(DashboardLine::IdleOverflow { hidden, expanded });
            }
            // Count consecutive top-level rows of this state from `i`
            // (looking forward until the next top-level state change
            // or end of list). Subagents are skipped over rather than
            // breaking the count, since they share their parent's
            // group. The count reflects the true group size even when
            // collapsed or capped. `recent` tracks how many are inside
            // the freshness window (Idle only).
            let mut count = 0usize;
            let mut recent = 0usize;
            for r in &rest[i..] {
                if r.indent != 0 {
                    continue;
                }
                if r.state == row.state {
                    count += 1;
                    if idle_row_is_recent(r, now) {
                        recent += 1;
                    }
                } else {
                    break;
                }
            }
            out.push(DashboardLine::Header {
                state: row.state,
                count,
            });
            last_top_state = Some(row.state);
            current_collapsed = collapsed.contains(&SectionKey::State(row.state));
            // Reset / arm the Idle cap for the new group.
            idle_limit = None;
            idle_top_seen = 0;
            idle_capping = false;
            if row.state == RowState::Idle && idle_cap_active && !current_collapsed {
                // Keep the freshest agents: at least MAX_VISIBLE_IDLE,
                // extended to cover everything still inside the freshness
                // window. Only fold when it hides >= MIN_IDLE_FOLD (a
                // single folded row saves no space).
                let base_limit = MAX_VISIBLE_IDLE.max(recent).min(count);
                let base_hidden = count - base_limit;
                if base_hidden >= MIN_IDLE_FOLD {
                    idle_limit = Some(if idle_show_all { count } else { base_limit });
                    pending_overflow = Some((base_hidden, idle_show_all));
                }
            }
        }
        if current_collapsed {
            continue;
        }
        // Idle cap: once past the limit, skip over-cap top-level rows and
        // their subagents (idle_capping latches until the next group).
        if let Some(limit) = idle_limit {
            if row.indent == 0 {
                idle_top_seen += 1;
                idle_capping = idle_top_seen > limit;
            }
            if idle_capping {
                continue;
            }
        }
        out.push(DashboardLine::Row(row));
    }
    if let Some((hidden, expanded)) = pending_overflow.take() {
        out.push(DashboardLine::IdleOverflow { hidden, expanded });
    }
    out
}

/// Maximum number of top-level Idle agents shown before the rest fold
/// into the "N more" overflow row. The Idle group is
/// sorted most-recent-first, so the folded tail is always the oldest.
pub const MAX_VISIBLE_IDLE: usize = 8;

/// Idle agents last active within this window are never folded, even
/// beyond [`MAX_VISIBLE_IDLE`] — a burst of fresh sessions stays visible
/// (the count cap only hides genuinely *old* idle agents).
const IDLE_FRESHNESS: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Don't fold fewer than this many rows: a "1 older" overflow row costs
/// the same vertical space as the single row it would hide.
const MIN_IDLE_FOLD: usize = 2;

/// Whether an Idle `row` was last active within [`IDLE_FRESHNESS`] of
/// `now`. Future timestamps (clock skew across pager processes / roster)
/// count as recent.
fn idle_row_is_recent(row: &DashboardRow, now: std::time::SystemTime) -> bool {
    now.duration_since(row.last_change_at)
        .map(|age| age < IDLE_FRESHNESS)
        .unwrap_or(true)
}

/// Ordered, display-order list of keyboard cursor targets — section
/// headers and visible rows — derived from the same
/// [`build_dashboard_lines`] the renderer paints, so navigation and
/// rendering never disagree about what's on screen. Collapsed sections
/// contribute only their header (their rows are skipped by the line
/// builder), and `… N more` placeholders are excluded (not selectable).
pub(crate) fn focusables(
    rows: &[DashboardRow],
    grouping: Grouping,
    filter: &Filter,
    collapsed: &std::collections::HashSet<SectionKey>,
    idle_show_all: bool,
    search_active: bool,
) -> Vec<Focusable> {
    build_dashboard_lines(
        rows,
        grouping,
        filter,
        collapsed,
        idle_show_all,
        search_active,
    )
    .into_iter()
    .filter_map(|line| match line {
        DashboardLine::PinnedHeader { .. } => Some(Focusable::Section(SectionKey::Pinned)),
        DashboardLine::Header { state, .. } => Some(Focusable::Section(SectionKey::State(state))),
        DashboardLine::Row(row) if !row.is_more_placeholder => Some(Focusable::Row(row.id.clone())),
        DashboardLine::IdleOverflow { .. } => Some(Focusable::IdleOverflow),
        _ => None,
    })
    .collect()
}

/// The section header that owns `row_id` under the current grouping /
/// filter, or `None` when no headers are emitted (directory grouping,
/// `s:state` filter) or the row isn't present in `rows` at all. Walks
/// the same [`build_dashboard_lines`] the renderer paints but with an
/// EMPTY collapsed set, so a row hidden inside a collapsed section
/// still resolves to its owning header — that's the point: it lets
/// `reanchor_selection` move a cursor stranded on a collapse-hidden
/// row onto the header that hid it. Subagent rows resolve to their
/// parent's section (they're emitted under the parent's header).
pub(crate) fn section_of_row(
    rows: &[DashboardRow],
    grouping: Grouping,
    filter: &Filter,
    row_id: &super::DashboardRowId,
) -> Option<SectionKey> {
    let none_collapsed = std::collections::HashSet::new();
    let mut current: Option<SectionKey> = None;
    // `idle_show_all = true` disables the Idle cap here, mirroring the
    // empty collapsed set: a row hidden by the cap must still resolve to
    // its Idle header so `reanchor_selection` can move a stranded cursor.
    for line in build_dashboard_lines(rows, grouping, filter, &none_collapsed, true, false) {
        match line {
            DashboardLine::PinnedHeader { .. } => current = Some(SectionKey::Pinned),
            DashboardLine::Header { state, .. } => current = Some(SectionKey::State(state)),
            DashboardLine::Row(row) if row.id == *row_id => return current,
            _ => {}
        }
    }
    None
}

fn render_rows(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    rows: &[DashboardRow],
    state: &mut DashboardState,
) {
    state.row_rects.clear();
    state.row_delete_rects.clear();
    state.section_rects.clear();
    state.idle_overflow_rect = None;
    if area.area() == 0 {
        return;
    }

    // Rows are 3 visual cells tall (title + secondary
    // + padding) and headers are 2 cells tall (label + gap).
    // Viewport scrolling works on cumulative cell offsets so partial
    // rows can't peek out at the top / bottom of the list. The
    // clamp helper still operates in "1 unit = 1 cell" — we just
    // pass cell offsets instead of line indices.
    let lines = build_dashboard_lines(
        rows,
        state.grouping,
        &state.filter,
        &state.collapsed_sections,
        state.idle_show_all,
        state.search_mode,
    );
    let heights: Vec<u16> = lines
        .iter()
        .map(|l| match l {
            DashboardLine::Row(_) => ROW_HEIGHT,
            DashboardLine::Header { .. }
            | DashboardLine::PinnedHeader { .. }
            | DashboardLine::IdleOverflow { .. }
            | DashboardLine::Divider => GROUP_HEADER_HEIGHT,
        })
        .collect();
    let total_cells: usize = heights.iter().map(|h| *h as usize).sum();

    // Compute the selected row's `(top_cell, height)`. The viewport
    // clamp ensures the WHOLE row stays in view rather than just its
    // top cell.
    let mut selected_cell: Option<(usize, u16)> = None;
    {
        let mut cum = 0usize;
        for (line, &h) in lines.iter().zip(heights.iter()) {
            // The cursor is whichever of the three targets is active: a
            // row, or a section header (the button lives outside the list).
            let is_cursor = match line {
                DashboardLine::Row(r) => state.selected.as_ref().is_some_and(|s| *s == r.id),
                DashboardLine::PinnedHeader { .. } => {
                    state.selected_section == Some(SectionKey::Pinned)
                }
                DashboardLine::Header { state: rs, .. } => {
                    state.selected_section == Some(SectionKey::State(*rs))
                }
                DashboardLine::IdleOverflow { .. } => state.selected_idle_overflow,
                DashboardLine::Divider => false,
            };
            if is_cursor {
                selected_cell = Some((cum, h));
                break;
            }
            cum += h as usize;
        }
    }
    let viewport_h = area.height as usize;
    let snap_target = selected_cell.map(|(top, h)| top + (h as usize).saturating_sub(1));
    let offset = state.clamp_viewport(snap_target, viewport_h, total_cells);
    // Bias the offset up if the selection's TOP cell ended up above
    // the visible window (clamp only guarantees the bottom edge of
    // the selection is in range when snap_target was used).
    //
    // Skipped while `manual_scroll_active` so the mouse-wheel can
    // travel past the selected row's top cell — the bias is the
    // same snap-to-selection that the clamp itself skips in that
    // mode. Keyboard nav clears the flag, which restores both the
    // clamp's snap AND this bias.
    let offset = if !state.manual_scroll_active
        && let Some((sel_top, _)) = selected_cell
        && sel_top < offset
    {
        state.viewport_offset = sel_top;
        sel_top
    } else {
        offset
    };

    // Snap the offset DOWN to the nearest line boundary. `render_row`
    // paints title + secondary starting at `rect.y`, so a partial
    // clip at the top would still show the title (cell 0) where the
    // gap (cell 2) belongs — visually the row "sticks" to the top
    // instead of scrolling away. Trackpad scroll deltas (1 line at a
    // time) made this trivially reproducible. Snapping ensures the
    // topmost visible item always starts at its first cell. Offsets
    // that already coincide with a boundary are left untouched.
    let offset = snap_offset_to_line_boundary(offset, &heights);
    state.viewport_offset = offset;

    let needs_scrollbar = total_cells > viewport_h && area.width >= 4;
    // Overlay the scrollbar on the right edge rather than reserving a column,
    // so showing / hiding it never shifts the row layout. Rows always paint at
    // full width; the thumb sits on the trailing margin column.
    let body_width = area.width;
    let max_y = area.y + area.height;

    // Content background per visible line (`None` = spacer), consumed
    // by the half-block halo pass after the items are painted.
    let mut line_bg: Vec<Option<Color>> = vec![None; viewport_h];

    let mut cell_y: usize = 0;
    for (line, &h) in lines.iter().zip(heights.iter()) {
        let next_cell_y = cell_y + h as usize;
        // Skip items entirely above the viewport.
        if next_cell_y <= offset {
            cell_y = next_cell_y;
            continue;
        }
        // Stop once we've painted past the visible window.
        if cell_y >= offset + viewport_h {
            break;
        }
        // Item start y (relative to the area top), accounting for
        // the part that may be clipped above the viewport.
        let visible_top = cell_y.max(offset);
        let y = area.y + (visible_top - offset) as u16;
        if y >= max_y {
            break;
        }
        let item_height = (next_cell_y - visible_top) as u16;
        let render_h = item_height.min(max_y - y);
        let line_rect = Rect {
            x: area.x,
            y,
            width: body_width,
            height: render_h,
        };
        // `line_bg` records each visible line's CONTENT background
        // (`None` = spacer line) for the half-block halo pass below.
        let mark = |line_bg: &mut Vec<Option<Color>>, dy: u16, bg: Color| {
            let idx = (y - area.y + dy) as usize;
            if let Some(slot) = line_bg.get_mut(idx) {
                *slot = Some(bg);
            }
        };
        match line {
            DashboardLine::PinnedHeader { count } => {
                let key = SectionKey::Pinned;
                let collapsed = state.is_section_collapsed(key);
                let selected = state.selected_section == Some(key);
                let hovered = state.hovered_section == Some(key);
                render_group_header(
                    buf, line_rect, theme, "Pinned", *count, collapsed, selected, hovered,
                );
                mark(&mut line_bg, 0, theme.bg_base);
                // Full-height hit rect (label + trailing gap) — no
                // hover/click dead zone between items.
                state
                    .section_rects
                    .push((key, Rect::new(area.x, y, body_width, render_h)));
            }
            DashboardLine::Divider => {
                render_divider(buf, line_rect, theme);
                mark(&mut line_bg, 0, theme.bg_base);
            }
            DashboardLine::Header { state: rs, count } => {
                // Headers only paint into the first cell; the
                // trailing gap stays at bg_base.
                let key = SectionKey::State(*rs);
                let collapsed = state.is_section_collapsed(key);
                let selected = state.selected_section == Some(key);
                let hovered = state.hovered_section == Some(key);
                render_group_header(
                    buf,
                    line_rect,
                    theme,
                    rs.group_label(),
                    *count,
                    collapsed,
                    selected,
                    hovered,
                );
                mark(&mut line_bg, 0, theme.bg_base);
                // Full-height hit rect (label + trailing gap) — no
                // hover/click dead zone between items.
                state
                    .section_rects
                    .push((key, Rect::new(area.x, y, body_width, render_h)));
            }
            DashboardLine::Row(row) => {
                render_row(buf, line_rect, theme, row, state);
                let bg = row_bg(theme, state, row);
                let content_top = row_content_offset(render_h, row);
                let content_h = row_content_height(row).min(render_h);
                for dy in content_top..(content_top + content_h).min(render_h) {
                    mark(&mut line_bg, dy, bg);
                }
                if !row.is_more_placeholder {
                    // Full-height hit rect (content + spacer lines) —
                    // no hover/click dead zone between items; the
                    // highlight covers the content plus half-cell
                    // halos on the neighbouring spacer lines.
                    let hit = Rect {
                        x: area.x,
                        y,
                        width: body_width,
                        height: render_h,
                    };
                    state.row_rects.push((row.id.clone(), hit));
                }
            }
            DashboardLine::IdleOverflow { hidden, expanded } => {
                render_idle_overflow(
                    buf,
                    line_rect,
                    theme,
                    *hidden,
                    *expanded,
                    state.selected_idle_overflow,
                    state.hovered_idle_overflow,
                );
                mark(&mut line_bg, 0, theme.bg_base);
                // Full-height hit rect (label + trailing gap) — no
                // hover/click dead zone below the overflow row.
                state.idle_overflow_rect = Some(Rect::new(area.x, y, body_width, render_h));
            }
        }
        cell_y = next_cell_y;
    }

    render_spacer_halos(buf, area, body_width, &line_bg, theme.bg_base);

    if needs_scrollbar {
        render_scrollbar(buf, area, offset, viewport_h, total_cells, theme);
    }
}

/// Paint the spacer lines between items as half-cell "halos" so a
/// highlighted row reads as vertically centered: the spacer below a
/// highlighted block shows the highlight in its TOP half, and the
/// spacer above shows it in its BOTTOM half. Implemented with the
/// upper-half-block glyph (`▀`, CP437 `0xDF` — safe on legacy
/// consoles): fg paints the top half with the colour of the content
/// line above, bg paints the bottom half with the colour of the
/// content line below. Spacers between two `bg_base` neighbours are
/// left untouched.
fn render_spacer_halos(
    buf: &mut Buffer,
    area: Rect,
    body_width: u16,
    line_bg: &[Option<Color>],
    base: Color,
) {
    for (i, slot) in line_bg.iter().enumerate() {
        if slot.is_some() {
            continue;
        }
        let above = if i > 0 {
            line_bg[i - 1].unwrap_or(base)
        } else {
            base
        };
        let below = line_bg.get(i + 1).copied().flatten().unwrap_or(base);
        if above == base && below == base {
            continue;
        }
        let y = area.y + i as u16;
        if above == below {
            let fill = " ".repeat(body_width as usize);
            buf.set_string(area.x, y, &fill, Style::default().bg(above));
        } else {
            let fill = "\u{2580}".repeat(body_width as usize);
            buf.set_string(area.x, y, &fill, Style::default().fg(above).bg(below));
        }
    }
}

/// Wide-mode group header reads:
///
/// ```text
/// Working 3 ──────────────────────────────────────────────────
/// ```
///
/// Bold flush-left label (within the padded list area), dim count two
/// cells later, faint horizontal rule filling the remainder of the row.
/// The dim rule visually anchors the boundary the way gh-dash / lazygit
/// / k9s do. Section titles are left-aligned within the list; row text
/// is indented after the marker + icon columns.
#[allow(clippy::too_many_arguments)]
fn render_group_header(
    buf: &mut Buffer,
    rect: Rect,
    theme: &Theme,
    label: &str,
    count: usize,
    collapsed: bool,
    selected: bool,
    hovered: bool,
) {
    let bg = Style::default().bg(theme.bg_base);
    let fill = " ".repeat(rect.width as usize);
    buf.set_string(rect.x, rect.y, fill, bg);
    if rect.width == 0 {
        return;
    }
    // Selected (keyboard cursor) → accent_user; hovered (mouse) →
    // brighter text_primary; otherwise the dim gray default.
    let label_fg = if selected {
        theme.accent_user
    } else if hovered {
        theme.text_primary
    } else {
        theme.gray
    };
    let count_str = format!(" {count}");
    let label_style = Style::default()
        .fg(label_fg)
        .bg(theme.bg_base)
        .add_modifier(Modifier::BOLD);
    let count_style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);
    let rule_style = Style::default()
        .fg(theme.selection_border)
        .bg(theme.bg_base);

    // Disclosure indicator: ▾ expanded, ▸ collapsed.
    let glyph_str = format!(
        "{} ",
        if collapsed {
            crate::glyphs::disclosure_closed()
        } else {
            crate::glyphs::disclosure_open()
        }
    );
    let glyph_w = UnicodeWidthStr::width(glyph_str.as_str()) as u16;
    let label_w = UnicodeWidthStr::width(label) as u16;
    let count_w = UnicodeWidthStr::width(count_str.as_str()) as u16;

    // Layout: disclosure glyph, then title text, then count + rule.
    let mut cx = rect.x;
    if glyph_w >= rect.width {
        return;
    }
    buf.set_string(cx, rect.y, &glyph_str, label_style);
    cx += glyph_w;

    let label_avail = (rect.x + rect.width).saturating_sub(cx);
    if label_w >= label_avail {
        let trunc = truncate_str(label, label_avail as usize);
        buf.set_string(cx, rect.y, trunc, label_style);
        return;
    }
    buf.set_string(cx, rect.y, label, label_style);
    cx += label_w;

    if cx + count_w >= rect.x + rect.width {
        return;
    }
    buf.set_string(cx, rect.y, &count_str, count_style);
    cx += count_w;

    // Single-cell pad between count and rule so the digits don't
    // bleed into the line.
    let pad = 1u16;
    if cx + pad >= rect.x + rect.width {
        return;
    }
    cx += pad;

    let rule_w = (rect.x + rect.width).saturating_sub(cx);
    if rule_w == 0 {
        return;
    }
    let rule: String = "\u{2500}".repeat(rule_w as usize);
    buf.set_string(cx, rect.y, &rule, rule_style);
}

/// Textless divider — a full-width horizontal rule in the section-header
/// rule style. Used when grouping is OFF to separate the pinned block from
/// the rest without a labelled header. Paints only the first cell; any
/// trailing cell of its rect stays at `bg_base` (the breathing gap).
fn render_divider(buf: &mut Buffer, rect: Rect, theme: &Theme) {
    let bg = Style::default().bg(theme.bg_base);
    let fill = " ".repeat(rect.width as usize);
    buf.set_string(rect.x, rect.y, fill, bg);
    if rect.width == 0 {
        return;
    }
    let rule_style = Style::default()
        .fg(theme.selection_border)
        .bg(theme.bg_base);
    let rule: String = "\u{2500}".repeat(rect.width as usize);
    buf.set_string(rect.x, rect.y, &rule, rule_style);
}

/// The Idle group's "N more" overflow toggle row — a
/// single dim line painted at the bottom of a capped Idle group: a
/// `+` / `-` expand indicator in the icon column (`+` collapsed, `-`
/// expanded) and the label in the agent-name column, aligned with the
/// rows above. The
/// cursor (keyboard) paints it in `accent_user`; mouse hover brightens
/// it to `text_primary`. `expanded` flips the label to "show fewer" so
/// the same row re-folds the list. Shared by the wide and narrow paths
/// (the affordance is single-line in both).
fn render_idle_overflow(
    buf: &mut Buffer,
    rect: Rect,
    theme: &Theme,
    hidden: usize,
    expanded: bool,
    selected: bool,
    hovered: bool,
) {
    let bg = Style::default().bg(theme.bg_base);
    let fill = " ".repeat(rect.width as usize);
    buf.set_string(rect.x, rect.y, fill, bg);
    if rect.width == 0 {
        return;
    }
    let fg = if selected {
        theme.accent_user
    } else if hovered {
        theme.text_primary
    } else {
        theme.gray_dim
    };
    let style = Style::default().fg(fg).bg(theme.bg_base);
    let label = if expanded {
        "show fewer".to_string()
    } else {
        format!("{hidden} more")
    };
    // A `+` / `-` expand indicator in the icon column + the label in the
    // agent-name column, so the row aligns with the Idle rows above:
    // marker (1) + gap (1) + icon + gap (1); the Idle group is top-level,
    // so indent is 0.
    let indicator = if expanded { "-" } else { "+" };
    let icon_w = unicode_width::UnicodeWidthStr::width(state_icon(RowState::Idle, 0)) as u16;
    let indicator_x = rect.x.saturating_add(2);
    let name_x = indicator_x.saturating_add(icon_w + 1);
    if indicator_x < rect.x + rect.width {
        buf.set_string(indicator_x, rect.y, indicator, style);
    }
    if name_x >= rect.x + rect.width {
        return;
    }
    let avail = (rect.x + rect.width).saturating_sub(name_x);
    let trunc = truncate_str(&label, avail as usize);
    buf.set_string(name_x, rect.y, trunc, style);
}

/// Narrow-mode group header. Compact `Done 12` form — no trailing
/// rule because the narrow layout doesn't have the width budget.
#[allow(clippy::too_many_arguments)]
fn render_group_header_narrow(
    buf: &mut Buffer,
    rect: Rect,
    theme: &Theme,
    label: &str,
    count: usize,
    collapsed: bool,
    selected: bool,
    hovered: bool,
) {
    let bg_style = Style::default().bg(theme.bg_base);
    let label_fg = if selected {
        theme.accent_user
    } else if hovered {
        theme.text_primary
    } else {
        theme.gray
    };
    let label_style = Style::default()
        .fg(label_fg)
        .bg(theme.bg_base)
        .add_modifier(Modifier::BOLD);
    let fill = " ".repeat(rect.width as usize);
    buf.set_string(rect.x, rect.y, fill, bg_style);
    let glyph = if collapsed {
        crate::glyphs::disclosure_closed()
    } else {
        crate::glyphs::disclosure_open()
    };
    let line = format!("{glyph} {label} {count}");
    let trunc = truncate_str(&line, rect.width as usize);
    buf.set_string(rect.x, rect.y, trunc, label_style);
}

/// Render the list scrollbar in the right gutter, stuck to the window's
/// rightmost column, using the exact scrollback style: a `█` thumb in
/// `scrollbar_fg` over a `scrollbar_bg` track (no `│` line). Reuses the
/// shared `render_scrollbar_styled` so the thumb sizing/colour matches the
/// scrollback pixel-for-pixel.
///
/// The list content is inset by `LIST_OUTER_HPAD`; the scrollbar lives in
/// that right margin so it never reserves width from the rows (no layout
/// shift). The position is clamped to the buffer edge so isolated renders
/// (where `area` spans the whole buffer, e.g. tests) still paint in-bounds.
fn render_scrollbar(
    buf: &mut Buffer,
    area: Rect,
    offset: usize,
    visible: usize,
    total: usize,
    theme: &Theme,
) {
    use crate::render::scrollbar::render_scrollbar_styled;

    let x = (area.x + area.width - 1 + super::layout::LIST_OUTER_HPAD)
        .min(buf.area.right().saturating_sub(1));
    let scrollbar_area = Rect {
        x,
        y: area.y,
        width: 1,
        height: area.height,
    };
    let track_style = Style::default().bg(theme.scrollbar_bg);
    let thumb_style = Style::default()
        .fg(theme.scrollbar_fg)
        .bg(theme.scrollbar_bg);
    let cap = |v: usize| v.min(u16::MAX as usize) as u16;
    render_scrollbar_styled(
        buf,
        Some(scrollbar_area),
        cap(total),
        cap(visible),
        cap(offset),
        track_style,
        thumb_style,
    );
}

/// Snap a cell-granular viewport offset DOWN to the nearest item-start
/// boundary in `heights`. Each entry in `heights` is the cell-height of
/// one `DashboardLine` (rows = 3, headers = 2). The returned offset
/// is the cumulative start of the item that currently overlaps `offset`.
/// Offsets that already land on a boundary are returned unchanged.
///
/// This exists because `render_row` paints its content from `rect.y`
/// (title at the top, secondary just below) — it has no notion of
/// "partial top clip". Without snapping, a trackpad scrolling by 1
/// cell would paint the row's title at the viewport top where the
/// row's gap-cell should be: visually the row "sticks" instead of
/// scrolling away. Snapping forces whole-row alignment so the
/// topmost row always starts at its first cell.
fn snap_offset_to_line_boundary(offset: usize, heights: &[u16]) -> usize {
    let mut cum = 0usize;
    let mut snapped = 0usize;
    for &h in heights {
        if cum > offset {
            break;
        }
        snapped = cum;
        cum = cum.saturating_add(h as usize);
    }
    snapped
}

/// Number of content lines a row renders: title + optional secondary.
fn row_content_height(row: &DashboardRow) -> u16 {
    if row.secondary_line.as_deref().is_some_and(|s| !s.is_empty()) {
        2
    } else {
        1
    }
}

/// Vertical offset of a row's content block within its rect. The
/// 1- or 2-line content is centered at cell granularity: a title-only
/// row in a 3-cell rect gets one padding line above and below, while
/// a title+secondary row stays top-aligned ((3 - 2) / 2 = 0).
fn row_content_offset(height: u16, row: &DashboardRow) -> u16 {
    height.saturating_sub(row_content_height(row)) / 2
}

/// The row's background: keyboard selection wins over mouse hover.
fn row_bg(theme: &Theme, state: &DashboardState, row: &DashboardRow) -> Color {
    if state.selected.as_ref().is_some_and(|s| *s == row.id) {
        theme.bg_highlight
    } else if state.hovered_row.as_ref().is_some_and(|h| *h == row.id) {
        theme.bg_hover
    } else {
        theme.bg_base
    }
}

/// Render a row as a 2-line block plus a trailing padding line
/// (`rect.height` is expected to be `>= 2`; the caller —
/// `render_rows` — sizes the rect to either 2 or 3 lines depending
/// on whether the padding is in budget).
///
/// Visual:
///
/// ```text
///   ◆ Add responsiveness to /context · echoagent my-branch-2 worktree       4 mins
///     Pending: plan approval plan.md
/// ```
///
/// Line 1 (title): selection marker + icon + label + subtitle + age.
/// Line 2 (secondary): aligned-under-label, dim text — the last
/// tool call, the last assistant message, or a `Pending: …` preview
/// of the front-most permission request.
///
/// The content block is vertically centered within the rect (see
/// [`row_content_offset`]): a title-only row in a 3-cell rect renders
/// as padding + title + padding.
///
/// Selection / hover backgrounds fill the CONTENT lines; the spacer
/// lines around them are painted afterwards by `render_rows`'s
/// half-block pass (see `render_spacer_halos`), which extends the
/// highlight half a cell above and below so it reads as centered on
/// the text.
fn render_row(
    buf: &mut Buffer,
    rect: Rect,
    theme: &Theme,
    row: &DashboardRow,
    state: &mut DashboardState,
) {
    if rect.area() == 0 {
        return;
    }
    let selected = state.selected.as_ref().is_some_and(|s| *s == row.id);
    let renaming = state.rename.as_ref().is_some_and(|r| r.row == row.id);
    let bg = row_bg(theme, state, row);

    // Paint the content lines with the row background. Spacer lines
    // keep `bg_base` here; the halo pass splits them between the
    // neighbouring items.
    let content_top = row_content_offset(rect.height, row);
    let content_h = row_content_height(row).min(rect.height);
    let fill = " ".repeat(rect.width as usize);
    for dy in content_top..(content_top + content_h).min(rect.height) {
        buf.set_string(rect.x, rect.y + dy, &fill, Style::default().bg(bg));
    }

    // Layout columns: marker (1) | gap (1) | indent (2*n) | icon (1-2)
    //                | gap (1) | label/secondary start.
    let marker = if selected {
        crate::glyphs::selection_bar()
    } else {
        " "
    };
    let marker_w = UnicodeWidthStr::width(marker) as u16;
    let indent_w = (row.indent as u16) * 2;
    let icon = state_icon(row.state, state.spinner_tick);
    // `NeedsInput` bullets blink yellow ↔ dim yellow; every other state uses
    // its steady state colour.
    let icon_color = if row.state == RowState::NeedsInput {
        needs_input_bullet_color(state.spinner_tick, theme)
    } else {
        state_color(row.state, theme)
    };
    let icon_w = UnicodeWidthStr::width(icon) as u16;
    // Title-row paint cursor (no leading 1-col gap before the marker
    // — the marker IS the leftmost cell, mirroring the wide-mode
    // header which starts flush-left at col 0). The content block is
    // vertically centered within the rect at cell granularity:
    // title-only rows sit padded above and below, while 2-line rows
    // stay top-aligned (2 lines cannot center in a 3-cell row).
    let title_y = rect.y + row_content_offset(rect.height, row);
    let content_start_x = rect.x + marker_w + 1 + indent_w + icon_w + 1;

    // Rename overlay: keep the row's chrome (marker + state icon) in
    // place and swap ONLY the title text for `rename: {draft}`, painted
    // at the title's own column so the row stays visually aligned with
    // its neighbours while editing.
    if renaming && let Some(rn) = state.rename.as_ref() {
        buf.set_string(
            rect.x,
            title_y,
            marker,
            Style::default()
                .bg(bg)
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD),
        );
        // Keep the left bar continuous on every content line even
        // while the rename overlay is active on the title line.
        if selected {
            let bar_style = Style::default()
                .bg(bg)
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD);
            for dy in content_top..(content_top + content_h).min(rect.height) {
                buf.set_string(
                    rect.x,
                    rect.y + dy,
                    crate::glyphs::selection_bar(),
                    bar_style,
                );
            }
        }
        // State icon stays put (same column + colour as the normal row).
        buf.set_string(
            rect.x + marker_w + 1 + indent_w,
            title_y,
            icon,
            Style::default().fg(icon_color).bg(bg),
        );
        let available = (rect.x + rect.width).saturating_sub(content_start_x);
        render_rename_editor(
            buf,
            content_start_x,
            title_y,
            available,
            Style::default()
                .fg(theme.accent_user)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
            rn,
        );
        return;
    }

    // Marker + icon.
    buf.set_string(
        rect.x,
        title_y,
        marker,
        Style::default()
            .bg(bg)
            .fg(theme.accent_user)
            .add_modifier(Modifier::BOLD),
    );

    // For the active selection, extend the thin left bar down every
    // content line of the row so it forms one continuous vertical rule
    // along the highlighted text. Hover and normal states keep their
    // marker only on the title line.
    if selected {
        let bar_style = Style::default()
            .bg(bg)
            .fg(theme.accent_user)
            .add_modifier(Modifier::BOLD);
        for dy in content_top..(content_top + content_h).min(rect.height) {
            buf.set_string(
                rect.x,
                rect.y + dy,
                crate::glyphs::selection_bar(),
                bar_style,
            );
        }
    }

    let icon_x = rect.x + marker_w + 1 + indent_w;
    buf.set_string(
        icon_x,
        title_y,
        icon,
        Style::default().bg(bg).fg(icon_color),
    );

    let armed_delete = state.armed_delete_row_ref();
    let show_delete = !row.is_more_placeholder
        && !row.id.is_subagent()
        && row.state.allows_delete()
        && !state.row_is_conversation(&row.id)
        && (state.hovered_row.as_ref() == Some(&row.id) || armed_delete == Some(&row.id));
    let delete_label = crate::glyphs::ballot_x_button();
    let delete_w = UnicodeWidthStr::width(delete_label) as u16;
    let age = format_time_ago(row.last_change_at.elapsed().unwrap_or_default());
    let age_str = format!("{age:>6}");
    let age_w = UnicodeWidthStr::width(age_str.as_str()) as u16;
    let right_w = if show_delete { delete_w } else { age_w };
    let right_x = rect.x + rect.width.saturating_sub(right_w + 1);
    if right_x > content_start_x {
        if show_delete {
            let fg = if state.hovered_delete.as_ref() == Some(&row.id)
                || armed_delete == Some(&row.id)
            {
                theme.accent_error
            } else {
                theme.text_secondary
            };
            buf.set_string(
                right_x,
                title_y,
                delete_label,
                Style::default().bg(bg).fg(fg),
            );
            state.row_delete_rects.push((
                row.id.clone(),
                Rect {
                    x: right_x,
                    y: title_y,
                    width: delete_w,
                    height: 1,
                },
            ));
        } else {
            buf.set_string(
                right_x,
                title_y,
                &age_str,
                Style::default().bg(bg).fg(theme.gray),
            );
        }
    }

    // Title text: `{label}` (bright) + ` · {subtitle}` (dim) +
    // optional `[badge]` chips for failed / pinned.
    // Trimmed to fit between the icon and the age column.
    let title_avail = right_x.saturating_sub(content_start_x).saturating_sub(2);
    let mut cx = content_start_x;
    if title_avail > 0 {
        let label_style = Style::default().bg(bg).fg(if row.is_more_placeholder {
            theme.gray_dim
        } else {
            theme.text_primary
        });
        // Fallback "New session #<id>" gets two-tone styling: the
        // `New session` head in the primary colour, the ` #id` suffix dim.
        // Detected on the shared prefix + a `#` to avoid dimming real titles
        // that merely start with "New session".
        let dim_suffix = (!row.is_more_placeholder)
            .then(|| row.label.strip_prefix(super::row::NEW_SESSION_LABEL))
            .flatten()
            .filter(|rest| rest.starts_with(" #"));
        if let Some(suffix) = dim_suffix {
            let head_trunc = truncate_str(super::row::NEW_SESSION_LABEL, title_avail as usize);
            let head_w = UnicodeWidthStr::width(&head_trunc[..]) as u16;
            buf.set_string(cx, title_y, &head_trunc, label_style);
            cx += head_w;
            let remaining = title_avail.saturating_sub(head_w) as usize;
            if remaining > 0 {
                let suffix_trunc = truncate_str(suffix, remaining);
                let suffix_w = UnicodeWidthStr::width(&suffix_trunc[..]) as u16;
                buf.set_string(
                    cx,
                    title_y,
                    &suffix_trunc,
                    Style::default().bg(bg).fg(theme.gray_dim),
                );
                cx += suffix_w;
            }
        } else {
            let label_trunc = truncate_str(&row.label, title_avail as usize);
            let label_w = UnicodeWidthStr::width(&label_trunc[..]) as u16;
            buf.set_string(cx, title_y, label_trunc, label_style);
            cx += label_w;
        }

        // Subtitle: ` · echoagent my-branch-2 worktree`.
        if let Some(sub) = row.subtitle.as_deref()
            && cx + 4 < right_x
        {
            let remaining = right_x.saturating_sub(cx).saturating_sub(2) as usize;
            let sub_str = format!(" \u{00B7} {sub}");
            let sub_trunc = truncate_str(&sub_str, remaining);
            let sub_w = UnicodeWidthStr::width(&sub_trunc[..]) as u16;
            buf.set_string(
                cx,
                title_y,
                sub_trunc,
                Style::default().bg(bg).fg(theme.gray_dim),
            );
            cx += sub_w;
        }

        // Compact badges — only the visually meaningful ones. Several are
        // dropped because another cue already conveys them: `worktree` is
        // folded into the subtitle; `needs-input` shows via the blinking
        // yellow bullet + yellow `Pending:` subtitle; `pinned` shows via the
        // dedicated Pinned section at the top of the list.
        for badge in &row.badges {
            if matches!(
                badge,
                RowBadge::Worktree | RowBadge::NeedsInput | RowBadge::Pinned
            ) {
                continue;
            }
            let label = match badge {
                RowBadge::NeedsInput | RowBadge::Worktree | RowBadge::Pinned => continue,
                RowBadge::Failed => "failed",
                RowBadge::BgTask => "bg",
            };
            let chip = format!(" [{label}]");
            let cw = UnicodeWidthStr::width(chip.as_str()) as u16;
            if cx + cw + 1 < right_x {
                buf.set_string(
                    cx,
                    title_y,
                    &chip,
                    Style::default().bg(bg).fg(badge_color(*badge, theme)),
                );
                cx += cw;
            }
        }
    }

    // Secondary row. The selected row brightens its secondary
    // line so the user can read what the agent is doing right
    // now without leaving the dashboard. Unselected rows keep
    // the dimmer `gray_dim` so they read as the row's metadata
    // tail rather than competing with the title.
    if rect.height >= 2
        && let Some(secondary) = row.secondary_line.as_deref()
        && !secondary.is_empty()
    {
        let sec_y = title_y + 1;
        let avail = rect
            .width
            .saturating_sub(content_start_x - rect.x)
            .saturating_sub(1);
        if avail > 0 {
            let trunc = truncate_str(secondary, avail as usize);
            let secondary_fg = if selected {
                theme.text_secondary
            } else {
                theme.gray_dim
            };
            // The awaiting-input subtitle is `Pending: …`; paint the
            // `Pending:` prefix in yellow so the actionable state stands out,
            // and the rest in the normal secondary colour.
            const PENDING_PREFIX: &str = "Pending:";
            if let Some(rest) = trunc.strip_prefix(PENDING_PREFIX) {
                buf.set_string(
                    content_start_x,
                    sec_y,
                    PENDING_PREFIX,
                    Style::default().bg(bg).fg(theme.warning),
                );
                let prefix_w = UnicodeWidthStr::width(PENDING_PREFIX) as u16;
                buf.set_string(
                    content_start_x + prefix_w,
                    sec_y,
                    rest,
                    Style::default().bg(bg).fg(secondary_fg),
                );
            } else {
                buf.set_string(
                    content_start_x,
                    sec_y,
                    trunc,
                    Style::default().bg(bg).fg(secondary_fg),
                );
            }
        }
    }
}

fn render_narrow_rows(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    rows: &[DashboardRow],
    state: &mut DashboardState,
) {
    state.row_rects.clear();
    state.row_delete_rects.clear();
    state.section_rects.clear();
    state.idle_overflow_rect = None;
    if area.area() == 0 {
        return;
    }

    // Narrow mode stays single-line per row (the wide path's 2-line
    // form would push too many rows off-screen on a 40-col terminal).
    // We still emit group headers and the selection marker so the
    // visual vocabulary stays consistent.
    let lines = build_dashboard_lines(
        rows,
        state.grouping,
        &state.filter,
        &state.collapsed_sections,
        state.idle_show_all,
        state.search_mode,
    );
    let viewport_h = area.height as usize;
    // The clamp follows whichever cursor is active — a row OR a section
    // header — so navigating onto a section title scrolls it into view,
    // matching the wide layout. Previously only a selected row was
    // tracked, so a selected header could stay off-screen.
    let selected_line_idx = lines.iter().position(|l| match l {
        DashboardLine::Row(r) => state.selected.as_ref().is_some_and(|s| r.id == *s),
        DashboardLine::PinnedHeader { .. } => state.selected_section == Some(SectionKey::Pinned),
        DashboardLine::Header { state: rs, .. } => {
            state.selected_section == Some(SectionKey::State(*rs))
        }
        DashboardLine::IdleOverflow { .. } => state.selected_idle_overflow,
        DashboardLine::Divider => false,
    });
    let offset = state.clamp_viewport(selected_line_idx, viewport_h, lines.len());

    let needs_scrollbar = lines.len() > viewport_h && area.width >= 4;
    // Overlay the scrollbar (see `render_rows`) — no reserved column, no shift.
    let body_width = area.width;

    let mut y = area.y;
    for line in lines.iter().skip(offset).take(viewport_h) {
        if y >= area.y + area.height {
            break;
        }
        let line_rect = Rect {
            x: area.x,
            y,
            width: body_width,
            height: 1,
        };
        let row = match line {
            DashboardLine::PinnedHeader { count } => {
                let key = SectionKey::Pinned;
                let collapsed = state.is_section_collapsed(key);
                let selected = state.selected_section == Some(key);
                let hovered = state.hovered_section == Some(key);
                render_group_header_narrow(
                    buf, line_rect, theme, "Pinned", *count, collapsed, selected, hovered,
                );
                state
                    .section_rects
                    .push((key, Rect::new(area.x, y, body_width, 1)));
                y += 1;
                continue;
            }
            DashboardLine::Header { state: rs, count } => {
                let key = SectionKey::State(*rs);
                let collapsed = state.is_section_collapsed(key);
                let selected = state.selected_section == Some(key);
                let hovered = state.hovered_section == Some(key);
                render_group_header_narrow(
                    buf,
                    line_rect,
                    theme,
                    rs.group_label(),
                    *count,
                    collapsed,
                    selected,
                    hovered,
                );
                state
                    .section_rects
                    .push((key, Rect::new(area.x, y, body_width, 1)));
                y += 1;
                continue;
            }
            DashboardLine::Divider => {
                render_divider(buf, line_rect, theme);
                y += 1;
                continue;
            }
            DashboardLine::IdleOverflow { hidden, expanded } => {
                render_idle_overflow(
                    buf,
                    line_rect,
                    theme,
                    *hidden,
                    *expanded,
                    state.selected_idle_overflow,
                    state.hovered_idle_overflow,
                );
                state.idle_overflow_rect = Some(Rect::new(area.x, y, body_width, 1));
                y += 1;
                continue;
            }
            DashboardLine::Row(row) => row,
        };

        let selected = state.selected.as_ref().is_some_and(|s| *s == row.id);
        let hovered = state.hovered_row.as_ref().is_some_and(|h| *h == row.id);
        let renaming = state.rename.as_ref().is_some_and(|r| r.row == row.id);
        let bg = if selected {
            theme.bg_highlight
        } else if hovered {
            theme.bg_hover
        } else {
            theme.bg_base
        };

        if renaming && let Some(rn) = state.rename.as_ref() {
            // Mirror the wide layout: keep the marker + state icon
            // chrome and swap only the label for `rename: {draft}`, so
            // the editing row stays column-aligned with its neighbours.
            let marker = if selected {
                crate::glyphs::selection_bar()
            } else {
                " "
            };
            let icon = state_icon(row.state, state.spinner_tick);
            let indent = "  ".repeat(row.indent as usize);
            let chrome = format!("{marker} {indent}{icon} ");
            let chrome_w = UnicodeWidthStr::width(chrome.as_str()) as u16;
            buf.set_string(
                area.x,
                y,
                &chrome,
                Style::default().fg(theme.text_primary).bg(bg),
            );
            render_rename_editor(
                buf,
                area.x + chrome_w,
                y,
                body_width.saturating_sub(chrome_w),
                Style::default()
                    .fg(theme.accent_user)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
                rn,
            );
        } else {
            let marker = if selected {
                crate::glyphs::selection_bar()
            } else {
                " "
            };
            let marker_w = UnicodeWidthStr::width(marker) as u16;
            let icon = state_icon(row.state, state.spinner_tick);
            let icon_w = UnicodeWidthStr::width(icon) as u16;
            let indent = "  ".repeat(row.indent as usize);
            let indent_w = UnicodeWidthStr::width(indent.as_str()) as u16;
            let gap_after_marker = 1u16;
            let chrome = marker_w + gap_after_marker + indent_w + icon_w + 1;
            let armed_here = state.armed_delete_row_ref() == Some(&row.id);
            let show_delete = !row.is_more_placeholder
                && !row.id.is_subagent()
                && row.state.allows_delete()
                && !state.row_is_conversation(&row.id)
                && (hovered || armed_here);
            let delete_label = crate::glyphs::ballot_x_button();
            let delete_w = UnicodeWidthStr::width(delete_label) as u16;
            let label_budget = body_width
                .saturating_sub(chrome)
                .saturating_sub(if show_delete { delete_w + 1 } else { 0 });
            let label = truncate_str(&row.label, label_budget as usize);
            let line = format!("{marker} {indent}{icon} {label}");
            buf.set_string(
                area.x,
                y,
                line,
                Style::default().fg(theme.text_primary).bg(bg),
            );
            if show_delete && body_width > chrome + delete_w {
                let dx = area.x + body_width.saturating_sub(delete_w);
                let fg = if state.hovered_delete.as_ref() == Some(&row.id) || armed_here {
                    theme.accent_error
                } else {
                    theme.text_secondary
                };
                buf.set_string(dx, y, delete_label, Style::default().fg(fg).bg(bg));
                state
                    .row_delete_rects
                    .push((row.id.clone(), Rect::new(dx, y, delete_w, 1)));
            }
        }
        if !row.is_more_placeholder {
            state.row_rects.push((row.id.clone(), line_rect));
        }
        y += 1;
    }

    if needs_scrollbar {
        render_scrollbar(buf, area, offset, viewport_h, lines.len(), theme);
    }
}

/// Rendered when agents exist but the filter has
/// hidden every row. Distinct from the empty-state hint so the
/// user knows their filter is what's hiding the rows.
fn render_no_match(buf: &mut Buffer, area: Rect, theme: &Theme, filter: &Filter) {
    if area.area() == 0 {
        return;
    }
    let hint = match filter {
        Filter::None => "No matching rows.".to_string(),
        Filter::Agent(n) => format!("No agents match `a:{n}`. Press Esc to clear the filter."),
        Filter::State(s) => format!(
            "No agents in state `{}`: press Esc to clear the filter.",
            s.group_label()
        ),
        Filter::Substring(n) => format!("No rows match `{n}`: press Esc to clear the filter."),
    };
    let truncated = truncate_str(&hint, area.width.saturating_sub(2) as usize);
    // Explicit offset to avoid `area.y + 1.min(...)`
    // precedence ambiguity. Drop one row of padding when the area
    // can accommodate it; otherwise stay at the top.
    let y_offset: u16 = if area.height >= 2 { 1 } else { 0 };
    buf.set_string(
        area.x + 1,
        area.y + y_offset,
        truncated,
        Style::default().fg(theme.gray),
    );
}

fn render_empty_state(buf: &mut Buffer, area: Rect, theme: &Theme, loading: bool) {
    if area.area() == 0 {
        return;
    }
    // A single dim line — the dispatch input below is the call to
    // action, so no multi-line onboarding is needed (edge case 25
    // still holds: never render a fully blank screen). While the local
    // session roster is still being fetched we show a loading hint so a
    // fresh open doesn't flash the "no agents" copy before rows land.
    let line = if loading {
        "Loading sessions…"
    } else {
        "No agents yet, type a prompt to start one."
    };
    let truncated = truncate_str(line, area.width.saturating_sub(2) as usize);
    // See `render_no_match` for the precedence rationale.
    let y_offset: u16 = if area.height >= 2 { 1 } else { 0 };
    buf.set_string(
        area.x + 1,
        area.y + y_offset,
        truncated,
        Style::default().fg(theme.gray),
    );
}

/// Paint a rounded-box
/// chrome around the dispatch input so it reads as a real input
/// field. On a 3-row rect the layout is:
///
/// ```text
/// ╭──────────────────────────────────────────────────────────╮
/// │ ❯ Dispatch a new agent                                   │
/// ╰──────────────────────────────────────────────────────────╯
/// ```
///
/// On a 1-row rect (very short terminals) we fall back to the
/// bare `❯ {text}` line so the input stays usable.
///
/// `reply_label` flips the placeholder between `Dispatch a new
/// agent` (`None`) and `Reply to {label}` (`Some`) so the
/// chrome reflects what Enter will do: dispatch a new session vs.
/// enqueue / send a prompt to the currently-selected agent.
/// Paint a short right-aligned feedback badge onto the dispatch box's
/// **top border** (e.g. `✗ Session no longer exists`, `✓ Theme: EchoAgent
/// Day`), in a neutral accent colour. The message is painted VERBATIM:
/// it already carries its own status glyph — errors are built via
/// [`DashboardState::set_error_toast`] (`✗`), while successes / info
/// arrive from the `show_toast` builders (`✓` / `⚠`). The badge
/// therefore neither prepends a glyph nor forces a colour; doing so
/// previously produced a doubled `✗ ✓ …` and painted non-errors (like
/// "Session closed") red. The glyph, not the colour, conveys severity —
/// mirroring the per-agent toast in [`crate::app::agent_view`]. The
/// badge ends one column before the right corner (`╮`) and is truncated
/// to fit. No-op when there's no toast or the box is too narrow.
fn paint_dispatch_feedback_badge(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    error_toast: Option<&str>,
) {
    let Some(err) = error_toast else {
        return;
    };
    // Leave room for the two rounded corners plus a little breathing
    // space, so the bar still reads as a border rather than a full
    // banner.
    let max_w = area.width.saturating_sub(4);
    if max_w < 6 {
        return;
    }
    // Leading/trailing spaces pad the chip away from the surrounding
    // border glyphs. No glyph is prepended — the message already owns
    // one (see the doc comment).
    let label = format!(" {err} ");
    let trunc = truncate_str(&label, max_w as usize);
    let label_w = UnicodeWidthStr::width(trunc.as_str()) as u16;
    // Right-align so the badge ends one column before the `╮` corner.
    let x = area.x + area.width.saturating_sub(1 + label_w);
    buf.set_string(
        x,
        area.y,
        &trunc,
        Style::default()
            .fg(theme.accent_user)
            .bg(theme.bg_base)
            .add_modifier(Modifier::BOLD),
    );
}

/// Paint the model + mode indicator onto the dispatch box's **bottom
/// border**, reusing the shared prompt info-line renderer so its style,
/// spacing, and position match the chat prompt exactly (`╰──model · flag──╯`).
/// The model name renders in dim secondary text; the mode shows as a `plan`
/// (plan accent) or `always-approve` (default) flag. No-op when the box is
/// too short or there's nothing to show.
fn paint_dispatch_config_badge(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    state: &DashboardState,
    input_focused: bool,
) {
    use crate::views::dashboard::DashboardDispatchMode;
    use crate::views::prompt_widget::{PromptFlag, PromptInfo};

    if area.height < 3 || area.width < 6 {
        return;
    }
    let model_label = state
        .pending_model
        .as_ref()
        .map(|m| match m.effort {
            Some(effort) => format!("{} ({effort})", m.display),
            None => m.display.clone(),
        })
        .or_else(|| state.models.current_model_name())
        .unwrap_or_default();

    // Mode flag, styled exactly like the chat prompt's mode flags.
    let mut flags: Vec<PromptFlag> = Vec::new();
    match state.pending_mode {
        DashboardDispatchMode::Plan => flags.push(PromptFlag {
            text: "plan",
            color: Some(theme.accent_plan),
            bold: false,
        }),
        DashboardDispatchMode::Auto => flags.push(PromptFlag {
            text: "auto",
            color: Some(theme.accent_system),
            bold: false,
        }),
        DashboardDispatchMode::AlwaysApprove => flags.push(PromptFlag {
            text: "always-approve",
            color: None,
            bold: false,
        }),
        DashboardDispatchMode::Normal => {}
    }

    if model_label.is_empty() && flags.is_empty() && !state.multiline_mode {
        return;
    }

    let info = PromptInfo {
        model_name: &model_label,
        flags: &flags,
        multiline: state.multiline_mode,
        usage_warning: None,
        usage_warning_critical: false,
    };
    // Bottom border row, inside the corners — the same content rect the chat
    // prompt uses for its info line.
    let info_rect = Rect {
        x: area.x + 1,
        y: area.y + area.height - 1,
        width: area.width.saturating_sub(2),
        height: 1,
    };
    state
        .dispatch
        .render_info_line(buf, info_rect, &info, theme.bg_base, theme, input_focused);
}

/// Paint the left-aligned `● rec` badge on a box's top border while the mic is
/// hot. Shared by the dispatch box and the peek panel that replaces it, so a
/// capture started in either surface shows the same indicator.
pub(super) fn paint_record_badge(buf: &mut Buffer, area: Rect, theme: &Theme, listening: bool) {
    if listening && area.width >= 12 {
        buf.set_string(
            area.x + 2,
            area.y,
            " \u{25CF} rec ",
            Style::default()
                .fg(theme.accent_error)
                .bg(theme.bg_base)
                .add_modifier(Modifier::BOLD),
        );
    }
}

fn render_dispatch(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    state: &mut DashboardState,
    overlay_area: Option<Rect>,
) -> Option<(u16, u16)> {
    use ratatui::widgets::{Block, BorderType, Borders, Widget};

    use crate::views::prompt_widget::{PromptBg, PromptStyle};

    if area.area() == 0 {
        return None;
    }
    let bg = Style::default().bg(theme.bg_base);
    let fill = " ".repeat(area.width as usize);
    for dy in 0..area.height {
        buf.set_string(area.x, area.y + dy, &fill, bg);
    }

    // Two-focus model: when the overview list is focused (Tab), the
    // input is inactive — dim its border and suppress the caret so the
    // focus cue is unambiguous. `input_focused` drives both.
    let input_focused = !state.list_focused;
    let border_fg = if input_focused {
        theme.selection_border
    } else {
        theme.prompt_border
    };

    // Draw the rounded box and carve out the content rows. The content
    // height tracks the box height so multiline input (Alt+Enter) is
    // fully visible — the box itself grows via
    // `compute_layout_with_dispatch`.
    let content = if area.height >= 3 {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_fg).bg(theme.bg_base));
        let inner = block.inner(area);
        block.render(area, buf);
        // Surface dispatch-validation feedback (e.g. "Too short") as a
        // right-aligned badge on the box's top border so it stays
        // visible even while the rejected text is still in the input.
        paint_dispatch_feedback_badge(buf, area, theme, state.error_toast.as_deref());
        // Bottom-right model + mode indicator, painted through the shared
        // prompt info-line renderer so its style, spacing, and position match
        // the chat prompt's info line exactly. Always shows the model the next
        // spawned agent will use (the `/model`-staged choice, else the current
        // default), plus the staged mode as a flag.
        paint_dispatch_config_badge(buf, area, theme, state, input_focused);
        paint_record_badge(buf, area, theme, state.voice_listening);
        Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: inner.height.max(1),
        }
    } else {
        // Fallback — single line, no chrome.
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        }
    };
    if content.width == 0 {
        return None;
    }

    // Search mode: the prompt is a live filter query, rendered on a
    // single line with a bold yellow `Search:` prefix so it's
    // unmistakable that typing filters rows (Enter confirms) rather than
    // dispatching. No chips / multiline here.
    if state.search_mode {
        let prefix = "Search: ";
        let prefix_w = UnicodeWidthStr::width(prefix) as u16;
        let painted_prefix_w = prefix_w.min(content.width);
        buf.set_span(
            content.x,
            content.y,
            &Span::styled(
                prefix,
                Style::default()
                    .fg(theme.warning)
                    .bg(theme.bg_base)
                    .add_modifier(Modifier::BOLD),
            ),
            painted_prefix_w,
        );
        let editor_x = content.x + painted_prefix_w;
        let avail = content.width - painted_prefix_w;
        let cursor_column = if state.dispatch.text().is_empty() {
            if avail > 0 {
                let placeholder = truncate_str("Type to filter sessions\u{2026}", avail as usize);
                buf.set_string(
                    editor_x,
                    content.y,
                    placeholder,
                    Style::default().fg(theme.gray_dim).bg(theme.bg_base),
                );
            }
            0
        } else {
            let viewport = echo_agent_ratatui_textarea::EditBuffer::from_parts(
                state.dispatch.text(),
                state.dispatch.cursor(),
            )
            .single_line_viewport(avail as usize);
            let visible = &state.dispatch.text()[viewport.visible_byte_range];
            if avail > 0 {
                buf.set_span(
                    editor_x,
                    content.y,
                    &Span::styled(
                        visible,
                        Style::default().fg(theme.text_primary).bg(theme.bg_base),
                    ),
                    (UnicodeWidthStr::width(visible) as u16).min(avail),
                );
            }
            viewport.cursor_display_column as u16
        };
        let cursor_offset = painted_prefix_w
            .saturating_add(cursor_column)
            .min(content.width - 1);
        let cx = content.x + cursor_offset;
        return input_focused.then_some((cx, content.y));
    }

    if content.width < 4 {
        return None;
    }

    let prefix = "\u{276F} ";
    let prefix_w = UnicodeWidthStr::width(prefix) as u16;

    // When voice is active, draw through PromptWidget even on an empty buffer
    // so the manual empty-state branch is skipped.
    let voice_overlay = (state.voice_listening || state.voice_interim.is_some()).then_some(
        crate::views::prompt_widget::VoicePromptOverlay {
            interim: state.voice_interim.as_deref(),
            color: theme.accent_running,
        },
    );

    // Empty input (non-search): paint the `❯` prefix + the contextual
    // placeholder (reply / error toast / new-session) and park the caret
    // at the text start. `PromptWidget::draw` only paints a placeholder
    // when *unfocused*, but we want a visible caret, so the empty state
    // is rendered directly here.
    if state.dispatch.text().is_empty() && voice_overlay.is_none() {
        buf.set_string(
            content.x,
            content.y,
            prefix,
            Style::default().fg(theme.accent_user).bg(theme.bg_base),
        );
        // The dispatch input always spawns a NEW session — never a
        // reply — so the placeholder is constant regardless of which
        // row the overview cursor is on. It only paints while the
        // input is UNFOCUSED (matching `PromptWidget::draw`); when
        // focused the visible caret is the affordance and the text
        // area stays clear. Validation feedback (e.g. "Too short")
        // is surfaced as a badge on the box's top border
        // (`paint_dispatch_feedback_badge`), not here, so it stays
        // visible even when the rejected text is still in the box.
        if !input_focused {
            let msg = "Dispatch a new agent";
            let style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);
            let trunc = truncate_str(msg, content.width.saturating_sub(prefix_w) as usize);
            buf.set_string(content.x + prefix_w, content.y, trunc, style);
        }
        return input_focused.then_some((content.x + prefix_w, content.y));
    }

    // Non-empty input (non-search): shared PromptWidget for cursor,
    // chips, multiline. `chrome: false` keeps the dashboard box;
    // `image_preview: false` keeps image chips without an overlay.
    let style = PromptStyle {
        focused: input_focused,
        show_prefix: true,
        vpad_top: 0,
        chrome: false,
        bg: PromptBg::Canvas(theme.bg_base),
        image_preview: false,
        ..PromptStyle::default()
    };
    state
        .dispatch
        .draw(buf, content, overlay_area, &style, None, voice_overlay)
        .cursor_pos
}

/// Desired number of *text* rows for the dispatch box given its content
/// and the box's outer width. Grows the box for multiline input
/// (Alt+Enter) while capping growth so the row list keeps usable space.
/// Returns ≥1.
fn dispatch_text_rows(state: &DashboardState, dispatch_width: u16, area_height: u16) -> u16 {
    use crate::views::prompt_widget::PromptStyle;

    // Match `render_dispatch`'s content width: box width minus the two
    // border columns and the 1-col inset on each side (`-4` total).
    let content_w = dispatch_width.saturating_sub(4);
    if content_w < 4 {
        return 1;
    }
    // Cap so the box never eats more than ~a third of the panel; the
    // textarea scrolls beyond that.
    let max_text_rows = (area_height / 3).clamp(1, 8);
    let style = PromptStyle {
        focused: true,
        show_prefix: true,
        vpad_top: 0,
        chrome: false,
        ..PromptStyle::default()
    };
    state
        .dispatch
        .desired_height(content_w, &style, false, max_text_rows)
}

/// Top and bottom border rows of a dispatch dropdown panel.
const DROPDOWN_CHROME_ROWS: u16 = 2;

/// Smallest panel that still carries both borders and one item row.
const MIN_DROPDOWN_PANEL_ROWS: u16 = DROPDOWN_CHROME_ROWS + 1;

/// Render the `/command` completion dropdown above the dispatch box.
/// Mirrors `agent_view`'s slash dropdown chrome. No-op (and clears the
/// stored hit rect) when the dropdown is closed.
fn render_slash_dropdown(
    buf: &mut Buffer,
    area: Rect,
    dispatch_rect: Rect,
    theme: &Theme,
    state: &mut DashboardState,
) {
    use ratatui::widgets::{Clear, Widget};

    use crate::views::slash_dropdown::{desired_item_rows, render_dropdown as render_slash};

    let snap = state.dispatch.slash_snapshot();
    if !snap.open || snap.matches.is_empty() {
        state.slash_dropdown_items_area = None;
        state.slash_dropdown_hit = Default::default();
        return;
    }

    let item_count = snap.matches.len();
    // Height in wrapped lines, not items (see `desired_item_rows`);
    // items render inset 1 col on each side.
    let item_rows = desired_item_rows(&snap.matches, dispatch_rect.width.saturating_sub(2));
    // The bottom is pinned to the input, so a panel taller than the space above it
    // would start above `area` and paint off the buffer.
    let panel_h = item_rows
        .saturating_add(DROPDOWN_CHROME_ROWS)
        .min(dispatch_rect.y.saturating_sub(area.y));
    if panel_h < MIN_DROPDOWN_PANEL_ROWS {
        state.slash_dropdown_items_area = None;
        state.slash_dropdown_hit = Default::default();
        return;
    }
    let top_y = dispatch_rect.y - panel_h;
    let panel_x = dispatch_rect.x;
    let panel_width = dispatch_rect.width;
    if panel_width < 4 {
        state.slash_dropdown_items_area = None;
        state.slash_dropdown_hit = Default::default();
        return;
    }
    let panel_area = Rect {
        x: panel_x,
        y: top_y,
        width: panel_width,
        height: panel_h,
    };

    Clear.render(panel_area, buf);
    buf.set_style(
        panel_area,
        Style::default().fg(theme.text_primary).bg(theme.bg_light),
    );

    let border_style = Style::default().fg(theme.bg_highlight).bg(theme.bg_base);
    let bar: String = "\u{2500}".repeat(panel_width as usize);
    buf.set_string(panel_x, top_y, &bar, border_style);
    buf.set_string(panel_x, top_y + panel_h - 1, &bar, border_style);

    let hint = format!("{item_count}");
    let hint_w = hint.len() as u16;
    if hint_w + 2 <= panel_width {
        let hint_x = panel_x + panel_width - hint_w - 1;
        buf.set_string(
            hint_x,
            top_y,
            &hint,
            Style::default().fg(theme.gray).bg(theme.bg_base),
        );
    }

    let items_x = panel_x + 1;
    let items_width = panel_width.saturating_sub(2);
    let items_area = Rect {
        x: items_x,
        y: top_y + 1,
        width: items_width,
        height: panel_h - DROPDOWN_CHROME_ROWS,
    };
    state.slash_dropdown_hit = render_slash(
        buf,
        items_area,
        &snap,
        state.dispatch.slash_hovered(),
        theme,
    );
    state.slash_dropdown_items_area = Some(items_area);
}

/// Working directory of the agent owning the peeked `row`, used to root
/// the reply's `@` file picker. Top-level rows use their own cwd;
/// subagent rows reply to — and resolve `@paths` against — their parent;
/// roster rows have no local agent (`None`). `None` too when the agent
/// has since vanished.
fn peeked_agent_cwd(
    row: &super::DashboardRowId,
    agents: &IndexMap<AgentId, AgentView>,
) -> Option<std::path::PathBuf> {
    let id = match row {
        super::DashboardRowId::TopLevel(id) => *id,
        super::DashboardRowId::Subagent { parent, .. } => *parent,
        super::DashboardRowId::Roster { .. } => return None,
    };
    agents.get(&id).map(|a| a.session.cwd.clone())
}

/// Render the session-less `@` file-context picker dropdown above the
/// dispatch box. Twin of [`render_slash_dropdown`] with a `k/n` count
/// hint. No-op (and clears the hit rect) when the picker is hidden.
fn render_file_search_dropdown(
    buf: &mut Buffer,
    area: Rect,
    dispatch_rect: Rect,
    theme: &Theme,
    state: &mut DashboardState,
) {
    state.file_search_dropdown_items_area = render_file_search_dropdown_for(
        buf,
        area,
        dispatch_rect,
        theme,
        &mut state.dispatch.file_search,
    );
}

/// Paint a session-less `@` file-context dropdown ABOVE `anchor_rect`
/// (the dispatch box, or the peek box when the reply is the active
/// input) for the given [`FileSearchState`]. Returns the items-area
/// rect for mouse routing, or `None` when nothing was drawn (hidden,
/// empty, or no room above the anchor).
fn render_file_search_dropdown_for(
    buf: &mut Buffer,
    area: Rect,
    anchor_rect: Rect,
    theme: &Theme,
    file_search: &mut crate::views::file_search::FileSearchState,
) -> Option<Rect> {
    use ratatui::widgets::{Clear, Widget};

    use crate::views::file_search::dropdown::{MAX_DROPDOWN_ROWS, render_dropdown as render_files};

    if !file_search.is_visible() {
        return None;
    }
    let item_count = file_search.result_count();
    let item_rows = (item_count as u16).min(MAX_DROPDOWN_ROWS);
    if item_rows == 0 {
        return None;
    }
    let panel_h = item_rows.saturating_add(2);
    let max_top = anchor_rect.y.saturating_sub(1);
    if max_top <= area.y {
        return None;
    }
    let top_y = max_top.saturating_sub(panel_h - 1);
    if top_y < area.y {
        return None;
    }
    let panel_x = anchor_rect.x;
    let panel_width = anchor_rect.width;
    if panel_width < 4 {
        return None;
    }

    // Keep the selected row inside the visible window before painting.
    file_search.ensure_visible(item_rows as usize);

    let panel_area = Rect {
        x: panel_x,
        y: top_y,
        width: panel_width,
        height: panel_h,
    };
    Clear.render(panel_area, buf);
    buf.set_style(
        panel_area,
        Style::default().fg(theme.text_primary).bg(theme.bg_light),
    );

    let border_style = Style::default().fg(theme.bg_highlight).bg(theme.bg_base);
    let bar: String = "\u{2500}".repeat(panel_width as usize);
    buf.set_string(panel_x, top_y, &bar, border_style);
    buf.set_string(panel_x, top_y + panel_h - 1, &bar, border_style);

    let (k, n) = (file_search.result_count(), file_search.total_items());
    let hint = if k >= 1000 {
        format!("1k+/{n}")
    } else {
        format!("{k}/{n}")
    };
    let hint_w = hint.len() as u16;
    if hint_w + 2 <= panel_width {
        let hint_x = panel_x + panel_width - hint_w - 1;
        buf.set_string(
            hint_x,
            top_y,
            &hint,
            Style::default().fg(theme.gray).bg(theme.bg_base),
        );
    }

    let items_x = panel_x + 1;
    let items_width = panel_width.saturating_sub(2);
    let items_area = Rect {
        x: items_x,
        y: top_y + 1,
        width: items_width,
        height: item_rows,
    };
    render_files(buf, items_area, file_search, theme);
    Some(items_area)
}

/// Render the dashboard's footer / shortcuts hint row.
///
/// Switched to the shared `ShortcutsBar` widget so
/// the dashboard's shortcut bar uses the same visual vocabulary as
/// the agent view's bottom bar: `Key:label` with bold keys + dim
/// ` │ ` separators on `bg_base`, instead of the previous custom
/// `key label · key label` gray-only string. When a
/// stop-confirm is armed, `ShortcutsBar::with_pending` paints the
/// `press again to {label}` hint in place of the regular list,
/// matching the agent view's identical mechanism.
///
/// Keys are resolved from the action registry every
/// frame so user rebindings show up immediately.
/// Footer hints flip based on the selected row's
/// state:
///
/// - `NeedsInput` selection → `Enter:see details · Ctrl+x:stop · ?:shortcuts`
///   (no inline approve/reject yet — punted per the user's note
///   "maybe its just easier to hit enter and go details view";
///   the dashboard is intentionally a navigator, not a permission UI).
/// - Anything else → `Enter:open · Ctrl+x:stop|delete · ?:shortcuts`.
///
/// The Ctrl+x chip label follows the selected agent's state: `stop`
/// for an agent with a live turn (Working, or NeedsInput — paused but
/// still running, so the first Ctrl+x cancels), `delete` otherwise.
#[allow(clippy::too_many_arguments)]
fn render_footer(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    state: &DashboardState,
    registry: &crate::actions::ActionRegistry,
    selected_state: Option<RowState>,
    peek_active: bool,
    pending_hint: Option<crate::views::shortcuts_bar::PendingHint>,
) {
    use ratatui::widgets::Widget;

    use crate::input::key::{KeyShortcut, key};
    use crate::views::shortcuts_bar::{HintItem, PendingHint, ShortcutsBar};

    if area.area() == 0 {
        return;
    }

    // 2-col left padding to match the agent view's footer position
    // (`block_pad_left = 2`).
    const FOOTER_PAD_LEFT: u16 = 2;
    let _ = theme;
    let inner = Rect {
        x: area.x.saturating_add(FOOTER_PAD_LEFT),
        y: area.y,
        width: area.width.saturating_sub(FOOTER_PAD_LEFT),
        height: area.height,
    };

    // App-level double-press confirmation (quit via Ctrl+Q / Ctrl+C /
    // Ctrl+D) takes precedence over the dashboard-local stop-confirm —
    // both render through `with_pending`. Without this the keys set a
    // pending quit but the dashboard showed no "press again" feedback.
    if let Some(pending) = pending_hint {
        ShortcutsBar::new(&[])
            .with_pending(Some(pending))
            .render(inner, buf);
        return;
    }

    // A live delete-confirm owns the footer: `y`/`n` when the list is
    // focused, else the second-`Ctrl+X` "press again" hint. An expired arm
    // falls through to the normal hints.
    if state.armed_delete_row_ref().is_some() {
        if state.list_focused {
            let hints = vec![
                HintItem::new(key!('y'), "confirm delete"),
                HintItem::new(key!('n'), "cancel"),
            ];
            ShortcutsBar::new(&hints)
                .compact(4, None)
                .render(inner, buf);
        } else {
            let stop_key = registry
                .find(crate::actions::ActionId::DashboardStop)
                .map(|d| d.default_key)
                .unwrap_or_else(|| key!('x', CONTROL));
            let pending = PendingHint {
                shortcut: stop_key,
                label: "delete this session",
            };
            ShortcutsBar::new(&[])
                .with_pending(Some(pending))
                .render(inner, buf);
        }
        return;
    }

    // An in-flight rename owns the keyboard (`handle_key` routes to
    // `handle_rename_key` before anything else), so the footer shows
    // exactly its two actions instead of the dispatch + nav matrix.
    if state.rename.is_some() {
        let hints = vec![
            HintItem::new(key!(Enter), "save"),
            HintItem::new(key!(Esc), "cancel"),
        ];
        ShortcutsBar::new(&hints)
            .compact(4, None)
            .render(inner, buf);
        return;
    }

    // Search mode owns the footer — show how to confirm / cancel the
    // live filter rather than the dispatch + nav matrix.
    if state.search_mode {
        let hints = vec![
            HintItem::paired(key!(Up), key!(Down), "nav"),
            HintItem::new(key!(Enter), "apply"),
            HintItem::new(key!(Esc), "cancel"),
        ];
        ShortcutsBar::new(&hints)
            .compact(4, None)
            .render(inner, buf);
        return;
    }

    let show_ctrl_x = selected_state.is_some_and(|s| {
        matches!(s, RowState::Working | RowState::NeedsInput) || s.allows_delete()
    });
    let stop_label = if matches!(
        selected_state,
        Some(RowState::Working | RowState::NeedsInput)
    ) {
        "stop"
    } else {
        "delete"
    };

    // Overview list focused (via Tab) — navigation hints: arrows / j-k
    // move between agents, Enter opens the focused one, Tab returns to
    // the input. Surfaced so the two-focus model is discoverable.
    //
    // Skipped while peek is open: peek owns Enter (and may disagree with
    // the dispatch list_focused flag — especially vim unfocused reply),
    // so fall through to the peek_active branch below.
    if state.list_focused && !peek_active {
        let key_for = |id: crate::actions::ActionId, fallback: KeyShortcut| -> KeyShortcut {
            registry.find(id).map(|d| d.default_key).unwrap_or(fallback)
        };
        let stop = key_for(crate::actions::ActionId::DashboardStop, key!('x', CONTROL));
        let help = key_for(
            crate::actions::ActionId::DashboardShortcutsHelp,
            key!('.', CONTROL),
        );
        // The ↑/↓ (and vim j/k) nav chip is intentionally omitted — the
        // overview is obviously arrow-navigable, and dropping it frees
        // the bottom bar for the action hints (open / stop).
        // Idle overflow toggle under the cursor — Enter reveals / re-folds
        // the older idle agents, so `open` / `stop` would lie. The nav chip
        // is omitted here too, mirroring the section-header row below.
        if state.selected_idle_overflow {
            let toggle = if state.idle_show_all {
                "show fewer"
            } else {
                "show all"
            };
            let hints = vec![
                HintItem::new(key!(Enter), toggle),
                HintItem::new(key!(Tab), "input"),
            ];
            ShortcutsBar::new(&hints)
                .compact(4, Some(HintItem::new(help, "shortcuts")))
                .render(inner, buf);
            return;
        }
        // Section header under the cursor — Enter toggles the section,
        // not a row, so `open` / `stop` would lie. Tab hands focus back
        // to the dispatch input (Esc does too, one tier at a time).
        if let Some(section) = state.selected_section {
            let toggle = if state.is_section_collapsed(section) {
                "expand"
            } else {
                "collapse"
            };
            let hints = vec![
                HintItem::new(key!(Enter), toggle),
                HintItem::new(key!(Tab), "input"),
            ];
            ShortcutsBar::new(&hints)
                .compact(4, Some(HintItem::new(help, "shortcuts")))
                .render(inner, buf);
            return;
        }
        let mut hints = vec![
            HintItem::new(key!(Enter), "open"),
            HintItem::new(key!(Tab), "input"),
        ];
        if show_ctrl_x {
            hints.push(HintItem::new(stop, stop_label).pinned());
        }

        ShortcutsBar::new(&hints)
            .compact(4, Some(HintItem::new(help, "shortcuts")))
            .render(inner, buf);
        return;
    }

    let resolve = |id: crate::actions::ActionId, fallback: KeyShortcut| -> KeyShortcut {
        registry.find(id).map(|d| d.default_key).unwrap_or(fallback)
    };
    let enter = key!(Enter);
    // "Send + open" is `Ctrl+S` (was `Shift+Enter`, which now inserts a
    // newline). Hardcoded in the dispatch / peek key handlers, not a
    // registry action, so the chip is built directly.
    let send_open = key!('s', CONTROL);
    // Multiline: bare Enter inserts a newline; Shift+Enter (or Alt+Enter
    // when the terminal can't distinguish Shift+Enter) sends — same as
    // the agent prompt keybar.
    let send_key = if state.multiline_mode {
        if crate::terminal::terminal_context().shift_enter_unavailable() {
            key!(Enter, ALT)
        } else {
            key!(Enter, SHIFT)
        }
    } else {
        enter
    };
    let stop = resolve(crate::actions::ActionId::DashboardStop, key!('x', CONTROL));
    let help = resolve(
        crate::actions::ActionId::DashboardShortcutsHelp,
        key!('.', CONTROL),
    );

    let help_hint = HintItem::new(help, "shortcuts");

    // Submit chord is `send_key` (Enter, or Shift/Alt+Enter in multiline).
    // Ctrl+S is send+open. Empty draft: create/open on the submit chord;
    // non-empty: send.
    let button_focused = state.new_agent_button_focused;
    let row_selected = state.selected.is_some();
    let prompt_empty = state.dispatch.text().trim().is_empty();

    let hints: Vec<HintItem> = if peek_active {
        // Peek mode — Enter labels mirror `DashboardState::handle_peek_key`:
        //   vim + unfocused reply → Enter focuses reply ("input")
        //   focused + non-empty   → Enter sends
        //   otherwise             → Enter opens / attaches
        // Ctrl+S remains send+open whenever there is a non-empty draft.
        // Esc: no draft → unselect ("New Agent"); with draft → "back".
        let esc = key!(Esc);
        // Pending permission / ask-tool: `1-9` selects even while unfocused
        // (handler focuses the panel). Focused + selected option → answer.
        // Mirrors `DashboardState::handle_peek_key`.
        let has_pending_question = state
            .peek
            .as_ref()
            .is_some_and(|p| p.question.is_some() && !p.options.is_empty());
        let option_selected = state
            .peek
            .as_ref()
            .is_some_and(|p| p.selected_option.is_some());
        let reply_empty = state.peek_reply.text().trim().is_empty();
        let esc_label = if reply_empty { "New Agent" } else { "back" };
        // Pin Esc when it clears a draft (`back`) so compact doesn't drop it
        // behind stop/help — matches how important it is in handle_peek_key.
        let esc_hint = {
            let h = HintItem::new(esc, esc_label);
            if reply_empty { h } else { h.pinned() }
        };
        let vim_mode = crate::appearance::cache::load_vim_mode();
        // Two-focus model: Tab toggles reply ↔ row nav. Vim opens the
        // reply unfocused so j/k keep selecting.
        let peek_focused = state.peek.as_ref().map(|p| p.focused).unwrap_or(true);
        let question_focused = peek_focused && has_pending_question;
        let tab_hint = HintItem::new(key!(Tab), if peek_focused { "list" } else { "input" });
        // `1-9 select` hint for the question picker (no single bound key).
        let select_hint = HintItem {
            keys: vec![],
            label: "select".into(),
            custom_display: Some("1-9"),
            description: None,
            pinned: false,
        };
        if question_focused && option_selected {
            // An option is selected — the panel is an answer surface:
            // Enter answers; `Tab` unfocuses to the row list (the same
            // two-focus toggle the other peek states surface). (↑/↓ still
            // move within the options; the nav chip is dropped to save
            // bottom-bar space.)
            vec![HintItem::new(enter, "answer"), tab_hint, esc_hint]
        } else if has_pending_question && peek_focused {
            // Question pending, focused, nothing selected — navigation + select.
            let mut h = vec![
                HintItem::new(enter, "open"),
                select_hint,
                tab_hint,
                esc_hint,
            ];
            if show_ctrl_x {
                h.push(HintItem::new(stop, stop_label).pinned());
            }
            h
        } else if vim_mode && !peek_focused {
            // Vim unfocused: Enter focuses the reply (not open/send).
            // Right still attaches — surface it so open stays discoverable.
            // Pending question: keep 1-9 select (digits still work unfocused).
            let mut h = vec![
                HintItem::new(enter, "input"),
                // Pin open: attach is the replacement for Enter in this mode.
                HintItem::new(key!(Right), "open").pinned(),
                tab_hint,
                esc_hint,
            ];
            if has_pending_question {
                h.insert(2, select_hint);
            }
            if !reply_empty {
                h.insert(1, HintItem::new(send_open, "send+open"));
            }
            if show_ctrl_x {
                h.push(HintItem::new(stop, stop_label).pinned());
            }
            h
        } else if has_pending_question {
            // Non-vim unfocused (or other) with a pending question — open + select.
            let mut h = vec![
                HintItem::new(enter, "open"),
                select_hint,
                tab_hint,
                esc_hint,
            ];
            if show_ctrl_x {
                h.push(HintItem::new(stop, stop_label).pinned());
            }
            h
        } else if peek_focused && !reply_empty {
            vec![
                HintItem::new(send_key, "send"),
                HintItem::new(send_open, "send+open"),
                tab_hint,
                HintItem::new(esc, "back").pinned(),
            ]
        } else {
            // Focused empty: open is on the submit chord (send_key). Unfocused:
            // bare Enter still attaches.
            let open_key = if peek_focused { send_key } else { enter };
            let mut h = vec![HintItem::new(open_key, "open"), tab_hint, esc_hint];
            if show_ctrl_x {
                h.push(HintItem::new(stop, stop_label).pinned());
            }
            h
        }
    } else if let Some(section) = state.selected_section {
        // A section header is selected. No stop chip in either state —
        // there's no session under a section header.
        if prompt_empty {
            // ↑↓ navigate, Enter toggles collapse/expand, Esc returns
            // to the `[+ New Agent]` button.
            let toggle = if state.is_section_collapsed(section) {
                "expand"
            } else {
                "collapse"
            };
            vec![
                HintItem::new(enter, toggle),
                HintItem::new(key!(Esc), "New Agent"),
            ]
        } else {
            // Typed text dispatches a NEW agent (a section header is
            // never a reply target), so surface the same chips as the
            // `[+ New Agent]` button with a draft: send_key sends (stays
            // on the dashboard), Ctrl+S sends + opens detail,
            // Shift+Tab cycles the dispatch mode.
            vec![
                HintItem::new(send_key, "send"),
                HintItem::new(send_open, "send+open"),
                HintItem::new(key!(BackTab), "mode"),
            ]
        }
    } else if state.selected_idle_overflow {
        // The Idle overflow toggle is selected. Like a section header,
        // there's no session under it — no stop chip.
        if prompt_empty {
            let toggle = if state.idle_show_all {
                "show fewer"
            } else {
                "show all"
            };
            vec![
                HintItem::new(enter, toggle),
                HintItem::new(key!(Esc), "New Agent"),
            ]
        } else {
            vec![
                HintItem::new(send_key, "send"),
                HintItem::new(send_open, "send+open"),
                HintItem::new(key!(BackTab), "mode"),
            ]
        }
    } else if button_focused {
        let mut h: Vec<HintItem> = vec![];
        if prompt_empty {
            h.push(HintItem::new(send_key, "create"));
            h.push(HintItem::new(key!(Tab), "list"));
        } else {
            h.push(HintItem::new(send_key, "send"));
            h.push(HintItem::new(send_open, "send+open"));
        }
        h.push(HintItem::new(key!(BackTab), "mode"));
        h
    } else if row_selected {
        let mut h: Vec<HintItem> = vec![];
        if prompt_empty {
            h.push(HintItem::new(send_key, "open"));
            h.push(HintItem::new(key!(Tab), "list"));
        } else {
            h.push(HintItem::new(send_key, "send"));
            h.push(HintItem::new(send_open, "send+open"));
        }
        if show_ctrl_x {
            h.push(HintItem::new(stop, stop_label).pinned());
        }
        h
    } else {
        // Defensive — neither the button nor a row is focused.
        vec![HintItem::new(send_key, "create")]
    };

    ShortcutsBar::new(&hints)
        .compact(4, Some(help_hint))
        .render(inner, buf);
}

fn state_icon(state: RowState, tick: u64) -> &'static str {
    match state {
        RowState::Working => {
            let frames = crate::glyphs::dot_spinner_frames();
            let i = (tick / SPINNER_DIVISOR) as usize % frames.len();
            frames[i]
        }
        // Hollow diamond for idle rows; filled diamond for every
        // state that needs visual presence (needs-input, completed,
        // failed, blocked). The foreground colour disambiguates
        // (accent_user for needs-input, accent_success for done,
        // accent_error for failed, warning for blocked).
        RowState::Idle | RowState::Inactive => crate::glyphs::diamond_hollow(),
        RowState::NeedsInput | RowState::Completed | RowState::Failed | RowState::Blocked => {
            crate::glyphs::diamond_filled()
        }
    }
}

fn state_color(state: RowState, theme: &Theme) -> Color {
    match state {
        RowState::Working => theme.accent_running,
        RowState::NeedsInput => theme.warning,
        RowState::Idle | RowState::Inactive => theme.gray_dim,
        RowState::Completed => theme.accent_success,
        RowState::Failed => theme.accent_error,
        RowState::Blocked => theme.warning,
    }
}

/// Bullet colour for a `NeedsInput` row: blinks between full yellow
/// (`warning`) and a dimmed yellow so an agent awaiting input draws the eye.
/// On non-truecolor terminals the dim blend falls back to the full colour
/// (the blink is invisible but the bullet stays yellow).
fn needs_input_bullet_color(tick: u64, theme: &Theme) -> Color {
    let bright = (tick / NEEDS_INPUT_BLINK_DIVISOR).is_multiple_of(2);
    if bright {
        theme.warning
    } else {
        crate::render::color::blend_color(theme.bg_base, theme.warning, 0.5)
            .unwrap_or(theme.warning)
    }
}

fn badge_color(badge: RowBadge, theme: &Theme) -> Color {
    match badge {
        RowBadge::Worktree => theme.accent_user,
        RowBadge::NeedsInput => theme.warning,
        RowBadge::BgTask => theme.command,
        RowBadge::Pinned => theme.accent_running,
        RowBadge::Failed => theme.accent_error,
    }
}

/// process-wide cached `$HOME`. Shared by render
/// and dispatch (`dispatch_dashboard_select`) so we don't re-read the
/// env var on every keystroke. Test-visible reset helper below.
pub(crate) fn cached_home() -> Option<&'static str> {
    HOME.get_or_init(|| std::env::var("HOME").ok().filter(|s| !s.is_empty()))
        .as_deref()
}

static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

// ---------------------------------------------------------------------------
// Popup overlay (banner-style)
// ---------------------------------------------------------------------------

/// Compute the rect for the attached-agent popup overlay.
///
/// The popup now takes the FULL bottom portion of
/// the screen (no horizontal inset, no bottom inset) with only a
/// dynamic top inset reserved for the dashboard banner.
///
/// The previous ~1/6-inset-on-all-sides design left the dashboard's
/// dispatch input + footer visible BELOW the popup, producing two
/// stacked input bars. The banner above the popup carries the live
/// row list in a bordered panel.
///
/// (Historical legacy comment kept for context — the old layout
/// description is no longer accurate; see the body of this function
/// for the current banner-style layout.)
/// terminal yields a ~164×48 popup with a ~36×12 dashboard frame
/// visible around it.
///
/// On terminals too small to honour the minimum inset, falls through
/// to a 0-inset takeover (no escape: any non-zero inset would clip
/// the agent's prompt below readability).
pub fn popup_rect(view: Rect) -> Rect {
    // Popup takes the FULL bottom area (no
    // horizontal inset, no bottom inset) with only a small TOP
    // inset reserved for the dashboard banner that shows the in-flight
    // rows. The previous ~1/6-inset-on-all-sides design left the
    // dashboard's own dispatch input + footer visible BELOW the
    // popup, producing two visible input bars stacked vertically.
    //
    // The banner height is dynamic: ~30% of the screen up to a
    // BANNER_MAX_HEIGHT cap, with a BANNER_MIN_HEIGHT floor on tall
    // terminals so a 1-row banner doesn't crowd the rows out. Very
    // short terminals (height < BANNER_MIN_HEIGHT + 10) drop the
    // banner to 0 so the popup gets every available row.
    const BANNER_MIN_HEIGHT: u16 = 6;
    const BANNER_MAX_HEIGHT: u16 = 14;

    let banner_h: u16 = if view.height >= BANNER_MIN_HEIGHT + 10 {
        (view.height / 3).clamp(BANNER_MIN_HEIGHT, BANNER_MAX_HEIGHT)
    } else {
        0
    };
    Rect {
        x: view.x,
        y: view.y + banner_h,
        width: view.width,
        height: view.height.saturating_sub(banner_h),
    }
}

/// Render the popup overlay frame and the attached agent's view via
/// the canonical [`picker::render_bordered_frame`] — the same chrome
/// primitive used by `agent_view::draw_subagent_fullscreen`. Returns
/// `(cursor_pos, post_flush)` from the agent's draw so the caller
/// can place the cursor on the popup's prompt and merge any
/// kitty/sixel image escape sequences into the frame's outgoing
/// notification stream.
///
/// `title_label` is passed by the caller (rather than computed here
/// from a borrowed `AgentView`) so the closure can take a mutable
/// borrow of the agents map without conflicting with the title
/// lookup.
///
/// Side effects on `state`:
/// - `popup_close_rect` is registered so a click on the `[✗]`
///   affordance can dispatch a popup close in `handle_mouse`.
/// - `popup_outer_rect` is registered so clicks outside the popup
///   can be routed correctly.
///
/// When the popup area is too small for the bordered frame
/// (`area.height < 5` or `area.width < 10`), paints a minimal
/// fallback ("terminal too small: Esc to close") instead of leaving
/// the user staring at an empty box and
/// returns `(None, None, false)`.
pub fn render_popup_overlay(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    title_label: &str,
    state: &mut DashboardState,
    draw_agent: impl FnOnce(
        Rect,
        &mut Buffer,
    ) -> (
        Option<(u16, u16)>,
        Option<crate::terminal::overlay::PostFlush>,
    ),
) -> (
    Option<(u16, u16)>,
    Option<crate::terminal::overlay::PostFlush>,
    bool,
) {
    use ratatui::widgets::{Block, Borders, Clear, Widget};

    if area.area() == 0 {
        state.popup_close_rect = None;
        state.popup_outer_rect = None;
        return (None, None, false);
    }
    state.popup_outer_rect = Some(area);

    let border_color = theme.selection_border;

    // The canonical bordered-frame primitive — used by
    // `AgentView::draw_subagent_fullscreen` in app/agent_view/render.rs (the
    // implementer cited it as the design reference). Paints
    // header (top border + title row), divider (T-junctions), and
    // content frame with full borders. The divider sits ABOVE the
    // returned `content` rect, so `draw_agent` cannot overwrite it.
    let Some(frame) =
        crate::views::picker::render_bordered_frame(buf, area, border_color, theme.bg_base)
    else {
        // Too small for the canonical frame (height < 5 || width < 10).
        // Paint a Clear + outline + single-line hint so the popup
        // still communicates its presence and how to dismiss it.
        //
        // `popup_outer_rect` is set above
        // so the modal-popup mouse intercept in `AppView::handle_input`
        // swallows ALL clicks inside this rect. The agent is never
        // drawn on this branch (we return `(None, None)` below without
        // invoking `draw_agent`), so its hit-area maps stay empty by
        // design — there's nothing for the user to click on inside
        // the fallback box other than the (also-non-functional)
        // border. The user's only exit is Esc / Ctrl+\\ / a wider
        // terminal.
        Clear.render(area, buf);
        buf.set_style(area, Style::default().bg(theme.bg_base));
        let outline = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color).bg(theme.bg_base));
        outline.render(area, buf);
        if area.height >= 3 && area.width >= 6 {
            let hint = truncate_str(
                "(terminal too small: Esc to close)",
                area.width.saturating_sub(2) as usize,
            );
            buf.set_string(
                area.x + 1,
                area.y + area.height / 2,
                hint,
                Style::default().fg(theme.gray_dim).bg(theme.bg_base),
            );
        }
        state.popup_close_rect = None;
        return (None, None, false);
    };

    let title_row = frame.title_row;
    let inner = frame.content;

    let title_text = format!(" \u{2771} {title_label} ");

    let close_label = crate::glyphs::ballot_x_button();
    let close_w = UnicodeWidthStr::width(close_label) as u16;
    // Reserve close-affordance width + a 1-cell gap on the right;
    // `truncate_str` handles overflow with an ellipsis.
    let title_max = title_row.width.saturating_sub(close_w + 2).max(1) as usize;
    let truncated = truncate_str(&title_text, title_max);
    buf.set_string(
        title_row.x,
        title_row.y,
        truncated,
        Style::default()
            .fg(theme.text_primary)
            .bg(theme.bg_base)
            .add_modifier(Modifier::BOLD),
    );

    // Close affordance — register its hit rect so `handle_mouse`
    // can dispatch a popup close on click.
    if close_w + 1 < title_row.width {
        let close_x = title_row.x + title_row.width - close_w;
        buf.set_string(
            close_x,
            title_row.y,
            close_label,
            Style::default().fg(theme.gray).bg(theme.bg_base),
        );
        state.popup_close_rect = Some(Rect {
            x: close_x,
            y: title_row.y,
            width: close_w,
            height: 1,
        });
    } else {
        state.popup_close_rect = None;
    }

    let (cursor, post_flush) = draw_agent(inner, buf);
    (cursor, post_flush, true)
}

/// Paint a bordered frame around an attached agent
/// view, matching the subagent-fullscreen pattern. The frame
/// reserves a title row at the top with `{title}` on the left
/// and `{i}/{n} [‹][›] [Dashboard]` on the right, then a horizontal
/// divider, then the agent's content area.
///
/// Layout:
///
/// ```text
/// ╭─────────────────────────────────────────────────────────╮
/// │ Add responsiveness to /context    1/3 [‹][›] [Dashboard] │
/// ├─────────────────────────────────────────────────────────┤
/// │                                                          │
/// │   <agent.draw paints into here>                          │
/// │                                                          │
/// ╰──────────────────────────────────────────────────────────╯
/// ```
///
/// The agent renders inside the frame's `content` rect — the
/// agent's own shortcuts bar (with the `Ctrl+\\:dashboard` hint, and
/// `Ctrl+[/]:prev/next agent` when the cycle order has more than one
/// agent) sits at the bottom of the content, inside the frame.
///
/// Returns `None` when the area is too small for the bordered
/// frame; the caller falls back to a chromeless render so the
/// user can still use the agent.
///
/// `position` carries `(current_1_based, total)` for the visible
/// row list. When `Some` and `total > 1`, the renderer paints
/// the position indicator + cycle chips; when `None` (or only
/// one row), the cycle chips are omitted. `hover` flags drive
/// per-button highlight so the mouse user gets visual feedback.
#[allow(clippy::too_many_arguments)]
pub fn render_dashboard_session_overlay(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    title_label: &str,
    position: Option<(usize, usize)>,
    hover_prev: bool,
    hover_next: bool,
    hover_close: bool,
) -> Option<DashboardOverlayChrome> {
    if area.area() == 0 || area.height < 5 || area.width < 20 {
        return None;
    }
    let border_color = theme.selection_border;
    let frame =
        crate::views::picker::render_bordered_frame(buf, area, border_color, theme.bg_base)?;
    // `(1, 1)` insets: keep a 1-col gap between the title / chips and
    // the frame's left / right borders.
    let (close_rect, prev_rect, next_rect) = paint_session_title_bar(
        buf,
        frame.title_row,
        theme,
        title_label,
        position,
        hover_prev,
        hover_next,
        hover_close,
        1,
        1,
    );
    Some(DashboardOverlayChrome {
        content: frame.content,
        close_rect,
        prev_rect,
        next_rect,
    })
}

/// Like [`render_dashboard_session_overlay`] but paints ONLY a top
/// header bar — the agent's title on the left and the `{i}/{n} [‹][›]
/// [Dashboard]` affordances on the right — with **no surrounding border**. The
/// returned `content` is `area` minus the header band, so the agent
/// body below renders full-width and its prompt position + overall
/// padding match the dashboard list view (instead of being inset by a
/// modal frame).
///
/// `pad_left` / `pad_right` / `pad_top` mirror the agent body's outer
/// padding (`eff_hpad_left` / `eff_hpad_right` / `eff_outer_vpad`) so
/// the title text lines up with the body's left content edge, the
/// chips line up with its right edge, and the bar gets the same top
/// breathing room as everything below it. The header band is
/// `pad_top` blank rows + one title row; the agent body that fills
/// `content` then applies its OWN top padding, separating it from the
/// header.
#[allow(clippy::too_many_arguments)]
pub fn render_dashboard_session_header(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    title_label: &str,
    position: Option<(usize, usize)>,
    hover_prev: bool,
    hover_next: bool,
    hover_close: bool,
    pad_left: u16,
    pad_right: u16,
    pad_top: u16,
) -> Option<DashboardOverlayChrome> {
    // Need room for the header band (`pad_top` + 1 title row) plus at
    // least one body row, and enough width for the title + chips after
    // the side padding is removed.
    let band_height = pad_top.saturating_add(1);
    if area.area() == 0
        || area.height <= band_height
        || area.width <= pad_left.saturating_add(pad_right).saturating_add(12)
    {
        return None;
    }
    // Fill the whole header band's background (top-padding rows + the
    // title row) so it reads as one contiguous bar flush with the
    // agent body's `bg_base` fill below.
    let fill = " ".repeat(area.width as usize);
    for dy in 0..band_height {
        buf.set_string(
            area.x,
            area.y + dy,
            &fill,
            Style::default().bg(theme.bg_base),
        );
    }
    // Title row sits below the top padding, inset by the side padding
    // so it aligns with the body. `(0, 0)` insets — the boundaries are
    // already encoded in `title_row`.
    let title_row = Rect {
        x: area.x + pad_left,
        y: area.y + pad_top,
        width: area.width.saturating_sub(pad_left + pad_right),
        height: 1,
    };
    let (close_rect, prev_rect, next_rect) = paint_session_title_bar(
        buf,
        title_row,
        theme,
        title_label,
        position,
        hover_prev,
        hover_next,
        hover_close,
        0,
        0,
    );
    let content = Rect {
        x: area.x,
        y: area.y + band_height,
        width: area.width,
        height: area.height.saturating_sub(band_height),
    };
    Some(DashboardOverlayChrome {
        content,
        close_rect,
        prev_rect,
        next_rect,
    })
}

/// Paint the session title bar (title on the left; `{i}/{n} [‹][›] [Dashboard]`
/// affordances on the right) onto a single-row `title_row`, on
/// `bg_base` with no button fills. Returns the three affordance hit
/// rects. Shared by the bordered overlay and the chromeless header bar.
///
/// `left_inset` / `right_inset` reserve that many columns inside
/// `title_row` before the title text (left) and after the chips
/// (right) — the bordered overlay passes `1` on each side for a small
/// gap from its border; the chromeless header passes `0` because its
/// `title_row` is already positioned at the desired padding boundaries.
#[allow(clippy::too_many_arguments)]
fn paint_session_title_bar(
    buf: &mut Buffer,
    title_row: Rect,
    theme: &Theme,
    title_label: &str,
    position: Option<(usize, usize)>,
    hover_prev: bool,
    hover_next: bool,
    hover_close: bool,
    left_inset: u16,
    right_inset: u16,
) -> (Option<Rect>, Option<Rect>, Option<Rect>) {
    // `‹` / `›` / `✗` are all painted as plain bracketed text
    // (no button background fills). Hover only changes the fg
    // color (`text_primary` vs `gray`) for subtle clickability
    // feedback. This matches the style of every other close
    // affordance in the pager's overlays (peek, popups, etc.).
    //
    // U+2039 / U+203A (single guillemets) read as paired nav
    // arrows in monospace fonts at small sizes — softer than
    // the ASCII `<` / `>` and visually closer to the chevrons
    // used in macOS-native pickers.
    // Labelled with its destination ("Dashboard") rather than a
    // generic `[✗]` so it's obvious that clicking it returns to the
    // dashboard. Plain ASCII, so no legacy-console fallback needed.
    let close_label = "[Dashboard]";
    let prev_label = format!("[{}]", crate::glyphs::chevron_left());
    let next_label = format!("[{}]", crate::glyphs::chevron());
    let close_w = UnicodeWidthStr::width(close_label) as u16;
    let prev_w = prev_label.width() as u16;
    let next_w = next_label.width() as u16;

    // Left boundary: nothing paints left of here (title text starts
    // exactly at `left_bound`).
    let left_bound = title_row.x.saturating_add(left_inset);

    // Right edge: paint right-to-left so the rightmost element
    // anchors the edge regardless of which buttons are enabled.
    let mut rx = title_row
        .x
        .saturating_add(title_row.width)
        .saturating_sub(right_inset);

    // [Dashboard] close — plain bracketed text on `bg_base` (NOT a
    // filled button), styled like every other close affordance in the
    // pager (peek panel, popup overlay, etc.). The label spells out
    // its destination so it's clear that clicking it returns to the
    // dashboard. Hover still bumps the fg colour for subtle feedback
    // that it's clickable.
    let close_rect = if rx >= left_bound + close_w {
        rx = rx.saturating_sub(close_w);
        let fg = if hover_close {
            theme.text_primary
        } else {
            theme.gray
        };
        let style = Style::default()
            .fg(fg)
            .bg(theme.bg_base)
            .add_modifier(Modifier::BOLD);
        buf.set_string(rx, title_row.y, close_label, style);
        Some(Rect {
            x: rx,
            y: title_row.y,
            width: close_w,
            height: 1,
        })
    } else {
        None
    };

    let cycle_enabled = position.is_some_and(|(_, n)| n > 1);
    let (prev_rect, next_rect) = if cycle_enabled {
        let mut prev_r = None;
        let mut next_r = None;

        // Paint `[›]` as plain text (no bg), like [Dashboard].
        if rx >= left_bound + 1 + next_w {
            rx = rx.saturating_sub(1 + next_w);
            let fg = if hover_next {
                theme.text_primary
            } else {
                theme.gray
            };
            let style = Style::default()
                .fg(fg)
                .bg(theme.bg_base)
                .add_modifier(Modifier::BOLD);
            buf.set_string(rx, title_row.y, &next_label, style);
            next_r = Some(Rect {
                x: rx,
                y: title_row.y,
                width: next_w,
                height: 1,
            });
        }

        // Paint `[‹]` flush against `[›]` (no separating space)
        // — the pair reads as one tight nav widget rather than
        // two unrelated chips. The space before `[‹]` (separating
        // it from the position indicator) is owned by the
        // indicator's paint call below.
        if rx >= left_bound + prev_w {
            rx = rx.saturating_sub(prev_w);
            let fg = if hover_prev {
                theme.text_primary
            } else {
                theme.gray
            };
            let style = Style::default()
                .fg(fg)
                .bg(theme.bg_base)
                .add_modifier(Modifier::BOLD);
            buf.set_string(rx, title_row.y, &prev_label, style);
            prev_r = Some(Rect {
                x: rx,
                y: title_row.y,
                width: prev_w,
                height: 1,
            });
        }

        // Position indicator `{i}/{n}` — painted to the LEFT of
        // the chips, separated by a single space. Dim foreground
        // so it reads as metadata, not as another clickable
        // chip.
        if let Some((cur, total)) = position {
            let pos_text = format!("{cur}/{total}");
            let pos_w = UnicodeWidthStr::width(pos_text.as_str()) as u16;
            if rx >= left_bound + 1 + pos_w {
                rx = rx.saturating_sub(1 + pos_w);
                let style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);
                buf.set_string(rx, title_row.y, &pos_text, style);
            }
        }
        (prev_r, next_r)
    } else {
        (None, None)
    };

    // Title on the left — plain bold text, no `❱` prefix, starting
    // exactly at `left_bound`. Leave a 1-col gap before whatever sits
    // to its right (chips / position indicator) via the `- 1`.
    if rx > left_bound {
        let title_avail = rx.saturating_sub(left_bound).saturating_sub(1) as usize;
        if title_avail > 0 {
            let trunc = truncate_str(title_label, title_avail);
            buf.set_string(
                left_bound,
                title_row.y,
                trunc,
                Style::default()
                    .fg(theme.text_primary)
                    .bg(theme.bg_base)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }

    (close_rect, prev_rect, next_rect)
}

/// Output of [`render_dashboard_session_overlay`]: the agent's
/// drawing rect (the bordered frame's inner content area) plus
/// the three hit rects for the title-bar affordances.
#[derive(Debug, Clone, Copy)]
pub struct DashboardOverlayChrome {
    pub content: Rect,
    pub close_rect: Option<Rect>,
    pub prev_rect: Option<Rect>,
    pub next_rect: Option<Rect>,
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
