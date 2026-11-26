//! `wait_tasks` tool — blocks until multiple background tasks complete.
//!
//! Prefer `get_task_output` / `get_command_or_subagent_output` with `task_ids`
//! and a positive `timeout_ms` (wait-all). This tool remains as a thin alias
//! for older prompts that still emit `wait_tasks` / `wait_commands_or_subagents`.
//!
//! `mode: wait_any` is still honored here for compatibility; the unified get
//! tool only supports wait-all for multi-id waits.

use crate::DEFAULT_TOOL_OUTPUT_BYTES;
use crate::implementations::echo_agent_build::task::backend::SubagentBackendResource;
use crate::implementations::echo_agent_build::task_output::{
    MAX_MULTI_WAIT_IDS, TaskOutputTool, WaitHint, resolve_tasks, wait_any_event_driven,
};
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::{Terminal, TruncationCfg};
use crate::types::template_renderer::TemplateRenderer;
use crate::types::tool::{ToolKind, ToolNamespace};
use echo_agent_tool_types::{
    MultiTaskOutputResult, TaskOutputOutput, WaitMode, WaitTasksToolInput,
};

#[derive(Debug, Default)]
pub struct WaitTasksTool;

impl crate::types::tool_metadata::ToolMetadata for WaitTasksTool {
    fn kind(&self) -> ToolKind {
        ToolKind::WaitTasksAction
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        // Canonical wording lives in the shared builder; `versioned_definition`
        // renders it context-aware from the finalized toolset. This static
        // fallback uses canonical tool/param names.
        static DESC: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            echo_agent_tool_types::build_wait_tasks_description(
                &echo_agent_tool_types::WaitTasksToolNaming {
                    background_retrieval_tool: "get_task_output",
                    bash_background_param: Some("is_background"),
                    subagent_background_param: Some("run_in_background"),
                },
            )
        });
        &DESC
    }

    fn versioned_definition(
        &self,
        _contract_version: Option<&str>,
        client_name: &str,
        description_override: Option<&str>,
        renderer: &TemplateRenderer,
        param_map: &std::collections::HashMap<String, String>,
        input_schema: &serde_json::Value,
        _effective_params: &serde_json::Value,
    ) -> crate::types::definition::ToolDefinition {
        let description = wait_tasks_description(renderer, description_override);
        let remapped_schema = if param_map.is_empty() {
            input_schema.clone()
        } else {
            crate::util::remap::remap_schema_properties(input_schema, param_map)
        };
        crate::types::definition::ToolDefinition::function(
            client_name,
            Some(&description),
            remapped_schema,
        )
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        super::task_output_requires_expr()
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

/// Resolve the model-facing `wait_tasks` description from the finalized toolset,
/// honoring an explicit config override. Wording lives in the shared
/// [`echo_agent_tool_types::build_wait_tasks_description`] builder so the CLI and
/// prod-chat can't drift. When no dedicated background-retrieval tool is
/// registered, fall back to naming this tool's own get-output sibling.
fn wait_tasks_description(
    renderer: &TemplateRenderer,
    description_override: Option<&str>,
) -> String {
    if let Some(ovr) = description_override {
        return renderer.render(ovr).unwrap_or_else(|e| {
            tracing::warn!("wait_tasks description override render failed, using raw: {e}");
            ovr.to_string()
        });
    }
    echo_agent_tool_types::build_wait_tasks_description(
        &echo_agent_tool_types::WaitTasksToolNaming {
            background_retrieval_tool: renderer
                .tool_for_kind(ToolKind::BackgroundTaskAction)
                .unwrap_or("get_task_output"),
            bash_background_param: renderer.param_for_kind(ToolKind::Execute, "is_background"),
            subagent_background_param: renderer.param_for_kind(ToolKind::Task, "run_in_background"),
        },
    )
}

impl echo_agent_tool_runtime::Tool for WaitTasksTool {
    type Args = WaitTasksToolInput;
    type Output = TaskOutputOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("wait_tasks").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "wait_tasks",
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
        name = "tool.wait_tasks",
        skip_all,
        fields(task_count = %input.task_ids.len(), mode = ?input.mode)
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: WaitTasksToolInput,
    ) -> Result<TaskOutputOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;

        let resources = shared_resources(&ctx)?;

        if input.task_ids.is_empty() {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                "task_ids must not be empty.".to_string(),
            ));
        }
        if input.task_ids.len() > MAX_MULTI_WAIT_IDS {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                format!("task_ids exceeds maximum of {MAX_MULTI_WAIT_IDS} entries."),
            ));
        }

        // wait_all (the common case) shares the unified multi path on get_task_output.
        if matches!(input.mode, WaitMode::WaitAll) {
            use super::DEFAULT_WAIT_TIMEOUT;
            // Legacy wait always blocks: omit or 0 => default budget (not a snapshot).
            let ms = input
                .timeout_ms
                .filter(|ms| *ms > 0)
                .unwrap_or(DEFAULT_WAIT_TIMEOUT.as_millis() as u64);
            return TaskOutputTool::run_multi_tasks(
                &input.task_ids,
                Some(ms),
                resources,
                "wait_commands_or_subagents",
            )
            .await;
        }

        // wait_any: keep legacy event-driven path (not exposed on get_task_output).
        let requested = input
            .timeout_ms
            .map(std::time::Duration::from_millis)
            .unwrap_or(super::DEFAULT_WAIT_TIMEOUT);
        let timeout = crate::implementations::echo_agent_build::task_output::capped_wait_timeout(
            input.timeout_ms,
            crate::implementations::echo_agent_build::task_output::max_wait_block(),
        );

        let (terminal, backend, read_file_name, max_output_bytes) = {
            let res = resources.lock().await;
            let terminal = res.require::<Terminal>()?.0.clone();
            let backend = res.get::<SubagentBackendResource>().cloned();
            let renderer = res.require::<TemplateRenderer>()?;
            let rfn = renderer.render("${{ tools.by_kind.read }}").map_err(|e| {
                echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string())
            })?;
            let mob = res
                .get::<TruncationCfg>()
                .map(|cfg| {
                    cfg.0.max_output_bytes_for(
                        "wait_commands_or_subagents",
                        DEFAULT_TOOL_OUTPUT_BYTES,
                    )
                })
                .unwrap_or(DEFAULT_TOOL_OUTPUT_BYTES);
            (terminal, backend, rfn, mob)
        };

        let initial = resolve_tasks(
            &input.task_ids,
            &terminal,
            &backend,
            &read_file_name,
            max_output_bytes,
            WaitHint::NotRequested,
        )
        .await;

        let has_pending =
            !initial.pending_bash_ids.is_empty() || !initial.pending_subagent_ids.is_empty();

        let results = if has_pending {
            let deadline = tokio::time::Instant::now() + timeout;
            let wait_hint = wait_any_event_driven(
                &terminal,
                &backend,
                &initial.pending_bash_ids,
                &initial.pending_subagent_ids,
                deadline,
            )
            .await
            .hint(requested, timeout);
            resolve_tasks(
                &input.task_ids,
                &terminal,
                &backend,
                &read_file_name,
                max_output_bytes,
                wait_hint,
            )
            .await
            .results
        } else {
            initial.results
        };

        let completed_count = results
            .iter()
            .filter(|r| super::is_terminal_status(&r.status))
            .count();
        let total = results.len();
        let summary = format!("{completed_count}/{total} tasks completed (wait_any)");

        Ok(TaskOutputOutput::MultiResult(MultiTaskOutputResult {
            mode: "wait_any".to_string(),
            results,
            summary,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::ToolMetadata;
    use crate::types::tool_metadata::test_ctx;
    use echo_agent_tool_runtime::Tool;

    #[test]
    fn tool_name_and_kind() {
        let tool = WaitTasksTool;
        assert_eq!(Tool::id(&tool).as_str(), "wait_tasks");
        assert_eq!(ToolMetadata::kind(&tool), ToolKind::WaitTasksAction);
    }

    #[test]
    fn description_mentions_key_concepts() {
        let tool = WaitTasksTool;
        let d = ToolMetadata::description_template(&tool);
        assert!(d.contains("task_ids"));
    }

    #[tokio::test]
    async fn rejects_empty_task_ids() {
        use crate::types::resources::Resources;
        use std::sync::Arc;
        let tool = WaitTasksTool;
        let resources = Arc::new(tokio::sync::Mutex::new(Resources::new()));
        let err = Tool::run(
            &tool,
            test_ctx(resources),
            WaitTasksToolInput {
                task_ids: vec![],
                mode: WaitMode::WaitAll,
                timeout_ms: None,
            },
        )
        .await
        .unwrap_err();
        let s = format!("{err:?}");
        assert!(s.contains("empty") || s.contains("task_ids"));
    }

    #[tokio::test]
    async fn rejects_too_many_ids() {
        use crate::types::resources::Resources;
        use std::sync::Arc;
        let tool = WaitTasksTool;
        let resources = Arc::new(tokio::sync::Mutex::new(Resources::new()));
        let ids: Vec<String> = (0..=MAX_MULTI_WAIT_IDS).map(|i| format!("t{i}")).collect();
        let err = Tool::run(
            &tool,
            test_ctx(resources),
            WaitTasksToolInput {
                task_ids: ids,
                mode: WaitMode::WaitAll,
                timeout_ms: Some(1000),
            },
        )
        .await
        .unwrap_err();
        let s = format!("{err:?}");
        assert!(s.contains("maximum") || s.contains("exceeds"));
    }
}
