//! Streaming/incremental markdown renderer.
//!
//! This module provides `StreamingMarkdownRenderer` which efficiently renders
//! markdown that arrives in chunks (e.g., from an LLM streaming response).
//!
//! # How It Works
//!
//! Instead of re-rendering the entire document on each chunk, it:
//! 1. Accumulates incoming chunks into an internal buffer
//! 2. Detects "checkpoints" - stable block boundaries where output won't change
//! 3. Freezes rendered output up to the last checkpoint
//! 4. Only re-renders the "tail" after the checkpoint
//!
//! This reduces complexity from O(N²) to approximately O(N) for streaming.
//!
//! # Example
//!
//! ```ignore
//! let mut renderer = StreamingMarkdownRenderer::new(style, true);
//!
//! // As tokens arrive from LLM:
//! for token in stream {
//!     renderer.push_and_render(&token, Some(&syntect));
//!     let view = renderer.view();
//!     display(view.lines);
//! }
//! ```

#[cfg(test)]
use crate::HyperlinkTarget;
use crate::open_code_highlighter::OpenCodeHighlighter;
#[cfg(test)]
use crate::render_markdown_ratatui_full;
use crate::{
    LatexDelimiterNormalizer, MarkdownBuffers, MarkdownRenderOutput, MarkdownRenderView,
    MarkdownStyle, Syntect, render_markdown_ratatui_with_link_id,
};

/// Tracks the frozen state for truncation.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FrozenState {
    /// Number of frozen lines (= number of frozen line_source_map entries).
    pub(crate) lines_len: usize,
    /// Number of frozen source bytes.
    pub(crate) source_bytes: usize,
    /// Next link ID, advanced ONLY when a checkpoint advances the frozen
    /// boundary (i.e. when frozen lines and their hyperlinks become
    /// permanent).  IDs assigned to url_scan hits inside a still-tail
    /// region are regenerated on every `rerender_tail` call — they only
    /// become stable once the line they live on becomes frozen.
    pub(crate) next_link_id: u32,
}

/// Count trailing blank lines in text.
///
/// This counts how many blank lines appear at the END of the text.
/// A trailing blank line is a line containing only whitespace followed by end-of-text,
/// or consecutive newlines at the end.
///
/// For markdown block separators:
/// - "\n\n" at the end = 1 blank line (one full blank line between blocks)
/// - "\n\n\n" at the end = 2 blank lines
/// - "text\n" at the end = 0 (just a line ending, no blank line)
///
/// Examples:
/// - "" → 0
/// - "hello" → 0
/// - "hello\n" → 0 (just a line ending)
/// - "hello\n\n" → 1
/// - "hello\n\n\n" → 2
/// - "hello\n  \n" → 1 (whitespace-only line counts as blank)
#[cfg(test)]
fn count_trailing_blank_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let bytes = text.as_bytes();
    let mut count = 0;
    let mut pos = bytes.len();

    // Work backwards through the text
    while pos > 0 {
        pos -= 1;

        match bytes[pos] {
            b'\n' => {
                // Found a newline - check if the line before it is blank
                // Scan backwards to find start of this line
                let line_end = pos;
                let mut line_start = pos;
                while line_start > 0 && bytes[line_start - 1] != b'\n' {
                    line_start -= 1;
                }

                // Check if the line is blank (only whitespace)
                let line_content = &bytes[line_start..line_end];
                let is_blank = line_content.iter().all(|&b| b == b' ' || b == b'\t');

                if is_blank {
                    count += 1;
                    pos = line_start;
                } else {
                    // Found a non-blank line, stop counting
                    break;
                }
            }
            b' ' | b'\t' => {
                // Trailing whitespace, continue scanning
            }
            _ => {
                // Non-whitespace character, stop
                break;
            }
        }
    }

    count
}

/// Incremental markdown renderer that efficiently handles streaming input.
///
/// Maintains frozen (stable) content and only re-renders the unfrozen tail,
/// dramatically reducing render time for long streaming content.
pub struct StreamingMarkdownRenderer {
    /// Accumulated source text (all chunks concatenated).
    source: String,

    /// Single output buffer - frozen content at start, tail appended after.
    output: MarkdownRenderOutput,

    /// Frozen state - where to truncate before re-rendering tail.
    pub(crate) frozen: FrozenState,

    /// Reusable buffers for highlighting and rendering (avoids allocation per render).
    buffers: MarkdownBuffers,

    /// Rendering style.
    style: MarkdownStyle,

    /// Whether to use pretty mode (hide markdown syntax).
    pretty: bool,

    /// Maximum width for rendered tables (in display columns).
    max_table_width: Option<usize>,

    /// Whether CommonMark soft breaks collapse to a space (default `true`).
    /// Set `false` for source-faithful rendering (plan preview).
    collapse_soft_breaks: bool,

    /// Incremental highlighter for the trailing still-open fenced code block.
    ///
    /// Persists syntect's resumable per-line state across `rerender_tail` calls
    /// so a large open code block is highlighted in O(N) total instead of O(N²).
    /// Created lazily on the first render with syntect, and cleared (so it
    /// rebuilds) on any state reset that would change output — theme/style,
    /// pretty mode, table width, soft-break mode, or `clear()`.
    open_code: Option<OpenCodeHighlighter>,

    /// Streaming LaTeX delimiter normalizer. Rewrites `\(…\)` / `\[…\]` /
    /// `\begin{equation}` into the canonical `$` / `$$` forms before text is
    /// appended to `source`, so the math handlers convert them uniformly —
    /// including inside table cells. Held-back ambiguous bytes (a partial
    /// delimiter at a chunk boundary) are flushed by `finish()`.
    normalizer: LatexDelimiterNormalizer,
}

impl std::fmt::Debug for StreamingMarkdownRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingMarkdownRenderer")
            .field("source_len", &self.source.len())
            .field("frozen_lines", &self.frozen.lines_len)
            .field("frozen_bytes", &self.frozen.source_bytes)
            .field("output_lines", &self.output.lines.len())
            .field("pretty", &self.pretty)
            .finish()
    }
}

impl Clone for StreamingMarkdownRenderer {
    fn clone(&self) -> Self {
        // Create a fresh renderer and push all source text
        // This recreates the frozen state correctly.
        //
        // We must propagate `max_table_width` BEFORE pushing/rendering so
        // the clone produces identical output to the original.  Forgetting
        // this caused tables (and, after the url_scan-on-render fix, URLs
        // in width-constrained renders) to differ between original and
        // clone.
        let mut new = Self::new(self.style, self.pretty);
        new.set_max_table_width(self.max_table_width);
        new.set_collapse_soft_breaks(self.collapse_soft_breaks);
        // `self.source` is already normalized, so append it verbatim (do NOT
        // re-run the normalizer, which could hold back a trailing ambiguous
        // suffix and make the clone's source diverge). Copy the normalizer
        // state separately so any held-back bytes survive the clone.
        new.push_normalized(&self.source);
        new.render(None);
        new.normalizer = self.normalizer.clone();
        new
    }
}

impl StreamingMarkdownRenderer {
    /// Create a new streaming renderer.
    pub fn new(style: MarkdownStyle, pretty: bool) -> Self {
        Self {
            source: String::new(),
            output: MarkdownRenderOutput::new(),
            frozen: FrozenState::default(),
            buffers: MarkdownBuffers::new(),
            style,
            pretty,
            max_table_width: None,
            collapse_soft_breaks: true,
            open_code: None,
            normalizer: LatexDelimiterNormalizer::new(),
        }
    }

    /// Replace the markdown style and trigger a full re-render.
    ///
    /// Used when the theme changes at runtime so existing blocks pick up
    /// the new colors on the next render pass.
    pub fn set_style(&mut self, style: MarkdownStyle) {
        self.style = style;
        self.frozen = FrozenState::default();
        self.output.clear();
        // Theme/style change alters colors, so any cached highlight is stale.
        self.open_code = None;
    }

    /// Set the maximum width for rendered tables.
    ///
    /// When set, column widths are shrunk proportionally so the table
    /// fits within the given number of display columns.  If the width
    /// changes, frozen state is reset to ensure consistent rendering.
    pub fn set_max_table_width(&mut self, width: Option<usize>) {
        if self.max_table_width != width {
            self.max_table_width = width;
            // Reset frozen state since table formatting may change
            self.frozen = FrozenState::default();
            self.output.clear();
            self.open_code = None;
        }
    }

    /// Set whether CommonMark soft breaks collapse to a space.
    ///
    /// Defaults to `true`. Set `false` for source-faithful rendering (plan
    /// preview) where each source line keeps its own visual line and
    /// `line_source_map` entry. Resets frozen state when the mode changes.
    pub fn set_collapse_soft_breaks(&mut self, collapse: bool) {
        if self.collapse_soft_breaks != collapse {
            self.collapse_soft_breaks = collapse;
            self.frozen = FrozenState::default();
            self.output.clear();
            self.open_code = None;
        }
    }

    /// Push a new chunk of markdown text (no rendering).
    ///
    /// The chunk is run through the streaming LaTeX delimiter normalizer and the
    /// normalized result is appended to the internal buffer. A bounded ambiguous
    /// suffix (a partial delimiter at the chunk boundary) may be held back until
    /// the next `push`; `finish()` flushes it. Call `render()` to process
    /// accumulated content, or use `push_and_render()` for convenience.
    pub fn push(&mut self, chunk: &str) {
        let normalized = self.normalizer.push(chunk);
        self.source.push_str(&normalized);
    }

    /// Append already-normalized source text, bypassing the delimiter
    /// normalizer. Used by `clone()` to reproduce an existing (already
    /// normalized) `source` exactly; the cloned normalizer state is copied
    /// separately so any held-back bytes are preserved.
    fn push_normalized(&mut self, text: &str) {
        self.source.push_str(text);
    }

    /// Render accumulated content.
    ///
    /// Processes the unfrozen tail and updates the output. Call `view()` to
    /// get the rendered lines.
    ///
    /// Pass `None` for syntect to disable syntax highlighting for code blocks.
    ///
    /// Theme stability: a still-open fenced code block is highlighted
    /// incrementally, caching the colors of the `syntect` theme seen so far.
    /// The `syntect` theme must stay stable between renders; switch themes via
    /// [`set_style`](Self::set_style), which clears that cache. (Passing a
    /// different theme without a reset would leave already-committed lines in
    /// the old colors.)
    pub fn render(&mut self, syntect: Option<&Syntect>) {
        self.rerender_tail(syntect);
    }

    /// Push a chunk and render immediately (convenience method).
    ///
    /// Equivalent to `push(chunk)` followed by `render(syntect)`.
    /// Use this for real-time streaming where you want to display after each chunk.
    pub fn push_and_render(&mut self, chunk: &str, syntect: Option<&Syntect>) {
        let normalized = self.normalizer.push(chunk);
        self.source.push_str(&normalized);
        self.rerender_tail(syntect);
    }

