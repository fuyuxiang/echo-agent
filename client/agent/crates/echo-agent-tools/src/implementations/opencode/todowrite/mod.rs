//! OpenCode `todowrite` tool — full-replace task list management.
//!
//! Follows the opencode convention: every call sends the **complete** todo list
//! (full-replace semantics, no merge). Items carry `content`, `status`, and
//! `priority` — no caller-supplied IDs.
//!
//! State is stored as `State<TodoState>` in Resources, shared with the
//! echo_agent_build todo infrastructure.

use std::fmt::Write;

use crate::implementations::echo_agent_build::todo::{
    TodoItem, TodoPriority, TodoState, TodoStatus,
};
use crate::types::output::{TodoWriteOutput, TodoWriteSuccess};
use crate::types::requirements::{Expr, ToolRequirement};
#[allow(unused_imports)]
use crate::types::resources::{SharedResources, State};
use crate::types::tool::{ToolKind, ToolNamespace};

// ─── Description ─────────────────────────────────────────────────────

const DESCRIPTION: &str = r#"Use this tool to create and manage a structured task list for your current coding session. This helps you track progress, organize complex tasks, and demonstrate thoroughness to the user.
It also helps the user understand the progress of the task and overall progress of their requests.

## When to Use This Tool
Use this tool proactively in these scenarios:

1. Complex multistep tasks - When a task requires 3 or more distinct steps or actions
2. Non-trivial and complex tasks - Tasks that require careful planning or multiple operations
3. User explicitly requests todo list - When the user directly asks you to use the todo list
4. User provides multiple tasks - When users provide a list of things to be done (numbered or comma-separated)
5. After receiving new instructions - Immediately capture user requirements as todos. Feel free to edit the todo list based on new information.
6. After completing a task - Mark it complete and add any new follow-up tasks
7. When you start working on a new task, mark the todo as in_progress. Ideally you should only have one todo as in_progress at a time. Complete existing tasks before starting new ones.

## When NOT to Use This Tool

Skip using this tool when:
1. There is only a single, straightforward task
2. The task is trivial and tracking it provides no organizational benefit
3. The task can be completed in less than 3 trivial steps
4. The task is purely conversational or informational

NOTE that you should not use this tool if there is only one trivial task to do. In this case you are better off just doing the task directly.

## Examples of When to Use the Todo List

<example>
User: I want to add a dark mode toggle to the application settings. Make sure you run the tests and build when you're done!
Assistant: I'll help add a dark mode toggle to your application settings. Let me create a todo list to track this implementation.
*Creates todo list with the following items:*
1. Create dark mode toggle component in Settings page
2. Add dark mode state management (context/store)
3. Implement CSS-in-JS styles for dark theme
4. Update existing components to support theme switching
5. Run tests and build process, addressing any failures or errors that occur
*Begins working on the first task*

<reasoning>
The assistant used the todo list because:
1. Adding dark mode is a multi-step feature requiring UI, state management, and styling changes
2. The user explicitly requested tests and build be run afterward
3. The assistant inferred that tests and build need to pass by adding "Ensure tests and build succeed" as the final task
</reasoning>
</example>

<example>
User: Help me rename the function getCwd to getCurrentWorkingDirectory across my project
Assistant: Let me first search through your codebase to find all occurrences of 'getCwd'.
*Uses grep or search tools to locate all instances of getCwd in the codebase*
Assistant: I've found 15 instances of 'getCwd' across 8 different files. Let me create a todo list to track these changes.
*Creates todo list with specific items for each file that needs updating*

<reasoning>
The assistant used the todo list because:
1. First, the assistant searched to understand the scope of the task
2. Upon finding multiple occurrences across different files, it determined this was a complex task with multiple steps
3. The todo list helps ensure every instance is tracked and updated systematically
4. This approach prevents missing any occurrences and maintains code consistency
</reasoning>
</example>


<example>
User: I need to implement these features for my e-commerce site: user registration, product catalog, shopping cart, and checkout flow.
Assistant: I'll help implement these features. First, let's add all the features to the todo list.
*Creates a todo list breaking down each feature into specific tasks based on the project architecture*
Assistant: Let's start with user registration. This will involve creating database models, API endpoints, and frontend forms that integrate with your existing authentication system.

