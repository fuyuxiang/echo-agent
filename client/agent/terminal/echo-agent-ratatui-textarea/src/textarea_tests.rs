use super::*;
// crossterm types are intentionally not imported here to avoid unused warnings
use rand::prelude::*;

fn rand_grapheme(rng: &mut rand::rngs::StdRng) -> String {
    let r: u8 = rng.random_range(0..100);
    match r {
        0..=4 => "\n".to_string(),
        5..=12 => " ".to_string(),
        13..=35 => (rng.random_range(b'a'..=b'z') as char).to_string(),
        36..=45 => (rng.random_range(b'A'..=b'Z') as char).to_string(),
        46..=52 => (rng.random_range(b'0'..=b'9') as char).to_string(),
        53..=65 => {
            // Some emoji (wide graphemes)
            let choices = ["👍", "😊", "🐍", "🚀", "🧪", "🌟"];
            choices[rng.random_range(0..choices.len())].to_string()
        }
        66..=75 => {
            // CJK wide characters
            let choices = ["漢", "字", "測", "試", "你", "好", "界", "编", "码"];
            choices[rng.random_range(0..choices.len())].to_string()
        }
        76..=85 => {
            // Combining mark sequences
            let base = ["e", "a", "o", "n", "u"][rng.random_range(0..5)];
            let marks = ["\u{0301}", "\u{0308}", "\u{0302}", "\u{0303}"];
            format!("{base}{}", marks[rng.random_range(0..marks.len())])
        }
        86..=92 => {
            // Some non-latin single codepoints (Greek, Cyrillic, Hebrew)
            let choices = ["Ω", "β", "Ж", "ю", "ש", "م", "ह"];
            choices[rng.random_range(0..choices.len())].to_string()
        }
        _ => {
            // ZWJ sequences (single graphemes but multi-codepoint)
            let choices = [
                "👩\u{200D}💻", // woman technologist
                "👨\u{200D}💻", // man technologist
                "🏳️\u{200D}🌈", // rainbow flag
            ];
            choices[rng.random_range(0..choices.len())].to_string()
        }
    }
}

fn ta_with(text: &str) -> TextArea {
    let mut t = TextArea::new();
    t.insert_str(text);
    t
}

#[test]
fn canonical_adapter_matches_standalone_edit_buffer() {
    let cases = [
        (
            "hello-world",
            "hello-world".len(),
            EditCommand::MoveWordLeft(WordStyle::Small),
        ),
        (
            "hello-world",
            0,
            EditCommand::MoveWordRight(WordStyle::Small),
        ),
        (
            "foo bar",
            "foo bar".len(),
            EditCommand::DeleteWordBackward(WordStyle::Small),
        ),
        (
            "foo bar",
            0,
            EditCommand::DeleteWordForward(WordStyle::Small),
        ),
        ("one\ntwo", 4, EditCommand::MoveLogicalLineStart),
        ("one\ntwo", 3, EditCommand::MoveLogicalLineEnd),
        ("abc", 2, EditCommand::DeleteGraphemeBackward),
        ("abc", 1, EditCommand::DeleteGraphemeForward),
    ];

    for (text, cursor, command) in cases {
        let mut textarea = TextArea::new();
        textarea.set_text(text);
        textarea.clear_history();
        textarea.set_cursor(cursor);
        let mut buffer = EditBuffer::from_parts(text, cursor);

        textarea.apply_classified_command(command);
        let _ = buffer.apply(command);

        assert_eq!(textarea.text(), buffer.text());
        assert_eq!(textarea.cursor(), buffer.cursor_byte());
    }
}

#[test]
fn canonical_adapter_updates_selection_from_applied_delta() {
    let mut textarea = ta_with("abcdef");
    textarea.set_selection(4, 6);
    textarea.replace_range(0..2, "X");
    assert_eq!(textarea.text(), "Xcdef");
    assert_eq!(textarea.selection_range(), Some(3..5));
}

#[test]
fn canonical_adapter_applies_same_byte_metadata_edits_with_history() {
    let mut textarea = TextArea::new();
    let id = textarea.insert_element("TOKEN", ElementKind(1), None);
    textarea.set_selection(0, 5);
    textarea.clear_history();

    textarea.replace_range(0..5, "TOKEN");
    assert_eq!(textarea.text(), "TOKEN");
    assert!(textarea.elements().is_empty());
    assert!(textarea.selection.is_none());
    assert!(textarea.can_undo());

    assert!(textarea.undo());
    assert_eq!(textarea.text(), "TOKEN");
    assert_eq!(textarea.elements().len(), 1);
    assert_eq!(textarea.elements()[0].id, id);
    assert!(textarea.redo());
    assert_eq!(textarea.text(), "TOKEN");
    assert!(textarea.elements().is_empty());
}

#[test]
fn replace_element_forces_cursor_end_and_restores_metadata() {
    let mut before = ta_with("left TOKEN right");
    before.clear_history();
    before.set_cursor(0);
    let id = before.replace_range_with_element(5..10, "NODE", ElementKind(1), None);
    let end = 5 + "NODE".len();
    assert_eq!(before.cursor(), end);
    assert_eq!(before.elements()[0].id, id);

    assert!(before.undo());
    assert_eq!(before.text(), "left TOKEN right");
    assert!(before.elements().is_empty());
    assert_eq!(before.cursor(), 0);
    assert!(before.redo());
    assert_eq!(before.text(), "left NODE right");
    assert_eq!(before.elements()[0].id, id);
    assert_eq!(before.cursor(), end);

    let mut after = ta_with("left TOKEN right");
    after.clear_history();
    after.set_cursor(after.text().len());
    after.replace_range_with_element(5..10, "NODE", ElementKind(1), None);
    assert_eq!(after.cursor(), end);
}

#[test]
fn empty_set_text_invalidates_redo_and_is_undoable() {
    let mut textarea = TextArea::new();
    textarea.insert_str("x");
    assert!(textarea.undo());
    assert!(textarea.can_redo());

    textarea.set_text("");
    assert!(!textarea.can_redo());
    assert!(textarea.can_undo());
    assert_eq!(textarea.cursor(), 0);
}

#[test]
fn set_text_preserves_cursor_clamped_across_grow_and_shrink() {
    let mut grow = ta_with("abcd");
    grow.set_cursor(2);
    grow.set_text("abcdefgh");
    assert_eq!(grow.cursor(), 2);

    let mut shrink = ta_with("abcdefgh");
    shrink.set_cursor(6);
    shrink.set_text("abc");
    assert_eq!(shrink.cursor(), 3);
}

#[test]
fn set_text_restores_zero_length_element_metadata_through_history() {
    let mut textarea = TextArea::new();
    let id = textarea.insert_element("", ElementKind(7), None);
    textarea.clear_history();

    textarea.set_text("");
    assert!(textarea.elements().is_empty());
    assert!(textarea.undo());
    assert_eq!(textarea.text(), "");
    assert_eq!(textarea.elements().len(), 1);
    assert_eq!(textarea.elements()[0].id, id);
    assert_eq!(textarea.elements()[0].range, 0..0);
    assert!(textarea.redo());
    assert!(textarea.elements().is_empty());
}

#[test]
fn rejected_adapter_plan_has_no_side_effects() {
    let mut textarea = TextArea::new();
    let id = textarea.insert_element("TOKEN", ElementKind(1), None);
    textarea.set_selection(0, 5);
    textarea.kill_buffer = "sentinel".to_owned();
    textarea.preferred_col = Some(3);
    textarea.scroll_override = Some(2);
    let _ = textarea.desired_height(20);
    textarea.clear_history();
    let plan = textarea.plan_edit_replacement(0..5, "X");
    let _ = textarea.text.set_cursor_byte(0);

    let result = textarea.try_apply_edit_plan(plan, Some(MutationKind::Replace));
    assert_eq!(result, Err(ApplyEditPlanError::StalePlan));
    assert_eq!(textarea.text(), "TOKEN");
    assert_eq!(textarea.elements().len(), 1);
    assert_eq!(textarea.elements()[0].id, id);
    assert_eq!(textarea.selection_range(), Some(0..5));
    assert_eq!(textarea.kill_buffer, "sentinel");
    assert_eq!(textarea.preferred_col, Some(3));
    assert_eq!(textarea.scroll_override, Some(2));
    assert!(textarea.wrap_cache.borrow().is_some());
    assert!(!textarea.can_undo());
}

#[test]
fn handled_boundary_navigation_clears_vertical_affinity() {
    let mut textarea = ta_with("ab\nwxyz");
    textarea.set_cursor(0);
    textarea.preferred_col = Some(3);
    textarea.scroll_override = Some(2);

    textarea.move_cursor_left();
    assert_eq!(textarea.cursor(), 0);
    assert_eq!(textarea.preferred_col, None);
    assert_eq!(textarea.scroll_override, None);

    textarea.move_cursor_down();
    assert_eq!(textarea.cursor(), 3);
}

#[test]
fn insert_str_at_inside_element_clamps_to_an_atomic_boundary() {
    let mut textarea = TextArea::new();
    textarea.insert_str("a");
    textarea.insert_element("TOKEN", ElementKind(1), None);
    textarea.insert_str("b");
    textarea.clear_history();

    textarea.insert_str_at(3, "X");
    assert_eq!(textarea.text(), "aXTOKENb");
    assert_eq!(textarea.cursor(), 8);
    assert_eq!(textarea.elements()[0].range, 2..7);
    assert!(textarea.undo());
    assert_eq!(textarea.text(), "aTOKENb");
    assert_eq!(textarea.elements()[0].range, 1..6);
}

#[test]
fn canonical_adapter_keeps_elements_atomic_for_motion_and_deletion() {
    let mut backward = TextArea::new();
    backward.insert_str("a");
    let id = backward.insert_element("TOKEN", ElementKind(1), None);
    backward.insert_str("b");
    let range = backward.elements()[0].range.clone();
    backward.set_cursor(range.end);
    backward.move_cursor_left();
    assert_eq!(backward.cursor(), range.start);
    backward.move_cursor_right();
    assert_eq!(backward.cursor(), range.end);
    backward.delete_backward(1);
    assert_eq!(backward.text(), "ab");
    assert!(backward.elements().iter().all(|element| element.id != id));

    let mut forward = TextArea::new();
    forward.insert_str("a");
    forward.insert_element("TOKEN", ElementKind(1), None);
    forward.insert_str("b");
    let range = forward.elements()[0].range.clone();
    forward.set_cursor(range.start);
    forward.delete_forward(1);
    assert_eq!(forward.text(), "ab");
    assert!(forward.elements().is_empty());
}

#[test]
fn canonical_adapter_ignores_element_newlines_and_restores_kills() {
    let mut textarea = TextArea::new();
    textarea.insert_str("a");
    textarea.insert_element("X\nY", ElementKind(1), None);
    textarea.insert_str("b\nc");
    textarea.clear_history();

    textarea.set_cursor(0);
    textarea.move_cursor_to_end_of_line(true);
    assert_eq!(textarea.cursor(), 5);
    textarea.set_cursor(0);
    textarea.kill_to_end_of_line();
    assert_eq!(textarea.text(), "\nc");
    assert_eq!(textarea.kill_buffer, "aX\nYb");

    assert!(textarea.undo());
    assert_eq!(textarea.text(), "aX\nYb\nc");
    assert_eq!(textarea.elements().len(), 1);
    assert!(textarea.redo());
    assert_eq!(textarea.text(), "\nc");
    assert!(textarea.elements().is_empty());
}

#[test]
fn canonical_adapter_preserves_right_affinity_through_undo_redo() {
    let woman = "👩";
    let tail = "👩🏽\u{200d}💻";
    let original = format!("{woman}{tail}");
    let mut textarea = ta_with(&original);
    textarea.clear_history();
    textarea.set_cursor(woman.len());

    textarea.insert_str("\u{200d}");
    assert_eq!(textarea.text().graphemes(true).count(), 1);
    assert_eq!(textarea.cursor(), textarea.text().len());

    assert!(textarea.undo());
    assert_eq!(textarea.text(), original);
    assert_eq!(textarea.cursor(), woman.len());
    assert!(textarea.redo());
    assert_eq!(textarea.text().graphemes(true).count(), 1);
    assert_eq!(textarea.cursor(), textarea.text().len());
}

#[test]
fn is_undo_input_accepts_ctrl_and_cmd_z() {
    assert!(is_undo_input(&KeyEvent::new(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL
    )));
    assert!(is_undo_input(&KeyEvent::new(
        KeyCode::Char('z'),
        KeyModifiers::SUPER
    )));
}

#[test]
fn is_undo_input_rejects_redo_and_plain_z() {
    // Uppercase 'Z' (redo) stays excluded so the guard is disjoint from
    // the redo arm regardless of match order.
    assert!(!is_undo_input(&KeyEvent::new(
        KeyCode::Char('Z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
    // A bare 'z' (no chord modifier) is plain typing, not undo.
    assert!(!is_undo_input(&KeyEvent::new(
        KeyCode::Char('z'),
        KeyModifiers::NONE
    )));
    assert!(!is_undo_input(&KeyEvent::new(
        KeyCode::Char('z'),
        KeyModifiers::SHIFT
    )));
}

#[test]
fn insert_and_replace_update_cursor_and_text() {
    // insert helpers
    let mut t = ta_with("hello");
    t.set_cursor(5);
    t.insert_str("!");
    assert_eq!(t.text(), "hello!");
    assert_eq!(t.cursor(), 6);

    t.insert_str_at(0, "X");
    assert_eq!(t.text(), "Xhello!");
    assert_eq!(t.cursor(), 7);

    // Insert after the cursor should not move it
    t.set_cursor(1);
    let end = t.text().len();
    t.insert_str_at(end, "Y");
    assert_eq!(t.text(), "Xhello!Y");
    assert_eq!(t.cursor(), 1);

    // replace_range cases
    // 1) cursor before range
    let mut t = ta_with("abcd");
    t.set_cursor(1);
    t.replace_range(2..3, "Z");
    assert_eq!(t.text(), "abZd");
    assert_eq!(t.cursor(), 1);

    // 2) cursor inside range
    let mut t = ta_with("abcd");
    t.set_cursor(2);
    t.replace_range(1..3, "Q");
    assert_eq!(t.text(), "aQd");
    assert_eq!(t.cursor(), 2);

    // 3) cursor after range with shifted by diff
    let mut t = ta_with("abcd");
    t.set_cursor(4);
    t.replace_range(0..1, "AA");
    assert_eq!(t.text(), "AAbcd");
    assert_eq!(t.cursor(), 5);
}

#[test]
fn delete_backward_and_forward_edges() {
    let mut t = ta_with("abc");
    t.set_cursor(1);
    t.delete_backward(1);
    assert_eq!(t.text(), "bc");
    assert_eq!(t.cursor(), 0);

    // deleting backward at start is a no-op
    t.set_cursor(0);
    t.delete_backward(1);
    assert_eq!(t.text(), "bc");
    assert_eq!(t.cursor(), 0);

    // forward delete removes next grapheme
    t.set_cursor(1);
    t.delete_forward(1);
    assert_eq!(t.text(), "b");
    assert_eq!(t.cursor(), 1);

    // forward delete at end is a no-op
    t.set_cursor(t.text().len());
    t.delete_forward(1);
    assert_eq!(t.text(), "b");
}

#[test]
fn delete_backward_word_and_kill_line_variants() {
    // delete backward word at end removes the whole previous word
    let mut t = ta_with("hello   world  ");
    t.set_cursor(t.text().len());
    t.delete_backward_word();
    assert_eq!(t.text(), "hello   ");
    assert_eq!(t.cursor(), 8);

    // From inside a word, delete from word start to cursor
    let mut t = ta_with("foo bar");
    t.set_cursor(6); // inside "bar" (after 'a')
    t.delete_backward_word();
    assert_eq!(t.text(), "foo r");
    assert_eq!(t.cursor(), 4);

    // From end, delete the last word only
    let mut t = ta_with("foo bar");
    t.set_cursor(t.text().len());
    t.delete_backward_word();
    assert_eq!(t.text(), "foo ");
    assert_eq!(t.cursor(), 4);

    let mut t = ta_with("hello-world");
    t.set_cursor(t.text().len());
    t.delete_backward_word();
    assert_eq!(t.text(), "hello-");
    assert_eq!(t.cursor(), "hello-".len());

    // kill_to_end_of_line when not at EOL
    let mut t = ta_with("abc\ndef");
    t.set_cursor(1); // on first line, middle
    t.kill_to_end_of_line();
    assert_eq!(t.text(), "a\ndef");
    assert_eq!(t.cursor(), 1);

    // kill_to_end_of_line when at EOL deletes newline
    let mut t = ta_with("abc\ndef");
    t.set_cursor(3); // EOL of first line
    t.kill_to_end_of_line();
    assert_eq!(t.text(), "abcdef");
    assert_eq!(t.cursor(), 3);

    // kill_to_beginning_of_line from middle of line
    let mut t = ta_with("abc\ndef");
    t.set_cursor(5); // on second line, after 'e'
    t.kill_to_beginning_of_line();
    assert_eq!(t.text(), "abc\nef");

    // kill_to_beginning_of_line at beginning of non-first line removes the previous newline
    let mut t = ta_with("abc\ndef");
    t.set_cursor(4); // beginning of second line
    t.kill_to_beginning_of_line();
    assert_eq!(t.text(), "abcdef");
    assert_eq!(t.cursor(), 3);

    // kill_current_line from middle of single line
    let mut t = ta_with("hello world");
    t.set_cursor(5);
    t.kill_current_line();
    assert_eq!(t.text(), "");
    assert_eq!(t.cursor(), 0);

    // kill_current_line from middle of multiline
    let mut t = ta_with("abc\ndef\nghi");
    t.set_cursor(5);
    t.kill_current_line();
    assert_eq!(t.text(), "abc\n\nghi");
    assert_eq!(t.cursor(), 4);

    // kill_current_line on empty line joins with previous
    let mut t = ta_with("abc\n\nghi");
    t.set_cursor(4);
    t.kill_current_line();
    assert_eq!(t.text(), "abc\nghi");
    assert_eq!(t.cursor(), 3);

    // kill_current_line at beginning of only line
    let mut t = ta_with("hello");
    t.set_cursor(0);
    t.kill_current_line();
    assert_eq!(t.text(), "");
    assert_eq!(t.cursor(), 0);
}

#[test]
fn delete_forward_word_variants() {
    let mut t = ta_with("hello   world ");
    t.set_cursor(0);
    t.delete_forward_word();
    assert_eq!(t.text(), "   world ");
    assert_eq!(t.cursor(), 0);

    let mut t = ta_with("hello   world ");
    t.set_cursor(1);
    t.delete_forward_word();
    assert_eq!(t.text(), "h   world ");
    assert_eq!(t.cursor(), 1);

    let mut t = ta_with("hello   world");
    t.set_cursor(t.text().len());
    t.delete_forward_word();
    assert_eq!(t.text(), "hello   world");
    assert_eq!(t.cursor(), t.text().len());

    let mut t = ta_with("foo   \nbar");
    t.set_cursor(3);
    t.delete_forward_word();
    assert_eq!(t.text(), "foo");
    assert_eq!(t.cursor(), 3);

    let mut t = ta_with("foo\nbar");
    t.set_cursor(3);
    t.delete_forward_word();
    assert_eq!(t.text(), "foo");
    assert_eq!(t.cursor(), 3);

    let mut t = ta_with("hello-world");
    t.set_cursor(0);
    t.delete_forward_word();
    assert_eq!(t.text(), "-world");
    assert_eq!(t.cursor(), 0);

    let mut t = ta_with("hello   world ");
    t.set_cursor(t.text().len() + 10);
    t.delete_forward_word();
    assert_eq!(t.text(), "hello   world ");
    assert_eq!(t.cursor(), t.text().len());
}

#[test]
fn super_right_moves_to_end_of_line() {
    let mut t = ta_with("hello world\nsecond line");
    t.set_cursor(3); // middle of "hello world"
    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::SUPER));
    assert_eq!(t.cursor(), 11); // end of "hello world" (before \n)

    // Already at end of line → stays there
    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::SUPER));
    assert_eq!(t.cursor(), 11);
}

#[test]
fn super_left_moves_to_beginning_of_line() {
    let mut t = ta_with("hello world\nsecond line");
    let second_line_start = t.text().find("second").unwrap();
    t.set_cursor(second_line_start + 4); // middle of "second line"
    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::SUPER));
    assert_eq!(t.cursor(), second_line_start);

    // Already at beginning of line → stays there
    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::SUPER));
    assert_eq!(t.cursor(), second_line_start);
}

#[test]
fn super_backspace_kills_to_beginning_of_line() {
    let mut t = ta_with("hello world\nsecond line");
    let second_line_start = t.text().find("second").unwrap();
    t.set_cursor(second_line_start + 7); // after "second "
    t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::SUPER));
    assert_eq!(t.text(), "hello world\nline");
    assert_eq!(t.cursor(), second_line_start);
}

#[test]
fn ctrl_u_kills_to_beginning_of_line_keeps_text_after_cursor() {
    let mut t = ta_with("hello world");
    t.set_cursor(5);
    t.input(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), " world");
    assert_eq!(t.cursor(), 0);
}

#[test]
fn delete_forward_word_handles_atomic_elements() {
    let kind = ElementKind(0);

    let mut t = TextArea::new();
    t.insert_element("<element>", kind, None);
    t.insert_str(" tail");

    t.set_cursor(0);
    t.delete_forward_word();
    assert_eq!(t.text(), " tail");
    assert_eq!(t.cursor(), 0);

    let mut t = TextArea::new();
    t.insert_str("   ");
    t.insert_element("<element>", kind, None);
    t.insert_str(" tail");

    t.set_cursor(0);
    t.delete_forward_word();
    assert_eq!(t.text(), " tail");
    assert_eq!(t.cursor(), 0);

    let mut t = TextArea::new();
    t.insert_str("prefix ");
    t.insert_element("<element>", kind, None);
    t.insert_str(" tail");

    // cursor in the middle of the element, delete_forward_word deletes the element
    let elem_range = t.elements()[0].range.clone();
    let _ = t
        .text
        .set_cursor_byte(elem_range.start + (elem_range.len() / 2));
    t.delete_forward_word();
    assert_eq!(t.text(), "prefix  tail");
    assert_eq!(t.cursor(), elem_range.start);
}

// ===== Phase 1: Typed element tests =====

#[test]
fn element_id_is_unique_and_stable() {
    let mut t = TextArea::new();
    let kind = ElementKind(1);

    let id1 = t.insert_element("aaa", kind, None);
    let id2 = t.insert_element("bbb", kind, None);
    assert_ne!(id1, id2);

    // ids survive after deletion of the first element
    t.set_cursor(0);
    t.delete_forward(1); // deletes "aaa" atomically
    assert_eq!(t.elements().len(), 1);
    assert_eq!(t.elements()[0].id, id2);
}

#[test]
fn element_kind_preserved() {
    let mut t = TextArea::new();
    let kind_paste = ElementKind(1);
    let kind_file = ElementKind(2);

    t.insert_element("paste", kind_paste, None);
    t.insert_element("file", kind_file, None);

    assert_eq!(t.elements()[0].kind, kind_paste);
    assert_eq!(t.elements()[1].kind, kind_file);
}

#[test]
fn element_at_cursor_returns_element() {
    let mut t = TextArea::new();
    let kind = ElementKind(0);

    t.insert_str("before ");
    let id = t.insert_element("[paste]", kind, None);
    t.insert_str(" after");

    // Cursor is at end of element after insert_element
    // Move to start of element
    t.set_cursor(7); // "before " is 7 bytes, element starts at 7
    let elem = t.element_at_cursor().expect("should find element");
    assert_eq!(elem.id, id);
    assert_eq!(elem.kind, kind);

    // Cursor before element
    t.set_cursor(0);
    assert!(t.element_at_cursor().is_none());

    // Cursor after element
    t.set_cursor(t.text().len());
    assert!(t.element_at_cursor().is_none());
}

#[test]
fn element_text_returns_buffer_text() {
    let mut t = TextArea::new();
    let id = t.insert_element("raw buffer content", ElementKind(0), None);
    assert_eq!(t.element_text(id), Some("raw buffer content"));

    // Non-existent id returns None
    let fake_id = ElementId(9999);
    assert_eq!(t.element_text(fake_id), None);
}

#[test]
fn element_display_can_be_set_and_updated() {
    let mut t = TextArea::new();
    let display = Line::from("[Pasted 5 lines]");
    let id = t.insert_element("lots of raw text here", ElementKind(1), Some(display));

    // Verify display is set
    let elem = &t.elements()[0];
    assert!(elem.display.is_some());
    assert_eq!(
        elem.display.as_ref().unwrap().to_string(),
        "[Pasted 5 lines]"
    );

    // Update display
    let new_display = Line::from("[Pasted 5 lines, 200 chars]");
    t.set_element_display(id, Some(new_display));
    let elem = &t.elements()[0];
    assert_eq!(
        elem.display.as_ref().unwrap().to_string(),
        "[Pasted 5 lines, 200 chars]"
    );

    // Clear display
    t.set_element_display(id, None);
    assert!(t.elements()[0].display.is_none());

    // Buffer text is unchanged
    assert_eq!(t.element_text(id), Some("lots of raw text here"));
}

#[test]
fn insert_element_returns_id_for_metadata_tracking() {
    let mut t = TextArea::new();
    let mut metadata: std::collections::HashMap<ElementId, String> =
        std::collections::HashMap::new();

    let id1 = t.insert_element("paste1", ElementKind(1), None);
    metadata.insert(id1, "First paste".to_string());

    let id2 = t.insert_element("paste2", ElementKind(1), None);
    metadata.insert(id2, "Second paste".to_string());

    // Verify we can look up metadata by id
    assert_eq!(metadata.get(&id1), Some(&"First paste".to_string()));
    assert_eq!(metadata.get(&id2), Some(&"Second paste".to_string()));

    // Delete first element
    t.set_cursor(0);
    t.delete_forward(1);

    // id2 still valid in our metadata map
    let remaining = &t.elements()[0];
    assert_eq!(remaining.id, id2);
    assert_eq!(
        metadata.get(&remaining.id),
        Some(&"Second paste".to_string())
    );
}

#[test]
fn elements_returns_sorted_slice() {
    let mut t = TextArea::new();
    let kind = ElementKind(0);

    t.insert_str("aaa ");
    t.insert_element("BBB", kind, None);
    t.insert_str(" ccc ");
    t.insert_element("DDD", kind, None);

    let elems = t.elements();
    assert_eq!(elems.len(), 2);
    assert!(elems[0].range.start < elems[1].range.start);
    assert_eq!(&t.text()[elems[0].range.clone()], "BBB");
    assert_eq!(&t.text()[elems[1].range.clone()], "DDD");
}

// ===== Phase 2: Display rendering & truncation tests =====

#[test]
fn render_element_with_display_shows_display_text() {
    use ratatui::style::Stylize;

    let mut t = TextArea::new();
    let display = Line::from("[Pasted]".cyan());
    t.insert_element("raw content here", ElementKind(0), Some(display));

    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::WidgetRef::render_ref(&(&t), area, &mut buf);

    // The rendered buffer should show "[Pasted]" not "raw content here"
    let rendered: String = (0..area.width)
        .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
        .collect::<String>();
    let rendered = rendered.trim_end();
    assert_eq!(rendered, "[Pasted]");
}

#[test]
fn render_element_without_display_shows_buffer_text_cyan() {
    let mut t = TextArea::new();
    t.insert_element("hello", ElementKind(0), None);

    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::WidgetRef::render_ref(&(&t), area, &mut buf);

    // Should show "hello" with cyan foreground
    let cell = buf.cell((0, 0)).unwrap();
    assert_eq!(cell.symbol(), "h");
    assert_eq!(cell.fg, Color::Cyan);
}

#[test]
fn truncate_line_display_no_truncation_needed() {
    let line = Line::from("[Short]");
    let result = truncate_line_display(&line, 20);
    let text: String = result.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "[Short]");
}

#[test]
fn truncate_line_display_with_bracket_preservation() {
    let line: Line<'static> = Line::from("[Pasted ~100 lines]");
    // Width 12: budget = 12 - 2 (ellipsis + bracket) = 10 chars content
    let result = truncate_line_display(&line, 12);
    let text: String = result.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.ends_with(']'), "should preserve ]: got {text:?}");
    assert!(text.contains('…'), "should contain ellipsis: got {text:?}");
    assert_eq!(text, "[Pasted ~1…]");
}

