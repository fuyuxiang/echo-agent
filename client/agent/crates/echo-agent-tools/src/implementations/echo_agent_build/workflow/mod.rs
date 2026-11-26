use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};

use super::task::types::SubagentDepthCounter;

pub use echo_agent_tools_api::slash_commands::WORKFLOW_TOOL_NAME;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WorkflowToolInput {
    #[serde(default)]
    #[schemars(
        range(min = 1, max = 1024),
        description = "Absolute cumulative cap on logical child-agent calls for this run. Every agent() and every parallel() item consumes one slot; schema retries do not. Defaults to 128 and may be set from 1 through 1,024. A panel that would exceed the remaining budget is rejected before any of its children launch."
    )]
    pub agent_budget: Option<u64>,

    #[serde(default)]
    #[schemars(
        description = "Name of a registered workflow (built-in, or discovered from the project `.echo-agent/workflows/` or user `~/.echo-agent/workflows/`). Exactly one of `name`, `script`, or `script_path` must be set."
    )]
    pub name: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "Inline Rhai workflow script. It must start with a pure-literal `let meta = #{ name: ..., description: ... };` map. Before authoring, read the `create-workflow` skill's SKILL.md. Run the path-specific `validate_only` smoke check with representative args."
    )]
    pub script: Option<String>,

    #[serde(default)]
    #[schemars(description = "Path to a .rhai workflow script on disk.")]
    pub script_path: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "JSON value bound to the script's `args` global. Use an object for named arguments."
    )]
    pub args: Option<serde_json::Value>,

    #[serde(default)]
    #[schemars(
        description = "Resume a same-process paused run, continuing its original immutable script and args; do not also pass name, script, script_path, or args. A budget-limited run resumes only when agent_budget is passed with a higher cap. Process-restart interruptions are terminal."
    )]
    pub resume_from_run_id: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "Run a path-specific smoke check without launching: validate metadata, compile the full script, and execute the single path selected by the supplied args and canned host results. It does not exercise every branch or prove live tools and agent outputs work."
    )]
    pub validate_only: bool,
}

impl WorkflowToolInput {
    pub const MAX_AGENT_BUDGET: u64 = 1_024;

    pub fn normalize(&mut self) {
        self.name = blank_to_none(self.name.take());
        self.script = blank_to_none(self.script.take());
        self.script_path = blank_to_none(self.script_path.take());
        self.resume_from_run_id = blank_to_none(self.resume_from_run_id.take());
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(budget) = self.agent_budget {
            if budget == 0 {
                return Err("`agent_budget` must be a positive integer".into());
            }
            if budget > Self::MAX_AGENT_BUDGET {
                return Err(format!(
                    "`agent_budget` must be at most {} agents",
                    Self::MAX_AGENT_BUDGET
                ));
            }
        }
        let present = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
        let sources = [
            present(&self.name),
            present(&self.script),
            present(&self.script_path),
        ]
        .iter()
        .filter(|v| **v)
        .count();
        if present(&self.resume_from_run_id) {
            return match sources {
                0 => Ok(()),
                _ => Err(
                    "`resume_from_run_id` continues a same-process paused run's original immutable script and args; do not combine it with `name`, `script`, or `script_path`"
                        .into(),
                ),
            };
        }
        match sources {
            0 => Err("provide one of `name`, `script`, or `script_path`".into()),
            1 => Ok(()),
            _ => Err("`name`, `script`, and `script_path` are mutually exclusive".into()),
        }
    }
}

