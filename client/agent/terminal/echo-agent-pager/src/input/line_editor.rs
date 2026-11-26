use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use echo_agent_ratatui_textarea::{
    EditBuffer, EditCommand, EditOutcome, SingleLineViewport, classify_key_event,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineEditOutcome {
    Unhandled,
    /// The key was recognized and consumed, but text and cursor stayed unchanged.
    HandledNoChange,
    CursorChanged,
    TextChanged,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LineEditor {
    buffer: EditBuffer,
}

impl LineEditor {
    pub(crate) fn text(&self) -> &str {
        self.buffer.text()
    }

    pub(crate) fn cursor_byte(&self) -> usize {
        self.buffer.cursor_byte()
    }

    pub(crate) fn set_text(&mut self, text: impl Into<String>) {
        self.buffer = EditBuffer::from_text(sanitize_single_line(text));
    }

    pub(crate) fn reset(&mut self) {
        self.buffer = EditBuffer::new();
    }

    #[cfg(test)]
    pub(crate) fn set_cursor_byte(&mut self, cursor_byte: usize) -> LineEditOutcome {
        Self::from_edit_outcome(self.buffer.set_cursor_byte(cursor_byte))
    }

    pub(crate) fn delete_last_grapheme(&mut self) -> LineEditOutcome {
        let _ = self.buffer.set_cursor_byte(self.buffer.text().len());
        Self::from_edit_outcome(self.buffer.apply(EditCommand::DeleteGraphemeBackward))
    }

    pub(crate) fn insert_paste(&mut self, text: &str) -> LineEditOutcome {
        self.insert_paste_with_policy(text, |_| true, usize::MAX)
    }

    pub(crate) fn insert_paste_with_policy(
        &mut self,
        text: &str,
        mut allow_insert: impl FnMut(char) -> bool,
        max_chars: usize,
    ) -> LineEditOutcome {
        let cleaned = sanitize_single_line(text);
        let accepted = cleaned
            .chars()
            .filter(|character| allow_insert(*character))
            .take(max_chars)
            .collect::<String>();
        if accepted.is_empty() {
            return LineEditOutcome::HandledNoChange;
        }
        Self::from_edit_outcome(self.buffer.insert_str(&accepted))
    }

    pub(crate) fn insert_paste_with_byte_limit(
        &mut self,
        text: &str,
        max_total_bytes: usize,
    ) -> LineEditOutcome {
        let cleaned = sanitize_single_line(text);
        let remaining = max_total_bytes.saturating_sub(self.buffer.text().len());
        let mut accepted_bytes = 0usize;
        let accepted = cleaned
            .chars()
            .take_while(|character| {
                let next = accepted_bytes + character.len_utf8();
                if next > remaining {
                    return false;
                }
                accepted_bytes = next;
                true
            })
            .collect::<String>();
        if accepted.is_empty() {
            return LineEditOutcome::HandledNoChange;
        }
        Self::from_edit_outcome(self.buffer.insert_str(&accepted))
    }

    pub(crate) fn handle_key(&mut self, key: &KeyEvent) -> LineEditOutcome {
        self.handle_key_with_insert_policy(key, |_| true)
    }

    pub(crate) fn handle_key_with_insert_policy(
        &mut self,
        key: &KeyEvent,
        allow_insert: impl FnOnce(char) -> bool,
    ) -> LineEditOutcome {
        if key.kind == KeyEventKind::Release {
            return LineEditOutcome::Unhandled;
        }
        let command = match key {
            KeyEvent {
                code: KeyCode::Home,
                ..
            }
            | KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::SUPER,
                ..
            } => Some(EditCommand::MoveLogicalLineStart),
            KeyEvent {
                code: KeyCode::End, ..
            }
            | KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::SUPER,
                ..
            } => Some(EditCommand::MoveLogicalLineEnd),
            _ => classify_key_event(key),
        };
        let Some(command) = command else {
            return LineEditOutcome::Unhandled;
        };
        if let EditCommand::Insert(character) = command
            && !allow_insert(character)
        {
            return LineEditOutcome::HandledNoChange;
        }
        Self::from_edit_outcome(self.buffer.apply(command))
    }

    pub(crate) fn viewport(&self, width: usize) -> SingleLineViewport {
        self.buffer.single_line_viewport(width)
    }

    fn from_edit_outcome(outcome: EditOutcome) -> LineEditOutcome {
        match outcome {
            EditOutcome::Unchanged => LineEditOutcome::HandledNoChange,
            EditOutcome::CursorOnly => LineEditOutcome::CursorChanged,
            EditOutcome::TextOnly(_) | EditOutcome::TextAndCursor(_) => {
                LineEditOutcome::TextChanged
            }
        }
    }
}

