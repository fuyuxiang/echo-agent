//! Turn-scoped bottom padding that makes a page-flipped prompt's pinned pose a
//! real scroll bottom, preventing layout clamping from jumping to the tail.

use super::ScrollbackState;

impl ScrollbackState {
    #[cfg(test)]
    pub(crate) fn is_pin_reserve_active(&self) -> bool {
        self.pin_reserve_active
    }

    #[cfg(test)]
    pub(crate) fn is_pin_reserve_after_turn(&self) -> bool {
        self.pin_reserve_after_turn
    }

    pub(super) fn arm_pin_reserve(&mut self) {
        self.pin_reserve_active = true;
        self.pin_reserve_after_turn = false;
        // Without a target, release checks stay inert until positioning captures the pose.
        self.pin_reserve_target = None;
        self.pin_reserve_prompt_id = None;
    }

    /// The armed turn is over, so finish-time height changes and the "Worked for…" marker
    /// must not consume follow-preserve after this point.
    pub(crate) fn note_pin_reserve_turn_finished(&mut self) {
        if self.pin_reserve_active {
            self.pin_reserve_after_turn = true;
        }
    }

    fn pin_reserve_scroll_target(&self) -> Option<usize> {
        self.pin_reserve_target
            .or_else(|| self.pin_reserve_prompt_scroll_target())
    }

    /// Shift the captured pin pose by any height change ABOVE the pinned prompt, which moves
    /// the prompt's virtual_y and so must move the captured offset (relative to the visible
    /// range top) with it. Changes at or below the prompt leave its offset put, so this is a
    /// no-op for ordinary streaming, where the response grows below the prompt.
    pub(super) fn shift_pin_reserve_target_for_changes(&mut self, changes: &[(usize, i32)]) {
        if !self.pin_reserve_active {
            return;
        }
        let Some(target) = self.pin_reserve_target else {
            return;
        };
        let Some(prompt_idx) = self.pin_reserve_prompt_index() else {
            return;
        };
        let start = self.visible_entry_range().start;
        let above: i64 = changes
            .iter()
            .filter(|&&(idx, _)| idx >= start && idx < prompt_idx)
            .map(|&(_, d)| d as i64)
            .sum();
        if above != 0 {
            let shifted = (target as i64 + above).max(0) as usize;
            self.pin_reserve_target = Some(shifted);
            if self.follow_mode && self.follow_preserve_scroll {
                self.scroll_offset = (self.scroll_offset as i64 + above).max(0) as usize;
            }
        }
    }

    /// Clear reserve identity and lifecycle state without changing scroll totals.
    pub(super) fn clear_pin_reserve(&mut self) {
        self.pin_reserve_active = false;
        self.pin_reserve_target = None;
        self.pin_reserve_prompt_id = None;
        self.pin_reserve_after_turn = false;
    }

    /// Index of the prompt the pin targets. Resolves the stable id captured at
    /// arm time so a mid-turn interjection cannot move it; falls back to the
    /// last user prompt only when no id is stored (e.g. a resize re-derive).
    fn pin_reserve_prompt_index(&self) -> Option<usize> {
        self.pin_reserve_prompt_id
            .and_then(|id| self.entries.get_index_of(&id))
            .or_else(|| self.last_user_prompt_index())
    }

    /// Release when the captured pin pose is fully below the viewport. The captured pose
    /// makes this O(1); without one, remain armed because remeasurement can move a derived target.
    pub(super) fn release_pin_reserve_if_below_fold(&mut self) -> bool {
        if !self.pin_reserve_active {
            return false;
        }
        // Use `>=`: when the pose sits exactly on the first row past the viewport the prompt
        // is already fully off-screen, so release then rather than one row later.
        let below_fold = self.pin_reserve_target.is_some_and(|target| {
            target
                >= self
                    .scroll_offset
                    .saturating_add(self.viewport_height as usize)
        });
        if below_fold {
            self.clear_pin_reserve();
        }
        below_fold
    }

    /// Release the reserve before an explicit bottom gesture resolves the real tail.
    pub(super) fn release_pin_reserve(&mut self) {
        if !self.pin_reserve_active && self.pin_reserve_pad == 0 {
            return;
        }
        self.clear_pin_reserve();
        if self.layout_cache.is_some() {
            self.compute_total_height_from_cache();
        } else {
            self.total_height = self.total_height.saturating_sub(self.pin_reserve_pad);
            self.pin_reserve_pad = 0;
        }
    }

