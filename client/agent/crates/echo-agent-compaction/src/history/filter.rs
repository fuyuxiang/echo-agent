//! Item filtering and user-query extraction for history compaction —
//! generic over [`CompactionItem`] / [`CompactionItemBuilder`].
//!
//! Behavior is byte-for-byte identical for EchoAgent chat (`T = Arc<EchoAgentTurn>`).

use tracing::info;

use crate::item::{CompactionItem, CompactionItemBuilder, CompactionRole};

/// Filter items for **basic** history compaction (both inter-compaction's
/// `Basic` strategy and intra-compaction's `history` target):
///
/// - Drop `System` items (the compaction LLM has its own system prompt).
/// - Drop `Developer` items that are not prior compaction summaries
///   (per-agent developer prompts shouldn't bleed into the summary; prior
///   compaction summaries must be preserved so they get re-summarised).
/// - Keep `User`, `Assistant`, and `Tool` items as-is.
pub fn filter_turns_for_basic<T: CompactionItem + Clone>(turns: &[T]) -> Vec<T> {
    turns
        .iter()
        .filter(|t| keep_turn_for_basic_compaction(*t))
        .cloned()
        .collect()
}

/// Predicate form of [`filter_turns_for_basic`]. Useful when callers need
/// to count or partition items without re-allocating the vector.
pub fn keep_turn_for_basic_compaction<T: CompactionItem + ?Sized>(turn: &T) -> bool {
    match turn.role() {
        CompactionRole::System => false,
        CompactionRole::Developer => turn.is_compaction_summary(),
        _ => true,
    }
}

/// Filter items for inter-compaction (used by both `Basic` and
/// `DivideAndConquer` — Basic is just a single-chunk run of the same
/// pipeline):
///
/// - Drop `Tool` items entirely (tool request/response).
/// - For `Assistant` items: drop tool-request contents; keep channels that
///   have visible user content (via
///   [`CompactionItemBuilder::strip_tool_content`]).
/// - Keep `User` items as-is (separation happens later).
/// - Drop `System` and non-summary `Developer` items; keep prior compaction
///   summaries so their `<echo_agent_user_queries>` sections can be split out.
pub fn filter_turns_for_inter_compaction<T: CompactionItemBuilder>(turns: &[T]) -> Vec<T> {
    turns
        .iter()
        .filter_map(|turn| match turn.role() {
            // Drop tool and system items.
            CompactionRole::Tool | CompactionRole::System => None,

            // Keep prior compaction summaries; drop all other developer items.
            CompactionRole::Developer => {
                if turn.is_compaction_summary() {
                    Some(turn.clone())
                } else {
                    None
                }
            }

            // Keep user items.
            CompactionRole::User => Some(turn.clone()),

            // Filter assistant item contents.
            CompactionRole::Assistant => turn.strip_tool_content(),
        })
        .collect()
}

/// Split prior compaction text into user_messages and the rest.
///
/// A prior compaction from DnC has the format:
/// ```text
/// <echo_agent_user_queries>
/// ...user messages...
/// </echo_agent_user_queries>
///
/// <chunk_summary index="0">
/// ...
/// </chunk_summary>
/// ```
///
/// Returns `(all_user_messages_sections, rest)`.
/// Extracts **all** `<echo_agent_user_queries>...</echo_agent_user_queries>` blocks
/// (there may be multiple after chained compactions) and concatenates them.
/// Everything outside these blocks is returned as `rest`.
/// If no blocks are found, returns `(None, full_text)`.
pub fn split_prior_compaction_text(text: &str) -> (Option<String>, String) {
    let start_tag = "<echo_agent_user_queries>";
    let end_tag = "</echo_agent_user_queries>";

    let mut user_sections = Vec::new();
    let mut rest = String::new();
    let mut cursor = 0;

    loop {
        let Some(start) = text[cursor..].find(start_tag) else {
            // No more blocks — append remaining text to rest.
            let remaining = text[cursor..].trim();
            if !remaining.is_empty() {
                if !rest.is_empty() {
                    rest.push('\n');
                }
                rest.push_str(remaining);
            }
            break;
        };
        let abs_start = cursor + start;

        let Some(end) = text[abs_start..].find(end_tag) else {
            // Malformed: opening tag without closing tag. Treat rest as non-user content.
            let remaining = text[cursor..].trim();
            if !remaining.is_empty() {
                if !rest.is_empty() {
                    rest.push('\n');
                }
                rest.push_str(remaining);
            }
            break;
        };
        let abs_end = abs_start + end + end_tag.len();

        // Text before this block → rest.
        let before = text[cursor..abs_start].trim();
        if !before.is_empty() {
            if !rest.is_empty() {
                rest.push('\n');
            }
            rest.push_str(before);
        }

        // The block itself → user_sections.
        user_sections.push(&text[abs_start..abs_end]);

        cursor = abs_end;
    }

    if user_sections.is_empty() {
        (None, text.to_string())
    } else {
        (Some(user_sections.join("\n")), rest)
    }
}