<reasoning>
The assistant used the todo list because:
1. The user provided multiple complex features to implement in a comma separated list
2. The todo list helps organize these large features into manageable tasks
3. This approach allows for tracking progress across the entire implementation
</reasoning>
</example>

<example>
User: Can you help optimize my React application? It's rendering slowly and has performance issues.</user>
Assistant: I'll help optimize your React application. First, let me examine your codebase to identify potential performance bottlenecks.
*Reviews component structure, render patterns, state management, and data fetching*
Assistant: After analyzing your codebase, I've identified several performance issues. Let me create a todo list to track our optimization efforts.
*Creates todo list with items like: 1) Implement memoization for expensive calculations in ProductList, 2) Add virtualization for long lists in Dashboard, 3) Optimize image loading in Gallery component, 4) Fix state update loops in ShoppingCart, 5) Review bundle size and implement code splitting*
Let's start by implementing memoization for the expensive calculations in your ProductList component.</assistant>

<reasoning>
The assistant used the todo list because:
1. First, the assistant examined the codebase to identify specific performance issues
2. Based on this analysis, it identified multiple optimization opportunities
3. Performance optimization is a non-trivial task requiring multiple steps
4. The todo list helps methodically track improvements across different components
5. This systematic approach ensures all performance bottlenecks are addressed
</reasoning>
</example>

## Examples of When NOT to Use the Todo List

<example>
User: How do I print 'Hello World' in Python?
Assistant: In Python, you can print "Hello World" with this simple code:

python
print("Hello World")

This will output the text "Hello World" to the console when executed.</assistant>

<reasoning>
The assistant did not use the todo list because this is a single, trivial task that can be completed in one step. There's no need to track multiple tasks or steps for such a straightforward request.
</reasoning>
</example>

<example>
User: What does the git status command do?
Assistant: The git status command shows the current state of your working directory and staging area. It displays which changes have been staged, which haven't, and which files aren't being tracked by Git. This is a fundamental command for understanding the current state of your repository before making commits.

<reasoning>
The assistant did not use the todo list because this is an informational request with no actual coding task to complete. The user is simply asking for an explanation, not for the assistant to perform multiple steps or tasks.
</reasoning>
</example>

<example>
User: Can you add a comment to the calculateTotal function to explain what it does?
Assistant: Sure, let me add a comment to the calculateTotal function to explain what it does.
* Uses the Edit tool to add a comment to the calculateTotal function *

<reasoning>
The assistant did not use the todo list because this is a single, straightforward task confined to one location in the code. Adding a comment doesn't require tracking multiple steps or systematic organization.
</reasoning>
</example>

<example>
User: Run npm install for me and tell me what happens.
Assistant: I'll run the npm install command for you.

*Executes: npm install*

The command completed successfully. Here's the output:
[Output of npm install command]

All dependencies have been installed according to your package.json file.

<reasoning>
The assistant did not use the todo list because this is a single command execution with immediate results. There are no multiple steps to track or organize, making the todo list unnecessary for this straightforward task.
</reasoning>
</example>

## Task States and Management

1. **Task States**: Use these states to track progress:
   - pending: Task not yet started
   - in_progress: Currently working on (limit to ONE task at a time)
   - completed: Task finished successfully
   - cancelled: Task no longer needed

2. **Task Management**:
   - Update task status in real-time as you work
   - Mark tasks complete IMMEDIATELY after finishing (don't batch completions)
   - Only have ONE task in_progress at any time
   - Complete current tasks before starting new ones
   - Cancel tasks that become irrelevant

3. **Task Breakdown**:
   - Create specific, actionable items
   - Break complex tasks into smaller, manageable steps
   - Use clear, descriptive task names

When in doubt, use this tool. Being proactive with task management demonstrates attentiveness and ensures you complete all requirements successfully."#;

// ─── Input ───────────────────────────────────────────────────────────

/// A single todo item in the opencode format.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct OpenCodeTodoItem {
    /// Brief description of the task.
    #[schemars(description = "Brief description of the task")]
    pub content: String,

    /// Current status: "pending", "in_progress", "completed", or "cancelled".
    #[schemars(
        description = "The current status of the todo item: pending, in_progress, completed, or cancelled"
    )]
    pub status: String,

    /// Priority level: "high", "medium", or "low".
    #[schemars(description = "Priority level: high, medium, or low")]
    pub priority: String,
}

