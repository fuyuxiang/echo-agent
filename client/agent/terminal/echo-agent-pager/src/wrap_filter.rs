//! Streaming output filter for `echoagent wrap`: OSC 52 clipboard interception,
//! host-image request handling, and DEC-mode observation.
//!
//! `Osc52Filter` sits between the wrap PTY reader and stdout (see
//! `crate::pty_wrap`). It consumes OSC 52 clipboard sequences (plain and tmux
//! DCS passthrough) and the private host-image request OSC (see
//! [`crate::wrap_clipboard_image`]); everything else — including every CSI
//! sequence, which is additionally reported to the wrap mode tracker — passes
//! through verbatim. The parser handles sequences split across arbitrary
//! chunk boundaries.

use base64::Engine as _;
use std::sync::Arc;

use crate::wrap_restore::ModeTracker;

/// Maximum size for a buffered escape sequence candidate (1 MiB).
///
/// This bounds the memory used while accumulating a candidate OSC 52 or DCS
/// sequence. Must be large enough to hold the base64-encoded form of
/// `MAX_CLIPBOARD_PAYLOAD` (~1.33x expansion) plus the escape envelope.
const MAX_ESC_BUFFER: usize = 1024 * 1024;

/// Maximum size for a buffered CSI sequence.
///
/// CSI bytes are withheld until the final byte arrives so complete sequences
/// can be reported to the wrap mode tracker before being forwarded verbatim.
/// Must comfortably fit a single DECSET listing every tracked mode (~69
/// bytes today; a unit test pins that relationship so mode-table growth
/// cannot silently cross the cap). Anything larger is malformed and flushes
/// through unreported (mirroring the `MAX_ESC_BUFFER` overflow pattern).
const MAX_CSI_BUFFER: usize = 128;

/// Maximum decoded clipboard payload size (768 KiB).
///
/// Aligned with `MAX_ESC_BUFFER`: a 768 KiB payload encodes to ~1 MiB of
/// base64, fitting within the buffer limit. Payloads larger than this are
/// unrealistic for clipboard content over SSH.
const MAX_CLIPBOARD_PAYLOAD: usize = 768 * 1024;

/// The prefix that identifies an OSC 52 sequence after the `ESC ]`.
const OSC52_PREFIX: &[u8] = b"52;";

/// The tmux DCS passthrough prefix after `ESC P`: `tmux;\x1b\x1b]`.
const TMUX_DCS_PREFIX: &[u8] = b"tmux;\x1b\x1b]";

/// Base64 engine that accepts both padded and unpadded input.
///
/// OSC 52 emitters in the wild (including some Go-based tools and terminals)
/// may omit `=` padding. Using `Indifferent` mode avoids silent decode
/// failures from legitimate clipboard sequences.
const BASE64_STANDARD_INDIFFERENT: base64::engine::GeneralPurpose =
    base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );

/// State machine states for the OSC 52 streaming parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterState {
    /// Normal output passthrough.
    Normal,
    /// Saw ESC (0x1b), waiting for next byte to determine sequence type.
    Esc,
    /// Inside CSI: saw `ESC [` -- accumulating until the final byte
    /// (0x40-0x7E) so the complete sequence can be reported to the mode
    /// tracker, then forwarded verbatim. A fragment truncated by child EOF
    /// is intentionally never flushed: emitting a half-open CSI would leave
    /// the real terminal's parser mid-sequence, where it would eat the
    /// restore bytes the exit path writes right after.
    Csi,
    /// Inside OSC: saw `ESC ]` -- accumulating until BEL or ST.
    Osc,
    /// Inside DCS: saw `ESC P` -- checking for tmux passthrough prefix.
    Dcs,
    /// Inside DCS tmux passthrough, accumulating inner OSC 52.
    DcsTmuxOsc,
    /// Saw ESC inside an OSC, could be ST terminator (`ESC \`).
    OscEsc,
    /// Saw ESC inside a DCS tmux OSC, could be inner ST or DCS ST.
    DcsTmuxOscEsc,
}