/// Truncate a string in the middle if it exceeds `max_chars`.
/// Returns `None` if no truncation is needed.
pub fn truncate_middle(msg: &str, max_chars: usize) -> Option<String> {
    let char_count = msg.chars().count();
    if char_count <= max_chars {
        return None;
    }
    let front_len = max_chars / 2;
    let back_len = max_chars - front_len; // handles odd max_chars
    let front: String = msg.chars().take(front_len).collect();
    let back: String = msg.chars().skip(char_count - back_len).collect();
    Some(format!("{}...[truncated]...{}", front, back))
}

/// Extract a `<echo_agent_user_queries>` XML block from `User` items in `turns`.
///
/// Walks `turns`, finds `User` items, and formats each as a `<echo_agent_query>`
/// element with text content (from [`CompactionItem::text`]) and any
/// `<echo_agent_file id="..." name="..." />` lines for the item's attachment
/// refs. Long user messages are truncated via [`truncate_middle`].
///
/// Returns `None` if no user items produced any non-empty content.
pub fn extract_user_queries_from_turns<T: CompactionItem>(
    turns: &[T],
    user_truncate_chars: u32,
) -> Option<String> {
    let threshold = user_truncate_chars as usize;
    let mut result = String::from("<echo_agent_user_queries>\n");
    let mut emitted_any = false;

    for turn in turns {
        if turn.role() != CompactionRole::User {
            continue;
        }

        let text = turn.text().unwrap_or_default();
        let attachments = turn.attachment_refs();

        // Skip user items that contribute neither text nor attachments.
        if text.is_empty() && attachments.is_empty() {
            continue;
        }
        emitted_any = true;

        result.push_str("<echo_agent_query>");
        match truncate_middle(&text, threshold) {
            Some(truncated) => {
                info!(
                    original_chars = text.chars().count(),
                    threshold = threshold,
                    "[Compaction] Truncated long user query"
                );
                result.push_str(&truncated);
            }
            None => result.push_str(&text),
        }
        if !attachments.is_empty() {
            result.push('\n');
            for att_ref in attachments {
                result.push_str(&format!(
                    "<echo_agent_file id=\"{}\" name=\"{}\" />\n",
                    att_ref.id, att_ref.name
                ));
            }
        }
        result.push_str("</echo_agent_query>\n");
    }

    if !emitted_any {
        return None;
    }
    result.push_str("</echo_agent_user_queries>");
    Some(result)
}

/// Walk `turns`, find any prior compaction summary items, extract their
/// `<echo_agent_user_queries>` blocks via [`split_prior_compaction_text`], and
/// concatenate them.
///
/// Returns `None` if no prior compaction items are present or none
/// contain a user-queries block.
///
/// Prefer [`separate_prior_user_queries`] when you also need the
/// compaction-stripped item list to feed to the LLM (i.e. both
/// inter-compaction and intra-compaction's `History` sampling) — it does
/// both jobs in one pass.
pub fn extract_prior_user_queries<T: CompactionItemBuilder>(turns: &[T]) -> Option<String> {
    separate_prior_user_queries(turns).prior_user_queries
}

/// Output of [`separate_prior_user_queries`].
#[derive(Debug, Clone)]
pub struct SeparatedHistoryTurns<T> {
    /// `turns` with the `<echo_agent_user_queries>` block stripped from every
    /// prior compaction summary item. Safe to feed to the compaction LLM —
    /// it will not re-emit the user-queries metadata.
    /// A prior compaction item whose `rest` is empty after stripping is
    /// dropped entirely.
    pub turns_for_llm: Vec<T>,
    /// Concatenation of every `<echo_agent_user_queries>` block found (in
    /// document order, joined by `\n`). `None` if no prior compaction
    /// item contained a user-queries block. Preserved verbatim so it
    /// can be passed to [`assemble_user_queries_preamble`].
    pub prior_user_queries: Option<String>,
    /// `true` if at least one prior compaction summary item was observed,
    /// regardless of whether it contained a `<echo_agent_user_queries>` block.
    /// Used by inter-compaction to record the
    /// `ConversationCompactionCount{status="recompaction"}` metric.
    pub has_prior_compaction: bool,
}

