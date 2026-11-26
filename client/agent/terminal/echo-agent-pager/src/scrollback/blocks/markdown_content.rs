//! Shared markdown content with cached word-wrapping.
//!
//! [`MarkdownContent`] wraps a [`StreamingMarkdownRenderer`] and caches the
//! word-wrapped output so that repeated calls to [`output()`](MarkdownContent::output)
//! at the same width are free after the first wrap.  Used by both
//! [`AgentMessageBlock`](super::AgentMessageBlock) and
//! [`ThinkingBlock`](super::ThinkingBlock).

use std::cell::RefCell;

use ratatui::text::Line;

use crate::render::wrapping::word_wrap_lines_with_joiners;
use crate::scrollback::types::{BlockLine, BlockOutput};

use super::quote_bar::QuoteBarStrip;

pub(crate) const MARKDOWN_BODY_RANGE: u16 = 0;
use crate::syntax::get_syntect;
use crate::theme::{ThemeKind, cache as theme_cache, md_style};
use echo_agent_markdown::StreamingMarkdownRenderer;

/// Mutable rendering state behind a single `RefCell`.
///
/// Groups the renderer and wrap-cache together so `ensure_wrapped` (called
/// from `&self` methods via the `BlockContent` trait) can update both the
/// table-width setting and the cache in a single borrow.
#[derive(Debug, Clone)]
struct RenderState {
    renderer: StreamingMarkdownRenderer,
    /// Cached word-wrap result keyed on `(width, generation, theme)`.
    cache_width: usize,
    cache_generation: u64,
    cache_theme: ThemeKind,
    cache_lines: Vec<Line<'static>>,
    cache_joiners: Vec<Option<String>>,
    /// Number of pre-wrap (renderer output) lines that were frozen at the time
    /// we last wrapped. Lines `0..frozen_pre_wrap_count` are stable and their
    /// wrapped output is cached in `cache_lines[0..frozen_wrapped_count]`.
    frozen_pre_wrap_count: usize,
    /// Number of post-wrap lines produced by the frozen prefix.
    frozen_wrapped_count: usize,
}

/// Shared markdown content with generation-tracked word-wrap cache.
///
/// Owns a [`StreamingMarkdownRenderer`] and provides:
/// - Mutation via `push_chunk`, `finish`, `set_raw_mode`
/// - Cached word-wrapping via `wrapped_lines` and `output`
///
/// Every mutation bumps an internal generation counter.  The wrap cache is
/// keyed on `(width, generation)`, so scrolling (which doesn't change content)
/// returns the cached result instantly.
#[derive(Debug, Clone)]
pub struct MarkdownContent {
    state: RefCell<RenderState>,
    current_raw: bool,
    generation: u64,
}

/// Borrowed view of cached wrapped lines + joiners.
///
/// Returned by [`MarkdownContent::wrapped_lines`] for blocks that need to
/// post-process the wrapped output (e.g., blending, truncation).
pub struct WrappedLines<'a> {
    pub lines: &'a [Line<'static>],
    pub joiners: &'a [Option<String>],
}

impl MarkdownContent {
    /// Create with initial text (rendered immediately).
    pub fn new(text: impl Into<String>) -> Self {
        Self::new_with_table_width(text, None)
    }

    /// Create with initial text and an optional table width constraint.
    ///
    /// When `max_table_width` is `Some(w)`, tables are constrained to fit
    /// within `w` display columns.  This is useful for pre-rendering
    /// markdown before the final display width is known (e.g., plan preview).
    pub fn new_with_table_width(text: impl Into<String>, max_table_width: Option<usize>) -> Self {
        Self::new_inner(text, max_table_width, true)
    }

    /// Create source-faithful content: CommonMark soft breaks are preserved
    /// as line breaks instead of collapsing to spaces, so each source line
    /// maps 1:1 to a rendered line.
    ///
    /// Used by the line-numbered plan preview, where rendered lines must map
    /// back to file lines (e.g. for commenting on a line range).
    pub fn new_source_faithful(text: impl Into<String>, max_table_width: Option<usize>) -> Self {
        Self::new_inner(text, max_table_width, false)
    }

