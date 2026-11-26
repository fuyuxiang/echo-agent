//! Inline edit-and-resubmit of a previous user prompt.
//!
//! Double-click (or Enter on) a previous user message to edit it in place.
//! Enter with changed text rewinds the conversation to that prompt
//! (conversation-only) and resubmits; unchanged/empty Enter or Esc exits.
//! Structure mirrors `queue_edit.rs`.

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use echo_agent_ratatui_textarea::{TextArea, TextAreaState};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::StatefulWidgetRef;
use unicode_width::UnicodeWidthStr;

use crate::key;
use crate::scrollback::block::RenderBlock;
use crate::scrollback::entry::EntryId;
use crate::scrollback::layout::HorizontalLayout;
use crate::theme::Theme;

use super::actions::Action;
use super::agent_view::AgentView;
use super::app_view::InputOutcome;

/// Master switch for the in-place prompt edit feature.
///
/// Disabled while we resolve an unsolved scroll jump on enter (see
/// `x/agottumukkala/inline-edit-scroll-jank.md`). Gates the user entry points
/// (Enter in `agent_view/panes.rs`, double-click in `agent_view/selection.rs`).
/// Everything else stays wired and unit-tested, so flipping this to `true`
/// re-enables the feature in one place.
pub(crate) const INLINE_EDIT_ENABLED: bool = false;

/// State of an in-place edit of a previous user prompt.
pub struct InlineEditState {
    /// Stable id of the edited entry (indices shift; re-resolve per use).
    pub entry_id: EntryId,
    /// Shell-side prompt index — the rewind target.
    pub prompt_index: usize,
    /// Original text, for dirty detection.
    pub original: String,
    pub textarea: TextArea,
    pub textarea_state: TextAreaState,
    /// Textarea rect from the last render (mouse hit-testing).
    pub last_text_area: Option<Rect>,
    /// Full overlay rect from the last render (mouse hit-testing).
    pub last_rect: Option<Rect>,
}

impl std::fmt::Debug for InlineEditState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineEditState")
            .field("entry_id", &self.entry_id)
            .field("prompt_index", &self.prompt_index)
            .finish_non_exhaustive()
    }
}

impl AgentView {
    /// Start editing the user prompt at `entry_idx` in place. Allowed even
    /// mid-turn (a running turn only matters at submit time). Returns `false`
    /// for non-editable entries (bash/cron, no rewind target) so callers can
    /// fall back to their old behavior.
    pub(super) fn enter_inline_edit(&mut self, entry_idx: usize) -> bool {
        if self.inline_edit.is_some() {
            return true;
        }
        let Some(entry) = self.scrollback.entry(entry_idx) else {
            return false;
        };
        let RenderBlock::UserPrompt(ref block) = entry.block else {
            return false;
        };
        // Interjections resolve to the enclosing turn's index — editing one
        // would rewind the wrong prompt.
        if block.is_bash || block.is_cron || block.is_interjection {
            return false;
        }
        let entry_id = entry.id;
        let text = block.text.clone();
        let Some(prompt_index) =
            super::dispatch::shell_prompt_index_at(&self.scrollback, entry_idx)
        else {
            return false;
        };

        // Inline edit takes input priority over the `/jump` picker; close a
        // lingering one first so it can't reappear (stale) after edit exits.
        self.dismiss_jump_picker();

        let mut textarea = TextArea::new();
        textarea.set_text(&text);
        textarea.set_cursor(text.len());
        self.inline_edit = Some(InlineEditState {
            entry_id,
            prompt_index,
            original: text,
            textarea,
            textarea_state: TextAreaState::default(),
            last_text_area: None,
            last_rect: None,
        });
        self.scrollback.set_selected(Some(entry_idx));
        self.scrollback.scroll_to_entry_center(entry_idx);
        true
    }

    /// Exit editing mode, discarding the edit and the height override.
    pub(super) fn exit_inline_edit(&mut self) {
        if self.inline_edit.take().is_some() {
            self.scrollback.set_inline_edit_height(None);
        }
    }

