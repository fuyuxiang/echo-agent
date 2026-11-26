// Derived from ratatui's Terminal implementation (MIT / Apache-2.0 dual license).
// Upstream: https://github.com/ratatui/ratatui — Copyright (c) The Ratatui Developers.
// Modified for inline viewport support. See ../NOTICE and repository THIRD-PARTY-NOTICES.
//
#![allow(clippy::collapsible_if)]

use std::io::{self, Write};
use std::sync::Arc;

use ratatui::{
    CompletedFrame, Frame, TerminalOptions, Viewport,
    backend::{Backend, ClearType},
    buffer::{Buffer, Cell},
    layout::{Position, Rect, Size},
};
use unicode_width::UnicodeWidthStr as _;

/// A hyperlink region on a single screen row, in absolute viewport coordinates.
///
/// Handed to [`Terminal::set_frame_links`] each frame. The terminal folds these
/// into a per-cell link layer that participates in the frame diff, so OSC 8
/// sequences are emitted (and cleared) by the same machinery that draws cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub row: u16,
    pub col_start: u16,
    pub col_end: u16,
    pub url: Arc<str>,
    /// Source grouping key (markdown / overlay id), not the emitted OSC 8 `id=`.
    pub id: Option<u32>,
}

/// Resolved hyperlink target stored in a frame's link table; `link_ids` entries
/// are 1-based indices into the matching `link_tables` vector.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LinkRef {
    url: Arc<str>,
    /// Reminted OSC 8 `id=`, not [`LinkSpan::id`].
    id: Option<u32>,
}

struct LastNamedOsc8 {
    source_id: u32,
    url: Arc<str>,
    osc8_id: u32,
}

/// Resolve a per-cell link id (`0` = none) to its [`LinkRef`].
fn resolve_link<'a>(ids: &[u32], table: &'a [LinkRef], i: usize) -> Option<&'a LinkRef> {
    match ids.get(i).copied().unwrap_or(0) {
        0 => None,
        id => table.get((id - 1) as usize),
    }
}

/// Emit an OSC 8 hyperlink open sequence.
///
/// Control characters are stripped from `url` to prevent premature sequence
/// termination or escape injection. The sequence is terminated with BEL
/// (`\x07`) for broadest terminal/multiplexer support; here the BEL is
/// immediately followed by the cell draw's ESC (cursor move / SGR), which all
/// mainstream terminals parse correctly (OSC-BEL followed by CSI is ubiquitous,
/// e.g. title sets). ST would also be valid but is less widely supported.
fn write_osc8_open<W: Write>(w: &mut W, url: &str, id: Option<u32>) -> io::Result<()> {
    let sanitized: std::borrow::Cow<str> = if url.chars().any(|c| c.is_control()) {
        std::borrow::Cow::Owned(url.chars().filter(|c| !c.is_control()).collect())
    } else {
        std::borrow::Cow::Borrowed(url)
    };
    match id {
        Some(id) => write!(w, "\x1b]8;id={id};{sanitized}\x07"),
        None => write!(w, "\x1b]8;;{sanitized}\x07"),
    }
}

/// Emit an OSC 8 hyperlink close sequence.
fn write_osc8_close<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(b"\x1b]8;;\x07")
}

/// Markdown ids restart per document; OSC 8 `id=` is terminal-global.
/// Consecutive wrap segments of the same source id+URL reuse the last emitted id.
fn next_osc8_id(
    span: &LinkSpan,
    last_named: &mut Option<LastNamedOsc8>,
    next: &mut u32,
) -> Option<u32> {
    let Some(source_id) = span.id else {
        *last_named = None;
        return None;
    };
    if let Some(last) = last_named.as_ref()
        && last.source_id == source_id
        && last.url.as_ref() == span.url.as_ref()
    {
        return Some(last.osc8_id);
    }
    *next = next.saturating_add(1);
    *last_named = Some(LastNamedOsc8 {
        source_id,
        url: Arc::clone(&span.url),
        osc8_id: *next,
    });
    Some(*next)
}

#[derive(Debug, Hash)]
pub struct OurFrame<'a> {
    /// Where should the cursor be after drawing this frame?
    ///
    /// If `None`, the cursor is hidden and its position is controlled by the backend. If `Some((x,
    /// y))`, the cursor is shown and placed at `(x, y)` after the call to `Terminal::draw()`.
    pub(crate) cursor_position: Option<Position>,

    /// The area of the viewport
    pub(crate) viewport_area: Rect,

    /// The buffer that is used to draw the current frame
    pub(crate) buffer: &'a mut Buffer,

    /// The frame count indicating the sequence number of this frame.
    pub(crate) count: usize,
}

impl<'a> From<OurFrame<'a>> for Frame<'a> {
    fn from(value: OurFrame<'a>) -> Self {
        assert_eq!(
            std::mem::size_of::<Frame>(),
            std::mem::size_of::<OurFrame>()
        );
        unsafe { std::mem::transmute(value) }
    }
}

impl<'a> From<Frame<'a>> for OurFrame<'a> {
    fn from(value: Frame<'a>) -> Self {
        assert_eq!(
            std::mem::size_of::<Frame>(),
            std::mem::size_of::<OurFrame>()
        );
        unsafe { std::mem::transmute(value) }
    }
}

/// An interface to interact and draw [`Frame`]s on the user's terminal.
///
/// This is the main entry point for Ratatui. It is responsible for drawing and maintaining the
/// state of the buffers, cursor and viewport.
///
/// The [`Terminal`] is generic over a [`Backend`] implementation which is used to interface with
/// the underlying terminal library. The [`Backend`] trait is implemented for three popular Rust
/// terminal libraries: [Crossterm], [Termion] and [Termwiz]. See the [`backend`] module for more
/// information.
///
/// The `Terminal` struct maintains two buffers: the current and the previous.
/// When the widgets are drawn, the changes are accumulated in the current buffer.
/// At the end of each draw pass, the two buffers are compared, and only the changes
/// between these buffers are written to the terminal, avoiding any redundant operations.
/// After flushing these changes, the buffers are swapped to prepare for the next draw cycle.
///
/// The terminal also has a viewport which is the area of the terminal that is currently visible to
/// the user. It can be either fullscreen, inline or fixed. See [`Viewport`] for more information.
///
/// Applications should detect terminal resizes and call [`Terminal::draw`] to redraw the
/// application with the new size. This will automatically resize the internal buffers to match the
/// new size for inline and fullscreen viewports. Fixed viewports are not resized automatically.
///
/// # Examples
///
/// ```rust,no_run
/// use std::io::stdout;
///
/// use ratatui::{backend::CrosstermBackend, widgets::Paragraph, Terminal};
///
/// let backend = CrosstermBackend::new(stdout());
/// let mut terminal = Terminal::new(backend)?;
/// terminal.draw(|frame| {
///     let area = frame.area();
///     frame.render_widget(Paragraph::new("Hello World!"), area);
/// })?;
/// # std::io::Result::Ok(())
/// ```
///
/// [Crossterm]: https://crates.io/crates/crossterm
/// [Termion]: https://crates.io/crates/termion
/// [Termwiz]: https://crates.io/crates/termwiz
/// [`backend`]: crate::backend
/// [`Backend`]: crate::backend::Backend
/// [`Buffer`]: crate::buffer::Buffer
#[derive(Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct Terminal<B>
where
    B: Backend,
{
    /// The backend used to interface with the terminal
    backend: B,
    /// Holds the results of the current and previous draw calls. The two are compared at the end
    /// of each draw pass to output the necessary updates to the terminal
    buffers: [Buffer; 2],
    /// Index of the current buffer in the previous array
    current: usize,
    /// Whether the cursor is currently hidden
    hidden_cursor: bool,
    /// Viewport
    viewport: Viewport,
    /// Area of the viewport
    viewport_area: Rect,
    /// Last known area of the terminal. Used to detect if the internal buffers have to be resized.
    last_known_area: Rect,
    /// Last known position of the cursor. Used to find the new area when the viewport is inlined
    /// and the terminal resized.
    last_known_cursor_pos: Position,
    /// Number of frames rendered up until current time.
    frame_count: usize,
    /// Per-cell hyperlink id layer (`0` = no link), one per entry in `buffers`
    /// and indexed identically (row-major over `viewport_area`). Populated by
    /// [`Terminal::set_frame_links`] and diffed in [`Terminal::flush_with_links`].
    link_ids: [Vec<u32>; 2],
    /// Per-frame hyperlink table; a `link_ids` value of `n` refers to entry
    /// `n - 1` of the matching table.
    link_tables: [Vec<LinkRef>; 2],
}

