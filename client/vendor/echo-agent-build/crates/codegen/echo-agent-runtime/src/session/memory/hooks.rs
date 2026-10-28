//! Session lifecycle hooks for the memory system.
//!
//! Provides `on_session_end()` which auto-saves a session summary to memory
//! when a session ends. This runs best-effort — failures are logged but
//! don't prevent shutdown.
//!
//! ## What is saved
//!
//! The fallback writes a **structured session summary** with
//! zero latency and no LLM call:
//! - message counts (user / assistant / tool results)
//! - the first few real user topics from the session (never synthetic prefixes)
//! - recent assistant outcomes, stripped of reasoning blocks
//! - session date
//!
//! The runtime also queues source-attributed extraction after completed turns
//! and checkpoints unfinished extraction at shutdown for the next live session.
//!
//! ## Reliability
//!
//! - **Minimum conversation gate:** Skip sessions with < 1 *real* user prompt
//!   or < 20 total query bytes (synthetic metadata-only prefixes and
//!   auto-continue markers are excluded).
//! - **`save_on_end` config gate:** Skipped when `[memory.session].save_on_end = false`.
//! - **SIGTERM:** Triggered via `SessionCommand::Shutdown` handler

use crate::sampling::ConversationItem;
use crate::session::memory::storage::{MemoryStorage, slugify};

/// Minimum number of *real* user prompts required to save a session summary.
///
/// "Real" excludes synthetic metadata prefixes and auto-continue sentinels —
/// see [`extract_real_user_queries`].
const MIN_USER_MESSAGES: usize = 1;

/// Minimum total byte length of all real user queries required to save.
///
/// Prevents trivial sessions (e.g. "hey" / "ok" / "thanks") from being indexed
/// even when they technically exceed [`MIN_USER_MESSAGES`].
///
/// Uses `str::len()` (byte length) rather than `chars().count()` — for the
/// mostly-ASCII inputs this gate targets, the distinction is immaterial.
const MIN_TOTAL_QUERY_BYTES: usize = 20;

/// Result of the session end hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEndResult {
    /// Session was too short (< [`MIN_USER_MESSAGES`] real user prompts or
    /// < [`MIN_TOTAL_QUERY_BYTES`] total bytes), or `save_on_end` was false.
    Skipped,
    /// Summary was written to the daily log.
    Written(String),
    /// Hook failed (logged, not fatal).
    Failed(String),
}

/// Real user queries iff the conversation meets the session-end size gate
/// (enough real prompts, enough total bytes). `None` for empty/brief sessions.
/// Independent of `save_on_end` so exit dream can still consolidate prior
/// logs when auto-save is off for a substantial session.
pub(crate) fn queries_meeting_session_end_threshold(
    conversation: &[ConversationItem],
) -> Option<Vec<String>> {
    // Real queries exclude synthetic metadata prefixes and `__auto_continue__`
    // sentinels (raw user-item counts inflate the gate).
    let real_queries =
        crate::session::helpers::session_compact::extract_real_user_queries(conversation);
    if real_queries.len() < MIN_USER_MESSAGES {
        tracing::debug!(
            real_count = real_queries.len(),
            min = MIN_USER_MESSAGES,
            "session too short for memory save/dream"
        );
        return None;
    }
    let total_bytes: usize = real_queries.iter().map(|q| q.len()).sum();
    if total_bytes < MIN_TOTAL_QUERY_BYTES {
        tracing::debug!(
            total_bytes,
            min = MIN_TOTAL_QUERY_BYTES,
            "session content too brief for memory save/dream"
        );
        return None;
    }
    Some(real_queries)
}