    /// Release a reserve below the viewport, recomputing totals only when state changes.
    pub(super) fn maybe_release_pin_reserve(&mut self) {
        if self.release_pin_reserve_if_below_fold() && self.layout_cache.is_some() {
            self.compute_total_height_from_cache();
        }
    }

    pub(super) fn pin_reserve_pad_rows(&self, content_height: usize) -> usize {
        if !self.pin_reserve_active || self.viewport_height == 0 {
            return 0;
        }
        let Some(target) = self.pin_reserve_scroll_target() else {
            return 0;
        };
        // max_offset = content + pad - viewport must be at least `target`
        // so the last user prompt can sit at the top.
        target
            .saturating_add(self.viewport_height as usize)
            .saturating_sub(content_height)
    }

    /// Sticky-adjusted scroll target of the prompt the pin is armed for, resolved by the
    /// captured id so a mid-turn interjection cannot retarget the pad or the resize re-derive
    /// to a later prompt. Falls back to the last user prompt only when no id is stored.
    pub(super) fn pin_reserve_prompt_scroll_target(&self) -> Option<usize> {
        let idx = self.pin_reserve_prompt_index()?;
        let cache = self.layout_cache.as_ref()?;
        let range = self.visible_entry_range();
        if !range.contains(&idx) {
            return None;
        }
        let base = *cache.virtual_y.get(range.start)?;
        let y = *cache.virtual_y.get(idx)?;
        let entry_y = y.saturating_sub(base);
        // Same sticky-header fixed point as `scroll_to_entry_top`. Using raw `entry_y` would
        // over-state the pad whenever an earlier prompt is still sticky, making max_offset >
        // scroll_offset and consuming follow-preserve on the next frame.
        Some(self.sticky_adjusted_entry_top(cache, &range, entry_y))
    }

    fn last_user_prompt_index(&self) -> Option<usize> {
        if let Some(idx) = self.turns.last().map(|turn| turn.prompt_index)
            && self
                .entries
                .get_index(idx)
                .is_some_and(|(_, entry)| entry.block.is_user_prompt())
        {
            return Some(idx);
        }
        self.entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, (_, entry))| entry.block.is_user_prompt().then_some(idx))
    }
}

#[cfg(test)]
mod tests {
    use super::super::ScrollbackState;
    use super::super::test_util::*;
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::types::DisplayMode;

    fn tall_history_then_prompt() -> (ScrollbackState, usize) {
        let mut state = ScrollbackState::new();
        for i in 0..30 {
            state.push_block(agent_block(&format!("filler line {i}")));
        }
        state.push_block(user_block("next question"));
        let prompt_idx = state.len() - 1;
        state.prepare_layout(80, 8);
        (state, prompt_idx)
    }

    #[test]
    fn page_flip_small_scroll_does_not_collapse_pad() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        assert!(state.is_pin_reserve_active());
        let pin = state.scroll_offset();
        let (_, vh, total) = state.scroll_info();
        assert_eq!(
            pin,
            total.saturating_sub(vh as usize),
            "pin pose must be a real bottom once the pad is in total_height"
        );