impl<B> Drop for Terminal<B>
where
    B: Backend,
{
    fn drop(&mut self) {
        // Attempt to restore the cursor state
        if self.hidden_cursor {
            let _ = self.show_cursor();
        }
    }
}

impl<B> Terminal<B>
where
    B: Backend,
{
    /// Creates a new [`Terminal`] with the given [`Backend`] with a full screen viewport.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::io::stdout;
    ///
    /// use ratatui::{backend::CrosstermBackend, Terminal};
    ///
    /// let backend = CrosstermBackend::new(stdout());
    /// let terminal = Terminal::new(backend)?;
    /// # std::io::Result::Ok(())
    /// ```
    pub fn new(backend: B) -> io::Result<Self> {
        Self::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fullscreen,
            },
        )
    }

    /// Creates a new [`Terminal`] with the given [`Backend`] and [`TerminalOptions`].
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::io::stdout;
    ///
    /// use ratatui::{backend::CrosstermBackend, layout::Rect, Terminal, TerminalOptions, Viewport};
    ///
    /// let backend = CrosstermBackend::new(stdout());
    /// let viewport = Viewport::Fixed(Rect::new(0, 0, 10, 10));
    /// let terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
    /// # std::io::Result::Ok(())
    /// ```
    pub fn with_options(mut backend: B, options: TerminalOptions) -> io::Result<Self> {
        let area = match options.viewport {
            Viewport::Fullscreen | Viewport::Inline(_) => {
                Rect::from((Position::ORIGIN, backend.size()?))
            }
            Viewport::Fixed(area) => area,
        };
        let (viewport_area, cursor_pos) = match options.viewport {
            Viewport::Fullscreen => (area, Position::ORIGIN),
            Viewport::Inline(height) => {
                compute_inline_size(&mut backend, height, area.as_size(), 0)?
            }
            Viewport::Fixed(area) => (area, area.as_position()),
        };
        let link_len = (viewport_area.width as usize) * (viewport_area.height as usize);
        Ok(Self {
            backend,
            buffers: [Buffer::empty(viewport_area), Buffer::empty(viewport_area)],
            current: 0,
            hidden_cursor: false,
            viewport: options.viewport,
            viewport_area,
            last_known_area: area,
            last_known_cursor_pos: cursor_pos,
            frame_count: 0,
            link_ids: [vec![0; link_len], vec![0; link_len]],
            link_tables: [Vec::new(), Vec::new()],
        })
    }

    /// Get a Frame object which provides a consistent view into the terminal state for rendering.
    pub fn get_frame(&mut self) -> Frame<'_> {
        let count = self.frame_count;
        OurFrame {
            cursor_position: None,
            viewport_area: self.viewport_area,
            buffer: self.current_buffer_mut(),
            count,
        }
        .into() // HACK
    }

    /// Gets the current buffer as a mutable reference.
    pub fn current_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current]
    }

    /// Gets the backend
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// Gets the backend as a mutable reference
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Obtains a difference between the previous and the current buffer and passes it to the
    /// current backend for drawing. Returns `true` if any cells were changed.
    ///
    /// Uses [`diff_large`] instead of ratatui's [`Buffer::diff`] to avoid a `u16`
    /// truncation bug: upstream `pos_of()` casts the flat cell index to `u16`
    /// before computing `(x, y)`, which silently wraps around when
    /// `width * height > 65 535`.  On extra-large terminals (e.g. 420×160 = 67 200
    /// cells) this causes the entire UI to be rendered into a tiny corner.
    pub fn flush(&mut self) -> io::Result<bool> {
        let previous_buffer = &self.buffers[1 - self.current];
        let current_buffer = &self.buffers[self.current];
        let updates = diff_large(previous_buffer, current_buffer);
        let has_changes = !updates.is_empty();
        if let Some((col, row, _)) = updates.last() {
            self.last_known_cursor_pos = Position { x: *col, y: *row };
        }
        self.backend.draw(updates.into_iter())?;
        Ok(has_changes)
    }

    /// Set the hyperlink spans for the frame about to be flushed.
    ///
    /// Rebuilds the *current* link layer from `spans` (absolute viewport
    /// coordinates); the previous layer is retained so [`flush_with_links`] can
    /// diff it. Call once per frame, after rendering and before
    /// [`flush_with_links`]. Passing an empty slice clears the frame's links
    /// (so links from the previous frame are diffed away).
    ///
    /// [`LinkSpan::id`] is a source grouping key. Consecutive spans with the
    /// same id and URL share one reminted OSC 8 `id=` starting at 1.
    ///
    /// [`flush_with_links`]: Self::flush_with_links
    pub fn set_frame_links(&mut self, spans: &[LinkSpan]) {
        let area = self.viewport_area;
        let width = area.width as usize;
        let len = width * (area.height as usize);

        let ids = &mut self.link_ids[self.current];
        ids.clear();
        ids.resize(len, 0);
        let table = &mut self.link_tables[self.current];
        table.clear();

        let mut next = 0u32;
        let mut last_named: Option<LastNamedOsc8> = None;

        for span in spans {
            if span.row < area.y || span.row >= area.bottom() {
                continue;
            }
            let start = span.col_start.max(area.x);
            let end = span.col_end.min(area.right());
            if start >= end {
                continue;
            }
            let osc8_id = next_osc8_id(span, &mut last_named, &mut next);
            let id = (table.len() + 1) as u32;
            table.push(LinkRef {
                url: span.url.clone(),
                id: osc8_id,
            });
            let row = (span.row - area.y) as usize;
            for col in start..end {
                let idx = row * width + (col - area.x) as usize;
                if idx < ids.len() {
                    ids[idx] = id;
                }
            }
        }
    }

    /// Like [`flush`](Self::flush) but emits OSC 8 hyperlinks for cells covered
    /// by the current link layer (see [`set_frame_links`](Self::set_frame_links)).
    ///
    /// A cell is rewritten when its content/style changed **or** its link
    /// changed, so links are cleared automatically when they disappear — no
    /// out-of-band repaint. Contiguous runs of cells sharing the same link are
    /// wrapped in a single OSC 8 open/close around the upstream cell draw.
    pub fn flush_with_links(&mut self) -> io::Result<bool>
    where
        B: Write,
    {
        let cur = self.current;
        let prev = 1 - cur;

        // Fast path: no hyperlinks in either the current or previous frame. The
        // link layer can't affect the diff or emission, so fall back to the
        // plain cell diff + draw — byte-for-byte identical to `flush` with zero
        // per-cell link resolution. This keeps the overwhelmingly common
        // link-free frame (streaming output, etc.) as cheap as before.
        if self.link_tables[cur].is_empty() && self.link_tables[prev].is_empty() {
            return self.flush();
        }

        let updates = diff_large_with_links(
            &self.buffers[prev],
            &self.buffers[cur],
            &self.link_ids[prev],
            &self.link_ids[cur],
            &self.link_tables[prev],
            &self.link_tables[cur],
        );
        let has_changes = !updates.is_empty();
        if let Some((col, row, _)) = updates.last() {
            self.last_known_cursor_pos = Position { x: *col, y: *row };
        }
        let area = self.buffers[cur].area;
        emit_frame_with_links(
            &mut self.backend,
            &updates,
            &self.link_ids[cur],
            &self.link_tables[cur],
            area,
        )?;
        Ok(has_changes)
    }

    /// Updates the Terminal so that internal buffers match the requested area.
    ///
    /// Requested area will be saved to remain consistent when rendering. This leads to a full clear
    /// of the screen.
    pub fn resize(&mut self, area: Rect) -> io::Result<()> {
        let next_area = match self.viewport {
            // Full-height inline viewport: the inline viewport currently spans the
            // entire terminal. This is how the viewport is used when the alternate
            // screen is unavailable (e.g. under Zellij or tmux control mode, or
            // with `--no-alt-screen`): the whole terminal is one inline viewport
            // standing in for a fullscreen app. On resize it must keep spanning
            // the entire terminal, exactly like a fullscreen viewport.
            //
            // The generic `compute_inline_size` path below is built for a *small*
            // inline viewport anchored near the cursor and is wrong here in two
            // ways: (1) it clamps the height to the fixed `Viewport::Inline(height)`
            // captured at startup, so enlarging the terminal never grows the
            // viewport — the UI ends up truncated at the bottom even though the
            // width tracks the resize; and (2) on shrink it can reposition the
            // viewport partly or fully off-screen. Filling the new area avoids both.
            Viewport::Inline(_)
                if self.viewport_area.y == 0
                    && self.viewport_area.height >= self.last_known_area.height =>
            {
                area
            }
            Viewport::Inline(height) => {
                let offset_in_previous_viewport = self
                    .last_known_cursor_pos
                    .y
                    .saturating_sub(self.viewport_area.top());
                compute_inline_size(
                    &mut self.backend,
                    height,
                    area.as_size(),
                    offset_in_previous_viewport,
                )?
                .0
            }
            Viewport::Fixed(_) | Viewport::Fullscreen => area,
        };
        self.set_viewport_area(next_area);
        self.clear()?;

        self.last_known_area = area;
        Ok(())
    }

    /// Queries the backend for size and resizes if it doesn't match the previous size.
    pub fn autoresize(&mut self) -> io::Result<()> {
        // fixed viewports do not get autoresized
        if matches!(self.viewport, Viewport::Fullscreen | Viewport::Inline(_)) {
            let area = Rect::from((Position::ORIGIN, self.size()?));
            if area != self.last_known_area {
                self.resize(area)?;
            }
        };
        Ok(())
    }

    /// Draws a single frame to the terminal.
    ///
    /// Returns a [`CompletedFrame`] if successful, otherwise a [`std::io::Error`].
    ///
    /// If the render callback passed to this method can fail, use [`try_draw`] instead.
    ///
    /// Applications should call `draw` or [`try_draw`] in a loop to continuously render the
    /// terminal. These methods are the main entry points for drawing to the terminal.
    ///
    /// [`try_draw`]: Terminal::try_draw
    ///
    /// This method will:
    ///
    /// - autoresize the terminal if necessary
    /// - call the render callback, passing it a [`Frame`] reference to render to
    /// - flush the current internal state by copying the current buffer to the backend
    /// - move the cursor to the last known position if it was set during the rendering closure
    /// - return a [`CompletedFrame`] with the current buffer and the area of the terminal
    ///
    /// The [`CompletedFrame`] returned by this method can be useful for debugging or testing
    /// purposes, but it is often not used in regular applicationss.
    ///
    /// The render callback should fully render the entire frame when called, including areas that
    /// are unchanged from the previous frame. This is because each frame is compared to the
    /// previous frame to determine what has changed, and only the changes are written to the
    /// terminal. If the render callback does not fully render the frame, the terminal will not be
    /// in a consistent state.
    ///
    /// # Examples
    ///
    /// ```
    /// # let backend = ratatui::backend::TestBackend::new(10, 10);
    /// # let mut terminal = ratatui::Terminal::new(backend)?;
    /// use ratatui::{layout::Position, widgets::Paragraph};
    ///
    /// // with a closure
    /// terminal.draw(|frame| {
    ///     let area = frame.area();
    ///     frame.render_widget(Paragraph::new("Hello World!"), area);
    ///     frame.set_cursor_position(Position { x: 0, y: 0 });
    /// })?;
    ///
    /// // or with a function
    /// terminal.draw(render)?;
    ///
    /// fn render(frame: &mut ratatui::Frame) {
    ///     frame.render_widget(Paragraph::new("Hello World!"), frame.area());
    /// }
    /// # std::io::Result::Ok(())
    /// ```
    pub fn draw<F>(&mut self, render_callback: F) -> io::Result<CompletedFrame<'_>>
    where
        F: FnOnce(&mut Frame),
    {
        self.try_draw(|frame| {
            render_callback(frame);
            io::Result::Ok(())
        })
    }

    /// Tries to draw a single frame to the terminal.
    ///
    /// Returns [`Result::Ok`] containing a [`CompletedFrame`] if successful, otherwise
    /// [`Result::Err`] containing the [`std::io::Error`] that caused the failure.
    ///
    /// This is the equivalent of [`Terminal::draw`] but the render callback is a function or
    /// closure that returns a `Result` instead of nothing.
    ///
    /// Applications should call `try_draw` or [`draw`] in a loop to continuously render the
    /// terminal. These methods are the main entry points for drawing to the terminal.
    ///
    /// [`draw`]: Terminal::draw
    ///
    /// This method will:
    ///
    /// - autoresize the terminal if necessary
    /// - call the render callback, passing it a [`Frame`] reference to render to
    /// - flush the current internal state by copying the current buffer to the backend
    /// - move the cursor to the last known position if it was set during the rendering closure
    /// - return a [`CompletedFrame`] with the current buffer and the area of the terminal
    ///
    /// The render callback passed to `try_draw` can return any [`Result`] with an error type that
    /// can be converted into an [`std::io::Error`] using the [`Into`] trait. This makes it possible
    /// to use the `?` operator to propagate errors that occur during rendering. If the render
    /// callback returns an error, the error will be returned from `try_draw` as an
    /// [`std::io::Error`] and the terminal will not be updated.
    ///
    /// The [`CompletedFrame`] returned by this method can be useful for debugging or testing
    /// purposes, but it is often not used in regular applicationss.
    ///
    /// The render callback should fully render the entire frame when called, including areas that
    /// are unchanged from the previous frame. This is because each frame is compared to the
    /// previous frame to determine what has changed, and only the changes are written to the
    /// terminal. If the render function does not fully render the frame, the terminal will not be
    /// in a consistent state.
    ///
    /// # Examples
    ///
    /// ```should_panic
    /// # use ratatui::layout::Position;;
    /// # let backend = ratatui::backend::TestBackend::new(10, 10);
    /// # let mut terminal = ratatui::Terminal::new(backend)?;
    /// use std::io;
    ///
    /// use ratatui::widgets::Paragraph;
    ///
    /// // with a closure
    /// terminal.try_draw(|frame| {
    ///     let value: u8 = "not a number".parse().map_err(io::Error::other)?;
    ///     let area = frame.area();
    ///     frame.render_widget(Paragraph::new("Hello World!"), area);
    ///     frame.set_cursor_position(Position { x: 0, y: 0 });
    ///     io::Result::Ok(())
    /// })?;
    ///
    /// // or with a function
    /// terminal.try_draw(render)?;
    ///
    /// fn render(frame: &mut ratatui::Frame) -> io::Result<()> {
    ///     let value: u8 = "not a number".parse().map_err(io::Error::other)?;
    ///     frame.render_widget(Paragraph::new("Hello World!"), frame.area());
    ///     Ok(())
    /// }
    /// # io::Result::Ok(())
    /// ```
    pub fn try_draw<F, E>(&mut self, render_callback: F) -> io::Result<CompletedFrame<'_>>
    where
        F: FnOnce(&mut Frame) -> Result<(), E>,
        E: Into<io::Error>,
    {
        // Autoresize - otherwise we get glitches if shrinking or potential desync between widgets
        // and the terminal (if growing), which may OOB.
        self.autoresize()?;

        let mut frame = self.get_frame();

        render_callback(&mut frame).map_err(Into::into)?;

        // We can't change the cursor position right away because we have to flush the frame to
        // stdout first. But we also can't keep the frame around, since it holds a &mut to
        // Buffer. Thus, we're taking the important data out of the Frame and dropping it.
        let cursor_position = OurFrame::from(frame).cursor_position;

        // Draw to stdout
        self.flush()?;

        match cursor_position {
            None => self.hide_cursor()?,
            Some(position) => {
                self.show_cursor()?;
                self.set_cursor_position(position)?;
            }
        }

        self.swap_buffers();

        // Flush
        self.backend.flush()?;

        let completed_frame = CompletedFrame {
            buffer: &self.buffers[1 - self.current],
            area: self.last_known_area,
            count: self.frame_count,
        };

        // increment frame count before returning from draw
        self.frame_count = self.frame_count.wrapping_add(1);

        Ok(completed_frame)
    }

    /// Hides the cursor.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()?;
        self.hidden_cursor = true;
        Ok(())
    }

    /// Shows the cursor.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.backend.show_cursor()?;
        self.hidden_cursor = false;
        Ok(())
    }

    /// Gets the current cursor position.
    ///
    /// This is the position of the cursor after the last draw call and is returned as a tuple of
    /// `(x, y)` coordinates.
    #[deprecated = "the method get_cursor_position indicates more clearly what about the cursor to get"]
    pub fn get_cursor(&mut self) -> io::Result<(u16, u16)> {
        let Position { x, y } = self.get_cursor_position()?;
        Ok((x, y))
    }

    /// Sets the cursor position.
    #[deprecated = "the method set_cursor_position indicates more clearly what about the cursor to set"]
    pub fn set_cursor(&mut self, x: u16, y: u16) -> io::Result<()> {
        self.set_cursor_position(Position { x, y })
    }

    /// Gets the current cursor position.
    ///
    /// This is the position of the cursor after the last draw call.
    pub fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.backend.get_cursor_position()
    }

    /// Sets the cursor position.
    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        self.backend.set_cursor_position(position)?;
        self.last_known_cursor_pos = position;
        Ok(())
    }

    /// Clear the terminal and force a full redraw on the next draw call.
    pub fn clear(&mut self) -> io::Result<()> {
        match self.viewport {
            Viewport::Fullscreen => self.backend.clear_region(ClearType::All)?,
            Viewport::Inline(_) => {
                self.backend
                    .set_cursor_position(self.viewport_area.as_position())?;
                self.backend.clear_region(ClearType::AfterCursor)?;
            }
            Viewport::Fixed(_) => {
                let area = self.viewport_area;
                for y in area.top()..area.bottom() {
                    self.backend.set_cursor_position(Position { x: 0, y })?;
                    self.backend.clear_region(ClearType::AfterCursor)?;
                }
            }
        }
        // Reset the back buffer to make sure the next update will redraw everything.
        self.buffers[1 - self.current].reset();
        self.reset_back_links();
        Ok(())
    }

    /// Reset the inactive (back) hyperlink layer in lockstep with the back cell
    /// buffer, so a stale link can never survive a buffer reset.
    fn reset_back_links(&mut self) {
        for id in self.link_ids[1 - self.current].iter_mut() {
            *id = 0;
        }
        self.link_tables[1 - self.current].clear();
    }

    /// Resets the back buffer without clearing the screen
    /// This is useful when you want to queue clear commands yourself
    pub fn reset_back_buffer(&mut self) {
        self.buffers[1 - self.current].reset();
        self.reset_back_links();
    }

    /// Clears the inactive buffer and swaps it with the current buffer
    pub fn swap_buffers(&mut self) {
        self.buffers[1 - self.current].reset();
        self.reset_back_links();
        self.current = 1 - self.current;
    }

    /// Queries the real size of the backend.
    pub fn size(&self) -> io::Result<Size> {
        self.backend.size()
    }

    /// Insert some content before the current inline viewport. This has no effect when the
    /// viewport is not inline.
    ///
    /// The `draw_fn` closure will be called to draw into a writable `Buffer` that is `height`
    /// lines tall. The content of that `Buffer` will then be inserted before the viewport.
    ///
    /// If the viewport isn't yet at the bottom of the screen, inserted lines will push it towards
    /// the bottom. Once the viewport is at the bottom of the screen, inserted lines will scroll
    /// the area of the screen above the viewport upwards.
    ///
    /// Before:
    /// ```ignore
    /// +---------------------+
    /// | pre-existing line 1 |
    /// | pre-existing line 2 |
    /// +---------------------+
    /// |       viewport      |
    /// +---------------------+
    /// |                     |
    /// |                     |
    /// +---------------------+
    /// ```
    ///
    /// After inserting 2 lines:
    /// ```ignore
    /// +---------------------+
    /// | pre-existing line 1 |
    /// | pre-existing line 2 |
    /// |   inserted line 1   |
    /// |   inserted line 2   |
    /// +---------------------+
    /// |       viewport      |
    /// +---------------------+
    /// +---------------------+
    /// ```
    ///
    /// After inserting 2 more lines:
    /// ```ignore
    /// +---------------------+
    /// | pre-existing line 2 |
    /// |   inserted line 1   |
    /// |   inserted line 2   |
    /// |   inserted line 3   |
    /// |   inserted line 4   |
    /// +---------------------+
    /// |       viewport      |
    /// +---------------------+
    /// ```
    ///
    /// If more lines are inserted than there is space on the screen, then the top lines will go
    /// directly into the terminal's scrollback buffer. At the limit, if the viewport takes up the
    /// whole screen, all lines will be inserted directly into the scrollback buffer.
    ///
    /// # Examples
    ///
    /// ## Insert a single line before the current viewport
    ///
    /// ```rust
    /// use ratatui::{
    ///     backend::TestBackend,
    ///     style::{Color, Style},
    ///     text::{Line, Span},
    ///     widgets::{Paragraph, Widget},
    ///     Terminal,
    /// };
    /// # let backend = TestBackend::new(10, 10);
    /// # let mut terminal = Terminal::new(backend).unwrap();
    /// terminal.insert_before(1, |buf| {
    ///     Paragraph::new(Line::from(vec![
    ///         Span::raw("This line will be added "),
    ///         Span::styled("before", Style::default().fg(Color::Blue)),
    ///         Span::raw(" the current viewport"),
    ///     ]))
    ///     .render(buf.area, buf);
    /// });
    /// ```
    pub fn insert_before<F>(&mut self, height: u16, draw_fn: F) -> io::Result<()>
    where
        F: FnOnce(&mut Buffer),
    {
        match self.viewport {
            #[cfg(feature = "scrolling-regions")]
            Viewport::Inline(_) => self.insert_before_scrolling_regions(height, draw_fn),
            #[cfg(not(feature = "scrolling-regions"))]
            Viewport::Inline(_) => self.insert_before_no_scrolling_regions(height, draw_fn),
            _ => Ok(()),
        }
    }

    /// Sets the height of an inline viewport and resizes it accordingly.
    ///
    /// This method only works with inline viewports. For other viewport types, it has no effect.
    /// The viewport will be resized to the new height, and the buffers will be cleared and
    /// reallocated to match the new size.
    ///
    /// # Arguments
    ///
    /// * `new_height` - The new height for the inline viewport in lines
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use ratatui::{Terminal, TerminalOptions, Viewport};
    ///
    /// let mut terminal = Terminal::with_options(backend, TerminalOptions {
    ///     viewport: Viewport::Inline(8),
    /// })?;
    ///
    /// // Later, resize the viewport to 12 lines
    /// terminal.set_viewport_height(12)?;
    /// ```
    pub fn set_viewport_height(&mut self, new_height: u16) -> io::Result<()> {
        if !matches!(self.viewport, Viewport::Inline(_)) {
            return Ok(());
        }
        // Judge grow-vs-shrink against the ACTUAL current viewport height, not
        // the stored `Viewport::Inline(height)`. The two can drift when a caller
        // repositions or resizes the inline viewport out-of-band via
        // `set_viewport_area` (minimal mode's content-anchored commit path
        // shrinks the viewport that way before `insert_before`). Comparing
        // against a stale stored height made a genuine grow read as a shrink,
        // skipping the grow-time `scroll_up` below — so the viewport's top never
        // moved up and the taller region ran off the bottom of the screen (an
        // opened dropdown's items landed off-screen). Keep the stored height in
        // lockstep with the area height on the way out so `resize`
        // (`compute_inline_size`) also sees the real height.
        let old_height = self.viewport_area.height;
        if let Viewport::Inline(height) = &mut self.viewport {
            *height = new_height;
        }
        if old_height == new_height {
            return Ok(());
        }

        self.clear()?;

        let new_y = match new_height.cmp(&old_height) {
            std::cmp::Ordering::Greater => {
                let overflow =
                    (self.viewport_area.y + new_height).saturating_sub(self.last_known_area.height);
                if overflow > 0 {
                    // Scroll the rows the taller viewport will cover up into the
                    // terminal's native scrollback *before* moving the viewport
                    // origin up, so committed content is preserved instead of
                    // overwritten. Minimal mode (and any inline consumer) relies
                    // on this when growing the viewport for an overlay near the
                    // bottom of the screen. The pager builds without the
                    // `scrolling-regions` feature; that variant is a separate
                    // (unused-by-the-pager) path left as a TODO.
                    #[cfg(not(feature = "scrolling-regions"))]
                    self.scroll_up(overflow)?;
                    self.viewport_area.y.saturating_sub(overflow)
                } else {
                    self.viewport_area.y
                }
            }
            _ => self.viewport_area.y,
        };

        self.set_viewport_area(Rect {
            height: new_height,
            y: new_y,
            ..self.viewport_area
        });
        self.clear()
    }

    /// Implement `Self::insert_before` using standard backend capabilities.
    #[cfg(not(feature = "scrolling-regions"))]
    fn insert_before_no_scrolling_regions(
        &mut self,
        height: u16,
        draw_fn: impl FnOnce(&mut Buffer),
    ) -> io::Result<()> {
        // The approach of this function is to first render all of the lines to insert into a
        // temporary buffer, and then to loop drawing chunks from the buffer to the screen. drawing
        // this buffer onto the screen.
        let area = Rect {
            x: 0,
            y: 0,
            width: self.viewport_area.width,
            height,
        };
        let mut buffer = Buffer::empty(area);
        draw_fn(&mut buffer);
        let mut buffer = buffer.content.as_slice();

        // Use i32 variables so we don't have worry about overflowed u16s when adding, or about
        // negative results when subtracting.
        let mut drawn_height: i32 = self.viewport_area.top().into();
        let mut buffer_height: i32 = height.into();
        let viewport_height: i32 = self.viewport_area.height.into();
        let screen_height: i32 = self.last_known_area.height.into();

        // The algorithm here is to loop, drawing large chunks of text (up to a screen-full at a
        // time), until the remainder of the buffer plus the viewport fits on the screen. We choose
        // this loop condition because it guarantees that we can write the remainder of the buffer
        // with just one call to Self::draw_lines().
        while buffer_height + viewport_height > screen_height {
            // We will draw as much of the buffer as possible on this iteration in order to make
            // forward progress. So we have:
            //
            //     to_draw = min(buffer_height, screen_height)
            //
            // We may need to scroll the screen up to make room to draw. We choose the minimal
            // possible scroll amount so we don't end up with the viewport sitting in the middle of
            // the screen when this function is done. The amount to scroll by is:
            //
            //     scroll_up = max(0, drawn_height + to_draw - screen_height)
            //
            // We want `scroll_up` to be enough so that, after drawing, we have used the whole
            // screen (drawn_height - scroll_up + to_draw = screen_height). However, there might
            // already be enough room on the screen to draw without scrolling (drawn_height +
            // to_draw <= screen_height). In this case, we just don't scroll at all.
            let to_draw = buffer_height.min(screen_height);
            let scroll_up = 0.max(drawn_height + to_draw - screen_height);
            self.scroll_up(scroll_up as u16)?;
            buffer = self.draw_lines((drawn_height - scroll_up) as u16, to_draw as u16, buffer)?;
            drawn_height += to_draw - scroll_up;
            buffer_height -= to_draw;
        }

        // There is now enough room on the screen for the remaining buffer plus the viewport,
        // though we may still need to scroll up some of the existing text first. It's possible
        // that by this point we've drained the buffer, but we may still need to scroll up to make
        // room for the viewport.
        //
        // We want to scroll up the exact amount that will leave us completely filling the screen.
        // However, it's possible that the viewport didn't start on the bottom of the screen and
        // the added lines weren't enough to push it all the way to the bottom. We deal with this
        // case by just ensuring that our scroll amount is non-negative.
        //
        // We want:
        //   screen_height = drawn_height - scroll_up + buffer_height + viewport_height
        // Or, equivalently:
        //   scroll_up = drawn_height + buffer_height + viewport_height - screen_height
        let scroll_up = 0.max(drawn_height + buffer_height + viewport_height - screen_height);
        self.scroll_up(scroll_up as u16)?;
        self.draw_lines(
            (drawn_height - scroll_up) as u16,
            buffer_height as u16,
            buffer,
        )?;
        drawn_height += buffer_height - scroll_up;

        self.set_viewport_area(Rect {
            y: drawn_height as u16,
            ..self.viewport_area
        });

        // Clear the viewport off the screen. We didn't clear earlier for two reasons. First, it
        // wasn't necessary because the buffer we drew out of isn't sparse, so it overwrote
        // whatever was on the screen. Second, there is a weird bug with tmux where a full screen
        // clear plus immediate scrolling causes some garbage to go into the scrollback.
        self.clear()?;

        Ok(())
    }

    /// Implement `Self::insert_before` using scrolling regions.
    ///
    /// If a terminal supports scrolling regions, it means that we can define a subset of rows of
    /// the screen, and then tell the terminal to scroll up or down just within that region. The
    /// rows outside of the region are not affected.
    ///
    /// This function utilizes this feature to avoid having to redraw the viewport. This is done
    /// either by splitting the screen at the top of the viewport, and then creating a gap by
    /// either scrolling the viewport down, or scrolling the area above it up. The lines to insert
    /// are then drawn into the gap created.
    #[cfg(feature = "scrolling-regions")]
    fn insert_before_scrolling_regions(
        &mut self,
        mut height: u16,
        draw_fn: impl FnOnce(&mut Buffer),
    ) -> io::Result<()> {
        // The approach of this function is to first render all of the lines to insert into a
        // temporary buffer, and then to loop drawing chunks from the buffer to the screen. drawing
        // this buffer onto the screen.
        let area = Rect {
            x: 0,
            y: 0,
            width: self.viewport_area.width,
            height,
        };
        let mut buffer = Buffer::empty(area);
        draw_fn(&mut buffer);
        let mut buffer = buffer.content.as_slice();

        // Handle the special case where the viewport takes up the whole screen.
        if self.viewport_area.height == self.last_known_area.height {
            // "Borrow" the top line of the viewport. Draw over it, then immediately scroll it into
            // scrollback. Do this repeatedly until the whole buffer has been put into scrollback.
            let mut first = true;
            while !buffer.is_empty() {
                buffer = if first {
                    self.draw_lines(0, 1, buffer)?
                } else {
                    self.draw_lines_over_cleared(0, 1, buffer)?
                };
                first = false;
                self.backend.scroll_region_up(0..1, 1)?;
            }

            // Redraw the top line of the viewport.
            let width = self.viewport_area.width as usize;
            let top_line = self.buffers[1 - self.current].content[0..width].to_vec();
            self.draw_lines_over_cleared(0, 1, &top_line)?;
            return Ok(());
        }

        // Handle the case where the viewport isn't yet at the bottom of the screen.
        {
            let viewport_top = self.viewport_area.top();
            let viewport_bottom = self.viewport_area.bottom();
            let screen_bottom = self.last_known_area.bottom();
            if viewport_bottom < screen_bottom {
                let to_draw = height.min(screen_bottom - viewport_bottom);
                self.backend
                    .scroll_region_down(viewport_top..viewport_bottom + to_draw, to_draw)?;
                buffer = self.draw_lines_over_cleared(viewport_top, to_draw, buffer)?;
                self.set_viewport_area(Rect {
                    y: viewport_top + to_draw,
                    ..self.viewport_area
                });
                height -= to_draw;
            }
        }

        let viewport_top = self.viewport_area.top();
        while height > 0 {
            let to_draw = height.min(viewport_top);
            self.backend.scroll_region_up(0..viewport_top, to_draw)?;
            buffer = self.draw_lines_over_cleared(viewport_top - to_draw, to_draw, buffer)?;
            height -= to_draw;
        }

        Ok(())
    }

    /// Draw lines at the given vertical offset. The slice of cells must contain enough cells
    /// for the requested lines. A slice of the unused cells are returned.
    fn draw_lines<'a>(
        &mut self,
        y_offset: u16,
        lines_to_draw: u16,
        cells: &'a [Cell],
    ) -> io::Result<&'a [Cell]> {
        let width: usize = self.last_known_area.width.into();
        let (to_draw, remainder) = cells.split_at(width * lines_to_draw as usize);
        if lines_to_draw > 0 {
            let iter = to_draw
                .iter()
                .enumerate()
                .map(|(i, c)| ((i % width) as u16, y_offset + (i / width) as u16, c));
            self.backend.draw(iter)?;
            self.backend.flush()?;
        }
        Ok(remainder)
    }

    /// Draw lines at the given vertical offset, assuming that the lines they are replacing on the
    /// screen are cleared. The slice of cells must contain enough cells for the requested lines. A
    /// slice of the unused cells are returned.
    #[cfg(feature = "scrolling-regions")]
    fn draw_lines_over_cleared<'a>(
        &mut self,
        y_offset: u16,
        lines_to_draw: u16,
        cells: &'a [Cell],
    ) -> io::Result<&'a [Cell]> {
        let width: usize = self.last_known_area.width.into();
        let (to_draw, remainder) = cells.split_at(width * lines_to_draw as usize);
        if lines_to_draw > 0 {
            let area = Rect::new(0, y_offset, width as u16, y_offset + lines_to_draw);
            let old = Buffer::empty(area);
            let new = Buffer {
                area,
                content: to_draw.to_vec(),
            };
            self.backend.draw(old.diff(&new).into_iter())?;
            self.backend.flush()?;
        }
        Ok(remainder)
    }

    /// Scroll the whole screen up by the given number of lines.
    #[cfg(not(feature = "scrolling-regions"))]
    fn scroll_up(&mut self, lines_to_scroll: u16) -> io::Result<()> {
        if lines_to_scroll > 0 {
            self.set_cursor_position(Position::new(
                0,
                self.last_known_area.height.saturating_sub(1),
            ))?;
            self.backend.append_lines(lines_to_scroll)?;
        }
        Ok(())
    }
}

