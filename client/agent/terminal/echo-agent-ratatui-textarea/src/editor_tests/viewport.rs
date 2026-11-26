use rand::{Rng as _, SeedableRng as _};
use unicode_width::UnicodeWidthStr as _;

use super::super::*;
use super::is_extended_grapheme_boundary;

fn assert_viewport_invariants(
    buffer: &EditBuffer,
    viewport: &SingleLineViewport,
    display_width: usize,
) {
    let visible = &buffer.text()[viewport.visible_byte_range.clone()];
    let prefix = &buffer.text()[viewport.visible_byte_range.start..buffer.cursor_byte()];
    assert!(!visible.contains('\n'));
    assert!(!visible.contains('\r'));
    assert!(visible.width() <= display_width);
    assert_eq!(prefix.width(), viewport.cursor_display_column);
    if display_width == 0 {
        assert_eq!(viewport.cursor_display_column, 0);
    } else {
        assert!(viewport.cursor_display_column < display_width);
    }
}

#[test]
fn single_line_viewport_clips_only_at_grapheme_boundaries() {
    let zwj = "👩🏽\u{200d}💻";
    let flag = "🇺🇸";
    let text = format!("a{zwj}b{flag}界");
    let after_b = 1 + zwj.len() + 1;
    let mut buffer = EditBuffer::from_parts(text.as_str(), after_b);

    let viewport = buffer.single_line_viewport(4);
    let expected = format!("{zwj}b");
    assert_eq!(
        &buffer.text()[viewport.visible_byte_range.clone()],
        expected.as_str()
    );
    assert_eq!(viewport.cursor_display_column, 3);
    assert_viewport_invariants(&buffer, &viewport, 4);

    let _ = buffer.set_cursor_byte(0);
    let viewport = buffer.single_line_viewport(4);
    let expected = format!("a{zwj}b");
    assert_eq!(
        &buffer.text()[viewport.visible_byte_range.clone()],
        expected.as_str()
    );
    assert_eq!(viewport.cursor_display_column, 0);
    assert_viewport_invariants(&buffer, &viewport, 4);

    let _ = buffer.set_cursor_byte(buffer.text().len());
    let viewport = buffer.single_line_viewport(4);
    assert_eq!(&buffer.text()[viewport.visible_byte_range.clone()], "界");
    assert_eq!(viewport.cursor_display_column, 2);
    assert_viewport_invariants(&buffer, &viewport, 4);

    let viewport = buffer.single_line_viewport(0);
    assert_eq!(
        viewport.visible_byte_range,
        buffer.cursor_byte()..buffer.cursor_byte()
    );
    assert_eq!(viewport.cursor_display_column, 0);
    assert_viewport_invariants(&buffer, &viewport, 0);

    let combining = EditBuffer::from_parts("e\u{301}x", 0);
    let viewport = combining.single_line_viewport(1);
    assert_eq!(
        &combining.text()[viewport.visible_byte_range.clone()],
        "e\u{301}"
    );
    assert_viewport_invariants(&combining, &viewport, 1);

    let narrow_zwj = EditBuffer::from_parts(zwj, 0);
    let viewport = narrow_zwj.single_line_viewport(1);
    assert!(viewport.visible_byte_range.is_empty());
    assert_viewport_invariants(&narrow_zwj, &viewport, 1);

    let zero_width = "\u{200b}";
    assert_eq!(zero_width.width(), 0);
    let text = format!("a{zero_width}b");
    let zero_width_buffer = EditBuffer::from_parts(text.as_str(), 1 + zero_width.len());
    let viewport = zero_width_buffer.single_line_viewport(2);
    assert_eq!(
        &zero_width_buffer.text()[viewport.visible_byte_range.clone()],
        text.as_str()
    );
    assert_eq!(viewport.cursor_display_column, 1);
    assert_viewport_invariants(&zero_width_buffer, &viewport, 2);
}

#[test]
fn single_line_viewport_stays_within_lf_and_crlf_logical_lines() {
    for (text, cursor_byte, expected) in [
        ("a\nb", "a\nb".len(), "b"),
        ("ab\r\ncd", "ab\r\ncd".len(), "cd"),
        ("ab\r\ncd", 2, "ab"),
    ] {
        let buffer = EditBuffer::from_parts(text, cursor_byte);
        let viewport = buffer.single_line_viewport(4);
        assert_eq!(
            &buffer.text()[viewport.visible_byte_range.clone()],
            expected
        );
        assert_viewport_invariants(&buffer, &viewport, 4);
    }
}

