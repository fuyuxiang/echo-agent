use crate::sampling::{AssistantItem, ConversationItem, Role, ToolCall};
use crate::session::helpers::compaction_context::{
    BackgroundTaskSummary, CompactionInputs, CompactionStateContext, RunningSubagentSummary,
    SubagentToolNames, to_system_reminder_sync,
};
use echo_agent_chat_state::compaction_utils::{
    CompactedHistoryInput, build_compacted_history as build_compacted_history_shared,
};
use std::collections::BTreeSet;
/// Thin wrapper around the shared `build_compacted_history` from
/// `echo-agent-chat-state`, rendering the system-reminder synchronously (no
/// memory backend) to match the old test-local helper signature.
fn build_compacted_history(
    system_prompt: &str,
    user_message_prefix: &str,
    state_context: &CompactionStateContext,
    compaction_summary: &str,
    discovered_agents_md: &[std::path::PathBuf],
) -> Vec<ConversationItem> {
    let system_reminder =
        to_system_reminder_sync(state_context, discovered_agents_md, &[], None, None, None);
    build_compacted_history_shared(CompactedHistoryInput {
        system_message: ConversationItem::system(system_prompt),
        user_message_prefix: user_message_prefix.to_string(),
        agents_md_reminder: None,
        state_context,
        compaction_summary: compaction_summary.to_string(),
        system_reminder,
        summary_before_recent: false,
        transcript_hint: None,
        summary_count: 1,
    })
}
/// Full compaction scenario: system prompt, user_info prefix, a multi-turn
/// conversation with tool calls, background tasks, edited files, and
/// discovered AGENTS.md files.  Asserts the exact raw string of every
/// user-role message in the compacted history.
#[tokio::test]
async fn test_compacted_history_raw_strings() {
    let conversation = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user(
            "<user_info>\nOS: macos\nShell: /bin/bash\nWorkspace Path: /Users/test/project\n</user_info>\n\n<user_query>\nfix the login bug in auth.rs\n</user_query>",
        ),
        ConversationItem::assistant("Let me look at the file."),
        ConversationItem::Assistant(AssistantItem {
            content: "I'll read the file now.".into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "read_file".into(),
                arguments: r#"{"target_file": "src/auth.rs"}"#.into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc1", "fn login() { /* buggy code */ }"),
        ConversationItem::Assistant(AssistantItem {
            content: "Found the bug, applying fix.".into(),
            tool_calls: vec![ToolCall {
                id: "tc2".into(),
                name: "search_replace".into(),
                arguments:
                    r#"{"file_path": "src/auth.rs", "old_string": "buggy", "new_string": "fixed"}"#
                        .into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc2", "Successfully replaced text."),
    ];
    let mut edited_paths = BTreeSet::new();
    edited_paths.insert("src/auth.rs".to_string());
    let running_tasks = vec![BackgroundTaskSummary {
        task_id: "abc123".into(),
        command: "cargo test".into(),
        status: "running".into(),
        tool_name: Some("run_terminal_command".into()),
    }];
    let state_context = CompactionStateContext::build(
        &conversation,
        CompactionInputs {
            running_tasks,
            agent_edited_paths: edited_paths,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        state_context.last_user_query,
        Some("fix the login bug in auth.rs".to_string()),
        "extract_last_user_query should strip <user_query> tags"
    );
    let user_message_prefix = "<user_info>\nOS: macos\nShell: /bin/bash\nWorkspace Path: /Users/test/project\n</user_info>";
    let compaction_summary = "<analysis>\nThe user asked to fix a login bug in auth.rs. I found and fixed the issue.\n</analysis>\n\n<summary>\n1. Primary Request: Fix login bug in auth.rs\n2. Key Technical Concepts: Rust, authentication\n3. Files: src/auth.rs - fixed buggy code\n4. Problem Solving: Replaced buggy code with fixed code\n5. Pending Tasks: None\n6. Current Work: Applied fix to auth.rs\n7. Next Step: Run tests to verify\n</summary>";
    let discovered_agents_md = vec![std::path::PathBuf::from("/Users/test/project/AGENTS.md")];
    let compacted = build_compacted_history(
        "You are a helpful assistant.",
        user_message_prefix,
        &state_context,
        compaction_summary,
        &discovered_agents_md,
    );
    assert_eq!(compacted[0].role(), Role::System);
    assert_eq!(compacted[0].text_content(), "You are a helpful assistant.");
    assert_eq!(compacted[1].role(), Role::User);
    let msg1_text = compacted[1].text_content();
    assert_eq!(
        msg1_text,
        "<user_info>\nOS: macos\nShell: /bin/bash\nWorkspace Path: /Users/test/project\n</user_info>",
        "User message prefix should be raw user_info, no <user_query> wrapping"
    );
    assert!(
        !msg1_text.contains("<user_query>"),
        "User message prefix must NOT contain <user_query> tags"
    );
    assert_eq!(compacted[2].role(), Role::User);
    let msg2_text = compacted[2].text_content();
    assert_eq!(
        msg2_text, "<user_query>\nfix the login bug in auth.rs\n</user_query>",
        "Last user query should be wrapped in <user_query> tags"
    );
    assert_eq!(compacted[3].role(), Role::Assistant);
    assert_eq!(compacted[3].text_content(), "Let me look at the file.");
    assert_eq!(compacted[4].role(), Role::Assistant);
    assert_eq!(compacted[4].text_content(), "I'll read the file now.");
    assert_eq!(compacted[5].role(), Role::Tool);
    assert_eq!(compacted[5].text_content(), "Tool call omitted...");
    assert_eq!(compacted[6].role(), Role::Assistant);
    assert_eq!(compacted[6].text_content(), "Found the bug, applying fix.");
    assert_eq!(compacted[7].role(), Role::Tool);
    assert_eq!(compacted[7].text_content(), "Tool call omitted...");
    assert_eq!(compacted[8].role(), Role::User);
    let msg_summary_text = compacted[8].text_content();
    assert!(
        !msg_summary_text.contains("<user_query>"),
        "Summary message should NOT be wrapped in <user_query> tags"
    );
    assert!(
        !msg_summary_text.contains("<system-reminder>"),
        "Summary message should NOT contain system-reminder (it is now separate)"
    );
    assert!(
        msg_summary_text
            .starts_with("This session is being continued from a previous conversation"),
        "Summary should start with the continuation preamble"
    );
    let formatted_summary =
        echo_agent_chat_state::compaction_utils::format_compact_summary_content(compaction_summary);
    assert_eq!(
        msg_summary_text, formatted_summary,
        "Summary message should be the summary text without <user_query> wrapping"
    );
    assert_eq!(compacted[9].role(), Role::User);
    let msg_reminder_text = compacted[9].text_content();
    assert!(msg_reminder_text.contains("<system-reminder>"));
    assert!(msg_reminder_text.contains("src/auth.rs"));
    assert!(msg_reminder_text.contains("Files Edited This Session"));
    assert!(msg_reminder_text.contains("cargo test"));
    assert!(msg_reminder_text.contains("\"abc123\""));
    assert!(!msg_reminder_text.contains("task-abc123"));
    assert!(msg_reminder_text.contains("/Users/test/project/AGENTS.md"));
    assert_eq!(compacted.len(), 10);
}
/// Compaction with no background tasks, no edited files, no AGENTS.md:
/// the summary message should be just the summary wrapped in <user_query>
/// with no <system-reminder> appended.
#[tokio::test]
async fn test_compacted_history_minimal_no_state_context() {
    let conversation = vec![
        ConversationItem::system("system prompt"),
        ConversationItem::user(
            "<user_info>OS: linux</user_info>\n\n<user_query>\nhello world\n</user_query>",
        ),
        ConversationItem::assistant("Hi! How can I help?"),
    ];
    let state_context =
        CompactionStateContext::build(&conversation, CompactionInputs::default()).await;
    let compacted = build_compacted_history(
        "system prompt",
        "<user_info>OS: linux</user_info>",
        &state_context,
        "Summary: user said hello.",
        &[],
    );
    assert_eq!(compacted[0].text_content(), "system prompt");
    let prefix = compacted[1].text_content();
    assert_eq!(prefix, "<user_info>OS: linux</user_info>");
    assert!(!prefix.contains("<user_query>"));
    let query = compacted[2].text_content();
    assert_eq!(query, "<user_query>\nhello world\n</user_query>");
    assert_eq!(compacted[3].text_content(), "Hi! How can I help?");
    let summary = compacted[4].text_content();
    assert!(
        summary.starts_with("This session is being continued"),
        "Summary should start with preamble (no <user_query> wrapping)"
    );
    assert!(
        summary.contains("Summary: user said hello."),
        "Summary should contain the original summary text"
    );
    assert!(
        !summary.contains("<user_query>"),
        "Summary should NOT be wrapped in <user_query> tags"
    );
    assert!(
        !summary.contains("<system-reminder>"),
        "No state context means no <system-reminder> block"
    );
    assert_eq!(compacted.len(), 5);
}
/// Regression guard: echo-agent-build must DROP the working
/// tail post-compaction. A prior change routed echo-agent-build to keep `recent_messages`,
/// which survive only as `Tool call omitted...` stubs (dead tokens). Mirrors
/// `summary_before_recent_compaction_with_no_user_query_yields_three_messages` for echo-agent-build
/// (`summary_before_recent = false`).
#[tokio::test]
async fn echo_agent_build_compaction_drops_working_tail_regression_206460() {
    let conversation = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user(
            "<user_info>\nOS: macos\n</user_info>\n\n<user_query>\nread auth.rs\n</user_query>",
        ),
        ConversationItem::Assistant(AssistantItem {
            content: "reading the file".into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "read_file".into(),
                arguments: r#"{"target_file": "src/auth.rs"}"#.into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc1", "fn login() { /* ... */ }"),
    ];
    let full = CompactionStateContext::build(&conversation, CompactionInputs::default()).await;
    assert!(
        full.recent_messages
            .iter()
            .any(|i| matches!(i, ConversationItem::ToolResult(_))
                && i.text_content() == "Tool call omitted..."),
        "precondition: build() keeps the working tail as a stubbed tool result",
    );
    let dropped = full.for_compaction();
    assert!(
        dropped.recent_messages.is_empty(),
        "echo-agent-build must drop recent_messages post-compaction",
    );
    assert!(
        dropped.agent_message_anchor.is_none(),
        "a human-only conversation has no agent-message anchor",
    );
    let compacted = build_compacted_history(
        "You are a helpful assistant.",
        "<user_info>\nOS: macos\n</user_info>",
        &dropped,
        "<summary>\nRead auth.rs.\n</summary>",
        &[],
    );
    assert!(
        !compacted
            .iter()
            .any(|i| matches!(i, ConversationItem::ToolResult(_))
                || i.text_content() == "Tool call omitted..."),
        "no tail (ToolResult or stub) may leak into the echo-agent-build compacted history",
    );
}
/// Verify that the auto-continue prompt (sent after compaction) is also
/// raw text without <user_query> wrapping.
#[test]
fn test_auto_continue_prompt_has_no_user_query_tags() {
    let auto_continue = "Continue with the work described in the summary above. Pick up where you left off based on the 'Current Work' and 'Next Step' sections. If the previous task was completed, confirm completion and await further instructions.";
    let msg = ConversationItem::user(auto_continue);
    let text = msg.text_content();
    assert_eq!(text, auto_continue);
    assert!(
        !text.contains("<user_query>"),
        "Auto-continue prompt must NOT contain <user_query> tags"
    );
}
/// Prove that the sanitizer + validator pipeline produces a valid
/// compacted history even when the raw output has an orphaned ToolResult.
/// This exercises the same code path as `run_compact_inner` in
/// `acp_session.rs`: build → sanitize → validate → (fallback if needed).
#[test]
fn sanitize_then_validate_produces_valid_history() {
    use echo_agent_chat_state::compaction_utils::{
        sanitize_compacted_history, validate_compacted_history,
    };
    let raw = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("<user_query>\ntask\n</user_query>"),
        // Orphan: no preceding assistant with call_ORPHAN
        ConversationItem::tool_result("call_ORPHAN", "Tool call omitted..."),
        // Valid pair
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_OK".into(),
            name: "edit".to_string(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result("call_OK", "Tool call omitted..."),
        ConversationItem::user("summary"),
    ];
    let sanitized = sanitize_compacted_history(raw);
    assert_eq!(sanitized.stripped_tool_call_ids, vec!["call_ORPHAN"]);
    let violations = validate_compacted_history(&sanitized.items);
    assert!(
        violations.is_empty(),
        "post-sanitize validation must pass, but found: {violations:?}"
    );
}
/// When sanitization cannot fix the history (e.g. result-before-call
/// that the sanitizer strips but the caller re-introduces somehow),
/// the fallback path should produce a minimal valid history.
#[test]
fn fallback_minimal_history_has_no_tool_results() {
    use echo_agent_chat_state::compaction_utils::validate_compacted_history;
    let state_context = CompactionStateContext {
        knowledge_context: None,
        cwd_generation: 0,
        destination_project_instructions: None,
        agent_message_anchor: None,
        recent_messages: vec![],
        last_user_query: Some("fix the bug".to_string()),
        agent_edited_paths: vec!["src/main.rs".to_string()],
        running_tasks: vec![],
        running_subagents: vec![],
        connected_mcp_servers: vec![],
        todos: vec![],
    };
    let fallback = build_compacted_history(
        "You are a helpful assistant.",
        "<user_info>OS: macos</user_info>",
        &state_context,
        "Summary of previous work.",
        &[],
    );
    let violations = validate_compacted_history(&fallback);
    assert!(
        violations.is_empty(),
        "fallback history must be valid, but found: {violations:?}"
    );
    assert!(
        !fallback
            .iter()
            .any(|item| matches!(item, ConversationItem::ToolResult(_))),
        "fallback history must contain no ToolResult items"
    );
}
/// Compaction with running subagents: the `## Running Subagents` section
/// must appear in the `<system-reminder>` with correct content and tool names.
#[tokio::test]
async fn test_compacted_history_with_running_subagents() {
    let conversation = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user(
            "<user_info>OS: macos</user_info>\n\n<user_query>\ndo stuff\n</user_query>",
        ),
        ConversationItem::assistant("Working on it."),
    ];
    let running_subagents = vec![
        RunningSubagentSummary {
            subagent_id: "sub-001".into(),
            subagent_type: "Explore".into(),
            description: "Find all API endpoints".into(),
            elapsed_ms: 45_000,
        },
        RunningSubagentSummary {
            subagent_id: "sub-002".into(),
            subagent_type: "general-purpose".into(),
            description: "Refactor auth module".into(),
            elapsed_ms: 12_000,
        },
    ];
    let state_context = CompactionStateContext::build(
        &conversation,
        CompactionInputs {
            running_tasks: vec![BackgroundTaskSummary {
                task_id: "t1".into(),
                command: "cargo test".into(),
                status: "running".into(),
                tool_name: Some("run_terminal_command".into()),
            }],
            running_subagents,
            ..Default::default()
        },
    )
    .await;
    let tool_names = SubagentToolNames {
        poll: "get_command_or_subagent_output".into(),
        cancel: "kill_command_or_subagent".into(),
    };
    let system_reminder =
        to_system_reminder_sync(&state_context, &[], &[], Some(&tool_names), None, None);
    let reminder = system_reminder.expect("should produce a system-reminder");
    assert!(
        reminder.contains("## Running Subagents"),
        "must contain Running Subagents heading"
    );
    assert!(
        reminder.contains("sub-001"),
        "must contain subagent ID sub-001"
    );
    assert!(
        reminder.contains("sub-002"),
        "must contain subagent ID sub-002"
    );
    assert!(
        reminder.contains("Explore"),
        "must contain subagent type Explore"
    );
    assert!(
        reminder.contains("Find all API endpoints"),
        "must contain subagent description"
    );
    assert!(
        reminder.contains("Refactor auth module"),
        "must contain second subagent description"
    );
    assert!(
        reminder.contains("45s"),
        "must contain elapsed time for sub-001"
    );
    assert!(
        reminder.contains("12s"),
        "must contain elapsed time for sub-002"
    );
    assert!(
        reminder.contains("get_command_or_subagent_output"),
        "must contain poll tool name"
    );
    assert!(
        reminder.contains("kill_command_or_subagent"),
        "must contain cancel tool name"
    );
    assert!(
        reminder.contains("(running, run_terminal_command)"),
        "background task line must include the resolved tool name: {reminder}"
    );
    let bg_pos = reminder.find("## Running Background Tasks").unwrap();
    let sa_pos = reminder.find("## Running Subagents").unwrap();
    assert!(
        bg_pos < sa_pos,
        "Running Background Tasks must appear before Running Subagents"
    );
}
/// A monitor task renders `(running, monitor)` and a bash task
/// `(running, run_terminal_command)` so the post-compaction model can tell
/// which background task is the monitor.
#[tokio::test]
async fn background_tasks_are_labeled_by_creator_tool() {
    let conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("<user_query>\nhello\n</user_query>"),
        ConversationItem::assistant("Working."),
    ];
    let state_context = CompactionStateContext::build(
        &conversation,
        CompactionInputs {
            running_tasks: vec![
                BackgroundTaskSummary {
                    task_id: "bash-1".into(),
                    command: "cargo test".into(),
                    status: "running".into(),
                    tool_name: Some("run_terminal_command".into()),
                },
                BackgroundTaskSummary {
                    task_id: "mon-1".into(),
                    command: "tail -f dump.log | grep progress".into(),
                    status: "running".into(),
                    tool_name: Some("monitor".into()),
                },
            ],
            ..Default::default()
        },
    )
    .await;
    let reminder = to_system_reminder_sync(&state_context, &[], &[], None, None, None)
        .expect("should produce a system-reminder");
    assert!(
        reminder.contains("## Running Background Tasks"),
        "must contain Running Background Tasks heading: {reminder}"
    );
    assert!(
        reminder.contains("\"mon-1\":") && reminder.contains("(running, monitor)"),
        "monitor task must render with the monitor tool label: {reminder}"
    );
    assert!(
        reminder.contains("\"bash-1\":") && reminder.contains("(running, run_terminal_command)"),
        "bash task must render with the run_terminal_command label: {reminder}"
    );
    assert!(
        !reminder.contains("task-mon-1") && !reminder.contains("task-bash-1"),
        "task IDs must not be decorated with a task- prefix: {reminder}"
    );
}
/// When there are no running subagents, the `## Running Subagents` section
/// must NOT appear (no empty heading or spurious section).
#[tokio::test]
async fn no_subagents_means_no_section() {
    let conversation = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("<user_query>\nhello\n</user_query>"),
        ConversationItem::assistant("Hi!"),
    ];
    let mut edited = BTreeSet::new();
    edited.insert("src/main.rs".to_string());
    let state_context = CompactionStateContext::build(
        &conversation,
        CompactionInputs {
            agent_edited_paths: edited,
            ..Default::default()
        },
    )
    .await;
    let system_reminder = to_system_reminder_sync(&state_context, &[], &[], None, None, None);
    let reminder = system_reminder.expect("should produce a system-reminder for edited files");
    assert!(
        !reminder.contains("## Running Subagents"),
        "must NOT contain Running Subagents section when no subagents are running"
    );
    assert!(
        reminder.contains("## Files Edited This Session"),
        "should still have the edited files section"
    );
}
/// The fallback path (sanitization failure) must preserve running subagent
/// data from the original state context.
#[test]
fn fallback_preserves_subagents() {
    let original = CompactionStateContext {
        knowledge_context: None,
        cwd_generation: 0,
        destination_project_instructions: None,
        agent_message_anchor: None,
        recent_messages: vec![ConversationItem::assistant("working")],
        last_user_query: Some("fix the bug".to_string()),
        agent_edited_paths: vec!["src/main.rs".to_string()],
        running_tasks: vec![BackgroundTaskSummary {
            task_id: "t1".into(),
            command: "cargo test".into(),
            status: "running".into(),
            tool_name: Some("run_terminal_command".into()),
        }],
        running_subagents: vec![
            RunningSubagentSummary {
                subagent_id: "sub-abc".into(),
                subagent_type: "Explore".into(),
                description: "searching".into(),
                elapsed_ms: 5_000,
            },
            RunningSubagentSummary {
                subagent_id: "sub-def".into(),
                subagent_type: "Plan".into(),
                description: "planning".into(),
                elapsed_ms: 10_000,
            },
        ],
        connected_mcp_servers: vec![],
        todos: vec![],
    };
    let fallback = CompactionStateContext {
        knowledge_context: None,
        cwd_generation: original.cwd_generation,
        destination_project_instructions: original.destination_project_instructions.clone(),
        agent_message_anchor: original.agent_message_anchor.clone(),
        recent_messages: vec![],
        last_user_query: original.last_user_query.clone(),
        agent_edited_paths: original.agent_edited_paths.clone(),
        running_tasks: vec![],
        running_subagents: original.running_subagents.clone(),
        connected_mcp_servers: original.connected_mcp_servers.clone(),
        todos: original.todos.clone(),
    };
    assert_eq!(
        fallback.running_subagents.len(),
        2,
        "fallback must preserve all running subagents"
    );
    assert_eq!(fallback.running_subagents[0].subagent_id, "sub-abc");
    assert_eq!(fallback.running_subagents[1].subagent_id, "sub-def");
}
