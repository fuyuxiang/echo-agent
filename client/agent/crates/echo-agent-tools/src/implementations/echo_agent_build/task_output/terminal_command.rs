//! Workspace-only, subagent-free variant of `get_task_output` (delegates to [`TaskOutputTool`]).

use super::{TaskOutputTool, background_bash_requires_exprs};
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};
use echo_agent_tool_types::{TaskOutputOutput, TaskOutputToolInput};

fn terminal_command_output_requires_expr() -> Expr<ToolRequirement> {
    Expr::Or(background_bash_requires_exprs())
}

#[derive(Debug, Default)]
pub struct GetTerminalCommandOutputTool;

impl crate::types::tool_metadata::ToolMetadata for GetTerminalCommandOutputTool {
    fn kind(&self) -> ToolKind {
        ToolKind::BackgroundTaskAction
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        // `{max_wait_ms}` is resolved per session by the finalize loop's
        // `TruncationConfig::interpolate_description`, like `{max_lines_read}`:
        // the cap is client-configurable, so it cannot be baked in here.
        r#"Get output and status from a background terminal command${%- if tools.by_kind.monitor %} or monitor${%- endif %}.

Usage notes:
- Pass ${{ params.background_task_action.task_ids }} with one or more ids from ${%- if params is defined and params.execute is defined and params.execute.is_background %} ${{ params.execute.is_background }}=true commands${%- else %} background commands${%- endif %}${%- if tools.by_kind.monitor %} (a monitor's ${{ params.kill_task_action.task_id }} is returned by ${{ tools.by_kind.monitor }})${%- endif %}; for a single task use a one-element array. Multiple ids with a positive ${{ params.background_task_action.timeout_ms }} wait until all complete
- Omit ${{ params.background_task_action.timeout_ms }} or pass 0 for a non-blocking status snapshot; set a positive ${{ params.background_task_action.timeout_ms }} to wait up to that many milliseconds, capped at {max_wait_ms}
- Returns current output, status, and exit code if completed${%- if tools.by_kind.read %}
- If output is large, use ${{ tools.by_kind.read }} on the output_file path${%- endif %}"#
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        terminal_command_output_requires_expr()
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

impl echo_agent_tool_runtime::Tool for GetTerminalCommandOutputTool {
    type Args = TaskOutputToolInput;
    type Output = TaskOutputOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("get_terminal_command_output").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "get_terminal_command_output",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    #[tracing::instrument(
        name = "tool.get_terminal_command_output",
        skip_all,
        fields(waits = %input.waits())
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: TaskOutputToolInput,
    ) -> Result<TaskOutputOutput, echo_agent_tool_runtime::ToolError> {
        echo_agent_tool_runtime::Tool::run(&TaskOutputTool, ctx, input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::implementations::echo_agent_build::task_output::test_helpers::{
        make_snapshot, resources_with_terminal,
    };
    use crate::types::tool_metadata::ToolMetadata;
    use crate::types::tool_metadata::test_ctx;

    #[test]
    fn tool_name_and_description_are_subagent_free() {
        let tool = GetTerminalCommandOutputTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "get_terminal_command_output"
        );
        let tmpl = ToolMetadata::description_template(&tool);
        assert!(tmpl.contains("background terminal command"));
        assert!(
            !tmpl.to_lowercase().contains("subagent"),
            "workspace tool must not mention subagents: {tmpl}"
        );
    }

    #[test]
    fn is_read_only() {
        let tool = GetTerminalCommandOutputTool;
        assert!(ToolMetadata::is_read_only(&tool));
    }

    #[tokio::test]
    async fn delegates_to_task_output_for_running_task() {
        let snapshot = make_snapshot("tc-1", false, None);
        let resources = resources_with_terminal(Some(snapshot));
        let result = echo_agent_tool_runtime::Tool::run(
            &GetTerminalCommandOutputTool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["tc-1".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        match result {
            TaskOutputOutput::Result(r) => {
                assert_eq!(r.task_id, "tc-1");
                assert_eq!(r.status, "running");
            }
            other => panic!("Expected Result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delegates_to_task_output_for_completed_task() {
        let snapshot = make_snapshot("tc-2", true, Some(0));
        let resources = resources_with_terminal(Some(snapshot));
        let result = echo_agent_tool_runtime::Tool::run(
            &GetTerminalCommandOutputTool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["tc-2".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        match result {
            TaskOutputOutput::Result(r) => {
                assert_eq!(r.status, "completed");
                assert_eq!(r.exit_code, Some(0));
            }
            other => panic!("Expected Result, got {other:?}"),
        }
    }
}
