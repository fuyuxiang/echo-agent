//! Selection and folding for [`ScrollbackState`]: selected-entry tracking,
//! fold/expand operations, group expansion, and view-mode visibility.

use super::*;

/// Scroll/follow state captured before a fold-shaped layout change (entry
/// fold or group expansion), restored by
/// [`ScrollbackState::rebuild_with_fold_anchor`] so the change doesn't move
/// the viewport.
struct FoldAnchor {
    vy_before: Option<usize>,
    scroll_before: usize,
    follow_before: bool,
    preserve_before: bool,
}

impl ScrollbackState {
    // View Mode

    /// Get current view mode.
    pub fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    pub(crate) fn set_view_mode(&mut self, mode: ViewMode) {
        self.view_mode = mode;
    }

    /// Get the range of entry indices visible in the current view mode.
    pub fn visible_entry_range(&self) -> Range<usize> {
        match self.view_mode {
            ViewMode::AllTurns => 0..self.entries.len(),
            ViewMode::SingleTurn => {
                if let Some(turn_idx) = self.current_turn
                    && let Some(turn) = self.turns.get(turn_idx)
                {
                    return turn.prompt_index..turn.end_index;
                }

                // current_turn is None - check for pre-turn
                // Pre-turn = entries 0..first_prompt_index (if any exist before first prompt)
                if let Some(first_turn) = self.turns.first() {
                    if first_turn.prompt_index > 0 {
                        // Pre-turn exists: entries before first prompt
                        return 0..first_turn.prompt_index;
                    }
                    // No pre-turn, default to first turn
                    return first_turn.prompt_index..first_turn.end_index;
                }

                // No turns at all - show all entries
                0..self.entries.len()
            }
        }
    }

    /// Check if an entry index is visible in the current view mode.
    pub fn is_entry_visible(&self, index: usize) -> bool {
        self.visible_entry_range().contains(&index)
    }

    // Selection

    /// Get selected index.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Set selected index.
    pub fn set_selected(&mut self, index: Option<usize>) {
        self.selected = index.filter(|&i| i < self.entries.len());
        if let Some(sel) = self.selected {
            self.current_turn = self.turn_containing(sel);
        }
    }

    /// Get the selection box computed during the last render.
    ///
    /// This is set by ScrollbackPane and should be rendered by the frame
    /// after the scrollback pane has been rendered.
    pub fn selection_box(&self) -> Option<&SelectionBox> {
        self.selection_box.as_ref()
    }

    /// Set the selection box (called by ScrollbackPane during render).
    pub fn set_selection_box(&mut self, selection_box: Option<SelectionBox>) {
        self.selection_box = selection_box;
    }

    /// Take the selection box (consumes it).
    pub fn take_selection_box(&mut self) -> Option<SelectionBox> {
        self.selection_box.take()
    }

    /// Select next selectable entry (j key).
    /// Skips entries where block.is_selectable() returns false.
    /// If already at the last entry, overscroll → follow (like list_pane one-past).
    pub fn select_next(&mut self) {
        let range = self.visible_entry_range();
        if range.is_empty() {
            return;
        }

        // Find starting position
        let start = match self.selected {
            None => range.start,
            Some(i) if i < range.start => range.start,
            Some(i) => i + 1,
        };

        // Find next selectable entry (skip hidden entries with height=0)
        for idx in start..range.end {
            if self.is_entry_hidden(idx) {
                continue;
            }
            if let Some(entry) = self.entries.get_index(idx).map(|(_, v)| v)
                && entry.block.is_selectable()
            {
                self.selected = Some(idx);
                self.sync_current_turn();
                self.ensure_selected_visible(NavDirection::Down);
                return;
            }
        }

        // No selectable entry found after current — we're at the last entry.
        // Single j at the bottom engages follow immediately. Unlike list_pane's
        // one-past pattern, scrollback entries can be multi-screen, so requiring
        // two presses would be confusing.
        if self.appearance.scrollback.scroll.follow_by_overscroll {
            self.follow_mode = true;
            self.goto_bottom();
        }
    }

