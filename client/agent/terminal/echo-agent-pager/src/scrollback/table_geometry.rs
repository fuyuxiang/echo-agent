//! Box-drawing table grid detection so selection inside rendered tables
//! operates on cells; anything `detect` can't prove falls back to linear.
//! Table lines never soft-wrap, so one rendered line is one block line.

use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A cell position within a detected grid: `row` indexes logical rows
/// (header = 0), `col` indexes columns left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRef {
    pub row: usize,
    pub col: usize,
}

/// Geometry of one box-drawing table, in the block's line/column space:
/// line indices are `block_line_idx` values, columns are display columns in
/// the same space as `RangeHit::col_within_range`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableGeometry {
    /// Full extent of the grid, top border line ..= bottom border line
    /// (half-open).
    line_range: Range<usize>,
    /// Display columns of the vertical grid lines, ascending.
    /// `junction_cols.len() == column count + 1`.
    junction_cols: Vec<u16>,
    /// Per logical row, the contiguous block-line range of its content lines
    /// (a row wrapped inside cells spans several lines). Never empty.
    rows: Vec<Range<usize>>,
}

/// Border-row family, keyed by its corner/junction glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BorderKind {
    /// `┌──┬──┐`
    Top,
    /// `├──┼──┤`
    Divider,
    /// `└──┴──┘`
    Bottom,
}

/// One line classified against (or independent of) a grid context.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GridLine {
    Border {
        junctions: Vec<u16>,
        kind: BorderKind,
    },
    Content,
    Other,
}

const BAR: char = '\u{2502}'; // │

/// Chars permitted before a grid's left edge: indentation and blockquote
/// bars (`│ `-prefixed tables render inside quotes with fully selectable
/// text — see `QuoteBarStrip`).
fn is_prefix_char(c: char) -> bool {
    c == ' ' || c == BAR
}

/// (display column, first char) for every grapheme in `text`, mirroring the
/// column arithmetic of `slice_display_cols` / `word_boundaries_at_col`.
fn grapheme_cols(text: &str) -> impl Iterator<Item = (u16, char)> + '_ {
    let mut col = 0u16;
    text.graphemes(true).filter_map(move |g| {
        let width = UnicodeWidthStr::width(g) as u16;
        if width == 0 {
            return None;
        }
        let at = col;
        col = col.saturating_add(width);
        Some((at, g.chars().next().unwrap_or(' ')))
    })
}

/// Parse a border row (`┌──┬──┐` / `├──┼──┤` / `└──┴──┘`), tolerating an
/// indentation/blockquote prefix. Returns the junction columns (corners
/// included) and the row family, or `None` when the line is not a border row.
fn parse_border_row(text: &str) -> Option<(Vec<u16>, BorderKind)> {
    let (kind, mid, close) = ('\u{250C}', '\u{252C}', '\u{2510}'); // ┌ ┬ ┐
    let (dkind, dmid, dclose) = ('\u{251C}', '\u{253C}', '\u{2524}'); // ├ ┼ ┤
    let (bkind, bmid, bclose) = ('\u{2514}', '\u{2534}', '\u{2518}'); // └ ┴ ┘
    const H: char = '\u{2500}'; // ─

    let mut junctions: Vec<u16> = Vec::new();
    let mut family: Option<BorderKind> = None;
    let mut closed = false;

    for (col, c) in grapheme_cols(text) {
        match family {
            None => {
                // Still in the optional prefix; the first corner glyph opens
                // the grid and fixes the family.
                let f = match c {
                    _ if c == kind => Some(BorderKind::Top),
                    _ if c == dkind => Some(BorderKind::Divider),
                    _ if c == bkind => Some(BorderKind::Bottom),
                    _ if is_prefix_char(c) => None,
                    _ => return None,
                };
                if let Some(f) = f {
                    family = Some(f);
                    junctions.push(col);
                }
            }
            Some(f) => {
                if closed {
                    // Trailing content after the closing corner: not a grid row.
                    return None;
                }
                let (m, cl) = match f {
                    BorderKind::Top => (mid, close),
                    BorderKind::Divider => (dmid, dclose),
                    BorderKind::Bottom => (bmid, bclose),
                };
                if c == m {
                    junctions.push(col);
                } else if c == cl {
                    junctions.push(col);
                    closed = true;
                } else if c != H {
                    return None;
                }
            }
        }
    }

    // A grid needs at least two junctions (one column) and a closing corner.
    if !closed || junctions.len() < 2 {
        return None;
    }
    Some((junctions, family.expect("closed implies family")))
}

