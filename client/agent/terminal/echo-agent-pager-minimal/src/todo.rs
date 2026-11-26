//! Minimal-mode todo panel: the persistent list shown directly above the prompt
//! while a turn has todos.
//!
//! It auto-hides once every todo is done (so a finished list doesn't linger),
//! unless pinned open with `Ctrl+T` ([`todo_panel_visible`]). The overlay host
//! sizes the idle viewport with [`todo_panel_height`] so the prompt sits right
//! after the panel; [`draw_live`](super::live::draw_live) paints it with
//! [`todo_panel_lines`] + [`render_todo_panel`]. Mirrors the full-TUI `TodoPane`
//! glyphs/colors without its interactive chrome.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use echo_agent_pager::theme::Theme;
use echo_agent_runtime::tools::TodoStatus;

/// Default cap on visible todo rows (the last becomes a `+N more` overflow row);
/// `Ctrl+T` expands past it.
pub(super) const MAX_TODO_ROWS: u16 = 8;

/// Whether the todo panel should render this frame. Hidden when there are no
/// todos, or when every todo is finished (so a completed list doesn't linger —
/// nit: "still showing old TODOs on every turn even though all are complete").
/// A new turn that creates fresh pending todos re-shows it immediately. `force`
/// (Ctrl+T) pins it visible regardless, e.g. to review a finished list.
pub(super) fn todo_panel_visible(
    agent: &echo_agent_pager::app::agent_view::AgentView,
    force: bool,
) -> bool {
    let todos = agent.todo.todos();
    if todos.is_empty() {
        return false;
    }
    if force {
        return true;
    }
    todos
        .iter()
        .any(|t| matches!(t.status, TodoStatus::Pending | TodoStatus::InProgress))
}

/// Rows the todo panel will occupy (0 when hidden — see [`todo_panel_visible`] —
/// or there are no todos), capped at [`MAX_TODO_ROWS`]. The overlay host uses
/// this to size the idle viewport to exactly its content so the prompt sits
/// right after the committed conversation (no bottom-pin, no gap).
pub(super) fn todo_panel_height(
    agent: &echo_agent_pager::app::agent_view::AgentView,
    force: bool,
) -> u16 {
    if !todo_panel_visible(agent, force) {
        return 0;
    }
    let len = agent.todo.todos().len() as u16;
    // Ctrl+T (force) expands the full list (clamped to the screen by the caller);
    // otherwise cap at `MAX_TODO_ROWS` with a `+N more` overflow row.
    if force { len } else { len.min(MAX_TODO_ROWS) }
}

/// Render the persistent todo panel into `area` (one line per item). Background
/// is reset so the panel inherits the terminal's own background (transparency),
/// matching the rest of the minimal live region.
pub(super) fn render_todo_panel(
    buf: &mut Buffer,
    area: Rect,
    theme: &Theme,
    lines: &[Line<'static>],
) {
    buf.set_style(area, theme.muted().bg(Color::Reset));
    for (i, line) in lines.iter().enumerate() {
        let y = area.y + i as u16;
        if y >= area.y + area.height {
            break;
        }
        buf.set_line(area.x, y, line, area.width);
    }
}

/// Build the persistent todo-panel lines (status glyph + content per item),
/// shown directly above the prompt while there are todos. Capped to `max_rows`
/// (the last row becomes `… +N more` on overflow). Empty when there are no
/// todos. Mirrors the full-TUI `TodoPane`'s glyphs/colors.
pub(super) fn todo_panel_lines(
    agent: &echo_agent_pager::app::agent_view::AgentView,
    max_rows: u16,
    force: bool,
) -> Vec<Line<'static>> {
    let todos = agent.todo.todos();
    if todos.is_empty() || max_rows == 0 {
        return Vec::new();
    }
    let theme = Theme::current();
    let cap = max_rows as usize;
    let overflow = todos.len() > cap;
    // Leave the last row for the overflow marker when truncating.
    let shown = if overflow {
        cap.saturating_sub(1)
    } else {
        todos.len()
    };

    let mut lines: Vec<Line<'static>> = todos
        .iter()
        .take(shown)
        .map(|t| {
            let (glyph, style) = match t.status {
                TodoStatus::Pending => ("\u{25a1}", Style::default().fg(theme.text_primary)),
                TodoStatus::InProgress => (
                    "\u{25b6}",
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                ),
                TodoStatus::Completed => (echo_agent_pager::glyphs::check_mark(), theme.muted()),
                TodoStatus::Cancelled => (
                    echo_agent_pager::glyphs::ballot_x(),
                    theme.muted().add_modifier(Modifier::CROSSED_OUT),
                ),
            };
            let content = truncate_chars(t.content.lines().next().unwrap_or("").trim(), 64);
            // No leading pad: the caller places the panel at the shared
            // live-region left edge (`live::live_left_inset` = 0, flush-left),
            // so the glyph
            // column lines up with committed `◆` bullets and the prompt `❯`.
            Line::from(vec![
                Span::styled(format!("{glyph} "), style),
                Span::styled(content, style),
            ])
        })
        .collect();

    if overflow {
        let remaining = todos.len() - shown;
        // When collapsed, advertise the chord that expands the full list; when
        // already forced open (still overflowing a tiny screen) drop the hint.
        let label = if force {
            format!("\u{2026} +{remaining} more")
        } else {
            format!("\u{2026} +{remaining} more \u{00b7} ctrl+t to expand")
        };
        lines.push(Line::from(Span::styled(label, theme.dim())));
    }
    lines
}

