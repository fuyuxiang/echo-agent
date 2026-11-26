//! Dense live-tail paint for the dashboard peek middle.
//!
//! Reads the agent's leased [`ScrollbackState`] without mutating fold state,
//! layout cache, follow mode, or view mode. Dashboard lease
//! (`begin_peek_viewport`) already forced follow + AllTurns for attach restore.
//!
//! Density vs full [`ScrollbackPane`]: no sticky headers, no vpad, no gap rows,
//! no horizontal accent/pad chrome. Foldable entries project Collapsed; messages
//! keep full expanded body.
//!
//! Layout (top → bottom):
//! 1. Last user prompt pinned (1 line), when present and height allows
//! 2. Top `…` when the current-turn body is truncated from above
//! 3. Pure tail of content **after** the last user (current turn only)
//!
//! Body is **always** current-turn when a last user exists (including the
//! list-first min-box ~3-row middle). Pin is dropped only when the middle
//! has no rows left for it. After a fresh user send with no agent lines,
//! the middle is pin + empty — prior turns are not pulled up.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::appearance::cache::load_show_thinking_blocks;
use crate::render::SafeBuf;
use crate::scrollback::block::BlockContent;
use crate::scrollback::entry::ScrollbackEntry;
use crate::scrollback::state::ScrollbackState;
use crate::scrollback::types::{BlockLine, DisplayMode};
use crate::theme::Theme;

/// Densified body line count for shrink-to-content (v1: current-turn body).
///
/// Content **after** the last user prompt only. Pin / ellipsis are layout
/// chrome and are budgeted separately in desired peek content.
/// `width` is the middle content width (same as the paint area width).
pub fn densified_body_line_count(scrollback: &ScrollbackState, width: u16) -> u16 {
    if width == 0 || scrollback.is_empty() {
        return 0;
    }
    let after = find_last_user_idx(scrollback).map(|i| i + 1).unwrap_or(0);
    densified_lines_from(scrollback, width, after).len() as u16
}

/// Whether scrollback has a user prompt that dense paint will pin when height
/// allows (drives the pin row in shrink desired content).
pub fn scrollback_has_last_user(scrollback: &ScrollbackState) -> bool {
    find_last_user_idx(scrollback).is_some()
}

/// Paint a dense live tail into `area`.
///
/// Does not call `prepare_layout` / `enable_follow` / `set_view_mode` — those
/// either belong to the viewport lease or would dirty attach-path state.
pub fn paint_peek_live_tail(scrollback: &ScrollbackState, area: Rect, buf: &mut Buffer) {
    if area.width < 1 || area.height == 0 || scrollback.is_empty() {
        return;
    }

    let theme = Theme::current();
    let appearance = scrollback.appearance();
    let cwd = scrollback.cwd();
    let content_w = area.width;
    let height = area.height as usize;

    let last_user = find_last_user_idx(scrollback);
    // Always current-turn when a last user exists; full stream otherwise.
    let body_start = last_user.map(|i| i + 1).unwrap_or(0);
    let flat = densified_lines_from(scrollback, content_w, body_start);

    // Pin when we have a last user and at least one middle row.
    let pin = last_user.and_then(|idx| {
        let entry = scrollback.entry(idx)?;
        let lines = dense_entry_lines(entry, content_w, appearance, cwd);
        lines.into_iter().next()
    });
    let pin_rows = usize::from(pin.is_some());
    let body_budget = height.saturating_sub(pin_rows);
    let (ellipsis, body) = pure_tail_with_ellipsis(flat, body_budget);

    let bg = Style::default().bg(theme.bg_base);
    for row in 0..area.height {
        let y = area.y + row;
        for x in area.x..area.x.saturating_add(area.width) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(bg);
                cell.set_symbol(" ");
            }
        }
    }

    let mut y = area.y;
    if let Some(line) = pin {
        // Mirrors the scrollback pane's content paint so the same rows read
        // the same direction in the peek preview.
        buf.set_line_safe_bidi(area.x, y, &line, content_w);
        y = y.saturating_add(1);
    }
    if ellipsis {
        let style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);
        let ell = Line::from(Span::styled("…", style));
        buf.set_line_safe(area.x, y, &ell, content_w);
        y = y.saturating_add(1);
    }
    for line in &body {
        if y >= area.y.saturating_add(area.height) {
            break;
        }
        buf.set_line_safe_bidi(area.x, y, line, content_w);
        y = y.saturating_add(1);
    }
}