#[test]
fn atomic_line_break_stays_inside_single_line_viewport() {
    let text = "aaX\nYbb";
    let cursor = text.find('Y').expect("Y") + 1;
    let buffer = EditBuffer::from_parts(text, cursor);
    let atomic = text.find('X').expect("X")..text.find('b').expect("b");

    let physical = buffer.single_line_viewport(16);
    assert_eq!(&buffer.text()[physical.visible_byte_range], "Ybb");

    let logical = buffer.single_line_viewport_with_atomic_ranges(16, &[atomic]);
    assert_eq!(&buffer.text()[logical.visible_byte_range], text);
    assert_eq!(logical.cursor_display_column, 5);
}

#[test]
fn atomic_viewport_preserves_raw_cursor_and_whole_spans() {
    let text = "aaTOKENbb";
    let atom = 2..7;
    let buffer = EditBuffer {
        text: text.to_string(),
        cursor_byte: 5,
        ..EditBuffer::default()
    };

    let viewport = buffer.single_line_viewport_with_atomic_ranges(4, std::slice::from_ref(&atom));
    assert!(viewport.visible_byte_range.start <= buffer.cursor_byte());
    assert!(buffer.cursor_byte() <= viewport.visible_byte_range.end);
    assert!(
        viewport.visible_byte_range.is_empty()
            || viewport.visible_byte_range.start <= atom.start
            || viewport.visible_byte_range.start >= atom.end
    );
    assert!(
        viewport.visible_byte_range.is_empty()
            || viewport.visible_byte_range.end <= atom.start
            || viewport.visible_byte_range.end >= atom.end
    );
    assert!(viewport.visible_byte_range.start <= buffer.cursor_byte());
    assert!(buffer.cursor_byte() <= viewport.visible_byte_range.end);
}

#[test]
fn fixed_seed_edit_sequence_preserves_cursor_and_viewport_invariants() {
    let atoms = [
        "",
        "a",
        "_",
        "-",
        " ",
        "\n",
        "\r\n",
        "\u{200b}",
        "e\u{301}",
        "👩🏽\u{200d}💻",
        "🇺🇸",
        "界",
    ];
    let mut rng = rand::rngs::StdRng::seed_from_u64(0x5eed_ed17);
    let mut buffer = EditBuffer::new();

    for _ in 0..2_000 {
        match rng.random_range(0..13) {
            0 => {
                let atom = atoms[rng.random_range(0..atoms.len())];
                let _ = buffer.insert_str(atom);
            }
            1 => {
                let _ = buffer.apply(EditCommand::MoveGraphemeLeft);
            }
            2 => {
                let _ = buffer.apply(EditCommand::MoveGraphemeRight);
            }
            3 => {
                let _ = buffer.apply(EditCommand::DeleteGraphemeBackward);
            }
            4 => {
                let _ = buffer.apply(EditCommand::DeleteGraphemeForward);
            }
            5 => {
                let _ = buffer.apply(EditCommand::DeleteWordBackward(WordStyle::Small));
            }
            6 => {
                let _ = buffer.apply(EditCommand::DeleteWordBackward(
                    WordStyle::WhitespaceDelimited,
                ));
            }
            7 => {
                let _ = buffer.apply(EditCommand::DeleteWordForward(WordStyle::Small));
            }
            8 => {
                let _ = buffer.apply(EditCommand::MoveWordLeft(WordStyle::Small));
            }
            9 => {
                let _ = buffer.apply(EditCommand::MoveWordRight(WordStyle::Small));
            }
            10 => {
                let byte = rng.random_range(0..=buffer.text().len().saturating_add(3));
                let _ = buffer.set_cursor_byte(byte);
            }
            11 => {
                let max = buffer.text().len().saturating_add(2);
                let start = rng.random_range(0..=max);
                let end = rng.random_range(0..=max);
                let replacement = atoms[rng.random_range(0..atoms.len())];
                let _ = buffer.replace_byte_range(start..end, replacement);
            }
            12 => {
                let command = if rng.random() {
                    EditCommand::MoveLogicalLineStart
                } else {
                    EditCommand::MoveLogicalLineEnd
                };
                let _ = buffer.apply(command);
            }
            _ => unreachable!(),
        }

        assert!(buffer.cursor_byte() <= buffer.text().len());
        assert!(is_extended_grapheme_boundary(
            buffer.text(),
            buffer.cursor_byte()
        ));

        let width = rng.random_range(0..8);
        let viewport = buffer.single_line_viewport(width);
        assert!(viewport.visible_byte_range.start <= buffer.cursor_byte());
        assert!(buffer.cursor_byte() <= viewport.visible_byte_range.end);
        assert!(is_extended_grapheme_boundary(
            buffer.text(),
            viewport.visible_byte_range.start
        ));
        assert!(is_extended_grapheme_boundary(
            buffer.text(),
            viewport.visible_byte_range.end
        ));
        assert_viewport_invariants(&buffer, &viewport, width);
    }
}
