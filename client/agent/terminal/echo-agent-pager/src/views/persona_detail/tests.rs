use super::*;
use crossterm::event::KeyModifiers;

fn editable_state() -> (tempfile::TempDir, PathBuf, PersonaDetailState) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reviewer.toml");
    std::fs::write(
        &path,
        concat!(
            "name = \"reviewer\"\n",
            "description = \"old description\"\n",
            "model = \"echoagent\"\n",
            "reasoning_effort = \"high\"\n",
            "default_isolation = \"worktree\"\n",
            "instructions = \"read only instructions\"\n",
        ),
    )
    .unwrap();
    let state = PersonaDetailState::from_toml_file(&path, true, "project").unwrap();
    (directory, path, state)
}

#[test]
fn detail_edit_save_updates_state_and_toml() {
    let (_directory, path, mut state) = editable_state();
    state.selected_field = PersonaField::Description;
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
    );
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
    );
    for ch in "new description".chars() {
        let _ = handle_persona_detail_key(
            &mut state,
            &KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
        );
    }
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    assert!(!state.is_editing());
    assert_eq!(state.description, "new description");
    assert!(state.dirty);
    let saved = std::fs::read_to_string(path).unwrap();
    assert!(saved.contains("description = \"new description\""));
}

#[test]
fn detail_edit_cancel_preserves_original_and_file() {
    let (_directory, path, mut state) = editable_state();
    let before = std::fs::read_to_string(&path).unwrap();
    state.selected_field = PersonaField::Name;
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
    );
    let _ = handle_persona_detail_key(&mut state, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(!state.is_editing());
    assert_eq!(state.name, "reviewer");
    assert!(!state.dirty);
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[test]
fn detail_unchanged_edit_does_not_write_or_mark_dirty() {
    let (_directory, path, mut state) = editable_state();
    let before = std::fs::read_to_string(&path).unwrap();
    state.selected_field = PersonaField::Model;
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    assert!(!state.is_editing());
    assert!(!state.dirty);
    assert!(state.message.is_none());
    assert_eq!(std::fs::read_to_string(path).unwrap(), before);
}

#[test]
fn multiline_values_require_source_file_editing() {
    let (_directory, _path, mut state) = editable_state();
    state.description = "first line\nsecond line".to_owned();
    state.selected_field = PersonaField::Description;

    let outcome = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(matches!(outcome, PersonaDetailOutcome::Changed));
    assert!(!state.is_editing());
    assert_eq!(state.description, "first line\nsecond line");
    assert_eq!(
        state.message.as_deref(),
        Some("Multiline values must be edited in the source file")
    );
}

#[test]
fn detail_instructions_remain_read_only_inline() {
    let (_directory, _path, mut state) = editable_state();
    state.selected_field = PersonaField::Instructions;
    let outcome = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(matches!(outcome, PersonaDetailOutcome::Changed));
    assert!(!state.is_editing());
    assert!(state.instructions_expanded);
}

#[test]
fn detail_paste_targets_only_active_editor_and_sanitizes() {
    let (_directory, _path, mut state) = editable_state();
    state.selected_field = PersonaField::Model;
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    state.set_editing_text("ab");
    let _ = state.set_editing_cursor_byte(1);
    let outcome = handle_persona_detail_paste(&mut state, "中\r\n");
    assert!(matches!(outcome, PersonaDetailOutcome::Changed));
    assert_eq!(state.editing_text(), Some("a中b"));

    let _ = handle_persona_detail_key(&mut state, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let outcome = handle_persona_detail_paste(&mut state, "ignored");
    assert!(matches!(outcome, PersonaDetailOutcome::Unchanged));
}

#[test]
fn detail_editor_uses_canonical_graphemes_and_keeps_cursor_visible() {
    let (_directory, _path, mut state) = editable_state();
    state.selected_field = PersonaField::Model;
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    let grapheme = "👩🏽\u{200d}💻";
    state.set_editing_text(format!("a{grapheme}b"));
    let _ = state.set_editing_cursor_byte(1);
    let _ = handle_persona_detail_key(
        &mut state,
        &KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
    );
    assert_eq!(state.editing_text(), Some("ab"));

    let text = format!("123456中e\u{301}{grapheme}z");
    state.set_editing_text(&text);
    let _ = state.set_editing_cursor_byte(text.len() - 1);

    let width = 10usize;
    let theme = Theme::current();
    let mut buffer = Buffer::empty(Rect::new(0, 0, width as u16, 1));
    let viewport = state.editing_viewport(width).unwrap();
    let visible = &state.editing_text().unwrap()[viewport.visible_byte_range.clone()];
    assert!(visible.contains('中'));
    assert!(visible.contains("e\u{301}"));
    assert!(visible.contains(grapheme));
    render_detail_editor(
        &mut buffer,
        0,
        0,
        width,
        state.editing_editor().unwrap(),
        Style::default(),
        &theme,
    );
    let cursor_x = viewport.cursor_display_column as u16;
    assert_eq!(buffer[(cursor_x, 0)].bg, theme.text_primary);
}