/// Walk `turns`, split every prior compaction summary item into (a) its
/// `<echo_agent_user_queries>` block (preserved verbatim for the next summary)
/// and (b) the rest of the summary content (rebuilt as a new summary item
/// and forwarded to the LLM). Non-compaction items are forwarded unchanged.
///
/// Shared by both compaction pipelines so inter and intra `History`
/// handle prior compactions identically:
///
/// - **inter** calls this on the filtered item list before its chunking
///   loop, so the LLM never sees `<echo_agent_user_queries>` from earlier rounds.
/// - **intra** calls this on `turns_to_compact` for the `History` target
///   before sampling, for the same reason. Without this stripping, the LLM
///   would see the prior `<echo_agent_user_queries>` and tend to copy it into the
///   new summary — which then chains with the explicit preamble we prepend,
///   snowballing across re-compactions.
pub fn separate_prior_user_queries<T: CompactionItemBuilder>(
    turns: &[T],
) -> SeparatedHistoryTurns<T> {
    let mut turns_for_llm: Vec<T> = Vec::with_capacity(turns.len());
    let mut prior_user_queries: Option<String> = None;
    let mut has_prior_compaction = false;

    for turn in turns {
        if turn.is_compaction_summary() {
            has_prior_compaction = true;
            let content = turn.text().unwrap_or_default();
            let (user_section, rest) = split_prior_compaction_text(&content);
            if let Some(user_sec) = user_section {
                match &mut prior_user_queries {
                    Some(existing) => {
                        existing.push('\n');
                        existing.push_str(&user_sec);
                    }
                    None => prior_user_queries = Some(user_sec),
                }
            }
            // Matches inter's previous inline behavior (`if !rest.is_empty()`):
            // a prior compaction item whose entire content was the
            // `<echo_agent_user_queries>` block (and therefore stripped to an empty
            // `rest`) contributes nothing for the LLM and is dropped here.
            if !rest.is_empty() {
                turns_for_llm.push(T::compaction_summary_item(rest));
            }
            continue;
        }
        turns_for_llm.push(turn.clone());
    }

    SeparatedHistoryTurns {
        turns_for_llm,
        prior_user_queries,
        has_prior_compaction,
    }
}

/// Assemble the final user-queries preamble that gets prepended to the
/// compaction summary: `prior\n\ncurrent\n\n`. Either side may be `None`;
/// when both are `None` an empty string is returned.
///
/// Used by both pipelines:
/// - inter passes `current = extract_original_user_messages(raw_request, …)`
/// - intra passes `current = extract_user_queries_from_turns(turns, …)`
///
/// `prior` is always [`separate_prior_user_queries`]`.prior_user_queries`.
pub fn assemble_user_queries_preamble(prior: Option<String>, current: Option<String>) -> String {
    let mut preamble = String::new();
    if let Some(p) = &prior {
        preamble.push_str(p);
        preamble.push_str("\n\n");
    }
    if let Some(c) = &current {
        preamble.push_str(c);
        preamble.push_str("\n\n");
    }
    preamble
}

/// Convenience wrapper around [`extract_prior_user_queries`] +
/// [`assemble_user_queries_preamble`].
///
/// Used by callers that don't separately need the
/// compaction-stripped item list (e.g. tests). Both production pipelines
/// instead call [`separate_prior_user_queries`] once and reuse both its
/// outputs (the stripped item list goes to the LLM, the prior queries
/// go to [`assemble_user_queries_preamble`]).
pub fn build_user_queries_preamble<T: CompactionItemBuilder>(
    turns: &[T],
    current_user_queries: Option<String>,
) -> String {
    assemble_user_queries_preamble(extract_prior_user_queries(turns), current_user_queries)
}