    /// Select previous selectable entry (k key).
    /// Skips entries where block.is_selectable() returns false.
    pub fn select_prev(&mut self) {
        let range = self.visible_entry_range();
        if range.is_empty() {
            return;
        }

        // Find starting position
        let start = match self.selected {
            None => range.end - 1,
            Some(i) if i >= range.end => range.end - 1,
            Some(i) if i <= range.start => {
                // Already at start, can't go further back
                return;
            }
            Some(i) => i - 1,
        };

        // Find previous selectable entry (iterate backwards, skip hidden entries)
        for idx in (range.start..=start).rev() {
            if self.is_entry_hidden(idx) {
                continue;
            }
            if let Some(entry) = self.entries.get_index(idx).map(|(_, v)| v)
                && entry.block.is_selectable()
            {
                self.selected = Some(idx);
                self.sync_current_turn();
                self.ensure_selected_visible(NavDirection::Up);
                return;
            }
        }
        // No selectable entry found before current - stay where we are
    }

    /// Clear selection.
    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// Called when pane is activated - auto-select last selectable entry.
    pub fn on_activate(&mut self) {
        let range = self.visible_entry_range();
        if range.is_empty() {
            self.selected = None;
            return;
        }

        // Find last selectable entry in range
        self.selected = self.find_last_selectable_in_range(range);
    }

    /// Keep current_turn in sync with selection.
    pub(super) fn sync_current_turn(&mut self) {
        if let Some(sel) = self.selected {
            self.current_turn = self.turn_containing(sel);
        }
    }

    /// Find the first selectable entry in a range.
    pub(super) fn find_first_selectable_in_range(&self, range: Range<usize>) -> Option<usize> {
        for idx in range {
            if let Some(entry) = self.entries.get_index(idx).map(|(_, v)| v)
                && entry.block.is_selectable()
            {
                return Some(idx);
            }
        }
        None
    }

    /// Find the last selectable entry in a range.
    pub(super) fn find_last_selectable_in_range(&self, range: Range<usize>) -> Option<usize> {
        for idx in range.rev() {
            if let Some(entry) = self.entries.get_index(idx).map(|(_, v)| v)
                && entry.block.is_selectable()
            {
                return Some(idx);
            }
        }
        None
    }

    /// Collapse selected entry (no-op if already at minimum fold mode or not foldable).
    ///
    /// Uses the block's `collapse_mode` to determine the target mode, which may be
    /// `Truncated` for running blocks (e.g., execute) instead of `Collapsed`.
    pub fn collapse_selected(&mut self) {
        if let Some(i) = self.selected
            && let Some((_, entry)) = self.entries.get_index(i)
            && entry.is_foldable()
        {
            let target_mode = entry.block.collapse_mode(entry.is_running);
            if entry.display_mode != target_mode {
                self.fold_selected_impl(|entry| {
                    let target = entry.block.collapse_mode(entry.is_running);
                    entry.set_display_mode(target);
                });
            }
        }
    }

    /// Expand selected entry (no-op if already expanded or not foldable).
    pub fn expand_selected(&mut self) {
        if let Some(i) = self.selected
            && let Some((_, entry)) = self.entries.get_index(i)
            && entry.is_foldable()
            && entry.display_mode != DisplayMode::Expanded
        {
            self.fold_selected_impl(|entry| entry.set_display_mode(DisplayMode::Expanded));
        }
    }

    /// Toggle fold on selected entry.
    pub fn toggle_fold_selected(&mut self) {
        if let Some(i) = self.selected
            && let Some((_, entry)) = self.entries.get_index(i)
            && entry.is_foldable()
        {
            self.fold_selected_impl(|entry| entry.toggle_fold());
        }
    }

    /// Shared implementation for fold operations with scroll anchoring.
    ///
    /// Captures virtual_y before the fold, applies the mutation, rebuilds
    /// layout, then either anchors scroll or falls back to ensure_visible.
    fn fold_selected_impl(&mut self, mutate: impl FnOnce(&mut ScrollbackEntry)) {
        let Some(i) = self.selected else { return };

        // 1. Capture state before the fold
        let anchor = self.capture_fold_anchor(i);
        let respect_manual_folds = self.appearance.scrollback.scroll.respect_manual_folds;

        // 2. Apply the fold mutation
        let mut grew = false;
        if let Some((id, entry)) = self.entries.get_index_mut(i) {
            let mode_before = entry.display_mode;
            mutate(entry);
            grew = display_rank(entry.display_mode) > display_rank(mode_before);
            if respect_manual_folds {
                entry.display_mode_pinned = true;
                tracing::debug!(
                    entry_id = id.value(),
                    mode = ?entry.display_mode,
                    "scrollback.fold.pinned"
                );
                if grew && anchor.follow_before {
                    tracing::debug!(entry_id = id.value(), "scrollback.follow.dropped_on_expand");
                }
            }
            self.dirty_heights.insert(*id);
        }

        // Re-key BEFORE the rebuild so the refold sees the migrated id.
        self.rekey_verb_group_expansion(i);
        self.rebuild_with_fold_anchor(i, grew, anchor);
        // Anything newly revealed further out is measured by the next prepare_layout.
        self.bump_generation();
    }

