use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;
use crate::views::prompt_widget::StashedPrompt;

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RewindPointInfo {
    #[serde(alias = "promptIndex")]
    pub prompt_index: usize,
    #[serde(default, alias = "createdAt")]
    pub created_at: String,
    #[serde(default, alias = "numFileSnapshots")]
    pub num_file_snapshots: usize,
    #[serde(default, alias = "promptPreview")]
    pub prompt_preview: Option<String>,
    #[serde(default, alias = "hasFileChanges")]
    pub has_file_changes: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RewindPointsResponse {
    #[serde(alias = "rewindPoints")]
    pub rewind_points: Vec<RewindPointInfo>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RewindResponse {
    pub success: bool,
    #[serde(alias = "targetPromptIndex")]
    pub target_prompt_index: usize,
    #[serde(default, alias = "revertedFiles")]
    pub reverted_files: Vec<String>,
    #[serde(default, alias = "cleanFiles")]
    pub clean_files: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<RewindConflictInfo>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default, alias = "promptText")]
    pub prompt_text: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RewindConflictInfo {
    pub path: String,
    #[serde(alias = "conflictType")]
    pub conflict_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindPhase {
    Loading,
    Picker {
        points: Vec<RewindPointInfo>,
        selected: usize,
    },
    CancelOffer {
        active_idx: usize,
    },
    /// Confirm before executing a conversation-only rewind.
    Confirm {
        target_prompt_index: usize,
        active_idx: usize,
        prompt_preview: Option<String>,
    },
    Executing {
        target_prompt_index: usize,
    },
    Error {
        message: String,
    },
}

#[derive(Debug)]
pub struct RewindState {
    pub phase: RewindPhase,
    pub anchor_entry_idx: usize,
    pub stashed_draft: Option<StashedPrompt>,
    pub selected_prompt_index: Option<usize>,
}

impl RewindState {
    pub fn new_cancel_offer(
        anchor: usize,
        draft: Option<StashedPrompt>,
        selected_prompt_index: Option<usize>,
    ) -> Self {
        Self {
            phase: RewindPhase::CancelOffer { active_idx: 0 },
            anchor_entry_idx: anchor,
            stashed_draft: draft,
            selected_prompt_index,
        }
    }
}

pub enum RewindInput {
    Dismissed,
    CancelTurnThenProceed,
    DismissError,
    Confirm(usize),
    /// Execute this rewind and turn off confirm-before-rewind.
    ConfirmNeverAsk(usize),
    PickerSelect(usize),
    MoveUp,
    MoveDown,
    ConfirmCursor,
    Consumed,
}

const CANCEL_OFFER_OPTIONS: usize = 2;
/// Yes / Yes, and don't ask again / No.
const CONFIRM_OPTIONS: usize = 3;

pub fn handle_rewind_key(state: &RewindState, key: &KeyEvent) -> RewindInput {
    if key.kind == crossterm::event::KeyEventKind::Release {
        return RewindInput::Consumed;
    }
    match &state.phase {
        RewindPhase::Picker { points, selected } => match key.code {
            KeyCode::Char('j') | KeyCode::Down => RewindInput::MoveDown,
            KeyCode::Char('k') | KeyCode::Up => RewindInput::MoveUp,
            KeyCode::Enter => {
                if let Some(p) = points.get(*selected) {
                    RewindInput::PickerSelect(p.prompt_index)
                } else {
                    RewindInput::Consumed
                }
            }
            KeyCode::Esc => RewindInput::Dismissed,
            _ => RewindInput::Consumed,
        },
        RewindPhase::CancelOffer { .. } => match key.code {
            KeyCode::Char('y') => RewindInput::CancelTurnThenProceed,
            KeyCode::Char('n') => RewindInput::Dismissed,
            KeyCode::Char('j') | KeyCode::Down => RewindInput::MoveDown,
            KeyCode::Char('k') | KeyCode::Up => RewindInput::MoveUp,
            KeyCode::Enter => RewindInput::ConfirmCursor,
            KeyCode::Esc => RewindInput::Dismissed,
            _ => RewindInput::Consumed,
        },
        RewindPhase::Confirm {
            target_prompt_index,
            ..
        } => match key.code {
            KeyCode::Char('y') => RewindInput::Confirm(*target_prompt_index),
            KeyCode::Char('n') => RewindInput::Dismissed,
            KeyCode::Char('a') => RewindInput::ConfirmNeverAsk(*target_prompt_index),
            KeyCode::Char('j') | KeyCode::Down => RewindInput::MoveDown,
            KeyCode::Char('k') | KeyCode::Up => RewindInput::MoveUp,
            KeyCode::Enter => RewindInput::ConfirmCursor,
            KeyCode::Esc => RewindInput::Dismissed,
            _ => RewindInput::Consumed,
        },
        RewindPhase::Error { .. } => match key.code {
            KeyCode::Esc | KeyCode::Enter => RewindInput::DismissError,
            _ => RewindInput::Consumed,
        },
        RewindPhase::Loading => match key.code {
            KeyCode::Esc => RewindInput::Dismissed,
            _ => RewindInput::Consumed,
        },
        RewindPhase::Executing { .. } => RewindInput::Consumed,
    }
}

pub fn move_cursor(phase: &mut RewindPhase, delta: i32) {
    match phase {
        RewindPhase::Picker { points, selected } => {
            if points.is_empty() {
                return;
            }
            let max = points.len() as i32 - 1;
            let new = (*selected as i32 + delta).clamp(0, max);
            *selected = new as usize;
        }
        RewindPhase::CancelOffer { active_idx } => {
            let new = (*active_idx as i32 + delta).clamp(0, CANCEL_OFFER_OPTIONS as i32 - 1);
            *active_idx = new as usize;
        }
        RewindPhase::Confirm { active_idx, .. } => {
            let new = (*active_idx as i32 + delta).clamp(0, CONFIRM_OPTIONS as i32 - 1);
            *active_idx = new as usize;
        }
        _ => {}
    }
}

pub fn confirm_cursor(phase: &RewindPhase) -> RewindInput {
    match phase {
        RewindPhase::CancelOffer { active_idx } => match active_idx {
            0 => RewindInput::CancelTurnThenProceed,
            _ => RewindInput::Dismissed,
        },
        RewindPhase::Confirm {
            target_prompt_index,
            active_idx,
            ..
        } => match active_idx {
            0 => RewindInput::Confirm(*target_prompt_index),
            1 => RewindInput::ConfirmNeverAsk(*target_prompt_index),
            _ => RewindInput::Dismissed,
        },
        _ => RewindInput::Consumed,
    }
}

/// Hit-test a screen position against the rewind overlay's clickable rows.
///
/// Returns the logical cursor index under `(col, row)` for the current
/// phase, or `None` if the position is not on a selectable row.
///
/// IMPORTANT: the row geometry here mirrors `render_rewind_overlay`. Keep
/// this, `render_rewind_overlay`, and `rewind_overlay_height` in sync when
/// changing layout.
pub fn rewind_row_at(phase: &RewindPhase, area: Rect, col: u16, row: u16) -> Option<usize> {
    if area.height == 0 || area.width < 10 {
        return None;
    }
    if col < area.x || col >= area.x + area.width {
        return None;
    }
    if row < area.y || row >= area.y + area.height {
        return None;
    }
    match phase {
        RewindPhase::Picker { points, selected } => crate::views::overlay_list::ListOverlay {
            len: points.len(),
            selected: *selected,
        }
        .row_at(area, col, row),
        RewindPhase::CancelOffer { .. } => match row.checked_sub(area.y + 3) {
            Some(0) => Some(0),
            Some(1) => Some(1),
            _ => None,
        },
        RewindPhase::Confirm { .. } => match row.checked_sub(area.y + 2) {
            Some(0) => Some(0),
            Some(1) => Some(1),
            Some(2) => Some(2),
            _ => None,
        },
        RewindPhase::Error { .. } => {
            if row == area.y + 3 {
                Some(0)
            } else {
                None
            }
        }
        RewindPhase::Loading | RewindPhase::Executing { .. } => None,
    }
}

/// Move the overlay cursor/selection to `idx` (used by mouse hover/click).
/// Returns `true` if the stored cursor changed.
pub fn set_rewind_cursor(phase: &mut RewindPhase, idx: usize) -> bool {
    match phase {
        RewindPhase::Picker { points, selected } => {
            if points.is_empty() {
                return false;
            }
            let new = idx.min(points.len() - 1);
            if *selected != new {
                *selected = new;
                true
            } else {
                false
            }
        }
        RewindPhase::CancelOffer { active_idx } => {
            let new = idx.min(CANCEL_OFFER_OPTIONS - 1);
            if *active_idx != new {
                *active_idx = new;
                true
            } else {
                false
            }
        }
        RewindPhase::Confirm { active_idx, .. } => {
            let new = idx.min(CONFIRM_OPTIONS - 1);
            if *active_idx != new {
                *active_idx = new;
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// The activation input for the current cursor position — equivalent to
/// pressing Enter on the focused row. Used by mouse-click handling.
pub fn rewind_activate(phase: &RewindPhase) -> RewindInput {
    match phase {
        RewindPhase::Picker { points, selected } => points
            .get(*selected)
            .map(|p| RewindInput::PickerSelect(p.prompt_index))
            .unwrap_or(RewindInput::Consumed),
        RewindPhase::Error { .. } => RewindInput::DismissError,
        other => confirm_cursor(other),
    }
}

pub fn rewind_overlay_height(phase: &RewindPhase, screen_h: u16) -> u16 {
    let content = match phase {
        RewindPhase::Loading => 2,
        RewindPhase::Picker { points, selected } => {
            return crate::views::overlay_list::ListOverlay {
                len: points.len(),
                selected: *selected,
            }
            .height(screen_h);
        }
        RewindPhase::CancelOffer { .. } => 5,
        RewindPhase::Executing { .. } => 2,
        RewindPhase::Confirm { .. } => 5,
        RewindPhase::Error { .. } => 4,
    };
    content + 1
}

pub fn render_rewind_overlay(buf: &mut Buffer, area: Rect, phase: &RewindPhase, focused: bool) {
    if area.height == 0 || area.width < 10 {
        return;
    }

    let theme = Theme::current();
    let bg = theme.bg_light;

    buf.set_style(area, Style::default().bg(bg));

    let accent_style = Style::default().fg(theme.accent_user);
    for row in area.y..area.y + area.height {
        if let Some(cell) = buf.cell_mut((area.x, row)) {
            cell.set_symbol(crate::glyphs::accent_bar());
            cell.set_style(accent_style);
        }
    }

    let content_x = area.x + 3;
    let content_w = area.width.saturating_sub(5);

    let title_style = Style::default()
        .fg(theme.accent_user)
        .add_modifier(Modifier::BOLD);

    match phase {
        RewindPhase::Loading => {
            let y = area.y + 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    "Loading rewind points...",
                    Style::default().fg(theme.gray),
                )),
                content_w,
            );
        }
        RewindPhase::Picker { points, selected } => {
            // Shared list-overlay chrome + row geometry (also used by /jump).
            // It applies the unfocus dim itself, so return before the shared
            // blend at the bottom of this function.
            crate::views::overlay_list::ListOverlay {
                len: points.len(),
                selected: *selected,
            }
            .render(buf, area, "Rewind to which turn?", focused, |i, ctx| {
                let point = &points[i];
                let dot_style = Style::default().fg(theme.gray).bg(ctx.row_bg);
                let preview: String = crate::render::line_utils::truncate_str(
                    point.prompt_preview.as_deref().unwrap_or("(no preview)"),
                    ctx.content_width.saturating_sub(8) as usize,
                );
                let text_style = Style::default()
                    .fg(theme.text_primary)
                    .bg(ctx.row_bg)
                    .add_modifier(if ctx.is_cursor {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    });

                Line::from(vec![
                    Span::styled("\u{00B7} ", dot_style),
                    Span::styled(preview, text_style),
                ])
            });
            return;
        }
        RewindPhase::CancelOffer { active_idx } => {
            let mut y = area.y + 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled("A turn is currently running.", title_style)),
                content_w,
            );
            y += 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    "Would you like to cancel it before rewinding?",
                    Style::default().fg(theme.gray),
                )),
                content_w,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'y',
                "Cancel turn and rewind",
                *active_idx == 0,
                focused,
                &theme,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'n',
                "Let it finish",
                *active_idx == 1,
                focused,
                &theme,
            );
        }
        RewindPhase::Executing { .. } => {
            let y = area.y + 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    "Rewinding...",
                    Style::default().fg(theme.gray),
                )),
                content_w,
            );
        }
        RewindPhase::Confirm {
            active_idx,
            prompt_preview,
            ..
        } => {
            let mut y = area.y + 1;
            let preview_text = prompt_preview.as_deref().unwrap_or("this turn");
            let prefix = "Rewind conversation to \u{201C}";
            let suffix = "\u{201D}?";
            let chrome = prefix.chars().count() + suffix.chars().count();
            let max_preview = (content_w as usize).saturating_sub(chrome + 1);
            let preview_trunc: String = if preview_text.chars().count() > max_preview {
                let truncated: String = preview_text
                    .chars()
                    .take(max_preview.saturating_sub(1))
                    .collect();
                format!("{truncated}\u{2026}")
            } else {
                preview_text.to_string()
            };
            let title = format!("{prefix}{preview_trunc}{suffix}");
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(title, title_style)),
                content_w,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'y',
                "Yes",
                *active_idx == 0,
                focused,
                &theme,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'a',
                "Yes, and don't ask again",
                *active_idx == 1,
                focused,
                &theme,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'n',
                "No",
                *active_idx == 2,
                focused,
                &theme,
            );
        }
        RewindPhase::Error { message } => {
            let mut y = area.y + 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    "Rewind failed",
                    Style::default()
                        .fg(theme.accent_error)
                        .add_modifier(Modifier::BOLD),
                )),
                content_w,
            );
            y += 1;
            let truncated: String = message.chars().take(content_w as usize).collect();
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    truncated,
                    Style::default().fg(theme.text_primary),
                )),
                content_w,
            );
            y += 1;
            render_radio_row(
                buf, content_x, y, content_w, '\x1b', "Dismiss", true, focused, &theme,
            );
        }
    }

    // Unfocus dim: when the prompt area is unfocused (e.g. user moved
    // to scrollback), blend foregrounds toward `bg_light` so the panel
    // visually recedes. Mirrors the unfocused prompt widget pattern
    // (`prompt_widget.rs:1948`).
    if !focused {
        crate::render::color::blend_area(buf, area, Some((bg, 0.66)), None);
    }
}

