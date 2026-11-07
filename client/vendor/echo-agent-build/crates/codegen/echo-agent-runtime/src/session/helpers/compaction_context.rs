//! Rendering helpers for [`CompactionStateContext`] that depend on
//! shell-specific types (`echo_agent_tools::MemoryBackend`, memory context).
//!
//! The core [`CompactionStateContext`] struct and its builder live in
//! `echo_agent_chat_state::compaction_utils`. This module adds system-reminder
//! rendering that requires dependencies not available in `echo-agent-chat-state`.
//!
//! The three **common** active-agent sections (background tasks, TODO list,
//! running subagents) are formatted by
//! [`echo_agent_compaction::reminder`] so echo-agent-chat and echo-agent-build stay in lockstep.
//! Harness-only sections (edited files, AGENTS.md, skills, MCP, memory) stay here.

use std::path::PathBuf;

pub use echo_agent_chat_state::compaction_utils::{
    BackgroundTaskSummary, CompactionInputs, CompactionServerSummary, CompactionStateContext,
    RunningSubagentSummary, TodoSummary, TodoSummaryStatus, extract_last_user_query,
    extract_messages_since_last_user, extract_user_query,
};
use echo_agent_compaction::reminder::{
    self, ActiveAgentReminderState, BackgroundTask, RunningSubagent, TodoItem, TodoStatus,
};

/// Resolved model-facing tool names for the MCP usage hint in compaction
/// reminders.
///
/// Resolved at runtime via `TemplateRenderer` from `ToolKind::SearchTool`
/// and `ToolKind::UseTool`. Never hard-code tool names -- they can be
/// renamed by the client.
pub struct McpToolNames {
    /// Model-facing name of the search/discover tool (e.g. "search_tool").
    pub search: String,
    /// Model-facing name of the dispatch/call tool (e.g. "use_tool").
    pub call: String,
}

/// Resolved model-facing tool names for the subagent reminder section.
///
/// Both names are resolved at runtime via `TemplateRenderer` from
/// `ToolKind::BackgroundTaskAction` and `ToolKind::KillTaskAction`.
/// Never hard-code tool names — they can be renamed by the client.
pub struct SubagentToolNames {
    /// Model-facing name of the poll/status tool (e.g. "get_task_output").
    pub poll: String,
    /// Model-facing name of the cancel/kill tool (e.g. "kill_task").
    pub cancel: String,
}

/// Format state info as system reminder, without memory search.
///
/// Use this from sync contexts (e.g., `build_compacted_history`) where
/// memory re-injection is handled separately by the session actor.
pub fn to_system_reminder_sync(
    ctx: &CompactionStateContext,
    discovered_agents_md: &[PathBuf],
    skills: &[echo_agent_tools::implementations::skills::types::SkillInfo],
    subagent_tool_names: Option<&SubagentToolNames>,
    mcp_tool_names: Option<&McpToolNames>,
    workflow_listing: Option<&str>,
) -> Option<String> {
    to_system_reminder_inner(
        ctx,
        discovered_agents_md,
        skills,
        &[],
        subagent_tool_names,
        mcp_tool_names,
        workflow_listing,
    )
}

/// Format state info as system reminder for injection into chat.
///
/// When a `memory_backend` is provided, searches memory for relevant
/// context from past sessions (post-compaction recovery).
pub async fn to_system_reminder(
    ctx: &CompactionStateContext,
    discovered_agents_md: &[PathBuf],
    skills: &[echo_agent_tools::implementations::skills::types::SkillInfo],
    memory_backend: Option<&dyn echo_agent_tools::types::memory_backend::MemoryBackend>,
    subagent_tool_names: Option<&SubagentToolNames>,
    mcp_tool_names: Option<&McpToolNames>,
    workflow_listing: Option<&str>,
) -> Option<String> {
    // Fetch memory results first (async), then pass to sync inner method
    let mut memory_results = Vec::new();
    if let Some(memory) = memory_backend {
        let query = ctx.last_user_query.as_deref().unwrap_or("project context");
        if let Ok(results) = memory.search(query, 3, 0.0).await {
            tracing::debug!(
                target: echo_agent_telemetry::memory_log::TARGET,
                results = results.len(),
                "recovered memory context after compaction"
            );
            memory_results = results;
        }
    }

    to_system_reminder_inner(
        ctx,
        discovered_agents_md,
        skills,
        &memory_results,
        subagent_tool_names,
        mcp_tool_names,
        workflow_listing,
    )
}

