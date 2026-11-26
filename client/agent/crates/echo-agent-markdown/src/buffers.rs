//! Reusable buffers and internal data types for markdown parsing and rendering.
//!
//! This module contains all the intermediate data structures used by
//! MarkdownHighlighter during parsing and rendering.

use std::ops::Range;

use anstyle::Style as AnsiStyle;
use ratatui::text::{Line, Span};
use syntect::highlighting::Style as SyntectStyle;

/// A range of text with optional styling.
#[derive(Debug, Clone)]
pub struct Highlight {
    pub style: Option<AnsiStyle>,
    pub range: Range<usize>,
}

/// Syntax-highlighted code block replacement.
///
/// Stores the raw highlighted spans per line (intermediate representation).
/// This allows rendering to either ANSI strings or ratatui Lines on demand.
#[derive(Debug, Clone)]
pub struct Replace {
    /// Raw highlighted spans per line: Vec<(style, text)>.
    /// Each inner Vec represents one line of the code block.
    pub highlighted: Vec<Vec<(SyntectStyle, String)>>,
    /// Source byte range this replaces.
    pub range: Range<usize>,
}

/// Internal representation of a hyperlink target discovered during parsing.
///
/// Populated in the `Tag::Link` / `Tag::Image` arm of `MarkdownParser::on_start`.
/// Consumed during rendering to produce public `HyperlinkTarget`s in the output.
#[derive(Debug, Clone)]
pub struct LinkTarget {
    /// Source byte range of the *link text* (not the full `[text](url)` span).
    pub source_range: Range<usize>,
    /// Destination URL.
    pub url: String,
    /// Monotonically increasing identifier assigned during parsing.
    pub id: u32,
}

/// Parse-time record of a closed fenced code block.
///
/// Populated in the `Tag::CodeBlock` arm of `MarkdownParser`; consumed during
/// rendering (see `output::build_code_block_spans`) to produce the public
/// [`crate::CodeBlockSpan`] once the output line range is known. Only **closed**
/// fences are recorded — an unterminated trailing fence yields no entry.
#[derive(Debug, Clone)]
pub struct CodeBlockMeta {
    /// Fence info string (e.g. `"mermaid"`), verbatim from pulldown-cmark.
    pub info: String,
    /// De-prefixed body content (container markers stripped, CRLF normalized) —
    /// pulldown's merged body text, i.e. the clean code/diagram source.
    pub body: String,
    /// Source byte range of the fence body (delimiter lines excluded).
    pub body_source_range: Range<usize>,
}

/// Text transformation for substituting characters (e.g., bullets).
#[derive(Debug, Clone)]
pub struct Transform {
    /// Source byte range to transform.
    pub(crate) range: Range<usize>,
    /// Replacement text.
    pub(crate) to: String,
    /// Apply this transform even in raw (non-pretty) mode.
    ///
    /// Invariant: `to.len() == range.end - range.start` and the
    /// substitution must stay valid UTF-8 at the same byte offsets.
    /// `render_ansi` substitutes force transforms in place into a byte
    /// buffer; violating the invariant panics at `copy_from_slice` or
    /// `String::from_utf8` before any bytes escape the renderer.
    pub(crate) force: bool,
}

/// A styled segment within a table cell.
#[derive(Debug, Clone)]
pub struct CellSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// Hyperlink (url, id) when this span is inside a `[label](url)` link
    /// or autolink inside a table cell. `None` for plain text.
    pub link: Option<(String, u32)>,
}

impl CellSpan {
    pub fn new(
        text: String,
        bold: bool,
        italic: bool,
        code: bool,
        link: Option<(String, u32)>,
    ) -> Self {
        Self {
            text,
            bold,
            italic,
            code,
            link,
        }
    }
}

/// A table cell with styled content.
#[derive(Debug, Clone, Default)]
pub struct StyledCell {
    pub spans: Vec<CellSpan>,
}

impl StyledCell {
    pub fn new() -> Self {
        Self { spans: Vec::new() }
    }

    /// Get plain text content (for width calculation).
    pub fn plain_text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// Clear the cell content.
    pub fn clear(&mut self) {
        self.spans.clear();
    }
}