    /// A display-mode flip on entry `i` can move the verb run's anchor
    /// (opening the head entry drops it to transparent, so the run
    /// re-anchors on the next member; closing it takes the anchor back).
    /// Migrate a
    /// manual expansion keyed on the flipped entry or a former anchor onto
    /// the run's CURRENT first entry, so the group stays expanded across
    /// member open/close instead of snapping into a fresh collapsed fold.
    pub(super) fn rekey_verb_group_expansion(&mut self, i: usize) {
        if !crate::appearance::cache::load_group_tool_verbs() {
            return;
        }
        let show_thinking = crate::appearance::cache::load_show_thinking_blocks();
        // The run `i` belongs to after the flip: `i` itself when it
        // (re)joined, else the run past the transparent entries it opened
        // out of (a Break wall means there is no adjacent run to migrate).
        let mut j = i;
        let range = loop {
            if let Some(range) = self.verb_group_range_of(j) {
                break range;
            }
            let Some((_, entry)) = self.entries.get_index(j) else {
                return;
            };
            if matches!(
                super::verb_group::run_step(entry, show_thinking),
                super::verb_group::RunStep::Break
            ) {
                return;
            }
            j += 1;
        };
        let Some((&first_id, _)) = self.entries.get_index(range.start) else {
            return;
        };
        if self.expanded_groups.contains(&first_id) {
            return;
        }
        // A stale key sits on the flipped entry (it just opened out of the
        // head) or on an interior entry (the head just rejoined in front of
        // the interim anchor); move it onto the current anchor.
        let stale = std::iter::once(i)
            .chain(range.start + 1..range.end)
            .filter(|&k| k != range.start)
            .find_map(|k| {
                let (&id, _) = self.entries.get_index(k)?;
                self.expanded_groups.contains(&id).then_some(id)
            });
        if let Some(old) = stale {
            self.expanded_groups.remove(&old);
            self.expanded_groups.insert(first_id);
        }
    }

    /// Capture the scroll/follow state a fold-shaped change must not disturb.
    /// Pass to [`Self::rebuild_with_fold_anchor`] after mutating the entry or
    /// the group-expansion set.
    fn capture_fold_anchor(&self, i: usize) -> FoldAnchor {
        FoldAnchor {
            vy_before: self
                .layout_cache
                .as_ref()
                .and_then(|c| c.virtual_y.get(i).copied()),
            scroll_before: self.scroll_offset,
            follow_before: self.follow_mode,
            preserve_before: self.follow_preserve_scroll,
        }
    }

