use std::cmp::{max, min};
use std::ops::Range;
use std::sync::{Arc, LazyLock};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::scrollback::table_geometry::{CellRef, TableGeometry};
use crate::scrollback::types::SelectionBoundary;
use crate::theme::Theme;

// ---------------------------------------------------------------------------
// Auto-scroll types
// ---------------------------------------------------------------------------

/// Direction for drag auto-scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoScrollDirection {
    Up,
    Down,
}

/// State for timer-driven drag auto-scroll.
///
/// While active, `tick_drag_autoscroll` scrolls by `speed` rows per tick
/// in the given direction. The direction and speed are recomputed from the
/// mouse position each time the pointer moves, and the state is cleared
/// when the pointer returns inside the content area or the drag ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragAutoScrollState {
    pub direction: AutoScrollDirection,
    /// Rows to scroll per tick.
    pub speed: u16,
}

/// Rows of near-edge interior zone that still trigger autoscroll.
const EDGE_THRESHOLD: u16 = 2;

/// Compute autoscroll direction and speed from mouse position relative to
/// the scrollback content area.
///
/// Returns `Some(state)` when the pointer is above, below, or within
/// [`EDGE_THRESHOLD`] rows of the content boundary. Returns `None` when
/// the pointer is comfortably inside the viewport.
pub fn compute_autoscroll(mouse_row: u16, content_area: Rect) -> Option<DragAutoScrollState> {
    let top = content_area.y;
    let bottom = content_area.y.saturating_add(content_area.height);

    if content_area.height == 0 {
        return None;
    }

    if mouse_row < top.saturating_add(EDGE_THRESHOLD) {
        // Above or near top edge.
        let distance = top.saturating_add(EDGE_THRESHOLD).saturating_sub(mouse_row);
        Some(DragAutoScrollState {
            direction: AutoScrollDirection::Up,
            speed: speed_for_distance(distance),
        })
    } else if mouse_row >= bottom.saturating_sub(EDGE_THRESHOLD) {
        // Below or near bottom edge.
        let distance = mouse_row
            .saturating_sub(bottom.saturating_sub(EDGE_THRESHOLD))
            .saturating_add(1);
        Some(DragAutoScrollState {
            direction: AutoScrollDirection::Down,
            speed: speed_for_distance(distance),
        })
    } else {
        None
    }
}

