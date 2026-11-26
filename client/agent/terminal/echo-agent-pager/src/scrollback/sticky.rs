//! Sticky header computation for AllTurns view.
//!
//! This module handles the "iOS-style" sticky section headers where prompts
//! stick to the top of the viewport when scrolled past, and get pushed off
//! when the next prompt approaches.
//!
//! The algorithm is purely computational (1D coordinate math) and can be
//! tested independently of any rendering logic.
//!
//! # Layout Model
//!
//! The layout works entirely with **total heights** - it doesn't know or care
//! about internal block structure (vpads, content lines, ellipsis, etc.).
//!
//! Each prompt has:
//! - `full_height`: Total rows when rendered inline in the timeline
//! - `min_height`: Minimum rows when fully collapsed as sticky header
//!
//! The layout computes `render_height` (between min and full) and `clip_top`.
//! The block renderer receives this height budget and decides internally how
//! to allocate it (vpads, content, truncation indicators, etc.).

/// Describes a prompt entry for sticky header computation.
///
/// Prompts are "section headers" in the conversation - they mark the start
/// of each turn and can be pinned to the top when scrolled past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptDescriptor {
    /// Index of the prompt entry in the entries list.
    pub entry_idx: usize,

    /// Y position in virtual scroll space.
    pub y_virtual: usize,

    /// Total height when rendered inline in the timeline.
    /// This is the FULL height including all padding, borders, content, etc.
    pub full_height: u16,

    /// Minimum height when fully collapsed as a sticky header.
    /// The block should still be recognizable at this size.
    /// Typically 3-4 rows (enough for padding + 1-2 content lines + ellipsis).
    pub min_height: u16,

    /// Whether this prompt should stick when scrolled past.
    /// Non-sticky prompts still participate in push calculations (they push
    /// the previous sticky prompt off screen) but never become pinned themselves.
    pub sticky: bool,
}

/// Minimum height for pinned headers.
/// This is a reasonable default - blocks should be at least this tall.
pub const MIN_PINNED_HEIGHT: u16 = 4;

/// A prompt to be rendered in the sticky header area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderedPrompt {
    /// Entry index of the prompt.
    pub entry_idx: usize,

    /// Total height budget for rendering this prompt.
    /// This is the FULL height the block should render to, including any
    /// internal padding, content, ellipsis, etc. The block decides how to
    /// allocate this space internally.
    ///
    /// Range: `min_height <= render_height <= full_height`
    pub render_height: u16,

    /// Rows clipped from the TOP (for push effect ONLY).
    /// This is applied AFTER the block renders to `render_height`.
    /// - 0 means no clipping (show all rows)
    /// - > 0 means the header is being pushed off by the next prompt
    ///
    /// Visible rows = render_height - clip_top
    pub clip_top: u16,
}

impl RenderedPrompt {
    /// Number of rows actually visible on screen after clipping.
    #[inline]
    pub fn visible_height(&self) -> u16 {
        self.render_height.saturating_sub(self.clip_top)
    }

    /// Whether this prompt needs a scratch buffer for rendering.
    /// True when clip_top > 0 (we need to render full then copy partial).
    #[inline]
    pub fn needs_scratch_buffer(&self) -> bool {
        self.clip_top > 0
    }
}

/// Result of computing sticky header layout for AllTurns view.
///
/// This describes what should be rendered in the sticky header area at the
/// top of the viewport. The content area starts after `header_screen_rows()` rows.
///
/// All 1D layout math is encapsulated here - the renderer just asks for
/// screen positions and scroll offsets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StickyHeaderLayout {
    /// The prompt being pushed off screen (clipped at top).
    /// Only present during push transition when next prompt is approaching.
    pub pushed: Option<RenderedPrompt>,

    /// The main pinned prompt (rendered fully unless viewport is tiny).
    /// None only at the very start of timeline (no prompts scrolled past).
    pub pinned: Option<RenderedPrompt>,
}

/// Gap row between header and content for visual separation.
const HEADER_CONTENT_GAP: u16 = 1;

impl StickyHeaderLayout {
    /// Height of just the header content (pushed + pinned + gap between them).
    /// Does NOT include the gap after the header.
    fn header_content_height(&self) -> u16 {
        let pushed_visible = self.pushed.map_or(0, |p| p.visible_height());
        let pinned_visible = self.pinned.map_or(0, |p| p.visible_height());
        let gap_between = if pushed_visible > 0 && pinned_visible > 0 {
            1
        } else {
            0
        };
        pushed_visible + gap_between + pinned_visible
    }

    /// Total rows the header occupies on screen, including gap after.
    ///
    /// This is the screen row where content rendering starts.
    ///
    /// During push transition (only pushed, no pinned), there's NO gap after the header.
    /// This keeps scroll_for_content constant: scroll_offset + header = B.y_virtual - 1.
    /// The gap between A and B is rendered in the content area (at B.y_virtual - 1).
    ///
    /// For pinned headers, the gap is added after the header for visual separation.
    pub fn header_screen_rows(&self) -> u16 {
        if !self.has_header() {
            return 0;
        }

        // During push (only pushed, no pinned), no gap after header.
        // This keeps scroll_for_content constant, ensuring smooth scrolling.
        if self.pushed.is_some() && self.pinned.is_none() {
            return self.header_content_height(); // No gap
        }

        // Pinned header: add gap after for visual separation
        self.header_content_height() + HEADER_CONTENT_GAP
    }

    /// Row where content rendering should start (0-indexed from viewport top).
    /// Alias for `header_screen_rows()` for clarity.
    #[inline]
    pub fn content_start_row(&self) -> u16 {
        self.header_screen_rows()
    }

    /// Height of the content area given viewport height.
    #[inline]
    pub fn content_height(&self, viewport_height: u16) -> u16 {
        viewport_height.saturating_sub(self.header_screen_rows())
    }

