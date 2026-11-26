//! `/jump` picker: an overlay listing every turn in the conversation.
//!
//! Pure client-side navigation over the scrollback timeline
//! ([`crate::scrollback::state::TimelineEntry`]): moving the cursor
//! live-scrolls the transcript to the hovered turn, Enter jumps there,
//! Esc restores the viewport the picker opened from. Unlike `/rewind`
//! nothing is fetched and nothing is mutated.
//!
//! Chrome, row geometry, and hit-testing come from
//! [`crate::views::overlay_list::ListOverlay`] (shared with the rewind
//! picker); this module owns only the row content and input mapping.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::render::line_utils::truncate_str;
use crate::scrollback::entry::EntryId;
use crate::scrollback::state::{ScrollAnchor, TimelineEntry};
use crate::theme::Theme;
use crate::views::overlay_list::ListOverlay;

/// Viewport snapshot captured when the picker opens, restored on Esc / failed
/// jump. The viewport is a width-stable [`ScrollAnchor`] bookmark, not a raw
/// scroll offset (which clamps and drifts under a resize).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JumpRestore {
    pub(crate) bookmark: Option<ScrollAnchor>,
    pub selected: Option<usize>,
    pub follow_mode: bool,
}

#[derive(Debug)]
pub struct JumpState {
    /// One row per turn, oldest first (row index == `turn_idx`).
    pub entries: Vec<TimelineEntry>,
    /// Cursor row.
    pub selected: usize,
    /// Viewport to restore on dismiss.
    pub restore: JumpRestore,
}

impl JumpState {
    fn list(&self) -> ListOverlay {
        ListOverlay {
            len: self.entries.len(),
            selected: self.selected,
        }
    }
}

pub enum JumpInput {
    /// Jump to the turn (by its prompt's stable id) and close.
    Select(EntryId),
    Dismissed,
    MoveUp,
    MoveDown,
    Consumed,
}

pub fn handle_jump_key(state: &JumpState, key: &KeyEvent) -> JumpInput {
    if key.kind == crossterm::event::KeyEventKind::Release {
        return JumpInput::Consumed;
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => JumpInput::MoveDown,
        KeyCode::Char('k') | KeyCode::Up => JumpInput::MoveUp,
        KeyCode::Enter => jump_activate(state),
        KeyCode::Esc => JumpInput::Dismissed,
        _ => JumpInput::Consumed,
    }
}

/// Move the cursor by `delta`, clamped to the entry list.
pub fn move_cursor(state: &mut JumpState, delta: i32) {
    if state.entries.is_empty() {
        return;
    }
    let max = state.entries.len() as i32 - 1;
    state.selected = (state.selected as i32 + delta).clamp(0, max) as usize;
}

/// Move the cursor to `idx` (mouse hover/click). Returns `true` on change.
pub fn set_jump_cursor(state: &mut JumpState, idx: usize) -> bool {
    if state.entries.is_empty() {
        return false;
    }
    let new = idx.min(state.entries.len() - 1);
    if state.selected != new {
        state.selected = new;
        true
    } else {
        false
    }
}

/// The activation input for the current cursor row (Enter-equivalent).
pub fn jump_activate(state: &JumpState) -> JumpInput {
    state
        .entries
        .get(state.selected)
        .map(|e| JumpInput::Select(e.prompt_entry_id))
        .unwrap_or(JumpInput::Consumed)
}

/// Hit-test a screen position against the picker's clickable rows.
pub fn jump_row_at(state: &JumpState, area: Rect, col: u16, row: u16) -> Option<usize> {
    state.list().row_at(area, col, row)
}

pub fn jump_overlay_height(state: &JumpState, screen_h: u16) -> u16 {
    state.list().height(screen_h)
}

pub fn render_jump_overlay(buf: &mut Buffer, area: Rect, state: &JumpState, focused: bool) {
    let theme = Theme::current();
    // Ordinal gutter sized to the widest turn number.
    let ord_width = state.entries.len().to_string().len();

    state
        .list()
        .render(buf, area, "Jump to which turn?", focused, |i, ctx| {
            let entry = &state.entries[i];
            let ordinal = format!("{:>ord_width$} ", entry.turn_idx + 1);
            let ord_style = Style::default().fg(theme.gray).bg(ctx.row_bg);
            let preview: String = if entry.preview.is_empty() {
                "(no preview)".to_string()
            } else {
                truncate_str(
                    &entry.preview,
                    ctx.content_width.saturating_sub(ord_width as u16 + 3) as usize,
                )
            };
            let text_style = Style::default()
                .fg(theme.text_primary)
                .bg(ctx.row_bg)
                .add_modifier(if ctx.is_cursor {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });
            Line::from(vec![
                Span::styled(ordinal, ord_style),
                Span::styled(preview, text_style),
            ])
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyModifiers};

    fn entry(turn_idx: usize) -> TimelineEntry {
        TimelineEntry {
            turn_idx,
            prompt_entry_id: EntryId::new(turn_idx as u64 * 2),
            preview: format!("turn {turn_idx}"),
        }
    }

    fn state(n: usize) -> JumpState {
        JumpState {
            entries: (0..n).map(entry).collect(),
            selected: 0,
            restore: JumpRestore {
                bookmark: None,
                selected: None,
                follow_mode: false,
            },
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

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        }
    }

    #[test]
    fn keys_map_to_inputs() {
        let s = state(3);
        assert!(matches!(
            handle_jump_key(&s, &key(KeyCode::Char('j'))),
            JumpInput::MoveDown
        ));
        assert!(matches!(
            handle_jump_key(&s, &key(KeyCode::Up)),
            JumpInput::MoveUp
        ));
        assert!(matches!(
            handle_jump_key(&s, &key(KeyCode::Enter)),
            JumpInput::Select(id) if id == EntryId::new(0)
        ));
        assert!(matches!(
            handle_jump_key(&s, &key(KeyCode::Esc)),
            JumpInput::Dismissed
        ));
        assert!(matches!(
            handle_jump_key(&s, &key(KeyCode::Char('x'))),
            JumpInput::Consumed
        ));
    }

    #[test]
    fn cursor_moves_and_clamps() {
        let mut s = state(3);
        move_cursor(&mut s, 1);
        assert_eq!(s.selected, 1);
        move_cursor(&mut s, 10);
        assert_eq!(s.selected, 2);
        move_cursor(&mut s, -10);
        assert_eq!(s.selected, 0);

        assert!(set_jump_cursor(&mut s, 2));
        assert!(!set_jump_cursor(&mut s, 2));
        assert!(!set_jump_cursor(&mut s, 99), "clamps to last (no change)");
        assert_eq!(s.selected, 2);
    }

    #[test]
    fn activate_selects_turn_under_cursor() {
        let mut s = state(3);
        s.selected = 2;
        assert!(matches!(jump_activate(&s), JumpInput::Select(id) if id == EntryId::new(4)));

        let empty = state(0);
        assert!(matches!(jump_activate(&empty), JumpInput::Consumed));
    }

    #[test]
    fn row_hit_test_maps_to_entry_index() {
        let s = state(3);
        // Title at y+1; rows start at y+2 (ListOverlay geometry).
        assert_eq!(jump_row_at(&s, area(), 5, 1), None);
        assert_eq!(jump_row_at(&s, area(), 5, 2), Some(0));
        assert_eq!(jump_row_at(&s, area(), 5, 4), Some(2));
        assert_eq!(jump_row_at(&s, area(), 5, 5), None);
    }
}