/// Whether `text` is a content row of a grid with the given junction set:
/// a `│` at every junction column, nothing but prefix chars before the left
/// edge, and nothing after the right edge (selection text is end-trimmed).
fn is_content_row(text: &str, junctions: &[u16]) -> bool {
    let (Some(&left), Some(&right)) = (junctions.first(), junctions.last()) else {
        return false;
    };
    let mut needed = junctions.iter().peekable();
    let mut last_col = 0u16;
    for (col, c) in grapheme_cols(text) {
        last_col = col;
        if col < left && !is_prefix_char(c) {
            return false;
        }
        if col > right {
            return false;
        }
        if needed.peek() == Some(&&col) {
            if c != BAR {
                return false;
            }
            needed.next();
        }
    }
    needed.peek().is_none() && last_col == right
}

/// Classify one line against a known junction set.
fn classify(text: &str, junctions: &[u16]) -> GridLine {
    if let Some((j, kind)) = parse_border_row(text) {
        if j == junctions {
            return GridLine::Border { junctions: j, kind };
        }
        return GridLine::Other;
    }
    if is_content_row(text, junctions) {
        return GridLine::Content;
    }
    GridLine::Other
}

impl TableGeometry {
    /// Detect the grid containing `at_line`, reading lines through
    /// `text_at`. `None` unless `at_line` sits inside a fully-enclosed,
    /// column-consistent grid — callers then fall back to linear.
    pub fn detect(text_at: impl Fn(usize) -> Option<String>, at_line: usize) -> Option<Self> {
        // The anchor line itself must be part of a grid; its border row (or,
        // for content rows, the nearest border row above) fixes the junction
        // set every other line is validated against.
        let anchor_text = text_at(at_line)?;
        let junctions: Vec<u16> = if let Some((j, _)) = parse_border_row(&anchor_text) {
            j
        } else {
            // Walk up to the nearest border row to fix the junction set.
            // Capped: a real anchor's border is at most one wrapped row
            // above; a long walk means prefix-led prose, not a table.
            const MAX_JUNCTION_SEARCH: usize = 400;
            let mut found: Option<Vec<u16>> = None;
            let mut line = at_line;
            while line > 0 && at_line - line < MAX_JUNCTION_SEARCH {
                line -= 1;
                let Some(text) = text_at(line) else { break };
                if let Some((j, _)) = parse_border_row(&text) {
                    found = Some(j);
                    break;
                }
                // Cheap plausibility gate so we don't scan a whole prose
                // block: rows of a grid always start with a prefix char.
                if !text.chars().next().is_some_and(is_prefix_char) {
                    break;
                }
            }
            found?
        };

        // Validate outward: walk up to the top border, down to the bottom
        // border, requiring every line in between to belong to the grid.
        let mut top = at_line;
        loop {
            let text = text_at(top)?;
            match classify(&text, &junctions) {
                GridLine::Border {
                    kind: BorderKind::Top,
                    ..
                } => break,
                // Hitting a bottom border strictly above `at_line` means
                // `at_line` was below the grid, not inside it. (`at_line`
                // itself may be the bottom border.)
                GridLine::Border {
                    kind: BorderKind::Bottom,
                    ..
                } if top < at_line => return None,
                GridLine::Border { .. } | GridLine::Content if top > 0 => top -= 1,
                _ => return None,
            }
        }
        let mut bottom = at_line;
        loop {
            let text = text_at(bottom)?;
            match classify(&text, &junctions) {
                GridLine::Border {
                    kind: BorderKind::Bottom,
                    ..
                } => break,
                GridLine::Border {
                    kind: BorderKind::Top,
                    ..
                } if bottom > at_line => return None,
                GridLine::Border { .. } | GridLine::Content => bottom += 1,
                GridLine::Other => return None,
            }
        }

        // Logical rows: contiguous content-line runs between border rows.
        let mut rows: Vec<Range<usize>> = Vec::new();
        let mut run_start: Option<usize> = None;
        for line in top..=bottom {
            let text = text_at(line)?;
            match classify(&text, &junctions) {
                GridLine::Content => {
                    run_start.get_or_insert(line);
                }
                GridLine::Border { .. } => {
                    if let Some(start) = run_start.take() {
                        rows.push(start..line);
                    }
                }
                GridLine::Other => return None,
            }
        }
        if rows.is_empty() {
            return None;
        }

        Some(Self {
            line_range: top..bottom + 1,
            junction_cols: junctions,
            rows,
        })
    }