    /// Internal: render the unfrozen tail and update frozen state.
    fn rerender_tail(&mut self, syntect: Option<&Syntect>) {
        // Truncate output to frozen state (discard stale tail)
        self.output.lines.truncate(self.frozen.lines_len);
        self.output.line_source_map.truncate(self.frozen.lines_len);
        // Discard stale tail hyperlinks (keep frozen ones)
        self.output
            .hyperlinks
            .retain(|h| h.line_index < self.frozen.lines_len);
        // Discard stale tail code-block spans (keep frozen ones — those whose
        // body lies entirely within the frozen prefix). A still-open fence in
        // the tail has no span at all, so spans become stable only once frozen.
        self.output
            .code_blocks
            .retain(|cb| cb.output_line_range.end <= self.frozen.lines_len);

        // Render the tail (unfrozen portion) using reusable buffers.
        // When the frozen source ends without a trailing newline (e.g., a
        // thematic break `---` at the end of a chunk) but the tail starts
        // with `\n`, that newline is the block-terminating newline consumed
        // by the frozen block.  Skip it to avoid a spurious blank line.
        let mut tail_start = self.frozen.source_bytes;
        if tail_start > 0
            && self.source.as_bytes().get(tail_start - 1) != Some(&b'\n')
            && self.source.as_bytes().get(tail_start) == Some(&b'\n')
        {
            tail_start += 1;
        }
        let tail = &self.source[tail_start..];
        // Lazily create the incremental open-code cache once syntect is present.
        // It rebuilds itself on fence/offset change, so a stale cache from a
        // previous tail (e.g. after a checkpoint advanced) is self-correcting.
        let open_code = match syntect {
            Some(syn) => Some(
                self.open_code
                    .get_or_insert_with(|| OpenCodeHighlighter::new(syn)),
            ),
            None => None,
        };
        let (tail_output, checkpoint, tail_next_link_id) = render_markdown_ratatui_with_link_id(
            tail,
            self.style,
            self.pretty,
            &mut self.buffers,
            syntect,
            self.max_table_width,
            self.frozen.next_link_id,
            self.collapse_soft_breaks,
            open_code,
        );

        // Append tail to output
        self.output.lines.extend(tail_output.lines);
        self.output
            .line_source_map
            .extend(tail_output.line_source_map);

        // Offset tail hyperlink line indices by frozen line count and append
        let frozen_lines = self.frozen.lines_len;
        self.output
            .hyperlinks
            .extend(tail_output.hyperlinks.into_iter().map(|mut h| {
                h.line_index += frozen_lines;
                h
            }));

        // Append tail code-block spans, rebasing their tail-relative ranges to
        // document coordinates (output lines by frozen line count, source bytes
        // by the tail's start offset) — mirroring the hyperlink offsetting.
        self.output
            .code_blocks
            .extend(tail_output.code_blocks.into_iter().map(|mut cb| {
                cb.output_line_range.start += frozen_lines;
                cb.output_line_range.end += frozen_lines;
                cb.source_byte_range.start += tail_start;
                cb.source_byte_range.end += tail_start;
                cb
            }));

        // Detect plain URLs (e.g. the `(url)` suffix in pretty-mode
        // markdown links, bare URLs in prose).  We run this here — not
        // only in `finish()` — for two reasons:
        //   (a) Non-streaming callers (e.g. `AgentMessageBlock::new(text)`
        //       during session replay) never call `finish()`, so without
        //       running url_scan here their URLs would never become
        //       HyperlinkTargets at all.
        //   (b) State resets (`set_max_table_width`, `set_pretty`,
        //       `set_style`) rebuild the output from scratch via a
        //       subsequent `render()`; URL hyperlinks added by an earlier
        //       `finish()` would otherwise be silently dropped here.
        //
        // We scan only the newly-rendered tail (`frozen_lines..end`); URLs
        // already on frozen lines were kept by the `retain` filter above
        // and the offset-aware scan emits document-absolute line indices.
        // `detect_plain_urls_with_offset` dedups against existing
        // hyperlinks per line, so it is idempotent.
        let tail_lines = &self.output.lines[frozen_lines..];
        let (extra_links, post_scan_next_id) = crate::url_scan::detect_plain_urls_with_offset(
            tail_lines,
            frozen_lines,
            &self.output.hyperlinks,
            tail_next_link_id,
        );
        self.output.hyperlinks.extend(extra_links);

        // Sort hyperlinks by (line_index, column_range.start) so downstream
        // consumers (`map_hyperlinks_to_overlay`, link map builders) see a
        // well-ordered list — matching the invariant `finish()` enforces.
        self.output
            .hyperlinks
            .sort_by_key(|h| (h.line_index, h.column_range.start));

        // If checkpoint found, update frozen state.
        // The checkpoint's source_bytes is relative to the tail we rendered,
        // so add tail_start (which may be > frozen.source_bytes if we skipped
        // a leading newline).
        if let Some(cp) = checkpoint {
            self.frozen = FrozenState {
                lines_len: self.frozen.lines_len + cp.output_lines,
                source_bytes: tail_start + cp.source_bytes,
                next_link_id: post_scan_next_id,
            };
        }
    }

    /// Get a view of the current rendered output.
    ///
    /// This is cheap - just returns a reference to cached output.
    /// The output was computed during `render()` or `push_and_render()`.
    pub fn view(&self) -> MarkdownRenderView<'_> {
        self.output.as_view()
    }

    /// Get the accumulated source text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Get the number of frozen source bytes.
    pub fn frozen_bytes(&self) -> usize {
        self.frozen.source_bytes
    }

    /// Get the number of frozen output lines.
    pub fn frozen_lines_count(&self) -> usize {
        self.frozen.lines_len
    }

    /// Reset the renderer, clearing all accumulated content.
    ///
    /// Also resets `max_table_width` to `None` for symmetry with the
    /// freshly-constructed state — otherwise a subsequent
    /// `set_max_table_width(Some(prev_width))` is silently a no-op
    /// (no state reset) because the inner equality check sees no change.
    pub fn clear(&mut self) {
        self.source.clear();
        self.output.clear();
        self.frozen = FrozenState::default();
        self.max_table_width = None;
        self.open_code = None;
        self.normalizer.reset();
    }

    /// Set pretty mode (true = hide syntax, false = show raw markdown).
    ///
    /// If the mode changes, frozen state is reset to ensure consistent rendering.
    pub fn set_pretty(&mut self, pretty: bool) {
        if self.pretty != pretty {
            self.pretty = pretty;
            // Reset frozen state - need to re-render everything with new mode
            self.frozen = FrozenState::default();
            self.output.clear();
            self.open_code = None;
        }
    }

    /// Get current pretty mode.
    pub fn pretty(&self) -> bool {
        self.pretty
    }

    /// Consume the renderer and return the owned output.
    ///
    /// Use this when streaming is complete and you want owned data.
    pub fn into_output(self) -> MarkdownRenderOutput {
        self.output
    }

    /// Finalize streaming with a full re-render.
    ///
    /// This does a complete non-streaming render of the accumulated source,
    /// replacing the incrementally-built output. Use this when streaming is
    /// complete to ensure correctness - it catches any edge cases where
    /// streaming might have produced slightly different output.
    ///
    /// After the parser pass, this runs `url_scan::detect_plain_urls`
    /// and sorts the hyperlink list by `(line_index, column_range.start)`
    /// so downstream consumers see a well-ordered list.  Note that
    /// `render()` ALSO runs the URL detector and sort — `finish()` no
    /// longer adds anything those calls didn't already produce; its
    /// distinguishing value is the unconditional full re-render
    /// (independent of frozen-state truncation).
    ///
    /// Returns a view of the finalized output.
    pub fn finish(&mut self, syntect: Option<&Syntect>) -> MarkdownRenderView<'_> {
        // Flush any bytes the normalizer held back at the last chunk boundary
        // (e.g. a trailing partial delimiter) so the full re-render sees the
        // complete, normalized source.
        let flushed = self.normalizer.finish();
        self.source.push_str(&flushed);

        // Do a full re-render of the entire source, preserving max_table_width.
        let mut buffers = MarkdownBuffers::new();
        let (full_output, _, full_next_link_id) = render_markdown_ratatui_with_link_id(
            &self.source,
            self.style,
            self.pretty,
            &mut buffers,
            syntect,
            self.max_table_width,
            // NOTE: Since full render restarts link IDs at 0, we MUST also reset our
            // counter to the post-render value :sadge:
            0,
            self.collapse_soft_breaks,
            // finish() is a full batch re-render: never use the incremental cache.
            None,
        );

        // Replace the output with the full render
        self.output = full_output;

        // Scan rendered lines for plain, non-md URLs that pulldown-cmark didn't
        // emit as Tag::Link. Dedup against existing hyperlinks by (line_index, column_range)
        // overlap to avoid double-linking.
        let (extra_links, post_scan_next_id) = crate::url_scan::detect_plain_urls(
            &self.output.lines,
            &self.output.hyperlinks,
            full_next_link_id,
        );
        self.output.hyperlinks.extend(extra_links);

        // Sort hyperlinks by (line_index, column_range.start) so downstream
        // consumers see a well-ordered list.
        self.output
            .hyperlinks
            .sort_by_key(|h| (h.line_index, h.column_range.start));

        // Mark everything as frozen (streaming is complete)
        self.frozen = FrozenState {
            lines_len: self.output.lines.len(),
            source_bytes: self.source.len(),
            next_link_id: post_scan_next_id,
        };

        // Streaming is over: release the highlighter caches (open-block state
        // + closed-fence memo) instead of retaining them for the lifetime of
        // the rendered block. Lazily rebuilt if rendering ever resumes.
        self.open_code = None;

        self.output.as_view()
    }

    /// Finalize streaming and return owned output.
    ///
    /// Combines `finish()` and `into_output()` - does a full re-render
    /// and returns the owned result.
    pub fn finish_into_output(mut self, syntect: Option<&Syntect>) -> MarkdownRenderOutput {
        self.finish(syntect);
        self.output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::test_style;
    use pretty_assertions::assert_eq;

    // Tests for count_trailing_blank_lines helper

    #[test]
    fn test_count_trailing_blank_lines_empty() {
        assert_eq!(count_trailing_blank_lines(""), 0);
    }

    #[test]
    fn test_count_trailing_blank_lines_no_newline() {
        assert_eq!(count_trailing_blank_lines("hello"), 0);
    }

    #[test]
    fn test_count_trailing_blank_lines_single_newline() {
        // Just a line ending, not a blank line
        assert_eq!(count_trailing_blank_lines("hello\n"), 0);
    }

    #[test]
    fn test_count_trailing_blank_lines_double_newline() {
        // One blank line (standard markdown block separator)
        assert_eq!(count_trailing_blank_lines("hello\n\n"), 1);
    }

    #[test]
    fn test_count_trailing_blank_lines_triple_newline() {
        // Two blank lines
        assert_eq!(count_trailing_blank_lines("hello\n\n\n"), 2);
    }

    #[test]
    fn test_count_trailing_blank_lines_quadruple_newline() {
        assert_eq!(count_trailing_blank_lines("hello\n\n\n\n"), 3);
    }

    #[test]
    fn test_count_trailing_blank_lines_with_spaces() {
        // Whitespace-only lines count as blank
        assert_eq!(count_trailing_blank_lines("hello\n  \n"), 1);
        assert_eq!(count_trailing_blank_lines("hello\n\t\n"), 1);
        assert_eq!(count_trailing_blank_lines("hello\n  \n  \n"), 2);
    }

    #[test]
    fn test_count_trailing_blank_lines_heading() {
        // Common markdown pattern: heading followed by blank line
        assert_eq!(count_trailing_blank_lines("# Heading\n\n"), 1);
        assert_eq!(count_trailing_blank_lines("# Heading\n\n\n"), 2);
    }

    // Basic Functionality Tests

    #[test]
    fn test_empty_renderer() {
        let renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        let output = renderer.view();
        assert!(output.lines.is_empty());
    }

    #[test]
    fn test_single_chunk() {
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render("# Hello\n\n", None);
        let output = renderer.view();
        assert!(!output.lines.is_empty());
    }

    #[test]
    fn test_multiple_chunks() {
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render("# Title\n\n", None);
        let out1_lines = renderer.view().lines.len();

        renderer.push_and_render("Some text\n\n", None);
        let out2_lines = renderer.view().lines.len();

        assert!(out2_lines >= out1_lines);
    }

    #[test]
    fn test_streaming_incomplete_paragraph() {
        // Test that incomplete paragraphs produce visible output
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);

        // First chunk: complete heading
        renderer.push_and_render("# Heading\n\n", None);
        let heading_lines = renderer.view().lines.len();
        assert!(heading_lines > 0, "Heading should produce lines");

        // Second chunk: start of paragraph (no newline)
        renderer.push_and_render("This is text ", None);
        let after_text_lines = renderer.view().lines.len();
        assert!(
            after_text_lines > heading_lines,
            "Incomplete paragraph should produce lines. Got {} lines, expected > {}",
            after_text_lines,
            heading_lines
        );

        // Third chunk: more text
        renderer.push_and_render("more text", None);
        let after_more_lines = renderer.view().lines.len();
        assert!(
            after_more_lines >= after_text_lines,
            "More text should not reduce lines. Got {}, expected >= {}",
            after_more_lines,
            after_text_lines
        );
    }

    #[test]
    fn test_freezing_occurs() {
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render("# Heading\n\n", None);
        // push() now renders automatically

        assert!(
            renderer.frozen_bytes() > 0,
            "Should freeze after complete heading"
        );
    }

    #[test]
    fn test_clear_resets_state() {
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render("# Hello\n\n", None);
        renderer.set_max_table_width(Some(80));

        renderer.clear();

        assert_eq!(renderer.source(), "");
        assert_eq!(renderer.frozen_bytes(), 0);
        assert_eq!(renderer.frozen_lines_count(), 0);

        // `clear()` must also reset `max_table_width`: otherwise a
        // subsequent `set_max_table_width(prev_value)` is silently a
        // no-op (the inner equality check sees no change), and the
        // expected reset behaviour disappears.  Verify the invariant
        // observationally — push content, observe a frozen state, then
        // re-set the prior width; the reset must wipe frozen state.
        renderer.push_and_render("# Heading\n\n", None);
        assert!(
            renderer.frozen_lines_count() > 0,
            "test setup: a complete heading should produce frozen lines",
        );
        // If clear() left max_table_width = Some(80), this call would
        // be a no-op and frozen_lines_count would stay > 0.
        renderer.set_max_table_width(Some(80));
        assert_eq!(
            renderer.frozen_lines_count(),
            0,
            "set_max_table_width(prev) after clear() must trigger a reset",
        );
    }

    #[test]
    fn test_finish_produces_full_render() {
        // Test that finish() produces identical output to full render
        let chunks = &["# Heading\n\n", "Some **bold** text.\n\n", "> Quote\n\n"];
        let full_text: String = chunks.iter().copied().collect();

        // Get full render for comparison
        let (full_output, _) =
            render_markdown_ratatui_full(&full_text, test_style::STYLE, true, None);
        let full_lines: Vec<String> = full_output
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        // Stream the content
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        for chunk in chunks {
            renderer.push_and_render(chunk, None);
            // push() now renders automatically
        }

        // After finish - should be identical to full render
        let finished = renderer.finish(None);
        let after_finish: Vec<String> = finished
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert_eq!(
            after_finish, full_lines,
            "finish() should produce identical output to full render"
        );

        // Verify frozen state is updated
        assert_eq!(renderer.frozen.source_bytes, full_text.len());
        assert_eq!(renderer.frozen.lines_len, full_lines.len());
    }

    #[test]
    fn streaming_without_finish_drops_trailing_inline_code_closer() {
        let msg = "already complete at:\n\n\
`/tmp/project/results/report.html`";

        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render(msg, None);

        let pre = renderer.source().to_string();
        assert!(
            !pre.ends_with('`'),
            "without finish(), trailing closer must still be held back; source ends {:?}",
            &pre[pre.len().saturating_sub(20)..]
        );
        assert!(
            pre.contains('`') && pre.contains("report.html"),
            "opener + path should be present before finish"
        );

        let lines_pre: Vec<String> = renderer
            .view()
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let joined_pre = lines_pre.join("\n");
        assert!(
            joined_pre.contains('`'),
            "pre-finish render should still show a backtick (unclosed span); got {joined_pre:?}"
        );

        renderer.finish(None);
        let post = renderer.source().to_string();
        assert_eq!(
            post, msg,
            "finish() flushes the held-back closer into source"
        );
    }

    // Correctness Tests - Streaming vs Full Render

    /// Comprehensive markdown document covering many edge cases.
    const COMPREHENSIVE_MARKDOWN: &str = r#"# Main Heading

