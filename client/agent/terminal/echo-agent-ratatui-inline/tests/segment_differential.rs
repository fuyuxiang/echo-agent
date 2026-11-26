//! Differential test: the anstyle-parse-based `split_into_line_segments`
//! against a reference copy of the previous termwiz-based implementation.
//!
//! Production code uses `anstyle-parse` for ANSI line splitting. This test
//! embeds the prior termwiz-based splitter as a reference and asserts
//! identical observable output so the rewrite cannot drift. `termwiz` is a
//! dev-dependency only (powers this test; not linked into shipped binaries).
//!
//! Inputs are restricted to what the production splitter actually sees:
//! complete escape sequences (the old implementation `debug_assert`ed on
//! trailing incomplete ones) and no raw C1 controls encoded as UTF-8 (termwiz
//! maps e.g. U+0085 to a control action while VTE prints it; that corner was
//! unspecified before and is not exercised by terminal output we render).

use echo_agent_ratatui_inline::split_into_line_segments;

// ─── Reference: the previous termwiz-based implementation, verbatim ────────

struct RefSegment<'a> {
    content: &'a str,
    ends_with_crlf: bool,
}

fn reference_split<'a>(input: &'a str, term_width: usize) -> Vec<RefSegment<'a>> {
    use termwiz::escape::{Action, ControlCode, parser::Parser};
    use unicode_width::UnicodeWidthChar as _;

    let mut parser = Parser::new();
    let mut remaining_bytes = input.as_bytes();

    let mut segments = Vec::<RefSegment>::new();
    let mut segment_start = 0_usize;
    let mut segment_end = 0_usize;
    let mut visual_width = 0_usize;
    let mut has_visual = false;
    let mut prev_is_cr = false;

    macro_rules! push_segment {
        ($end:expr, $crlf:expr) => {
            #[allow(unused_assignments)]
            {
                segments.push(RefSegment {
                    content: &input[segment_start..$end],
                    ends_with_crlf: $crlf,
                });
                visual_width = 0;
                has_visual = false;
            }
        };
    }

    while let Some((ansi_action, consumed)) = parser.parse_first(remaining_bytes) {
        remaining_bytes = &remaining_bytes[consumed..];
        let mut is_cr = false;

        match ansi_action {
            Action::Control(ControlCode::LineFeed) => {
                push_segment!(segment_end - usize::from(prev_is_cr), true);
                segment_end += consumed;
                segment_start = segment_end;
            }
            Action::Control(ControlCode::CarriageReturn) => {
                segment_end += consumed;
                visual_width = 0;
                is_cr = true;
            }
            Action::Print(ch) => {
                let char_width = ch.width().unwrap_or(0);
                let new_width = visual_width + char_width;
                if new_width > term_width && has_visual {
                    push_segment!(segment_end, false);
                    segment_start = segment_end;
                    segment_end += consumed;
                    visual_width = char_width;
                    has_visual = true;
                    if char_width > term_width {
                        push_segment!(segment_end, false);
                        segment_start = segment_end;
                    }
                } else {
                    segment_end += consumed;
                    visual_width = new_width;
                    has_visual = true;
                }
            }
            Action::PrintString(_) => unreachable!(),
            _ => {
                segment_end += consumed;
            }
        }

        prev_is_cr = is_cr;
    }

    assert!(remaining_bytes.is_empty(), "{remaining_bytes:?}");

    if segment_end > segment_start {
        let input_start = input.as_ptr();
        if let Some(last) = segments.last_mut() {
            let last_start = last.content.as_ptr();
            let last_end = unsafe { last_start.add(last.content.len()) };
            if !last.ends_with_crlf && !has_visual {
                assert_eq!(segment_start, (last_end as usize - input_start as usize));
                let last_offset = last_start as usize - input_start as usize;
                last.content = &input[last_offset..];
            } else {
                push_segment!(segment_end, false);
            }
        } else {
            push_segment!(segment_end, false);
        }
    }

    segments
}

// ─── Comparison harness ─────────────────────────────────────────────────────

#[track_caller]
fn assert_same(input: &str, widths: &[usize]) {
    for &width in widths {
        let actual = split_into_line_segments(input, width);
        let expected = reference_split(input, width);
        let actual_view: Vec<(&str, bool)> = actual
            .iter()
            .map(|s| (s.content, s.ends_with_crlf))
            .collect();
        let expected_view: Vec<(&str, bool)> = expected
            .iter()
            .map(|s| (s.content, s.ends_with_crlf))
            .collect();
        assert_eq!(
            actual_view, expected_view,
            "divergence for width {width}, input: {input:?}"
        );
    }
}

const WIDTHS: &[usize] = &[1, 2, 3, 5, 8, 10, 20, 80, 200];

#[test]
fn corpus_matches_reference() {
    let corpus: &[&str] = &[
        "",
        "hello",
        "hello world, this is a longer line that will wrap several times",
        "line1\nline2\nline3",
        "line1\r\nline2\r\n",
        "12345\r67",
        "\r\r\n\n\r",
        "😊😊😊 emoji wall 😊😊😊",
        "hello 你好 混合 width",
        "\x1b[31mred\x1b[0m plain \x1b[1;32;44mstyled\x1b[m",
        "\x1b[31m\x1b[1m\x1b[4mnested styles no text\x1b[0m",
        "12345678\x1b[0m90",
        "text\x1b]8;;https://example.com\x07link\x1b]8;;\x07 after",
        "osc title\x1b]0;window title\x07body",
        "cursor \x1b[2Amoves \x1b[10;20H everywhere",
        "tab\tand\x08backspace and \x07bell",
        "\x1b[38;5;196mext colors\x1b[38;2;10;20;30m truecolor\x1b[0m",
        "interrupted \x1b[3\nmid-sequence",
        "\x1b[31m\nstyle then newline",
        "trailing style 12345678\x1b[0m",
        "\x1b(Bcharset\x1b)0 escapes",
        "zero\u{200b}width\u{fe0f}chars",
        "combining a\u{0301}e\u{0301} accents",
    ];
    for input in corpus {
        assert_same(input, WIDTHS);
    }
}

/// Deterministic pseudo-random ANSI soup (xorshift, no extra deps).
#[test]
fn randomized_ansi_soup_matches_reference() {
    let mut state = 0x243F_6A88_85A3_08D3_u64; // seed: pi digits

    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    const PIECES: &[&str] = &[
        "word",
        "a",
        "longer-token",
        " ",
        "  ",
        "\n",
        "\r",
        "\r\n",
        "😊",
        "你好",
        "é",
        "\u{200b}",
        "\x1b[31m",
        "\x1b[0m",
        "\x1b[1;44;38;5;10m",
        "\x1b[2K",
        "\x1b[10D",
        "\x1b]0;title\x07",
        "\x1b]8;;http://x\x07",
        "\t",
        "\x07",
    ];

    for _ in 0..2000 {
        let mut input = String::new();
        let len = (next() % 30) as usize;
        for _ in 0..len {
            input.push_str(PIECES[(next() % PIECES.len() as u64) as usize]);
        }
        let width = 1 + (next() % 40) as usize;
        assert_same(&input, &[width]);
    }
}