    fn new_inner(
        text: impl Into<String>,
        max_table_width: Option<usize>,
        collapse_soft_breaks: bool,
    ) -> Self {
        let mut renderer = StreamingMarkdownRenderer::new(md_style::style(), true);
        renderer.set_max_table_width(max_table_width);
        renderer.set_collapse_soft_breaks(collapse_soft_breaks);
        let text = text.into();
        let expanded = echo_agent_pager_render::appearance::expand_tabs(&text);
        renderer.push(&expanded);
        // finish() (not render()) so the streaming LaTeX-delimiter normalizer
        // flushes any trailing held-back delimiter bytes for this complete,
        // one-shot document.
        renderer.finish(Some(get_syntect()));
        Self {
            state: RefCell::new(RenderState {
                renderer,
                cache_width: 0,
                cache_generation: 0,
                cache_theme: theme_cache::current_kind(),
                cache_lines: Vec::new(),
                cache_joiners: Vec::new(),
                frozen_pre_wrap_count: 0,
                frozen_wrapped_count: 0,
            }),
            current_raw: false,
            generation: 1,
        }
    }

    /// Create empty for streaming.
    pub fn streaming() -> Self {
        Self {
            state: RefCell::new(RenderState {
                renderer: StreamingMarkdownRenderer::new(md_style::style(), true),
                cache_width: 0,
                cache_generation: 0,
                cache_theme: theme_cache::current_kind(),
                cache_lines: Vec::new(),
                cache_joiners: Vec::new(),
                frozen_pre_wrap_count: 0,
                frozen_wrapped_count: 0,
            }),
            current_raw: false,
            generation: 0,
        }
    }

    /// Append a streaming chunk and re-render.
    pub fn push_chunk(&mut self, chunk: &str) {
        let expanded = echo_agent_pager_render::appearance::expand_tabs(chunk);
        self.state
            .get_mut()
            .renderer
            .push_and_render(&expanded, Some(get_syntect()));
        self.generation += 1;
    }

    /// Append a chunk without rendering immediately.
    ///
    /// Used for historical replay during `session/load` so the pager can batch
    /// markdown work and render once after replay completes.
    pub fn push_chunk_deferred(&mut self, chunk: &str) {
        let expanded = echo_agent_pager_render::appearance::expand_tabs(chunk);
        self.state.get_mut().renderer.push(&expanded);
        self.generation += 1;
    }

    /// Finish streaming — full re-render for correctness.
    pub fn finish(&mut self) {
        let state = self.state.get_mut();
        state.renderer.finish(Some(get_syntect()));
        // finish() does a full re-render; reset frozen tracking so the
        // next ensure_wrapped re-wraps everything from the new output.
        state.frozen_pre_wrap_count = 0;
        state.frozen_wrapped_count = 0;
        self.generation += 1;
    }

    /// Get the source markdown text.
    pub fn text(&self) -> String {
        self.state.borrow().renderer.source().to_string()
    }

    /// Whether the source markdown is empty (zero-alloc, unlike `text()`).
    pub fn is_empty(&self) -> bool {
        self.state.borrow().renderer.source().is_empty()
    }

    /// Get the rendered text as plain text (styles stripped).
    ///
    /// Returns the styled markdown output with all ratatui styles removed,
    /// producing a plain-text representation of the rendered content.
    /// Useful for copy-to-clipboard in pretty mode.
    pub fn rendered_plain_text(&self) -> String {
        let state = self.state.borrow();
        let view = state.renderer.view();
        let mut result = String::new();
        for (i, line) in view.lines.iter().enumerate() {
            if i > 0 {
                result.push('\n');
            }
            for span in &line.spans {
                result.push_str(&span.content);
            }
        }
        result
    }

    /// Get the line source map (rendered line index → source line number).
    ///
    /// Each entry maps a pre-wrap rendered line to the source line it came from.
    /// Used for cursor stability when toggling raw/pretty mode.
    pub fn line_source_map(&self) -> Vec<usize> {
        self.state.borrow().renderer.view().line_source_map.to_vec()
    }