#[test]
fn truncate_line_display_without_bracket() {
    let line: Line<'static> = Line::from("very long display text");
    let result = truncate_line_display(&line, 10);
    let text: String = result.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains('…'));
    assert!(!text.ends_with(']'));
    // 9 chars content + 1 ellipsis = 10
    assert_eq!(text, "very long…");
}

#[test]
fn truncate_line_display_zero_width() {
    let line: Line<'static> = Line::from("[Pasted]");
    let result = truncate_line_display(&line, 0);
    assert!(result.spans.is_empty() || result.width() == 0);
}

#[test]
fn truncate_line_display_width_1() {
    let line: Line<'static> = Line::from("[Pasted]");
    let result = truncate_line_display(&line, 1);
    let text: String = result.spans.iter().map(|s| s.content.as_ref()).collect();
    // Only room for "…" (the bracket can't fit with content)
    assert_eq!(text, "…");
}

#[test]
fn truncate_preserves_multi_span_styles() {
    use ratatui::text::Span;

    let line: Line<'static> = Line::from(vec![
        Span::styled("[", Style::default().fg(Color::Yellow)),
        Span::styled("Pasted ~100 lines", Style::default().fg(Color::Cyan)),
        Span::styled("]", Style::default().fg(Color::Yellow)),
    ]);
    let result = truncate_line_display(&line, 10);
    let text: String = result.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.ends_with(']'));
    assert!(text.contains('…'));
    // "[" (1) + content budget (10-2=8) + "…" (1) + "]" (1) = 11? No...
    // Budget: 10 - 2 = 8 for content. "[" is 1, so 7 more chars of "Pasted ~"
    // Result: "[Pasted …]" which is 10 wide
    assert!(
        result.width() <= 10,
        "width should be <= 10, got {}",
        result.width()
    );
}

#[test]
fn render_element_with_prefix_text() {
    let mut t = TextArea::new();
    t.insert_str("hi ");
    let display = Line::from("[P]");
    t.insert_element("raw", ElementKind(0), Some(display));

    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::WidgetRef::render_ref(&(&t), area, &mut buf);

    let rendered: String = (0..area.width)
        .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
        .collect::<String>();
    let rendered = rendered.trim_end();
    assert_eq!(rendered, "hi [P]");
}

#[test]
fn render_text_after_element_uses_display_width() {
    // User scenario: "foo " + element("Clean build", display="[📎 Pasted 1 line, 11 chars]") + " abcde"
    // Display: "[📎 Pasted 1 line, 11 chars]" = 1+2+1+23+1 = 28 display cols
    // Buffer: "Clean build" = 11 bytes
    // Without fix, text after element renders at buffer x, overlapping with element display.
    let mut t = TextArea::new();
    t.insert_str("foo ");
    let display = Line::from(vec![
        ratatui::text::Span::raw("["),
        ratatui::text::Span::raw("📎 "),
        ratatui::text::Span::raw("Pasted 1 line, 11 chars"),
        ratatui::text::Span::raw("]"),
    ]);
    t.insert_element("Clean build", ElementKind(0), Some(display));
    t.insert_str(" abcde");

    // Area wide enough to fit everything
    let area = Rect::new(0, 0, 80, 1);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::WidgetRef::render_ref(&(&t), area, &mut buf);

    // Verify key cells: text before element, element display, text after element.
    // "foo " occupies cols 0-3
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "f");
    assert_eq!(buf.cell((3, 0)).unwrap().symbol(), " ");

    // Element display starts at col 4: "[📎 Pasted 1 line, 11 chars]"
    assert_eq!(buf.cell((4, 0)).unwrap().symbol(), "[");
    assert_eq!(buf.cell((5, 0)).unwrap().symbol(), "📎");
    // col 6 is the wide-char continuation cell
    assert_eq!(buf.cell((7, 0)).unwrap().symbol(), " ");
    assert_eq!(buf.cell((8, 0)).unwrap().symbol(), "P");

    // Element display ends at col 31 ("]")
    assert_eq!(buf.cell((31, 0)).unwrap().symbol(), "]");

    // Text after element: " abcde" starting at col 32
    assert_eq!(buf.cell((32, 0)).unwrap().symbol(), " ");
    assert_eq!(buf.cell((33, 0)).unwrap().symbol(), "a");
    assert_eq!(buf.cell((34, 0)).unwrap().symbol(), "b");
    assert_eq!(buf.cell((35, 0)).unwrap().symbol(), "c");
    assert_eq!(buf.cell((36, 0)).unwrap().symbol(), "d");
    assert_eq!(buf.cell((37, 0)).unwrap().symbol(), "e");
}

#[test]
fn render_text_after_wider_display_element_simple() {
    // Simpler case: element buffer text "x" (1 byte), display "[LONG]" (6 cols)
    // Suffix text "!" should render at column 6, not column 1.
    let mut t = TextArea::new();
    let display = Line::from("[LONG]");
    t.insert_element("x", ElementKind(0), Some(display));
    t.insert_str("!");

    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::WidgetRef::render_ref(&(&t), area, &mut buf);

    let rendered: String = (0..area.width)
        .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
        .collect::<String>();
    let rendered = rendered.trim_end();
    assert_eq!(rendered, "[LONG]!");
}

// ===== Phase 3: Display projection tests =====

#[test]
fn display_width_of_range_plain_text() {
    let t = ta_with("hello world");
    assert_eq!(t.display_width_of_range(0, 5), 5); // "hello"
    assert_eq!(t.display_width_of_range(0, 11), 11); // "hello world"
    assert_eq!(t.display_width_of_range(6, 11), 5); // "world"
}

#[test]
fn insert_expands_tabs_to_spaces() {
    let mut t = TextArea::new();
    assert_eq!(t.tab_width(), 4);
    t.insert_str("a\tb");
    assert_eq!(t.text(), "a    b");
    assert_eq!(t.cursor(), 6);
    assert_eq!(t.display_width_of_range(0, t.text().len()), 6);

    let mut t2 = TextArea::new();
    t2.set_tab_width(8);
    t2.insert_str("x\ty");
    assert_eq!(t2.text(), "x        y");
    assert_eq!(t2.cursor(), 10);
    assert_eq!(t2.display_width_of_range(0, t2.text().len()), 10);

    let mut t3 = TextArea::new();
    t3.insert_str("\ta");
    assert_eq!(t3.text(), "    a");
    t3.set_text("");
    t3.insert_str("a\t");
    assert_eq!(t3.text(), "a    ");
    assert_eq!(t3.cursor(), 5);
    t3.set_text("");
    t3.insert_str("\t\t");
    assert_eq!(t3.text(), "        ");
    assert_eq!(t3.display_width_of_range(0, 8), 8);
    t3.insert_str("");
    assert_eq!(t3.text(), "        ");
    t3.insert_str_at(0, "z\t");
    assert_eq!(t3.text(), "z            ");
}

#[test]
fn set_text_and_replace_expand_tabs() {
    let mut t = TextArea::new();
    t.set_text("col1\tcol2");
    assert_eq!(t.text(), "col1    col2");

    t.replace_range(4..8, "\t");
    assert_eq!(t.text(), "col1    col2");

    t.replace_range(4..4, "\tx");
    assert_eq!(t.text(), "col1    x    col2");
    // Insert-only replace places cursor at end of inserted expansion (4 spaces + 'x').
    assert_eq!(&t.text()[4..9], "    x");

    let mut t0 = TextArea::new();
    t0.set_tab_width(0);
    t0.insert_str("a\tb");
    assert_eq!(t0.text(), "a\tb");
    t0.set_text("x\ty");
    assert_eq!(t0.text(), "x\ty");
    // Passthrough: display width matches unicode-width (no expansion).
    assert_eq!(
        t0.display_width_of_range(0, t0.text().len()),
        "x\ty".width()
    );
    t0.set_cursor(t0.text().len());
    let area = Rect::new(0, 0, 80, 1);
    let (x, _y) = t0.cursor_pos(area).unwrap();
    assert_eq!(x as usize, "x\ty".width());
}

#[test]
fn remaining_tabs_count_in_display_width_and_cursor() {
    // Simulate leftover tabs without going through expand (tab_width set after set_text
    // would still expand on set_text; inject via tab_width=0 then enable display tabs).
    let mut t = TextArea::new();
    t.set_tab_width(0);
    t.set_text("a\tb\tc");
    assert!(t.text().contains('\t'));
    t.set_tab_width(4);
    // "a" + 4 + "b" + 4 + "c" = 11
    assert_eq!(t.display_width_of_range(0, t.text().len()), 11);
    t.set_cursor(t.text().len());
    let area = Rect::new(0, 0, 80, 1);
    let (x, _y) = t.cursor_pos(area).unwrap();
    assert_eq!(x, 11);
}

#[test]
fn set_tab_width_does_not_rewrite_existing_spaces() {
    let mut t = TextArea::new();
    t.insert_str("a\tb");
    assert_eq!(t.text(), "a    b");
    t.set_tab_width(8);
    assert_eq!(t.text(), "a    b");
    t.insert_str("\tc");
    assert_eq!(t.text(), "a    b        c");
}

#[test]
fn multi_column_paste_tabs_readable() {
    let mut t = TextArea::new();
    t.insert_str("Name\tAge\tCity\nAda\t36\tLondon");
    assert_eq!(t.text(), "Name    Age    City\nAda    36    London");
    assert_eq!(t.cursor(), t.text().len());
    let end = t.text().len();
    let area = Rect::new(0, 0, 80, 3);
    let (x, _y) = t.cursor_pos(area).unwrap();
    // Ada(3) + 4 + 36(2) + 4 + London(6) = 19
    assert_eq!(x, 19);
    let bol = t.text().rfind('\n').map(|i| i + 1).unwrap_or(0);
    assert_eq!(x as usize, t.display_width_of_range(bol, end));
    let last_line = &t.text()[bol..];
    let (paint, paint_w) = paint_plain_for_display(last_line, 80, 4);
    assert_eq!(paint.as_ref(), last_line);
    assert_eq!(paint_w, 19);
}

#[test]
fn insert_element_expands_tabs_and_covers_full_range() {
    let mut t = TextArea::new();
    t.insert_element("a\tb", ElementKind(0), None);
    assert_eq!(t.text(), "a    b");
    assert_eq!(t.elements().len(), 1);
    assert_eq!(t.elements()[0].range, 0..6);
    assert_eq!(t.cursor(), 6);

    let mut t2 = TextArea::new();
    t2.insert_element("a\tb\nc\td", ElementKind(1), Some(Line::from("[P]")));
    assert_eq!(t2.text(), "a    b\nc    d");
    assert_eq!(t2.elements()[0].range, 0..t2.text().len());
    assert_eq!(t2.cursor(), t2.text().len());
    assert!(!t2.text().contains('\t'));
}

#[test]
fn replace_range_with_element_expands_tabs() {
    let mut t = TextArea::new();
    t.insert_str("xx");
    t.replace_range_with_element(0..2, "a\tb", ElementKind(0), None);
    assert_eq!(t.text(), "a    b");
    assert_eq!(t.elements()[0].range, 0..6);
    assert_eq!(t.cursor(), 6);
}

#[test]
fn unicode_plus_tabs_expansion_and_residual() {
    let mut t = TextArea::new();
    t.insert_str("名\tAge");
    // 名 is typically width 2; plus 4 spaces + Age
    assert_eq!(t.text(), "名    Age");
    assert_eq!(
        t.display_width_of_range(0, t.text().len()),
        "名".width() + 4 + 3
    );
    t.set_cursor(t.text().len());
    let area = Rect::new(0, 0, 80, 1);
    let (x, _) = t.cursor_pos(area).unwrap();
    assert_eq!(x as usize, "名".width() + 4 + 3);

    let mut t2 = TextArea::new();
    t2.set_tab_width(0);
    t2.set_text("😀\tb");
    t2.set_tab_width(4);
    let expected = "😀".width() + 4 + 1;
    assert_eq!(t2.display_width_of_range(0, t2.text().len()), expected);
    t2.set_cursor(t2.text().len());
    let (x, _) = t2.cursor_pos(area).unwrap();
    assert_eq!(x as usize, expected);
}

#[test]
fn tab_helpers_clip_and_paint() {
    assert_eq!(expand_tabs_with_width("a\tb", 4).as_ref(), "a    b");
    assert!(matches!(
        expand_tabs_with_width("a\tb", 0),
        std::borrow::Cow::Borrowed("a\tb")
    ));
    assert!(matches!(
        expand_tabs_with_width("ab", 4),
        std::borrow::Cow::Borrowed("ab")
    ));
    assert_eq!(plain_display_width_with_tab("a\tb\tc", 4), 11);
    assert_eq!(
        plain_display_width_with_tab("a\tb\tc", 0),
        "a\tb\tc".width()
    );
    assert_eq!(clip_str_to_display_width_with_tab("a\tb", 3, 4), "a");
    let (paint, w) = paint_plain_for_display("a\tb", 80, 4);
    assert_eq!(paint.as_ref(), "a    b");
    assert_eq!(w, 6);
    let (paint2, w2) = paint_plain_for_display("a\tb", 3, 4);
    assert_eq!(paint2.as_ref(), "a");
    assert_eq!(w2, 1);
}

#[test]
fn display_width_of_range_with_display_element() {
    let mut t = TextArea::new();
    t.insert_str("ab");
    // Element has 100 bytes of buffer text but displays as "[P]" (3 chars)
    let buffer_text = "x".repeat(100);
    let display = Line::from("[P]");
    t.insert_element(&buffer_text, ElementKind(0), Some(display));
    t.insert_str("cd");

    // Range covering just "ab" = 2
    assert_eq!(t.display_width_of_range(0, 2), 2);
    // Range covering "ab" + element = 2 + 3 = 5
    assert_eq!(t.display_width_of_range(0, 102), 5);
    // Range covering "ab" + element + "cd" = 2 + 3 + 2 = 7
    assert_eq!(t.display_width_of_range(0, 104), 7);
    // Range covering just the element = 3
    assert_eq!(t.display_width_of_range(2, 102), 3);
    // Range covering just "cd" = 2
    assert_eq!(t.display_width_of_range(102, 104), 2);
}

#[test]
fn cursor_pos_uses_display_width() {
    let mut t = TextArea::new();
    t.insert_str("ab");
    let buffer_text = "x".repeat(50);
    let display = Line::from("[P]");
    t.insert_element(&buffer_text, ElementKind(0), Some(display));
    t.insert_str("cd");

    // Cursor at end of element (buffer pos 52)
    t.set_cursor(52);
    let area = Rect::new(0, 0, 80, 1);
    let (x, _y) = t.cursor_pos(area).unwrap();
    // Expected: "ab" (2) + "[P]" (3) = column 5
    assert_eq!(x, 5);

    // Cursor at end of text (buffer pos 54)
    t.set_cursor(54);
    let (x, _y) = t.cursor_pos(area).unwrap();
    // Expected: "ab" (2) + "[P]" (3) + "cd" (2) = column 7
    assert_eq!(x, 7);

    // Cursor at start
    t.set_cursor(0);
    let (x, _y) = t.cursor_pos(area).unwrap();
    assert_eq!(x, 0);
}

#[test]
fn display_width_no_elements() {
    let t = ta_with("abc");
    assert_eq!(t.display_width_of_range(0, 3), 3);
    assert_eq!(t.display_width_of_range(1, 2), 1);
    assert_eq!(t.display_width_of_range(3, 3), 0);
}

#[test]
fn display_width_element_without_display() {
    let mut t = TextArea::new();
    t.insert_element("elem", ElementKind(0), None);
    // No display override — width should equal buffer text width
    assert_eq!(t.display_width_of_range(0, 4), 4);
}

// ===== Wide unicode in display text =====

#[test]
fn display_width_with_wide_unicode_display() {
    let mut t = TextArea::new();
    t.insert_str("ab");
    // Display text has emoji (each width 2) and CJK
    let display = Line::from("📎漢字"); // 2 + 2 + 2 = 6 display columns
    t.insert_element("raw", ElementKind(0), Some(display));
    t.insert_str("cd");

    // "ab" = 2, element display = 6, "cd" = 2 → total 10
    assert_eq!(t.display_width_of_range(0, 2), 2);
    assert_eq!(t.display_width_of_range(2, 5), 6); // element "raw" = 3 bytes
    assert_eq!(t.display_width_of_range(0, 7), 10); // "ab" + elem + "cd"
}

#[test]
fn cursor_pos_with_wide_unicode_display() {
    let mut t = TextArea::new();
    t.insert_str("a");
    // Element display is "🚀" (width 2), buffer text is "xyz" (3 bytes)
    let display = Line::from("🚀");
    t.insert_element("xyz", ElementKind(0), Some(display));
    t.insert_str("b");

    let area = Rect::new(0, 0, 40, 1);

    // Cursor after "a" (at element start → element_at_cursor returns it)
    t.set_cursor(1);
    let (x, _) = t.cursor_pos(area).unwrap();
    assert_eq!(x, 1); // "a" = 1 col

    // Cursor after element (buffer pos 4 = 1 + 3)
    t.set_cursor(4);
    let (x, _) = t.cursor_pos(area).unwrap();
    assert_eq!(x, 3); // "a" (1) + "🚀" (2) = 3

    // Cursor at end "b" (buffer pos 5)
    t.set_cursor(5);
    let (x, _) = t.cursor_pos(area).unwrap();
    assert_eq!(x, 4); // "a" (1) + "🚀" (2) + "b" (1) = 4
}

#[test]
fn truncate_display_with_wide_unicode() {
    // Display: "📎paste" = 2+5 = 7 cols, truncate to 5
    let line: Line<'static> = Line::from("📎paste");
    let result = truncate_line_display(&line, 5);
    let text: String = result.spans.iter().map(|s| s.content.as_ref()).collect();
    // Budget = 5 - 1 (ellipsis) = 4 content cols → "📎pa" (2+1+1=4)
    assert!(text.contains('…'));
    assert!(
        result.width() <= 5,
        "width should be <= 5, got {}",
        result.width()
    );
}

#[test]
fn clip_str_to_display_width_preserves_zwj_graphemes() {
    let s = "👩\u{200D}💻a";

    assert_eq!(clip_str_to_display_width(s, 0), "");
    assert_eq!(clip_str_to_display_width(s, 1), "");
    assert_eq!(clip_str_to_display_width(s, 2), "👩\u{200D}💻");
    assert_eq!(clip_str_to_display_width(s, 3), s);
}

#[test]
fn truncate_display_preserves_zwj_graphemes() {
    let line: Line<'static> = Line::from("👩\u{200D}💻abc");
    let result = truncate_line_display(&line, 3);
    let text: String = result.spans.iter().map(|s| s.content.as_ref()).collect();

    assert_eq!(text, "👩\u{200D}💻…");
    assert_eq!(result.width(), 3);
}

#[test]
fn truncate_display_wide_char_at_boundary() {
    // Display: "ab🚀cd" = 2+2+2 = 6 cols, truncate to 4
    // Budget = 4 - 1 = 3 content cols. "ab" = 2, "🚀" = 2 → doesn't fit → "ab…"
    let line: Line<'static> = Line::from("ab🚀cd");
    let result = truncate_line_display(&line, 4);
    let text: String = result.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "ab…");
    assert!(result.width() <= 4);
}

#[test]
fn truncate_display_bracket_with_wide_chars() {
    // "[📎 pasted]" = 1+2+1+6+1 = 11 cols, truncate to 7
    // Budget = 7 - 2 (ellipsis + bracket) = 5 content cols → "[📎 p…]"
    let line: Line<'static> = Line::from("[📎 pasted]");
    let result = truncate_line_display(&line, 7);
    let text: String = result.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.ends_with(']'), "should preserve ]: got {text:?}");
    assert!(text.contains('…'));
    assert!(result.width() <= 7, "got {}", result.width());
}

#[test]
fn render_element_with_wide_unicode_display() {
    let mut t = TextArea::new();
    let display = Line::from("📎漢");
    t.insert_element("hidden text", ElementKind(0), Some(display));

    let area = Rect::new(0, 0, 20, 1);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::WidgetRef::render_ref(&(&t), area, &mut buf);

    // Should show "📎漢" (4 display cols: 2+2)
    let cell0 = buf.cell((0, 0)).unwrap();
    assert_eq!(cell0.symbol(), "📎");
    let cell2 = buf.cell((2, 0)).unwrap();
    assert_eq!(cell2.symbol(), "漢");
}

// ===== Element-aware editing behavior (explicit tests) =====

#[test]
fn backspace_at_element_end_deletes_entire_element() {
    let mut t = TextArea::new();
    t.insert_str("before ");
    t.insert_element("[paste]", ElementKind(0), None);
    // Cursor is now at end of element
    assert_eq!(t.cursor(), 14); // "before " (7) + "[paste]" (7)

    t.delete_backward(1);
    assert_eq!(t.text(), "before ");
    assert_eq!(t.cursor(), 7);
    assert!(t.elements().is_empty());
}

#[test]
fn delete_at_element_start_deletes_entire_element() {
    let mut t = TextArea::new();
    t.insert_element("[paste]", ElementKind(0), None);
    t.insert_str(" after");
    t.set_cursor(0);

    t.delete_forward(1);
    assert_eq!(t.text(), " after");
    assert_eq!(t.cursor(), 0);
    assert!(t.elements().is_empty());
}

#[test]
fn left_right_navigation_jumps_over_element() {
    let mut t = TextArea::new();
    t.insert_str("a");
    t.insert_element("[elem]", ElementKind(0), None);
    t.insert_str("b");
    // text = "a[elem]b", element at 1..7

    // Start at end, move left: should jump from 8 → 7 (before 'b'),
    // then 7 → 1 (before element, atomic jump), then 1 → 0
    t.set_cursor(8);
    t.move_cursor_left(); // 8 → 7
    assert_eq!(t.cursor(), 7);
    t.move_cursor_left(); // 7 → 1 (atomic jump over "[elem]")
    assert_eq!(t.cursor(), 1);
    t.move_cursor_left(); // 1 → 0
    assert_eq!(t.cursor(), 0);

    // Now right: 0 → 1, then 1 → 7 (atomic jump), then 7 → 8
    t.move_cursor_right(); // 0 → 1
    assert_eq!(t.cursor(), 1);
    t.move_cursor_right(); // 1 → 7 (atomic jump over "[elem]")
    assert_eq!(t.cursor(), 7);
    t.move_cursor_right(); // 7 → 8
    assert_eq!(t.cursor(), 8);
}

#[test]
fn word_delete_backward_removes_element_atomically() {
    let mut t = TextArea::new();
    t.insert_str("prefix ");
    t.insert_element("[pasted content]", ElementKind(0), None);
    // Cursor at end of element
    assert_eq!(t.cursor(), 23); // 7 + 16

    t.delete_backward_word();
    // Should remove the entire element (it's one "word" unit)
    assert_eq!(t.text(), "prefix ");
    assert!(t.elements().is_empty());
}

#[test]
fn word_delete_forward_removes_element_atomically() {
    let mut t = TextArea::new();
    t.insert_element("[element]", ElementKind(0), None);
    t.insert_str(" suffix");
    t.set_cursor(0);

    t.delete_forward_word();
    assert_eq!(t.text(), " suffix");
    assert!(t.elements().is_empty());
}

#[test]
fn kill_to_eol_removes_element_in_range() {
    let mut t = TextArea::new();
    t.insert_str("start ");
    t.insert_element("[elem]", ElementKind(0), None);
    t.insert_str(" end");
    t.set_cursor(6); // right after "start "

    t.kill_to_end_of_line();
    assert_eq!(t.text(), "start ");
    assert!(t.elements().is_empty());
}

// ===== Element newline skipping in BOL/EOL =====
//
// Elements with multi-line buffer text (e.g. paste blocks) should be
// treated as atomic for line navigation. Newlines inside elements are
// NOT line boundaries.

#[test]
fn ctrl_e_skips_newline_inside_element() {
    // "foo <element:line1\nline2> bar"
    // Ctrl-E from start should go to end of the whole line, not stop
    // at the \n inside the element.
    let mut t = TextArea::new();
    t.insert_str("foo ");
    t.insert_element("line1\nline2", ElementKind(1), None);
    t.insert_str(" bar");
    // buffer = "foo line1\nline2 bar" (19 bytes), element at 4..15

    t.set_cursor(0);
    t.move_cursor_to_end_of_line(false);
    assert_eq!(t.cursor(), t.text().len()); // should reach end of "foo ... bar"
}

#[test]
fn ctrl_a_skips_newline_inside_element() {
    let mut t = TextArea::new();
    t.insert_str("foo ");
    t.insert_element("line1\nline2", ElementKind(1), None);
    t.insert_str(" bar");
    // buffer = "foo line1\nline2 bar" (19 bytes)

    // Set cursor to end
    t.set_cursor(t.text().len());
    t.move_cursor_to_beginning_of_line(false);
    assert_eq!(t.cursor(), 0); // should reach beginning, not stop inside element
}

#[test]
fn ctrl_e_from_element_boundary_skips_to_real_eol() {
    let mut t = TextArea::new();
    t.insert_str("foo ");
    t.insert_element("a\nb\nc", ElementKind(1), None);
    t.insert_str(" bar");
    // element at 4..9

    // Place cursor at element start boundary
    t.set_cursor(4);
    t.move_cursor_to_end_of_line(false);
    assert_eq!(t.cursor(), t.text().len());
}

#[test]
fn ctrl_a_from_after_element_skips_to_real_bol() {
    let mut t = TextArea::new();
    t.insert_str("foo ");
    t.insert_element("a\nb\nc", ElementKind(1), None);
    t.insert_str(" bar");
    // element at 4..9, " bar" at 9..13

    // Place cursor on " bar"
    t.set_cursor(10);
    t.move_cursor_to_beginning_of_line(false);
    assert_eq!(t.cursor(), 0);
}

#[test]
fn kill_to_eol_with_multiline_element() {
    let mut t = TextArea::new();
    t.insert_str("foo ");
    t.insert_element("x\ny\nz", ElementKind(1), None);
    t.insert_str(" bar");
    // buffer = "foo x\ny\nz bar", element at 4..9

    t.set_cursor(0);
    t.kill_to_end_of_line();
    // Should kill everything on the line: "foo " + element + " bar"
    assert_eq!(t.text(), "");
}

#[test]
fn kill_to_bol_with_multiline_element() {
    let mut t = TextArea::new();
    t.insert_str("foo ");
    t.insert_element("x\ny\nz", ElementKind(1), None);
    t.insert_str(" bar");

    t.set_cursor(t.text().len()); // end of " bar"
    t.kill_to_beginning_of_line();
    assert_eq!(t.text(), "");
}

#[test]
fn bol_eol_with_real_newline_and_element() {
    // "hello\nfoo <element:a\nb> bar"
    // Two real lines. Element is on the second line.
    let mut t = TextArea::new();
    t.insert_str("hello\nfoo ");
    t.insert_element("a\nb", ElementKind(1), None);
    t.insert_str(" bar");
    // buffer = "hello\nfoo a\nb bar"
    // Real newline at 5. Element at 10..13. Element's \n at 11 should be skipped.

    // From start of second line (pos 6), Ctrl-E should reach end
    t.set_cursor(6);
    t.move_cursor_to_end_of_line(false);
    assert_eq!(t.cursor(), t.text().len());

    // From end, Ctrl-A should go back to pos 6
    t.move_cursor_to_beginning_of_line(false);
    assert_eq!(t.cursor(), 6);
}

#[test]
fn bol_eol_no_element_unchanged() {
    // Verify the fix doesn't break normal (no-element) behavior.
    let mut t = TextArea::new();
    t.insert_str("line1\nline2\nline3");

    t.set_cursor(6); // start of "line2"
    t.move_cursor_to_end_of_line(false);
    assert_eq!(t.cursor(), 11); // end of "line2" (before \n)

    t.move_cursor_to_beginning_of_line(false);
    assert_eq!(t.cursor(), 6);
}

// ===== Element-display-aware wrapping =====

#[test]
fn wrapping_uses_element_display_width() {
    // Scenario: "foo bar " + element("Clean build", display 28 cols) at width 20.
    // Buffer text: "foo bar Clean build" = 19 buffer cols → textwrap says it fits on one line.
    // Display text: "foo bar [📎 Pasted 1 line, 11 chars]" = 8 + 28 = 36 display cols → should wrap.
    let mut t = TextArea::new();
    t.insert_str("foo bar ");
    let display = Line::from("[📎 Pasted 1 line, 11 chars]"); // 28 display cols
    t.insert_element("Clean build", ElementKind(0), Some(display));
    // buffer = "foo bar Clean build" (19 bytes)

    let lines = t.wrapped_lines(20);
    // The element display (28 cols) doesn't fit after "foo bar " (8 cols) on a 20-col line.
    // But it DOES fit on a fresh 20-col line (28 > 20, so it overflows but gets its own line).
    // Expected: line 1 = "foo bar ", line 2 = element.
    assert!(
        lines.len() >= 2,
        "Expected wrapping to produce at least 2 lines, got {} lines. \
         Line ranges: {:?}",
        lines.len(),
        &*lines,
    );
}