/// Like [`Buffer::diff`] but safe for buffers whose `width * height > u16::MAX`.
///
/// Upstream ratatui (0.29) `Buffer::pos_of()` casts the flat index to `u16`
/// before dividing by width, silently wrapping at 65 535.  This replacement
/// performs the division in `usize` so terminals with >65 535 cells render
/// correctly.
fn diff_large<'a>(prev: &Buffer, next: &'a Buffer) -> Vec<(u16, u16, &'a Cell)> {
    let previous_buffer = &prev.content;
    let next_buffer = &next.content;

    let area = prev.area;
    let width = area.width as usize;

    let mut updates: Vec<(u16, u16, &Cell)> = Vec::new();
    let mut invalidated: usize = 0;
    let mut to_skip: usize = 0;

    for (i, (current, previous)) in next_buffer.iter().zip(previous_buffer.iter()).enumerate() {
        if !current.skip && (current != previous || invalidated > 0) && to_skip == 0 {
            // Safe coordinate conversion: divide in usize, then narrow to u16.
            let x = area.x + (i % width) as u16;
            let y = area.y + (i / width) as u16;
            updates.push((x, y, &next_buffer[i]));
        }

        to_skip = current.symbol().width().saturating_sub(1);

        let affected_width = std::cmp::max(current.symbol().width(), previous.symbol().width());
        invalidated = std::cmp::max(affected_width, invalidated).saturating_sub(1);
    }
    updates
}

