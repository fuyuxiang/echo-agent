use std::{
    collections::VecDeque,
    io::{self, Write},
};

use ratatui::layout::{Rect, Size};

use crate::common::TerminalLike;

/// Mock terminal for testing
#[derive(Debug, Clone)]
pub struct MockTerminal {
    pub size: Size,
    pub viewport_area: Rect,
    pub clear_count: usize,
    pub viewport_updates: Vec<Rect>,
    pub writer: MockWriter,
}

/// Mock writer that captures all output
#[derive(Debug, Clone)]
pub struct MockWriter {
    pub buffer: Vec<u8>,
    pub flush_count: usize,
    pub commands: VecDeque<String>,
}

impl Write for MockWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        // Parse and store readable command representation
        if let Ok(s) = std::str::from_utf8(buf) {
            self.commands.push_back(s.to_string());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_count += 1;
        Ok(())
    }
}

impl MockTerminal {
    pub fn new(width: u16, height: u16, viewport_height: u16) -> Self {
        let viewport_y = height - viewport_height;
        Self {
            size: Size { width, height },
            viewport_area: Rect::new(0, viewport_y, width, viewport_height),
            clear_count: 0,
            viewport_updates: Vec::new(),
            writer: MockWriter {
                buffer: Vec::new(),
                flush_count: 0,
                commands: VecDeque::new(),
            },
        }
    }
}

impl TerminalLike for MockTerminal {
    type Writer = MockWriter;

    fn size(&self) -> io::Result<Size> {
        Ok(self.size)
    }

    fn viewport_area(&self) -> Rect {
        self.viewport_area
    }

    fn clear(&mut self) -> io::Result<()> {
        self.clear_count += 1;
        Ok(())
    }

    fn set_viewport_area(&mut self, area: Rect) {
        self.viewport_updates.push(area);
        self.viewport_area = area;
    }

    fn writer_mut(&mut self) -> &mut Self::Writer {
        &mut self.writer
    }

    fn reset_back_buffer(&mut self) {
        // Mock implementation - just track that it was called
        self.clear_count += 1;
    }
}

/// Tests for the diffed OSC 8 hyperlink layer (`set_frame_links` /
/// `flush_with_links`).
mod links {
    use std::io::{self, Write};

    use ratatui::backend::{Backend, WindowSize};
    use ratatui::buffer::Cell;
    use ratatui::layout::{Position, Rect, Size};
    use ratatui::style::Style;
    use ratatui::{TerminalOptions, Viewport};

    use crate::{LinkSpan, Terminal};

    /// Backend that records the raw byte stream and renders each drawn cell as
    /// its bare symbol, so tests can assert on OSC 8 sequences interleaved with
    /// cell content without depending on crossterm's exact SGR output.
    #[derive(Default)]
    struct RecordingBackend {
        buf: Vec<u8>,
        /// Total lines passed to `append_lines` (used by the
        /// `set_viewport_height` grow-path test).
        appended_lines: u16,
    }