#[test]
fn wrapping_element_fits_on_next_line() {
    // Element display (10 cols) doesn't fit after "hello " (6 cols) on 12-col line,
    // but fits on a fresh line.
    let mut t = TextArea::new();
    t.insert_str("hello ");
    let display = Line::from("[Pasted!]"); // 9 display cols
    t.insert_element("xy", ElementKind(0), Some(display));
    t.insert_str(" z");
    // buffer: "hello xy z" (10 bytes)
    // display: "hello [Pasted!] z" = 6 + 9 + 2 = 17 display cols

    let lines = t.wrapped_lines(12);
    // Line 1: "hello " (6 cols, fits)
    // Line 2: "[Pasted!] z" (9 + 2 = 11 cols, fits in 12)
    assert_eq!(
        lines.len(),
        2,
        "Expected 2 wrapped lines, got {}. Ranges: {:?}",
        lines.len(),
        &*lines,
    );
}

#[test]
fn wrapping_element_without_display_uses_buffer_width() {
    // Element without display override: wrapping should use buffer text width (unchanged behavior).
    let mut t = TextArea::new();
    t.insert_str("hello ");
    t.insert_element("xy", ElementKind(0), None);
    t.insert_str(" z");
    // buffer: "hello xy z" (10 bytes), no display override
    // display = buffer = "hello xy z" = 10 cols

    let lines = t.wrapped_lines(12);
    // 10 cols fits on 12-col line → 1 line
    assert_eq!(lines.len(), 1);
}

#[test]
fn wrapping_element_display_renders_on_correct_lines() {
    // End-to-end: wrapping + rendering with display element.
    // "abc " (4) + element("xy", display="[ELEM]" = 6 cols) + " d" (2)
    // At width 8: "abc " (4) + "[ELEM]" (6) = 10 > 8 → wrap before element
    // Line 1: "abc " (4 cols), Line 2: "[ELEM] d" (8 cols)
    let mut t = TextArea::new();
    t.insert_str("abc ");
    let display = Line::from("[ELEM]");
    t.insert_element("xy", ElementKind(0), Some(display));
    t.insert_str(" d");
    // buffer: "abc xy d" (8 bytes)

    // Check wrapping (drop the Ref before rendering)
    {
        let lines = t.wrapped_lines(8);
        assert_eq!(
            lines.len(),
            2,
            "Should wrap into 2 lines, got {:?}",
            &*lines
        );
    }

    // Render and verify
    let area = Rect::new(0, 0, 8, 2);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::WidgetRef::render_ref(&(&t), area, &mut buf);

    // Line 1 (y=0): "abc " padded to 8
    assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "a");
    assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "b");
    assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "c");
    assert_eq!(buf.cell((3, 0)).unwrap().symbol(), " ");

    // Line 2 (y=1): "[ELEM] d"
    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "[");
    assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "E");
    assert_eq!(buf.cell((5, 1)).unwrap().symbol(), "]");
    assert_eq!(buf.cell((6, 1)).unwrap().symbol(), " ");
    assert_eq!(buf.cell((7, 1)).unwrap().symbol(), "d");
}

#[test]
fn wrapping_element_with_newlines_stays_single_line() {
    // When an element's buffer text contains \n, wrapping must NOT split at those
    // newlines. The element's display is a single-line chip; the \n is internal.
    // Scenario: "hello " + element("line1\nline2\nline3", display="[paste]") + " world"
    // Buffer: "hello line1\nline2\nline3 world"  (contains \n inside element)
    // Display: "hello [paste] world" = 6 + 7 + 6 = 19 cols
    // At width 40: should be 1 visual line.
    let mut t = TextArea::new();
    t.insert_str("hello ");
    let display = Line::from("[paste]"); // 7 display cols
    t.insert_element("line1\nline2\nline3", ElementKind(0), Some(display));
    t.insert_str(" world");
    // buffer: "hello line1\nline2\nline3 world"

    let lines = t.wrapped_lines(40);
    assert_eq!(
        lines.len(),
        1,
        "Element with internal \\n should NOT create extra visual lines. \
         Got {} lines: {:?}",
        lines.len(),
        &*lines,
    );
}

#[test]
fn cursor_pos_after_multiline_element() {
    // After inserting text after a multiline element, the cursor should be on the
    // same visual line as the element chip, not bumped down by internal newlines.
    let mut t = TextArea::new();
    t.insert_str("hello ");
    let display = Line::from("[paste]"); // 7 display cols
    t.insert_element("line1\nline2", ElementKind(0), Some(display));
    t.insert_str(" world");

    let area = Rect::new(0, 0, 80, 10);
    let pos = t.cursor_pos(area);
    assert_eq!(
        pos,
        Some((19, 0)), // 6 + 7 + 6 = 19, row 0
        "Cursor should be at col 19, row 0 after multiline element. \
         Got {:?}. Buffer: {:?}, cursor byte: {}",
        pos,
        t.text(),
        t.cursor(),
    );
}

#[test]
fn yank_restores_last_kill() {
    let mut t = ta_with("hello");
    t.set_cursor(0);
    t.kill_to_end_of_line();
    assert_eq!(t.text(), "");
    assert_eq!(t.cursor(), 0);

    t.yank();
    assert_eq!(t.text(), "hello");
    assert_eq!(t.cursor(), 5);

    let mut t = ta_with("hello world");
    t.set_cursor(t.text().len());
    t.delete_backward_word();
    assert_eq!(t.text(), "hello ");
    assert_eq!(t.cursor(), 6);

    t.yank();
    assert_eq!(t.text(), "hello world");
    assert_eq!(t.cursor(), 11);

    let mut t = ta_with("hello");
    t.set_cursor(5);
    t.kill_to_beginning_of_line();
    assert_eq!(t.text(), "");
    assert_eq!(t.cursor(), 0);

    t.yank();
    assert_eq!(t.text(), "hello");
    assert_eq!(t.cursor(), 5);
}

#[test]
fn no_op_kill_preserves_the_kill_buffer() {
    let mut textarea = ta_with("hello");
    textarea.set_cursor(0);
    textarea.kill_to_end_of_line();
    assert_eq!(textarea.kill_buffer, "hello");

    textarea.set_text("world");
    textarea.set_cursor(textarea.text().len());
    textarea.kill_to_end_of_line();
    textarea.yank();
    assert_eq!(textarea.text(), "worldhello");
}

#[test]
fn kill_buffer_survives_set_text() {
    // A cut must outlive the buffer reset that send does via set_text("").
    let mut t = ta_with("hello");
    t.set_cursor(0);
    t.kill_to_end_of_line();
    assert_eq!(t.text(), "");

    t.set_text(""); // send resets the prompt
    assert_eq!(t.text(), "");

    t.yank();
    assert_eq!(t.text(), "hello");
    assert_eq!(t.cursor(), 5);
}

#[test]
fn cursor_left_and_right_handle_graphemes() {
    let mut t = ta_with("a👍b");
    t.set_cursor(t.text().len());

    t.move_cursor_left(); // before 'b'
    let after_first_left = t.cursor();
    t.move_cursor_left(); // before '👍'
    let after_second_left = t.cursor();
    t.move_cursor_left(); // before 'a'
    let after_third_left = t.cursor();

    assert!(after_first_left < t.text().len());
    assert!(after_second_left < after_first_left);
    assert!(after_third_left < after_second_left);

    // Move right back to end safely
    t.move_cursor_right();
    t.move_cursor_right();
    t.move_cursor_right();
    assert_eq!(t.cursor(), t.text().len());
}

#[test]
fn control_b_and_f_move_cursor() {
    let mut t = ta_with("abcd");
    t.set_cursor(1);

    t.input(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert_eq!(t.cursor(), 2);

    t.input(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert_eq!(t.cursor(), 1);
}

#[test]
fn control_b_f_fallback_control_chars_move_cursor() {
    let mut t = ta_with("abcd");
    t.set_cursor(2);

    // Simulate terminals that send C0 control chars without CONTROL modifier.
    // ^B (U+0002) should move left
    t.input(KeyEvent::new(KeyCode::Char('\u{0002}'), KeyModifiers::NONE));
    assert_eq!(t.cursor(), 1);

    // ^F (U+0006) should move right
    t.input(KeyEvent::new(KeyCode::Char('\u{0006}'), KeyModifiers::NONE));
    assert_eq!(t.cursor(), 2);
}

/// Regression (user report): Ctrl+W must rubout to whitespace, not stop
/// at punctuation.
#[test]
fn ctrl_w_unix_word_rubout_deletes_to_whitespace() {
    let mut t = ta_with("git commit -m hello-world");
    t.set_cursor(t.text().len());
    t.input(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), "git commit -m ");
    t.input(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), "git commit ");
}

#[test]
fn unix_word_rubout_whitespace_runs_paths_and_edges() {
    let mut t = ta_with("cat path/to/file.rs   ");
    t.set_cursor(t.text().len());
    t.delete_backward_unix_word();
    assert_eq!(t.text(), "cat ");
    assert_eq!(t.cursor(), 4);

    let mut t = ta_with("foo bar-baz");
    t.set_cursor(7); // foo bar|-baz
    t.delete_backward_unix_word();
    assert_eq!(t.text(), "foo -baz");
    assert_eq!(t.cursor(), 4);

    // Newlines are whitespace: rubout crosses line boundaries.
    let mut t = ta_with("line1\nword  ");
    t.set_cursor(t.text().len());
    t.delete_backward_unix_word();
    assert_eq!(t.text(), "line1\n");

    let mut t = ta_with("");
    t.delete_backward_unix_word();
    assert_eq!(t.text(), "");
    let mut t = ta_with("   ");
    t.set_cursor(3);
    t.delete_backward_unix_word();
    assert_eq!(t.text(), "");
}

/// Readline parity: only C-w is whitespace-delimited; M-DEL/C-Backspace
/// stay chunked.
#[test]
fn alt_backspace_keeps_word_chunk_semantics() {
    let mut t = ta_with("hello-world");
    t.set_cursor(t.text().len());
    t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
    assert_eq!(t.text(), "hello-");

    let mut t = ta_with("hello-world");
    t.set_cursor(t.text().len());
    t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL));
    assert_eq!(t.text(), "hello-");
}

#[test]
fn delete_backward_word_alt_keys() {
    // Test the custom Alt+Ctrl+h binding
    let mut t = ta_with("hello world");
    t.set_cursor(t.text().len()); // cursor at the end
    t.input(KeyEvent::new(
        KeyCode::Char('h'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ));
    assert_eq!(t.text(), "hello ");
    assert_eq!(t.cursor(), 6);

    // Test the standard Alt+Backspace binding
    let mut t = ta_with("hello world");
    t.set_cursor(t.text().len()); // cursor at the end
    t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
    assert_eq!(t.text(), "hello ");
    assert_eq!(t.cursor(), 6);
}

#[test]
fn ctrl_backspace_deletes_backward_word() {
    let mut t = ta_with("hello world");
    t.set_cursor(t.text().len());
    t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL));
    assert_eq!(t.text(), "hello ");
    assert_eq!(t.cursor(), 6);

    // From end of middle word: deletes "bar", leaves surrounding spaces
    let mut t = ta_with("foo bar baz");
    t.set_cursor(7); // after "bar"
    t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL));
    assert_eq!(t.text(), "foo  baz");
    assert_eq!(t.cursor(), 4);
}

#[test]
fn ctrl_delete_deletes_forward_word() {
    // Mirror of ctrl_backspace_deletes_backward_word.
    let mut t = ta_with("hello world");
    t.set_cursor(0);
    t.input(KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL));
    assert_eq!(t.text(), " world");
    assert_eq!(t.cursor(), 0);

    // From start of middle word: deletes "bar", leaves surrounding spaces
    let mut t = ta_with("foo bar baz");
    t.set_cursor(4); // before "bar"
    t.input(KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL));
    assert_eq!(t.text(), "foo  baz");
    assert_eq!(t.cursor(), 4);
}

#[test]
fn delete_backward_word_handles_narrow_no_break_space() {
    let mut t = ta_with("32\u{202F}AM");
    t.set_cursor(t.text().len());
    t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
    pretty_assertions::assert_eq!(t.text(), "32\u{202F}");
    pretty_assertions::assert_eq!(t.cursor(), t.text().len());
}

#[test]
fn delete_forward_word_with_without_alt_modifier() {
    let mut t = ta_with("hello world");
    t.set_cursor(0);
    t.input(KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT));
    assert_eq!(t.text(), " world");
    assert_eq!(t.cursor(), 0);

    let mut t = ta_with("hello");
    t.set_cursor(0);
    t.input(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(t.text(), "ello");
    assert_eq!(t.cursor(), 0);
}

#[test]
fn alt_d_deletes_forward_word() {
    // Alt+D (Meta-d, Emacs) → delete forward word
    let mut t = ta_with("hello world foo");
    t.set_cursor(0);
    t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT));
    assert_eq!(t.text(), " world foo");
    assert_eq!(t.cursor(), 0);

    // Alt+D at a word boundary
    let mut t = ta_with("hello world");
    t.set_cursor(5); // cursor right after "hello"
    t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT));
    assert_eq!(t.text(), "hello");
    assert_eq!(t.cursor(), 5);

    // Super+D (Cmd+D on macOS with Kitty protocol) also works
    let mut t = ta_with("hello world");
    t.set_cursor(0);
    t.input(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::SUPER));
    assert_eq!(t.text(), " world");
    assert_eq!(t.cursor(), 0);
}

#[test]
fn ctrl_p_moves_cursor_up() {
    let mut t = ta_with("first\nsecond\nthird");
    let second_line_start = 6; // after "first\n"
    t.set_cursor(second_line_start + 2); // middle of "second"
    t.input(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    // Should be on first line now
    assert!(t.cursor() < second_line_start);
}

#[test]
fn ctrl_n_moves_cursor_down() {
    let mut t = ta_with("first\nsecond\nthird");
    t.set_cursor(2); // middle of "first"
    t.input(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    let second_line_start = 6;
    // Should be on second line now
    assert!(t.cursor() >= second_line_start);
}

#[test]
fn control_h_backspace() {
    // Test Ctrl+H as backspace
    let mut t = ta_with("12345");
    t.set_cursor(3); // cursor after '3'
    t.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), "1245");
    assert_eq!(t.cursor(), 2);

    // Test Ctrl+H at beginning (should be no-op)
    t.set_cursor(0);
    t.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), "1245");
    assert_eq!(t.cursor(), 0);

    // Test Ctrl+H at end
    t.set_cursor(t.text().len());
    t.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), "124");
    assert_eq!(t.cursor(), 3);
}
#[test]
fn char_bs_backspace() {
    // Test Char('\x08') (BS) as backspace
    let mut t = ta_with("12345");
    t.set_cursor(3); // cursor after '3'
    t.input(KeyEvent::new(KeyCode::Char('\x08'), KeyModifiers::NONE));
    assert_eq!(t.text(), "1245");
    assert_eq!(t.cursor(), 2);
}

#[test]
fn char_del_deletes_backward() {
    // Char('\x7f') (DEL) should delete backward — on Unix terminals,
    // Backspace sends 0x7F in legacy mode (no Kitty protocol).
    let mut t = ta_with("12345");
    t.set_cursor(2); // cursor after '2'
    t.input(KeyEvent::new(KeyCode::Char('\x7f'), KeyModifiers::NONE));
    assert_eq!(t.text(), "1345");
    assert_eq!(t.cursor(), 1);
}

#[test]
fn raw_delete_chars_ignore_stray_modifiers() {
    for raw in ['\u{0008}', '\u{007f}'] {
        for modifiers in [
            KeyModifiers::ALT,
            KeyModifiers::CONTROL,
            KeyModifiers::SUPER,
            KeyModifiers::ALT | KeyModifiers::CONTROL,
        ] {
            let mut t = ta_with("alpha beta");
            t.input(KeyEvent::new(KeyCode::Char(raw), modifiers));
            assert_eq!(
                t.text(),
                "alpha bet",
                "raw {raw:?} with {modifiers:?} must delete one grapheme",
            );
        }
    }
}

#[test]
fn del_char_treated_as_backspace() {
    // When Kitty keyboard protocol gets silently popped, Backspace can
    // arrive as raw DEL (0x7F) instead of KeyCode::Backspace. Ensure it
    // deletes backward instead of inserting an invisible character.
    let mut t = ta_with("hello");
    t.set_cursor(3);
    t.input(KeyEvent::new(KeyCode::Char('\u{007f}'), KeyModifiers::NONE));
    assert_eq!(t.text(), "helo");
    assert_eq!(t.cursor(), 2);

    // At beginning: no-op
    t.set_cursor(0);
    t.input(KeyEvent::new(KeyCode::Char('\u{007f}'), KeyModifiers::NONE));
    assert_eq!(t.text(), "helo");
    assert_eq!(t.cursor(), 0);

    t.set_cursor(t.text().len());
    t.input(KeyEvent::new(KeyCode::Char('\u{007f}'), KeyModifiers::ALT));
    assert_eq!(t.text(), "hel");
    assert_eq!(t.cursor(), 3);
}

#[test]
fn bs_char_treated_as_backspace() {
    // BS (0x08) arriving as Char without CONTROL modifier should also
    // delete backward (Ctrl-H without the modifier flag).
    let mut t = ta_with("abcde");
    t.set_cursor(4);
    t.input(KeyEvent::new(KeyCode::Char('\u{0008}'), KeyModifiers::NONE));
    assert_eq!(t.text(), "abce");
    assert_eq!(t.cursor(), 3);
}

#[test]
fn del_char_with_selection_deletes_selection() {
    // DEL (0x7F) arriving as Char with an active selection should delete
    // the selection cleanly, not insert an invisible control character.
    let mut t = ta_with("hello world");
    t.set_selection(0, 5);
    t.input(KeyEvent::new(KeyCode::Char('\u{007f}'), KeyModifiers::NONE));
    assert_eq!(t.text(), " world");
    assert_eq!(t.cursor(), 0);
    assert!(t.selection_range().is_none());
}

#[test]
fn cursor_vertical_movement_across_lines_and_bounds() {
    let mut t = ta_with("short\nloooooooooong\nmid");
    // Place cursor on second line, column 5
    let second_line_start = 6; // after first '\n'
    t.set_cursor(second_line_start + 5);

    // Move up: target column preserved, clamped by line length
    t.move_cursor_up();
    assert_eq!(t.cursor(), 5); // first line has len 5

    // Move up again goes to start of text
    t.move_cursor_up();
    assert_eq!(t.cursor(), 0);

    // Move down: from start to target col tracked
    t.move_cursor_down();
    // On first move down, we should land on second line, at col 0 (target col remembered as 0)
    let pos_after_down = t.cursor();
    assert!(pos_after_down >= second_line_start);

    // Move down again to third line; clamp to its length
    t.move_cursor_down();
    let third_line_start = t.text().find("mid").unwrap();
    let third_line_end = third_line_start + 3;
    assert!(t.cursor() >= third_line_start && t.cursor() <= third_line_end);

    // Moving down at last line jumps to end
    t.move_cursor_down();
    assert_eq!(t.cursor(), t.text().len());
}

#[test]
fn home_end_and_emacs_style_home_end() {
    let mut t = ta_with("one\ntwo\nthree");
    // Position at middle of second line
    let second_line_start = t.text().find("two").unwrap();
    t.set_cursor(second_line_start + 1);

    t.move_cursor_to_beginning_of_line(false);
    assert_eq!(t.cursor(), second_line_start);

    // Ctrl-A behavior: if at BOL, go to beginning of previous line
    t.move_cursor_to_beginning_of_line(true);
    assert_eq!(t.cursor(), 0); // beginning of first line

    // Move to EOL of first line
    t.move_cursor_to_end_of_line(false);
    assert_eq!(t.cursor(), 3);

    // Ctrl-E: if at EOL, go to end of next line
    t.move_cursor_to_end_of_line(true);
    // end of second line ("two") is right before its '\n'
    let end_second_nl = t.text().find("\nthree").unwrap();
    assert_eq!(t.cursor(), end_second_nl);
}