/// Clipboard sink type: a boxed closure that receives decoded clipboard data.
type ClipboardSink = Box<dyn FnMut(&[u8])>;

type WrapImageRequestHandler = Box<dyn FnMut()>;

/// Streaming filter that intercepts OSC 52 clipboard sequences from PTY
/// output and sends their decoded payload to the local clipboard.
///
/// All non-OSC-52 bytes pass through unchanged. The parser handles sequences
/// split across arbitrary byte boundaries.
pub(crate) struct Osc52Filter {
    state: FilterState,
    buf: Vec<u8>,
    clipboard_sink: ClipboardSink,
    wrap_image_handler: Option<WrapImageRequestHandler>,
    mode_tracker: Option<Arc<ModeTracker>>,
}

impl Osc52Filter {
    /// Create a new filter that sends clipboard data to the system clipboard.
    pub(crate) fn new() -> Self {
        Self {
            state: FilterState::Normal,
            buf: Vec::new(),
            clipboard_sink: Box::new(set_local_clipboard),
            wrap_image_handler: None,
            mode_tracker: None,
        }
    }

    pub(crate) fn with_wrap_image_handler(mut self, handler: impl FnMut() + 'static) -> Self {
        self.wrap_image_handler = Some(Box::new(handler));
        self
    }

    /// Report every complete CSI sequence flowing through to `tracker`.
    pub(crate) fn with_mode_tracker(mut self, tracker: Arc<ModeTracker>) -> Self {
        self.mode_tracker = Some(tracker);
        self
    }

    /// Create a filter with a custom clipboard sink (for testing).
    #[cfg(test)]
    fn with_sink(sink: impl FnMut(&[u8]) + 'static) -> Self {
        Self {
            state: FilterState::Normal,
            buf: Vec::new(),
            clipboard_sink: Box::new(sink),
            wrap_image_handler: None,
            mode_tracker: None,
        }
    }