/// Wrap a single chunk's thinking text in a `<chunk_analysis index="i">…</chunk_analysis>` block.
///
/// Returns the empty string when `thinking` is empty after trimming so we don't
/// persist empty wrappers.
pub fn wrap_chunk_analysis(index: usize, thinking: &str) -> String {
    let trimmed = thinking.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    format!(
        "<chunk_analysis index=\"{}\">\n{}\n</chunk_analysis>\n\n",
        index, trimmed
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::CompactionFileRef;

    /// Pure mock item for the shared filter algorithms.
    #[derive(Debug, Clone, PartialEq)]
    enum MockItem {
        System,
        Developer { text: String, summary: bool },
        User { text: String },
        Assistant { text: Option<String>, tools: bool },
        Tool,
    }

    impl MockItem {
        fn user(text: &str) -> Self {
            Self::User {
                text: text.to_string(),
            }
        }
        fn summary(text: &str) -> Self {
            Self::Developer {
                text: text.to_string(),
                summary: true,
            }
        }
    }

    impl CompactionItem for MockItem {
        fn role(&self) -> CompactionRole {
            match self {
                Self::System => CompactionRole::System,
                Self::Developer { .. } => CompactionRole::Developer,
                Self::User { .. } => CompactionRole::User,
                Self::Assistant { .. } => CompactionRole::Assistant,
                Self::Tool => CompactionRole::Tool,
            }
        }
        fn text(&self) -> Option<String> {
            match self {
                Self::Developer { text, .. } | Self::User { text } => Some(text.clone()),
                Self::Assistant { text, .. } => text.clone(),
                _ => None,
            }
        }
        fn has_tool_requests(&self) -> bool {
            matches!(self, Self::Assistant { tools: true, .. })
        }
        fn is_compaction_summary(&self) -> bool {
            matches!(self, Self::Developer { summary: true, .. })
        }
        fn attachment_refs(&self) -> Vec<CompactionFileRef> {
            Vec::new()
        }
    }

    impl CompactionItemBuilder for MockItem {
        fn compaction_summary_item(text: String) -> Self {
            Self::Developer {
                text,
                summary: true,
            }
        }
        fn strip_tool_content(&self) -> Option<Self> {
            match self {
                Self::Assistant { text: Some(t), .. } if !t.is_empty() => Some(Self::Assistant {
                    text: Some(t.clone()),
                    tools: false,
                }),
                Self::Assistant { .. } => None,
                other => Some(other.clone()),
            }
        }
    }

    #[test]
    fn basic_filter_drops_system_and_plain_developer() {
        let items = vec![
            MockItem::System,
            MockItem::Developer {
                text: "agent prompt".into(),
                summary: false,
            },
            MockItem::summary("prior summary"),
            MockItem::user("hi"),
            MockItem::Tool,
        ];
        let kept = filter_turns_for_basic(&items);
        assert_eq!(
            kept,
            vec![
                MockItem::summary("prior summary"),
                MockItem::user("hi"),
                MockItem::Tool
            ]
        );
    }

    #[test]
    fn inter_filter_drops_tools_and_strips_assistant() {
        let items = vec![
            MockItem::Tool,
            MockItem::Assistant {
                text: Some("visible".into()),
                tools: true,
            },
            MockItem::Assistant {
                text: None,
                tools: true,
            },
            MockItem::user("q"),
        ];
        let kept = filter_turns_for_inter_compaction(&items);
        assert_eq!(
            kept,
            vec![
                MockItem::Assistant {
                    text: Some("visible".into()),
                    tools: false
                },
                MockItem::user("q"),
            ]
        );
    }

    #[test]
    fn extract_user_queries_returns_none_when_no_user_turns() {
        let turns = vec![MockItem::Assistant {
            text: Some("a".into()),
            tools: false,
        }];
        assert!(extract_user_queries_from_turns(&turns, 3_000).is_none());
    }

    #[test]
    fn extract_user_queries_wraps_single_user_turn() {
        let turns = vec![MockItem::user("hello world")];
        let out = extract_user_queries_from_turns(&turns, 3_000).expect("got block");
        assert!(out.starts_with("<echo_agent_user_queries>"));
        assert!(out.ends_with("</echo_agent_user_queries>"));
        assert!(out.contains("<echo_agent_query>hello world</echo_agent_query>"));
    }

    #[test]
    fn extract_user_queries_truncates_long_messages() {
        let long = "x".repeat(5_000);
        let turns = vec![MockItem::user(&long)];
        let out = extract_user_queries_from_turns(&turns, 100).expect("got block");
        assert!(out.contains("...[truncated]..."));
        assert!(!out.contains(&"x".repeat(5_000)));
    }

    #[test]
    fn extract_prior_user_queries_concatenates_blocks() {
        let inner = "<echo_agent_user_queries>\n<echo_agent_query>first</echo_agent_query>\n</echo_agent_user_queries>";
        let inner2 = "<echo_agent_user_queries>\n<echo_agent_query>second</echo_agent_query>\n</echo_agent_user_queries>";
        let turns = vec![MockItem::summary(inner), MockItem::summary(inner2)];
        let out = extract_prior_user_queries(&turns).expect("found prior");
        assert!(out.contains("first"));
        assert!(out.contains("second"));
    }

    #[test]
    fn extract_prior_user_queries_none_for_non_compaction_turns() {
        let turns = vec![MockItem::user("hi")];
        assert!(extract_prior_user_queries(&turns).is_none());
    }

    #[test]
    fn separate_strips_user_queries_from_summary_item() {
        let prior = "<echo_agent_user_queries>\n<echo_agent_query>Q1</echo_agent_query>\n</echo_agent_user_queries>\n\n<chunk_summary index=\"0\">S1</chunk_summary>";
        let turns = vec![MockItem::summary(prior), MockItem::user("Q2")];

        let sep = separate_prior_user_queries(&turns);

        assert!(sep.has_prior_compaction);
        let prior = sep.prior_user_queries.expect("prior queries extracted");
        assert!(prior.contains("Q1"));
        assert!(prior.contains("<echo_agent_user_queries>"));

        assert_eq!(sep.turns_for_llm.len(), 2);
        match &sep.turns_for_llm[0] {
            MockItem::Developer { text, summary } => {
                assert!(*summary);
                assert!(text.contains("<chunk_summary"));
                assert!(!text.contains("<echo_agent_user_queries>"));
                assert!(!text.contains("Q1"));
            }
            other => panic!("expected summary item, got {:?}", other),
        }
        assert!(matches!(&sep.turns_for_llm[1], MockItem::User { .. }));
    }

    #[test]
    fn separate_drops_summary_item_with_no_rest() {
        let only_queries = "<echo_agent_user_queries>\n<echo_agent_query>Q</echo_agent_query>\n</echo_agent_user_queries>";
        let turns = vec![MockItem::summary(only_queries), MockItem::user("hello")];

        let sep = separate_prior_user_queries(&turns);

        assert!(sep.has_prior_compaction);
        assert!(sep.prior_user_queries.unwrap().contains("Q"));
        assert_eq!(sep.turns_for_llm.len(), 1);
        assert!(matches!(&sep.turns_for_llm[0], MockItem::User { .. }));
    }

    #[test]
    fn separate_passes_through_when_no_prior_compaction() {
        let turns = vec![MockItem::user("hi"), MockItem::user("there")];
        let sep = separate_prior_user_queries(&turns);
        assert!(!sep.has_prior_compaction);
        assert!(sep.prior_user_queries.is_none());
        assert_eq!(sep.turns_for_llm.len(), 2);
    }

    #[test]
    fn separate_records_recompaction_flag_even_without_user_queries_block() {
        let no_block = "<chunk_summary index=\"0\">just a summary</chunk_summary>";
        let turns = vec![MockItem::summary(no_block)];

        let sep = separate_prior_user_queries(&turns);

        assert!(sep.has_prior_compaction);
        assert!(sep.prior_user_queries.is_none());
        assert_eq!(sep.turns_for_llm.len(), 1);
    }

    #[test]
    fn assemble_empty_when_both_none() {
        assert!(assemble_user_queries_preamble(None, None).is_empty());
    }

    #[test]
    fn assemble_prior_only() {
        let out = assemble_user_queries_preamble(Some("PRIOR".into()), None);
        assert_eq!(out, "PRIOR\n\n");
    }

    #[test]
    fn assemble_current_only() {
        let out = assemble_user_queries_preamble(None, Some("CURRENT".into()));
        assert_eq!(out, "CURRENT\n\n");
    }

    #[test]
    fn assemble_combines_prior_then_current() {
        let out = assemble_user_queries_preamble(Some("PRIOR".into()), Some("CURRENT".into()));
        assert_eq!(out, "PRIOR\n\nCURRENT\n\n");
    }

    #[test]
    fn test_wrap_chunk_analysis_empty_thinking() {
        assert_eq!(wrap_chunk_analysis(0, ""), "");
        assert_eq!(wrap_chunk_analysis(2, "   \n\t"), "");
    }

    #[test]
    fn test_wrap_chunk_analysis_non_empty_thinking() {
        let wrapped = wrap_chunk_analysis(3, "reasoned about X");
        assert_eq!(
            wrapped,
            "<chunk_analysis index=\"3\">\nreasoned about X\n</chunk_analysis>\n\n"
        );
    }

    #[test]
    fn test_wrap_chunk_analysis_trims() {
        let wrapped = wrap_chunk_analysis(0, "  reasoned about X  \n");
        assert_eq!(
            wrapped,
            "<chunk_analysis index=\"0\">\nreasoned about X\n</chunk_analysis>\n\n"
        );
    }
}