/// Run the session end hook — save a structured metadata summary to memory.
///
/// This is called from the `SessionCommand::Shutdown` handler and the
/// channel-closed path. It is best-effort: errors are logged but do not
/// prevent shutdown.
///
/// Generates a metadata summary with zero latency — **no LLM call is made**.
/// The summary includes message counts, real user topics, and session date.
///
/// For rich content capture (decisions, patterns, reasoning), use `/flush`.
///
/// Returns the path written (if any) for logging purposes.
pub fn on_session_end(
    storage: &MemoryStorage,
    conversation: &[ConversationItem],
    session_id: &str,
    save_on_end: bool,
) -> SessionEndResult {
    // Respect the user's config choice.  Callers that have `save_on_end = false`
    // should still call this function (to keep the call-site simple), trusting
    // that the gate is enforced here.
    if !save_on_end {
        tracing::debug!("session end: save_on_end=false, skipping memory summary");
        return SessionEndResult::Skipped;
    }

    let Some(real_queries) = queries_meeting_session_end_threshold(conversation) else {
        return SessionEndResult::Skipped;
    };

    // Slug from first *real* query (not the synthetic prefix User item).
    let first_real_query = real_queries.first().map(String::as_str).unwrap_or("");
    let slug = slugify(first_real_query, 30);
    let slug = if slug.is_empty() { "session" } else { &slug };

    // Generate a lightweight summary from conversation metadata (no LLM).
    let summary = generate_metadata_summary(conversation, &real_queries);

    // Write to daily session log.
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    match storage.write_daily_log(&date, slug, session_id, &summary, false) {
        Ok(path) => {
            tracing::info!(
                path = %path.display(),
                real_user_messages = real_queries.len(),
                "session end: wrote memory summary"
            );
            SessionEndResult::Written(path.display().to_string())
        }
        Err(e) => {
            tracing::warn!(error = %e, "session end: failed to write memory summary");
            SessionEndResult::Failed(e.to_string())
        }
    }
}