This is a paragraph with **bold**, *italic*, and `inline code`.

## Code Blocks

```rust
fn main() {
    println!("Hello, world!");
}
```

```
plain code
```

## Lists

- Item one
- Item two with **bold**

1. First
2. Second

- Outer item
  - Nested item 1
  - Nested item 2

## Blockquotes

> This is a blockquote.
> It spans multiple lines.

> Nested quote:
> > Inner quote

## Tables

| Column A | Column B | Column C |
|----------|:--------:|---------:|
| Left     | Center   | Right    |

## Mixed Content

- Step one
  ```python
  print("hello")
  ```
- Step two

> Some quote:
> - Quoted item 1
> - Quoted item 2

## Links and Images

Here's a [link](https://example.com) and another [one](https://test.com "with title").

## Thematic Breaks

Above the break.

---

Below the break.

## Edge Cases

Inline elements: ***bold italic*** and ~~strikethrough~~.

Final paragraph with no trailing newline."#;

    /// Stream character by character and compare final output.
    /// Uses push_raw() to batch all characters, then update() once at end.
    #[track_caller]
    fn assert_streaming_matches_full(text: &str, pretty: bool) {
        let mode = if pretty { "pretty" } else { "raw" };
        let (full_output, _) = render_markdown_ratatui_full(text, test_style::STYLE, pretty, None);

        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, pretty);
        for ch in text.chars() {
            renderer.push(&ch.to_string());
        }
        renderer.render(None);
        let streaming_output = renderer.view();

        assert_eq!(
            streaming_output.lines,
            full_output.lines.as_slice(),
            "[{}] char-by-char streaming mismatch for: {:?}",
            mode,
            &text[..text.len().min(50)]
        );
    }

    /// Test both pretty and raw modes.
    #[track_caller]
    fn assert_streaming_matches_full_both(text: &str) {
        assert_streaming_matches_full(text, true);
        assert_streaming_matches_full(text, false);
    }

    /// Stream in variable-sized chunks and compare final output.
    #[track_caller]
    fn assert_streaming_chunks_match_full(text: &str, pretty: bool, chunk_sizes: &[usize]) {
        let mode = if pretty { "pretty" } else { "raw" };
        let (full_output, _) = render_markdown_ratatui_full(text, test_style::STYLE, pretty, None);

        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, pretty);
        let mut pos = 0;
        let mut chunk_idx = 0;

        while pos < text.len() {
            let desired_end = pos + chunk_sizes[chunk_idx % chunk_sizes.len()];
            // Find next valid char boundary at or after desired_end
            let end = text[pos..]
                .char_indices()
                .map(|(i, _)| pos + i)
                .find(|&i| i >= desired_end)
                .unwrap_or(text.len());
            renderer.push_and_render(&text[pos..end], None);
            pos = end;
            chunk_idx += 1;
        }
        let streaming_output = renderer.view();

        assert_eq!(
            streaming_output.lines,
            full_output.lines.as_slice(),
            "[{}] chunk streaming mismatch for: {:?}",
            mode,
            &text[..text.len().min(50)]
        );
    }

    #[test]
    fn test_comprehensive_char_by_char() {
        assert_streaming_matches_full_both(COMPREHENSIVE_MARKDOWN);
    }

    #[test]
    fn test_comprehensive_small_chunks() {
        assert_streaming_chunks_match_full(COMPREHENSIVE_MARKDOWN, true, &[3, 5, 7, 11]);
        assert_streaming_chunks_match_full(COMPREHENSIVE_MARKDOWN, false, &[3, 5, 7, 11]);
    }

    #[test]
    fn test_comprehensive_large_chunks() {
        assert_streaming_chunks_match_full(COMPREHENSIVE_MARKDOWN, true, &[50, 100, 200]);
        assert_streaming_chunks_match_full(COMPREHENSIVE_MARKDOWN, false, &[50, 100, 200]);
    }

    #[test]
    fn test_individual_block_types() {
        // Headings
        assert_streaming_matches_full_both("# Hello World\n\n");
        assert_streaming_matches_full_both("## Level 2\n\n### Level 3\n\n");

        // Paragraphs
        assert_streaming_matches_full_both("This is a paragraph.\n\n");
        assert_streaming_matches_full_both("Para one.\n\nPara two.\n\n");

        // Code blocks
        assert_streaming_matches_full_both("```rust\nfn main() {}\n```\n");
        assert_streaming_matches_full_both("```\nplain\n```\n");

        // Lists
        assert_streaming_matches_full_both("- Item 1\n- Item 2\n- Item 3\n\n");
        assert_streaming_matches_full_both("1. First\n2. Second\n\n");
        assert_streaming_matches_full_both("- Outer\n  - Inner 1\n  - Inner 2\n\n");

        // Blockquotes
        assert_streaming_matches_full_both("> Quote line 1\n> Quote line 2\n\n");

        // Tables
        assert_streaming_matches_full_both("| A | B |\n|---|---|\n| 1 | 2 |\n\n");

        // Thematic breaks
        assert_streaming_matches_full_both("Above\n\n---\n\nBelow\n\n");
    }

    #[test]
    fn test_mermaid_streaming_matches_full() {
        assert_streaming_matches_full_both(
            "```mermaid\ngraph TD\n  A[Start] --> B{Go?}\n  B -->|yes| C[Ship]\n  B -->|no| A\n```\n\nDone.\n",
        );
    }

    #[test]
    fn test_nested_constructs() {
        // Code in list
        assert_streaming_matches_full_both("- Step 1\n  ```\n  code\n  ```\n- Step 2\n\n");

        // List in blockquote
        assert_streaming_matches_full_both("> Quote:\n> - Item 1\n> - Item 2\n\n");

        // Nested blockquotes
        assert_streaming_matches_full_both("> Outer\n> > Inner\n\n");

        // Deeply nested list
        assert_streaming_matches_full_both("- L1\n  - L2\n    - L3\n\n");
    }

    #[test]
    fn test_inline_formatting() {
        assert_streaming_matches_full_both("Text with **bold**, *italic*, `code`.\n\n");
        assert_streaming_matches_full_both("A [link](url) and ![image](src).\n\n");
        assert_streaming_matches_full_both("***bold italic*** and ~~strike~~.\n\n");
    }

    #[test]
    fn test_edge_cases() {
        // Empty
        assert_streaming_matches_full_both("");

        // Whitespace only
        assert_streaming_matches_full_both("   \n\n  \n");

        // No trailing newline
        assert_streaming_matches_full_both("# Title\n\nNo newline at end");

        // Just a heading (minimal)
        assert_streaming_matches_full_both("# H\n");

        // Multiple blank lines
        assert_streaming_matches_full_both("Para 1\n\n\n\nPara 2\n\n");
    }

    // Line Source Map Correctness Tests

    /// Verify line_source_map matches between streaming and full render.
    /// Uses push_raw() to batch all characters, then update() once at end.
    #[track_caller]
    fn assert_line_source_map_matches(text: &str, pretty: bool) {
        let (full_output, _) = render_markdown_ratatui_full(text, test_style::STYLE, pretty, None);

        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, pretty);
        for ch in text.chars() {
            renderer.push(&ch.to_string());
        }
        renderer.render(None);
        let streaming_output = renderer.view();

        // Compare line source maps
        assert_eq!(
            full_output.line_source_map,
            streaming_output.line_source_map,
            "Line source map mismatch for {:?}",
            &text[..text.len().min(50)]
        );
    }

    #[test]
    fn test_line_source_map_simple() {
        // Simple paragraph - no freezing happens
        assert_line_source_map_matches("Hello world.\n\n", true);
    }

    #[test]
    fn test_line_source_map_with_checkpoints() {
        // Multiple blocks with checkpoints
        assert_line_source_map_matches("# Title\n\nParagraph one.\n\nParagraph two.\n\n", true);
    }

    #[test]
    fn test_soft_break_preserved_when_collapse_disabled() {
        // With collapse disabled, soft breaks stay as line breaks so each
        // source line becomes its own rendered line mapping 1:1.
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.set_collapse_soft_breaks(false);
        renderer.push_and_render("Line one,\nLine two,\nLine three.", None);
        let output = renderer.view();

        let texts: Vec<String> = output
            .lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(texts, vec!["Line one,", "Line two,", "Line three."]);
        assert_eq!(output.line_source_map, vec![0, 1, 2]);
    }

    #[test]
    fn test_soft_break_collapse_still_default_on() {
        // Default behavior is unchanged: soft breaks collapse to a space.
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render("Line one,\nLine two,\nLine three.", None);
        let output = renderer.view();
        let texts: Vec<String> = output
            .lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(texts, vec!["Line one, Line two, Line three."]);
    }

    #[test]
    fn test_soft_break_disabled_preserves_inline_style() {
        // Each preserved line keeps its inline styling (unlike a raw-text
        // fallback). Bold on line 1 must survive.
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.set_collapse_soft_breaks(false);
        renderer.push_and_render("a **bold** c\nplain line", None);
        let output = renderer.view();
        assert_eq!(output.lines.len(), 2, "lines: {:?}", output.lines);
        let has_bold = output.lines[0].spans.iter().any(|s| {
            s.style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        });
        assert!(has_bold, "bold must survive: {:?}", output.lines[0].spans);
    }

    #[test]
    fn test_streaming_preserves_heading_style() {
        // Verify that streaming produces styled output for headings
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);

        // Push a complete heading
        renderer.push_and_render("# Heading\n\n", None);
        let output = renderer.view();

        // Should have at least one line
        assert!(!output.lines.is_empty(), "Should produce lines for heading");

        // Check that the first line has styling (heading should be bold and colored)
        let first_line = &output.lines[0];
        assert!(!first_line.spans.is_empty(), "First line should have spans");

        // The heading text should have some style applied (bold, color, etc.)
        let has_style = first_line.spans.iter().any(|span| {
            span.style.fg.is_some()
                || span
                    .style
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
        });
        assert!(
            has_style,
            "Heading should have styling (color or bold). Got spans: {:?}",
            first_line.spans
        );
    }

    #[test]
    fn test_incremental_streaming_preserves_styles() {
        // Test that styles are preserved when streaming character by character
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);

        // Push heading character by character
        for c in "# Heading\n\n".chars() {
            renderer.push_and_render(&c.to_string(), None);
        }
        let output = renderer.view();

        assert!(!output.lines.is_empty(), "Should produce lines");

        // After complete heading, should have styled output
        let first_line = &output.lines[0];
        let has_style = first_line.spans.iter().any(|span| {
            span.style.fg.is_some()
                || span
                    .style
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
        });
        assert!(
            has_style,
            "Incrementally streamed heading should have styling. Spans: {:?}",
            first_line.spans
        );
    }

    #[test]
    fn test_line_source_map_code_block() {
        assert_line_source_map_matches("```rust\nlet x = 1;\n```\n", true);
    }

    /// Debug test: trace streaming behavior with demo chunks
    #[test]
    fn test_demo_streaming_chunks() {
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);

        let chunks = [
            "# Streaming Demo\n\n",
            "This text is being streamed ",
            "**incrementally** ",
            "just like a real LLM response!\n\n",
        ];

        for (i, chunk) in chunks.iter().enumerate() {
            renderer.push_and_render(chunk, None);
            let output = renderer.view();

            // Collect output info while we still have the borrow
            let line_count = output.lines.len();
            let lines_debug: Vec<String> = output
                .lines
                .iter()
                .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();

            eprintln!("=== After chunk {} ===", i);
            eprintln!("Chunk: {:?}", chunk);
            eprintln!(
                "Frozen: {} bytes, {} lines",
                renderer.frozen_bytes(),
                renderer.frozen_lines_count()
            );
            eprintln!("Output lines: {}", line_count);
            for (j, text) in lines_debug.iter().enumerate() {
                eprintln!("  Line {}: {:?}", j, text);
            }
            eprintln!();
        }

        // After all chunks, should have 3 lines:
        // - heading
        // - blank line separator (between blocks)
        // - paragraph
        let final_output = renderer.view();
        assert_eq!(
            final_output.lines.len(),
            3,
            "Should have 3 lines: heading + separator + paragraph. Got: {}",
            final_output.lines.len()
        );
    }

    /// Test trailing newlines don't create extra empty lines at the end.
    #[test]
    fn test_no_trailing_empty_lines() {
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);

        // Push content ending with blank lines (like the demo table row)
        // The table row "| 10KB | 850ms | 10ms |\n\n" ends with \n\n
        renderer.push_and_render("| A | B |\n|---|---|\n| 1 | 2 |\n\n", None);
        let output = renderer.view();

        let line_count = output.lines.len();
        eprintln!("Total lines: {}", line_count);
        for (i, line) in output.lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            eprintln!("  Line {}: {:?} (empty: {})", i, text, text.is_empty());
        }

        // Check if the last line is empty (which would be a trailing newline issue)
        let last_line = output.lines.last().expect("Should have lines");
        let last_text: String = last_line.spans.iter().map(|s| s.content.as_ref()).collect();

        // The last line should NOT be empty (trailing blank lines are bad)
        assert!(
            !last_text.is_empty(),
            "Last line should not be empty. Got {} lines with last = {:?}",
            line_count,
            last_text
        );
    }

    /// Test the actual demo ending chunks don't produce trailing empty lines.
    #[test]
    fn test_demo_ending_no_trailing_lines() {
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);

        // These are the actual final chunks from the demo
        let chunks = [
            "| Feature | Before | After |\n",
            "|---------|--------|-------|\n",
            "| Complexity | O(N²) | O(N) |\n",
            "| 10KB render | 850ms | 10ms |\n\n",
            "✨ *Streaming complete!*",
        ];

        for chunk in chunks {
            renderer.push_and_render(chunk, None);
        }
        let output = renderer.view();

        let line_count = output.lines.len();
        eprintln!("Total lines after demo ending: {}", line_count);
        for (i, line) in output.lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            eprintln!("  Line {}: {:?}", i, text);
        }

        // Count trailing empty lines
        let trailing_empty = output
            .lines
            .iter()
            .rev()
            .take_while(|line| {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                text.is_empty()
            })
            .count();

        assert_eq!(
            trailing_empty, 0,
            "Should have no trailing empty lines, got {}",
            trailing_empty
        );
    }

    /// Test that streaming produces the same output as full render.
    /// This is the key correctness test - streaming should be identical to full render.
    #[test]
    fn test_streaming_matches_full_render_with_block_spacing() {
        let full_content = "# Heading\n\nParagraph one.\n\n## Subheading\n\nParagraph two.\n\n";

        // Full render
        let (full_output, _) =
            render_markdown_ratatui_full(full_content, test_style::STYLE, true, None);

        // Streaming render (chunk by chunk)
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        let chunks = [
            "# Heading\n\n",
            "Paragraph one.\n\n",
            "## Subheading\n\n",
            "Paragraph two.\n\n",
        ];
        for chunk in chunks {
            renderer.push_and_render(chunk, None);
            // push() now renders automatically // Render after each chunk
        }
        let streaming_output = renderer.view();

        // Debug output
        eprintln!("Full render ({} lines):", full_output.lines.len());
        for (i, line) in full_output.lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            eprintln!("  Line {}: {:?}", i, text);
        }
        eprintln!("Streaming render ({} lines):", streaming_output.lines.len());
        for (i, line) in streaming_output.lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            eprintln!("  Line {}: {:?}", i, text);
        }

        // Also check what each chunk produces individually
        eprintln!("\nIndividual chunk renders:");
        for chunk in &chunks {
            let (chunk_output, _) =
                render_markdown_ratatui_full(chunk, test_style::STYLE, true, None);
            eprintln!("Chunk {:?} -> {} lines:", chunk, chunk_output.lines.len());
            for (i, line) in chunk_output.lines.iter().enumerate() {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                eprintln!("    Line {}: {:?}", i, text);
            }
        }

        // They should match exactly
        assert_eq!(
            streaming_output.lines.len(),
            full_output.lines.len(),
            "Streaming should produce same number of lines as full render"
        );

        for (i, (stream_line, full_line)) in streaming_output
            .lines
            .iter()
            .zip(full_output.lines.iter())
            .enumerate()
        {
            let stream_text: String = stream_line
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            let full_text: String = full_line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(
                stream_text, full_text,
                "Line {} mismatch: streaming={:?}, full={:?}",
                i, stream_text, full_text
            );
        }
    }

    // Comprehensive blank line tests - streaming must match full render exactly

    /// Helper to compare streaming vs full render for given chunks.
    /// Content is derived by joining chunks - no need to specify it separately.
    #[track_caller]
    fn assert_streaming_equals_full(chunks: &[&str], description: &str) {
        // Derive content from chunks
        let content: String = chunks.iter().copied().collect();

        // Full render
        let (full_output, _) =
            render_markdown_ratatui_full(&content, test_style::STYLE, true, None);

        // Streaming render
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        for chunk in chunks {
            renderer.push_and_render(chunk, None);
            // push() now renders automatically
        }
        let streaming_output = renderer.view();

        // Collect info while we have the borrow
        let streaming_lines: Vec<String> = streaming_output
            .lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let frozen_bytes = renderer.frozen_bytes();
        let frozen_source = renderer.source()[..frozen_bytes].to_string();
        let trailing_blanks = count_trailing_blank_lines(&frozen_source);

        let full_lines: Vec<String> = full_output
            .lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        // Check for mismatch
        let lines_match = streaming_lines.len() == full_lines.len();
        let content_matches = streaming_lines == full_lines;

        if !lines_match || !content_matches {
            eprintln!("=== {} ===", description);
            eprintln!("Content: {:?}", content);
            eprintln!("Chunks: {:?}", chunks);
            eprintln!("Full render ({} lines):", full_lines.len());
            for (i, text) in full_lines.iter().enumerate() {
                eprintln!("  Line {}: {:?}", i, text);
            }
            eprintln!("Streaming render ({} lines):", streaming_lines.len());
            for (i, text) in streaming_lines.iter().enumerate() {
                eprintln!("  Line {}: {:?}", i, text);
            }
            eprintln!(
                "Frozen source ({} bytes): {:?}",
                frozen_bytes, frozen_source
            );
            eprintln!("Trailing blank lines in frozen: {}", trailing_blanks);
        }

        assert_eq!(
            streaming_lines.len(),
            full_lines.len(),
            "{}: line count mismatch",
            description
        );

        for (i, (stream_text, full_text)) in
            streaming_lines.iter().zip(full_lines.iter()).enumerate()
        {
            assert_eq!(
                stream_text, full_text,
                "{}: line {} mismatch",
                description, i
            );
        }
    }

    #[test]
    fn test_streaming_double_newline() {
        assert_streaming_equals_full(&["# Heading\n\n", "Paragraph\n\n"], "double newline");
    }

    #[test]
    fn test_streaming_triple_newline() {
        assert_streaming_equals_full(&["# Heading\n\n\n", "Paragraph\n\n"], "triple newline");
    }

    #[test]
    fn test_streaming_quadruple_newline() {
        assert_streaming_equals_full(&["# Heading\n\n\n\n", "Paragraph\n\n"], "quadruple newline");
    }

    #[test]
    fn test_streaming_newlines_with_spaces() {
        assert_streaming_equals_full(
            &["# Heading\n\n  \n\n", "Paragraph\n\n"],
            "newlines with spaces",
        );
    }

    #[test]
    fn test_streaming_code_block_then_paragraph() {
        // Test that blank line after code block is preserved
        assert_streaming_equals_full(
            &[
                "```rust\nfn main() {}\n```\n\n",
                "Paragraph after code.\n\n",
            ],
            "code block then paragraph",
        );
    }

    /// Test that code blocks have proper syntax highlighting in streaming mode.
    #[test]
    fn test_streaming_code_block_syntax_highlighting() {
        let chunks = [
            "```rust\n",
            "fn main() {\n",
            "    println!(\"Hello!\");\n",
            "}\n",
            "```\n\n",
        ];
        let content: String = chunks.iter().copied().collect();

        // Full render
        let (full_output, _) =
            render_markdown_ratatui_full(&content, test_style::STYLE, true, None);

        // Streaming render
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        for chunk in &chunks {
            renderer.push_and_render(chunk, None);
            // push() now renders automatically
        }
        let streaming_output = renderer.view();

        // Check that streaming and full have the same line count
        assert_eq!(
            streaming_output.lines.len(),
            full_output.lines.len(),
            "Line count should match"
        );

        // Check that spans match (indicating syntax highlighting worked)
        for (i, (stream_line, full_line)) in streaming_output
            .lines
            .iter()
            .zip(full_output.lines.iter())
            .enumerate()
        {
            assert_eq!(
                stream_line.spans.len(),
                full_line.spans.len(),
                "Line {} span count should match (syntax highlighting)",
                i
            );
        }
    }

    #[test]
    fn test_streaming_code_block_triple_newline() {
        // Test multiple blank lines after code block
        assert_streaming_equals_full(
            &["```rust\nfn main() {}\n```\n\n\n", "Paragraph\n\n"],
            "code block triple newline",
        );
    }

    #[test]
    fn test_streaming_table_then_paragraph() {
        assert_streaming_equals_full(
            &["| A | B |\n|---|---|\n| 1 | 2 |\n\n", "Paragraph\n\n"],
            "table then paragraph",
        );
    }

    #[test]
    fn test_streaming_list_then_paragraph() {
        assert_streaming_equals_full(
            &["- Item 1\n- Item 2\n\n", "Paragraph\n\n"],
            "list then paragraph",
        );
    }

    #[test]
    fn test_streaming_blockquote_then_paragraph() {
        assert_streaming_equals_full(
            &["> Quote line 1\n> Quote line 2\n\n", "Paragraph\n\n"],
            "blockquote then paragraph",
        );
    }

    #[test]
    fn test_streaming_nested_blockquote() {
        // Test nested blockquotes with "│ │ " prefix
        assert_streaming_equals_full(
            &[
                "> Outer quote\n>> Nested quote\n> Back to outer\n\n",
                "Paragraph\n\n",
            ],
            "nested blockquote",
        );
    }

    /// Nested blockquote with blank lines and a list, streamed token-by-token.
    #[test]
    fn test_streaming_nested_blockquote_with_list() {
        // Token-by-token (realistic LLM chunking)
        assert_streaming_equals_full(
            &["> Foo\n", ">\n", "> > Bar\n", "> >\n", "> > - Baz\n"],
            "nested blockquote with list (line-by-line)",
        );
        // Also test char-by-char
        assert_streaming_matches_full("> Foo\n>\n> > Bar\n> >\n> > - Baz\n", true);
    }

    #[test]
    fn test_blockquote_prefix_rendering() {
        // Verify blockquotes render with "│" prefix instead of ">"
        let text = "> Single line quote\n\n>> Nested quote\n\n";
        let (output, _) = render_markdown_ratatui_full(text, test_style::STYLE, true, None);

        let lines: Vec<String> = output
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert_eq!(lines[0], "│ Single line quote");
        assert_eq!(lines[2], "││ Nested quote");
    }

    #[test]
    fn test_streaming_thematic_break() {
        assert_streaming_equals_full(&["Above\n\n", "---\n\n", "Below\n\n"], "thematic break");
    }

    /// Regression: thematic break `---` at the end of a chunk (no trailing newline)
    /// was invisible in pretty mode because the checkpoint's `output_lines` didn't
    /// include the `───` line (it was pending in `current_spans`, unflushed).
    ///
    #[test]
    fn test_streaming_thematic_break_at_chunk_boundary() {
        assert_streaming_equals_full(
            &["hello\n\n---", "\n### World", "\nText"],
            "thematic break at chunk boundary",
        );
    }

    #[test]
    fn test_streaming_multiple_paragraphs() {
        assert_streaming_equals_full(
            &["Para 1\n\n", "Para 2\n\n", "Para 3\n\n"],
            "multiple paragraphs",
        );
    }

    #[test]
    fn test_streaming_mixed_blocks() {
        assert_streaming_equals_full(
            &[
                "# Heading\n\n",
                "Paragraph.\n\n",
                "```\ncode\n```\n\n",
                "- list\n\n",
                "> quote\n\n",
            ],
            "mixed blocks",
        );
    }

    /// The exact content from pager_v3_demo that shows bugs.
    const DEMO_CONTENT: &str = r#"# Streaming Demo