    /// Get the pre-wrap rendered lines (before word wrapping).
    ///
    /// Returns cloned lines from the markdown renderer's current output.
    /// These are styled `Line<'static>` objects at their natural width,
    /// suitable for feeding into a ListPane which handles its own wrapping.
    pub fn pre_wrap_lines(&self) -> Vec<Line<'static>> {
        self.state.borrow().renderer.view().lines.to_vec()
    }

    /// Access the pre-wrap hyperlink targets via a closure, avoiding allocation.
    pub fn with_hyperlinks<R>(
        &self,
        f: impl FnOnce(&[echo_agent_markdown::HyperlinkTarget]) -> R,
    ) -> R {
        let state = self.state.borrow();
        f(state.renderer.view().hyperlinks)
    }

    /// Pre-wrap line ranges of the ` ```mermaid ` blocks in the current
    /// rendered output, reflecting the current render width.
    ///
    /// Allocation-light (no source rebuild) for the per-frame caption path; the
    /// detection skeleton with the diagram source lives in
    /// [`mermaid_content`](Self::mermaid_content).
    pub fn mermaid_block_ranges(&self) -> Vec<std::ops::Range<usize>> {
        let state = self.state.borrow();
        super::mermaid_content::mermaid_block_ranges(&state.renderer.view())
    }

    /// Build the Mermaid detection skeleton from the current rendered output.
    ///
    /// Call at construction/finish (never per streaming chunk) to capture the
    /// detected diagrams (detection only — rendering is lazy, driven by the
    /// affordance row on click).
    pub fn mermaid_content(&self) -> super::mermaid_content::MermaidContent {
        let state = self.state.borrow();
        super::mermaid_content::MermaidContent::from_view(&state.renderer.view())
    }

    /// Get the current generation counter.
    ///
    /// Bumped on every content mutation (push_chunk, finish, set_raw_mode).
    /// Used by viewers to detect when items need rebuilding.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the content is currently in raw mode.
    pub fn is_raw(&self) -> bool {
        self.current_raw
    }

    /// Drop the word-wrap cache (`cache_lines` / `cache_joiners`).
    ///
    /// The next `output()` / `wrapped_lines()` call transparently rebuilds it
    /// from the renderer's pre-wrap output — the exact path a width or theme
    /// change already takes. Used by off-screen cache eviction: for a long
    /// session the post-wrap copy of every styled line is one of the largest
    /// per-block allocations, and only entries near the viewport need it hot.
    pub fn evict_wrap_cache(&self) {
        let mut state = self.state.borrow_mut();
        if state.cache_lines.is_empty() && state.cache_joiners.is_empty() {
            return;
        }
        state.cache_lines = Vec::new();
        state.cache_joiners = Vec::new();
        state.cache_generation = u64::MAX; // force rebuild on next use
        state.frozen_pre_wrap_count = 0;
        state.frozen_wrapped_count = 0;
    }

    /// Toggle raw mode, re-rendering if changed.
    pub fn set_raw_mode(&mut self, raw: bool) {
        if self.current_raw != raw {
            self.current_raw = raw;
            let state = self.state.get_mut();
            state.renderer.set_pretty(!raw);
            state.renderer.render(Some(get_syntect()));
            // set_pretty resets renderer frozen state
            state.frozen_pre_wrap_count = 0;
            state.frozen_wrapped_count = 0;
            self.generation += 1;
        }
    }

    /// Ensure the wrap cache is populated for the given width.
    ///
    /// Uses incremental wrapping: only re-wraps lines after the renderer's
    /// frozen boundary. Frozen (stable) lines are wrapped once and cached.
    /// This turns streaming from O(N^2) total wrapping to ~O(N).
    fn ensure_wrapped(&self, width: usize) {
        let mut state = self.state.borrow_mut();
        let current_theme = theme_cache::current_kind();

        // If the theme changed, update the renderer's style so the re-render
        // below picks up the new colors. Resetting cache_generation forces
        // the cache to rebuild even if width and content haven't changed.
        if state.cache_theme != current_theme {
            state.renderer.set_style(md_style::style());
            state.cache_theme = current_theme;
            state.cache_generation = u64::MAX; // force cache miss
            // set_style resets renderer frozen state, so our tracking is stale
            state.frozen_pre_wrap_count = 0;
            state.frozen_wrapped_count = 0;
        }

        if state.cache_width == width && state.cache_generation == self.generation {
            return;
        }

        // Width or theme changed → full re-wrap (frozen cache invalid)
        let width_changed = state.cache_width != width;
        if width_changed {
            state.frozen_pre_wrap_count = 0;
            state.frozen_wrapped_count = 0;
        }

        // Update table width and re-render (only re-renders tail internally).
        state.renderer.set_max_table_width(Some(width));
        state.renderer.render(Some(get_syntect()));

        let frozen_count = state.renderer.frozen_lines_count();

        // --- Incremental wrapping ---
        //
        // The renderer guarantees that view().lines[0..frozen_count] are stable.
        // We only need to wrap:
        //   1. Newly frozen lines (frozen_pre_wrap_count..frozen_count)
        //   2. Tail lines (frozen_count..total_lines)
        //
        // The cached frozen wrapped output (cache_lines[0..frozen_wrapped_count])
        // is preserved as-is.
        //
        // We clone the line slices we need *before* mutating cache_lines,
        // because view() borrows the renderer immutably.

        // Step 1: Wrap any newly frozen lines
        let new_frozen_wrapped = if frozen_count > state.frozen_pre_wrap_count {
            let new_frozen: Vec<Line<'static>> =
                state.renderer.view().lines[state.frozen_pre_wrap_count..frozen_count].to_vec();
            Some(word_wrap_lines_with_joiners(new_frozen, width))
        } else {
            None
        };

        // Step 2: Wrap the tail (unfrozen) lines
        let total_lines = state.renderer.view().lines.len();
        let tail_wrapped = if frozen_count < total_lines {
            let tail: Vec<Line<'static>> = state.renderer.view().lines[frozen_count..].to_vec();
            Some(word_wrap_lines_with_joiners(tail, width))
        } else {
            None
        };

        // Now mutate the cache (no more borrows of view/renderer)
        // Truncate stale tail, keeping only the previously frozen wrapped output
        let frozen_wc = state.frozen_wrapped_count;
        state.cache_lines.truncate(frozen_wc);
        state.cache_joiners.truncate(frozen_wc);

        // Append newly frozen wrapped lines
        if let Some((new_lines, new_joiners)) = new_frozen_wrapped {
            state.cache_lines.extend(new_lines);
            state.cache_joiners.extend(new_joiners);
            state.frozen_pre_wrap_count = frozen_count;
            state.frozen_wrapped_count = state.cache_lines.len();
        }

        // Append tail wrapped lines
        if let Some((tail_lines, tail_joiners)) = tail_wrapped {
            state.cache_lines.extend(tail_lines);
            state.cache_joiners.extend(tail_joiners);
        }

        state.cache_width = width;
        state.cache_generation = self.generation;
    }

    /// Access cached wrapped lines + joiners for post-processing.
    ///
    /// The closure receives a [`WrappedLines`] reference valid for the
    /// duration of the call.  This avoids cloning when the caller only
    /// needs to inspect or slice the lines (e.g., ThinkingBlock truncation).
    pub fn with_wrapped_lines<R>(&self, width: usize, f: impl FnOnce(WrappedLines<'_>) -> R) -> R {
        self.ensure_wrapped(width);
        let state = self.state.borrow();
        f(WrappedLines {
            lines: &state.cache_lines,
            joiners: &state.cache_joiners,
        })
    }

    /// Build a [`BlockOutput`] from the cached wrapped lines.
    ///
    /// Each line is converted to a [`BlockLine`] with joiner and optional
    /// background color (from the line's style, e.g., for code blocks).
    /// This is the common path used by [`AgentMessageBlock`](super::AgentMessageBlock).
    pub fn output(&self, width: usize) -> BlockOutput {
        // Raw mode shows the source `>` markers verbatim — nothing to exclude.
        let strip = QuoteBarStrip::new(!self.current_raw);
        self.with_wrapped_lines(width, |wrapped| {
            if wrapped.lines.is_empty() {
                BlockOutput {
                    lines: vec![Line::from("").into()],
                }
            } else {
                BlockOutput {
                    lines: wrapped
                        .lines
                        .iter()
                        .zip(wrapped.joiners.iter())
                        .map(|(line, joiner)| {
                            let mut content = line.clone();
                            let selectable = strip.selectable(&mut content);
                            let mut block_line = BlockLine::styled(content)
                                .with_selection_range(Some(MARKDOWN_BODY_RANGE))
                                .with_joiner(joiner.clone());
                            block_line.selectable = selectable;
                            if let Some(bg) = line.style.bg {
                                block_line.with_background(bg)
                            } else {
                                block_line
                            }
                        })
                        .collect(),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::types::Selectable;

    #[test]
    fn cache_hit_on_same_width() {
        let md = MarkdownContent::new("Hello world, this is a test line");
        let out1 = md.output(80);
        let out2 = md.output(80);
        // Same content and width → should return identical output.
        assert_eq!(out1.lines.len(), out2.lines.len());
        // Verify cache was actually used (generation matches).
        let state = md.state.borrow();
        assert_eq!(state.cache_generation, 1);
        assert_eq!(state.cache_width, 80);
    }

    #[test]
    fn cache_invalidated_on_width_change() {
        let md = MarkdownContent::new("short");
        let out_wide = md.output(80);
        let out_narrow = md.output(5);
        // Different widths may produce different line counts.
        // At minimum, verify we didn't panic and cache updated.
        let state = md.state.borrow();
        assert_eq!(state.cache_width, 5);
        assert!(!out_wide.lines.is_empty());
        assert!(!out_narrow.lines.is_empty());
    }

    #[test]
    fn cache_invalidated_on_push_chunk() {
        let mut md = MarkdownContent::streaming();
        md.push_chunk("Hello");
        let out1 = md.output(80);
        md.push_chunk(" world");
        let out2 = md.output(80);
        // Content changed → output should differ.
        let text1: String = out1.lines.iter().map(|l| l.content.to_string()).collect();
        let text2: String = out2.lines.iter().map(|l| l.content.to_string()).collect();
        assert_ne!(text1, text2);
    }

    #[test]
    fn with_wrapped_lines_provides_access() {
        // Use CommonMark hard breaks (two trailing spaces + \n) so the
        // three logical lines render as three visual lines. Bare `\n`
        // between text lines is a soft break and collapses to a space.
        let md = MarkdownContent::new("Line one  \nLine two  \nLine three");
        md.with_wrapped_lines(80, |wrapped| {
            assert_eq!(wrapped.lines.len(), 3);
            assert_eq!(wrapped.joiners.len(), 3);
        });
    }

    /// End-to-end regression for the table "ghost cell" bug: a markdown table
    /// with emoji-presentation glyphs (`⚠\u{FE0F}`, `✅`, `✗`) and an em-dash
    /// must render every row at exactly the content width.
    #[test]
    fn table_rows_fill_content_width_with_emoji() {
        use unicode_width::UnicodeWidthStr;

        let md = "| Status | Note |\n|---|---|\n| \u{26A0}\u{FE0F} warn | em \u{2014} dash |\n| \u{2705} ok | \u{2717} no |\n";
        let width = 48;
        let md_content = MarkdownContent::new(md);
        let out = md_content.output(width);

        assert!(out.lines.len() >= 6, "table should produce border + rows");
        for (i, line) in out.lines.iter().enumerate() {
            let text: String = line
                .content
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert_eq!(
                text.width(),
                width,
                "table line {i} must fill the content width, got {:?}",
                text
            );
        }
    }

    /// In-cell reflow regression: a six-column table of unbreakable tokens at
    /// width 30 must keep its right border on every line — a table that
    /// overflows the budget gets hard-clipped by the wrap layer, which eats
    /// the right `│`.
    #[test]
    fn test_six_col_table_output_30_keeps_right_border() {
        use unicode_width::UnicodeWidthStr;

        let md = "| Alpha | Bravo | Ident | DeptName | RoleName | Amount |\n\
                  |---|---|---|---|---|---|\n\
                  | LongalphaToken | TokenTwo | ID-AA1001 | EngineeringOps | ManagerRole | $145,000 |\n";
        let width = 30;
        let out = MarkdownContent::new(md).output(width);

        assert!(out.lines.len() >= 5, "table should produce borders + rows");
        for (i, line) in out.lines.iter().enumerate() {
            let text: String = line
                .content
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert_eq!(
                text.width(),
                width,
                "table line {i} must fill the content width, got {text:?}"
            );
            let right_edge = text.trim_end().chars().last();
            assert!(
                matches!(right_edge, Some('│' | '┐' | '┘' | '┤')),
                "table line {i} lost its right border: {text:?}"
            );
        }
    }

    #[test]
    fn empty_content_returns_placeholder() {
        let md = MarkdownContent::streaming();
        let out = md.output(80);
        assert_eq!(out.lines.len(), 1);
    }

    #[test]
    fn raw_mode_invalidates_cache() {
        let mut md = MarkdownContent::new("**bold** text");
        let _out1 = md.output(80);
        let gen_before = md.generation;
        md.set_raw_mode(true);
        assert_eq!(md.generation, gen_before + 1);
        md.set_raw_mode(true);
        assert_eq!(md.generation, gen_before + 1);
    }

    #[test]
    fn markdown_body_lines_share_one_selection_range() {
        let md = MarkdownContent::new("hello world this should wrap across lines");
        let out = md.output(10);
        assert!(out.lines.len() > 1);
        assert!(
            out.lines
                .iter()
                .all(|line| line.selection_range == Some(MARKDOWN_BODY_RANGE))
        );
    }

    #[test]
    fn markdown_output_keeps_joiners_for_wrapped_lines() {
        let md = MarkdownContent::new("hello world this should wrap across lines");
        let out = md.output(10);
        assert!(out.lines.len() > 1);
        assert_eq!(out.lines[0].joiner, None);
        assert!(out.lines.iter().skip(1).any(|line| line.joiner.is_some()));
    }

    #[test]
    fn markdown_body_lines_remain_selectable() {
        let md = MarkdownContent::new("hello");
        let out = md.output(80);
        assert!(matches!(out.lines[0].selectable, Selectable::All));
    }

    /// Verify that incremental wrapping during streaming produces the same
    /// output as creating a fresh MarkdownContent with the full text.
    #[test]
    fn incremental_wrap_matches_full_wrap() {
        let width = 40;

        // Build content incrementally (simulating streaming)
        let mut streaming = MarkdownContent::streaming();
        let chunks = [
            "Hello world, this is a fairly long line that should definitely wrap.\n\n",
            "Second paragraph with more text to wrap around the edges.\n\n",
            "- bullet one\n",
            "- bullet two with extra words to cause wrapping\n",
            "- bullet three\n",
        ];
        for chunk in &chunks {
            streaming.push_chunk(chunk);
            // Call output() between chunks to exercise incremental path
            let _ = streaming.output(width);
        }
        let incremental_output = streaming.output(width);

        // Build the same content in one shot
        let full_text: String = chunks.iter().copied().collect();
        let full = MarkdownContent::new(&full_text);
        let full_output = full.output(width);

        // Compare line-by-line text content
        let incremental_text: Vec<String> = incremental_output
            .lines
            .iter()
            .map(|l| l.content.to_string())
            .collect();
        let full_text_lines: Vec<String> = full_output
            .lines
            .iter()
            .map(|l| l.content.to_string())
            .collect();

        assert_eq!(
            incremental_text.len(),
            full_text_lines.len(),
            "Line count mismatch: incremental={}, full={}",
            incremental_text.len(),
            full_text_lines.len(),
        );
        for (i, (inc, full)) in incremental_text
            .iter()
            .zip(full_text_lines.iter())
            .enumerate()
        {
            assert_eq!(inc, full, "Line {i} mismatch");
        }
    }

    /// Verify that the frozen wrap cache is actually being used (not just
    /// re-wrapping everything each time).
    #[test]
    fn frozen_cache_is_reused() {
        let width = 40;
        let mut md = MarkdownContent::streaming();

        // Push enough content to establish frozen lines
        md.push_chunk("First paragraph of text.\n\nSecond paragraph.\n\n");
        let _ = md.output(width);

        let state = md.state.borrow();
        let frozen_count_after_first = state.frozen_wrapped_count;
        drop(state);

        // Push more content
        md.push_chunk("Third paragraph.\n\n");
        let _ = md.output(width);

        let state = md.state.borrow();
        // Frozen wrapped count should have grown (more lines became frozen)
        assert!(
            state.frozen_wrapped_count >= frozen_count_after_first,
            "Frozen count should grow monotonically: before={}, after={}",
            frozen_count_after_first,
            state.frozen_wrapped_count,
        );
    }
}
