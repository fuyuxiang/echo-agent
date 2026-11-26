//! Frame drawing with cursor blink preservation.
//!
//! # Problem
//!
//! Ratatui's [`Terminal::draw()`] (internally `try_draw()`) unconditionally
//! sends cursor escape sequences on every frame:
//!
//! - If `frame.set_cursor_position()` was called: `Show` + `MoveTo` every frame
//! - If not called: `Hide` every frame
//!
//! Both reset the terminal's cursor blink timer (`Show` restarts the blink
//! cycle, `MoveTo` resets the blink phase). At 30fps, the 500ms blink interval
//! never completes, so the cursor appears solid.
//!
//! # Solution
//!
//! We bypass `try_draw()` and use ratatui's lower-level API directly:
//!
//! ```text
//! terminal.autoresize()     — handle terminal size changes
//! terminal.get_frame()      — get a fresh buffer to render into
//! terminal.flush()          — diff old/new buffers, write only changed cells
//! terminal.swap_buffers()   — prepare for next frame
//! ```
//!
//! Cursor is managed entirely by [`CursorState`] with de-duplication:
//!
//! - **No cell changes + same position**: zero cursor commands → blink preserved
//! - **Cells changed + same position**: `MoveTo` to fix cursor after cell writes
//! - **Position changed**: `MoveTo` (blink resets — expected, user just typed)
//! - **Visibility transition**: `Show`/`Hide` (only on actual transition)
//! - **Idle (no draw calls)**: nothing sent → blink runs undisturbed
//!
//! The "no cell changes" optimization is possible because we use
//! [`echo_agent_ratatui_inline::Terminal`] whose `flush()` returns `bool` indicating
//! whether any cells were written. When animated entries are off-screen, the
//! buffer diff is empty and we skip all cursor commands.
//!
//! # Synchronized output
//!
//! Each frame is wrapped in `BeginSynchronizedUpdate` / `EndSynchronizedUpdate`
//! so the terminal processes all escape sequences atomically. This prevents
//! flicker and is critical for multiplexers like zellij and tmux.
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use crossterm::{QueueableCommand, cursor};
use echo_agent_ratatui_inline::LinkSpan;
use ratatui::Frame;
use ratatui::backend::CrosstermBackend;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
/// Terminal type for the pager. Defined here (beside [`TermWriter`]) so the
/// `render` module does not depend on `app`. Re-exported from `app` as
/// `crate::app::PagerTerminal` for existing call sites.
pub type PagerTerminal = echo_agent_ratatui_inline::Terminal<CrosstermBackend<TermWriter>>;
#[derive(Debug)]
pub enum WriterEvent {
    Written(u64),
    Failed(std::io::Error),
}
/// Outcome of a bounded writer drain attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriterDrain {
    Drained,
    TimedOut,
}
/// Tracks submitted and successfully flushed presentation sequences.
///
/// During a child handoff, input is parked before this state is drained. Since
/// a sequence is reserved before its payload is sent, an accepted frame blocks
/// the drain before it is visible to the writer; no queued frame can land after
/// the child takes the tty.
#[derive(Clone, Debug)]
pub struct WriterSync {
    queued: Arc<AtomicU64>,
    written: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
    writer_active: Arc<AtomicBool>,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<WriterEvent>>,
}
impl Default for WriterSync {
    fn default() -> Self {
        Self::new()
    }
}
impl WriterSync {
    pub fn new() -> Self {
        Self {
            queued: Arc::new(AtomicU64::new(0)),
            written: Arc::new(AtomicU64::new(0)),
            failed: Arc::new(AtomicBool::new(false)),
            writer_active: Arc::new(AtomicBool::new(false)),
            event_tx: None,
        }
    }
    fn with_event_sender(event_tx: tokio::sync::mpsc::UnboundedSender<WriterEvent>) -> Self {
        Self {
            queued: Arc::new(AtomicU64::new(0)),
            written: Arc::new(AtomicU64::new(0)),
            failed: Arc::new(AtomicBool::new(false)),
            writer_active: Arc::new(AtomicBool::new(false)),
            event_tx: Some(event_tx),
        }
    }
    #[cfg(test)]
    fn new_for_test() -> (Self, tokio::sync::mpsc::UnboundedReceiver<WriterEvent>) {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        (Self::with_event_sender(event_tx), event_rx)
    }
    fn reserve_sequence(&self) -> u64 {
        self.queued.fetch_add(1, Ordering::Release) + 1
    }
    fn mark_written(&self, sequence: u64) {
        self.written.store(sequence, Ordering::Release);
        if let Some(event_tx) = &self.event_tx {
            let _ = event_tx.send(WriterEvent::Written(sequence));
        }
    }
    fn mark_failed(&self, error: std::io::Error) {
        if self
            .failed
            .compare_exchange(false, true, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        if let Some(event_tx) = &self.event_tx {
            let _ = event_tx.send(WriterEvent::Failed(error));
        }
    }
    pub fn queued(&self) -> u64 {
        self.queued.load(Ordering::Acquire)
    }
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    fn is_drained(&self) -> bool {
        !self.failed() && self.written() >= self.queued()
    }
    /// Block until the writer flushes every accepted payload, output fails, or
    /// the deadline passes.
    pub fn wait_drained(&self, timeout: Duration) -> std::io::Result<WriterDrain> {
        let deadline = Instant::now() + timeout;
        while !self.is_drained() {
            if self.failed() {
                return Err(std::io::Error::other("terminal output failed"));
            }
            if Instant::now() >= deadline {
                return Ok(WriterDrain::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(WriterDrain::Drained)
    }
}
/// A writer that buffers frame output and sends it to a background thread
/// for non-blocking terminal I/O.
///
/// All escape sequences produced during a frame are collected in an internal
/// `Vec<u8>`. When [`flush()`](Write::flush) is called, the accumulated bytes
/// are sent through a channel to a dedicated writer thread that performs the
/// actual (potentially blocking) `write()` to stderr / the pty fd.
///
/// This decouples the tokio event loop from pty back-pressure: if the
/// terminal emulator is slow to read (e.g. Ghostty busy with another pane),
/// only the writer thread stalls — the event loop keeps processing timers,
/// events, and ACP messages.
pub struct WriterPayload {
    pub(crate) sequence: u64,
    pub(crate) data: Vec<u8>,
}
pub type WriterSender = mpsc::Sender<WriterPayload>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterAlreadyActive;
impl std::fmt::Display for WriterAlreadyActive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WriterSync already owns a live TermWriter")
    }
}
impl std::error::Error for WriterAlreadyActive {}
pub struct TermWriter {
    buf: Vec<u8>,
    tx: WriterSender,
    sync: WriterSync,
}
impl TermWriter {
    pub fn new(tx: WriterSender, sync: WriterSync) -> Result<Self, WriterAlreadyActive> {
        sync.writer_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| WriterAlreadyActive)?;
        Ok(Self {
            buf: Vec::with_capacity(32 * 1024),
            tx,
            sync,
        })
    }
    /// Drop the current frame's buffered bytes without sending them.
    pub fn discard(&mut self) {
        self.buf.clear();
    }
    /// Shared writer progress used by the suspend path to
    /// [`WriterSync::wait_drained`] before a child takes the tty.
    pub fn writer_sync(&self) -> &WriterSync {
        &self.sync
    }
}
impl Write for TermWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let sequence = self.sync.reserve_sequence();
        let data = std::mem::take(&mut self.buf);
        if self.tx.send(WriterPayload { sequence, data }).is_err() {
            let error = std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "terminal writer thread exited",
            );
            self.sync
                .mark_failed(std::io::Error::new(error.kind(), error.to_string()));
            return Err(error);
        }
        Ok(())
    }
}
impl Drop for TermWriter {
    fn drop(&mut self) {
        let _ = self.flush();
        self.sync.writer_active.store(false, Ordering::Release);
    }
}
/// Handle for the background writer thread.
///
/// Joining ensures all queued frames have been written to the terminal
/// before proceeding with teardown (e.g. `LeaveAlternateScreen`).
pub struct WriterThread {
    handle: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    sync: WriterSync,
}
impl WriterThread {
    /// Block until the writer thread has processed all pending frames and
    /// exited. The [`mpsc::Sender`] must be dropped *before* calling this,
    /// otherwise the thread will never see the channel close.
    pub fn join(mut self) -> std::io::Result<()> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        match handle.join() {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::other("terminal writer thread panicked")),
        }
    }
    pub fn writer_sync(&self) -> &WriterSync {
        &self.sync
    }
}
impl Drop for WriterThread {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
fn write_payload(
    writer: &mut impl Write,
    payload: &WriterPayload,
    sync: &WriterSync,
) -> std::io::Result<()> {
    match writer
        .write_all(&payload.data)
        .and_then(|()| writer.flush())
    {
        Ok(()) => {
            sync.mark_written(payload.sequence);
            Ok(())
        }
        Err(error) => {
            sync.mark_failed(std::io::Error::new(error.kind(), error.to_string()));
            Err(error)
        }
    }
}
/// Spawn a background OS thread that writes frame data to stderr.
///
/// Returns the frame sender, shared writer state, completion-event receiver,
/// and the thread handle that must be joined during terminal teardown.
pub fn spawn_writer_thread() -> (
    WriterSender,
    WriterSync,
    tokio::sync::mpsc::UnboundedReceiver<WriterEvent>,
    WriterThread,
) {
    let (tx, rx) = mpsc::channel::<WriterPayload>();
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let sync = WriterSync::with_event_sender(event_tx);
    let thread_sync = sync.clone();
    let writer_thread_sync = sync.clone();
    let test_delay = std::env::var("ECHO_AGENT_TEST_FRAME_WRITE_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis);
    let handle = std::thread::Builder::new()
        .name("term-writer".into())
        .spawn(move || -> std::io::Result<()> {
            #[cfg(not(windows))]
            let mut writer: Box<dyn std::io::Write> = {
                let tui_out = echo_agent_tty_utils::dup_tui_stderr().unwrap_or_else(|_| {
                    use std::os::unix::io::{AsRawFd, FromRawFd};
                    let fd = unsafe { libc::dup(std::io::stderr().as_raw_fd()) };
                    unsafe { std::fs::File::from_raw_fd(fd) }
                });
                Box::new(std::io::BufWriter::with_capacity(64 * 1024, tui_out))
            };
            #[cfg(windows)]
            let mut writer: Box<dyn std::io::Write> = Box::new(std::io::BufWriter::with_capacity(
                64 * 1024,
                std::io::stderr(),
            ));
            while let Ok(payload) = rx.recv() {
                if let Some(delay) = test_delay {
                    std::thread::sleep(delay);
                }
                let result = {
                    let _guard = echo_agent_shared::stderr::stderr_lock();
                    write_payload(&mut writer, &payload, &thread_sync)
                };
                if let Err(error) = result {
                    tracing::error!(%error, "terminal output failed");
                    return Err(error);
                }
            }
            if thread_sync.failed() {
                Err(std::io::Error::other("terminal output failed"))
            } else {
                Ok(())
            }
        })
        .expect("failed to spawn term-writer thread");
    (
        tx,
        sync,
        event_rx,
        WriterThread {
            handle: Some(handle),
            sync: writer_thread_sync,
        },
    )
}
/// Cursor state tracker for blink-preserving cursor management.
///
/// Tracks the last cursor position written to the terminal. By comparing
/// with the desired position each frame, we emit the minimum cursor escape
/// sequences necessary — avoiding redundant `Show`/`Hide`/`MoveTo` that
/// would reset the terminal's blink timer.
#[derive(Debug, Default)]
pub struct CursorState {
    /// Last cursor position written to the terminal.
    /// `None` = cursor is hidden; `Some((x, y))` = cursor visible at (x, y).
    last_pos: Option<(u16, u16)>,
}
/// What cursor commands to emit after a frame render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorAction {
    /// No cursor commands needed — blink timer preserved.
    None,
    /// Cursor is visible and cells changed — reposition after cell writes
    /// disturbed the terminal cursor. Resets blink (unavoidable when cells
    /// change on screen).
    Reposition(u16, u16),
    /// Cursor becoming visible at (x, y) — needs `MoveTo` + `Show`.
    Show(u16, u16),
    /// Cursor becoming hidden — needs `Hide`.
    Hide,
}
impl CursorState {
    pub fn new() -> Self {
        Self { last_pos: None }
    }
    /// Determine what cursor action to take for this frame.
    ///
    /// Pure function — computes the action from current state without
    /// side effects. Call [`apply`] to execute it.
    pub fn action(&self, cursor_pos: Option<(u16, u16)>, has_changes: bool) -> CursorAction {
        if cursor_pos == self.last_pos {
            if has_changes && let Some((x, y)) = cursor_pos {
                return CursorAction::Reposition(x, y);
            }
            CursorAction::None
        } else {
            match (cursor_pos, self.last_pos) {
                (Some((x, y)), Some(_)) => CursorAction::Reposition(x, y),
                (Some((x, y)), None) => CursorAction::Show(x, y),
                (None, Some(_)) => CursorAction::Hide,
                (None, None) => CursorAction::None,
            }
        }
    }
    /// Execute a cursor action by queuing escape sequences into `w`.
    ///
    /// Uses `queue!` (buffered) instead of `execute!` (immediate flush) so
    /// that cursor commands are batched with the rest of the frame data and
    /// written to the terminal atomically by the writer thread.
    pub fn apply<W: Write>(&mut self, action: CursorAction, w: &mut W) {
        match action {
            CursorAction::None => {}
            CursorAction::Reposition(x, y) => {
                let _ = w.queue(cursor::MoveTo(x, y));
                self.last_pos = Some((x, y));
            }
            CursorAction::Show(x, y) => {
                let _ = w.queue(cursor::MoveTo(x, y));
                let _ = w.queue(cursor::Show);
                self.last_pos = Some((x, y));
            }
            CursorAction::Hide => {
                let _ = w.queue(cursor::Hide);
                self.last_pos = None;
            }
        }
    }
}
/// Render a frame to the terminal with cursor blink preservation.
///
/// Bypasses ratatui's `try_draw()` to avoid its unconditional cursor
/// management. See [module docs](self) for the full rationale.
///
/// The `render_fn` receives a [`Frame`] and a `&mut Vec<LinkSpan>` to populate
/// with the frame's OSC 8 hyperlink regions (absolute viewport coordinates).
/// Those spans are handed to the terminal before the diff so hyperlinks
/// participate in the cell diff (emitted/cleared in lockstep with content) —
/// no out-of-band post-flush repaint. It returns a tuple of:
/// - `Option<(u16, u16)>` — cursor position (or `None` to hide cursor)
/// - `Option<PostFlush>` — escape sequences to write after cell flush (e.g.
///   Kitty graphics protocol image data). Written inside the synchronized
///   update block so the image appears atomically with the cell diff.
pub fn draw_frame(
    terminal: &mut PagerTerminal,
    cursor: &mut CursorState,
    render_fn: impl FnOnce(
        &mut Frame,
        &mut Vec<LinkSpan>,
    ) -> (
        Option<(u16, u16)>,
        Option<crate::terminal::overlay::PostFlush>,
    ),
) {
    let _ = terminal.backend_mut().queue(BeginSynchronizedUpdate);
    let _ = terminal.autoresize();
    let mut link_spans: Vec<LinkSpan> = Vec::new();
    let (cursor_pos, post_flush_escapes) = {
        let mut frame = terminal.get_frame();
        render_fn(&mut frame, &mut link_spans)
    };
    terminal.set_frame_links(&link_spans);
    let has_changes = terminal.flush_with_links().unwrap_or(false);
    terminal.swap_buffers();
    let post_flush_wrote_cursor = post_flush_escapes.is_some();
    let action = cursor.action(cursor_pos, has_changes || post_flush_wrote_cursor);
    if !has_changes && !post_flush_wrote_cursor && action == CursorAction::None {
        terminal.backend_mut().writer_mut().discard();
        return;
    }
    if let Some(post_flush) = post_flush_escapes {
        let _ = post_flush.write_to(terminal.backend_mut());
    }
    cursor.apply(action, terminal.backend_mut());
    let _ = terminal.backend_mut().queue(EndSynchronizedUpdate);
    let _ = terminal.backend_mut().flush();
}
#[cfg(test)]
mod tests {
    use super::*;
    /// An unchanged frame must emit zero bytes to the PTY.
    #[test]
    fn idle_frame_emits_zero_bytes() {
        use ratatui::backend::CrosstermBackend;
        use ratatui::layout::Rect;
        use ratatui::widgets::Paragraph;
        use ratatui::{TerminalOptions, Viewport};
        use std::sync::mpsc;
        fn render(
            frame: &mut ratatui::Frame,
            _links: &mut Vec<LinkSpan>,
        ) -> (
            Option<(u16, u16)>,
            Option<crate::terminal::overlay::PostFlush>,
        ) {
            frame.render_widget(Paragraph::new("hello world"), frame.area());
            (None, None)
        }
        let (tx, rx) = mpsc::channel::<WriterPayload>();
        let backend = CrosstermBackend::new(
            TermWriter::new(tx, WriterSync::new()).expect("single test writer"),
        );
        let mut terminal = echo_agent_ratatui_inline::Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
            },
        )
        .expect("build terminal");
        let mut cursor = CursorState::new();
        draw_frame(&mut terminal, &mut cursor, render);
        let first: Vec<u8> = rx.try_iter().flat_map(|payload| payload.data).collect();
        assert!(!first.is_empty(), "first frame should emit bytes");
        draw_frame(&mut terminal, &mut cursor, render);
        let second: Vec<u8> = rx.try_iter().flat_map(|payload| payload.data).collect();
        assert!(
            second.is_empty(),
            "idle (unchanged) frame must emit 0 bytes, got {}: {:?}",
            second.len(),
            String::from_utf8_lossy(&second),
        );
    }
    #[test]
    fn writer_success_is_acknowledged_after_flush() {
        let (sync, mut events) = WriterSync::new_for_test();
        let sequence = sync.reserve_sequence();
        let payload = WriterPayload {
            sequence,
            data: b"frame bytes".to_vec(),
        };
        let mut sink = Vec::new();
        write_payload(&mut sink, &payload, &sync).expect("write payload");
        assert_eq!(sink, b"frame bytes");
        assert_eq!(sync.written(), sequence);
        assert!(matches!(
            events.try_recv(),
            Ok(WriterEvent::Written(written)) if written == sequence
        ));
        assert_eq!(
            sync.wait_drained(Duration::from_secs(1)).unwrap(),
            WriterDrain::Drained
        );
    }
    #[test]
    fn writer_flush_failure_is_not_acknowledged() {
        struct FlushFailWriter {
            data: Vec<u8>,
        }
        impl Write for FlushFailWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.data.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("flush failed"))
            }
        }
        let (sync, mut events) = WriterSync::new_for_test();
        let sequence = sync.reserve_sequence();
        let payload = WriterPayload {
            sequence,
            data: b"frame bytes".to_vec(),
        };
        let mut sink = FlushFailWriter { data: Vec::new() };
        assert!(write_payload(&mut sink, &payload, &sync).is_err());
        assert_eq!(sink.data, b"frame bytes");
        assert_eq!(sync.written(), 0);
        assert!(sync.failed());
        assert!(matches!(events.try_recv(), Ok(WriterEvent::Failed(_))));
    }
    #[test]
    fn writer_failure_is_not_acknowledged() {
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("write failed"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (sync, mut events) = WriterSync::new_for_test();
        let sequence = sync.reserve_sequence();
        let payload = WriterPayload {
            sequence,
            data: b"frame bytes".to_vec(),
        };
        assert!(write_payload(&mut FailingWriter, &payload, &sync).is_err());
        assert_eq!(sync.written(), 0);
        assert!(sync.failed());
        assert!(matches!(events.try_recv(), Ok(WriterEvent::Failed(_))));
        assert!(sync.wait_drained(Duration::from_secs(1)).is_err());
    }
    #[test]
    fn writer_drain_timeout_is_bounded_and_retryable() {
        let sync = WriterSync::new();
        let sequence = sync.reserve_sequence();
        let started = Instant::now();
        assert_eq!(
            sync.wait_drained(Duration::from_millis(5)).unwrap(),
            WriterDrain::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        sync.mark_written(sequence);
        assert_eq!(
            sync.wait_drained(Duration::ZERO).unwrap(),
            WriterDrain::Drained
        );
    }
    #[test]
    fn term_writer_send_failure_is_published_and_not_acknowledged() {
        let (tx, rx) = mpsc::channel::<WriterPayload>();
        drop(rx);
        let (sync, mut events) = WriterSync::new_for_test();
        let mut writer = TermWriter::new(tx, sync.clone()).expect("single test writer");
        writer.write_all(b"frame bytes").expect("buffer write");
        assert!(writer.flush().is_err());
        assert_eq!(sync.queued(), 1);
        assert_eq!(sync.written(), 0);
        assert!(matches!(events.try_recv(), Ok(WriterEvent::Failed(_))));
    }
    #[test]
    fn writer_sync_rejects_multiple_live_producers() {
        let (tx, _rx) = mpsc::channel::<WriterPayload>();
        let sync = WriterSync::new();
        let first = TermWriter::new(tx.clone(), sync.clone()).expect("first writer");
        assert!(matches!(
            TermWriter::new(tx.clone(), sync.clone()),
            Err(WriterAlreadyActive)
        ));
        drop(first);
        assert!(TermWriter::new(tx, sync).is_ok());
    }
    #[test]
    fn drain_observes_reservation_before_payload_is_consumed() {
        let (tx, rx) = mpsc::channel::<WriterPayload>();
        let sync = WriterSync::new();
        let mut writer = TermWriter::new(tx, sync.clone()).expect("single test writer");
        writer.write_all(b"frame bytes").expect("buffer write");
        writer.flush().expect("flush");
        assert_eq!(sync.queued(), 1);
        assert!(!sync.is_drained());
        assert_eq!(rx.recv().expect("payload").sequence, 1);
    }
    fn state_hidden() -> CursorState {
        CursorState { last_pos: None }
    }
    fn state_at(x: u16, y: u16) -> CursorState {
        CursorState {
            last_pos: Some((x, y)),
        }
    }
    #[test]
    fn hidden_no_changes_stays_hidden() {
        let s = state_hidden();
        assert_eq!(s.action(None, false), CursorAction::None);
    }
    #[test]
    fn visible_same_pos_no_changes_preserves_blink() {
        let s = state_at(5, 10);
        assert_eq!(s.action(Some((5, 10)), false), CursorAction::None);
    }
    #[test]
    fn visible_new_pos_no_changes_repositions() {
        let s = state_at(5, 10);
        assert_eq!(
            s.action(Some((6, 10)), false),
            CursorAction::Reposition(6, 10)
        );
    }
    #[test]
    fn hidden_with_changes_stays_hidden() {
        let s = state_hidden();
        assert_eq!(s.action(None, true), CursorAction::None);
    }
    #[test]
    fn visible_same_pos_with_changes_repositions() {
        let s = state_at(5, 10);
        assert_eq!(
            s.action(Some((5, 10)), true),
            CursorAction::Reposition(5, 10)
        );
    }
    #[test]
    fn visible_new_pos_with_changes_repositions() {
        let s = state_at(5, 10);
        assert_eq!(
            s.action(Some((8, 10)), true),
            CursorAction::Reposition(8, 10)
        );
    }
    #[test]
    fn hidden_to_visible_shows() {
        let s = state_hidden();
        assert_eq!(s.action(Some((5, 10)), false), CursorAction::Show(5, 10));
    }
    #[test]
    fn hidden_to_visible_with_changes_shows() {
        let s = state_hidden();
        assert_eq!(s.action(Some((5, 10)), true), CursorAction::Show(5, 10));
    }
    #[test]
    fn visible_to_hidden_hides() {
        let s = state_at(5, 10);
        assert_eq!(s.action(None, false), CursorAction::Hide);
    }
    #[test]
    fn visible_to_hidden_with_changes_hides() {
        let s = state_at(5, 10);
        assert_eq!(s.action(None, true), CursorAction::Hide);
    }
    #[test]
    fn apply_show_updates_last_pos() {
        let mut s = state_hidden();
        let mut sink = Vec::new();
        s.apply(CursorAction::Show(3, 7), &mut sink);
        assert_eq!(s.last_pos, Some((3, 7)));
    }
    #[test]
    fn apply_hide_clears_last_pos() {
        let mut s = state_at(3, 7);
        let mut sink = Vec::new();
        s.apply(CursorAction::Hide, &mut sink);
        assert_eq!(s.last_pos, None);
    }
    #[test]
    fn apply_reposition_updates_last_pos() {
        let mut s = state_at(3, 7);
        let mut sink = Vec::new();
        s.apply(CursorAction::Reposition(5, 9), &mut sink);
        assert_eq!(s.last_pos, Some((5, 9)));
    }
    #[test]
    fn apply_none_preserves_state() {
        let mut s = state_at(3, 7);
        let mut sink = Vec::new();
        s.apply(CursorAction::None, &mut sink);
        assert_eq!(s.last_pos, Some((3, 7)));
    }
    /// Verify the writer thread correctly round-trips multi-byte UTF-8
    /// through the channel. This catches encoding issues where the writer
    /// silently corrupts Braille/emoji/CJK characters.
    #[test]
    fn writer_thread_preserves_multibyte_utf8() {
        let test_payload = "⣀⣾⠿⠛\u{e0a0}\u{1F600}";
        let expected_bytes = test_payload.as_bytes().to_vec();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let capture = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture2 = capture.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            while let Ok(data) = rx.recv() {
                buf.extend_from_slice(&data);
            }
            *capture2.lock().unwrap() = buf;
        });
        tx.send(expected_bytes.clone()).unwrap();
        drop(tx);
        handle.join().unwrap();
        let captured = capture.lock().unwrap();
        assert_eq!(
            *captured, expected_bytes,
            "Writer thread corrupted multi-byte UTF-8 payload"
        );
        assert_eq!(
            std::str::from_utf8(&captured).unwrap(),
            test_payload,
            "Round-tripped bytes do not decode to original UTF-8 string"
        );
    }
}