This text is being streamed **incrementally** just like a real LLM response!


## How It Works

The `StreamingMarkdownRenderer` efficiently handles chunks by:

1. Accumulating text in a buffer
2. Detecting stable block boundaries
3. Freezing rendered output up to checkpoints
4. Only re-rendering the tail

```rust
// This code block appears character by character!
fn stream_demo() {
    println!("Hello from streaming!");
}
```

The frozen lines are **never re-rendered**, making streaming O(N) instead of O(N²).

> **Note:** This blockquote contains *italic*, **bold**, and `inline code`.
> It spans multiple lines to test blockquote rendering.

---

| Feature | Before | After |
|---------|--------|-------|
| Complexity | O(N²) | O(N) |
| 10KB render | 850ms | 10ms |

✨ *Streaming complete!*"#;

    /// Test streaming with 4-char chunks (matches demo)
    #[test]
    fn test_demo_content_4char_chunks() {
        // Split into 4-char chunks like the demo
        let chunks = split_into_chunks(DEMO_CONTENT, 4);
        let full_content: String = chunks.iter().copied().collect();
        assert_eq!(full_content, DEMO_CONTENT);

        // Full render
        let (full_output, _) =
            render_markdown_ratatui_full(DEMO_CONTENT, test_style::STYLE, true, None);

        // Streaming render
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        for chunk in &chunks {
            renderer.push_and_render(chunk, None);
            // push() now renders automatically
        }
        let streaming_output = renderer.view();

        // Compare line by line
        assert_eq!(
            streaming_output.lines.len(),
            full_output.lines.len(),
            "Demo content: Line count should match (streaming: {}, full: {})",
            streaming_output.lines.len(),
            full_output.lines.len()
        );

        for (i, (stream_line, full_line)) in streaming_output
            .lines
            .iter()
            .zip(full_output.lines.iter())
            .enumerate()
        {
            let stream_text: String = stream_line
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            let full_text: String = full_line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(
                stream_text, full_text,
                "Demo content line {}: text mismatch\nStreaming: {:?}\nFull: {:?}",
                i, stream_text, full_text
            );
        }
    }

    /// Comprehensive test document with various edge cases.
    /// Covers: headings, ALL list types, blockquotes, code blocks, styling,
    /// various newline patterns (double, triple, quadruple),
    /// spaces/tabs between newlines, etc.
    const EDGE_CASE_DOC: &str = concat!(
        // Heading with double newline (standard)
        "# Heading One\n\n",
        // Paragraph
        "Some **bold** and *italic* text.\n\n",
        // Heading with triple newline
        "## Heading Two\n\n\n",
        // Numbered list (1. 2. 3.)
        "1. First item\n",
        "2. Second item\n",
        "3. Third item\n\n",
        // Heading with quadruple newline
        "### Heading Three\n\n\n\n",
        // Blockquote with styling
        "> Quote with **bold** and `code`\n",
        "> Second quote line\n\n",
        // Heading with space between newlines
        "#### Heading Four\n \n",
        // Dash bullet list (-)
        "- Dash one\n",
        "- Dash two\n\n",
        // Asterisk bullet list (*)
        "* Star one\n",
        "* Star two\n\n",
        // Plus bullet list (+)
        "+ Plus one\n",
        "+ Plus two\n\n",
        // Nested list
        "- Parent item\n",
        "  - Nested child\n",
        "  - Another child\n",
        "- Back to parent\n\n",
        // Heading with tab between newlines
        "##### Heading Five\n\t\n",
        // Code block
        "```rust\nfn main() {\n    println!(\"Hello\");\n}\n```\n\n",
        // Heading
        "###### Heading Six\n\n",
        // RESTORE: Mixed whitespace - space, newline, tab, newline (the hidden bug!)
        "Final paragraph.\n \n\t\n",
        // Trailing content
        "The end.",
    );

    /// Test ALL possible 2-way split points for the edge case document.
    #[test]
    fn test_edge_cases_2way_splits() {
        let text = EDGE_CASE_DOC;

        let (full_output, _) = render_markdown_ratatui_full(text, test_style::STYLE, true, None);
        let full_lines: Vec<String> = full_output
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        let mut failures = Vec::new();

        // Test every possible split point
        for split_at in 1..text.len() {
            if !text.is_char_boundary(split_at) {
                continue;
            }

            let chunk1 = &text[..split_at];
            let chunk2 = &text[split_at..];

            let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
            renderer.push_and_render(chunk1, None);
            // push() now renders automatically
            renderer.push_and_render(chunk2, None);
            let streaming_output = renderer.view();

            let streaming_lines: Vec<String> = streaming_output
                .lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();

            if streaming_lines != full_lines {
                // Find first difference
                let diff_line = streaming_lines
                    .iter()
                    .zip(full_lines.iter())
                    .enumerate()
                    .find(|(_, (s, f))| s != f)
                    .map(|(i, _)| i);

                failures.push(format!(
                    "byte {}: stream={} lines, full={} lines, first_diff={:?}\n  chunk1_end: {:?}\n  chunk2_start: {:?}",
                    split_at,
                    streaming_lines.len(),
                    full_lines.len(),
                    diff_line,
                    &chunk1[chunk1.len().saturating_sub(30)..],
                    &chunk2[..chunk2.len().min(30)],
                ));
            }
        }

        if !failures.is_empty() {
            panic!(
                "{} failures out of {} split points:\n{}",
                failures.len(),
                text.len() - 1,
                failures.join("\n")
            );
        }
    }

    /// Test 4-way splits: split into 2, then split each half again.
    /// This catches bugs that only manifest with multiple re-renders.
    #[test]
    fn test_edge_cases_4way_splits() {
        let text = EDGE_CASE_DOC;

        let (full_output, _) = render_markdown_ratatui_full(text, test_style::STYLE, true, None);
        let full_lines: Vec<String> = full_output
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        let mut failures = Vec::new();
        let mut tested = 0;

        // For each primary split point
        for split1 in 1..text.len() {
            if !text.is_char_boundary(split1) {
                continue;
            }

            let first_half = &text[..split1];
            let second_half = &text[split1..];

            // Split first half (if possible)
            let first_splits: Vec<usize> = if first_half.len() > 1 {
                vec![first_half.len() / 2]
            } else {
                vec![first_half.len()] // No split, use whole thing
            };

            // Split second half (if possible)
            let second_splits: Vec<usize> = if second_half.len() > 1 {
                vec![second_half.len() / 2]
            } else {
                vec![second_half.len()]
            };

            for &sub1 in &first_splits {
                // Ensure valid char boundary
                let sub1 = find_char_boundary(first_half, sub1);

                for &sub2 in &second_splits {
                    let sub2 = find_char_boundary(second_half, sub2);

                    let chunks: Vec<&str> = vec![
                        &first_half[..sub1],
                        &first_half[sub1..],
                        &second_half[..sub2],
                        &second_half[sub2..],
                    ]
                    .into_iter()
                    .filter(|c| !c.is_empty())
                    .collect();

                    if chunks.len() < 2 {
                        continue; // Need at least 2 chunks
                    }

                    tested += 1;

                    let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
                    for chunk in &chunks {
                        renderer.push_and_render(chunk, None);
                        // push() now renders automatically
                    }
                    let streaming_output = renderer.view();

                    let streaming_lines: Vec<String> = streaming_output
                        .lines
                        .iter()
                        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                        .collect();

                    if streaming_lines != full_lines {
                        let chunk_preview: Vec<String> = chunks
                            .iter()
                            .map(|c| {
                                if c.len() > 15 {
                                    format!("{:?}...", &c[..15])
                                } else {
                                    format!("{:?}", c)
                                }
                            })
                            .collect();
                        failures.push(format!(
                            "4-way split at {}: [{}]",
                            split1,
                            chunk_preview.join(", ")
                        ));
                    }
                }
            }
        }

        if !failures.is_empty() {
            panic!(
                "{} failures out of {} 4-way split combinations:\n{}",
                failures.len(),
                tested,
                failures
                    .iter()
                    .take(20)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
    }

    /// Find nearest valid char boundary at or before `pos`.
    fn find_char_boundary(s: &str, pos: usize) -> usize {
        let mut p = pos.min(s.len());
        while p > 0 && !s.is_char_boundary(p) {
            p -= 1;
        }
        p
    }

    /// Test ALL possible split points for a smaller test document.
    /// For a document of N bytes, there are N-1 possible split points.
    /// This test catches edge cases at every possible boundary.
    #[test]
    fn test_all_split_points() {
        // A smaller document that covers key features
        let text = "# Heading\n\n1. Item\n2. Item\n\n> Quote\n\n";

        let (full_output, _) = render_markdown_ratatui_full(text, test_style::STYLE, true, None);
        let full_lines: Vec<String> = full_output
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        // Test every possible split point
        for split_at in 1..text.len() {
            if !text.is_char_boundary(split_at) {
                continue;
            }

            let chunk1 = &text[..split_at];
            let chunk2 = &text[split_at..];

            let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
            renderer.push_and_render(chunk1, None);
            let view1 = renderer.view();
            let lines_after_chunk1: Vec<String> = view1
                .lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            let frozen_before = (renderer.frozen.source_bytes, renderer.frozen.lines_len);

            renderer.push_and_render(chunk2, None);
            let streaming_output = renderer.view();

            let streaming_lines: Vec<String> = streaming_output
                .lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();

            assert_eq!(
                streaming_lines,
                full_lines,
                "Split at byte {}: MISMATCH\n\
                chunk1: {:?}\n\
                chunk2: {:?}\n\
                After chunk1: {:?} (frozen: {:?})\n\
                Streaming lines: {:?}\n\
                Full lines: {:?}",
                split_at,
                chunk1,
                chunk2,
                lines_after_chunk1,
                frozen_before,
                streaming_lines,
                full_lines
            );
        }
    }

    /// Test blockquote specifically - this is reported as broken in demo
    #[test]
    fn test_blockquote_multiline_streaming() {
        let text = "> Line 1\n> Line 2\n\n";

        // Split in various ways
        for chunk_size in 1..=text.len() {
            let chunks = split_into_chunks(text, chunk_size);
            let rejoined: String = chunks.iter().copied().collect();
            assert_eq!(rejoined, text);

            let (full_output, _) =
                render_markdown_ratatui_full(text, test_style::STYLE, true, None);

            let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
            for chunk in &chunks {
                renderer.push_and_render(chunk, None);
                // push() now renders automatically
            }
            let streaming_output = renderer.view();

            let streaming_lines: Vec<String> = streaming_output
                .lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            let full_lines: Vec<String> = full_output
                .lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();

            assert_eq!(
                streaming_lines, full_lines,
                "Blockquote with chunk_size {}: mismatch\nStreaming: {:?}\nFull: {:?}",
                chunk_size, streaming_lines, full_lines
            );
        }
    }

    /// Test numbered list specifically - freestanding "1." bug
    #[test]
    fn test_numbered_list_streaming() {
        let text = "1. First\n2. Second\n3. Third\n\n";

        // Split in various ways
        for chunk_size in 1..=text.len() {
            let chunks = split_into_chunks(text, chunk_size);
            let rejoined: String = chunks.iter().copied().collect();
            assert_eq!(rejoined, text);

            let (full_output, _) =
                render_markdown_ratatui_full(text, test_style::STYLE, true, None);

            let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
            for chunk in &chunks {
                renderer.push_and_render(chunk, None);
                // push() now renders automatically
            }
            let streaming_output = renderer.view();

            let streaming_lines: Vec<String> = streaming_output
                .lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            let full_lines: Vec<String> = full_output
                .lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();

            assert_eq!(
                streaming_lines, full_lines,
                "Numbered list with chunk_size {}: mismatch\nStreaming: {:?}\nFull: {:?}",
                chunk_size, streaming_lines, full_lines
            );
        }
    }

    /// URL detection runs in BOTH `render()` and `finish()`.  This test
    /// pins:
    ///   1. The URL surfaces as a HyperlinkTarget during streaming
    ///      (every `push_and_render` call), not only after `finish()`.
    ///   2. `finish()` does not duplicate the URL HyperlinkTarget that
    ///      `render()` already added — the dedup in `detect_plain_urls`
    ///      makes the second pass idempotent.
    ///   3. The full `HyperlinkTarget` (URL + line_index + column_range)
    ///      is identical before and after `finish()`.  Ids may be
    ///      reassigned by `finish()` (it restarts the parser counter at
    ///      0), but every other field must match.
    #[test]
    fn streaming_byte_by_byte_url_appears_during_render_and_survives_finish() {
        let text = "See https://example.com for details.\n";

        // Stream byte-by-byte
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        for byte in text.as_bytes() {
            let buf = [*byte];
            let s = std::str::from_utf8(&buf).expect("ascii test input");
            renderer.push_and_render(s, None);
        }

        // Snapshot the URL HyperlinkTarget before finish().
        let before: Vec<_> = renderer
            .view()
            .hyperlinks
            .iter()
            .filter(|h| h.url == "https://example.com")
            .map(|h| (h.url.clone(), h.line_index, h.column_range.clone()))
            .collect();
        assert_eq!(
            before.len(),
            1,
            "render() must surface the plain URL exactly once before finish(); \
             got hyperlinks: {:?}",
            renderer.view().hyperlinks,
        );

        // After finish: the URL must still be present, with the same
        // (URL, line_index, column_range), and no duplicates.
        renderer.finish(None);
        let after: Vec<_> = renderer
            .view()
            .hyperlinks
            .iter()
            .filter(|h| h.url == "https://example.com")
            .map(|h| (h.url.clone(), h.line_index, h.column_range.clone()))
            .collect();
        assert_eq!(
            before, after,
            "finish() must preserve URL HyperlinkTargets added by render() \
             (location-stable, no duplicates)",
        );
    }

    /// URL split across two `push_and_render` boundaries: the renderer
    /// must produce a single full-URL HyperlinkTarget after both chunks,
    /// not a stale partial-URL target left over from the first chunk.
    /// Regression guard against the dedup-overlap trap where a frozen
    /// partial-URL hyperlink would block detection of the full URL on
    /// the next render.
    #[test]
    fn streaming_url_split_across_chunks_produces_single_full_target() {
        let part1 = "[link](https://exam";
        let part2 = "ple.com/some/path)\n";

        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render(part1, None);
        renderer.push_and_render(part2, None);
        let view = renderer.view();

        let full_url = "https://example.com/some/path";
        let matches: Vec<&HyperlinkTarget> = view
            .hyperlinks
            .iter()
            .filter(|h| h.url == full_url)
            .collect();
        // Pretty-mode `[link](url)` produces two HyperlinkTargets pointing
        // at the same URL: the parser one over "link" and the url_scan one
        // over the `(url)` suffix.  Pinning the EXACT count guards against
        // (a) a parser hyperlink dropped on the chunk-boundary, leaving
        // only url_scan's; and (b) url_scan adding a duplicate.
        assert_eq!(
            matches.len(),
            2,
            "the full URL must be present as exactly the parser + url_scan \
             HyperlinkTargets; got hyperlinks: {:?}",
            view.hyperlinks,
        );
        // The two ranges must be disjoint — the parser one covers "link",
        // the url_scan one covers the URL in the `(url)` suffix.
        let (a, b) = (&matches[0], &matches[1]);
        assert!(
            a.column_range.end <= b.column_range.start
                || b.column_range.end <= a.column_range.start,
            "the parser and url_scan ranges for the same URL must be disjoint; \
             got {:?} and {:?}",
            a.column_range,
            b.column_range,
        );
        // No partial-URL hyperlink should survive.
        let stale: Vec<&HyperlinkTarget> = view
            .hyperlinks
            .iter()
            .filter(|h| h.url.starts_with("https://exam") && h.url != full_url)
            .collect();
        assert!(
            stale.is_empty(),
            "no partial-URL HyperlinkTargets must linger across chunks; got {:?}",
            stale,
        );
    }

    /// Idempotency: repeated `render()` calls with no source change must
    /// produce the same `view().hyperlinks` — same URLs, line indices,
    /// column ranges, AND ids.  Without dedup, each call would re-add
    /// the url_scan results; without deterministic id assignment, ids
    /// would drift between calls and break OSC 8 grouping continuity.
    #[test]
    fn back_to_back_render_calls_are_idempotent() {
        let text = "See https://example.com and [link](https://other.example).\n";
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render(text, None);

        // Snap includes `id`: a regression where ids drift across renders
        // (e.g. a non-reset global counter) would fail here.  Since the
        // source is unchanged, the parser counter restarts at the same
        // `frozen.next_link_id` and the url_scan counter also resumes at
        // a stable value — every field of every hyperlink must match.
        let snap =
            |r: &StreamingMarkdownRenderer| -> Vec<(u32, String, usize, std::ops::Range<usize>)> {
                r.view()
                    .hyperlinks
                    .iter()
                    .map(|h| (h.id, h.url.clone(), h.line_index, h.column_range.clone()))
                    .collect()
            };
        let s1 = snap(&renderer);
        renderer.render(None);
        let s2 = snap(&renderer);
        renderer.render(None);
        let s3 = snap(&renderer);

        assert_eq!(s1, s2, "second render() must produce identical hyperlinks");
        assert_eq!(s2, s3, "third render() must produce identical hyperlinks");
    }

    /// Across multiple streaming chunks every emitted hyperlink id must
    /// be unique.  Catches a regression where url_scan reuses an id
    /// already assigned to a parser hyperlink (or vice versa), which
    /// would silently merge OSC 8 hyperlinks for the terminal.
    ///
    /// The first assertion is the regression target: it runs on the
    /// streaming-path output (post-`rerender_tail`, pre-`finish()`).  A
    /// regression in the `post_scan_next_id` vs `tail_next_link_id`
    /// bookkeeping (production change at the bottom of `rerender_tail`)
    /// would surface here.  We keep a second post-`finish()` assertion
    /// because `finish()`'s full re-render is the recovery path that
    /// users always see eventually — both must produce unique ids.
    #[test]
    fn render_assigns_monotonic_ids_across_chunks() {
        let chunks = [
            "Para one: [a](https://a.example).\n\n",
            "Para two: see https://b.example here.\n\n",
            "Para three: [c](https://c.example) end.\n",
        ];
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        for chunk in &chunks {
            renderer.push_and_render(chunk, None);
        }

        // Pre-finish assertion: this is the regression target. `finish()`
        // restarts the parser counter at 0 and re-numbers everything, so
        // a `rerender_tail` ID-counter regression would NOT surface
        // after `finish()` — only here.
        let pre_finish_view = renderer.view();
        let pre_finish_ids: std::collections::HashSet<u32> =
            pre_finish_view.hyperlinks.iter().map(|h| h.id).collect();
        assert_eq!(
            pre_finish_ids.len(),
            pre_finish_view.hyperlinks.len(),
            "every hyperlink id must be unique BEFORE finish() (streaming path); \
             got: {:?}",
            pre_finish_view.hyperlinks,
        );

        // Secondary post-finish assertion: full-render path must also
        // produce unique ids.
        renderer.finish(None);
        let view = renderer.view();
        let ids: std::collections::HashSet<u32> = view.hyperlinks.iter().map(|h| h.id).collect();
        assert_eq!(
            ids.len(),
            view.hyperlinks.len(),
            "every hyperlink id must be unique after finish(); got: {:?}",
            view.hyperlinks,
        );
    }

    /// Pin: byte-by-byte streaming through a checkpoint advance with a
    /// URL straddling the freeze boundary must produce a single
    /// full-URL hyperlink (not a stuck partial-URL one left over from
    /// when only the prefix was visible).
    ///
    /// The PRE-finish assertion is the regression target: the
    /// concern is a partial-URL hyperlink left in the streaming-path
    /// `view().hyperlinks`.  `finish()` does a full re-render and would
    /// recover from any stuck state, masking the regression — so the
    /// streaming-path snapshot is taken first and the assertion runs on
    /// it directly.  The post-finish assertion is retained as a
    /// secondary check.
    #[test]
    fn streaming_byte_by_byte_through_checkpoint_with_url_at_boundary() {
        let text = "# Header\n\nSee https://example.com/path here.\n\n";
        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        for byte in text.as_bytes() {
            let buf = [*byte];
            let s = std::str::from_utf8(&buf).expect("ascii test input");
            renderer.push_and_render(s, None);
        }

        // Closure: assert the URL is fully present and no partial-URL
        // hyperlinks linger.  Used for both the pre- and post-finish
        // snapshots so they exercise the identical invariant.
        let assert_clean = |view: &MarkdownRenderView<'_>, when: &str| {
            let full_url_count = view
                .hyperlinks
                .iter()
                .filter(|h| h.url == "https://example.com/path")
                .count();
            assert_eq!(
                full_url_count, 1,
                "{when}: the full URL must be exactly one HyperlinkTarget; got: {:?}",
                view.hyperlinks,
            );
            let stale: Vec<&HyperlinkTarget> = view
                .hyperlinks
                .iter()
                .filter(|h| h.url.starts_with("https://") && h.url != "https://example.com/path")
                .collect();
            assert!(
                stale.is_empty(),
                "{when}: no partial-URL HyperlinkTargets must remain; got {:?}",
                stale,
            );
        };

        // Pre-finish: the regression target.
        let pre_finish_view = renderer.view();
        assert_clean(&pre_finish_view, "before finish()");

        // Post-finish: secondary check.
        renderer.finish(None);
        let view = renderer.view();
        assert_clean(&view, "after finish()");
    }

    /// A document with one markdown link `[a](url)` and one plain URL
    /// `https://b.example` after `finish` should have monotonic IDs:
    /// the markdown-link target with `id = 0`, and the plain-URL target
    /// with a higher id (continuing from `frozen.next_link_id`).
    ///
    /// NOTE: one might expect `id = 1` for the plain URL.
    /// In pretty mode, `[a](url)` renders as `a (url)`, so the url_scan
    /// pass assigns id=1 to the pretty-mode suffix first, pushing the
    /// plain URL to id=2.
    #[test]
    fn url_scan_ids_continue_from_frozen_counter() {
        let text = "[a](https://a.example) and https://b.example\n";

        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        renderer.push_and_render(text, None);
        let view = renderer.finish(None);
        let hyperlinks = view.hyperlinks;

        assert!(
            hyperlinks.len() >= 2,
            "expected at least 2 hyperlinks, got {}",
            hyperlinks.len()
        );

        let md_link = hyperlinks
            .iter()
            .find(|h| h.url == "https://a.example")
            .expect("markdown link target should exist");
        let plain_url = hyperlinks
            .iter()
            .find(|h| h.url == "https://b.example")
            .expect("plain URL target should exist");

        assert_eq!(md_link.id, 0, "markdown link should have id=0");
        // id=1 is taken by the pretty-mode URL suffix for `(https://a.example)`,
        // so the plain URL gets id=2.
        assert_eq!(
            plain_url.id, 2,
            "plain URL gets id=2 (parser id=0, pretty-mode suffix id=1)"
        );
    }

    /// Helper: split text into chunks of approximately `chunk_size` bytes.
    fn split_into_chunks(text: &str, chunk_size: usize) -> Vec<&str> {
        let mut chunks = Vec::new();
        let bytes = text.as_bytes();
        let mut start = 0;

        while start < bytes.len() {
            let end = (start + chunk_size).min(bytes.len());
            // Don't split in the middle of a UTF-8 char
            let mut actual_end = end;
            while actual_end > start && !text.is_char_boundary(actual_end) {
                actual_end -= 1;
            }
            if actual_end == start {
                actual_end = end;
                while actual_end < bytes.len() && !text.is_char_boundary(actual_end) {
                    actual_end += 1;
                }
            }
            chunks.push(&text[start..actual_end]);
            start = actual_end;
        }

        chunks
    }

    #[test]
    fn test_malformed_table_wraps_per_row() {
        // 11-column header but 12-cell delimiter — pulldown-cmark rejects
        // as a table.  Each pipe-prefixed row must stay on its own line
        // (soft break NOT collapsed) so the TUI can wrap it.
        let text = "\
| ColA | ColB | ColC | ColD | ColE | ColF | ColG | ColH | ColI | ColJ | ColK |
|---|---|---|---|---|---|---|---|---|---|---|------------------------------------|\n\
| A001 | data | more | 2026-01-01 | 1.00 | 0.0 | 20.0 | 10.0 | 50.00 | left | 1 |

";
        let (full_output, _) = render_markdown_ratatui_full(text, test_style::STYLE, true, None);
        // Must NOT collapse into a single line — each source row should
        // produce its own output line.
        assert!(
            full_output.lines.len() >= 3,
            "malformed table rows must not collapse to 1 line, got {} lines",
            full_output.lines.len()
        );
        // Streaming must match.
        assert_streaming_matches_full_both(text);
    }

    // ----------------------------------------------------------------------
    // Syntect-enabled streaming equivalence (incremental open-code highlighter)
    // ----------------------------------------------------------------------

    /// Build a nested YAML body of at least `num_lines` lines (no fences).
    fn yaml_body(num_lines: usize) -> String {
        let mut out = String::new();
        let mut i = 0usize;
        let mut lines = 0usize;
        while lines < num_lines {
            for line in [
                format!("service_{i}:"),
                format!("  name: \"svc-{i}\""),
                "  enabled: true".to_string(),
                format!("  replicas: {}", i % 7 + 1),
                "  env:".to_string(),
                "    - name: LOG_LEVEL".to_string(),
                format!(
                    "      value: \"{}\"",
                    if i.is_multiple_of(2) { "info" } else { "debug" }
                ),
                "  ports:".to_string(),
                format!("    - {}", 8000 + i),
            ] {
                out.push_str(&line);
                out.push('\n');
                lines += 1;
            }
            i += 1;
        }
        out
    }

    /// Stream `text` in `chunk`-byte pieces (char-boundary aware), rendering
    /// after every chunk so the incremental open-code cache is exercised, then
    /// assert the final view matches a one-shot full render byte-for-byte
    /// (both `lines` and `line_source_map`).
    #[track_caller]
    fn assert_streaming_matches_full_syntect(text: &str, pretty: bool, chunk: usize) {
        let syntect = crate::syntax::test_syntect();
        let (full_output, _) =
            render_markdown_ratatui_full(text, test_style::STYLE, pretty, Some(syntect));

        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, pretty);
        let mut pos = 0;
        while pos < text.len() {
            let desired = pos + chunk;
            let end = text[pos..]
                .char_indices()
                .map(|(i, _)| pos + i)
                .find(|&i| i >= desired)
                .unwrap_or(text.len());
            renderer.push_and_render(&text[pos..end], Some(syntect));
            pos = end;
        }
        let streaming_output = renderer.view();

        assert_eq!(
            streaming_output.lines,
            full_output.lines.as_slice(),
            "[chunk={chunk}] syntect streaming lines mismatch",
        );
        assert_eq!(
            streaming_output.line_source_map, full_output.line_source_map,
            "[chunk={chunk}] syntect streaming line_source_map mismatch",
        );
    }

    #[test]
    fn test_open_yaml_block_streaming_matches_full_char_by_char() {
        // An UNCLOSED ```yaml block: the streaming renderer keeps it in the
        // tail and highlights it incrementally; full render highlights it from
        // scratch. They must be byte-identical.
        let text = format!("```yaml\n{}", yaml_body(120));
        assert_streaming_matches_full_syntect(&text, true, 1);
    }

    #[test]
    fn test_open_yaml_block_streaming_matches_full_chunks() {
        let text = format!("```yaml\n{}", yaml_body(120));
        for chunk in [3, 7, 17, 64] {
            assert_streaming_matches_full_syntect(&text, true, chunk);
        }
    }

    /// Like [`assert_streaming_matches_full_syntect`] but compares only
    /// `lines` (the highlighted content). Used where the stream crosses a
    /// checkpoint/freeze boundary: `line_source_map` is tail-relative across
    /// freezes (pre-existing streaming behavior, unrelated to highlighting),
    /// so only the rendered content is asserted equal.
    #[track_caller]
    fn assert_streaming_lines_match_full_syntect(text: &str, pretty: bool, chunk: usize) {
        let syntect = crate::syntax::test_syntect();
        let (full_output, _) =
            render_markdown_ratatui_full(text, test_style::STYLE, pretty, Some(syntect));

        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, pretty);
        let mut pos = 0;
        while pos < text.len() {
            let desired = pos + chunk;
            let end = text[pos..]
                .char_indices()
                .map(|(i, _)| pos + i)
                .find(|&i| i >= desired)
                .unwrap_or(text.len());
            renderer.push_and_render(&text[pos..end], Some(syntect));
            pos = end;
        }
        assert_eq!(
            renderer.view().lines,
            full_output.lines.as_slice(),
            "[chunk={chunk}] syntect streaming lines mismatch",
        );
    }

    #[test]
    fn test_closed_yaml_block_streaming_matches_full() {
        // A CLOSED block plus following prose: the closed block never uses the
        // incremental cache (the trailing-open branch requires the body to
        // reach EOF), so its highlighted output must equal the batch path.
        let text = format!("```yaml\n{}```\n\nDone.\n\n", yaml_body(80));
        assert_streaming_lines_match_full_syntect(&text, true, 1);
        assert_streaming_lines_match_full_syntect(&text, true, 11);
    }

    #[test]
    fn test_second_open_block_after_closed_resets_cache() {
        // A closed rust block, then a still-open yaml block. The cache must
        // re-key on the new fence/offset and produce output identical to full.
        let text = format!("```rust\nfn main() {{}}\n```\n\n```yaml\n{}", yaml_body(60));
        assert_streaming_lines_match_full_syntect(&text, true, 1);
        assert_streaming_lines_match_full_syntect(&text, true, 9);
    }

    #[test]
    fn test_closed_fences_inside_open_list_match_full() {
        // Shape: closed fences in a list that keeps streaming. The
        // open list blocks checkpointing, so every push re-parses the fences
        // via `highlight_closed`; output must match a one-shot full render.
        let mut text = String::new();
        for i in 0..2 {
            text.push_str(&format!(
                "- **item {i}**\n  ```rust\n  fn f{i}(x: u64) -> u64 {{\n      x + {i}\n  }}\n  ```\n",
            ));
        }
        for w in 0..30 {
            if w % 6 == 0 {
                text.push_str("\n- more: ");
            }
            text.push_str(&format!("word{w} "));
        }
        text.push('\n');
        for chunk in [1, 7, 23] {
            assert_streaming_lines_match_full_syntect(&text, true, chunk);
        }
    }

    #[test]
    fn test_closed_fence_in_list_then_open_fence_match_full() {
        // Memo path (closed fence in open list) and incremental path
        // (trailing open fence) active simultaneously must not disturb
        // each other.
        let text = format!(
            "- **pinned**\n  ```rust\n  fn pinned() -> u64 {{ 7 }}\n  ```\n- streaming on\n\n```yaml\n{}",
            yaml_body(40),
        );
        for chunk in [1, 9] {
            assert_streaming_lines_match_full_syntect(&text, true, chunk);
        }
    }

    #[test]
    fn test_open_block_utf8_split_across_chunks() {
        // Multibyte chars in the still-streaming last line, split across chunk
        // boundaries, must not panic and must match full render.
        let text = "```yaml\nname: \"café — naïve 日本語 🎉 résumé\"\nother: 1\n".to_string();
        for chunk in [1, 2, 3, 5] {
            assert_streaming_matches_full_syntect(&text, true, chunk);
        }
    }

    #[test]
    fn test_open_block_crlf_line_endings_match_full() {
        // CRLF line endings inside the open block: `LinesWithEndings` keeps the
        // `\r\n` on the committed line, so incremental == batch. (Single open
        // block, no freeze, so line_source_map is asserted too.)
        let mut text = String::from("```yaml\r\n");
        for line in yaml_body(40).lines() {
            text.push_str(line);
            text.push_str("\r\n");
        }
        for chunk in [1, 3, 7, 17] {
            assert_streaming_matches_full_syntect(&text, true, chunk);
        }
    }

    #[test]
    fn test_theme_change_mid_stream_clears_cache() {
        let syntect = crate::syntax::test_syntect();
        // A second, distinct style (changes code_untagged so a difference would
        // be observable if the cache were not cleared).
        let mut style2 = test_style::STYLE;
        style2.code_untagged = anstyle::Style::new().bold();

        let text = format!("```yaml\n{}", yaml_body(40));

        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        // Stream the first half with the original style.
        let mid = text.len() / 2;
        let mid = text
            .char_indices()
            .map(|(i, _)| i)
            .find(|&i| i >= mid)
            .unwrap_or(text.len());
        renderer.push_and_render(&text[..mid], Some(syntect));
        assert!(
            renderer.open_code.is_some(),
            "cache should exist after rendering an open block with syntect",
        );

        // Theme change must drop the cache.
        renderer.set_style(style2);
        assert!(
            renderer.open_code.is_none(),
            "set_style must clear the open-code cache",
        );

        // Finish streaming under the new style and compare to a full render
        // under that same style.
        renderer.push_and_render(&text[mid..], Some(syntect));
        let (full_output, _) = render_markdown_ratatui_full(&text, style2, true, Some(syntect));
        assert_eq!(
            renderer.view().lines,
            full_output.lines.as_slice(),
            "post-theme-change streaming must match full render with new style",
        );
    }

    /// Document exercising every math delimiter form, used to verify the
    /// streaming renderer converges to the full render no matter where
    /// chunk boundaries fall.
    const MATH_DOC: &str = concat!(
        "# Math test\n\n",
        "Euler: $e^{i\\pi} + 1 = 0$ inline.\n\n",
        "Display:\n\n",
        "$$\n\\int_0^\\infty e^{-x} dx = 1\n$$\n\n",
        "Paren \\(\\alpha + \\beta\\) inline.\n\n",
        "Padded \\( u + v \\) inline.\n\n",
        "\\[\n\\frac{a+b}{2} \\ge \\sqrt{ab}\n\\]\n\n",
        "| Col | Math |\n|-----|------|\n| a | $x^2$ |\n\n",
        "- item \\(p \\to q\\)\n",
        "- plain\n\n",
        "> quote $$E = mc^2$$\n\n",
        "## Heading \\[h = x^3\\]\n\n",
        "Aligned:\n\n",
        "\\[\n\\begin{aligned}\nf(x) &= x^2 \\\\\ng(x) &= 2x\n\\end{aligned}\n\\]\n\n",
        "The end.\n",
    );

    /// Math content must render identically whether it arrives whole or
    /// split at ANY byte boundary (checkpoint/tail re-render interplay).
    #[test]
    fn test_math_doc_2way_splits_match_full() {
        let text = MATH_DOC;

        let (full_output, _) = render_markdown_ratatui_full(text, test_style::STYLE, true, None);
        let full_lines: Vec<String> = full_output
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        // Sanity: the full render actually produced converted math.
        let joined = full_lines.join("\n");
        assert!(joined.contains("e^(iπ) + 1 = 0"), "inline $ math: {joined}");
        assert!(
            joined.contains("∫₀^∞ e⁻ˣ dx = 1"),
            "display $$ math: {joined}"
        );
        assert!(joined.contains("α + β"), "paren inline math: {joined}");
        assert!(
            joined.contains("u + v"),
            "padded paren inline math: {joined}"
        );
        assert!(
            joined.contains("(a+b)/2 ≥ √(ab)"),
            "bracket display math: {joined}"
        );
        assert!(joined.contains("x²"), "table cell math: {joined}");
        assert!(joined.contains("p → q"), "list item math: {joined}");
        assert!(joined.contains("E = mc²"), "blockquote math: {joined}");
        assert!(joined.contains("h = x³"), "heading bracket math: {joined}");
        assert!(joined.contains("f(x) = x²"), "aligned env: {joined}");

        let mut failures = Vec::new();
        for split_at in 1..text.len() {
            if !text.is_char_boundary(split_at) {
                continue;
            }

            let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
            renderer.push_and_render(&text[..split_at], None);
            renderer.push_and_render(&text[split_at..], None);
            let streaming_output = renderer.view();

            let streaming_lines: Vec<String> = streaming_output
                .lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();

            if streaming_lines != full_lines {
                let diff_line = streaming_lines
                    .iter()
                    .zip(full_lines.iter())
                    .enumerate()
                    .find(|(_, (s, f))| s != f)
                    .map(|(i, _)| i);
                failures.push(format!(
                    "byte {}: stream={} lines, full={} lines, first_diff={:?}",
                    split_at,
                    streaming_lines.len(),
                    full_lines.len(),
                    diff_line,
                ));
            }
        }

        assert!(
            failures.is_empty(),
            "{} failures out of {} split points:\n{}",
            failures.len(),
            text.len() - 1,
            failures.join("\n")
        );
    }

    fn view_text_lines(r: &StreamingMarkdownRenderer) -> Vec<String> {
        r.view()
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// `\(…\)` / `\[…\]` inside a table cell must converge under
    /// streaming and convert to Unicode (the normalizer rewrites them to
    /// `$`/`$$` before parsing, so the in-cell math path handles them).
    #[test]
    fn streaming_table_with_backslash_math_matches_full() {
        let doc =
            "| Mode | Metric |\n|------|--------|\n| Open | \\(\\alpha\\) then \\[x^2\\] |\n\n";
        assert_streaming_matches_full_both(doc);
        let joined = view_text_lines(&{
            let mut r = StreamingMarkdownRenderer::new(test_style::STYLE, true);
            r.push_and_render(doc, None);
            r.finish(None);
            r
        })
        .join("\n");
        assert!(joined.contains('α'), "cell math converted: {joined:?}");
        assert!(
            joined.contains("x²"),
            "cell display math converted: {joined:?}"
        );
        assert!(!joined.contains("\\("), "no raw TeX: {joined:?}");
    }

    /// `clone()` must reproduce the rendered output exactly even when the source
    /// contained backslash math (which is normalized into `source`).
    #[test]
    fn clone_reproduces_backslash_math_output() {
        let mut r = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        r.push_and_render("Sum \\(\\alpha + \\beta\\) and \\[x^2\\].\n\n", None);
        let cloned = r.clone();
        assert_eq!(view_text_lines(&r), view_text_lines(&cloned));
    }

    /// `clone()` must preserve the normalizer's held-back pending state: stream
    /// up to a chunk boundary that holds back a trailing `\`, clone, then feed
    /// the completion to both — they must stay identical and convert correctly.
    #[test]
    fn clone_preserves_held_back_pending() {
        let mut r = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        r.push_and_render("ab \\(\\alpha\\) cd \\", None); // trailing `\` held back
        let mut cloned = r.clone();
        r.push_and_render("(\\beta\\) ef\n\n", None);
        cloned.push_and_render("(\\beta\\) ef\n\n", None);
        r.finish(None);
        cloned.finish(None);
        assert_eq!(view_text_lines(&r), view_text_lines(&cloned));
        let joined = view_text_lines(&r).join("\n");
        assert!(
            joined.contains('α') && joined.contains('β'),
            "both math spans converted: {joined:?}"
        );
        assert!(!joined.contains('\\'), "no raw backslashes: {joined:?}");
    }

    /// finish() (full re-render) must also match the incremental view for
    /// math-heavy content streamed in small chunks.
    #[test]
    fn test_math_doc_small_chunks_finish_matches_full() {
        let text = MATH_DOC;
        let (full_output, _) = render_markdown_ratatui_full(text, test_style::STYLE, true, None);

        let mut renderer = StreamingMarkdownRenderer::new(test_style::STYLE, true);
        for chunk in split_into_chunks(text, 3) {
            renderer.push_and_render(chunk, None);
        }
        renderer.finish(None);
        let view = renderer.view();

        let view_lines: Vec<String> = view
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let full_lines: Vec<String> = full_output
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(view_lines, full_lines);
    }
}
