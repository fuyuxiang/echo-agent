//! Render output types for markdown.

use std::ops::Range;

use ratatui::text::Line;

use crate::buffers::CodeBlockMeta;

/// A hyperlink target extracted from rendered markdown.
///
/// Each instance maps a contiguous cell range on one rendered line to a URL.
/// When a link wraps across lines, multiple `HyperlinkTarget`s share the same
/// `id` and `url` -- the `id` enables OSC 8 hover-grouping across wrapped lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperlinkTarget {
    /// Index of the rendered line this target appears on.
    pub line_index: usize,
    /// Column range (in display cells) of the link text on that line.
    pub column_range: Range<usize>,
    /// The destination URL.
    pub url: String,
    /// Stable identifier for grouping link fragments that belong to the
    /// same logical link (e.g., a link whose text wraps across lines).
    pub id: u32,
}

/// A fenced code block discovered while rendering markdown.
///
/// One `CodeBlockSpan` is produced per **closed** fenced code block, in
/// document order. An unterminated (still-open) fence at the end of the input
/// produces no span: `pulldown-cmark` synthesizes a block end at end-of-input,
/// so closure is detected structurally (a closing fence must follow the body)
/// rather than from the end event alone.
///
/// This is a generic, reusable description of a fenced block — it is not
/// specific to any one info string (e.g. `mermaid`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlockSpan {
    /// The fence info string, e.g. `"mermaid"` or `"rust"`.
    ///
    /// Empty for a fence opened with no info (just ` ``` `). Reported verbatim
    /// as `pulldown-cmark` yields it (the full info string, not just the first
    /// word).
    pub info: String,

    /// The fence body content — the clean, container-stripped code/diagram
    /// source.
    ///
    /// This is `pulldown-cmark`'s merged body text, so container markers are
    /// removed (a blockquote `>` / list indentation does **not** leak in) and
    /// CRLF line endings are normalized to `\n`. It ends with the body's
    /// trailing newline and is empty for an empty-body fence. Prefer this over
    /// slicing [`source_byte_range`](Self::source_byte_range) when you need the
    /// logical body (e.g. a Mermaid diagram nested in a blockquote).
    pub body: String,

    /// Range of **pre-wrap** rendered body lines for this block, as indices
    /// into [`MarkdownRenderOutput::lines`] / [`MarkdownRenderView::lines`].
    ///
    /// Covers only the body — the delimiter ` ``` ` lines are excluded — so it
    /// is independent of whether the renderer hides those delimiters in pretty
    /// mode. Empty (`start == end`) for a fence with an empty body.
    pub output_line_range: Range<usize>,

    /// Byte range of the fence body in the **raw** source text.
    ///
    /// Spans from the first body byte to the last, with the delimiter fence
    /// lines excluded; empty (`start == end`) for an empty body. Unlike
    /// [`body`](Self::body) this is a raw slice, so for a fence nested in a
    /// blockquote or list it covers the source between the delimiters and may
    /// include container markers/indentation (and `\r` for CRLF) on
    /// continuation lines. Use [`body`](Self::body) for the clean content.
    pub source_byte_range: Range<usize>,
}

/// Output from rendering markdown to ratatui Lines.
///
/// Contains all the information needed to display rendered markdown and
/// support copy operations back to source text.
#[derive(Debug, Clone, Default)]
pub struct MarkdownRenderOutput {
    /// Rendered lines ready for display.
    pub lines: Vec<Line<'static>>,

    /// Maps each rendered line index to its source line number.
    /// `line_source_map[rendered_line_idx]` = source line number (0-indexed).
    pub line_source_map: Vec<usize>,

    /// Maps a cell range on a rendered line to a URL. Links that
    /// wrap across lines produce multiple entries with the same `id` and `url`.
    pub hyperlinks: Vec<HyperlinkTarget>,

    /// Fenced code blocks discovered during rendering, in document order.
    /// One entry per closed fenced block; see [`CodeBlockSpan`].
    pub code_blocks: Vec<CodeBlockSpan>,
}