/// State for buffering table content during parsing.
#[derive(Debug, Clone)]
pub struct TableState {
    /// Column alignments from the table header.
    pub alignments: Vec<pulldown_cmark::Alignment>,
    /// Header row cells.
    pub header: Vec<StyledCell>,
    /// Body rows (each row is a Vec of styled cells).
    pub rows: Vec<Vec<StyledCell>>,
    /// Current row being built.
    pub current_row: Vec<StyledCell>,
    /// Current cell content being accumulated.
    pub current_cell: StyledCell,
    /// Current style state for the cell.
    pub cell_bold: bool,
    pub cell_italic: bool,
    pub cell_code: bool,
    /// Current link state: `Some((url, id))` while inside a `Tag::Link` /
    /// `Tag::Image` inside a table cell.  Text events captured while this
    /// is set produce link-tagged `CellSpan`s so the table renderer can
    /// apply link styling and emit `HyperlinkTarget`s.
    pub cell_link: Option<(String, u32)>,
    /// Whether we're in the header section.
    pub in_header: bool,
    /// Source byte range of the entire table.
    pub range: Range<usize>,
}

impl TableState {
    pub fn new(alignments: Vec<pulldown_cmark::Alignment>, start: usize) -> Self {
        Self {
            alignments,
            header: Vec::new(),
            rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: StyledCell::new(),
            cell_bold: false,
            cell_italic: false,
            cell_code: false,
            cell_link: None,
            in_header: false,
            range: start..start,
        }
    }

    /// Push text with current styling to the cell.
    pub fn push_text(&mut self, text: &str) {
        self.current_cell.spans.push(CellSpan::new(
            text.to_string(),
            self.cell_bold,
            self.cell_italic,
            self.cell_code,
            self.cell_link.clone(),
        ));
    }
}

/// One hyperlink target inside a formatted table.
///
/// Coordinates are local to the table's `styled_lines`:
/// `line_offset` indexes into `TableReplace::styled_lines`; the renderer
/// adds the current absolute line count to produce a public
/// `HyperlinkTarget`.
#[derive(Debug, Clone)]
pub struct TableHyperlink {
    /// Index within `TableReplace::styled_lines`.
    pub line_offset: usize,
    /// Column range (display cells) on that line.
    pub column_range: Range<usize>,
    /// Destination URL.
    pub url: String,
    /// Stable identifier shared with the paragraph link path.
    pub id: u32,
}

/// Formatted table replacement for pretty mode rendering.
#[derive(Debug, Clone)]
pub struct TableReplace {
    /// Formatted table lines (plain strings for ANSI rendering).
    pub lines: Vec<String>,
    /// Styled table lines for ratatui rendering.
    pub styled_lines: Vec<Line<'static>>,
    /// Source byte range this replaces.
    pub range: Range<usize>,
    /// Per-rendered-line source offset from the table start.
    ///
    /// Maps each entry in `styled_lines` to the source line offset
    /// within the table (0 = header, 1 = separator, 2+ = body rows).
    /// Used by the renderer to produce correct `line_source_map` entries
    /// instead of the naive `table_start + line_idx` which overshoots
    /// when the rendered table has more lines than the source (borders,
    /// separators, wrapped cells).
    pub line_source_offsets: Vec<usize>,
    /// Hyperlinks for `[label](url)` / autolinks inside table cells.
    ///
    /// The paragraph link path (`LinkTarget` -> `chunk_link_offsets`)
    /// cannot project links onto a rendered table because the table
    /// replace consumes the entire source range — no text chunk's
    /// rendering walks over the link text.  The parser instead emits
    /// `TableHyperlink`s during table formatting with positions in
    /// table-local coordinates; the renderer translates them to absolute
    /// `HyperlinkTarget`s.
    pub hyperlinks: Vec<TableHyperlink>,
}

/// Rendered Mermaid diagram replacement for pretty mode rendering.
#[derive(Debug, Clone)]
pub struct MermaidReplace {
    /// Plain lines for ANSI rendering.
    pub lines: Vec<String>,
    /// Styled lines for ratatui rendering.
    pub styled_lines: Vec<Line<'static>>,
    /// Source byte range this replaces.
    pub range: Range<usize>,
}