#[test]
fn home_end_use_logical_line_when_soft_wrapped() {
    // width 4 → "abcd" | "efgh" | "ij"
    let mut t = ta_with("abcdefghij");
    let _ = t.desired_height(4);
    t.set_cursor(6); // mid second visual row

    t.input(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(t.cursor(), 0);
    t.input(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(t.cursor(), t.text().len());

    // Super+Left/Right stay on the visual wrap row.
    t.set_cursor(6);
    t.move_cursor_to_beginning_of_line(false);
    assert_eq!(t.cursor(), 4);
    t.move_cursor_to_end_of_line(false);
    assert_eq!(t.cursor(), 7);

    // Multiline: Home/End stay on this logical line, not wrap-row or buffer.
    let mut multi = ta_with("abcdefghij\nxyz");
    let _ = multi.desired_height(4);
    multi.set_cursor(6);
    multi.input(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(multi.cursor(), 0);
    multi.set_cursor(6);
    multi.input(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(multi.cursor(), "abcdefghij".len());
    multi.set_cursor("abcdefghij\nxy".len());
    multi.input(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(multi.cursor(), "abcdefghij\n".len());
    multi.input(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(multi.cursor(), multi.text().len());

    // Ctrl+A/E stay logical (and chain across lines).
    t.set_cursor(6);
    t.move_cursor_to_beginning_of_line(true);
    assert_eq!(t.cursor(), 0);
    t.set_cursor(6);
    t.move_cursor_to_end_of_line(true);
    assert_eq!(t.cursor(), t.text().len());
}

#[test]
fn end_of_line_or_down_at_end_of_text() {
    let mut t = ta_with("one\ntwo");
    // Place cursor at absolute end of the text
    t.set_cursor(t.text().len());
    // Should remain at end without panicking
    t.move_cursor_to_end_of_line(true);
    assert_eq!(t.cursor(), t.text().len());

    // Also verify behavior when at EOL of a non-final line:
    let eol_first_line = 3; // index of '\n' in "one\ntwo"
    t.set_cursor(eol_first_line);
    t.move_cursor_to_end_of_line(true);
    assert_eq!(t.cursor(), t.text().len()); // moves to end of next (last) line
}

#[test]
fn word_navigation_helpers() {
    let t = ta_with("  alpha  beta   gamma");
    let mut t = t; // make mutable for set_cursor
    // Put cursor after "alpha"
    let after_alpha = t.text().find("alpha").unwrap() + "alpha".len();
    t.set_cursor(after_alpha);
    assert_eq!(t.beginning_of_previous_word(), 2); // skip initial spaces

    // Put cursor at start of beta
    let beta_start = t.text().find("beta").unwrap();
    t.set_cursor(beta_start);
    assert_eq!(t.end_of_next_word(), beta_start + "beta".len());

    // If at end, end_of_next_word returns len
    t.set_cursor(t.text().len());
    assert_eq!(t.end_of_next_word(), t.text().len());
}

#[test]
fn word_navigation_splits_on_hyphen() {
    let mut t = ta_with("hello-world");
    let hyphen = t.text().find('-').unwrap();
    let after_hyphen = hyphen + 1;

    t.set_cursor(t.text().len());
    assert_eq!(t.beginning_of_previous_word(), after_hyphen);

    t.set_cursor(after_hyphen);
    assert_eq!(t.beginning_of_previous_word(), hyphen);

    t.set_cursor(hyphen);
    assert_eq!(t.beginning_of_previous_word(), 0);

    t.set_cursor(0);
    assert_eq!(t.end_of_next_word(), hyphen);

    t.set_cursor(hyphen);
    assert_eq!(t.end_of_next_word(), after_hyphen);

    t.set_cursor(after_hyphen);
    assert_eq!(t.end_of_next_word(), t.text().len());
}

#[test]
fn alt_arrow_navigation_splits_on_hyphen() {
    let mut t = ta_with("hello-world");
    let hyphen = t.text().find('-').unwrap();
    let after_hyphen = hyphen + 1;
    let end = t.text().len();

    t.set_cursor(0);
    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(t.cursor(), hyphen);

    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(t.cursor(), after_hyphen);

    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(t.cursor(), end);

    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(t.cursor(), after_hyphen);

    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(t.cursor(), hyphen);

    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(t.cursor(), 0);
}

#[test]
fn cursor_at_wrap_boundary_shows_on_next_line() {
    // When typing fills an entire line, the cursor sits at the exact wrap
    // boundary.  It should be reported on the *next* visual line at col 0,
    // not at col == width (which is the invisible right border).

    // Case 1: text exactly fills one line — cursor at text.len()
    let mut t = ta_with("abcde");
    let area = Rect::new(0, 0, 5, 3); // width 5
    t.set_cursor(5); // cursor right after 'e'

    let (x, y) = t.cursor_pos(area).unwrap();
    assert_eq!(x, 0, "cursor x should be 0 (start of virtual next line)");
    assert_eq!(y, 1, "cursor y should be 1 (next line)");

    // Case 2: text wraps — cursor at the boundary between two wrapped lines
    let mut t = ta_with("abcdefgh");
    let area = Rect::new(0, 0, 5, 3); // width 5, wraps after 'e'
    // cursor at position 5 = start of "fgh" = should be col 0, row 1
    t.set_cursor(5);

    let (x, y) = t.cursor_pos(area).unwrap();
    assert_eq!(x, 0, "cursor at wrap point should be col 0 of next line");
    assert_eq!(y, 1, "cursor at wrap point should be on second visual line");
}

#[test]
fn wrapping_and_cursor_positions() {
    let mut t = ta_with("hello world here");
    let area = Rect::new(0, 0, 6, 10); // width 6 -> wraps words
    // desired height counts wrapped lines
    assert!(t.desired_height(area.width) >= 3);

    // Place cursor in "world"
    let world_start = t.text().find("world").unwrap();
    t.set_cursor(world_start + 3);
    let (_x, y) = t.cursor_pos(area).unwrap();
    assert_eq!(y, 1); // world should be on second wrapped line

    // With state and small height, cursor is mapped onto visible row
    let mut state = TextAreaState::default();
    let small_area = Rect::new(0, 0, 6, 1);
    // First call: cursor not visible -> effective scroll ensures it is
    let (_x, y) = t.cursor_pos_with_state(small_area, state).unwrap();
    assert_eq!(y, 0);

    // Render with state to update actual scroll value
    let mut buf = Buffer::empty(small_area);
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), small_area, &mut buf, &mut state);
    // After render, state.scroll should be adjusted so cursor row fits
    let effective_lines = t.desired_height(small_area.width);
    assert!(state.scroll < effective_lines);
}

#[test]
fn cursor_pos_with_state_basic_and_scroll_behaviors() {
    // Case 1: No wrapping needed, height fits — scroll ignored, y maps directly.
    let mut t = ta_with("hello world");
    t.set_cursor(3);
    let area = Rect::new(2, 5, 20, 3);
    // Even if an absurd scroll is provided, when content fits the area the
    // effective scroll is 0 and the cursor position matches cursor_pos.
    let bad_state = TextAreaState { scroll: 999 };
    let (x1, y1) = t.cursor_pos(area).unwrap();
    let (x2, y2) = t.cursor_pos_with_state(area, bad_state).unwrap();
    assert_eq!((x2, y2), (x1, y1));

    // Case 2: Cursor below the current window — y should be clamped to the
    // bottom row (area.height - 1) after adjusting effective scroll.
    let mut t = ta_with("one two three four five six");
    // Force wrapping to many visual lines.
    let wrap_width = 4;
    let _ = t.desired_height(wrap_width);
    // Put cursor somewhere near the end so it's definitely below the first window.
    t.set_cursor(t.text().len().saturating_sub(2));
    let small_area = Rect::new(0, 0, wrap_width, 2);
    let state = TextAreaState { scroll: 0 };
    let (_x, y) = t.cursor_pos_with_state(small_area, state).unwrap();
    assert_eq!(y, small_area.y + small_area.height - 1);

    // Case 3: Cursor above the current window — y should be top row (0)
    // when the provided scroll is too large.
    let mut t = ta_with("alpha beta gamma delta epsilon zeta");
    let wrap_width = 5;
    let lines = t.desired_height(wrap_width);
    // Place cursor near start so an excessive scroll moves it to top row.
    t.set_cursor(1);
    let area = Rect::new(0, 0, wrap_width, 3);
    let state = TextAreaState {
        scroll: lines.saturating_mul(2),
    };
    let (_x, y) = t.cursor_pos_with_state(area, state).unwrap();
    assert_eq!(y, area.y);
}

#[test]
fn screen_spans_of_range_single_row() {
    let t = ta_with("xy /model tail");
    let area = Rect::new(2, 1, 40, 3);
    let state = TextAreaState::default();

    // "/model" = bytes 3..9, all on the first visual row.
    let spans = t.screen_spans_of_range(3..9, area, state);
    assert_eq!(spans, vec![Rect::new(5, 1, 6, 1)]);

    // Degenerate ranges yield no spans.
    assert!(t.screen_spans_of_range(4..4, area, state).is_empty());
    assert!(t.screen_spans_of_range(4..999, area, state).is_empty());
}

#[test]
fn screen_spans_of_range_rejects_non_char_boundaries() {
    // 'é' spans bytes 1..3; an endpoint inside it must yield no spans
    // (tolerated like the other invalid-range shapes, never a panic).
    let t = ta_with("héllo");
    let area = Rect::new(0, 0, 10, 2);
    let state = TextAreaState::default();

    assert!(t.screen_spans_of_range(2..5, area, state).is_empty());
    assert!(t.screen_spans_of_range(0..2, area, state).is_empty());
}

#[test]
fn screen_spans_of_range_covers_wrapped_rows() {
    // A token wider than the wrap width must split at the line end and
    // report one span per visual row it lands on.
    let mut t = ta_with("aa /pr-workflow");
    t.show_scrollbar = false;
    let area = Rect::new(0, 0, 8, 4);
    let state = TextAreaState::default();

    // "/pr-workflow" = bytes 3..15, display width 12 > wrap width 8.
    let spans = t.screen_spans_of_range(3..15, area, state);
    assert!(
        spans.len() >= 2,
        "token must cover multiple rows: {spans:?}"
    );
    assert!(spans.iter().all(|r| r.height == 1));
    for pair in spans.windows(2) {
        assert_eq!(pair[1].y, pair[0].y + 1, "rows must be consecutive");
    }
    for r in &spans[1..] {
        assert_eq!(r.x, area.x, "continuation rows start at the left edge");
        assert!(r.right() <= area.x + area.width);
    }
    // Tokens contain no whitespace, so no cell is lost at wrap boundaries:
    // the summed span widths equal the token's display width.
    let total: u16 = spans.iter().map(|r| r.width).sum();
    assert_eq!(total, 12);
}

#[test]
fn screen_spans_of_range_skips_offscreen_rows() {
    // Cursor at the end scrolls the viewport to the tail: the token's
    // first row is above the viewport, but its visible tail must still
    // be reported (screen_position_of on the start would return None).
    let mut t = ta_with("/pr-workflow abc");
    t.show_scrollbar = false;
    let area = Rect::new(0, 0, 8, 2);
    let state = TextAreaState::default();

    let spans = t.screen_spans_of_range(0..12, area, state);
    assert!(!spans.is_empty(), "visible token tail must be reported");
    for r in &spans {
        assert!((area.y..area.y + area.height).contains(&r.y));
        assert!(r.width > 0 && r.right() <= area.x + area.width);
    }
    let total: u16 = spans.iter().map(|r| r.width).sum();
    assert!(
        total < 12,
        "off-screen head must not be reported: {spans:?}"
    );
}

#[test]
fn screen_spans_of_range_uses_display_width() {
    // 2-cell CJK chars: 日本語 (9 bytes, display width 6) at wrap width 4
    // renders as 日本 / 語.
    let mut t = ta_with("日本語");
    t.show_scrollbar = false;
    let area = Rect::new(1, 0, 4, 3);
    let state = TextAreaState::default();

    let spans = t.screen_spans_of_range(0..9, area, state);
    assert_eq!(spans, vec![Rect::new(1, 0, 4, 1), Rect::new(1, 1, 2, 1)]);
}

#[test]
fn screen_spans_of_range_clamps_to_content_edge() {
    // Overflowing content puts the scrollbar up, so content is only
    // `tw = width - 1` columns. Row 0's byte range keeps its trailing
    // wrap spaces ("ab   " measures 5), but the reported span must stop
    // at the content edge (4), never reaching the scrollbar column.
    let mut t = ta_with("ab   cd ef gh");
    t.set_cursor(0);
    let area = Rect::new(0, 0, 5, 2);
    let state = TextAreaState::default();

    let spans = t.screen_spans_of_range(0..5, area, state);
    assert_eq!(spans, vec![Rect::new(0, 0, 4, 1)]);
}

#[test]
fn wrapped_navigation_across_visual_lines() {
    let mut t = ta_with("abcdefghij");
    t.show_scrollbar = false;
    // Force wrapping at width 4: lines -> ["abcd", "efgh", "ij"]
    let _ = t.desired_height(4);

    // From the very start, moving down should go to the start of the next wrapped line (index 4)
    t.set_cursor(0);
    t.move_cursor_down();
    assert_eq!(t.cursor(), 4);

    // Cursor at boundary index 4 should be displayed at start of second wrapped line
    t.set_cursor(4);
    let area = Rect::new(0, 0, 4, 10);
    let (x, y) = t.cursor_pos(area).unwrap();
    assert_eq!((x, y), (0, 1));

    // With state and small height, cursor should be visible at row 0, col 0
    let small_area = Rect::new(0, 0, 4, 1);
    let state = TextAreaState::default();
    let (x, y) = t.cursor_pos_with_state(small_area, state).unwrap();
    assert_eq!((x, y), (0, 0));

    // Place cursor in the middle of the second wrapped line ("efgh"), at 'g'
    t.set_cursor(6);
    // Move up should go to same column on previous wrapped line -> index 2 ('c')
    t.move_cursor_up();
    assert_eq!(t.cursor(), 2);

    // Move down should return to same position on the next wrapped line -> back to index 6 ('g')
    t.move_cursor_down();
    assert_eq!(t.cursor(), 6);

    // Move down again should go to third wrapped line. Target col is 2, but the line has len 2 -> clamp to end
    t.move_cursor_down();
    assert_eq!(t.cursor(), t.text().len());
}

#[test]
fn cursor_pos_with_state_after_movements() {
    let mut t = ta_with("abcdefghij");
    // Wrap width 4 -> visual lines: abcd | efgh | ij
    let _ = t.desired_height(4);
    let area = Rect::new(0, 0, 4, 2);
    let mut state = TextAreaState::default();
    let mut buf = Buffer::empty(area);

    // Start at beginning
    t.set_cursor(0);
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
    let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
    assert_eq!((x, y), (0, 0));

    // Move down to second visual line; should be at bottom row (row 1) within 2-line viewport
    t.move_cursor_down();
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
    let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
    assert_eq!((x, y), (0, 1));

    // Move down to third visual line; viewport scrolls and keeps cursor on bottom row
    t.move_cursor_down();
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
    let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
    assert_eq!((x, y), (0, 1));

    // Move up to second visual line; with current scroll, it appears on top row
    t.move_cursor_up();
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
    let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
    assert_eq!((x, y), (0, 0));

    // Column preservation across moves: set to col 2 on first line, move down
    t.set_cursor(2);
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
    let (x0, y0) = t.cursor_pos_with_state(area, state).unwrap();
    assert_eq!((x0, y0), (2, 0));
    t.move_cursor_down();
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
    let (x1, y1) = t.cursor_pos_with_state(area, state).unwrap();
    assert_eq!((x1, y1), (2, 1));
}

#[test]
fn wrapped_navigation_with_newlines_and_spaces() {
    // Include spaces and an explicit newline to exercise boundaries
    let mut t = ta_with("word1  word2\nword3");
    // Width 6 will wrap "word1  " and then "word2" before the newline
    let _ = t.desired_height(6);

    // Put cursor on the second wrapped line before the newline, at column 1 of "word2"
    let start_word2 = t.text().find("word2").unwrap();
    t.set_cursor(start_word2 + 1);

    // Up should go to first wrapped line, column 1 -> index 1
    t.move_cursor_up();
    assert_eq!(t.cursor(), 1);

    // Down should return to the same visual column on "word2"
    t.move_cursor_down();
    assert_eq!(t.cursor(), start_word2 + 1);

    // Down again should cross the logical newline to the next visual line ("word3"), clamped to its length if needed
    t.move_cursor_down();
    let start_word3 = t.text().find("word3").unwrap();
    assert!(t.cursor() >= start_word3 && t.cursor() <= start_word3 + "word3".len());
}

#[test]
fn wrapped_navigation_with_wide_graphemes() {
    // Four thumbs up, each of display width 2, with width 3 to force wrapping inside grapheme boundaries
    let mut t = ta_with("👍👍👍👍");
    let _ = t.desired_height(3);

    // Put cursor after the second emoji (which should be on first wrapped line)
    t.set_cursor("👍👍".len());

    // Move down should go to the start of the next wrapped line (same column preserved but clamped)
    t.move_cursor_down();
    // We expect to land somewhere within the third emoji or at the start of it
    let pos_after_down = t.cursor();
    assert!(pos_after_down >= "👍👍".len());

    // Moving up should take us back to the original position
    t.move_cursor_up();
    assert_eq!(t.cursor(), "👍👍".len());
}

#[test]
fn wrapped_navigation_with_zwj_graphemes() {
    let grapheme = "👩\u{200D}💻";
    let mut t = ta_with(&format!("{grapheme}{grapheme}{grapheme}"));
    let _ = t.desired_height(4);

    t.set_cursor(grapheme.len() * 2);

    t.move_cursor_down();
    let pos_after_down = t.cursor();
    assert!(pos_after_down >= grapheme.len() * 2);

    t.move_cursor_up();
    assert_eq!(t.cursor(), grapheme.len() * 2);
}

#[test]
fn element_aware_wrap_ranges_preserve_zwj_graphemes() {
    let grapheme = "👩\u{200D}💻";
    let mut t = TextArea::new();
    t.insert_str(&format!("{grapheme}{grapheme}"));
    t.insert_element("raw", ElementKind(0), Some(Line::from("[P]")));

    let ranges = {
        let lines = t.wrapped_lines(2);
        lines.iter().cloned().collect::<Vec<_>>()
    };

    assert_eq!(ranges.len(), 3);
    assert_eq!(&t.text()[ranges[0].clone()], grapheme);
    assert_eq!(&t.text()[ranges[1].clone()], grapheme);
}

#[test]
fn fuzz_textarea_randomized() {
    // Deterministic seed for reproducibility
    // Seed the RNG based on the current day in Pacific Time (PST/PDT). This
    // keeps the fuzz test deterministic within a day while still varying
    // day-to-day to improve coverage.
    let pst_today_seed: u64 = (chrono::Utc::now() - chrono::Duration::hours(8))
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp() as u64;
    let mut rng = rand::rngs::StdRng::seed_from_u64(pst_today_seed);

    for _case in 0..500 {
        let mut ta = TextArea::new();
        let mut state = TextAreaState::default();
        // Track element payloads we insert. Payloads use characters '[' and ']' which
        // are not produced by rand_grapheme(), avoiding accidental collisions.
        let mut elem_texts: Vec<String> = Vec::new();
        let mut next_elem_id: usize = 0;
        // Start with a random base string
        let base_len = rng.random_range(0..30);
        let mut base = String::new();
        for _ in 0..base_len {
            base.push_str(&rand_grapheme(&mut rng));
        }
        ta.set_text(&base);
        // Choose a valid char boundary for initial cursor
        let mut boundaries: Vec<usize> = vec![0];
        boundaries.extend(ta.text().char_indices().map(|(i, _)| i).skip(1));
        boundaries.push(ta.text().len());
        let init = boundaries[rng.random_range(0..boundaries.len())];
        ta.set_cursor(init);

        let mut width: u16 = rng.random_range(1..=12);
        let mut height: u16 = rng.random_range(1..=4);

        for _step in 0..60 {
            // Mostly stable width/height, occasionally change
            if rng.random_bool(0.1) {
                width = rng.random_range(1..=12);
            }
            if rng.random_bool(0.1) {
                height = rng.random_range(1..=4);
            }

            // Pick an operation
            match rng.random_range(0..18) {
                0 => {
                    // insert small random string at cursor
                    let len = rng.random_range(0..6);
                    let mut s = String::new();
                    for _ in 0..len {
                        s.push_str(&rand_grapheme(&mut rng));
                    }
                    ta.insert_str(&s);
                }
                1 => {
                    // Include mid-grapheme char boundaries so normalization stays exercised.
                    let mut b: Vec<usize> = vec![0];
                    b.extend(ta.text().char_indices().map(|(i, _)| i).skip(1));
                    b.push(ta.text().len());
                    let i1 = rng.random_range(0..b.len());
                    let i2 = rng.random_range(0..b.len());
                    let (start, end) = if b[i1] <= b[i2] {
                        (b[i1], b[i2])
                    } else {
                        (b[i2], b[i1])
                    };
                    let insert_len = rng.random_range(0..=4);
                    let mut s = String::new();
                    for _ in 0..insert_len {
                        s.push_str(&rand_grapheme(&mut rng));
                    }
                    let before = ta.text().len();
                    let atomic_ranges = ta.element_ranges();
                    let plan = ta
                        .text
                        .plan_replace_byte_range(start..end, &s, &atomic_ranges);
                    let normalized_len = plan.replaced_byte_range().len();
                    ta.replace_range(start..end, &s);
                    let after = ta.text().len();
                    assert_eq!(
                        after as isize,
                        before as isize + (s.len() as isize) - (normalized_len as isize)
                    );
                }
                2 => ta.delete_backward(rng.random_range(0..=3)),
                3 => ta.delete_forward(rng.random_range(0..=3)),
                4 => ta.delete_backward_word(),
                5 => ta.kill_to_beginning_of_line(),
                6 => ta.kill_to_end_of_line(),
                7 => ta.move_cursor_left(),
                8 => ta.move_cursor_right(),
                9 => ta.move_cursor_up(),
                10 => ta.move_cursor_down(),
                11 => ta.move_cursor_to_beginning_of_line(true),
                12 => ta.move_cursor_to_end_of_line(true),
                13 => {
                    // Insert an element with a unique sentinel payload
                    let payload =
                        format!("[[EL#{}:{}]]", next_elem_id, rng.random_range(1000..9999));
                    next_elem_id += 1;
                    ta.insert_element(&payload, ElementKind(0), None);
                    elem_texts.push(payload);
                }
                14 => {
                    // Try inserting inside an existing element (should clamp to boundary)
                    if let Some(payload) = elem_texts.choose(&mut rng).cloned()
                        && let Some(start) = ta.text().find(&payload)
                    {
                        let end = start + payload.len();
                        if end - start > 2 {
                            let pos = rng.random_range(start + 1..end - 1);
                            let ins = rand_grapheme(&mut rng);
                            ta.insert_str_at(pos, &ins);
                        }
                    }
                }
                15 => {
                    // Replace a range that intersects an element -> whole element should be replaced
                    if let Some(payload) = elem_texts.choose(&mut rng).cloned()
                        && let Some(start) = ta.text().find(&payload)
                    {
                        let end = start + payload.len();
                        // Create an intersecting range [start-δ, end-δ2)
                        let mut s = start.saturating_sub(rng.random_range(0..=2));
                        let mut e = (end + rng.random_range(0..=2)).min(ta.text().len());
                        // Align to char boundaries to satisfy String::replace_range contract
                        let txt = ta.text();
                        while s > 0 && !txt.is_char_boundary(s) {
                            s -= 1;
                        }
                        while e < txt.len() && !txt.is_char_boundary(e) {
                            e += 1;
                        }
                        if s < e {
                            // Small replacement text
                            let mut srep = String::new();
                            for _ in 0..rng.random_range(0..=2) {
                                srep.push_str(&rand_grapheme(&mut rng));
                            }
                            ta.replace_range(s..e, &srep);
                        }
                    }
                }
                16 => {
                    // Try setting the cursor to a position inside an element; it should clamp out
                    if let Some(payload) = elem_texts.choose(&mut rng).cloned()
                        && let Some(start) = ta.text().find(&payload)
                    {
                        let end = start + payload.len();
                        if end - start > 2 {
                            let pos = rng.random_range(start + 1..end - 1);
                            ta.set_cursor(pos);
                        }
                    }
                }
                _ => {
                    // Jump to word boundaries
                    if rng.random_bool(0.5) {
                        let p = ta.beginning_of_previous_word();
                        ta.set_cursor(p);
                    } else {
                        let p = ta.end_of_next_word();
                        ta.set_cursor(p);
                    }
                }
            }

            // Sanity invariants
            assert!(ta.cursor() <= ta.text().len());

            // Element invariants
            for payload in &elem_texts {
                if let Some(start) = ta.text().find(payload) {
                    let end = start + payload.len();
                    // 1) Text inside elements matches the initially set payload
                    assert_eq!(&ta.text()[start..end], payload);
                    // 2) Cursor is never strictly inside an element
                    let c = ta.cursor();
                    assert!(
                        c <= start || c >= end,
                        "cursor inside element: {start}..{end} at {c}"
                    );
                }
            }

            // Render and compute cursor positions; ensure they are in-bounds and do not panic
            let area = Rect::new(0, 0, width, height);
            // Stateless render into an area tall enough for all wrapped lines
            let total_lines = ta.desired_height(width);
            let full_area = Rect::new(0, 0, width, total_lines.max(1));
            let mut buf = Buffer::empty(full_area);
            ratatui::widgets::WidgetRef::render_ref(&(&ta), full_area, &mut buf);

            // cursor_pos: x must be within width when present
            let _ = ta.cursor_pos(area);

            // cursor_pos_with_state: always within viewport rows
            let (_x, _y) = ta
                .cursor_pos_with_state(area, state)
                .unwrap_or((area.x, area.y));

            // Stateful render should not panic, and updates scroll
            let mut sbuf = Buffer::empty(area);
            ratatui::widgets::StatefulWidgetRef::render_ref(&(&ta), area, &mut sbuf, &mut state);

            // After wrapping, desired height equals the number of lines we would render without scroll
            let total_lines = total_lines as usize;
            // state.scroll must not exceed total_lines when content fits within area height
            if (height as usize) >= total_lines {
                assert_eq!(state.scroll, 0);
            }
        }
    }
}

// ── Mouse M1: Screen→Buffer mapping tests ──

#[test]
fn buffer_pos_at_screen_plain_text_start() {
    let t = ta_with("hello");
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    // Click at column 0 → pos 0
    assert_eq!(t.buffer_pos_at_screen(0, 0, area, state), Some(0));
}

#[test]
fn buffer_pos_at_screen_plain_text_middle() {
    let t = ta_with("hello");
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    // Click at column 3 → pos 3
    assert_eq!(t.buffer_pos_at_screen(3, 0, area, state), Some(3));
}

#[test]
fn buffer_pos_at_screen_past_end_of_line() {
    let t = ta_with("hello");
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    // Click at column 10, line only has 5 chars → snap to end of text
    assert_eq!(t.buffer_pos_at_screen(10, 0, area, state), Some(5));
}

#[test]
fn buffer_pos_at_screen_below_text() {
    let t = ta_with("hello");
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    // Click on row 3, text only occupies row 0 → end of text
    assert_eq!(t.buffer_pos_at_screen(0, 3, area, state), Some(5));
}

#[test]
fn buffer_pos_at_screen_outside_area() {
    let t = ta_with("hello");
    let area = Rect::new(5, 5, 20, 5);
    let state = TextAreaState::default();
    // Click at (0, 0) which is outside area starting at (5, 5)
    assert_eq!(t.buffer_pos_at_screen(0, 0, area, state), None);
    // Click at (4, 5) — just left of area
    assert_eq!(t.buffer_pos_at_screen(4, 5, area, state), None);
    // Click at (5, 4) — just above area
    assert_eq!(t.buffer_pos_at_screen(5, 4, area, state), None);
}

#[test]
fn buffer_pos_at_screen_with_area_offset() {
    let t = ta_with("hello");
    let area = Rect::new(10, 5, 20, 5);
    let state = TextAreaState::default();
    // Click at screen (13, 5) = column 3 within the area → pos 3
    assert_eq!(t.buffer_pos_at_screen(13, 5, area, state), Some(3));
}

#[test]
fn buffer_pos_at_screen_multiline() {
    let t = ta_with("hello\nworld");
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    // Click on row 0, col 2 → "hello" pos 2
    assert_eq!(t.buffer_pos_at_screen(2, 0, area, state), Some(2));
    // Click on row 1, col 1 → "world" pos 6+1 = 7
    assert_eq!(t.buffer_pos_at_screen(1, 1, area, state), Some(7));
}

#[test]
fn buffer_pos_at_screen_wrapped_text() {
    // "abcdefghij" at width 5 wraps into "abcde" (0..5) and "fghij" (5..10)
    let t = ta_with("abcdefghij");
    let area = Rect::new(0, 0, 5, 5);
    let state = TextAreaState::default();
    // Click on row 0, col 2 → pos 2
    assert_eq!(t.buffer_pos_at_screen(2, 0, area, state), Some(2));
    // Click on row 1, col 0 → pos 5 (start of second wrapped line)
    assert_eq!(t.buffer_pos_at_screen(0, 1, area, state), Some(5));
    // Click on row 1, col 3 → pos 8
    assert_eq!(t.buffer_pos_at_screen(3, 1, area, state), Some(8));
}

#[test]
fn buffer_pos_at_screen_scrolled() {
    // 3 lines, area height 2 → first line scrolled off when cursor is at end
    let mut t = ta_with("aaa\nbbb\nccc");
    t.set_cursor(t.text().len()); // cursor at end → scroll to show last lines
    let area = Rect::new(0, 0, 20, 2);
    let mut state = TextAreaState::default();
    // Render to compute scroll
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
    // state.scroll should be 1 (skipping "aaa")
    assert_eq!(state.scroll, 1);
    // Click row 0 = visual row 0 = wrapped line 1 ("bbb"), col 1 → pos 5
    assert_eq!(t.buffer_pos_at_screen(1, 0, area, state), Some(5));
    // Click row 1 = visual row 1 = wrapped line 2 ("ccc"), col 2 → pos 10
    assert_eq!(t.buffer_pos_at_screen(2, 1, area, state), Some(10));
}

#[test]
fn buffer_pos_at_screen_wide_unicode() {
    // "a🦀b" — 🦀 is 2 columns wide (4 bytes)
    let t = ta_with("a🦀b");
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    // col 0 → 'a' at pos 0
    assert_eq!(t.buffer_pos_at_screen(0, 0, area, state), Some(0));
    // col 1 → first column of 🦀 → pos 1
    assert_eq!(t.buffer_pos_at_screen(1, 0, area, state), Some(1));
    // col 2 → second column of 🦀 → still pos 1 (within the 2-wide grapheme;
    // display_col_to_buffer_pos snaps to start of grapheme since target_col < width_so_far)
    // Actually: width_so_far after 'a' is 1, then 🦀 adds 2 → width_so_far=3 > target_col=2
    // → returns pos 1 (start of 🦀)
    assert_eq!(t.buffer_pos_at_screen(2, 0, area, state), Some(1));
    // col 3 → 'b' at pos 5 (1 + 4 bytes for 🦀)
    assert_eq!(t.buffer_pos_at_screen(3, 0, area, state), Some(5));
}

#[test]
fn buffer_pos_at_screen_element_with_display() {
    // "ab" + element(buffer="raw_text", display="[X]") + "cd"
    // Display: "ab[X]cd" — element is at display cols 2..5
    let mut t = TextArea::new();
    t.insert_str("ab");
    let display = Line::from("[X]");
    t.insert_element("raw_text", ElementKind(0), Some(display));
    t.insert_str("cd");
    // Buffer: "abraw_textcd", element range 2..10
    assert_eq!(t.text(), "abraw_textcd");

    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();

    // col 0 → 'a' at pos 0
    assert_eq!(t.buffer_pos_at_screen(0, 0, area, state), Some(0));
    // col 1 → 'b' at pos 1
    assert_eq!(t.buffer_pos_at_screen(1, 0, area, state), Some(1));
    // col 2 → start of element display "[X]" → snap to element start (pos 2)
    assert_eq!(t.buffer_pos_at_screen(2, 0, area, state), Some(2));
    // col 3 → middle of element display → snap to nearest boundary
    // display width = 3, dist_start = 1, dist_end = 2 → snap to start (pos 2)
    assert_eq!(t.buffer_pos_at_screen(3, 0, area, state), Some(2));
    // col 4 → near end of element display → snap to end (pos 10)
    // dist_start = 2, dist_end = 1 → snap to end
    assert_eq!(t.buffer_pos_at_screen(4, 0, area, state), Some(10));
    // col 5 → 'c' at pos 10
    assert_eq!(t.buffer_pos_at_screen(5, 0, area, state), Some(10));
    // col 6 → 'd' at pos 11
    assert_eq!(t.buffer_pos_at_screen(6, 0, area, state), Some(11));
}

#[test]
fn element_at_screen_hit_and_miss() {
    let mut t = TextArea::new();
    t.insert_str("ab");
    let display = Line::from("[File]");
    let id = t.insert_element("file.rs", ElementKind(1), Some(display));
    t.insert_str("cd");
    // Display: "ab[File]cd"

    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();

    // Click on 'a' (col 0) → no element
    assert!(t.element_at_screen(0, 0, area, state).is_none());
    // Click on 'b' (col 1) → no element
    assert!(t.element_at_screen(1, 0, area, state).is_none());
    // Click on element display (col 2) → element (snaps to start, pos 2 = element start)
    let elem = t.element_at_screen(2, 0, area, state);
    assert!(elem.is_some());
    assert_eq!(elem.unwrap().id, id);
    // Click on element display (col 3) → still the element
    assert_eq!(t.element_at_screen(3, 0, area, state).unwrap().id, id);
    // Click past element (col 8) → 'c' or 'd', no element
    assert!(t.element_at_screen(8, 0, area, state).is_none());
}

#[test]
fn buffer_pos_at_screen_empty_textarea() {
    let t = TextArea::new();
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    // Click on empty textarea → pos 0
    assert_eq!(t.buffer_pos_at_screen(0, 0, area, state), Some(0));
    // Click at col 5 → still pos 0 (end of empty text)
    assert_eq!(t.buffer_pos_at_screen(5, 0, area, state), Some(0));
}

// ── Mouse M2: Selection state + rendering tests ──

#[test]
fn selection_range_normalizes_anchor_head() {
    let mut t = ta_with("hello world");
    // anchor > head → range should be normalized to start..end
    t.set_selection(8, 3);
    let range = t.selection_range().unwrap();
    assert_eq!(range, 3..8);
}

#[test]
fn selection_range_anchor_equals_head_is_none() {
    let mut t = ta_with("hello");
    t.set_selection(3, 3);
    assert!(t.selection_range().is_none());
}

#[test]
fn selected_text_returns_buffer_substring() {
    let mut t = ta_with("hello world");
    t.set_selection(6, 11);
    assert_eq!(t.selected_text().unwrap(), "world");
}

#[test]
fn selection_expands_to_element_boundaries() {
    let mut t = TextArea::new();
    t.insert_str("ab");
    t.insert_element("element_text", ElementKind(0), None);
    t.insert_str("cd");
    // Buffer: "abelement_textcd", element range 2..14
    assert_eq!(t.text(), "abelement_textcd");

    // Select only part of the element (bytes 5..10) → should expand to 2..14
    t.set_selection(5, 10);
    let range = t.selection_range().unwrap();
    assert_eq!(range.start, 2); // expanded to element start
    assert_eq!(range.end, 14); // expanded to element end
    assert_eq!(t.selected_text().unwrap(), "element_text");
}

#[test]
fn clear_selection_clears() {
    let mut t = ta_with("hello");
    t.set_selection(0, 3);
    assert!(t.selection_range().is_some());
    t.clear_selection();
    assert!(t.selection_range().is_none());
}

#[test]
fn take_clipboard_returns_and_clears() {
    let mut t = TextArea::new();
    t.set_clipboard_text("copied text".to_string());
    let text = t.take_clipboard();
    assert_eq!(text, Some("copied text".to_string()));
    // Second take returns None
    assert_eq!(t.take_clipboard(), None);
}

#[test]
fn no_selection_returns_none() {
    let t = ta_with("hello");
    assert!(t.selection_range().is_none());
    assert!(t.selected_text().is_none());
}

#[test]
fn selection_rendering_applies_default_selection_style() {
    let mut t = ta_with("hello");
    t.set_selection(1, 4); // select "ell"

    let area = Rect::new(0, 0, 10, 1);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::WidgetRef::render_ref(&(&t), area, &mut buf);

    let default_bg = Color::Rgb(49, 62, 115);
    let default_fg = Color::Rgb(192, 202, 245);
    // Cells 1, 2, 3 should have the default selection bg + fg
    for col in 1..4u16 {
        let cell = &buf[(col, 0)];
        assert_eq!(
            cell.bg, default_bg,
            "cell at col {col} should have default selection bg"
        );
        assert_eq!(
            cell.fg, default_fg,
            "cell at col {col} should have default selection fg"
        );
    }
    // Cell 0 ('h') and cell 4 ('o') should NOT have selection bg
    assert_ne!(buf[(0, 0)].bg, default_bg);
    assert_ne!(buf[(4, 0)].bg, default_bg);
}

// ── Phase 1: Undo/Redo plumbing tests ──

#[test]
fn undo_insert_chars_one_at_a_time() {
    let mut ta = TextArea::new();
    ta.insert_str("a");
    ta.insert_str("b");
    ta.insert_str("c");
    assert_eq!(ta.text(), "abc");
    assert_eq!(ta.cursor(), 3);

    // Phase 2: consecutive single-char inserts are batched into 1 undo step.
    assert!(ta.undo());
    assert_eq!(ta.text(), "");
    assert_eq!(ta.cursor(), 0);

    // Nothing left to undo.
    assert!(!ta.undo());
}

#[test]
fn redo_after_undo_restores() {
    let mut ta = TextArea::new();
    ta.insert_str("hello");
    ta.insert_str(" ");
    ta.insert_str("world");
    assert_eq!(ta.text(), "hello world");

    // With word boundary batching: "hello" / " " / "world" = 3 steps.
    ta.undo(); // undo "world"
    assert_eq!(ta.text(), "hello ");
    ta.undo(); // undo " "
    assert_eq!(ta.text(), "hello");
    ta.undo(); // undo "hello"
    assert_eq!(ta.text(), "");

    // Redo walks forward.
    ta.redo();
    assert_eq!(ta.text(), "hello");
    ta.redo();
    assert_eq!(ta.text(), "hello ");
    ta.redo();
    assert_eq!(ta.text(), "hello world");
    assert_eq!(ta.cursor(), 11);
}

#[test]
fn undo_via_super_modifier() {
    let mut ta = TextArea::new();
    ta.insert_str("hello");
    assert_eq!(ta.text(), "hello");

    // Cmd+Z (SUPER) triggers undo.
    ta.input(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::SUPER));
    assert_eq!(ta.text(), "");
}

#[test]
fn redo_via_super_modifier() {
    let mut ta = TextArea::new();
    ta.insert_str("hello");
    ta.undo();
    assert_eq!(ta.text(), "");

    // Cmd+Shift+Z (SUPER, reported as uppercase Z) triggers redo.
    ta.input(KeyEvent::new(
        KeyCode::Char('Z'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ));
    assert_eq!(ta.text(), "hello");
}

#[test]
fn redo_cleared_by_new_mutation() {
    let mut ta = TextArea::new();
    ta.insert_str("abc");

    ta.undo(); // undo "abc" → ""
    assert_eq!(ta.text(), "");
    assert!(ta.can_redo());

    ta.insert_str("x"); // new mutation clears redo
    assert!(!ta.can_redo());
    assert_eq!(ta.text(), "x");
}

#[test]
fn undo_delete_backward_restores_char() {
    let mut ta = TextArea::new();
    ta.insert_str("hello");
    ta.delete_backward(1); // "hell"
    assert_eq!(ta.text(), "hell");

    ta.undo(); // undo delete → "hello"
    assert_eq!(ta.text(), "hello");
    assert_eq!(ta.cursor(), 5);
}

#[test]
fn undo_redo_preserves_cursor() {
    let mut ta = TextArea::new();
    ta.insert_str("abc");
    // cursor is at 3
    ta.set_cursor(1);
    ta.insert_str("X"); // "aXbc", cursor at 2
    assert_eq!(ta.text(), "aXbc");
    assert_eq!(ta.cursor(), 2);

    ta.undo(); // undo insert "X" → "abc", cursor at 1
    assert_eq!(ta.text(), "abc");
    assert_eq!(ta.cursor(), 1);

    ta.redo(); // redo → "aXbc", cursor at 2
    assert_eq!(ta.text(), "aXbc");
    assert_eq!(ta.cursor(), 2);
}

#[test]
fn can_undo_can_redo_reflect_state() {
    let mut ta = TextArea::new();
    assert!(!ta.can_undo());
    assert!(!ta.can_redo());

    ta.insert_str("a");
    assert!(ta.can_undo());
    assert!(!ta.can_redo());

    ta.undo();
    assert!(!ta.can_undo());
    assert!(ta.can_redo());

    ta.redo();
    assert!(ta.can_undo());
    assert!(!ta.can_redo());
}

#[test]
fn undo_stack_depth_capped() {
    let mut ta = TextArea::new();
    // Override max_depth for testing.
    ta.undo.max_depth = 5;

    // Use set_text (Replace — always discrete) to force separate undo steps.
    for i in 0..10 {
        ta.set_text(&format!("v{i}"));
    }
    assert_eq!(ta.text(), "v9");
    // Stack should be capped at 5.
    assert_eq!(ta.undo.stack.len(), 5);

    // We can undo at most 5 times.
    let mut count = 0;
    while ta.undo() {
        count += 1;
    }
    assert_eq!(count, 5);
    // We've undone 5 set_text calls, landing on the 5th oldest state.
    assert_eq!(ta.text(), "v4");
}

#[test]
fn undo_set_text_restores_previous() {
    let mut ta = TextArea::new();
    ta.insert_str("hello");
    ta.set_text("new");
    assert_eq!(ta.text(), "new");

    ta.undo(); // undo set_text → "hello"
    assert_eq!(ta.text(), "hello");
}

#[test]
fn undo_redo_multiple_round_trips() {
    let mut ta = TextArea::new();
    // Use separate insert kinds so they don't batch together.
    ta.insert_str("hello");
    ta.delete_backward(2); // "hel" — kind changes Insert→Delete, new undo step
    assert_eq!(ta.text(), "hel");

    ta.undo(); // undo delete → "hello"
    assert_eq!(ta.text(), "hello");

    ta.undo(); // undo insert → ""
    assert_eq!(ta.text(), "");

    ta.redo(); // redo insert → "hello"
    assert_eq!(ta.text(), "hello");

    ta.redo(); // redo delete → "hel"
    assert_eq!(ta.text(), "hel");

    // undo one, insert new → redo cleared
    ta.undo(); // undo delete → "hello"
    assert_eq!(ta.text(), "hello");
    ta.insert_str("z"); // "helloz" — new branch
    assert_eq!(ta.text(), "helloz");
    assert!(!ta.can_redo());

    // undo "z" — but wait, "z" extends the "hello" batch (same kind, consecutive cursor)?
    // No: undo reset last_kind=None, so "z" is a fresh group.
    ta.undo();
    assert_eq!(ta.text(), "hello");
}

// ── Phase 2: Batching tests ──

#[test]
fn batch_consecutive_inserts_into_one_undo_step() {
    // Typing "hello" char by char → batched into 1 undo step.
    let mut ta = TextArea::new();
    ta.insert_str("h");
    ta.insert_str("e");
    ta.insert_str("l");
    ta.insert_str("l");
    ta.insert_str("o");
    assert_eq!(ta.text(), "hello");
    assert_eq!(ta.undo.stack.len(), 1); // single checkpoint

    ta.undo();
    assert_eq!(ta.text(), "");
    assert!(!ta.undo());
}

#[test]
fn multi_grapheme_delete_calls_are_single_undo_steps() {
    for forward in [false, true] {
        let mut ta = ta_with("hello");
        if forward {
            ta.set_cursor(0);
            ta.delete_forward(2);
            assert_eq!(ta.text(), "llo");
        } else {
            ta.delete_backward(2);
            assert_eq!(ta.text(), "hel");
        }
        assert!(ta.undo());
        assert_eq!(ta.text(), "hello");
    }
}

#[test]
fn multi_count_deletes_cross_atomic_element_boundaries() {
    let mut backward = TextArea::new();
    backward.insert_str("a");
    backward.insert_element("TOKEN", ElementKind(1), None);
    backward.insert_str("b");
    backward.delete_backward(2);
    assert_eq!(backward.text(), "a");
    assert!(backward.elements().is_empty());
    assert!(backward.undo());
    assert_eq!(backward.text(), "aTOKENb");

    let mut forward = TextArea::new();
    forward.insert_str("a");
    forward.insert_element("TOKEN", ElementKind(1), None);
    forward.insert_str("b");
    forward.set_cursor(0);
    forward.delete_forward(2);
    assert_eq!(forward.text(), "b");
    assert!(forward.elements().is_empty());
    assert!(forward.undo());
    assert_eq!(forward.text(), "aTOKENb");
}

#[test]
fn batch_consecutive_deletes_into_one_undo_step() {
    let mut ta = TextArea::new();
    ta.insert_str("hello");
    // 5 backspaces — all Delete kind, consecutive cursor
    ta.delete_backward(1); // o
    ta.delete_backward(1); // l
    ta.delete_backward(1); // l
    ta.delete_backward(1); // e
    ta.delete_backward(1); // h
    assert_eq!(ta.text(), "");

    // 2 undo steps: 1 for insert batch, 1 for delete batch
    ta.undo(); // undo all deletes
    assert_eq!(ta.text(), "hello");

    ta.undo(); // undo insert
    assert_eq!(ta.text(), "");
    assert!(!ta.undo());
}

#[test]
fn kind_change_breaks_batch() {
    let mut ta = TextArea::new();
    ta.insert_str("hello");
    ta.delete_backward(1); // "hell" — kind changes → new step
    assert_eq!(ta.text(), "hell");

    // 2 undo steps
    ta.undo(); // undo delete
    assert_eq!(ta.text(), "hello");
    ta.undo(); // undo insert
    assert_eq!(ta.text(), "");
}

#[test]
fn cursor_jump_breaks_insert_batch() {
    let mut ta = TextArea::new();
    ta.insert_str("he"); // cursor at 2
    ta.set_cursor(0); // move cursor to 0 (no mutation, just movement)
    ta.insert_str("X"); // cursor was at 0, last_cursor was 2 → jump → new step
    assert_eq!(ta.text(), "Xhe");

    // 2 undo steps
    ta.undo(); // undo "X"
    assert_eq!(ta.text(), "he");
    ta.undo(); // undo "he"
    assert_eq!(ta.text(), "");
}

#[test]
fn kill_always_discrete() {
    let mut ta = TextArea::new();
    ta.insert_str("hello world");
    ta.set_cursor(5);
    ta.kill_to_end_of_line(); // kills " world"
    assert_eq!(ta.text(), "hello");
    ta.kill_to_end_of_line(); // kills nothing (already at EOL with no newline... wait)

    // Second kill at EOL does nothing (text.len() == cursor_pos).
    // So only 1 kill undo step.
    ta.undo(); // undo kill
    assert_eq!(ta.text(), "hello world");
}

#[test]
fn kill_consecutive_each_own_step() {
    // Two kill operations back-to-back should be separate undo steps.
    let mut ta = TextArea::new();
    ta.insert_str("aaa bbb ccc");
    ta.set_cursor(7); // after "aaa bbb"
    ta.kill_to_end_of_line(); // kills " ccc" → "aaa bbb"
    assert_eq!(ta.text(), "aaa bbb");
    ta.set_cursor(3);
    ta.kill_to_end_of_line(); // kills " bbb" → "aaa"
    assert_eq!(ta.text(), "aaa");

    ta.undo(); // undo second kill
    assert_eq!(ta.text(), "aaa bbb");
    ta.undo(); // undo first kill
    assert_eq!(ta.text(), "aaa bbb ccc");
}

#[test]
fn insert_str_multi_char_is_one_step() {
    // A single insert_str("hello world") call is 1 undo step.
    let mut ta = TextArea::new();
    ta.insert_str("hello world");
    assert_eq!(ta.undo.stack.len(), 1);

    ta.undo();
    assert_eq!(ta.text(), "");
}

#[test]
fn set_text_always_discrete() {
    let mut ta = TextArea::new();
    ta.set_text("first");
    ta.set_text("second");
    assert_eq!(ta.text(), "second");

    // Each set_text is its own undo step (Replace is always discrete)
    ta.undo();
    assert_eq!(ta.text(), "first");
    ta.undo();
    assert_eq!(ta.text(), "");
}

#[test]
fn insert_then_undo_then_insert_fresh_batch() {
    // After undo, last_kind is reset, so new inserts start a fresh batch.
    let mut ta = TextArea::new();
    ta.insert_str("ab");
    ta.undo(); // → ""
    ta.insert_str("cd");
    ta.insert_str("ef"); // should batch with "cd"
    assert_eq!(ta.text(), "cdef");

    ta.undo(); // undo "cdef" batch
    assert_eq!(ta.text(), "");
}

#[test]
fn delete_forward_batches() {
    let mut ta = TextArea::new();
    ta.insert_str("abcde");
    ta.set_cursor(0);
    ta.delete_forward(1); // "bcde"
    ta.delete_forward(1); // "cde"
    ta.delete_forward(1); // "de"
    assert_eq!(ta.text(), "de");

    // All delete_forward calls batch into 1 step
    ta.undo(); // undo all deletes
    assert_eq!(ta.text(), "abcde");
}

#[test]
fn word_boundary_breaks_insert_batch() {
    // Typing "foo bar" char by char: ws↔non-ws transitions create checkpoints.
    let mut ta = TextArea::new();
    // "foo" — all non-ws, batches into 1 step
    ta.insert_str("f");
    ta.insert_str("o");
    ta.insert_str("o");
    // " " — whitespace, class change → new step
    ta.insert_str(" ");
    // "bar" — non-ws, class change → new step
    ta.insert_str("b");
    ta.insert_str("a");
    ta.insert_str("r");
    assert_eq!(ta.text(), "foo bar");

    ta.undo(); // undo "bar"
    assert_eq!(ta.text(), "foo ");
    ta.undo(); // undo " "
    assert_eq!(ta.text(), "foo");
    ta.undo(); // undo "foo"
    assert_eq!(ta.text(), "");
    assert!(!ta.undo());
}

#[test]
fn word_boundary_whitespace_runs_batch_together() {
    // Multiple consecutive whitespace chars batch into one step.
    let mut ta = TextArea::new();
    ta.insert_str("a");
    ta.insert_str(" ");
    ta.insert_str(" ");
    ta.insert_str(" ");
    ta.insert_str("b");
    assert_eq!(ta.text(), "a   b");

    ta.undo(); // undo "b"
    assert_eq!(ta.text(), "a   ");
    ta.undo(); // undo "   "
    assert_eq!(ta.text(), "a");
    ta.undo(); // undo "a"
    assert_eq!(ta.text(), "");
}

#[test]
fn word_boundary_newlines_are_whitespace() {
    // Newlines are whitespace — they batch with spaces, break from words.
    let mut ta = TextArea::new();
    ta.insert_str("foo");
    ta.insert_str("\n");
    ta.insert_str("\n");
    ta.insert_str(" ");
    ta.insert_str(" ");
    ta.insert_str("bar");
    assert_eq!(ta.text(), "foo\n\n  bar");

    ta.undo(); // undo "bar"
    assert_eq!(ta.text(), "foo\n\n  ");
    ta.undo(); // undo "\n\n  " (all whitespace batched)
    assert_eq!(ta.text(), "foo");
    ta.undo(); // undo "foo"
    assert_eq!(ta.text(), "");
}

#[test]
fn word_boundary_multi_char_insert_str_is_one_step() {
    // A single insert_str("hello world") call is still 1 undo step,
    // even though it contains a space. Boundary check only applies
    // between separate insert_str calls.
    let mut ta = TextArea::new();
    ta.insert_str("hello world");
    assert_eq!(ta.undo.stack.len(), 1);

    ta.undo();
    assert_eq!(ta.text(), "");
}

#[test]
fn word_boundary_after_undo_starts_fresh() {
    // After undo, last_kind is reset — no stale boundary check.
    let mut ta = TextArea::new();
    ta.insert_str("abc");
    ta.insert_str(" ");
    ta.undo(); // undo " "
    assert_eq!(ta.text(), "abc");
    // Now insert non-ws — should start a fresh batch, no boundary check against stale state.
    ta.insert_str("d");
    ta.insert_str("e");
    assert_eq!(ta.text(), "abcde");

    ta.undo(); // undo "de"
    assert_eq!(ta.text(), "abc");
}

#[test]
fn element_insert_always_discrete() {
    let mut ta = TextArea::new();
    ta.insert_str("hi ");
    ta.insert_element("@file.rs", ElementKind(0), None);
    // Element should be its own undo step, not batched with the insert.
    assert_eq!(ta.text(), "hi @file.rs");

    ta.undo(); // undo element
    assert_eq!(ta.text(), "hi ");
    assert!(ta.elements().is_empty());

    ta.undo(); // undo "hi "
    assert_eq!(ta.text(), "");
}

// ── Phase 3: Element undo/redo tests ──

#[test]
fn undo_insert_element_redo_preserves_element_id() {
    let mut ta = TextArea::new();
    let id = ta.insert_element("@foo", ElementKind(1), None);
    assert_eq!(ta.elements().len(), 1);
    assert_eq!(ta.elements()[0].id, id);
    assert_eq!(ta.cursor(), "@foo".len());

    ta.undo(); // remove element
    assert!(ta.elements().is_empty());
    assert_eq!(ta.text(), "");
    assert_eq!(ta.cursor(), 0);

    ta.redo(); // restore element — same ElementId
    assert_eq!(ta.elements().len(), 1);
    assert_eq!(ta.elements()[0].id, id);
    assert_eq!(ta.text(), "@foo");
    assert_eq!(ta.cursor(), "@foo".len());
}

#[test]
fn undo_redo_zero_length_element_preserves_metadata_and_cursor() {
    let mut ta = TextArea::new();
    let id = ta.insert_element("", ElementKind(9), None);
    assert_eq!(ta.cursor(), 0);
    assert_eq!(ta.elements()[0].range, 0..0);

    assert!(ta.undo());
    assert!(ta.elements().is_empty());
    assert_eq!(ta.cursor(), 0);

    assert!(ta.redo());
    assert_eq!(ta.elements().len(), 1);
    assert_eq!(ta.elements()[0].id, id);
    assert_eq!(ta.elements()[0].range, 0..0);
    assert_eq!(ta.cursor(), 0);
}

#[test]
fn undo_replace_range_with_element_restores_original() {
    let mut ta = TextArea::new();
    ta.insert_str("hello @foo world");
    // Replace "@foo" (6..10) with an element
    let id = ta.replace_range_with_element(6..10, "@bar.rs", ElementKind(2), None);
    assert_eq!(ta.text(), "hello @bar.rs world");
    assert_eq!(ta.elements().len(), 1);
    assert_eq!(ta.elements()[0].id, id);

    ta.undo(); // undo replace → original text, no elements
    assert_eq!(ta.text(), "hello @foo world");
    assert!(ta.elements().is_empty());

    ta.redo(); // redo → element back
    assert_eq!(ta.text(), "hello @bar.rs world");
    assert_eq!(ta.elements().len(), 1);
    assert_eq!(ta.elements()[0].id, id);
}

#[test]
fn undo_element_display_preserved() {
    let mut ta = TextArea::new();
    let display = Line::from(vec![
        ratatui::text::Span::styled("[", Style::default().fg(Color::Green)),
        ratatui::text::Span::raw("file.rs"),
        ratatui::text::Span::styled("]", Style::default().fg(Color::Green)),
    ]);
    let id = ta.insert_element("@file.rs", ElementKind(0), Some(display));
    assert!(ta.elements()[0].display.is_some());

    ta.undo();
    assert!(ta.elements().is_empty());

    ta.redo();
    assert_eq!(ta.elements().len(), 1);
    assert_eq!(ta.elements()[0].id, id);
    // Display should be restored from the snapshot clone
    let restored = ta.elements()[0].display.as_ref().unwrap();
    assert_eq!(restored.spans.len(), 3);
    let text: String = restored.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "[file.rs]");
}

#[test]
fn next_element_id_never_decreases_after_undo() {
    let mut ta = TextArea::new();
    let id1 = ta.insert_element("a", ElementKind(0), None);
    let id2 = ta.insert_element("b", ElementKind(0), None);

    ta.undo(); // undo element "b"
    ta.undo(); // undo element "a"
    assert!(ta.elements().is_empty());

    // New element after undo should get a fresh ID, never reuse id1 or id2.
    let id3 = ta.insert_element("c", ElementKind(0), None);
    assert_ne!(id3, id1);
    assert_ne!(id3, id2);
    // IDs are monotonically increasing
    assert!(id3.0 > id2.0);
}

#[test]
fn backspace_on_element_undo_restores_element() {
    let mut ta = TextArea::new();
    ta.insert_str("before ");
    let id = ta.insert_element("[paste]", ElementKind(0), None);
    assert_eq!(ta.text(), "before [paste]");
    assert_eq!(ta.cursor(), 14);

    // Backspace at element end → deletes entire element atomically
    ta.delete_backward(1);
    assert_eq!(ta.text(), "before ");
    assert!(ta.elements().is_empty());

    // Undo → element restored with same ID
    ta.undo();
    assert_eq!(ta.text(), "before [paste]");
    assert_eq!(ta.elements().len(), 1);
    assert_eq!(ta.elements()[0].id, id);
    assert_eq!(ta.elements()[0].range, 7..14);
}

// ── Phase 4: Undo group tests ──

#[test]
fn undo_group_collapses_multiple_mutations() {
    // Autocomplete scenario: replace trigger + insert trailing space = 1 undo step.
    let mut ta = TextArea::new();
    ta.insert_str("hello @fo");
    assert_eq!(ta.text(), "hello @fo");

    ta.begin_undo_group();
    ta.replace_range_with_element(6..9, "@foo.rs", ElementKind(1), None);
    ta.insert_str(" "); // trailing space after element
    ta.end_undo_group();

    assert_eq!(ta.text(), "hello @foo.rs ");
    assert_eq!(ta.elements().len(), 1);

    // Single undo undoes the entire autocomplete operation.
    ta.undo();
    assert_eq!(ta.text(), "hello @fo");
    assert!(ta.elements().is_empty());
}

#[test]
fn cancel_undo_group_restores_original() {
    // Line-select cancel: enter → N live-updates → cancel = 0 undo entries.
    let mut ta = TextArea::new();
    ta.insert_str("original");
    let stack_before = ta.undo.stack.len();

    ta.begin_undo_group();
    ta.set_text("modified once");
    ta.set_text("modified twice");
    ta.cancel_undo_group();

    // State restored to before the group.
    assert_eq!(ta.text(), "original");
    // No new undo entries created by the group.
    assert_eq!(ta.undo.stack.len(), stack_before);
}

#[test]
fn nested_groups_only_outermost_pushes() {
    let mut ta = TextArea::new();
    ta.insert_str("start");

    ta.begin_undo_group(); // depth 1
    ta.insert_str(" A");
    ta.begin_undo_group(); // depth 2
    ta.insert_str(" B");
    ta.end_undo_group(); // depth 1 (inner end — no push)
    assert_eq!(ta.text(), "start A B");
    ta.insert_str(" C");
    ta.end_undo_group(); // depth 0 (outermost end — push)

    assert_eq!(ta.text(), "start A B C");

    // Single undo undoes everything in the group.
    ta.undo();
    assert_eq!(ta.text(), "start");
}

#[test]
fn group_with_no_mutations_creates_no_entry() {
    let mut ta = TextArea::new();
    ta.insert_str("hello");
    let stack_len = ta.undo.stack.len();

    ta.begin_undo_group();
    // No mutations inside the group.
    ta.end_undo_group();

    // Stack unchanged — no empty undo entry created.
    assert_eq!(ta.undo.stack.len(), stack_len);
}

#[test]
fn redo_cleared_by_end_undo_group() {
    let mut ta = TextArea::new();
    ta.insert_str("hello");
    ta.undo(); // undo → ""
    assert!(ta.can_redo());

    ta.begin_undo_group();
    ta.insert_str("world");
    ta.end_undo_group();

    // Redo from the previous undo should be cleared.
    assert!(!ta.can_redo());
    assert_eq!(ta.text(), "world");
}

#[test]
fn cancel_nested_group_restores_outermost() {
    // Even if deeply nested, cancel restores to the outermost group snapshot.
    let mut ta = TextArea::new();
    ta.insert_str("original");

    ta.begin_undo_group();
    ta.insert_str(" X");
    ta.begin_undo_group();
    ta.insert_str(" Y");
    // Cancel from inner level — should still restore to outermost snapshot.
    ta.cancel_undo_group();

    assert_eq!(ta.text(), "original");
    assert_eq!(ta.undo.group_depth, 0);
}

#[test]
fn mutations_after_group_work_normally() {
    // After a group ends, normal batching resumes.
    let mut ta = TextArea::new();

    ta.begin_undo_group();
    ta.insert_str("grouped");
    ta.end_undo_group();

    // Normal insert after group — should be its own batch.
    ta.insert_str("X");
    ta.insert_str("Y"); // batches with X

    ta.undo(); // undo "XY"
    assert_eq!(ta.text(), "grouped");

    ta.undo(); // undo group
    assert_eq!(ta.text(), "");
}

// ── M3: Click-to-place cursor tests ──

/// Helper to create a MouseEvent for testing.
fn mouse_down(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn mouse_up(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn click_places_cursor_at_correct_position() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Click at column 3 → cursor at byte 3
    let action = ta.handle_mouse(mouse_down(3, 0), area, state);
    assert_eq!(action, MouseAction::CursorPlaced);
    assert_eq!(ta.cursor(), 3);

    // Click at column 0 → cursor at byte 0
    let action = ta.handle_mouse(mouse_down(0, 0), area, state);
    assert_eq!(action, MouseAction::CursorPlaced);
    assert_eq!(ta.cursor(), 0);
}

#[test]
fn click_on_element_returns_clicked_element() {
    let mut ta = TextArea::new();
    ta.insert_str("hi ");
    let id = ta.insert_element("elem", ElementKind(0), None);
    ta.insert_str(" bye");

    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Element occupies cols 3..7, click at col 4
    let action = ta.handle_mouse(mouse_down(4, 0), area, state);
    assert_eq!(action, MouseAction::CursorPlaced);
    let ev = ta.poll_element_event().expect("should emit element click");
    assert_eq!(ev.id, id);
    assert_eq!(ev.kind, TextElementEventKind::Click);
}

#[test]
fn click_past_end_of_line_snaps_to_line_end() {
    let mut ta = ta_with("hi");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Click far past end of "hi" (col 20)
    let action = ta.handle_mouse(mouse_down(20, 0), area, state);
    assert_eq!(action, MouseAction::CursorPlaced);
    assert_eq!(ta.cursor(), 2); // end of "hi"
}

#[test]
fn click_below_text_snaps_to_text_end() {
    let mut ta = ta_with("hello");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Click on row 3 (only 1 row of text)
    let action = ta.handle_mouse(mouse_down(0, 3), area, state);
    assert_eq!(action, MouseAction::CursorPlaced);
    assert_eq!(ta.cursor(), 5); // text.len()
}

#[test]
fn click_clears_existing_selection() {
    let mut ta = ta_with("hello world");
    ta.set_selection(0, 5);
    assert!(ta.selection_range().is_some());

    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    ta.handle_mouse(mouse_down(8, 0), area, state);
    assert!(ta.selection_range().is_none());
}

#[test]
fn click_outside_area_returns_nothing() {
    let mut ta = ta_with("hello");
    let area = Rect::new(5, 5, 20, 3);
    let state = TextAreaState::default();

    // Click outside the area
    let action = ta.handle_mouse(mouse_down(0, 0), area, state);
    assert_eq!(action, MouseAction::Nothing);
}

#[test]
fn mouse_up_clears_down_pos() {
    let mut ta = ta_with("hello");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    ta.handle_mouse(mouse_down(2, 0), area, state);
    assert!(ta.mouse_down_pos.is_some());

    ta.handle_mouse(mouse_up(2, 0), area, state);
    assert!(ta.mouse_down_pos.is_none());
}

#[test]
fn click_on_second_line_multiline_text() {
    let mut ta = ta_with("hello\nworld");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Click on row 1, col 2 → "world" starts at byte 6, so byte 8 = 'r'
    let action = ta.handle_mouse(mouse_down(2, 1), area, state);
    assert_eq!(action, MouseAction::CursorPlaced);
    assert_eq!(ta.cursor(), 8); // "hello\nwo" = 8 bytes → cursor at 'r'
}

// ── M4: Drag selection tests ──

fn mouse_drag(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn drag_selects_text() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Mouse down at col 0, drag to col 5
    ta.handle_mouse(mouse_down(0, 0), area, state);
    let action = ta.handle_mouse(mouse_drag(5, 0), area, state);
    assert_eq!(action, MouseAction::SelectionUpdated);
    assert_eq!(ta.selection_range(), Some(0..5));
    assert_eq!(ta.selected_text(), Some("hello".to_string()));
}

#[test]
fn drag_across_element_expands_to_element_boundaries() {
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    let mut ta = TextArea::new();
    ta.insert_str("ab");
    ta.insert_element("ELEM", ElementKind(0), None);
    ta.insert_str("cd");
    // buffer: "abELEMcd"
    // element range: 2..6
    // display cols: a(0) b(1) E(2) L(3) E(4) M(5) c(6) d(7)

    // Drag from col 1 ("b") to col 7 ("d") — fully crosses the element.
    // Raw selection: anchor=1, head=7. Element at 2..6 is fully inside.
    ta.handle_mouse(mouse_down(1, 0), area, state);
    ta.handle_mouse(mouse_drag(7, 0), area, state);
    let range = ta.selection_range().unwrap();
    assert_eq!(range, 1..7);

    let mut ta = TextArea::new();
    ta.insert_str("ab");
    ta.insert_element("ELEM", ElementKind(0), None);
    ta.insert_str("cd");

    // Now test partial overlap: drag from col 0 to col 3 (into the element).
    // display_col_to_buffer_pos snaps col 3 to element start (2) since dist
    // to start (1) < dist to end (3). Raw selection 0..2 → but element at
    // 2..6 is NOT overlapped, so no expansion.
    ta.handle_mouse(mouse_down(0, 0), area, state);
    ta.handle_mouse(mouse_drag(3, 0), area, state);
    let range = ta.selection_range().unwrap();
    assert_eq!(range, 0..2);

    let mut ta = TextArea::new();
    ta.insert_str("ab");
    ta.insert_element("ELEM", ElementKind(0), None);
    ta.insert_str("cd");

    // Drag from col 0 to col 5 — past element midpoint, so snaps to end (6).
    // Raw selection 0..6 → element fully covered.
    ta.handle_mouse(mouse_down(0, 0), area, state);
    ta.handle_mouse(mouse_drag(5, 0), area, state);
    let range = ta.selection_range().unwrap();
    assert_eq!(range, 0..6);
}

#[test]
fn mouse_up_after_drag_copies_to_clipboard() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    ta.handle_mouse(mouse_down(6, 0), area, state);
    ta.handle_mouse(mouse_drag(11, 0), area, state);
    let action = ta.handle_mouse(mouse_up(11, 0), area, state);
    assert_eq!(action, MouseAction::SelectionFinished);

    // Clipboard should contain "world"
    assert_eq!(ta.take_clipboard(), Some("world".to_string()));
    // take_clipboard clears it
    assert_eq!(ta.take_clipboard(), None);
}

#[test]
fn selection_persists_after_mouseup_by_default() {
    let mut ta = ta_with("hello");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    ta.handle_mouse(mouse_down(0, 0), area, state);
    ta.handle_mouse(mouse_drag(3, 0), area, state);
    ta.handle_mouse(mouse_up(3, 0), area, state);

    // Default: keep_selection_after_mouseup = true
    assert!(ta.selection_range().is_some());
    assert_eq!(ta.selected_text(), Some("hel".to_string()));
}

#[test]
fn selection_clears_after_mouseup_when_configured() {
    let mut ta = ta_with("hello");
    ta.keep_selection_after_mouseup = false;
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    ta.handle_mouse(mouse_down(0, 0), area, state);
    ta.handle_mouse(mouse_drag(3, 0), area, state);
    ta.handle_mouse(mouse_up(3, 0), area, state);

    // Clipboard was still set
    assert_eq!(ta.take_clipboard(), Some("hel".to_string()));
    // But selection is cleared
    assert!(ta.selection_range().is_none());
}

#[test]
fn backspace_deletes_selection_only() {
    let mut ta = ta_with("hello world");
    ta.set_selection(0, 5);
    assert_eq!(ta.selected_text(), Some("hello".to_string()));

    // Backspace should delete "hello", not an extra char.
    ta.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(ta.text(), " world");
    assert_eq!(ta.cursor(), 0);
    assert!(ta.selection_range().is_none());
}

#[test]
fn typing_replaces_selection() {
    let mut ta = ta_with("hello world");
    ta.set_selection(0, 5);

    // Typing 'X' should replace "hello" with "X".
    ta.input(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE));
    assert_eq!(ta.text(), "X world");
    assert_eq!(ta.cursor(), 1);
    assert!(ta.selection_range().is_none());
}

#[test]
fn arrow_clears_selection() {
    let mut ta = ta_with("hello world");
    ta.set_selection(0, 5);
    assert!(ta.selection_range().is_some());

    ta.input(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(ta.selection_range().is_none());
}

#[test]
fn undo_after_delete_selection_restores() {
    let mut ta = ta_with("hello world");
    ta.set_cursor(ta.text().len());
    ta.set_selection(0, 5);

    ta.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(ta.text(), " world");
    assert_eq!(ta.cursor(), 0);
    assert_eq!(ta.undo.last_cursor, 0);

    ta.undo();
    assert_eq!(ta.text(), "hello world");
}

#[test]
fn undo_after_type_replace_selection_restores() {
    let mut ta = ta_with("hello world");
    ta.set_selection(0, 5);

    ta.input(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE));
    assert_eq!(ta.text(), "X world");

    // Single undo should restore to pre-replacement state (undo group).
    ta.undo();
    assert_eq!(ta.text(), "hello world");
}

#[test]
fn backspace_works_with_zero_width_selection() {
    // Regression: a zero-width selection (anchor == head, from mouse
    // jitter) caused Backspace/Delete to be silently swallowed because
    // delete_selection() returned false but input() still returned early.
    let mut ta = ta_with("hello");
    ta.set_cursor(5);
    // Simulate a zero-width selection (anchor == head at cursor).
    ta.set_selection(5, 5);
    assert!(ta.selection_range().is_none()); // zero-width → no range

    // Backspace must still delete the char before cursor.
    ta.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(ta.text(), "hell");
    assert_eq!(ta.cursor(), 4);
    // Selection should be cleared.
    assert!(ta.selection.is_none());
}

#[test]
fn delete_forward_works_with_zero_width_selection() {
    let mut ta = ta_with("hello");
    ta.set_cursor(2);
    ta.set_selection(2, 2);

    ta.input(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(ta.text(), "helo");
    assert_eq!(ta.cursor(), 2);
    assert!(ta.selection.is_none());
}

#[test]
fn ctrl_x_with_zero_width_selection_falls_through() {
    let mut ta = ta_with("hello");
    ta.set_cursor(5);
    ta.set_selection(5, 5);

    // Ctrl-X on zero-width selection shouldn't eat the key.
    // It should clear selection and fall through to normal handling
    // (which for Ctrl-X without selection is a no-op, but the selection
    // must be cleared).
    ta.input(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    assert!(ta.selection.is_none());
}

#[test]
fn mouse_up_discards_zero_width_drag_selection() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Click at col 3 then drag to same position (zero distance).
    ta.handle_mouse(mouse_down(3, 0), area, state);
    let action = ta.handle_mouse(mouse_drag(3, 0), area, state);
    assert_eq!(action, MouseAction::CursorPlaced);
    let action = ta.handle_mouse(mouse_up(3, 0), area, state);
    assert_eq!(action, MouseAction::Nothing);

    // Zero-width drag should not leave a selection behind.
    assert!(ta.selection.is_none());
}

#[test]
fn drag_backward_selects_correctly() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Click at col 8, drag back to col 3 (backward selection)
    ta.handle_mouse(mouse_down(8, 0), area, state);
    ta.handle_mouse(mouse_drag(3, 0), area, state);
    // selection_range() normalizes anchor/head
    assert_eq!(ta.selection_range(), Some(3..8));
    assert_eq!(ta.selected_text(), Some("lo wo".to_string()));
}

#[test]
fn click_after_drag_clears_selection() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Drag to select
    ta.handle_mouse(mouse_down(0, 0), area, state);
    ta.handle_mouse(mouse_drag(5, 0), area, state);
    ta.handle_mouse(mouse_up(5, 0), area, state);
    assert!(ta.selection_range().is_some());

    // New click clears the old selection
    ta.handle_mouse(mouse_down(8, 0), area, state);
    assert!(ta.selection_range().is_none());
}

#[test]
fn set_text_clears_selection_and_drag_state() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    ta.handle_mouse(mouse_down(0, 0), area, state);
    ta.handle_mouse(mouse_drag(5, 0), area, state);
    assert!(ta.selection_range().is_some());
    assert!(ta.drag_anchor.is_some());
    assert!(ta.drag_active);
    assert!(ta.mouse_down_pos.is_some());

    ta.set_text("reset");

    assert!(ta.selection.is_none());
    assert!(ta.selection_range().is_none());
    assert!(ta.drag_anchor.is_none());
    assert!(!ta.drag_active);
    assert!(ta.mouse_down_pos.is_none());
    assert!(ta.pending_drag_scroll.is_none());
}