    /// Rebuild the layout after a fold-shaped change to entry `i` (display
    /// mode or group expansion) and restore the captured scroll/follow state
    /// so the change doesn't move the viewport. `grew` = the change made the
    /// entry/group taller (reading intent).
    fn rebuild_with_fold_anchor(&mut self, i: usize, grew: bool, anchor: FoldAnchor) {
        let drop_follow =
            self.appearance.scrollback.scroll.respect_manual_folds && grew && anchor.follow_before;

        // Rebuild the cache (estimates). Measure the folded entry's region
        // exactly when anchoring so the anchor delta below reads exact offsets.
        let anchor_on_fold = self.appearance.scrollback.scroll.anchor_on_fold;
        self.rebuild_layout();
        if anchor_on_fold && self.last_width > 0 {
            self.measure_around_entry(i, self.last_width);
        }
        // Clear dirty_heights — we just did a full rebuild so heights are fresh.
        // Without this, the leftover dirty entry triggers prepare_layout Case 2
        // on the next frame, which calls handle_follow_mode and could snap to bottom.
        self.dirty_heights.clear();

        // Anchor scroll or ensure visible
        if anchor_on_fold {
            if let Some(vy_before) = anchor.vy_before
                && let Some(ref cache) = self.layout_cache
                && let Some(&vy_after) = cache.virtual_y.get(i)
            {
                let delta = vy_after as i64 - vy_before as i64;
                let new_scroll = (anchor.scroll_before as i64 + delta).max(0) as usize;
                // Only clamp to max_offset if not in preserve mode.
                // During preserve, scroll_offset can be above max_offset
                // (prompt pinned at a position with content below fitting in viewport).
                if anchor.preserve_before {
                    self.scroll_offset = new_scroll;
                } else {
                    let max_offset = self
                        .total_height
                        .saturating_sub(self.viewport_height as usize);
                    self.scroll_offset = new_scroll.min(max_offset);
                }
            }

            // Folding is a display change, not navigation, so follow/preserve
            // state is restored as it was — EXCEPT when the fold GREW the
            // entry's display mode while following: that's reading intent, so
            // follow (and preserve) are dropped and the viewport stays where
            // the user put it. Follow resumes via the existing explicit gestures.
            self.follow_mode = anchor.follow_before && !drop_follow;
            self.follow_preserve_scroll = self.follow_mode && anchor.preserve_before;
        } else {
            self.ensure_selected_visible(NavDirection::Down);
            if drop_follow {
                self.follow_mode = false;
                self.follow_preserve_scroll = false;
            }
        }

        // Preserve-pinned page flip + fold growth: the next follow pass would
        // read the overflow as streaming fill and snap to the bottom
        // (`follow_scroll_to_bottom` consumes the pin once max_offset passes
        // it). A fold is reading intent, not new content — drop follow and
        // leave the viewport pinned where the user was looking.
        if grew && self.follow_mode && self.follow_preserve_scroll {
            let max_offset = self
                .total_height
                .saturating_sub(self.viewport_height as usize);
            if max_offset > self.scroll_offset {
                self.follow_mode = false;
                self.follow_preserve_scroll = false;
            }
        }
    }

    /// Toggle raw mode on selected entry.
    pub fn toggle_raw_selected(&mut self) {
        // Invisible on a group header (content hidden); skip the rebuild.
        if self.is_selected_group_header() {
            return;
        }
        if let Some(i) = self.selected
            && let Some((id, entry)) = self.entries.get_index_mut(i)
        {
            entry.toggle_raw();
            self.dirty_heights.insert(*id);
        }
        self.rebuild_layout();
        self.bump_generation();
    }

    /// Collapse all foldable entries.
    pub fn collapse_all(&mut self) {
        let mut changed_ids = Vec::new();
        for (id, entry) in &mut self.entries {
            entry.display_mode_pinned = false;
            if entry.is_foldable() {
                entry.display_mode = DisplayMode::Collapsed;
                entry.invalidate_cache();
                changed_ids.push(*id);
            }
        }
        for id in changed_ids {
            self.dirty_heights.insert(id);
        }
        // Clear manual expansions so newly-formed groups are truncated
        self.expanded_groups.clear();
        self.gaps_may_be_dirty = true;
        self.bump_generation();
    }

    /// Expand all foldable entries.
    pub fn expand_all(&mut self) {
        let mut changed_ids = Vec::new();
        for (id, entry) in &mut self.entries {
            entry.display_mode_pinned = false;
            if entry.is_foldable() {
                entry.display_mode = DisplayMode::Expanded;
                entry.invalidate_cache();
                changed_ids.push(*id);
            }
        }
        for id in changed_ids {
            self.dirty_heights.insert(id);
        }
        // Expanding breaks all groups, so manual expansion state is irrelevant
        self.expanded_groups.clear();
        self.gaps_may_be_dirty = true;
        self.bump_generation();
    }

    /// Smart toggle: if ANY foldable entry is collapsed, expand all.
    /// Otherwise collapse all.
    pub fn toggle_expand_all(&mut self) {
        let any_collapsed = self
            .entries
            .values()
            .any(|entry| entry.is_foldable() && entry.display_mode == DisplayMode::Collapsed);
        if any_collapsed {
            self.expand_all();
        } else {
            self.collapse_all();
        }
    }