impl MarkdownRenderOutput {
    /// Create a new empty output.
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear all content, keeping allocated capacity.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.line_source_map.clear();
        self.hyperlinks.clear();
        self.code_blocks.clear();
    }

    /// Get a borrowed view of this output.
    pub fn as_view(&self) -> MarkdownRenderView<'_> {
        MarkdownRenderView {
            lines: &self.lines,
            line_source_map: &self.line_source_map,
            hyperlinks: &self.hyperlinks,
            code_blocks: &self.code_blocks,
        }
    }
}

/// Borrowed view of rendered markdown output.
///
/// This is a zero-copy reference to rendered content, used by the streaming
/// renderer to avoid cloning frozen content on every render.
#[derive(Debug, Clone, Copy)]
pub struct MarkdownRenderView<'a> {
    /// Rendered lines ready for display.
    pub lines: &'a [Line<'static>],

    /// Maps each rendered line index to its source line number.
    pub line_source_map: &'a [usize],

    /// Hyperlink targets extracted from the rendered markdown.
    pub hyperlinks: &'a [HyperlinkTarget],

    /// Fenced code blocks discovered during rendering, in document order.
    /// One entry per closed fenced block; see [`CodeBlockSpan`].
    pub code_blocks: &'a [CodeBlockSpan],
}

impl<'a> MarkdownRenderView<'a> {
    /// Get the number of lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

/// Map parse-time code-block metadata onto the rendered output.
///
/// Runs after `render_ratatui` has produced `line_source_map`, turning each
/// captured [`CodeBlockMeta`] into a public [`CodeBlockSpan`]. The
/// pre-wrap body line range is derived from `line_source_map`: a fence body
/// occupies source lines `[src_first, src_last]`, and because the renderer
/// emits exactly one output line per body source line (and never maps a
/// non-body line into that source-line range), the matching output lines form
/// one contiguous run. `line_source_map` is non-decreasing, so the run is
/// located with two `partition_point`s.
///
/// Cost is O(text_len + lines·log) per render: the metas are in ascending body
/// order, so newline counts come from a single monotonic forward cursor over
/// `text` rather than rescanning from byte 0 for every meta (which would be
/// O(metas·text_len) — quadratic in the number of fences on the streaming hot
/// path).
pub(crate) fn build_code_block_spans(
    text: &str,
    line_source_map: &[usize],
    metas: Vec<CodeBlockMeta>,
) -> Vec<CodeBlockSpan> {
    if metas.is_empty() {
        return Vec::new();
    }

    let bytes = text.as_bytes();
    // Monotonic newline cursor. Each query advances from the previous position
    // (metas ascend by body offset), so the whole pass is O(text_len). '\n' is
    // single-byte ASCII, so byte counting is UTF-8-safe at any offset.
    let mut cursor_pos = 0usize;
    let mut cursor_newlines = 0usize;
    let mut newlines_before = |pos: usize| -> usize {
        let pos = pos.min(bytes.len());
        debug_assert!(
            pos >= cursor_pos,
            "metas must be processed in ascending body order",
        );
        while cursor_pos < pos {
            if bytes[cursor_pos] == b'\n' {
                cursor_newlines += 1;
            }
            cursor_pos += 1;
        }
        cursor_newlines
    };

    metas
        .into_iter()
        .map(|meta| {
            let range = meta.body_source_range;
            let src_first = newlines_before(range.start);
            let output_line_range = if range.end <= range.start {
                // Empty body: no rendered body lines. Anchor an empty range at
                // the first output line that does not precede the body.
                let start = line_source_map.partition_point(|&src| src < src_first);
                start..start
            } else {
                // `range.end - 1` is the last body byte; its source line is the
                // inclusive last body source line, robust to a trailing newline.
                let src_last = newlines_before(range.end - 1);
                let start = line_source_map.partition_point(|&src| src < src_first);
                let end = line_source_map.partition_point(|&src| src <= src_last);
                start..end
            };
            CodeBlockSpan {
                info: meta.info,
                body: meta.body,
                output_line_range,
                source_byte_range: range,
            }
        })
        .collect()
}

#[cfg(test)]
mod code_block_span_tests {
    use ratatui::text::Line;