/// Map distance-beyond-edge to scroll speed (rows per tick).
fn speed_for_distance(distance: u16) -> u16 {
    match distance {
        0..=2 => 1,
        3..=5 => 2,
        6..=10 => 3,
        _ => 5,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedSelectionModel {
    pub ranges: Vec<ResolvedSelectableRange>,
    pub visible_blocks: Vec<VisibleBlockGeometry>,
    pub content_area: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedSelectionBoundary {
    entry_idx: usize,
    range_id: u16,
    block_line_idx: usize,
    boundary: Arc<SelectionBoundary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ResolvedSelectionBoundaries(Vec<ResolvedSelectionBoundary>);

impl ResolvedSelectionBoundaries {
    pub(crate) fn push(&mut self, line: &ResolvedSelectableLine, boundary: Arc<SelectionBoundary>) {
        self.0.push(ResolvedSelectionBoundary {
            entry_idx: line.entry_idx,
            range_id: line.range_id,
            block_line_idx: line.block_line_idx,
            boundary,
        });
    }

    fn boundary_for_line(&self, line: &ResolvedSelectableLine) -> Option<&SelectionBoundary> {
        self.0
            .iter()
            .find(|boundary| {
                boundary.entry_idx == line.entry_idx
                    && boundary.range_id == line.range_id
                    && boundary.block_line_idx == line.block_line_idx
            })
            .map(|boundary| boundary.boundary.as_ref())
    }

    pub(crate) fn boundary_for_hit(&self, hit: &RangeHit) -> Option<&SelectionBoundary> {
        self.0
            .iter()
            .find(|boundary| {
                boundary.entry_idx == hit.entry_idx
                    && boundary.range_id == hit.range_id
                    && boundary.block_line_idx == hit.block_line_idx
            })
            .map(|boundary| boundary.boundary.as_ref())
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSelectableRange {
    pub entry_idx: usize,
    pub range_id: u16,
    pub lines: Vec<ResolvedSelectableLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSelectableLine {
    pub entry_idx: usize,
    /// Block `selection_range` ids count up from 0; `u16::MAX` is reserved
    /// for the labeled group header's synthetic row
    /// (`render::GROUP_HEADER_RANGE_ID`).
    pub range_id: u16,
    pub block_line_idx: usize,
    pub screen_y: u16,
    pub screen_x: u16,
    pub selectable_cols: Range<u16>,
    /// Logical copy text: trailing-trimmed for `Selectable::All`, a span subset
    /// for `Selectable::Spans`, or the stored override for tool headers.
    pub text: String,
    /// Exact text painted in the selectable columns (logical order, untrimmed),
    /// or `None` for synthetic rows whose painted cells equal `text`. Selection
    /// snaps and slices against this so drag columns line up with drawn cells
    /// even when `text` diverges from what was painted (see
    /// [`crate::scrollback::types::painted_selectable_region`]).
    pub painted_region: Option<String>,
    pub joiner_to_previous: Option<String>,
}

impl ResolvedSelectableLine {
    /// `(col distance, clamped col-within-range)` for a pointer at screen
    /// `col` on this line: distance 0 with the exact offset inside the
    /// selectable span, otherwise the gap to the nearer edge with the offset
    /// clamped to that edge. `None` when the line has no selectable width.
    ///
    /// The single source of column semantics for every hit test, so their
    /// same-row behavior cannot diverge.
    fn col_metrics(&self, col: u16) -> Option<(u16, u16)> {
        let start = self.screen_x.saturating_add(self.selectable_cols.start);
        let end = self.screen_x.saturating_add(self.selectable_cols.end);
        if end <= start {
            return None;
        }

        let width = end.saturating_sub(start);
        Some(if col < start {
            (start.saturating_sub(col), 0)
        } else if col >= end {
            (
                col.saturating_sub(end.saturating_sub(1)),
                width.saturating_sub(1),
            )
        } else {
            (0, col.saturating_sub(start))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleBlockGeometry {
    pub entry_idx: usize,
    pub area: Rect,
    pub content_area: Rect,
    pub selection_area: Rect,
    pub content_width: u16,
    pub top_clipped: bool,
    pub bottom_clipped: bool,
    pub drag_startable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeHit {
    pub entry_idx: usize,
    pub range_id: u16,
    /// Stable line identifier: the line's index within the block's full output.
    /// Unlike the position in the visible `range.lines[]` array, this does not
    /// change when the viewport scrolls.
    pub block_line_idx: usize,
    pub col_within_range: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingTextDrag {
    pub anchor: RangeHit,
    pub start_col: u16,
    pub start_row: u16,
    /// The anchor block's `VisibleBlockGeometry.content_width` at mouse-down
    /// (`None` when the block had no geometry then). Copy needs the width the
    /// drag's `block_line_idx` values were captured against even after the
    /// block scrolls fully out of `visible_blocks`.
    pub anchor_content_width: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveTextDrag {
    pub anchor: RangeHit,
    pub head: RangeHit,
    pub kind: SelectionKind,
    /// Carried over from [`PendingTextDrag::anchor_content_width`].
    pub anchor_content_width: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionEndpoint {
    pub block_line_idx: usize,
    pub col_within_range: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionOrigin {
    Drag,
    DoubleClick,
    TripleClick,
}

/// Shape of a text selection: `Linear` sweeps whole lines between the
/// endpoints; drags anchored inside a detected table cell are table-shaped
/// (see [`crate::scrollback::table_geometry`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionKind {
    #[default]
    Linear,
    /// Head latched to the anchor cell: text selection clamped to the cell's
    /// column band, spanning its wrapped fragment lines.
    TableCell,
    /// A rectangular range of whole cells (`anchor` to `head`), copied as
    /// TSV. Cells are carried, not re-derived: endpoints can sit on columns
    /// that resolve to no cell, and paint/copy must match resolution.
    TableGrid { anchor: CellRef, head: CellRef },
}

/// Side-car [`TableGeometry`] keyed to the selection it was resolved for.
/// Consumers check the key; a stale side-car is ignored (table kinds paint
/// nothing without geometry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSelectionGeometry {
    pub entry_idx: usize,
    pub range_id: u16,
    pub geometry: TableGeometry,
}

impl TableSelectionGeometry {
    /// The geometry, if it was resolved for (`entry_idx`, `range_id`).
    pub fn for_selection(&self, entry_idx: usize, range_id: u16) -> Option<&TableGeometry> {
        (self.entry_idx == entry_idx && self.range_id == range_id).then_some(&self.geometry)
    }
}

/// A text selection that persists after mouse-up for visual feedback.
///
/// Cleared on the next click elsewhere, Escape, or any scrollback navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistentTextSelection {
    pub entry_idx: usize,
    pub range_id: u16,
    pub anchor: SelectionEndpoint,
    pub head: SelectionEndpoint,
    pub origin: SelectionOrigin,
    pub kind: SelectionKind,
}

impl ResolvedSelectionModel {
    pub fn push_line(&mut self, line: ResolvedSelectableLine) {
        if let Some(last) = self.ranges.last_mut()
            && last.entry_idx == line.entry_idx
            && last.range_id == line.range_id
        {
            last.lines.push(line);
            return;
        }

        self.ranges.push(ResolvedSelectableRange {
            entry_idx: line.entry_idx,
            range_id: line.range_id,
            lines: vec![line],
        });
    }

    pub fn hit_test_selectable_range(&self, col: u16, row: u16) -> Option<RangeHit> {
        let mut best: Option<(u16, RangeHit)> = None;

        for range in &self.ranges {
            for line in &range.lines {
                if line.screen_y != row {
                    continue;
                }
                let Some((distance, col_within_range)) = line.col_metrics(col) else {
                    continue;
                };

                let hit = RangeHit {
                    entry_idx: range.entry_idx,
                    range_id: range.range_id,
                    block_line_idx: line.block_line_idx,
                    col_within_range,
                };
                if distance == 0 {
                    return Some(hit);
                }
                if best
                    .as_ref()
                    .is_none_or(|(best_distance, _)| distance < *best_distance)
                {
                    best = Some((distance, hit));
                }
            }
        }

        best.map(|(_, hit)| hit)
    }

    /// Nearest line of the anchor's `(entry_idx, range_id)` to `(col, row)`,
    /// by `(|screen_y - row|, then col distance)` — the drag-head resolver.
    ///
    /// Unlike [`Self::hit_test_selectable_range`] this never lands on another
    /// range and never misses while the anchor's range has visible lines, so
    /// the head tracks the pointer across gap/vpad/chrome rows and past the
    /// range's last line (native drag semantics). Same-row behavior is
    /// identical to `hit_test_selectable_range` restricted to that range.
    /// Full ties (a pointer row equidistant between two lines) resolve to the
    /// line farther from the anchor, so a drag over a dead row keeps
    /// extending the selection instead of retreating.
    ///
    /// `None` only when the range has no selectable lines in this model
    /// (scrolled fully out) — callers keep the previous head then.
    pub fn hit_test_nearest_in_range(
        &self,
        anchor: RangeHit,
        col: u16,
        row: u16,
    ) -> Option<RangeHit> {
        let range = self.range(anchor.entry_idx, anchor.range_id)?;
        let mut best: Option<((u16, u16), usize, RangeHit)> = None;

        for line in &range.lines {
            let Some((col_distance, col_within_range)) = line.col_metrics(col) else {
                continue;
            };

            let key = (line.screen_y.abs_diff(row), col_distance);
            let anchor_distance = line.block_line_idx.abs_diff(anchor.block_line_idx);
            let hit = RangeHit {
                entry_idx: range.entry_idx,
                range_id: range.range_id,
                block_line_idx: line.block_line_idx,
                col_within_range,
            };
            if best.as_ref().is_none_or(|(best_key, best_anchor_dist, _)| {
                key < *best_key || (key == *best_key && anchor_distance > *best_anchor_dist)
            }) {
                best = Some((key, anchor_distance, hit));
            }
        }

        best.map(|(_, _, hit)| hit)
    }

    pub fn hit_test_visible_block(&self, col: u16, row: u16) -> Option<&VisibleBlockGeometry> {
        self.visible_blocks
            .iter()
            .find(|block| rect_contains(block.area, col, row))
    }

    /// The content width `entry_idx`'s block was rendered at this frame, or
    /// `None` when the block is not in the viewport.
    pub fn visible_block_content_width(&self, entry_idx: usize) -> Option<u16> {
        self.visible_blocks
            .iter()
            .find(|b| b.entry_idx == entry_idx)
            .map(|b| b.content_width)
    }

    pub fn range(&self, entry_idx: usize, range_id: u16) -> Option<&ResolvedSelectableRange> {
        self.ranges
            .iter()
            .find(|range| range.entry_idx == entry_idx && range.range_id == range_id)
    }

    /// Hit-test for text selection: only return a hit when the click is
    /// directly on selectable columns (distance == 0).
    ///
    /// This ensures clicks on accent bars, borders, and padding fall through
    /// to block-level handling (fold toggle).
    pub fn hit_test_text_exact(&self, col: u16, row: u16) -> Option<RangeHit> {
        for range in &self.ranges {
            for line in &range.lines {
                if line.screen_y != row {
                    continue;
                }
                let Some((distance, col_within_range)) = line.col_metrics(col) else {
                    continue;
                };
                if distance == 0 {
                    return Some(RangeHit {
                        entry_idx: range.entry_idx,
                        range_id: range.range_id,
                        block_line_idx: line.block_line_idx,
                        col_within_range,
                    });
                }
            }
        }
        None
    }

    /// Look up the selectable line matching a [`RangeHit`].
    ///
    /// Convenience shorthand for `model.range(hit) → find(block_line_idx)`.
    pub fn line_for_hit(&self, hit: &RangeHit) -> Option<&ResolvedSelectableLine> {
        self.range(hit.entry_idx, hit.range_id)?
            .lines
            .iter()
            .find(|l| l.block_line_idx == hit.block_line_idx)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingBlockDrag {
    pub anchor_entry_idx: usize,
    pub start_col: u16,
    pub start_row: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveBlockDrag {
    pub anchor_entry_idx: usize,
    pub head_entry_idx: usize,
}

pub fn drag_threshold_exceeded(pending: &PendingTextDrag, col: u16, row: u16) -> bool {
    let dx = pending.start_col.abs_diff(col);
    let dy = pending.start_row.abs_diff(row);
    dx >= 1 || dy >= 1
}

pub fn reconstruct_selection_text(
    model: &ResolvedSelectionModel,
    drag: &ActiveTextDrag,
) -> Option<String> {
    reconstruct_selection_text_with_boundaries(model, &ResolvedSelectionBoundaries::default(), drag)
}

pub(crate) fn reconstruct_selection_text_with_boundaries(
    model: &ResolvedSelectionModel,
    boundaries: &ResolvedSelectionBoundaries,
    drag: &ActiveTextDrag,
) -> Option<String> {
    let range = model.range(drag.anchor.entry_idx, drag.anchor.range_id)?;
    let start_bl = min(drag.anchor.block_line_idx, drag.head.block_line_idx);
    let end_bl = max(drag.anchor.block_line_idx, drag.head.block_line_idx);
    let mut out = String::new();
    let mut first = true;

    for line in &range.lines {
        if line.block_line_idx < start_bl || line.block_line_idx > end_bl {
            continue;
        }
        if !first {
            out.push_str(line.joiner_to_previous.as_deref().unwrap_or("\n"));
        }
        first = false;
        let slice =
            selection_slice_for_line_by_block_idx(drag, line, boundaries.boundary_for_line(line))?;
        out.push_str(&slice);
    }

    if out.is_empty() && !first {
        return Some(out);
    }
    if first {
        // No visible lines found in the current model for the drag range.
        // This can happen when both anchor and head have scrolled off-screen.
        return None;
    }

    Some(out)
}

pub fn block_drag_threshold_exceeded(pending: &PendingBlockDrag, col: u16, row: u16) -> bool {
    let dx = pending.start_col.abs_diff(col);
    let dy = pending.start_row.abs_diff(row);
    dx >= 1 || dy >= 1
}

pub fn render_block_drag_overlay(
    model: &ResolvedSelectionModel,
    drag: &ActiveBlockDrag,
    buf: &mut Buffer,
) {
    let theme = Theme::current();
    let start = min(drag.anchor_entry_idx, drag.head_entry_idx);
    let end = max(drag.anchor_entry_idx, drag.head_entry_idx);

    for block in &model.visible_blocks {
        if block.entry_idx >= start && block.entry_idx <= end {
            for y in block.area.y..block.area.y.saturating_add(block.area.height) {
                for x in block.area.x..block.area.x.saturating_add(block.area.width) {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        apply_selection_highlight(&theme, cell);
                    }
                }
            }
        }
    }
}

pub fn render_active_selection_overlay(
    model: &ResolvedSelectionModel,
    drag: &ActiveTextDrag,
    table: Option<&TableGeometry>,
    buf: &mut Buffer,
) {
    render_selection_overlay_impl(
        model,
        drag.anchor.entry_idx,
        drag.anchor.range_id,
        SelectionEndpoint {
            block_line_idx: drag.anchor.block_line_idx,
            col_within_range: drag.anchor.col_within_range,
        },
        SelectionEndpoint {
            block_line_idx: drag.head.block_line_idx,
            col_within_range: drag.head.col_within_range,
        },
        drag.kind,
        table,
        buf,
    );
}

/// Render a persistent text selection overlay (after mouse-up).
///
/// Unlike [`render_active_selection_overlay`] (which reads from [`ActiveTextDrag`]),
/// this reads from [`PersistentTextSelection`] and maps stable `block_line_idx`
/// coordinates back to screen positions using the current frame's
/// [`ResolvedSelectionModel`].
pub fn render_persistent_selection_overlay(
    model: &ResolvedSelectionModel,
    selection: &PersistentTextSelection,
    table: Option<&TableGeometry>,
    buf: &mut Buffer,
) {
    render_selection_overlay_impl(
        model,
        selection.entry_idx,
        selection.range_id,
        selection.anchor,
        selection.head,
        selection.kind,
        table,
        buf,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_selection_overlay_impl(
    model: &ResolvedSelectionModel,
    entry_idx: usize,
    range_id: u16,
    anchor: SelectionEndpoint,
    head: SelectionEndpoint,
    kind: SelectionKind,
    table: Option<&TableGeometry>,
    buf: &mut Buffer,
) {
    let Some(range) = model.range(entry_idx, range_id) else {
        return;
    };
    // Table kinds need their geometry; without it (btw overlay, stale
    // side-car) paint nothing rather than a misleading linear sweep.
    let table = match kind {
        SelectionKind::Linear => None,
        SelectionKind::TableCell | SelectionKind::TableGrid { .. } => match table {
            Some(geom) => Some(geom),
            None => return,
        },
    };

    let theme = Theme::current();
    let start_bl = min(anchor.block_line_idx, head.block_line_idx);
    let end_bl = max(anchor.block_line_idx, head.block_line_idx);

    for line in &range.lines {
        let col_ranges: Vec<Range<u16>> = if let Some(geom) = table {
            // Snapped to grapheme boundaries and clipped to content so the highlight matches the copy.
            table_selected_cols_for_line(geom, kind, anchor, head, line.block_line_idx)
                .into_iter()
                .map(|cols| {
                    let display = display_text_for_cols(&line.text);
                    let snapped = endpoint_start_col(display.as_ref(), cols.start)
                        ..endpoint_end_col(display.as_ref(), cols.end.saturating_sub(1));
                    clip_cols_to_content(display.as_ref(), snapped)
                })
                .collect()
        } else {
            if line.block_line_idx < start_bl || line.block_line_idx > end_bl {
                continue;
            }
            match selected_cols_for_endpoints(
                anchor.block_line_idx,
                anchor.col_within_range,
                head.block_line_idx,
                head.col_within_range,
                line,
            ) {
                Some(cols) => vec![cols],
                None => continue,
            }
        };
        for cols in col_ranges {
            for col in cols.start..cols.end {
                let screen_x = line
                    .screen_x
                    .saturating_add(line.selectable_cols.start)
                    .saturating_add(col);
                if let Some(cell) = buf.cell_mut((screen_x, line.screen_y)) {
                    apply_selection_highlight(&theme, cell);
                }
            }
        }
    }
}

/// A table-cell selection's endpoints clamped into the cell's box (fragment
/// lines x column band), normalized so start <= end. The head can sit
/// outside the cell whenever the latch kept the selection there.
fn table_cell_span(
    geom: &TableGeometry,
    cell: crate::scrollback::table_geometry::CellRef,
    anchor: SelectionEndpoint,
    head: SelectionEndpoint,
) -> ((usize, u16), (usize, u16)) {
    let lines = geom.row_lines(cell.row);
    let band = geom.band(cell.col);
    let clamp = |ep: SelectionEndpoint| {
        (
            ep.block_line_idx
                .clamp(lines.start, lines.end.saturating_sub(1)),
            ep.col_within_range
                .clamp(band.start, band.end.saturating_sub(1)),
        )
    };
    let a = clamp(anchor);
    let h = clamp(head);
    if a <= h { (a, h) } else { (h, a) }
}

/// Clip a painted column range to the non-whitespace content it covers,
/// mirroring the copy (which trims fragments and skips blank ones).
/// Whitespace between content columns stays inside the range.
fn clip_cols_to_content(text: &str, cols: Range<u16>) -> Range<u16> {
    let mut start: Option<u16> = None;
    let mut end = cols.start;
    let mut col = 0u16;
    for grapheme in text.graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme) as u16;
        if width == 0 {
            continue;
        }
        let next = col.saturating_add(width);
        if col >= cols.end {
            break;
        }
        if next > cols.start && !grapheme.chars().all(char::is_whitespace) {
            start.get_or_insert(col.max(cols.start));
            end = next.min(cols.end);
        }
        col = next;
    }
    match start {
        Some(start) => start..end,
        None => cols.start..cols.start,
    }
}

/// Selected column ranges on one line of a table-shaped selection:
/// `TableCell` at most one band-clamped range, `TableGrid` one band per
/// selected column. Border glyphs are never included.
fn table_selected_cols_for_line(
    geom: &TableGeometry,
    kind: SelectionKind,
    anchor: SelectionEndpoint,
    head: SelectionEndpoint,
    block_line_idx: usize,
) -> Vec<Range<u16>> {
    // Lines outside the grid can never paint; keep them allocation-free.
    if !geom.line_range().contains(&block_line_idx) {
        return Vec::new();
    }
    match kind {
        SelectionKind::Linear => Vec::new(),
        SelectionKind::TableCell => {
            let Some(cell) = geom.cell_at(anchor.block_line_idx, anchor.col_within_range) else {
                return Vec::new();
            };
            let band = geom.band(cell.col);
            let ((l0, c0), (l1, c1)) = table_cell_span(geom, cell, anchor, head);
            if block_line_idx < l0 || block_line_idx > l1 {
                return Vec::new();
            }
            let start = if block_line_idx == l0 { c0 } else { band.start };
            let end = if block_line_idx == l1 {
                c1.saturating_add(1).min(band.end)
            } else {
                band.end
            };
            if start >= end {
                return Vec::new();
            }
            // Named binding: `vec![start..end]` trips single_range_in_vec_init.
            let band_cols: Range<u16> = start..end;
            vec![band_cols]
        }
        SelectionKind::TableGrid { anchor: a, head: h } => {
            let Some(row) = geom.row_of_line(block_line_idx) else {
                return Vec::new();
            };
            let (r0, r1) = (a.row.min(h.row), a.row.max(h.row));
            let (c0, c1) = (a.col.min(h.col), a.col.max(h.col));
            if row < r0 || row > r1 {
                return Vec::new();
            }
            (c0..=c1).map(|c| geom.band(c)).collect()
        }
    }
}

/// Resolve a drag's [`SelectionKind`], with hysteresis: the head is
/// latched from the cell the drag already holds, so only another cell's
/// content changes the mode. Grid-line anchors stay `Linear`.
pub fn resolve_table_drag_kind(
    geom: Option<&TableGeometry>,
    anchor: &RangeHit,
    head: &RangeHit,
    prev: SelectionKind,
) -> SelectionKind {
    let Some(geom) = geom else {
        return SelectionKind::Linear;
    };
    let Some(a) = geom.cell_at(anchor.block_line_idx, anchor.col_within_range) else {
        return SelectionKind::Linear;
    };
    let held = match prev {
        SelectionKind::TableGrid { head, .. } => head,
        SelectionKind::Linear | SelectionKind::TableCell => a,
    };
    let h = geom.latched_cell_at(held, head.block_line_idx, head.col_within_range);
    if a == h {
        SelectionKind::TableCell
    } else {
        SelectionKind::TableGrid { anchor: a, head: h }
    }
}

/// Copied text for a table-shaped selection: the band-clamped span for
/// `TableCell`, whole cells as TSV for `TableGrid`. `None` (= fall back to
/// linear) for `Linear` drags or an anchor that no longer resolves.
pub fn reconstruct_table_selection_text(
    geom: &TableGeometry,
    drag: &ActiveTextDrag,
    text_at: impl Fn(usize) -> Option<String>,
) -> Option<String> {
    let anchor = SelectionEndpoint {
        block_line_idx: drag.anchor.block_line_idx,
        col_within_range: drag.anchor.col_within_range,
    };
    let head = SelectionEndpoint {
        block_line_idx: drag.head.block_line_idx,
        col_within_range: drag.head.col_within_range,
    };
    match drag.kind {
        SelectionKind::Linear => None,
        SelectionKind::TableCell => {
            let cell = geom.cell_at(anchor.block_line_idx, anchor.col_within_range)?;
            let band = geom.band(cell.col);
            let ((l0, c0), (l1, c1)) = table_cell_span(geom, cell, anchor, head);
            let mut out = String::new();
            for line in l0..=l1 {
                let text = text_at(line)?;
                let start = if line == l0 {
                    endpoint_start_col(&text, c0).max(band.start)
                } else {
                    band.start
                };
                let end = if line == l1 {
                    endpoint_end_col(&text, c1).min(band.end)
                } else {
                    band.end
                };
                let slice = crate::scrollback::types::slice_display_cols(&text, start, end);
                let fragment = slice.trim();
                if fragment.is_empty() {
                    continue;
                }
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(fragment);
            }
            Some(out)
        }
        SelectionKind::TableGrid { anchor, head } => Some(geom.grid_tsv(anchor, head, text_at)),
    }
}

/// Uniform selection band in the classic inverted colors (`bg_base` on
/// `text_primary`): styled spans (inline code, links, syntax highlighting)
/// join the band instead of inverting to their own colors.
/// Terminal-native / colorless themes fall back to reverse video.
pub(crate) fn apply_selection_highlight(theme: &Theme, cell: &mut ratatui::buffer::Cell) {
    let band = theme.text_primary;
    if band == Color::Reset || theme.bg_base == Color::Reset {
        cell.modifier.insert(Modifier::REVERSED);
        return;
    }
    // A search-match highlight painted earlier in the frame sets REVERSED;
    // left in place it would swap the band right back out.
    cell.modifier.remove(Modifier::REVERSED);
    cell.set_fg(theme.bg_base);
    cell.set_bg(band);
}

fn selection_slice_for_line_by_block_idx(
    drag: &ActiveTextDrag,
    line: &ResolvedSelectableLine,
    boundary: Option<&SelectionBoundary>,
) -> Option<String> {
    let cols = selected_cols_for_line_by_block_idx(drag, line)?;
    let visible_width = line
        .selectable_cols
        .end
        .saturating_sub(line.selectable_cols.start);
    // Columns are visual cells of the painted row. Slice the painted region
    // (the exact drawn cells) back to logical order for the clipboard so a
    // trailing-trimmed or overridden `text` can't drift from the cells the user
    // dragged. Identity when reordering is off (falls back to `text`).
    let selected = if crate::render::bidi::is_enabled() {
        match line.painted_region.as_deref() {
            // Override rows (tool headers) paint a display that differs from the
            // stored copy text; its columns can't index the painted cells, so
            // copy the whole override on overlap.
            Some(region) if line.text.trim_end() != region.trim_end() => line.text.clone(),
            Some(region) => {
                let sliced = slice_text_cols(region, cols.clone());
                // Strip render-only trailing padding only for a row whose copy
                // `text` is the trimmed form of the painted region (i.e. a
                // `Selectable::All` row) once the selection reaches the row end,
                // matching the multi-row path. A `Selectable::Spans` row with
                // legitimate trailing spaces keeps them.
                if cols.end == visible_width && line.text == region.trim_end() {
                    sliced.trim_end().to_string()
                } else {
                    sliced
                }
            }
            None => slice_text_cols(&line.text, cols.clone()),
        }
    } else {
        slice_text_cols(&line.text, cols.clone())
    };
    Some(apply_selection_boundary(
        selected,
        boundary,
        cols.start == 0,
        cols.end == visible_width,
    ))
}

pub(crate) fn apply_selection_boundary(
    selected: String,
    boundary: Option<&SelectionBoundary>,
    include_prefix: bool,
    include_suffix: bool,
) -> String {
    let Some(boundary) = boundary else {
        return selected;
    };
    boundary.apply(selected, include_prefix, include_suffix)
}

/// Compute the selected column range for a given line based on anchor/head endpoints.
///
/// Shared implementation used by both active drag and persistent selection overlays. Endpoints snap to grapheme
/// boundaries: starts floor onto the grapheme under the anchor, ends advance past the grapheme under the head.
/// - Single-line: `floor(min(anchor_col, head_col))..past(max(anchor_col, head_col))`
/// - Multi-line first: `floor(start_col)..width`
/// - Multi-line last: `0..past(end_col)`
/// - Multi-line middle: `0..width` (full line)
///
/// Returns `None` if the line falls outside the anchor/head range.
fn selected_cols_for_endpoints(
    anchor_block_line: usize,
    anchor_col: u16,
    head_block_line: usize,
    head_col: u16,
    line: &ResolvedSelectableLine,
) -> Option<Range<u16>> {
    let width = line
        .selectable_cols
        .end
        .saturating_sub(line.selectable_cols.start);
    let bl = line.block_line_idx;
    let start_bl = min(anchor_block_line, head_block_line);
    let end_bl = max(anchor_block_line, head_block_line);
    let anchor_is_start = anchor_block_line <= head_block_line;

    if bl < start_bl || bl > end_bl {
        return None;
    }

    // Snap on the painted row so ranges line up with the drawn cells. Prefer
    // the painted region (untrimmed, exact painted cells) over `text`, which is
    // trailing-trimmed: on an RTL row those spaces paint at the opposite edge,
    // so a narrower `text` would snap the highlight band off the drawn cells.
    let snap_src = if crate::render::bidi::is_enabled() {
        line.painted_region.as_deref().unwrap_or(line.text.as_str())
    } else {
        line.text.as_str()
    };
    let display = display_text_for_cols(snap_src);
    if start_bl == end_bl {
        let start = endpoint_start_col(display.as_ref(), min(anchor_col, head_col));
        return Some(
            start.min(width)
                ..endpoint_end_col(display.as_ref(), max(anchor_col, head_col)).min(width),
        );
    }

    if bl == start_bl {
        let start = if anchor_is_start {
            anchor_col
        } else {
            head_col
        };
        return Some(endpoint_start_col(display.as_ref(), start).min(width)..width);
    }

    if bl == end_bl {
        let end = if anchor_is_start {
            head_col
        } else {
            anchor_col
        };
        return Some(0..endpoint_end_col(display.as_ref(), end).min(width));
    }

    Some(0..width)
}

/// Exclusive end for a selection that includes the cell at `col`: one column past the grapheme there.
/// On a padded or empty line `col` can sit past the text, so the result is never less than `col + 1`.
fn endpoint_end_col(text: &str, col: u16) -> u16 {
    crate::scrollback::types::col_past_grapheme(text, col).max(col.saturating_add(1))
}

/// Start of a selection whose first cell is `col`: the first column of the grapheme there, so a wide character is selected whole.
/// When `col` is past the text, it is returned unchanged.
fn endpoint_start_col(text: &str, col: u16) -> u16 {
    crate::scrollback::types::grapheme_cells_at(text, col).map_or(col, |cells| cells.start)
}

fn selected_cols_for_line_by_block_idx(
    drag: &ActiveTextDrag,
    line: &ResolvedSelectableLine,
) -> Option<Range<u16>> {
    selected_cols_for_endpoints(
        drag.anchor.block_line_idx,
        drag.anchor.col_within_range,
        drag.head.block_line_idx,
        drag.head.col_within_range,
        line,
    )
}

/// Reconstruct the full selected text from the block's complete output lines.
///
/// Unlike [`reconstruct_selection_text`], which only has access to lines
/// currently visible on screen, this function reads from the block's full
/// output. This ensures copy produces the complete selection even when the
/// anchor or head has scrolled off-screen.
pub fn reconstruct_full_selection_text(
    block_lines: &[crate::scrollback::types::BlockLine],
    drag: &ActiveTextDrag,
) -> Option<String> {
    reconstruct_full_selection_text_with_boundaries(
        block_lines,
        &crate::scrollback::types::SelectionBoundaries::default(),
        drag,
    )
}

pub(crate) fn reconstruct_full_selection_text_with_boundaries(
    block_lines: &[crate::scrollback::types::BlockLine],
    boundaries: &crate::scrollback::types::SelectionBoundaries,
    drag: &ActiveTextDrag,
) -> Option<String> {
    use crate::scrollback::types::{
        Selectable, painted_selectable_region, selectable_cols, slice_display_cols,
    };

    let start_bl = min(drag.anchor.block_line_idx, drag.head.block_line_idx);
    let end_bl = max(drag.anchor.block_line_idx, drag.head.block_line_idx);
    let anchor_is_start = drag.anchor.block_line_idx <= drag.head.block_line_idx;

    let mut out = String::new();
    let mut first = true;

    for (idx, line) in block_lines.iter().enumerate() {
        if idx < start_bl || idx > end_bl {
            continue;
        }
        // Only include lines that belong to the same selection range.
        if line.selection_range != Some(drag.anchor.range_id) {
            continue;
        }

        let Some(cols) = selectable_cols(&line.content, &line.selectable) else {
            continue;
        };
        let width = cols.end.saturating_sub(cols.start);

        // Map against the exact text painted in the selectable columns, in
        // logical order: this is the string `set_line_safe_bidi` reorders, so
        // the visual drag columns line up 1:1 with the drawn cells. Shared with
        // the resolved-model overlay path via `painted_region`.
        let region = painted_selectable_region(line);

        // Tool-header rows paint a truncated/reworded display but copy a stored
        // string. When that override diverges from what is painted (ignoring
        // render-only trailing padding), its columns can't be reorder-mapped
        // onto it. The same trim-tolerant predicate the overlay path uses.
        let override_text = line
            .selection_text
            .as_deref()
            .filter(|ov| ov.trim_end() != region.trim_end());

        // Snap endpoints on the painted region (matches drawn cells).
        // `col_range.end == width` decides suffix re-attachment below.
        let display = display_text_for_cols(&region);
        let col_range = if start_bl == end_bl {
            let s = min(drag.anchor.col_within_range, drag.head.col_within_range);
            let e = max(drag.anchor.col_within_range, drag.head.col_within_range);
            endpoint_start_col(display.as_ref(), s).min(width)
                ..endpoint_end_col(display.as_ref(), e).min(width)
        } else if idx == start_bl {
            let s = if anchor_is_start {
                drag.anchor.col_within_range
            } else {
                drag.head.col_within_range
            };
            endpoint_start_col(display.as_ref(), s).min(width)..width
        } else if idx == end_bl {
            let e = if anchor_is_start {
                drag.head.col_within_range
            } else {
                drag.anchor.col_within_range
            };
            0..endpoint_end_col(display.as_ref(), e).min(width)
        } else {
            0..width
        };

        if !first {
            out.push_str(line.joiner.as_deref().unwrap_or("\n"));
        }
        first = false;
        let selected = match override_text {
            // Painted display is reordered and differs from the stored copy
            // text, so visual columns don't index it: copy it whole on overlap.
            Some(ov) if crate::render::bidi::is_enabled() => ov.to_string(),
            // LTR: column-precise partial copy of the stored string.
            Some(ov) => slice_display_cols(ov, col_range.start, col_range.end),
            None => {
                let sliced = slice_text_cols(&region, col_range.clone());
                // Strip render-only trailing padding once the selection reaches
                // the row end (matches `derive_selection_text` for `All` rows).
                if matches!(line.selectable, Selectable::All) && col_range.end == width {
                    sliced.trim_end().to_string()
                } else {
                    sliced
                }
            }
        };
        out.push_str(&apply_selection_boundary(
            selected,
            boundaries.get(idx).map(Arc::as_ref),
            col_range.start == 0,
            col_range.end == width,
        ));
    }

    if first {
        return None;
    }
    Some(out)
}

/// Visual display order for column snap when rtl_bidi paints visual cells.
fn display_text_for_cols(text: &str) -> std::borrow::Cow<'_, str> {
    crate::render::bidi::visual_text(text)
}

/// Slice by visual columns; returns logical-order text for the clipboard.
fn slice_text_cols(text: &str, cols: Range<u16>) -> String {
    if crate::render::bidi::is_enabled() && crate::render::bidi::needs_bidi(text) {
        return crate::render::bidi::logical_slice_for_visual_cols(
            text,
            cols.start as usize,
            cols.end as usize,
        );
    }
    crate::scrollback::types::slice_display_cols(text, cols.start, cols.end)
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

// ---------------------------------------------------------------------------
// Word / URL boundary detection (for double-click selection)
// ---------------------------------------------------------------------------

/// All printable ASCII punctuation except underscore, matching tmux's
/// `word-separators` default from `options-table.c`.
pub const DEFAULT_WORD_SEPARATORS: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~";

/// Load `[ui] word_separators` from config, falling back to [`DEFAULT_WORD_SEPARATORS`].
pub fn configured_word_separators() -> &'static str {
    static CACHED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHED.get_or_init(|| {
        let root = match echo_agent_runtime::config::load_effective_config() {
            Ok(r) => r,
            Err(_) => return DEFAULT_WORD_SEPARATORS.to_owned(),
        };
        root.get("ui")
            .and_then(|ui| ui.get("word_separators"))
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_WORD_SEPARATORS)
            .to_owned()
    })
}

/// Three-class partition for word boundary detection (whitespace / separator / word).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Whitespace,
    Separator,
}

fn classify_grapheme(grapheme: &str, separators: &str) -> CharClass {
    match grapheme.chars().next() {
        Some(c) if c.is_whitespace() => CharClass::Whitespace,
        Some(c) if separators.contains(c) => CharClass::Separator,
        _ => CharClass::Word,
    }
}

/// Find word boundaries around a display column using tmux-style three-class
/// grouping. Returns the range in display columns.
pub fn word_boundaries_at_col(text: &str, col: u16, separators: &str) -> Range<u16> {
    let mut segments: Vec<(u16, u16, CharClass)> = Vec::new();
    let mut current_col: u16 = 0;

    for grapheme in text.graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme) as u16;
        if width == 0 {
            continue;
        }
        let next_col = current_col.saturating_add(width);
        segments.push((
            current_col,
            next_col,
            classify_grapheme(grapheme, separators),
        ));
        current_col = next_col;
    }

    if segments.is_empty() {
        return 0..0;
    }

    // If col is past the end, clamp to the last segment.
    let target_idx = segments
        .iter()
        .position(|(start, end, _)| col >= *start && col < *end)
        .unwrap_or(segments.len() - 1);

    let target_class = segments[target_idx].2;

    let mut left = target_idx;
    while left > 0 && segments[left - 1].2 == target_class {
        left -= 1;
    }

    let mut right = target_idx;
    while right + 1 < segments.len() && segments[right + 1].2 == target_class {
        right += 1;
    }

    segments[left].0..segments[right].1
}

/// Pre-compiled regex for URL detection, cached for the process lifetime.
static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:https?|ftp|file)://[^\s\x{00}-\x{1f}]+").expect("URL regex must compile")
});

/// Punctuation that may appear as trailing prose punctuation after a URL.
const TRAILING_URL_PUNCT: &[char] = &['.', ',', ':', ';', '!', '?', ')', ']', '}', '>', '"', '\''];

/// Strip trailing punctuation from a URL match, respecting balanced brackets.
///
/// Closing brackets are only stripped when unbalanced (no matching opener
/// inside the URL body), handling prose like `(see https://example.com)`.
fn strip_trailing_url_punctuation(url: &str) -> &str {
    let mut end = url.len();

    loop {
        let last = match url[..end].chars().next_back() {
            Some(c) if TRAILING_URL_PUNCT.contains(&c) => c,
            _ => break,
        };

        if let Some(open) = match last {
            ')' => Some('('),
            ']' => Some('['),
            '}' => Some('{'),
            '>' => Some('<'),
            _ => None,
        } {
            let opens = url[..end].chars().filter(|&c| c == open).count();
            let closes = url[..end].chars().filter(|&c| c == last).count();
            if opens >= closes {
                break;
            }
        }

        end -= last.len_utf8();
    }

    &url[..end]
}

/// Compute the display-column width of a string via grapheme clusters.
fn display_width(text: &str) -> u16 {
    text.graphemes(true).fold(0u16, |acc, g| {
        acc.saturating_add(UnicodeWidthStr::width(g) as u16)
    })
}

/// Try to find a URL that spans the given display column in `text`.
///
/// Scans `text` for URLs matching common schemes (`https?://`, `ftp://`,
/// `file://`) and returns the display-column range of the URL containing
/// `col`, or `None` if `col` is not within any URL.
///
/// Trailing punctuation (`.`, `,`, `)`, etc.) is stripped when unbalanced,
/// handling prose contexts like `"see https://example.com."`.
pub fn url_range_at_col(text: &str, col: u16) -> Option<Range<u16>> {
    for m in URL_RE.find_iter(text) {
        let col_start = display_width(&text[..m.start()]);
        let url = strip_trailing_url_punctuation(m.as_str());

        // Skip degenerate URLs reduced to just the scheme (e.g. "https://").
        if url.find("://").is_some_and(|i| url[i + 3..].is_empty()) {
            continue;
        }

        let col_end = col_start.saturating_add(display_width(url));

        if col >= col_start && col < col_end {
            return Some(col_start..col_end);
        }
    }

    None
}

/// Wrap-aware word or URL selection. `head` is inclusive; `text` is the joined
/// fragment text and is never empty. `None` means nothing selectable.
#[derive(Debug, PartialEq, Eq)]
pub struct SemanticSelection {
    pub anchor: SelectionEndpoint,
    pub head: SelectionEndpoint,
    pub text: String,
}

struct ConcatFragment {
    block_line_idx: usize,
    text_start: u16,
    text_end: u16,
}

/// Snap when an inclusive concat column lands inside a joiner: `Forward` → next
/// fragment col 0, `Backward` → previous fragment's last col.
#[derive(Clone, Copy)]
enum JoinerColSnap {
    Forward,
    Backward,
}

/// Select the wrap-group word or URL at `hit`.
///
/// The wrap group is the maximal run of `Some` joiners inside one range.
/// `head` is inclusive; `text` is the joined fragment text and is never empty.
/// `None` means nothing selectable.
///
/// `joiner_to_previous: None` is a hard source-line break and is not crossed,
/// even though `'\n'` is not in `word_separators`.
#[must_use]
pub fn semantic_selection_at(
    model: &ResolvedSelectionModel,
    hit: &RangeHit,
    separators: &str,
) -> Option<SemanticSelection> {
    let range = model.range(hit.entry_idx, hit.range_id)?;
    let lines = &range.lines;
    let hit_pos = lines
        .iter()
        .position(|line| line.block_line_idx == hit.block_line_idx)?;

    let mut lo = hit_pos;
    while lo > 0 && lines[lo].joiner_to_previous.is_some() {
        lo -= 1;
    }
    let mut hi = hit_pos;
    while hi + 1 < lines.len() && lines[hi + 1].joiner_to_previous.is_some() {
        hi += 1;
    }

    // Single-row case (and, under reordering, the wrap-group case too — see
    // below): the hit column is a visual cell of the painted row and
    // `word_or_url_slice` maps it against that same row.
    if lo == hi || (crate::render::bidi::is_enabled() && wrap_group_needs_bidi(lines, lo, hi)) {
        let line = &lines[if lo == hi { lo } else { hit_pos }];
        // The hit column is a visual cell of the painted row, so resolve the
        // word against the painted region (identity/`text` when reordering off).
        let src = if crate::render::bidi::is_enabled() {
            line.painted_region.as_deref().unwrap_or(line.text.as_str())
        } else {
            line.text.as_str()
        };
        let (sel, text) = word_or_url_slice(src, hit.col_within_range, separators)?;
        return Some(SemanticSelection {
            anchor: SelectionEndpoint {
                block_line_idx: line.block_line_idx,
                col_within_range: sel.start,
            },
            head: SelectionEndpoint {
                block_line_idx: line.block_line_idx,
                col_within_range: sel.end.saturating_sub(1),
            },
            text,
        });
    }

    let mut concat = String::new();
    let mut text_byte_ranges = Vec::with_capacity(hi - lo + 1);
    for (offset, line) in lines[lo..=hi].iter().enumerate() {
        if offset > 0
            && let Some(joiner) = line.joiner_to_previous.as_deref()
        {
            concat.push_str(joiner);
        }
        let text_byte_start = concat.len();
        concat.push_str(&line.text);
        text_byte_ranges.push((line.block_line_idx, text_byte_start, concat.len()));
    }

    // Measure fragment columns on the finished concat so a grapheme split
    // across a wrap boundary is not counted twice.
    let mut fragments = Vec::with_capacity(text_byte_ranges.len());
    let mut hit_concat_col = None;
    for (block_line_idx, text_byte_start, text_byte_end) in text_byte_ranges {
        let text_start = display_width(&concat[..text_byte_start]);
        let text_end = display_width(&concat[..text_byte_end]);
        fragments.push(ConcatFragment {
            block_line_idx,
            text_start,
            text_end,
        });
        if block_line_idx == hit.block_line_idx {
            hit_concat_col = Some(map_local_hit_to_concat_col(
                &concat[text_byte_start..text_byte_end],
                text_start,
                text_end,
                hit.col_within_range,
            ));
        }
    }

    let (sel, text) = word_or_url_slice(&concat, hit_concat_col?, separators)?;
    let last_col = sel.end.saturating_sub(1);
    let anchor = map_inclusive_concat_col(&fragments, sel.start, JoinerColSnap::Forward)?;
    let head = map_inclusive_concat_col(&fragments, last_col, JoinerColSnap::Backward)?;
    if (anchor.block_line_idx, anchor.col_within_range)
        > (head.block_line_idx, head.col_within_range)
    {
        return None;
    }

    Some(SemanticSelection { anchor, head, text })
}

/// Map a fragment-local click column into concat display columns.
///
/// When a wrap splits a grapheme, the continuation still has local width but
/// adds little or none in concat. Those absorbed columns snap to the last
/// concat column of the split cluster instead of drifting into later text.
fn map_local_hit_to_concat_col(
    local_text: &str,
    text_start: u16,
    text_end: u16,
    col_within_range: u16,
) -> u16 {
    let local_width = display_width(local_text);
    let concat_width = text_end.saturating_sub(text_start);
    let absorbed = local_width.saturating_sub(concat_width);
    if col_within_range < absorbed {
        text_start.saturating_sub(1)
    } else {
        text_start.saturating_add(col_within_range - absorbed)
    }
}

/// Whether any row in the wrap group `[lo, hi]` needs bidi reordering. Rows are
/// painted per row, so a whole-group concat would reorder differently than the
/// screen; when reordering applies we resolve the word on the hit row alone.
fn wrap_group_needs_bidi(lines: &[ResolvedSelectableLine], lo: usize, hi: usize) -> bool {
    lines[lo..=hi]
        .iter()
        .any(|l| crate::render::bidi::needs_bidi(&l.text))
}

fn word_or_url_slice(text: &str, col: u16, separators: &str) -> Option<(Range<u16>, String)> {
    // Word boundaries follow visual cells when RTL is present; clipboard is logical.
    let display = display_text_for_cols(text);
    let range = url_range_at_col(display.as_ref(), col)
        .unwrap_or_else(|| word_boundaries_at_col(display.as_ref(), col, separators));
    if range.is_empty() {
        return None;
    }
    let sliced = if crate::render::bidi::needs_bidi(text) {
        slice_text_cols(text, range.clone())
    } else {
        crate::scrollback::types::slice_display_cols(text, range.start, range.end)
    };
    if sliced.is_empty() {
        return None;
    }
    Some((range, sliced))
}

fn fragment_last_col(frag: &ConcatFragment) -> Option<SelectionEndpoint> {
    let width = frag.text_end.saturating_sub(frag.text_start);
    (width > 0).then_some(SelectionEndpoint {
        block_line_idx: frag.block_line_idx,
        col_within_range: width.saturating_sub(1),
    })
}

fn map_inclusive_concat_col(
    fragments: &[ConcatFragment],
    col: u16,
    snap: JoinerColSnap,
) -> Option<SelectionEndpoint> {
    for (i, frag) in fragments.iter().enumerate() {
        if col < frag.text_start {
            return match snap {
                JoinerColSnap::Forward => Some(SelectionEndpoint {
                    block_line_idx: frag.block_line_idx,
                    col_within_range: 0,
                }),
                JoinerColSnap::Backward => fragment_last_col(fragments.get(i.checked_sub(1)?)?),
            };
        }
        if col < frag.text_end {
            return Some(SelectionEndpoint {
                block_line_idx: frag.block_line_idx,
                col_within_range: col.saturating_sub(frag.text_start),
            });
        }
    }

    match snap {
        JoinerColSnap::Forward => None,
        JoinerColSnap::Backward => fragment_last_col(fragments.last()?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single_line_drag(block_line_idx: usize, width: u16) -> ActiveTextDrag {
        ActiveTextDrag {
            anchor: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx,
                col_within_range: 0,
            },
            head: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx,
                col_within_range: width - 1,
            },
            kind: SelectionKind::Linear,
            anchor_content_width: None,
        }
    }

    #[test]
    fn ordinary_copy_uses_only_selected_visible_columns() {
        for (text, joiner, expected) in [
            ("world", Some(" ".to_string()), "world"),
            ("world   ", None, "world"),
        ] {
            let mut model = ResolvedSelectionModel::default();
            let boundaries = ResolvedSelectionBoundaries::default();
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 1,
                screen_y: 0,
                screen_x: 0,
                selectable_cols: 0..5,
                text: text.to_string(),
                painted_region: None,
                joiner_to_previous: joiner.clone(),
            });
            assert!(boundaries.is_empty());

            assert_eq!(
                reconstruct_selection_text_with_boundaries(
                    &model,
                    &boundaries,
                    &single_line_drag(1, 5),
                ),
                Some(expected.to_string())
            );

            let block_line = crate::scrollback::types::BlockLine {
                content: ratatui::text::Line::from("world"),
                selectable: crate::scrollback::types::Selectable::All,
                selection_range: Some(0),
                selection_text: Some(text.to_string()),
                joiner,
                ..Default::default()
            };
            assert_eq!(
                reconstruct_full_selection_text(&[block_line], &single_line_drag(0, 5)),
                Some(expected.to_string())
            );
        }
    }

    // Serialize the process-global bidi latch; restore it on scope exit even if
    // `f` panics, so a failed assertion can't leak `rtl_bidi = true` into other
    // tests in the process. LTR cases need no reorder so only these RTL cases
    // need the guard. "خوب" avoids the lam-alef ligature so columns map 1:1.
    static BIDI_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct BidiLatchGuard(bool);
    impl Drop for BidiLatchGuard {
        fn drop(&mut self) {
            crate::render::bidi::set_enabled(self.0);
        }
    }
    fn with_rtl_bidi<R>(f: impl FnOnce() -> R) -> R {
        let _g = BIDI_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _latch = BidiLatchGuard(crate::render::bidi::is_enabled());
        crate::render::bidi::set_enabled(true);
        f()
    }

    /// A drag over painted (visual) RTL cells copies the logical text, not a
    /// mismapped substring of the trailing-trimmed derived text.
    #[test]
    fn rtl_drag_copies_logical_text_from_painted_cells() {
        use crate::scrollback::types::{BlockLine, Selectable};
        with_rtl_bidi(|| {
            let make = |s: &str| BlockLine {
                content: ratatui::text::Line::from(s.to_string()),
                selectable: Selectable::All,
                selection_range: Some(0),
                ..Default::default()
            };
            let drag_cols = |a: u16, h: u16| ActiveTextDrag {
                anchor: RangeHit {
                    entry_idx: 0,
                    range_id: 0,
                    block_line_idx: 0,
                    col_within_range: a,
                },
                head: RangeHit {
                    entry_idx: 0,
                    range_id: 0,
                    block_line_idx: 0,
                    col_within_range: h,
                },
                kind: SelectionKind::Linear,
                anchor_content_width: None,
            };

            // "Hi خوب" paints "Hi بوخ"; dragging the Persian cells (visual 3..=5)
            // copies logical "خوب".
            assert_eq!(
                reconstruct_full_selection_text(&[make("Hi خوب")], &drag_cols(3, 5)),
                Some("خوب".to_string()),
            );
            // A trailing-padded pure-RTL row copies logical order with the pad
            // trimmed.
            assert_eq!(
                reconstruct_full_selection_text(&[make("خوب  ")], &drag_cols(0, 4)),
                Some("خوب".to_string()),
            );
        });
    }

    /// A truncated RTL row paints `content + "…"` where the ellipsis is not
    /// selectable. Under an RTL base the ellipsis reorders to the visual left,
    /// shifting the selectable region right, so the stored hit box must be the
    /// region's visual span (past the ellipsis), not its logical columns.
    #[test]
    fn visual_selectable_cols_shifts_region_past_rtl_ellipsis() {
        use crate::scrollback::types::{
            BlockLine, Selectable, selectable_cols, visual_selectable_cols,
        };
        let line = BlockLine {
            content: ratatui::text::Line::from(vec![
                ratatui::text::Span::raw("خوب"),
                ratatui::text::Span::raw("…"),
            ]),
            selectable: Selectable::Spans(0..1),
            selection_range: Some(0),
            ..Default::default()
        };
        // Region is logical columns 0..3 (the "…" span is excluded).
        assert_eq!(selectable_cols(&line.content, &line.selectable), Some(0..3));
        // Reordering off: identity.
        assert_eq!(visual_selectable_cols(&line), Some(0..3));
        with_rtl_bidi(|| {
            // Painted "خوب…" -> "…بوخ"; the word occupies visual cells 1..4.
            assert_eq!(visual_selectable_cols(&line), Some(1..4));
        });
    }

    /// The overlay copy path (single-line, resolved-model) maps drag columns
    /// against the painted region, not the trailing-trimmed `text`. On a padded
    /// RTL row the pad paints at the left, so dragging the visible letters must
    /// copy the word — slicing the narrower `text` dropped letters.
    #[test]
    fn rtl_overlay_slice_maps_painted_region_not_trimmed_text() {
        with_rtl_bidi(|| {
            let mut model = ResolvedSelectionModel::default();
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 0,
                screen_y: 0,
                screen_x: 0,
                selectable_cols: 0..5,
                text: "خوب".to_string(),
                painted_region: Some("خوب  ".to_string()),
                joiner_to_previous: None,
            });
            // Painted "خوب  " reorders to "  بوخ"; the Persian letters occupy
            // visual cells 2..5. Dragging them copies the logical word.
            let drag = ActiveTextDrag {
                anchor: RangeHit {
                    entry_idx: 0,
                    range_id: 0,
                    block_line_idx: 0,
                    col_within_range: 2,
                },
                head: RangeHit {
                    entry_idx: 0,
                    range_id: 0,
                    block_line_idx: 0,
                    col_within_range: 4,
                },
                kind: SelectionKind::Linear,
                anchor_content_width: None,
            };
            assert_eq!(
                reconstruct_selection_text(&model, &drag),
                Some("خوب".to_string()),
            );
        });
    }

    /// The overlay copy path honors the tool-header override the same way the
    /// multi-row path does: a divergent stored string is copied whole under RTL.
    #[test]
    fn rtl_overlay_override_row_copies_whole_stored_text() {
        with_rtl_bidi(|| {
            let mut model = ResolvedSelectionModel::default();
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 0,
                screen_y: 0,
                screen_x: 0,
                selectable_cols: 0..3,
                text: "/path/خوب/file".to_string(),
                painted_region: Some("خوب".to_string()),
                joiner_to_previous: None,
            });
            let drag = ActiveTextDrag {
                anchor: RangeHit {
                    entry_idx: 0,
                    range_id: 0,
                    block_line_idx: 0,
                    col_within_range: 0,
                },
                head: RangeHit {
                    entry_idx: 0,
                    range_id: 0,
                    block_line_idx: 0,
                    col_within_range: 1,
                },
                kind: SelectionKind::Linear,
                anchor_content_width: None,
            };
            assert_eq!(
                reconstruct_selection_text(&model, &drag),
                Some("/path/خوب/file".to_string()),
            );
        });
    }

    /// An override row (tool header) paints a display that differs from its
    /// stored copy text. With `rtl_bidi` on, the visual drag columns index the
    /// painted display, not the override, so a partial drag copies the whole
    /// override rather than a mismapped, reversed substring of it.
    #[test]
    fn rtl_override_row_copies_whole_stored_text() {
        use crate::scrollback::types::{BlockLine, Selectable};
        with_rtl_bidi(|| {
            let line = BlockLine {
                content: ratatui::text::Line::from("خوب".to_string()),
                selectable: Selectable::All,
                selection_range: Some(0),
                selection_text: Some("/path/خوب/file".to_string()),
                ..Default::default()
            };
            let drag = ActiveTextDrag {
                anchor: RangeHit {
                    entry_idx: 0,
                    range_id: 0,
                    block_line_idx: 0,
                    col_within_range: 0,
                },
                head: RangeHit {
                    entry_idx: 0,
                    range_id: 0,
                    block_line_idx: 0,
                    col_within_range: 1,
                },
                kind: SelectionKind::Linear,
                anchor_content_width: None,
            };
            assert_eq!(
                reconstruct_full_selection_text(&[line], &drag),
                Some("/path/خوب/file".to_string()),
            );
        });
    }

    /// A line containing a ligature (lam-alef `لا`) still selects its last cell.
    /// `UnicodeWidthStr` reports `سلام` as 3 wide but ratatui paints 4 cells, so
    /// the selectable width must count painted cells or the last cell is dropped.
    #[test]
    fn ligature_line_selects_last_cell() {
        use crate::scrollback::types::{BlockLine, Selectable};
        let line = BlockLine {
            content: ratatui::text::Line::from("Hello سلام world".to_string()),
            selectable: Selectable::All,
            selection_range: Some(0),
            ..Default::default()
        };
        // 16 painted cells (one per grapheme); drag the whole row (0..=15).
        let drag = ActiveTextDrag {
            anchor: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 0,
                col_within_range: 0,
            },
            head: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 0,
                col_within_range: 15,
            },
            kind: SelectionKind::Linear,
            anchor_content_width: None,
        };
        assert_eq!(
            reconstruct_full_selection_text(&[line], &drag),
            Some("Hello سلام world".to_string()),
        );
    }

    /// Double-click on a painted RTL word copies it in logical order: the click
    /// lands on a visual cell ("خوب" paints as "بوخ") and the word maps back.
    #[test]
    fn rtl_double_click_selects_logical_word_from_painted_cells() {
        with_rtl_bidi(|| {
            let mut model = ResolvedSelectionModel::default();
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 0,
                screen_y: 0,
                screen_x: 0,
                selectable_cols: 0..3,
                text: "خوب".to_string(),
                painted_region: None,
                joiner_to_previous: None,
            });
            let hit = RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 0,
                col_within_range: 1,
            };
            let sel = semantic_selection_at(&model, &hit, DEFAULT_WORD_SEPARATORS)
                .expect("word selection");
            assert_eq!(sel.text, "خوب");
        });
    }

    #[test]
    fn edit_boundaries_apply_only_at_selected_path_edges() {
        use crate::scrollback::types::{
            BlockLine, Selectable, SelectionBoundaries, SelectionBoundary, SelectionBoundaryEntry,
        };

        let mut model = ResolvedSelectionModel::default();
        let mut resolved_boundaries = ResolvedSelectionBoundaries::default();
        for (idx, text, joiner, prefix, suffix) in [
            (1, "foo", None, "   ", ""),
            (2, "bar", Some(" ".to_string()), "", "   "),
        ] {
            let line = ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: idx,
                screen_y: idx as u16,
                screen_x: 0,
                selectable_cols: 0..3,
                text: text.to_string(),
                painted_region: None,
                joiner_to_previous: joiner,
            };
            resolved_boundaries.push(
                &line,
                Arc::new(SelectionBoundary::new(
                    prefix.to_string(),
                    suffix.to_string(),
                )),
            );
            model.push_line(line);
        }
        let drag = ActiveTextDrag {
            anchor: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 1,
                col_within_range: 0,
            },
            head: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 2,
                col_within_range: 2,
            },
            kind: SelectionKind::Linear,
            anchor_content_width: None,
        };
        assert_eq!(
            reconstruct_selection_text_with_boundaries(&model, &resolved_boundaries, &drag),
            Some("   foo bar   ".to_string())
        );

        let block_lines = vec![
            BlockLine::separator(ratatui::text::Line::from("Edit ")),
            BlockLine {
                content: ratatui::text::Line::from("foo"),
                selectable: crate::scrollback::types::Selectable::All,
                selection_range: Some(0),
                ..Default::default()
            },
            BlockLine {
                content: ratatui::text::Line::from("bar"),
                selectable: Selectable::All,
                selection_range: Some(0),
                joiner: Some(" ".to_string()),
                ..Default::default()
            },
        ];
        let boundaries = SelectionBoundaries::from_entries(vec![
            SelectionBoundaryEntry {
                line_index: 1,
                boundary: Arc::new(SelectionBoundary::new("   ".to_string(), String::new())),
            },
            SelectionBoundaryEntry {
                line_index: 2,
                boundary: Arc::new(SelectionBoundary::new(String::new(), "   ".to_string())),
            },
        ]);
        assert_eq!(
            reconstruct_full_selection_text_with_boundaries(&block_lines, &boundaries, &drag),
            Some("   foo bar   ".to_string())
        );
    }

    #[test]
    fn hit_test_selectable_range_returns_matching_hit() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(ResolvedSelectableLine {
            entry_idx: 1,
            range_id: 7,
            block_line_idx: 0,
            screen_y: 3,
            screen_x: 10,
            selectable_cols: 2..6,
            text: "body".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        });

        let hit = model.hit_test_selectable_range(13, 3).unwrap();
        assert_eq!(hit.entry_idx, 1);
        assert_eq!(hit.range_id, 7);
        assert_eq!(hit.block_line_idx, 0);
        assert_eq!(hit.col_within_range, 1);
    }

    #[test]
    fn hit_test_selectable_range_clamps_to_nearest_column_on_same_row() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(ResolvedSelectableLine {
            entry_idx: 1,
            range_id: 7,
            block_line_idx: 0,
            screen_y: 3,
            screen_x: 10,
            selectable_cols: 2..6,
            text: "body".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        });

        let left = model.hit_test_selectable_range(11, 3).unwrap();
        assert_eq!(left.col_within_range, 0);

        let right = model.hit_test_selectable_range(16, 3).unwrap();
        assert_eq!(right.col_within_range, 3);
    }

    // -----------------------------------------------------------------------
    // hit_test_nearest_in_range tests
    // -----------------------------------------------------------------------

    /// One selectable line for the nearest-in-range fixtures.
    fn nearest_line(
        range_id: u16,
        block_line_idx: usize,
        screen_y: u16,
        selectable_cols: Range<u16>,
    ) -> ResolvedSelectableLine {
        ResolvedSelectableLine {
            entry_idx: 0,
            range_id,
            block_line_idx,
            screen_y,
            screen_x: 4,
            selectable_cols,
            text: "text".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        }
    }

    fn range_anchor(range_id: u16, block_line_idx: usize) -> RangeHit {
        RangeHit {
            entry_idx: 0,
            range_id,
            block_line_idx,
            col_within_range: 0,
        }
    }

    /// Rows with a line of the anchor's range behave exactly like the
    /// range-restricted `hit_test_selectable_range`: exact col, and clamping
    /// to both ends.
    #[test]
    fn nearest_in_range_same_row_parity_with_selectable_range_hit() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(nearest_line(7, 0, 3, 2..6));
        let anchor = range_anchor(7, 0);

        for col in 0u16..14 {
            let nearest = model.hit_test_nearest_in_range(anchor, col, 3).unwrap();
            let direct = model.hit_test_selectable_range(col, 3).unwrap();
            assert_eq!(nearest, direct, "col {col} must match the same-row hit");
        }
    }

    /// A pointer on a dead row (no line of the range) snaps to the nearest
    /// row of the range instead of missing.
    #[test]
    fn nearest_in_range_snaps_across_gap_rows() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(nearest_line(0, 0, 2, 0..10));
        model.push_line(nearest_line(0, 2, 5, 0..10));
        let anchor = range_anchor(0, 0);

        // Row 3 is one row from the line at y=2 and two from y=5.
        let hit = model.hit_test_nearest_in_range(anchor, 6, 3).unwrap();
        assert_eq!(hit.block_line_idx, 0);
        assert_eq!(hit.col_within_range, 2);

        // Row 4 is nearer the line at y=5.
        let hit = model.hit_test_nearest_in_range(anchor, 6, 4).unwrap();
        assert_eq!(hit.block_line_idx, 2);
    }

    /// A pointer below the range's last line selects toward its end
    /// (native semantics) instead of freezing.
    #[test]
    fn nearest_in_range_snaps_from_below_last_line() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(nearest_line(0, 0, 2, 0..10));
        model.push_line(nearest_line(0, 1, 3, 0..8));
        let anchor = range_anchor(0, 0);

        let hit = model.hit_test_nearest_in_range(anchor, 30, 9).unwrap();
        assert_eq!(hit.block_line_idx, 1);
        // Pointer right of the line clamps to its last column.
        assert_eq!(hit.col_within_range, 7);

        let hit = model.hit_test_nearest_in_range(anchor, 1, 9).unwrap();
        assert_eq!(hit.block_line_idx, 1);
        // Pointer left of the line clamps to its first column.
        assert_eq!(hit.col_within_range, 0);
    }

    /// Lines of other ranges are never candidates, even on nearer rows —
    /// the head stays pinned to the anchor's range.
    #[test]
    fn nearest_in_range_ignores_other_ranges_on_nearer_rows() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(nearest_line(0, 0, 2, 0..10));
        model.push_line(nearest_line(1, 0, 6, 0..10));
        let anchor = range_anchor(0, 0);

        // Row 6 holds range 1's line; range 0's nearest is 4 rows away.
        let hit = model.hit_test_nearest_in_range(anchor, 3, 6).unwrap();
        assert_eq!(hit.range_id, 0);
        assert_eq!(hit.block_line_idx, 0);
    }

    /// A full (row, col) tie resolves to the line farther from the anchor,
    /// so a drag paused on a dead row keeps the selection extended across it.
    #[test]
    fn nearest_in_range_tie_prefers_line_farther_from_anchor() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(nearest_line(0, 0, 2, 0..10));
        model.push_line(nearest_line(0, 2, 4, 0..10));

        // Row 3 is equidistant; dragging down from line 0 crosses the gap.
        let hit = model
            .hit_test_nearest_in_range(range_anchor(0, 0), 5, 3)
            .unwrap();
        assert_eq!(hit.block_line_idx, 2);

        // Dragging up from line 2 crosses it the other way.
        let hit = model
            .hit_test_nearest_in_range(range_anchor(0, 2), 5, 3)
            .unwrap();
        assert_eq!(hit.block_line_idx, 0);
    }

    /// The range scrolled fully out of the model → no candidate; callers
    /// keep the previous head.
    #[test]
    fn nearest_in_range_misses_when_range_absent() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(nearest_line(1, 0, 2, 0..10));
        assert!(
            model
                .hit_test_nearest_in_range(range_anchor(0, 0), 5, 2)
                .is_none()
        );
    }

    #[test]
    fn hit_test_visible_block_uses_visible_geometry() {
        let block = VisibleBlockGeometry {
            entry_idx: 2,
            area: Rect::new(4, 5, 10, 3),
            content_area: Rect::new(7, 5, 6, 3),
            selection_area: Rect::new(3, 5, 12, 3),
            content_width: 6,
            top_clipped: false,
            bottom_clipped: false,
            drag_startable: true,
        };
        let model = ResolvedSelectionModel {
            visible_blocks: vec![block],
            ..Default::default()
        };

        let hit = model.hit_test_visible_block(8, 6).unwrap();
        assert_eq!(hit.entry_idx, 2);
        assert!(model.hit_test_visible_block(20, 20).is_none());
    }

    #[test]
    fn block_drag_overlay_inverts_selected_blocks() {
        let model = ResolvedSelectionModel {
            visible_blocks: vec![
                VisibleBlockGeometry {
                    entry_idx: 0,
                    area: Rect::new(0, 0, 5, 1),
                    content_area: Rect::new(2, 0, 3, 1),
                    selection_area: Rect::new(0, 0, 5, 1),
                    content_width: 3,
                    top_clipped: false,
                    bottom_clipped: false,
                    drag_startable: true,
                },
                VisibleBlockGeometry {
                    entry_idx: 1,
                    area: Rect::new(0, 1, 5, 1),
                    content_area: Rect::new(2, 1, 3, 1),
                    selection_area: Rect::new(0, 1, 5, 1),
                    content_width: 3,
                    top_clipped: false,
                    bottom_clipped: false,
                    drag_startable: true,
                },
                VisibleBlockGeometry {
                    entry_idx: 2,
                    area: Rect::new(0, 2, 5, 1),
                    content_area: Rect::new(2, 2, 3, 1),
                    selection_area: Rect::new(0, 2, 5, 1),
                    content_width: 3,
                    top_clipped: false,
                    bottom_clipped: false,
                    drag_startable: true,
                },
            ],
            ..Default::default()
        };
        let drag = ActiveBlockDrag {
            anchor_entry_idx: 0,
            head_entry_idx: 1,
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 3));
        render_block_drag_overlay(&model, &drag, &mut buf);
        // Blocks 0 and 1 should be inverted, block 2 should not.
        // Just verify it runs without panic — visual correctness
        // is validated by manual testing.
        assert_eq!(buf.area.height, 3);
    }

    #[test]
    fn autoscroll_above_viewport() {
        let area = Rect::new(0, 10, 80, 20);
        let result = compute_autoscroll(5, area);
        assert!(result.is_some());
        let s = result.unwrap();
        assert_eq!(s.direction, AutoScrollDirection::Up);
        assert!(s.speed >= 1);
    }

    #[test]
    fn autoscroll_below_viewport() {
        let area = Rect::new(0, 10, 80, 20);
        let result = compute_autoscroll(35, area);
        assert!(result.is_some());
        let s = result.unwrap();
        assert_eq!(s.direction, AutoScrollDirection::Down);
        assert!(s.speed >= 1);
    }

    #[test]
    fn autoscroll_inside_viewport() {
        let area = Rect::new(0, 10, 80, 20);
        assert!(compute_autoscroll(20, area).is_none());
    }

    #[test]
    fn autoscroll_near_top_edge() {
        let area = Rect::new(0, 10, 80, 20);
        // Row 11 is within EDGE_THRESHOLD (2) of top (10)
        let result = compute_autoscroll(11, area);
        assert!(result.is_some());
        assert_eq!(result.unwrap().direction, AutoScrollDirection::Up);
    }

    #[test]
    fn autoscroll_near_bottom_edge() {
        let area = Rect::new(0, 10, 80, 20);
        // Row 28 is within EDGE_THRESHOLD (2) of bottom (30)
        let result = compute_autoscroll(28, area);
        assert!(result.is_some());
        assert_eq!(result.unwrap().direction, AutoScrollDirection::Down);
    }

    #[test]
    fn autoscroll_speed_ramp() {
        // Farther from edge → faster speed
        assert!(speed_for_distance(1) < speed_for_distance(5));
        assert!(speed_for_distance(5) <= speed_for_distance(15));
    }

    #[test]
    fn block_drag_threshold() {
        let pending = PendingBlockDrag {
            anchor_entry_idx: 0,
            start_col: 5,
            start_row: 5,
        };
        assert!(!block_drag_threshold_exceeded(&pending, 5, 5));
        assert!(block_drag_threshold_exceeded(&pending, 6, 5));
        assert!(block_drag_threshold_exceeded(&pending, 5, 6));
    }

    /// Simulate selecting from the last line, then scrolling so the anchor
    /// scrolls off-screen. The overlay and copy should still work for all
    /// visible lines in the selection range.
    #[test]
    fn selection_survives_anchor_scrolling_off_screen() {
        // 10 lines, block_line_idx 0..9. Initially all visible.
        let lines: Vec<&str> = vec![
            "line zero",
            "line one",
            "line two",
            "line three",
            "line four",
            "line five",
            "line six",
            "line seven",
            "line eight",
            "line nine",
        ];

        // Build initial model with all 10 lines visible.
        let mut model = ResolvedSelectionModel::default();
        for (i, text) in lines.iter().enumerate() {
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: i,
                screen_y: i as u16,
                screen_x: 0,
                selectable_cols: 0..(text.len() as u16),
                text: text.to_string(),
                painted_region: None,
                joiner_to_previous: if i > 0 { Some("\n".to_string()) } else { None },
            });
        }

        // User clicks on line 9 (last line).
        let anchor = model.hit_test_selectable_range(0, 9).unwrap();
        assert_eq!(anchor.block_line_idx, 9);

        // User drags to line 5.
        let head = model.hit_test_selectable_range(0, 5).unwrap();
        assert_eq!(head.block_line_idx, 5);

        let drag = ActiveTextDrag {
            anchor,
            head,
            kind: SelectionKind::Linear,
            anchor_content_width: None,
        };

        // Selection overlay and copy work normally.
        // Both anchor (line 9) and head (line 5) are at col 0, so
        // the start line gets partial selection (from col 0 to end)
        // and the end line gets partial selection (from 0 to col 0+1).
        let text = reconstruct_selection_text(&model, &drag).unwrap();
        assert!(
            text.contains("line five"),
            "head line should be in selection"
        );
        assert!(
            text.contains("line eight"),
            "middle lines should be fully selected"
        );

        // Now simulate scroll: viewport shifts so only lines 0-6 are visible.
        // Lines 7-9 (including anchor at 9) are OFF SCREEN.
        let mut scrolled_model = ResolvedSelectionModel::default();
        for (i, line_text) in lines.iter().enumerate().take(7) {
            scrolled_model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: i,
                screen_y: i as u16,
                screen_x: 0,
                selectable_cols: 0..(line_text.len() as u16),
                text: line_text.to_string(),
                painted_region: None,
                joiner_to_previous: if i > 0 { Some("\n".to_string()) } else { None },
            });
        }

        // The anchor (block_line_idx=9) is now off-screen. During autoscroll
        // the resolver misses there and the caller keeps the previous head
        // (pinned by active_drag_motion_miss_keeps_previous_head).
        let new_head = head; // head was at block_line_idx=5

        let scrolled_drag = ActiveTextDrag {
            anchor,
            head: new_head,
            kind: SelectionKind::Linear,
            anchor_content_width: None,
        };

        // Overlay should highlight visible lines 5-6 (head=5, anchor=9 off-screen,
        // so range is 5..=9, visible portion is lines 5 and 6).
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 7));
        render_active_selection_overlay(&scrolled_model, &scrolled_drag, None, &mut buf);

        // Copy from the visible-only model produces text for visible lines in range.
        let text = reconstruct_selection_text(&scrolled_model, &scrolled_drag).unwrap();
        assert!(text.contains("line five"), "should contain head line");
        assert!(
            text.contains("line six"),
            "should contain visible line in range"
        );
        assert!(
            !text.contains("line zero"),
            "line before head not in selection"
        );
        // Off-screen lines aren't in the model, so they can't be in the text.
        assert!(!text.contains("line nine"), "off-screen line not in model");
    }

    // -----------------------------------------------------------------------
    // word_boundaries_at_col tests
    // -----------------------------------------------------------------------

    /// Shorthand for tests using the default tmux separator set.
    fn wb(text: &str, col: u16) -> Range<u16> {
        word_boundaries_at_col(text, col, DEFAULT_WORD_SEPARATORS)
    }

    #[test]
    fn word_boundaries_empty_string() {
        assert_eq!(wb("", 0), 0..0);
    }

    #[test]
    fn word_boundaries_single_word() {
        // "hello" → all Word class, col anywhere → 0..5
        assert_eq!(wb("hello", 0), 0..5);
        assert_eq!(wb("hello", 2), 0..5);
        assert_eq!(wb("hello", 4), 0..5);
    }

    #[test]
    fn word_boundaries_two_words() {
        // "hello world" → Word(0..5), Whitespace(5..6), Word(6..11)
        assert_eq!(wb("hello world", 0), 0..5);
        assert_eq!(wb("hello world", 4), 0..5);
        assert_eq!(wb("hello world", 5), 5..6);
        assert_eq!(wb("hello world", 6), 6..11);
        assert_eq!(wb("hello world", 10), 6..11);
    }

    #[test]
    fn word_boundaries_underscore_joins_words() {
        // "foo_bar" → all Word class
        assert_eq!(wb("foo_bar", 0), 0..7);
        assert_eq!(wb("foo_bar", 3), 0..7); // on '_'
        assert_eq!(wb("foo_bar", 6), 0..7);
    }

    #[test]
    fn word_boundaries_punctuation_run() {
        // "a---b" → Word(0..1), Separator(1..4), Word(4..5)
        assert_eq!(wb("a---b", 0), 0..1);
        assert_eq!(wb("a---b", 1), 1..4);
        assert_eq!(wb("a---b", 3), 1..4);
        assert_eq!(wb("a---b", 4), 4..5);
    }

    #[test]
    fn word_boundaries_whitespace_run() {
        // "a   b" → Word(0..1), Whitespace(1..4), Word(4..5)
        assert_eq!(wb("a   b", 1), 1..4);
        assert_eq!(wb("a   b", 3), 1..4);
    }

    #[test]
    fn word_boundaries_mixed_punct_and_word() {
        // "hello.world" → Word(0..5), Separator(5..6), Word(6..11)
        assert_eq!(wb("hello.world", 4), 0..5);
        assert_eq!(wb("hello.world", 5), 5..6);
        assert_eq!(wb("hello.world", 6), 6..11);
    }

    #[test]
    fn word_boundaries_line_start_and_end() {
        // " hello " → Whitespace(0..1), Word(1..6), Whitespace(6..7)
        assert_eq!(wb(" hello ", 0), 0..1);
        assert_eq!(wb(" hello ", 1), 1..6);
        assert_eq!(wb(" hello ", 6), 6..7);
    }

    #[test]
    fn word_boundaries_single_char() {
        assert_eq!(wb("a", 0), 0..1);
        assert_eq!(wb(".", 0), 0..1);
        assert_eq!(wb(" ", 0), 0..1);
    }

    #[test]
    fn word_boundaries_col_beyond_text() {
        // col past end clamps to last segment
        assert_eq!(wb("ab", 10), 0..2);
        assert_eq!(wb("a b", 10), 2..3);
    }

    #[test]
    fn word_boundaries_wide_char_cjk() {
        // CJK characters are not in DEFAULT_WORD_SEPARATORS and not whitespace,
        // so they are Word class (matching tmux). Each occupies 2 display cols.
        // "a\u{754c}b" → all Word class → 0..4
        assert_eq!(wb("a\u{754c}b", 0), 0..4);
        assert_eq!(wb("a\u{754c}b", 1), 0..4); // first col of wide char
        assert_eq!(wb("a\u{754c}b", 2), 0..4); // second col of wide char
        assert_eq!(wb("a\u{754c}b", 3), 0..4);
    }

    #[test]
    fn word_boundaries_consecutive_wide_chars() {
        // Two CJK chars → both Word class, grouped: Word(0..4)
        assert_eq!(wb("\u{754c}\u{4e16}", 0), 0..4);
        assert_eq!(wb("\u{754c}\u{4e16}", 2), 0..4);
    }

    #[test]
    fn word_boundaries_combining_mark() {
        // "e\u{0301}f" → graphemes: "e\u{0301}" (width 1) + "f" (width 1)
        // Both are Word class (not separators, not whitespace).
        assert_eq!(wb("e\u{0301}f", 0), 0..2);
        assert_eq!(wb("e\u{0301}f", 1), 0..2);
    }

    #[test]
    fn word_boundaries_digits_are_word_chars() {
        // "x86_64" → all Word
        assert_eq!(wb("x86_64", 0), 0..6);
        assert_eq!(wb("x86_64", 5), 0..6);
    }

    #[test]
    fn word_boundaries_mixed_separators_same_class_grouped() {
        // "-=" → both Separator, grouped as consecutive same-class
        assert_eq!(wb("-=", 0), 0..2);
        assert_eq!(wb("-=", 1), 0..2);
    }

    #[test]
    fn word_boundaries_tab_is_whitespace() {
        // Tab has width 1 via UnicodeWidthStr 0.2 and is classified as
        // Whitespace, so it separates adjacent Word segments:
        // Word(0..1), Whitespace(1..2), Word(2..3).
        assert_eq!(wb("a\tb", 0), 0..1);
        assert_eq!(wb("a\tb", 1), 1..2);
        assert_eq!(wb("a\tb", 2), 2..3);
    }

    #[test]
    fn word_boundaries_non_ascii_letters_are_word_chars() {
        // Non-ASCII letters group with adjacent ASCII word chars,
        // matching tmux behaviour where only ASCII punctuation is a separator.
        // "caf\u{e9}" → all Word class → 0..4
        assert_eq!(wb("caf\u{e9}", 0), 0..4);
        assert_eq!(wb("caf\u{e9}", 3), 0..4);
    }

    #[test]
    fn word_boundaries_cjk_between_separators() {
        // Separator, CJK (Word), Separator:
        // ".\u{754c}." → Separator(0..1), Word(1..3), Separator(3..4)
        assert_eq!(wb(".\u{754c}.", 0), 0..1);
        assert_eq!(wb(".\u{754c}.", 1), 1..3);
        assert_eq!(wb(".\u{754c}.", 3), 3..4);
    }

    #[test]
    fn word_boundaries_separator_chars_from_tmux_defaults() {
        // Verify all separator characters from DEFAULT_WORD_SEPARATORS are classified correctly.
        // Each should be its own Separator group when surrounded by Word chars.
        for sep in [
            '!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '-', '+', '=', '[', ']', '{', '}',
            '|', '\\', '/', ':', ';', '\'', '"', ',', '.', '<', '>', '?', '`', '~',
        ] {
            let text = format!("a{sep}b");
            assert_eq!(
                wb(&text, 0),
                0..1,
                "char '{sep}' should be a separator (Word before)",
            );
            assert_eq!(
                wb(&text, 1),
                1..2,
                "char '{sep}' should be its own Separator group",
            );
            assert_eq!(
                wb(&text, 2),
                2..3,
                "char '{sep}' should be a separator (Word after)",
            );
        }
    }

    #[test]
    fn word_boundaries_underscore_is_not_a_separator() {
        // Underscore is NOT in DEFAULT_WORD_SEPARATORS (matches tmux).
        // "a_b" → all Word → 0..3
        assert_eq!(wb("a_b", 0), 0..3);
        assert_eq!(wb("a_b", 1), 0..3);
        assert_eq!(wb("a_b", 2), 0..3);
    }

    #[test]
    fn word_boundaries_custom_separators_empty() {
        // Empty separators string → only whitespace breaks words.
        // "hello-world" with no separators → all Word → 0..11
        assert_eq!(word_boundaries_at_col("hello-world", 0, ""), 0..11);
        assert_eq!(word_boundaries_at_col("hello-world", 5, ""), 0..11);
        assert_eq!(word_boundaries_at_col("hello-world", 6, ""), 0..11);
        // "a.b@c" → all Word → 0..5
        assert_eq!(word_boundaries_at_col("a.b@c", 2, ""), 0..5);
    }

    #[test]
    fn word_boundaries_custom_separators_subset() {
        // Only "." and "/" are separators — hyphen and @ are word chars.
        let seps = "./";
        assert_eq!(word_boundaries_at_col("hello-world", 0, seps), 0..11);
        assert_eq!(word_boundaries_at_col("hello.world", 0, seps), 0..5);
        assert_eq!(word_boundaries_at_col("hello.world", 5, seps), 5..6);
        assert_eq!(word_boundaries_at_col("a/b", 0, seps), 0..1);
        assert_eq!(word_boundaries_at_col("user@host", 0, seps), 0..9);
    }

    // -----------------------------------------------------------------------
    // strip_trailing_url_punctuation tests
    // -----------------------------------------------------------------------

    #[test]
    fn strip_trailing_no_punctuation() {
        assert_eq!(
            strip_trailing_url_punctuation("https://example.com/path"),
            "https://example.com/path"
        );
    }

    #[test]
    fn strip_trailing_period() {
        assert_eq!(
            strip_trailing_url_punctuation("https://example.com."),
            "https://example.com"
        );
    }

    #[test]
    fn strip_trailing_comma() {
        assert_eq!(
            strip_trailing_url_punctuation("https://example.com,"),
            "https://example.com"
        );
    }

    #[test]
    fn strip_trailing_multiple_punct() {
        assert_eq!(
            strip_trailing_url_punctuation("https://example.com.)"),
            "https://example.com"
        );
    }

    #[test]
    fn strip_trailing_balanced_parens_kept() {
        assert_eq!(
            strip_trailing_url_punctuation(
                "https://en.wikipedia.org/wiki/Rust_(programming_language)"
            ),
            "https://en.wikipedia.org/wiki/Rust_(programming_language)"
        );
    }

    #[test]
    fn strip_trailing_unbalanced_close_paren() {
        // "(see https://example.com)" → regex captures "https://example.com)"
        assert_eq!(
            strip_trailing_url_punctuation("https://example.com)"),
            "https://example.com"
        );
    }

    #[test]
    fn strip_trailing_balanced_then_unbalanced() {
        // URL has one ( and two ): the last ) is unbalanced
        assert_eq!(
            strip_trailing_url_punctuation("https://example.com/wiki_(test))"),
            "https://example.com/wiki_(test)"
        );
    }

    #[test]
    fn strip_trailing_exclamation() {
        assert_eq!(
            strip_trailing_url_punctuation("https://example.com!"),
            "https://example.com"
        );
    }

    #[test]
    fn strip_trailing_semicolon() {
        assert_eq!(
            strip_trailing_url_punctuation("https://example.com;"),
            "https://example.com"
        );
    }

    #[test]
    fn strip_trailing_colon() {
        assert_eq!(
            strip_trailing_url_punctuation("https://example.com:"),
            "https://example.com"
        );
    }

    // -----------------------------------------------------------------------
    // url_range_at_col tests
    // -----------------------------------------------------------------------

    #[test]
    fn url_range_simple_https() {
        let text = "see https://example.com here";
        // "see " = 4 cols, URL = 19 cols → 4..23
        assert_eq!(url_range_at_col(text, 4), Some(4..23));
        assert_eq!(url_range_at_col(text, 10), Some(4..23));
        assert_eq!(url_range_at_col(text, 22), Some(4..23));
    }

    #[test]
    fn url_range_no_url() {
        assert_eq!(url_range_at_col("hello world", 0), None);
        assert_eq!(url_range_at_col("hello world", 5), None);
    }

    #[test]
    fn url_range_empty_string() {
        assert_eq!(url_range_at_col("", 0), None);
    }

    #[test]
    fn url_range_url_at_start() {
        let text = "https://example.com rest";
        assert_eq!(url_range_at_col(text, 0), Some(0..19));
    }

    #[test]
    fn url_range_url_at_end() {
        let text = "visit https://example.com";
        assert_eq!(url_range_at_col(text, 6), Some(6..25));
        assert_eq!(url_range_at_col(text, 24), Some(6..25));
    }

    #[test]
    fn url_range_trailing_period_stripped() {
        let text = "see https://example.com.";
        // URL match = "https://example.com." → stripped to "https://example.com"
        assert_eq!(url_range_at_col(text, 4), Some(4..23));
        assert_eq!(url_range_at_col(text, 22), Some(4..23));
        // Click ON the trailing period → past the stripped range → None
        assert_eq!(url_range_at_col(text, 23), None);
    }

    #[test]
    fn url_range_trailing_comma_stripped() {
        let text = "see https://example.com, more";
        assert_eq!(url_range_at_col(text, 4), Some(4..23));
        // Click on comma → None
        assert_eq!(url_range_at_col(text, 23), None);
    }

    #[test]
    fn url_range_balanced_parens_preserved() {
        let text = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        let len = text.len() as u16;
        assert_eq!(url_range_at_col(text, 0), Some(0..len));
        assert_eq!(url_range_at_col(text, len - 1), Some(0..len));
    }

    #[test]
    fn url_range_unbalanced_paren_in_prose() {
        let text = "(see https://example.com)";
        // Match: "https://example.com)" → stripped to "https://example.com"
        assert_eq!(url_range_at_col(text, 5), Some(5..24));
        // Click on the trailing ')' → None
        assert_eq!(url_range_at_col(text, 24), None);
    }

    #[test]
    fn url_range_query_string() {
        let text = "https://example.com/path?q=1&r=2";
        assert_eq!(url_range_at_col(text, 0), Some(0..32));
    }

    #[test]
    fn url_range_fragment() {
        let text = "https://example.com/page#section";
        assert_eq!(url_range_at_col(text, 0), Some(0..32));
    }

    #[test]
    fn url_range_ftp_scheme() {
        let text = "get ftp://files.example.com/data";
        assert_eq!(url_range_at_col(text, 4), Some(4..32));
    }

    #[test]
    fn url_range_file_scheme() {
        let text = "open file:///tmp/readme.txt";
        assert_eq!(url_range_at_col(text, 5), Some(5..27));
    }

    #[test]
    fn url_range_case_insensitive() {
        let text = "HTTP://EXAMPLE.COM/PATH";
        assert_eq!(url_range_at_col(text, 0), Some(0..23));
    }

    #[test]
    fn url_range_multiple_urls() {
        let text = "see https://a.com and https://b.com";
        // First URL: col 4..17
        assert_eq!(url_range_at_col(text, 4), Some(4..17));
        // Between URLs: None
        assert_eq!(url_range_at_col(text, 18), None);
        // Second URL: col 22..35
        assert_eq!(url_range_at_col(text, 22), Some(22..35));
    }

    #[test]
    fn url_range_col_outside_text() {
        let text = "https://example.com";
        assert_eq!(url_range_at_col(text, 100), None);
    }

    #[test]
    fn url_range_not_a_url_scheme() {
        // "notascheme://foo" should not match
        assert_eq!(url_range_at_col("notascheme://foo", 0), None);
    }

    #[test]
    fn url_range_url_with_port() {
        let text = "http://localhost:8080/api";
        assert_eq!(url_range_at_col(text, 0), Some(0..25));
    }

    #[test]
    fn url_range_degenerate_scheme_only_skipped() {
        // "https://." → regex matches "https://." → strip produces "https://"
        // which is scheme-only and should be skipped.
        assert_eq!(url_range_at_col("see https://.", 4), None);
    }

    #[test]
    fn url_range_combined_with_word_boundaries() {
        // Simulates the double-click fallback: url_range_at_col → word_boundaries_at_col
        let text = "click https://example.com/path or this_word";
        // On URL → url_range returns the range
        let url = url_range_at_col(text, 6);
        assert!(url.is_some());
        // On plain word → url_range returns None, word_boundaries takes over
        let non_url_col = 36; // inside "this_word"
        assert_eq!(url_range_at_col(text, non_url_col), None);
        assert_eq!(wb(text, non_url_col), 34..43);
    }

    fn wrap_model(fragments: &[(&str, Option<&str>)]) -> ResolvedSelectionModel {
        let mut model = ResolvedSelectionModel::default();
        for (i, (text, joiner)) in fragments.iter().enumerate() {
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: i,
                screen_y: i as u16,
                screen_x: 0,
                selectable_cols: 0..display_width(text),
                text: (*text).to_string(),
                painted_region: None,
                joiner_to_previous: joiner.map(str::to_string),
            });
        }
        model
    }

    fn wrap_hit(block_line_idx: usize, col_within_range: u16) -> RangeHit {
        RangeHit {
            entry_idx: 0,
            range_id: 0,
            block_line_idx,
            col_within_range,
        }
    }

    fn semantic(model: &ResolvedSelectionModel, line: usize, col: u16) -> SemanticSelection {
        semantic_selection_at(model, &wrap_hit(line, col), DEFAULT_WORD_SEPARATORS)
            .expect("semantic selection")
    }

    fn ep(block_line_idx: usize, col_within_range: u16) -> SelectionEndpoint {
        SelectionEndpoint {
            block_line_idx,
            col_within_range,
        }
    }

    #[test]
    fn semantic_selection_mid_word_wrap_selects_full_identifier() {
        let model = wrap_model(&[("hello_world_", None), ("identifier", Some(""))]);
        let expected = SemanticSelection {
            anchor: ep(0, 0),
            head: ep(1, 9),
            text: "hello_world_identifier".to_string(),
        };
        assert_eq!(semantic(&model, 0, 0), expected);
        assert_eq!(semantic(&model, 0, 11), expected);
        assert_eq!(semantic(&model, 1, 0), expected);
        assert_eq!(semantic(&model, 1, 5), expected);
    }

    #[test]
    fn semantic_selection_space_wrap_does_not_cross() {
        let model = wrap_model(&[("hello", None), ("world", Some(" "))]);
        assert_eq!(
            semantic(&model, 0, 1),
            SemanticSelection {
                anchor: ep(0, 0),
                head: ep(0, 4),
                text: "hello".to_string(),
            }
        );
        assert_eq!(
            semantic(&model, 1, 0),
            SemanticSelection {
                anchor: ep(1, 0),
                head: ep(1, 4),
                text: "world".to_string(),
            }
        );
    }

    #[test]
    fn semantic_selection_hard_break_does_not_cross() {
        let model = wrap_model(&[("hello", None), ("hello", None)]);
        let first = SemanticSelection {
            anchor: ep(0, 0),
            head: ep(0, 4),
            text: "hello".to_string(),
        };
        let second = SemanticSelection {
            anchor: ep(1, 0),
            head: ep(1, 4),
            text: "hello".to_string(),
        };
        assert_eq!(semantic(&model, 0, 2), first);
        assert_eq!(semantic(&model, 1, 2), second);
    }

    #[test]
    fn semantic_selection_wrapped_url_spans_fragments() {
        let model = wrap_model(&[
            ("https://example.", None),
            ("com/very/long/", Some("")),
            ("path", Some("")),
        ]);
        let expected = SemanticSelection {
            anchor: ep(0, 0),
            head: ep(2, 3),
            text: "https://example.com/very/long/path".to_string(),
        };
        assert_eq!(semantic(&model, 0, 2), expected);
        assert_eq!(semantic(&model, 1, 3), expected);
        assert_eq!(semantic(&model, 2, 0), expected);
    }

    #[test]
    fn semantic_selection_custom_separators_across_wrap() {
        let model = wrap_model(&[("hello-", None), ("world", Some(""))]);
        let grouped = semantic_selection_at(&model, &wrap_hit(0, 1), "").expect("grouped");
        assert_eq!(
            grouped,
            SemanticSelection {
                anchor: ep(0, 0),
                head: ep(1, 4),
                text: "hello-world".to_string(),
            }
        );
        assert_eq!(
            semantic_selection_at(&model, &wrap_hit(1, 0), "").expect("grouped from row 1"),
            grouped
        );

        let split = semantic_selection_at(&model, &wrap_hit(0, 1), "-").expect("split");
        assert_eq!(
            split,
            SemanticSelection {
                anchor: ep(0, 0),
                head: ep(0, 4),
                text: "hello".to_string(),
            }
        );
        assert_eq!(
            semantic_selection_at(&model, &wrap_hit(1, 2), "-").expect("world side"),
            SemanticSelection {
                anchor: ep(1, 0),
                head: ep(1, 4),
                text: "world".to_string(),
            }
        );
    }

    #[test]
    fn semantic_selection_single_row_word_and_url() {
        let words = wrap_model(&[("hello world", None)]);
        assert_eq!(
            semantic(&words, 0, 1),
            SemanticSelection {
                anchor: ep(0, 0),
                head: ep(0, 4),
                text: "hello".to_string(),
            }
        );
        assert_eq!(
            semantic(&words, 0, 7),
            SemanticSelection {
                anchor: ep(0, 6),
                head: ep(0, 10),
                text: "world".to_string(),
            }
        );

        let urls = wrap_model(&[("see https://echo-agent.invalid now", None)]);
        assert_eq!(
            semantic(&urls, 0, 6),
            SemanticSelection {
                anchor: ep(0, 4),
                head: ep(0, 15),
                text: "https://echo-agent.invalid".to_string(),
            }
        );
    }

    #[test]
    fn semantic_selection_three_fragment_mid_word_click_middle() {
        let model = wrap_model(&[
            ("hello_", None),
            ("world_", Some("")),
            ("identifier", Some("")),
        ]);
        let expected = SemanticSelection {
            anchor: ep(0, 0),
            head: ep(2, 9),
            text: "hello_world_identifier".to_string(),
        };
        assert_eq!(semantic(&model, 1, 0), expected);
        assert_eq!(semantic(&model, 1, 5), expected);
    }

    #[test]
    fn semantic_selection_partial_first_row_then_wrap() {
        let model = wrap_model(&[("foo bar_very", None), ("long", Some(""))]);
        let wrapped = SemanticSelection {
            anchor: ep(0, 4),
            head: ep(1, 3),
            text: "bar_verylong".to_string(),
        };
        assert_eq!(semantic(&model, 0, 4), wrapped);
        assert_eq!(semantic(&model, 0, 11), wrapped);
        assert_eq!(semantic(&model, 1, 1), wrapped);
        assert_eq!(
            semantic(&model, 0, 1),
            SemanticSelection {
                anchor: ep(0, 0),
                head: ep(0, 2),
                text: "foo".to_string(),
            }
        );
    }

    #[test]
    fn semantic_selection_head_stops_mid_continuation_row() {
        let model = wrap_model(&[("foo bar_very", None), ("long baz", Some(""))]);
        let wrapped = SemanticSelection {
            anchor: ep(0, 4),
            head: ep(1, 3),
            text: "bar_verylong".to_string(),
        };
        assert_eq!(semantic(&model, 0, 4), wrapped);
        assert_eq!(semantic(&model, 1, 0), wrapped);
        assert_eq!(
            semantic(&model, 1, 5),
            SemanticSelection {
                anchor: ep(1, 5),
                head: ep(1, 7),
                text: "baz".to_string(),
            }
        );

        let one_char = wrap_model(&[("hello_", None), ("x", Some(""))]);
        assert_eq!(
            semantic(&one_char, 0, 0),
            SemanticSelection {
                anchor: ep(0, 0),
                head: ep(1, 0),
                text: "hello_x".to_string(),
            }
        );
    }

    #[test]
    fn semantic_selection_mid_word_wrap_wide_glyphs() {
        let cjk = wrap_model(&[("你", None), ("好世界", Some(""))]);
        let expected = SemanticSelection {
            anchor: ep(0, 0),
            head: ep(1, 5),
            text: "你好世界".to_string(),
        };
        assert_eq!(semantic(&cjk, 0, 0), expected);
        assert_eq!(semantic(&cjk, 0, 1), expected);
        assert_eq!(semantic(&cjk, 1, 0), expected);
        assert_eq!(semantic(&cjk, 1, 4), expected);

        let mixed = wrap_model(&[("hello_", None), ("世界", Some(""))]);
        let expected = SemanticSelection {
            anchor: ep(0, 0),
            head: ep(1, 3),
            text: "hello_世界".to_string(),
        };
        assert_eq!(semantic(&mixed, 0, 3), expected);
        assert_eq!(semantic(&mixed, 1, 1), expected);
    }

    #[test]
    fn semantic_selection_wrap_split_cluster_uses_concat_width() {
        // Per-fragment widths are 2+2; concatenated ZWJ pair is width 2.
        let model = wrap_model(&[("👨", None), ("\u{200d}👩", Some(""))]);
        let from_first = semantic(&model, 0, 0);
        assert_eq!(from_first.text, "👨‍👩");
        assert_eq!(from_first.anchor, ep(0, 0));
        // Finished-concat widths assign both columns to the first fragment.
        assert_eq!(from_first.head, ep(0, 1));
        assert_eq!(semantic(&model, 1, 0), from_first);

        // Local col 1 on the continuation is still the cluster, not `tail`.
        let with_tail = wrap_model(&[("👨", None), ("\u{200d}👩 tail", Some(""))]);
        assert_eq!(semantic(&with_tail, 1, 1).text, "👨‍👩");
        assert_eq!(semantic(&with_tail, 1, 3).text, "tail");
    }

    #[test]
    fn semantic_selection_joiner_only_range_returns_none() {
        let model = wrap_model(&[("ab", None), ("cd", Some(" "))]);
        // Past-end col is not hit-testable when selectable_cols matches text width.
        assert_eq!(
            semantic_selection_at(&model, &wrap_hit(0, 2), DEFAULT_WORD_SEPARATORS),
            None
        );

        let mut padded = ResolvedSelectionModel::default();
        padded.push_line(ResolvedSelectableLine {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 0,
            screen_y: 0,
            screen_x: 0,
            selectable_cols: 0..5,
            text: "ab".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        });
        padded.push_line(ResolvedSelectableLine {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 1,
            screen_y: 1,
            screen_x: 0,
            selectable_cols: 0..2,
            text: "cd".to_string(),
            painted_region: None,
            joiner_to_previous: Some(" ".to_string()),
        });
        assert_eq!(
            semantic_selection_at(&padded, &wrap_hit(0, 2), DEFAULT_WORD_SEPARATORS),
            None
        );

        assert_eq!(
            semantic_selection_at(
                &wrap_model(&[("", None), ("", Some(" "))]),
                &wrap_hit(0, 0),
                DEFAULT_WORD_SEPARATORS,
            ),
            None
        );
    }

    #[test]
    fn semantic_selection_multi_space_and_newline_joiners_do_not_merge() {
        let spaces = wrap_model(&[("hello", None), ("world", Some("   "))]);
        assert_eq!(
            semantic(&spaces, 0, 1),
            SemanticSelection {
                anchor: ep(0, 0),
                head: ep(0, 4),
                text: "hello".to_string(),
            }
        );
        assert_eq!(
            semantic(&spaces, 1, 0),
            SemanticSelection {
                anchor: ep(1, 0),
                head: ep(1, 4),
                text: "world".to_string(),
            }
        );

        let newline = wrap_model(&[("https://ex", None), ("ample.com", Some("\n"))]);
        assert_eq!(
            semantic(&newline, 0, 0),
            SemanticSelection {
                anchor: ep(0, 0),
                head: ep(0, 9),
                text: "https://ex".to_string(),
            }
        );
        assert_eq!(
            semantic(&newline, 1, 0),
            SemanticSelection {
                anchor: ep(1, 0),
                head: ep(1, 4),
                text: "ample".to_string(),
            }
        );
    }

    #[test]
    fn semantic_selection_empty_wrap_fragments() {
        let trailing = wrap_model(&[("hello_world", None), ("", Some(""))]);
        let expected = SemanticSelection {
            anchor: ep(0, 0),
            head: ep(0, 10),
            text: "hello_world".to_string(),
        };
        assert_eq!(semantic(&trailing, 0, 0), expected);
        assert_eq!(semantic(&trailing, 1, 0), expected);

        let middle = wrap_model(&[("hello_", None), ("", Some("")), ("world", Some(""))]);
        let expected = SemanticSelection {
            anchor: ep(0, 0),
            head: ep(2, 4),
            text: "hello_world".to_string(),
        };
        assert_eq!(semantic(&middle, 0, 0), expected);
        assert_eq!(semantic(&middle, 1, 0), expected);
        assert_eq!(semantic(&middle, 2, 0), expected);
    }

    #[test]
    fn semantic_selection_wrapped_url_among_words() {
        let model = wrap_model(&[("see https://ex", None), ("ample.com please", Some(""))]);
        assert_eq!(
            semantic(&model, 0, 0),
            SemanticSelection {
                anchor: ep(0, 0),
                head: ep(0, 2),
                text: "see".to_string(),
            }
        );
        let url = SemanticSelection {
            anchor: ep(0, 4),
            head: ep(1, 8),
            text: "https://example.com".to_string(),
        };
        assert_eq!(semantic(&model, 0, 12), url);
        assert_eq!(semantic(&model, 1, 0), url);
        assert_eq!(
            semantic(&model, 1, 10),
            SemanticSelection {
                anchor: ep(1, 10),
                head: ep(1, 15),
                text: "please".to_string(),
            }
        );
    }

    #[test]
    fn semantic_selection_from_word_wrap_joiners() {
        let source = ratatui::text::Line::from("hello_world_identifier");
        let (wrapped, joiners) = crate::render::wrapping::word_wrap_line_with_joiners(&source, 12);
        let mut model = ResolvedSelectionModel::default();
        let mut last_width = 0u16;
        for (i, (line, joiner)) in wrapped.iter().zip(joiners).enumerate() {
            let text = crate::scrollback::types::line_plain_text(line);
            last_width = display_width(&text);
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: i,
                screen_y: i as u16,
                screen_x: 0,
                selectable_cols: 0..last_width,
                text,
                painted_region: None,
                joiner_to_previous: joiner,
            });
        }
        let expected = SemanticSelection {
            anchor: ep(0, 0),
            head: ep(
                model.ranges[0].lines.len() - 1,
                last_width.saturating_sub(1),
            ),
            text: "hello_world_identifier".to_string(),
        };
        for line in &model.ranges[0].lines {
            if !line.text.is_empty() {
                assert_eq!(semantic(&model, line.block_line_idx, 0), expected);
            }
        }
    }

    // -----------------------------------------------------------------------
    // selected_cols_for_endpoints tests
    // -----------------------------------------------------------------------

    fn make_test_line(
        block_line_idx: usize,
        selectable_cols: Range<u16>,
    ) -> ResolvedSelectableLine {
        ResolvedSelectableLine {
            entry_idx: 0,
            range_id: 0,
            block_line_idx,
            screen_y: block_line_idx as u16,
            screen_x: 0,
            selectable_cols,
            text: String::new(),
            painted_region: None,
            joiner_to_previous: None,
        }
    }

    #[test]
    fn endpoints_single_line_selection() {
        let line = make_test_line(5, 0..20);
        assert_eq!(selected_cols_for_endpoints(5, 3, 5, 10, &line), Some(3..11));
    }

    #[test]
    fn endpoints_single_line_anchor_after_head() {
        let line = make_test_line(5, 0..20);
        // head before anchor produces same result (min/max)
        assert_eq!(selected_cols_for_endpoints(5, 10, 5, 3, &line), Some(3..11));
    }

    #[test]
    fn endpoints_single_line_same_col() {
        let line = make_test_line(5, 0..20);
        assert_eq!(selected_cols_for_endpoints(5, 7, 5, 7, &line), Some(7..8));
    }

    #[test]
    fn endpoints_snap_to_grapheme_boundaries() {
        let mut line = make_test_line(5, 0..16);
        line.text = "需要我帮你做什么".to_string();

        // Ends snap past the glyph: 么 spans [14, 16), so either endpoint cell yields the full range.
        assert_eq!(selected_cols_for_endpoints(5, 0, 5, 14, &line), Some(0..16));
        assert_eq!(selected_cols_for_endpoints(5, 0, 5, 15, &line), Some(0..16));

        // Starts floor onto the glyph: 要 spans [2, 4), so an anchor on cell 3 selects from cell 2. The head on 你 ([8, 10)) snaps the end past it.
        assert_eq!(selected_cols_for_endpoints(5, 3, 5, 8, &line), Some(2..10));

        // Multi-line last and first lines snap the same way.
        assert_eq!(selected_cols_for_endpoints(2, 4, 5, 14, &line), Some(0..16));
        assert_eq!(selected_cols_for_endpoints(5, 3, 7, 8, &line), Some(2..16));

        // Padded fixture lines (text narrower than the range) keep raw columns.
        let padded = make_test_line(5, 0..20);
        assert_eq!(
            selected_cols_for_endpoints(5, 3, 5, 10, &padded),
            Some(3..11)
        );
    }

    #[test]
    fn endpoints_multi_line_first_line() {
        let line = make_test_line(2, 0..15);
        // anchor=2, head=5: line 2 is first, selects anchor_col..width
        assert_eq!(selected_cols_for_endpoints(2, 4, 5, 10, &line), Some(4..15));
    }

    #[test]
    fn endpoints_multi_line_last_line() {
        let line = make_test_line(5, 0..15);
        // anchor=2, head=5: line 5 is last, selects 0..head_col+1
        assert_eq!(selected_cols_for_endpoints(2, 4, 5, 10, &line), Some(0..11));
    }

    #[test]
    fn endpoints_multi_line_middle_line() {
        let line = make_test_line(3, 0..15);
        // anchor=2, head=5: line 3 is middle, selects 0..width (full line)
        assert_eq!(selected_cols_for_endpoints(2, 4, 5, 10, &line), Some(0..15));
    }

    #[test]
    fn endpoints_multi_line_reversed_anchor_after_head() {
        // anchor=5, head=2 (reversed): first line (2) uses head_col, last line (5) uses anchor_col
        let first = make_test_line(2, 0..15);
        assert_eq!(
            selected_cols_for_endpoints(5, 10, 2, 4, &first),
            Some(4..15)
        );
        let last = make_test_line(5, 0..15);
        assert_eq!(selected_cols_for_endpoints(5, 10, 2, 4, &last), Some(0..11));
        let middle = make_test_line(3, 0..15);
        assert_eq!(
            selected_cols_for_endpoints(5, 10, 2, 4, &middle),
            Some(0..15)
        );
    }

    #[test]
    fn endpoints_line_outside_range_returns_none() {
        let line = make_test_line(10, 0..20);
        assert_eq!(selected_cols_for_endpoints(2, 4, 5, 10, &line), None);

        let before = make_test_line(0, 0..20);
        assert_eq!(selected_cols_for_endpoints(2, 4, 5, 10, &before), None);
    }

    #[test]
    fn endpoints_width_clamping_single_line() {
        let line = make_test_line(5, 0..10);
        // head_col 20 exceeds width 10, clamped to width
        assert_eq!(selected_cols_for_endpoints(5, 3, 5, 20, &line), Some(3..10));
    }

    #[test]
    fn endpoints_width_clamping_both_cols_beyond() {
        let line = make_test_line(5, 0..5);
        // Both cols 15 and 20 exceed width 5 -> clamped to 5..5 (empty range)
        assert_eq!(selected_cols_for_endpoints(5, 15, 5, 20, &line), Some(5..5));
    }

    #[test]
    fn endpoints_width_clamping_first_line() {
        let line = make_test_line(2, 0..8);
        // anchor=2 (first line), col=12 exceeds width=8 -> 8..8
        assert_eq!(selected_cols_for_endpoints(2, 12, 5, 3, &line), Some(8..8));
    }

    #[test]
    fn endpoints_width_clamping_last_line() {
        let line = make_test_line(5, 0..8);
        // head=5 (last line), col=20 exceeds width=8 -> 0..8
        assert_eq!(selected_cols_for_endpoints(2, 3, 5, 20, &line), Some(0..8));
    }

    #[test]
    fn endpoints_nonzero_selectable_start() {
        // selectable_cols 5..15 means width = 10
        let line = make_test_line(3, 5..15);
        assert_eq!(selected_cols_for_endpoints(3, 2, 3, 7, &line), Some(2..8));
    }

    #[test]
    fn endpoints_wrapper_matches_direct_call() {
        // Verify selected_cols_for_line_by_block_idx produces the same result
        // as a direct call to selected_cols_for_endpoints with the drag fields.
        let line = make_test_line(3, 0..20);
        let drag = ActiveTextDrag {
            anchor: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 2,
                col_within_range: 5,
            },
            head: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 4,
                col_within_range: 12,
            },
            kind: SelectionKind::Linear,
            anchor_content_width: None,
        };
        let via_wrapper = selected_cols_for_line_by_block_idx(&drag, &line);
        let via_direct = selected_cols_for_endpoints(2, 5, 4, 12, &line);
        assert_eq!(via_wrapper, via_direct);
        assert_eq!(via_wrapper, Some(0..20));
    }

    // -----------------------------------------------------------------------
    // render_persistent_selection_overlay tests
    // -----------------------------------------------------------------------

    /// Marker foreground: test themes quantize to no color, so the
    /// highlight takes its reverse-video path (a modifier change).
    fn paint_marker(buf: &mut Buffer) {
        for y in buf.area.y..buf.area.y.saturating_add(buf.area.height) {
            for x in buf.area.x..buf.area.x.saturating_add(buf.area.width) {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_fg(Color::White);
                }
            }
        }
    }

    /// Returns true if the cell at `(x, y)` was modified by the overlay
    /// relative to the baseline buffer.
    fn cell_was_modified(buf: &Buffer, baseline: &Buffer, x: u16, y: u16) -> bool {
        let cell = buf.cell((x, y)).unwrap();
        let orig = baseline.cell((x, y)).unwrap();
        (cell.fg, cell.bg, cell.modifier) != (orig.fg, orig.bg, orig.modifier)
    }

    #[test]
    fn persistent_overlay_renders_multi_line_correctly() {
        let mut model = ResolvedSelectionModel::default();
        for i in 0..3u16 {
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: i as usize,
                screen_y: i,
                screen_x: 2,
                selectable_cols: 0..10,
                text: format!("line {i}"),
                painted_region: None,
                joiner_to_previous: if i > 0 { Some("\n".into()) } else { None },
            });
        }
        // anchor=(line 0, col 3), head=(line 2, col 5)
        let sel = PersistentTextSelection {
            entry_idx: 0,
            range_id: 0,
            anchor: SelectionEndpoint {
                block_line_idx: 0,
                col_within_range: 3,
            },
            head: SelectionEndpoint {
                block_line_idx: 2,
                col_within_range: 5,
            },
            origin: SelectionOrigin::Drag,
            kind: SelectionKind::Linear,
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 3));
        paint_marker(&mut buf);
        let baseline = buf.clone();
        render_persistent_selection_overlay(&model, &sel, None, &mut buf);

        // Line 0 (first): cols 3..10 → screen_x 2+3=5 through 2+9=11
        assert!(!cell_was_modified(&buf, &baseline, 4, 0));
        for x in 5..12 {
            assert!(
                cell_was_modified(&buf, &baseline, x, 0),
                "line 0, screen_x={x} should be inverted"
            );
        }
        assert!(!cell_was_modified(&buf, &baseline, 12, 0));

        // Line 1 (middle): cols 0..10 → screen_x 2..12
        assert!(!cell_was_modified(&buf, &baseline, 1, 1));
        for x in 2..12 {
            assert!(
                cell_was_modified(&buf, &baseline, x, 1),
                "line 1, screen_x={x} should be inverted"
            );
        }
        assert!(!cell_was_modified(&buf, &baseline, 12, 1));

        // Line 2 (last): cols 0..6 → screen_x 2..8
        assert!(!cell_was_modified(&buf, &baseline, 1, 2));
        for x in 2..8 {
            assert!(
                cell_was_modified(&buf, &baseline, x, 2),
                "line 2, screen_x={x} should be inverted"
            );
        }
        assert!(!cell_was_modified(&buf, &baseline, 8, 2));
    }

    #[test]
    fn persistent_overlay_nonexistent_range_is_noop() {
        let model = ResolvedSelectionModel::default();
        let sel = PersistentTextSelection {
            entry_idx: 99,
            range_id: 5,
            anchor: SelectionEndpoint {
                block_line_idx: 0,
                col_within_range: 0,
            },
            head: SelectionEndpoint {
                block_line_idx: 0,
                col_within_range: 5,
            },
            origin: SelectionOrigin::DoubleClick,
            kind: SelectionKind::Linear,
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 3));
        paint_marker(&mut buf);
        let baseline = buf.clone();
        render_persistent_selection_overlay(&model, &sel, None, &mut buf);
        assert_eq!(buf, baseline);
    }

    #[test]
    fn persistent_overlay_single_line_word_selection() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(ResolvedSelectableLine {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 0,
            screen_y: 0,
            screen_x: 5,
            selectable_cols: 0..20,
            text: "hello world foo bar".into(),
            painted_region: None,
            joiner_to_previous: None,
        });
        // Selecting cols 6..11 ("world") on a line at screen_x=5.
        let sel = PersistentTextSelection {
            entry_idx: 0,
            range_id: 0,
            anchor: SelectionEndpoint {
                block_line_idx: 0,
                col_within_range: 6,
            },
            head: SelectionEndpoint {
                block_line_idx: 0,
                col_within_range: 10,
            },
            origin: SelectionOrigin::DoubleClick,
            kind: SelectionKind::Linear,
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 1));
        paint_marker(&mut buf);
        let baseline = buf.clone();
        render_persistent_selection_overlay(&model, &sel, None, &mut buf);

        // screen cols: 5+0+6=11 through 5+0+10=15 (cols 6..11 = values 6,7,8,9,10)
        assert!(!cell_was_modified(&buf, &baseline, 10, 0));
        for x in 11..16 {
            assert!(
                cell_was_modified(&buf, &baseline, x, 0),
                "screen_x={x} should be inverted"
            );
        }
        assert!(!cell_was_modified(&buf, &baseline, 16, 0));
    }

    #[test]
    fn persistent_and_active_overlay_agree_on_same_endpoints() {
        // Pin theme to avoid races with parallel tests that call `cache::set`.
        crate::theme::cache::set(crate::theme::ThemeKind::EchoAgentNight);
        let mut model = ResolvedSelectionModel::default();
        for i in 0..4u16 {
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: i as usize,
                screen_y: i,
                screen_x: 0,
                selectable_cols: 0..15,
                text: format!("line {i} content"),
                painted_region: None,
                joiner_to_previous: if i > 0 { Some("\n".into()) } else { None },
            });
        }
        let drag = ActiveTextDrag {
            anchor: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 1,
                col_within_range: 3,
            },
            head: RangeHit {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: 3,
                col_within_range: 8,
            },
            kind: SelectionKind::Linear,
            anchor_content_width: None,
        };
        let sel = PersistentTextSelection {
            entry_idx: 0,
            range_id: 0,
            anchor: SelectionEndpoint {
                block_line_idx: 1,
                col_within_range: 3,
            },
            head: SelectionEndpoint {
                block_line_idx: 3,
                col_within_range: 8,
            },
            origin: SelectionOrigin::Drag,
            kind: SelectionKind::Linear,
        };
        // Render both overlays into separate buffers and compare results.
        let mut buf_active = Buffer::empty(Rect::new(0, 0, 20, 4));
        let mut buf_persist = Buffer::empty(Rect::new(0, 0, 20, 4));
        render_active_selection_overlay(&model, &drag, None, &mut buf_active);
        render_persistent_selection_overlay(&model, &sel, None, &mut buf_persist);

        for y in 0..4u16 {
            for x in 0..20u16 {
                let a = buf_active.cell((x, y)).unwrap();
                let p = buf_persist.cell((x, y)).unwrap();
                assert_eq!((a.fg, a.bg), (p.fg, p.bg), "mismatch at ({x}, {y})");
            }
        }
    }

    // hit_test_text_exact tests

    #[test]
    fn hit_test_text_exact_returns_hit_on_selectable_column() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(ResolvedSelectableLine {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 0,
            screen_y: 5,
            screen_x: 4,
            selectable_cols: 2..10,
            text: "hello wo".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        });

        // Column 6 = screen_x(4) + selectable_cols.start(2) = within range.
        let hit = model.hit_test_text_exact(6, 5).unwrap();
        assert_eq!(hit.entry_idx, 0);
        assert_eq!(hit.range_id, 0);
        assert_eq!(hit.block_line_idx, 0);
        assert_eq!(hit.col_within_range, 0);

        // Column 10 = screen_x(4) + 6 → col_within_range = 4.
        let hit = model.hit_test_text_exact(10, 5).unwrap();
        assert_eq!(hit.col_within_range, 4);
    }

    #[test]
    fn hit_test_text_exact_returns_none_outside_selectable_cols() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(ResolvedSelectableLine {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 0,
            screen_y: 5,
            screen_x: 4,
            selectable_cols: 2..6,
            text: "body".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        });

        // Click on accent bar (col 4, which is screen_x but before selectable_cols.start).
        assert!(model.hit_test_text_exact(4, 5).is_none());
        assert!(model.hit_test_text_exact(5, 5).is_none());

        // Click past end of selectable cols (col 10 = 4 + 6).
        assert!(model.hit_test_text_exact(10, 5).is_none());

        // Wrong row.
        assert!(model.hit_test_text_exact(7, 4).is_none());
    }

    #[test]
    fn hit_test_text_exact_vs_nearest_range_comparison() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(ResolvedSelectableLine {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 0,
            screen_y: 3,
            screen_x: 10,
            selectable_cols: 2..6,
            text: "body".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        });

        // Click at col 11 (within screen_x but before selectable start 12).
        // Nearest-range should return a hit (clamped), exact should return None.
        let nearest = model.hit_test_selectable_range(11, 3);
        assert!(nearest.is_some());
        assert!(model.hit_test_text_exact(11, 3).is_none());

        // Click at col 12 (selectable start). Both should succeed.
        assert!(model.hit_test_selectable_range(12, 3).is_some());
        assert!(model.hit_test_text_exact(12, 3).is_some());
    }

    #[test]
    fn hit_test_text_exact_multiple_ranges_same_row() {
        let mut model = ResolvedSelectionModel::default();
        model.push_line(ResolvedSelectableLine {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 0,
            screen_y: 2,
            screen_x: 0,
            selectable_cols: 0..5,
            text: "hello".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        });
        model.push_line(ResolvedSelectableLine {
            entry_idx: 1,
            range_id: 1,
            block_line_idx: 0,
            screen_y: 2,
            screen_x: 10,
            selectable_cols: 0..5,
            text: "world".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        });

        // Hit on first range.
        let hit = model.hit_test_text_exact(2, 2).unwrap();
        assert_eq!(hit.entry_idx, 0);
        assert_eq!(hit.col_within_range, 2);

        // Gap between ranges — no exact hit.
        assert!(model.hit_test_text_exact(7, 2).is_none());

        // Hit on second range.
        let hit = model.hit_test_text_exact(12, 2).unwrap();
        assert_eq!(hit.entry_idx, 1);
        assert_eq!(hit.col_within_range, 2);
    }

    // ── Table-aware selection ────────────────────────────────────────────

    const TABLE_LINES: &[&str] = &[
        "┌─────────┬────────┐",
        "│ Name    │ Role   │",
        "├─────────┼────────┤",
        "│ Alice   │ Eng    │",
        "│ Smith   │        │",
        "├─────────┼────────┤",
        "│ Bob     │ Design │",
        "└─────────┴────────┘",
    ];

    fn table_text_at(i: usize) -> Option<String> {
        TABLE_LINES.get(i).map(|s| s.to_string())
    }

    fn table_geometry() -> TableGeometry {
        TableGeometry::detect(table_text_at, 3).expect("grid")
    }

    fn table_model() -> ResolvedSelectionModel {
        let mut model = ResolvedSelectionModel::default();
        for (i, text) in TABLE_LINES.iter().enumerate() {
            model.push_line(ResolvedSelectableLine {
                entry_idx: 0,
                range_id: 0,
                block_line_idx: i,
                screen_y: i as u16,
                screen_x: 0,
                selectable_cols: 0..(text.chars().count() as u16),
                text: text.to_string(),
                painted_region: None,
                joiner_to_previous: None,
            });
        }
        model
    }

    fn table_hit(block_line_idx: usize, col_within_range: u16) -> RangeHit {
        RangeHit {
            entry_idx: 0,
            range_id: 0,
            block_line_idx,
            col_within_range,
        }
    }

    fn table_drag(anchor: (usize, u16), head: (usize, u16), kind: SelectionKind) -> ActiveTextDrag {
        ActiveTextDrag {
            anchor: table_hit(anchor.0, anchor.1),
            head: table_hit(head.0, head.1),
            kind,
            anchor_content_width: None,
        }
    }

    #[test]
    fn resolve_kind_same_cell_vs_grid_vs_border() {
        let geom = table_geometry();
        let linear = SelectionKind::Linear;
        // Same cell (Name/Alice) → TableCell, even across its wrapped line.
        assert_eq!(
            resolve_table_drag_kind(Some(&geom), &table_hit(3, 3), &table_hit(4, 6), linear),
            SelectionKind::TableCell
        );
        // Reaching the Role column's content → TableGrid carrying that cell.
        assert_eq!(
            resolve_table_drag_kind(Some(&geom), &table_hit(3, 3), &table_hit(3, 14), linear),
            SelectionKind::TableGrid {
                anchor: CellRef { row: 1, col: 0 },
                head: CellRef { row: 1, col: 1 },
            }
        );
        // Head on the bottom border latches back to the anchor cell.
        assert_eq!(
            resolve_table_drag_kind(Some(&geom), &table_hit(6, 3), &table_hit(7, 3), linear),
            SelectionKind::TableCell
        );
        // Grid-line anchors stay Linear (the line-by-line escape hatch for
        // selecting the rendered table text); whole-table TSV is the
        // triple-click gesture instead.
        for anchor_line in [0, 2, 5, 7] {
            assert_eq!(
                resolve_table_drag_kind(
                    Some(&geom),
                    &table_hit(anchor_line, 3),
                    &table_hit(3, 3),
                    linear
                ),
                SelectionKind::Linear,
                "border anchor line {anchor_line}"
            );
        }
        // No geometry → Linear.
        assert_eq!(
            resolve_table_drag_kind(None, &table_hit(3, 3), &table_hit(4, 3), linear),
            SelectionKind::Linear
        );
    }

    #[test]
    fn resolve_kind_dead_zone_and_hysteresis() {
        let geom = table_geometry();
        let anchor = table_hit(3, 3); // Name/Alice cell
        let cell = SelectionKind::TableCell;
        // Junction column (10), own padding (9), and the neighbor's padding
        // (11) keep the cell selection — no escalation on a small overshoot.
        for col in [9, 10, 11] {
            assert_eq!(
                resolve_table_drag_kind(Some(&geom), &anchor, &table_hit(3, col), cell),
                SelectionKind::TableCell,
                "boundary col {col} must not escalate"
            );
        }
        // The divider row below keeps the cell selection too.
        assert_eq!(
            resolve_table_drag_kind(Some(&geom), &anchor, &table_hit(5, 3), cell),
            SelectionKind::TableCell
        );
        // Neighbor content escalates; boundary touches then keep the grid
        // (and its head cell) instead of flickering back.
        let grid = resolve_table_drag_kind(Some(&geom), &anchor, &table_hit(3, 14), cell);
        let anchor_cell = CellRef { row: 1, col: 0 };
        let head = CellRef { row: 1, col: 1 };
        assert_eq!(
            grid,
            SelectionKind::TableGrid {
                anchor: anchor_cell,
                head
            }
        );
        assert_eq!(
            resolve_table_drag_kind(Some(&geom), &anchor, &table_hit(3, 10), grid),
            SelectionKind::TableGrid {
                anchor: anchor_cell,
                head
            }
        );
        // Returning into the anchor cell's content de-escalates.
        assert_eq!(
            resolve_table_drag_kind(Some(&geom), &anchor, &table_hit(3, 5), grid),
            SelectionKind::TableCell
        );
    }

    #[test]
    fn reconstruct_cell_selection_joins_wrapped_fragments() {
        let geom = table_geometry();
        // Whole Name cell of the wrapped row (lines 3-4).
        let drag = table_drag((3, 1), (4, 9), SelectionKind::TableCell);
        assert_eq!(
            reconstruct_table_selection_text(&geom, &drag, table_text_at),
            Some("Alice Smith".to_string())
        );
        // Reversed endpoints produce the same text.
        let drag = table_drag((4, 9), (3, 1), SelectionKind::TableCell);
        assert_eq!(
            reconstruct_table_selection_text(&geom, &drag, table_text_at),
            Some("Alice Smith".to_string())
        );
        // Partial selection within the cell respects the columns
        // (cols 2..=4 of "│ Alice" are "Ali").
        let drag = table_drag((3, 2), (3, 4), SelectionKind::TableCell);
        assert_eq!(
            reconstruct_table_selection_text(&geom, &drag, table_text_at),
            Some("Ali".to_string())
        );
    }

    #[test]
    fn reconstruct_grid_selection_as_tsv() {
        let geom = table_geometry();
        // Column drag down the Name column: header row through Bob.
        let drag = table_drag(
            (1, 3),
            (6, 3),
            SelectionKind::TableGrid {
                anchor: CellRef { row: 0, col: 0 },
                head: CellRef { row: 2, col: 0 },
            },
        );
        assert_eq!(
            reconstruct_table_selection_text(&geom, &drag, table_text_at),
            Some("Name\nAlice Smith\nBob".to_string())
        );
        // Rectangle over both columns of the body rows (reversed endpoints).
        let drag = table_drag(
            (6, 14),
            (3, 3),
            SelectionKind::TableGrid {
                anchor: CellRef { row: 2, col: 1 },
                head: CellRef { row: 1, col: 0 },
            },
        );
        assert_eq!(
            reconstruct_table_selection_text(&geom, &drag, table_text_at),
            Some("Alice Smith\tEng\nBob\tDesign".to_string())
        );
        // Whole-table selection (grid-line anchor): every cell as TSV.
        let drag = table_drag(
            (2, 3),
            (3, 3),
            SelectionKind::TableGrid {
                anchor: CellRef { row: 0, col: 0 },
                head: CellRef { row: 2, col: 1 },
            },
        );
        assert_eq!(
            reconstruct_table_selection_text(&geom, &drag, table_text_at),
            Some("Name\tRole\nAlice Smith\tEng\nBob\tDesign".to_string())
        );
        // Empty cell lands as an empty TSV field (constructed grid state;
        // resolution itself would keep this same-cell drag a TableCell).
        let drag = table_drag(
            (3, 12),
            (4, 12),
            SelectionKind::TableGrid {
                anchor: CellRef { row: 1, col: 1 },
                head: CellRef { row: 1, col: 1 },
            },
        );
        assert_eq!(
            reconstruct_table_selection_text(&geom, &drag, table_text_at),
            Some("Eng".to_string())
        );
    }

    #[test]
    fn table_cell_copy_snaps_wide_graphemes() {
        const CJK_LINES: &[&str] = &[
            "┌────────┬───────┐",
            "│ 需要我 │ 帮你  │",
            "└────────┴───────┘",
        ];
        let text_at = |i: usize| CJK_LINES.get(i).map(|s| s.to_string());
        let geom = TableGeometry::detect(text_at, 1).expect("grid");

        // 需 spans [2, 4), 我 spans [6, 8): anchor on 需's second cell, head on 我's first cell.
        let drag = table_drag((1, 3), (1, 6), SelectionKind::TableCell);
        assert_eq!(
            reconstruct_table_selection_text(&geom, &drag, text_at),
            Some("需要我".to_string())
        );
    }

    #[test]
    fn endpoints_and_slice_agree_with_zero_width_graphemes() {
        let mut line = make_test_line(5, 0..4);
        line.text = "x\u{200B}界y".to_string();

        // 界 paints on cells [1, 3): a head on its second cell snaps the end to 3.
        let cols = selected_cols_for_endpoints(5, 0, 5, 2, &line).expect("in range");
        assert_eq!(cols, 0..3);

        // The slice keeps the ZWSP attached to 界 and excludes the trailing y.
        assert_eq!(
            crate::scrollback::types::slice_display_cols(&line.text, cols.start, cols.end),
            "x\u{200B}界"
        );
    }

    #[test]
    fn linear_drag_ignores_table_reconstruction() {
        let geom = table_geometry();
        let drag = table_drag((3, 3), (4, 3), SelectionKind::Linear);
        assert_eq!(
            reconstruct_table_selection_text(&geom, &drag, table_text_at),
            None
        );
    }

    #[test]
    fn table_overlay_paints_bands_not_borders() {
        let geom = table_geometry();
        let model = table_model();
        let area = Rect::new(0, 0, 25, 8);

        // Grid selection over both columns of the wrapped row + Bob row.
        let drag = table_drag(
            (3, 3),
            (6, 14),
            SelectionKind::TableGrid {
                anchor: CellRef { row: 1, col: 0 },
                head: CellRef { row: 2, col: 1 },
            },
        );
        let mut buf = Buffer::empty(area);
        paint_marker(&mut buf);
        let baseline = buf.clone();
        render_active_selection_overlay(&model, &drag, Some(&geom), &mut buf);

        // Cell interiors of selected rows are painted...
        assert!(
            cell_was_modified(&buf, &baseline, 2, 3),
            "Name band, Alice row"
        );
        assert!(
            cell_was_modified(&buf, &baseline, 12, 6),
            "Role band, Bob row"
        );
        assert!(
            cell_was_modified(&buf, &baseline, 2, 4),
            "Name band, wrapped row"
        );
        // ...while padding spaces and all-blank fragments are clipped out...
        assert!(
            !cell_was_modified(&buf, &baseline, 1, 3),
            "padding before Alice"
        );
        assert!(
            !cell_was_modified(&buf, &baseline, 8, 3),
            "padding after Alice"
        );
        assert!(
            !cell_was_modified(&buf, &baseline, 12, 4),
            "blank Role fragment of the wrapped row"
        );
        // ...and border columns, border rows, and unselected rows are not.
        for y in 0..8u16 {
            assert!(
                !cell_was_modified(&buf, &baseline, 0, y),
                "left border, y={y}"
            );
            assert!(
                !cell_was_modified(&buf, &baseline, 10, y),
                "mid border, y={y}"
            );
            assert!(
                !cell_was_modified(&buf, &baseline, 19, y),
                "right border, y={y}"
            );
        }
        for x in 0..20u16 {
            assert!(
                !cell_was_modified(&buf, &baseline, x, 0),
                "top border, x={x}"
            );
            assert!(
                !cell_was_modified(&buf, &baseline, x, 1),
                "header row, x={x}"
            );
            assert!(!cell_was_modified(&buf, &baseline, x, 2), "divider, x={x}");
            assert!(!cell_was_modified(&buf, &baseline, x, 5), "divider, x={x}");
            assert!(
                !cell_was_modified(&buf, &baseline, x, 7),
                "bottom border, x={x}"
            );
        }

        // A table-shaped selection with no geometry paints nothing.
        let mut buf2 = Buffer::empty(area);
        paint_marker(&mut buf2);
        render_active_selection_overlay(&model, &drag, None, &mut buf2);
        for y in 0..8u16 {
            for x in 0..25u16 {
                assert!(!cell_was_modified(&buf2, &baseline, x, y));
            }
        }
    }

    #[test]
    fn table_cell_overlay_clamps_to_band() {
        let geom = table_geometry();
        let model = table_model();
        let area = Rect::new(0, 0, 25, 8);

        // Whole Name cell on the wrapped row.
        let drag = table_drag((3, 1), (4, 9), SelectionKind::TableCell);
        let mut buf = Buffer::empty(area);
        paint_marker(&mut buf);
        let baseline = buf.clone();
        render_active_selection_overlay(&model, &drag, Some(&geom), &mut buf);

        assert!(
            cell_was_modified(&buf, &baseline, 2, 3),
            "band on fragment 1"
        );
        assert!(
            cell_was_modified(&buf, &baseline, 2, 4),
            "band on fragment 2"
        );
        assert!(
            !cell_was_modified(&buf, &baseline, 0, 3),
            "left border untouched"
        );
        assert!(
            !cell_was_modified(&buf, &baseline, 10, 3),
            "mid border untouched"
        );
        assert!(
            !cell_was_modified(&buf, &baseline, 12, 3),
            "other column untouched"
        );
        assert!(
            !cell_was_modified(&buf, &baseline, 2, 6),
            "other row untouched"
        );
        // Content clipping: the drag endpoints sit in the padding (cols 1
        // and 9) but only "Alice" / "Smith" glyph columns paint.
        assert!(
            !cell_was_modified(&buf, &baseline, 1, 3),
            "leading padding untouched"
        );
        assert!(
            !cell_was_modified(&buf, &baseline, 7, 4),
            "trailing padding untouched"
        );
    }

    #[test]
    fn clip_cols_to_content_trims_padding_and_blanks() {
        //        0123456789
        let text = "│ Quick Fox │";
        // Full band (1..12) clips to the content span, keeping the interior space.
        assert_eq!(clip_cols_to_content(text, 1..12), 2..11);
        // A sub-range starting in padding clips its leading edge only.
        assert_eq!(clip_cols_to_content(text, 1..7), 2..7);
        // All-blank ranges collapse to empty.
        assert_eq!(clip_cols_to_content("│        │", 1..9), 1..1);
        assert_eq!(clip_cols_to_content("", 0..5), 0..0);
        // Wide glyphs count display columns.
        assert_eq!(clip_cols_to_content("│ 名前 │", 1..6), 2..6);
    }
}
