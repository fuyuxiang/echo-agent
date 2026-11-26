//! Pure builders for prefire two-pass compaction (shell-only).
//!
//! Pass1 summarizes ~95% of history (by estimated-token weight) → NOTE₁.
//! Pass2 rewrites NOTE₁ + the ~5% tail into the successor-visible NOTE₂.
//! Sampling lives in [`super::compaction`]; this module has no I/O.

use echo_agent_chat_state::estimate_item_tokens;
use echo_agent_sampling_types::ConversationItem;

/// Default history fraction covered by pass1; the remainder is the blocking
/// pass2 tail, so keep it small (prod pass2 latency is dominated by tail prefill).
pub(crate) const TWO_PASS_DEFAULT_SPLIT_FRACTION: f64 = 0.95;

/// Minimum char length for a closed `<summary>` block to be preferred as NOTE₁
/// over the full pass1 response.
const TWO_PASS_MIN_SUMMARY_BLOCK_CHARS: usize = 1000;

/// Cap on NOTE₁ text embedded in pass2 (carrier + special turn).
const TWO_PASS_MAX_NOTE1_CHARS: usize = 12_000;

/// Result of splitting a conversation for two-pass compaction.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TwoPassSplit<'a> {
    pub prefix: &'a [ConversationItem],
    pub tail: &'a [ConversationItem],
    pub split_idx: usize,
}

/// Choose a split index so prefix weight is at least `fraction` of total.
fn split_index_by_token_fraction(weights: &[u64], fraction: f64) -> usize {
    if weights.is_empty() {
        return 0;
    }
    let frac = fraction.clamp(0.05, 0.95);
    let total_w = weights.iter().copied().sum::<u64>().max(1);
    let target_w = frac * total_w as f64;
    let mut acc = 0u64;
    let mut split_idx = weights.len().saturating_sub(1).max(1);
    for (i, w) in weights.iter().enumerate() {
        acc = acc.saturating_add(*w);
        if acc as f64 >= target_w {
            split_idx = (i + 1).max(1);
            break;
        }
    }
    if split_idx >= weights.len() && weights.len() > 1 {
        split_idx = weights.len() - 1;
    }
    split_idx
}

/// Never separate an assistant `tool_calls` turn from its following `ToolResult`s.
fn snap_split_idx_to_tool_boundaries(
    conversation: &[ConversationItem],
    mut split_idx: usize,
) -> usize {
    let n = conversation.len();
    if n == 0 {
        return 0;
    }
    split_idx = split_idx.min(n);

    while split_idx < n && matches!(conversation[split_idx], ConversationItem::ToolResult(_)) {
        split_idx += 1;
    }
    if split_idx < n
        && let ConversationItem::Assistant(a) = &conversation[split_idx]
        && !a.tool_calls.is_empty()
    {
        split_idx += 1;
        while split_idx < n && matches!(conversation[split_idx], ConversationItem::ToolResult(_)) {
            split_idx += 1;
        }
    }
    while split_idx > 0 && split_idx < n {
        let Some(ConversationItem::Assistant(a)) = conversation.get(split_idx - 1) else {
            break;
        };
        if a.tool_calls.is_empty() {
            break;
        }
        if !matches!(
            conversation.get(split_idx),
            Some(ConversationItem::ToolResult(_))
        ) {
            break;
        }
        while split_idx < n && matches!(conversation[split_idx], ConversationItem::ToolResult(_)) {
            split_idx += 1;
        }
    }

    if split_idx >= n && n > 1 {
        let mut candidate = n - 1;
        while candidate > 1 && matches!(conversation[candidate], ConversationItem::ToolResult(_)) {
            candidate -= 1;
        }
        if candidate > 0
            && let ConversationItem::Assistant(a) = &conversation[candidate]
            && !a.tool_calls.is_empty()
        {
            // candidate at assistant — good for tail start.
        } else if candidate > 0
            && matches!(conversation[candidate], ConversationItem::ToolResult(_))
        {
            let mut i = candidate;
            while i > 0 && matches!(conversation[i], ConversationItem::ToolResult(_)) {
                i -= 1;
            }
            if matches!(
                conversation.get(i),
                Some(ConversationItem::Assistant(a)) if !a.tool_calls.is_empty()
            ) {
                candidate = i;
            }
        }
        if candidate >= 1 && candidate < n {
            split_idx = candidate;
        }
    }

    split_idx.min(n)
}

/// Split `conversation` into pass1 prefix / pass2 tail by estimated-token weight.
pub(crate) fn split_conversation_for_two_pass(
    conversation: &[ConversationItem],
    split_fraction: f64,
) -> TwoPassSplit<'_> {
    let weights: Vec<u64> = conversation.iter().map(estimate_item_tokens).collect();
    let mut split_idx = split_index_by_token_fraction(&weights, split_fraction);
    split_idx = snap_split_idx_to_tool_boundaries(conversation, split_idx);
    let split_idx = split_idx.min(conversation.len());
    TwoPassSplit {
        prefix: &conversation[..split_idx],
        tail: &conversation[split_idx..],
        split_idx,
    }
}