/// Truncate `s` to at most `max` characters, appending `…` when shortened.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_pager::minimal_api;
    use echo_agent_runtime::tools::{TodoItem, TodoPriority};

    fn agent() -> echo_agent_pager::app::agent_view::AgentView {
        minimal_api::test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"))
    }

    fn todo(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.into(),
            priority: TodoPriority::default(),
            status,
            meta: None,
        }
    }

    /// Plain text of a rendered line (span contents concatenated).
    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn todo_panel_visibility_auto_hides_when_work_is_done() {
        use echo_agent_pager::app::agent::AgentState;
        let mut a = agent();
        // No todos → hidden.
        assert!(!todo_panel_visible(&a, false));

        // At least one unfinished todo → shown.
        a.todo.update_todos(vec![
            todo("done", TodoStatus::Completed),
            todo("doing", TodoStatus::InProgress),
        ]);
        assert!(todo_panel_visible(&a, false));

        // All completed + idle → auto-hidden (don't linger forever).
        a.todo.update_todos(vec![
            todo("a", TodoStatus::Completed),
            todo("b", TodoStatus::Completed),
        ]);
        assert!(
            !todo_panel_visible(&a, false),
            "auto-hide once every todo is done and the turn is idle"
        );

        // …and stays hidden even while a turn is actively running, so a previous
        // turn's finished list never lingers at the start of the next turn.
        a.session.state = AgentState::TurnRunning;
        assert!(
            !todo_panel_visible(&a, false),
            "all-complete list hides even mid-turn"
        );

        // The Ctrl+T force-show pin overrides the auto-hide.
        a.session.state = AgentState::Idle;
        assert!(
            todo_panel_visible(&a, true),
            "Ctrl+T pin keeps a finished list visible"
        );
    }

    #[test]
    fn todo_panel_empty_when_no_todos() {
        assert!(todo_panel_lines(&agent(), 8, false).is_empty());
        // …and empty when the cap is zero, regardless of todos.
        let mut a = agent();
        a.todo.update_todos(vec![todo("x", TodoStatus::Pending)]);
        assert!(todo_panel_lines(&a, 0, false).is_empty());
    }

    #[test]
    fn todo_panel_lists_items_with_status_glyphs() {
        let mut agent = agent();
        agent.todo.update_todos(vec![
            todo("done one", TodoStatus::Completed),
            todo("active item", TodoStatus::InProgress),
            todo("later", TodoStatus::Pending),
        ]);
        let lines = todo_panel_lines(&agent, 8, false);
        assert_eq!(lines.len(), 3);
        assert!(line_text(&lines[0]).contains("done one"));
        assert!(
            line_text(&lines[1]).contains("\u{25b6}"),
            "in-progress row uses the ▶ glyph"
        );
        assert!(line_text(&lines[1]).contains("active item"));
        assert!(
            line_text(&lines[2]).contains("\u{25a1}"),
            "pending row uses the □ glyph"
        );
    }

    #[test]
    fn todo_panel_caps_with_overflow_row() {
        let mut agent = agent();
        agent.todo.update_todos(
            (0..10)
                .map(|i| todo(&format!("item {i}"), TodoStatus::Pending))
                .collect(),
        );
        let lines = todo_panel_lines(&agent, 4, false);
        assert_eq!(lines.len(), 4, "capped to max_rows");
        // 3 items + a "+7 more" overflow row (10 total, 3 shown), with a hint.
        assert!(
            line_text(&lines[3]).contains("+7 more"),
            "got: {:?}",
            line_text(&lines[3])
        );
        assert!(
            line_text(&lines[3]).contains("ctrl+t"),
            "overflow row advertises the expand chord: {:?}",
            line_text(&lines[3])
        );
    }

    #[test]
    fn truncate_chars_adds_ellipsis_only_when_needed() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello world", 5), "hell…");
    }
}