/// Take a pure tail of `flat` into `budget` rows, reserving one row for a top
/// `…` when content is omitted above.
fn pure_tail_with_ellipsis(flat: Vec<Line<'static>>, budget: usize) -> (bool, Vec<Line<'static>>) {
    if budget == 0 {
        return (false, Vec::new());
    }
    if flat.len() <= budget {
        return (false, flat);
    }
    if budget == 1 {
        // No room for both marker and content — keep the live tail line.
        return (false, flat[flat.len() - 1..].to_vec());
    }
    let take = budget - 1;
    (true, flat[flat.len() - take..].to_vec())
}

fn find_last_user_idx(scrollback: &ScrollbackState) -> Option<usize> {
    (0..scrollback.len()).rev().find(|&idx| {
        scrollback
            .entry(idx)
            .is_some_and(|e| e.block.is_user_prompt())
    })
}

/// Densified lines from entry index `start` (inclusive) through the end.
fn densified_lines_from(
    scrollback: &ScrollbackState,
    width: u16,
    start: usize,
) -> Vec<Line<'static>> {
    let appearance = scrollback.appearance();
    let cwd = scrollback.cwd();
    let show_thinking = load_show_thinking_blocks();
    let mut flat = Vec::new();
    for idx in start..scrollback.len() {
        let Some(entry) = scrollback.entry(idx) else {
            continue;
        };
        if entry.is_hidden_thinking(show_thinking) {
            continue;
        }
        flat.extend(dense_entry_lines(entry, width, appearance, cwd));
    }
    flat
}

fn dense_mode(entry: &ScrollbackEntry) -> DisplayMode {
    if entry.is_foldable() {
        entry.block.collapse_mode(entry.is_running)
    } else if entry.block.is_user_prompt() {
        DisplayMode::Collapsed
    } else {
        DisplayMode::Expanded
    }
}

fn dense_entry_lines(
    entry: &ScrollbackEntry,
    width: u16,
    appearance: &crate::appearance::AppearanceConfig,
    cwd: Option<&std::path::Path>,
) -> Vec<Line<'static>> {
    let mode = dense_mode(entry);
    let ctx = entry.context_with_mode(width, mode, appearance, cwd);
    let output = entry.output_with_hooks(&ctx);
    let keep_blanks = !entry.is_foldable();
    output
        .lines
        .into_iter()
        .map(|bl: BlockLine| bl.content)
        .filter(|line| keep_blanks || !line_is_blank(line))
        .collect()
}

