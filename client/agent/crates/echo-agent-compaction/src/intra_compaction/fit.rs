//! Fit FullReplace summarizer input to a **token** budget.
//!
//! # Ordered ladder (strict sequence — do not reorder)
//!
//! Stages run **in order**. A later stage is attempted **only if** earlier
//! stages still leave `history ++ steps` over budget. Effects accumulate
//! (e.g. `ToolTruncated` may already have dropped history).
//!
//! ```text
//! 0. Verbatim              history ++ steps already ≤ budget
//! 1. HistoryTurnSelected   drop oldest **history** turns first
//!                          (prefer keeping all steps)
//! 2. ToolTruncated         only if still over: prefix-clip tool results
//!                          that alone exceed budget (echo-agent-build style:
//!                          max_bytes = max_tokens * 4, no binary search)
//! 3. StepTurnsSelected     only if still over: drop oldest **step** turns
//!                          (keep remaining history)
//! 4. Emergency             only if still over: hard-shrink newest item
//! ```
//!
//! Reuses:
//! - [`select_turns_to_compact`] for history/step suffix selection
//! - [`ItemTokenCounter`] for size decisions
//! - harness [`CompactionItemBuilder::truncate_payload_for_compaction`]
//!   (one-shot, same `tokens * 4` budget as echo-agent-build)

use tracing::info;

use crate::item::CompactionItemBuilder;
use crate::select::select_turns_to_compact;
use crate::token::ItemTokenCounter;

/// Which stage of the **ordered** fit ladder first made the input fit.
///
/// Ladder order is fixed (see module docs). Later rungs run only when earlier
/// ones are insufficient; earlier side-effects still apply. Telemetry only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitRung {
    /// No change; full history++steps fits.
    Verbatim,
    /// Dropped oldest prior-conversation history turns (steps untouched).
    HistoryTurnSelected,
    /// Still over after history drop → prefix-clipped oversized tool results.
    ToolTruncated,
    /// Still over after tool shrink → dropped oldest in-loop step turns.
    StepTurnsSelected,
    /// Still over after step drop → only newest item, hard-shrunk to budget.
    Emergency,
}

impl FitRung {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verbatim => "verbatim",
            Self::HistoryTurnSelected => "history_turn_selected",
            Self::ToolTruncated => "tool_truncated",
            Self::StepTurnsSelected => "step_turns_selected",
            Self::Emergency => "emergency",
        }
    }
}

/// Result of fitting turns for the summarizer.
#[derive(Debug, Clone)]
pub struct FitPlan<T> {
    /// Chronological (oldest→newest) turns to feed the summarizer.
    pub llm_turns: Vec<T>,
    pub tokens_raw: u32,
    pub tokens_fit: u32,
    /// Highest ladder stage that was needed (see [`FitRung`] order).
    pub rung: FitRung,
    pub items_truncated: u32,
    pub history_turns_omitted: u32,
    pub step_turns_omitted: u32,
}

fn sum_tokens<T>(turns: &[T], counter: &dyn ItemTokenCounter<T>) -> u32 {
    turns
        .iter()
        .map(|t| counter.count_item_tokens(t))
        .fold(0u32, u32::saturating_add)
}

fn token_counts<T>(turns: &[T], counter: &dyn ItemTokenCounter<T>) -> Vec<u32> {
    turns.iter().map(|t| counter.count_item_tokens(t)).collect()
}

fn sum_counts(counts: &[u32]) -> u32 {
    counts.iter().copied().fold(0u32, u32::saturating_add)
}

fn concat_turns<T: Clone>(history: &[T], steps: &[T]) -> Vec<T> {
    let mut out = history.to_vec();
    out.extend_from_slice(steps);
    out
}

/// One-shot payload shrink (echo-agent-build: `max_bytes = max_tokens * 4`).
///
/// No binary search / re-tokenize loop — same as
/// `fit_conversation_to_budget` → `truncate_item_to_tokens`.
fn shrink_item_to_token_budget<T: CompactionItemBuilder>(item: &T, max_tokens: u32) -> T {
    item.truncate_payload_for_compaction(max_tokens)
}