/// Generate a structured session summary from conversation metadata.
///
/// Uses `real_queries` (pre-computed via [`extract_real_user_queries`]) for
/// the user-message count and topics so that synthetic bootstrap messages and
/// auto-continue sentinels are never surfaced in the saved summary.
///
/// This does NOT call an LLM — it extracts structured information directly
/// from the conversation items: message counts, session date, and the first
/// few real user topics. For richer content capture use `/flush`.
pub(crate) fn generate_metadata_summary(
    conversation: &[ConversationItem],
    real_queries: &[String],
) -> String {
    let real_count = real_queries.len();

    let assistant_count = conversation
        .iter()
        .filter(|item| matches!(item, ConversationItem::Assistant(_)))
        .count();

    let tool_count = conversation
        .iter()
        .filter(|item| matches!(item, ConversationItem::ToolResult(_)))
        .count();

    // ── Assemble summary ─────────────────────────────────────────────────────
    let mut summary = String::new();
    summary.push_str("## Session Summary\n\n");
    summary.push_str(&format!(
        "- **Messages:** {} user, {} assistant, {} tool results\n",
        real_count, assistant_count, tool_count
    ));
    summary.push_str(&format!(
        "- **Date:** {}\n\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    ));

    // Topics — first few real queries (never the synthetic prefix text).
    // chars().take(100) avoids byte-boundary panics on multi-byte Unicode.
    let topics: Vec<String> = real_queries
        .iter()
        .take(5)
        .map(|q| q.chars().take(100).collect::<String>())
        .collect();

    if !topics.is_empty() {
        summary.push_str("## Topics Discussed\n\n");
        for (i, topic) in topics.iter().enumerate() {
            summary.push_str(&format!("{}. {}\n", i + 1, topic));
        }
        summary.push('\n');
    }

    // Retain the actual final outcome when a model extraction is interrupted.
    // These are attributed excerpts, never inferred facts or tool payloads.
    let outcomes: Vec<_> = conversation
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            if let ConversationItem::Assistant(assistant) = item {
                let text =
                    echo_agent_memory::text_utils::strip_reasoning_blocks(&assistant.content);
                let text = text.trim();
                if text.chars().count() >= 20 {
                    return Some(format!(
                        "### Assistant message {} (verify before reuse)\n\n{}\n",
                        index + 1,
                        text.chars().take(2000).collect::<String>()
                    ));
                }
            }
            None
        })
        .rev()
        .take(4)
        .collect();
    if !outcomes.is_empty() {
        summary.push_str("## Recorded Outcomes\n\n");
        for outcome in outcomes.into_iter().rev() {
            summary.push_str(&outcome);
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampling::conversation::{AssistantItem, ContentPart, ToolResultItem, UserItem};
    use tempfile::TempDir;

    fn make_user(text: &str) -> ConversationItem {
        ConversationItem::User(UserItem {
            content: vec![ContentPart::Text { text: text.into() }],
            synthetic_reason: None,
            ..Default::default()
        })
    }

    /// Build a realistic first-turn user message: metadata prefix + user query in tags.
    ///
    /// This matches what `construct_user_message` + `user_query()` produce.
    fn make_synthetic_prefix_with_query(query: &str) -> ConversationItem {
        make_user(&format!(
            "<user_info>\nOS Version: macos\nShell: /bin/bash\n</user_info>\n\
             <git_status>\n(no changes)\n</git_status>\n\
             <user_query>\n{query}\n</user_query>"
        ))
    }

    /// Build a metadata-only prefix (no <user_query> tag) — represents the
    /// synthetic bootstrap message on sessions that never received a real prompt.
    fn make_metadata_only() -> ConversationItem {
        make_user("<user_info>\nOS Version: macos\n</user_info>")
    }

    fn make_assistant(text: &str) -> ConversationItem {
        ConversationItem::Assistant(AssistantItem {
            content: text.into(),
            tool_calls: vec![],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        })
    }

    fn test_storage(tmp: &TempDir) -> MemoryStorage {
        let global = tmp.path().join("memory");
        let workspace = global.join("test_ws");
        MemoryStorage::with_paths(global, workspace)
    }

    // -----------------------------------------------------------------------
    // Existing behaviour tests (updated for new signatures / semantics)
    // -----------------------------------------------------------------------

    #[test]
    fn test_on_session_end_skips_short_sessions() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        // A greeting contains no substantial query — should skip.
        let conv = vec![make_user("hello"), make_assistant("hi")];
        let result = on_session_end(&storage, &conv, "test-session-id", true);
        assert_eq!(result, SessionEndResult::Skipped);
    }

    #[test]
    fn test_on_session_end_skips_brief_sessions() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        // 3 real messages but very short — total bytes < MIN_TOTAL_QUERY_BYTES (20).
        // "hi" (2) + "ok" (2) + "bye" (3) = 7 bytes
        let conv = vec![
            make_user("hi"),
            make_assistant("hello"),
            make_user("ok"),
            make_assistant("sure"),
            make_user("bye"),
            make_assistant("goodbye"),
        ];

        let result = on_session_end(&storage, &conv, "sess-brief", true);
        assert_eq!(
            result,
            SessionEndResult::Skipped,
            "sessions with brief content should be skipped even with enough messages"
        );
    }

    #[test]
    fn test_on_session_end_writes_summary() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        let conv = vec![
            make_user("help me fix the auth bug"),
            make_assistant("sure, let me look at auth.rs"),
            make_user("also check the tests"),
            make_assistant("found the issue"),
            make_user("great, can you fix the login page too"),
            make_assistant("on it"),
        ];

        let result = on_session_end(&storage, &conv, "sess12345678", true);
        assert!(
            matches!(result, SessionEndResult::Written(_)),
            "should write summary, got {result:?}"
        );

        // Verify file was created.
        let files = storage.list_memory_files().unwrap();
        let session_files: Vec<_> = files
            .iter()
            .filter(|f| {
                f.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("sess1234")
            })
            .collect();
        assert!(!session_files.is_empty(), "session log file should exist");
    }

    #[test]
    fn test_on_session_end_summary_has_structure() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        let conv = vec![
            make_user("implement feature X"),
            make_assistant("working on it"),
            make_user("also add tests for edge cases"),
            make_assistant("done"),
            make_user("make sure everything compiles cleanly"),
            make_assistant("verified"),
            ConversationItem::ToolResult(ToolResultItem {
                tool_call_id: "tc_1".to_string(),
                content: "file written".into(),
                images: Vec::new(),
            }),
        ];

        on_session_end(&storage, &conv, "sess12345678", true);

        let files = storage.list_memory_files().unwrap();
        let session_file = files
            .iter()
            .find(|f| {
                f.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("sess1234")
            })
            .unwrap();

        let content = std::fs::read_to_string(session_file).unwrap();
        assert!(content.contains("## Session Summary"));
        assert!(content.contains("3 user"));
        assert!(content.contains("## Topics Discussed"));
        assert!(content.contains("implement feature X"));
        assert!(content.contains("also add tests for edge cases"));
    }

    #[test]
    fn test_on_session_end_empty_conversation() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        let result = on_session_end(&storage, &[], "test-id", true);
        assert_eq!(result, SessionEndResult::Skipped);
    }

    #[test]
    fn test_generate_metadata_summary_format() {
        let conv = vec![
            make_user("first question"),
            make_assistant("answer"),
            make_user("second question"),
        ];
        let real_queries = vec!["first question".to_string(), "second question".to_string()];
        let summary = generate_metadata_summary(&conv, &real_queries);
        assert!(summary.contains("## Session Summary"));
        assert!(summary.contains("2 user"));
        assert!(summary.contains("1 assistant"));
        assert!(summary.contains("first question"));
    }

    // -----------------------------------------------------------------------
    // Real-user-query extraction tests
    // -----------------------------------------------------------------------

    /// `save_on_end = false` always skips — even for a long conversation.
    #[test]
    fn test_on_session_end_save_on_end_false_skips() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        let conv = vec![
            make_user("task one"),
            make_assistant("done"),
            make_user("task two"),
            make_assistant("done"),
        ];

        let result = on_session_end(&storage, &conv, "sess-disabled", false);
        assert_eq!(
            result,
            SessionEndResult::Skipped,
            "save_on_end=false must skip even with enough messages"
        );

        // No session log file should have been written (the MEMORY.md templates
        // created by ensure_initialized are expected and are not session logs).
        let files = storage.list_memory_files().unwrap();
        let session_logs: Vec<_> = files
            .iter()
            .filter(|f| f.components().any(|c| c.as_os_str() == "sessions"))
            .collect();
        assert!(
            session_logs.is_empty(),
            "no session log should be created when save_on_end=false"
        );
    }

    /// Threshold is independent of `save_on_end` so exit dream can still run.
    #[test]
    fn test_conversation_meets_session_end_threshold_ignores_save_config() {
        let short = vec![make_user("hi"), make_assistant("hey")];
        assert!(
            queries_meeting_session_end_threshold(&short).is_none(),
            "brief session must fail threshold"
        );

        let substantial = vec![
            make_user("help me fix the auth bug in login"),
            make_assistant("looking"),
            make_user("also check the integration tests please"),
            make_assistant("found it"),
            make_user("great, can you patch the login page too"),
            make_assistant("done"),
        ];
        assert!(
            queries_meeting_session_end_threshold(&substantial).is_some(),
            "substantial session must pass threshold even when save is off"
        );
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();
        assert_eq!(
            on_session_end(&storage, &substantial, "sess-no-save", false),
            SessionEndResult::Skipped,
            "save_on_end=false still skips write"
        );
    }

    /// Synthetic metadata-only prefix (no `<user_query>`) is excluded from the
    /// real-message count so it cannot push the session over the gate threshold.
    #[test]
    fn test_synthetic_prefix_alone_does_not_count_as_real_message() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        // The conversation has 2 User items, but the first is metadata-only
        // and the second is a brief greeting. Only its real query bytes count.
        let conv = vec![
            make_metadata_only(), // synthetic, no <user_query>
            make_assistant("hi"),
            make_user("hello"), // 1 brief real prompt
            make_assistant("sure"),
        ];

        let result = on_session_end(&storage, &conv, "sess-synth", true);
        assert_eq!(
            result,
            SessionEndResult::Skipped,
            "metadata-only prefix must not count toward the real-message gate"
        );
    }

    /// With a real synthetic prefix + two real queries the session IS written,
    /// and the slug is derived from the first real query (not the prefix text).
    #[test]
    fn test_slug_derived_from_real_query_not_prefix() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        // First User item: metadata prefix wrapping the real first query.
        // Second User item: a follow-up plain-text query.
        let conv = vec![
            make_synthetic_prefix_with_query("fix the login bug"),
            make_assistant("on it"),
            make_user("also add a test for it"),
            make_assistant("done"),
            make_user("and update the error messages"),
            make_assistant("updated"),
        ];

        let result = on_session_end(&storage, &conv, "sess-slug-check", true);
        assert!(
            matches!(result, SessionEndResult::Written(_)),
            "should write, got {result:?}"
        );

        let files = storage.list_memory_files().unwrap();
        // The file name slug should come from "fix the login bug", not from the
        // raw metadata prefix text.
        assert!(
            files
                .iter()
                .any(|f| f.to_str().unwrap().contains("fix-the-login-bug")),
            "slug should be derived from the first real query"
        );
    }

    /// Topics in the summary contain real query text, not the metadata prefix.
    #[test]
    fn test_topics_contain_real_queries_not_metadata() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        let conv = vec![
            make_synthetic_prefix_with_query("implement the auth feature"),
            make_assistant("working on it"),
            make_user("add integration tests too"),
            make_assistant("done"),
            make_user("check the error handling as well"),
            make_assistant("verified"),
        ];

        on_session_end(&storage, &conv, "sess-topics", true);

        let files = storage.list_memory_files().unwrap();
        let session_file = files
            .iter()
            .find(|f| f.to_str().unwrap().contains("implement-the-auth"))
            .expect("session file should exist with correct slug");

        let content = std::fs::read_to_string(session_file).unwrap();
        assert!(
            content.contains("implement the auth feature"),
            "topics must include the real first query"
        );
        assert!(
            content.contains("add integration tests too"),
            "topics must include the second real query"
        );
        // The raw metadata prefix text must NOT appear as a topic.
        assert!(
            !content.contains("<user_info>"),
            "metadata tag text must not appear in topics"
        );
        assert!(
            !content.contains("OS Version"),
            "metadata content must not appear in topics"
        );
    }

    /// The *actual* AUTO_CONTINUE_PROMPT text pushed into the conversation after
    /// auto-compaction must not be counted as a real user message, and must not
    /// appear in session-end topics.
    ///
    /// This is the key regression test for the correctness fix: we use the
    /// real stored text, not just the `"__auto_continue__"` request-id sentinel.
    #[test]
    fn test_actual_auto_continue_prompt_excluded() {
        use crate::session::helpers::session_compact::AUTO_CONTINUE_PROMPT;

        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        // Old sessions may contain AUTO_CONTINUE_PROMPT as a User item after
        // auto-compaction. Verify it's excluded from real user query counts.
        let conv = vec![
            make_synthetic_prefix_with_query("implement feature Z"),
            make_assistant("done"),
            // Simulated auto-compaction: AUTO_CONTINUE_PROMPT is pushed as a User item.
            make_user(AUTO_CONTINUE_PROMPT),
            make_assistant("continuing..."),
        ];

        // Only 1 real query ("implement feature Z") — should skip (< MIN_USER_MESSAGES).
        let result = on_session_end(&storage, &conv, "sess-autocompact", true);
        assert_eq!(
            result,
            SessionEndResult::Skipped,
            "AUTO_CONTINUE_PROMPT must not count as a real user message"
        );
    }

    /// Long Unicode user queries are truncated at a character boundary, never a byte boundary.
    #[test]
    fn test_topics_unicode_truncation_no_panic() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        // Build a query that is > 100 chars when byte-counted but the 100th byte
        // falls inside a multi-byte sequence (emoji are 4 bytes each).
        let emoji_query = "🦀".repeat(30); // 30 × 4 = 120 bytes, but 30 chars
        assert!(emoji_query.len() > 100, "precondition: > 100 bytes");

        let conv = vec![
            make_user(&emoji_query),
            make_assistant("done"),
            make_user("second question about the codebase"),
            make_assistant("ok"),
            make_user("third question about testing"),
            make_assistant("yes"),
        ];

        // Must not panic; the summary should be produced successfully.
        let result = on_session_end(&storage, &conv, "sess-unicode", true);
        assert!(
            matches!(result, SessionEndResult::Written(_)),
            "should write summary without panic on Unicode query, got {result:?}"
        );
    }

    /// `__auto_continue__` sentinels do not count as real user messages.
    #[test]
    fn test_auto_continue_sentinel_excluded_from_count() {
        let tmp = TempDir::new().unwrap();
        let storage = test_storage(&tmp);
        storage.ensure_initialized().unwrap();

        // Session has only 1 real human query; the other two User items are
        // auto-continue sentinels.
        let conv = vec![
            make_user("<user_query>\n__auto_continue__\n</user_query>"),
            make_assistant("continuing"),
            make_user("real human question"),
            make_assistant("answer"),
            make_user("<user_query>\n__auto_continue__\n</user_query>"),
            make_assistant("done"),
        ];

        let result = on_session_end(&storage, &conv, "sess-autocont", true);
        assert_eq!(
            result,
            SessionEndResult::Skipped,
            "auto-continue sentinels must not count toward the real-message gate"
        );
    }

    // -----------------------------------------------------------------------
    // Summary format tests
    // -----------------------------------------------------------------------

    /// Summary only contains Session Summary and Topics Discussed — no
    /// Tools Used or Files Touched sections (those are low-value noise).
    #[test]
    fn test_generate_metadata_summary_only_session_and_topics() {
        let conv = vec![
            make_user("hello"),
            make_assistant("hi there"),
            make_user("how are you"),
            make_assistant("great"),
        ];
        let queries = vec!["hello".to_string(), "how are you".to_string()];
        let summary = generate_metadata_summary(&conv, &queries);

        assert!(summary.contains("## Session Summary"));
        assert!(summary.contains("## Topics Discussed"));
        assert!(
            !summary.contains("## Tools Used"),
            "tools section must not appear"
        );
        assert!(
            !summary.contains("## Files Touched"),
            "files section must not appear"
        );
        assert!(
            !summary.contains("## Shell Commands"),
            "commands section must not appear"
        );
    }
}