    use crate::style::test_style::STYLE;
    use crate::{CodeBlockSpan, StreamingMarkdownRenderer, render_markdown_ratatui_full};

    fn lines_text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// Plain text of the rendered body lines a span points at.
    fn body_lines(lines: &[Line<'static>], span: &CodeBlockSpan) -> Vec<String> {
        lines_text(&lines[span.output_line_range.clone()])
    }

    /// Source bytes a span's `source_byte_range` selects.
    fn body_source<'a>(src: &'a str, span: &CodeBlockSpan) -> &'a str {
        &src[span.source_byte_range.clone()]
    }

    fn blocks(src: &str, pretty: bool) -> (Vec<Line<'static>>, Vec<CodeBlockSpan>) {
        let (out, _) = render_markdown_ratatui_full(src, STYLE, pretty, None);
        (out.lines, out.code_blocks)
    }

    #[test]
    fn closed_fence_top_level_pretty_and_raw() {
        // A non-rendered language (`text`): its rendered body lines are the
        // verbatim source, so the span's `output_line_range` maps back to them.
        // (A `mermaid` fence renders to diagram art instead — see
        // `mermaid_fence_renders_inline_but_span_keeps_clean_source`.)
        let src = "```text\nflowchart TD\n  A --> B\n```\n";
        for pretty in [true, false] {
            let (lines, cbs) = blocks(src, pretty);
            assert_eq!(cbs.len(), 1, "pretty={pretty}");
            assert_eq!(cbs[0].info, "text");
            // Body line range excludes the delimiter fences in both modes.
            assert_eq!(
                body_lines(&lines, &cbs[0]),
                vec!["flowchart TD", "  A --> B"],
                "pretty={pretty}",
            );
            // Byte range and clean body are mode-independent; for a top-level
            // fence both equal the verbatim fence body.
            assert_eq!(body_source(src, &cbs[0]), "flowchart TD\n  A --> B\n");
            assert_eq!(cbs[0].body, "flowchart TD\n  A --> B\n");
        }
    }

    #[test]
    fn open_fence_produces_no_span() {
        // Unterminated fence at EOF: pulldown still emits a block end, but no
        // span must be produced (malformed/partial input).
        for src in [
            "```mermaid\nflowchart TD\n",
            "```mermaid\nflowchart TD",
            "```mermaid",
            "intro\n\n```rust\nlet x = 1;",
        ] {
            for pretty in [true, false] {
                let (_, cbs) = blocks(src, pretty);
                assert!(
                    cbs.is_empty(),
                    "open fence {src:?} (pretty={pretty}) should yield no span, got {cbs:?}",
                );
            }
        }
    }

    #[test]
    fn multiple_interspersed_blocks_in_order() {
        let src = "Intro\n\n```rust\nfn a() {}\n```\n\nMid prose\n\n```text\nA-->B\n```\n\nEnd\n";
        for pretty in [true, false] {
            let (lines, cbs) = blocks(src, pretty);
            assert_eq!(cbs.len(), 2, "pretty={pretty}");
            assert_eq!(cbs[0].info, "rust");
            assert_eq!(cbs[1].info, "text");
            assert_eq!(body_source(src, &cbs[0]), "fn a() {}\n");
            assert_eq!(body_source(src, &cbs[1]), "A-->B\n");
            assert_eq!(body_lines(&lines, &cbs[0]), vec!["fn a() {}"]);
            assert_eq!(body_lines(&lines, &cbs[1]), vec!["A-->B"]);
            // Document order ⇒ disjoint, increasing line ranges.
            assert!(cbs[0].output_line_range.end <= cbs[1].output_line_range.start);
        }
    }

