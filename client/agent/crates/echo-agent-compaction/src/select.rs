//! Turn selection for compaction.
//!
//! Walks the item list backward to find a split point: keep the newest items
//! whose cumulative token count fits the target budget, compact everything
//! older.
//!
//! The split point must respect a critical invariant: an assistant item with
//! tool requests and the subsequent tool-result items that satisfy those
//! requests must stay together. Splitting between them would produce orphan
//! tool results in the next prompt, which the model API rejects with a 400.
//!
//! This is the harness-agnostic core: it operates over any slice of
//! [`CompactionItem`], so both EchoAgent chat (`EchoAgentTurn`) and echo-agent-build
//! (`ConversationItem`) share one implementation.

use crate::item::CompactionItem;

/// Output of [`select_turns_to_compact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPlan {
    /// Compact items at indices `0..split_idx`. Keep `split_idx..total`.
    pub split_idx: usize,
    /// Sum of `item_token_counts[..split_idx]`.
    pub tokens_to_compact: u32,
}

/// Decide where to split the items for compaction.
///
/// Algorithm:
/// 1. Walk backward from the newest item, accumulating "keep" tokens.
/// 2. The candidate split index is the first one where adding more would
///    exceed `target_tokens`.
/// 3. **Snap forward** to a safe boundary: if the split would orphan tool
///    results, walk forward until past the matching tool-result items.
/// 4. Return `None` if the resulting compactable region's token count is
///    below `min_compactable` — not worth running the LLM.
///
/// # Tool-pair boundary safety
///
/// `items` is the agent's running state. A typical sequence:
///
/// ```text
/// [Assistant(tool_request_A, tool_request_B),
///  Tool(A_result),
///  Tool(B_result),
///  Assistant(response_text),
///  Assistant(tool_request_C),
///  Tool(C_result),
///  ...]
/// ```
///
/// A safe split point is one where everything **before** the split is
/// self-contained (no dangling tool requests waiting for results that live
/// after the split).
///
/// The rule we enforce: the split index must not fall in the middle of a
/// `[Assistant-with-tool-requests, Tool, Tool, ...]` run. If the candidate
/// split lands on a tool-result item, walk it forward until we pass the last
/// tool-result item following the most recent assistant-with-tool-requests.
pub fn select_turns_to_compact<T: CompactionItem>(
    item_token_counts: &[u32],
    items: &[T],
    target_tokens: u32,
    min_compactable: u32,
) -> Option<SplitPlan> {
    debug_assert_eq!(
        item_token_counts.len(),
        items.len(),
        "token counts and items must have the same length"
    );

    let total = items.len();
    if total == 0 {
        return None;
    }

    // Step 1: Walk backward, sum "keep" tokens until target is reached.
    // Find the highest split_idx such that sum(item_token_counts[split_idx..]) ≤ target_tokens.
    let mut kept = 0u32;
    let mut split_idx = total; // start with "compact nothing", will move down
    for i in (0..total).rev() {
        let count = item_token_counts[i];
        if kept.saturating_add(count) > target_tokens {
            // Adding this item would exceed the budget — split here.
            split_idx = i + 1;
            break;
        }
        kept = kept.saturating_add(count);
        split_idx = i;
    }

    // If the whole list fits within the budget, nothing to compact.
    if split_idx == 0 {
        return None;
    }

    // Step 2: Snap the split forward to a safe boundary.
    let safe_split_idx = snap_to_safe_boundary(items, split_idx);

    // After snapping forward we might have eaten everything.
    if safe_split_idx >= total {
        return None;
    }

    // Step 3: Compute tokens to compact and check the minimum.
    let tokens_to_compact: u32 = item_token_counts[..safe_split_idx]
        .iter()
        .copied()
        .fold(0u32, u32::saturating_add);

    if tokens_to_compact < min_compactable {
        return None;
    }

    Some(SplitPlan {
        split_idx: safe_split_idx,
        tokens_to_compact,
    })
}