pub(crate) fn sanitize_single_line(text: impl Into<String>) -> String {
    let mut text = text.into();
    text.retain(|character| !matches!(character, '\r' | '\n'));
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn canonical_word_keys_and_legacy_alt_bindings() {
        for event in [
            key(KeyCode::Left, KeyModifiers::ALT),
            key(KeyCode::Char('b'), KeyModifiers::ALT),
            key(KeyCode::Left, KeyModifiers::CONTROL),
        ] {
            let mut editor = LineEditor::default();
            editor.set_text("hello-world");
            assert_eq!(editor.handle_key(&event), LineEditOutcome::CursorChanged);
            assert_eq!(editor.cursor_byte(), "hello-".len());
        }

        let mut editor = LineEditor::default();
        editor.set_text("hello-world");
        assert_eq!(
            editor.handle_key(&key(KeyCode::Backspace, KeyModifiers::ALT)),
            LineEditOutcome::TextChanged
        );
        assert_eq!(editor.text(), "hello-");
    }

    #[test]
    fn home_end_super_and_outcomes() {
        let mut editor = LineEditor::default();
        editor.set_text("abc");
        assert_eq!(
            editor.handle_key(&key(KeyCode::Home, KeyModifiers::NONE)),
            LineEditOutcome::CursorChanged
        );
        assert_eq!(
            editor.handle_key(&key(KeyCode::Left, KeyModifiers::SUPER)),
            LineEditOutcome::HandledNoChange
        );
        assert_eq!(
            editor.handle_key(&key(KeyCode::Right, KeyModifiers::SUPER)),
            LineEditOutcome::CursorChanged
        );
        assert_eq!(
            editor.handle_key(&key(KeyCode::End, KeyModifiers::NONE)),
            LineEditOutcome::HandledNoChange
        );
        assert_eq!(
            editor.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE)),
            LineEditOutcome::Unhandled
        );
    }

    #[test]
    fn ctrl_u_kills_only_to_the_cursor() {
        let mut editor = LineEditor::default();
        editor.set_text("hello world");
        assert_eq!(
            editor.handle_key(&key(KeyCode::Left, KeyModifiers::NONE)),
            LineEditOutcome::CursorChanged
        );
        assert_eq!(
            editor.handle_key(&key(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            LineEditOutcome::TextChanged
        );
        assert_eq!(editor.text(), "d");
        assert_eq!(editor.cursor_byte(), 0);
    }

    #[test]
    fn paste_byte_limit_keeps_whole_characters_at_cursor() {
        let mut editor = LineEditor::default();
        editor.set_text("ab");
        let _ = editor.set_cursor_byte(1);
        assert_eq!(
            editor.insert_paste_with_byte_limit("中x", 5),
            LineEditOutcome::TextChanged
        );
        assert_eq!(editor.text(), "a中b");
        assert_eq!(editor.cursor_byte(), "a中".len());
    }

    #[test]
    fn delete_last_grapheme_ignores_cursor_and_deletes_one_cluster() {
        let mut editor = LineEditor::default();
        editor.set_text("a👩🏽\u{200d}💻");
        let _ = editor.set_cursor_byte(0);
        assert_eq!(editor.delete_last_grapheme(), LineEditOutcome::TextChanged);
        assert_eq!(editor.text(), "a");
        assert_eq!(editor.cursor_byte(), 1);
    }

    #[test]
    fn set_text_sanitizes_and_places_cursor_at_end() {
        let mut editor = LineEditor::default();
        editor.set_text("one\r\ntwo\nthree\rfour");
        assert_eq!(editor.text(), "onetwothreefour");
        assert_eq!(editor.cursor_byte(), editor.text().len());
    }

    #[test]
    fn insert_policy_only_gates_insert_commands() {
        let mut editor = LineEditor::default();
        editor.set_text("ab");
        assert_eq!(
            editor.handle_key_with_insert_policy(
                &key(KeyCode::Char('x'), KeyModifiers::NONE),
                |_| false,
            ),
            LineEditOutcome::HandledNoChange
        );
        assert_eq!(editor.text(), "ab");
        assert_eq!(
            editor
                .handle_key_with_insert_policy(&key(KeyCode::Left, KeyModifiers::NONE), |_| false,),
            LineEditOutcome::CursorChanged
        );

        let mut unrestricted = LineEditor::default();
        assert_eq!(
            unrestricted.handle_key(&key(KeyCode::Char('\u{202e}'), KeyModifiers::NONE)),
            LineEditOutcome::TextChanged
        );
        assert_eq!(unrestricted.text(), "\u{202e}");
    }

    #[test]
    fn viewport_keeps_graphemes_and_cursor_visible() {
        let grapheme = "👩🏽\u{200d}💻";
        let mut editor = LineEditor::default();
        editor.set_text(format!("a{grapheme}b"));
        assert_eq!(
            editor.handle_key(&key(KeyCode::Left, KeyModifiers::NONE)),
            LineEditOutcome::CursorChanged
        );
        let viewport = editor.viewport(3);
        assert_eq!(
            &editor.text()[viewport.visible_byte_range.clone()],
            format!("{grapheme}b")
        );
        assert_eq!(viewport.cursor_display_column, 2);
    }
}