/// Like [`diff_large`] but a cell is also considered changed when its hyperlink
/// changed between the previous and current frame (even if the glyph/style is
/// identical). This is what makes OSC 8 links participate in the frame diff:
/// adding, removing, or retargeting a link forces the affected cells to be
/// rewritten so the terminal's link state stays in sync.
#[allow(clippy::too_many_arguments)]
fn diff_large_with_links<'a>(
    prev: &Buffer,
    next: &'a Buffer,
    prev_ids: &[u32],
    next_ids: &[u32],
    prev_table: &[LinkRef],
    next_table: &[LinkRef],
) -> Vec<(u16, u16, &'a Cell)> {
    let previous_buffer = &prev.content;
    let next_buffer = &next.content;

    // `prev.area == next.area` always (resized together); match `diff_large`.
    let area = prev.area;
    let width = area.width as usize;

    let mut updates: Vec<(u16, u16, &Cell)> = Vec::new();
    let mut invalidated: usize = 0;
    let mut to_skip: usize = 0;

    for (i, (current, previous)) in next_buffer.iter().zip(previous_buffer.iter()).enumerate() {
        let link_changed =
            resolve_link(next_ids, next_table, i) != resolve_link(prev_ids, prev_table, i);
        if !current.skip && (current != previous || link_changed || invalidated > 0) && to_skip == 0
        {
            let x = area.x + (i % width) as u16;
            let y = area.y + (i / width) as u16;
            updates.push((x, y, &next_buffer[i]));
        }

        to_skip = current.symbol().width().saturating_sub(1);

        let affected_width = std::cmp::max(current.symbol().width(), previous.symbol().width());
        invalidated = std::cmp::max(affected_width, invalidated).saturating_sub(1);
    }
    updates
}