        state.scroll_up(2);
        state.prepare_layout(80, 8);
        assert!(state.is_pin_reserve_active());
        assert_eq!(
            state.scroll_offset(),
            pin.saturating_sub(2),
            "a small scroll must not clamp to the unpadded tail"
        );
    }

    #[test]
    fn page_flip_scroll_down_at_pin_does_not_jump() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        let pin = state.scroll_offset();

        state.scroll_down(3);
        state.prepare_layout(80, 8);
        assert_eq!(state.scroll_offset(), pin);
        assert!(state.is_pin_reserve_active());
    }

    #[test]
    fn page_flip_drops_pad_once_last_user_is_below_the_fold() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        assert!(state.is_pin_reserve_active());

        state.scroll_up(12);
        state.prepare_layout(80, 8);
        assert!(
            !state.is_pin_reserve_active(),
            "scrolling the last user prompt fully off the bottom must drop the pad"
        );
    }

    #[test]
    fn page_flip_drops_pad_when_offset_moves_without_scroll_up() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        assert!(state.is_pin_reserve_active());

        state.scroll_offset = 0;
        state.follow_mode = false;
        state.prepare_layout(80, 8);
        assert!(
            !state.is_pin_reserve_active(),
            "layout must drop the pad once the last user prompt is below the fold"
        );
    }

    #[test]
    fn page_flip_scroll_after_turn_complete_does_not_jump() {
        crate::appearance::cache::set_show_thinking_blocks(true);
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);

        let think_id = state.push_block(RenderBlock::thinking("line1"));
        if let Some(entry) = state.entries.get_mut(&think_id) {
            entry.is_running = true;
            entry.set_display_mode(DisplayMode::Truncated);
        }
        state.running.insert(think_id);
        state.prepare_layout(80, 8);
        for i in 0..8 {
            state.push_chunk_to_thinking(think_id, &format!("\nline{}", i + 2));
            state.prepare_layout(80, 8);
        }

        state.scroll_up(2);
        state.prepare_layout(80, 8);
        assert!(state.is_pin_reserve_active());
        let mid = state.scroll_offset();

        state.finish_running(think_id);
        state.note_pin_reserve_turn_finished();
        state.push_block(RenderBlock::session_event(
            crate::scrollback::blocks::SessionEvent::TurnCompleted {
                elapsed: Some(std::time::Duration::from_secs(1)),
            },
        ));
        state.prepare_layout(80, 8);
        assert!(
            state.is_pin_reserve_active(),
            "completing the turn must not drop the pad"
        );
        assert_eq!(
            state.scroll_offset(),
            mid,
            "finish must not snap a midstream scroll to the tail"
        );

        state.scroll_up(2);
        state.prepare_layout(80, 8);
        assert!(state.is_pin_reserve_active());
        assert_eq!(
            state.scroll_offset(),
            mid.saturating_sub(2),
            "scroll after complete must not clamp to the unpadded tail"
        );
    }

    #[test]
    fn page_flip_stays_pinned_when_short_answer_finishes_idle() {
        crate::appearance::cache::set_show_thinking_blocks(true);
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        let pin = state.scroll_offset();

        let think_id = state.push_block(RenderBlock::thinking("line1"));
        if let Some(entry) = state.entries.get_mut(&think_id) {
            entry.is_running = true;
            entry.set_display_mode(DisplayMode::Truncated);
        }
        state.running.insert(think_id);
        state.prepare_layout(80, 8);
        state.push_chunk_to_thinking(think_id, "\nline2");
        state.prepare_layout(80, 8);
        assert!(state.is_follow_preserve_scroll());
        assert!(state.is_pin_reserve_active());

        state.finish_running(think_id);
        state.note_pin_reserve_turn_finished();
        state.push_block(RenderBlock::session_event(
            crate::scrollback::blocks::SessionEvent::TurnCompleted {
                elapsed: Some(std::time::Duration::from_secs(2)),
            },
        ));
        state.prepare_layout(80, 8);
        assert!(state.is_pin_reserve_active());
        assert_eq!(
            state.scroll_offset(),
            pin,
            "an idle finish must keep the last user prompt at the top"
        );
    }

    #[test]
    fn page_flip_arms_while_scrolled_up_despite_stale_target() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        // Simulate arming while reading history with a stale prior target.
        state.scroll_offset = 0;
        state.follow_mode = false;
        state.pin_reserve_target = Some(999);

        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        assert!(
            state.is_pin_reserve_active(),
            "arming while scrolled up must not disarm the reserve"
        );
        let pin = state.scroll_offset();
        let (_, vh, total) = state.scroll_info();
        assert_eq!(
            pin,
            total.saturating_sub(vh as usize),
            "pin pose must be a real bottom, not an offset past the unpadded tail"
        );

        state.scroll_up(2);
        state.prepare_layout(80, 8);
        assert!(state.is_pin_reserve_active());
        assert_eq!(
            state.scroll_offset(),
            pin.saturating_sub(2),
            "a scroll after arming-while-scrolled-up must not clamp to the tail"
        );
    }

    #[test]
    fn shift_pin_reserve_target_tracks_only_above_prompt_changes() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        let base = state.pin_reserve_target.expect("armed pose");
        let start = state.visible_entry_range().start;

        state.shift_pin_reserve_target_for_changes(&[(prompt_idx, 5), (prompt_idx + 1, 9)]);
        assert_eq!(
            state.pin_reserve_target,
            Some(base),
            "below-prompt change is a no-op"
        );

        state.shift_pin_reserve_target_for_changes(&[(start, 3)]);
        assert_eq!(
            state.pin_reserve_target,
            Some(base + 3),
            "above-prompt growth shifts down"
        );
        assert_eq!(
            state.scroll_offset,
            base + 3,
            "the viewport follows the pinned prompt's new row"
        );

        state.shift_pin_reserve_target_for_changes(&[(start, -2)]);
        assert_eq!(
            state.pin_reserve_target,
            Some(base + 1),
            "above-prompt shrink shifts up"
        );
        assert_eq!(
            state.scroll_offset,
            base + 1,
            "the viewport remains aligned with the shifted prompt"
        );
    }

    #[test]
    fn page_flip_tracks_height_changes_above_prompt() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        let before = state.scroll_offset;
        let entry_id = *state.entries.get_index(0).expect("history entry").0;
        let before_height = state.layout_cache.as_ref().expect("layout cache").entries[0].height;
        {
            let entry = state.entry_mut(0).expect("history entry");
            entry.block = tall_agent_block();
            entry.invalidate_cache();
        }
        state.dirty_heights.insert(entry_id);
        state.prepare_layout(80, 8);

        let after_height = state.layout_cache.as_ref().expect("layout cache").entries[0].height;
        assert!(
            after_height > before_height,
            "fixture must grow above the prompt"
        );
        assert!(state.scroll_offset > before);
        assert_eq!(
            state.scroll_offset,
            state
                .pin_reserve_prompt_scroll_target()
                .expect("prompt target"),
            "the viewport must remain aligned with the prompt after content above it grows"
        );
    }

    #[test]
    fn streaming_fast_path_releases_pin_below_fold() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        state.scroll_up(2);
        let entry_id = *state.entries.get_index(0).expect("history entry").0;
        {
            let entry = state.entry_mut(0).expect("history entry");
            entry.block = tall_agent_block();
            entry.invalidate_cache();
        }
        state.dirty_heights.insert(entry_id);

        state.prepare_layout(80, 8);

        assert!(!state.is_pin_reserve_active());
        assert_eq!(state.pin_reserve_pad, 0);
    }

    #[test]
    fn pin_reserve_prompt_index_survives_interjection() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);

        // An interjection becomes the last user prompt but must not retarget the reserve.
        state.push_block(user_block("interjection"));
        let interjection_idx = state.len() - 1;
        state.turns.clear();

        assert_eq!(
            state.last_user_prompt_index(),
            Some(interjection_idx),
            "pre-fix boundary would follow the interjection"
        );
        assert_eq!(
            state.pin_reserve_prompt_index(),
            Some(prompt_idx),
            "the pin tracks its armed prompt via the captured id, not the last user prompt"
        );
    }

    #[test]
    fn page_flip_survives_narrower_resize() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        assert!(state.is_pin_reserve_active());

        // A narrower resize re-wraps every entry taller, moving the pin pose
        // down in the new coordinate space. The reserve must survive it.
        state.prepare_layout(40, 8);
        assert!(
            state.is_pin_reserve_active(),
            "a resize must not drop the page-flip pin"
        );
        let (_, vh, total) = state.scroll_info();
        assert_eq!(
            state.scroll_offset(),
            total.saturating_sub(vh as usize),
            "the prompt stays pinned at the top (padded bottom) after a resize"
        );
    }

    #[test]
    fn goto_bottom_releases_pin_reserve() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        assert!(state.is_pin_reserve_active());
        let pinned = state.scroll_offset();

        state.goto_bottom();
        assert!(
            !state.is_pin_reserve_active(),
            "an explicit bottom gesture must drop the page-flip pin"
        );
        let (_, vh, total) = state.scroll_info();
        assert_eq!(
            state.scroll_offset(),
            total.saturating_sub(vh as usize),
            "End must land on the real (unpadded) tail"
        );
        assert!(
            state.scroll_offset() < pinned,
            "the released tail sits below the padded pin pose"
        );
    }

    #[test]
    fn clear_drops_pin_reserve() {
        let (mut state, prompt_idx) = tall_history_then_prompt();
        state.follow_new_turn(Some(prompt_idx), true);
        state.prepare_layout(80, 8);
        assert!(state.is_pin_reserve_active());

        state.clear();
        assert!(!state.is_pin_reserve_active());
        state.push_block(user_block("reopened"));
        state.push_block(agent_block("answer"));
        state.prepare_layout(80, 8);
        assert!(!state.is_pin_reserve_active());
        let (_, vh, total) = state.scroll_info();
        assert!(
            total <= vh as usize,
            "reopening must not keep leftover bottom pad (total={total}, vh={vh})"
        );
    }
}