fn blank_to_none(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

#[derive(Debug)]
pub struct WorkflowLaunchRequest {
    pub input: WorkflowToolInput,
}

#[derive(Debug)]
pub enum WorkflowLaunchAck {
    Started {
        run_id: String,
        task_id: String,
        name: String,
        script_path: Option<String>,
    },
    Validated {
        name: String,
        phases: usize,
        summary: String,
    },
    Rejected {
        code: &'static str,
        detail: String,
    },
}

pub type WorkflowLaunchEnvelope = (
    WorkflowLaunchRequest,
    tokio::sync::oneshot::Sender<WorkflowLaunchAck>,
);

pub struct WorkflowLaunchHandle(pub tokio::sync::mpsc::UnboundedSender<WorkflowLaunchEnvelope>);

impl std::fmt::Debug for WorkflowLaunchHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowLaunchHandle").finish()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WorkflowToolOutput {
    pub run_id: String,
    #[schemars(
        description = "Alias of run_id; workflow runs are not background tasks — do not pass to task_output/wait_tasks. Completion notifies automatically."
    )]
    pub task_id: String,
    #[schemars(
        description = "The session-unique display handle for this run, such as review-changes or review-changes-2. Use it in user-facing status and /workflow management; keep run_id internal."
    )]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_path: Option<String>,
    pub message: String,
}

impl echo_agent_tool_runtime::ToolOutput for WorkflowToolOutput {}

#[derive(Debug, Default)]
pub struct WorkflowTool;

impl crate::types::tool_metadata::ToolMetadata for WorkflowTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Workflow
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        r##"Launch a workflow: a Rhai script that orchestrates subagents as one background run. Provide exactly one source: `name` (a registered workflow — built-in, or from the project `.echo-agent/workflows/` or user `~/.echo-agent/workflows/`), an inline `script`, or a `script_path`. Optionally pass `args` (bound to the script's `args`) and `agent_budget`, an absolute cap on cumulative child-agent calls: every agent() and parallel() item consumes one slot (schema retries do not); default 128. The host also caps live children per run (32 by default, host-configured) — larger parallel() panels are queued and still act as a barrier. The call returns immediately; progress appears in `/workflow runs`${%- if system_reminders_enabled %} and completion is reported automatically — do not poll or sleep-wait${%- endif %}.

Prefer a registered workflow when one fits; author a script for bounded fan-out over a known work list, staged research and verification, or several independent perspectives. Before writing or editing a script, read the `create-workflow` skill's SKILL.md. `validate_only: true` runs a path-specific smoke check (metadata, compile, one canned-host path) — not proof that every branch or live tool works.

A started run gets a session-unique display name (e.g. `review-changes`, `review-changes-2`) — the handle to show the user and use with `/workflow pause|resume|stop <name>`; keep run IDs internal. Each launch persists an editable `script_path`; edit it and launch as a new run to iterate. Use `resume_from_run_id` only for a same-process paused run (process restarts are terminal); a budget-limited run resumes only with a higher `agent_budget`. Save reusable scripts to `.echo-agent/workflows/<name>.rhai`."##
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

impl echo_agent_tool_runtime::Tool for WorkflowTool {
    type Args = WorkflowToolInput;
    type Output = WorkflowToolOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new(WORKFLOW_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            WORKFLOW_TOOL_NAME,
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

    #[tracing::instrument(name = "new_tool.workflow", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        mut input: WorkflowToolInput,
    ) -> Result<WorkflowToolOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        input.normalize();

        if let Err(detail) = input.validate() {
            return Err(echo_agent_tool_runtime::ToolError::custom(
                "workflow_invalid_input",
                detail,
            ));
        }

        let (depth, sender) = {
            let res = resources.lock().await;
            let depth = res.get::<SubagentDepthCounter>().map(|d| d.0).unwrap_or(0);
            let sender = res.get::<WorkflowLaunchHandle>().map(|h| h.0.clone());
            (depth, sender)
        };

        // Workflows stay top-level-only regardless of configurable subagent depth.
        if depth > 0 {
            return Err(echo_agent_tool_runtime::ToolError::custom(
                "workflow_depth_exceeded",
                "Workflows can only be launched from a top-level session (subagents and \
                 workflow-spawned agents cannot start workflows)",
            ));
        }