/// Visible label for sentinel-encoded keys (`Esc`, `Bksp`).
fn key_label(key: char) -> String {
    match key {
        '\x1b' => "Esc".into(),
        '\x08' => "Bksp".into(),
        other => other.to_string(),
    }
}

fn render_radio_row(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    w: u16,
    key: char,
    label: &str,
    is_cursor: bool,
    panel_focused: bool,
    theme: &Theme,
) {
    let bg = if is_cursor && panel_focused {
        theme.bg_visual
    } else {
        theme.bg_light
    };

    let row_rect = Rect {
        x: x.saturating_sub(1),
        y,
        width: w + 2,
        height: 1,
    };
    buf.set_style(row_rect, Style::default().bg(bg));

    let marker = if is_cursor {
        crate::glyphs::filled_dot()
    } else {
        "\u{25CB}"
    };
    let key_display = key_label(key);

    let num_style = Style::default().fg(theme.accent_user).bg(bg);
    let marker_style = if is_cursor {
        Style::default().fg(theme.accent_user).bg(bg)
    } else {
        Style::default().fg(theme.gray).bg(bg)
    };
    let label_style = Style::default()
        .fg(theme.text_primary)
        .bg(bg)
        .add_modifier(if is_cursor {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });

    let line = Line::from(vec![
        Span::styled(format!("{key_display:<4}"), num_style),
        Span::styled(format!("({marker}) "), marker_style),
        Span::styled(label.to_string(), label_style),
    ]);
    buf.set_line(x, y, &line, w);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyModifiers};

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        }
    }

    fn point(prompt_index: usize) -> RewindPointInfo {
        RewindPointInfo {
            prompt_index,
            created_at: String::new(),
            num_file_snapshots: 0,
            prompt_preview: Some(format!("turn {prompt_index}")),
            has_file_changes: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        }
    }

    fn confirm_state() -> RewindState {
        RewindState {
            phase: RewindPhase::Confirm {
                target_prompt_index: 3,
                active_idx: 0,
                prompt_preview: None,
            },
            anchor_entry_idx: 0,
            stashed_draft: None,
            selected_prompt_index: Some(3),
        }
    }

    #[test]
    fn picker_row_hit_test_maps_to_point_index() {
        let phase = RewindPhase::Picker {
            points: vec![point(0), point(1), point(2)],
            selected: 0,
        };
        // Title is at y+1; rows start at y+2.
        assert_eq!(rewind_row_at(&phase, area(), 5, 1), None);
        assert_eq!(rewind_row_at(&phase, area(), 5, 2), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 4), Some(2));
        // Past the last point.
        assert_eq!(rewind_row_at(&phase, area(), 5, 5), None);
        // Outside the overlay horizontally.
        assert_eq!(rewind_row_at(&phase, area(), 99, 2), None);
    }

    #[test]
    fn cancel_offer_rows() {
        let phase = RewindPhase::CancelOffer { active_idx: 0 };
        assert_eq!(rewind_row_at(&phase, area(), 5, 3), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 4), Some(1));
        assert_eq!(rewind_row_at(&phase, area(), 5, 5), None);
    }

    #[test]
    fn confirm_rows() {
        let phase = RewindPhase::Confirm {
            target_prompt_index: 0,
            active_idx: 0,
            prompt_preview: None,
        };
        assert_eq!(rewind_row_at(&phase, area(), 5, 2), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 3), Some(1));
        assert_eq!(rewind_row_at(&phase, area(), 5, 4), Some(2));
        assert_eq!(rewind_row_at(&phase, area(), 5, 5), None);
    }

    #[test]
    fn error_dismiss_row() {
        let phase = RewindPhase::Error {
            message: "boom".into(),
        };
        assert_eq!(rewind_row_at(&phase, area(), 5, 3), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 2), None);
    }

    #[test]
    fn non_interactive_phases_have_no_rows() {
        for phase in [
            RewindPhase::Loading,
            RewindPhase::Executing {
                target_prompt_index: 0,
            },
        ] {
            for row in 0..10 {
                assert_eq!(rewind_row_at(&phase, area(), 5, row), None);
            }
        }
    }

    #[test]
    fn set_cursor_moves_and_clamps() {
        let mut phase = RewindPhase::Picker {
            points: vec![point(0), point(1)],
            selected: 0,
        };
        assert!(set_rewind_cursor(&mut phase, 1));
        assert!(!set_rewind_cursor(&mut phase, 1)); // no change
        // Clamp out-of-range to last point (already at last → no change).
        assert!(!set_rewind_cursor(&mut phase, 99));
        if let RewindPhase::Picker { selected, .. } = phase {
            assert_eq!(selected, 1);
        } else {
            panic!("expected picker");
        }

        let mut confirm = RewindPhase::Confirm {
            target_prompt_index: 0,
            active_idx: 0,
            prompt_preview: None,
        };
        set_rewind_cursor(&mut confirm, 2);
        if let RewindPhase::Confirm { active_idx, .. } = confirm {
            assert_eq!(active_idx, 2);
        } else {
            panic!("expected confirm");
        }
        set_rewind_cursor(&mut confirm, 99);
        if let RewindPhase::Confirm { active_idx, .. } = confirm {
            assert_eq!(active_idx, 2);
        } else {
            panic!("expected confirm");
        }
    }

    #[test]
    fn activate_matches_enter_semantics() {
        let picker = RewindPhase::Picker {
            points: vec![point(10), point(20)],
            selected: 1,
        };
        assert!(matches!(
            rewind_activate(&picker),
            RewindInput::PickerSelect(20)
        ));

        let error = RewindPhase::Error {
            message: "x".into(),
        };
        assert!(matches!(rewind_activate(&error), RewindInput::DismissError));

        let confirm_go = RewindPhase::Confirm {
            target_prompt_index: 4,
            active_idx: 0,
            prompt_preview: None,
        };
        assert!(matches!(
            rewind_activate(&confirm_go),
            RewindInput::Confirm(4)
        ));

        let confirm_never = RewindPhase::Confirm {
            target_prompt_index: 4,
            active_idx: 1,
            prompt_preview: None,
        };
        assert!(matches!(
            rewind_activate(&confirm_never),
            RewindInput::ConfirmNeverAsk(4)
        ));

        let confirm_no = RewindPhase::Confirm {
            target_prompt_index: 4,
            active_idx: 2,
            prompt_preview: None,
        };
        assert!(matches!(
            rewind_activate(&confirm_no),
            RewindInput::Dismissed
        ));
    }

    #[test]
    fn confirm_letter_keys() {
        let state = confirm_state();
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Char('y'))),
            RewindInput::Confirm(3)
        ));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Char('n'))),
            RewindInput::Dismissed
        ));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Char('a'))),
            RewindInput::ConfirmNeverAsk(3)
        ));
    }

    #[test]
    fn esc_dismisses_from_confirm() {
        let state = confirm_state();
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Esc)),
            RewindInput::Dismissed
        ));
    }

    #[test]
    fn backspace_ignored_on_confirm() {
        let state = confirm_state();
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Backspace)),
            RewindInput::Consumed
        ));
    }

    #[test]
    fn esc_dismisses_from_picker_and_other_phases() {
        let s = RewindState {
            phase: RewindPhase::Picker {
                points: vec![],
                selected: 0,
            },
            anchor_entry_idx: 0,
            stashed_draft: None,
            selected_prompt_index: None,
        };
        assert!(matches!(
            handle_rewind_key(&s, &key(KeyCode::Esc)),
            RewindInput::Dismissed
        ));

        let s = RewindState::new_cancel_offer(0, None, None);
        assert!(matches!(
            handle_rewind_key(&s, &key(KeyCode::Esc)),
            RewindInput::Dismissed
        ));

        let s = RewindState {
            phase: RewindPhase::Loading,
            anchor_entry_idx: 0,
            stashed_draft: None,
            selected_prompt_index: None,
        };
        assert!(matches!(
            handle_rewind_key(&s, &key(KeyCode::Esc)),
            RewindInput::Dismissed
        ));
    }

    #[test]
    fn key_label_renders_special_sentinels() {
        assert_eq!(key_label('\x1b'), "Esc");
        assert_eq!(key_label('\x08'), "Bksp");
        assert_eq!(key_label('y'), "y");
        assert_eq!(key_label('a'), "a");
    }
}