    #[test]
    fn fence_nested_in_list() {
        // Multi-line body so the list's base indent stripping is exercised on a
        // continuation line ("    A --> B" → "  A --> B").
        let src = "- item\n  ```mermaid\n  flowchart TD\n    A --> B\n  ```\n- next\n";
        for pretty in [true, false] {
            let (_lines, cbs) = blocks(src, pretty);
            assert_eq!(cbs.len(), 1, "pretty={pretty}");
            assert_eq!(cbs[0].info, "mermaid");
            // `body` is the clean, de-prefixed source: the list base indent is
            // stripped but inner relative indentation is preserved.
            assert_eq!(cbs[0].body, "flowchart TD\n  A --> B\n", "pretty={pretty}");
            // The raw byte range, by contrast, also strips the per-line base
            // indent here (pulldown's text-event range starts after it).
            assert_eq!(
                body_source(src, &cbs[0]),
                "flowchart TD\n    A --> B\n",
                "pretty={pretty}",
            );
            assert!(!cbs[0].output_line_range.is_empty());
        }
    }

    #[test]
    fn fence_nested_in_blockquote() {
        // The motivating case for the structural closure rule: the closing
        // fence line is "> ```" (not a bare fence), and the body must come out
        // de-prefixed (no leaked "> " / "│ ").
        let src = "> ```mermaid\n> flowchart TD\n>   A --> B\n> ```\n";
        for pretty in [true, false] {
            let (_lines, cbs) = blocks(src, pretty);
            assert_eq!(cbs.len(), 1, "pretty={pretty}");
            assert_eq!(cbs[0].info, "mermaid");
            assert_eq!(cbs[0].body, "flowchart TD\n  A --> B\n", "pretty={pretty}");
        }
    }

    #[test]
    fn open_blockquote_fence_produces_no_span() {
        // Unterminated fence inside a blockquote ⇒ no span.
        let src = "> ```mermaid\n> flowchart TD\n";
        for pretty in [true, false] {
            let (_, cbs) = blocks(src, pretty);
            assert!(cbs.is_empty(), "pretty={pretty} got {cbs:?}");
        }
    }

    #[test]
    fn indented_code_block_is_not_a_fence() {
        // A 4-space indented code block is not a fenced block ⇒ no span, even
        // when its literal content looks like a fence.
        let src = "para\n\n    ```mermaid\n    A-->B\n    ```\n";
        for pretty in [true, false] {
            let (_, cbs) = blocks(src, pretty);
            assert!(cbs.is_empty(), "pretty={pretty} got {cbs:?}");
        }
    }

    #[test]
    fn empty_body_closed_fence() {
        let src = "```mermaid\n```\n";
        // Pretty: both fence lines are hidden ⇒ no output lines ⇒ the empty
        // anchor lands at 0..0 (exact, not merely is_empty()).
        let (_, cbs) = blocks(src, true);
        assert_eq!(cbs.len(), 1);
        assert_eq!(cbs[0].info, "mermaid");
        assert_eq!(cbs[0].output_line_range, 0..0);
        assert_eq!(body_source(src, &cbs[0]), "");
        assert_eq!(cbs[0].body, "");
        // Raw: both fence lines are shown ⇒ the empty body is anchored between
        // them at 1..1.
        let (_, cbs_raw) = blocks(src, false);
        assert_eq!(cbs_raw.len(), 1);
        assert_eq!(cbs_raw[0].output_line_range, 1..1);
        assert_eq!(cbs_raw[0].body, "");
    }

    #[test]
    fn fence_without_info_string() {
        let src = "```\nplain code\n```\n";
        let (lines, cbs) = blocks(src, true);
        assert_eq!(cbs.len(), 1);
        assert_eq!(cbs[0].info, "");
        assert_eq!(body_lines(&lines, &cbs[0]), vec!["plain code"]);
    }

    #[test]
    fn tilde_fence_and_no_trailing_newline() {
        // Tilde delimiters and a closed fence at EOF without a final newline.
        for src in ["~~~text\nA-->B\n~~~\n", "```text\nfoo\n```"] {
            let (lines, cbs) = blocks(src, true);
            assert_eq!(cbs.len(), 1, "{src:?}");
            assert_eq!(cbs[0].info, "text");
            assert_eq!(body_lines(&lines, &cbs[0]).len(), 1);
        }
    }