    /// Full grid extent (top border ..= bottom border, half-open).
    pub fn line_range(&self) -> Range<usize> {
        self.line_range.clone()
    }

    pub fn n_cols(&self) -> usize {
        self.junction_cols.len() - 1
    }

    pub fn n_rows(&self) -> usize {
        self.rows.len()
    }

    /// The logical row containing `line`, if `line` is a content line.
    pub fn row_of_line(&self, line: usize) -> Option<usize> {
        self.rows.iter().position(|r| r.contains(&line))
    }

    /// Content-line range of a logical row.
    pub fn row_lines(&self, row: usize) -> Range<usize> {
        self.rows[row].clone()
    }

    /// Display-column band of a column's cell interior: everything strictly
    /// between the two flanking `│` glyphs (padding included).
    pub fn band(&self, col: usize) -> Range<u16> {
        self.junction_cols[col].saturating_add(1)..self.junction_cols[col + 1]
    }

    /// The cell at (`line`, `col`), or `None` when `line` is a border row or
    /// `col` falls outside the grid. A click exactly on a `│` snaps to the
    /// cell on its right (left for the closing border).
    pub fn cell_at(&self, line: usize, col: u16) -> Option<CellRef> {
        let row = self.row_of_line(line)?;
        let first = *self.junction_cols.first().expect("non-empty");
        let last = *self.junction_cols.last().expect("non-empty");
        if col < first || col > last {
            return None;
        }
        let c = match self.junction_cols.iter().rposition(|&j| j <= col) {
            Some(j) if j == self.junction_cols.len() - 1 => self.n_cols() - 1,
            Some(j) => j,
            None => 0,
        };
        Some(CellRef { row, col: c })
    }

    /// The column whose content interior (band minus the renderer's one
    /// padding column per side) contains `col`.
    fn interior_col_at(&self, col: u16) -> Option<usize> {
        (0..self.n_cols()).find(|&c| {
            let band = self.band(c);
            let lo = band.start.saturating_add(1);
            let hi = band.end.saturating_sub(1);
            (lo..hi).contains(&col)
        })
    }

    /// Latched head-cell resolution: borders, padding, and divider rows
    /// keep `held`; only another cell's content interior (or the grid's
    /// outer edge, which clamps) moves it. Empty cells never capture it.
    pub fn latched_cell_at(&self, held: CellRef, line: usize, col: u16) -> CellRef {
        let row = if let Some(row) = self.row_of_line(line) {
            row
        } else if line < self.line_range.start {
            0
        } else if line >= self.line_range.end {
            self.n_rows() - 1
        } else {
            held.row
        };
        let col = if let Some(col) = self.interior_col_at(col) {
            col
        } else if col < *self.junction_cols.first().expect("non-empty") {
            0
        } else if col > *self.junction_cols.last().expect("non-empty") {
            self.n_cols() - 1
        } else {
            held.col
        };
        CellRef { row, col }
    }

    /// A cell's text: its per-line band slices trimmed and joined with a
    /// space (cells wrap at spaces/punctuation, so a space join reconstructs
    /// the content).
    pub fn cell_text(&self, cell: CellRef, text_at: impl Fn(usize) -> Option<String>) -> String {
        let band = self.band(cell.col);
        let mut out = String::new();
        for line in self.row_lines(cell.row) {
            let Some(text) = text_at(line) else { continue };
            let slice = crate::scrollback::types::slice_display_cols(&text, band.start, band.end);
            let fragment = slice.trim();
            if fragment.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(fragment);
        }
        out
    }

