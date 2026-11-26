//! ScrollbackPane widget - the main conversation display.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::StatefulWidget;
use std::ops::Range;

use crate::render::SafeBuf;
use crate::render::color::{blend_color, fade_region};
use crate::scrollback::EntryLayoutInfo;
use crate::scrollback::block::{BlockContent, RenderBlock};
use crate::scrollback::entry::ScrollbackEntry;
use crate::scrollback::layout::HorizontalLayout;
use crate::scrollback::render::{ScratchBuffer, render_scrolled_entries_with_selection_boundaries};
use crate::scrollback::selection::{RenderOutput, ScrollInfo, SelectionBox};
use crate::scrollback::state::{ScrollbackState, ViewMode};
use crate::scrollback::sticky::{PromptDescriptor, StickyHeaderLayout, compute_sticky_layout};
use crate::scrollback::text_selection::{
    ResolvedSelectableLine, ResolvedSelectionBoundaries, ResolvedSelectionModel,
    VisibleBlockGeometry,
};
use crate::scrollback::types::{BlockContext, DisplayMode, derive_selection_text, selectable_cols};
use crate::theme::Theme;

/// Scrollback pane widget.
///
/// Displays conversation entries with optional pinned header for the current turn's prompt.
///
/// # Scratch Buffers
/// For efficiency, scratch buffers should be owned by the caller and reused across frames.
/// Use `render_with_scratch()` for optimal performance. The `StatefulWidget::render()` impl
/// creates a temporary scratch buffer for API compatibility but is less efficient.
#[derive(Debug, Clone, Default)]
pub struct ScrollbackPane {
    pub is_active: bool,
    pub mouse_pos: Option<(u16, u16)>,
    pub dim_from_entry: Option<usize>,
    /// Index of the entry currently under the mouse cursor (post-`hit_test`).
    /// Used to paint a hover bg + swap the indicator for tool-call entries.
    /// Hover painting still fires when `is_active` is false — hover is a
    /// mouse affordance, not a focus indicator.
    pub hovered_entry: Option<usize>,
    /// When set, every visible content row is post-painted to invert cells
    /// matching this regex (scrollback search). `None` disables highlighting.
    pub search_highlight: Option<regex::Regex>,
    /// Absolute paths of media generated in this transcript, used to resolve the
    /// short relative paths the model prints (`images/1.jpg`) into clickable
    /// `file://` links. Empty disables relative-path resolution.
    pub media_paths: Vec<std::path::PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RenderOutputWithSelectionBoundaries {
    pub(crate) output: RenderOutput,
    pub(crate) selection_boundaries: ResolvedSelectionBoundaries,
}

/// Screen-coordinate selection data for one sticky header: the header paints outside the content renderer, so it publishes this itself.
#[derive(Debug)]
struct StickyHeaderSelection {
    lines: Vec<ResolvedSelectableLine>,
    block: VisibleBlockGeometry,
}

/// Header rows precede every content entry, keeping `visible_blocks` sorted by `entry_idx` as drag autoscroll expects.
fn prepend_header_selection(
    model: &mut ResolvedSelectionModel,
    headers: Vec<StickyHeaderSelection>,
) {
    let mut merged = ResolvedSelectionModel::default();
    let blocks: Vec<VisibleBlockGeometry> = headers
        .into_iter()
        .map(|header| {
            header.lines.into_iter().for_each(|l| merged.push_line(l));
            header.block
        })
        .collect();

    // splice(0..0, ..) cannot panic: an empty range at index 0 is in bounds for any vec.
    model.ranges.splice(0..0, merged.ranges);
    model.visible_blocks.splice(0..0, blocks);
}

impl ScrollbackPane {
    /// Create a new scrollback pane.
    pub fn new() -> Self {
        Self {
            is_active: false,
            mouse_pos: None,
            dim_from_entry: None,
            hovered_entry: None,
            search_highlight: None,
            media_paths: Vec::new(),
        }
    }

    /// Set active state.
    pub fn active(mut self, active: bool) -> Self {
        self.is_active = active;
        self
    }

    /// Set mouse position for timestamp hover detection.
    pub fn with_mouse_pos(mut self, pos: (u16, u16)) -> Self {
        self.mouse_pos = Some(pos);
        self
    }

    pub fn with_dim_from(mut self, dim_from: Option<usize>) -> Self {
        self.dim_from_entry = dim_from;
        self
    }

    /// Set the entry under the mouse cursor.
    pub fn with_hovered_entry(mut self, hovered: Option<usize>) -> Self {
        self.hovered_entry = hovered;
        self
    }

    /// Set the regex whose matches are inverted on every visible content row.
    pub fn with_search_highlight(mut self, re: Option<regex::Regex>) -> Self {
        self.search_highlight = re;
        self
    }

    /// Set the transcript's generated-media paths used to resolve relative
    /// file-path link targets (`images/1.jpg`) into clickable `file://` links.
    pub fn with_media_paths(mut self, media_paths: Vec<std::path::PathBuf>) -> Self {
        self.media_paths = media_paths;
        self
    }
}

impl StatefulWidget for ScrollbackPane {
    type State = ScrollbackState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        // Create a temporary scratch buffer for API compatibility.
        // For better performance, use render_with_scratch() with a reusable scratch buffer.
        let mut scratch = ScratchBuffer::new();
        self.render_with_scratch(area, buf, state, &mut scratch);
    }
}

impl ScrollbackPane {
    /// Render the scrollback pane with an externally-owned scratch buffer.
    ///
    /// This is the preferred method for rendering - the scratch buffer should be
    /// created once and reused across frames to avoid allocations.
    ///
    /// # TODO: Make render pure (Phase 2-5)
    ///
    /// After implementing `prepare_layout()` in ScrollbackState, this method should
    /// take `state: &ScrollbackState` (immutable) instead of `&mut ScrollbackState`.
    /// All state mutations will move to `prepare_layout()`, making render truly pure.
    ///
    /// # TODO: Consider StatefulWidget-like pattern
    ///
    /// Once state is immutable, we could have a pattern like:
    /// ```ignore
    /// trait PureWidget {
    ///     type ReadState;
    ///     type ScratchState;
    ///     fn render(&self, area: Rect, buf: &mut Buffer,
    ///               state: &Self::ReadState, scratch: &mut Self::ScratchState);
    /// }
    /// ```
    /// This separates read-only state from mutable scratch/working memory.
    ///
    /// Returns RenderOutput containing elements to render in a post-pass (e.g., selection box, scroll info).
    pub fn render_with_scratch(
        self,
        area: Rect,
        buf: &mut Buffer,
        state: &ScrollbackState, // Now immutable!
        scratch: &mut ScratchBuffer,
    ) -> RenderOutput {
        self.render_with_scratch_and_selection_boundaries(area, buf, state, scratch)
            .output
    }

