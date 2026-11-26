//! Scroll-aware rendering for scrollback entries.

use std::sync::Arc;

use derive_more::{Deref, DerefMut};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::block::{BlockContent, RenderBlock};
use super::entry::ScrollbackEntry;
use super::layout::HorizontalLayout;
use super::state::EntryLayoutInfo;
use super::state::groups::{GroupKind, GroupSpan, span_containing};
use super::state::verb_group::{
    GroupHeaderLabel, truncation_header_label, verb_group_header_label,
};
use super::text_selection::{
    ResolvedSelectableLine, ResolvedSelectionBoundaries, ResolvedSelectionModel,
    VisibleBlockGeometry,
};
use super::types::{
    DisplayMode, derive_selection_text, line_plain_text_into, selectable_cols,
    selectable_cols_usize,
};
use super::wrappers::{EntryRenderer, group_header_chrome_prefix_width};
use crate::appearance::AppearanceConfig;
use crate::render::Renderable;
use crate::render::osc8::{LinkOverlay, OverlayLink};
use crate::theme::Theme;

/// `range_id` of a labeled group header's synthetic selectable row — verb-run
/// headers and truncation headers carrying an aggregated label. Reserved at
/// the top of the id space: block `selection_range` ids count up from 0, and
/// in the EXPANDED verb slot member 0's own line 0 (range 0, block line 0) is
/// mapped alongside the header — a shared id would merge both rows into one
/// selectable range, so a drag on either row selected and copied both.
pub(crate) const GROUP_HEADER_RANGE_ID: u16 = u16::MAX;

/// Label for the inline-media native-open text button (terminals without
/// inline graphics). Graphics terminals use a shorter overlay `[Open]` instead.
pub fn media_open_button_label(is_video: bool) -> &'static str {
    if is_video {
        "[Open Video]"
    } else {
        "[Open Image]"
    }
}

/// Centered left column for the `[Open]` text button. Shared by the renderer
/// and the hit-area computation so the label and click target stay aligned.
pub fn media_open_button_col(content_width: u16, is_video: bool) -> u16 {
    let label_w = media_open_button_label(is_video).len() as u16;
    content_width.saturating_sub(label_w) / 2
}

/// Width reserved for the timestamp on message blocks.
///
/// Matches the constant in `EntryRenderer::timestamp_reserved()`.
fn timestamp_reserved_for_block(block: &RenderBlock, appearance: &AppearanceConfig) -> u16 {
    if appearance.show_timestamps
        && matches!(
            block,
            RenderBlock::UserPrompt(_) | RenderBlock::AgentMessage(_) | RenderBlock::Btw(_)
        )
    {
        10
    } else {
        0
    }
}

/// A reusable scratch buffer for rendering clipped entries.
///
/// This wraps ratatui's `Buffer` with `Deref`/`DerefMut` so all standard
/// Buffer methods work directly. The wrapper exists to:
/// - Make scratch buffer usage greppable in the codebase
/// - Provide a place for future optimization methods (bulk copy, fill, etc.)
///
/// # Usage
///
/// Create once and reuse across frames:
/// ```ignore
/// let mut scratch = ScratchBuffer::new();
/// // In render loop:
/// scratch.prepare(width, height);
/// // ... use scratch for rendering ...
/// ```
///
/// # Future Enhancements
///
/// ratatui's Buffer has limitations we could address:
/// - `resize()` always reallocates when growing (no capacity tracking)
/// - No efficient `fill()` - could use `vec.fill()` instead of cell-by-cell
/// - No bulk copy operations
#[derive(Default, Deref, DerefMut)]
pub struct ScratchBuffer(Buffer);

impl ScratchBuffer {
    /// Create a new empty scratch buffer.
    pub fn new() -> Self {
        Self(Buffer::default())
    }

    /// Resize and reset buffer for reuse.
    ///
    /// We must reset because `set_style()` only changes style, not content.
    /// If previous content was longer than new content, old chars would remain.
    /// TODO: When we fork Buffer, use `vec.fill(Cell::default())` for efficiency.
    pub fn prepare(&mut self, width: u16, height: u16) {
        self.resize(Rect::new(0, 0, width, height));
        self.reset();
    }

    /// Prepare and return mutable reference (convenience for chaining).
    pub fn prepared(&mut self, width: u16, height: u16) -> &mut Self {
        self.prepare(width, height);
        self
    }
}

/// A visible inline media entry with its screen position and crop info.
#[derive(Debug, Clone)]
pub struct InlineMediaPlacement {
    /// Media metadata (path, dimensions, type).
    pub info: crate::prompt_images::InlineMediaInfo,
    /// Screen rect where the visible portion of the image is rendered.
    pub screen_rect: ratatui::layout::Rect,
    /// Total image rows when fully visible (for crop calculation).
    pub full_rows: u16,
    /// Number of rows cropped from the top (0 = no crop).
    pub top_crop_rows: u16,
    /// Screen rect of the filepath line (second line of the media block header),
    /// if visible. Used for click-to-copy hit testing.
    pub filepath_screen_rect: Option<ratatui::layout::Rect>,
    /// Screen rect of the text `[Open]` button line, if visible. Present only
    /// for text-button placements (media on terminals without inline-graphics
    /// support; `full_rows` is 0). Used for click-to-open-natively hit testing.
    pub open_button_screen_rect: Option<ratatui::layout::Rect>,
    /// Whether this placement reserves a trailing `[Open]/[Copy]` (or play)
    /// button row beneath the image. True for an overlay/image tool-media
    /// placement; false for the text-`[Open]` placement (terminals without
    /// inline graphics), whose button is the `[Open]` text line itself.
    pub has_button_row: bool,
}