    /// Process a chunk of bytes from PTY output.
    ///
    /// Returns bytes that should be written to stdout. OSC 52 clipboard
    /// sequences are consumed (not included in the output) and their decoded
    /// payload is sent to the clipboard sink.
    pub(crate) fn feed(&mut self, data: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(data.len());
        for &byte in data {
            match self.state {
                FilterState::Normal => {
                    if byte == 0x1b {
                        self.state = FilterState::Esc;
                        self.buf.clear();
                        self.buf.push(byte);
                    } else {
                        output.push(byte);
                    }
                }
                FilterState::Esc => {
                    self.buf.push(byte);
                    match byte {
                        b']' => self.state = FilterState::Osc,
                        b'P' => self.state = FilterState::Dcs,
                        b'[' => self.state = FilterState::Csi,
                        _ => {
                            // Not an OSC, DCS, or CSI -- flush buffer and continue.
                            output.extend_from_slice(&self.buf);
                            self.buf.clear();
                            self.state = FilterState::Normal;
                        }
                    }
                }
                FilterState::Csi => {
                    self.buf.push(byte);
                    if (0x40..=0x7e).contains(&byte) {
                        // Final byte: the sequence is complete. Report it to
                        // the tracker, then forward verbatim -- CSI is only
                        // observed, never consumed or modified.
                        if let Some(tracker) = &self.mode_tracker {
                            tracker.observe_csi(&self.buf);
                        }
                        output.extend_from_slice(&self.buf);
                        self.buf.clear();
                        self.state = FilterState::Normal;
                    } else if byte == 0x1b {
                        // A new ESC aborts the CSI. Flush the fragment and let
                        // the ESC start a fresh sequence so OSC 52 right after
                        // a malformed CSI is still intercepted.
                        self.buf.pop();
                        output.extend_from_slice(&self.buf);
                        self.buf.clear();
                        self.buf.push(0x1b);
                        self.state = FilterState::Esc;
                    } else if !(0x20..=0x3f).contains(&byte) || self.buf.len() > MAX_CSI_BUFFER {
                        // Not a parameter/intermediate byte, or oversized:
                        // malformed. Flush verbatim without reporting.
                        output.extend_from_slice(&self.buf);
                        self.buf.clear();
                        self.state = FilterState::Normal;
                    }
                }
                FilterState::Osc => {
                    self.buf.push(byte);
                    match byte {
                        // BEL terminates the OSC sequence.
                        0x07 => {
                            if !self.try_handle_consumed_osc() {
                                output.extend_from_slice(&self.buf);
                            }
                            self.buf.clear();
                            self.state = FilterState::Normal;
                        }
                        // ESC could be the start of ST (ESC \).
                        0x1b => {
                            self.state = FilterState::OscEsc;
                        }
                        _ => {}
                    }
                }
                FilterState::OscEsc => {
                    self.buf.push(byte);
                    if byte == b'\\' {
                        // ST terminator: ESC \.
                        if !self.try_handle_consumed_osc() {
                            output.extend_from_slice(&self.buf);
                        }
                        self.buf.clear();
                        self.state = FilterState::Normal;
                    } else {
                        // Not ST -- continue accumulating in Osc state.
                        // The ESC we saw might be part of the payload in some
                        // broken sequence; just keep buffering.
                        self.state = FilterState::Osc;
                    }
                }
                FilterState::Dcs => {
                    self.buf.push(byte);
                    // buf starts with \x1bP so tmux prefix bytes start at offset 2.
                    let prefix_pos = self.buf.len() - 2;
                    if prefix_pos <= TMUX_DCS_PREFIX.len() {
                        if TMUX_DCS_PREFIX[prefix_pos - 1] == byte {
                            if prefix_pos == TMUX_DCS_PREFIX.len() {
                                // Full tmux prefix matched: \x1bPtmux;\x1b\x1b]
                                self.state = FilterState::DcsTmuxOsc;
                            }
                            // else keep matching prefix
                        } else {
                            // Prefix mismatch: not a tmux passthrough, flush.
                            output.extend_from_slice(&self.buf);
                            self.buf.clear();
                            self.state = FilterState::Normal;
                        }
                    } else {
                        // Exceeded prefix length without matching; flush.
                        output.extend_from_slice(&self.buf);
                        self.buf.clear();
                        self.state = FilterState::Normal;
                    }
                }
                FilterState::DcsTmuxOsc => {
                    self.buf.push(byte);
                    match byte {
                        // BEL terminates the inner OSC.
                        0x07 => {
                            // Inner OSC is done but we still need DCS ST
                            // (ESC \) to close the tmux wrapper.
                            // Remain in this state to catch the ESC.
                        }
                        0x1b => {
                            self.state = FilterState::DcsTmuxOscEsc;
                        }
                        _ => {}
                    }
                }
                FilterState::DcsTmuxOscEsc => {
                    self.buf.push(byte);
                    if byte == b'\\' {
                        // DCS ST: ESC \. The full tmux-wrapped sequence is done.
                        if !self.try_handle_tmux_osc52() {
                            output.extend_from_slice(&self.buf);
                        }
                        self.buf.clear();
                        self.state = FilterState::Normal;
                    } else {
                        // Not ST. Continue accumulating in DcsTmuxOsc.
                        self.state = FilterState::DcsTmuxOsc;
                    }
                }
            }

            // Guard: if the buffer grows beyond the limit, flush and reset.
            if self.buf.len() > MAX_ESC_BUFFER {
                output.extend_from_slice(&self.buf);
                self.buf.clear();
                self.state = FilterState::Normal;
            }
        }
        output
    }

