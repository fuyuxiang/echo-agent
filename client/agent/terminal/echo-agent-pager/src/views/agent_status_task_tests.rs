use super::task_status_line;
use crate::theme::Theme;
use crate::views::tasks_pane::TaskStatusCounts;
use ratatui::style::Modifier;
use ratatui::text::Line;

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn hidden_for_zero_counts() {
    let theme = Theme::echo_agent_night();
    assert!(task_status_line(TaskStatusCounts::default(), &theme, false).is_none());
}

#[test]
fn running_is_a_static_diamond_not_a_spinner() {
    let theme = Theme::echo_agent_night();
    let counts = TaskStatusCounts {
        running: 2,
        paused_workflows: 0,
    };
    let first = task_status_line(counts, &theme, false).expect("running line");

    assert_eq!(
        line_text(&first),
        format!("{} 2", crate::glyphs::diamond_filled())
    );
    assert_eq!(first.spans.len(), 1);
    assert_eq!(first.spans[0].style.fg, Some(theme.accent_running));
    assert_eq!(first.spans[0].style.bg, Some(theme.bg_base));
}

#[test]
fn paused_is_static_warning_styled_and_hover_bold() {
    let _theme = crate::theme::cache::pin_theme();
    let theme = Theme::current();
    let counts = TaskStatusCounts {
        running: 0,
        paused_workflows: 3,
    };
    let first = task_status_line(counts, &theme, false).expect("paused line");
    let hovered = task_status_line(counts, &theme, true).expect("paused line");

    assert_eq!(line_text(&first), "P 3");
    assert_eq!(first.spans[0].style.fg, Some(theme.warning));
    assert_eq!(first.spans[0].style.bg, Some(theme.bg_base));
    assert!(hovered.spans[0].style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn mixed_uses_separate_styles_and_neither_animates() {
    let theme = Theme::echo_agent_night();
    let counts = TaskStatusCounts {
        running: 1,
        paused_workflows: 2,
    };
    let line = task_status_line(counts, &theme, false).expect("mixed line");

    assert_eq!(line.spans.len(), 2);
    assert_eq!(line.spans[0].style.fg, Some(theme.accent_running));
    assert_eq!(line.spans[1].style.fg, Some(theme.warning));
    assert_eq!(
        line.spans[0].content,
        format!("{} 1", crate::glyphs::diamond_filled())
    );
    assert_eq!(line.spans[1].content, "  P 2");
}
