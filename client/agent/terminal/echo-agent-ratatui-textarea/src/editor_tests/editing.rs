use unicode_segmentation::UnicodeSegmentation as _;

use super::super::*;
use super::{delta, is_extended_grapheme_boundary};

#[test]
fn edit_outcome_is_closed_over_cursor_and_text_changes() {
    let mut buffer = EditBuffer::new();
    assert_eq!(
        buffer.apply(EditCommand::MoveGraphemeLeft),
        EditOutcome::Unchanged
    );

    assert_eq!(
        buffer.apply(EditCommand::Insert('é')),
        EditOutcome::TextAndCursor(delta(0..0, 0.."é".len()))
    );
    assert_eq!(buffer.text(), "é");
    assert_eq!(buffer.cursor_byte(), "é".len());

    assert_eq!(
        buffer.apply(EditCommand::MoveGraphemeLeft),
        EditOutcome::CursorOnly
    );
    assert_eq!(buffer.cursor_byte(), 0);

    assert_eq!(
        buffer.apply(EditCommand::DeleteGraphemeForward),
        EditOutcome::TextOnly(delta(0.."é".len(), 0..0))
    );
    assert_eq!(buffer.text(), "");
}

#[test]
fn grapheme_motion_treats_combining_zwj_flags_and_cjk_atomically() {
    let graphemes = ["e\u{301}", "👩🏽\u{200d}💻", "🇺🇸", "界"];
    let text = graphemes.concat();
    let mut boundaries = vec![0];
    for grapheme in graphemes {
        boundaries.push(boundaries.last().copied().unwrap_or(0) + grapheme.len());
    }

    let mut buffer = EditBuffer::from_parts(text, usize::MAX);
    for expected in boundaries.iter().rev().skip(1) {
        let _ = buffer.apply(EditCommand::MoveGraphemeLeft);
        assert_eq!(buffer.cursor_byte(), *expected);
    }
    for expected in boundaries.iter().skip(1) {
        let _ = buffer.apply(EditCommand::MoveGraphemeRight);
        assert_eq!(buffer.cursor_byte(), *expected);
    }
}

#[test]
fn grapheme_deletion_and_replacement_never_split_clusters() {
    let combining = "e\u{301}";
    let zwj = "👩🏽\u{200d}💻";
    let flag = "🇺🇸";
    let text = format!("{combining}{zwj}{flag}");
    let mut buffer = EditBuffer::from_parts(text.as_str(), combining.len() + zwj.len());

    let _ = buffer.apply(EditCommand::DeleteGraphemeBackward);
    let expected = format!("{combining}{flag}");
    assert_eq!(buffer.text(), expected.as_str());
    assert_eq!(buffer.cursor_byte(), combining.len());

    let _ = buffer.apply(EditCommand::DeleteGraphemeForward);
    assert_eq!(buffer.text(), combining);
    assert_eq!(buffer.cursor_byte(), combining.len());

    let text = format!("a{zwj}b");
    let mut buffer = EditBuffer::from_parts(text.as_str(), text.len());
    let outcome = buffer.replace_byte_range(2..(1 + zwj.len() - 1), "X");
    assert_eq!(buffer.text(), "aXb");
    assert_eq!(buffer.cursor_byte(), 3);
    assert_eq!(
        outcome,
        EditOutcome::TextAndCursor(delta(1..(1 + zwj.len()), 1..2))
    );

    let mut combining_insert = EditBuffer::from_text("e");
    let _ = combining_insert.insert_str("\u{301}");
    assert_eq!(combining_insert.text(), combining);
    let _ = combining_insert.apply(EditCommand::DeleteGraphemeBackward);
    assert_eq!(combining_insert.text(), "");

    let base = "👩🏽";
    let laptop = "💻";
    let text = format!("{base}{laptop}");
    let mut zwj_insert = EditBuffer::from_parts(text, base.len());
    let _ = zwj_insert.insert_str("\u{200d}");
    assert_eq!(zwj_insert.text(), zwj);
    assert_eq!(zwj_insert.cursor_byte(), zwj.len());
}