        let sender = sender.ok_or_else(|| {
            echo_agent_tool_runtime::ToolError::custom(
                "workflow_not_available",
                "Workflow launching is not available in this session (WorkflowLaunchHandle not \
                 registered)",
            )
        })?;

        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel::<WorkflowLaunchAck>();
        sender
            .send((WorkflowLaunchRequest { input }, ack_tx))
            .map_err(|_| {
                echo_agent_tool_runtime::ToolError::custom(
                    "workflow_channel_closed",
                    "Workflow launch channel closed — the session may be shutting down",
                )
            })?;

        match ack_rx.await {
            Ok(WorkflowLaunchAck::Started {
                run_id,
                task_id,
                name,
                script_path,
            }) => Ok(WorkflowToolOutput {
                message: {
                    let iterate = script_path
                        .as_deref()
                        .map(|p| {
                            format!(
                                " The editable script projection is at {p}. Edit it and launch \
                                 that `script_path` as a new run to iterate; same-process pause \
                                 resume continues only this run's original immutable source."
                            )
                        })
                        .unwrap_or_default();
                    format!(
                        "Workflow '{name}' started in the background. Progress appears in \
                         /workflow runs and completion is reported automatically. '{name}' is \
                         the session-unique display handle for user-facing status and /workflow \
                         management; keep the structured run id internal.{iterate}"
                    )
                },
                run_id,
                task_id,
                name,
                script_path,
            }),
            Ok(WorkflowLaunchAck::Validated {
                name,
                phases,
                summary,
            }) => Ok(WorkflowToolOutput {
                message: format!(
                    "Smoke check passed for workflow '{name}' ({phases} declared phases; \
                     canned-host path {summary}). This did not launch the workflow and did not \
                     exercise every branch or live dependency. Offer a real run next."
                ),
                run_id: String::new(),
                task_id: String::new(),
                name,
                script_path: None,
            }),
            Ok(WorkflowLaunchAck::Rejected { code, detail }) => {
                Err(echo_agent_tool_runtime::ToolError::custom(code, detail))
            }
            Err(_) => Err(echo_agent_tool_runtime::ToolError::custom(
                "workflow_launch_no_ack",
                "The session dropped the launch channel before answering; the workflow may not \
                 have started.",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_requires_exactly_one_source_and_bounded_positive_budget() {
        let base = WorkflowToolInput {
            agent_budget: None,
            name: None,
            script: None,
            script_path: None,
            args: None,
            resume_from_run_id: None,
            validate_only: false,
        };
        assert!(base.validate().is_err());

        let named = WorkflowToolInput {
            name: Some("deep-research".into()),
            ..base.clone()
        };
        assert!(named.validate().is_ok());

        let both = WorkflowToolInput {
            name: Some("goal".into()),
            script: Some("let meta = #{};".into()),
            ..base.clone()
        };
        assert!(both.validate().is_err());

        let resume_only = WorkflowToolInput {
            resume_from_run_id: Some("wf_123".into()),
            ..base.clone()
        };
        assert!(resume_only.validate().is_ok());

        let edited_resume = WorkflowToolInput {
            script_path: Some("edited.rhai".into()),
            resume_from_run_id: Some("wf_123".into()),
            ..base.clone()
        };
        assert!(edited_resume.validate().is_err());
        assert!(
            WorkflowToolInput {
                agent_budget: Some(10),
                resume_from_run_id: Some("wf_123".into()),
                name: None,
                ..base.clone()
            }
            .validate()
            .is_ok()
        );

        assert!(
            WorkflowToolInput {
                agent_budget: Some(0),
                name: Some("deep-research".into()),
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            WorkflowToolInput {
                agent_budget: Some(WorkflowToolInput::MAX_AGENT_BUDGET + 1),
                name: Some("deep-research".into()),
                ..base.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            WorkflowToolInput {
                agent_budget: Some(1),
                name: Some("deep-research".into()),
                ..base.clone()
            }
            .validate()
            .is_ok()
        );
        assert!(
            WorkflowToolInput {
                agent_budget: Some(1),
                script: Some("let meta = #{};".into()),
                validate_only: true,
                ..base
            }
            .validate()
            .is_ok()
        );
    }
}