fn extract_summary_block(text: &str, min_chars: usize) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let open = "<summary>";
    let close = "</summary>";
    let lower = text.to_ascii_lowercase();
    let mut blocks: Vec<String> = Vec::new();
    let mut search_from = 0usize;
    let text_bytes = text.as_bytes();
    let lower_bytes = lower.as_bytes();
    while search_from < lower_bytes.len() {
        let Some(rel) = find_bytes(&lower_bytes[search_from..], open.as_bytes()) else {
            break;
        };
        let start = search_from + rel + open.len();
        let Some(rel_close) = find_bytes(&lower_bytes[start..], close.as_bytes()) else {
            break;
        };
        let end = start + rel_close;
        let inner = std::str::from_utf8(&text_bytes[start..end]).unwrap_or("");
        blocks.push(inner.to_string());
        search_from = end + close.len();
    }
    for block in blocks.into_iter().rev() {
        let stripped = block.trim();
        if stripped.chars().count() > min_chars {
            return Some(stripped.to_string());
        }
    }
    None
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Prefer a substantive `<summary>` inner for NOTE₁; otherwise the full pass1 response.
pub(crate) fn note_for_two_pass_pass2(pass1_raw: &str) -> String {
    let mut note = extract_summary_block(pass1_raw, TWO_PASS_MIN_SUMMARY_BLOCK_CHARS)
        .unwrap_or_else(|| pass1_raw.trim().to_string());
    let n = note.chars().count();
    if n > TWO_PASS_MAX_NOTE1_CHARS {
        note = note.chars().take(TWO_PASS_MAX_NOTE1_CHARS).collect();
        note.push_str("\n\n[… NOTE₁ truncated for pass2 input budget …]");
    }
    note
}

fn format_two_pass_note1_carrier(note1: &str) -> String {
    let note1 = note1.trim();
    format!(
        "Your conversation was summarized due to context constraints. \
         Here is the summary of the conversation so far:\n\n\
         <summary_content>\n{note1}\n</summary_content>\n\n\
         Continue with the compaction task below."
    )
}

fn format_two_pass_special_pass2_user(note1: &str, compaction_prompt: &str) -> String {
    let note1 = note1.trim();
    let summary_block = format!("<summary_content>\n{note1}\n</summary_content>");
    let uq = if compaction_prompt.trim().is_empty() {
        "Please summarize the conversation so far."
    } else {
        compaction_prompt
    };
    format!(
        "This is a special compaction case (two-pass / hierarchical summarization).\n\
         You are writing the *final* compaction note that a successor assistant will \
         rely on as their only memory of the conversation.\n\n\
         Critical requirements:\n\
         - Incorporate the **entire** prior summary below into your final note — do not \
         omit sections, defer to \"see prior compaction\", or drop early history because \
         newer turns are in context.\n\
         - Merge that prior summary with the more recent conversation turns above into \
         one coherent, faithful, self-contained summary (same structure/sections you \
         normally use for compaction).\n\
         - Preserve concrete values, file paths, errors/blockers, operational how-tos, \
         key findings, and pending tasks from *both* the prior summary and the recent \
         turns when they still matter.\n\n\
         Prior summary to incorporate in full (duplicate of the summary_content above):\n\n\
         {summary_block}\n\n\
         Compaction instruction:\n\
         {uq}"
    )
}

/// Pass1 sample history: `prefix` + compaction instruction user turn.
pub(crate) fn build_two_pass_pass1_history(
    prefix: &[ConversationItem],
    compaction_prompt: &str,
) -> Vec<ConversationItem> {
    let mut history = prefix.to_vec();
    history.push(ConversationItem::user(compaction_prompt.to_string()));
    history
}