#[test]
fn edit_created_grapheme_merges_keep_right_cursor_affinity() {
    let woman = "👩";
    let tail = "👩🏽\u{200d}💻";
    let text = format!("{woman}{tail}");
    let mut zwj_insert = EditBuffer::from_parts(text, woman.len());
    let outcome = zwj_insert.insert_str("\u{200d}");
    let inserted_end = woman.len() + "\u{200d}".len();
    assert_eq!(zwj_insert.text().graphemes(true).count(), 1);
    assert_eq!(zwj_insert.cursor_byte(), zwj_insert.text().len());
    assert_eq!(
        outcome,
        EditOutcome::TextAndCursor(delta(woman.len()..woman.len(), woman.len()..inserted_end,))
    );

    let mut flag_insert = EditBuffer::from_parts("🇺", 0);
    let outcome = flag_insert.insert_str("🇨");
    assert_eq!(flag_insert.text(), "🇨🇺");
    assert_eq!(flag_insert.cursor_byte(), flag_insert.text().len());
    assert_eq!(
        outcome,
        EditOutcome::TextAndCursor(delta(0..0, 0.."🇨".len()))
    );

    let mut flag_replace = EditBuffer::from_parts("x🇺", 0);
    let outcome = flag_replace.replace_byte_range(0..1, "🇨");
    assert_eq!(flag_replace.text(), "🇨🇺");
    assert_eq!(flag_replace.cursor_byte(), flag_replace.text().len());
    assert_eq!(
        outcome,
        EditOutcome::TextAndCursor(delta(0..1, 0.."🇨".len()))
    );

    let regional_indicator_len = "🇨".len();
    let mut flag_delete = EditBuffer::from_parts("🇨x🇺", regional_indicator_len + "x".len());
    let outcome = flag_delete.apply(EditCommand::DeleteGraphemeBackward);
    assert_eq!(flag_delete.text(), "🇨🇺");
    assert_eq!(flag_delete.cursor_byte(), flag_delete.text().len());
    assert_eq!(
        outcome,
        EditOutcome::TextAndCursor(delta(
            regional_indicator_len..(regional_indicator_len + "x".len()),
            regional_indicator_len..regional_indicator_len,
        ))
    );
}

#[test]
fn invalid_cursor_bytes_normalize_to_the_nearest_grapheme_boundary() {
    let mut buffer = EditBuffer::from_parts("e\u{301}x", 1);
    assert_eq!(buffer.cursor_byte(), 0);

    let outcome = buffer.set_cursor_byte(2);
    assert_eq!(buffer.cursor_byte(), "e\u{301}".len());
    assert_eq!(outcome, EditOutcome::CursorOnly);

    let _ = buffer.set_cursor_byte(usize::MAX);
    assert_eq!(buffer.cursor_byte(), buffer.text().len());
    assert!(is_extended_grapheme_boundary(
        buffer.text(),
        buffer.cursor_byte()
    ));

    let tied = EditBuffer::from_parts("🇨🇺", "🇨".len());
    assert_eq!(tied.cursor_byte(), 0);
}

#[test]
fn range_replacement_tracks_a_cursor_before_inside_or_after_the_edit() {
    let mut before = EditBuffer::from_parts("alpha beta", 1);
    let outcome = before.replace_byte_range(6..10, "B");
    assert_eq!(before.text(), "alpha B");
    assert_eq!(before.cursor_byte(), 1);
    assert_eq!(outcome, EditOutcome::TextOnly(delta(6..10, 6..7)));

    let mut inside = EditBuffer::from_parts("alpha beta", 8);
    let outcome = inside.replace_byte_range(6..10, "B");
    assert_eq!(inside.cursor_byte(), 7);
    assert_eq!(outcome, EditOutcome::TextAndCursor(delta(6..10, 6..7)));

    let mut after = EditBuffer::from_parts("alpha beta", 10);
    let outcome = after.replace_byte_range(0..5, "A");
    assert_eq!(after.text(), "A beta");
    assert_eq!(after.cursor_byte(), 6);
    assert_eq!(outcome, EditOutcome::TextAndCursor(delta(0..5, 0..1)));
}

#[test]
fn small_words_keep_textarea_punctuation_classes() {
    let mut buffer = EditBuffer::from_parts("hello-world", 0);
    for expected in [5, 6, 11] {
        let _ = buffer.apply(EditCommand::MoveWordRight(WordStyle::Small));
        assert_eq!(buffer.cursor_byte(), expected);
    }
    for expected in [6, 5, 0] {
        let _ = buffer.apply(EditCommand::MoveWordLeft(WordStyle::Small));
        assert_eq!(buffer.cursor_byte(), expected);
    }

    let mut buffer = EditBuffer::from_text("hello-world");
    let _ = buffer.apply(EditCommand::DeleteWordBackward(WordStyle::Small));
    assert_eq!(buffer.text(), "hello-");

    let mut buffer = EditBuffer::from_parts("hello-world", 0);
    let _ = buffer.apply(EditCommand::DeleteWordForward(WordStyle::Small));
    assert_eq!(buffer.text(), "-world");

    let mut buffer = EditBuffer::from_parts("hello-world", 0);
    let _ = buffer.apply(EditCommand::MoveWordRight(WordStyle::WhitespaceDelimited));
    assert_eq!(buffer.cursor_byte(), buffer.text().len());
}

