//! Compacted-history assembly (echo-agent-build's rebuild structure, generic).
//!
//! Moved from `echo-agent-chat-state::compaction_utils::build_compacted_history` and
//! made generic over a write-side item factory so any harness can assemble
//! the canonical post-compaction history:
//!
//! ```text
//! [SP, UP', AGENTS_MD?, UQ_last?, recent…, summary, reminder?]
//! ```
//!
//! echo-agent-build is the canonical harness. The summary carrier text is built by
//! [`super::summary::format_compact_summary_content`].

use crate::item::CompactionItemFactory;

use super::summary::{format_compact_summary_content, wrap_user_query};

/// Input data for building a compacted conversation history.
///
/// All fields are plain data — no I/O, no network, no shell dependencies.
/// The caller is responsible for:
/// - Generating the `compaction_summary` via the LLM.
/// - Rendering the optional `system_reminder` (which may depend on
///   harness-specific backends such as memory search).
/// - Providing the `user_message_prefix` (e.g. `<user_info>` block).
/// - Extracting `last_user_query` / `recent_messages` from its own state.
pub struct CompactedHistoryParts<T> {
    /// The original system message from the conversation.
    pub system_message: T,
    /// The user-info / project-layout prefix (not wrapped in `<user_query>`).
    pub user_message_prefix: String,
    /// Pre-rendered AGENTS.md `<system-reminder>` block to re-inject after the
    /// user prefix. `None` means no project instructions to re-inject.
    pub agents_md_reminder: Option<String>,
    /// The last real user query text (raw, unwrapped).
    pub last_user_query: Option<String>,
    /// Messages retained verbatim from after the last real user turn.
    pub recent_messages: Vec<T>,
    /// The LLM-generated compaction summary text.
    pub compaction_summary: String,
    /// An optional pre-rendered `<system-reminder>` block to append after the
    /// summary. `None` means no state reminder is appended.
    pub system_reminder: Option<String>,
    /// Pre-built transcript hint appended to the summary (`None` to omit).
    pub transcript_hint: Option<String>,
}

/// Build the compacted conversation history from pure data inputs.
///
/// The returned `Vec<T>` is structured as:
///
/// 1. **System message** -- the original system prompt.
/// 2. **User message prefix** -- e.g. `<user_info>` block (no `<user_query>` tags).
/// 3. **AGENTS.md reminder** (if any) -- project instructions re-injected verbatim.
/// 4. **Last user query** (if any) -- wrapped in `<user_query>` tags.
/// 5. **Recent messages** (if any) -- retained verbatim from after the last
///    real user turn.
/// 6. **Compaction summary** -- with the optional `<system-reminder>`
///    appended as a separate message.
///
/// This is a pure function with no I/O.
pub fn assemble_compacted_history<T: CompactionItemFactory>(
    parts: CompactedHistoryParts<T>,
) -> Vec<T> {
    let mut compacted: Vec<T> = vec![
        parts.system_message,
        T::new_user_meta(parts.user_message_prefix),
    ];

    // Re-inject AGENTS.md as a user message so project instructions survive
    // compaction verbatim (not dependent on the summarizer). The
    // `ProjectInstructions` tag is what the spawn-time idempotence guard
    // recognizes on resume, so post-compaction sessions stay duplicate-free.
    if let Some(ref reminder) = parts.agents_md_reminder {
        compacted.push(T::new_project_instructions(reminder.clone()));
    }

    // Last user query wrapped in <user_query> tags for consistency.
    if let Some(ref last_query) = parts.last_user_query {
        compacted.push(T::new_user(wrap_user_query(last_query.as_str())));
    }

    // echo-agent-build keeps the legacy `<user_query>`-wrapped continuation text and
    // appends the transcript hint after the continuation summary.
    let mut formatted_summary = format_compact_summary_content(&parts.compaction_summary);
    if let Some(ref hint) = parts.transcript_hint {
        formatted_summary.push_str(hint);
    }
    let summary_item = T::new_user_meta(formatted_summary);

    // Recent messages come first, then the summary.
    for msg in parts.recent_messages {
        compacted.push(msg);
    }
    compacted.push(summary_item);

    if let Some(ref reminder) = parts.system_reminder {
        compacted.push(T::new_system_reminder(reminder.clone()));
    }

    compacted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal mock item recording which factory constructor produced it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum MockItem {
        System(String),
        User(String),
        UserMeta(String),
        ProjectInstructions(String),
        SystemReminder(String),
        Recent(String),
    }

    impl CompactionItemFactory for MockItem {
        fn new_user(text: String) -> Self {
            Self::User(text)
        }
        fn new_user_meta(text: String) -> Self {
            Self::UserMeta(text)
        }
        fn new_project_instructions(text: String) -> Self {
            Self::ProjectInstructions(text)
        }
        fn new_system_reminder(text: String) -> Self {
            Self::SystemReminder(text)
        }
    }

    fn parts(recent: Vec<MockItem>) -> CompactedHistoryParts<MockItem> {
        CompactedHistoryParts {
            system_message: MockItem::System("sys".into()),
            user_message_prefix: "<user_info>OS: macos</user_info>".into(),
            agents_md_reminder: Some("AGENTS.md content".into()),
            last_user_query: Some("fix the bug".into()),
            recent_messages: recent,
            compaction_summary: "Summary: did things.".into(),
            system_reminder: Some("<system-reminder>state</system-reminder>".into()),
            transcript_hint: None,
        }
    }

    #[test]
    fn echo_agent_build_order_recent_before_summary() {
        let recent = vec![MockItem::Recent("a1".into()), MockItem::Recent("t1".into())];
        let out = assemble_compacted_history(parts(recent));
        // [sys, prefix, agents_md, query, a1, t1, summary, reminder]
        assert_eq!(out.len(), 8);
        assert_eq!(out[0], MockItem::System("sys".into()));
        assert_eq!(
            out[1],
            MockItem::UserMeta("<user_info>OS: macos</user_info>".into())
        );
        assert_eq!(
            out[2],
            MockItem::ProjectInstructions("AGENTS.md content".into())
        );
        assert_eq!(
            out[3],
            MockItem::User("<user_query>\nfix the bug\n</user_query>".into())
        );
        assert_eq!(out[4], MockItem::Recent("a1".into()));
        assert_eq!(out[5], MockItem::Recent("t1".into()));
        let MockItem::UserMeta(summary) = &out[6] else {
            panic!("expected UserMeta summary, got {:?}", out[6]);
        };
        assert!(summary.starts_with("This session is being continued"));
        assert_eq!(
            out[7],
            MockItem::SystemReminder("<system-reminder>state</system-reminder>".into())
        );
    }

    #[test]
    fn omits_optional_sections() {
        let mut p = parts(vec![]);
        p.agents_md_reminder = None;
        p.last_user_query = None;
        p.system_reminder = None;
        let out = assemble_compacted_history(p);
        // [sys, prefix, summary]
        assert_eq!(out.len(), 3);
        assert!(
            matches!(&out[2], MockItem::UserMeta(s) if s.starts_with("This session is being continued"))
        );
    }

    #[test]
    fn appends_transcript_hint_after_summary() {
        let mut p = parts(vec![]);
        p.transcript_hint = Some("\n\n<transcript_location>/x</transcript_location>".into());
        let out = assemble_compacted_history(p);
        let MockItem::UserMeta(summary) = &out[4] else {
            panic!("expected UserMeta summary, got {:?}", out[4]);
        };
        assert!(summary.ends_with("</transcript_location>"));
    }
}