/// A visible Mermaid diagram affordance row with its screen position and the
/// diagram source its buttons act on. The draw loop paints
/// `◇ mermaid [Open Image] [Copy Image Path] [Copy Source]` onto `screen_rect` and registers
/// the click hit-rects; the reserved (blank) row already scrolls with the
/// surrounding content. Rendering is lazy (driven from the source on click), so
/// no rendered path/state is carried here.
#[derive(Debug, Clone)]
pub struct DiagramAffordancePlacement {
    /// Screen rect of the affordance row (one row tall, content-area width).
    pub screen_rect: ratatui::layout::Rect,
    /// Diagram source (the fence body); the data every button acts on.
    pub source: String,
}

/// Result of rendering entries.
#[derive(Debug, Clone, Default)]
pub struct ScrollRenderResult {
    /// Virtual-y end of the passed slice: `content_y0` + heights + gaps of the
    /// entries given to the renderer. Equals the full content height only when
    /// the caller passes the full list from content top; the production
    /// windowed caller ignores it and uses `prepare_layout()`'s total.
    /// `usize` so tall sessions (> `u16::MAX` rows) are not truncated.
    pub total_height: usize,
    /// Area occupied by the selected entry (if visible).
    /// This is used for drawing selection borders.
    /// None if selected entry is not visible or partially clipped.
    pub selected_area: Option<SelectedEntryArea>,
    /// Per-frame resolved selection metadata for visible content.
    pub selection_model: ResolvedSelectionModel,
    /// Accumulated link overlay for OSC 8 post-flush emission.
    pub link_overlay: LinkOverlay,
    /// Inline media to render via post-flush escape sequences.
    pub inline_media: Vec<InlineMediaPlacement>,
    /// Diagram affordance rows to paint + register click hit-rects for.
    pub diagram_affordances: Vec<DiagramAffordancePlacement>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ScrollRenderResultWithBoundaries {
    pub(crate) result: ScrollRenderResult,
    pub(crate) selection_boundaries: ResolvedSelectionBoundaries,
}

/// Information about the selected entry's visible area.
#[derive(Debug, Clone)]
pub struct SelectedEntryArea {
    /// The area where the entry was rendered.
    pub area: Rect,
    /// Whether the top of the entry is clipped (don't draw top border).
    pub top_clipped: bool,
    /// Whether the bottom of the entry is clipped (don't draw bottom border).
    pub bottom_clipped: bool,
}

/// Render entries with scroll support.
///
/// # Parameters
/// - `entry_layouts_cache`: Pre-computed layout info (height + gap_after) for each entry.
///   Must be same length as `entries`. Comes from the LayoutCache populated by `prepare_layout()`.
/// - `tick`: Animation tick counter for animated elements (e.g., running block accents).
/// - `content_y0`: Virtual-Y of `entries[0]` in scroll-offset space (0 when the
///   slice starts at content top). Callers that pass a viewport-only window set
///   this from the layout cache so off-screen history is not re-walked here.
/// - `entry_index_base`: Added to each slice index for selection-model indices and
///   `selected_idx` / `dim_from_entry` matching (relative to the caller's full
///   visible range, not the paint window).
/// - `group_spans`: The fold model from the layout cache plus the absolute
///   entry index of `entries[0]`, used to bound verb-group header labels to
///   exactly the folded run and to build the aggregated labels on truncation
///   headers. `None` (harnesses without a layout pass) falls back to the
///   verb label walk's own run classification and to the plain "N more" /
///   "N tool calls & thoughts" truncation text.
/// - `cwd`: Session/worktree cwd used for path-aware measurement and paint.
///
/// # Panics
/// Debug-asserts if `entry_layouts_cache.len() != entries.len()`.
#[allow(clippy::too_many_arguments)]
pub fn render_scrolled_entries_with_scratch(
    buf: &mut Buffer,
    viewport: Rect,
    entries: &[&ScrollbackEntry],
    scroll_offset: usize,
    selected_idx: Option<usize>,
    theme: &Theme,
    appearance: &AppearanceConfig,
    entry_layouts_cache: &[EntryLayoutInfo],
    tick: u64,
    mouse_pos: Option<(u16, u16)>,
    dim_from_entry: Option<usize>,
    search_highlight: Option<&regex::Regex>,
    content_y0: usize,
    entry_index_base: usize,
    // Absolute paths of media generated in this transcript, used to resolve the
    // short relative paths the model prints (`images/1.jpg`) to clickable links.
    media_paths: &[std::path::PathBuf],
    group_spans: Option<(&[GroupSpan], usize)>,
    cwd: Option<&std::path::Path>,
) -> ScrollRenderResult {
    render_scrolled_entries_with_selection_boundaries(
        buf,
        viewport,
        entries,
        scroll_offset,
        selected_idx,
        theme,
        appearance,
        entry_layouts_cache,
        tick,
        mouse_pos,
        dim_from_entry,
        search_highlight,
        content_y0,
        entry_index_base,
        media_paths,
        group_spans,
        cwd,
    )
    .result
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_scrolled_entries_with_selection_boundaries(
    buf: &mut Buffer,
    viewport: Rect,
    entries: &[&ScrollbackEntry],
    scroll_offset: usize,
    selected_idx: Option<usize>,
    theme: &Theme,
    appearance: &AppearanceConfig,
    entry_layouts_cache: &[EntryLayoutInfo],
    tick: u64,
    mouse_pos: Option<(u16, u16)>,
    dim_from_entry: Option<usize>,
    search_highlight: Option<&regex::Regex>,
    content_y0: usize,
    entry_index_base: usize,
    media_paths: &[std::path::PathBuf],
    group_spans: Option<(&[GroupSpan], usize)>,
    cwd: Option<&std::path::Path>,
) -> ScrollRenderResultWithBoundaries {
    if entries.is_empty() || viewport.width == 0 || viewport.height == 0 {
        return ScrollRenderResultWithBoundaries::default();
    }

    let mut result = ScrollRenderResult::default();
    let mut selection_boundaries = ResolvedSelectionBoundaries::default();

    debug_assert_eq!(
        entry_layouts_cache.len(),
        entries.len(),
        "entry_layouts_cache must match entries length - was prepare_layout() called?"
    );

    // Create horizontal layout for this viewport using config
    let layout = HorizontalLayout::new(viewport, &appearance.scrollback.layout);

    // Total height = y of first passed entry + span of this slice (incl. gaps).
    // Use usize so tall sessions are never truncated. When the caller
    // passes a viewport window, this is only the window's end — production uses
    // prepare_layout()'s total; full-slice callers (tests) get the true total.
    result.total_height = content_y0
        + entry_layouts_cache
            .iter()
            .map(|l| l.height as usize + l.gap_after as usize)
            .sum::<usize>();

    let viewport_start = scroll_offset;
    let viewport_end = scroll_offset + viewport.height as usize;

    result.selection_model.content_area = layout.content;

    // Reused across all visible rows so the search-highlight pass allocates at
    // most once per frame (not once per row). Empty until search is active.
    let mut highlight_text = String::new();

    // Walk only the passed slice (viewport window in production). No full-list
    // EntryLayout vec — y advances from content_y0 using cached heights/gaps.
    let mut y = content_y0;
    for (i, entry) in entries.iter().enumerate() {
        let height = entry_layouts_cache[i].height;
        let entry_start = y;
        let entry_end = entry_start + height as usize;

        // Skip if completely above viewport
        if entry_end <= viewport_start {
            y = entry_end + entry_layouts_cache[i].gap_after as usize;
            continue;
        }
        // Stop if completely below viewport
        if entry_start >= viewport_end {
            break;
        }

        let logical_idx = i + entry_index_base;

        // Calculate visibility
        let top_clipped = entry_start < viewport_start;
        let bottom_clipped = entry_end > viewport_end;

        // Calculate render position (narrowed to u16 for screen coordinates —
        // these deltas are always within viewport height which fits in u16).
        let render_y: u16 = if top_clipped {
            viewport.y
        } else {
            viewport.y + (entry_start - viewport_start) as u16
        };

        // Calculate visible height
        let skip_rows: u16 = if top_clipped {
            (viewport_start - entry_start) as u16
        } else {
            0
        };
        let visible_height: u16 = if bottom_clipped {
            (viewport_end - entry_start.max(viewport_start)) as u16
        } else {
            height - skip_rows
        };

        // Calculate actual render height (clamped to viewport)
        let render_height = visible_height.min(viewport.height);

        if render_height == 0 {
            y = entry_end + entry_layouts_cache[i].gap_after as usize;
            continue;
        }

        // Create the area for this entry's content
        let entry_row_layout = layout.for_row(render_y, render_height);
        let entry_content_area = entry_row_layout.entry_content_area();

        // Render the entry — skip_rows handles partial visibility directly
        let is_selected = selected_idx == Some(logical_idx);
        let entry_layout_info = &entry_layouts_cache[i];
        // Group headers rebuild their aggregated label each frame so counts/
        // tense/live target track the streaming run in place. Both fold
        // families feed the one label channel; a header row belongs to
        // exactly one fold, so the branches are exclusive by construction.
        //
        // Verb-group headers: the fold's span bounds the walk to exactly the
        // claimed run; without spans the walk stops at its own run-breaker
        // classification.
        let header_label = if entry_layout_info.verb_group_header {
            let show_thinking = crate::appearance::cache::load_show_thinking_blocks();
            let end = group_spans
                .and_then(|(spans, base)| {
                    let span = span_containing(spans, base + i)?;
                    Some(span.range.end.saturating_sub(base))
                })
                .unwrap_or(entries.len());
            Some(GroupHeaderLabel::VerbRun(verb_group_header_label(
                entries,
                i,
                end,
                show_thinking,
                theme,
            )))
        } else if entry_layout_info.is_group_header()
            && crate::appearance::cache::load_group_tool_verbs()
        {
            // Truncation headers get the same aggregated vocabulary over the
            // rows they hide (prefix only while collapsed; the whole run when
            // expanded), gated on the "Group tool calls" setting that owns
            // this vocabulary. Falls back to the renderer's plain "N more"
            // count when the setting is off, spans are absent, or the walk
            // declines (pure thoughts, or hidden rows it cannot name).
            let show_thinking = crate::appearance::cache::load_show_thinking_blocks();
            group_spans
                .and_then(|(spans, base)| {
                    let span = span_containing(spans, base + i)?;
                    let GroupKind::Truncation { hidden, .. } = span.kind else {
                        return None;
                    };
                    let start = span.range.start.saturating_sub(base);
                    let end = span.range.end.saturating_sub(base);
                    truncation_header_label(
                        entries,
                        start..end,
                        (!span.expanded).then_some(hidden),
                        show_thinking,
                        theme,
                    )
                })
                .map(GroupHeaderLabel::Truncation)
        } else {
            None
        };
        let renderer = EntryRenderer::new(entry, theme)
            .with_appearance_ref(appearance)
            .with_tick(tick)
            .with_skip_rows(skip_rows)
            .with_groupable(entry.block.is_groupable())
            .with_selected(is_selected)
            .with_mouse_pos(mouse_pos)
            .with_group_header_count(entry_layout_info.group_header_count)
            .with_group_collapse_header(entry_layout_info.group_collapse_header)
            .with_group_header_label(header_label.as_ref())
            .with_cwd(cwd);
        renderer.render(entry_content_area, buf);

        if dim_from_entry.is_some_and(|d| logical_idx >= d) {
            let dim_fg = theme.gray_dim;
            for cy in entry_content_area.y..entry_content_area.y + entry_content_area.height {
                for cx in entry_content_area.x..entry_content_area.x + entry_content_area.width {
                    if let Some(cell) = buf.cell_mut((cx, cy)) {
                        cell.fg = dim_fg;
                    }
                }
            }
        }

        // Use cached output for selection model building.
        // EntryRenderer::render() above already populated the cache for non-selected
        // entries. For selected entries, ensure_cached() will compute and cache the
        // output once. This avoids a redundant block.output() call (which includes
        // expensive syntax highlighting for edit blocks) that was previously done
        // via effective_output() on every frame.
        //
        // Must use the same effective width as the renderer (reduced by timestamp
        // reservation for message blocks) to avoid cache thrashing.
        let ts_reserved = timestamp_reserved_for_block(&entry.block, appearance);
        let content_width = entry_row_layout.content_width().saturating_sub(ts_reserved);
        entry.ensure_cached(content_width, appearance, is_selected, cwd);
        let cached_rendered = entry.cached_rendered_output_ref();
        let cached_output = &cached_rendered.output;
        let cached_boundaries = &cached_rendered.boundaries;

        result
            .selection_model
            .visible_blocks
            .push(VisibleBlockGeometry {
                entry_idx: logical_idx,
                area: entry_row_layout.entry_area(),
                content_area: entry_row_layout.content,
                selection_area: entry_row_layout.selection_area(),
                content_width,
                top_clipped,
                bottom_clipped,
                drag_startable: entry.block.is_drag_block_selectable(),
            });

        let ctx = entry.context(content_width, appearance, cwd);
        let has_vpad = entry.block.has_vpad(&ctx);
        let vpad_top = if has_vpad { 1u16 } else { 0 };
        let content_skip = skip_rows.saturating_sub(vpad_top) as usize;
        let first_visible_content_y = render_y + if skip_rows < vpad_top { 1 } else { 0 };
        let max_y = render_y + render_height;

        // Group-header entries draw synthetic "N more" text instead of
        // `cached_output.lines` (the truncation fold forces height 1), so
        // every pass mapping content lines or geometry to screen rows skips
        // them. The EXPANDED verb-group slot is the exception: its header line
        // sits above member 0's own content, which stays mapped (selectable
        // like every other member row) one row below.
        let is_group_header = entry_layout_info.is_group_header();
        let verb_expanded_slot = entry_layout_info.is_expanded_verb_header();

        let mapped_lines = if is_group_header && !verb_expanded_slot {
            &[][..]
        } else {
            &cached_output.lines[..]
        };

        // Expanded verb-group slot: the header consumes the slot's first
        // screen row, so member 0's content maps one row below (mirroring
        // EntryRenderer's collapse-header render path: when the header is
        // scrolled off, the first skipped row is the header, not content).
        // Shared by the selection lines, the hyperlink map, and the URL
        // scanner below — they all read these two offsets.
        let (first_visible_content_y, content_skip) = if verb_expanded_slot {
            if skip_rows == 0 {
                (first_visible_content_y + 1, content_skip)
            } else {
                (first_visible_content_y, content_skip.saturating_sub(1))
            }
        } else {
            (first_visible_content_y, content_skip)
        };
        let mut screen_y = first_visible_content_y;

        // Labeled group header (either fold family): one synthetic selectable
        // row so drag/copy on the header yields the aggregated label text.
        // Plain-count headers carry no label and stay non-selectable.
        if let Some(header) = &header_label
            && skip_rows == 0
            && render_y < max_y
        {
            let label = header.label();
            // Every labeled header draws the diamond chrome before the label;
            // shift the hitbox onto the label glyphs so highlight matches
            // the copied text (the chrome is affordance, not content).
            let chrome_offset = group_header_chrome_prefix_width();
            result.selection_model.push_line(ResolvedSelectableLine {
                entry_idx: logical_idx,
                range_id: GROUP_HEADER_RANGE_ID,
                block_line_idx: 0,
                screen_y: render_y,
                screen_x: entry_row_layout.content.x.saturating_add(chrome_offset),
                // Painted-cell width (per grapheme), matching the reorder/hit map.
                selectable_cols: 0..(crate::scrollback::types::str_display_cells(&label.text)
                    as u16),
                text: label.text.clone(),
                painted_region: None,
                joiner_to_previous: None,
            });
        }
        for (block_line_idx, line) in mapped_lines.iter().enumerate().skip(content_skip) {
            if screen_y >= max_y {
                break;
            }
            // Search highlight: re-run the query regex over this rendered row
            // and invert matching cells (decoupled from the source-text index,
            // mirroring `list_pane`). Each `BlockLine` is one already-wrapped
            // screen row, so the single-row paint path applies.
            //
            // The haystack is the rendered glyphs, not the indexed source text,
            // so the highlighted set can diverge from the index match set: a
            // match split across a soft-wrap boundary highlights on neither row,
            // and markdown markers present in source but absent on screen won't
            // highlight. This is the intended decoupling — navigation/counting
            // use the index; on-screen highlighting follows what's drawn.
            if let Some(re) = search_highlight {
                highlight_text.clear();
                line_plain_text_into(&line.content, &mut highlight_text);
                crate::render::highlight::paint_match_highlights(
                    buf,
                    entry_row_layout.content,
                    screen_y,
                    max_y,
                    0,
                    0,
                    &highlight_text,
                    re,
                    true,
                    // Scrollback content paints bidi-reordered; remap matches.
                    true,
                );
            }
            if let (Some(range_id), Some(cols)) = (
                line.selection_range,
                selectable_cols(&line.content, &line.selectable),
            ) {
                // Logical text; drag columns are visual and remapped on copy
                // via `logical_slice_for_visual_cols`.
                let resolved_line = ResolvedSelectableLine {
                    entry_idx: logical_idx,
                    range_id,
                    block_line_idx,
                    screen_y,
                    screen_x: entry_row_layout.content.x,
                    // Visual span so the hit box matches the reordered cells
                    // even when a non-selectable suffix shifts the region.
                    selectable_cols: crate::scrollback::types::visual_selectable_cols(line)
                        .unwrap_or(cols),
                    text: derive_selection_text(line),
                    painted_region: Some(crate::scrollback::types::painted_selectable_region(line)),
                    joiner_to_previous: line.joiner.clone(),
                };
                if let Some(boundary) = cached_boundaries.get(block_line_idx) {
                    selection_boundaries.push(&resolved_line, Arc::clone(boundary));
                }
                result.selection_model.push_line(resolved_line);
            }
            screen_y += 1;
        }

        // Collect hyperlinks for the link overlay. Group headers render
        // synthetic label text with no links; the expanded verb slot's member
        // row keeps its links (row offsets already shifted past the header).
        if !is_group_header || verb_expanded_slot {
            let content_line_offset = match &entry.block {
                RenderBlock::Btw(_) if ctx.mode != DisplayMode::Collapsed => 2,
                RenderBlock::Thinking(_)
                    if ctx.mode != DisplayMode::Collapsed
                        && appearance.scrollback.blocks.thinking.header =>
                {
                    2
                }
                _ => 0,
            };
            entry.block.with_hyperlinks(|hyperlinks| {
                if !hyperlinks.is_empty() {
                    map_hyperlinks_to_overlay(
                        hyperlinks,
                        cached_output,
                        content_skip,
                        first_visible_content_y,
                        max_y,
                        entry_row_layout.content.x,
                        content_line_offset,
                        media_paths,
                        cwd,
                        &mut result.link_overlay,
                    );
                }
            });

            // Basename/relative tool headers need the stored absolute target.
            // Hit box = selectable path span (respects bullet prepend + Selectable shift).
            {
                for (idx, bl) in cached_output.lines.iter().enumerate().skip(content_skip) {
                    let visible_offset = (idx - content_skip) as u16;
                    let screen_row = first_visible_content_y + visible_offset;
                    if screen_row >= max_y {
                        break;
                    }
                    let Some(target) = bl.link_target.as_ref() else {
                        continue;
                    };
                    let Some(cols) = selectable_cols_usize(&bl.content, &bl.selectable) else {
                        continue;
                    };
                    let visible_width =
                        usize::from(content_width.min(entry_row_layout.content.width));
                    let start = cols.start.min(visible_width);
                    let end = cols.end.min(visible_width);
                    if start >= end {
                        continue;
                    }
                    let painted = derive_selection_text(bl);
                    let fully_visible = cols.end <= visible_width;
                    // The row paints bidi-reordered when rtl_bidi is on, so map
                    // the logical path span to its visual cell range(s) — the
                    // hit box and OSC 8 underline must sit on the drawn glyphs.
                    // Identity (one range) under LTR / no reorder.
                    let plain = crate::scrollback::types::line_plain_text(&bl.content);
                    for (vs, ve) in crate::render::bidi::logical_cols_to_visual(&plain, start, end)
                    {
                        let (Ok(vs), Ok(ve)) = (u16::try_from(vs), u16::try_from(ve)) else {
                            continue;
                        };
                        let (Some(col_start), Some(col_end)) = (
                            entry_row_layout.content.x.checked_add(vs),
                            entry_row_layout.content.x.checked_add(ve),
                        ) else {
                            continue;
                        };
                        if result.link_overlay.overlaps(screen_row, col_start, col_end) {
                            continue;
                        }
                        result.link_overlay.push(OverlayLink {
                            screen_row,
                            col_start,
                            col_end,
                            target: target.clone(),
                            presentation: if fully_visible {
                                crate::render::osc8::file_link_presentation(&painted, target, cwd)
                            } else {
                                crate::render::osc8::LinkPresentation::Opaque
                            },
                            id: None,
                        });
                    }
                }
            }

            // Scan post-wrap lines for plain-text URLs and file paths.
            // For markdown blocks, markdown hyperlinks are already in the
            // overlay (mapped above); explicit tool-link rows are authoritative.
            {
                let visible_lines = cached_output
                    .lines
                    .iter()
                    .enumerate()
                    .skip(content_skip)
                    .filter(|(_, bl)| bl.link_target.is_none())
                    .map(|(idx, bl)| {
                        let visible_offset = (idx - content_skip) as u16;
                        let screen_row = first_visible_content_y + visible_offset;
                        (screen_row, &bl.content, bl.joiner.as_deref())
                    })
                    .take_while(|(screen_row, _, _)| *screen_row < max_y);

                crate::render::osc8::scan_lines_for_url_overlays(
                    visible_lines,
                    entry_row_layout.content.x,
                    media_paths,
                    &mut result.link_overlay,
                );
            }
        }

        // Collect inline media placements for visible media. Each media block
        // (tool media only) yields one trailing placement anchored at its own
        // `row_offset`. Partial visibility crops top/bottom so the image slides
        // into/out of view during scrolling.
        // Member 0's content starts one virtual row below the slot's header
        // line; every virtual anchor below offsets from here.
        let content_y_start = entry_start + usize::from(verb_expanded_slot);
        let media_placements = if is_group_header && !verb_expanded_slot {
            Vec::new()
        } else {
            entry.block.inline_media_placements(&ctx)
        };
        for placement in media_placements {
            let image_offset = placement.row_offset as usize;
            let full_image_h = placement.rows as usize;
            let image_virtual_start = content_y_start + image_offset;
            let image_virtual_end = image_virtual_start + full_image_h;
            let viewport_bottom = viewport_start + viewport.height as usize;
            // Keep the image clear of the right-aligned timestamp overlay
            // (message blocks reserve trailing columns for it); tool blocks
            // reserve 0, so this is a no-op there.
            let media_width = entry_content_area.width.saturating_sub(ts_reserved);

            // Check if any part of the image area is visible (height 1 = hint-only banner).
            if image_virtual_start < viewport_bottom
                && image_virtual_end > viewport_start
                && full_image_h >= 1
                && media_width >= 4
            {
                // Compute visible portion, cropping top and bottom.
                // Results are narrowed to u16 — they are viewport-relative
                // offsets that always fit in screen coordinates.
                let top_crop = viewport_start.saturating_sub(image_virtual_start) as u16;
                let visible_start = image_virtual_start.max(viewport_start);
                let visible_end = image_virtual_end.min(viewport_bottom);
                let visible_h = visible_end.saturating_sub(visible_start) as u16;
                let screen_y = viewport.y + (visible_start - viewport_start) as u16;

                if visible_h >= 1 {
                    // Tool media exposes its second output line as the
                    // click-to-copy filepath and reserves a button row.
                    let filepath_virtual_y = content_y_start + 1;
                    let filepath_screen_rect = if filepath_virtual_y >= viewport_start
                        && filepath_virtual_y < viewport_bottom
                    {
                        Some(ratatui::layout::Rect {
                            x: entry_content_area.x,
                            y: viewport.y + (filepath_virtual_y - viewport_start) as u16,
                            width: entry_content_area.width,
                            height: 1,
                        })
                    } else {
                        None
                    };

                    result.inline_media.push(InlineMediaPlacement {
                        info: placement.info,
                        screen_rect: ratatui::layout::Rect {
                            x: entry_content_area.x,
                            y: screen_y,
                            width: media_width,
                            height: visible_h,
                        },
                        full_rows: full_image_h as u16,
                        top_crop_rows: top_crop,
                        filepath_screen_rect,
                        open_button_screen_rect: None,
                        has_button_row: true,
                    });
                }
            }
        }

        // Diagram affordance rows: map each block-relative reserved row to a
        // screen rect when visible. The blank row already scrolls with the
        // content; the draw loop paints the buttons + registers click hit-rects.
        // Agent messages (the only producer) have no top vpad, so `row_offset`
        // is measured straight from `y_start`, like inline media above.
        // Header gate unreachable today (producers are agent messages — run
        // breakers, so never verb-group members either); structural.
        let diagram_affordances = if is_group_header {
            Vec::new()
        } else {
            entry.block.diagram_affordances(&ctx)
        };
        for aff in diagram_affordances {
            let virtual_y = entry_start + aff.row_offset as usize;
            let viewport_bottom = viewport_start + viewport.height as usize;
            if virtual_y >= viewport_start && virtual_y < viewport_bottom {
                result.diagram_affordances.push(DiagramAffordancePlacement {
                    screen_rect: ratatui::layout::Rect {
                        x: entry_row_layout.content.x,
                        y: viewport.y + (virtual_y - viewport_start) as u16,
                        width: content_width,
                        height: 1,
                    },
                    source: aff.source,
                });
            }
        }

        // Click targets for the text `[Open]` button + filepath of media blocks
        // without an inline overlay (terminals without inline-graphics support).
        if (!is_group_header || verb_expanded_slot)
            && let Some((open_path, is_video)) = entry.block.inline_open_button()
        {
            let content_lines = cached_output.lines.len();
            let viewport_bottom = viewport_start + viewport.height as usize;

            // Virtual-y coordinates are usize (tall scrollback); the resulting
            // screen y is a viewport-relative offset that fits in u16.
            let line_screen_rect =
                |virtual_y: usize, width: u16| -> Option<ratatui::layout::Rect> {
                    if virtual_y >= viewport_start && virtual_y < viewport_bottom {
                        Some(ratatui::layout::Rect {
                            x: entry_content_area.x,
                            y: viewport.y + (virtual_y - viewport_start) as u16,
                            width,
                            height: 1,
                        })
                    } else {
                        None
                    }
                };

            // Filepath line (index 1) → click-to-copy.
            let filepath_screen_rect =
                line_screen_rect(content_y_start + 1, entry_content_area.width);

            // Centered `[Open]` button → click-to-open. It is the second-to-last
            // content line (the last line is a blank spacer).
            let open_button_screen_rect = if content_lines >= 2 {
                let label_w = media_open_button_label(is_video).len() as u16;
                let col = media_open_button_col(content_width, is_video);
                let button_virtual_y = content_y_start + (content_lines - 2);
                line_screen_rect(button_virtual_y, label_w).map(|mut rect| {
                    rect.x = rect.x.saturating_add(col);
                    rect
                })
            } else {
                None
            };

            if filepath_screen_rect.is_some() || open_button_screen_rect.is_some() {
                result.inline_media.push(InlineMediaPlacement {
                    info: crate::prompt_images::InlineMediaInfo {
                        path: open_path,
                        width: 0,
                        height: 0,
                        is_video,
                        alt_text: String::new(),
                    },
                    screen_rect: ratatui::layout::Rect {
                        x: entry_content_area.x,
                        y: viewport.y,
                        width: 0,
                        height: 0,
                    },
                    full_rows: 0,
                    top_crop_rows: 0,
                    filepath_screen_rect,
                    open_button_screen_rect,
                    // Text-button placement: the button is the text [Open] line
                    // (`open_button_screen_rect`), not an image-overlay row.
                    has_button_row: false,
                });
            }
        }

        // Track selected entry
        if selected_idx == Some(logical_idx) {
            let selection_area = entry_row_layout.selection_area();
            result.selected_area = Some(SelectedEntryArea {
                area: selection_area,
                top_clipped,
                bottom_clipped,
            });
        }

        y = entry_end + entry_layouts_cache[i].gap_after as usize;
    }

    ScrollRenderResultWithBoundaries {
        result,
        selection_boundaries,
    }
}

use super::types::BlockOutput;

/// Map pre-wrap `HyperlinkTarget`s to screen-space `OverlayLink`s.
///
/// Walks the post-wrap `BlockOutput` lines, using joiner metadata to
/// reconstruct the pre-wrap → post-wrap line mapping.  For each
/// hyperlink, finds the wrapped line(s) it overlaps and emits an
/// `OverlayLink` with the correct screen row and column offsets.
///
/// `content_line_offset` accounts for non-markdown header lines prepended
/// by block types like `BtwBlock` (header + separator) and `ThinkingBlock`
/// (header + blank when the header config is enabled).
///
/// Also used by the `/btw` inline panel (no header offset — pure markdown body).
#[allow(clippy::too_many_arguments)]
pub(crate) fn map_hyperlinks_to_overlay(
    hyperlinks: &[echo_agent_markdown::HyperlinkTarget],
    block_output: &BlockOutput,
    content_skip: usize,
    first_visible_screen_y: u16,
    max_screen_y: u16,
    content_x: u16,
    content_line_offset: usize,
    media_paths: &[std::path::PathBuf],
    cwd: Option<&std::path::Path>,
    overlay: &mut LinkOverlay,
) {
    // Build mapping: pre-wrap line index → vec of (wrapped_idx, col_start_in_prewrap, col_end_in_prewrap).
    // A joiner of None means a new pre-wrap line starts.
    let mut pre_wrap_segments: Vec<Vec<(usize, usize, usize)>> = Vec::new();
    let mut current_segments: Vec<(usize, usize, usize)> = Vec::new();
    let mut cumulative_col: usize = 0;

    for (wrapped_idx, line) in block_output.lines.iter().enumerate() {
        if line.joiner.is_none() && !current_segments.is_empty() {
            pre_wrap_segments.push(std::mem::take(&mut current_segments));
            cumulative_col = 0;
        }
        // Joiner represents the whitespace consumed at the wrap point —
        // it occupies display columns in the pre-wrap line but doesn't
        // appear in either wrapped line. Add BEFORE this segment so
        // the column mapping stays aligned.
        if let Some(ref joiner) = line.joiner {
            cumulative_col += unicode_width::UnicodeWidthStr::width(joiner.as_str());
        }
        let line_width = line.content.width();
        current_segments.push((wrapped_idx, cumulative_col, cumulative_col + line_width));
        cumulative_col += line_width;
    }
    if !current_segments.is_empty() {
        pre_wrap_segments.push(current_segments);
    }

    // Map each hyperlink to screen-space OverlayLinks. Unsafe schemes
    // (javascript:, data:, …) are dropped since OSC 8 URLs reach the terminal
    // without the link_opener scheme filter. Local-file destinations such as
    // `[videos/1.mp4](videos/1.mp4)` resolve against generated media, then
    // against existing files under the session `cwd`.
    let scheme_filter = crate::terminal::hyperlinks::SchemeFilter::Standard;
    // Reused across every link segment so the row's plain text is not
    // reallocated per segment per frame; only written when reordering is on.
    let mut row_plain_buf = String::new();
    for h in hyperlinks {
        let target = if crate::app::link_opener::is_safe_to_open(&h.url, scheme_filter) {
            crate::render::osc8::LinkTarget::Url(Arc::from(h.url.as_str()))
        } else if let Some(file_target) =
            crate::render::osc8::local_link_to_file_target(&h.url, media_paths, cwd)
        {
            file_target
        } else {
            continue;
        };
        let adjusted_line = h.line_index + content_line_offset;
        if adjusted_line >= pre_wrap_segments.len() {
            continue;
        }
        let segments = &pre_wrap_segments[adjusted_line];
        for &(wrapped_idx, seg_col_start, seg_col_end) in segments {
            // Check if hyperlink's column range overlaps this wrapped segment.
            let overlap_start = h.column_range.start.max(seg_col_start);
            let overlap_end = h.column_range.end.min(seg_col_end);
            if overlap_start >= overlap_end {
                continue;
            }

            // Check visibility (accounting for content_skip).
            if wrapped_idx < content_skip {
                continue;
            }
            let visible_offset = (wrapped_idx - content_skip) as u16;
            let screen_row = first_visible_screen_y + visible_offset;
            if screen_row >= max_screen_y {
                continue;
            }

            let local_col_start = overlap_start - seg_col_start;
            let local_col_end = overlap_end - seg_col_start;

            // Link columns are logical; map to visual only when paint reorders.
            let visual_ranges = if crate::render::bidi::is_enabled() {
                row_plain_buf.clear();
                line_plain_text_into(&block_output.lines[wrapped_idx].content, &mut row_plain_buf);
                if crate::render::bidi::needs_bidi(&row_plain_buf) {
                    crate::render::bidi::logical_cols_to_visual(
                        &row_plain_buf,
                        local_col_start,
                        local_col_end,
                    )
                } else {
                    vec![(local_col_start, local_col_end)]
                }
            } else {
                vec![(local_col_start, local_col_end)]
            };
            for (vs, ve) in visual_ranges {
                if vs >= ve {
                    continue;
                }
                overlay.push(OverlayLink {
                    screen_row,
                    col_start: content_x + vs as u16,
                    col_end: content_x + ve as u16,
                    target: target.clone(),
                    presentation: crate::render::osc8::LinkPresentation::Opaque,
                    id: Some(h.id),
                });
            }
        }
    }
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