#[test]
fn logical_line_commands_chain_at_line_boundaries() {
    let mut buffer = EditBuffer::from_parts("one\ntwo\nthree", 6);
    let _ = buffer.apply(EditCommand::MoveLogicalLineStart);
    assert_eq!(buffer.cursor_byte(), 4);
    let _ = buffer.apply(EditCommand::MoveLogicalLineEnd);
    assert_eq!(buffer.cursor_byte(), 7);

    let _ = buffer.set_cursor_byte(4);
    let _ = buffer.apply(EditCommand::MoveLogicalLineStart);
    assert_eq!(buffer.cursor_byte(), 0);

    let _ = buffer.set_cursor_byte(3);
    let _ = buffer.apply(EditCommand::MoveLogicalLineEnd);
    assert_eq!(buffer.cursor_byte(), 7);

    let _ = buffer.set_cursor_byte(6);
    let _ = buffer.apply(EditCommand::DeleteToLineStart);
    assert_eq!(buffer.text(), "one\no\nthree");
    assert_eq!(buffer.cursor_byte(), 4);

    let _ = buffer.apply(EditCommand::DeleteToLineStart);
    assert_eq!(buffer.text(), "oneo\nthree");
    assert_eq!(buffer.cursor_byte(), 3);

    let mut buffer = EditBuffer::from_parts("one\ntwo\nthree", 7);
    let _ = buffer.apply(EditCommand::DeleteToLineEnd);
    assert_eq!(buffer.text(), "one\ntwothree");
    assert_eq!(buffer.cursor_byte(), 7);
}

#[test]
fn crlf_line_motion_and_deletion_keep_the_line_ending_atomic() {
    let mut motion = EditBuffer::from_parts("ab\r\ncd", 1);
    let _ = motion.apply(EditCommand::MoveLogicalLineEnd);
    assert_eq!(motion.cursor_byte(), 2);
    let _ = motion.apply(EditCommand::MoveLogicalLineEnd);
    assert_eq!(motion.cursor_byte(), 6);
    let _ = motion.set_cursor_byte(4);
    let _ = motion.apply(EditCommand::MoveLogicalLineStart);
    assert_eq!(motion.cursor_byte(), 0);

    let mut midline = EditBuffer::from_parts("ab\r\ncd", 1);
    let outcome = midline.apply(EditCommand::DeleteToLineEnd);
    assert_eq!(midline.text(), "a\r\ncd");
    assert_eq!(midline.cursor_byte(), 1);
    assert_eq!(outcome, EditOutcome::TextOnly(delta(1..2, 1..1)));

    let mut at_eol = EditBuffer::from_parts("ab\r\ncd", 2);
    let outcome = at_eol.apply(EditCommand::DeleteToLineEnd);
    assert_eq!(at_eol.text(), "abcd");
    assert_eq!(at_eol.cursor_byte(), 2);
    assert_eq!(outcome, EditOutcome::TextOnly(delta(2..4, 2..2)));

    let mut on_lf = EditBuffer::from_parts("ab\r\ncd", 3);
    let outcome = on_lf.apply(EditCommand::DeleteToLineEnd);
    assert_eq!(on_lf.text(), "abcd");
    assert_eq!(outcome, EditOutcome::TextOnly(delta(2..4, 2..2)));

    let mut second_line = EditBuffer::from_parts("ab\r\ncd", 5);
    let outcome = second_line.apply(EditCommand::DeleteToLineStart);
    assert_eq!(second_line.text(), "ab\r\nd");
    assert_eq!(second_line.cursor_byte(), 4);
    assert_eq!(outcome, EditOutcome::TextAndCursor(delta(4..5, 4..4)));

    let mut at_bol = EditBuffer::from_parts("ab\r\ncd", 4);
    let outcome = at_bol.apply(EditCommand::DeleteToLineStart);
    assert_eq!(at_bol.text(), "abcd");
    assert_eq!(at_bol.cursor_byte(), 2);
    assert_eq!(outcome, EditOutcome::TextAndCursor(delta(2..4, 2..2)));
}
