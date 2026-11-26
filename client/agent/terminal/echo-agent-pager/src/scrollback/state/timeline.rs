//! Conversation timeline: one entry per turn, for jump navigation UIs
//! (`/jump` picker; the timeline sidebar builds on the same data).

use super::*;

/// Max preview length stored per timeline entry. Render paths truncate
/// further to the available width; this only bounds the snapshot.
const PREVIEW_MAX_CHARS: usize = 120;

/// One turn in the conversation timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEntry {
    /// Turn's display ordinal (snapshot-only; not used to act on the transcript).
    pub turn_idx: usize,
    /// Stable id of the turn's `UserPrompt` entry — the jump/preview target,
    /// resolved to an index only at the [`ScrollbackState`] boundary so a
    /// removal (`shift_remove`) can't make a stale index target another block.
    pub prompt_entry_id: EntryId,
    /// First non-empty line of the prompt text, char-capped.
    pub preview: String,
}

/// First non-empty line, char-capped with a `…` marker. Bounded single pass:
/// the length probe stops one char past the cap, so a huge one-line prompt
/// costs O(cap), not O(line length). Char cap (not display width) on purpose —
/// this bounds the stored snapshot; render paths re-truncate to their width.
fn prompt_preview(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out: String = line.chars().take(PREVIEW_MAX_CHARS).collect();
    if out.chars().count() == PREVIEW_MAX_CHARS && line.chars().nth(PREVIEW_MAX_CHARS).is_some() {
        out.pop();
        out.push('\u{2026}');
    }
    out
}

impl ScrollbackState {
    /// Timeline entries, one per turn in conversation order (oldest first).
    ///
    /// Each entry carries the prompt's stable [`EntryId`]; dispatch resolves it
    /// to an index at the boundary, so the snapshot stays correct across both
    /// appends and removals.
    pub fn timeline_entries(&self) -> Vec<TimelineEntry> {
        self.turns
            .iter()
            .enumerate()
            .filter_map(|(turn_idx, turn)| {
                let (id, entry) = self.entries.get_index(turn.prompt_index)?;
                let preview = match &entry.block {
                    RenderBlock::UserPrompt(block) => prompt_preview(&block.text),
                    _ => String::new(),
                };
                Some(TimelineEntry {
                    turn_idx,
                    prompt_entry_id: *id,
                    preview,
                })
            })
            .collect()
    }

    /// Preview for one turn (avoids building the whole entry list when a
    /// single hover needs it, e.g. the sidebar tick popup).
    pub fn turn_preview(&self, turn_idx: usize) -> Option<String> {
        let turn = self.turns.get(turn_idx)?;
        self.entries
            .get_index(turn.prompt_index)
            .and_then(|(_, entry)| match &entry.block {
                RenderBlock::UserPrompt(block) => Some(prompt_preview(&block.text)),
                _ => None,
            })
    }

    /// The focused turn: the last turn whose prompt is at/above the
    /// viewport top, or the first turn while pre-turn content owns the top.
    /// `None` only when there are no turns or no layout. Trailing turns
    /// short enough to never own the top row never become active — they're
    /// fully on screen when it matters.
    pub fn active_turn_for_viewport(&self) -> Option<usize> {
        if self.view_mode == ViewMode::SingleTurn {
            return self.current_turn;
        }
        if self.turns.is_empty() {
            return None;
        }
        Some(self.prompts_above_top(false)?.saturating_sub(1))
    }

    /// The nearest turn an upward scroll can land on: the last turn whose
    /// prompt is STRICTLY above the viewport top, `None` when nothing is
    /// above. The ▲ chevron steps here rather than `active - 1`: from
    /// mid-turn it first aligns the current turn's own prompt (like the
    /// h key), and it can never target a trailing turn that no scroll
    /// reaches (the stuck-▲ bug).
    pub fn turn_above_viewport_top(&self) -> Option<usize> {
        if self.view_mode == ViewMode::SingleTurn {
            return self.current_turn?.checked_sub(1);
        }
        self.prompts_above_top(true)?.checked_sub(1)
    }