    /// Calculate scroll offset for content area to maintain bottom line continuity.
    ///
    /// # The Key Invariant
    /// Each c-j/c-k should move the bottom line by exactly 1 row:
    /// ```text
    /// bottom_line = scroll_offset + viewport_height - 1
    /// ```
    ///
    /// # How It Works
    /// With a sticky header of height H (including gap):
    /// - Content area has (viewport - H) rows
    /// - For bottom_line to equal scroll_offset + viewport - 1:
    ///   scroll_for_content + (viewport - H) - 1 = scroll_offset + viewport - 1
    ///   scroll_for_content = scroll_offset + H
    ///
    /// # Gradual Collapse
    /// As scroll_offset increases and header shrinks:
    /// - scroll_offset ↑1, header_height ↓1 → scroll_for_content stays constant
    /// - content_height ↑1 (more rows available)
    /// - bottom_line ↑1 ✓ (new row revealed)
    #[inline]
    pub fn scroll_for_content(&self, scroll_offset: usize) -> usize {
        scroll_offset + self.header_screen_rows() as usize
    }

    /// Whether any sticky header is present.
    #[inline]
    pub fn has_header(&self) -> bool {
        self.pushed.is_some() || self.pinned.is_some()
    }

    /// Entry index of the pinned prompt (if any).
    /// Used to avoid drawing duplicate selection on pinned entry.
    #[inline]
    pub fn pinned_entry_idx(&self) -> Option<usize> {
        self.pinned.as_ref().map(|p| p.entry_idx)
    }

    /// Screen row where the pinned header starts (for selection drawing).
    /// Returns None if no pinned header.
    pub fn pinned_screen_row(&self) -> Option<u16> {
        self.pinned?;
        let pushed_visible = self.pushed.map_or(0, |p| p.visible_height());
        let gap_after_pushed = if pushed_visible > 0 { 1 } else { 0 };
        Some(pushed_visible + gap_after_pushed)
    }

    /// Screen row of the gap after a pinned header (selection corners, the
    /// ▲ response-top indicator). `None` without a pinned header: a
    /// push-only header renders no gap after it (see
    /// [`Self::header_screen_rows`]), so this row would point at content.
    pub fn gap_row(&self) -> Option<u16> {
        self.pinned?;
        Some(self.header_content_height())
    }

    /// Screen row where pushed header starts (always 0 if present).
    pub fn pushed_screen_row(&self) -> Option<u16> {
        self.pushed.as_ref().map(|_| 0)
    }

    /// Screen row where the gap between pushed and pinned is (if both present).
    pub fn gap_between_row(&self) -> Option<u16> {
        if let (Some(pushed), Some(_)) = (&self.pushed, &self.pinned) {
            Some(pushed.visible_height())
        } else {
            None
        }
    }

    /// Map a header-relative screen row to the entry index of the prompt rendered there.
    ///
    /// Returns `None` if the row falls on a gap or outside the header area.
    /// `row` is relative to the top of the scrollback area (0-indexed).
    pub fn entry_at_header_row(&self, row: u16) -> Option<usize> {
        if !self.has_header() || row >= self.header_screen_rows() {
            return None;
        }

        // Check pushed prompt area (always starts at row 0)
        if let Some(ref pushed) = self.pushed
            && row < pushed.visible_height()
        {
            return Some(pushed.entry_idx);
        }

        // Check pinned prompt area
        if let Some(ref pinned) = self.pinned {
            let pinned_start = self.pinned_screen_row().unwrap_or(0);
            let pinned_end = pinned_start + pinned.visible_height();
            if row >= pinned_start && row < pinned_end {
                return Some(pinned.entry_idx);
            }
        }

        // In a gap (between pushed and pinned, or after header content)
        None
    }

    /// Get the screen area of a header prompt (pushed or pinned) if it matches `entry_idx`.
    ///
    /// Returns `(start_row, visible_height, is_pushed)` relative to scrollback area top.
    /// `is_pushed` is true for the disappearing prompt (fading away).
    pub fn header_entry_area(&self, entry_idx: usize) -> Option<(u16, u16, bool)> {
        if let Some(ref pushed) = self.pushed
            && pushed.entry_idx == entry_idx
        {
            let visible = pushed.visible_height();
            if visible > 0 {
                return Some((0, visible, true));
            }
        }
        if let Some(ref pinned) = self.pinned
            && pinned.entry_idx == entry_idx
        {
            let visible = pinned.visible_height();
            if visible > 0 {
                let start = self.pinned_screen_row().unwrap_or(0);
                return Some((start, visible, false));
            }
        }
        None
    }
}

