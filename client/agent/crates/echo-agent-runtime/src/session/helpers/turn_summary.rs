//! Per-turn dashboard summary helpers.
//!
//! After each turn the shell generates an ultra-short one-line summary of the
//! agent's reply for that turn (not a meta activity log), shown as the
//! dashboard row's secondary line. Like recap, it is display-only and never
//! mutates the conversation; the request shape (conversation prefix verbatim +
//! one instruction turn) is shared with recap via
//! [`super::session_recap::budget_instruction_items`].

use crate::sampling::ConversationItem;
use crate::session::helpers::chat::floor_char_boundary;

/// Hard cap on the summary (characters). The instruction targets 5–12 words;
/// this only guards against runaway output. Rows truncate to width on render.
pub(crate) const TURN_SUMMARY_MAX_CHARS: usize = 200;

/// Max characters of the user message quoted in the instruction as the
/// last-turn anchor.
const ANCHOR_MAX_CHARS: usize = 120;

/// First ~[`ANCHOR_MAX_CHARS`] of the last *real* user message
/// (`synthetic_reason.is_none()`), whitespace-collapsed.
///
/// The conversation contains user-role turns the user never wrote (reminders,
/// injected context); quoting the real message in the instruction is how the
/// model learns where "the last turn" starts. Angle brackets are dropped so
/// the quote cannot close the instruction's reminder tag. `None` when no real
/// user message with text exists (caller should skip generation).
pub(crate) fn last_user_anchor(conversation: &[ConversationItem]) -> Option<String> {
    let text = conversation.iter().rev().find_map(|item| match item {
        ConversationItem::User(u) if u.synthetic_reason.is_none() => {
            let text = item.text_content();
            (!text.trim().is_empty()).then_some(text)
        }
        _ => None,
    })?;
    let mut anchor: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| *c != '<' && *c != '>')
        .collect();
    if anchor.len() > ANCHOR_MAX_CHARS {
        let cut = floor_char_boundary(&anchor, ANCHOR_MAX_CHARS);
        anchor.truncate(cut);
        anchor = anchor.trim_end().to_string();
        anchor.push('\u{2026}');
    }
    Some(anchor)
}

/// Build the instruction turn appended to the conversation snapshot.
///
/// Same single-user-message design as recap (`recap_instruction`): all
/// directions live in one reminder-wrapped turn so the conversation prefix is
/// reused verbatim and the prompt cache stays warm. Few-shots must stay
/// synthetic — never embed real eval/session content.
pub(crate) fn turn_summary_instruction(tag: &str, anchor: &str) -> String {
    format!(
        "<{tag}>Write an ultra-short dashboard line that captures the AGENT'S REPLY for the \
         last turn only — everything after the user message beginning: \"{anchor}\". \
         Focus on what the assistant concluded, answered, recommended, or delivered — not a \
         meta description of the turn (avoid \"Explained…\", \"Answered…\", \"Greeted…\", \
         \"Reviewed…\"). User-role messages wrapped in reminder tags like this one are \
         injected context, not the user.\n\n\
         Output ONLY the fragment: 5-12 words, plain text, glanceable on a status row. \
         Prefer the payload: answer, finding, change, or decision needed. \
         Do NOT call any tools — respond with plain text only.\n\n\
         Synthetic examples (style only — adapt to THIS turn, do not copy):\n\
         `queue_worker` shutdown race fixed; suite green\n\
         Payment retries: exp backoff in `billing/retry.rs`, 5× on 429\n\
         Retry backoff wired into `billing/retry.rs`; tests pending\n\
         Need decision: keep or drop `sqlx` cache before refactor\n\
         Black — matches the terminal aesthetic\n\n\
         Bad (never):\n\
         - Lead with Explained / Answered / Greeted / Reviewed / Confirmed / Flagged / Summarized\n\
         - Labels, quotes, bullets, markdown, code fences, multi-sentence dumps\n\
         - Filler like \"no code changes\" or \"awaiting task\" unless that is the whole point\n\
         - Summarize earlier turns or the whole session\n\
         - Call tools or invent content not in the agent's reply</{tag}>"
    )
}

/// Clean the model's raw output into a one-line fragment: recap normalization
/// (whitespace collapse, stray label/quote stripping) plus the tighter
/// [`TURN_SUMMARY_MAX_CHARS`] cap.
pub(crate) fn clean_turn_summary_text(raw: &str) -> String {
    let mut out = super::session_recap::clean_recap_text(raw);
    if out.len() > TURN_SUMMARY_MAX_CHARS {
        let cut = floor_char_boundary(&out, TURN_SUMMARY_MAX_CHARS);
        out.truncate(cut);
        out = out.trim_end().to_string();
        out.push('\u{2026}');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ConversationItem {
        ConversationItem::user(text.to_string())
    }

    fn synthetic_user(text: &str) -> ConversationItem {
        use echo_agent_sampling_types::{ContentPart, SyntheticReason, UserItem};
        ConversationItem::User(UserItem {
            content: vec![ContentPart::Text {
                text: std::sync::Arc::from(text),
            }],
            synthetic_reason: Some(SyntheticReason::SystemReminder),
            ..Default::default()
        })
    }

    #[test]
    fn anchor_skips_synthetic_user_turns() {
        let conv = vec![
            ConversationItem::system("sys".to_string()),
            user("fix the parser"),
            ConversationItem::assistant("done".to_string()),
            synthetic_user("<system-reminder>injected</system-reminder>"),
        ];
        assert_eq!(last_user_anchor(&conv).as_deref(), Some("fix the parser"));
    }

    #[test]
    fn anchor_none_without_real_user_message() {
        let conv = vec![
            ConversationItem::system("sys".to_string()),
            synthetic_user("injected"),
        ];
        assert_eq!(last_user_anchor(&conv), None);
        assert_eq!(last_user_anchor(&[user("   \n ")]), None);
    }

    #[test]
    fn anchor_collapses_drops_angle_brackets_and_truncates() {
        let long = format!("review <the>   plan\n{}", "x".repeat(200));
        let anchor = last_user_anchor(&[user(&long)]).unwrap();
        assert!(anchor.starts_with("review the plan"));
        assert!(!anchor.contains('<') && !anchor.contains('>'));
        assert!(anchor.ends_with('\u{2026}'));
        assert!(anchor.chars().count() <= ANCHOR_MAX_CHARS + 1);
    }

    #[test]
    fn instruction_embeds_tag_and_anchor() {
        let text = turn_summary_instruction("system-reminder", "fix the parser");
        assert!(text.starts_with("<system-reminder>"));
        assert!(text.ends_with("</system-reminder>"));
        assert!(text.contains("beginning: \"fix the parser\""));
        // Reply-substance framing (not activity-log meta verbs as the task).
        assert!(text.contains("AGENT'S REPLY"));
        assert!(text.contains("avoid \"Explained…\""));
        assert!(!text.contains("verb-first past tense"));
    }

    #[test]
    fn clean_normalizes_and_caps() {
        assert_eq!(
            clean_turn_summary_text("Summary: \"Fixed the\n\n  parser\""),
            "Fixed the parser"
        );
        let capped = clean_turn_summary_text(&"word ".repeat(100));
        assert!(capped.len() <= TURN_SUMMARY_MAX_CHARS + '\u{2026}'.len_utf8());
        assert!(capped.ends_with('\u{2026}'));
    }
}