    /// Key intercept while editing: bare Enter submits when changed, exits
    /// when unchanged/empty; Esc (or Ctrl+C on empty) discards; everything
    /// else (incl. Shift/Alt-Enter newlines) goes to the textarea.
    pub(super) fn handle_inline_edit_key(&mut self, key: &KeyEvent) -> InputOutcome {
        let Some(ref mut edit) = self.inline_edit else {
            return InputOutcome::Unchanged;
        };

        if key!(Enter).matches(key) {
            let text = edit.textarea.text().trim().to_string();
            if text.is_empty() || text == edit.original.trim() {
                self.exit_inline_edit();
            } else {
                return InputOutcome::Action(Action::InlineEditSubmit);
            }
            return InputOutcome::Changed;
        }

        let ctrl_c_empty = key!('c', CONTROL).matches(key) && edit.textarea.text().is_empty();
        if (key.code == KeyCode::Esc && key.modifiers.is_empty()) || ctrl_c_empty {
            self.exit_inline_edit();
            return InputOutcome::Changed;
        }

        edit.textarea.input(*key);
        InputOutcome::Changed
    }

    /// Mouse intercept while editing: click inside moves the cursor, click
    /// outside discards, wheel scrolls the transcript.
    pub(super) fn handle_inline_edit_mouse(&mut self, mouse: &MouseEvent) -> InputOutcome {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scrollback.scroll_up(3);
                InputOutcome::Changed
            }
            MouseEventKind::ScrollDown => {
                self.scrollback.scroll_down(3);
                InputOutcome::Changed
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(ref mut edit) = self.inline_edit else {
                    return InputOutcome::Unchanged;
                };
                if let Some(ta) = edit.last_text_area
                    && let Some(pos) = edit.textarea.buffer_pos_at_screen(
                        mouse.column,
                        mouse.row,
                        ta,
                        edit.textarea_state,
                    )
                {
                    edit.textarea.clear_selection();
                    edit.textarea.set_cursor(pos);
                    return InputOutcome::Changed;
                }
                let inside = edit
                    .last_rect
                    .is_some_and(|r| r.contains((mouse.column, mouse.row).into()));
                if !inside {
                    // Click elsewhere discards the edit; swallow the click.
                    self.exit_inline_edit();
                }
                InputOutcome::Changed
            }
            _ => InputOutcome::Unchanged,
        }
    }

    /// Per-frame layout sync (before `prepare_layout`): refresh the height
    /// override (textarea height), abandon the edit if the entry vanished,
    /// and return the dim-from entry index.
    pub(super) fn sync_inline_edit_layout(&mut self, scrollback_width: u16) -> Option<usize> {
        let entry_id = self.inline_edit.as_ref()?.entry_id;
        let Some(idx) = self.scrollback.index_of_id(entry_id) else {
            self.exit_inline_edit();
            return None;
        };
        let ta_width = self.inline_edit_text_width(scrollback_width);
        let edit = self.inline_edit.as_ref()?;
        let height = edit.textarea.desired_height(ta_width).max(1);
        self.scrollback
            .set_inline_edit_height(Some((entry_id, height)));
        Some(idx.saturating_add(1))
    }

    /// Width the textarea wraps at: entry content column minus the `❯ ` prefix.
    fn inline_edit_text_width(&self, scrollback_width: u16) -> u16 {
        let content_w = self.scrollback.entry_text_column_width(scrollback_width);
        let prefix_w = crate::glyphs::prompt_arrow().width() as u16;
        content_w.saturating_sub(prefix_w).max(1)
    }

    /// Overlay-draw the editor over the edited entry (after the scrollback
    /// pane renders, same rect) and return the hardware cursor position.
    pub(super) fn render_inline_edit(
        &mut self,
        buf: &mut Buffer,
        scrollback_area: Rect,
    ) -> Option<(u16, u16)> {
        // Drop last frame's rects up front: when the overlay doesn't draw
        // this frame (entry scrolled off-viewport), clicks must not hit-test
        // against where the editor used to be.
        let edit = self.inline_edit.as_mut()?;
        edit.last_text_area = None;
        edit.last_rect = None;
        let entry_id = edit.entry_id;
        let idx = self.scrollback.index_of_id(entry_id)?;
        let (rect, _top_clipped, _bottom_clipped) =
            self.scrollback.entry_screen_area(idx, scrollback_area)?;
        if rect.width == 0 || rect.height == 0 {
            return None;
        }
        let theme = Theme::current();

        // Blank the entry's rendered content (style alone keeps old glyphs).
        let blank = " ".repeat(rect.width as usize);
        for y in rect.y..rect.y.saturating_add(rect.height) {
            buf.set_string(rect.x, y, &blank, Style::default().bg(theme.bg_visual));
        }

        // Content column, mirroring `entry_screen_area`'s layout math.
        let hl = HorizontalLayout::new(
            scrollback_area,
            &self.scrollback.appearance().scrollback.layout,
        );
        let entry_area = hl.entry_content_area();
        let content_x = entry_area.x
            + HorizontalLayout::ACCENT
            + self
                .scrollback
                .appearance()
                .scrollback
                .layout
                .block_pad_left;
        let content_w = hl.content_width();

        let prefix = crate::glyphs::prompt_arrow();
        let prefix_w = prefix.width() as u16;
        buf.set_string(content_x, rect.y, prefix, theme.fg(theme.accent_user));

        let ta_area = Rect {
            x: content_x + prefix_w,
            y: rect.y,
            width: content_w.saturating_sub(prefix_w).max(1),
            height: rect.height,
        };

        let edit = self.inline_edit.as_mut()?;
        (&edit.textarea).render_ref(ta_area, buf, &mut edit.textarea_state);
        edit.last_text_area = Some(ta_area);
        edit.last_rect = Some(rect);

        edit.textarea
            .cursor_pos_with_state(ta_area, edit.textarea_state)
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::agent::AgentState;
    use crate::app::agent_view::test_fixtures::make_agent;

    fn enter_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn esc_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    }

    /// Idle agent with one user prompt ("fix the bug") followed by an agent
    /// message, laid out at 80x40.
    fn agent_with_prompt() -> AgentView {
        let mut agent = make_agent();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("fix the bug"));
        agent
            .scrollback
            .push_block(RenderBlock::agent_message("done"));
        agent.scrollback.prepare_layout(80, 40);
        agent
    }

    /// Entering edit mode on a plain user prompt captures the original text,
    /// resolves the rewind target, and selects the entry.
    #[test]
    fn enter_inline_edit_on_user_prompt_starts_editing() {
        let mut agent = agent_with_prompt();
        assert!(agent.enter_inline_edit(0));
        let edit = agent.inline_edit.as_ref().expect("editing state");
        assert_eq!(edit.original, "fix the bug");
        assert_eq!(edit.textarea.text(), "fix the bug");
        assert_eq!(edit.prompt_index, 0);
        assert_eq!(agent.scrollback.selected(), Some(0));
    }

    /// Bash and cron prompts are not editable — callers fall back to the old
    /// double-click/Enter behavior.
    #[test]
    fn enter_inline_edit_rejects_bash_and_cron_prompts() {
        let mut agent = make_agent();
        agent.scrollback.push_block(RenderBlock::bash_prompt("ls"));
        agent
            .scrollback
            .push_block(RenderBlock::cron_prompt("check ci"));
        agent.scrollback.prepare_layout(80, 40);
        assert!(!agent.enter_inline_edit(0));
        assert!(!agent.enter_inline_edit(1));
        assert!(agent.inline_edit.is_none());
    }

    /// Enter with unchanged text exits editing mode without dispatching
    /// anything — no rewind, no resubmit.
    #[test]
    fn enter_with_unchanged_text_just_exits_editing() {
        let mut agent = agent_with_prompt();
        assert!(agent.enter_inline_edit(0));
        let outcome = agent.handle_inline_edit_key(&enter_key());
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.inline_edit.is_none(), "editing mode must exit");
    }

    /// Enter after emptying the editor also just exits (an empty prompt can
    /// never be submitted).
    #[test]
    fn enter_with_emptied_text_just_exits_editing() {
        let mut agent = agent_with_prompt();
        assert!(agent.enter_inline_edit(0));
        agent.inline_edit.as_mut().unwrap().textarea.set_text("  ");
        let outcome = agent.handle_inline_edit_key(&enter_key());
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.inline_edit.is_none());
    }

    /// Enter with changed text dispatches the submit action (the rewind +
    /// resubmit is driven by dispatch, tested in dispatch.rs).
    #[test]
    fn enter_with_changed_text_dispatches_submit() {
        let mut agent = agent_with_prompt();
        assert!(agent.enter_inline_edit(0));
        agent
            .inline_edit
            .as_mut()
            .unwrap()
            .textarea
            .set_text("fix the bug properly");
        let outcome = agent.handle_inline_edit_key(&enter_key());
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::InlineEditSubmit)
        ));
        assert!(
            agent.inline_edit.is_some(),
            "state stays until dispatch consumes it"
        );
    }

    /// Esc discards the edit; the transcript entry (and its layout height)
    /// are restored.
    #[test]
    fn esc_discards_edit_and_clears_height_override() {
        let mut agent = agent_with_prompt();
        assert!(agent.enter_inline_edit(0));
        agent.sync_inline_edit_layout(80);
        assert!(agent.scrollback.inline_edit_height().is_some());

        agent
            .inline_edit
            .as_mut()
            .unwrap()
            .textarea
            .set_text("scrapped edit");
        let outcome = agent.handle_inline_edit_key(&esc_key());
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.inline_edit.is_none());
        assert!(agent.scrollback.inline_edit_height().is_none());
        // Original text untouched.
        let RenderBlock::UserPrompt(ref block) = agent.scrollback.entry(0).unwrap().block else {
            panic!("expected user prompt");
        };
        assert_eq!(block.text, "fix the bug");
    }

    /// Typed keys reach the textarea (the intercept only handles
    /// Enter/Esc/Ctrl+C).
    #[test]
    fn typing_reaches_the_inline_textarea() {
        let mut agent = agent_with_prompt();
        assert!(agent.enter_inline_edit(0));
        let key = KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE);
        let _ = agent.handle_inline_edit_key(&key);
        assert_eq!(
            agent.inline_edit.as_ref().unwrap().textarea.text(),
            "fix the bug!"
        );
    }

    /// Editing opens immediately even while a turn is running — the
    /// cancel-offer only appears at submit time (see dispatch tests).
    #[test]
    fn enter_inline_edit_while_busy_opens_editor_immediately() {
        let mut agent = agent_with_prompt();
        agent.session.state = AgentState::TurnRunning;
        assert!(agent.enter_inline_edit(0));
        assert!(agent.inline_edit.is_some(), "editor must open mid-turn");
        assert!(agent.rewind_state.is_none(), "no cancel-offer on entry");
    }

    /// When the edited entry scrolls out of the viewport, render drops the
    /// previous frame's hit-test rects — a click where the editor *used* to
    /// be must count as click-outside (discard), not move the edit cursor.
    #[test]
    fn render_clears_stale_mouse_rects_when_entry_scrolls_off_screen() {
        let mut agent = make_agent();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("fix the bug"));
        agent
            .scrollback
            .push_block(RenderBlock::agent_message("done"));
        // A later prompt + enough content that entry 0 leaves the viewport
        // (and is not the sticky header) when scrolled to the bottom.
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("second question"));
        for i in 0..50 {
            agent
                .scrollback
                .push_block(RenderBlock::agent_message(format!("reply {i}")));
        }
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        agent.scrollback.prepare_layout(80, 10);
        assert!(agent.enter_inline_edit(0));

        // Editor visible: render records the hit-test rects.
        agent.scrollback.prepare_layout(80, 10);
        let mut buf = Buffer::empty(area);
        agent.render_inline_edit(&mut buf, area);
        assert!(agent.inline_edit.as_ref().unwrap().last_rect.is_some());
        assert!(agent.inline_edit.as_ref().unwrap().last_text_area.is_some());

        // Entry scrolls off-viewport: the stale rects are dropped, so a
        // click at the old rect discards the edit instead of hitting it.
        agent.scrollback.goto_bottom();
        agent.scrollback.prepare_layout(80, 10);
        let mut buf = Buffer::empty(area);
        agent.render_inline_edit(&mut buf, area);
        let edit = agent.inline_edit.as_ref().unwrap();
        assert!(edit.last_rect.is_none(), "stale overlay rect dropped");
        assert!(edit.last_text_area.is_none(), "stale textarea rect dropped");
    }

    /// The per-frame layout sync reserves the textarea's height and abandons
    /// the edit when the entry disappears (e.g. transcript replaced).
    #[test]
    fn sync_layout_reserves_height_and_survives_entry_removal() {
        let mut agent = agent_with_prompt();
        assert!(agent.enter_inline_edit(0));

        let dim_from = agent.sync_inline_edit_layout(80);
        assert_eq!(dim_from, Some(1), "dim everything below the edited entry");
        let (id, h) = agent.scrollback.inline_edit_height().expect("override");
        assert_eq!(Some(id), agent.scrollback.entry(0).map(|e| e.id));
        assert_eq!(h, 1, "single-line prompt: 1 text row");

        // Grow the text to several lines: the reserved height follows.
        agent
            .inline_edit
            .as_mut()
            .unwrap()
            .textarea
            .set_text("one\ntwo\nthree");
        agent.sync_inline_edit_layout(80);
        let (_, h) = agent.scrollback.inline_edit_height().expect("override");
        assert_eq!(h, 3, "3 text rows");

        // Entry vanishes → edit is abandoned and the override cleared.
        agent.scrollback.remove_from(0);
        assert_eq!(agent.sync_inline_edit_layout(80), None);
        assert!(agent.inline_edit.is_none());
        assert!(agent.scrollback.inline_edit_height().is_none());
    }
}