/// If `candidate` lands on a tool-result item, advance forward past all
/// tool-result items in the same tool-pair run. The "run" is delimited by the
/// previous assistant item (with tool requests) and the next non-tool item.
///
/// In effect: ensure the split lands either right before an assistant, user,
/// system, or developer item — never between an assistant-with-tool-requests
/// and its tool results.
fn snap_to_safe_boundary<T: CompactionItem>(items: &[T], candidate: usize) -> usize {
    let total = items.len();
    if candidate >= total {
        return total;
    }

    // If candidate is not a tool-result item, no snap needed.
    if !items[candidate].is_tool_result() {
        return candidate;
    }

    // Candidate is a tool-result item. Find the run of contiguous tool-result
    // items (starting from the assistant-with-tool-requests that preceded
    // them) and advance to just past the last one in that run.
    let mut idx = candidate;
    while idx < total && items[idx].is_tool_result() {
        idx += 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::CompactionRole;

    /// Minimal mock implementing [`CompactionItem`] for selection tests.
    struct MockItem {
        role: CompactionRole,
    }

    impl MockItem {
        fn user() -> Self {
            Self {
                role: CompactionRole::User,
            }
        }
        fn assistant() -> Self {
            Self {
                role: CompactionRole::Assistant,
            }
        }
        fn tool() -> Self {
            Self {
                role: CompactionRole::Tool,
            }
        }
    }

    impl CompactionItem for MockItem {
        fn role(&self) -> CompactionRole {
            self.role
        }
        fn text(&self) -> Option<String> {
            None
        }
        fn has_tool_requests(&self) -> bool {
            false
        }
        fn is_compaction_summary(&self) -> bool {
            false
        }
        fn attachment_refs(&self) -> Vec<crate::item::CompactionFileRef> {
            Vec::new()
        }
    }

    #[test]
    fn empty_returns_none() {
        let items: Vec<MockItem> = vec![];
        assert!(select_turns_to_compact(&[], &items, 100, 10).is_none());
    }

    #[test]
    fn all_fits_in_budget_returns_none() {
        let items = vec![MockItem::user(), MockItem::assistant()];
        let counts = vec![10, 20];
        assert!(select_turns_to_compact(&counts, &items, 1000, 5).is_none());
    }

    #[test]
    fn splits_at_correct_index() {
        // Total 100; target 30 → keep last few that fit in 30.
        let items = vec![
            MockItem::user(),
            MockItem::assistant(),
            MockItem::user(),
            MockItem::assistant(),
        ];
        let counts = vec![40, 30, 20, 10]; // keep last two (sum 30)
        let plan = select_turns_to_compact(&counts, &items, 30, 5).expect("should split");
        assert_eq!(plan.split_idx, 2);
        assert_eq!(plan.tokens_to_compact, 70);
    }

    #[test]
    fn below_min_compactable_returns_none() {
        let items = vec![MockItem::user(), MockItem::assistant()];
        let counts = vec![5, 100];
        // Would split after index 0, but 5 < min_compactable (10).
        assert!(select_turns_to_compact(&counts, &items, 50, 10).is_none());
    }

    #[test]
    fn snaps_past_tool_results() {
        // Layout: [User, Assistant-text, Assistant-with-tools, Tool, Tool, Assistant-text]
        // If the naïve split lands on a Tool, snap forward past all Tools.
        let items = vec![
            MockItem::user(),
            MockItem::assistant(),
            MockItem::assistant(), // pretend this had tool_requests
            MockItem::tool(),
            MockItem::tool(),
            MockItem::assistant(),
        ];
        let counts = vec![10, 10, 10, 50, 50, 10];

        // Target 60 → walking back: keep 10 (idx 5), keep 50 (idx 4)
        // → 60 used. Adding idx 3 (50) overflows.
        // Naïve split = 4. But items[4] is Tool → snap forward.
        // Walk forward: items[4]=Tool, items[5]=Assistant → snap to 5.
        let plan = select_turns_to_compact(&counts, &items, 60, 5).expect("should split");
        assert_eq!(plan.split_idx, 5);
        assert_eq!(plan.tokens_to_compact, 10 + 10 + 10 + 50 + 50);
    }

    #[test]
    fn snap_does_not_advance_when_already_safe() {
        let items = vec![
            MockItem::user(),
            MockItem::assistant(),
            MockItem::user(), // safe split here
            MockItem::assistant(),
        ];
        let counts = vec![50, 50, 10, 10];
        // Target 30 → keep last two (sum 20).
        // Naïve split = 2. items[2] = User → safe, no snap needed.
        let plan = select_turns_to_compact(&counts, &items, 30, 5).expect("should split");
        assert_eq!(plan.split_idx, 2);
    }

    #[test]
    fn snap_walks_to_end_returns_none() {
        // Pathological: split would need to snap past all items.
        let items = vec![MockItem::assistant(), MockItem::tool(), MockItem::tool()];
        let counts = vec![10, 50, 50];
        // Target 0 → naïve split = 1 (items[1] is Tool).
        // Snap forward: items[1]=Tool, items[2]=Tool, idx=3=total.
        // Return None — nothing left to keep.
        assert!(select_turns_to_compact(&counts, &items, 0, 5).is_none());
    }
}
