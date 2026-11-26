//! Summary output cleaning and carrier formatting.
//!
//! Moved verbatim from `echo-agent-chat-state`'s `compaction_utils`. Covers:
//!
//! - cleaning the compaction model's raw output ([`format_compact_summary`]),
//! - the echo-agent-build continuation carrier ([`format_compact_summary_content`]),
//! - the canonical `<user_query>` wrapping ([`wrap_user_query`]).

/// Clean the compaction model's raw output into the plain-text `Summary:`
/// block that seeds the next turn.
///
/// Drafting scratchpad (a top-level `<analysis>` block, or a nested
/// `<analysis>`/`<summary>` wrapper / untagged markdown "**Analysis**" header
/// inside the summary) is stripped; control tokens echoed *within* the body
/// (the model sometimes quotes its own instruction under section 6) are
/// neutralized so they can't prime the next turn to re-emit a `<summary>`
/// block. A summary that already leads with a numbered section is preserved
/// verbatim even when it quotes `</analysis>`/`<summary>` in a later section.
pub fn format_compact_summary(summary: &str) -> String {
    let mut result = summary.to_string();

    // 1. Remove leading <analysis>…</analysis> drafting block(s). A block is
    //    only stripped when it is a genuinely LEADING scratchpad: top-level
    //    (before any <summary>) or immediately after the <summary> open modulo
    //    whitespace (nested). An <analysis> quoted mid-body — after real
    //    sections, e.g. a section-6 instruction echo — is NOT leading and is
    //    left for step 3 to neutralize, so neither a balanced body quote
    //    spanning sections nor an unclosed one ever deletes real content. The
    //    loop peels successive leading blocks should the model emit more than
    //    one.
    while let Some(start) = result.find("<analysis>") {
        let is_leading = match result.find("<summary>") {
            Some(sp) => start < sp || result[sp + "<summary>".len()..start].trim().is_empty(),
            None => result[..start].trim().is_empty(),
        };
        if !is_leading {
            break;
        }
        match result[start..].find("</analysis>") {
            Some(rel) => {
                let end = start + rel + "</analysis>".len();
                result = format!("{}{}", &result[..start], &result[end..]);
            }
            None => {
                // Unclosed leading <analysis>: drop up to the next <summary>
                // (preserving a summary that follows) or to the end (truncation).
                let drop_to = result[start..]
                    .find("<summary>")
                    .map_or(result.len(), |rel| start + rel);
                result = format!("{}{}", &result[..start], &result[drop_to..]);
                break;
            }
        }
    }

    // 2. Convert the outer <summary>…</summary> to "Summary:\n{inner}", keeping
    //    any text outside the wrapper. `rfind` matches the outer close, so a
    //    literal "</summary>" echoed in the body does not truncate the summary;
    //    `end > start` guards a malformed "</summary> … <summary>" order. Leading
    //    scratchpad inside the block is peeled (see `strip_leading_scratchpad`);
    //    a body echo that quotes the instruction is left for step 3 to defuse.
    if let Some(start) = result.find("<summary>")
        && let Some(end) = result.rfind("</summary>")
        && end > start
    {
        let before = result[..start].to_string();
        let after = result[end + "</summary>".len()..].to_string();
        let inner = strip_leading_scratchpad(result[start + "<summary>".len()..end].trim());
        result = format!("{before}Summary:\n{inner}{after}");
    }

    // 3. Defuse any compaction-control tokens still echoed inside the body so the
    //    seed can't prime the next turn to re-emit a <summary> block.
    result = neutralize_compaction_control_tokens(&result);

    // Collapse excessive blank lines (3+ newlines → 2)
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result.trim().to_string()
}

/// Peel leading drafting scratchpad off an extracted `<summary>` block.
///
/// A markdown "**Analysis**"-style header has no opening `<analysis>` tag for
/// step 1 to catch; it ends at an orphan `</analysis>`. Everything up to and
/// including the *last* `</analysis>` is dropped, so a scratchpad that itself
/// quotes `</analysis>` mid-reasoning is still removed whole. The peel is
/// skipped when the block already starts with a numbered section — including a
/// markdown-decorated one like `## 1.` or `**1.**` — so a `</analysis>` merely
/// echoed inside a real section never truncates the summary. Any leftover
/// leading `<summary>` wrapper is then unwrapped.
fn strip_leading_scratchpad(inner: &str) -> String {
    let mut s = inner.trim();
    let lead = s.trim_start_matches(['#', '*', '-', '>', ' ', '\t']);
    if !lead.starts_with(|c: char| c.is_ascii_digit())
        && let Some(pos) = s.rfind("</analysis>")
    {
        s = s[pos + "</analysis>".len()..].trim_start();
    }
    if let Some(rest) = s.strip_prefix("<summary>") {
        s = rest.trim_start();
    }
    s.to_string()
}

/// Defuse compaction-control tokens echoed inside a summary body by inserting
/// a zero-width space after `<`, so they can't be read as live tags by the next
/// turn. Closers first so the inserted sentinel never re-matches.
fn neutralize_compaction_control_tokens(text: &str) -> String {
    text.replace("</summary>", "<\u{200b}/summary>")
        .replace("<summary>", "<\u{200b}summary>")
        .replace("</analysis>", "<\u{200b}/analysis>")
        .replace("<analysis>", "<\u{200b}analysis>")
        .replace("</summary_request>", "<\u{200b}/summary_request>")
        .replace("<summary_request>", "<\u{200b}summary_request>")
}