#[test]
fn same_cell_drag_does_not_create_selection() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    ta.handle_mouse(mouse_down(3, 0), area, state);
    let action = ta.handle_mouse(mouse_drag(3, 0), area, state);

    assert_eq!(action, MouseAction::CursorPlaced);
    assert!(ta.selection.is_none());
    assert!(ta.selection_range().is_none());
    assert!(!ta.drag_active);
}

#[test]
fn typing_with_zero_width_selection_inserts_character() {
    let mut ta = ta_with("hello");
    ta.set_cursor(5);
    ta.set_selection(5, 5);

    ta.input(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::SHIFT));

    assert_eq!(ta.text(), "hello!");
    assert_eq!(ta.cursor(), 6);
    assert!(ta.selection.is_none());
}

#[test]
fn mouse_up_after_drag_clears_drag_anchor() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    ta.handle_mouse(mouse_down(0, 0), area, state);
    ta.handle_mouse(mouse_drag(5, 0), area, state);
    assert!(ta.drag_anchor.is_some());

    ta.handle_mouse(mouse_up(5, 0), area, state);

    assert!(ta.drag_anchor.is_none());
    assert!(!ta.drag_active);
}

// ── M5: Double/triple click tests ──

#[test]
fn double_click_selects_word() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Double-click on "hello" (col 2)
    ta.handle_mouse(mouse_down(2, 0), area, state);
    let action = ta.handle_mouse(mouse_down(2, 0), area, state);
    assert_eq!(action, MouseAction::SelectionFinished);
    assert_eq!(ta.selection_range(), Some(0..5));
    assert_eq!(ta.selected_text(), Some("hello".to_string()));
    assert_eq!(ta.take_clipboard(), Some("hello".to_string()));
}

