//! Bounds-checked buffer helpers.
//!
//! Ratatui's `Buffer::set_line`, `set_span`, and `set_string` panic when
//! given out-of-bounds coordinates (via `index_of`). During terminal resize
//! races, computed widget areas can momentarily exceed the buffer; these
//! helpers skip the write instead of panicking.
//!
//! Optional RTL reordering lives on [`Self::set_line_safe_bidi`] only — used by
//! scrollback and list content, which also own the column maps for search,
//! selection, and links. Generic chrome (`set_line_safe`) stays logical so
//! dropdowns and modals keep logical hit-testing.

use ratatui::buffer::Buffer;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::bidi::{is_enabled, visual_line};

/// Extension trait for bounds-checked buffer writes.
pub trait SafeBuf {
    /// Like `Buffer::set_line`, skipping out-of-bounds `y`. No bidi reorder.
    fn set_line_safe(&mut self, x: u16, y: u16, line: &Line<'_>, width: u16);

    /// Like [`Self::set_line_safe`], but reorders the full line when
    /// `[scrollback.display] rtl_bidi` is on. Use only where consumers map
    /// visual columns (scrollback, list content).
    fn set_line_safe_bidi(&mut self, x: u16, y: u16, line: &Line<'_>, width: u16);

    /// Like `Buffer::set_span`, skipping out-of-bounds `y` (no bidi reorder).
    fn set_span_safe(&mut self, x: u16, y: u16, span: &Span<'_>, width: u16);

    /// Like `Buffer::set_string`, skipping out-of-bounds `y` (no bidi reorder).
    fn set_string_safe<S: AsRef<str>>(&mut self, x: u16, y: u16, string: S, style: Style);
}

impl SafeBuf for Buffer {
    #[inline]
    fn set_line_safe(&mut self, x: u16, y: u16, line: &Line<'_>, width: u16) {
        if y >= self.area.y && y < self.area.bottom() && x < self.area.right() {
            self.set_line(x, y, line, width);
        }
    }

    #[inline]
    fn set_line_safe_bidi(&mut self, x: u16, y: u16, line: &Line<'_>, width: u16) {
        if y >= self.area.y && y < self.area.bottom() && x < self.area.right() {
            if is_enabled()
                && let Some(visual) = visual_line(line)
            {
                self.set_line(x, y, &visual, width);
            } else {
                self.set_line(x, y, line, width);
            }
        }
    }

    #[inline]
    fn set_span_safe(&mut self, x: u16, y: u16, span: &Span<'_>, width: u16) {
        if y >= self.area.y && y < self.area.bottom() && x < self.area.right() {
            self.set_span(x, y, span, width);
        }
    }

    #[inline]
    fn set_string_safe<S: AsRef<str>>(&mut self, x: u16, y: u16, string: S, style: Style) {
        if y >= self.area.y && y < self.area.bottom() && x < self.area.right() {
            self.set_string(x, y, string, style);
        }
    }
}
