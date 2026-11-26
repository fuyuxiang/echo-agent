use super::*;

impl ListPaneState {
    /// Create a new state with the given wrap mode and follow-mode flag.
    ///
    /// Uses default config (all features enabled).
    /// For custom config, use [`new_with_config`].
    pub fn new(wrap_mode: WrapMode, follow_mode: bool) -> Self {
        Self::new_with_config(wrap_mode, follow_mode, ListPaneConfig::default())
    }

    /// Create a new state with explicit config.
    pub fn new_with_config(wrap_mode: WrapMode, follow_mode: bool, config: ListPaneConfig) -> Self {
        Self {
            scroll_offset: 0,
            viewport_height: 0,
            selected_id: None,
            selected_index: None,
            multi_selected_ids: None,
            multi_range: None,
            visual_mode: false,
            visual_anchor_id: None,
            layout: ListLayoutCache::fixed(0),
            wrap_mode,
            last_stamp: None,
            filter_dirty: true, // force first build
            last_item_count: 0,
            scroll_anchor: None,
            scroll_screen_y: None,
            follow_mode: follow_mode && config.follow_enabled,
            at_content_edge: false,
            overscroll_ticks: 0,
            matcher: None,
            vis_map: None,
            scroll_margin: 2,
            last_scrollbar_area: None,
            show_highlights: true,
            height_cache: Vec::new(),
            height_cache_width: 0,
            config,
            input_mode: None,
            input_textarea: {
                let mut ta = TextArea::new();
                ta.show_scrollbar = false;
                ta
            },
            input_textarea_state: TextAreaState::default(),
            input_cursor_screen_pos: None,
            scrollbar_dragging: false,
            clipboard: Box::new(InternalClipboard::default()),
            copy_toast_until: None,
            goto_line_snapshot: None,
            goto_line_had_visual: false,
        }
    }

    // =======================================================================
    // Public accessors
    // =======================================================================

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Set the scroll offset directly (clamped to valid range on next layout).
    pub fn set_scroll_offset(&mut self, offset: usize) {
        self.scroll_offset = offset;
    }