/// Emit a frame's cell updates with OSC 8 hyperlinks.
///
/// Updates are grouped into maximal runs that resolve to the same link, and the
/// upstream [`Backend::draw`] is reused per run (so all SGR / wide-char / cursor
/// handling is unchanged); each linked run is wrapped in one OSC 8 open/close.
/// Keeping a link open across `draw`'s internal cursor moves is correct because
/// OSC 8 is a sticky terminal mode — only the written cells inherit it, and
/// unchanged cells in any gap keep whatever link they already had.
fn emit_frame_with_links<B: Backend + Write>(
    backend: &mut B,
    updates: &[(u16, u16, &Cell)],
    cur_ids: &[u32],
    cur_table: &[LinkRef],
    area: Rect,
) -> io::Result<()> {
    let width = area.width as usize;
    let resolve = |x: u16, y: u16| -> Option<&LinkRef> {
        let idx = (y - area.y) as usize * width + (x - area.x) as usize;
        resolve_link(cur_ids, cur_table, idx)
    };

    let mut i = 0;
    while i < updates.len() {
        // Invariant: `i < updates.len()` (loop guard) and below `i < j <=
        // updates.len()`, so `updates[i]` and the slice `updates[i..j]` never
        // panic. `resolve` indexes via `cur_ids.get(..)` (bounds-safe) and the
        // coordinates come from `diff_large_with_links` as `area.{x,y} + ..`, so
        // `(y - area.y)` / `(x - area.x)` cannot underflow.
        let (x, y, _) = updates[i];
        let link = resolve(x, y);

        // Extend the run while the resolved link is identical.
        let mut j = i + 1;
        while j < updates.len() {
            let (nx, ny, _) = updates[j];
            if resolve(nx, ny) != link {
                break;
            }
            j += 1;
        }

        if let Some(link) = link {
            write_osc8_open(backend, &link.url, link.id)?;
            // Always close the hyperlink, even if the cell draw errors, so a
            // dangling OSC 8 open can never be flushed to the terminal.
            let drawn = backend.draw(updates[i..j].iter().copied());
            write_osc8_close(backend)?;
            drawn?;
        } else {
            backend.draw(updates[i..j].iter().copied())?;
        }

        i = j;
    }
    Ok(())
}