    /// Toggle expand/collapse for all thinking blocks only.
    ///
    /// If ANY thinking block is collapsed, expand all thinking blocks.
    /// Otherwise collapse all thinking blocks.
    ///
    /// Also sets `thinking_display_mode` so that future thinking blocks
    /// adopt the chosen mode when they finish running.
    pub fn expand_all_thinking(&mut self) {
        let any_collapsed = self.entries.values().any(|entry| {
            matches!(entry.block, RenderBlock::Thinking(_))
                && entry.block.is_foldable()
                && entry.display_mode == DisplayMode::Collapsed
        });

        let target_mode = if any_collapsed {
            DisplayMode::Expanded
        } else {
            DisplayMode::Collapsed
        };

        self.thinking_display_mode = target_mode;

        let mut changed_ids = Vec::new();
        for (id, entry) in &mut self.entries {
            // Only expand/collapse thinking blocks — tool calls stay
            // collapsed as one-liners. Group truncation is handled
            // separately below (all hidden entries become visible).
            if matches!(entry.block, RenderBlock::Thinking(_)) && entry.block.is_foldable() {
                entry.display_mode = target_mode;
                entry.display_mode_pinned = false;
                entry.invalidate_cache();
                changed_ids.push(*id);
            }
        }
        for &id in &changed_ids {
            self.dirty_heights.insert(id);
        }
        // When expanding: also expand all truncated groups so everything
        // is visible. When collapsing: clear expansions so groups re-truncate.
        if target_mode == DisplayMode::Expanded {
            // Opened thoughts go transparent, so a keyed thought-anchored
            // run re-anchors on its first tool; migrate the keys.
            for id in &changed_ids {
                if let Some(idx) = self.entries.get_index_of(id) {
                    self.rekey_verb_group_expansion(idx);
                }
            }
            // Mark all group-start entries as expanded so truncation is skipped.
            self.expand_all_groups();
        } else {
            self.expanded_groups.clear();
        }
        self.gaps_may_be_dirty = true;
        self.bump_generation();
    }

    /// Expand all truncated groups (add every group-start ID to
    /// expanded_groups). Runs are walked with the shared
    /// `joins_dense_run` predicate, so the inserted ids agree with the
    /// truncation pass's claimed-entry breaks (leading hidden thinking can
    /// still skew the keyed id off the truncation header — same pre-existing
    /// divergence as `group_range_of`).
    fn expand_all_groups(&mut self) {
        let max_visible = self.appearance.scrollback.display.group_max_visible as usize;
        if max_visible == 0 {
            return;
        }
        let n = self.entries.len();
        let mut i = 0;
        while i < n {
            if !self.joins_dense_run(i, /*collapsed_only=*/ true) {
                i += 1;
                continue;
            }
            let group_start = i;
            let Some((&first_id, _)) = self.entries.get_index(i) else {
                i += 1;
                continue;
            };
            let mut j = i + 1;
            while j < n && self.joins_dense_run(j, /*collapsed_only=*/ true) {
                j += 1;
            }
            let group_len = j - group_start;
            if group_len > max_visible + 1 && self.expanded_groups.insert(first_id) {
                // Members still carry the fold-forced height 0; the Case 2
                // refold never raises stale heights (fold passes only force
                // heights down), so re-measure them for the reveal. The
                // header at `group_start` stays fold-owned.
                for k in (group_start + 1)..j {
                    if let Some((&member_id, _)) = self.entries.get_index(k) {
                        self.dirty_heights.insert(member_id);
                    }
                }
            }
            i = j;
        }
    }