    #[test]
    fn blank_line_inside_body_is_counted() {
        let src = "```text\nfoo\n\nbar\n```\n";
        let (lines, cbs) = blocks(src, true);
        assert_eq!(cbs.len(), 1);
        assert_eq!(body_lines(&lines, &cbs[0]), vec!["foo", "", "bar"]);
        assert_eq!(body_source(src, &cbs[0]), "foo\n\nbar\n");
        assert_eq!(cbs[0].body, "foo\n\nbar\n");
    }

    #[test]
    fn crlf_body_is_normalized_but_byte_range_retains_cr() {
        // CRLF: pulldown normalizes the body content to `\n`, while the raw
        // byte range still slices the `\r`. Line counting (over `\n`) is
        // unaffected.
        let src = "```text\r\nA-->B\r\n```\r\n";
        let (lines, cbs) = blocks(src, true);
        assert_eq!(cbs.len(), 1);
        assert_eq!(cbs[0].body, "A-->B\n");
        assert_eq!(body_source(src, &cbs[0]), "A-->B\r\n");
        // One rendered body line (the renderer keeps the raw `\r`; `body` is the
        // normalized source of truth).
        assert_eq!(body_lines(&lines, &cbs[0]).len(), 1);
    }

    #[test]
    fn multibyte_body_slices_safely() {
        // Multi-byte UTF-8 in the body: `source_byte_range` must land on char
        // boundaries (no slice panic) and `body` is the exact content.
        let src = "```text\nA --> \u{65e5}\u{672c}\u{8a9e}\nC --> \u{1f980}\n```\n";
        let (lines, cbs) = blocks(src, true);
        assert_eq!(cbs.len(), 1);
        assert_eq!(
            cbs[0].body,
            "A --> \u{65e5}\u{672c}\u{8a9e}\nC --> \u{1f980}\n"
        );
        assert_eq!(
            body_source(src, &cbs[0]),
            "A --> \u{65e5}\u{672c}\u{8a9e}\nC --> \u{1f980}\n",
        );
        assert_eq!(body_lines(&lines, &cbs[0]).len(), 2);
    }

    #[test]
    fn long_fence_with_inner_backticks() {
        // A 4-backtick fence whose body contains a ``` line must not close
        // early — one span whose body includes the inner fence text.
        let src = "````mermaid\n```\ninner\n```\n````\n";
        let (_lines, cbs) = blocks(src, true);
        assert_eq!(cbs.len(), 1);
        assert_eq!(cbs[0].info, "mermaid");
        assert_eq!(cbs[0].body, "```\ninner\n```\n");
    }

    #[test]
    fn tab_in_body_is_preserved_by_crate() {
        // The markdown crate preserves a literal tab in the body (the pager
        // expands tabs before rendering; the crate itself does not).
        let src = "```mermaid\n\tA --> B\n```\n";
        let (_lines, cbs) = blocks(src, true);
        assert_eq!(cbs.len(), 1);
        assert_eq!(cbs[0].body, "\tA --> B\n");
    }

    #[test]
    fn rendered_body_lines_match_span_body() {
        // The rendered (pre-wrap) body lines reconstruct to the span body
        // (sans the trailing newline a line-join drops). Both pretty and raw.
        let src = "```text\nflowchart TD\n  A --> B\n  B --> C\n```\n";
        for pretty in [true, false] {
            let (lines, cbs) = blocks(src, pretty);
            let joined = body_lines(&lines, &cbs[0]).join("\n");
            assert_eq!(
                joined, "flowchart TD\n  A --> B\n  B --> C",
                "pretty={pretty}"
            );
            assert_eq!(format!("{joined}\n"), cbs[0].body, "pretty={pretty}");
        }
    }

    #[test]
    fn mermaid_fence_renders_inline_but_span_keeps_clean_source() {
        // A closed ```mermaid fence is rendered inline by the markdown crate
        // (its body lines are replaced with diagram art), yet its CodeBlockSpan
        // still exposes the clean SOURCE via `body` — what the pager feeds the
        // PNG engine — and an `output_line_range` that spans the rendered
        // diagram, where the pager anchors its affordance row. This contract is
        // what the pager's Mermaid affordance row relies on.
        let src = "```mermaid\nflowchart TD\n  A --> B\n```\n";
        let (lines, cbs) = blocks(src, true);
        assert_eq!(cbs.len(), 1);
        assert_eq!(cbs[0].info, "mermaid");
        // `body` is the verbatim diagram source, independent of rendering.
        assert_eq!(cbs[0].body, "flowchart TD\n  A --> B\n");
        // The fence is rendered inline: the spanned output lines are the diagram
        // art, not the verbatim source.
        assert!(!cbs[0].output_line_range.is_empty());
        let rendered = body_lines(&lines, &cbs[0]).join("\n");
        assert_ne!(rendered, "flowchart TD\n  A --> B");
    }