fn line_is_blank(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .all(|s| s.content.chars().all(|c| c.is_whitespace()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::entry::ScrollbackEntry;
    use crate::scrollback::state::ScrollbackState;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn filled_buf(area: Rect) -> Buffer {
        Buffer::empty(area)
    }

    fn plain_cells(buf: &Buffer, area: Rect) -> Vec<String> {
        (area.y..area.y + area.height)
            .map(|y| {
                let mut s = String::new();
                for x in area.x..area.x + area.width {
                    if let Some(cell) = buf.cell((x, y)) {
                        s.push_str(cell.symbol());
                    }
                }
                s.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn pure_tail_with_ellipsis_fits_without_marker() {
        let lines: Vec<Line<'static>> = (0..3).map(|i| Line::raw(format!("L{i}"))).collect();
        let (ell, body) = pure_tail_with_ellipsis(lines, 5);
        assert!(!ell);
        assert_eq!(body.len(), 3);
    }

    #[test]
    fn pure_tail_with_ellipsis_takes_tail_and_marks() {
        let lines: Vec<Line<'static>> = (0..10).map(|i| Line::raw(format!("L{i}"))).collect();
        let (ell, body) = pure_tail_with_ellipsis(lines, 4);
        assert!(ell);
        assert_eq!(body.len(), 3);
        assert_eq!(body[0].spans[0].content.as_ref(), "L7");
        assert_eq!(body[2].spans[0].content.as_ref(), "L9");
    }

    #[test]
    fn dense_tail_pins_last_user_at_top() {
        let mut sb = ScrollbackState::new();
        sb.push(ScrollbackEntry::new(RenderBlock::user_prompt(
            "first prompt",
        )));
        sb.push(ScrollbackEntry::new(RenderBlock::agent_message(
            "old answer",
        )));
        sb.push(ScrollbackEntry::new(RenderBlock::user_prompt(
            "latest prompt",
        )));
        sb.push(ScrollbackEntry::new(RenderBlock::tool_call(
            "bash", "tool-a", true,
        )));
        sb.push(ScrollbackEntry::new(RenderBlock::agent_message(
            "new answer",
        )));

        let area = Rect::new(0, 0, 48, 8);
        let mut buf = filled_buf(area);
        paint_peek_live_tail(&sb, area, &mut buf);
        let rows = plain_cells(&buf, area);
        assert!(
            rows[0].contains("latest prompt"),
            "pinned user on first row: {rows:?}"
        );
        assert!(
            !rows[0].contains("first prompt"),
            "older user must not pin: {rows:?}"
        );
        let joined = rows.join("\n");
        assert!(
            joined.contains("new answer") || joined.contains("tool-a"),
            "current turn body under pin: {joined:?}"
        );
        assert!(
            !joined.contains("old answer"),
            "prior-turn body must not fill under pin: {joined:?}"
        );
        assert!(
            !joined.contains("first prompt"),
            "prior user must not appear under pin: {joined:?}"
        );
    }

    #[test]
    fn dense_tail_after_fresh_user_send_body_is_empty() {
        let mut sb = ScrollbackState::new();
        sb.push(ScrollbackEntry::new(RenderBlock::user_prompt("old ask")));
        sb.push(ScrollbackEntry::new(RenderBlock::agent_message(
            "long prior answer with a table and status",
        )));
        sb.push(ScrollbackEntry::new(RenderBlock::user_prompt(
            "say hi to me and nothing else",
        )));

        for h in [3u16, 8] {
            let area = Rect::new(0, 0, 48, h);
            let mut buf = filled_buf(area);
            paint_peek_live_tail(&sb, area, &mut buf);
            let rows = plain_cells(&buf, area);
            assert!(
                rows[0].contains("say hi to me"),
                "h={h}: new user pinned: {rows:?}"
            );
            let body = rows[1..].join("\n");
            assert!(
                !body.contains("prior answer")
                    && !body.contains("old ask")
                    && !body.contains("table"),
                "h={h}: fresh send must not re-show prior turn: {body:?}"
            );
            assert!(
                body.chars().all(|c| c.is_whitespace()) || body.is_empty(),
                "h={h}: body under pin empty until agent streams: {body:?}"
            );
        }
    }

    #[test]
    fn dense_tail_shows_top_ellipsis_when_body_truncated() {
        let mut sb = ScrollbackState::new();
        sb.push(ScrollbackEntry::new(RenderBlock::user_prompt("ask")));
        for i in 0..20 {
            sb.push(ScrollbackEntry::new(RenderBlock::tool_call(
                "bash",
                format!("cmd-{i}"),
                true,
            )));
        }

        let area = Rect::new(0, 0, 40, 6);
        let mut buf = filled_buf(area);
        paint_peek_live_tail(&sb, area, &mut buf);
        let rows = plain_cells(&buf, area);
        assert!(rows[0].contains("ask"), "user pin first: {rows:?}");
        assert!(
            rows[1].contains('…') || rows[1].contains("..."),
            "top ellipsis under pin when truncated: {rows:?}"
        );
        let joined = rows.join("\n");
        assert!(
            joined.contains("cmd-19"),
            "pure tail keeps latest: {joined:?}"
        );
        assert!(
            !joined.contains("cmd-0"),
            "oldest dropped under ellipsis: {joined:?}"
        );
    }

    #[test]
    fn dense_tail_min_box_middle_is_current_turn_with_pin() {
        // List-first min box leaves ~3 middle rows after status/reply/blank.
        let mut sb = ScrollbackState::new();
        sb.push(ScrollbackEntry::new(RenderBlock::user_prompt("old")));
        sb.push(ScrollbackEntry::new(RenderBlock::agent_message(
            "prior turn bulk answer",
        )));
        sb.push(ScrollbackEntry::new(RenderBlock::user_prompt("ask")));
        for i in 0..12 {
            sb.push(ScrollbackEntry::new(RenderBlock::tool_call(
                "bash",
                format!("cmd-{i}"),
                true,
            )));
        }

        let area = Rect::new(0, 0, 40, 3);
        let mut buf = filled_buf(area);
        paint_peek_live_tail(&sb, area, &mut buf);
        let rows = plain_cells(&buf, area);
        let joined = rows.join("\n");
        assert!(rows[0].contains("ask"), "pin on min-box middle: {rows:?}");
        assert!(
            !joined.contains("prior turn") && !joined.contains("old"),
            "prior turn must not fill min-box middle: {joined:?}"
        );
        assert!(
            joined.contains("cmd-11"),
            "current-turn pure tail on min-box middle: {joined:?}"
        );
    }

    #[test]
    fn dense_tail_pure_tail_keeps_message_end() {
        let mut sb = ScrollbackState::new();
        let body = (0..20)
            .map(|i| format!("LINE{i:02}-{}", "x".repeat(36)))
            .collect::<Vec<_>>()
            .join("\n\n");
        sb.push(ScrollbackEntry::new(RenderBlock::agent_message(body)));

        let area = Rect::new(0, 0, 40, 4);
        let mut buf = filled_buf(area);
        paint_peek_live_tail(&sb, area, &mut buf);
        let joined = plain_cells(&buf, area).join("\n");
        assert!(
            joined.contains("LINE19") || joined.contains("LINE18"),
            "pure tail keeps end: {joined:?}"
        );
        assert!(
            joined.contains('…') || !joined.contains("LINE00"),
            "head omitted with ellipsis or absence: {joined:?}"
        );
    }

    #[test]
    fn dense_tail_does_not_mutate_scrollback_viewport() {
        let mut sb = ScrollbackState::new();
        sb.push(ScrollbackEntry::new(RenderBlock::agent_message("hello")));
        let before = sb.capture_viewport_snapshot();
        let area = Rect::new(0, 0, 40, 4);
        let mut buf = filled_buf(area);
        paint_peek_live_tail(&sb, area, &mut buf);
        let after = sb.capture_viewport_snapshot();
        assert_eq!(before, after);
    }

    #[test]
    fn dense_tail_empty_scrollback_is_noop() {
        let sb = ScrollbackState::new();
        let area = Rect::new(0, 0, 40, 4);
        let mut buf = filled_buf(area);
        paint_peek_live_tail(&sb, area, &mut buf);
        assert!(plain_cells(&buf, area).iter().all(|r| r.is_empty()));
    }

    #[test]
    fn densified_body_line_count_is_current_turn_only() {
        let mut current = ScrollbackState::new();
        current.push(ScrollbackEntry::new(RenderBlock::user_prompt("ask")));
        current.push(ScrollbackEntry::new(RenderBlock::tool_call(
            "bash", "one", true,
        )));
        current.push(ScrollbackEntry::new(RenderBlock::tool_call(
            "bash", "two", true,
        )));
        let current_n = densified_body_line_count(&current, 40);
        assert!(current_n >= 2, "current-turn tools contribute: {current_n}");

        let mut with_prior = ScrollbackState::new();
        with_prior.push(ScrollbackEntry::new(RenderBlock::user_prompt("old")));
        with_prior.push(ScrollbackEntry::new(RenderBlock::agent_message(
            "prior turn bulk that is many lines of text",
        )));
        with_prior.push(ScrollbackEntry::new(RenderBlock::user_prompt("ask")));
        with_prior.push(ScrollbackEntry::new(RenderBlock::tool_call(
            "bash", "one", true,
        )));
        with_prior.push(ScrollbackEntry::new(RenderBlock::tool_call(
            "bash", "two", true,
        )));
        assert_eq!(
            densified_body_line_count(&with_prior, 40),
            current_n,
            "prior turn must not inflate densified body count"
        );

        let mut user_only = ScrollbackState::new();
        user_only.push(ScrollbackEntry::new(RenderBlock::user_prompt("only")));
        assert_eq!(densified_body_line_count(&user_only, 40), 0);
    }
}