    /// TSV for the rectangular cell range spanned by `a` and `b` (order
    /// irrelevant): cells tab-joined, rows newline-joined. Tabs inside cell
    /// text are flattened to spaces so the TSV shape survives.
    pub fn grid_tsv(
        &self,
        a: CellRef,
        b: CellRef,
        text_at: impl Fn(usize) -> Option<String>,
    ) -> String {
        let (r0, r1) = (a.row.min(b.row), a.row.max(b.row));
        let (c0, c1) = (a.col.min(b.col), a.col.max(b.col));
        let mut rows_out: Vec<String> = Vec::new();
        for row in r0..=r1 {
            let cells: Vec<String> = (c0..=c1)
                .map(|col| {
                    self.cell_text(CellRef { row, col }, &text_at)
                        .replace('\t', " ")
                })
                .collect();
            rows_out.push(cells.join("\t"));
        }
        rows_out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Text source over a static list of lines.
    fn src<'a>(lines: &'a [&'a str]) -> impl Fn(usize) -> Option<String> + 'a {
        move |i| lines.get(i).map(|s| s.to_string())
    }

    const TABLE: &[&str] = &[
        "Intro prose",
        "┌─────────┬────────┐",
        "│ Name    │ Role   │",
        "├─────────┼────────┤",
        "│ Alice   │ Eng    │",
        "├─────────┼────────┤",
        "│ Bob     │ Design │",
        "└─────────┴────────┘",
        "Outro prose",
    ];

    #[test]
    fn detects_from_content_and_border_lines() {
        for at in 1..=7 {
            let geom = TableGeometry::detect(src(TABLE), at).expect("grid detected");
            assert_eq!(geom.line_range(), 1..8);
            assert_eq!(geom.n_cols(), 2);
            assert_eq!(geom.n_rows(), 3);
            assert_eq!(geom.row_lines(0), 2..3);
            assert_eq!(geom.row_lines(2), 6..7);
        }
    }

    #[test]
    fn no_grid_outside_table() {
        assert_eq!(TableGeometry::detect(src(TABLE), 0), None);
        assert_eq!(TableGeometry::detect(src(TABLE), 8), None);
    }

    #[test]
    fn cell_lookup_and_bands() {
        let geom = TableGeometry::detect(src(TABLE), 4).unwrap();
        // "│ Alice   │ Eng    │" — junctions at cols 0, 10, 19.
        assert_eq!(geom.band(0), 1..10);
        assert_eq!(geom.band(1), 11..19);
        assert_eq!(geom.cell_at(4, 3), Some(CellRef { row: 1, col: 0 }));
        assert_eq!(geom.cell_at(4, 12), Some(CellRef { row: 1, col: 1 }));
        // Junction col snaps right; closing border snaps left.
        assert_eq!(geom.cell_at(4, 10), Some(CellRef { row: 1, col: 1 }));
        assert_eq!(geom.cell_at(4, 19), Some(CellRef { row: 1, col: 1 }));
        assert_eq!(geom.cell_at(4, 0), Some(CellRef { row: 1, col: 0 }));
        // Border rows have no cells.
        assert_eq!(geom.cell_at(3, 3), None);
        // Outside the grid columns.
        assert_eq!(geom.cell_at(4, 25), None);
    }

    #[test]
    fn latched_cell_moves_only_via_content_or_past_the_edge() {
        let geom = TableGeometry::detect(src(TABLE), 4).unwrap();
        let held = CellRef { row: 1, col: 0 };
        // Another row's content line moves the row latch.
        assert_eq!(geom.latched_cell_at(held, 6, 3), CellRef { row: 2, col: 0 });
        // Divider and border rows keep the held row (no snap below).
        assert_eq!(geom.latched_cell_at(held, 5, 3), held);
        assert_eq!(geom.latched_cell_at(held, 1, 3), held);
        // Above / below the grid clamps to the first / last row.
        assert_eq!(geom.latched_cell_at(held, 0, 3), CellRef { row: 0, col: 0 });
        assert_eq!(geom.latched_cell_at(held, 8, 3), CellRef { row: 2, col: 0 });
        // "│ Alice   │ Eng    │" — junctions at 0, 10, 19; bands 1..10, 11..19.
        // The junction and both flanking padding columns keep the held column.
        assert_eq!(geom.latched_cell_at(held, 4, 9), held);
        assert_eq!(geom.latched_cell_at(held, 4, 10), held);
        assert_eq!(geom.latched_cell_at(held, 4, 11), held);
        // The neighbor's content interior captures the latch.
        assert_eq!(
            geom.latched_cell_at(held, 4, 12),
            CellRef { row: 1, col: 1 }
        );
        // Past the right edge clamps to the last column.
        assert_eq!(
            geom.latched_cell_at(held, 4, 40),
            CellRef { row: 1, col: 1 }
        );
        // Latch releases symmetrically: held in Role, back into Name content.
        let held_role = CellRef { row: 1, col: 1 };
        assert_eq!(geom.latched_cell_at(held_role, 4, 10), held_role);
        assert_eq!(
            geom.latched_cell_at(held_role, 4, 5),
            CellRef { row: 1, col: 0 }
        );
    }

    #[test]
    fn cell_text_and_tsv() {
        let geom = TableGeometry::detect(src(TABLE), 4).unwrap();
        assert_eq!(
            geom.cell_text(CellRef { row: 1, col: 0 }, src(TABLE)),
            "Alice"
        );
        assert_eq!(
            geom.grid_tsv(
                CellRef { row: 1, col: 0 },
                CellRef { row: 2, col: 0 },
                src(TABLE)
            ),
            "Alice\nBob"
        );
        assert_eq!(
            geom.grid_tsv(
                CellRef { row: 2, col: 1 },
                CellRef { row: 1, col: 0 },
                src(TABLE)
            ),
            "Alice\tEng\nBob\tDesign"
        );
    }

    const WRAPPED: &[&str] = &[
        "┌─────────┬──────────┐",
        "│ Name    │ Notes    │",
        "├─────────┼──────────┤",
        "│ Alice   │ likes    │",
        "│         │ long     │",
        "│         │ walks    │",
        "└─────────┴──────────┘",
    ];

    #[test]
    fn wrapped_cell_fragments_join_with_space() {
        let geom = TableGeometry::detect(src(WRAPPED), 4).unwrap();
        assert_eq!(geom.n_rows(), 2);
        assert_eq!(geom.row_lines(1), 3..6);
        assert_eq!(
            geom.cell_text(CellRef { row: 1, col: 1 }, src(WRAPPED)),
            "likes long walks"
        );
        // Empty fragments (the padding rows of the Name cell) are skipped.
        assert_eq!(
            geom.cell_text(CellRef { row: 1, col: 0 }, src(WRAPPED)),
            "Alice"
        );
    }

    #[test]
    fn blockquoted_table_with_quote_bar_prefix() {
        let quoted: &[&str] = &[
            "│ ┌─────┬─────┐",
            "│ │ A   │ B   │",
            "│ ├─────┼─────┤",
            "│ │ one │ two │",
            "│ └─────┴─────┘",
        ];
        let geom = TableGeometry::detect(src(quoted), 3).expect("quoted grid");
        assert_eq!(geom.n_cols(), 2);
        assert_eq!(
            geom.cell_text(CellRef { row: 1, col: 0 }, src(quoted)),
            "one"
        );
    }

    #[test]
    fn wide_glyphs_use_display_columns() {
        let emoji: &[&str] = &["┌──────┬──────┐", "│ 名前 │ ok   │", "└──────┴──────┘"];
        let geom = TableGeometry::detect(src(emoji), 1).expect("grid");
        assert_eq!(geom.band(0), 1..7);
        assert_eq!(
            geom.cell_text(CellRef { row: 0, col: 0 }, src(emoji)),
            "名前"
        );
        // Click on the second display column of 名 resolves to col 0.
        assert_eq!(geom.cell_at(1, 3), Some(CellRef { row: 0, col: 0 }));
    }

    #[test]
    fn inconsistent_junctions_bail() {
        let broken: &[&str] = &[
            "┌─────┬─────┐",
            "│ A   │ B   │",
            "├────────┼──┤", // misaligned divider
            "│ one │ two │",
            "└─────┴─────┘",
        ];
        assert_eq!(TableGeometry::detect(src(broken), 1), None);
    }

    #[test]
    fn unclosed_grid_bails() {
        let unclosed: &[&str] = &["┌─────┬─────┐", "│ A   │ B   │", "prose again"];
        assert_eq!(TableGeometry::detect(src(unclosed), 1), None);
    }

    #[test]
    fn stray_bar_in_cell_content_is_not_a_junction() {
        let stray: &[&str] = &["┌───────┬─────┐", "│ a │ b │ c   │", "└───────┴─────┘"];
        let geom = TableGeometry::detect(src(stray), 1).expect("grid");
        assert_eq!(geom.n_cols(), 2);
        // The stray │ inside the first cell is content, not a boundary.
        assert_eq!(
            geom.cell_text(CellRef { row: 0, col: 0 }, src(stray)),
            "a │ b"
        );
    }

    #[test]
    fn plain_prose_and_rules_are_not_grids() {
        let prose: &[&str] = &["hello world", "─────────", "goodbye"];
        assert_eq!(TableGeometry::detect(src(prose), 0), None);
        assert_eq!(TableGeometry::detect(src(prose), 1), None);
    }
}