/// Shared implementation for both sync and async variants.
fn to_system_reminder_inner(
    ctx: &CompactionStateContext,
    discovered_agents_md: &[PathBuf],
    skills: &[echo_agent_tools::implementations::skills::types::SkillInfo],
    memory_results: &[echo_agent_tools::types::memory_backend::MemorySearchResult],
    subagent_tool_names: Option<&SubagentToolNames>,
    mcp_tool_names: Option<&McpToolNames>,
    workflow_listing: Option<&str>,
) -> Option<String> {
    let mut sections = Vec::new();

    // Agent-edited files (shell-only)
    if !ctx.agent_edited_paths.is_empty() {
        let files = ctx
            .agent_edited_paths
            .iter()
            .map(|f| format!("- {}", f))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!(
            "## Files Edited This Session\n\
             These files were modified by you during this session:\n{}",
            files
        ));
    }

    // Discovered AGENTS.md files (runtime, not in initial system prompt; shell-only)
    if !discovered_agents_md.is_empty() {
        let paths = discovered_agents_md
            .iter()
            .map(|p| format!("- {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!(
            "## Discovered Project Instruction Files\n\
             These project instruction files were found during the session \
             and may contain relevant coding conventions:\n{}",
            paths
        ));
    }

    // Available skills (startup + dynamically discovered, from SkillManager).
    // Reuse the standard listing renderer so the post-compaction listing matches
    // the startup `<system-reminder>` (no hard-coded tool name, includes
    // `Use when:` triggers and `Absolute path:`).
    if let Some(listing) =
        echo_agent_tools::types::skill_discovery_tracker::format_compaction_skill_listing(skills)
    {
        sections.push(format!("## Available Skills\n{listing}"));
    }

    if let Some(listing) = workflow_listing.filter(|text| !text.is_empty()) {
        sections.push(format!("## Available Workflows\n{listing}"));
    }

    // Common sections (BG → TODO → subagents) via shared formatter. Borrow
    // long fields from `ctx` rather than cloning them into an owned DTO.
    let commands: Vec<_> = ctx
        .running_tasks
        .iter()
        .map(|t| BackgroundTask {
            task_id: &t.task_id,
            command: &t.command,
            status: &t.status,
            tool_name: t.tool_name.as_deref(),
        })
        .collect();
    let todos: Vec<_> = ctx
        .todos
        .iter()
        .map(|t| TodoItem {
            id: &t.id,
            content: &t.content,
            status: match t.status {
                TodoSummaryStatus::Pending => TodoStatus::Pending,
                TodoSummaryStatus::InProgress => TodoStatus::InProgress,
                TodoSummaryStatus::Completed => TodoStatus::Completed,
                TodoSummaryStatus::Cancelled => TodoStatus::Cancelled,
            },
        })
        .collect();
    let subagents: Vec<_> = ctx
        .running_subagents
        .iter()
        .map(|s| RunningSubagent {
            subagent_id: &s.subagent_id,
            subagent_type: Some(&s.subagent_type),
            description: Some(&s.description),
            elapsed_secs: s.elapsed_ms / 1000,
        })
        .collect();
    sections.extend(reminder::format_active_agent_sections(
        &ActiveAgentReminderState {
            running_commands: &commands,
            todos: &todos,
            running_subagents: &subagents,
        },
        subagent_tool_names
            .map(|t| reminder::SubagentToolNames {
                poll: &t.poll,
                cancel: &t.cancel,
            })
            .as_ref(),
    ));

    // Connected MCP servers (shell-only)
    if !ctx.connected_mcp_servers.is_empty() {
        use echo_agent_tools::implementations::search_tool::format_compaction_server_line;
        let servers: String = ctx
            .connected_mcp_servers
            .iter()
            .map(|s| format_compaction_server_line(&s.name, s.tool_count, &s.description))
            .collect();
        let hint = if let Some(names) = mcp_tool_names {
            format!(
                "\nTo use MCP tools, you MUST call `{}` first to retrieve the tool's input schema before calling `{}`. NEVER guess parameter names — always use the exact schema returned by `{}`.",
                names.search, names.call, names.search
            )
        } else {
            String::new()
        };
        sections.push(format!(
            "## Connected MCP Servers\n{}{}",
            servers.trim_end(),
            hint
        ));
    }

    // Relevant memory from past sessions (post-compaction recovery; shell-only)
    if !memory_results.is_empty()
        && let Some(reminder) = super::memory_context::format_memory_reminder(memory_results)
    {
        sections.push(reminder);
    }

    reminder::wrap_system_reminder(sections)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_running_subagents() -> CompactionStateContext {
        CompactionStateContext {
            knowledge_context: None,
            cwd_generation: 0,
            destination_project_instructions: None,
            agent_message_anchor: None,
            running_subagents: vec![RunningSubagentSummary {
                subagent_id: "sub-1".into(),
                subagent_type: "explore".into(),
                description: "find files".into(),
                elapsed_ms: 5000,
            }],
            recent_messages: vec![],
            last_user_query: None,
            agent_edited_paths: vec![],
            running_tasks: vec![],
            connected_mcp_servers: vec![],
            todos: vec![],
        }
    }

    #[test]
    fn system_reminder_includes_subagent_section_when_tool_names_present() {
        let ctx = ctx_with_running_subagents();
        let names = SubagentToolNames {
            poll: "get_command_or_subagent_output".into(),
            cancel: "kill_command_or_subagent".into(),
        };
        let result = to_system_reminder_sync(&ctx, &[], &[], Some(&names), None, None);
        let text = result.expect("should produce a reminder");
        assert!(
            text.contains("Running Subagents"),
            "missing subagent section"
        );
        assert!(text.contains("get_command_or_subagent_output"));
        assert!(text.contains("kill_command_or_subagent"));
        assert!(text.contains("sub-1"));
    }

    #[test]
    fn system_reminder_includes_mcp_server_section() {
        let ctx = CompactionStateContext {
            knowledge_context: None,
            cwd_generation: 0,
            destination_project_instructions: None,
            agent_message_anchor: None,
            connected_mcp_servers: vec![
                CompactionServerSummary {
                    name: "grafana".into(),
                    tool_count: 28,
                    description: Some("Observability platform".into()),
                },
                CompactionServerSummary {
                    name: "linear".into(),
                    tool_count: 12,
                    description: None,
                },
            ],
            recent_messages: vec![],
            last_user_query: None,
            agent_edited_paths: vec![],
            running_tasks: vec![],
            running_subagents: vec![],
            todos: vec![],
        };
        let result = to_system_reminder_sync(&ctx, &[], &[], None, None, None);
        let text = result.expect("should produce a reminder");
        let expected = "\
<system-reminder>
## Connected MCP Servers
- grafana (28 tools): Observability platform
- linear (12 tools)
</system-reminder>";
        assert_eq!(text, expected, "got:\n{text}");
    }

    /// Regression: task IDs in the post-compaction reminder must be rendered
    /// verbatim. A fabricated `task-` prefix produces an ID that does not
    /// exist in the task registry, so the model's follow-up
    /// `get_task_output(task_id="task-<uuid>")` calls fail.
    #[test]
    fn running_task_ids_render_verbatim() {
        let ctx = CompactionStateContext {
            knowledge_context: None,
            cwd_generation: 0,
            destination_project_instructions: None,
            agent_message_anchor: None,
            running_tasks: vec![BackgroundTaskSummary {
                task_id: "019ea7f0-cb66-7aa2-9a09-488a3a795795".into(),
                command: "cargo test".into(),
                status: "running".into(),
                tool_name: Some("run_terminal_command".into()),
            }],
            recent_messages: vec![],
            last_user_query: None,
            agent_edited_paths: vec![],
            running_subagents: vec![],
            connected_mcp_servers: vec![],
            todos: vec![],
        };
        let text = to_system_reminder_sync(&ctx, &[], &[], None, None, None)
            .expect("should produce a reminder");
        assert!(
            text.contains("- \"019ea7f0-cb66-7aa2-9a09-488a3a795795\": `cargo test`"),
            "task ID must be quoted verbatim: {text}"
        );
        assert!(
            !text.contains("task-019ea7f0"),
            "task ID must not be decorated with a task- prefix: {text}"
        );
    }

    #[test]
    fn system_reminder_skips_subagent_section_when_tool_names_none() {
        let ctx = ctx_with_running_subagents();
        let result = to_system_reminder_sync(&ctx, &[], &[], None, None, None);
        if let Some(text) = result {
            assert!(
                !text.contains("Running Subagents"),
                "subagent section should be omitted when tool names are None"
            );
        }
    }

    fn ctx_with_todos(todos: Vec<TodoSummary>) -> CompactionStateContext {
        CompactionStateContext {
            knowledge_context: None,
            cwd_generation: 0,
            destination_project_instructions: None,
            agent_message_anchor: None,
            todos,
            recent_messages: vec![],
            last_user_query: None,
            agent_edited_paths: vec![],
            running_tasks: vec![],
            running_subagents: vec![],
            connected_mcp_servers: vec![],
        }
    }

    fn todo(id: &str, status: TodoSummaryStatus, content: &str) -> TodoSummary {
        TodoSummary {
            id: id.into(),
            content: content.into(),
            status,
        }
    }

    /// Active todos are re-surfaced post-compaction: pending/in_progress items
    /// render verbatim with id + status; completed/cancelled collapse to counts.
    #[test]
    fn system_reminder_includes_active_todos() {
        let ctx = ctx_with_todos(vec![
            todo("1", TodoSummaryStatus::InProgress, "wire up auth"),
            todo("2", TodoSummaryStatus::Pending, "add tests"),
            todo("3", TodoSummaryStatus::Completed, "read the code"),
            todo("4", TodoSummaryStatus::Cancelled, "abandoned idea"),
        ]);
        let text = to_system_reminder_sync(&ctx, &[], &[], None, None, None)
            .expect("should produce a reminder");
        assert!(
            text.contains("## TODO List"),
            "missing TODO section: {text}"
        );
        assert!(
            text.contains("- [in_progress] 1: wire up auth"),
            "got:\n{text}"
        );
        assert!(text.contains("- [pending] 2: add tests"), "got:\n{text}");
        // Done/cancelled items are summarized, not listed verbatim.
        assert!(text.contains("(1 completed, 1 cancelled)"), "got:\n{text}");
        assert!(
            !text.contains("read the code"),
            "completed item must not be listed verbatim: {text}"
        );
        assert!(
            !text.contains("abandoned idea"),
            "cancelled item must not be listed verbatim: {text}"
        );
    }

    /// The TODO List section is rendered directly below Running Background Tasks.
    #[test]
    fn system_reminder_places_todos_below_background_tasks() {
        let mut ctx = ctx_with_todos(vec![todo(
            "1",
            TodoSummaryStatus::InProgress,
            "wire up auth",
        )]);
        ctx.running_tasks = vec![BackgroundTaskSummary {
            task_id: "t1".into(),
            command: "cargo test".into(),
            status: "running".into(),
            tool_name: Some("run_terminal_command".into()),
        }];
        let text = to_system_reminder_sync(&ctx, &[], &[], None, None, None)
            .expect("should produce a reminder");
        let tasks_pos = text
            .find("## Running Background Tasks")
            .expect("tasks section");
        let todo_pos = text.find("## TODO List").expect("todo section");
        assert!(
            tasks_pos < todo_pos,
            "TODO List must appear below Running Background Tasks:\n{text}"
        );
    }

    #[test]
    fn system_reminder_places_workflows_below_skills() {
        let ctx = CompactionStateContext {
            knowledge_context: None,
            cwd_generation: 0,
            destination_project_instructions: None,
            agent_message_anchor: None,
            recent_messages: vec![],
            last_user_query: None,
            agent_edited_paths: vec![],
            running_tasks: vec![],
            running_subagents: vec![],
            connected_mcp_servers: vec![],
            todos: vec![],
        };
        let skills = [
            echo_agent_tools::implementations::skills::types::SkillInfo {
                name: "commit".into(),
                description: "Create a git commit.".into(),
                path: "/skills/commit/SKILL.md".into(),
                ..Default::default()
            },
        ];
        let workflows =
            "The following workflows are available:\n\n- review-pr: Review a PR.\n  Source: user";
        let text = to_system_reminder_sync(&ctx, &[], &skills, None, None, Some(workflows))
            .expect("should produce a reminder");
        let skills_at = text.find("## Available Skills").expect("skills section");
        let workflows_at = text
            .find("## Available Workflows")
            .expect("workflows section");
        assert!(
            skills_at < workflows_at,
            "workflows must sit under skills:\n{text}"
        );
        assert!(text.contains("review-pr"), "{text}");
    }

    /// No actionable items (all completed/cancelled) → no TODO section.
    #[test]
    fn system_reminder_omits_todos_when_none_active() {
        let ctx = ctx_with_todos(vec![
            todo("1", TodoSummaryStatus::Completed, "done"),
            todo("2", TodoSummaryStatus::Cancelled, "scrapped"),
        ]);
        let result = to_system_reminder_sync(&ctx, &[], &[], None, None, None);
        if let Some(text) = result {
            assert!(
                !text.contains("## TODO List"),
                "TODO section should be omitted when nothing is active: {text}"
            );
        }
    }
}
