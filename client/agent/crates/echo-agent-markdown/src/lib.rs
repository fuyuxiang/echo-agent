//! Streaming markdown renderer for terminal UIs.
//!
//! This crate provides incremental/streaming markdown rendering optimized for
//! displaying LLM responses in terminal UIs. Key features:
//!
//! - **Streaming rendering**: Efficiently render markdown as it arrives chunk by chunk
//! - **Checkpoint-based freezing**: Only re-render the "tail" after stable boundaries
//! - **Syntax highlighting**: Code blocks highlighted via syntect
//! - **Terminal color adaptation**: Automatic downgrade for 256-color/16-color terminals
//! - **LaTeX math rendering**: `$...$`, `$$...$$`, `\(...\)` and `\[...\]` math is
//!   converted to a Unicode approximation (`$E=mc^2$` → `E=mc²`) in pretty mode
//!
//! # Example
//!
//! ```ignore
//! use echo_agent_markdown::{StreamingMarkdownRenderer, MarkdownStyle, Syntect};
//!
//! let syntect = Syntect::new(include_bytes!("theme.tmTheme"));
//! let style = MarkdownStyle::default();
//! let mut renderer = StreamingMarkdownRenderer::new(style, true);
//!
//! for token in stream {
//!     renderer.push_and_render(&token, Some(&syntect));
//!     let view = renderer.view();
//!     // display view.lines
//! }
//! ```

mod buffers;
pub mod checkpoint;
mod colors;
mod hyperlinks;
mod latex;
mod latex_delimiters;
mod mermaid;
mod open_code_highlighter;
mod output;
mod parse;
mod render;
mod source_map;
pub mod streaming;
pub mod style;
mod syntax;
mod url_scan;

// Re-export public API
pub use buffers::MarkdownBuffers;
pub use checkpoint::{Checkpoint, CheckpointKind};
pub use colors::{
    ColorLevel, adapt_color, adapt_style, detect_color_level, get_color_level,
    polarity_safe_syntax, polarity_safe_syntax_ansi, set_color_level_cap, set_polarity_safe_syntax,
};
pub use latex_delimiters::{LatexDelimiterNormalizer, normalize_latex_delimiters};
pub use output::{CodeBlockSpan, HyperlinkTarget, MarkdownRenderOutput, MarkdownRenderView};
pub use parse::{MarkdownParser, ParsedMarkdown};
pub use source_map::SourceMap;
pub use streaming::StreamingMarkdownRenderer;
pub use style::{MarkdownStyle, TableBorders};
pub use syntax::Syntect;

// Re-export test helpers when fuzzing
#[cfg(fuzzing)]
pub use syntax::test_syntect;

/// Render markdown to ratatui Lines with full output including checkpoint.
///
/// Runs the parser pass followed by the `url_scan` pass so the returned
/// output's `hyperlinks` mirrors what `StreamingMarkdownRenderer::finish()`
/// produces for the same input (plain-URL detection for the pretty-mode
/// `(url)` suffix and bare URLs in prose).
pub fn render_markdown_ratatui_full(
    text: &str,
    ms: MarkdownStyle,
    pretty: bool,
    syntect: Option<&Syntect>,
) -> (MarkdownRenderOutput, Option<Checkpoint>) {
    let mut buffers = MarkdownBuffers::new();
    render_markdown_ratatui_with_buffers(text, ms, pretty, &mut buffers, syntect)
}

/// Render markdown to ratatui Lines, reusing the provided buffers.
pub fn render_markdown_ratatui_with_buffers(
    text: &str,
    ms: MarkdownStyle,
    pretty: bool,
    buffers: &mut MarkdownBuffers,
    syntect: Option<&Syntect>,
) -> (MarkdownRenderOutput, Option<Checkpoint>) {
    render_markdown_ratatui_with_buffers_width(text, ms, pretty, buffers, syntect, None)
}