#[test]
fn double_click_cursor_on_last_char() {
    // Neovim places cursor on the last character of the selection,
    // not one past the end.
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Double-click on "hello"
    ta.handle_mouse(mouse_down(2, 0), area, state);
    ta.handle_mouse(mouse_down(2, 0), area, state);

    assert_eq!(ta.selection_range(), Some(0..5));
    // Cursor should be on 'o' (byte 4), not on ' ' (byte 5)
    assert_eq!(
        ta.cursor(),
        4,
        "double-click cursor should be on last char 'o' (byte 4), got {}",
        ta.cursor()
    );
}

#[test]
fn double_click_cursor_on_last_char_unicode() {
    // Test with multi-byte characters — cursor must land on a valid char boundary
    let mut ta = ta_with("café bar");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Double-click on "café" (col 1 = 'a')
    ta.handle_mouse(mouse_down(1, 0), area, state);
    ta.handle_mouse(mouse_down(1, 0), area, state);

    assert_eq!(ta.selected_text(), Some("café".to_string()));
    // 'é' is 2 bytes (0xC3 0xA9), so "café" = [c(0), a(1), f(2), é(3,4)]
    // Last char 'é' starts at byte 3
    assert_eq!(
        ta.cursor(),
        3,
        "cursor should be on 'é' (byte 3), got {}",
        ta.cursor()
    );
}

#[test]
fn double_click_on_second_word() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Double-click on "world" (col 8)
    ta.handle_mouse(mouse_down(8, 0), area, state);
    let action = ta.handle_mouse(mouse_down(8, 0), area, state);
    assert_eq!(action, MouseAction::SelectionFinished);
    assert_eq!(ta.selection_range(), Some(6..11));
    assert_eq!(ta.selected_text(), Some("world".to_string()));
}

#[test]
fn double_click_stops_at_punctuation() {
    // "hello, world," — double-click on 'h' should select "hello", not "hello,"
    let mut ta = ta_with("hello, world,");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Double-click on "hello" (col 2 = 'l')
    ta.handle_mouse(mouse_down(2, 0), area, state);
    let action = ta.handle_mouse(mouse_down(2, 0), area, state);
    assert_eq!(action, MouseAction::SelectionFinished);
    assert_eq!(
        ta.selected_text(),
        Some("hello".to_string()),
        "should select only 'hello', not include trailing comma"
    );
    assert_eq!(ta.selection_range(), Some(0..5));
}

#[test]
fn double_click_on_punctuation_selects_punctuation_run() {
    // Double-click on punctuation selects the contiguous punctuation
    let mut ta = ta_with("hello... world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Double-click on "..." (col 6 = first '.')
    ta.handle_mouse(mouse_down(6, 0), area, state);
    let action = ta.handle_mouse(mouse_down(6, 0), area, state);
    assert_eq!(action, MouseAction::SelectionFinished);
    assert_eq!(
        ta.selected_text(),
        Some("...".to_string()),
        "double-click on punctuation should select the punctuation run"
    );
}

#[test]
fn double_click_word_with_underscore() {
    // Underscores are part of a word (like vim's iskeyword)
    let mut ta = ta_with("hello_world foo");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Double-click on "hello_world" (col 3)
    ta.handle_mouse(mouse_down(3, 0), area, state);
    let action = ta.handle_mouse(mouse_down(3, 0), area, state);
    assert_eq!(action, MouseAction::SelectionFinished);
    assert_eq!(
        ta.selected_text(),
        Some("hello_world".to_string()),
        "underscore should be part of the word"
    );
}

#[test]
fn double_click_on_element_snaps_like_single_click() {
    // Word-selecting an element would copy its hidden buffer text to
    // the clipboard; a double-click must instead snap to the element
    // start and re-emit Click so the host decides what it means.
    let mut ta = TextArea::new();
    ta.insert_str("hi ");
    let display = Line::from("[chip]");
    let id = ta.insert_element("hidden\ntext", ElementKind(0), Some(display));
    ta.insert_str(" bye");
    // Buffer: "hi hidden\ntext bye", element at 3..14, display "[chip]".

    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // First click on the display (col 4) emits its own Click event.
    ta.handle_mouse(mouse_down(4, 0), area, state);
    assert!(ta.poll_element_event().is_some());

    let action = ta.handle_mouse(mouse_down(4, 0), area, state);
    assert_eq!(action, MouseAction::CursorPlaced);
    assert_eq!(ta.cursor(), 3); // element start
    assert!(ta.selection_range().is_none());
    assert_eq!(ta.take_clipboard(), None);
    let ev = ta
        .poll_element_event()
        .expect("double-click re-emits Click");
    assert_eq!(ev.id, id);
    assert_eq!(ev.kind, TextElementEventKind::Click);
}

#[test]
fn triple_click_selects_line() {
    let mut ta = ta_with("hello world\nsecond line\nthird");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Triple-click on first line (col 3)
    ta.handle_mouse(mouse_down(3, 0), area, state);
    ta.handle_mouse(mouse_down(3, 0), area, state);
    let action = ta.handle_mouse(mouse_down(3, 0), area, state);
    assert_eq!(action, MouseAction::SelectionFinished);
    // Should select "hello world\n" (including the newline)
    assert_eq!(ta.selection_range(), Some(0..12));
    assert_eq!(ta.selected_text(), Some("hello world\n".to_string()));
}

#[test]
fn triple_click_on_last_line_selects_to_end() {
    let mut ta = ta_with("hello\nworld");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Triple-click on "world" (row 1, col 2)
    ta.handle_mouse(mouse_down(2, 1), area, state);
    ta.handle_mouse(mouse_down(2, 1), area, state);
    let action = ta.handle_mouse(mouse_down(2, 1), area, state);
    assert_eq!(action, MouseAction::SelectionFinished);
    // Last line has no trailing \n — selects to text.len()
    assert_eq!(ta.selection_range(), Some(6..11));
    assert_eq!(ta.selected_text(), Some("world".to_string()));
}

#[test]
fn triple_click_cursor_stays_at_click_pos() {
    // Triple-click should select the whole line but keep the cursor
    // at the click position, not at the end of the selection.
    let mut ta = ta_with("hello world\nsecond line\nthird");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Triple-click on first line at col 3 (byte 3 = 'l')
    ta.handle_mouse(mouse_down(3, 0), area, state);
    ta.handle_mouse(mouse_down(3, 0), area, state);
    ta.handle_mouse(mouse_down(3, 0), area, state);

    // Selection covers the full line "hello world\n"
    assert_eq!(ta.selection_range(), Some(0..12));

    // Cursor should be at the click position (byte 3), not at line_end (12)
    assert_eq!(
        ta.cursor(),
        3,
        "triple-click cursor should stay at click pos (3), got {}",
        ta.cursor()
    );
}

#[test]
fn selection_uses_custom_style_override() {
    let mut t = ta_with("hello");
    t.selection_style = Style::default().bg(Color::Blue);
    t.set_selection(1, 4); // select "ell"

    let area = Rect::new(0, 0, 10, 1);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::WidgetRef::render_ref(&(&t), area, &mut buf);

    // Cells 1, 2, 3 should have Blue background (custom selection style)
    for col in 1..4u16 {
        let cell = &buf[(col, 0)];
        assert_eq!(
            cell.bg,
            Color::Blue,
            "cell at col {col} should have Blue bg"
        );
    }
    // Cell 0 ('h') and cell 4 ('o') should NOT have Blue bg
    assert_ne!(buf[(0, 0)].bg, Color::Blue);
    assert_ne!(buf[(4, 0)].bg, Color::Blue);
}

#[test]
fn double_click_on_whitespace_places_cursor() {
    let mut ta = ta_with("hello   world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Double-click on whitespace (col 6)
    ta.handle_mouse(mouse_down(6, 0), area, state);
    let action = ta.handle_mouse(mouse_down(6, 0), area, state);
    // Whitespace has no word → just places cursor
    assert_eq!(action, MouseAction::CursorPlaced);
    assert!(ta.selection_range().is_none());
}

#[test]
fn click_tracker_resets_on_position_change() {
    let mut ta = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Click at col 2, then at col 8 → not a double-click
    ta.handle_mouse(mouse_down(2, 0), area, state);
    let action = ta.handle_mouse(mouse_down(8, 0), area, state);
    // Should be a single click, not a double-click
    assert_eq!(action, MouseAction::CursorPlaced);
    assert!(ta.selection_range().is_none());
}

// ── Drag-to-scroll tests ──

#[test]
fn drag_below_area_scrolls_down_and_extends_selection() {
    // Text with 5 lines, visible area is only 3 rows tall.
    let mut ta = ta_with("aaa\nbbb\nccc\nddd\neee");
    // Place cursor at start so scroll=0.
    ta.set_cursor(0);
    let area = Rect::new(0, 0, 40, 3);
    let state = TextAreaState::default();

    // Click on first line.
    ta.handle_mouse(mouse_down(0, 0), area, state);
    assert_eq!(ta.cursor(), 0);

    // Drag below the visible area (row 5, past area.height=3).
    let action = ta.handle_mouse(mouse_drag(0, 5), area, state);
    assert_eq!(action, MouseAction::SelectionUpdated);

    // Cursor should have moved past the visible area.
    // With scroll=0 and height=3, visible lines are 0,1,2 (aaa,bbb,ccc).
    // Dragging below → target_line = visible_end = 3 → "ddd" starts at byte 12.
    // At col 0, cursor should be at byte 12 (start of "ddd").
    assert!(ta.cursor() >= 12);

    // Selection should extend from anchor (0) to the new cursor position.
    let range = ta.selection_range().unwrap();
    assert_eq!(range.start, 0);
    assert!(range.end >= 12);
}

#[test]
fn drag_above_area_scrolls_up_and_extends_selection() {
    // Text with 5 lines, start with cursor on the last line.
    let mut ta = ta_with("aaa\nbbb\nccc\nddd\neee");
    let area = Rect::new(0, 0, 40, 3);

    // Place cursor at the end first (to ensure scroll is at the bottom).
    ta.set_cursor(ta.text().len());
    let state = TextAreaState { scroll: 2 };

    // Click on bottom visible line (row 2).
    ta.handle_mouse(mouse_down(1, 2), area, state);

    // Drag above the visible area (row is before area.y).
    // Since area.y = 0, dragging to row=0 when scroll=2 means the row
    // is at the top edge. We need a row *above* the area. With area.y=0,
    // we can't go negative, but we can use an area with area.y > 0.
    let area2 = Rect::new(0, 5, 40, 3); // area starts at row 5
    ta.handle_mouse(mouse_down(1, 7), area2, state); // click at row 7 (visible)

    // Drag above: row 3 (above area2.y=5)
    let action = ta.handle_mouse(mouse_drag(0, 3), area2, state);
    assert_eq!(action, MouseAction::SelectionUpdated);

    // Cursor should have moved to a line above the visible region.
    // The exact position depends on how many lines we scroll per drag.
    let range = ta.selection_range().unwrap();
    assert!(range.start < range.end);
}

#[test]
fn drag_below_area_moves_cursor_past_last_visible_line() {
    // 10 short lines, area shows only 2.
    let text = "L0\nL1\nL2\nL3\nL4\nL5\nL6\nL7\nL8\nL9";
    let mut ta = ta_with(text);
    // Place cursor at start so scroll=0.
    ta.set_cursor(0);
    let area = Rect::new(0, 0, 40, 2);
    let state = TextAreaState::default();

    // Click at start.
    ta.handle_mouse(mouse_down(0, 0), area, state);
    assert_eq!(ta.cursor(), 0);

    // Drag below area (row 10, way below the 2-row area).
    let action = ta.handle_mouse(mouse_drag(1, 10), area, state);
    assert_eq!(action, MouseAction::SelectionUpdated);

    // With scroll=0 and height=2, visible lines are 0,1 (L0,L1).
    // Dragging below → target_line = 2 → "L2" starts at byte 6.
    // Cursor should be at col 1 of L2 → byte 7.
    assert!(ta.cursor() >= 6, "cursor={} should be >= 6", ta.cursor());
}

#[test]
fn drag_above_wide_column_still_scrolls_up() {
    // Bug: when dragging above the area with a column wider than the
    // target line, display_col_to_buffer_pos returns line_end which
    // equals the *next* line's start.  wrapped_line_index_by_start
    // then resolves to the next line, so effective_scroll sees the
    // cursor as still within the viewport and doesn't scroll.
    //
    // Scenario: 10 short lines ("ab"), area is 3 rows tall with
    // area.y = 2 (so we can drag above).  Scroll starts at line 5.
    // We drag to row 1 (above area.y = 2) at column 50 (way past
    // each 3-byte line).  The cursor must land ON the target line
    // (line 4), not spill over to line 5.
    let text = "ab\nab\nab\nab\nab\nab\nab\nab\nab\nab";
    let mut ta = ta_with(text);
    let area = Rect::new(0, 2, 40, 3); // area starts at row 2
    let state = TextAreaState { scroll: 5 };

    // Place cursor on wrapped line 6 (within viewport at scroll=5).
    // "ab\n" is 3 bytes per line, so line 6 starts at byte 18.
    ta.set_cursor(18);

    // Click inside the area (row 3 = area.y + 1).
    ta.handle_mouse(mouse_down(1, 3), area, state);

    // Drag above the area: row 1 (< area.y=2), column 50 (far right).
    let action = ta.handle_mouse(mouse_drag(50, 1), area, state);
    assert_eq!(action, MouseAction::SelectionUpdated);

    // target_line = scroll(5) - 1 = 4.  Line 4 spans bytes 12..15.
    // The cursor MUST be within line 4's range [12, 14], NOT at 15
    // (which is line 5's start).
    let cursor = ta.cursor();
    assert!(
        (12..15).contains(&cursor),
        "cursor={cursor} should be in [12, 15) (on line 4), \
         not at 15 (line 5 start)"
    );
}

#[test]
fn drag_below_wide_column_still_scrolls_down() {
    // Same bug but for scrolling down: dragging below the area with
    // a wide column should place the cursor on the target line, not
    // spill over to the next line.
    let text = "ab\nab\nab\nab\nab\nab\nab\nab\nab\nab";
    let mut ta = ta_with(text);
    let area = Rect::new(0, 0, 40, 3);
    let state = TextAreaState::default(); // scroll=0

    // Place cursor on first line.
    ta.set_cursor(0);

    // Click inside the area.
    ta.handle_mouse(mouse_down(1, 0), area, state);

    // Drag below the area: row 5 (>= area.y + height=3), column 50.
    let action = ta.handle_mouse(mouse_drag(50, 5), area, state);
    assert_eq!(action, MouseAction::SelectionUpdated);

    // visible_end = 0 + 3 = 3.  dist = 5 - 3 + 1 = 3.
    // n = drag_scroll_lines_for_distance(3) = 2.
    // target_line = (3 + 2 - 1) = 4.  Line 4 spans bytes 12..15.
    // Cursor must be within [12, 14], not at 15.
    let cursor = ta.cursor();
    assert!(
        (12..15).contains(&cursor),
        "cursor={cursor} should be in [12, 15) (on line 4), \
         not at 15 (line 5 start)"
    );
}

#[test]
fn drag_above_with_multibyte_line_end_does_not_panic() {
    // Regression: clamp_to_line used `line_end - 1` which can land inside
    // a multi-byte character (e.g. '│' = 3 bytes).
    let text = "aaa│\nbbb│\nccc│\nddd│\neee│\nfff│\nggg│";
    let mut ta = ta_with(text);
    let area = Rect::new(0, 0, 40, 3);
    // Start scrolled down so we can drag above.
    let state = TextAreaState { scroll: 3 };
    ta.set_cursor(20); // somewhere in the middle

    // Click inside the area.
    ta.handle_mouse(mouse_down(1, 1), area, state);

    // Drag above the area at a wide column (beyond line width).
    let action = ta.handle_mouse(mouse_drag(50, 0), area, state);
    // Should not panic — cursor should be on a valid char boundary.
    assert!(
        matches!(action, MouseAction::SelectionUpdated),
        "drag above should create selection, got {action:?}"
    );
    // Verify cursor is at a valid char boundary by reading from it.
    let cursor = ta.cursor();
    assert!(
        ta.text().is_char_boundary(cursor),
        "cursor at byte {cursor} is not a char boundary"
    );
}

#[test]
fn selection_across_element_with_multibyte_chars_does_not_panic() {
    // Regression: display_col_to_buffer_pos used `line_end + 1` to skip
    // past elements, but `line_end + 1` can land inside a multi-byte
    // character (e.g. '│' = 3 bytes).
    let mut ta = TextArea::new();
    ta.insert_str("before ");
    // Create an element whose backing text contains multi-byte '│' chars
    // across multiple lines — this triggers wrapping mid-element.
    let backing = "│  Ctrl+Shift+Z/Y  redo  │\n│  Ctrl+C  clear  │";
    ta.insert_element(backing, ElementKind(0), None);
    ta.insert_str(" after");

    let area = Rect::new(0, 0, 30, 5); // narrow so wrapping is forced

    // Select across the entire text (from start to end).
    ta.set_selection(0, ta.text().len());

    // Render should not panic.
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::WidgetRef::render_ref(&(&ta), area, &mut buf);
}

#[test]
fn click_on_text_with_multibyte_chars_does_not_panic() {
    // Plain text with '│' — clicking anywhere should not panic.
    let text = "│  Ctrl+Shift+Z/Y  redo  │\n│  Ctrl+C  clear  │";
    let mut ta = ta_with(text);
    let area = Rect::new(0, 0, 30, 5);
    let state = TextAreaState::default();

    // Click at various columns — should not panic.
    for col in 0..25u16 {
        ta.handle_mouse(mouse_down(col, 0), area, state);
    }
    // Double-click should also be safe.
    ta.handle_mouse(mouse_down(5, 0), area, state);
    ta.handle_mouse(mouse_down(5, 0), area, state);
}

#[test]
fn selecting_wrapped_line_ending_with_multibyte_char_does_not_panic() {
    // Regression: when a line wraps and '│' (3-byte char) ends up right
    // at the wrap boundary, the wrapping code (or rendering) can produce
    // a byte position inside the multi-byte character.
    //
    // Reproduce: enough spaces so '│' is pushed to the next wrapped line.
    let text = format!("{}│", " ".repeat(29)); // 29 spaces + '│' = 30 display cols
    let mut ta = ta_with(&text);
    let area = Rect::new(0, 0, 30, 5); // width 30 → '│' wraps to next line
    let _state = TextAreaState::default();

    // Select across the wrap boundary.
    ta.set_selection(0, ta.text().len());

    // Render should not panic.
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::WidgetRef::render_ref(&(&ta), area, &mut buf);
}

#[test]
fn clicking_on_wrapped_multibyte_line_does_not_panic() {
    // Same as above but triggered via click/drag rather than render.
    for extra_spaces in 28..33 {
        let text = format!("{}│end", " ".repeat(extra_spaces));
        let mut ta = ta_with(&text);
        let area = Rect::new(0, 0, 30, 5);
        let state = TextAreaState::default();

        // Click on every column of both rows.
        for row in 0..2u16 {
            for col in 0..30u16 {
                ta.handle_mouse(mouse_down(col, row), area, state);
            }
        }
    }
}

// ── Inline element tests ──

#[test]
fn inline_element_replaces_element_with_text() {
    let mut ta = TextArea::new();
    ta.insert_str("before ");
    let id = ta.insert_element("pasted\ncontent\nhere", ElementKind(1), None);
    ta.insert_str(" after");
    // Buffer: "before pasted\ncontent\nhere after"
    // Element at "pasted\ncontent\nhere" (bytes 7..26)

    let inlined = ta.inline_element(id);
    assert!(inlined);

    // Text should remain the same (the element's buffer text is kept).
    assert_eq!(ta.text(), "before pasted\ncontent\nhere after");
    // But the element should be gone.
    assert!(ta.elements().is_empty());
    // Cursor should be at the end of the inlined text.
    assert_eq!(ta.cursor(), 26); // end of "pasted\ncontent\nhere"
}

#[test]
fn inline_element_is_undoable() {
    let mut ta = TextArea::new();
    ta.insert_str("A ");
    let id = ta.insert_element("multi\nline", ElementKind(1), None);
    ta.insert_str(" B");
    // Buffer: "A multi\nline B", element at 2..12

    assert_eq!(ta.elements().len(), 1);

    ta.inline_element(id);
    assert!(ta.elements().is_empty());
    assert_eq!(ta.text(), "A multi\nline B");

    // Undo should restore the element.
    assert!(ta.undo());
    assert_eq!(ta.elements().len(), 1);
    assert_eq!(ta.element_text(id), Some("multi\nline"));
}

#[test]
fn inline_nonexistent_element_returns_false() {
    let mut ta = ta_with("hello");
    let fake_id = ElementId(9999);
    assert!(!ta.inline_element(fake_id));
}

#[test]
fn inline_element_cursor_at_element_start() {
    let mut ta = TextArea::new();
    let id = ta.insert_element("elem", ElementKind(0), None);
    ta.insert_str(" tail");
    // Cursor is after " tail" → at end.
    // Move cursor to element start.
    ta.set_cursor(0);

    ta.inline_element(id);
    // Element removed, text unchanged.
    assert!(ta.elements().is_empty());
    // Cursor at end of inlined region.
    assert_eq!(ta.cursor(), 4);
}

// ── Click-on-element edge cases ──

#[test]
fn click_on_element_second_half_snaps_to_start() {
    // Element with a wide display: clicking on the right half should still
    // snap cursor to element start and emit a Click element event.
    let mut ta = TextArea::new();
    ta.insert_str("ab");
    // Element display is "ELEM" (4 chars). Buffer text is "xy".
    let display = Line::from("ELEM");
    let id = ta.insert_element("xy", ElementKind(0), Some(display));
    ta.insert_str("cd");
    // Buffer: "abxycd", element at 2..4, display "ELEM" (4 wide)
    // Visual: a b E L E M c d
    //         0 1 2 3 4 5 6 7

    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();
    ta.set_cursor(0);

    // Click on col 5 → second half of "ELEM" display.
    // display_col_to_buffer_pos should return elem_end=4 (closer to end).
    // handle_mouse should detect this as on-element and snap to start.
    let action = ta.handle_mouse(mouse_down(5, 0), area, state);
    assert_eq!(action, MouseAction::CursorPlaced);
    let ev = ta.poll_element_event().expect("should emit element click");
    assert_eq!(ev.id, id);
    assert_eq!(ev.kind, TextElementEventKind::Click);
    assert_eq!(ta.cursor(), 2); // element start
}

#[test]
fn click_on_element_first_half_snaps_to_start() {
    let mut ta = TextArea::new();
    ta.insert_str("ab");
    let display = Line::from("ELEM");
    let id = ta.insert_element("xy", ElementKind(0), Some(display));
    ta.insert_str("cd");
    ta.set_cursor(0);

    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Click on col 2 → first half of "ELEM" display.
    let action = ta.handle_mouse(mouse_down(2, 0), area, state);
    assert_eq!(action, MouseAction::CursorPlaced);
    let ev = ta.poll_element_event().expect("should emit element click");
    assert_eq!(ev.id, id);
    assert_eq!(ev.kind, TextElementEventKind::Click);
    assert_eq!(ta.cursor(), 2);
}

#[test]
fn click_after_element_places_cursor_not_element() {
    let mut ta = TextArea::new();
    ta.insert_str("ab");
    let display = Line::from("EL");
    ta.insert_element("xy", ElementKind(0), Some(display));
    ta.insert_str("cd");
    ta.set_cursor(0);
    // Visual: a b E L c d
    //         0 1 2 3 4 5

    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Click on col 4 → 'c' (after element).
    let action = ta.handle_mouse(mouse_down(4, 0), area, state);
    assert_eq!(action, MouseAction::CursorPlaced);
    assert_eq!(ta.cursor(), 4); // byte 4 = 'c'
}

// ── Mouse wheel tests ──

fn mouse_scroll_down(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn mouse_scroll_up(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn scroll_down_returns_scrolled() {
    let mut ta = ta_with("aaa\nbbb\nccc\nddd\neee");
    ta.set_cursor(0);
    let area = Rect::new(0, 0, 40, 3);
    let state = TextAreaState::default();

    let action = ta.handle_mouse(mouse_scroll_down(5, 1), area, state);
    assert_eq!(action, MouseAction::Scrolled);
}

#[test]
fn scroll_up_returns_scrolled() {
    let mut ta = ta_with("aaa\nbbb\nccc\nddd\neee");
    ta.set_cursor(ta.text().len());
    let area = Rect::new(0, 0, 40, 3);
    let state = TextAreaState { scroll: 2 };

    let action = ta.handle_mouse(mouse_scroll_up(5, 1), area, state);
    assert_eq!(action, MouseAction::Scrolled);
}

#[test]
fn scroll_down_when_content_fits_returns_nothing() {
    let mut ta = ta_with("short");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    let action = ta.handle_mouse(mouse_scroll_down(5, 1), area, state);
    assert_eq!(action, MouseAction::Nothing);
}

#[test]
fn mousewheel_scrolls_viewport_not_cursor() {
    // Mousewheel should scroll the viewport without moving the cursor.
    // The cursor should stay at its current buffer position.
    let text = (0..20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut ta = ta_with(&text);
    let area = Rect::new(0, 0, 40, 5); // only 5 lines visible
    let state = TextAreaState::default();

    // Place cursor on "line 2"
    let line2_start = text.find("line 2").unwrap();
    ta.set_cursor(line2_start);
    let cursor_before = ta.cursor();

    // Scroll down
    ta.handle_mouse(mouse_scroll_down(0, 0), area, state);

    // Cursor must NOT have moved
    assert_eq!(
        ta.cursor(),
        cursor_before,
        "mousewheel should not move cursor"
    );
}

#[test]
fn click_after_scroll_places_cursor_at_clicked_line() {
    // After scrolling the viewport away from the cursor via mousewheel,
    // clicking on a visible line should place the cursor on THAT line —
    // not jump to some other position based on the old cursor location.
    let text = (0..40)
        .map(|i| format!("line {:02}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let mut ta = ta_with(&text);
    // height=20 → scroll_lines_for_height returns 3 lines/tick
    let area = Rect::new(0, 0, 40, 20);
    let mut state = TextAreaState::default();

    // Cursor starts at line 0.
    ta.set_cursor(0);

    // Render to initialize state.scroll.
    let mut buf = Buffer::empty(area);
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&ta), area, &mut buf, &mut state);

    // Scroll down 3 ticks (3 lines × 3 = 9 lines).
    for _ in 0..3 {
        ta.handle_mouse(mouse_scroll_down(0, 0), area, state);
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&ta), area, &mut buf, &mut state);
    }

    // Viewport should now start around line 9.
    assert!(
        state.scroll >= 9,
        "viewport should have scrolled; scroll={}",
        state.scroll
    );

    // Click on visual row 0 (which is now "line 09" or similar).
    ta.handle_mouse(mouse_down(0, 0), area, state);

    // The cursor should now be on a line that was VISIBLE.
    let cursor = ta.cursor();
    let lines = ta.wrapped_lines(area.width);
    let cursor_line = TextArea::wrapped_line_index_by_start(&lines, cursor).unwrap();
    assert!(
        cursor_line >= state.scroll as usize && cursor_line < (state.scroll + area.height) as usize,
        "click on visible row 0 should place cursor on a visible line; \
         cursor_line={cursor_line}, scroll={}, visible=[{}..{})",
        state.scroll,
        state.scroll,
        state.scroll as usize + area.height as usize,
    );
}

#[test]
fn drag_select_after_scroll_selects_visible_text() {
    // After scrolling, drag-selecting should work on the visible text,
    // not jump the viewport back.
    let text = (0..20)
        .map(|i| format!("line {:02}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let mut ta = ta_with(&text);
    let area = Rect::new(0, 0, 40, 5);
    let mut state = TextAreaState::default();

    ta.set_cursor(0);

    // Render + scroll down.
    let mut buf = Buffer::empty(area);
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&ta), area, &mut buf, &mut state);
    for _ in 0..3 {
        ta.handle_mouse(mouse_scroll_down(0, 0), area, state);
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&ta), area, &mut buf, &mut state);
    }
    let scroll_after = state.scroll;

    // Click-down on row 1, then drag to row 3.
    ta.handle_mouse(mouse_down(0, 1), area, state);
    // Re-render so state.scroll updates after the click.
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&ta), area, &mut buf, &mut state);
    ta.handle_mouse(mouse_drag(5, 3), area, state);

    // Selection should exist.
    let sel = ta.selection_range().expect("drag should create selection");

    // The selected region should be within the visible range, not at line 0.
    let lines = ta.wrapped_lines(area.width);
    let sel_start_line = TextArea::wrapped_line_index_by_start(&lines, sel.start).unwrap();
    assert!(
        sel_start_line >= scroll_after as usize,
        "selection start should be in scrolled region; \
         sel_start_line={sel_start_line}, scroll={scroll_after}"
    );
}