    pub(crate) fn render_with_scratch_and_selection_boundaries(
        self,
        area: Rect,
        buf: &mut Buffer,
        state: &ScrollbackState,
        scratch: &mut ScratchBuffer,
    ) -> RenderOutputWithSelectionBoundaries {
        if area.width == 0 || area.height == 0 {
            return RenderOutputWithSelectionBoundaries::default();
        }

        // Get scroll info for Viewport to render scrollbar
        let (scroll_offset, viewport_height, total_height) = state.scroll_info();
        let scroll_info = ScrollInfo {
            scroll_offset,
            viewport_height,
            total_height,
        };

        let theme = Theme::current();

        // NOTE: All layout preparation (viewport, heights, follow mode, cache invalidation)
        // is now done by state.prepare_layout() BEFORE render is called.
        // This keeps render as close to pure as possible.

        // Branch based on view mode - render to full area
        // Scrollbar is now rendered by Viewport at the correct position
        let mut output = match state.view_mode() {
            ViewMode::SingleTurn => self.render_single_turn(area, buf, state, &theme, scratch),
            ViewMode::AllTurns => {
                let full_range = 0..state.len();
                self.render_with_sticky_headers(area, buf, state, &theme, full_range, scratch)
            }
        };

        // Hover affordance for tool-call entries: paint a hover bg + swap
        // the bullet to a chevron when collapsed/foldable. Runs whether or
        // not the scrollback owns focus — hover is a mouse signal.
        self.render_tool_call_hover(buf, area, state, &theme);

        // Add scroll info for Viewport to render scrollbar
        output.output.scroll_info = Some(scroll_info);
        output
    }

    /// Paint hover affordance for foldable header-style blocks
    /// (`ToolCall`, `Thinking`, `BgTask`, `Subagent`).
    ///
    /// - Skips when nothing is hovered, or when the hovered entry is the
    ///   currently selected entry (selection wins; avoids double-paint).
    /// - Skips entries that aren't header-style — markdown / agent message
    ///   blocks already get the hover *border* via
    ///   `agent::render_entry_hover` and don't need a full bg patch.
    /// - Hover bg is `blend(bg_base, bg_dark, 0.5)` so it's strictly
    ///   dimmer than the selection bg (`bg_dark`) across all themes.
    /// - Inset matches the group-selection bg rule (skip 1 col on each
    ///   side unless `display.highlight_overlays_border` is set).
    /// - Chevron paint is shared with the selected-entry path via
    ///   [`paint_expandable_indicator`]. `BgTask` / `Subagent` aren't
    ///   foldable so the chevron is a no-op there, but the hover bg
    ///   still paints to match the other collapsed tool-call rows.
    fn render_tool_call_hover(
        &self,
        buf: &mut Buffer,
        area: Rect,
        state: &ScrollbackState,
        theme: &Theme,
    ) {
        let Some(hover_idx) = self.hovered_entry else {
            return;
        };
        if state.selected() == Some(hover_idx) {
            return;
        }
        let Some(entry) = state.entry(hover_idx) else {
            return;
        };
        if !matches!(
            entry.block,
            crate::scrollback::block::RenderBlock::ToolCall(_)
                | crate::scrollback::block::RenderBlock::Thinking(_)
                | crate::scrollback::block::RenderBlock::BgTask(_)
                | crate::scrollback::block::RenderBlock::Subagent(_)
        ) {
            return;
        }
        // Skip hover bg whenever the entry isn't collapsed. Once the user
        // has folded the entry open (Truncated for `Execute`/`Other` while
        // streaming, `Expanded` for Edit/markdown), the row already has
        // line-level styling — diff green/red, stdout `bg_dark`, etc. —
        // and a full bg patch either clobbers it or is redundant.
        // The hover border (`render_entry_hover`) still fires to indicate
        // hover; the chevron paint below is a no-op for non-collapsed
        // entries anyway.
        if entry.display_mode != DisplayMode::Collapsed {
            return;
        }
        let Some((entry_area, top_clipped, _bottom_clipped)) =
            state.entry_screen_area(hover_idx, area)
        else {
            return;
        };

        let display_cfg = &state.appearance().scrollback.display;
        let hover_bg = blend_color(theme.bg_base, theme.bg_dark, 0.5).unwrap_or(theme.bg_dark);
        let bg_style = Style::default().bg(hover_bg);

        // Inset the hover bg by 1 column on each side unless the appearance
        // config opts into overlaying the border, mirroring the group
        // selection bg behaviour.
        let (hl_x, hl_width) = if display_cfg.highlight_overlays_border {
            (entry_area.x, entry_area.width)
        } else {
            (entry_area.x + 1, entry_area.width.saturating_sub(2))
        };

        if hl_width > 0 {
            for y in entry_area.y..entry_area.y + entry_area.height {
                if y >= area.y && y < area.y + area.height {
                    for x in hl_x..hl_x + hl_width {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_style(bg_style);
                        }
                    }
                }
            }
        }

