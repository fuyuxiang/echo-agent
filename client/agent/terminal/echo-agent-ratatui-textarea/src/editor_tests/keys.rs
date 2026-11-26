use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::*;

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

#[test]
fn ctrl_w_uses_whitespace_delimited_word_deletion() {
    let command = classify_key_event(&key(KeyCode::Char('w'), KeyModifiers::CONTROL))
        .expect("Ctrl+W must classify");
    assert_eq!(
        command,
        EditCommand::DeleteWordBackward(WordStyle::WhitespaceDelimited)
    );

    let mut buffer = EditBuffer::from_text("git commit -m hello-world");
    let _ = buffer.apply(command);
    assert_eq!(buffer.text(), "git commit -m ");

    let mut small = EditBuffer::from_text("git commit -m hello-world");
    let _ = small.apply(EditCommand::DeleteWordBackward(WordStyle::Small));
    assert_eq!(small.text(), "git commit -m hello-");
}

#[test]
fn common_editing_keys_classify_to_semantic_commands() {
    let cases = [
        (
            key(KeyCode::Char('a'), KeyModifiers::NONE),
            EditCommand::Insert('a'),
        ),
        (
            key(KeyCode::Char('a'), KeyModifiers::SHIFT),
            EditCommand::Insert('A'),
        ),
        (
            key(KeyCode::Left, KeyModifiers::NONE),
            EditCommand::MoveGraphemeLeft,
        ),
        (
            key(KeyCode::Right, KeyModifiers::NONE),
            EditCommand::MoveGraphemeRight,
        ),
        (
            key(KeyCode::Left, KeyModifiers::ALT),
            EditCommand::MoveWordLeft(WordStyle::Small),
        ),
        (
            key(KeyCode::Left, KeyModifiers::CONTROL),
            EditCommand::MoveWordLeft(WordStyle::Small),
        ),
        (
            key(KeyCode::Right, KeyModifiers::ALT),
            EditCommand::MoveWordRight(WordStyle::Small),
        ),
        (
            key(KeyCode::Right, KeyModifiers::CONTROL),
            EditCommand::MoveWordRight(WordStyle::Small),
        ),
        (
            key(KeyCode::Char('a'), KeyModifiers::CONTROL),
            EditCommand::MoveLogicalLineStart,
        ),
        (
            key(KeyCode::Char('e'), KeyModifiers::CONTROL),
            EditCommand::MoveLogicalLineEnd,
        ),
        (
            key(KeyCode::Char('u'), KeyModifiers::CONTROL),
            EditCommand::DeleteToLineStart,
        ),
        (
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            EditCommand::DeleteToLineEnd,
        ),
        (
            key(KeyCode::Char('b'), KeyModifiers::CONTROL),
            EditCommand::MoveGraphemeLeft,
        ),
        (
            key(KeyCode::Char('f'), KeyModifiers::CONTROL),
            EditCommand::MoveGraphemeRight,
        ),
        (
            key(KeyCode::Char('d'), KeyModifiers::CONTROL),
            EditCommand::DeleteGraphemeForward,
        ),
        (
            key(KeyCode::Char('h'), KeyModifiers::CONTROL),
            EditCommand::DeleteGraphemeBackward,
        ),
        (
            key(KeyCode::Char('b'), KeyModifiers::ALT),
            EditCommand::MoveWordLeft(WordStyle::Small),
        ),
        (
            key(KeyCode::Char('f'), KeyModifiers::ALT),
            EditCommand::MoveWordRight(WordStyle::Small),
        ),
        (
            key(KeyCode::Char('d'), KeyModifiers::ALT),
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        (
            key(KeyCode::Char('d'), KeyModifiers::SUPER),
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        (
            key(KeyCode::Char('\u{0002}'), KeyModifiers::NONE),
            EditCommand::MoveGraphemeLeft,
        ),
        (
            key(KeyCode::Char('\u{0006}'), KeyModifiers::NONE),
            EditCommand::MoveGraphemeRight,
        ),
    ];

    for (event, expected) in cases {
        assert_eq!(classify_key_event(&event), Some(expected), "{event:?}");
    }
}

#[test]
fn visual_row_keys_and_modified_home_end_remain_adapter_owned() {
    let events = [
        key(KeyCode::Home, KeyModifiers::NONE),
        key(KeyCode::Home, KeyModifiers::SHIFT),
        key(KeyCode::Home, KeyModifiers::CONTROL),
        key(KeyCode::Home, KeyModifiers::ALT),
        key(KeyCode::Home, KeyModifiers::SUPER),
        key(KeyCode::Home, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        key(KeyCode::End, KeyModifiers::NONE),
        key(KeyCode::End, KeyModifiers::SHIFT),
        key(KeyCode::End, KeyModifiers::CONTROL),
        key(KeyCode::End, KeyModifiers::ALT),
        key(KeyCode::End, KeyModifiers::SUPER),
        key(KeyCode::End, KeyModifiers::ALT | KeyModifiers::SUPER),
        key(KeyCode::Left, KeyModifiers::SUPER),
        key(KeyCode::Right, KeyModifiers::SUPER),
    ];

    for event in events {
        assert_eq!(classify_key_event(&event), None, "{event:?}");
    }

    assert_eq!(
        classify_key_event(&key(KeyCode::Char('a'), KeyModifiers::CONTROL)),
        Some(EditCommand::MoveLogicalLineStart)
    );
    assert_eq!(
        classify_key_event(&key(KeyCode::Char('e'), KeyModifiers::CONTROL)),
        Some(EditCommand::MoveLogicalLineEnd)
    );
    assert_eq!(
        classify_key_event(&key(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        )),
        None
    );
}

#[test]
fn lifecycle_and_host_owned_keys_remain_unclassified() {
    let events = [
        key(KeyCode::Esc, KeyModifiers::NONE),
        key(KeyCode::Enter, KeyModifiers::NONE),
        key(KeyCode::Tab, KeyModifiers::NONE),
        key(KeyCode::Char('\t'), KeyModifiers::NONE),
        key(KeyCode::BackTab, KeyModifiers::SHIFT),
        key(KeyCode::Up, KeyModifiers::NONE),
        key(KeyCode::Down, KeyModifiers::NONE),
        key(KeyCode::Char('j'), KeyModifiers::CONTROL),
        key(KeyCode::Char('m'), KeyModifiers::CONTROL),
        key(KeyCode::Char('v'), KeyModifiers::CONTROL),
        key(KeyCode::Char('z'), KeyModifiers::CONTROL),
        key(KeyCode::Char('\n'), KeyModifiers::NONE),
    ];

    for event in events {
        assert_eq!(classify_key_event(&event), None, "{event:?}");
    }
}

#[test]
fn backspace_delete_and_raw_encodings_have_modifier_parity() {
    let cases = [
        (
            KeyModifiers::NONE,
            EditCommand::DeleteGraphemeBackward,
            EditCommand::DeleteGraphemeForward,
        ),
        (
            KeyModifiers::SHIFT,
            EditCommand::DeleteGraphemeBackward,
            EditCommand::DeleteGraphemeForward,
        ),
        (
            KeyModifiers::ALT,
            EditCommand::DeleteWordBackward(WordStyle::Small),
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        (
            KeyModifiers::CONTROL,
            EditCommand::DeleteWordBackward(WordStyle::Small),
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        (
            KeyModifiers::SUPER,
            EditCommand::DeleteToLineStart,
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        (
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            EditCommand::DeleteGraphemeBackward,
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        (
            KeyModifiers::ALT | KeyModifiers::SHIFT,
            EditCommand::DeleteGraphemeBackward,
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        (
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
            EditCommand::DeleteGraphemeBackward,
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        (
            KeyModifiers::CONTROL | KeyModifiers::ALT,
            EditCommand::DeleteGraphemeBackward,
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        (
            KeyModifiers::CONTROL | KeyModifiers::SUPER,
            EditCommand::DeleteGraphemeBackward,
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        (
            KeyModifiers::ALT | KeyModifiers::SUPER,
            EditCommand::DeleteGraphemeBackward,
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        (
            KeyModifiers::META,
            EditCommand::DeleteGraphemeBackward,
            EditCommand::DeleteGraphemeForward,
        ),
        (
            KeyModifiers::META | KeyModifiers::SHIFT,
            EditCommand::DeleteGraphemeBackward,
            EditCommand::DeleteGraphemeForward,
        ),
        (
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
            EditCommand::DeleteGraphemeBackward,
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
    ];

    for (modifiers, expected_backspace, expected_delete) in cases {
        let backspace = key(KeyCode::Backspace, modifiers);
        let delete = key(KeyCode::Delete, modifiers);
        let raw_bs = key(KeyCode::Char('\u{0008}'), modifiers);
        let raw_del = key(KeyCode::Char('\u{007f}'), modifiers);
        let backspace_command = classify_key_event(&backspace);
        assert_eq!(backspace_command, Some(expected_backspace), "{backspace:?}");
        assert_eq!(
            classify_key_event(&delete),
            Some(expected_delete),
            "{delete:?}"
        );
        assert_eq!(
            classify_key_event(&raw_bs),
            Some(EditCommand::DeleteGraphemeBackward),
            "{raw_bs:?}",
        );
        assert_eq!(
            classify_key_event(&raw_del),
            Some(EditCommand::DeleteGraphemeBackward),
            "{raw_del:?}",
        );
    }
}

#[test]
fn altgr_insertion_and_ctrl_alt_h_precedence_follow_platform_encoding() {
    let ctrl_alt = KeyModifiers::CONTROL | KeyModifiers::ALT;
    assert_eq!(
        classify_key_event(&key(KeyCode::Char('h'), ctrl_alt)),
        Some(EditCommand::DeleteWordBackward(WordStyle::Small))
    );

    for (character, modifiers) in [
        ('q', ctrl_alt),
        ('€', ctrl_alt | KeyModifiers::SHIFT),
        ('h', ctrl_alt | KeyModifiers::SHIFT),
    ] {
        let expected = if cfg!(target_os = "windows") {
            Some(EditCommand::Insert(character))
        } else {
            None
        };
        assert_eq!(
            classify_key_event(&key(KeyCode::Char(character), modifiers)),
            expected
        );
    }

    if cfg!(target_os = "windows") {
        let mut buffer = EditBuffer::new();
        let command = classify_key_event(&key(KeyCode::Char('€'), ctrl_alt | KeyModifiers::SHIFT))
            .expect("shifted AltGr must classify on Windows");
        let _ = buffer.apply(command);
        assert_eq!(buffer.text(), "€");
    }
}
