/// Truncation threshold, matching the shell's large-prompt limit.
pub const LARGE_PROMPT_THRESHOLD: usize = 25_000;

pub const INTERJECTION_NOTE: &str = "The user sent a message while you were working:";
pub const INTERRUPT_NOTE: &str = "The user interrupted the previous turn:";

/// Trailing reminder so a mid-turn steer or post-cancel follow-up does not drop in-flight work.
const UNFINISHED_TASKS_REMINDER: &str =
    "Make sure to complete any unfinished tasks from previous turns.";

/// Wrap a user message in the canonical `<user_query>` envelope.
pub fn user_query(user_message: &str) -> String {
    format!(
        r#"<user_query>
{user_message}
</user_query>"#
    )
}

/// Prefix `note` and the unfinished-task trailer around an already-assembled
/// user turn (a `<user_query>` block, optionally with skill/context tails).
pub fn frame_user_turn(note: &str, assembled: &str) -> String {
    format!("{note}\n{assembled}\n{UNFINISHED_TASKS_REMINDER}")
}

fn format_steered_query(note: &str, text: String) -> String {
    let truncated = if text.len() <= LARGE_PROMPT_THRESHOLD {
        text
    } else {
        let end = text
            .char_indices()
            .take_while(|(i, _)| *i < LARGE_PROMPT_THRESHOLD)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(text.len());
        format!("{}... [truncated]", &text[..end])
    };
    frame_user_turn(note, &user_query(&truncated))
}

/// Wrap interjection text as a synthetic user message with a mid-turn note
/// and a reminder to finish in-flight work from earlier turns.
pub fn format_interjection(text: String) -> String {
    format_steered_query(INTERJECTION_NOTE, text)
}

/// Same envelope as [`format_interjection`], for the next real user query
/// after a text-only mid-turn abort.
pub fn format_interrupt(text: String) -> String {
    format_steered_query(INTERRUPT_NOTE, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_in_user_query_with_midturn_note() {
        let out = format_interjection("stop and fix the test first".into());
        assert!(out.starts_with(&format!("{INTERJECTION_NOTE}\n<user_query>\n")));
        assert!(out.contains("stop and fix the test first"));
        assert!(
            out.ends_with(&format!("</user_query>\n{UNFINISHED_TASKS_REMINDER}")),
            "unfinished-task reminder must follow the wrapped query, got: {out}"
        );
    }

    #[test]
    fn interrupt_shares_envelope_with_different_note() {
        let out = format_interrupt("do the other thing".into());
        assert_eq!(
            out,
            format!(
                "{INTERRUPT_NOTE}\n<user_query>\ndo the other thing\n</user_query>\n{UNFINISHED_TASKS_REMINDER}"
            )
        );
    }

    #[test]
    fn truncates_at_utf8_boundary() {
        let s = "é".repeat(LARGE_PROMPT_THRESHOLD);
        let out = format_interjection(s);
        assert!(out.contains("... [truncated]"));
        assert!(out.len() < LARGE_PROMPT_THRESHOLD + 200);
    }

    #[test]
    fn short_text_untouched() {
        let out = format_interjection("hi".into());
        assert!(!out.contains("[truncated]"));
        assert_eq!(
            out,
            format!(
                "{INTERJECTION_NOTE}\n<user_query>\nhi\n</user_query>\n{UNFINISHED_TASKS_REMINDER}"
            )
        );
    }
}