    /// Handle OSC 52 clipboard or wrap image request; `true` if consumed.
    fn try_handle_consumed_osc(&mut self) -> bool {
        let body = self.buf[2..].to_vec();
        let body = strip_osc_terminator(&body);
        if self.try_handle_wrap_image_request(body) {
            return true;
        }
        self.extract_and_set_clipboard(body)
    }

    fn try_handle_wrap_image_request(&mut self, body: &[u8]) -> bool {
        if body != crate::wrap_clipboard_image::REQUEST_BODY {
            return false;
        }
        if let Some(handler) = self.wrap_image_handler.as_mut() {
            handler();
        }
        true
    }

    /// Try to handle the buffered bytes as a tmux-wrapped OSC 52 sequence.
    ///
    /// Expected buffer format:
    ///   `\x1bPtmux;\x1b\x1b]52;<sel>;<base64>\x07\x1b\\`
    ///
    /// Returns `true` if the sequence was a valid OSC 52 and was consumed.
    fn try_handle_tmux_osc52(&mut self) -> bool {
        // Strip the DCS tmux prefix: \x1bPtmux;\x1b\x1b]  (total 9 bytes)
        // and the DCS ST terminator: \x1b\  (2 bytes at the end).
        // Copy the body to avoid borrowing self.buf while calling &mut self.
        let prefix_len = 2 + TMUX_DCS_PREFIX.len(); // \x1bP + tmux;\x1b\x1b]
        if self.buf.len() < prefix_len + 2 {
            return false;
        }
        let body = self.buf[prefix_len..self.buf.len() - 2].to_vec(); // strip DCS ST
        let body = strip_osc_terminator(&body); // strip inner BEL if present
        self.extract_and_set_clipboard(body)
    }

    /// Parse OSC 52 body (`52;<sel>;<base64>`), decode, and set clipboard.
    ///
    /// Returns `true` if successfully handled.
    fn extract_and_set_clipboard(&mut self, body: &[u8]) -> bool {
        // Must start with "52;"
        if !body.starts_with(OSC52_PREFIX) {
            return false;
        }
        let after_52 = &body[OSC52_PREFIX.len()..];

        // Find the selection parameter separator (next ';').
        let payload_start = match after_52.iter().position(|&b| b == b';') {
            Some(pos) => pos + 1,
            None => return false,
        };
        let b64_payload = &after_52[payload_start..];

        // Decode base64.
        let decoded = match BASE64_STANDARD_INDIFFERENT.decode(b64_payload) {
            Ok(data) => data,
            Err(_) => return false,
        };

        // Check payload size limit.
        if decoded.len() > MAX_CLIPBOARD_PAYLOAD {
            tracing::warn!(
                "OSC 52 payload too large ({} bytes), ignoring",
                decoded.len()
            );
            return false;
        }

        (self.clipboard_sink)(&decoded);
        true
    }
}

/// Strip the OSC terminator from the end of a body slice.
///
/// Removes trailing BEL (`\x07`) or ST (`\x1b\x5c`) if present.
fn strip_osc_terminator(body: &[u8]) -> &[u8] {
    if body.ends_with(&[0x1b, b'\\']) {
        &body[..body.len() - 2]
    } else if body.ends_with(&[0x07]) {
        &body[..body.len() - 1]
    } else {
        body
    }
}

/// Write decoded clipboard payload to the local system clipboard.
///
/// Delegates to [`echo_agent_runtime::util::clipboard::set_text`] which uses
/// `pbcopy` on macOS and `arboard` elsewhere. Failures are logged but do
/// not propagate -- clipboard access is best-effort.
fn set_local_clipboard(data: &[u8]) {
    let text = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("OSC 52 payload is not valid UTF-8: {e}");
            return;
        }
    };
    if let Err(e) = echo_agent_runtime::util::clipboard::set_text(text) {
        tracing::warn!("clipboard copy failed: {e}");
    }
}