#[test]
fn drag_outside_after_mousewheel_still_scrolls() {
    // After mousewheel scrolling during a drag, dragging outside the
    // textarea area should continue to auto-scroll the viewport.
    let text = (0..30)
        .map(|i| format!("line {:02}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let mut ta = ta_with(&text);
    let area = Rect::new(0, 0, 40, 5);
    let mut state = TextAreaState::default();

    ta.set_cursor(0);

    // Render to initialize state.
    let mut buf = Buffer::empty(area);
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&ta), area, &mut buf, &mut state);

    // Start drag at row 0.
    ta.handle_mouse(mouse_down(0, 0), area, state);
    ta.handle_mouse(mouse_drag(0, 1), area, state);
    assert!(ta.selection_range().is_some());

    // Mousewheel scroll down during drag.
    ta.handle_mouse(mouse_scroll_down(0, 0), area, state);
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&ta), area, &mut buf, &mut state);
    let scroll_after_wheel = state.scroll;

    // Now drag below the area (row = area.y + area.height = 5).
    // This should auto-scroll the viewport further down.
    // We need to bypass throttle, so reset the timer.
    ta.last_drag_scroll = None;
    ta.drag_scroll_steps = 0;
    ta.handle_mouse(mouse_drag(0, area.y + area.height), area, state);
    ratatui::widgets::StatefulWidgetRef::render_ref(&(&ta), area, &mut buf, &mut state);

    assert!(
        state.scroll > scroll_after_wheel,
        "drag-below after mousewheel should continue scrolling; \
         scroll={}, expected > {scroll_after_wheel}",
        state.scroll
    );
}

#[test]
fn scroll_during_drag_preserves_selection_anchor() {
    // Start drag at "bbb", scroll down — selection should extend,
    // anchor stays at original position.
    let mut ta = ta_with("aaa\nbbb\nccc\nddd\neee\nfff\nggg");
    ta.set_cursor(0); // put cursor at start so scroll=0 is consistent
    let area = Rect::new(0, 0, 40, 3);
    let state = TextAreaState::default();

    // Click-down on "bbb" (row 1, col 1 → byte 5 = second 'b')
    ta.handle_mouse(mouse_down(1, 1), area, state);
    let anchor = ta.cursor();
    assert_eq!(
        anchor, 5,
        "click on bbb col 1 should place cursor at byte 5"
    );

    // Start drag → creates selection
    ta.handle_mouse(mouse_drag(2, 1), area, state);
    assert!(
        ta.selection_range().is_some(),
        "drag should create selection"
    );

    // Now scroll down while dragging
    let action = ta.handle_mouse(mouse_scroll_down(1, 1), area, state);
    assert_eq!(action, MouseAction::Scrolled);

    // Selection should still exist and anchor should not have moved
    let sel = ta
        .selection_range()
        .expect("selection should survive scroll");
    assert!(
        sel.contains(&anchor),
        "anchor byte {anchor} should still be inside selection {sel:?}"
    );
}

#[test]
fn scroll_during_drag_extends_selection_head() {
    // Scroll down during drag should move the selection head forward.
    let mut ta = ta_with("aaa\nbbb\nccc\nddd\neee\nfff\nggg");
    ta.set_cursor(0);
    let area = Rect::new(0, 0, 40, 3);
    let state = TextAreaState::default();

    // Click-down on "aaa" (row 0, col 1)
    ta.handle_mouse(mouse_down(1, 0), area, state);
    // Start drag
    ta.handle_mouse(mouse_drag(2, 0), area, state);
    let sel_before = ta.selection_range().unwrap();

    // Scroll down
    ta.handle_mouse(mouse_scroll_down(1, 1), area, state);
    let sel_after = ta.selection_range().unwrap();

    // Selection should have grown (head moved forward)
    assert!(
        sel_after.end > sel_before.end,
        "scroll-down during drag should extend selection: before={sel_before:?} after={sel_after:?}"
    );
    // Anchor should not have moved
    assert_eq!(sel_after.start, sel_before.start);
}

#[test]
fn down_during_active_drag_does_not_reset_anchor() {
    // Some terminals re-emit Down(Left) after a scroll event even though
    // the button was held the whole time.  When `drag_active` is true,
    // a Down should be treated as a drag continuation, not a new click.
    let mut ta = ta_with("aaa\nbbb\nccc\nddd\neee\nfff\nggg");
    ta.set_cursor(0);
    let area = Rect::new(0, 0, 40, 3);
    let state = TextAreaState::default();

    // Click on "aaa" (row 0, col 1), then drag to start selection.
    ta.handle_mouse(mouse_down(1, 0), area, state);
    ta.handle_mouse(mouse_drag(2, 0), area, state);
    let anchor_before = ta.selection_range().unwrap().start;

    // Scroll down while dragging.
    ta.handle_mouse(mouse_scroll_down(1, 1), area, state);
    assert!(
        ta.selection_range().is_some(),
        "selection must survive scroll"
    );

    // Simulate terminal re-emitting Down(Left) at a different row.
    ta.handle_mouse(mouse_down(1, 2), area, state);

    // Selection should still exist and anchor should NOT have moved.
    let sel = ta
        .selection_range()
        .expect("Down during drag must not kill selection");
    assert_eq!(
        sel.start, anchor_before,
        "anchor must not reset: expected {anchor_before}, got {}",
        sel.start
    );
}

// ── Drag-scroll acceleration / distance helpers ──

#[test]
fn drag_scroll_interval_ramps_up() {
    // Step 0 → 80ms, step 1 → 60ms, step 2+ → 40ms.
    assert_eq!(TextArea::drag_scroll_interval(0), 80);
    assert_eq!(TextArea::drag_scroll_interval(1), 60);
    assert_eq!(TextArea::drag_scroll_interval(2), 40);
    assert_eq!(TextArea::drag_scroll_interval(100), 40);
}

#[test]
fn drag_scroll_lines_for_distance_tiers() {
    // Close: 1 line, farther: more lines.
    assert_eq!(TextArea::drag_scroll_lines_for_distance(1), 1);
    assert_eq!(TextArea::drag_scroll_lines_for_distance(2), 1);
    assert_eq!(TextArea::drag_scroll_lines_for_distance(3), 2);
    assert_eq!(TextArea::drag_scroll_lines_for_distance(5), 2);
    assert_eq!(TextArea::drag_scroll_lines_for_distance(6), 3);
    assert_eq!(TextArea::drag_scroll_lines_for_distance(10), 3);
    assert_eq!(TextArea::drag_scroll_lines_for_distance(20), 5);
}

// ── Scrollbar tests ──

#[test]
fn scrollbar_not_shown_when_content_fits() {
    // 3 lines of text in a 5-row viewport → no scrollbar needed.
    let mut ta = TextArea::new();
    ta.insert_str("aaa\nbbb\nccc");
    let area = Rect::new(0, 0, 20, 5);
    let (cw, needs) = ta.content_width(area.width, area.height);
    assert!(!needs, "should not need scrollbar when content fits");
    assert_eq!(cw, 20, "full width when no scrollbar");
}

#[test]
fn scrollbar_shown_when_content_overflows() {
    // 10 lines of text in a 5-row viewport → scrollbar needed.
    let mut ta = TextArea::new();
    ta.insert_str("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
    let area = Rect::new(0, 0, 20, 5);
    let (cw, needs) = ta.content_width(area.width, area.height);
    assert!(needs, "should need scrollbar when content overflows");
    assert_eq!(cw, 19, "width reduced by 1 for scrollbar");
}

#[test]
fn scrollbar_respects_show_scrollbar_false() {
    let mut ta = TextArea::new();
    ta.show_scrollbar = false;
    ta.insert_str("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
    let area = Rect::new(0, 0, 20, 5);
    let (cw, needs) = ta.content_width(area.width, area.height);
    assert!(!needs, "scrollbar disabled");
    assert_eq!(cw, 20, "full width when scrollbar disabled");
}

#[test]
fn scrollbar_wrapping_uses_narrower_width() {
    // A line that fits in 20 cols but not in 19 should wrap differently
    // when scrollbar is present.
    let mut ta = TextArea::new();
    // 19 'a's → fits in 19 cols (no wrap).
    // Then enough other lines to overflow the viewport.
    ta.insert_str(&format!("{}\n2\n3\n4\n5\n6", "a".repeat(19)));
    let area = Rect::new(0, 0, 20, 5);
    let (cw, needs) = ta.content_width(area.width, area.height);
    assert!(needs, "overflows");
    assert_eq!(cw, 19);
    // The 19-char line should NOT wrap at width 19 — it fits exactly.
    let lines = ta.wrapped_lines(cw);
    // First wrapped line should contain all 19 chars.
    assert_eq!(&ta.text()[lines[0].clone()], &"a".repeat(19));
}

#[test]
fn click_on_scrollbar_column_scrolls() {
    let mut ta = TextArea::new();
    ta.insert_str("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
    // Move cursor to start so we can verify it doesn't move.
    let _ = ta.text.set_cursor_byte(0);
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    // Click on the scrollbar column (rightmost column = 19),
    // at the bottom row of the viewport → should scroll to end.
    let action = ta.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 19,
            row: 4,
            modifiers: KeyModifiers::NONE,
        },
        area,
        state,
    );
    assert_eq!(action, MouseAction::Scrolled);
    // Cursor should not have moved — scrollbar click doesn't place cursor.
    assert_eq!(ta.cursor(), 0);
    // scroll_override should be set.
    assert!(ta.scroll_override.is_some());
}

#[test]
fn click_on_scrollbar_top_scrolls_to_top() {
    let mut ta = TextArea::new();
    ta.insert_str("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    // Scroll to the bottom first so the top of the track is NOT the thumb.
    ta.scroll_override = Some(5);
    // Click at top of scrollbar (row 0) — should be track, jump to top.
    ta.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 19,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        area,
        state,
    );
    assert_eq!(ta.scroll_override, Some(0));
}

#[test]
fn click_on_text_area_does_not_trigger_scrollbar() {
    let mut ta = TextArea::new();
    ta.insert_str("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    // Click on column 18 (text area, not scrollbar column 19).
    let action = ta.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 18,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        area,
        state,
    );
    // Should place cursor, not scroll.
    assert_eq!(action, MouseAction::CursorPlaced);
    assert!(!ta.scrollbar_dragging);
}

#[test]
fn drag_on_scrollbar_scrolls_proportionally() {
    let mut ta = TextArea::new();
    ta.insert_str("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    // Scroll to bottom so the thumb is at the bottom, then click
    // on the track at row 0 to start a track-based drag.
    ta.scroll_override = Some(5);
    ta.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 19,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        area,
        state,
    );
    assert!(ta.scrollbar_dragging);
    let scroll_at_top = ta.scroll_override.unwrap();
    assert_eq!(scroll_at_top, 0, "track click at top should jump to 0");
    // Drag to middle of scrollbar track.
    ta.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 19,
            row: 2,
            modifiers: KeyModifiers::NONE,
        },
        area,
        state,
    );
    let scroll_at_mid = ta.scroll_override.unwrap();
    assert!(
        scroll_at_mid > scroll_at_top,
        "dragging down should scroll further"
    );
    // Drag to bottom.
    ta.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 19,
            row: 4,
            modifiers: KeyModifiers::NONE,
        },
        area,
        state,
    );
    let scroll_at_bottom = ta.scroll_override.unwrap();
    assert!(
        scroll_at_bottom > scroll_at_mid,
        "dragging to bottom should scroll to max"
    );
    // Mouse up ends drag.
    ta.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 19,
            row: 4,
            modifiers: KeyModifiers::NONE,
        },
        area,
        state,
    );
    assert!(!ta.scrollbar_dragging);
}

#[test]
fn scrollbar_render_produces_track_and_thumb() {
    // Render a textarea with overflow and verify the scrollbar column
    // has non-default styled cells.
    let mut ta = TextArea::new();
    ta.insert_str("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
    let area = Rect::new(0, 0, 20, 5);
    let mut buf = Buffer::empty(area);
    let mut state = TextAreaState::default();
    StatefulWidgetRef::render_ref(&&ta, area, &mut buf, &mut state);

    let sb_col = 19u16;
    // All cells in the scrollbar column should have the track bg color.
    let mut has_thumb = false;
    for row in 0..5u16 {
        let cell = &buf[(sb_col, row)];
        // Track bg is Rgb(45,45,55); check bg is set.
        assert!(cell.style().bg.is_some(), "scrollbar cell should have bg");
        if cell.symbol() != " " {
            has_thumb = true;
        }
    }
    assert!(has_thumb, "should have at least one thumb cell");
}

#[test]
fn no_scrollbar_column_when_content_fits() {
    // When content fits, the rightmost column should not have
    // scrollbar styling.
    let mut ta = TextArea::new();
    ta.insert_str("hello");
    let area = Rect::new(0, 0, 20, 5);
    let mut buf = Buffer::empty(area);
    let mut state = TextAreaState::default();
    StatefulWidgetRef::render_ref(&&ta, area, &mut buf, &mut state);

    let last_col = 19u16;
    let cell = &buf[(last_col, 0u16)];
    // Should be default (empty space), not scrollbar styled.
    assert!(
        cell.style().bg.is_none() || !matches!(cell.style().bg, Some(Color::Rgb(32, 35, 53))),
        "should not have scrollbar bg when content fits"
    );
}

#[test]
fn cursor_pos_accounts_for_scrollbar_width() {
    // When scrollbar is shown, cursor position should use content width,
    // not full area width.
    let mut ta = TextArea::new();
    // Fill 18 chars + enough lines to overflow.
    ta.insert_str(&format!("{}\n2\n3\n4\n5\n6", "x".repeat(18)));
    let _ = ta.text.set_cursor_byte(0); // at start
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();
    let pos = ta.cursor_pos_with_state(area, state);
    // Cursor at pos 0 should be at (0, 0).
    assert_eq!(pos, Some((0, 0)));
}

#[test]
fn click_on_scrollbar_thumb_does_not_jump() {
    // With 10 lines in a 5-row viewport, the thumb is near the top
    // when scroll is at 0.  Clicking on the thumb should NOT jump —
    // it should just start a drag from the current position.
    let mut ta = TextArea::new();
    ta.insert_str("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();

    // Scroll is at 0 — thumb should be at the top of the track.
    // Click on row 0 (top of track = on the thumb).
    let action = ta.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 19,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        area,
        state,
    );
    assert_eq!(action, MouseAction::Scrolled);
    assert!(ta.scrollbar_dragging);
    // The scroll should NOT have changed — thumb click = no jump.
    assert!(
        ta.scroll_override.is_none() || ta.scroll_override == Some(0),
        "thumb click should not jump: {:?}",
        ta.scroll_override,
    );
}

#[test]
fn click_on_scrollbar_track_jumps() {
    // Clicking on the track (outside the thumb) should jump.
    let mut ta = TextArea::new();
    ta.insert_str("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
    let area = Rect::new(0, 0, 20, 5);
    let state = TextAreaState::default();

    // Scroll at 0, thumb near top.  Click at bottom of track (row 4)
    // which should be on the track, not the thumb.
    let action = ta.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 19,
            row: 4,
            modifiers: KeyModifiers::NONE,
        },
        area,
        state,
    );
    assert_eq!(action, MouseAction::Scrolled);
    // Should have jumped to a non-zero scroll position.
    assert!(
        ta.scroll_override.unwrap_or(0) > 0,
        "track click should jump"
    );
}

// ── Clipboard provider tests ──