        // Swap ◆ → › on the bullet row when the entry is foldable +
        // collapsed (or running and at min fold mode). Same predicate as
        // the selected-entry chevron paint. An expanded verb slot hovers ⌄
        // on its header row; when the header is top-clipped off-screen the
        // first visible row is member 0's, which takes the normal ›.
        let verb_expanded = state
            .get_cached_entry_layouts()
            .and_then(|l| l.get(hover_idx))
            .is_some_and(EntryLayoutInfo::is_expanded_verb_header);
        paint_expandable_indicator(
            buf,
            area,
            entry_area.y,
            state.appearance(),
            entry,
            verb_expanded && !top_clipped,
        );
    }

    // Comment markers for sticky header rendering follow below

    // Unified sticky header rendering for both SingleTurn and AllTurns modes.
    //
    // The key insight: SingleTurn is just AllTurns with a filtered range.
    // Both modes use the same gradual header collapse logic via compute_sticky_layout().
    //
    // For SingleTurn:
    //   - visible_range = state.visible_entry_range() (entries 0..N of the current turn)
    //   - Prompt descriptors have y_virtual relative to range start
    //   - The first entry (prompt) becomes a sticky header when scrolled past
    //
    // For AllTurns:
    //   - visible_range = 0..state.len() (all entries)
    //   - Prompt descriptors have y_virtual as cumulative from entry 0
    //   - Multiple prompts can become sticky headers as user scrolls

    /// Render in SingleTurn mode using unified sticky header logic.
    fn render_single_turn(
        &self,
        area: Rect,
        buf: &mut Buffer,
        state: &ScrollbackState, // Now immutable!
        theme: &Theme,
        scratch: &mut ScratchBuffer,
    ) -> RenderOutputWithSelectionBoundaries {
        let visible_range = state.visible_entry_range();
        self.render_with_sticky_headers(area, buf, state, theme, visible_range, scratch)
    }

    // Unified sticky header rendering

    /// Render entries with sticky section headers.
    ///
    /// This is the unified rendering path for both SingleTurn and AllTurns modes.
    /// Prompts within the entry_range act as section headers that stick to the top
    /// when scrolled past, and get pushed off by the next approaching prompt.
    ///
    /// - For SingleTurn: entry_range = visible_entry_range() (one turn's entries)
    /// - For AllTurns: entry_range = 0..len() (all entries)
    ///
    /// Returns RenderOutput containing selection box (if any) to be rendered after.
    fn render_with_sticky_headers(
        &self,
        area: Rect,
        buf: &mut Buffer,
        state: &ScrollbackState, // Now immutable!
        theme: &Theme,
        entry_range: Range<usize>,
        scratch: &mut ScratchBuffer,
    ) -> RenderOutputWithSelectionBoundaries {
        let layout_cfg = &state.appearance().scrollback.layout;
        let layout = HorizontalLayout::new(area, layout_cfg);
        let entry_content_width = layout.entry_content_area().width;

        // Build prompt descriptors for the given entry range.
        // For SingleTurn, this gives us relative y_virtual coordinates.
        // For AllTurns, this is equivalent to the full range.
        let prompts = self.build_prompt_descriptors_for_range(
            state,
            entry_content_width,
            theme,
            entry_range.clone(),
        );

        // Compute sticky header layout (disabled in compact mode).
        let use_sticky = state.appearance().scrollback.display.sticky_headers
            && !state.appearance().prompt.compact;
        let sticky = if use_sticky {
            compute_sticky_layout(state.scroll_offset(), area.height, &prompts)
        } else {
            StickyHeaderLayout::default()
        };

        // The sticky layout encapsulates all 1D coordinate math.
        // We just ask it for screen positions and scroll offsets.
        let header_height = sticky.header_screen_rows();

        let content_area = if header_height > 0 && header_height < area.height {
            Rect {
                x: area.x,
                y: area.y + header_height,
                width: area.width,
                height: area.height.saturating_sub(header_height),
            }
        } else if header_height >= area.height {
            // Header takes entire viewport (edge case).
            // Clamp y to the last valid row of area so it stays within the
            // buffer, even though height=0 means nothing will render here.
            Rect {
                x: area.x,
                y: area.y + area.height.saturating_sub(1),
                width: area.width,
                height: 0,
            }
        } else {
            area
        };

        // Render pushed header (if any) - this one is being pushed off
        // Also track selection info for pushed headers
        // Sticky descriptors carry absolute indices; the selection model is relative to the rendered range (`render_content` remaps the same way).
        let selection_idx = |entry_idx: usize| entry_idx.saturating_sub(entry_range.start);
        let mut header_selection: Vec<StickyHeaderSelection> = Vec::new();
        let mut pushed_header_selection_box: Option<SelectionBox> = None;
        if let Some(ref pushed) = sticky.pushed {
            let visible_height = pushed.visible_height();
            if visible_height > 0 {
                let screen_row = sticky.pushed_screen_row().unwrap_or(0);
                let header_area = Rect {
                    x: area.x,
                    y: area.y + screen_row,
                    width: area.width,
                    height: visible_height,
                };
                header_selection.extend(self.render_sticky_header(
                    buf,
                    header_area,
                    state,
                    pushed.entry_idx,
                    selection_idx(pushed.entry_idx),
                    theme,
                    pushed.render_height,
                    pushed.clip_top,
                    scratch,
                    self.is_active && state.selected() == Some(pushed.entry_idx),
                    self.mouse_pos,
                ));

                // Fade out the pushed header as it's being pushed off.
                // The fade makes the transition smoother visually.
                // opacity = visible_rows / (full_height + 1)
                // So even a fully visible pushed header (clip_top=0) starts at 80% for 4-row header.
                let opacity =
                    visible_height as f32 / (pushed.render_height.saturating_add(1)) as f32;
                fade_region(buf, header_area, theme.bg_base, opacity);

                // Compute selection box for pushed header if it's selected
                if self.is_active && state.selected() == Some(pushed.entry_idx) {
                    let layout = HorizontalLayout::new(header_area, layout_cfg);
                    let selection_area = layout.selection_area();

                    // Selection border fades with content.
                    // If needed, use opacity.max(0.5) to enforce a minimum visibility floor.
                    let border_color = blend_color(theme.bg_base, theme.selection_border, opacity)
                        .unwrap_or(theme.selection_border);

                    // Top is clipped only when content is actually being clipped off (clip_top > 0)
                    // When clip_top == 0, the full header is visible (just fading), so show full border
                    let top_clipped = pushed.clip_top > 0;

                    let sel_box =
                        SelectionBox::new(selection_area, Style::default().fg(border_color))
                            .with_top_clipped(top_clipped)
                            .with_bottom_clipped(false);

                    pushed_header_selection_box = Some(sel_box);
                }
            }
        }

        // Render pinned header (if any) - the main sticky header
        let mut pinned_header_selection: Option<(Rect, usize)> = None;
        if let Some(ref pinned) = sticky.pinned {
            let visible_height = pinned.visible_height();
            if visible_height > 0 {
                let screen_row = sticky.pinned_screen_row().unwrap_or(0);
                let header_area = Rect {
                    x: area.x,
                    y: area.y + screen_row,
                    width: area.width,
                    height: visible_height,
                };
                header_selection.extend(self.render_sticky_header(
                    buf,
                    header_area,
                    state,
                    pinned.entry_idx,
                    selection_idx(pinned.entry_idx),
                    theme,
                    pinned.render_height,
                    pinned.clip_top,
                    scratch,
                    self.is_active && state.selected() == Some(pinned.entry_idx),
                    self.mouse_pos,
                ));

                // For selection, use HorizontalLayout to match content entries' selection width
                let layout = HorizontalLayout::new(header_area, layout_cfg);
                let selection_area = layout.selection_area();
                pinned_header_selection = Some((selection_area, pinned.entry_idx));
            }
        }

        // NOTE: The gap row is part of the header area height.
        // It should naturally be empty since we don't render anything there.

        // Compute selection box for pinned header if it's selected
        let mut pinned_header_selection_box: Option<SelectionBox> = None;
        if self.is_active
            && let Some((selection_area, entry_idx)) = pinned_header_selection
            && state.selected() == Some(entry_idx)
        {
            // Selection box for sticky header:
            // - y_top = selection_area.y (first row of selection content)
            // - y_bottom = selection_area.y + selection_area.height - 1 (last row)
            // - top_clipped = false if there's a gap row above for corners
            // - bottom_clipped = false since there's always a gap row between header and content
            let screen_row = sticky.pinned_screen_row().unwrap_or(0);

            // Check if there's room for top corners
            let top_clipped = screen_row == 0 && area.y == 0;

            let sel_box =
                SelectionBox::new(selection_area, Style::default().fg(theme.selection_border))
                    .with_top_clipped(top_clipped)
                    .with_bottom_clipped(false);

            pinned_header_selection_box = Some(sel_box);
        }

        // Render content
        let mut output = if content_area.height > 0 {
            // Use the entry_range for content rendering (same range we built descriptors for)
            let visible_range = entry_range.clone();

            // Use the sticky layout's scroll_for_content() which maintains bottom line continuity.
            let scroll_for_content = sticky.scroll_for_content(state.scroll_offset());

            // Get the entry index shown in the pinned header (to avoid duplicate selection)
            let pinned_entry_idx = sticky.pinned_entry_idx();

            let mut content_output = self.render_content(
                area,
                content_area,
                buf,
                state,
                theme,
                visible_range,
                scroll_for_content,
                0,
                // In AllTurns mode, we may have already computed selection for pinned header.
                // Pass None to avoid duplicate selection box computation in render_content.
                None,
                pinned_entry_idx,
                pinned_header_selection_box.is_some(),
                scratch,
            );

            // Return the selection box - prioritize pushed > pinned > content
            let content_selection_box = content_output.output.selection_box.take();
            content_output.output.selection_box = pushed_header_selection_box
                .or(pinned_header_selection_box)
                .or(content_selection_box);
            content_output
        } else {
            // Return header selection if we have one (pushed > pinned)
            RenderOutputWithSelectionBoundaries {
                output: RenderOutput {
                    selection_box: pushed_header_selection_box.or(pinned_header_selection_box),
                    ..Default::default()
                },
                ..Default::default()
            }
        };
        prepend_header_selection(&mut output.output.selection_model, header_selection);
        // A header-only viewport skips the content renderer, so publish the (zero-height, pane-bottom) content
        // rect here; drag autoscroll reads it to tell "all chrome" apart from "no frame yet".
        if output.output.selection_model.content_area == Rect::default() {
            output.output.selection_model.content_area = content_area;
        }

        // Publish the gap row this frame's pinned header actually produced
        // (None during push transitions and degenerate tiny viewports).
        output.output.sticky_gap_row = sticky.gap_row().filter(|row| *row < area.height);
        output
    }

    /// Build prompt descriptors for a range of entries.
    ///
    /// The y_virtual coordinates are relative to the range start, not absolute.
    /// This allows the same sticky layout logic to work for both:
    /// - SingleTurn: range = visible_entry_range() (one turn's entries)
    /// - AllTurns: range = 0..len() (all entries)
    ///
    /// NOTE: This method now uses cached data from prepare_layout().
    /// Heights are NOT recomputed - they come from the LayoutCache.
    fn build_prompt_descriptors_for_range(
        &self,
        state: &ScrollbackState,
        _entry_content_width: u16, // Kept for API compat, but not used (heights from cache)
        _theme: &Theme,            // Kept for API compat, but not used
        entry_range: Range<usize>,
    ) -> Vec<PromptDescriptor> {
        // Get cached data - must be valid after prepare_layout()
        let cached_descriptors = state
            .get_cached_prompt_descriptors()
            .expect("layout cache must be valid - was prepare_layout() called?");

        let cached_virtual_y = state
            .get_cached_virtual_y()
            .expect("layout cache must be valid - was prepare_layout() called?");

        // Get the y_offset of the first entry in the range
        // All y_virtual values will be adjusted relative to this
        let y_offset = cached_virtual_y
            .get(entry_range.start)
            .copied()
            .unwrap_or(0);

        // Filter descriptors to only those in the entry range,
        // and adjust y_virtual to be relative to range start
        cached_descriptors
            .iter()
            .filter(|p| entry_range.contains(&p.entry_idx))
            .map(|p| PromptDescriptor {
                entry_idx: p.entry_idx,
                y_virtual: p.y_virtual.saturating_sub(y_offset),
                full_height: p.full_height,
                min_height: p.min_height,
                sticky: p.sticky,
            })
            .collect()
    }

    /// Render a sticky header (pushed or pinned).
    ///
    /// - `render_height`: Total height budget for the block (including vpads)
    /// - `clip_top`: If > 0, clips rendered output from top (for push effect)
    /// - `selection_entry_idx`: `entry_idx` rebased onto the rendered range (the selection model's key space)
    #[allow(clippy::too_many_arguments)]
    fn render_sticky_header(
        &self,
        buf: &mut Buffer,
        area: Rect,
        state: &ScrollbackState, // Now immutable! No entry mutation needed.
        entry_idx: usize,
        selection_entry_idx: usize,
        theme: &Theme,
        render_height: u16,
        clip_top: u16,
        scratch: &mut ScratchBuffer,
        is_selected: bool,
        mouse_pos: Option<(u16, u16)>,
    ) -> Option<StickyHeaderSelection> {
        let appearance = state.appearance();

        let entry = state.entry(entry_idx)?;

        let layout = HorizontalLayout::new(area, &appearance.scrollback.layout);

        // Compute content lines from render_height
        // The block adds vpad (2 rows) if has_vpad is true
        let cwd = state.cwd();
        let has_vpad = entry
            .block
            .has_vpad(&entry.context(area.width, appearance, cwd));
        let vpad_rows = if has_vpad { 2 } else { 0 };
        let content_lines = render_height.saturating_sub(vpad_rows);

        // User prompts use their actual display mode so collapsed prompts
        // stay truncated (3 lines + ellipsis) in sticky headers.
        // Other blocks use Expanded + max_lines budget as before.
        let mode = if entry.block.is_user_prompt() {
            entry.display_mode()
        } else {
            DisplayMode::Expanded
        };

        // When timestamps are shown on message blocks, reserve right margin in the
        // block's content width so wrapped text doesn't collide with the overlaid
        // timestamp (matches behavior in EntryRenderer for normal content).
        let ts_reserved = if appearance.show_timestamps
            && matches!(
                &entry.block,
                RenderBlock::UserPrompt(_) | RenderBlock::AgentMessage(_) | RenderBlock::Btw(_)
            ) {
            10
        } else {
            0
        };
        let content_width_for_block = layout.content_width().saturating_sub(ts_reserved);

        let ctx = entry.context_with_mode_and_budget(
            content_width_for_block,
            mode,
            content_lines,
            appearance,
            is_selected,
            cwd,
        );

        // Published `block_line_idx` values index this budgeted output, while copy re-derives them without `max_lines`; only equal when ignored.
        debug_assert!(
            entry.block.is_user_prompt(),
            "only max_lines-insensitive blocks (user prompts) may be sticky"
        );

        let rendered_lines = if clip_top > 0 {
            // For pushed headers being pushed OFF screen:
            // - The TOP rows disappear first (pushed up, out of view)
            // - The BOTTOM rows stay visible longest
            //
            // We render the full header to a scratch buffer, then copy only
            // the bottom (visible) rows to the actual buffer.

            let visible_height = render_height.saturating_sub(clip_top);

            if visible_height == 0 {
                return None;
            }

            // Use reusable scratch buffer (avoids allocation per frame)
            let scratch_buf = scratch.prepared(area.width, render_height);
            let scratch_area = Rect::new(0, 0, area.width, render_height);

            // Render full header to scratch using existing method
            let mut lines = Self::render_entry_with_ctx_static(
                entry,
                &ctx,
                theme,
                scratch_area,
                scratch_buf,
                mouse_pos,
                selection_entry_idx,
            );

            // Copy visible rows (after clip_top) to output buffer
            for dy in 0..visible_height {
                let src_y = clip_top + dy;
                let dst_y = area.y + dy;
                for dx in 0..area.width {
                    if let Some(src_cell) = scratch_buf.cell((dx, src_y))
                        && let Some(dst_cell) = buf.cell_mut((area.x + dx, dst_y))
                    {
                        *dst_cell = src_cell.clone();
                    }
                }
            }

            // Rebase the scratch-space rows onto the screen: rows above `clip_top` were never copied, so they are not selectable.
            lines.retain_mut(|line| {
                if line.screen_y < clip_top {
                    return false;
                }
                line.screen_y = area.y + (line.screen_y - clip_top);
                line.screen_x = line.screen_x.saturating_add(area.x);
                true
            });
            lines
        } else {
            // No clipping needed - render directly with max_lines
            Self::render_entry_with_ctx_static(
                entry,
                &ctx,
                theme,
                area,
                buf,
                mouse_pos,
                selection_entry_idx,
            )
        };

        // Text reaching the last header row means more may be hidden below.
        let bottom_clipped = rendered_lines
            .last()
            .is_some_and(|last| last.screen_y + 1 >= area.y + area.height);
        Some(StickyHeaderSelection {
            block: VisibleBlockGeometry {
                entry_idx: selection_entry_idx,
                area: layout.entry_area(),
                content_area: layout.content,
                selection_area: layout.selection_area(),
                // Copy re-renders the block at this width.
                content_width: content_width_for_block,
                top_clipped: clip_top > 0,
                bottom_clipped,
                // Text selection only: a block drag anchored here would sweep in the off-screen entries the pinned prompt scrolled past.
                drag_startable: false,
            },
            lines: rendered_lines,
        })
    }

    /// Render an entry with a specific BlockContext (for max_lines support).
    /// Static method to avoid borrow issues.
    ///
    /// `mouse_pos` is forwarded for timestamp hover expansion in sticky headers.
    ///
    /// Returns a selectable line per painted row, in `area`/`buf` coordinates (a scratch render yields scratch rows for the caller to rebase).
    fn render_entry_with_ctx_static(
        entry: &ScrollbackEntry,
        ctx: &BlockContext,
        theme: &Theme,
        area: Rect,
        buf: &mut Buffer,
        mouse_pos: Option<(u16, u16)>,
        selection_entry_idx: usize,
    ) -> Vec<ResolvedSelectableLine> {
        use crate::scrollback::types::BlockBackground;

        let layout = HorizontalLayout::new(area, &ctx.appearance.scrollback.layout);

        // Use the actual content area (not entry_content_area which includes accent)
        let content_area = layout.content;
        let left_pad = layout.left_padding;
        let right_pad = layout.right_padding;

        // Get block output with max_lines budget
        let output = entry.block.output(ctx);
        let block_has_vpad = entry.block.has_vpad(ctx);

        let block_bg = entry.block.background(ctx);

        // Resolve the background color from block_bg
        let bg_color = match block_bg {
            BlockBackground::None => None,
            BlockBackground::Light => Some(theme.bg_light),
            BlockBackground::Dark => Some(theme.bg_dark),
        };

        // Only use vpad if there's enough room for vpad + at least 1 content line.
        // Need at least 3 rows: vpad_top (1) + content (1) + vpad_bottom (1)
        // If less space, skip vpad to prioritize content.
        let use_vpad = block_has_vpad && content_area.height >= 3;

        // Calculate actual content height
        let content_height = output.len() as u16;
        let total_height = content_height + if use_vpad { 2 } else { 0 };

        // Fill the entire entry area with block background (if any)
        // This includes vpad rows, content rows, and padding columns
        if let Some(bg) = bg_color {
            let bg_style = ratatui::style::Style::default().bg(bg);
            let fill_height = total_height.min(area.height);

            for y in content_area.y..content_area.y + fill_height {
                // Fill left padding
                for x in left_pad.x..left_pad.x + left_pad.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(bg_style);
                    }
                }
                // Fill content area
                for x in content_area.x..content_area.x + content_area.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(bg_style);
                    }
                }
                // Fill right padding
                for x in right_pad.x..right_pad.x + right_pad.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(bg_style);
                    }
                }
            }
        }

        // Render vpad top if needed (skip 1 row)
        let mut y = content_area.y;
        if use_vpad {
            y += 1;
        }

        // `block_line_idx` counts every line, painted or not.
        let mut selection_lines = Vec::new();
        for (block_line_idx, line) in output.lines.iter().enumerate() {
            if y >= content_area.y + content_area.height {
                break;
            }
            // Render line in the content area (not overlapping with accent).
            // Bidi: content paint only — selection maps visual columns back.
            buf.set_line_safe_bidi(content_area.x, y, &line.content, content_area.width);
            if let (Some(range_id), Some(cols)) = (
                line.selection_range,
                selectable_cols(&line.content, &line.selectable),
            ) {
                selection_lines.push(ResolvedSelectableLine {
                    entry_idx: selection_entry_idx,
                    range_id,
                    block_line_idx,
                    screen_y: y,
                    screen_x: content_area.x,
                    // Visual span so the hit box matches the reordered cells
                    // even when a non-selectable suffix shifts the region.
                    selectable_cols: crate::scrollback::types::visual_selectable_cols(line)
                        .unwrap_or(cols),
                    text: derive_selection_text(line),
                    painted_region: Some(crate::scrollback::types::painted_selectable_region(line)),
                    joiner_to_previous: line.joiner.clone(),
                });
            }
            y += 1;
        }

        // Overlay timestamp on the first content line for message blocks in sticky
        // headers (pinned/pushed user prompts, agent messages, etc.). Mirrors the
        // overlay in EntryRenderer but adapted for the header render path + clip
        // (scratch buffer) case. Hover expansion works for pinned (absolute coords);
        // pushed headers always get the short format.
        if ctx.appearance.show_timestamps
            && !output.lines.is_empty()
            && matches!(
                &entry.block,
                RenderBlock::UserPrompt(_) | RenderBlock::AgentMessage(_) | RenderBlock::Btw(_)
            )
            && let Some(ts) = entry.created_at
        {
            let first_content_y = content_area.y + if use_vpad { 1 } else { 0 };
            let ts_hovered = mouse_pos.is_some_and(|(mx, my)| {
                my == first_content_y
                    && mx >= content_area.x + content_area.width.saturating_sub(10)
                    && mx < content_area.x + content_area.width
            });
            let ts_str = if ts_hovered {
                ts.format("  %H:%M:%S | %b %d").to_string()
            } else {
                ts.format("  %-I:%M %p").to_string()
            };
            let ts_width = ts_str.len() as u16;
            if content_area.width > ts_width + 1
                && first_content_y < content_area.y + content_area.height
            {
                let ts_x = content_area.x + content_area.width - ts_width;
                let ts_style = Style::default().fg(theme.gray);
                buf.set_string_safe(ts_x, first_content_y, &ts_str, ts_style);
            }
        }

        // vpad bottom is just empty space - no need to track y further

        // The accent column is kept for alignment but never painted. Clear it so content from a previous frame cannot bleed through.
        let accent_area = layout.accent;
        let clear_style = ratatui::style::Style::default().bg(bg_color.unwrap_or(theme.bg_base));
        for y in accent_area.y..accent_area.y + total_height.min(area.height) {
            if let Some(cell) = buf.cell_mut((accent_area.x, y)) {
                cell.set_char(' ');
                cell.set_style(clear_style);
            }
        }

        selection_lines
    }

    // Shared Rendering Helpers

    /// Render the main content area (shared between SingleTurn and AllTurns).
    ///
    /// Returns the selection box for content entries (if any), to be rendered by the frame.
    #[allow(clippy::too_many_arguments)]
    fn render_content(
        &self,
        area: Rect,
        content_area: Rect,
        buf: &mut Buffer,
        state: &ScrollbackState, // Now immutable!
        theme: &Theme,
        visible_range: Range<usize>,
        scroll_for_content: usize,
        _prompt_content_height: u16, // Unused after Phase 2 (total_height now from cache)
        pinned_header_selection_area: Option<Rect>,
        pinned_entry_idx: Option<usize>, // Entry idx shown in header (to skip in content selection)
        header_has_selection: bool,      // Whether selection was already computed for header
        _scratch: &mut ScratchBuffer,
    ) -> RenderOutputWithSelectionBoundaries {
        // Layout cache must be valid after prepare_layout().
        let all_layouts = state
            .get_cached_entry_layouts()
            .expect("layout cache must be valid - was prepare_layout() called?");

        // O(log n) paint window: only entries that can intersect the content
        // viewport. Avoids collecting/walking the full AllTurns history each frame.
        // Group headers (verb or truncation) that land in the window extend the
        // end of the slice through the rest of their run so the aggregated header
        // labels still see off-screen members (counts/tense/failures) without
        // re-collecting all history.
        let (paint_range, content_y0) = state.paint_window(
            visible_range.clone(),
            scroll_for_content,
            content_area.height as usize,
        );

        let entries: Vec<&ScrollbackEntry> = state.entries_in_range(paint_range.clone());
        let entry_layouts_cache = all_layouts
            .get(paint_range.clone())
            .expect("paint_range out of bounds for cached layouts");
        // paint_window keeps the window inside the visible range.
        let entry_index_base = paint_range.start - visible_range.start;

        // Selection / dim indices are relative to the full visible range (not the
        // paint window); entry_index_base remaps slice indices inside the renderer.
        let visible_start = visible_range.start;
        let relative_selected = if self.is_active {
            state
                .selected()
                .filter(|&s| visible_range.contains(&s))
                .map(|s| s - visible_start)
        } else {
            None
        };

        let relative_dim_from = self
            .dim_from_entry
            .filter(|&d| d >= visible_range.start && d < visible_range.end)
            .map(|d| d - visible_start);

        let rendered = render_scrolled_entries_with_selection_boundaries(
            buf,
            content_area,
            &entries,
            scroll_for_content,
            relative_selected,
            theme,
            state.appearance(),
            entry_layouts_cache,
            state.current_tick(),
            self.mouse_pos,
            relative_dim_from,
            self.search_highlight.as_ref(),
            content_y0,
            entry_index_base,
            &self.media_paths,
            Some((state.group_spans(), paint_range.start)),
            state.cwd(),
        );
        let result = rendered.result;
        let selection_boundaries = rendered.selection_boundaries;

        // NOTE: total_height is now computed by prepare_layout() before render,
        // so we don't update it here. The result.total_height is only used locally
        // if needed for debugging.

        // Capture selected entry's screen area for inline button positioning.
        let selected_entry_rect = result.selected_area.as_ref().map(|s| s.area);
        let selected_area = result.selected_area;
        let mut content_output = RenderOutputWithSelectionBoundaries {
            output: RenderOutput {
                selection_model: result.selection_model,
                link_overlay: result.link_overlay,
                inline_media: result.inline_media,
                diagram_affordances: result.diagram_affordances,
                ..Default::default()
            },
            selection_boundaries,
        };

        // Post-render: highlight the selected entry's rows with `bg_dark`.
        //
        // Fires when:
        //   1. The selected entry is part of a multi-entry group (so the
        //      individual selected row stands out within the group), OR
        //   2. The selected entry is a header-style foldable block
        //      (`ToolCall`, `Thinking`) — singletons get the same bg so
        //      selection looks consistent across grouped vs lone entries.
        //
        // Skipped when the selected entry isn't collapsed — once the row
        // is folded open (Truncated for `Execute`/`Other` while streaming,
        // `Expanded` for Edit/markdown), a full bg fill is too heavy and
        // would clobber line-level styling (diff green/red, stdout
        // `bg_dark`, etc.). The SelectionBox border alone is enough.
        //
        // Other singleton blocks (markdown messages, user prompts, etc.)
        // intentionally don't get the bg patch — for big markdown blocks
        // a full bg fill would be too heavy and the SelectionBox border
        // alone is enough.
        //
        // The highlight is inset by 1 column on each side unless
        // `display.highlight_overlays_border` is set, to avoid clobbering
        // the SelectionBox border characters (│).
        if self.is_active
            && let Some(ref selected) = selected_area
            && let Some(selected_abs) = state.selected()
        {
            let display_cfg = &state.appearance().scrollback.display;
            let split_mode = display_cfg.group_selection_split;
            let sel_range = state.group_range_of(selected_abs, split_mode);
            let is_header_style_block = state.entry(selected_abs).is_some_and(|e| {
                matches!(
                    e.block,
                    crate::scrollback::block::RenderBlock::ToolCall(_)
                        | crate::scrollback::block::RenderBlock::Thinking(_)
                )
            });
            let is_collapsed = state
                .entry(selected_abs)
                .is_some_and(|e| e.display_mode == DisplayMode::Collapsed);

            if (sel_range.len() > 1 || is_header_style_block) && is_collapsed {
                let highlight_area = selected.area;
                let bg_style = Style::default().bg(theme.bg_dark);

                let (hl_x, hl_width) = if display_cfg.highlight_overlays_border {
                    (highlight_area.x, highlight_area.width)
                } else {
                    (highlight_area.x + 1, highlight_area.width.saturating_sub(2))
                };

                if hl_width > 0 {
                    for y in highlight_area.y..highlight_area.y + highlight_area.height {
                        if y >= content_area.y && y < content_area.y + content_area.height {
                            for x in hl_x..hl_x + hl_width {
                                if let Some(cell) = buf.cell_mut((x, y)) {
                                    cell.set_style(bg_style);
                                }
                            }
                        }
                    }
                }
            }

            // Expandable indicator: replace the bullet character with "›" (or configured char)
            // when the selected entry is foldable and at its minimum fold mode.
            // Works for both grouped and singleton entries. In an expanded
            // verb-group slot the selection acts as MEMBER 0, so the caret
            // sits on the member row (below the header line) pointing right;
            // the ⌄ group affordance lives on the hover pass instead.
            if let Some(entry) = state.entry(selected_abs) {
                let verb_expanded = state
                    .get_cached_entry_layouts()
                    .and_then(|l| l.get(selected_abs))
                    .is_some_and(EntryLayoutInfo::is_expanded_verb_header);
                // Bottom-clip that cuts the member row off is handled by the
                // paint fn's bounds guard (the offset row is simply skipped).
                paint_expandable_indicator(
                    buf,
                    content_area,
                    verb_member_indicator_row(selected.area.y, verb_expanded, selected.top_clipped),
                    state.appearance(),
                    entry,
                    false,
                );
            }
        }

        // Compute selection box for content entries
        // Skip if:
        // 1. Selection was already computed for header (header_has_selection is true), OR
        // 2. The selected entry is the pinned header entry and pinned_header_selection_area is set
        //    (for SingleTurn mode where selection is computed later)
        let skip_content_selection = header_has_selection
            || (pinned_entry_idx.is_some()
                && state.selected() == pinned_entry_idx
                && pinned_header_selection_area.is_some());

        if self.is_active
            && !skip_content_selection
            && let Some(ref selected) = selected_area
            && let Some(selected_abs) = state.selected()
        {
            // Determine the selection range (group or individual)
            let split_mode = state.appearance().scrollback.display.group_selection_split;
            let sel_range = state.group_range_of(selected_abs, split_mode);

            if sel_range.len() <= 1 {
                // Singleton (non-groupable, or expanded in Mode B, or lone groupable):
                // Use the individual entry's area directly (same as before).
                let top_clipped = selected.top_clipped
                    || (selected.area.y == content_area.y && content_area.y == 0);
                let bottom = selected.area.y + selected.area.height;
                let bottom_clipped =
                    selected.bottom_clipped || bottom > content_area.y + content_area.height;

                let sel_box =
                    SelectionBox::new(selected.area, Style::default().fg(theme.selection_border))
                        .with_top_clipped(top_clipped)
                        .with_bottom_clipped(bottom_clipped);

                content_output.output.selection_box = Some(sel_box);
                content_output.output.selected_entry_area = selected_entry_rect;
                return content_output;
            }

            // Multi-entry group: compute a Rect spanning from first to last entry in the range.
            // Use virtual_y from cache to find screen positions.
            if let Some(all_virtual_y) = state.get_cached_virtual_y()
                && let Some(all_layouts) = state.get_cached_entry_layouts()
            {
                // Virtual y positions (relative to visible range start)
                let base_y = all_virtual_y[visible_range.start];
                let group_start_vy = all_virtual_y[sel_range.start] - base_y;
                let last_idx = sel_range.end - 1;
                let group_end_vy =
                    all_virtual_y[last_idx] - base_y + all_layouts[last_idx].height as usize;

                // Convert virtual y to screen y. Cumulative positions stay usize
                // (tall sessions exceed u16::MAX); the screen y/height
                // below are viewport-relative and provably fit in u16.
                let viewport_start = scroll_for_content;
                let viewport_end = scroll_for_content + content_area.height as usize;

                // Clip to viewport
                let visible_start_vy = group_start_vy.max(viewport_start);
                let visible_end_vy = group_end_vy.min(viewport_end);

                if visible_start_vy < visible_end_vy {
                    let screen_y = content_area.y + (visible_start_vy - viewport_start) as u16;
                    let visible_height = (visible_end_vy - visible_start_vy) as u16;

                    // Use HorizontalLayout for consistent selection width
                    let layout_cfg = &state.appearance().scrollback.layout;
                    let row_layout = HorizontalLayout::new(
                        Rect::new(content_area.x, screen_y, content_area.width, visible_height),
                        layout_cfg,
                    );
                    let sel_area = row_layout.selection_area();

                    let top_clipped = group_start_vy < viewport_start;
                    let bottom_clipped = group_end_vy > viewport_end;

                    let sel_box =
                        SelectionBox::new(sel_area, Style::default().fg(theme.selection_border))
                            .with_top_clipped(top_clipped)
                            .with_bottom_clipped(bottom_clipped);

                    content_output.output.selection_box = Some(sel_box);
                    content_output.output.selected_entry_area = selected_entry_rect;
                    return content_output;
                }
            }
        }

        // Compute selection box for pinned header (SingleTurn mode)
        if self.is_active
            && let Some(sel_area) = pinned_header_selection_area
        {
            let top_clipped = area.y == 0;
            // Bottom is not clipped since there's a gap row (pinned_height includes gap)
            let bottom_clipped = false;

            let sel_box = SelectionBox::new(sel_area, Style::default().fg(theme.selection_border))
                .with_top_clipped(top_clipped)
                .with_bottom_clipped(bottom_clipped);

            content_output.output.selection_box = Some(sel_box);
            return content_output;
        }

        content_output
    }
}