fn compute_inline_size<B: Backend>(
    backend: &mut B,
    height: u16,
    size: Size,
    offset_in_previous_viewport: u16,
) -> io::Result<(Rect, Position)> {
    let pos = backend.get_cursor_position()?;
    let mut row = pos.y;

    let max_height = size.height.min(height);

    let lines_after_cursor = height
        .saturating_sub(offset_in_previous_viewport)
        .saturating_sub(1);

    backend.append_lines(lines_after_cursor)?;

    let available_lines = size.height.saturating_sub(row).saturating_sub(1);
    let missing_lines = lines_after_cursor.saturating_sub(available_lines);
    if missing_lines > 0 {
        row = row.saturating_sub(missing_lines);
    }
    row = row.saturating_sub(offset_in_previous_viewport);

    Ok((
        Rect {
            x: 0,
            y: row,
            width: size.width,
            height: max_height,
        },
        pos,
    ))
}

impl<B: Backend> Terminal<B> {
    /// HACK: this is added
    pub fn viewport_area(&self) -> Rect {
        self.viewport_area
    }

    /// The full terminal area as last seen by `autoresize` (i.e. the whole
    /// screen, not just the inline viewport). This is the exact value
    /// `set_viewport_height`'s grow/shrink math uses, so callers that size the
    /// viewport relative to the screen (e.g. the minimal-mode overlay host)
    /// should read it here for consistency.
    pub fn last_known_area(&self) -> Rect {
        self.last_known_area
    }