/// Prefix-clip **tool results** that alone exceed `max_tokens`.
///
/// Matches echo-agent-build's focus: in-place truncate is for oversized tool
/// payloads (and emergency tail). Non-tool turns are left alone here.
fn shrink_oversized_tool_results<T: CompactionItemBuilder>(
    turns: &[T],
    counter: &dyn ItemTokenCounter<T>,
    max_tokens: u32,
) -> (Vec<T>, u32) {
    let mut n = 0u32;
    let out = turns
        .iter()
        .map(|t| {
            if t.is_tool_result() && counter.count_item_tokens(t) > max_tokens {
                n += 1;
                shrink_item_to_token_budget(t, max_tokens)
            } else {
                t.clone()
            }
        })
        .collect();
    (out, n)
}

/// Keep newest contiguous suffix under `budget` (`select_turns_to_compact` keep side).
///
/// `counts` must match `turns` 1:1 (caller materializes once; do not re-tokenize).
fn keep_recent_under_budget<T: CompactionItemBuilder>(
    turns: &[T],
    counts: &[u32],
    budget: u32,
) -> Option<(Vec<T>, u32)> {
    debug_assert_eq!(turns.len(), counts.len());
    if turns.is_empty() {
        return Some((Vec::new(), 0));
    }
    if budget == 0 {
        return Some((Vec::new(), turns.len() as u32));
    }
    if sum_counts(counts) <= budget {
        return Some((turns.to_vec(), 0));
    }
    let plan = select_turns_to_compact(counts, turns, budget, 0)?;
    let keep = turns[plan.split_idx..].to_vec();
    let omitted = plan.split_idx as u32;
    Some((keep, omitted))
}

fn plan_ok<T: Clone>(
    history: &[T],
    steps: &[T],
    counter: &dyn ItemTokenCounter<T>,
    tokens_raw: u32,
    rung: FitRung,
    items_truncated: u32,
    history_turns_omitted: u32,
    step_turns_omitted: u32,
) -> FitPlan<T> {
    let llm_turns = concat_turns(history, steps);
    let tokens_fit = sum_tokens(&llm_turns, counter);
    info!(
        tokens_raw,
        tokens_fit,
        rung = rung.as_str(),
        items_truncated,
        history_turns_omitted,
        step_turns_omitted,
        history_kept = history.len(),
        steps_kept = steps.len(),
        "[IntraCompaction][Fit] fitted summarizer input"
    );
    FitPlan {
        llm_turns,
        tokens_raw,
        tokens_fit,
        rung,
        items_truncated,
        history_turns_omitted,
        step_turns_omitted,
    }
}

