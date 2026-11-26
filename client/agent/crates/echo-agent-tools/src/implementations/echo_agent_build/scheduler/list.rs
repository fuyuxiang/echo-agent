use crate::types::requirements::{Expr, ToolRequirement};

use crate::types::tool::{ToolKind, ToolNamespace};

use super::interval::interval_to_human;
use super::types::{SchedulerCommand, SchedulerHandle};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SchedulerListInput {}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskSummary {
    pub id: String,
    pub prompt: String,
    pub interval_human: String,
    pub next_fire_at: String,
    pub created_at: String,
    pub recurring: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SchedulerListOutput {
    pub tasks: Vec<ScheduledTaskSummary>,
}

impl echo_agent_tool_runtime::ToolOutput for SchedulerListOutput {}

#[derive(Debug, Default)]
pub struct SchedulerListTool;

impl crate::types::tool_metadata::ToolMetadata for SchedulerListTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        "List all active scheduled tasks with their IDs, prompts, intervals, and next fire times."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        use super::create::SchedulerCreateTool;
        use crate::types::tool_metadata::ToolMetadata as TM;
        Expr::Value(ToolRequirement::Tool {
            namespace: TM::tool_namespace(&SchedulerCreateTool).to_string(),
            id: echo_agent_tool_runtime::Tool::id(&SchedulerCreateTool).to_string(),
            if_params: None,
        })
    }
}

impl echo_agent_tool_runtime::Tool for SchedulerListTool {
    type Args = SchedulerListInput;
    type Output = SchedulerListOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("scheduler_list").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "scheduler_list",
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

    #[tracing::instrument(name = "tool.scheduler_list", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        _input: SchedulerListInput,
    ) -> Result<SchedulerListOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let sender = {
            let res = resources.lock().await;
            res.get::<SchedulerHandle>()
                .ok_or_else(|| {
                    echo_agent_tool_runtime::ToolError::custom(
                        "missing_dependency",
                        "missing dependency: SchedulerHandle",
                    )
                })?
                .0
                .clone()
        };

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        sender
            .send(SchedulerCommand::List { reply: reply_tx })
            .map_err(|_| {
                echo_agent_tool_runtime::ToolError::execution(
                    echo_agent_tool_protocol::ToolId::new("scheduler_list").expect("valid"),
                    "Scheduler actor stopped",
                )
            })?;

        let snapshot = reply_rx.await.map_err(|_| {
            echo_agent_tool_runtime::ToolError::execution(
                echo_agent_tool_protocol::ToolId::new("scheduler_list").expect("valid"),
                "Scheduler actor dropped reply",
            )
        })?;

        let summaries = snapshot
            .tasks
            .into_iter()
            .map(|t| {
                let next_fire = t.next_fire_at().to_rfc3339();
                let created = t.created_at.to_rfc3339();
                let prompt = if t.prompt.len() > 80 {
                    let cut = crate::util::floor_char_boundary(&t.prompt, 80);
                    format!("{}...", &t.prompt[..cut])
                } else {
                    t.prompt
                };
                ScheduledTaskSummary {
                    id: t.id,
                    prompt,
                    interval_human: interval_to_human(t.interval_secs),
                    next_fire_at: next_fire,
                    created_at: created,
                    recurring: t.recurring,
                }
            })
            .collect();

        Ok(SchedulerListOutput { tasks: summaries })
    }
}
