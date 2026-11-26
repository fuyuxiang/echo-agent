//! Workspace-only, subagent-free variant of `kill_task` (delegates to [`KillTaskTool`]).

use super::KillTaskTool;
use crate::implementations::echo_agent_build::task_output::background_bash_requires_exprs;
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};
use echo_agent_tool_types::{KillTaskOutput, KillTaskToolInput};

fn kill_terminal_command_requires_expr() -> Expr<ToolRequirement> {
    Expr::Or(background_bash_requires_exprs())
}

#[derive(Debug, Default)]
pub struct KillTerminalCommandTool;

impl crate::types::tool_metadata::ToolMetadata for KillTerminalCommandTool {
    fn kind(&self) -> ToolKind {
        ToolKind::KillTaskAction
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        r#"Terminate a running background terminal command${%- if tools.by_kind.monitor %} or monitor${%- endif %}.

Usage notes:
- Pass its ${{ params.kill_task_action.task_id }}${%- if tools.by_kind.monitor %} (a monitor's ${{ params.kill_task_action.task_id }} is returned by ${{ tools.by_kind.monitor }})${%- endif %}.
- ${%- if is_windows %} Terminates the Job Object of${%- else %} Sends SIGTERM/SIGKILL to${%- endif %} a background command${%- if tools.by_kind.monitor %} or monitor${%- endif %}.
- Returns success if the command was killed or had already exited."#
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        kill_terminal_command_requires_expr()
    }
}

impl echo_agent_tool_runtime::Tool for KillTerminalCommandTool {
    type Args = KillTaskToolInput;
    type Output = KillTaskOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("kill_terminal_command").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "kill_terminal_command",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    #[tracing::instrument(
        name = "tool.kill_terminal_command",
        skip_all,
        fields(task_id = %input.task_id)
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: KillTaskToolInput,
    ) -> Result<KillTaskOutput, echo_agent_tool_runtime::ToolError> {
        echo_agent_tool_runtime::Tool::run(&KillTaskTool, ctx, input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer::types::{
        BackgroundHandle, KillOutcome, TaskSnapshot, TerminalBackend, TerminalRunRequest,
        TerminalRunResult,
    };
    use crate::types::resources::{Resources, Terminal};
    use crate::types::tool_metadata::ToolMetadata;
    use crate::types::tool_metadata::test_ctx_with_call_id;
    use std::sync::Arc;
    use std::time::Duration;

    struct MockTerminal {
        outcome: KillOutcome,
    }

    #[async_trait::async_trait]
    impl TerminalBackend for MockTerminal {
        async fn run(
            &self,
            _request: TerminalRunRequest,
        ) -> Result<TerminalRunResult, crate::computer::types::ComputerError> {
            unimplemented!()
        }
        async fn run_background(
            &self,
            _request: TerminalRunRequest,
        ) -> Result<BackgroundHandle, crate::computer::types::ComputerError> {
            unimplemented!()
        }
        async fn kill_task(&self, _task_id: &str) -> KillOutcome {
            self.outcome
        }
        async fn get_task(&self, _task_id: &str) -> Option<TaskSnapshot> {
            None
        }
        async fn wait_for_completion(
            &self,
            _task_id: &str,
            _timeout: Option<Duration>,
        ) -> Option<TaskSnapshot> {
            None
        }
        async fn list_tasks(&self) -> Vec<TaskSnapshot> {
            vec![]
        }
    }

    fn resources_with_terminal(outcome: KillOutcome) -> Resources {
        let mut resources = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminal { outcome });
        resources.insert(Terminal(backend));
        resources
    }

    #[test]
    fn tool_name_and_description_are_subagent_free() {
        let tool = KillTerminalCommandTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "kill_terminal_command"
        );
        let tmpl = ToolMetadata::description_template(&tool);
        assert!(tmpl.contains("background terminal command"));
        assert!(tmpl.contains("Terminate"));
        assert!(
            !tmpl.to_lowercase().contains("subagent"),
            "workspace tool must not mention subagents: {tmpl}"
        );
    }

    #[test]
    fn description_template_tracks_renamed_task_id() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;
        use std::collections::HashMap;

        let tools = HashMap::from([
            (ToolKind::Monitor, "monitor".to_string()),
            (
                ToolKind::KillTaskAction,
                "kill_terminal_command".to_string(),
            ),
        ]);
        let params = HashMap::from([(
            ToolKind::KillTaskAction,
            HashMap::from([("task_id".to_string(), "id".to_string())]),
        )]);
        let rendered = TemplateRenderer::new(tools, params)
            .render(ToolMetadata::description_template(&KillTerminalCommandTool))
            .unwrap();
        assert!(
            rendered.contains("Pass its id (a monitor's id is returned by monitor)"),
            "renamed task_id must appear in pass-line and monitor aside:\n{rendered}"
        );
        assert!(
            !rendered.contains("task_id"),
            "canonical task_id must not remain after rename:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn delegates_kill_killed() {
        let resources = resources_with_terminal(KillOutcome::Killed);
        let result = echo_agent_tool_runtime::Tool::run(
            &KillTerminalCommandTool,
            test_ctx_with_call_id(resources.into_shared(), "tool_call"),
            KillTaskToolInput {
                task_id: "tc-1".into(),
            },
        )
        .await
        .unwrap();

        match result {
            KillTaskOutput::Result(r) => {
                assert_eq!(r.outcome, "killed");
                assert_eq!(r.task_id, "tc-1");
            }
            other => panic!("Expected Result(killed), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delegates_kill_already_exited() {
        let resources = resources_with_terminal(KillOutcome::AlreadyExited);
        let result = echo_agent_tool_runtime::Tool::run(
            &KillTerminalCommandTool,
            test_ctx_with_call_id(resources.into_shared(), "tool_call"),
            KillTaskToolInput {
                task_id: "tc-2".into(),
            },
        )
        .await
        .unwrap();

        match result {
            KillTaskOutput::Result(r) => assert_eq!(r.outcome, "already_exited"),
            other => panic!("Expected Result(already_exited), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delegates_kill_not_found() {
        let resources = resources_with_terminal(KillOutcome::NotFound);
        let result = echo_agent_tool_runtime::Tool::run(
            &KillTerminalCommandTool,
            test_ctx_with_call_id(resources.into_shared(), "tool_call"),
            KillTaskToolInput {
                task_id: "tc-3".into(),
            },
        )
        .await
        .unwrap();

        match result {
            KillTaskOutput::TaskNotFound(msg) => {
                assert!(msg.contains("not found"), "message: {msg}");
                assert!(
                    msg.contains("tc-3"),
                    "message should include task ID: {msg}"
                );
            }
            other => panic!("Expected TaskNotFound, got {other:?}"),
        }
    }
}