    pub fn viewport_height(&self) -> u16 {
        self.viewport_height
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    pub fn selected_id(&self) -> Option<u64> {
        self.selected_id
    }

    /// Set the selected item by stable ID.
    ///
    /// The index will be resolved on the next `prepare_layout` call.
    pub fn select_by_id(&mut self, id: u64) {
        self.selected_id = Some(id);
        self.selected_index = None; // will be resolved in prepare_layout
    }

    pub fn multi_range(&self) -> Option<Range<usize>> {
        self.multi_range.clone()
    }

    /// Get the range of physical item indices to copy.
    ///
    /// In visual mode: the visual selection range.
    /// Without visual mode: the single selected item (range of 1).
    /// Returns `None` if nothing is selected.
    pub fn copy_range(&self) -> Option<Range<usize>> {
        if self.visual_mode {
            self.multi_range.clone()
        } else {
            self.selected_index.map(|vi| {
                let pi = self.to_physical(vi);
                pi..pi + 1
            })
        }
    }

    pub fn layout(&self) -> &ListLayoutCache {
        &self.layout
    }

    pub fn wrap_mode(&self) -> WrapMode {
        self.wrap_mode
    }

    /// The active matcher, if any.
    pub fn matcher(&self) -> Option<&ListMatcher> {
        self.matcher.as_ref()
    }

    /// Backward-compatible accessor: returns a `ListFilter` view when a
    /// Filter-mode matcher is active.
    pub fn filter(&self) -> Option<ListFilter> {
        self.matcher
            .as_ref()
            .map(|m| ListFilter { matcher: m.clone() })
    }

    /// Map visible index → physical index.
    ///
    /// When no filter is active, returns `vi` unchanged (identity).
    /// When filtering, looks up the stored vis map.
    #[inline]
    pub fn to_physical(&self, vi: usize) -> usize {
        match &self.vis_map {
            Some(v) => v.get(vi).copied().unwrap_or(vi),
            None => vi,
        }
    }

    /// Resolve a visible index to an item.
    ///
    /// Returns `None` when layout/`vis_map` is stale relative to `items`
    /// (model cleared or shrunk between `prepare_layout` and a key/mouse handler).
    #[inline]
    fn item_at<'a, T: ListItem>(&self, vi: usize, items: &'a [T]) -> Option<&'a T> {
        let pi = match &self.vis_map {
            Some(v) => *v.get(vi)?,
            None => vi,
        };
        items.get(pi)
    }

    /// Total height in visual lines (from the layout cache).
    pub fn total_height(&self) -> usize {
        self.layout.total_height()
    }

    /// Number of visible items (from the layout cache).
    pub fn visible_count(&self) -> usize {
        self.layout.item_count()
    }

    /// The scrollbar's screen area from the last render.
    /// `None` if content fits the viewport (no scrollbar shown).
    pub fn scrollbar_area(&self) -> Option<Rect> {
        self.last_scrollbar_area
    }

    /// Set the scrollbar's screen area (called by the renderer).
    pub fn set_scrollbar_area(&mut self, area: Option<Rect>) {
        self.last_scrollbar_area = area;
    }

    /// Whether a scrollbar drag is in progress.
    pub fn is_scrollbar_dragging(&self) -> bool {
        self.scrollbar_dragging
    }

    /// Whether a mouse position lands in the scrollbar's grab zone
    /// ([`crate::render::scrollbar::scrollbar_grab_zone`]).
    pub fn scrollbar_hit(&self, column: u16, row: u16) -> bool {
        self.scrollbar_area().is_some_and(|sb| {
            crate::render::scrollbar::scrollbar_grab_zone(sb).contains((column, row).into())
        })
    }

    /// Whether search/filter is enabled in the config.
    pub fn is_search_enabled(&self) -> bool {
        self.config.search_enabled
    }

    /// Whether selection should be shown when unfocused.
    pub fn show_selection_when_unfocused(&self) -> bool {
        self.config.show_selection_when_unfocused
    }

    /// Number of items matching the current search/filter query.
    /// Returns 0 when no matcher is active.
    pub fn match_count(&self) -> usize {
        self.matcher.as_ref().map(|m| m.match_count()).unwrap_or(0)
    }

    /// Whether the input bar is currently active.
    pub fn input_mode(&self) -> Option<InputBarMode> {
        self.input_mode
    }

    /// Number of rows the bottom bar (editable input bar or accepted-matcher
    /// status) will occupy when this pane is rendered into an area of the
    /// given height. Returns `0` when no bar is shown.
    ///
    /// Mirrors the split logic in [`ListPane::render`](super::render) so panes
    /// that draw their own overlays on top of the list (e.g. the tasks pane's
    /// kill/view buttons and spinners) can avoid painting over the bar row.
    pub fn bottom_bar_height(&self, area_height: u16) -> u16 {
        // Comment mode may need multiple rows for multi-line input; every
        // other mode (and the accepted-matcher status line) uses a single row.
        let bar_height = if matches!(self.input_mode, Some(InputBarMode::Comment)) {
            let line_count = self.input_text().chars().filter(|c| *c == '\n').count() + 1;
            (line_count as u16).clamp(1, 5)
        } else {
            1
        };
        if (self.input_mode.is_some() || self.matcher.is_some()) && area_height > bar_height {
            bar_height
        } else {
            0
        }
    }

    /// Whether the "Copied!" toast should be displayed.
    ///
    /// Returns `true` for ~800ms after a successful copy.
    pub fn copy_toast_active(&self) -> bool {
        self.copy_toast_until
            .is_some_and(|t| std::time::Instant::now() < t)
    }

    /// Whether the pane needs animation ticks (for toast expiry).
    pub fn needs_tick(&self) -> bool {
        self.copy_toast_until.is_some()
    }

    /// Advance time-based state. Returns `true` if a redraw is needed
    /// (e.g., toast just expired).
    pub fn tick(&mut self) -> bool {
        if let Some(t) = self.copy_toast_until
            && std::time::Instant::now() >= t
        {
            self.copy_toast_until = None;
            return true; // toast just expired, need redraw
        }
        false
    }

    /// The input textarea (for rendering by the `ListPane` widget).
    pub fn input_textarea(&self) -> &TextArea {
        &self.input_textarea
    }

    /// Render the input textarea into the given area, with visible cursor.
    ///
    /// Encapsulates the borrow split (textarea + textarea_state are both
    /// fields on `self`).  Called by the renderer.  Stores the cursor
    /// screen position for [`cursor_position`].
    pub fn render_input_textarea(&mut self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        use ratatui::style::Modifier;
        use ratatui::widgets::StatefulWidgetRef;
        use unicode_width::UnicodeWidthStr;

        (&self.input_textarea).render_ref(area, buf, &mut self.input_textarea_state);

        // Draw a visible cursor (REVERSED cell at cursor position).
        // Also store the screen position so the caller can set the
        // terminal hardware cursor via Frame::set_cursor_position().
        if area.width > 0 && area.height > 0 {
            let text_before_cursor = &self.input_textarea.text()[..self.input_textarea.cursor()];
            let cursor_line = text_before_cursor.chars().filter(|c| *c == '\n').count();
            let last_line_start = text_before_cursor.rfind('\n').map(|i| i + 1).unwrap_or(0);
            let cursor_col = text_before_cursor[last_line_start..]
                .width()
                .min(area.width as usize - 1) as u16;
            let x = area.x + cursor_col;
            let y = (area.y + cursor_line as u16).min(area.y + area.height - 1);
            buf[(x, y)].modifier.insert(Modifier::REVERSED);
            self.input_cursor_screen_pos = Some((x, y));
        }
    }

    /// Screen position of the input bar cursor, if the input bar is active.
    ///
    /// Returns `Some((x, y))` after rendering.  The caller can pass this
    /// to `Frame::set_cursor_position()` for a hardware blinking cursor.
    /// Returns `None` when the input bar is closed or before the first render.
    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        if self.input_mode.is_some() {
            self.input_cursor_screen_pos
        } else {
            None
        }
    }

    // =======================================================================
    // Visual select mode
    // =======================================================================

    /// Enter visual selection mode, anchored at the current selection.
    ///
    /// If in follow mode, exits follow first (materializes cursor),
    /// then enters visual.  No-op if visual select is disabled.
    pub fn enter_visual_mode<T: ListItem>(&mut self, items: &[T]) {
        if !self.config.visual_select_enabled || self.visual_mode {
            return;
        }
        // Exit follow mode to materialize a cursor.
        if self.follow_mode {
            self.exit_follow(items);
        }
        if let Some(sid) = self.selected_id {
            self.visual_mode = true;
            self.visual_anchor_id = Some(sid);
            // multi_selected_ids will be set in prepare_layout.
        }
    }

    /// Exit visual selection mode, clearing the range.
    pub fn exit_visual_mode(&mut self) {
        self.visual_mode = false;
        self.visual_anchor_id = None;
        self.multi_selected_ids = None;
        self.multi_range = None;
    }

    /// Clear visual mode if active.  Called by non-navigation actions
    /// (`/`, `f`, `F`, `G`, `g`) to implicitly exit visual selection.
    fn clear_visual_if_active(&mut self) {
        if self.visual_mode {
            self.exit_visual_mode();
        }
    }

    // =======================================================================
    // Clipboard (copy)
    // =======================================================================

    /// Replace the clipboard provider (e.g. with a system clipboard).
    pub fn set_clipboard_provider(&mut self, provider: Box<dyn ClipboardProvider>) {
        self.clipboard = provider;
    }

    /// Disable follow mode permanently (sets `follow_enabled = false`).
    ///
    /// After this, `G` selects the last item instead of engaging follow.
    /// Use when the data source is no longer streaming.
    pub fn disable_follow_permanently(&mut self) {
        self.config.follow_enabled = false;
        self.follow_mode = false;
    }

    /// Set a scroll anchor: after the next layout rebuild, the selected item
    /// will be placed at this screen-y offset.
    ///
    /// Use this before operations that change item content/count (e.g., raw
    /// mode toggle) to keep the selected item at the same screen position.
    pub fn set_scroll_anchor(&mut self) {
        if let Some(vi) = self.selected_index {
            let item_y = self.layout.virtual_y(vi);
            let screen_y = item_y.saturating_sub(self.scroll_offset);
            self.scroll_anchor = Some(screen_y);
        }
    }

    /// Invalidate the layout cache, forcing a full rebuild on the next
    /// `prepare_layout` call.
    ///
    /// Use this when the item content has changed without changing the item
    /// count (e.g., streaming content updates where existing items are replaced
    /// with different content/heights).
    pub fn invalidate_layout(&mut self) {
        self.last_stamp = None;
        self.height_cache.clear();
        self.height_cache_width = 0;
    }

    /// Copy the selected item(s) content to the clipboard.
    ///
    /// Single selection: copies the plain text of `content()`.
    /// Visual selection: copies all items in the range, joined by `\n`.
    /// No-op in follow mode (no selection) or when `copy_enabled` is false.
    /// Returns `true` if something was copied.
    pub fn copy_selected<T: ListItem>(&mut self, items: &[T]) -> bool {
        if !self.config.copy_enabled {
            return false;
        }

        // Visual mode: copy the entire range.
        if self.visual_mode {
            if let Some(ref range) = self.multi_range {
                let mut lines = Vec::with_capacity(range.len());
                for vi in range.clone() {
                    if let Some(item) = self.item_at(vi, items) {
                        lines.push(item.copy_text());
                    }
                }
                let joined = lines.join("\n");
                if !joined.is_empty() {
                    self.clipboard.set(&joined);
                    return true;
                }
            }
            return false;
        }

        // Single selection.
        let Some(vi) = self.selected_index else {
            return false;
        };
        let Some(item) = self.item_at(vi, items) else {
            return false;
        };
        let text = item.copy_text();
        if text.is_empty() {
            return false;
        }
        self.clipboard.set(&text);
        true
    }

    // =======================================================================
    // Scrollbar interaction
    // =======================================================================

    /// Set scroll offset to a specific value and select the nearest item
    /// at viewport center.
    ///
    /// Used by scrollbar click/drag — the caller computes the offset using
    /// [`crate::render::scrollbar::scrollbar_click_to_offset`] which uses
    /// the same `ScrollMetrics` as the renderer, guaranteeing the thumb
    /// lands exactly where the user clicked.
    pub fn set_scroll_offset_and_center<T: ListItem>(&mut self, offset: usize, items: &[T]) {
        let total = self.layout.total_height();
        let vp = self.viewport_height as usize;
        let max_offset = total.saturating_sub(vp);
        self.scroll_offset = offset.min(max_offset);
        self.scroll_screen_y = None;
        self.reset_edge_state();

        if self.scroll_offset >= max_offset && self.config.follow_enabled {
            self.engage_follow();
        } else {
            self.follow_mode = false;
            self.clamp_scroll();
            // Select nearest item at viewport center.
            let mid_y = self.scroll_offset + vp / 2;
            self.select_nearest_at_y(mid_y, items);
        }
    }

    /// Scroll by a percentage of total content height.
    ///
    /// Positive = scroll down, negative = scroll up.
    /// Selects the nearest item at viewport center afterward.
    pub fn scroll_percent<T: ListItem>(&mut self, percent: f64, items: &[T]) {
        let total = self.layout.total_height() as f64;
        let delta = (total * percent / 100.0).round() as i32;
        self.scroll_and_center(delta, items);
    }

    /// Scroll by the given number of lines (positive = down), then select
    /// the nearest item at viewport center.
    ///
    /// Unlike [`scroll_lines`] (which pins selection at the same screen-y),
    /// this is designed for scrollbar interactions where the user expects
    /// proportional navigation rather than cursor-locked scrolling.
    pub fn scroll_and_center<T: ListItem>(&mut self, delta: i32, items: &[T]) {
        if delta == 0 {
            return;
        }
        self.follow_mode = false;
        self.reset_edge_state();
        self.scroll_screen_y = None;
        if delta > 0 {
            self.scroll_down_raw(delta as usize);
        } else {
            self.scroll_up_raw((-delta) as usize);
        }
        if self.is_at_bottom() && self.config.follow_enabled {
            self.engage_follow();
        } else {
            // Move selection to center of viewport.
            let mid_y = self.scroll_offset + (self.viewport_height as usize / 2);
            self.select_nearest_at_y(mid_y, items);
        }
    }

    // =======================================================================
    // Wrap mode
    // =======================================================================

    /// Toggle wrap mode: NoWrap ↔ Wrap.
    ///
    /// Records a scroll anchor so that `prepare_layout` will keep the
    /// selected item's top line at the same screen-y position (fold/unfold
    /// effect).
    pub fn cycle_wrap_mode(&mut self) {
        // Record anchor: screen-y of the selected item before the mode switch.
        if let Some(vi) = self.selected_index {
            let item_y = self.layout.virtual_y(vi);
            let screen_y = item_y.saturating_sub(self.scroll_offset);
            self.scroll_anchor = Some(screen_y);
        }
        self.wrap_mode = match self.wrap_mode {
            WrapMode::NoWrap => WrapMode::Wrap,
            WrapMode::Wrap => WrapMode::NoWrap,
        };
    }

    /// Set wrap mode explicitly.
    pub fn set_wrap_mode(&mut self, mode: WrapMode) {
        self.wrap_mode = mode;
        // Wrap mode change forces full rebuild on next prepare_layout.
    }

    // =======================================================================
    // Filter
    // =======================================================================

    /// Set or clear the matcher (filter or search).
    ///
    /// Caller should call `prepare_layout` afterward to recompute
    /// match indices and layout.
    pub fn set_matcher(&mut self, matcher: Option<ListMatcher>) {
        self.matcher = matcher;
        self.filter_dirty = true;
    }

    /// Set or clear the filter (backward-compatible).
    pub fn set_filter(&mut self, matcher: Option<ListMatcher>) {
        self.set_matcher(matcher);
    }

    // =======================================================================
    // Match navigation (n / N)
    // =======================================================================

    /// Jump to the next match after the current selection.
    ///
    /// In Filter mode, moves to the next filtered item.
    /// In Search mode, moves to the next matching item (all items visible).
    /// Wraps around at the end.
    pub fn next_match<T: ListItem>(&mut self, items: &[T]) {
        let current_pi = self.selected_index.map(|vi| self.to_physical(vi));
        let Some(m) = self.matcher.as_mut() else {
            return;
        };
        let pi = current_pi.unwrap_or(0);
        let Some(target_pi) = m.next_match_after(pi) else {
            return;
        };
        self.jump_to_physical(target_pi, items);
    }

    /// Jump to the previous match before the current selection.
    ///
    /// Wraps around at the beginning.
    pub fn prev_match<T: ListItem>(&mut self, items: &[T]) {
        let current_pi = self.selected_index.map(|vi| self.to_physical(vi));
        let Some(m) = self.matcher.as_mut() else {
            return;
        };
        let pi = current_pi.unwrap_or(0);
        let Some(target_pi) = m.prev_match_before(pi) else {
            return;
        };
        self.jump_to_physical(target_pi, items);
    }

    /// Select the item at the given physical index and scroll it into view.
    ///
    /// Works in both filtered and unfiltered mode: when a filter is active,
    /// finds the visible index for the physical index; when no filter,
    /// vi == pi.
    fn jump_to_physical<T: ListItem>(&mut self, target_pi: usize, items: &[T]) {
        // Find the visible index for this physical index.
        let target_vi = match &self.vis_map {
            Some(v) => {
                // Binary search in the sorted vis_map.
                v.binary_search(&target_pi).ok()
            }
            None => {
                // No filter → vi == pi.
                if target_pi < items.len() {
                    Some(target_pi)
                } else {
                    None
                }
            }
        };
        if let Some(vi) = target_vi
            && let Some(item) = items.get(target_pi)
        {
            self.follow_mode = false;
            self.reset_edge_state();
            self.scroll_screen_y = None;
            self.selected_index = Some(vi);
            self.selected_id = Some(item.stable_id());
            self.ensure_selected_visible();
        }
    }

    // =======================================================================
    // prepare_layout — the ONE generic entry point
    // =======================================================================

    /// Recompute layout, resolve stable-ID selection → indices, clamp scroll.
    ///
    /// Call this once per frame before rendering.  `items` is the full
    /// (unfiltered) slice from the model.  `width` is the content width
    /// available for item rendering.  `viewport_height` is the pane height
    /// in terminal rows.
    ///
    /// # Performance
    ///
    /// Uses dirty tracking to avoid redundant work:
    /// - **No change**: width, wrap mode, filter, and item count unchanged →
    ///   skips layout rebuild entirely (still resolves selection + clamps scroll).
    /// - **Append only** (Wrap mode, same width, no filter change, count grew):
    ///   computes `desired_height` only for the *new* items and extends the
    ///   existing prefix-sum cache.
    /// - **Full rebuild**: width/mode/filter changed, or items were removed.
    ///
    // Terminal resize triggers a full rebuild (new width). Resize events are
    // debounced at the event-loop level so only the final size rebuilds.
    pub fn prepare_layout<T: ListItem>(&mut self, items: &[T], width: u16, viewport_height: u16) {
        // Reserve 1 row for the bottom bar when the input bar is open
        // or a matcher is accepted (showing status).
        self.viewport_height = if self.input_mode.is_some() || self.matcher.is_some() {
            viewport_height.saturating_sub(1)
        } else {
            viewport_height
        };

        // -- Build visible-item index map (filter) ----------------------------
        let filter_changed = self.filter_dirty;
        if filter_changed {
            self.filter_dirty = false;
        }

        // Reuse the cached vis_map when the filter hasn't changed and item
        // count is unchanged.  This avoids re-running the filter predicate
        // on every frame (expensive for 100K+ items).
        let items_len_changed = items.len() != self.last_item_count;
        let need_refilter = filter_changed || items_len_changed;
        self.last_item_count = items.len();

        // Rebuild match indices when needed, then derive vis_map.
        if need_refilter && let Some(ref mut m) = self.matcher {
            m.rebuild_matches(items);
        }

        let vis: Option<Vec<usize>> = match &self.matcher {
            Some(m) if m.mode == MatchMode::Filter => {
                if need_refilter {
                    // Derive vis_map from match_indices (they're physical indices).
                    Some(m.match_indices.clone())
                } else {
                    // Reuse existing vis_map — filter and items unchanged.
                    self.vis_map.take()
                }
            }
            // Search mode or no matcher → all items visible.
            _ => None,
        };

        let vis_count = match &vis {
            Some(v) => v.len(),
            None => items.len(),
        };

        let to_physical = |vi: usize| -> usize {
            match &vis {
                Some(v) => v[vi],
                None => vi,
            }
        };

        // -- SCROLLBAR WIDTH FIX: Determine effective width for layout ---------
        //
        // In Wrap mode, the scrollbar takes SCROLLBAR_TOTAL_COLS (2) columns.
        // If we compute heights at full width but render at narrow width (due to
        // scrollbar), items may need MORE lines at the narrower width, causing
        // truncation.
        //
        // Two-phase approach:
        // 1. If vis_count > viewport, scrollbar is definitely needed → use width - 2
        // 2. Otherwise, compute at full width, check total, recompute if wrong
        //
        // Phase 2 only triggers when we guess wrong (rare: few items that wrap
        // heavily to exceed viewport). This avoids constant re-computation.
        let definitely_needs_scrollbar =
            self.wrap_mode == WrapMode::Wrap && vis_count > self.viewport_height as usize;
        let mut effective_width = if definitely_needs_scrollbar {
            width.saturating_sub(SCROLLBAR_TOTAL_COLS)
        } else {
            width
        };

        // -- Maintain per-physical-item height cache (ALL modes) ----------------
        //
        // The height cache stores `desired_height(effective_width)` for every
        // physical item, regardless of the current wrap mode.  It is:
        //   - Fully rebuilt when `effective_width` changes.
        //   - Extended when new items are appended (same width).
        //   - Looked up (not recomputed) when only the filter changes.
        //
        // By caching eagerly even in NoWrap mode, toggling to Wrap is nearly
        // free — all heights are already computed.  The per-item cost on
        // append is microseconds (one `desired_height` call per new item).
        {
            let width_changed = self.height_cache_width != effective_width;
            let items_shrunk = items.len() < self.height_cache.len();
            if width_changed || items_shrunk {
                // Full recompute — width changed or items were evicted
                // from the front (indices shifted, cache is stale).
                self.height_cache = items
                    .iter()
                    .map(|item| item.desired_height(effective_width))
                    .collect();
                self.height_cache_width = effective_width;
            } else if items.len() > self.height_cache.len() {
                // Incremental extend — new items appended at same width.
                let old_len = self.height_cache.len();
                self.height_cache.extend(
                    items[old_len..]
                        .iter()
                        .map(|item| item.desired_height(effective_width)),
                );
            }
        }

        // -- Decide whether we can skip / incrementally update the layout -----
        let (width_same, mode_same, count_grew, old_count) = match self.last_stamp {
            Some(s) => (
                s.width == effective_width,
                s.wrap == self.wrap_mode,
                vis_count >= s.count,
                s.count,
            ),
            None => (false, false, true, 0),
        };
        let count_same = vis_count == old_count;

        // Filter refilter can keep the same vis_count while remapping visible
        // rows to different physical items; heights would go stale without a rebuild.
        let filter_refiltered = need_refilter && vis.is_some();
        let needs_full_rebuild =
            !width_same || !mode_same || filter_changed || !count_grew || filter_refiltered;

        if needs_full_rebuild {
            // Full layout rebuild — but heights come from the cache (no wrapping).
            self.layout = match self.wrap_mode {
                WrapMode::NoWrap => ListLayoutCache::fixed(vis_count),
                WrapMode::Wrap => {
                    let heights = (0..vis_count).map(|vi| self.height_cache[to_physical(vi)]);
                    ListLayoutCache::from_heights(effective_width, heights)
                }
            };
        } else if !count_same {
            // Incremental append — heights from cache.
            match self.wrap_mode {
                WrapMode::NoWrap => {
                    self.layout = ListLayoutCache::fixed(vis_count);
                }
                WrapMode::Wrap => {
                    let new_heights =
                        (old_count..vis_count).map(|vi| self.height_cache[to_physical(vi)]);
                    self.layout.extend_heights(new_heights);
                }
            }
        }
        // else: count_same + same width/mode/filter → cache is still valid.

        // -- SCROLLBAR WIDTH FIX Phase 2: Check if we guessed wrong ------------
        //
        // If we computed at full width but total_height > viewport (scrollbar
        // will actually be shown), recompute at narrower width.
        // This only happens when: Wrap mode + few items that wrap a lot.
        if self.wrap_mode == WrapMode::Wrap
            && !definitely_needs_scrollbar
            && self.layout.total_height() > self.viewport_height as usize
        {
            let narrow_width = width.saturating_sub(SCROLLBAR_TOTAL_COLS);
            if narrow_width != effective_width {
                // We guessed wrong — scrollbar will be shown, need narrower width.
                effective_width = narrow_width;

                // Recompute height cache at narrow width.
                self.height_cache = items
                    .iter()
                    .map(|item| item.desired_height(effective_width))
                    .collect();
                self.height_cache_width = effective_width;

                // Rebuild layout with corrected heights.
                let heights = (0..vis_count).map(|vi| self.height_cache[to_physical(vi)]);
                self.layout = ListLayoutCache::from_heights(effective_width, heights);
            }
        }

        // Update dirty-tracking stamp with effective_width.
        self.last_stamp = Some(LayoutStamp {
            width: effective_width,
            count: vis_count,
            wrap: self.wrap_mode,
        });

        // -- Resolve selected_id → selected_index -----------------------------
        self.selected_index = self
            .selected_id
            .and_then(|sid| (0..vis_count).find(|&vi| items[to_physical(vi)].stable_id() == sid));

        // If the selected ID is gone, clear selection.
        if self.selected_id.is_some() && self.selected_index.is_none() {
            self.selected_id = None;
        }

        // -- Visual mode: sync multi_selected_ids from anchor + cursor --------
        if self.visual_mode
            && let (Some(anchor), Some(cursor)) = (self.visual_anchor_id, self.selected_id)
        {
            self.multi_selected_ids = Some((anchor, cursor));
        }

        // -- Resolve multi_selected_ids → multi_range -------------------------
        self.multi_range = self.multi_selected_ids.and_then(|(a, b)| {
            let a_idx = (0..vis_count).find(|&vi| items[to_physical(vi)].stable_id() == a);
            let b_idx = (0..vis_count).find(|&vi| items[to_physical(vi)].stable_id() == b);
            match (a_idx, b_idx) {
                (Some(a), Some(b)) => {
                    let lo = a.min(b);
                    let hi = a.max(b);
                    Some(lo..hi + 1)
                }
                _ => None,
            }
        });

        // -- Apply scroll anchor (wrap toggle y-stability) --------------------
        // If a scroll anchor was set (e.g. by cycle_wrap_mode), adjust
        // scroll_offset so the selected item appears at the recorded screen-y.
        if let Some(desired_screen_y) = self.scroll_anchor.take()
            && let Some(vi) = self.selected_index
        {
            let new_item_y = self.layout.virtual_y(vi);
            self.scroll_offset = new_item_y.saturating_sub(desired_screen_y);
        }

        // -- Follow mode: auto-scroll to bottom, no cursor ----------------------
        if self.follow_mode {
            let total = self.layout.total_height();
            let vp = self.viewport_height as usize;
            self.scroll_offset = total.saturating_sub(vp);
            // No selection in follow mode.
            self.selected_id = None;
            self.selected_index = None;
        }

        // -- Auto-select if nothing selected (NAV mode only) --------------------
        // A list with items but no selection feels broken (no highlight, j/k do
        // nothing visible).  Auto-select the first selectable item.
        // In follow mode, selection is always None — skip this block.
        if !self.follow_mode && self.selected_index.is_none() && vis_count > 0 {
            for vi in 0..vis_count {
                if items[to_physical(vi)].is_selectable() {
                    self.selected_index = Some(vi);
                    self.selected_id = Some(items[to_physical(vi)].stable_id());
                    break;
                }
            }
        }

        // Store the vis map for the renderer — must come after auto-select
        // (which uses `to_physical` closure that borrows `vis`).
        self.vis_map = vis;

        // -- Clamp scroll -------------------------------------------------------
        self.clamp_scroll();

        // -- Keep selection visible after eviction --------------------------------
        // Only when items SHRINK (eviction from front), which shifts indices and
        // can push the selection off-screen.  NOT on append (which just extends
        // below) — that would fight with user's click/scroll position via margin.
        let items_shrunk = vis_count < old_count;
        if !self.follow_mode && items_shrunk {
            self.ensure_selected_visible();
        }
    }

    // =======================================================================
    // Scroll
    // =======================================================================

    /// Scroll down by `n` visual lines (viewport only, no follow logic).
    ///
    /// This is a low-level primitive.  Higher-level methods
    /// (`half_page_down`, `scroll_lines`, etc.) call this and then
    /// handle follow-mode transitions.
    fn scroll_down_raw(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
        self.clamp_scroll();
    }

    /// Scroll up by `n` visual lines (viewport only, no follow logic).
    fn scroll_up_raw(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Public scroll_down — exits follow if in follow mode, then scrolls.
    /// Used by scrollbar interactions.
    pub fn scroll_down(&mut self, n: usize) {
        self.follow_mode = false;
        self.scroll_down_raw(n);
    }

    /// Public scroll_up — exits follow if in follow mode, then scrolls.
    pub fn scroll_up(&mut self, n: usize) {
        self.follow_mode = false;
        self.scroll_up_raw(n);
    }

    /// Scroll the viewport by `delta` visual lines (positive = down),
    /// keeping the selection at the same screen-y position.
    ///
    /// This is nvim's Ctrl-d/u behavior: both viewport and cursor move
    /// by the same amount, so the cursor stays at the same row on screen.
    ///
    /// **Edge case (nvim-correct):** when the viewport is clamped at the
    /// top or bottom (can't scroll the full delta), the cursor continues
    /// to move by the remaining amount.  E.g. Ctrl-d near the bottom:
    /// viewport stops at max scroll, cursor keeps going down to the
    /// last selectable item.
    ///
    /// Uses [`scroll_screen_y`] to pin the desired screen-y across
    /// consecutive scroll operations, preventing drift when crossing
    /// non-selectable items (separators).
    ///
    /// Behavior:
    /// - In FOLLOW + downward: exit follow, cursor at last visible,
    ///   `at_content_edge = true` (already at bottom).
    /// - In FOLLOW + upward: exit follow, cursor at last visible,
    ///   then scroll up.
    /// - In NAV + downward: scroll, then one-past logic if at bottom.
    /// - In NAV + upward: scroll, reset edge state.
    fn scroll_keeping_screen_y<T: ListItem>(&mut self, delta: isize, items: &[T]) {
        let is_down = delta > 0;

        // -- Handle FOLLOW mode -----------------------------------------------
        if self.follow_mode {
            if is_down {
                return; // no-op: already at the bottom
            }
            // Upward: exit follow, materialize cursor, then scroll.
            self.exit_follow(items);
        }

        // -- NAV mode scroll --------------------------------------------------
        if !is_down {
            self.reset_edge_state();
        }

        // Use pinned screen-y if available, otherwise compute and pin it.
        let screen_y = match self.scroll_screen_y {
            Some(sy) => Some(sy),
            None => self.selected_index.map(|vi| {
                let item_y = self.layout.virtual_y(vi);
                item_y.saturating_sub(self.scroll_offset)
            }),
        };

        // Record scroll offset before clamping so we can detect how much
        // was actually consumed.
        let offset_before = self.scroll_offset;

        // Scroll (raw — no follow logic).
        if is_down {
            self.scroll_down_raw(delta as usize);
        } else {
            self.scroll_up_raw((-delta) as usize);
        }

        let actual_scroll = self.scroll_offset as isize - offset_before as isize;
        let leftover = delta - actual_scroll; // lines the viewport couldn't consume

        // Restore selection at the pinned screen-y, then apply leftover.
        if let Some(sy) = screen_y {
            self.scroll_screen_y = Some(sy); // persist for next scroll
            // Target virtual-y: pinned screen-y + leftover movement.
            let target_y = (self.scroll_offset + sy).saturating_add_signed(leftover);
            self.select_nearest_at_y(target_y, items);
        }

        // -- One-past logic for downward scrolls ------------------------------
        // Suppressed in visual mode (don't snap to follow mid-selection).
        if self.config.follow_enabled && !self.visual_mode && is_down && self.is_at_bottom() {
            if self.at_content_edge {
                self.engage_follow();
            } else {
                self.at_content_edge = true;
            }
        }
    }

    /// Select the nearest selectable item whose virtual-y range contains `y`.
    /// If the exact item isn't selectable, search forward then backward,
    /// but only within the current viewport to avoid selecting off-screen items.
    fn select_nearest_at_y<T: ListItem>(&mut self, y: usize, items: &[T]) {
        let count = self.layout.item_count();
        if count == 0 || items.is_empty() {
            return;
        }
        let Some(target_vi) = self.layout.item_at_y(y) else {
            return;
        };

        // Try the target item first.
        if let Some(item) = self.item_at(target_vi, items)
            && item.is_selectable()
        {
            self.selected_index = Some(target_vi);
            self.selected_id = Some(item.stable_id());
            return;
        }

        // Viewport bounds for constrained search.
        let vp_top = self.scroll_offset;
        let vp_bottom = self.scroll_offset + self.viewport_height as usize;

        // Search forward (within viewport).
        for vi in (target_vi + 1)..count {
            if self.layout.virtual_y(vi) >= vp_bottom {
                break;
            }
            if let Some(item) = self.item_at(vi, items)
                && item.is_selectable()
            {
                self.selected_index = Some(vi);
                self.selected_id = Some(item.stable_id());
                return;
            }
        }

        // Search backward (within viewport).
        for vi in (0..target_vi).rev() {
            let vy = self.layout.virtual_y(vi);
            let vh = self.layout.item_height(vi) as usize;
            if vy + vh <= vp_top {
                break;
            }
            if let Some(item) = self.item_at(vi, items)
                && item.is_selectable()
            {
                self.selected_index = Some(vi);
                self.selected_id = Some(item.stable_id());
                return;
            }
        }
        // No selectable item in viewport — leave selection unchanged.
    }

    /// Scroll down by half a page, keeping selection at same screen-y.
    pub fn half_page_down<T: ListItem>(&mut self, items: &[T]) {
        let half = (self.viewport_height / 2).max(1) as isize;
        self.scroll_keeping_screen_y(half, items);
    }

    /// Scroll up by half a page, keeping selection at same screen-y.
    pub fn half_page_up<T: ListItem>(&mut self, items: &[T]) {
        let half = (self.viewport_height / 2).max(1) as isize;
        self.scroll_keeping_screen_y(-half, items);
    }

    /// Scroll down by a full page, keeping selection at same screen-y.
    pub fn page_down<T: ListItem>(&mut self, items: &[T]) {
        let page = self.viewport_height.max(1) as isize;
        self.scroll_keeping_screen_y(page, items);
    }

    /// Scroll up by a full page, keeping selection at same screen-y.
    pub fn page_up<T: ListItem>(&mut self, items: &[T]) {
        let page = self.viewport_height.max(1) as isize;
        self.scroll_keeping_screen_y(-page, items);
    }

    /// Scroll by `lines` (positive = down, negative = up) from mouse wheel.
    /// Keeps selection at same screen-y.
    ///
    /// Uses overscroll counter for NAV → FOLLOW
    /// transition instead of the one-past boolean used by keyboard scrolls.
    pub fn scroll_lines<T: ListItem>(&mut self, lines: i32, items: &[T]) {
        let is_down = lines > 0;

        // No-op when content fits viewport (nothing to scroll).
        let total = self.layout.total_height();
        let vp = self.viewport_height as usize;
        if total <= vp && !self.follow_mode {
            return;
        }

        // Mouse wheel exits visual mode (mouse = single-select interaction).
        self.clear_visual_if_active();

        // -- Handle FOLLOW mode -----------------------------------------------
        if self.follow_mode {
            if is_down {
                return; // no-op: already at the bottom
            }
            // Upward: exit follow, materialize cursor, then scroll.
            self.exit_follow(items);
        }

        if !is_down {
            self.reset_edge_state();
        }

        // Mouse wheel: scroll viewport + move selection to keep same screen-y.
        // Same as scroll_keeping_screen_y but with overscroll counter.
        let screen_y = match self.scroll_screen_y {
            Some(sy) => Some(sy),
            None => self.selected_index.map(|vi| {
                let item_y = self.layout.virtual_y(vi);
                item_y.saturating_sub(self.scroll_offset)
            }),
        };

        let offset_before = self.scroll_offset;
        let delta = lines as isize;

        if is_down {
            self.scroll_down_raw(delta as usize);
        } else {
            self.scroll_up_raw((-delta) as usize);
        }

        let actual_scroll = self.scroll_offset as isize - offset_before as isize;

        // Only move selection if the viewport actually scrolled.
        // Unlike ctrl-d/u, mouse wheel does NOT apply leftover — if the
        // viewport is clamped (top/bottom), the selection stays put.
        if actual_scroll != 0
            && let Some(sy) = screen_y
        {
            self.scroll_screen_y = Some(sy);
            let target_y = self.scroll_offset + sy;
            self.select_nearest_at_y(target_y, items);
        }

        // -- Overscroll counter for mouse wheel -------------------------------
        // Suppressed in visual mode.
        if self.config.follow_enabled && !self.visual_mode && is_down && self.is_at_bottom() {
            let scroll_amount = lines.unsigned_abs().min(u8::MAX as u32) as u8;
            if self.at_content_edge {
                self.overscroll_ticks = self.overscroll_ticks.saturating_add(scroll_amount);
                if self.overscroll_ticks >= MOUSE_OVERSCROLL_THRESHOLD {
                    self.engage_follow();
                }
            } else {
                self.at_content_edge = true;
                self.overscroll_ticks = 0;
            }
        }
    }

    /// Jump to the top.  Used internally; callers typically use `select_first`.
    pub fn goto_top(&mut self) {
        self.follow_mode = false;
        self.reset_edge_state();
        self.scroll_screen_y = None;
        self.scroll_offset = 0;
    }

    /// Jump to the bottom and engage follow mode.
    pub fn goto_bottom(&mut self) {
        self.engage_follow();
    }

    /// Center the viewport on the selected item.
    ///
    /// Like vim's `zz`: places the selected item vertically centered.
    /// No-op in follow mode (no cursor to center).
    pub fn center_selected(&mut self) {
        if self.follow_mode {
            return;
        }
        let Some(vi) = self.selected_index else {
            return;
        };
        self.reset_edge_state();
        self.scroll_screen_y = None;
        let item_y = self.layout.virtual_y(vi);
        let item_h = self.layout.item_height(vi) as usize;
        let vp = self.viewport_height as usize;
        // Place item midpoint at viewport midpoint.
        let item_mid = item_y + item_h / 2;
        self.scroll_offset = item_mid.saturating_sub(vp / 2);
        self.clamp_scroll();
    }

    /// Clamp `scroll_offset` so the viewport doesn't extend past content.
    fn clamp_scroll(&mut self) {
        let total = self.layout.total_height();
        let vp = self.viewport_height as usize;
        if total <= vp {
            self.scroll_offset = 0;
        } else {
            self.scroll_offset = self.scroll_offset.min(total - vp);
        }
    }

    /// Check if the viewport is at the bottom of the content.
    ///
    /// Returns `true` when `scroll_offset + viewport_height >= total_height`,
    /// i.e. there are no more lines below the viewport.
    fn is_at_bottom(&self) -> bool {
        let total = self.layout.total_height();
        let vp = self.viewport_height as usize;
        total <= vp || self.scroll_offset >= total.saturating_sub(vp)
    }

    // =======================================================================
    // Follow ↔ NAV transitions
    // =======================================================================

    /// Engage follow mode: jump viewport to bottom, hide cursor.
    ///
    /// No-op when `follow_enabled` is false in config.
    fn engage_follow(&mut self) {
        if !self.config.follow_enabled {
            return;
        }
        self.follow_mode = true;
        self.selected_id = None;
        self.selected_index = None;
        self.scroll_screen_y = None;
        self.at_content_edge = false;
        self.overscroll_ticks = 0;
        let total = self.layout.total_height();
        let vp = self.viewport_height as usize;
        self.scroll_offset = total.saturating_sub(vp);
    }

    /// Exit follow mode and materialize a cursor at the last visible
    /// selectable item.  Returns the visible index placed, if any.
    ///
    /// Callers that need a different initial cursor position (click,
    /// `g`/Home, match navigation) should call this first, then
    /// override `selected_index` / `selected_id` with the desired target.
    fn exit_follow<T: ListItem>(&mut self, items: &[T]) -> Option<usize> {
        self.follow_mode = false;
        self.at_content_edge = false;
        self.overscroll_ticks = 0;
        self.scroll_screen_y = None;

        // Materialize cursor at last visible selectable item.
        let range = self.visible_range();
        for vi in range.rev() {
            if let Some(item) = self.item_at(vi, items)
                && item.is_selectable()
            {
                self.selected_index = Some(vi);
                self.selected_id = Some(item.stable_id());
                return Some(vi);
            }
        }
        None
    }

    /// Reset edge-tracking state (at_content_edge + overscroll_ticks).
    /// Called on any action that is NOT a "downward push at the bottom."
    fn reset_edge_state(&mut self) {
        self.at_content_edge = false;
        self.overscroll_ticks = 0;
    }

    // =======================================================================
    // Selection
    // =======================================================================

    /// Select the next selectable item (downward) — `j` / `↓`.
    ///
    /// Behavior:
    /// - In FOLLOW: **no-op** (already at the bottom, nowhere to go down).
    /// - In NAV: move down.  If can't move (already last selectable),
    ///   one-past logic applies (second j at end → engage follow).
    pub fn select_next<T: ListItem>(&mut self, items: &[T]) {
        if self.follow_mode {
            return; // no-op: already at the bottom
        }

        self.scroll_screen_y = None;
        let count = self.layout.item_count();
        if count == 0 || items.is_empty() {
            return;
        }

        // Try to move down.
        let start = self.selected_index.map(|i| i + 1).unwrap_or(0);
        for vi in start..count {
            if let Some(item) = self.item_at(vi, items)
                && item.is_selectable()
            {
                // Moved successfully — reset edge state.
                self.selected_index = Some(vi);
                self.selected_id = Some(item.stable_id());
                self.ensure_selected_visible();
                self.reset_edge_state();
                return;
            }
        }

        // Can't move down — one-past logic.
        // Suppressed in visual mode (don't snap to follow mid-selection).
        if self.config.follow_enabled && !self.visual_mode {
            if self.at_content_edge {
                self.engage_follow();
            } else {
                self.at_content_edge = true;
            }
        }
    }

    /// Select the previous selectable item (upward) — `k` / `↑`.
    ///
    /// Behavior:
    /// - In FOLLOW: exit follow, cursor at last visible, then move up 1.
    /// - In NAV: move up.  Resets edge state.
    pub fn select_prev<T: ListItem>(&mut self, items: &[T]) {
        if items.is_empty() {
            return;
        }
        if self.follow_mode {
            self.exit_follow(items);
            // Now move up one from the materialized cursor.
        }

        self.reset_edge_state();
        self.scroll_screen_y = None;
        let start = match self.selected_index {
            Some(0) | None => return,
            Some(i) => i - 1,
        };
        for vi in (0..=start).rev() {
            if let Some(item) = self.item_at(vi, items)
                && item.is_selectable()
            {
                self.selected_index = Some(vi);
                self.selected_id = Some(item.stable_id());
                self.ensure_selected_visible();
                return;
            }
        }
    }

    /// Select the first selectable item — `g` / `Home`.
    ///
    /// Behavior:
    /// - In FOLLOW: exit follow, cursor at first selectable, scroll to top.
    /// - In NAV: cursor to first selectable, scroll to top.  Resets edge state.
    pub fn select_first<T: ListItem>(&mut self, items: &[T]) {
        if self.follow_mode {
            self.follow_mode = false;
        }
        self.reset_edge_state();
        self.scroll_screen_y = None;
        let count = self.layout.item_count();
        for vi in 0..count {
            if let Some(item) = self.item_at(vi, items)
                && item.is_selectable()
            {
                self.selected_index = Some(vi);
                self.selected_id = Some(item.stable_id());
                self.ensure_selected_visible();
                return;
            }
        }
    }

    /// Select the last selectable item — `G` / `End`.
    ///
    /// When follow is enabled and NOT in visual mode:
    /// - In FOLLOW: no-op (already following).
    /// - In NAV: engage follow immediately.
    ///
    /// When follow is disabled or in visual mode: selects the last selectable
    /// item normally (visual mode needs the cursor to stay visible).
    pub fn select_last<T: ListItem>(&mut self, items: &[T]) {
        if self.config.follow_enabled && !self.visual_mode {
            if self.follow_mode {
                return; // already following — no-op
            }
            self.engage_follow();
            return;
        }

        // Follow disabled or visual mode — just select the last selectable item.
        self.reset_edge_state();
        self.scroll_screen_y = None;
        let count = self.layout.item_count();
        for vi in (0..count).rev() {
            if let Some(item) = self.item_at(vi, items)
                && item.is_selectable()
            {
                self.selected_index = Some(vi);
                self.selected_id = Some(item.stable_id());
                self.ensure_selected_visible();
                return;
            }
        }
    }

    /// Select item at visible index `vi`, if selectable.
    ///
    /// On click: exits follow, cursor on clicked item.
    /// Clears visual mode (click = single select).
    pub fn select_at<T: ListItem>(&mut self, target_vi: usize, items: &[T]) {
        let count = self.layout.item_count();
        if target_vi >= count {
            return;
        }
        if let Some(item) = self.item_at(target_vi, items)
            && item.is_selectable()
        {
            self.clear_visual_if_active();
            self.follow_mode = false;
            self.reset_edge_state();
            self.scroll_screen_y = None;
            self.selected_index = Some(target_vi);
            self.selected_id = Some(item.stable_id());
        }
    }

    /// Clear selection.
    pub fn clear_selection(&mut self) {
        self.selected_id = None;
        self.selected_index = None;
        self.multi_selected_ids = None;
        self.multi_range = None;
        self.visual_mode = false;
        self.visual_anchor_id = None;
        self.scroll_screen_y = None;
    }

    /// Ensure the selected item is visible within the viewport,
    /// accounting for scroll margin.
    fn ensure_selected_visible(&mut self) {
        let Some(vi) = self.selected_index else {
            return;
        };
        let item_y = self.layout.virtual_y(vi);
        let item_h = self.layout.item_height(vi) as usize;
        let vp = self.viewport_height as usize;
        // Adaptive margin: shrink when viewport is too small for full margin.
        // Need at least 1 row for the item itself + 2×margin, so margin ≤ (vp-1)/2.
        let margin = (self.scroll_margin as usize).min(vp.saturating_sub(1) / 2);

        // If item top is above viewport (with margin), scroll up.
        let desired_top = item_y.saturating_sub(margin);
        if item_y < self.scroll_offset + margin {
            self.scroll_offset = desired_top;
        }

        // If item bottom is below viewport (with margin), scroll down.
        let item_bottom = item_y + item_h;
        let desired_bottom = item_bottom + margin;
        if desired_bottom > self.scroll_offset + vp {
            self.scroll_offset = desired_bottom.saturating_sub(vp);
        }

        self.clamp_scroll();
    }

    // =======================================================================
    // Visible range (for rendering)
    // =======================================================================

    /// Return the range of visible-item indices that overlap the viewport.
    ///
    /// For `FixedHeight`, this is `scroll_offset .. scroll_offset + viewport_height`
    /// clamped to `0..item_count`.
    ///
    /// For `Variable`, uses `item_at_y` for the top and then walks forward.
    pub fn visible_range(&self) -> Range<usize> {
        let count = self.layout.item_count();
        if count == 0 || self.viewport_height == 0 {
            return 0..0;
        }
        let vp = self.viewport_height as usize;

        let first = self.layout.item_at_y(self.scroll_offset).unwrap_or(0);

        // Walk forward to find the last visible item.
        let viewport_end = self.scroll_offset + vp;
        let mut last = first;
        for i in first..count {
            let item_y = self.layout.virtual_y(i);
            if item_y >= viewport_end {
                break;
            }
            last = i;
        }

        first..last + 1
    }

    /// Number of visual lines to skip at the top of the first visible item
    /// (partial visibility due to scroll offset).
    pub fn first_item_skip_rows(&self) -> u16 {
        let range = self.visible_range();
        if range.is_empty() {
            return 0;
        }
        let first_y = self.layout.virtual_y(range.start);
        (self.scroll_offset - first_y) as u16
    }

    // =======================================================================
    // Keyboard input
    // =======================================================================

    /// Paste into the active input bar. Returns `false` when no editor is open.
    pub fn handle_paste<T: ListItem>(&mut self, text: &str, items: &[T]) -> bool {
        let Some(mode) = self.input_mode else {
            return false;
        };
        let old_text = self.input_textarea.text().to_owned();
        if mode == InputBarMode::Comment {
            self.input_textarea.insert_str(text);
        } else {
            let cleaned = crate::input::line_editor::sanitize_single_line(text);
            self.input_textarea.insert_str(&cleaned);
        }
        if self.input_textarea.text() == old_text {
            return false;
        }
        match mode {
            InputBarMode::GotoLine => self.apply_goto_line_live(items),
            InputBarMode::Search | InputBarMode::Filter => self.apply_input_buffer(items),
            InputBarMode::Comment => {}
        }
        true
    }

    /// Handle a key event for navigation, search, and filter.
    ///
    /// Returns `true` if the key was consumed (state changed), `false` if
    /// the key is unrecognized and should be propagated to the caller.
    ///
    /// **Must be called after `prepare_layout`** (so the layout cache and
    /// vis_map are current).
    ///
    /// When the input bar is active, typing keys are routed to the textarea.
    /// Navigation keys (j/k, Ctrl-d/u, arrows, PgDn/PgUp) still work
    /// while the input bar is open.
    pub fn handle_key_event<T: ListItem>(
        &mut self,
        event: &crossterm::event::KeyEvent,
        items: &[T],
    ) -> bool {
        // -- Input bar active: intercept Enter/Esc, route typing to textarea --
        if let Some(mode) = self.input_mode {
            // GotoLine mode has its own Enter/Esc/text handling.
            if mode == InputBarMode::GotoLine {
                if key!(Enter).matches(event) {
                    self.accept_goto_line(items);
                    return true;
                }
                if key!(Esc).matches(event) || key!('c', CONTROL).matches(event) {
                    self.cancel_goto_line();
                    return true;
                }
                if self.input_textarea.text().is_empty()
                    && (key!(Backspace).matches(event) || key!('w', CONTROL).matches(event))
                {
                    self.cancel_goto_line();
                    return true;
                }
                // Type into the bar, then apply live preview.
                let old_text = self.input_textarea.text().to_owned();
                self.input_textarea.input(*event);
                // Strip newlines.
                if self.input_textarea.text().contains('\n') {
                    let cleaned: String = self
                        .input_textarea
                        .text()
                        .chars()
                        .filter(|c| *c != '\n' && *c != '\r')
                        .collect();
                    let cursor = self.input_textarea.cursor().min(cleaned.len());
                    self.input_textarea.set_text(&cleaned);
                    self.input_textarea.set_cursor(cursor);
                }
                if self.input_textarea.text() != old_text {
                    self.apply_goto_line_live(items);
                }
                return true;
            }

            // Comment/Feedback mode: bare Enter and Esc are not consumed —
            // the caller manages save/cancel. Shift+Enter inserts a newline.
            if mode == InputBarMode::Comment {
                // Apple Terminal: Shift+Enter arrives as bare Enter (no Kitty
                // protocol). Poll CoreGraphics for the real modifier state
                // and insert a newline instead of returning to the caller.
                if key!(Enter).matches(event)
                    && crate::input::is_apple_terminal_newline_modifier_held()
                {
                    self.input_textarea.insert_str("\n");
                    return true;
                }
                if key!(Enter).matches(event) || key!(Esc).matches(event) {
                    return false; // let the caller handle
                }
                // Shift+Enter / Alt+Enter: insert a literal newline.
                if key!(Enter, SHIFT).matches(event) || key!(Enter, ALT).matches(event) {
                    let pos = self.input_textarea.cursor();
                    let mut text = self.input_textarea.text().to_owned();
                    text.insert(pos, '\n');
                    self.input_textarea.set_text(&text);
                    self.input_textarea.set_cursor(pos + 1);
                    return true;
                }
                self.input_textarea.input(*event);
                return true;
            }

            // Search/Filter mode:
            // Enter on non-empty → accept (keep matcher, close bar).
            if key!(Enter).matches(event) {
                self.accept_input(items);
                return true;
            }
            if key!(Esc).matches(event) {
                self.cancel_input();
                return true;
            }
            // Ctrl-C: clear input, or close if already empty.
            if key!('c', CONTROL).matches(event) {
                if self.input_textarea.text().is_empty() {
                    self.cancel_input();
                } else {
                    self.input_textarea.set_text("");
                    self.apply_input_buffer(items);
                }
                return true;
            }
            // Backspace / Ctrl-W on empty → close input bar (like nvim).
            if self.input_textarea.text().is_empty()
                && (key!(Backspace).matches(event) || key!('w', CONTROL).matches(event))
            {
                self.cancel_input();
                return true;
            }
            // All other keys → textarea (typing, backspace, cursor, etc.)
            let old_text = self.input_textarea.text().to_owned();
            self.input_textarea.input(*event);
            // Strip newlines — input bar is single-line only.
            if self.input_textarea.text().contains('\n') {
                let cleaned: String = self
                    .input_textarea
                    .text()
                    .chars()
                    .filter(|c| *c != '\n' && *c != '\r')
                    .collect();
                let cursor = self.input_textarea.cursor().min(cleaned.len());
                self.input_textarea.set_text(&cleaned);
                self.input_textarea.set_cursor(cursor);
            }
            if self.input_textarea.text() != old_text {
                self.apply_input_buffer(items);
            }
            return true; // always consume when input bar is open
        }

        // -- Normal mode: check for search/filter/follow keys first -----------

        // '/' → open search bar (clears visual mode)
        if self.config.search_enabled && key!('/').matches(event) {
            self.clear_visual_if_active();
            self.open_input(InputBarMode::Search, items);
            return true;
        }

        // 'f' → open filter bar (clears visual mode)
        if self.config.filter_enabled && key!('f').matches(event) {
            self.clear_visual_if_active();
            self.open_input(InputBarMode::Filter, items);
            return true;
        }

        // ':' → open goto-line bar (preserves visual mode for range extension)
        if self.config.goto_line_enabled && key!(':').matches(event) {
            self.open_goto_line(items);
            return true;
        }

        // 'F' → follow toggle (clears visual mode)
        if self.config.follow_enabled && key!('F').matches(event) {
            self.clear_visual_if_active();
            self.toggle_follow(items);
            return true;
        }

        // n/N → next/prev match (only when matcher is active)
        if self.config.search_enabled && self.matcher.is_some() {
            if key!('n').matches(event) {
                self.next_match(items);
                return true;
            }
            if key!('N').matches(event) {
                self.prev_match(items);
                return true;
            }
        }

        // 'v' / 'V' (Shift-v) → toggle visual selection mode
        if self.config.visual_select_enabled
            && (key!('v').matches(event) || key!('V').matches(event))
        {
            if self.visual_mode {
                self.exit_visual_mode();
            } else {
                self.enter_visual_mode(items);
            }
            return true;
        }

        // Shift-j / Shift-k / Shift-Down / Shift-Up → start or extend visual selection
        if self.config.visual_select_enabled
            && (key!('J').matches(event) || key!(Down, SHIFT).matches(event))
        {
            if !self.visual_mode {
                self.enter_visual_mode(items);
            }
            self.select_next(items);
            return true;
        }
        if self.config.visual_select_enabled
            && (key!('K').matches(event) || key!(Up, SHIFT).matches(event))
        {
            if !self.visual_mode {
                self.enter_visual_mode(items);
            }
            self.select_prev(items);
            return true;
        }
        // Shift-PageDown / Shift-Ctrl-D → visual select + page down
        if self.config.visual_select_enabled
            && (key!(PageDown, SHIFT).matches(event) || key!('d', CONTROL | SHIFT).matches(event))
        {
            if !self.visual_mode {
                self.enter_visual_mode(items);
            }
            self.page_down(items);
            return true;
        }
        // Shift-PageUp / Shift-Ctrl-U → visual select + page up
        if self.config.visual_select_enabled
            && (key!(PageUp, SHIFT).matches(event) || key!('u', CONTROL | SHIFT).matches(event))
        {
            if !self.visual_mode {
                self.enter_visual_mode(items);
            }
            self.page_up(items);
            return true;
        }

        // 'y' → copy selected item(s) (clears visual mode after copy)
        if self.config.copy_enabled && key!('y').matches(event) {
            if self.copy_selected(items) {
                self.copy_toast_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
            }
            self.clear_visual_if_active();
            return true;
        }

        // Esc → clear visual mode or accepted matcher
        if key!(Esc).matches(event) {
            if self.visual_mode {
                self.exit_visual_mode();
                return true;
            }
            if self.matcher.is_some() {
                self.set_matcher(None);
                self.show_highlights = true;
                return true;
            }
        }

        // Navigation keys.
        self.handle_nav_key(event, items)
    }

    /// Handle navigation keys (shared between normal mode and input-bar-open).
    fn handle_nav_key<T: ListItem>(
        &mut self,
        event: &crossterm::event::KeyEvent,
        items: &[T],
    ) -> bool {
        // Selection movement (j/k/↑/↓/Ctrl-N/Ctrl-P)
        if key!('j').matches(event)
            || key!(Down).matches(event)
            || key!('n', CONTROL).matches(event)
        {
            self.select_next(items);
            return true;
        }
        if key!('k').matches(event) || key!(Up).matches(event) || key!('p', CONTROL).matches(event)
        {
            self.select_prev(items);
            return true;
        }

        // Viewport scroll (Ctrl-j/k) — selection follows at same screen-y
        if key!('j', CONTROL).matches(event) {
            self.scroll_keeping_screen_y(1, items);
            return true;
        }
        if key!('k', CONTROL).matches(event) {
            self.scroll_keeping_screen_y(-1, items);
            return true;
        }

        // Half-page scroll (Ctrl-d/u) — selection follows at same screen-y
        if key!('d', CONTROL).matches(event) {
            self.half_page_down(items);
            return true;
        }
        if key!('u', CONTROL).matches(event) {
            self.half_page_up(items);
            return true;
        }

        // Full page scroll (PgDn/PgUp) — selection follows at same screen-y
        if key!(PageDown).matches(event) {
            self.page_down(items);
            return true;
        }
        if key!(PageUp).matches(event) {
            self.page_up(items);
            return true;
        }

        // Top/bottom (g / G / Home / End)
        if key!('g').matches(event) {
            self.select_first(items);
            return true;
        }
        if key!('G').matches(event) {
            self.select_last(items);
            return true;
        }
        if key!(Home).matches(event) {
            self.select_first(items);
            return true;
        }
        if key!(End).matches(event) {
            self.select_last(items);
            return true;
        }

        // Wrap mode toggle (w) — gated on config.
        if self.config.wrap_toggle_enabled && key!('w').matches(event) {
            self.cycle_wrap_mode();
            return true;
        }

        // Center selected item (z — like vim's zz)
        if key!('z').matches(event) {
            self.center_selected();
            return true;
        }

        false
    }

    // =======================================================================
    // Input bar lifecycle
    // =======================================================================

    /// Open the input bar in the given mode.
    fn open_input<T: ListItem>(&mut self, mode: InputBarMode, items: &[T]) {
        self.input_mode = Some(mode);
        self.show_highlights = true;
        // If reopening with an existing matcher in the same mode, keep the
        // buffer text.  Otherwise start fresh.
        let reopen_same = self
            .matcher
            .as_ref()
            .is_some_and(|m| m.mode == mode.match_mode());
        if !reopen_same {
            self.input_textarea.set_text("");
            self.set_matcher(None);
        }
        // Exit follow mode so the user can see matches.
        if self.follow_mode {
            self.exit_follow(items);
        }
    }

    /// Accept the current input (Enter) — keep matcher active, close bar.
    fn accept_input<T: ListItem>(&mut self, _items: &[T]) {
        let is_filter = matches!(
            self.matcher.as_ref().map(|m| m.mode),
            Some(MatchMode::Filter)
        );
        // In Filter mode every visible line matches — suppress highlights.
        // In Search mode highlights help find matches — keep them.
        self.show_highlights = !is_filter;
        if self.input_textarea.text().is_empty() {
            // Empty query → cancel.
            self.set_matcher(None);
            self.show_highlights = true;
        }
        self.input_mode = None;
    }

    /// Cancel the input bar (Esc) — clear matcher, close bar.
    fn cancel_input(&mut self) {
        self.input_mode = None;
        self.input_textarea.set_text("");
        self.set_matcher(None);
        self.show_highlights = true;
    }

    /// Clear any active input bar and matcher.
    ///
    /// Public counterpart of `cancel_input`. Clears both the input bar
    /// AND any accepted matcher.
    pub fn clear_input_and_matcher(&mut self) {
        self.cancel_input();
    }

    // ── Goto-line mode ────────────────────────────────────────────────

    /// Open the goto-line input bar. Saves a snapshot for cancel/restore.
    fn open_goto_line<T: ListItem>(&mut self, items: &[T]) {
        // Save snapshot.
        self.goto_line_snapshot = Some(GotoLineSnapshot {
            scroll_offset: self.scroll_offset,
            selected_id: self.selected_id,
            visual_mode: self.visual_mode,
            visual_anchor_id: self.visual_anchor_id,
        });
        self.goto_line_had_visual = self.visual_mode;

        self.input_mode = Some(InputBarMode::GotoLine);
        self.input_textarea.set_text("");
        // Exit follow mode.
        if self.follow_mode {
            self.exit_follow(items);
        }
    }

    /// Apply live goto-line preview while the user types.
    ///
    /// Parses the current input as `N` or `N-M` and live-updates the
    /// selection and scroll position. Uses 1-based line numbers mapped
    /// to `stable_id` (which for SourceLine is the line number).
    fn apply_goto_line_live<T: ListItem>(&mut self, items: &[T]) {
        let text = self.input_textarea.text().to_owned();
        if text.is_empty() {
            // Restore snapshot when input is cleared.
            if let Some(ref snap) = self.goto_line_snapshot {
                self.scroll_offset = snap.scroll_offset;
                self.selected_id = snap.selected_id;
                self.visual_mode = snap.visual_mode;
                self.visual_anchor_id = snap.visual_anchor_id;
                self.selected_index = None; // will be resolved in prepare_layout
            }
            return;
        }

        let item_count = items.len();
        if item_count == 0 {
            return;
        }

        let parsed = parse_goto_input(&text, item_count);

        // Helper: center the viewport on a given item index.
        let vp = self.viewport_height as usize;
        let center_on = |this: &mut Self, idx: usize| {
            if vp > 0 && this.layout.total_height() > vp {
                let target_y = this.layout.virtual_y(idx);
                let max_offset = this.layout.total_height().saturating_sub(vp);
                this.scroll_offset = target_y.saturating_sub(vp / 2).min(max_offset);
            }
        };

        // Resolve a user-typed line number to an item index.
        // If any item implements goto_line_number(), find the first item
        // whose source line number matches. Otherwise fall back to
        // 0-based index (line - 1).
        let has_line_numbers = items.iter().any(|i| i.goto_line_number().is_some());
        let resolve_line = |line: usize| -> usize {
            if has_line_numbers {
                items
                    .iter()
                    .position(|i| i.goto_line_number() == Some(line))
                    .unwrap_or_else(|| {
                        // Find the closest source line.
                        items
                            .iter()
                            .enumerate()
                            .filter_map(|(idx, i)| i.goto_line_number().map(|ln| (idx, ln)))
                            .min_by_key(|(_, ln)| (*ln as isize - line as isize).unsigned_abs())
                            .map(|(idx, _)| idx)
                            .unwrap_or((line - 1).min(item_count - 1))
                    })
            } else {
                (line - 1).min(item_count - 1)
            }
        };

        match parsed {
            GotoTarget::Single(line) => {
                let idx = resolve_line(line);
                if self.goto_line_had_visual {
                    // Extend existing visual selection to this line.
                    if !self.visual_mode
                        && let Some(ref snap) = self.goto_line_snapshot
                    {
                        self.visual_mode = snap.visual_mode;
                        self.visual_anchor_id = snap.visual_anchor_id;
                    }
                    self.selected_id = Some(items[idx].stable_id());
                    self.selected_index = None;
                } else {
                    // Just jump (no visual).
                    self.exit_visual_mode();
                    self.selected_id = Some(items[idx].stable_id());
                    self.selected_index = None;
                }
                center_on(self, idx);
            }
            GotoTarget::Range(start, end) => {
                let start_idx = resolve_line(start);
                let end_idx = resolve_line(end);
                let end_idx = end_idx.max(start_idx);

                self.visual_mode = true;
                self.visual_anchor_id = Some(items[start_idx].stable_id());
                self.selected_id = Some(items[end_idx].stable_id());
                self.selected_index = None;
                // Center on the end of the range (the part the user is actively typing).
                center_on(self, end_idx);
            }
            GotoTarget::Invalid => {
                if let Some(ref snap) = self.goto_line_snapshot {
                    self.scroll_offset = snap.scroll_offset;
                    self.selected_id = snap.selected_id;
                    self.visual_mode = snap.visual_mode;
                    self.visual_anchor_id = snap.visual_anchor_id;
                    self.selected_index = None;
                }
            }
        }
    }

    /// Accept goto-line input (Enter). Re-applies the input text to finalize,
    /// then closes the bar. This ensures Enter always confirms what's typed,
    /// even if the user clicked elsewhere with the mouse during goto mode.
    fn accept_goto_line<T: ListItem>(&mut self, items: &[T]) {
        // Re-apply the input to ensure we land on what's typed, not
        // whatever the mouse may have changed.
        self.apply_goto_line_live(items);
        self.input_mode = None;
        self.input_textarea.set_text("");
        self.goto_line_snapshot = None;
        self.goto_line_had_visual = false;
    }

    /// Cancel goto-line (Esc). Restores the snapshot.
    fn cancel_goto_line(&mut self) {
        if let Some(snap) = self.goto_line_snapshot.take() {
            self.scroll_offset = snap.scroll_offset;
            self.selected_id = snap.selected_id;
            self.visual_mode = snap.visual_mode;
            self.visual_anchor_id = snap.visual_anchor_id;
            self.selected_index = None;
        }
        self.input_mode = None;
        self.input_textarea.set_text("");
        self.goto_line_had_visual = false;
    }

    /// Close the input bar if it's open, but preserve any accepted matcher.
    ///
    /// Use when hiding a pane — the user's committed search/filter should
    /// persist across show/hide cycles. Only the mid-typing input bar
    /// state is discarded.
    pub fn close_input_bar(&mut self) {
        if self.input_mode.is_some() {
            self.input_mode = None;
            self.input_textarea.set_text("");
            // Don't clear the matcher — it was already accepted.
        }
    }

    /// Open the input bar in comment mode with optional pre-filled text.
    pub fn open_comment_input(&mut self, prefill: &str) {
        self.input_mode = Some(InputBarMode::Comment);
        self.input_textarea.set_text(prefill);
    }

    /// Returns the current input bar text (for reading comment content).
    pub fn input_text(&self) -> &str {
        self.input_textarea.text()
    }

    /// Rebuild the matcher from the current input textarea content.
    ///
    /// In Search mode, performs incremental search: jumps to the nearest
    /// match at or after the current selection (or scroll position).
    fn apply_input_buffer<T: ListItem>(&mut self, items: &[T]) {
        let Some(mode) = self.input_mode else {
            return;
        };
        let query = self.input_textarea.text().to_owned();
        if query.is_empty() {
            self.set_matcher(None);
            return;
        }
        let matcher = ListMatcher::new(&query, QueryKind::Regex, mode.match_mode());
        self.set_matcher(Some(matcher));

        // Incremental search: jump to nearest match (Search mode only).
        // In Filter mode the list restructures visually, no jump needed.
        if mode == InputBarMode::Search {
            // Need to rebuild match indices first (prepare_layout hasn't
            // run yet, so do it manually on the matcher).
            if let Some(ref mut m) = self.matcher {
                m.rebuild_matches(items);
            }
            // Only jump if the current selection is NOT already a match.
            // This prevents the viewport from shifting on every keystroke
            // when the user is already looking at matching content.
            let current_pi = self
                .selected_index
                .map(|vi| self.to_physical(vi))
                .unwrap_or(0);
            let already_matches = self
                .matcher
                .as_ref()
                .is_some_and(|m| m.match_indices.binary_search(&current_pi).is_ok());
            if !already_matches && let Some(ref mut m) = self.matcher {
                // Find nearest match at or after current position.
                let pos = m.match_indices.partition_point(|&mi| mi < current_pi);
                let target = if pos < m.match_indices.len() {
                    Some(m.match_indices[pos])
                } else {
                    // Wrap to first match.
                    m.match_indices.first().copied()
                };
                if let Some(pi) = target {
                    m.current_match = Some(m.match_indices.binary_search(&pi).unwrap_or(0));
                    self.jump_to_physical(pi, items);
                }
            }
        }
    }

    /// Toggle follow mode (explicit pause/resume).
    ///
    /// `F` key:
    /// - In FOLLOW: exit to NAV, cursor at last visible (pause).
    /// - In NAV: engage follow immediately (resume from anywhere).
    ///
    /// No-op when `follow_enabled` is false in config.
    pub fn toggle_follow<T: ListItem>(&mut self, items: &[T]) {
        if !self.config.follow_enabled {
            return;
        }
        if self.follow_mode {
            self.exit_follow(items);
        } else {
            self.engage_follow();
        }
    }

    /// Select the item at virtual-y position `y`, if it's selectable.
    ///
    /// Used for mouse click-to-select.  Returns `true` if an item was
    /// selected, `false` if the click hit a non-selectable item or empty space.
    ///
    /// On click: exits follow, cursor on clicked item.
    pub fn select_at_y<T: ListItem>(&mut self, y: usize, items: &[T]) -> bool {
        let Some(vi) = self.layout.item_at_y(y) else {
            return false;
        };
        if let Some(item) = self.item_at(vi, items)
            && item.is_selectable()
        {
            self.clear_visual_if_active();
            self.follow_mode = false;
            self.reset_edge_state();
            self.scroll_screen_y = None;
            self.selected_index = Some(vi);
            self.selected_id = Some(item.stable_id());
            true
        } else {
            false
        }
    }

    // =======================================================================
    // Mouse event handling
    // =======================================================================

    /// Handle a mouse event within this pane's area.
    ///
    /// `pane_area` is the screen `Rect` where the pane was rendered (used
    /// to compute relative row for item click-to-select).
    ///
    /// Returns `true` if the event was consumed.
    pub fn handle_mouse_event<T: ListItem>(
        &mut self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        pane_area: Rect,
        items: &[T],
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};

        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.scrollbar_hit(column, row) {
                    self.scrollbar_dragging = true;
                    return self.apply_scrollbar_click(row, items);
                }
                // A new press elsewhere ends any stale thumb latch (lost Up
                // from terminal coalescing / SSH / focus loss). Callers that
                // treat `is_scrollbar_dragging()` after this dispatch as
                // "this Down hit the track" rely on that.
                self.scrollbar_dragging = false;
                // Content click → select item.
                if pane_area.width > 0 && pane_area.height > 0 && row >= pane_area.y {
                    let ry = (row - pane_area.y) as usize;
                    let vy = self.scroll_offset() + ry;
                    self.select_at_y(vy, items);
                    return true;
                }
                false
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.scrollbar_dragging {
                    self.apply_scrollbar_click(row, items);
                    return true;
                }
                false
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.scrollbar_dragging = false;
                false
            }
            _ => false,
        }
    }

    /// Handle a scroll event within this pane's area.
    ///
    /// If the mouse is over the scrollbar, scrolls by percentage (fast).
    /// Otherwise scrolls by line count (normal).
    pub fn handle_scroll_event<T: ListItem>(
        &mut self,
        lines: i32,
        column: u16,
        row: u16,
        items: &[T],
    ) {
        // Check if mouse is on scrollbar → percentage scroll.
        if self.scrollbar_hit(column, row) {
            let total = self.total_height();
            let pct_delta = ((total as f64) * 0.0025).round() as i32;
            let effective = pct_delta.max(lines.abs()) * lines.signum();
            self.scroll_and_center(effective, items);
            return;
        }
        self.scroll_lines(lines, items);
    }

    /// Map a scrollbar click to a scroll offset.
    fn apply_scrollbar_click<T: ListItem>(&mut self, screen_y: u16, items: &[T]) -> bool {
        use crate::render::scrollbar::scrollbar_click_to_offset;

        let Some(sb) = self.scrollbar_area() else {
            return false;
        };
        if sb.height == 0 {
            return false;
        }

        let cell_index = screen_y.saturating_sub(sb.y);
        let total_height = self.total_height();
        let scale = if total_height > u16::MAX as usize {
            (total_height / u16::MAX as usize) + 1
        } else {
            1
        };
        let scaled_total = (total_height / scale) as u16;

        let result =
            scrollbar_click_to_offset(cell_index, sb.height, scaled_total, self.viewport_height());

        match result {
            crate::render::scrollbar::ScrollbarClickResult::Top => {
                self.select_first(items);
            }
            crate::render::scrollbar::ScrollbarClickResult::Bottom => {
                self.select_last(items);
            }
            crate::render::scrollbar::ScrollbarClickResult::Offset(scaled_offset) => {
                let offset = scaled_offset * scale;
                self.set_scroll_offset_and_center(offset, items);
            }
        }
        true
    }
}