/// Input for the opencode `todowrite` tool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TodoWriteInput {
    /// The complete todo list. Replaces all existing todos.
    #[schemars(
        description = "Array of todo items — the complete updated todo list. Replaces all existing todos."
    )]
    pub todos: Vec<OpenCodeTodoItem>,
}

// ─── ToolInput conversions (via Dynamic variant) ─────────────────────

impl TryFrom<crate::types::tool_io::ToolInput> for TodoWriteInput {
    type Error = String;
    fn try_from(value: crate::types::tool_io::ToolInput) -> Result<Self, Self::Error> {
        match value {
            crate::types::tool_io::ToolInput::Dynamic(v) => {
                serde_json::from_value(v).map_err(|e| format!("TodoWriteInput: {e}"))
            }
            _ => Err("expected Dynamic variant for TodoWriteInput".into()),
        }
    }
}

impl From<TodoWriteInput> for crate::types::tool_io::ToolInput {
    fn from(value: TodoWriteInput) -> Self {
        crate::types::tool_io::ToolInput::Dynamic(
            serde_json::to_value(value).expect("TodoWriteInput serializes to JSON"),
        )
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

/// Parse a status string into `TodoStatus`, defaulting to `Pending`.
fn parse_status(s: &str) -> TodoStatus {
    match s {
        "in_progress" => TodoStatus::InProgress,
        "completed" => TodoStatus::Completed,
        "cancelled" => TodoStatus::Cancelled,
        // "pending" and anything unrecognized
        _ => TodoStatus::Pending,
    }
}

/// Parse a priority string into `TodoPriority`, defaulting to `Medium`.
fn parse_priority(s: &str) -> TodoPriority {
    match s {
        "high" => TodoPriority::High,
        "low" => TodoPriority::Low,
        // "medium" and anything unrecognized
        _ => TodoPriority::Medium,
    }
}

/// Build a human-readable summary of the current todo state.
fn summarize(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "No tasks currently tracked.".into();
    }
    let mut out = String::new();
    for (i, t) in todos.iter().enumerate() {
        writeln!(&mut out, "- {} {}: {}", t.status.tag(), i + 1, t.content).ok();
    }
    out
}

// ─── Tool ────────────────────────────────────────────────────────────

/// OpenCode `todowrite` tool.
#[derive(Debug, Default)]
pub struct TodoWriteTool;

// ─── Tests ───────────────────────────────────────────────────────────

impl crate::types::tool_metadata::ToolMetadata for TodoWriteTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Plan
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::OpenCode
    }

    fn description_template(&self) -> &str {
        DESCRIPTION
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for TodoWriteTool {
    type Args = TodoWriteInput;
    type Output = TodoWriteOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("todowrite").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "todowrite",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    #[tracing::instrument(
        name = "tool.opencode.todowrite",
        skip_all,
        fields(todo_count = input.todos.len())
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: TodoWriteInput,
    ) -> Result<TodoWriteOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let (summary_for_prompt, todos, state_snapshot) = {
            let mut res = resources.lock().await;
            let todo_state = res.get_or_default::<State<TodoState>>();

            // Full-replace: clear existing state and insert all incoming items.
            todo_state.0.clear();

            for (i, item) in input.todos.iter().enumerate() {
                let status = parse_status(&item.status);
                let priority = parse_priority(&item.priority);

                // Use a positional id since opencode items don't carry IDs.
                let id = format!("{}", i + 1);

                todo_state.0.push(
                    id,
                    TodoItem {
                        content: item.content.clone(),
                        priority,
                        status,
                        meta: None,
                    },
                );
            }

            let todos: Vec<TodoItem> = todo_state.0.todo_items().cloned().collect();
            let state_snapshot = todo_state.0.clone();
            let summary_for_prompt = summarize(&todos);

            (summary_for_prompt, todos, state_snapshot)
        };

        Ok(TodoWriteOutput::TodosUpdated(TodoWriteSuccess {
            summary_for_prompt,
            todos,
            state: state_snapshot,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx;

    #[allow(unused_imports)]
    use crate::types::resources::Resources;

    fn make_item(content: &str, status: &str, priority: &str) -> OpenCodeTodoItem {
        OpenCodeTodoItem {
            content: content.to_owned(),
            status: status.to_owned(),
            priority: priority.to_owned(),
        }
    }

    /// Unwrap a `TodoWriteOutput` expecting the `TodosUpdated` variant.
    fn expect_success(output: TodoWriteOutput) -> TodoWriteSuccess {
        match output {
            TodoWriteOutput::TodosUpdated(s) => s,
            other => panic!("expected TodosUpdated, got {other:?}"),
        }
    }

    #[test]
    fn id_and_kind() {
        use crate::types::tool_metadata::ToolMetadata;
        let tool = TodoWriteTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "todowrite"
        );
        assert!(matches!(tool.kind(), ToolKind::Plan));
    }

    #[tokio::test]
    async fn basic_replace() {
        let tool = TodoWriteTool;
        let resources = Resources::new();

        let input = TodoWriteInput {
            todos: vec![
                make_item("Build project", "in_progress", "high"),
                make_item("Run tests", "pending", "medium"),
            ],
        };

        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap(),
        );
        assert_eq!(output.todos.len(), 2);
        assert!(output.summary_for_prompt.contains("Build project"));
        assert!(output.summary_for_prompt.contains("Run tests"));
    }

    #[tokio::test]
    async fn replace_clears_previous() {
        let tool = TodoWriteTool;
        let resources = Resources::new();
        let shared = resources.into_shared();

        // First call
        let input1 = TodoWriteInput {
            todos: vec![make_item("Old task", "completed", "low")],
        };
        echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input1)
            .await
            .unwrap();

        // Second call replaces everything
        let input2 = TodoWriteInput {
            todos: vec![make_item("New task", "pending", "high")],
        };
        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input2)
                .await
                .unwrap(),
        );

        assert_eq!(output.todos.len(), 1);
        assert!(output.summary_for_prompt.contains("New task"));
        assert!(!output.summary_for_prompt.contains("Old task"));
    }

    #[tokio::test]
    async fn empty_todos() {
        let tool = TodoWriteTool;
        let resources = Resources::new();

        let input = TodoWriteInput { todos: vec![] };
        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap(),
        );

        assert!(output.todos.is_empty());
        assert!(output.summary_for_prompt.contains("No tasks"));
    }

    #[tokio::test]
    async fn state_persists_in_resources() {
        let tool = TodoWriteTool;
        let resources = Resources::new();

        let input = TodoWriteInput {
            todos: vec![make_item("Task A", "pending", "medium")],
        };
        let shared = resources.into_shared();
        echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();

        let res = shared.lock().await;
        let state = res.get::<State<TodoState>>().unwrap();
        assert_eq!(state.0.todo_items().count(), 1);
    }

    #[test]
    fn parse_status_variants() {
        assert_eq!(parse_status("pending"), TodoStatus::Pending);
        assert_eq!(parse_status("in_progress"), TodoStatus::InProgress);
        assert_eq!(parse_status("completed"), TodoStatus::Completed);
        assert_eq!(parse_status("cancelled"), TodoStatus::Cancelled);
        // Unknown defaults to Pending
        assert_eq!(parse_status("unknown"), TodoStatus::Pending);
    }

    #[test]
    fn parse_priority_variants() {
        assert_eq!(parse_priority("high"), TodoPriority::High);
        assert_eq!(parse_priority("medium"), TodoPriority::Medium);
        assert_eq!(parse_priority("low"), TodoPriority::Low);
        // Unknown defaults to Medium
        assert_eq!(parse_priority("unknown"), TodoPriority::Medium);
    }

    #[tokio::test]
    async fn cancelled_status_parsed() {
        let tool = TodoWriteTool;
        let resources = Resources::new();

        let input = TodoWriteInput {
            todos: vec![make_item("Dropped task", "cancelled", "low")],
        };
        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap(),
        );

        assert_eq!(output.todos[0].status, TodoStatus::Cancelled);
    }

    #[tokio::test]
    async fn summary_format() {
        let tool = TodoWriteTool;
        let resources = Resources::new();

        let input = TodoWriteInput {
            todos: vec![
                make_item("First", "completed", "high"),
                make_item("Second", "in_progress", "medium"),
                make_item("Third", "pending", "low"),
            ],
        };
        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap(),
        );

        assert!(output.summary_for_prompt.contains("[completed] 1: First"));
        assert!(
            output
                .summary_for_prompt
                .contains("[in_progress] 2: Second")
        );
        assert!(output.summary_for_prompt.contains("[pending] 3: Third"));
    }

    #[test]
    fn namespace_verification() {
        use crate::types::tool_metadata::ToolMetadata;
        let tool = TodoWriteTool;
        assert!(matches!(tool.tool_namespace(), ToolNamespace::OpenCode));
    }

    #[test]
    fn serde_roundtrip() {
        let input = TodoWriteInput {
            todos: vec![
                make_item("Task A", "pending", "high"),
                make_item("Task B", "in_progress", "medium"),
                make_item("Task C", "completed", "low"),
            ],
        };
        let json = serde_json::to_value(&input).unwrap();

        // Verify the JSON structure has the expected shape.
        let arr = json["todos"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["content"], "Task A");
        assert_eq!(arr[0]["status"], "pending");
        assert_eq!(arr[0]["priority"], "high");

        // Round-trip back.
        let deserialized: TodoWriteInput = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.todos.len(), 3);
        assert_eq!(deserialized.todos[1].content, "Task B");
        assert_eq!(deserialized.todos[1].status, "in_progress");
        assert_eq!(deserialized.todos[2].priority, "low");
    }

    #[tokio::test]
    async fn large_todo_list() {
        let tool = TodoWriteTool;
        let resources = Resources::new();

        let todos: Vec<OpenCodeTodoItem> = (0..25)
            .map(|i| make_item(&format!("Task {i}"), "pending", "medium"))
            .collect();
        let input = TodoWriteInput { todos };

        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap(),
        );
        assert_eq!(output.todos.len(), 25);
        for i in 0..25 {
            assert!(
                output.summary_for_prompt.contains(&format!("Task {i}")),
                "missing Task {i} in summary"
            );
        }
    }

    #[tokio::test]
    async fn priority_preserved() {
        let tool = TodoWriteTool;
        let resources = Resources::new();

        let input = TodoWriteInput {
            todos: vec![
                make_item("High task", "pending", "high"),
                make_item("Medium task", "pending", "medium"),
                make_item("Low task", "pending", "low"),
            ],
        };
        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap(),
        );

        assert_eq!(output.todos[0].priority, TodoPriority::High);
        assert_eq!(output.todos[1].priority, TodoPriority::Medium);
        assert_eq!(output.todos[2].priority, TodoPriority::Low);
    }

    #[tokio::test]
    async fn mixed_statuses() {
        let tool = TodoWriteTool;
        let resources = Resources::new();

        let input = TodoWriteInput {
            todos: vec![
                make_item("Pending task", "pending", "medium"),
                make_item("Active task", "in_progress", "high"),
                make_item("Done task", "completed", "low"),
                make_item("Dropped task", "cancelled", "medium"),
            ],
        };
        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap(),
        );

        assert_eq!(output.todos.len(), 4);
        assert_eq!(output.todos[0].status, TodoStatus::Pending);
        assert_eq!(output.todos[1].status, TodoStatus::InProgress);
        assert_eq!(output.todos[2].status, TodoStatus::Completed);
        assert_eq!(output.todos[3].status, TodoStatus::Cancelled);
    }

    #[tokio::test]
    async fn repeated_calls() {
        let tool = TodoWriteTool;
        let resources = Resources::new();
        let shared = resources.into_shared();

        // Call 1
        let input1 = TodoWriteInput {
            todos: vec![make_item("First batch", "pending", "high")],
        };
        echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input1)
            .await
            .unwrap();

        // Call 2
        let input2 = TodoWriteInput {
            todos: vec![
                make_item("Second A", "in_progress", "medium"),
                make_item("Second B", "pending", "low"),
            ],
        };
        echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input2)
            .await
            .unwrap();

        // Call 3 — only these should survive.
        let input3 = TodoWriteInput {
            todos: vec![
                make_item("Final X", "completed", "high"),
                make_item("Final Y", "pending", "medium"),
                make_item("Final Z", "in_progress", "low"),
            ],
        };
        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input3)
                .await
                .unwrap(),
        );

        assert_eq!(output.todos.len(), 3);
        assert!(!output.summary_for_prompt.contains("First batch"));
        assert!(!output.summary_for_prompt.contains("Second A"));
        assert!(!output.summary_for_prompt.contains("Second B"));
        assert!(output.summary_for_prompt.contains("Final X"));
        assert!(output.summary_for_prompt.contains("Final Y"));
        assert!(output.summary_for_prompt.contains("Final Z"));

        // Verify shared state also only has 3 items.
        let res = shared.lock().await;
        let state = res.get::<State<TodoState>>().unwrap();
        assert_eq!(state.0.todo_items().count(), 3);
    }

    #[test]
    fn serde_opencode_todo_item() {
        let item = OpenCodeTodoItem {
            content: "Write tests".to_owned(),
            status: "in_progress".to_owned(),
            priority: "high".to_owned(),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["content"], "Write tests");
        assert_eq!(json["status"], "in_progress");
        assert_eq!(json["priority"], "high");

        // Deserialize back.
        let recovered: OpenCodeTodoItem = serde_json::from_value(json).unwrap();
        assert_eq!(recovered.content, "Write tests");
        assert_eq!(recovered.status, "in_progress");
        assert_eq!(recovered.priority, "high");
    }

    #[tokio::test]
    async fn status_preserved_per_item() {
        let tool = TodoWriteTool;
        let resources = Resources::new();

        let input = TodoWriteInput {
            todos: vec![
                make_item("Pending task", "pending", "medium"),
                make_item("Active task", "in_progress", "medium"),
                make_item("Done task", "completed", "medium"),
                make_item("Dropped task", "cancelled", "medium"),
            ],
        };
        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap(),
        );

        assert_eq!(output.todos[0].status, TodoStatus::Pending);
        assert_eq!(output.todos[1].status, TodoStatus::InProgress);
        assert_eq!(output.todos[2].status, TodoStatus::Completed);
        assert_eq!(output.todos[3].status, TodoStatus::Cancelled);
    }

    #[tokio::test]
    async fn state_snapshot_in_output() {
        let tool = TodoWriteTool;
        let resources = Resources::new();

        let input = TodoWriteInput {
            todos: vec![
                make_item("Task A", "pending", "high"),
                make_item("Task B", "in_progress", "medium"),
                make_item("Task C", "completed", "low"),
            ],
        };
        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap(),
        );

        // output.state should be a valid TodoState with all 3 items.
        assert!(!output.state.is_empty());
        assert_eq!(output.state.todo_items().count(), 3);

        // Verify items in the snapshot match the input.
        let items: Vec<_> = output.state.todo_items().collect();
        assert_eq!(items[0].content, "Task A");
        assert_eq!(items[1].content, "Task B");
        assert_eq!(items[2].content, "Task C");
    }

    #[tokio::test]
    async fn runtime_trait_interface() {
        let tool = TodoWriteTool;
        let resources = Resources::new();

        let input = TodoWriteInput {
            todos: vec![
                make_item("Build project", "in_progress", "high"),
                make_item("Run tests", "pending", "medium"),
            ],
        };
        let output = expect_success(
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap(),
        );
        assert_eq!(output.todos.len(), 2);
        assert!(output.summary_for_prompt.contains("Build project"));
        assert!(output.summary_for_prompt.contains("Run tests"));
    }
}