    /// Switch the viewport kind in place, keeping the backend alive.
    ///
    /// `Viewport::Inline` issues a cursor-position query (caller must be the
    /// only stdin reader), and both buffers reset — follow with a full redraw.
    pub fn set_viewport(&mut self, viewport: Viewport) -> io::Result<()> {
        let area = match viewport {
            Viewport::Fullscreen | Viewport::Inline(_) => {
                Rect::from((Position::ORIGIN, self.backend.size()?))
            }
            Viewport::Fixed(area) => area,
        };
        let (viewport_area, cursor_pos) = match viewport {
            Viewport::Fullscreen => (area, Position::ORIGIN),
            Viewport::Inline(height) => {
                compute_inline_size(&mut self.backend, height, area.as_size(), 0)?
            }
            Viewport::Fixed(area) => (area, area.as_position()),
        };
        let link_len = (viewport_area.width as usize) * (viewport_area.height as usize);
        self.viewport = viewport;
        self.viewport_area = viewport_area;
        self.last_known_area = area;
        self.last_known_cursor_pos = cursor_pos;
        self.buffers = [Buffer::empty(viewport_area), Buffer::empty(viewport_area)];
        self.current = 0;
        for layer in self.link_ids.iter_mut() {
            layer.clear();
            layer.resize(link_len, 0);
        }
        self.link_tables[0].clear();
        self.link_tables[1].clear();
        Ok(())
    }