    /// Returns "expand thinking" or "collapse thinking" based on current state.
    ///
    /// Uses the same logic as `expand_all_thinking`: if ANY thinking block is
    /// collapsed the next toggle will expand, so the label is "expand thinking".
    pub fn thinking_fold_label(&self) -> &'static str {
        let any_collapsed = self.entries.values().any(|entry| {
            matches!(entry.block, RenderBlock::Thinking(_))
                && entry.block.is_foldable()
                && entry.display_mode == DisplayMode::Collapsed
        });
        if any_collapsed {
            "expand thinking"
        } else {
            "collapse thinking"
        }
    }

    /// Whether the selected entry is any kind of group header.
    ///
    /// Returns true for both expand headers ("N more", content replaced)
    /// and collapse headers ("▾ N tool calls", standalone header entry).
    /// An EXPANDED verb-group header is deliberately excluded: its slot also
    /// hosts member 0's own row, so the selected entry acts as that member
    /// (fold/Enter/raw operate on the block); group re-collapse stays on
    /// Left / the header-row mouse path.
    pub fn is_selected_group_header(&self) -> bool {
        let Some(sel) = self.selected else {
            return false;
        };
        self.layout_cache
            .as_ref()
            .and_then(|c| c.entries.get(sel))
            .is_some_and(|e| e.is_group_header() && !e.is_expanded_verb_header())
    }

    /// "expand" / "collapse" when the selected entry is a group header, else
    /// `None`. Distinct from the entry-level fold label: a collapse header's
    /// entry stays `DisplayMode::Collapsed` (expansion lives in
    /// `expanded_groups`), which would mislabel it "expand".
    pub fn selected_group_header_fold_label(&self) -> Option<&'static str> {
        let sel = self.selected?;
        let info = self.layout_cache.as_ref()?.entries.get(sel)?;
        if info.is_expanded_verb_header() {
            // Expanded verb slot: the selection acts as member 0, so the
            // footer advertises the member's own fold, not the group's.
            None
        } else if info.group_collapse_header {
            Some("collapse")
        } else if info.group_header_count > 0 {
            Some("expand")
        } else {
            None
        }
    }

    /// Toggle expansion of the group whose header is the currently selected entry.
    ///
    /// If the selected entry is a group header (`is_group_header`), toggles
    /// its EntryId in `expanded_groups` (adds if absent, removes if present)
    /// and triggers a layout rebuild so truncation is recomputed.
    ///
    /// Returns `true` if a group was toggled (caller should skip normal expand).
    pub fn toggle_group_expansion(&mut self) -> bool {
        let Some(sel) = self.selected else {
            return false;
        };
        let Some(info) = self.layout_cache.as_ref().and_then(|c| c.entries.get(sel)) else {
            return false;
        };
        let is_verb_header = info.verb_group_header;
        if !info.is_group_header() {
            return false;
        }
        // Expanded verb slot: don't re-toggle — fall through so Expand /
        // ToggleFold / Enter act on member 0's own block. Collapse stays on
        // Left (`collapse_group_if_expanded`) and the header-row mouse path.
        if is_verb_header && info.group_collapse_header {
            return false;
        }
        let Some((&id, _)) = self.entries.get_index(sel) else {
            return false;
        };
        let anchor = self.capture_fold_anchor(sel);
        let expanding = !self.expanded_groups.contains(&id);
        if expanding {
            self.expanded_groups.insert(id);
        } else {
            self.expanded_groups.remove(&id);
        }
        // Rebuild so truncation is recomputed, keeping the header's screen
        // row put (same anchor discipline as entry-level folds).
        self.rebuild_with_fold_anchor(sel, expanding, anchor);
        // When expanding an N-more group: clear selection so the first entry
        // doesn't appear "active" with the collapse header; the user can
        // navigate into the group with j/k. A verb-group header stays
        // selected — it remains one synthetic header row while expanded, and
        // keeping it selected lets an immediate Collapse re-fold the group.
        if expanding && !is_verb_header {
            self.selected = None;
        }
        self.bump_generation();
        true
    }

    /// Drop every manual group expansion. Called on grouping-shape flips
    /// (`group_tool_verbs`, `show_thinking_blocks`): the set is shared by
    /// verb runs and N-more dense groups with no provenance, and a flip
    /// re-shapes every grouped run (verb and dense runs share start ids and
    /// their boundaries differ per flag value), so stale ids could reopen a
    /// verb slot expanded or mark a coincident dense run expanded. The flip
    /// is a global re-layout; expansion state resets with it.
    pub fn clear_group_expansion(&mut self) {
        self.expanded_groups.clear();
    }

    /// Collapse a group back if the selected entry is inside an expanded group.
    ///
    /// Finds the group range containing the selected entry, then checks if the
    /// group's first entry's ID is in `expanded_groups`. If so, removes it and
    /// triggers a layout rebuild to re-apply truncation.
    ///
    /// Returns `true` if a group was collapsed (caller should skip normal collapse).
    pub fn collapse_group_if_expanded(&mut self) -> bool {
        let Some(sel) = self.selected else {
            return false;
        };
        // Find the group range containing the selected entry
        let group = self.group_range_of(sel, true);
        let Some((&first_id, _)) = self.entries.get_index(group.start) else {
            return false;
        };
        let anchor = self.capture_fold_anchor(group.start);
        if !self.expanded_groups.remove(&first_id) {
            return false;
        }
        // Rebuild so truncation is re-applied, keeping the header's screen
        // row put (same anchor discipline as entry-level folds).
        self.rebuild_with_fold_anchor(group.start, false, anchor);
        self.fixup_hidden_selection();
        self.bump_generation();
        true
    }
}

fn display_rank(mode: DisplayMode) -> u8 {
    match mode {
        DisplayMode::Collapsed => 0,
        DisplayMode::Truncated => 1,
        DisplayMode::Expanded => 2,
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
