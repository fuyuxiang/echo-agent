//! Two-dimensional math layout box.

use crate::buffers::unicode_display_width;

/// Two-dimensional text box with an anchor row where horizontal flow
/// attaches.
///
/// Multi-row content (matrix-family environments) extends above/below the
/// anchor row; subsequent output continues on the anchor row. This keeps a
/// prefix, a matrix, and a suffix aligned:
///
/// ```text
/// A = ⎛1  2⎞,   det(A) = −2
///     ⎝3  4⎠
/// ```
pub(super) struct MathBox {
    lines: Vec<String>,
    /// Row index that horizontal flow currently appends to.
    anchor: usize,
    /// First row belonging to the current visual line. Rows before `floor`
    /// are completed lines from earlier `\\` breaks and must never be
    /// touched by box attachment.
    floor: usize,
    /// Flat mode (inline math): vertical layout is impossible, so row breaks
    /// render as `; ` and environments render single-row.
    pub(super) flat: bool,
}

impl MathBox {
    pub(super) fn new(flat: bool) -> Self {
        Self {
            lines: vec![String::new()],
            anchor: 0,
            floor: 0,
            flat,
        }
    }

    fn cur(&mut self) -> &mut String {
        &mut self.lines[self.anchor]
    }

    /// `true` when nothing has been emitted on the current flow row yet.
    pub(super) fn at_line_start(&self) -> bool {
        self.lines[self.anchor].is_empty()
    }

    pub(super) fn ends_with_space(&self) -> bool {
        self.lines[self.anchor].ends_with(' ')
    }

    pub(super) fn push(&mut self, c: char) {
        if c == '\n' {
            self.vbreak();
        } else {
            self.cur().push(c);
        }
    }

    pub(super) fn push_str(&mut self, s: &str) {
        if s.contains('\n') {
            self.hcat_rows(s.split('\n').map(str::to_string).collect());
        } else {
            self.cur().push_str(s);
        }
    }

    /// End the current visual line; flow continues on a fresh row below all
    /// existing rows. Flat mode renders the break as `; `.
    fn vbreak(&mut self) {
        if self.flat {
            if !self.at_line_start() {
                let cur = self.cur();
                while cur.ends_with(' ') {
                    cur.pop();
                }
                cur.push_str("; ");
            }
        } else {
            self.lines.push(String::new());
            self.anchor = self.lines.len() - 1;
            self.floor = self.anchor;
        }
    }

    /// Attach `rows` as a box at the current flow position, anchored at the
    /// box's upper-middle row. All box rows start at the same column; flow
    /// resumes on the anchor row past the box's widest row.
    pub(super) fn hcat_rows(&mut self, rows: Vec<String>) {
        if rows.is_empty() {
            return;
        }
        if self.flat || rows.len() == 1 {
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    self.vbreak();
                }
                self.cur().push_str(row);
            }
            return;
        }
        let box_anchor = (rows.len() - 1) / 2;
        let attach_col = unicode_display_width(&self.lines[self.anchor]);
        let box_width = rows
            .iter()
            .map(|r| unicode_display_width(r))
            .max()
            .unwrap_or(0);

        // Ensure enough rows above the anchor within the current visual line.
        let have_above = self.anchor - self.floor;
        if box_anchor > have_above {
            let add = box_anchor - have_above;
            for _ in 0..add {
                self.lines.insert(self.floor, String::new());
            }
            self.anchor += add;
        }
        // Ensure enough rows below the anchor.
        let below = rows.len() - box_anchor - 1;
        let have_below = self.lines.len() - self.anchor - 1;
        if below > have_below {
            for _ in 0..(below - have_below) {
                self.lines.push(String::new());
            }
        }
        // Place the box rows, left-padded to the attach column.
        for (i, row) in rows.iter().enumerate() {
            let target = self.anchor - box_anchor + i;
            let line = &mut self.lines[target];
            let cur_w = unicode_display_width(line);
            if cur_w < attach_col {
                line.push_str(&" ".repeat(attach_col - cur_w));
            }
            line.push_str(row);
        }
        // Flow resumes past the box's widest row.
        let frontier = attach_col + box_width;
        let cur_w = unicode_display_width(&self.lines[self.anchor]);
        if cur_w < frontier {
            let pad = frontier - cur_w;
            self.lines[self.anchor].push_str(&" ".repeat(pad));
        }
    }

    pub(super) fn into_lines(self) -> Vec<String> {
        self.lines
    }
}

impl std::fmt::Write for MathBox {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.push_str(s);
        Ok(())
    }
}
