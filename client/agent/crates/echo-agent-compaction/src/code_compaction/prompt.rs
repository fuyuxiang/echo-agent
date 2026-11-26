//! echo-agent-build's session-level summarization prompt.
//!
//! Split out of the crate-root `prompt` module so echo-agent-build's full-replace
//! prompt lives alongside the rest of its [`code_compaction`](crate::code_compaction)
//! subsystem. The EchoAgent chat's step-level intra prompt
//! ([`format_compaction_prompt`](crate::prompt::format_compaction_prompt))
//! stays at the crate root.

/// Build echo-agent-build's session-level summarization prompt (no chat history).
///
/// `user_context` is the optional `/compact <text>` user-provided context,
/// spliced inline into the structured prompt. Ported verbatim from
/// `echo-agent-runtime::session::helpers::session_compact::build_compaction_prompt`
/// (the `use_short_prompt == false` branch).
pub fn build_summary_prompt(user_context: Option<&str>) -> String {
    let user_context_section = match user_context {
        Some(context) => format!(
            "\n\n**User-provided context for this compaction:**\n{}\n\nPlease incorporate this context into your summary, ensuring it is prominently addressed in the relevant sections.\n\n",
            context
        ),
        None => String::new(),
    };

    include_str!("templates/full_replace_summary_prompt.txt")
        .replace("{user_context_section}", &user_context_section)
}

/// The short "self-summarization" prompt variant
/// (mirrors `echo-agent-runtime`'s `SELF_SUMMARIZATION_PROMPT`). Framed
/// as "summarize for a successor assistant that only sees the user's original
/// query plus this summary." Kept here so every harness (the shell and the
/// harness crate) shares one definition instead of each carrying a
/// private copy.
pub const SELF_SUMMARIZATION_PROMPT: &str = r#"<summary_request>
Please summarize the conversation so far. This summary (everything after your
thinking) will be provided to another AI assistant to continue working on the
task. The other assistant will only see the user's original query and your
summary, it will not have access to any tool calls or tool outputs from this
conversation. The purpose of the summary is to compress the conversation
context while preserving the essential information needed to seamlessly
continue. Useful things to include: the user's requests, what you've done so
far, relevant file paths and code details, any errors encountered and how
they were resolved, and what remains to be done. DO NOT call any tools in
your response.
</summary_request>"#;

/// Which summarization prompt a full-replace pass should send.
///
/// The prompt is owned by the harness's [`CompactionSampler`] impl (it appends
/// the prompt as the final user message before sampling), not by the shared
/// orchestrator. This enum lets each harness select the right one in one place
/// so the structured (echo-agent-build) and short self-summary prompts stay
/// shared instead of duplicated per harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SummaryPromptKind {
    /// echo-agent-build's detailed, numbered-section summary prompt.
    #[default]
    Structured,
    /// The short self-summarization prompt.
    SelfSummary,
}

/// Build the full-replace summarization prompt for the given [`SummaryPromptKind`].
///
/// `user_context` is the optional `/compact <text>` user-provided context.
/// For [`SummaryPromptKind::Structured`] it is spliced inline (see
/// [`build_summary_prompt`]); for [`SummaryPromptKind::SelfSummary`] it is
/// appended as a sibling `<user_provided_context>` block, matching the shell's
/// `build_compaction_prompt(use_short_prompt = true)` behavior.
pub fn build_summary_prompt_kind(kind: SummaryPromptKind, user_context: Option<&str>) -> String {
    match kind {
        SummaryPromptKind::Structured => build_summary_prompt(user_context),
        SummaryPromptKind::SelfSummary => match user_context {
            Some(ctx) => format!(
                "{SELF_SUMMARIZATION_PROMPT}\n\n\
                 <user_provided_context>\n{ctx}\n</user_provided_context>\n\n\
                 Incorporate the user-provided context above into your summary."
            ),
            None => SELF_SUMMARIZATION_PROMPT.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_prompt_splices_context_section_inline() {
        let p = build_summary_prompt(Some("focus on auth"));
        assert!(p.contains("**User-provided context for this compaction:**\nfocus on auth"));
        assert!(p.contains("1. Primary Request and Intent"));
        assert!(p.contains("9. Optional Next Step"));
    }

    #[test]
    fn summary_prompt_without_context_has_no_context_header() {
        let p = build_summary_prompt(None);
        assert!(!p.contains("**User-provided context for this compaction:**"));
        assert!(p.contains("6. All User Messages"));
        // Current prompt: no separate analysis block, concise framing.
        assert!(p.contains("do NOT emit a separate analysis block"));
        assert!(p.contains("faithful, concise summary"));
    }

    #[test]
    fn kind_structured_matches_build_summary_prompt() {
        // The Structured kind must be byte-identical to the legacy entry point
        // so routing through the selector never changes echo-agent-build's prompt.
        assert_eq!(
            build_summary_prompt_kind(SummaryPromptKind::Structured, None),
            build_summary_prompt(None)
        );
        assert_eq!(
            build_summary_prompt_kind(SummaryPromptKind::Structured, Some("focus on auth")),
            build_summary_prompt(Some("focus on auth"))
        );
    }

    #[test]
    fn kind_self_summary_without_context_is_bare_prompt() {
        let p = build_summary_prompt_kind(SummaryPromptKind::SelfSummary, None);
        assert_eq!(p, SELF_SUMMARIZATION_PROMPT);
        assert!(p.contains("<summary_request>"));
        // Must NOT carry the structured prompt's numbered sections.
        assert!(!p.contains("1. Primary Request and Intent"));
    }

    #[test]
    fn kind_self_summary_with_context_appends_sibling_block() {
        let p = build_summary_prompt_kind(SummaryPromptKind::SelfSummary, Some("focus on auth"));
        assert!(p.starts_with(SELF_SUMMARIZATION_PROMPT));
        assert!(p.contains("<user_provided_context>\nfocus on auth\n</user_provided_context>"));
        assert!(p.contains("Incorporate the user-provided context above"));
    }

    #[test]
    fn default_kind_is_structured() {
        assert_eq!(SummaryPromptKind::default(), SummaryPromptKind::Structured);
    }
}