    /// HACK: this is made pub
    pub fn set_viewport_area(&mut self, area: Rect) {
        self.buffers[self.current].resize(area);
        self.buffers[1 - self.current].resize(area);
        let len = (area.width as usize) * (area.height as usize);
        for layer in self.link_ids.iter_mut() {
            layer.clear();
            layer.resize(len, 0);
        }
        self.link_tables[0].clear();
        self.link_tables[1].clear();
        self.viewport_area = area;
    }
}

#[cfg(test)]
mod inline_resize_tests {
    use ratatui::backend::TestBackend;
    use ratatui::{TerminalOptions, Viewport, layout::Rect};

    use super::Terminal;

    fn full_height_inline(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::with_options(
            TestBackend::new(width, height),
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )
        .unwrap()
    }

    /// A full-height inline viewport (the alt-screen-unavailable case used under
    /// Zellij / tmux control mode / `--no-alt-screen`) must GROW to fill the
    /// terminal when it is enlarged.
    ///
    /// Regression test for the bug where the viewport height was clamped to the
    /// startup height (truncated at the bottom) while the width still tracked the
    /// resize.
    #[test]
    fn inline_full_height_grows_with_terminal() {
        let mut terminal = full_height_inline(80, 24);
        assert_eq!(terminal.viewport_area(), Rect::new(0, 0, 80, 24));

        terminal.backend_mut().resize(80, 40);
        terminal.autoresize().unwrap();

        // Both dimensions track the new terminal size, anchored at the top.
        assert_eq!(terminal.viewport_area(), Rect::new(0, 0, 80, 40));
    }

    /// Width-only growth keeps working (this part was never broken).
    #[test]
    fn inline_full_height_grows_in_width() {
        let mut terminal = full_height_inline(80, 24);

        terminal.backend_mut().resize(120, 24);
        terminal.autoresize().unwrap();

        assert_eq!(terminal.viewport_area(), Rect::new(0, 0, 120, 24));
    }

    /// Shrinking must also track the terminal and must not position the viewport
    /// off-screen (which previously panicked the strict `TestBackend` buffer and
    /// would leave a real terminal's UI invisible/garbled).
    #[test]
    fn inline_full_height_shrinks_with_terminal() {
        let mut terminal = full_height_inline(80, 40);

        terminal.backend_mut().resize(80, 20);
        terminal.autoresize().unwrap();

        assert_eq!(terminal.viewport_area(), Rect::new(0, 0, 80, 20));
    }

    /// `set_viewport` must match what a fresh `with_options` would compute.
    #[test]
    fn set_viewport_round_trips_fullscreen_and_inline() {
        let mut terminal = Terminal::with_options(
            TestBackend::new(80, 24),
            TerminalOptions {
                viewport: Viewport::Fullscreen,
            },
        )
        .unwrap();
        assert_eq!(terminal.viewport_area(), Rect::new(0, 0, 80, 24));

        terminal.set_viewport(Viewport::Inline(10)).unwrap();
        let inline_area = terminal.viewport_area();
        assert_eq!(inline_area.width, 80);
        assert_eq!(inline_area.height, 10);
        terminal
            .draw(|f| {
                let area = f.area();
                assert_eq!(area.height, 10);
            })
            .unwrap();

        terminal.set_viewport(Viewport::Fullscreen).unwrap();
        assert_eq!(terminal.viewport_area(), Rect::new(0, 0, 80, 24));
        terminal
            .draw(|f| {
                assert_eq!(f.area(), Rect::new(0, 0, 80, 24));
            })
            .unwrap();
    }

    /// Growth after a shrink must expand again — the viewport tracks the live
    /// terminal size in both directions, repeatedly.
    #[test]
    fn inline_full_height_tracks_across_shrink_then_grow() {
        let mut terminal = full_height_inline(80, 30);

        terminal.backend_mut().resize(80, 10);
        terminal.autoresize().unwrap();
        assert_eq!(terminal.viewport_area(), Rect::new(0, 0, 80, 10));

        terminal.backend_mut().resize(100, 50);
        terminal.autoresize().unwrap();
        assert_eq!(terminal.viewport_area(), Rect::new(0, 0, 100, 50));
    }

    /// A *small* inline viewport (height < terminal height, anchored near the
    /// bottom) must NOT be forced to full height — it keeps the standard
    /// `compute_inline_size` behavior, so the full-height special-case does not
    /// over-apply.
    #[test]
    fn small_inline_viewport_is_not_forced_full() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(3),
            },
        )
        .unwrap();
        assert_eq!(terminal.viewport_area().height, 3);

        terminal.backend_mut().resize(120, 40);
        terminal.autoresize().unwrap();

        // The full-height special-case keys off the viewport spanning the whole
        // terminal (height >= terminal height). A small inline viewport does not,
        // so its height stays clamped to the small inline target while the width
        // tracks the resize — i.e. it keeps the standard `compute_inline_size`
        // behavior and is not ballooned to full height.
        assert_eq!(terminal.viewport_area().height, 3);
        assert_eq!(terminal.viewport_area().width, 120);
    }

    /// Fullscreen viewports already track the full size; behavior is unchanged.
    #[test]
    fn fullscreen_tracks_terminal() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        assert_eq!(terminal.viewport_area(), Rect::new(0, 0, 80, 24));

        terminal.backend_mut().resize(80, 40);
        terminal.autoresize().unwrap();

        assert_eq!(terminal.viewport_area(), Rect::new(0, 0, 80, 40));
    }
}