/// Screen row the selection caret belongs on within an entry's slot. An
/// expanded verb-group slot stacks the header line above member 0's own row
/// and the caret is the MEMBER's affordance, so it sits one row below the
/// slot top — unless the header is top-clipped off-screen, in which case the
/// slot's first visible row already IS the member row.
fn verb_member_indicator_row(slot_top: u16, verb_expanded: bool, header_clipped: bool) -> u16 {
    slot_top + u16::from(verb_expanded && !header_clipped)
}

/// Replace the bullet character on `entry_y` (a screen row) with the
/// configured expandable indicator (e.g. `›`) when the entry is foldable
/// and at its minimum fold mode. With `point_down` the indicator is the
/// down variant (`⌄`) — the hover pass uses it on an expanded verb-group
/// header row to advertise that the header collapses the group.
///
/// Shared between the selected-entry post-pass and the hover post-pass so
/// the chevron behaves identically in both states — only the bg differs.
///
/// No-op when `expandable_indicator` is disabled, the entry isn't
/// foldable, the entry isn't collapsed (or running + at min fold mode for
/// streaming blocks like `Execute`/`Thinking`), or the block has no
/// bullet.
fn paint_expandable_indicator(
    buf: &mut Buffer,
    content_area: Rect,
    entry_y: u16,
    appearance: &crate::appearance::AppearanceConfig,
    entry: &ScrollbackEntry,
    point_down: bool,
) {
    let display_cfg = &appearance.scrollback.display;
    if !display_cfg.expandable_indicator {
        return;
    }
    if !entry.block.is_foldable() {
        return;
    }
    let at_min_fold = entry.display_mode == DisplayMode::Collapsed
        || (display_cfg.expandable_indicator_running
            && entry.is_running
            && entry.display_mode == entry.block.collapse_mode(true));
    if !at_min_fold {
        return;
    }
    if !entry.block.has_bullet(&entry.context(0, appearance, None)) {
        return;
    }

    // The bullet sits at the start of the entry's content area on the
    // first row. `entry_y` is that first row in screen coords. Compute
    // the bullet column from the horizontal layout for a 1-row strip
    // anchored at `entry_y`.
    let layout_cfg = &appearance.scrollback.layout;
    let entry_layout = HorizontalLayout::new(
        Rect::new(content_area.x, entry_y, content_area.width, 1),
        layout_cfg,
    );
    let bullet_x = entry_layout.content.x;

    if entry_y >= content_area.y
        && entry_y < content_area.y + content_area.height
        && let Some(cell) = buf.cell_mut((bullet_x, entry_y))
    {
        // The down variant is fixed (not the configured char): it exists to
        // contrast with the right-pointing state, and `chevron_down` matches
        // `›`'s weight with its own ConHost fallback.
        let ch = if point_down {
            crate::glyphs::chevron_down().chars().next().unwrap_or('v')
        } else {
            display_cfg
                .expandable_indicator_char
                .chars()
                .next()
                .unwrap_or('›')
        };
        cell.set_char(ch);
    }
}