/// Calculate the display width of a string (accounting for Unicode).
pub fn unicode_display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    s.width()
}

/// Polyfill for `str::floor_char_boundary` (stable in Rust 1.91+).
///
/// Snaps `index` down to the nearest UTF-8 char boundary in `s`.  Indices
/// past the end of `s` are clamped to `s.len()`.  Replace with the std
/// method once the workspace toolchain is bumped to 1.91+.
pub(crate) fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut i = index.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Polyfill for `str::ceil_char_boundary` (stable in Rust 1.91+).
///
/// Snaps `index` up to the nearest UTF-8 char boundary in `s`.  Indices
/// past the end of `s` are clamped to `s.len()`.  Replace with the std
/// method once the workspace toolchain is bumped to 1.91+.
pub(crate) fn ceil_char_boundary(s: &str, index: usize) -> usize {
    let mut i = index.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Event kind for the render loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum RenderEventKind {
    Highlight = 0,
    Replace = 1,
    Table = 2,
    Mermaid = 3,
}

/// Render event: marks where a highlight/replace/table starts or ends.
/// Derives Ord for sorting by (pos, kind, index, is_end).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RenderEvent {
    pub pos: usize,
    pub kind: RenderEventKind,
    pub index: usize,
    pub is_end: bool,
}

/// Reusable buffers for markdown highlighting and rendering.
///
/// All vectors are cleared (keeping capacity) between renders, eliminating
/// allocation overhead in the streaming hot path.
///
/// # Buffer Categories
///
/// **Parse output buffers** - populated during `run()`, read-only during `render()`:
/// - `highlights`: Style ranges for inline formatting
/// - `replaces`: Syntax-highlighted code blocks
/// - `transforms`: Character substitutions (e.g., bullets)
/// - `untagged_code_ranges`: Code blocks without language tags
/// - `table_replaces`: Formatted table replacements
///
/// **Render scratch buffers** - temporary storage during `render()`:
/// - `render_events`: Sorted event queue for the render loop
/// - `current_spans`: Building current line's spans
/// - `active_highlights`: Stack of active highlight indices
pub struct MarkdownBuffers {
    // Parse output buffers (written by run(), read by render())
    pub highlights: Vec<Highlight>,
    pub replaces: Vec<Replace>,
    pub transforms: Vec<Transform>,
    pub untagged_code_ranges: Vec<Range<usize>>,
    pub table_replaces: Vec<TableReplace>,
    pub mermaid_replaces: Vec<MermaidReplace>,
    pub link_targets: Vec<LinkTarget>,
    /// Closed fenced code blocks, in document order (see [`CodeBlockMeta`]).
    pub code_blocks: Vec<CodeBlockMeta>,

    // Render scratch buffers (used only during render())
    pub render_events: Vec<RenderEvent>,
    pub current_spans: Vec<Span<'static>>,
    pub active_highlights: Vec<usize>,
}

impl MarkdownBuffers {
    pub fn new() -> Self {
        Self {
            highlights: Vec::new(),
            replaces: Vec::new(),
            transforms: Vec::new(),
            untagged_code_ranges: Vec::new(),
            table_replaces: Vec::new(),
            mermaid_replaces: Vec::new(),
            link_targets: Vec::new(),
            code_blocks: Vec::new(),
            render_events: Vec::new(),
            current_spans: Vec::new(),
            active_highlights: Vec::new(),
        }
    }

    /// Clear all buffers, keeping allocated capacity.
    pub fn clear(&mut self) {
        self.highlights.clear();
        self.replaces.clear();
        self.transforms.clear();
        self.untagged_code_ranges.clear();
        self.table_replaces.clear();
        self.mermaid_replaces.clear();
        self.link_targets.clear();
        self.code_blocks.clear();
        self.render_events.clear();
        self.current_spans.clear();
        self.active_highlights.clear();
    }
}

impl Default for MarkdownBuffers {
    fn default() -> Self {
        Self::new()
    }
}