    #[test]
    fn streaming_span_stable_once_frozen() {
        let full = "Intro\n\n```mermaid\nA-->B\nC-->D\n```\n\nAfter the block.\n\nMore prose.\n";

        let mut renderer = StreamingMarkdownRenderer::new(STYLE, true);
        let mut frozen_span: Option<CodeBlockSpan> = None;
        for ch in full.chars() {
            renderer.push_and_render(&ch.to_string(), None);
            let view = renderer.view();
            let frozen_lines = renderer.frozen_lines_count();
            if let Some(cb) = view.code_blocks.iter().find(|c| c.info == "mermaid") {
                // Only assert stability once the block is within frozen content.
                if cb.output_line_range.end <= frozen_lines {
                    match &frozen_span {
                        None => frozen_span = Some(cb.clone()),
                        Some(prev) => {
                            assert_eq!(prev, cb, "frozen mermaid span changed across pushes",)
                        }
                    }
                }
            }
        }

        let frozen_span = frozen_span.expect("mermaid block should freeze mid-stream");

        // Streaming output (after finish) must match a one-shot full render.
        let finished = renderer.finish(None);
        let streamed = finished
            .code_blocks
            .iter()
            .find(|c| c.info == "mermaid")
            .expect("finish() keeps the mermaid span");
        assert_eq!(&frozen_span, streamed);

        let (full_out, _) = render_markdown_ratatui_full(full, STYLE, true, None);
        let full_cb = full_out
            .code_blocks
            .iter()
            .find(|c| c.info == "mermaid")
            .expect("full render finds the mermaid span");
        assert_eq!(&frozen_span, full_cb);
    }

    #[test]
    fn streaming_open_fence_has_no_frozen_span() {
        // While the fence is still open, any transient span must remain in the
        // unfrozen tail (never within the frozen prefix).
        let mut renderer = StreamingMarkdownRenderer::new(STYLE, true);
        for chunk in ["intro\n\n", "```mermaid\n", "flowchart TD\n", "A --> B\n"] {
            renderer.push_and_render(chunk, None);
            let frozen_lines = renderer.frozen_lines_count();
            let view = renderer.view();
            for cb in view.code_blocks {
                assert!(
                    cb.output_line_range.end > frozen_lines,
                    "open fence span must not be frozen: {cb:?} frozen_lines={frozen_lines}",
                );
            }
        }
    }

    #[test]
    fn streaming_chunked_matches_full_render() {
        let full = "# Title\n\n```mermaid\nA-->B\n```\n\ntext\n\n```rust\nfn f() {}\n```\n\nbye\n";
        for pretty in [true, false] {
            let mut renderer = StreamingMarkdownRenderer::new(STYLE, pretty);
            // Irregular chunk boundaries to exercise tail rebasing.
            let bytes = full.as_bytes();
            let mut pos = 0;
            for step in [4usize, 9, 1, 13, 7].iter().cycle() {
                if pos >= bytes.len() {
                    break;
                }
                let mut end = (pos + step).min(bytes.len());
                while end < bytes.len() && !full.is_char_boundary(end) {
                    end += 1;
                }
                renderer.push_and_render(&full[pos..end], None);
                pos = end;
            }
            let view = renderer.finish(None);
            let streamed: Vec<_> = view.code_blocks.to_vec();

            let (full_out, _) = render_markdown_ratatui_full(full, STYLE, pretty, None);
            assert_eq!(
                streamed, full_out.code_blocks,
                "pretty={pretty}: chunked stream must match full render",
            );
        }
    }
}