    impl Write for RecordingBackend {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.buf.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Backend for RecordingBackend {
        fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            for (_x, _y, cell) in content {
                self.buf.extend_from_slice(cell.symbol().as_bytes());
            }
            Ok(())
        }
        fn hide_cursor(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn show_cursor(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn get_cursor_position(&mut self) -> io::Result<Position> {
            Ok(Position::ORIGIN)
        }
        fn set_cursor_position<P: Into<Position>>(&mut self, _position: P) -> io::Result<()> {
            Ok(())
        }
        fn clear(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn clear_region(&mut self, _clear_type: ratatui::backend::ClearType) -> io::Result<()> {
            Ok(())
        }
        fn append_lines(&mut self, n: u16) -> io::Result<()> {
            self.appended_lines += n;
            Ok(())
        }
        fn size(&self) -> io::Result<Size> {
            Ok(Size::new(80, 24))
        }
        fn window_size(&mut self) -> io::Result<WindowSize> {
            Ok(WindowSize {
                columns_rows: Size::new(80, 24),
                pixels: Size::new(0, 0),
            })
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn term(w: u16, h: u16) -> Terminal<RecordingBackend> {
        Terminal::with_options(
            RecordingBackend::default(),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, w, h)),
            },
        )
        .unwrap()
    }

    fn span(col_start: u16, col_end: u16, url: &str, id: Option<u32>) -> LinkSpan {
        span_at(0, col_start, col_end, url, id)
    }

    fn span_at(row: u16, col_start: u16, col_end: u16, url: &str, id: Option<u32>) -> LinkSpan {
        LinkSpan {
            row,
            col_start,
            col_end,
            url: url.into(),
            id,
        }
    }

    /// Render `text` at (0,0), set `spans`, flush, and return the bytes emitted
    /// during this single frame.
    fn frame(t: &mut Terminal<RecordingBackend>, text: &str, spans: &[LinkSpan]) -> String {
        t.backend_mut().buf.clear();
        {
            let mut f = t.get_frame();
            f.buffer_mut().set_string(0, 0, text, Style::default());
        }
        t.set_frame_links(spans);
        t.flush_with_links().unwrap();
        t.swap_buffers();
        String::from_utf8(t.backend().buf.clone()).unwrap()
    }

    #[test]
    fn emits_osc8_around_linked_cells() {
        let mut t = term(20, 3);
        let out = frame(
            &mut t,
            "AB",
            &[span(0, 2, "https://echo-agent.invalid", None)],
        );
        assert!(
            out.contains("\x1b]8;;https://echo-agent.invalid\x07"),
            "missing open: {out:?}"
        );
        assert!(out.contains("AB"));
        assert!(out.contains("\x1b]8;;\x07"), "missing close: {out:?}");
    }

    #[test]
    fn no_link_emits_no_osc8() {
        let mut t = term(20, 3);
        let out = frame(&mut t, "AB", &[]);
        assert!(!out.contains("\x1b]8;"), "unexpected OSC8: {out:?}");
    }

    #[test]
    fn grow_viewport_scrolls_committed_lines_into_history() {
        // A small inline viewport near the bottom of the screen, grown to full
        // height, must scroll the rows it will cover up into native scrollback
        // (append_lines) instead of overwriting them. Regression guard for the
        // previously-commented-out scroll_up in set_viewport_height's grow path
        // (the overlay host depends on this in minimal mode).
        let mut t = Terminal::with_options(
            RecordingBackend::default(),
            TerminalOptions {
                viewport: Viewport::Inline(3),
            },
        )
        .unwrap();
        // Pin the 3-row viewport near the bottom of the 24-row screen.
        t.set_viewport_area(Rect::new(0, 21, 80, 3));
        let before = t.backend().appended_lines;
        // Grow to full height: overflow = (21 + 24) - 24 = 21 rows must scroll up.
        t.set_viewport_height(24).unwrap();
        let scrolled = t.backend().appended_lines - before;
        assert!(
            scrolled >= 21,
            "expected >= 21 lines scrolled into history, got {scrolled}"
        );
    }

    /// Regression: `set_viewport_height` must judge grow-vs-shrink against the
    /// live `viewport_area.height`, not the stored `Viewport::Inline(height)`.
    ///
    /// Minimal mode resizes the viewport out-of-band via `set_viewport_area`
    /// (its content-anchored commit path shrinks the region before
    /// `insert_before`), which leaves the stored `Inline` height STALE. If the
    /// next `set_viewport_height` compared against that stale (larger) height, a
    /// genuine grow would be misread as a shrink: the grow-time `scroll_up`
    /// would be skipped and the viewport's top would not move up, so the taller
    /// viewport would run off the bottom of the screen (dropdown items rendered
    /// off-screen — the "empty dropdown over a full screen" bug).
    #[test]
    fn grow_after_out_of_band_area_shrink_still_scrolls() {
        let mut t = Terminal::with_options(
            RecordingBackend::default(),
            TerminalOptions {
                // Stored Inline height starts tall (mimics a streaming turn that
                // grew the viewport to near full screen).
                viewport: Viewport::Inline(21),
            },
        )
        .unwrap();
        // Out-of-band shrink to a 3-row viewport pinned at the bottom of the
        // 24-row screen — as the commit path does. This does NOT update the
        // stored Inline height (still 21), creating the drift.
        t.set_viewport_area(Rect::new(0, 21, 80, 3));

        let before = t.backend().appended_lines;
        // Grow to 10 rows. Against the real height (3) this is a GROW that
        // overflows the bottom by (21 + 10) - 24 = 7 rows, which must scroll up.
        // Against the stale stored height (21) it would look like a shrink and
        // scroll nothing.
        t.set_viewport_height(10).unwrap();

        let scrolled = t.backend().appended_lines - before;
        assert!(
            scrolled >= 7,
            "grow after an out-of-band area shrink must scroll the covered rows \
             into history (expected >= 7, got {scrolled})"
        );
        // The viewport top moved up so the whole 10-row region fits on screen.
        let area = t.viewport_area();
        assert_eq!(area.height, 10, "height should be the requested 10");
        assert!(
            area.y + area.height <= 24,
            "viewport must fit on screen, got y={} h={}",
            area.y,
            area.height
        );
    }

    #[test]
    fn link_removed_next_frame_rewrites_cells_without_osc8() {
        let mut t = term(20, 3);
        let _ = frame(
            &mut t,
            "AB",
            &[span(0, 2, "https://echo-agent.invalid", None)],
        );
        // Same glyphs, but the link is gone: the cells must be rewritten (so the
        // terminal's hyperlink clears) and carry no OSC 8. This is the `/new`
        // regression — clearing is driven purely by the diff.
        let out = frame(&mut t, "AB", &[]);
        assert!(out.contains("AB"), "cells should be redrawn: {out:?}");
        assert!(!out.contains("\x1b]8;"), "stale OSC8 leaked: {out:?}");
    }

    #[test]
    fn unchanged_link_and_content_emits_nothing() {
        let mut t = term(20, 3);
        let _ = frame(
            &mut t,
            "AB",
            &[span(0, 2, "https://echo-agent.invalid", None)],
        );
        // Identical glyphs AND identical link → empty diff → no output at all.
        let out = frame(
            &mut t,
            "AB",
            &[span(0, 2, "https://echo-agent.invalid", None)],
        );
        assert!(out.is_empty(), "expected empty diff, got: {out:?}");
    }

    #[test]
    fn retargeted_link_rewrites_cells() {
        let mut t = term(20, 3);
        let _ = frame(&mut t, "AB", &[span(0, 2, "https://a", None)]);
        let out = frame(&mut t, "AB", &[span(0, 2, "https://b", None)]);
        assert!(
            out.contains("\x1b]8;;https://b\x07"),
            "new url not emitted: {out:?}"
        );
    }

    #[test]
    fn emit_id_param_included() {
        let mut t = term(20, 3);
        let out = frame(
            &mut t,
            "AB",
            &[span(0, 2, "https://echo-agent.invalid", Some(7))],
        );
        assert!(
            out.contains("\x1b]8;id=1;https://echo-agent.invalid\x07"),
            "id param missing: {out:?}"
        );
    }

    #[test]
    fn colliding_source_ids_different_urls_get_distinct_osc8_ids() {
        let mut t = term(20, 3);
        let out = frame(
            &mut t,
            "AxBxC",
            &[
                span(0, 1, "https://first.com", Some(0)),
                span(2, 3, "https://second.com", Some(0)),
                span(4, 5, "https://third.com", Some(0)),
            ],
        );
        assert!(
            out.contains("\x1b]8;id=1;https://first.com\x07"),
            "first: {out:?}"
        );
        assert!(
            out.contains("\x1b]8;id=2;https://second.com\x07"),
            "second: {out:?}"
        );
        assert!(
            out.contains("\x1b]8;id=3;https://third.com\x07"),
            "third: {out:?}"
        );
    }

    fn id1_payloads(out: &str, url: &str) -> String {
        let open = format!("\x1b]8;id=1;{url}\x07");
        let close = "\x1b]8;;\x07";
        let mut payload = String::new();
        let mut rest = out;
        while let Some(i) = rest.find(&open) {
            let after = &rest[i + open.len()..];
            let end = after.find(close).expect("OSC 8 close after id=1 open");
            payload.push_str(&after[..end]);
            rest = &after[end + close.len()..];
        }
        payload
    }

    #[test]
    fn wrapped_same_source_id_and_url_share_osc8_id() {
        let mut t = term(20, 3);
        t.backend_mut().buf.clear();
        {
            let mut f = t.get_frame();
            f.buffer_mut().set_string(0, 0, "AB", Style::default());
            f.buffer_mut().set_string(0, 1, "CD", Style::default());
            f.buffer_mut().set_string(0, 2, "EF", Style::default());
        }
        t.set_frame_links(&[
            span_at(0, 0, 2, "https://wrap.com", Some(3)),
            span_at(1, 0, 2, "https://wrap.com", Some(3)),
            span_at(2, 0, 2, "https://other.com", Some(3)),
        ]);
        t.flush_with_links().unwrap();
        t.swap_buffers();
        let out = String::from_utf8(t.backend().buf.clone()).unwrap();
        let payload = id1_payloads(&out, "https://wrap.com");
        assert!(
            payload.contains("AB") && payload.contains("CD"),
            "both wrap fragments must sit in id=1 runs: payload={payload:?} out={out:?}"
        );
        assert!(
            !payload.contains('E') && !payload.contains('F'),
            "following collision must not share wrap id: payload={payload:?} out={out:?}"
        );
        assert!(
            !out.contains("\x1b]8;;https://wrap.com\x07"),
            "un-id'd open must be absent: {out:?}"
        );
        assert!(
            out.contains("\x1b]8;id=2;https://other.com\x07"),
            "collision remint missing: {out:?}"
        );
    }

    #[test]
    fn url_control_chars_sanitized() {
        let mut t = term(20, 3);
        let out = frame(&mut t, "AB", &[span(0, 2, "https://x\x07\x1b/y", None)]);
        assert!(
            out.contains("\x1b]8;;https://x/y\x07"),
            "url not sanitized: {out:?}"
        );
    }

    #[test]
    fn distinct_links_split_into_separate_runs() {
        let mut t = term(20, 3);
        // "AxB": A→a, gap x (no link), B→b.
        let out = frame(
            &mut t,
            "AxB",
            &[span(0, 1, "https://a", None), span(2, 3, "https://b", None)],
        );
        // Each link wraps exactly its own cell; the gap is not wrapped.
        assert!(
            out.contains("\x1b]8;;https://a\x07A\x1b]8;;\x07"),
            "a-run: {out:?}"
        );
        assert!(
            out.contains("\x1b]8;;https://b\x07B\x1b]8;;\x07"),
            "b-run: {out:?}"
        );
    }

    #[test]
    fn wide_char_under_link_wraps_lead_cell_only() {
        let mut t = term(20, 3);
        // A width-2 char occupies two cells; only the lead cell is drawn, and
        // the OSC 8 wraps it.
        let out = frame(
            &mut t,
            "世",
            &[span(0, 2, "https://echo-agent.invalid", None)],
        );
        assert!(
            out.contains("\x1b]8;;https://echo-agent.invalid\x07世\x1b]8;;\x07"),
            "wide-char run: {out:?}"
        );
    }

    #[test]
    fn nonzero_origin_viewport_maps_links() {
        // The screen→cell mapping subtracts the viewport offset; verify a link
        // at an absolute (row, col) inside a non-origin viewport wraps the right
        // cells (regression guard for `(y - area.y)` / `(x - area.x)`).
        let area = Rect::new(2, 5, 20, 4);
        let mut t = Terminal::with_options(
            RecordingBackend::default(),
            TerminalOptions {
                viewport: Viewport::Fixed(area),
            },
        )
        .unwrap();
        {
            let mut f = t.get_frame();
            f.buffer_mut().set_string(2, 5, "AB", Style::default());
        }
        t.set_frame_links(&[LinkSpan {
            row: 5,
            col_start: 2,
            col_end: 4,
            url: "https://echo-agent.invalid".into(),
            id: None,
        }]);
        t.flush_with_links().unwrap();
        let out = String::from_utf8(t.backend().buf.clone()).unwrap();
        assert!(
            out.contains("\x1b]8;;https://echo-agent.invalid\x07AB\x1b]8;;\x07"),
            "non-origin mapping: {out:?}"
        );
    }
}
