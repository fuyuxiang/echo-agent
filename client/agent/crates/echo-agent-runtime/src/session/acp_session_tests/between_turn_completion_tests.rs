use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentCompletionSummary;
use echo_agent_tools::reminders::task_completion::format_between_turn_completions;

fn summary(
    id: &str,
    typ: &str,
    desc: &str,
    success: bool,
    ms: u64,
    tools: u32,
) -> SubagentCompletionSummary {
    SubagentCompletionSummary {
        subagent_id: id.into(),
        subagent_type: typ.into(),
        description: desc.into(),
        success,
        duration_ms: ms,
        tool_calls: tools,
        turns: 1,
        output: std::sync::Arc::from(format!("the answer for {id}").as_str()),
    }
}

#[test]
fn single_successful_completion_with_poll_tool() {
    let completions = vec![summary(
        "abc-123",
        "explore",
        "Search for auth patterns",
        true,
        12300,
        5,
    )];
    let result = format_between_turn_completions(&completions, Some("get_task_output"));
    assert!(result.starts_with("While you were idle, 1 background subagent completed:\n"));
    assert!(result.contains("[explore]"));
    assert!(result.contains("completed successfully"));
    assert!(result.contains("12.3s"));
    assert!(result.contains("5 tool calls"));
    assert!(result.contains("abc-123"));
    assert!(result.contains("get_task_output"));
}

#[test]
fn failed_completion_with_poll_tool() {
    let completions = vec![summary(
        "def-456",
        "general-purpose",
        "Implement feature X",
        false,
        45200,
        12,
    )];
    let result = format_between_turn_completions(&completions, Some("get_task_output"));
    assert!(result.contains("failed"));
    assert!(result.contains("45.2s"));
    assert!(result.contains("12 tool calls"));
}

#[test]
fn multiple_completions_batched_with_poll_tool() {
    let completions = vec![
        summary("a", "explore", "task 1", true, 1000, 2),
        summary("b", "general-purpose", "task 2", false, 5000, 8),
        summary("c", "explore", "task 3", true, 3000, 4),
    ];
    let result = format_between_turn_completions(&completions, Some("get_task_output"));
    assert!(result.starts_with("While you were idle, 3 background subagents completed:\n"));
    // All three entries should appear
    assert!(result.contains("subagent_id: a."));
    assert!(result.contains("subagent_id: b."));
    assert!(result.contains("subagent_id: c."));
}

#[test]
fn no_poll_tool_inlines_output() {
    // No BackgroundTaskAction tool exposed. The
    // model has no way to retrieve the subagent's output later, so the
    // completion notification MUST inline the output text.
    let completions = vec![summary(
        "abc-123",
        "explore",
        "Search for auth patterns",
        true,
        12300,
        5,
    )];
    let result = format_between_turn_completions(&completions, None);
    assert!(result.contains("[explore]"));
    assert!(result.contains("abc-123"));
    assert!(
        !result.contains("get_task_output"),
        "must not mention a polling tool when none is available: {result}"
    );
    assert!(
        result.contains("response:\nthe answer for abc-123"),
        "must inline the subagent's output text: {result}"
    );
}