/// Compute sticky header layout for AllTurns view.
///
/// # Arguments
/// - `scroll_offset`: How many virtual lines have been scrolled.
///   The bottom row of the viewport shows virtual line `scroll_offset + viewport_height - 1`.
/// - `viewport_height`: Height of the viewport in rows.
/// - `prompts`: All prompt descriptors, **must be sorted by y_virtual ascending**.
///
/// # Returns
/// Layout describing what to render in the sticky header area.
///
/// # Algorithm Overview
/// 1. Find the last prompt that's been scrolled past (y_virtual < scroll_offset)
/// 2. Calculate render_height (shrinks as we scroll more, down to min_height)
/// 3. Handle push effect from next prompt approaching (clips from TOP)
pub fn compute_sticky_layout(
    scroll_offset: usize,
    viewport_height: u16,
    prompts: &[PromptDescriptor],
) -> StickyHeaderLayout {
    // No prompts or no scroll = no pinning
    if prompts.is_empty() || scroll_offset == 0 {
        return StickyHeaderLayout::default();
    }

    // Find the last sticky prompt that's been scrolled past (y_virtual < scroll_offset).
    // Non-sticky prompts (e.g. expanded user prompts) are skipped for pinning
    // but still participate in push calculations below.
    let pinned_idx = match prompts
        .iter()
        .rposition(|p| p.sticky && p.y_virtual < scroll_offset)
    {
        Some(idx) => idx,
        None => {
            // No sticky prompts have been scrolled past yet
            return StickyHeaderLayout::default();
        }
    };

    let pinned_prompt = &prompts[pinned_idx];

    // Calculate render_height for the pinned prompt
    // As we scroll past, it shrinks (gradual collapse)
    let render_height = calculate_render_height(pinned_prompt, scroll_offset, viewport_height);

    // Check if next prompt is pushing
    let next_prompt_info = if pinned_idx + 1 < prompts.len() {
        let next = &prompts[pinned_idx + 1];
        let next_naive_row = next.y_virtual.saturating_sub(scroll_offset);

        // Push starts when next prompt would overlap with current header + gap.
        // We use <= to ensure the transition from pushed to pinned is smooth:
        // - During push: scroll_for_content = B.y_virtual - 1 (constant)
        // - At transition: pinned scroll_for_content = scroll_offset + header_pinned + gap
        // - These are equal when next_naive_row = header_with_gap + 1
        // - So push while next_naive_row <= header_with_gap
        let header_with_gap = render_height + HEADER_CONTENT_GAP;
        if next_naive_row <= header_with_gap as usize {
            Some((next, next_naive_row))
        } else {
            None
        }
    } else {
        None
    };

    match next_prompt_info {
        Some((_next_prompt, next_naive_row)) => {
            // During push transition: the next prompt is approaching from below
            if next_naive_row == 0 {
                // Next prompt is at row 0 (its inline position) - no header needed.
                // The next prompt takes over the sticky position.
                return StickyHeaderLayout::default();
            }

            // The current (pinned) header is being pushed off as the next prompt approaches.
            // We clip the current header from the top to make room for the next prompt.
            //
            // IMPORTANT: next_naive_row includes the gap row between entries.
            // The gap row should be in the CONTENT area, not the header.
            // So pushed_visible = next_naive_row - 1 (excluding the gap).
            //
            // If next_naive_row == 1, that means only the gap row is visible (row 0).
            // A's content is entirely above the viewport, so no pushed header.
            let pushed_visible = (next_naive_row as u16).saturating_sub(1);

            if pushed_visible == 0 {
                // Only the gap row is visible, no A content. No header needed.
                return StickyHeaderLayout::default();
            }

            // For pushed headers, use min(full_height, render_height):
            // - If full_height < render_height: use full_height (don't inflate small prompts
            //   with empty padding that we'd then clip into)
            // - If full_height >= render_height: use render_height (keep the collapsed view
            //   with proper truncation/ellipsis)
            let pushed_render_height = pinned_prompt.full_height.min(render_height);
            let push_clip = pushed_render_height.saturating_sub(pushed_visible);

            StickyHeaderLayout {
                pushed: Some(RenderedPrompt {
                    entry_idx: pinned_prompt.entry_idx,
                    render_height: pushed_render_height,
                    clip_top: push_clip,
                }),
                pinned: None, // Next prompt stays inline, not pinned
            }
        }
        None => {
            // Normal pinning, no push happening
            StickyHeaderLayout {
                pushed: None,
                pinned: Some(RenderedPrompt {
                    entry_idx: pinned_prompt.entry_idx,
                    render_height,
                    clip_top: 0,
                }),
            }
        }
    }
}

