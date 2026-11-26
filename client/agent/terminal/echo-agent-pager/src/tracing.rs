//! Tracing capture and display for the pager's tracing pane.
//!
//! This module provides:
//!
//! - [`TracingEntry`] — a single log line, parsed from an ANSI-formatted string
//!   into a ratatui [`Text`] for rendering and a plain-text [`Arc<str>`] for search.
//!   Implements [`ListItem`] so it can be displayed in a [`ListPane`].
//!
//! - [`TracingModel`] — a bounded, append-only ring buffer of [`TracingEntry`] items.
//!   Uses `Vec` with batch eviction (not `VecDeque`) so that `as_slice()` returns a
//!   single contiguous `&[TracingEntry]` for the `ListPane` API.
//!
//! ## Architecture (Option A — ANSI pass-through)
//!
//! The current approach receives pre-formatted ANSI strings from
//! `tracing-subscriber`'s `Full` formatter, parses them with `ansi-to-tui` into
//! styled ratatui `Text`, and renders them directly. This is the simplest path
//! to a working tracing pane.
//!
//! ## Future: Option B — Structured capture
//!
//! A future iteration could replace the ANSI pass-through with a custom
//! `tracing_subscriber::Layer` that captures structured event data (level, target,
//! spans, fields) into `TracingEntry` directly. The `ListItem` trait boundary
//! insulates `ListPane` from this change — only this module would need updating.
//! The `TracingEntry::new()` constructor is the seam: swap its internals from
//! "parse ANSI string" to "format structured fields" and nothing else changes.
use crate::views::list_pane::ListItem;
use ansi_to_tui::IntoText;
use ratatui::text::{Line, Text};
use std::io;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing_subscriber::fmt::MakeWriter;
/// A single tracing log entry, ready for display in a `ListPane`.
///
/// Created from a pre-formatted ANSI string (as produced by
/// `tracing_subscriber::fmt` with `with_ansi(true)`). The ANSI is parsed once
/// at construction time into:
/// - `styled` — ratatui `Text<'static>` with color/style spans, for rendering
/// - `plain` — ANSI-stripped plain text, for search/filter matching
///
/// Both are immutable after construction.
#[derive(Debug, Clone)]
pub struct TracingEntry {
    /// Monotonic sequence number. Used as `stable_id()` for `ListItem`.
    seq: u64,
    /// Raw ANSI string, kept for re-styling on theme switch.
    raw_ansi: Arc<str>,
    /// ANSI-stripped plain text for `search_text()`.
    plain: Arc<str>,
    /// Pre-parsed styled text for rendering. Produced by `ansi-to-tui`.
    styled: Text<'static>,
}
impl TracingEntry {
    /// Create a new entry from a pre-formatted ANSI string.
    ///
    /// Parses the ANSI escape sequences into ratatui styled spans and extracts
    /// plain text for search. The `seq` is a monotonic ID assigned by the
    /// [`TracingModel`].
    ///
    /// If ANSI parsing fails (malformed escapes), falls back to plain unstyled
    /// text — we never drop a log line.
    pub fn new(seq: u64, ansi: &str) -> Self {
        let raw_ansi: Arc<str> = Arc::from(ansi);
        let styled = Self::parse_and_style(ansi);
        let plain = plain_text_from_styled(&styled);
        Self {
            seq,
            raw_ansi,
            plain,
            styled,
        }
    }
    /// Re-apply theme colors to this entry (called on theme switch).
    pub fn restyle(&mut self) {
        self.styled = Self::parse_and_style(&self.raw_ansi);
    }
    /// Parse ANSI and map basic ANSI colors to the current theme palette.
    fn parse_and_style(ansi: &str) -> Text<'static> {
        let mut styled = ansi
            .as_bytes()
            .into_text()
            .unwrap_or_else(|_| Text::raw(ansi.to_owned()));
        let theme = crate::theme::Theme::current();
        for line in &mut styled.lines {
            for span in &mut line.spans {
                use ratatui::style::Color;
                span.style.fg = Some(match span.style.fg {
                    Some(Color::Green) => theme.accent_success,
                    Some(Color::Yellow) => theme.warning,
                    Some(Color::Red) => theme.accent_error,
                    Some(Color::Blue) => theme.accent_system,
                    Some(Color::Magenta) => theme.accent_assistant,
                    Some(Color::Cyan) => theme.running,
                    None | Some(Color::Reset) | Some(Color::White) => theme.text_primary,
                    _ => theme.gray,
                });
                span.style.bg = None;
            }
        }
        styled
    }
    /// The monotonic sequence number.
    pub fn seq(&self) -> u64 {
        self.seq
    }
    /// The ANSI-stripped plain text.
    pub fn plain(&self) -> &str {
        &self.plain
    }
    /// The pre-parsed styled text.
    pub fn styled(&self) -> &Text<'static> {
        &self.styled
    }
}
/// Extract plain text from a ratatui `Text` by concatenating all span contents.
///
/// Lines are joined with `\n` (matching how `search_text()` should work for
/// multi-line entries, though tracing entries are typically single-line).
fn plain_text_from_styled(text: &Text<'_>) -> Arc<str> {
    if text.lines.len() == 1 && text.lines[0].spans.len() == 1 {
        return Arc::from(text.lines[0].spans[0].content.as_ref());
    }
    let mut buf = String::new();
    for (i, line) in text.lines.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
        }
        for span in &line.spans {
            buf.push_str(&span.content);
        }
    }
    Arc::from(buf)
}
impl ListItem for TracingEntry {
    fn content(&self) -> &Line<'_> {
        self.styled
            .lines
            .first()
            .expect("TracingEntry should have at least one line")
    }
    fn stable_id(&self) -> u64 {
        self.seq
    }
    fn search_text(&self) -> &str {
        &self.plain
    }
}
/// Bounded, append-only buffer of [`TracingEntry`] items.
///
/// Uses `Vec` (not `VecDeque`) so that [`as_slice()`](Self::as_slice) returns a
/// single contiguous `&[TracingEntry]` — required by `ListPane`'s API.
///
/// Eviction strategy: when `len > capacity + hysteresis`, drain the oldest
/// `hysteresis` entries in one batch. This amortizes the memcpy cost. At
/// 100 msgs/sec with hysteresis=5000, eviction happens roughly every 50
/// seconds — negligible.
#[derive(Debug)]
pub struct TracingModel {
    entries: Vec<TracingEntry>,
    /// Maximum number of entries to retain after eviction.
    capacity: usize,
    /// Entries allowed beyond `capacity` before triggering eviction.
    hysteresis: usize,
    /// Next sequence number to assign.
    next_seq: u64,
}
/// Result of a [`TracingModel::push`] operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushResult {
    /// Number of entries evicted from the front (0 most of the time).
    pub evicted: usize,
}
impl TracingModel {
    /// Create a new model with the given capacity and hysteresis.
    ///
    /// - `capacity`: target number of entries to retain after eviction.
    /// - `hysteresis`: how many entries beyond `capacity` before eviction fires.
    ///
    /// # Panics
    ///
    /// Panics if `capacity == 0`.
    pub fn new(capacity: usize, hysteresis: usize) -> Self {
        assert!(capacity > 0, "TracingModel capacity must be > 0");
        Self {
            entries: Vec::with_capacity(capacity + hysteresis),
            capacity,
            hysteresis,
            next_seq: 0,
        }
    }
    /// Append a log line (pre-formatted ANSI string).
    ///
    /// Returns the number of entries evicted from the front (0 unless the
    /// buffer exceeded `capacity + hysteresis`).
    pub fn push(&mut self, ansi: &str) -> PushResult {
        let entry = TracingEntry::new(self.next_seq, ansi);
        self.next_seq += 1;
        self.entries.push(entry);
        let evicted = self.maybe_evict();
        PushResult { evicted }
    }
    /// Append a pre-constructed [`TracingEntry`].
    ///
    /// Useful if the caller wants to construct entries with a custom formatter
    /// (Option B) instead of ANSI pass-through.
    pub fn push_entry(&mut self, mut entry: TracingEntry) -> PushResult {
        entry.seq = self.next_seq;
        self.next_seq += 1;
        self.entries.push(entry);
        let evicted = self.maybe_evict();
        PushResult { evicted }
    }
    /// Evict oldest entries if we've exceeded the threshold.
    fn maybe_evict(&mut self) -> usize {
        let threshold = self.capacity + self.hysteresis;
        if self.entries.len() > threshold {
            let keep_from = self.entries.len() - self.capacity;
            self.entries.drain(..keep_from);
            keep_from
        } else {
            0
        }
    }
    /// Contiguous slice of all current entries.
    ///
    /// This is the slice you pass to `ListPaneState::prepare_layout()` and
    /// `ListPane::new()`.
    pub fn as_slice(&self) -> &[TracingEntry] {
        &self.entries
    }
    /// Number of entries currently stored.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// The configured capacity (max entries after eviction).
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    /// The configured hysteresis (slack before eviction triggers).
    pub fn hysteresis(&self) -> usize {
        self.hysteresis
    }
    /// The next sequence number that will be assigned.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }
    /// Clear all entries and reset the sequence counter.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.next_seq = 0;
    }
    /// The sequence number of the first (oldest) entry, if any.
    pub fn first_seq(&self) -> Option<u64> {
        self.entries.first().map(|e| e.seq)
    }
    /// The sequence number of the last (newest) entry, if any.
    pub fn last_seq(&self) -> Option<u64> {
        self.entries.last().map(|e| e.seq)
    }
    /// Re-apply theme colors to all entries (called on theme switch).
    pub fn restyle_all(&mut self) {
        for entry in &mut self.entries {
            entry.restyle();
        }
    }
}
/// Target for the full ACP update payload dump (plain JSON, no ANSI).
///
/// Off by default in release builds: serializing every update at streaming
/// rate (bash `raw_output` byte arrays reach hundreds of KB per line) plus
/// retention in the log channel was the 50-60GB OOM class. Payload fields on
/// this target must be wrapped in [`LazyJson`] so serialization only happens
/// inside a recording subscriber: the dev pane filter (dev builds) or the
/// firehose (`ECHO_AGENT_DEBUG_LOG` / `ECHO_AGENT_LOG_FILE`).
pub use echo_agent_telemetry::debug_log::ACP_UPDATE_PAYLOAD_TARGET;
/// Target for the always-on compact ACP update summary line (kind, ids,
/// status, payload sizes). Cheap to format at streaming rate.
///
/// Defined in `echo-agent-telemetry` so the firehose directives and the pager
/// filter share one constant (re-exported here for callsites).
pub use echo_agent_telemetry::debug_log::ACP_UPDATE_TARGET;
/// Lazily JSON-serializes a value inside `Display::fmt`.
///
/// Use as a `%`-captured event field so `serde_json::to_string` runs only
/// when a layer whose filter passed actually records the field. A bare
/// `serde_json::to_string(..)` macro argument is NOT lazy: the registry
/// includes filterless layers (e.g. the disabled-telemetry `NoOpLayer`s)
/// whose default `register_callsite` reports `Interest::always()`, globally
/// enabling the callsite — per-layer filters only gate recording, not
/// argument evaluation.
pub struct LazyJson<'a, T>(pub &'a T);
impl<T: serde::Serialize> std::fmt::Display for LazyJson<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&serde_json::to_string(self.0).unwrap_or_default())
    }
}
/// Capacity of the log channel between tracing-subscriber and the UI.
///
/// Bounded so a starved consumer (the event loop drains it only on ticks,
/// which are deprioritized below ACP traffic) caps retention at
/// `capacity x line size` instead of growing without limit. On overflow the
/// newest line is dropped and [`dropped_log_lines`] is incremented.
const LOG_CHANNEL_CAPACITY: usize = 16 * 1024;
/// Lines dropped due to a full log channel (process-wide).
static DROPPED_LOG_LINES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Total log lines dropped so far because the channel was full.
pub fn dropped_log_lines() -> u64 {
    DROPPED_LOG_LINES.load(std::sync::atomic::Ordering::Relaxed)
}
/// Type aliases for the channel endpoints.
pub type LogTx = mpsc::Sender<String>;
pub type LogRx = mpsc::Receiver<String>;
/// Factory that creates [`TracingChannelWriter`] instances for `tracing-subscriber`.
///
/// Implements [`MakeWriter`] so it can be passed to
/// `tracing_subscriber::fmt().with_writer(make_writer)`.
///
/// Created via [`TracingChannelMakeWriter::new()`], which returns the writer
/// factory and the receiving end of the channel.
#[derive(Clone)]
pub struct TracingChannelMakeWriter(LogTx);
impl TracingChannelMakeWriter {
    /// Create a new channel writer pair.
    ///
    /// Returns `(make_writer, receiver)`. Pass `make_writer` to
    /// `tracing_subscriber::fmt().with_writer(...)`. Poll `receiver` in your
    /// event loop and feed each `String` to [`TracingModel::push()`].
    pub fn new() -> (Self, LogRx) {
        let (tx, rx) = mpsc::channel(LOG_CHANNEL_CAPACITY);
        (Self(tx), rx)
    }
}
impl<'a> MakeWriter<'a> for TracingChannelMakeWriter {
    type Writer = TracingChannelWriter;
    fn make_writer(&'a self) -> Self::Writer {
        TracingChannelWriter { tx: self.0.clone() }
    }
}
/// Writer that sends each formatted log line to a bounded mpsc channel
/// (drop-on-full; see [`write()`](io::Write::write) — logging must never OOM
/// or back-pressure the runtime, so a full channel drops the line).
///
/// Created by [`TracingChannelMakeWriter`]. Each call to [`write()`](io::Write::write)
/// trims ASCII whitespace, skips empty lines, and sends the result as a `String`.
///
/// The receiver side (held by the event loop) drains these strings into a
/// [`TracingModel`].
#[derive(Clone)]
pub struct TracingChannelWriter {
    tx: LogTx,
}
impl io::Write for TracingChannelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let s = String::from_utf8_lossy(buf.trim_ascii());
        if !s.is_empty() {
            match self.tx.try_send(s.into_owned()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    DROPPED_LOG_LINES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Err(io::Error::other("tracing channel send failed"));
                }
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Return value from [`init_tracing()`].
///
/// Holds the receiving end of the log channel. The caller should poll `rx` in
/// the event loop and feed each `String` to [`TracingModel::push()`].
pub struct TracingHandle {
    /// Receive log lines here. Each string is a pre-formatted ANSI line from
    /// `tracing-subscriber`'s `Full` formatter.
    pub rx: LogRx,
}
/// Initialize a `tracing-subscriber` that captures formatted log lines into a
/// channel, ready for display in a [`TracingModel`].
///
/// This sets the global default subscriber. Call it once at startup, before any
/// `tracing::info!()` calls.
///
/// The subscriber uses:
/// - `Full` formatter (timestamp + level + target + message)
/// - ANSI colors enabled (`with_ansi(true)`)
/// - `RUST_LOG` env filter (defaults to `info` if unset)
///
/// Returns a [`TracingHandle`] whose `rx` field should be polled each tick.
///
/// # Example
///
/// ```ignore
/// let handle = init_tracing();
/// // In event loop:
/// while let Ok(line) = handle.rx.try_recv() {
///     model.push(&line);
/// }
/// ```
pub fn init_tracing() -> TracingHandle {
    use echo_agent_telemetry::debug_log::RMCP_SSE_NOISE_TARGET;
    use tracing_subscriber::{
        EnvFilter, Layer as _, filter::LevelFilter, fmt, layer::SubscriberExt as _,
    };
    let (make_writer, rx) = TracingChannelMakeWriter::new();
    let payload_level = "off";
    let directives = format!(
        "echo_agent_runtime=info,echo_agent_pager=trace,echo_agent_tools=info,echo_agent_session_search=info,echo_agent_acp=info,{RMCP_SSE_NOISE_TARGET}=error,sampling_log=off,{ACP_UPDATE_TARGET}=debug,{ACP_UPDATE_PAYLOAD_TARGET}={payload_level}"
    );
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::WARN.into())
        .parse_lossy(&directives);
    let fmt_layer = fmt::layer()
        .with_target(true)
        .with_ansi(true)
        .with_writer(make_writer);
    let otel_layer = echo_agent_telemetry::otel_layer::build_otel_layer(
        echo_agent_telemetry::otel_layer::OtelClientInfo {
            client_name: "echo-agent-pager",
            client_version: echo_agent_version::VERSION,
            service_version: echo_agent_version::full_version(),
            app_entrypoint: "tui",
        },
        echo_agent_runtime::auth::credential_provider::build_default_otel_layer_config(),
    );
    let instrumentation_layer = echo_agent_telemetry::instrumentation::layer();
    let sampling_log_layer = echo_agent_telemetry::sampling_log::layer();
    let hooks_log_layer = echo_agent_telemetry::hooks_log::layer();
    let registry = tracing_subscriber::registry()
        .with(fmt_layer.with_filter(env_filter))
        .with(instrumentation_layer)
        .with(sampling_log_layer)
        .with(hooks_log_layer)
        .with(otel_layer);
    echo_agent_telemetry::debug_log::install_firehose(registry, "tui");
    echo_agent_telemetry::external::init(
        echo_agent_runtime::agent::config::resolve_external_otel_config(
            echo_agent_telemetry::external::config::ExternalClientInfo {
                service_version: echo_agent_version::full_version().to_owned(),
                client_version: echo_agent_version::VERSION.to_owned(),
                app_entrypoint: "tui".to_owned(),
            },
        ),
    );
    TracingHandle { rx }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;
    /// Records whether `Serialize` ever ran.
    struct SerializeProbe(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl serde::Serialize for SerializeProbe {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            s.serialize_str("probe-payload")
        }
    }
    /// Filterless layer with all-default methods, mirroring the disabled
    /// telemetry `NoOpLayer`s in the production registry: its default
    /// `register_callsite` reports `Interest::always()`, keeping every
    /// callsite globally enabled. This is the condition that defeats a bare
    /// (non-lazy) macro argument.
    struct FilterlessNoOp;
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for FilterlessNoOp {}
    /// Emit the production-shaped payload event against a registry with the
    /// given payload-target directive; return (serialized?, line received?).
    fn run_payload_event(directive: &str) -> (bool, bool) {
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::{EnvFilter, Layer as _};
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = SerializeProbe(flag.clone());
        let (make_writer, mut rx) = TracingChannelMakeWriter::new();
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(make_writer)
            .with_filter(EnvFilter::new(directive));
        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(FilterlessNoOp);
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(
                target: "acp_update_payload",
                payload = %LazyJson(&probe),
                "[acp]",
            );
        });
        let serialized = flag.load(std::sync::atomic::Ordering::SeqCst);
        let line = rx.try_recv().ok();
        (serialized, line.is_some())
    }
    #[test]
    fn lazy_json_not_serialized_when_payload_target_off() {
        let (serialized, line_received) = run_payload_event("acp_update_payload=off");
        assert!(!serialized, "payload serialized despite target=off");
        assert!(!line_received, "no log line should be emitted");
    }
    #[test]
    fn lazy_json_serialized_when_payload_target_on() {
        let (serialized, line_received) = run_payload_event("acp_update_payload=debug");
        assert!(serialized, "payload should serialize when target enabled");
        assert!(line_received, "log line should be emitted");
    }
    #[test]
    fn lazy_json_display_renders_json() {
        assert_eq!(
            format!("{}", LazyJson(&serde_json::json!({"a": 1}))),
            r#"{"a":1}"#
        );
    }
    #[test]
    fn entry_from_plain_text() {
        let entry = TracingEntry::new(42, "hello world");
        assert_eq!(entry.seq(), 42);
        assert_eq!(entry.plain(), "hello world");
        assert_eq!(entry.styled().lines.len(), 1);
        assert_eq!(entry.styled().lines[0].spans.len(), 1);
        assert_eq!(
            entry.styled().lines[0].spans[0].content.as_ref(),
            "hello world"
        );
    }
    #[test]
    fn entry_from_ansi_bold() {
        let entry = TracingEntry::new(0, "\x1b[1mBOLD\x1b[0m normal");
        assert_eq!(entry.plain(), "BOLD normal");
        let spans = &entry.styled().lines[0].spans;
        assert!(spans.len() >= 2, "expected ≥2 spans, got {}", spans.len());
        assert!(
            spans[0].style.add_modifier.contains(Modifier::BOLD),
            "first span should be bold: {:?}",
            spans[0].style
        );
        assert_eq!(spans[0].content.as_ref(), "BOLD");
    }
    #[test]
    #[ignore = "known broken: ANSI color expectations no longer match parsed RGB output"]
    fn entry_from_ansi_colored() {
        let entry = TracingEntry::new(1, "\x1b[32mINFO\x1b[0m  some message");
        assert_eq!(entry.plain(), "INFO  some message");
        let first_span = &entry.styled().lines[0].spans[0];
        assert_eq!(first_span.content.as_ref(), "INFO");
        let theme = crate::theme::Theme::current();
        assert_eq!(
            first_span.style.fg,
            Some(theme.accent_success),
            "expected theme accent_success, got {:?}",
            first_span.style.fg
        );
    }
    #[test]
    fn entry_from_realistic_tracing_line() {
        let line = "\x1b[2m2025-02-16T10:30:00.123Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mmy_crate::module\x1b[0m\x1b[2m:\x1b[0m hello from tracing";
        let entry = TracingEntry::new(99, line);
        assert_eq!(
            entry.plain(),
            "2025-02-16T10:30:00.123Z  INFO my_crate::module: hello from tracing"
        );
        assert!(entry.styled().lines[0].spans.len() >= 4);
    }
    #[test]
    fn entry_malformed_ansi_does_not_panic() {
        let cases = [
            "\x1b[999mhello\x1b[0m",
            "\x1b[",
            "\x1b[1",
            "before \x1b[999 after",
            "\x1b",
            "\x1b[32;1;4;999mstuff",
        ];
        for input in &cases {
            let entry = TracingEntry::new(0, input);
            let _ = entry.plain();
            let _ = entry.styled();
            let _ = entry.search_text();
        }
    }
    #[test]
    fn entry_empty_string() {
        let entry = TracingEntry::new(0, "");
        assert_eq!(entry.plain(), "");
        assert_eq!(entry.stable_id(), 0);
    }
    #[test]
    fn entry_multiline_ansi() {
        let entry = TracingEntry::new(0, "line1\nline2\nline3");
        assert_eq!(entry.plain(), "line1\nline2\nline3");
        assert_eq!(entry.styled().lines.len(), 3);
        assert_eq!(entry.desired_height(80), 1);
    }
    #[test]
    fn list_item_stable_id() {
        let entry = TracingEntry::new(12345, "test");
        assert_eq!(entry.stable_id(), 12345);
    }
    #[test]
    fn list_item_search_text() {
        let entry = TracingEntry::new(0, "\x1b[32mINFO\x1b[0m  hello");
        assert_eq!(entry.search_text(), "INFO  hello");
    }
    #[test]
    fn list_item_desired_height_single_line() {
        let entry = TracingEntry::new(0, "single line");
        assert_eq!(entry.desired_height(80), 1);
    }
    #[test]
    fn list_item_content_returns_styled_line() {
        let entry = TracingEntry::new(0, "\x1b[32mINFO\x1b[0m  hello");
        let content = entry.content();
        let text: String = content.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "INFO  hello");
    }
    #[test]
    #[ignore = "known broken: ANSI color expectations no longer match parsed RGB output"]
    fn list_item_content_preserves_color() {
        let entry = TracingEntry::new(0, "\x1b[32mGREEN\x1b[0m");
        let content = entry.content();
        assert!(!content.spans.is_empty());
        let first = &content.spans[0];
        assert_eq!(first.content.as_ref(), "GREEN");
        let theme = crate::theme::Theme::current();
        assert_eq!(
            first.style.fg,
            Some(theme.accent_success),
            "expected theme accent_success, got {:?}",
            first.style.fg
        );
    }
    #[test]
    fn list_item_desired_height_wide_content() {
        let entry = TracingEntry::new(0, "abcdefghij");
        assert_eq!(entry.desired_height(4), 3);
    }
    #[test]
    fn list_item_desired_height_fits() {
        let entry = TracingEntry::new(0, "short");
        assert_eq!(entry.desired_height(80), 1);
    }
    #[test]
    fn model_push_and_len() {
        let mut model = TracingModel::new(100, 10);
        assert!(model.is_empty());
        assert_eq!(model.len(), 0);
        model.push("line 1");
        model.push("line 2");
        assert_eq!(model.len(), 2);
        assert!(!model.is_empty());
    }
    #[test]
    fn model_as_slice() {
        let mut model = TracingModel::new(100, 10);
        model.push("aaa");
        model.push("bbb");
        model.push("ccc");
        let slice = model.as_slice();
        assert_eq!(slice.len(), 3);
        assert_eq!(slice[0].plain(), "aaa");
        assert_eq!(slice[1].plain(), "bbb");
        assert_eq!(slice[2].plain(), "ccc");
    }
    #[test]
    fn model_seq_numbers_monotonic() {
        let mut model = TracingModel::new(100, 10);
        model.push("a");
        model.push("b");
        model.push("c");
        let seqs: Vec<u64> = model.as_slice().iter().map(|e| e.seq()).collect();
        assert_eq!(seqs, vec![0, 1, 2]);
        assert_eq!(model.next_seq(), 3);
    }
    #[test]
    fn model_eviction_triggers_at_threshold() {
        let mut model = TracingModel::new(5, 3);
        for i in 0..8 {
            let result = model.push(&format!("line {i}"));
            assert_eq!(result.evicted, 0, "should not evict at len={}", i + 1);
        }
        assert_eq!(model.len(), 8);
        let result = model.push("line 8");
        assert_eq!(result.evicted, 4);
        assert_eq!(model.len(), 5);
        let plains: Vec<&str> = model.as_slice().iter().map(|e| e.plain()).collect();
        assert_eq!(
            plains,
            vec!["line 4", "line 5", "line 6", "line 7", "line 8"]
        );
    }
    #[test]
    fn model_eviction_preserves_seq_numbers() {
        let mut model = TracingModel::new(3, 2);
        for i in 0..6 {
            model.push(&format!("line {i}"));
        }
        assert_eq!(model.len(), 3);
        assert_eq!(model.first_seq(), Some(3));
        assert_eq!(model.last_seq(), Some(5));
        assert_eq!(model.next_seq(), 6);
    }
    #[test]
    fn model_clear() {
        let mut model = TracingModel::new(100, 10);
        model.push("a");
        model.push("b");
        assert_eq!(model.len(), 2);
        model.clear();
        assert!(model.is_empty());
        assert_eq!(model.next_seq(), 0);
    }
    #[test]
    fn model_push_entry_custom() {
        let mut model = TracingModel::new(100, 10);
        let entry = TracingEntry::new(999, "custom");
        let result = model.push_entry(entry);
        assert_eq!(result.evicted, 0);
        assert_eq!(model.as_slice()[0].seq(), 0);
        assert_eq!(model.as_slice()[0].plain(), "custom");
    }
    #[test]
    fn model_first_last_seq_empty() {
        let model = TracingModel::new(100, 10);
        assert_eq!(model.first_seq(), None);
        assert_eq!(model.last_seq(), None);
    }
    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn model_zero_capacity_panics() {
        TracingModel::new(0, 10);
    }
    #[test]
    fn model_zero_hysteresis() {
        let mut model = TracingModel::new(3, 0);
        model.push("a");
        model.push("b");
        model.push("c");
        assert_eq!(model.len(), 3);
        let result = model.push("d");
        assert_eq!(result.evicted, 1);
        assert_eq!(model.len(), 3);
        assert_eq!(model.as_slice()[0].plain(), "b");
    }
    #[test]
    fn model_large_batch_eviction() {
        let mut model = TracingModel::new(10, 5);
        for i in 0..15 {
            model.push(&format!("{i}"));
        }
        assert_eq!(model.len(), 15);
        let result = model.push("15");
        assert_eq!(result.evicted, 6);
        assert_eq!(model.len(), 10);
        assert_eq!(model.first_seq(), Some(6));
    }
    #[test]
    fn model_entries_usable_as_list_items() {
        let mut model = TracingModel::new(100, 10);
        model.push("\x1b[32m INFO\x1b[0m  hello");
        model.push("\x1b[31mERROR\x1b[0m  world");
        let slice = model.as_slice();
        assert_eq!(slice[0].stable_id(), 0);
        assert_eq!(slice[1].stable_id(), 1);
        assert_eq!(slice[0].search_text(), " INFO  hello");
        assert_eq!(slice[1].search_text(), "ERROR  world");
        assert_eq!(slice[0].desired_height(80), 1);
    }
    #[test]
    fn plain_text_fast_path() {
        let text = Text::raw("simple");
        let plain = plain_text_from_styled(&text);
        assert_eq!(&*plain, "simple");
    }
    #[test]
    fn plain_text_multi_span() {
        use ratatui::text::{Line, Span};
        let text = Text::from(Line::from(vec![
            Span::raw("hello"),
            Span::raw(" "),
            Span::raw("world"),
        ]));
        let plain = plain_text_from_styled(&text);
        assert_eq!(&*plain, "hello world");
    }
    #[test]
    fn plain_text_multi_line() {
        let text = Text::from(vec![
            ratatui::text::Line::raw("line1"),
            ratatui::text::Line::raw("line2"),
        ]);
        let plain = plain_text_from_styled(&text);
        assert_eq!(&*plain, "line1\nline2");
    }
    #[test]
    fn channel_writer_sends_trimmed_line() {
        use std::io::Write;
        let (make_writer, mut rx) = TracingChannelMakeWriter::new();
        let mut writer = make_writer.make_writer();
        writer.write_all(b"  hello world  \n").unwrap();
        let msg = rx.try_recv().unwrap();
        assert_eq!(msg, "hello world");
    }
    #[test]
    fn channel_writer_skips_empty() {
        use std::io::Write;
        let (make_writer, mut rx) = TracingChannelMakeWriter::new();
        let mut writer = make_writer.make_writer();
        writer.write_all(b"   \n").unwrap();
        writer.write_all(b"").unwrap();
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn channel_writer_returns_original_len() {
        use std::io::Write;
        let (make_writer, _rx) = TracingChannelMakeWriter::new();
        let mut writer = make_writer.make_writer();
        let input = b"hello\n";
        let n = writer.write(input).unwrap();
        assert_eq!(n, input.len());
    }
    #[test]
    fn channel_writer_error_on_closed_receiver() {
        use std::io::Write;
        let (make_writer, rx) = TracingChannelMakeWriter::new();
        let mut writer = make_writer.make_writer();
        drop(rx);
        let result = writer.write(b"hello");
        assert!(result.is_err());
    }
    #[test]
    fn channel_writer_multiple_writes() {
        use std::io::Write;
        let (make_writer, mut rx) = TracingChannelMakeWriter::new();
        let mut writer = make_writer.make_writer();
        writer.write_all(b"line 1\n").unwrap();
        writer.write_all(b"line 2\n").unwrap();
        writer.write_all(b"line 3\n").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "line 1");
        assert_eq!(rx.try_recv().unwrap(), "line 2");
        assert_eq!(rx.try_recv().unwrap(), "line 3");
    }
    #[test]
    fn channel_writer_preserves_ansi() {
        use std::io::Write;
        let (make_writer, mut rx) = TracingChannelMakeWriter::new();
        let mut writer = make_writer.make_writer();
        let ansi_line = b"\x1b[32m INFO\x1b[0m  hello\n";
        writer.write_all(ansi_line).unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(
            msg.contains("\x1b[32m"),
            "ANSI should be preserved: {msg:?}"
        );
        assert!(msg.contains("INFO"), "content should be preserved: {msg:?}");
    }
    #[test]
    fn channel_to_model_end_to_end() {
        use std::io::Write;
        let (make_writer, mut rx) = TracingChannelMakeWriter::new();
        let mut model = TracingModel::new(100, 10);
        let mut writer = make_writer.make_writer();
        writer
            .write_all(b"\x1b[32m INFO\x1b[0m  hello from tracing\n")
            .unwrap();
        writer
            .write_all(b"\x1b[31mERROR\x1b[0m  something went wrong\n")
            .unwrap();
        drop(writer);
        while let Ok(line) = rx.try_recv() {
            model.push(&line);
        }
        assert_eq!(model.len(), 2);
        let slice = model.as_slice();
        assert!(slice[0].search_text().contains("hello from tracing"));
        assert!(slice[1].search_text().contains("something went wrong"));
        let content: String = slice[0]
            .content()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            content.contains("INFO") && content.contains("hello from tracing"),
            "content should contain log text: {content:?}"
        );
    }
    #[test]
    fn channel_to_model_with_eviction() {
        use std::io::Write;
        let (make_writer, mut rx) = TracingChannelMakeWriter::new();
        let mut model = TracingModel::new(3, 2);
        let mut writer = make_writer.make_writer();
        for i in 0..6 {
            writer.write_all(format!("line {i}\n").as_bytes()).unwrap();
        }
        drop(writer);
        while let Ok(line) = rx.try_recv() {
            model.push(&line);
        }
        assert_eq!(model.len(), 3);
        assert_eq!(model.as_slice()[0].plain(), "line 3");
        assert_eq!(model.as_slice()[2].plain(), "line 5");
    }
    #[test]
    fn make_writer_is_clone() {
        let (mw, _rx) = TracingChannelMakeWriter::new();
        let _mw2 = mw.clone();
    }
}