/// Pass2 sample history: system (from prefix) + NOTE₁ carrier + tail + special turn.
///
/// Successor-visible artifact is the model output of *this* history only (NOTE₂).
pub(crate) fn build_two_pass_pass2_history(
    prefix: &[ConversationItem],
    tail: &[ConversationItem],
    note1: &str,
    compaction_prompt: &str,
) -> Vec<ConversationItem> {
    let mut history: Vec<ConversationItem> = Vec::new();

    for item in prefix {
        if matches!(item, ConversationItem::System(_)) {
            history.push(item.clone());
        }
    }
    if !history
        .iter()
        .any(|i| matches!(i, ConversationItem::System(_)))
    {
        history.push(ConversationItem::system("You are a helpful assistant."));
    }

    history.push(ConversationItem::user(format_two_pass_note1_carrier(note1)));
    history.extend(tail.iter().cloned());
    history.push(ConversationItem::user(format_two_pass_special_pass2_user(
        note1,
        compaction_prompt,
    )));
    history
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_leaves_tail_when_possible() {
        let weights = vec![10u64, 10, 10, 10, 10];
        let idx = split_index_by_token_fraction(&weights, 0.9);
        assert!(idx < weights.len());
        assert!(idx >= 1);
    }

    #[test]
    fn default_split_fraction_leaves_five_percent_tail() {
        assert_eq!(TWO_PASS_DEFAULT_SPLIT_FRACTION, 0.95);
        // With the default fraction a non-empty tail must always survive the
        // split (pass2 needs recent turns to rewrite against NOTE₁).
        let weights = vec![10u64; 40];
        let idx = split_index_by_token_fraction(&weights, TWO_PASS_DEFAULT_SPLIT_FRACTION);
        assert_eq!(idx, 38); // 38/40 = 95% by weight
        assert!(idx < weights.len());
    }

    #[test]
    fn split_does_not_sever_tool_pairs() {
        use echo_agent_sampling_types::ToolCall;
        let mut assistant = ConversationItem::assistant("call");
        if let ConversationItem::Assistant(a) = &mut assistant {
            a.tool_calls.push(ToolCall {
                id: "tc1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            });
        }
        let conv = vec![
            ConversationItem::user("a".repeat(400)),
            ConversationItem::assistant("b".repeat(400)),
            assistant,
            ConversationItem::tool_result("tc1", "ok"),
            ConversationItem::user("tail"),
        ];
        let split = split_conversation_for_two_pass(&conv, 0.9);
        if let Some(ConversationItem::Assistant(a)) = split.prefix.last()
            && !a.tool_calls.is_empty()
        {
            assert!(!matches!(
                split.tail.first(),
                Some(ConversationItem::ToolResult(_))
            ));
        }
        assert!(!matches!(
            split.tail.first(),
            Some(ConversationItem::ToolResult(_))
        ));
        let prefix_has_call = split.prefix.iter().any(|i| {
            matches!(i, ConversationItem::Assistant(a) if a.tool_calls.iter().any(|t| t.id.as_ref() == "tc1"))
        });
        let prefix_has_result = split
            .prefix
            .iter()
            .any(|i| matches!(i, ConversationItem::ToolResult(t) if t.tool_call_id == "tc1"));
        let tail_has_call = split.tail.iter().any(|i| {
            matches!(i, ConversationItem::Assistant(a) if a.tool_calls.iter().any(|t| t.id.as_ref() == "tc1"))
        });
        let tail_has_result = split
            .tail
            .iter()
            .any(|i| matches!(i, ConversationItem::ToolResult(t) if t.tool_call_id == "tc1"));
        assert_eq!(prefix_has_call, prefix_has_result);
        assert_eq!(tail_has_call, tail_has_result);
    }

    #[test]
    fn note_prefers_summary_block_and_caps_huge_raw() {
        let text = format!(
            "<summary>short</summary>\n<summary>\n{}\n</summary>",
            "x".repeat(1001)
        );
        assert_eq!(note_for_two_pass_pass2(&text), "x".repeat(1001));
        assert_eq!(note_for_two_pass_pass2("no tags"), "no tags");
        let huge = "n".repeat(TWO_PASS_MAX_NOTE1_CHARS + 500);
        let note = note_for_two_pass_pass2(&huge);
        assert!(note.chars().count() <= TWO_PASS_MAX_NOTE1_CHARS + 80);
        assert!(note.contains("truncated"));
    }

    #[test]
    fn pass_histories_shape() {
        let conv = vec![
            ConversationItem::system("You are EchoAgent."),
            ConversationItem::user("early"),
            ConversationItem::assistant("early-a"),
            ConversationItem::user("late"),
            ConversationItem::assistant("late-a"),
        ];
        let split = split_conversation_for_two_pass(&conv, 0.5);
        let prompt = "1. Primary Request and Intent: x\n5. Optional Next Step: y\n";
        let pass1 = build_two_pass_pass1_history(split.prefix, prompt);
        assert!(matches!(pass1.last(), Some(ConversationItem::User(_))));

        let note1 = "x".repeat(1001);
        let pass2 = build_two_pass_pass2_history(split.prefix, split.tail, &note1, prompt);
        assert!(pass2.iter().any(|i| matches!(
            i,
            ConversationItem::System(s) if s.content.as_ref() == "You are EchoAgent."
        )));
        let texts: Vec<String> = pass2.iter().map(|i| i.text_content()).collect();
        assert!(texts.iter().any(|t| t.contains("<summary_content>")));
        assert!(
            texts
                .last()
                .is_some_and(|t| t.contains("special compaction case"))
        );
    }
}