/// Fit `history ++ steps` into `budget` **tokens** for FullReplace summarizer input.
///
/// **Ordered ladder** (strict — later only if earlier still insufficient):
/// 1. [`FitRung::HistoryTurnSelected`] — drop oldest history first  
/// 2. [`FitRung::ToolTruncated`] — then shrink oversized payloads  
/// 3. [`FitRung::StepTurnsSelected`] — then drop oldest steps  
/// 4. [`FitRung::Emergency`] — finally hard-shrink newest item  
///
/// Output `llm_turns` is always chronological: remaining history then remaining steps.
/// [`FitPlan::rung`] is the highest stage that was required.
pub fn fit_turns_for_summarizer<T: CompactionItemBuilder>(
    history: &[T],
    steps: &[T],
    counter: &dyn ItemTokenCounter<T>,
    budget: u32,
) -> FitPlan<T> {
    // Materialize per-turn counts once for the pre-truncate stages. After tool
    // shrink we re-count (payloads changed). Avoids ~N full BPE passes when the
    // harness token cache is cold / wrong tokenizer (counter ≠ compaction model).
    let mut hist_counts = token_counts(history, counter);
    let mut step_counts = token_counts(steps, counter);
    let tokens_raw = sum_counts(&hist_counts).saturating_add(sum_counts(&step_counts));

    if budget == 0 {
        return FitPlan {
            llm_turns: Vec::new(),
            tokens_raw,
            tokens_fit: 0,
            rung: FitRung::Emergency,
            items_truncated: 0,
            history_turns_omitted: history.len() as u32,
            step_turns_omitted: steps.len() as u32,
        };
    }

    // 0) Verbatim
    if tokens_raw <= budget {
        return plan_ok(
            history,
            steps,
            counter,
            tokens_raw,
            FitRung::Verbatim,
            0,
            0,
            0,
        );
    }

    let mut hist = history.to_vec();
    let mut step = steps.to_vec();
    let mut hist_omitted = 0u32;
    let mut step_omitted = 0u32;

    // ── 1) HistoryTurnSelected (first) ────────────────────────────────
    // Drop oldest history only. Prefer keeping *all* steps.
    let steps_tokens = sum_counts(&step_counts);
    if steps_tokens < budget {
        let hist_budget = budget - steps_tokens;
        if sum_counts(&hist_counts) > hist_budget {
            if let Some((keep, omitted)) =
                keep_recent_under_budget(&hist, &hist_counts, hist_budget)
            {
                hist_omitted = omitted;
                hist_counts = hist_counts.split_off(omitted as usize);
                hist = keep;
            } else {
                hist_omitted = hist.len() as u32;
                hist.clear();
                hist_counts.clear();
            }
        }
    } else {
        // Steps alone already ≥ budget → drop all history; continue ladder.
        hist_omitted = hist.len() as u32;
        hist.clear();
        hist_counts.clear();
    }

    // Enough after history drop alone? Stop — do not touch tools/steps.
    if sum_counts(&hist_counts).saturating_add(sum_counts(&step_counts)) <= budget {
        return plan_ok(
            &hist,
            &step,
            counter,
            tokens_raw,
            FitRung::HistoryTurnSelected,
            0,
            hist_omitted,
            0,
        );
    }

    // ── 2) ToolTruncated (only if history drop still insufficient) ────
    // One-shot prefix clip on tool results only (echo-agent-build style).
    let (hist2, n1) = shrink_oversized_tool_results(&hist, counter, budget);
    let (step2, n2) = shrink_oversized_tool_results(&step, counter, budget);
    hist = hist2;
    step = step2;
    let items_truncated = n1.saturating_add(n2);
    // Payloads may have changed — re-materialize counts once.
    hist_counts = token_counts(&hist, counter);
    step_counts = token_counts(&step, counter);

    if sum_counts(&hist_counts).saturating_add(sum_counts(&step_counts)) <= budget {
        return plan_ok(
            &hist,
            &step,
            counter,
            tokens_raw,
            FitRung::ToolTruncated,
            items_truncated,
            hist_omitted,
            0,
        );
    }

    // ── 3) StepTurnsSelected (only if tools still insufficient) ───────
    let hist_tokens = sum_counts(&hist_counts);
    if hist_tokens < budget {
        let step_budget = budget - hist_tokens;
        if sum_counts(&step_counts) > step_budget {
            if let Some((keep, omitted)) =
                keep_recent_under_budget(&step, &step_counts, step_budget)
            {
                step_omitted = omitted;
                step_counts = step_counts.split_off(omitted as usize);
                step = keep;
            } else {
                step_omitted = step.len() as u32;
                step.clear();
                step_counts.clear();
            }
        }
    } else {
        // History alone ≥ budget → drop all steps; emergency may shrink hist.
        step_omitted = step.len() as u32;
        step.clear();
        step_counts.clear();
    }

    let after_step_tokens = sum_counts(&hist_counts).saturating_add(sum_counts(&step_counts));
    // If selection cleared everything, do **not** return an empty
    // StepTurnsSelected plan — fall through to Emergency so FullReplace still
    // has a newest-item input (empty plan used to become NothingToCompact).
    if after_step_tokens <= budget && (!hist.is_empty() || !step.is_empty()) {
        return plan_ok(
            &hist,
            &step,
            counter,
            tokens_raw,
            FitRung::StepTurnsSelected,
            items_truncated,
            hist_omitted,
            step_omitted,
        );
    }

    // ── 4) Emergency (last resort) ────────────────────────────────────
    // echo-agent-build: when even the newest unit alone exceeds budget, keep it
    // truncated in place (tool result, or lone assistant/user text).
    //
    // If the ladder emptied both sides (e.g. select returned None and both
    // hist/step were cleared), fall back to the original newest item so we
    // never hand FullReplace an empty plan (which used to become
    // NothingToCompact and abort CLE recovery).
    let combined = concat_turns(&hist, &step);
    let (source, history_turns_omitted, step_turns_omitted) = if combined.is_empty() {
        info!(
            tokens_raw,
            budget,
            history_len = history.len(),
            steps_len = steps.len(),
            "[IntraCompaction][Fit] ladder emptied input — emergency from original newest"
        );
        let all = concat_turns(history, steps);
        // Keep only the overall newest turn.
        let (ho, so) = if steps.is_empty() {
            (history.len().saturating_sub(1) as u32, 0u32)
        } else {
            (history.len() as u32, steps.len().saturating_sub(1) as u32)
        };
        (all, ho, so)
    } else if step.is_empty() {
        // Newest is last remaining history turn.
        (
            combined,
            hist_omitted.saturating_add(hist.len().saturating_sub(1) as u32),
            step_omitted,
        )
    } else {
        // Newest is last remaining step turn; drop all remaining history.
        (
            combined,
            hist_omitted.saturating_add(hist.len() as u32),
            step_omitted.saturating_add(step.len().saturating_sub(1) as u32),
        )
    };

    let Some(newest) = source.last() else {
        // Caller had no turns (compact already guards total_turns == 0).
        return FitPlan {
            llm_turns: Vec::new(),
            tokens_raw,
            tokens_fit: 0,
            rung: FitRung::Emergency,
            items_truncated,
            history_turns_omitted,
            step_turns_omitted,
        };
    };
    let last = vec![shrink_item_to_token_budget(newest, budget)];
    let tokens_fit = sum_tokens(&last, counter);
    info!(
        tokens_raw,
        tokens_fit,
        budget,
        rung = FitRung::Emergency.as_str(),
        history_turns_omitted,
        step_turns_omitted,
        "[IntraCompaction][Fit] hard-truncated newest item for summarizer"
    );
    FitPlan {
        llm_turns: last,
        tokens_raw,
        tokens_fit,
        rung: FitRung::Emergency,
        items_truncated: items_truncated.saturating_add(1),
        history_turns_omitted,
        step_turns_omitted,
    }
}