/// Encode a host clipboard image (or NONE) as a bracketed-paste frame.
pub(crate) fn host_clipboard_image_frame() -> Vec<u8> {
    let image = echo_agent_pager_render::clipboard::system_clipboard_get_image();
    crate::wrap_clipboard_image::encode_wrap_image_response(image.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Helper: run data through the filter with a capturing clipboard sink.
    /// Returns (stdout_output, captured_clipboard_payloads).
    fn filter_output(input: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
        let clips = Rc::new(RefCell::new(Vec::new()));
        let clips_clone = Rc::clone(&clips);
        let mut filter = Osc52Filter::with_sink(move |data: &[u8]| {
            clips_clone.borrow_mut().push(data.to_vec());
        });
        let output = filter.feed(input);
        let captured = clips.borrow().clone();
        (output, captured)
    }

    /// Helper: run data through the filter in multiple small chunks.
    fn filter_output_chunked(input: &[u8], chunk_size: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
        let clips = Rc::new(RefCell::new(Vec::new()));
        let clips_clone = Rc::clone(&clips);
        let mut filter = Osc52Filter::with_sink(move |data: &[u8]| {
            clips_clone.borrow_mut().push(data.to_vec());
        });
        let mut output = Vec::new();
        for chunk in input.chunks(chunk_size) {
            output.extend_from_slice(&filter.feed(chunk));
        }
        let captured = clips.borrow().clone();
        (output, captured)
    }

    /// Encode text as a plain OSC 52 sequence with BEL terminator.
    fn make_osc52_bel(text: &str) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        format!("\x1b]52;c;{b64}\x07").into_bytes()
    }

    /// Encode text as a plain OSC 52 sequence with ST terminator.
    fn make_osc52_st(text: &str) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        format!("\x1b]52;c;{b64}\x1b\\").into_bytes()
    }

    /// Encode text as a tmux-wrapped OSC 52 sequence.
    fn make_osc52_tmux(text: &str) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        format!("\x1bPtmux;\x1b\x1b]52;c;{b64}\x07\x1b\\").into_bytes()
    }

    #[test]
    fn osc52_normal_text_unchanged() {
        let input = b"Hello, world!\r\n";
        let (output, clips) = filter_output(input);
        assert_eq!(output, input);
        assert!(clips.is_empty());
    }

    #[test]
    fn osc52_ansi_escapes_pass_through() {
        // SGR color: ESC [ 31 m
        let input = b"\x1b[31mred text\x1b[0m";
        let (output, clips) = filter_output(input);
        assert_eq!(output, input.as_slice());
        assert!(clips.is_empty());
    }

    #[test]
    fn osc52_plain_bel_terminated() {
        let seq = make_osc52_bel("hello");
        let (output, clips) = filter_output(&seq);
        assert!(
            output.is_empty(),
            "OSC 52 should be consumed, got: {output:?}"
        );
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0], b"hello");
    }

    #[test]
    fn osc52_plain_st_terminated() {
        let seq = make_osc52_st("hello");
        let (output, clips) = filter_output(&seq);
        assert!(
            output.is_empty(),
            "OSC 52 should be consumed, got: {output:?}"
        );
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0], b"hello");
    }

    #[test]
    fn osc52_with_s0_selection() {
        // Selection parameter "s0" instead of "c".
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"clipboard data");
        let seq = format!("\x1b]52;s0;{b64}\x07").into_bytes();
        let (output, clips) = filter_output(&seq);
        assert!(output.is_empty());
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0], b"clipboard data");
    }

    #[test]
    fn osc52_tmux_wrapped() {
        let seq = make_osc52_tmux("hello from tmux");
        let (output, clips) = filter_output(&seq);
        assert!(
            output.is_empty(),
            "tmux OSC 52 should be consumed, got: {output:?}"
        );
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0], b"hello from tmux");
    }

    #[test]
    fn osc52_surrounded_by_text() {
        let mut input = b"before ".to_vec();
        input.extend_from_slice(&make_osc52_bel("copied"));
        input.extend_from_slice(b" after");
        let (output, clips) = filter_output(&input);
        assert_eq!(output, b"before  after");
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0], b"copied");
    }

    #[test]
    fn osc52_multiple_sequences() {
        let mut input = make_osc52_bel("first");
        input.extend_from_slice(b"gap");
        input.extend_from_slice(&make_osc52_st("second"));
        let (output, clips) = filter_output(&input);
        assert_eq!(output, b"gap");
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0], b"first");
        assert_eq!(clips[1], b"second");
    }

    #[test]
    fn osc52_split_across_chunks() {
        let seq = make_osc52_bel("split test");
        // Feed one byte at a time.
        let (output, clips) = filter_output_chunked(&seq, 1);
        assert!(output.is_empty(), "should be consumed even byte-by-byte");
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0], b"split test");
    }

    #[test]
    fn osc52_split_at_various_sizes() {
        let seq = make_osc52_st("chunk test");
        for chunk_size in 2..=seq.len() {
            let (output, clips) = filter_output_chunked(&seq, chunk_size);
            assert!(
                output.is_empty(),
                "chunk_size={chunk_size}: should be consumed"
            );
            assert_eq!(clips.len(), 1, "chunk_size={chunk_size}: expected 1 clip");
            assert_eq!(clips[0], b"chunk test");
        }
    }

    #[test]
    fn osc52_tmux_split_across_chunks() {
        let seq = make_osc52_tmux("tmux split");
        let (output, clips) = filter_output_chunked(&seq, 3);
        assert!(output.is_empty());
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0], b"tmux split");
    }

    #[test]
    fn osc52_invalid_base64_passes_through() {
        // Invalid base64 payload: "!!!" is not valid base64.
        let seq = b"\x1b]52;c;!!!\x07";
        let (output, clips) = filter_output(seq);
        assert_eq!(output, seq.as_slice(), "invalid base64 should pass through");
        assert!(clips.is_empty());
    }

    #[test]
    fn osc52_non_52_osc_passes_through() {
        // OSC 0 (window title) should pass through.
        let seq = b"\x1b]0;my title\x07";
        let (output, clips) = filter_output(seq);
        assert_eq!(output, seq.as_slice());
        assert!(clips.is_empty());
    }

    #[test]
    fn osc52_non_52_osc_st_passes_through() {
        // OSC 0 with ST terminator.
        let seq = b"\x1b]0;my title\x1b\\";
        let (output, clips) = filter_output(seq);
        assert_eq!(output, seq.as_slice());
        assert!(clips.is_empty());
    }

    #[test]
    fn osc52_oversized_buffer_flushes() {
        // Build a sequence that exceeds MAX_ESC_BUFFER.
        let mut seq = b"\x1b]52;c;".to_vec();
        // Fill with valid base64 chars until we exceed the limit.
        seq.resize(MAX_ESC_BUFFER + 100, b'A');
        seq.push(0x07);

        let (output, clips) = filter_output(&seq);
        // The oversized sequence should have been flushed through.
        assert!(
            !output.is_empty(),
            "oversized sequence should flush through"
        );
        assert!(
            clips.is_empty(),
            "oversized sequence should not set clipboard"
        );
    }

    #[test]
    fn osc52_empty_payload() {
        // Empty base64 payload should still work (copies empty string).
        let seq = b"\x1b]52;c;\x07";
        let (output, clips) = filter_output(seq);
        assert!(output.is_empty());
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0], b"");
    }

    #[test]
    fn osc52_non_tmux_dcs_passes_through() {
        // A DCS that doesn't start with the tmux prefix should flush.
        let seq = b"\x1bPother;stuff\x1b\\";
        let (output, clips) = filter_output(seq);
        // The flush happens when the prefix mismatch is detected.
        assert!(!output.is_empty(), "non-tmux DCS should pass through");
        assert!(clips.is_empty());
    }

    #[test]
    fn osc52_missing_selection_separator() {
        // No second ';' after "52;" -- missing selection param separator.
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"data");
        let seq = format!("\x1b]52;{b64}\x07").into_bytes();
        // This has "52;" followed by base64 with no second ';'. The parser
        // will treat everything after "52;" up to the next ';' as the
        // selection param. If there's no ';', it returns false.
        let (output, clips) = filter_output(&seq);
        assert_eq!(output, seq, "should pass through without second ';'");
        assert!(clips.is_empty());
    }

    #[test]
    fn wrap_image_request_consumed_and_handler_runs() {
        let calls = Rc::new(RefCell::new(0usize));
        let calls_clone = Rc::clone(&calls);
        let clips = Rc::new(RefCell::new(Vec::new()));
        let clips_clone = Rc::clone(&clips);
        let mut filter = Osc52Filter::with_sink(move |data: &[u8]| {
            clips_clone.borrow_mut().push(data.to_vec());
        })
        .with_wrap_image_handler(move || {
            *calls_clone.borrow_mut() += 1;
        });
        let mut input = b"before".to_vec();
        input.extend_from_slice(&crate::wrap_clipboard_image::request_osc_bytes());
        input.extend_from_slice(b"after");
        let output = filter.feed(&input);
        assert_eq!(output, b"beforeafter");
        assert_eq!(*calls.borrow(), 1);
        assert!(clips.borrow().is_empty());
    }

    #[test]
    fn wrap_image_request_split_across_chunks() {
        let calls = Rc::new(RefCell::new(0usize));
        let calls_clone = Rc::clone(&calls);
        let mut filter = Osc52Filter::with_sink(|_| {}).with_wrap_image_handler(move || {
            *calls_clone.borrow_mut() += 1;
        });
        let seq = crate::wrap_clipboard_image::request_osc_bytes();
        let mut output = Vec::new();
        for chunk in seq.chunks(3) {
            output.extend_from_slice(&filter.feed(chunk));
        }
        assert!(output.is_empty(), "request OSC must be fully consumed");
        assert_eq!(*calls.borrow(), 1);
    }

    /// Helper: run data through a tracker-attached filter in chunks.
    /// Returns (stdout_output, restore_bytes for the final latched state).
    fn filter_output_tracked(input: &[u8], chunk_size: usize) -> (Vec<u8>, Vec<u8>) {
        let tracker = Arc::new(ModeTracker::new());
        let mut filter = Osc52Filter::with_sink(|_| {}).with_mode_tracker(Arc::clone(&tracker));
        let mut output = Vec::new();
        for chunk in input.chunks(chunk_size) {
            output.extend_from_slice(&filter.feed(chunk));
        }
        let restore = crate::wrap_restore::restore_bytes(tracker.snapshot());
        (output, restore)
    }

    #[test]
    fn csi_latch_and_unlatch_track_across_chunk_splits() {
        let input = b"pre\x1b[?1049h\x1b[?1003hmid\x1b[?1003lpost";
        for chunk_size in 1..=input.len() {
            let (output, restore) = filter_output_tracked(input, chunk_size);
            assert_eq!(
                output,
                input.as_slice(),
                "chunk_size={chunk_size}: CSI must pass through verbatim"
            );
            assert_eq!(
                restore, b"\x1b[?1049l",
                "chunk_size={chunk_size}: only the still-latched mode restores"
            );
        }
    }

    #[test]
    fn csi_kitty_push_tracks_across_chunk_splits() {
        let input = b"\x1b[>1u";
        for chunk_size in 1..=input.len() {
            let (output, restore) = filter_output_tracked(input, chunk_size);
            assert_eq!(output, input.as_slice(), "chunk_size={chunk_size}");
            assert_eq!(restore, b"\x1b[<u", "chunk_size={chunk_size}");
        }
    }

    #[test]
    fn csi_multi_param_set_latches_both_modes() {
        let (output, restore) = filter_output_tracked(b"\x1b[?1002;1006h", 1);
        assert_eq!(output, b"\x1b[?1002;1006h");
        assert_eq!(restore, b"\x1b[?1002l\x1b[?1006l");
    }

    #[test]
    fn csi_balanced_modes_restore_nothing() {
        let (output, restore) =
            filter_output_tracked(b"\x1b[?2004h\x1b[31mtext\x1b[0m\x1b[?2004l", 3);
        assert_eq!(output, b"\x1b[?2004h\x1b[31mtext\x1b[0m\x1b[?2004l");
        assert!(restore.is_empty(), "balanced state must restore nothing");
    }

    #[test]
    fn csi_oversized_flushes_verbatim_without_latching() {
        // A "CSI" longer than the cap is malformed: it must flush through
        // unmodified and must not latch even though it ends in `h`.
        let mut input = b"\x1b[?".to_vec();
        input.extend(std::iter::repeat_n(b'1', MAX_CSI_BUFFER + 10));
        input.push(b'h');
        input.extend_from_slice(b"after");
        let (output, restore) = filter_output_tracked(&input, 7);
        assert_eq!(output, input, "oversized CSI must flush verbatim");
        assert!(restore.is_empty(), "oversized CSI must not be reported");
    }

    #[test]
    fn csi_single_decset_with_every_tracked_mode_fits_the_cap() {
        // One legal DECSET enabling every set-side tracked mode (25 is
        // inverted — its latch side is `l`, appended separately). Pins the
        // MAX_CSI_BUFFER-vs-mode-table relationship: growing the tracked set
        // must not silently push this sequence over the cap into unreported
        // passthrough.
        let mut input =
            b"\x1b[?47;1000;1002;1003;1004;1005;1006;1015;1016;1047;1049;2004;2026h".to_vec();
        input.extend_from_slice(b"\x1b[?25l");
        let (output, restore) = filter_output_tracked(&input, 5);
        assert_eq!(output, input, "must pass through verbatim");
        let restore = String::from_utf8(restore).expect("restore bytes are ASCII");
        for needle in [
            "\x1b[?2026l",
            "\x1b[?25h",
            "\x1b[?1000l",
            "\x1b[?1002l",
            "\x1b[?1003l",
            "\x1b[?1005l",
            "\x1b[?1015l",
            "\x1b[?1016l",
            "\x1b[?1006l",
            "\x1b[?2004l",
            "\x1b[?1004l",
            "\x1b[?1047l",
            "\x1b[?47l",
            "\x1b[?1049l",
        ] {
            assert!(
                restore.contains(needle),
                "restore must cover {needle:?}, got {restore:?}"
            );
        }
    }

    #[test]
    fn csi_aborted_by_esc_still_intercepts_following_osc52() {
        let mut input = b"\x1b[?12".to_vec();
        input.extend_from_slice(&make_osc52_bel("after malformed csi"));
        let (clips, output) = {
            let clips = Rc::new(RefCell::new(Vec::new()));
            let clips_clone = Rc::clone(&clips);
            let mut filter = Osc52Filter::with_sink(move |data: &[u8]| {
                clips_clone.borrow_mut().push(data.to_vec());
            });
            let output = filter.feed(&input);
            (clips.borrow().clone(), output)
        };
        assert_eq!(
            output, b"\x1b[?12",
            "the malformed CSI fragment must flush through"
        );
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0], b"after malformed csi");
    }

    #[test]
    fn csi_without_tracker_still_passes_through() {
        let input = b"\x1b[?1003h\x1b[2J\x1b[H";
        let (output, clips) = filter_output_chunked(input, 2);
        assert_eq!(output, input.as_slice());
        assert!(clips.is_empty());
    }
}