#[test]
fn default_clipboard_provider_round_trips() {
    let mut ta = TextArea::new();
    ta.insert_str("hello world");
    // Select all via set_selection and cut
    ta.set_selection(0, 5);
    ta.input(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    // take_clipboard returns the cut text
    assert_eq!(ta.take_clipboard(), Some("hello".to_string()));
    // Ctrl-V pastes it back
    ta.input(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert_eq!(ta.text(), "hello world");
}

#[test]
fn custom_clipboard_provider() {
    #[derive(Debug)]
    struct TestClip {
        stored: Option<String>,
    }
    impl ClipboardProvider for TestClip {
        fn get(&mut self) -> Option<String> {
            self.stored.clone()
        }
        fn set(&mut self, text: &str) {
            self.stored = Some(format!("CUSTOM:{text}"));
        }
    }

    let mut ta = TextArea::new();
    ta.set_clipboard_provider(Box::new(TestClip { stored: None }));
    ta.insert_str("abc");
    ta.set_selection(0, 3);
    // Ctrl-X should call provider.set with "abc"
    ta.input(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    // Ctrl-V should paste from provider.get → "CUSTOM:abc"
    ta.input(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert_eq!(ta.text(), "CUSTOM:abc");
}

#[test]
fn ctrl_v_pastes_from_provider() {
    #[derive(Debug)]
    struct PreloadedClip;
    impl ClipboardProvider for PreloadedClip {
        fn get(&mut self) -> Option<String> {
            Some("pasted!".to_string())
        }
        fn set(&mut self, _text: &str) {}
    }

    let mut ta = TextArea::new();
    ta.set_clipboard_provider(Box::new(PreloadedClip));
    ta.input(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert_eq!(ta.text(), "pasted!");
}

#[test]
fn copy_on_selection_finalized_sets_provider() {
    // Drag-select → mouse up should call provider.set
    #[derive(Debug)]
    struct RecordingClip {
        last_set: Option<String>,
    }
    impl ClipboardProvider for RecordingClip {
        fn get(&mut self) -> Option<String> {
            self.last_set.clone()
        }
        fn set(&mut self, text: &str) {
            self.last_set = Some(text.to_string());
        }
    }

    let mut ta = TextArea::new();
    ta.set_clipboard_provider(Box::new(RecordingClip { last_set: None }));
    ta.insert_str("hello");
    ta.set_cursor(0);

    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Click at 0, drag to 5, release
    ta.handle_mouse(mouse_down(0, 0), area, state);
    ta.handle_mouse(mouse_drag(5, 0), area, state);
    ta.handle_mouse(mouse_up(5, 0), area, state);

    // Mouse-up copies to the provider; drop the highlight so Ctrl+V inserts
    // instead of replacing the selection.
    ta.clear_selection();
    ta.set_cursor(5);
    ta.input(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert_eq!(ta.text(), "hellohello");
}

// ── Hover / element event tests ──

fn mouse_moved(col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Moved,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn hover_enter_on_element() {
    let mut ta = TextArea::new();
    ta.insert_str("hi ");
    let id = ta.insert_element("elem", ElementKind(0), None);
    ta.insert_str(" bye");

    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Move over plain text — no event
    ta.handle_mouse(mouse_moved(0, 0), area, state);
    assert!(ta.poll_element_event().is_none());

    // Move over element (col 3)
    ta.handle_mouse(mouse_moved(3, 0), area, state);
    let ev = ta.poll_element_event().expect("should emit HoverEnter");
    assert_eq!(ev.id, id);
    assert_eq!(ev.kind, TextElementEventKind::HoverEnter);
}

#[test]
fn hover_leave_on_element() {
    let mut ta = TextArea::new();
    ta.insert_str("hi ");
    let id = ta.insert_element("elem", ElementKind(0), None);
    ta.insert_str(" bye");

    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Enter the element
    ta.handle_mouse(mouse_moved(3, 0), area, state);
    ta.poll_element_event(); // consume

    // Leave the element
    ta.handle_mouse(mouse_moved(0, 0), area, state);
    let ev = ta.poll_element_event().expect("should emit HoverLeave");
    assert_eq!(ev.id, id);
    assert_eq!(ev.kind, TextElementEventKind::HoverLeave);
}

#[test]
fn hover_stays_on_same_element_no_event() {
    let mut ta = TextArea::new();
    ta.insert_str("hi ");
    ta.insert_element("elem", ElementKind(0), None);
    ta.insert_str(" bye");

    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Enter the element (col 3)
    ta.handle_mouse(mouse_moved(3, 0), area, state);
    ta.poll_element_event(); // consume enter

    // Move within the element (col 4) — no new event
    ta.handle_mouse(mouse_moved(4, 0), area, state);
    assert!(ta.poll_element_event().is_none());
}

#[test]
fn hover_between_two_elements() {
    let mut ta = TextArea::new();
    let id1 = ta.insert_element("AA", ElementKind(0), None);
    ta.insert_str(" ");
    let id2 = ta.insert_element("BB", ElementKind(0), None);
    // Visual: A A   B B
    //         0 1 2 3 4

    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();

    // Hover element 1
    ta.handle_mouse(mouse_moved(0, 0), area, state);
    let ev = ta.poll_element_event().unwrap();
    assert_eq!(ev.id, id1);
    assert_eq!(ev.kind, TextElementEventKind::HoverEnter);

    // Move to element 2 — should emit enter for id2
    // (HoverLeave for id1 gets overwritten by HoverEnter for id2)
    ta.handle_mouse(mouse_moved(3, 0), area, state);
    let ev = ta.poll_element_event().unwrap();
    assert_eq!(ev.id, id2);
    assert_eq!(ev.kind, TextElementEventKind::HoverEnter);
}

// ── set_scroll_override / scroll_override tests ────────────────────

#[test]
fn scroll_override_getter_setter() {
    let mut ta = TextArea::new();
    assert_eq!(ta.scroll_override(), None);
    ta.set_scroll_override(Some(5));
    assert_eq!(ta.scroll_override(), Some(5));
    ta.set_scroll_override(None);
    assert_eq!(ta.scroll_override(), None);
}

/// Helper: stateful render (saves typing the full trait path).
fn render_stateful(ta: &TextArea, area: Rect, buf: &mut Buffer, state: &mut TextAreaState) {
    ratatui::widgets::StatefulWidgetRef::render_ref(&ta, area, buf, state);
}

#[test]
fn scroll_override_forces_viewport_ignoring_cursor() {
    // With 20 lines, cursor at end, and viewport of 5 rows,
    // effective_scroll normally follows the cursor to the bottom.
    // set_scroll_override(Some(0)) should force viewport to top.
    let text = (0..20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut ta = ta_with(&text);
    ta.set_cursor(ta.text().len()); // cursor at end
    let area = Rect::new(0, 0, 40, 5);
    let mut state = TextAreaState::default();
    let mut buf = Buffer::empty(area);

    // Render without override: cursor-follow scrolls to bottom.
    render_stateful(&ta, area, &mut buf, &mut state);
    assert!(state.scroll > 0, "should scroll to show cursor at end");
    let normal_scroll = state.scroll;

    // Set override to 0 and render: viewport at top despite cursor at end.
    ta.set_scroll_override(Some(0));
    render_stateful(&ta, area, &mut buf, &mut state);
    assert_eq!(state.scroll, 0, "override should force scroll to 0");

    // Clear override: cursor-follow resumes.
    ta.set_scroll_override(None);
    render_stateful(&ta, area, &mut buf, &mut state);
    assert_eq!(
        state.scroll, normal_scroll,
        "clearing override should resume cursor-follow"
    );
}

#[test]
fn scroll_override_clamped_to_max() {
    // Override value larger than max_scroll should be clamped.
    let text = "line 0\nline 1\nline 2"; // 3 lines
    let mut ta = ta_with(text);
    ta.set_cursor(0);
    let area = Rect::new(0, 0, 40, 2); // 2 rows visible, max_scroll = 1
    let mut state = TextAreaState::default();
    let mut buf = Buffer::empty(area);

    ta.set_scroll_override(Some(999));
    render_stateful(&ta, area, &mut buf, &mut state);
    assert_eq!(state.scroll, 1, "override should be clamped to max_scroll");
}

#[test]
fn scroll_override_survives_render_cycles() {
    // The override should persist across multiple renders (unlike
    // mousewheel override which clears on cursor movement).
    let text = (0..20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut ta = ta_with(&text);
    ta.set_cursor(ta.text().len());
    let area = Rect::new(0, 0, 40, 5);
    let mut state = TextAreaState::default();
    let mut buf = Buffer::empty(area);

    ta.set_scroll_override(Some(3));
    for _ in 0..5 {
        render_stateful(&ta, area, &mut buf, &mut state);
        assert_eq!(state.scroll, 3, "override should persist across renders");
    }
}

#[test]
fn scroll_override_save_restore_round_trip() {
    // Simulates the collapsed-prompt pattern:
    // 1. Render normally (cursor-follow)
    // 2. Save state.scroll + scroll_override
    // 3. Override to 0, render collapsed
    // 4. Restore both → next render shows original viewport
    let text = (0..30)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut ta = ta_with(&text);
    ta.set_cursor(ta.text().len()); // cursor at end
    let area = Rect::new(0, 0, 40, 5);
    let mut state = TextAreaState::default();
    let mut buf = Buffer::empty(area);

    // 1. Initial render — cursor-follow scrolls to bottom.
    render_stateful(&ta, area, &mut buf, &mut state);
    let original_scroll = state.scroll;
    let original_override = ta.scroll_override();
    assert!(original_scroll > 0);
    assert_eq!(original_override, None);

    // 2. "Collapse": override to 0, render a few frames.
    ta.set_scroll_override(Some(0));
    for _ in 0..3 {
        render_stateful(&ta, area, &mut buf, &mut state);
        assert_eq!(state.scroll, 0);
    }

    // 3. Restore both.
    ta.set_scroll_override(original_override);
    state.scroll = original_scroll;

    // 4. Render "uncollapsed" — should show original position.
    render_stateful(&ta, area, &mut buf, &mut state);
    assert_eq!(
        state.scroll, original_scroll,
        "restored scroll should match original"
    );
}

#[test]
fn scroll_override_save_restore_with_mousewheel() {
    // Same as above but the user had mousewheel-scrolled away from cursor
    // before collapse. Both state.scroll and scroll_override must be
    // saved/restored for the viewport to return to its pre-collapse position.
    let text = (0..30)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut ta = ta_with(&text);
    ta.set_cursor(0); // cursor at start
    let area = Rect::new(0, 0, 40, 5);
    let mut state = TextAreaState::default();
    let mut buf = Buffer::empty(area);

    // Render at start, then mousewheel to scroll viewport away from cursor.
    render_stateful(&ta, area, &mut buf, &mut state);
    assert_eq!(state.scroll, 0);
    for _ in 0..5 {
        ta.handle_mouse(mouse_scroll_down(0, 0), area, state);
        render_stateful(&ta, area, &mut buf, &mut state);
    }
    let mousewheel_scroll = state.scroll;
    let mousewheel_override = ta.scroll_override();
    assert!(mousewheel_scroll > 0, "should have scrolled away");
    assert!(mousewheel_override.is_some(), "mousewheel sets override");

    // "Collapse": save both, override to 0.
    let saved_scroll = state.scroll;
    let saved_override = ta.scroll_override();
    ta.set_scroll_override(Some(0));
    for _ in 0..3 {
        render_stateful(&ta, area, &mut buf, &mut state);
        assert_eq!(state.scroll, 0);
    }

    // Restore both.
    ta.set_scroll_override(saved_override);
    state.scroll = saved_scroll;

    // Render "uncollapsed" — viewport should be at the mousewheel position,
    // NOT snapped to cursor (which is at line 0).
    render_stateful(&ta, area, &mut buf, &mut state);
    assert_eq!(
        state.scroll, mousewheel_scroll,
        "viewport should restore to mousewheel position, not snap to cursor"
    );
}

#[test]
fn shifted_character_classification_only_uppercases_letters() {
    for (input, expected) in [
        ('a', 'A'),
        ('z', 'Z'),
        ('A', 'A'),
        ('7', '7'),
        ('/', '/'),
        (';', ';'),
    ] {
        assert_eq!(
            classify_key_event(&KeyEvent::new(KeyCode::Char(input), KeyModifiers::SHIFT)),
            Some(EditCommand::Insert(expected))
        );
    }
}

#[test]
fn modified_delete_and_arrow_keys_keep_word_semantics() {
    for modifiers in [
        KeyModifiers::ALT,
        KeyModifiers::CONTROL,
        KeyModifiers::ALT | KeyModifiers::CONTROL,
    ] {
        assert_eq!(
            classify_key_event(&KeyEvent::new(KeyCode::Delete, modifiers)),
            Some(EditCommand::DeleteWordForward(WordStyle::Small)),
        );
        assert_eq!(
            classify_key_event(&KeyEvent::new(KeyCode::Left, modifiers)),
            Some(EditCommand::MoveWordLeft(WordStyle::Small)),
        );
        assert_eq!(
            classify_key_event(&KeyEvent::new(KeyCode::Right, modifiers)),
            Some(EditCommand::MoveWordRight(WordStyle::Small)),
        );
    }
    for modifiers in [KeyModifiers::ALT, KeyModifiers::SUPER] {
        assert_eq!(
            classify_key_event(&KeyEvent::new(KeyCode::Char('d'), modifiers)),
            Some(EditCommand::DeleteWordForward(WordStyle::Small)),
        );
    }
}

#[test]
fn modifier_keys_do_not_insert_text() {
    let mut t = ta_with("hello");
    let len = t.text().len();
    t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(t.text().len(), len);
}

#[test]
fn alt_word_nav_preserved() {
    let mut t = ta_with("hello world");
    t.set_cursor(t.text().len());
    let text = t.text().to_owned();
    t.input(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT));
    assert_eq!(t.text(), text);
    assert!(t.cursor() < text.len());

    t.set_cursor(0);
    t.input(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT));
    assert_eq!(t.text(), text);
    assert!(t.cursor() > 0);
}

#[test]
fn ctrl_alt_h_deletes_word() {
    let mut t = ta_with("hello world");
    t.set_cursor(t.text().len());
    t.input(KeyEvent::new(
        KeyCode::Char('h'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ));
    assert_eq!(t.text(), "hello ");
}

#[test]
fn plain_and_shifted_chars_insert() {
    let mut t = TextArea::new();
    for c in ['a', 'z', '1', '/', '@', '{', '!', '~'] {
        t.input(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(t.text(), "az1/@{!~");

    let mut t = TextArea::new();
    t.input(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SHIFT));
    t.input(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::SHIFT));
    assert_eq!(t.text(), "AZ");
}

#[test]
fn altgr_char_insertion_platform_dependent() {
    let mut t = TextArea::new();
    t.input(KeyEvent::new(
        KeyCode::Char('@'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ));
    if cfg!(target_os = "windows") {
        assert_eq!(t.text(), "@");
    } else {
        assert_eq!(t.text(), "");
    }
}

#[test]
fn shift_number_trusts_terminal_character() {
    // QWERTZ: terminal sends Char('/') + SHIFT for Shift+7.
    let mut t = TextArea::new();
    t.input(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::SHIFT));
    assert_eq!(t.text(), "/");
}

#[test]
fn shift_arrow_extends_selection_by_grapheme() {
    let mut t = ta_with("abc");
    t.set_cursor(0);
    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(0..1));
    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(0..2));
    // Reversing shrinks toward the sticky anchor.
    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(0..1));
}

#[test]
fn alt_shift_arrow_extends_selection_by_word() {
    let mut t = ta_with("hello-world tail");
    let hyphen = t.text().find('-').unwrap();
    let space = t.text().find(' ').unwrap();

    t.set_cursor(0);
    t.input(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(0..hyphen));
    t.input(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(0..hyphen + 1));

    // From the far side: word-extend left keeps its own anchor.
    t.clear_selection();
    t.set_cursor(space);
    t.input(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(hyphen + 1..space));
}

#[test]
fn super_shift_arrow_extends_selection_to_line_edges() {
    let mut t = ta_with("hello world");
    let mid = t.text().find(' ').unwrap();
    t.set_cursor(mid);
    t.input(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(mid..t.text().len()));
    // Extending to the other edge crosses the anchor.
    t.input(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(0..mid));
}

#[test]
fn shift_extension_anchor_sticky_across_granularities() {
    let mut t = ta_with_word_selected();
    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(0.."alpha ".len()));
}

/// `"alpha beta"` with `"alpha"` selected via Alt+Shift+Right from 0.
fn ta_with_word_selected() -> TextArea {
    let mut t = ta_with("alpha beta");
    t.set_cursor(0);
    t.input(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(0..5));
    t
}

#[test]
fn keyboard_selection_feeds_existing_selection_actions() {
    // Backspace deletes it, like a mouse selection would.
    let mut t = ta_with_word_selected();
    t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(t.text(), " beta");

    // Typing replaces it.
    let mut t = ta_with_word_selected();
    t.input(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(t.text(), "x beta");

    // A plain arrow collapses it to the corresponding edge.
    let mut t = ta_with_word_selected();
    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(t.selection_range(), None);
    assert_eq!(t.cursor(), 0);
}

#[test]
fn shift_up_down_extends_selection_by_line() {
    let mut t = ta_with("one\ntwo\nthree");
    let two = t.text().find("two").unwrap();
    t.set_cursor(two);
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    assert_eq!(
        t.selection_range(),
        Some(two..t.text().find("three").unwrap())
    );
    // Reversing crosses the anchor into the first line.
    t.input(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    t.input(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(0..two));
}

/// A stray bit alongside SUPER must not degrade the chord to a plain arrow.
#[test]
fn super_chords_tolerate_extra_modifier_bits() {
    let mut t = ta_with("hello world");
    let mid = t.text().find(' ').unwrap();
    t.set_cursor(mid);
    t.input(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SUPER | KeyModifiers::META,
    ));
    assert_eq!(t.cursor(), 0);
    t.input(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::SUPER | KeyModifiers::META,
    ));
    assert_eq!(t.cursor(), t.text().len());
}

#[test]
fn plain_up_down_collapse_selection_then_move_a_line() {
    let mut t = ta_with("one\ntwo\nthree");
    let two = t.text().find("two").unwrap();
    let three = t.text().find("three").unwrap();
    t.set_cursor(two);
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(two..three));

    // Down: collapse to the end edge, then one line further.
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(t.selection_range(), None);
    assert!(t.cursor() > three, "moved a line past the end edge");

    // Up: collapse to the start edge, then one line up.
    t.set_cursor(two);
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    t.input(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(t.selection_range(), None);
    assert!(t.cursor() < two, "moved a line above the start edge");
}

#[test]
fn cmd_c_copies_selection_and_keeps_it() {
    let mut t = ta_with_word_selected();
    t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
    assert_eq!(t.take_clipboard().as_deref(), Some("alpha"));
    assert_eq!(
        t.selection_range(),
        Some(0..5),
        "highlight survives the copy"
    );
    assert_eq!(t.text(), "alpha beta");
}

#[test]
fn cmd_x_cuts_selection() {
    let mut t = ta_with_word_selected();
    t.input(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::SUPER));
    assert_eq!(t.take_clipboard().as_deref(), Some("alpha"));
    assert_eq!(t.text(), " beta");
    assert_eq!(t.selection_range(), None);
}

#[test]
fn cmd_c_without_selection_is_inert() {
    let mut t = ta_with("alpha beta");
    t.set_cursor(3);
    t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
    assert_eq!(t.take_clipboard(), None);
    assert_eq!(t.text(), "alpha beta");
    assert_eq!(t.cursor(), 3);
}

/// Copy over a chip-spanning highlight yields the chip's raw text.
#[test]
fn cmd_c_copies_chip_raw_text() {
    let mut t = TextArea::new();
    t.insert_str("ab");
    t.insert_element("element_text", ElementKind(0), None);
    t.insert_str("cd");
    t.set_selection(0, t.text().len());
    t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
    assert_eq!(t.take_clipboard().as_deref(), Some("abelement_textcd"));
    assert!(t.selection_range().is_some());
}

/// Word moves over a highlight collapse to the edge FIRST, then move a word.
#[test]
fn word_move_collapses_leftward_selection_first() {
    let mut t = ta_with("abc abc abc");
    t.set_cursor(4);
    t.input(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(0..4));

    // Right edge (4), then one word right → end of the second word.
    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(t.cursor(), 7);
    assert_eq!(t.selection_range(), None);

    t.set_cursor(4);
    t.input(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    // Left edge (0), then one word left → stays at 0.
    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(t.cursor(), 0);
    assert_eq!(t.selection_range(), None);
}

#[test]
fn word_move_collapses_rightward_selection_first() {
    let mut t = ta_with("abc abc abc");
    t.set_cursor(4);
    t.input(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(4..7));

    // Left edge (4), then one word left → 0 — NOT word-left from the
    // head at 7 (which would land back on 4).
    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(t.cursor(), 0);

    t.set_cursor(4);
    t.input(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    // Right edge (7), then one word right → end of the third word.
    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(t.cursor(), 11);
}

#[test]
fn super_arrow_collapses_multiline_selection_first() {
    let mut t = ta_with("one two\nthree four");
    let two = t.text().find("two").unwrap();
    t.set_cursor(two);
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    let range = t.selection_range().expect("selection spans lines");
    assert!(t.text()[range].contains('\n'));

    // Cmd+Left: line start of the START edge's line, not the head's.
    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::SUPER));
    assert_eq!(t.cursor(), 0);
    assert_eq!(t.selection_range(), None);
}

#[test]
fn ctrl_f_collapses_like_plain_arrow() {
    let mut t = ta_with_word_selected();
    t.input(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert_eq!(t.cursor(), 5, "collapse to the end edge, no extra move");
    assert_eq!(t.selection_range(), None);
}

/// Plain arrows over a leftward selection collapse to the edges, no extra move.
#[test]
fn plain_arrows_collapse_leftward_selection_to_edges() {
    let mut t = ta_with("hello world");
    let start = "hello ".len();
    t.set_cursor(start + 2);
    t.input(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(0..start + 2));

    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(t.cursor(), start + 2, "Right collapses to the anchor");
    assert_eq!(t.selection_range(), None);

    t.input(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ));
    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(t.cursor(), 0, "Left collapses to the head");
    assert_eq!(t.selection_range(), None);
    assert_eq!(t.text(), "hello world", "collapse never edits");
}

/// Shift+arrows extend a mouse selection from the HEAD, not the parked cursor.
#[test]
fn double_click_then_shift_arrows_extend_from_the_head() {
    let mut t = ta_with("hello world");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();
    t.handle_mouse(mouse_down(2, 0), area, state);
    t.handle_mouse(mouse_down(2, 0), area, state);
    assert_eq!(t.selection_range(), Some(0..5));
    assert_eq!(t.cursor(), 4, "cursor on last char, one before the head");

    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(0..6), "extends past the head");
    t.input(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(0..5), "shrinks back to the head");
}

/// Vertical extends after a triple-click move from the head, not the click position.
#[test]
fn triple_click_then_shift_vertical_extends_from_the_head() {
    let mut t = ta_with("one\ntwo\nthree");
    let area = Rect::new(0, 0, 40, 5);
    let state = TextAreaState::default();
    // Triple-click "two" (row 1) → selects "two\n" (4..8), cursor at 5.
    for _ in 0..3 {
        t.handle_mouse(mouse_down(1, 1), area, state);
    }
    assert_eq!(t.selection_range(), Some(4..8));
    assert_eq!(t.cursor(), 5, "cursor stays at the click position");

    // Down from the head (start of "three") reaches the buffer end.
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(4..t.text().len()));

    // And Up from a fresh triple-click returns the head to the anchor,
    // emptying the selection (browser semantics).
    for _ in 0..3 {
        t.handle_mouse(mouse_down(1, 1), area, state);
    }
    t.input(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), None);
}

/// Shift+Home/End extend like every text field.
#[test]
fn shift_home_end_extend_selection() {
    let mut t = ta_with("hello world");
    let mid = t.text().find(' ').unwrap();
    t.set_cursor(mid);
    t.input(KeyEvent::new(KeyCode::End, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(mid..t.text().len()));
    // Crossing the anchor to the row start.
    t.input(KeyEvent::new(KeyCode::Home, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(0..mid));
}

/// Home/End with a selection collapse to the edge first, like Cmd+Left/Right.
#[test]
fn home_end_collapse_to_selection_edge_before_moving() {
    let mut t = ta_with("one\ntwo\nthree");
    let w = t.text().find('w').unwrap();
    t.set_cursor(w);
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    let head = t.selection_range().unwrap().end;
    assert!(head > w);

    t.input(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(t.selection_range(), None);
    assert_eq!(t.cursor(), t.text().len(), "end of the END edge's line");

    t.set_cursor(w);
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    t.input(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(t.cursor(), w - 1, "start of the START edge's line");
}

/// Kill chords over a highlight delete just the selection and stash it for yank.
#[test]
fn kill_chords_delete_only_the_selection_and_stash_for_yank() {
    let mut t = ta_with_word_selected();
    t.input(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), " beta", "Ctrl+K deletes the selection only");
    t.input(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), "alpha beta", "yank restores what was killed");

    let mut t = ta_with_word_selected();
    t.input(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), " beta", "Ctrl+W deletes the selection only");
}

/// Yank over a highlight replaces it as one undo step.
#[test]
fn ctrl_y_yank_replaces_selection() {
    let mut t = ta_with("alpha beta");
    t.set_cursor(5);
    t.input(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), "alpha", "Ctrl+K stashed \" beta\"");
    t.set_cursor(5);
    t.set_selection(0, 5);
    t.input(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), " beta", "yank replaced the selection");
    assert_eq!(t.selection_range(), None);
    t.undo();
    assert_eq!(t.text(), "alpha", "replace is a single undo step");
}

/// Internal-clipboard paste (Ctrl+V) over a highlight replaces it.
#[test]
fn ctrl_v_paste_replaces_selection() {
    let mut t = ta_with("hello world");
    t.set_clipboard_text("X".to_owned());
    t.set_cursor(t.text().len());
    t.set_selection(6, t.text().len());
    t.input(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert_eq!(t.text(), "hello X");
    assert_eq!(t.selection_range(), None);
}

/// Shift-extended emacs line chords (Cocoa supports these).
#[test]
fn ctrl_shift_a_e_extend_to_logical_line_edges() {
    let mut t = ta_with("hello world");
    let mid = t.text().find(' ').unwrap();
    t.set_cursor(mid);
    t.input(KeyEvent::new(
        KeyCode::Char('e'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(mid..t.text().len()));
    t.input(KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(0..mid));
}

/// Windows Win32 input reports shifted letters uppercase (`Char('E')` +
/// CTRL|SHIFT); the intercept folds case so emacs extends work there.
#[test]
fn ctrl_shift_uppercase_letter_extends_like_lowercase() {
    let mut t = ta_with("hello world");
    let mid = t.text().find(' ').unwrap();
    t.set_cursor(mid);
    t.input(KeyEvent::new(
        KeyCode::Char('E'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(t.selection_range(), Some(mid..t.text().len()));
    // Plain typed capitals are untouched: Shift+X still inserts 'X'.
    let mut t = ta_with("");
    t.input(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT));
    assert_eq!(t.text(), "X");
}

/// Ctrl+P/N are movement rows in the shared table.
#[test]
fn ctrl_p_n_move_by_visual_row() {
    let mut t = ta_with("one\ntwo");
    t.set_cursor(0);
    t.input(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert_eq!(t.cursor(), t.text().find("two").unwrap());
    t.input(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    assert_eq!(t.cursor(), 0);
}

/// Extending over a chip hops it atomically.
#[test]
fn shift_right_extends_across_a_chip_atomically() {
    let mut t = TextArea::new();
    t.insert_str("ab");
    t.insert_element("element_text", ElementKind(0), None);
    t.insert_str("cd");
    t.set_cursor(2);
    t.input(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    assert_eq!(t.selected_text().as_deref(), Some("element_text"));
}

/// The collapse path shares the Super arms' stray-bit tolerance.
#[test]
fn super_chords_with_stray_bits_collapse_to_edge_then_move() {
    let mut t = ta_with("hello world");
    t.set_cursor(8);
    t.set_selection(3, 8);
    t.input(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::SUPER | KeyModifiers::META,
    ));
    assert_eq!(t.selection_range(), None);
    assert_eq!(t.cursor(), 0, "line start from the START edge");
}

/// Vertical extends at the buffer edges degrade to extend-to-0/len.
#[test]
fn shift_vertical_at_buffer_edges_extends_to_bounds() {
    let mut t = ta_with("one\ntwo");
    t.set_cursor(1);
    t.input(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(0..1));

    let last = t.text().len() - 1;
    t.clear_selection();
    t.set_cursor(last);
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    assert_eq!(t.selection_range(), Some(last..t.text().len()));
}

/// A plain vertical collapse keeps the sticky column from the extend,
/// so the caret continues down the column the user was tracking.
#[test]
fn plain_down_after_extend_keeps_preferred_col() {
    let mut t = ta_with("longline\nab\nlongline");
    t.set_cursor(6);
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let third_line = t.text().rfind('\n').unwrap() + 1;
    assert_eq!(t.selection_range(), None);
    assert_eq!(
        t.cursor(),
        third_line + 6,
        "column 6 restored past the 2-char line"
    );
}

/// `preferred_col` survives a vertical extend through a short line.
#[test]
fn shift_down_zigzag_keeps_preferred_col() {
    let mut t = ta_with("longline\nab\nlongline");
    t.set_cursor(6);
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    t.input(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    let third_line = t.text().rfind('\n').unwrap() + 1;
    assert_eq!(
        t.selection_range(),
        Some(6..third_line + 6),
        "column 6 restored after passing the 2-char line"
    );
}

/// Cmd+C on a zero-width selection copies nothing and drops the stale selection.
#[test]
fn cmd_c_on_zero_width_selection_clears_it() {
    let mut t = ta_with("hello");
    t.set_selection(3, 3);
    t.input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
    assert!(t.selection.is_none());
    assert_eq!(t.take_clipboard(), None);
}