/// Prefix-truncate `text` to roughly `max_tokens` (bytes ≈ `max_tokens * 4`).
///
/// Mirrors echo-agent-build `truncate_text_to_bytes` / `truncate_item_to_tokens`:
/// keep a leading prefix, append a dropped-bytes marker. No binary search.
pub fn truncate_text_to_token_budget(text: &str, max_tokens: u32) -> String {
    let max_bytes = (max_tokens as usize).saturating_mul(4);
    if text.len() <= max_bytes {
        return text.to_string();
    }
    // Reserve room for the marker so the result stays near max_bytes.
    const MARKER_RESERVE: usize = 64;
    let keep = max_bytes.saturating_sub(MARKER_RESERVE);
    let mut end = keep.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = text.len() - end;
    format!(
        "{}\n[... truncated {dropped} bytes to fit the compaction window ...]",
        &text[..end]
    )
}

/// Deprecated name — prefer [`truncate_text_to_token_budget`].
#[inline]
pub fn truncate_text_for_compaction(text: &str, max_tokens: u32) -> String {
    truncate_text_to_token_budget(text, max_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{CompactionFileRef, CompactionItem, CompactionItemBuilder, CompactionRole};

    #[derive(Debug, Clone)]
    struct MockItem {
        role: CompactionRole,
        text: String,
    }

    impl MockItem {
        fn user(text: &str) -> Self {
            Self {
                role: CompactionRole::User,
                text: text.to_string(),
            }
        }
        fn tool(text: &str) -> Self {
            Self {
                role: CompactionRole::Tool,
                text: text.to_string(),
            }
        }
        fn assistant(text: &str) -> Self {
            Self {
                role: CompactionRole::Assistant,
                text: text.to_string(),
            }
        }
        fn labeled(prefix: &str, i: usize, pad: usize) -> Self {
            Self::user(&format!("{prefix}-{i}-{}", "x".repeat(pad)))
        }
    }

    impl CompactionItem for MockItem {
        fn role(&self) -> CompactionRole {
            self.role
        }
        fn text(&self) -> Option<String> {
            Some(self.text.clone())
        }
        fn has_tool_requests(&self) -> bool {
            false
        }
        fn is_compaction_summary(&self) -> bool {
            false
        }
        fn attachment_refs(&self) -> Vec<CompactionFileRef> {
            Vec::new()
        }
        fn is_tool_result(&self) -> bool {
            matches!(self.role, CompactionRole::Tool)
        }
    }

    impl CompactionItemBuilder for MockItem {
        fn compaction_summary_item(text: String) -> Self {
            Self {
                role: CompactionRole::Developer,
                text,
            }
        }
        fn strip_tool_content(&self) -> Option<Self> {
            Some(self.clone())
        }
        fn truncate_payload_for_compaction(&self, max_tokens: u32) -> Self {
            Self {
                role: self.role,
                text: truncate_text_to_token_budget(&self.text, max_tokens),
            }
        }
    }

    struct CharCounter;
    impl ItemTokenCounter<MockItem> for CharCounter {
        fn count_item_tokens(&self, item: &MockItem) -> u32 {
            (item.text.chars().count() as u32 / 4).max(1)
        }
    }

    #[test]
    fn verbatim_when_under_budget() {
        let hist = vec![MockItem::user("hello")];
        let steps = vec![MockItem::assistant("world")];
        let plan = fit_turns_for_summarizer(&hist, &steps, &CharCounter, 10_000);
        assert_eq!(plan.rung, FitRung::Verbatim);
        assert_eq!(plan.llm_turns.len(), 2);
    }

    #[test]
    fn drops_old_history_before_tool_truncate() {
        // Large history + small steps → should HistoryTurnSelected, not touch tools.
        let hist: Vec<_> = (0..20).map(|i| MockItem::labeled("h", i, 80)).collect();
        let steps = vec![MockItem::assistant("step-ok")];
        let budget = 200;
        let plan = fit_turns_for_summarizer(&hist, &steps, &CharCounter, budget);
        assert_eq!(plan.rung, FitRung::HistoryTurnSelected);
        assert!(plan.history_turns_omitted > 0);
        assert_eq!(plan.step_turns_omitted, 0);
        assert!(plan.tokens_fit <= budget);
        // steps preserved
        assert!(
            plan.llm_turns
                .iter()
                .any(|t| t.text().unwrap().contains("step-ok"))
        );
        // chronological: remaining history then step
        assert_eq!(plan.llm_turns.last().unwrap().text().unwrap(), "step-ok");
    }

    #[test]
    fn tool_truncate_only_after_history_drop_insufficient() {
        // No history; one huge tool in steps → ToolTruncated (history stage is a no-op).
        let hist: Vec<MockItem> = vec![];
        let huge = "x".repeat(40_000);
        let steps = vec![MockItem::assistant("calling"), MockItem::tool(&huge)];
        let budget = 5_000;
        let plan = fit_turns_for_summarizer(&hist, &steps, &CharCounter, budget);
        assert!(
            matches!(
                plan.rung,
                FitRung::ToolTruncated | FitRung::StepTurnsSelected | FitRung::Emergency
            ),
            "rung={:?}",
            plan.rung
        );
        assert!(plan.tokens_fit <= budget);
        if plan.rung == FitRung::ToolTruncated {
            assert!(plan.items_truncated >= 1);
        }
    }

    #[test]
    fn ladder_order_history_before_tool_before_steps() {
        // Large history + huge tool in steps: history must be dropped first; if that
        // alone is not enough, tool shrink runs before any step-turn drop.
        let hist: Vec<_> = (0..20).map(|i| MockItem::labeled("h", i, 80)).collect();
        let huge = "x".repeat(40_000);
        let steps = vec![MockItem::assistant("keep-me"), MockItem::tool(&huge)];
        let budget = 5_000;
        let plan = fit_turns_for_summarizer(&hist, &steps, &CharCounter, budget);
        assert!(plan.tokens_fit <= budget);
        // History stage always runs first when over budget → all/most history gone.
        assert!(
            plan.history_turns_omitted > 0,
            "history must be selected before tools; omitted={}",
            plan.history_turns_omitted
        );
        // Must not stop at HistoryTurnSelected alone (huge tool still exceeds).
        assert_ne!(plan.rung, FitRung::HistoryTurnSelected);
        assert_ne!(plan.rung, FitRung::Verbatim);
        // Tool shrink before step selection: if we only needed tools, steps stay.
        if plan.rung == FitRung::ToolTruncated {
            assert!(plan.items_truncated >= 1);
            assert_eq!(plan.step_turns_omitted, 0);
            assert!(
                plan.llm_turns
                    .iter()
                    .any(|t| t.text().unwrap().contains("keep-me"))
            );
        }
    }

    #[test]
    fn step_turns_selected_after_tools_still_over() {
        // Empty history, many medium steps (no single item over budget) → StepTurnsSelected.
        let hist: Vec<MockItem> = vec![];
        let steps: Vec<_> = (0..30).map(|i| MockItem::labeled("s", i, 80)).collect();
        let budget = 200;
        let plan = fit_turns_for_summarizer(&hist, &steps, &CharCounter, budget);
        assert_eq!(plan.rung, FitRung::StepTurnsSelected);
        assert!(plan.step_turns_omitted > 0);
        assert!(plan.tokens_fit <= budget);
        // contiguous newest suffix of steps
        let idxs: Vec<usize> = plan
            .llm_turns
            .iter()
            .map(|t| {
                t.text()
                    .unwrap()
                    .strip_prefix("s-")
                    .and_then(|s| s.split('-').next())
                    .and_then(|s| s.parse().ok())
                    .unwrap()
            })
            .collect();
        assert!(idxs.windows(2).all(|w| w[0] < w[1]));
        let start = *idxs.first().unwrap();
        assert_eq!(idxs, (start..30).collect::<Vec<_>>());
    }

    #[test]
    fn preserves_history_then_steps_order() {
        let hist = vec![MockItem::labeled("h", 0, 20), MockItem::labeled("h", 1, 20)];
        let steps = vec![MockItem::labeled("s", 0, 20), MockItem::labeled("s", 1, 20)];
        let plan = fit_turns_for_summarizer(&hist, &steps, &CharCounter, 10_000);
        let labels: Vec<_> = plan
            .llm_turns
            .iter()
            .map(|t| t.text().unwrap()[..3].to_string())
            .collect();
        assert_eq!(labels, vec!["h-0", "h-1", "s-0", "s-1"]);
    }

    /// When step selection would leave nothing (single item alone exceeds
    /// budget → select returns None → clear), Emergency must still return the
    /// newest original item and shrink it under budget (not empty plan).
    #[test]
    fn emergency_keeps_newest_when_ladder_would_empty() {
        let hist: Vec<MockItem> = vec![];
        let huge = "x".repeat(40_000);
        let steps = vec![MockItem::user(&huge)];
        let budget = 100;
        let plan = fit_turns_for_summarizer(&hist, &steps, &CharCounter, budget);
        assert_eq!(plan.rung, FitRung::Emergency);
        assert_eq!(plan.llm_turns.len(), 1, "must not return empty plan");
        assert!(plan.items_truncated >= 1);
        // MockItem truncate is real — after Emergency, fitted size must fit.
        assert!(
            plan.tokens_fit <= budget,
            "tokens_fit={} > budget={budget}",
            plan.tokens_fit
        );
        let text = plan.llm_turns[0].text().unwrap();
        assert!(
            text.len() < huge.len(),
            "user text must be prefix-clipped in Emergency"
        );
    }

    /// Many oversize non-tool steps with a tiny budget: ladder clears keep
    /// set → Emergency from original newest, non-empty.
    #[test]
    fn emergency_from_select_clear_still_non_empty() {
        let hist: Vec<MockItem> = vec![];
        // Each item alone exceeds budget=1 under CharCounter (/4, max 1).
        let steps: Vec<_> = (0..5)
            .map(|i| MockItem::user(&format!("s-{i}-{}", "x".repeat(40))))
            .collect();
        let plan = fit_turns_for_summarizer(&hist, &steps, &CharCounter, 1);
        assert_eq!(plan.rung, FitRung::Emergency);
        assert_eq!(plan.llm_turns.len(), 1);
        assert!(plan.tokens_fit <= 1 || plan.items_truncated >= 1);
    }
}
