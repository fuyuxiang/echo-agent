use super::*;

fn render(text: &str, area: Rect, padding: u16) -> (Buffer, Vec<LinkSpan>) {
    let mut buf = Buffer::empty(Rect::new(0, 0, area.right(), area.bottom()));
    let display = StatusLineDisplay::Text(SanitizedText::new(text));
    let links = render_status_line(&mut buf, area, &display, padding, &Theme::tokyonight());
    (buf, links)
}

fn buffer_line(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
}

#[test]
fn a_builtin_row_separates_its_segments_and_colours_the_warning() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 1));
    let area = Rect::new(0, 0, 40, 1);
    let theme = Theme::tokyonight();
    let display = StatusLineDisplay::Segments(vec![
        StatusSegment {
            text: "echoagent".into(),
            tone: SegmentTone::Dim,
        },
        StatusSegment {
            text: "90% ctx".into(),
            tone: SegmentTone::Warn,
        },
    ]);

    let links = render_status_line(&mut buf, area, &display, 0, &theme);
    assert!(links.is_empty(), "segments carry no links");

    // The separator is spelled out rather than read from the constant, which
    // would make this assertion true whatever the constant became.
    assert_eq!(buffer_line(&buf, 0).trim_end(), "echoagent │ 90% ctx");
    let warn_at = buffer_line(&buf, 0)
        .find("90%")
        .expect("the warning paints");
    assert_eq!(
        buf[(warn_at as u16, 0)].fg,
        theme.warning,
        "a segment near the compaction threshold has to stand out"
    );
}

#[test]
fn multiline_ansi_paints_each_line() {
    let (buf, _) = render("first\nsecond", Rect::new(0, 0, 10, 2), 0);
    assert_eq!(buffer_line(&buf, 0).trim_end(), "first");
    assert_eq!(buffer_line(&buf, 1).trim_end(), "second");
}

#[test]
fn tabs_are_drawn_as_spaces_rather_than_deleted() {
    let (buf, _) = render("a\tb", Rect::new(0, 0, 10, 1), 0);
    // The pager's default tab width is four.
    assert_eq!(buffer_line(&buf, 0).trim_end(), "a    b");
}

#[test]
fn carriage_return_takes_no_column_so_nothing_is_elided() {
    let (buf, _) = render("abcde\r", Rect::new(0, 0, 5, 1), 0);
    assert_eq!(buffer_line(&buf, 0), "abcde");
}

#[test]
fn text_the_script_did_not_colour_is_muted_and_never_blinks() {
    let theme = Theme::tokyonight();
    let (buf, _) = render("\x1b[32mX\x1b[0mY", Rect::new(0, 0, 10, 1), 0);
    assert_eq!(
        buf[(1, 0)].style().fg,
        theme.muted().fg,
        "the row is chrome, but not quieter than the hints below it"
    );
    assert_ne!(
        buf[(0, 0)].style().fg,
        theme.muted().fg,
        "a colour the script asked for has to survive"
    );

    let (buf, _) = render("\x1b[5mtick\x1b[8mhidden", Rect::new(0, 0, 20, 1), 0);
    let modifiers = buf[(0, 0)].style().add_modifier | buf[(5, 0)].style().add_modifier;
    assert!(
        !modifiers.intersects(
            ratatui::style::Modifier::SLOW_BLINK
                | ratatui::style::Modifier::RAPID_BLINK
                | ratatui::style::Modifier::HIDDEN
        ),
        "a row that blinks or hides itself cannot be read"
    );
}

#[test]
fn padding_that_leaves_no_columns_reserves_nothing() {
    assert_eq!(inner_width(10, 5), None);
    assert_eq!(inner_width(11, 5), Some(1));
    assert_eq!(inner_width(80, 0), Some(80));
}

#[test]
fn row_holds_its_height_before_the_first_result() {
    // Padding does not enter the height.
    let padding = 0;
    let two_lines = Arc::new(StatusLineDisplay::Text(SanitizedText::new("a\nb")));

    assert_eq!(StatusLineFrame::Off.height(), 0);
    assert_eq!(StatusLineFrame::Reserved { padding }.height(), 1);
    assert_eq!(
        StatusLineFrame::On {
            display: two_lines,
            padding
        }
        .height(),
        2
    );
}

#[test]
fn elide_never_exceeds_the_width_it_was_given() {
    // One case per way the two width models disagree, ASCII being the control:
    // under `Line::width` two of these cases fit and would assert nothing.
    for (name, cluster) in [
        ("ascii", "hello world "),
        ("variation selector", "\u{26a0}\u{fe0f}"),
        ("zwj", "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}"),
    ] {
        let line = Line::from(vec![Span::raw(cluster.to_string()); 60]);
        assert!(painted_line_width(&line) > 80, "{name}: the input fits");
        assert!(painted_line_width(&elide(&line, 80).0) <= 80, "{name}");
    }
}

#[test]
fn elide_marks_the_cut_without_eating_the_scripts_own_marker() {
    let (cut, kept) = elide(&Line::from(vec![Span::raw("abcdefghij")]), 6);
    assert_eq!(cut.spans[0].content.as_ref(), "abcde");
    assert_eq!(cut.spans.last().unwrap().content.as_ref(), "\u{2026}");
    assert_eq!(kept, 5, "the marker is not text a link can cover");

    let (untouched, _) = elide(&Line::from(vec![Span::raw("ab\u{2026}")]), 80);
    assert_eq!(untouched.spans.len(), 1);
    assert_eq!(untouched.spans[0].content.as_ref(), "ab\u{2026}");
}

/// A link whose text the elision cut away must not survive on the marker: the
/// `…` is the only thing left in those columns, and it is not the link's text.
#[test]
fn link_elided_away_does_not_make_the_marker_clickable() {
    let input = "0123456789\x1b]8;;https://example.com\x07AB\x1b]8;;\x07";
    let (_, spans) = render(input, Rect::new(0, 0, 11, 1), 0);

    assert!(
        spans.is_empty(),
        "the link covers columns the row replaced with a marker: {spans:?}"
    );
}

#[test]
fn render_ansi_emits_absolute_link_spans() {
    let input = "[EchoAgent] \x1b]8;;https://example.com/repo\x07repo\x1b]8;;\x07";
    let (_, spans) = render(input, Rect::new(3, 5, 40, 1), 2);

    assert_eq!(spans.len(), 1);
    let span = &spans[0];
    // Area x 3, plus 2 columns of padding, plus the 7 of `[EchoAgent] `.
    assert_eq!((span.row, span.col_start, span.col_end), (5, 12, 16));
    assert_eq!(span.url.as_ref(), "https://example.com/repo");
}