#[cfg(test)]
mod tests {
    use super::verb_member_indicator_row;
    use super::*;
    use crate::appearance::AppearanceConfig;
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::render::ScratchBuffer;
    use crate::scrollback::state::ScrollbackState;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn render_model(state: &mut ScrollbackState, area: Rect) -> ResolvedSelectionModel {
        state.prepare_layout(area.width, area.height);
        let mut buf = Buffer::empty(area);
        let mut scratch = ScratchBuffer::default();
        let pane = ScrollbackPane::new().active(true);
        pane.render_with_scratch(area, &mut buf, state, &mut scratch)
            .selection_model
    }

    // Pinned-header selectability is covered end to end by the sticky_header_drag_copy_pty e2e; only the
    // pushed/clip rebase branch needs a unit test (the PTY case never drags during a push).

    /// A pushed header paints through a scratch buffer, so its lines must be rebased onto the rows that reached the screen; clipped rows are dropped.
    #[test]
    fn pushed_sticky_header_lines_are_rebased_onto_visible_rows() {
        let area = Rect::new(0, 0, 80, 12);
        let mut state = ScrollbackState::new();
        let mut appearance = AppearanceConfig::default();
        appearance.scrollback.blocks.prompt.vpad = false;
        appearance.scrollback.blocks.prompt.show_prefix = false;
        state.set_appearance(appearance);
        // Two turns: the second prompt pushes the first one's header off.
        state.push_block(RenderBlock::user_prompt(
            "FIRSTPROMPT line one\nFIRSTPROMPT line two",
        ));
        for i in 0..6 {
            state.push_block(RenderBlock::stub(
                format!("first response {i}"),
                ratatui::style::Color::Blue,
            ));
        }
        state.push_block(RenderBlock::user_prompt("SECONDPROMPT"));
        for i in 0..20 {
            state.push_block(RenderBlock::stub(
                format!("second response {i}"),
                ratatui::style::Color::Blue,
            ));
        }
        state.prepare_layout(area.width, area.height);

        // Sweep every reachable scroll offset; the clipped-frame counter binds the assertions to the rebase branch.
        let mut saw_clipped_push = false;
        let mut checked_clipped_lines = 0usize;
        let mut trace: Vec<String> = Vec::new();
        let max_scroll = state.scroll_info().2;
        for scroll in 1..=max_scroll {
            state.set_scroll_offset(scroll);
            let model = render_model(&mut state, area);
            let Some(sticky) = state.sticky_layout() else {
                continue;
            };
            trace.push(format!("scroll={scroll} sticky={sticky:?}"));
            let Some(pushed) = sticky.pushed else {
                continue;
            };
            saw_clipped_push |= pushed.clip_top > 0;
            let visible = pushed.visible_height();
            let band = area.y..area.y + visible;
            for range in model
                .ranges
                .iter()
                .filter(|r| r.entry_idx == pushed.entry_idx)
            {
                for line in &range.lines {
                    if pushed.clip_top > 0 {
                        checked_clipped_lines += 1;
                    }
                    assert!(
                        band.contains(&line.screen_y),
                        "pushed-header line at row {} escapes its visible band \
                         {band:?} (scroll={scroll})",
                        line.screen_y
                    );
                    assert!(
                        line.text.contains("FIRSTPROMPT"),
                        "the rebased row must carry the pushed prompt's text, got {:?}",
                        line.text
                    );
                    let hit = model.hit_test_text_exact(line.screen_x, line.screen_y);
                    assert_eq!(
                        hit.map(|h| h.entry_idx),
                        Some(pushed.entry_idx),
                        "pushed-header row {} must hit-test back to the header entry \
                         (scroll={scroll})",
                        line.screen_y
                    );
                }
            }
        }
        let trace = trace.join("\n");
        assert!(
            saw_clipped_push,
            "the sweep never reached a top-clipped push\n{trace}"
        );
        assert!(
            checked_clipped_lines > 0,
            "a top-clipped header published no selectable line, so every \
             rebase assertion was skipped\n{trace}"
        );
    }

    // Pins the "caret offset ignores clipping" fix: the member caret
    // sits one row below the slot top only while the header row is visible.
    #[test]
    fn verb_member_indicator_row_tracks_header_clipping() {
        // Plain rows: caret on the slot's first row, clipped or not.
        assert_eq!(verb_member_indicator_row(4, false, false), 4);
        assert_eq!(verb_member_indicator_row(4, false, true), 4);
        // Expanded verb slot with the header visible: member row is slot+1.
        assert_eq!(verb_member_indicator_row(4, true, false), 5);
        // Header top-clipped off-screen: the first visible row IS member 0.
        assert_eq!(verb_member_indicator_row(4, true, true), 4);
    }
}