/// Render markdown to ratatui Lines, reusing the provided buffers,
/// with an optional maximum table width.
pub fn render_markdown_ratatui_with_buffers_width(
    text: &str,
    ms: MarkdownStyle,
    pretty: bool,
    buffers: &mut MarkdownBuffers,
    syntect: Option<&Syntect>,
    max_table_width: Option<usize>,
) -> (MarkdownRenderOutput, Option<Checkpoint>) {
    // Normalize LaTeX delimiters (`\(…\)`/`\[…\]`/`\begin{equation}`) into the
    // canonical `$`/`$$` forms before parsing, so the math handlers convert them
    // uniformly (incl. inside table cells). All offsets are in normalized space;
    // `StreamingMarkdownRenderer` normalizes at ingestion so its stored source
    // matches. Streaming tail renders (`render_markdown_ratatui_with_link_id`)
    // do NOT re-normalize — they receive already-normalized source.
    let normalized = latex_delimiters::normalize_latex_delimiters(text);
    let mut parsed = MarkdownParser::new(&normalized, ms, buffers, syntect)
        .max_table_width(max_table_width)
        .parse();
    let next_link_id = parsed.next_link_id;
    let (mut output, checkpoint) = parsed.render_ratatui(pretty);
    // Mirror `StreamingMarkdownRenderer::finish()`: detect plain URLs
    // so a one-shot full render produces the same hyperlinks a
    // `push_and_render` + `finish()` sequence would.
    let (extra_links, _post_scan_next_id) =
        url_scan::detect_plain_urls(&output.lines, &output.hyperlinks, next_link_id);
    output.hyperlinks.extend(extra_links);
    output
        .hyperlinks
        .sort_by_key(|h| (h.line_index, h.column_range.start));
    (output, checkpoint)
}

/// Render markdown to ratatui Lines and provide `next_link_id` so the
/// streaming renderer can resume link ID assignment across tail re-renders.
///
/// `open_code` threads an optional incremental highlighter for the trailing
/// still-open fenced code block: only the streaming tail re-render passes
/// `Some(cache)`; `finish()` and non-streaming callers pass `None`. Everything
/// other than that one open block (closed code blocks, HTML, math, tables,
/// inline) always goes through the unchanged batch highlighter, so output is
/// byte-for-byte identical to the cache-less path. See [`open_code_highlighter`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_markdown_ratatui_with_link_id(
    text: &str,
    ms: MarkdownStyle,
    pretty: bool,
    buffers: &mut MarkdownBuffers,
    syntect: Option<&Syntect>,
    max_table_width: Option<usize>,
    link_id_start: u32,
    collapse_soft_breaks: bool,
    open_code: Option<&mut open_code_highlighter::OpenCodeHighlighter>,
) -> (MarkdownRenderOutput, Option<Checkpoint>, u32) {
    let mut parsed = MarkdownParser::new(text, ms, buffers, syntect)
        .max_table_width(max_table_width)
        .link_id_start(link_id_start)
        .collapse_soft_breaks(collapse_soft_breaks)
        .open_code(open_code)
        .parse();

    // NOTE: There can be multiple links in a tail, hence next_link_id is the return.
    let next_link_id = parsed.next_link_id;
    let (output, checkpoint) = parsed.render_ratatui(pretty);
    (output, checkpoint, next_link_id)
}

/// Render markdown to an ANSI-styled string.
pub fn render_markdown(
    text: &str,
    ms: MarkdownStyle,
    pretty: bool,
    syntect: Option<&Syntect>,
) -> (String, SourceMap) {
    let mut buffers = MarkdownBuffers::new();
    let normalized = latex_delimiters::normalize_latex_delimiters(text);
    MarkdownParser::new(&normalized, ms, &mut buffers, syntect)
        .parse()
        .render_ansi(pretty)
}

/// Render markdown to ratatui Lines (simple API).
pub fn render_markdown_ratatui(
    text: &str,
    ms: MarkdownStyle,
    pretty: bool,
    syntect: Option<&Syntect>,
) -> (Vec<ratatui::text::Line<'static>>, Vec<usize>) {
    let (out, _checkpoint) = render_markdown_ratatui_full(text, ms, pretty, syntect);
    (out.lines, out.line_source_map)
}