    /// The nearest turn below the viewport top. Before the first prompt,
    /// this is the first turn; on a prompt row, it is the following turn.
    pub fn turn_below_viewport_top(&self) -> Option<usize> {
        if self.view_mode == ViewMode::SingleTurn {
            let next = self.current_turn?.checked_add(1)?;
            return (next < self.turns.len()).then_some(next);
        }
        let next = self.prompts_above_top(false)?;
        (next < self.turns.len()).then_some(next)
    }

    /// Count of turns whose prompt row is above the viewport top (`strict`:
    /// strictly above; else at-or-above). Prompt rows are monotone in turn
    /// order, so this is a partition point over cached `virtual_y`.
    fn prompts_above_top(&self, strict: bool) -> Option<usize> {
        let cache = self.layout_cache.as_ref()?;
        let range = self.visible_entry_range();
        let base = *cache.virtual_y.get(range.start)?;
        let top = base + self.scroll_offset;
        Some(self.turns.partition_point(|turn| {
            cache
                .virtual_y
                .get(turn.prompt_index)
                .is_some_and(|&prompt_y| {
                    if strict {
                        prompt_y < top
                    } else {
                        prompt_y <= top
                    }
                })
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn timeline_entries_one_per_turn_in_order() {
        let mut state = ScrollbackState::new();
        state.push_block(stub_block("session banner")); // 0: pre-turn
        state.push_block(user_block("first question")); // 1
        state.push_block(agent_block("first answer")); // 2
        state.push_block(user_block("second question")); // 3
        state.push_block(tool_block("ls")); // 4
        state.prepare_layout(80, 10);

        let entries = state.timeline_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].turn_idx, 0);
        assert_eq!(state.index_of_id(entries[0].prompt_entry_id), Some(1));
        assert_eq!(entries[0].preview, "first question");
        assert_eq!(entries[1].turn_idx, 1);
        assert_eq!(state.index_of_id(entries[1].prompt_entry_id), Some(3));
        assert_eq!(entries[1].preview, "second question");
    }

    #[test]
    fn preview_takes_first_nonempty_line_and_caps_length() {
        let mut state = ScrollbackState::new();
        state.push_block(user_block("\n\n  leading blanks skipped  \nsecond line"));
        let long = "x".repeat(500);
        state.push_block(user_block(&long));
        state.prepare_layout(80, 10);

        let entries = state.timeline_entries();
        assert_eq!(entries[0].preview, "leading blanks skipped");
        assert_eq!(entries[1].preview.chars().count(), 120);
        assert!(entries[1].preview.ends_with('\u{2026}'));
    }

    #[test]
    fn active_turn_tracks_viewport_top() {
        let mut state = ScrollbackState::new();
        state.push_block(user_block("Q1")); // 0
        state.push_block(tall_agent_block()); // 1
        state.push_block(user_block("Q2")); // 2
        state.push_block(tall_agent_block()); // 3
        state.push_block(user_block("Q3")); // 4
        state.push_block(tall_agent_block()); // 5
        state.prepare_layout(80, 6);

        state.goto_top();
        assert_eq!(state.active_turn_for_viewport(), Some(0));

        state.scroll_to_entry_top(2);
        assert_eq!(state.active_turn_for_viewport(), Some(1));

        state.goto_bottom();
        assert_eq!(state.active_turn_for_viewport(), Some(2));
    }

    #[test]
    fn active_turn_stays_top_anchored_at_the_bottom() {
        // A screenful of short trailing turns: even at the bottom the
        // active turn is the one owning the top row (the web-timeline
        // rule) — never a newest-turn clamp, whose one-step-off-bottom
        // highlight leap and stuck-▲ chevron this replaced.
        let mut state = ScrollbackState::new();
        state.push_block(user_block("Q1"));
        state.push_block(tall_agent_block());
        for i in 2..8 {
            state.push_block(user_block(&format!("Q{i}")));
            state.push_block(agent_block("ok"));
        }
        state.prepare_layout(80, 12);

        state.goto_bottom();
        let at_bottom = state.active_turn_for_viewport().expect("active at bottom");
        assert!(at_bottom < 6, "top-anchored, not the newest: {at_bottom}");

        // Nudging off the bottom moves the highlight at most one boundary
        // (the old clamp leapt from the newest turn to the top-anchored one).
        state.scroll_up(1);
        let nudged = state.active_turn_for_viewport().expect("still in a turn");
        assert!(
            at_bottom - nudged <= 1,
            "no highlight leap: {at_bottom} -> {nudged}"
        );
    }

    /// One render-frame + chevron click, wired exactly like the app:
    /// render.rs builds the rail from viewport state, mouse.rs resolves the
    /// hit through `chevron_target` and jumps. `None` = the chevron was dim.
    fn click_chevron(state: &mut ScrollbackState, viewport_height: u16, up: bool) -> Option<usize> {
        use crate::views::timeline::{RailViewport, TimelineHit, chevron_target, compute_rail};
        state.prepare_layout(80, viewport_height);
        let area = ratatui::layout::Rect::new(0, 0, 80, viewport_height);
        let vp = RailViewport {
            active: state.active_turn_for_viewport(),
            up_target: state.turn_above_viewport_top(),
            down_target: state.turn_below_viewport_top(),
            at_bottom: !state.has_content_below(),
        };
        let rail = compute_rail(area, 78, state.turn_count(), vp).expect("rail eligible");
        let hit = if up {
            TimelineHit::Up
        } else {
            TimelineHit::Down
        };
        let target = chevron_target(&rail, hit)?;
        state.jump_to_turn(target);
        Some(target)
    }

    #[test]
    fn chevrons_walk_the_conversation_end_to_end_without_sticking() {
        // The stuck-▲ shape: one tall response, then six short turns that
        // all cluster inside the final screenful.
        let mut state = ScrollbackState::new();
        state.push_block(user_block("Q1"));
        state.push_block(tall_agent_block());
        for i in 2..8 {
            state.push_block(user_block(&format!("Q{i}")));
            state.push_block(agent_block("ok"));
        }
        state.prepare_layout(80, 12);
        state.goto_bottom();

        // ▲ to the very top: every click moves the viewport up, one
        // boundary per click once on a prompt row, no sticking.
        let mut up_visits = Vec::new();
        while up_visits.len() < 16 {
            let before = state.scroll_offset();
            let Some(target) = click_chevron(&mut state, 12, true) else {
                break;
            };
            assert!(
                state.scroll_offset() < before,
                "▲ #{} must move the viewport up",
                up_visits.len()
            );
            up_visits.push(target);
        }
        assert_eq!(state.scroll_offset(), 0, "▲ walk reaches the top");
        assert_eq!(up_visits.last(), Some(&0), "▲ walk ends at the first turn");
        assert!(
            up_visits.windows(2).all(|w| w[0] - w[1] == 1),
            "one boundary per click: {up_visits:?}"
        );
        assert_eq!(click_chevron(&mut state, 12, true), None, "▲ dim at top");

        // ▼ back down: strictly forward, never sticking, and it terminates
        // (dims) rather than repeating a turn or running forever.
        let mut down_visits = Vec::new();
        while down_visits.len() < 16 {
            let Some(target) = click_chevron(&mut state, 12, false) else {
                break;
            };
            if let Some(&prev) = down_visits.last() {
                assert!(
                    target > prev,
                    "▼ moves strictly forward: {down_visits:?} then {target}"
                );
            }
            down_visits.push(target);
        }
        assert!(!down_visits.is_empty(), "▼ steps down from the top");
        assert!(
            down_visits.len() < 16,
            "▼ walk terminates (dims), no sticking: {down_visits:?}"
        );
    }

    #[test]
    fn down_chevron_enters_trailing_turns_at_the_bottom() {
        // Reported bug: a cluster of short turns fills the final screenful,
        // leaving ▼ dim at the bottom even though clicking those ticks jumped
        // to them. ▼ now targets the next turn — the same turn a tick click
        // resolves to (both go through jump_to_turn).
        let mut state = ScrollbackState::new();
        state.push_block(user_block("Q1"));
        state.push_block(tall_agent_block());
        for i in 2..8 {
            state.push_block(user_block(&format!("Q{i}")));
            state.push_block(agent_block("ok"));
        }
        state.prepare_layout(80, 12);
        state.goto_bottom();

        let active = state.active_turn_for_viewport().expect("active at bottom");
        assert!(active < 7, "trailing turns sit below the top-anchored turn");
        assert_eq!(
            state.turn_below_viewport_top(),
            Some(active + 1),
            "▼ has a target below the top-anchored turn"
        );
        assert_eq!(
            click_chevron(&mut state, 12, false),
            Some(active + 1),
            "▼ steps to the next turn instead of dimming"
        );
    }

    #[test]
    fn up_chevron_snaps_to_the_current_prompt_mid_turn() {
        // Midway through a response ▲ first aligns the current turn's own
        // prompt to the top (matching the h key), then steps to older turns.
        let mut state = ScrollbackState::new();
        state.push_block(user_block("Q1"));
        state.push_block(tall_agent_block());
        state.push_block(user_block("Q2"));
        state.push_block(tall_agent_block());
        state.prepare_layout(80, 6);
        state.goto_top();
        state.scroll_down(3);

        assert_eq!(state.active_turn_for_viewport(), Some(0));
        assert_eq!(
            click_chevron(&mut state, 6, true),
            Some(0),
            "snap to own prompt"
        );
        assert_eq!(state.scroll_offset(), 0);
        assert_eq!(
            click_chevron(&mut state, 6, true),
            None,
            "then dim at the top"
        );
    }

    #[test]
    fn chevrons_when_everything_fits_on_one_screen() {
        // Fits with room to spare: the first turn owns the top. ▲ dims (nothing
        // above), but ▼ still enters the next turn — anchoring it to the top
        // like clicking its tick — rather than dimming.
        let mut state = ScrollbackState::new();
        state.push_block(user_block("Q1"));
        state.push_block(agent_block("a1"));
        state.push_block(user_block("Q2"));
        state.push_block(agent_block("a2"));
        state.prepare_layout(80, 40);

        assert_eq!(state.active_turn_for_viewport(), Some(0));
        assert_eq!(
            click_chevron(&mut state, 40, true),
            None,
            "▲ dim at first turn"
        );
        assert_eq!(
            click_chevron(&mut state, 40, false),
            Some(1),
            "▼ enters the second turn"
        );
    }

    #[test]
    fn pre_turn_content_dims_up_and_down_enters_the_first_turn() {
        let mut state = ScrollbackState::new();
        for i in 0..10 {
            state.push_block(stub_block(&format!("banner {i}")));
        }
        state.push_block(user_block("Q1"));
        state.push_block(tall_agent_block());
        state.push_block(user_block("Q2"));
        state.push_block(agent_block("ok"));
        state.prepare_layout(80, 12);
        state.goto_top();

        // Pre-turn content focuses the first tick; ▲ is dim while ▼ enters
        // that first turn rather than skipping it.
        assert_eq!(state.active_turn_for_viewport(), Some(0));
        assert_eq!(click_chevron(&mut state, 12, true), None);
        assert_eq!(click_chevron(&mut state, 12, false), Some(0));
        assert_eq!(state.active_turn_for_viewport(), Some(0));
        assert_eq!(
            click_chevron(&mut state, 12, true),
            None,
            "▲ dim on the first turn (nothing above)"
        );
    }

    #[test]
    fn active_turn_is_first_before_first_prompt() {
        let mut state = ScrollbackState::new();
        for i in 0..10 {
            state.push_block(stub_block(&format!("banner {i}")));
        }
        state.push_block(user_block("Q1"));
        state.push_block(tall_agent_block());
        state.prepare_layout(80, 4);

        state.goto_top();
        assert_eq!(state.active_turn_for_viewport(), Some(0));

        state.goto_bottom();
        assert_eq!(state.active_turn_for_viewport(), Some(0));
    }
}