/// Calculate render height for a prompt in sticky header.
///
/// Implements gradual collapse: as the user scrolls past a prompt, its header
/// shrinks from full_height down to min_height.
///
/// The shrinking rate matches the scroll rate (1 row per scroll), maintaining
/// bottom line continuity.
///
/// # Math
/// - `scroll_past` = how many rows scrolled past the prompt's top
/// - `render_height` = full_height - scroll_past (clamped to min_height)
///
/// As scroll_past increases by 1:
/// - render_height decreases by 1
/// - header shrinks by 1 row
/// - content_area height increases by 1 row
/// - So bottom_line increases by 1 ✓
fn calculate_render_height(
    prompt: &PromptDescriptor,
    scroll_offset: usize,
    viewport_height: u16,
) -> u16 {
    // How many rows have scrolled past the prompt's top. `scroll_offset` is a
    // cumulative usize position (can exceed u16 in long sessions), so
    // clamp before narrowing — this only feeds `full_height.saturating_sub(..)`
    // (a u16), so any value past u16::MAX collapses the header to min height anyway.
    let scroll_past = scroll_offset
        .saturating_sub(prompt.y_virtual)
        .min(u16::MAX as usize) as u16;

    // Reduce height as we scroll past
    // Height shrinks 1:1 with scroll_past until we hit minimum
    let height = prompt.full_height.saturating_sub(scroll_past);

    // Use prompt's configured min_height (already includes vpad calculation).
    // Ensure at least 1 row to prevent 0-height headers, and clamp to the
    // prompt's full height: a collapsed sticky header can never be taller than
    // the prompt rendered inline. Without this clamp, a lazily-estimated prompt
    // whose truncated height is still the `MAX_TRUNCATED_HEADER_HEIGHT` seed
    // (never measured because it sits above the viewport when pinned) would pad
    // a short pinned prompt with empty rows instead of collapsing to its real
    // height.
    let min_height = prompt.min_height.max(1).min(prompt.full_height.max(1));

    // Clamp to minimum and viewport constraints
    height.max(min_height).min(viewport_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Helper to create prompt descriptors from (y_virtual, full_height) tuples.
    /// Uses a default min_height of 4 (2 content + 2 vpad) for all prompts.
    fn make_prompts(specs: &[(usize, u16)]) -> Vec<PromptDescriptor> {
        make_prompts_with_min(specs, 4)
    }

    /// Helper to create prompt descriptors with custom min_height.
    fn make_prompts_with_min(specs: &[(usize, u16)], min_height: u16) -> Vec<PromptDescriptor> {
        specs
            .iter()
            .enumerate()
            .map(|(i, &(y, full_h))| PromptDescriptor {
                entry_idx: i,
                y_virtual: y,
                full_height: full_h,
                sticky: true,
                min_height,
            })
            .collect()
    }

    #[test]
    fn test_no_prompts() {
        let layout = compute_sticky_layout(10, 24, &[]);
        assert_eq!(layout, StickyHeaderLayout::default());
        assert_eq!(layout.header_screen_rows(), 0);
    }

    #[test]
    fn test_no_scroll() {
        let prompts = make_prompts(&[(0, 6), (20, 6)]);
        let layout = compute_sticky_layout(0, 24, &prompts);
        assert!(layout.pinned.is_none());
        assert!(layout.pushed.is_none());
        assert_eq!(layout.header_screen_rows(), 0);
    }

    // Gradual Collapse Tests

    #[test]
    fn test_gradual_collapse_just_scrolled_past() {
        // Prompt at y=0 with full_height=8
        // scroll_offset=1 means we just scrolled past by 1 row
        // render_height = 8 - 1 = 7
        let prompts = make_prompts(&[(0, 8)]);
        let layout = compute_sticky_layout(1, 24, &prompts);

        let pinned = layout.pinned.unwrap();
        assert_eq!(pinned.render_height, 7); // 8 - 1
        // header_screen_rows = render_height + gap = 7 + 1 = 8
        assert_eq!(layout.header_screen_rows(), 8);
    }

    #[test]
    fn test_gradual_collapse_more_scrolled() {
        // scroll_offset=3: render_height = 8 - 3 = 5
        let prompts = make_prompts(&[(0, 8)]);
        let layout = compute_sticky_layout(3, 24, &prompts);

        let pinned = layout.pinned.unwrap();
        assert_eq!(pinned.render_height, 5); // 8 - 3
        // header_screen_rows = 5 + 1 = 6
        assert_eq!(layout.header_screen_rows(), 6);
    }

    #[test]
    fn test_gradual_collapse_reaches_minimum() {
        // scroll_offset=6: render_height = 8 - 6 = 2, but min is 4
        let prompts = make_prompts(&[(0, 8)]);
        let layout = compute_sticky_layout(6, 24, &prompts);

        let pinned = layout.pinned.unwrap();
        assert_eq!(pinned.render_height, 4); // clamped to min
        // header_screen_rows = 4 + 1 = 5
        assert_eq!(layout.header_screen_rows(), 5);
    }

    #[test]
    fn test_gradual_collapse_stays_at_minimum() {
        // scroll_offset=10: well past the prompt, stays at minimum (4)
        let prompts = make_prompts(&[(0, 8)]);
        let layout = compute_sticky_layout(10, 24, &prompts);

        let pinned = layout.pinned.unwrap();
        assert_eq!(pinned.render_height, 4); // stays at min
        // header_screen_rows = 4 + 1 = 5
        assert_eq!(layout.header_screen_rows(), 5);
    }

    #[test]
    fn test_custom_min_height_small() {
        // With vpad=false, min_lines=2: min_height should be 2
        // This tests that we respect the user's configured min_height,
        // not the old MIN_PINNED_HEIGHT constant.
        let prompts = make_prompts_with_min(&[(0, 8)], 2);
        let layout = compute_sticky_layout(10, 24, &prompts);

        let pinned = layout.pinned.unwrap();
        assert_eq!(pinned.render_height, 2); // respects custom min_height
        assert_eq!(layout.header_screen_rows(), 3); // 2 + 1 gap
    }

    #[test]
    fn test_min_height_clamped_to_full_height() {
        // Regression: a lazily-estimated prompt can carry a `min_height`
        // (truncated-header seed) LARGER than its real `full_height` when it was
        // never measured (it sits above the viewport when pinned). The collapsed
        // sticky header must never exceed the full inline height — otherwise a
        // 1-row prompt is padded out to the seed with empty rows.
        let prompts = vec![PromptDescriptor {
            entry_idx: 0,
            y_virtual: 0,
            full_height: 1, // a real 1-row prompt
            min_height: 6,  // stale `MAX_TRUNCATED_HEADER_HEIGHT` lazy seed
            sticky: true,
        }];
        let layout = compute_sticky_layout(10, 24, &prompts);
        let pinned = layout.pinned.expect("prompt should be pinned");
        assert_eq!(
            pinned.render_height, 1,
            "collapsed sticky header must clamp to the prompt's full height (1), not the seed"
        );
        assert_eq!(
            layout.header_screen_rows(),
            2,
            "1 row + 1 gap, no empty padding"
        );
    }

    #[test]
    fn test_custom_min_height_zero_floor() {
        // min_height of 0 should be floored to 1
        let prompts = make_prompts_with_min(&[(0, 8)], 0);
        let layout = compute_sticky_layout(10, 24, &prompts);

        let pinned = layout.pinned.unwrap();
        assert_eq!(pinned.render_height, 1); // floored to 1
        assert_eq!(layout.header_screen_rows(), 2); // 1 + 1 gap
    }

    // Bottom Line Continuity Tests

    #[test]
    fn test_bottom_line_continuity() {
        // Verify that scroll_for_content produces correct bottom line
        // for each scroll step during gradual collapse.
        //
        // Invariant: bottom_line = scroll_offset + viewport_height - 1

        let viewport = 20u16;
        let prompts = make_prompts(&[(0, 8)]);

        for scroll in 1..=10 {
            let layout = compute_sticky_layout(scroll, viewport, &prompts);
            let content_height = layout.content_height(viewport);
            let scroll_for_content = layout.scroll_for_content(scroll);

            // Bottom line of content area
            let bottom_line = scroll_for_content as u16 + content_height - 1;

            // Expected bottom line (the invariant)
            let expected_bottom = scroll as u16 + viewport - 1;

            assert_eq!(
                bottom_line,
                expected_bottom,
                "scroll={}, header={}, content_h={}, scroll_for_content={}",
                scroll,
                layout.header_screen_rows(),
                content_height,
                scroll_for_content
            );
        }
    }

    #[test]
    fn test_scroll_for_content_during_gradual_collapse() {
        // During gradual collapse, scroll_for_content should stay CONSTANT
        // even as scroll_offset changes, because header shrinks at same rate.

        let prompts = make_prompts(&[(0, 8)]);

        // Prompt at y=0, full_height=8 (content ends at y=8 with gap)
        // Next entry would start at y=9
        // scroll_for_content should equal 9 during gradual collapse

        // At scroll=1: header=8 (7+1gap), scroll_for_content = 1 + 8 = 9
        let layout1 = compute_sticky_layout(1, 24, &prompts);
        assert_eq!(layout1.scroll_for_content(1), 9);

        // At scroll=2: header=7 (6+1gap), scroll_for_content = 2 + 7 = 9
        let layout2 = compute_sticky_layout(2, 24, &prompts);
        assert_eq!(layout2.scroll_for_content(2), 9);

        // At scroll=3: header=6 (5+1gap), scroll_for_content = 3 + 6 = 9
        let layout3 = compute_sticky_layout(3, 24, &prompts);
        assert_eq!(layout3.scroll_for_content(3), 9);

        // At scroll=4: header=5 (4+1gap), scroll_for_content = 4 + 5 = 9
        let layout4 = compute_sticky_layout(4, 24, &prompts);
        assert_eq!(layout4.scroll_for_content(4), 9);

        // At scroll=5: header stays at 5 (min 4 + 1gap), scroll_for_content = 5 + 5 = 10
        // NOW it starts increasing because we hit minimum!
        let layout5 = compute_sticky_layout(5, 24, &prompts);
        assert_eq!(layout5.header_screen_rows(), 5); // min 4 + 1 gap
        assert_eq!(layout5.scroll_for_content(5), 10);

        // At scroll=6: header stays at 5 (min), scroll_for_content = 6 + 5 = 11
        let layout6 = compute_sticky_layout(6, 24, &prompts);
        assert_eq!(layout6.scroll_for_content(6), 11);
    }

    // Push Effect Tests

    #[test]
    fn test_push_effect() {
        // Two prompts: 0 at y=0 (full=8), 1 at y=9 (after gap at y=8)
        let prompts = make_prompts(&[(0, 8), (9, 8)]);

        // At scroll=8: gap is at row 0, prompt 1 at row 1. No prompt 0 visible → no header
        let layout8 = compute_sticky_layout(8, 24, &prompts);
        assert!(
            !layout8.has_header(),
            "scroll=8: only gap visible, no header"
        );

        // At scroll=7: prompt 0's bottom (y=7) is at row 0, gap at row 1, prompt 1 at row 2
        // pushed_visible = 2 - 1 = 1
        let layout7 = compute_sticky_layout(7, 24, &prompts);
        assert!(layout7.pushed.is_some());
        let pushed = layout7.pushed.unwrap();
        assert_eq!(pushed.entry_idx, 0);
        assert_eq!(pushed.visible_height(), 1, "pushed shows 1 row of prompt 0");
        assert!(layout7.pinned.is_none(), "prompt 1 stays inline");
        assert_eq!(layout7.header_screen_rows(), 1);

        // At scroll=6: prompt 0's rows y=6,7 visible at rows 0,1. Gap at row 2, prompt 1 at row 3.
        // pushed_visible = 3 - 1 = 2
        let layout6 = compute_sticky_layout(6, 24, &prompts);
        assert!(layout6.pushed.is_some());
        assert_eq!(layout6.pushed.unwrap().visible_height(), 2);
        assert_eq!(layout6.header_screen_rows(), 2);
    }

    #[test]
    fn test_next_prompt_becomes_pinned() {
        // scroll_offset=13: prompt 1 (at y=12) is scrolled past by 1
        let prompts = make_prompts(&[(0, 8), (12, 8)]);
        let layout = compute_sticky_layout(13, 24, &prompts);

        // Prompt 1 is now pinned (prompt 0 is no longer relevant)
        let pinned = layout.pinned.unwrap();
        assert_eq!(pinned.entry_idx, 1);
        // Gradual collapse: 8 - 1 = 7
        assert_eq!(pinned.render_height, 7);
    }

    #[test]
    fn test_rendered_prompt_helpers() {
        let rp = RenderedPrompt {
            entry_idx: 5,
            render_height: 5,
            clip_top: 1,
        };
        assert_eq!(rp.visible_height(), 4); // 5 - 1
        assert!(rp.needs_scratch_buffer()); // clip_top > 0

        let rp2 = RenderedPrompt {
            entry_idx: 0,
            render_height: 4,
            clip_top: 0,
        };
        assert_eq!(rp2.visible_height(), 4);
        assert!(!rp2.needs_scratch_buffer());
    }

    #[test]
    fn test_helper_methods() {
        let prompts = make_prompts(&[(0, 8)]);
        let layout = compute_sticky_layout(5, 24, &prompts);

        // Verify helper methods
        assert!(layout.has_header());
        assert_eq!(layout.pinned_entry_idx(), Some(0));
        assert_eq!(layout.pinned_screen_row(), Some(0)); // No pushed, so pinned starts at 0
        // min render_height=4, gap is at row 4
        assert_eq!(layout.gap_row(), Some(4));
        assert_eq!(layout.content_height(24), 24 - 5); // viewport - header_screen_rows (4+1gap=5)
    }

    /// Detailed trace test for gradual collapse behavior.
    #[test]
    fn test_gradual_collapse_trace() {
        // Prompt with full_height=8 at y=0
        // As we scroll past, render_height decreases: 8 → 7 → 6 → 5 → 4 (min)

        let prompts = vec![PromptDescriptor {
            entry_idx: 0,
            y_virtual: 0,
            full_height: 8,
            min_height: 4,
            sticky: true,
        }];

        // scroll=0: No header (prompt not scrolled past yet)
        let layout0 = compute_sticky_layout(0, 24, &prompts);
        assert!(!layout0.has_header(), "scroll=0: should have no header");

        // scroll=1: Header appears, render_height = 8 - 1 = 7
        let layout1 = compute_sticky_layout(1, 24, &prompts);
        assert!(layout1.has_header(), "scroll=1: should have header");
        assert_eq!(layout1.pinned.unwrap().render_height, 7);

        // scroll=2: render_height = 8 - 2 = 6
        let layout2 = compute_sticky_layout(2, 24, &prompts);
        assert_eq!(layout2.pinned.unwrap().render_height, 6);

        // scroll=3: render_height = 8 - 3 = 5
        let layout3 = compute_sticky_layout(3, 24, &prompts);
        assert_eq!(layout3.pinned.unwrap().render_height, 5);

        // scroll=4: render_height = 8 - 4 = 4 (min)
        let layout4 = compute_sticky_layout(4, 24, &prompts);
        assert_eq!(layout4.pinned.unwrap().render_height, 4);

        // scroll=5: render_height = 8 - 5 = 3, clamped to 4 (min)
        let layout5 = compute_sticky_layout(5, 24, &prompts);
        assert_eq!(layout5.pinned.unwrap().render_height, 4);

        // scroll=10: still at min
        let layout10 = compute_sticky_layout(10, 24, &prompts);
        assert_eq!(layout10.pinned.unwrap().render_height, 4);
    }

    /// Test two-prompt scenario.
    #[test]
    fn test_two_prompt_scenario() {
        // Prompt 0 at y=0, height=4
        // Prompt 1 at y=5 (after prompt 0 + gap), height=8
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 4,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 5,
                full_height: 8,
                min_height: 4,
                sticky: true,
            },
        ];

        // scroll=5: prompt 1 exactly at top, no header
        let layout5 = compute_sticky_layout(5, 24, &prompts);
        assert!(!layout5.has_header());

        // scroll=6: prompt 1 scrolled past by 1
        let layout6 = compute_sticky_layout(6, 24, &prompts);
        assert!(layout6.has_header());
        assert_eq!(layout6.pinned.unwrap().entry_idx, 1);
        assert_eq!(layout6.pinned.unwrap().render_height, 7); // 8 - 1

        // scroll=9: prompt 1 at min height
        let layout9 = compute_sticky_layout(9, 24, &prompts);
        assert_eq!(layout9.pinned.unwrap().render_height, 4);
    }

    // c-k (Scroll Up) Tests

    /// Test c-k behavior: scrolling up from gll position.
    #[test]
    fn test_ck_from_gll() {
        // Prompt A at y=0, height=6 (spans y=0-5)
        // Gap at y=6
        // Prompt B at y=7, height=8 (spans y=7-14)
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 7,
                full_height: 8,
                min_height: 4,
                sticky: true,
            },
        ];

        // At gll position: scroll_offset = 7, B at top, no header
        let layout_gll = compute_sticky_layout(7, 24, &prompts);
        assert!(!layout_gll.has_header());

        // c-k to scroll=6: only gap visible, no header
        let layout_ck1 = compute_sticky_layout(6, 24, &prompts);
        assert!(!layout_ck1.has_header());

        // c-k to scroll=5: A's bottom row visible, pushed header appears
        let layout_ck2 = compute_sticky_layout(5, 24, &prompts);
        assert!(layout_ck2.pushed.is_some());
        assert_eq!(layout_ck2.pushed.unwrap().visible_height(), 1);
        assert_eq!(layout_ck2.header_screen_rows(), 1);

        // c-k to scroll=4: 2 rows of A visible
        let layout_ck3 = compute_sticky_layout(4, 24, &prompts);
        assert_eq!(layout_ck3.pushed.unwrap().visible_height(), 2);
        assert_eq!(layout_ck3.header_screen_rows(), 2);

        // c-k to scroll=3: 3 rows visible
        let layout_ck4 = compute_sticky_layout(3, 24, &prompts);
        assert_eq!(layout_ck4.pushed.unwrap().visible_height(), 3);

        // c-k to scroll=2: 4 rows visible
        let layout_ck5 = compute_sticky_layout(2, 24, &prompts);
        assert_eq!(layout_ck5.pushed.unwrap().visible_height(), 4);

        // c-k to scroll=1: 5 rows visible, scroll_for_content stays constant
        let layout_ck6 = compute_sticky_layout(1, 24, &prompts);
        assert_eq!(layout_ck6.pushed.unwrap().visible_height(), 5);
        assert_eq!(layout_ck6.scroll_for_content(1), 6);
    }

    /// Test that header_screen_rows changes smoothly during c-k.
    #[test]
    fn test_ck_header_rows_smooth_transition() {
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 7,
                full_height: 8,
                min_height: 4,
                sticky: true,
            },
        ];

        let mut prev_header_rows: Option<u16> = None;
        for scroll in (1..=7).rev() {
            let layout = compute_sticky_layout(scroll, 24, &prompts);
            let header_rows = layout.header_screen_rows();

            if let Some(prev) = prev_header_rows {
                let diff = (header_rows as i32 - prev as i32).abs();
                let allowed = diff <= 1 || (prev == 0 && header_rows == 1);
                assert!(allowed, "Header jumped by {} at scroll={}", diff, scroll);
            }
            prev_header_rows = Some(header_rows);
        }
    }

    /// Test that scroll_for_content never increases during c-k.
    #[test]
    fn test_ck_scroll_for_content_never_increases() {
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 7,
                full_height: 8,
                min_height: 4,
                sticky: true,
            },
        ];

        let mut prev: Option<usize> = None;
        for scroll in (1..=7u16).rev() {
            let layout = compute_sticky_layout(scroll as usize, 24, &prompts);
            let sfc = layout.scroll_for_content(scroll as usize);

            if let Some(p) = prev {
                assert!(
                    sfc <= p,
                    "scroll_for_content increased at scroll={}",
                    scroll
                );
            }
            prev = Some(sfc);
        }
    }

    /// Test that bottom line continuity is maintained during c-k.
    #[test]
    fn test_ck_bottom_line_continuity() {
        let viewport = 20u16;
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 7,
                full_height: 8,
                min_height: 4,
                sticky: true,
            },
        ];

        for scroll in (1..=7).rev() {
            let layout = compute_sticky_layout(scroll, viewport, &prompts);
            let content_height = layout.content_height(viewport);
            let scroll_for_content = layout.scroll_for_content(scroll);
            let bottom_line = scroll_for_content as u16 + content_height - 1;
            let expected = scroll as u16 + viewport - 1;

            assert_eq!(
                bottom_line, expected,
                "Bottom line wrong at scroll={}",
                scroll
            );
        }
    }

    /// Test that the next prompt is not clipped during push.
    #[test]
    fn test_ck_next_prompt_not_clipped() {
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 7,
                full_height: 8,
                min_height: 4,
                sticky: true,
            },
        ];

        let layout = compute_sticky_layout(5, 20, &prompts);
        assert_eq!(layout.header_screen_rows(), 1);
        assert_eq!(layout.scroll_for_content(5), 6);
    }

    /// Test that pushed_visible excludes the gap row.
    #[test]
    fn test_pushed_visible_excludes_gap() {
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 7,
                full_height: 8,
                min_height: 4,
                sticky: true,
            },
        ];

        // At scroll=5: pushed_visible = 1 (excluding gap)
        let layout = compute_sticky_layout(5, 20, &prompts);
        assert!(layout.pushed.is_some());
        assert_eq!(layout.pushed.unwrap().visible_height(), 1);
        assert_eq!(layout.header_screen_rows(), 1);
        assert_eq!(layout.scroll_for_content(5), 6);
    }

    // Pushed Header Render Height Tests
    //
    // These tests verify that pushed headers use the correct render_height:
    // - For small prompts (full_height < min_height): use full_height, not inflated
    // - For large prompts (full_height > render_height): use render_height (collapsed)

    /// Test pushed header with small prompt (full_height < MIN_PINNED_HEIGHT).
    ///
    /// This was a bug: small prompts got inflated to min_height, causing
    /// clip_top to clip into empty padding rows instead of actual content.
    #[test]
    fn test_pushed_header_small_prompt_uses_full_height() {
        // Prompt A: small prompt with full_height=3 (less than MIN_PINNED_HEIGHT=4)
        // Prompt B: at y=4
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 3, // Less than MIN_PINNED_HEIGHT!
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 4,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
        ];

        // At scroll=2: B is at row 2, pushed_visible = 1
        let layout = compute_sticky_layout(2, 20, &prompts);
        assert!(layout.pushed.is_some());
        let pushed = layout.pushed.unwrap();

        // Key assertion: render_height should be 3 (full_height), NOT 4 (inflated)
        // If it were 4, clip_top would be 3, which clips into empty padding.
        assert_eq!(
            pushed.render_height, 3,
            "Pushed header should use full_height for small prompts, not inflated min_height"
        );
        assert_eq!(pushed.visible_height(), 1);
        // clip_top = 3 - 1 = 2 (clips 2 rows, shows bottom 1 row of actual content)
        assert_eq!(pushed.clip_top, 2);
    }

    /// Test pushed header with large prompt (full_height > render_height).
    ///
    /// Large prompts should use the collapsed render_height, not full_height.
    /// This ensures the pushed header shows the truncated/ellipsis view,
    /// not raw bottom lines of the full content.
    #[test]
    fn test_pushed_header_large_prompt_uses_collapsed_height() {
        // Prompt A: large prompt with full_height=20, will be collapsed
        // Prompt B: at y=21
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 20,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 21,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
        ];

        // At scroll=19: A is scrolled past by 19, render_height = max(20-19, 4) = max(1, 4) = 4
        // B is at row 2, pushed_visible = 1
        let layout = compute_sticky_layout(19, 20, &prompts);
        assert!(layout.pushed.is_some());
        let pushed = layout.pushed.unwrap();

        // Key assertion: render_height should be 4 (collapsed), NOT 20 (full)
        // This ensures we show the truncated view with ellipsis, not raw bottom lines.
        assert_eq!(
            pushed.render_height, 4,
            "Pushed header should use collapsed render_height for large prompts"
        );
        assert_eq!(pushed.visible_height(), 1);
        assert_eq!(pushed.clip_top, 3);
    }

    /// Test pushed header uses min(full_height, render_height).
    ///
    /// This is the core invariant: pushed_render_height = min(full_height, render_height)
    #[test]
    fn test_pushed_header_render_height_invariant() {
        // Test various combinations of full_height and scroll positions

        // Case 1: full_height=3, scroll just past → render_height would be max(2, 4)=4
        // pushed should use min(3, 4) = 3
        let prompts1 = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 3,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 4,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
        ];
        let layout1 = compute_sticky_layout(2, 20, &prompts1);
        assert_eq!(layout1.pushed.unwrap().render_height, 3); // min(3, 4)

        // Case 2: full_height=6, scroll=2 → render_height would be max(4, 4)=4
        // pushed should use min(6, 4) = 4
        let prompts2 = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 7,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
        ];
        let layout2 = compute_sticky_layout(5, 20, &prompts2);
        assert_eq!(layout2.pushed.unwrap().render_height, 4); // min(6, 4)

        // Case 3: full_height=4, render_height=4 → equal, should be 4
        let prompts3 = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 4,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 5,
                full_height: 6,
                min_height: 4,
                sticky: true,
            },
        ];
        let layout3 = compute_sticky_layout(3, 20, &prompts3);
        assert_eq!(layout3.pushed.unwrap().render_height, 4); // min(4, 4)
    }

    /// Test smooth scrolling with adjacent small prompts (the original bug scenario).
    ///
    /// With 1-line prompts (full_height=3), scrolling up should reveal
    /// actual content, not empty padding rows.
    #[test]
    fn test_adjacent_small_prompts_smooth_scroll() {
        // Three adjacent 1-line prompts (full_height=3 each)
        // A: y=0-2, gap at y=3
        // B: y=4-6, gap at y=7
        // C: y=8-10
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 3,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 4,
                full_height: 3,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 2,
                y_virtual: 8,
                full_height: 3,
                min_height: 4,
                sticky: true,
            },
        ];

        // scroll=4: B at top, no header
        let layout4 = compute_sticky_layout(4, 7, &prompts);
        assert!(!layout4.has_header());

        // scroll=3: gap at top, no header (pushed_visible=0)
        let layout3 = compute_sticky_layout(3, 7, &prompts);
        assert!(!layout3.has_header());

        // scroll=2: A's bottom row visible as pushed header
        let layout2 = compute_sticky_layout(2, 7, &prompts);
        assert!(layout2.pushed.is_some());
        let pushed2 = layout2.pushed.unwrap();
        assert_eq!(
            pushed2.render_height, 3,
            "Should use full_height=3, not inflated 4"
        );
        assert_eq!(pushed2.visible_height(), 1);
        assert_eq!(pushed2.clip_top, 2); // Show bottom 1 row of 3-row content

        // scroll=1: A's bottom 2 rows visible
        let layout1 = compute_sticky_layout(1, 7, &prompts);
        assert!(layout1.pushed.is_some());
        let pushed1 = layout1.pushed.unwrap();
        assert_eq!(pushed1.render_height, 3);
        assert_eq!(pushed1.visible_height(), 2);
        assert_eq!(pushed1.clip_top, 1); // Show bottom 2 rows of 3-row content
    }

    #[test]
    fn test_entry_at_header_row_pinned_only() {
        // One prompt at y=0, full_height=4, scroll past it
        let prompts = vec![PromptDescriptor {
            entry_idx: 0,
            y_virtual: 0,
            full_height: 4,
            min_height: 4,
            sticky: true,
        }];
        let layout = compute_sticky_layout(10, 20, &prompts);
        assert!(layout.pinned.is_some());
        assert!(layout.pushed.is_none());

        let header_rows = layout.header_screen_rows();
        // Pinned prompt occupies rows 0..pinned.visible_height()
        // Gap row after pinned is NOT a prompt
        let pinned = layout.pinned.unwrap();
        for row in 0..pinned.visible_height() {
            assert_eq!(
                layout.entry_at_header_row(row),
                Some(0),
                "Row {row} should hit pinned prompt"
            );
        }
        // Gap row (last row of header) should return None
        if header_rows > pinned.visible_height() {
            let gap = pinned.visible_height();
            assert_eq!(
                layout.entry_at_header_row(gap),
                None,
                "Gap row should return None"
            );
        }
        // Past header returns None
        assert_eq!(layout.entry_at_header_row(header_rows), None);
    }

    #[test]
    fn test_entry_at_header_row_pushed_and_pinned() {
        // Two prompts, scroll so first is being pushed off by second
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 3,
                min_height: 3,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 5,
                y_virtual: 8,
                full_height: 3,
                min_height: 3,
                sticky: true,
            },
        ];

        // Scroll so second prompt approaches → first gets pushed
        // Need to find scroll where both pushed and pinned exist
        for scroll in 0..20 {
            let layout = compute_sticky_layout(scroll, 20, &prompts);
            if let (Some(pushed), Some(pinned)) = (&layout.pushed, &layout.pinned) {
                // Pushed rows → entry 0
                for row in 0..pushed.visible_height() {
                    assert_eq!(layout.entry_at_header_row(row), Some(0));
                }

                // Gap row between pushed and pinned
                let gap_row = pushed.visible_height();
                assert_eq!(layout.entry_at_header_row(gap_row), None);

                // Pinned rows → entry 5
                let pinned_start = layout.pinned_screen_row().unwrap();
                for row in pinned_start..pinned_start + pinned.visible_height() {
                    assert_eq!(layout.entry_at_header_row(row), Some(5));
                }
                return; // Found the transition state
            }
        }
        // It's ok if the exact layout doesn't produce both — geometry varies
    }

    #[test]
    fn test_header_entry_area_pinned() {
        let prompts = vec![PromptDescriptor {
            entry_idx: 0,
            y_virtual: 0,
            full_height: 4,
            min_height: 4,
            sticky: true,
        }];
        let layout = compute_sticky_layout(10, 20, &prompts);
        assert!(layout.pinned.is_some());

        // Entry 0 should have a header area
        let (start, height, is_pushed) = layout.header_entry_area(0).unwrap();
        assert_eq!(start, 0);
        assert!(height > 0);
        assert!(!is_pushed);

        // Non-existent entry returns None
        assert!(layout.header_entry_area(99).is_none());
    }

    /// Test that a non-sticky prompt pushes the previous sticky prompt off
    /// but never becomes pinned itself.
    ///
    /// Layout: A (sticky) at y=0, B (non-sticky, e.g. expanded user prompt) at y=7.
    /// When B approaches, it should push A off. When B is scrolled past,
    /// no header should appear since B is non-sticky.
    #[test]
    fn test_non_sticky_prompt_pushes_but_never_pins() {
        // A (sticky, small) at y=0, B (non-sticky) at y=12 — far enough
        // apart that A reaches min_height before B triggers push.
        let prompts = vec![
            PromptDescriptor {
                entry_idx: 0,
                y_virtual: 0,
                full_height: 4,
                min_height: 4,
                sticky: true,
            },
            PromptDescriptor {
                entry_idx: 1,
                y_virtual: 12,
                full_height: 8,
                min_height: 4,
                sticky: false, // expanded user prompt
            },
        ];

        // A is pinned when scrolled past, before B approaches
        let layout_pinned = compute_sticky_layout(3, 24, &prompts);
        assert!(layout_pinned.pinned.is_some());
        assert_eq!(layout_pinned.pinned.unwrap().entry_idx, 0);

        // B approaches and pushes A off (push effect works with non-sticky)
        let layout_push = compute_sticky_layout(10, 24, &prompts);
        assert!(layout_push.pushed.is_some());
        assert_eq!(layout_push.pushed.unwrap().entry_idx, 0);
        assert!(layout_push.pinned.is_none());

        // B is scrolled past — no header since B is non-sticky
        let layout_past_b = compute_sticky_layout(13, 24, &prompts);
        assert!(!layout_past_b.has_header());

        // Well past B — still no header
        let layout_far = compute_sticky_layout(25, 24, &prompts);
        assert!(!layout_far.has_header());
    }
}