/// True when the cleaned summary seed is too small to plausibly carry the
/// task state of the conversation it would replace. Callers should
/// retry like a transient failure.
pub fn is_degenerate_summary(raw_summary: &str) -> bool {
    format_compact_summary(raw_summary).chars().count() < super::config::MIN_SUMMARY_SEED_CHARS
}

/// Clean tags via [`format_compact_summary`] and prepend the continuation
/// preamble. This is the user message content that replaces the compacted
/// conversation.
pub fn format_compact_summary_content(raw_summary: &str) -> String {
    let cleaned = format_compact_summary(raw_summary);
    format!(
        "This session is being continued from a previous conversation that ran out of context. \
         The summary below covers the earlier portion of the conversation.\n\n{cleaned}"
    )
}

/// Wrap text in `<user_query>...</user_query>` tags.
///
/// This is the canonical wrapping used for user messages that contain
/// a query or compaction summary. Centralised here so all harnesses
/// share the same format.
pub fn wrap_user_query(text: impl Into<String>) -> String {
    let text = text.into();
    format!("<user_query>\n{text}\n</user_query>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degenerate_summary_below_min_seed_chars() {
        let raw = "<summary>\n1. Primary Request: q\n</summary>";
        assert!(is_degenerate_summary(raw));
        let long = format!(
            "<summary>\n1. Primary Request: q\n{}\n</summary>",
            "y".repeat(500)
        );
        assert!(!is_degenerate_summary(&long));
    }

    #[test]
    fn strips_analysis_keeps_summary() {
        let input = "<analysis>\nThinking about the problem...\n</analysis>\n\n<summary>\n1. Primary Request: Fix the bug\n</summary>";
        let result = format_compact_summary(input);
        assert!(!result.contains("Thinking about the problem"));
        assert!(result.contains("Summary:\n1. Primary Request: Fix the bug"));
        assert!(!result.contains("<analysis>"));
        assert!(!result.contains("<summary>"));
    }

    #[test]
    fn no_tags_passthrough() {
        assert_eq!(
            format_compact_summary("Just plain text summary."),
            "Just plain text summary."
        );
    }

    #[test]
    fn only_summary_becomes_heading() {
        let result = format_compact_summary("<summary>\n1. Request: Do something\n</summary>");
        assert_eq!(result, "Summary:\n1. Request: Do something");
    }

    #[test]
    fn collapses_blank_lines() {
        let input = "<analysis>\nThought\n</analysis>\n\n\n\n<summary>\nResult\n</summary>";
        assert!(!format_compact_summary(input).contains("\n\n\n"));
    }

    #[test]
    fn unclosed_analysis_strips_remainder() {
        assert_eq!(
            format_compact_summary("<analysis>\nPartial reasoning about the task..."),
            ""
        );
    }

    #[test]
    fn keeps_sections_on_section6_instruction_echo() {
        // The model echoes the summarization instruction under section 6,
        // which would otherwise seed the next turn to re-emit a stray block.
        let raw = "<summary>\n1. Primary Request and Intent: build app\n2. Key Technical Concepts: webgl\n6. All user messages: 'respond with ONLY the <summary> block.'\n9. Optional Next Step: rerun\n</summary>";
        let result = format_compact_summary(raw);
        for needle in [
            "1. Primary Request",
            "2. Key Technical Concepts",
            "9. Optional Next Step",
        ] {
            assert!(result.contains(needle), "dropped {needle:?}: {result:?}");
        }
        assert!(!result.contains("<summary>"), "live <summary>: {result:?}");
        assert!(
            !result.contains("</summary>"),
            "live </summary>: {result:?}"
        );
    }

    #[test]
    fn unclosed_summary_open_preserves_body() {
        let input = "<summary>\n1. Primary Request: do the thing\n9. Optional Next Step: continue";
        let result = format_compact_summary(input);
        assert!(result.contains("1. Primary Request: do the thing"));
        assert!(result.contains("9. Optional Next Step: continue"));
        assert!(!result.contains("<summary>"));
    }

    #[test]
    fn multibyte_adjacent_to_tags_no_panic() {
        let raw =
            "<summary>1. Primary Request: ship 🚀 to 北京\n9. Optional Next Step: 完成</summary>";
        let result = format_compact_summary(raw);
        assert!(result.starts_with("Summary:\n1. Primary Request: ship 🚀 to 北京"));
        assert!(result.contains("9. Optional Next Step: 完成"));
    }

    #[test]
    fn malformed_tag_order_does_not_panic() {
        let result = format_compact_summary("intro </summary> middle <summary> tail");
        assert!(!result.contains("<summary>"));
        assert!(!result.contains("</summary>"));
        assert!(result.contains("intro"));
        assert!(result.contains("tail"));
    }

    #[test]
    fn content_adds_preamble_and_cleans() {
        let result = format_compact_summary_content(
            "<analysis>\nThinking\n</analysis>\n\n<summary>\n1. Fix bug\n</summary>",
        );
        assert!(result.starts_with("This session is being continued"));
        assert!(result.contains("Summary:\n1. Fix bug"));
        assert!(!result.contains("Thinking"));
        assert!(!result.contains("<summary>"));
    }

    #[test]
    fn wrap_user_query_wraps_text() {
        assert_eq!(
            wrap_user_query("hello world"),
            "<user_query>\nhello world\n</user_query>"
        );
    }
}
