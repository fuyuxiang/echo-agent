use crate::types::requirements::{Expr, ToolRequirement};

use crate::types::tool::{ToolKind, ToolNamespace};

use super::interval::{interval_to_human, parse_interval};
use super::types::{ScheduledTask, SchedulerCommand, SchedulerHandle, scheduler_tool_error};

// Canonical /loop wording lives in the light API crate so other consumers can
// link it without the tools implementation crate; re-exported to keep paths stable.
pub use echo_agent_tools_api::slash_commands::{
    LoopFireMode, SCHEDULER_CREATE_TOOL_NAME, loop_schedule_instruction, loop_usage_message,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SchedulerCreateInput {
    #[serde(default)]
    #[schemars(
        description = "Id of an existing task to update in place: provided fields replace old \
                       values, omitted ones are unchanged, the schedule keeps its phase, and an \
                       unknown id errors. Omit to create a task."
    )]
    pub task_id: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "Interval between executions, e.g. \"5m\", \"2h\", \"1d\". \
                       Required to create; optional with task_id"
    )]
    pub interval: Option<String>,

    #[serde(default)]
    #[schemars(description = "The prompt text to execute on each scheduled fire. \
                       Required to create; optional with task_id")]
    pub prompt: Option<String>,

    #[serde(
        default = "default_true",
        deserialize_with = "crate::types::schema::deserialize_lenient_bool"
    )]
    #[schemars(skip)]
    pub recurring: bool,

    /// Whether the task persists across sessions. Default false (session-only).
    #[serde(
        default,
        deserialize_with = "crate::types::schema::deserialize_lenient_option_bool"
    )]
    #[schemars(
        description = "Whether the task persists across sessions. Default: false. \
                       Create-only: ignored with task_id"
    )]
    pub durable: Option<bool>,

    #[serde(
        default,
        deserialize_with = "crate::types::schema::deserialize_lenient_option_bool"
    )]
    #[schemars(
        description = "Run each fire as a main-conversation turn instead of a background \
                       subagent; set true only when runs need the conversation's context. \
                       Default: false. Create-only: ignored with task_id"
    )]
    pub foreground: Option<bool>,

    /// Whether to fire immediately on creation. Default false (wait for the
    /// first interval — a "scheduled" task should not run on creation unless
    /// explicitly asked to).
    #[serde(
        default,
        deserialize_with = "crate::types::schema::deserialize_lenient_bool"
    )]
    #[schemars(
        description = "Whether to fire immediately on creation (true) or wait for the first \
                       interval (false). Default: false. Create-only: ignored with task_id"
    )]
    pub fire_immediately: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerCreateOutput {
    pub id: String,
    pub human_schedule: String,
    #[serde(default)]
    pub updated: bool,
}

impl echo_agent_tool_runtime::ToolOutput for SchedulerCreateOutput {}

#[derive(Debug, Default)]
pub struct SchedulerCreateTool;

impl crate::types::tool_metadata::ToolMetadata for SchedulerCreateTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        // Formatted once so the TTL copy is derived from RECURRING_TASK_TTL_DAYS instead of
        // being pinned by a duplicate literal.
        static DESCRIPTION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            format!(
                r#"Create a scheduled task that runs a prompt on a recurring interval, or update an existing one in place.

Set fire_immediately: true to also fire once on creation; by default the first run waits for the interval.

To change an existing task, pass its task_id: provided fields replace old values, omitted ones are unchanged, and the schedule keeps its phase. An unknown id errors.

Usage notes:
- Interval format: "5m" (minutes), "2h" (hours), "1d" (days), "60s" (seconds, min 60)
- Maximum 50 scheduled tasks at once
- Tasks auto-expire after {} days
- For one-time delayed work, run a background terminal command (e.g. `sleep 1800 && <command>`) instead; its completion notifies you"#,
                super::types::RECURRING_TASK_TTL_DAYS
            )
        });
        &DESCRIPTION
        // TODO: scheduler tools share ToolKind::Other so they can't be template-ized
        // via ${{ tools.by_kind.* }}. If tool name randomization is needed, add
        // dedicated ToolKind variants (SchedulerCreate, SchedulerDelete, SchedulerList).
    }

    fn emitted_notifications(&self) -> &'static [&'static str] {
        &["ScheduledTaskCreated"]
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for SchedulerCreateTool {
    type Args = SchedulerCreateInput;
    type Output = SchedulerCreateOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new(SCHEDULER_CREATE_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "scheduler_create",
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
        name = "tool.scheduler_create",
        skip_all,
        fields(interval = input.interval.as_deref().unwrap_or(""), task_id = input.task_id.as_deref().unwrap_or(""))
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: SchedulerCreateInput,
    ) -> Result<SchedulerCreateOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let interval_secs = input
            .interval
            .as_deref()
            .map(parse_interval)
            .transpose()
            .map_err(|e| echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string()))?;

        let sender = {
            let res = resources.lock().await;
            res.get::<SchedulerHandle>()
                .ok_or_else(|| {
                    echo_agent_tool_runtime::ToolError::custom(
                        "missing_resource",
                        "SchedulerHandle",
                    )
                })?
                .0
                .clone()
        };

        let send_and_wait = |cmd: SchedulerCommand,
                             reply_rx: tokio::sync::oneshot::Receiver<
            Result<ScheduledTask, super::types::SchedulerError>,
        >| async move {
            sender.send(cmd).map_err(|_| {
                echo_agent_tool_runtime::ToolError::custom(
                    "process_manager",
                    "Scheduler actor stopped",
                )
            })?;
            reply_rx
                .await
                .map_err(|_| {
                    echo_agent_tool_runtime::ToolError::custom(
                        "process_manager",
                        "Scheduler actor dropped reply",
                    )
                })?
                .map_err(scheduler_tool_error)
        };

        if let Some(task_id) = input.task_id {
            if input.prompt.is_none() && interval_secs.is_none() {
                return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                    "nothing to update: provide interval and/or prompt alongside task_id",
                ));
            }
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            let updated = send_and_wait(
                SchedulerCommand::Update {
                    id: task_id,
                    prompt: input.prompt,
                    interval_secs,
                    reply: reply_tx,
                },
                reply_rx,
            )
            .await?;

            return Ok(SchedulerCreateOutput {
                id: updated.id,
                human_schedule: interval_to_human(updated.interval_secs),
                updated: true,
            });
        }

        if !input.recurring {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                "one-shot tasks are not supported; run a background terminal command instead \
                 (`sleep <secs> && <command>`, background: true) or do the work now",
            ));
        }

        let interval_secs = interval_secs.ok_or_else(|| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(
                "interval is required when creating a task",
            )
        })?;
        let prompt = input.prompt.ok_or_else(|| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(
                "prompt is required when creating a task",
            )
        })?;

        let durable = input.durable.unwrap_or(false);
        let mut task = ScheduledTask::with_fire_immediately(
            interval_secs,
            prompt,
            true,
            durable,
            input.fire_immediately,
        );
        task.foreground = input.foreground.unwrap_or(false);

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let created = send_and_wait(
            SchedulerCommand::Create {
                task,
                reply: reply_tx,
            },
            reply_rx,
        )
        .await?;

        Ok(SchedulerCreateOutput {
            id: created.id,
            human_schedule: interval_to_human(interval_secs),
            updated: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::implementations::echo_agent_build::scheduler::actor::SchedulerActor;
    use crate::notification::types::ToolNotificationHandle;
    use crate::types::resources::{Resources, SharedResources, State};
    use crate::types::tool_metadata::test_ctx;
    use echo_agent_tool_runtime::Tool;

    fn scheduler_resources() -> (SharedResources, tokio_util::sync::CancellationToken) {
        let mut resources = Resources::new();
        resources.register_state::<super::super::types::SchedulerState>();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        resources.insert(SchedulerHandle(cmd_tx));
        let shared = resources.into_shared();

        let (notif_handle, _notif_rx) = ToolNotificationHandle::channel();
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let actor = SchedulerActor {
            resources: shared.clone(),
            resources_persistence: std::sync::Arc::new(
                crate::persistence::ResourcesPersistence::noop(),
            ),
            notification_handle: notif_handle,
            cmd_rx,
            cancel_token: cancel_token.clone(),
            clock: Default::default(),
            pending_removal: None,
            blocked_expiries: Default::default(),
        };
        tokio::spawn(actor.run());
        (shared, cancel_token)
    }

    fn input(json: serde_json::Value) -> SchedulerCreateInput {
        serde_json::from_value(json).expect("valid input json")
    }

    async fn task_count(resources: &SharedResources) -> usize {
        let res = resources.lock().await;
        res.get::<State<super::super::types::SchedulerState>>()
            .map(|s| s.tasks.len())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn create_requires_interval_and_prompt() {
        let (resources, cancel) = scheduler_resources();

        let err = SchedulerCreateTool
            .run(test_ctx(resources.clone()), input(serde_json::json!({})))
            .await
            .expect_err("create without interval must fail");
        assert!(err.to_string().contains("interval is required"));

        let err = SchedulerCreateTool
            .run(
                test_ctx(resources.clone()),
                input(serde_json::json!({"interval": "5m"})),
            )
            .await
            .expect_err("create without prompt must fail");
        assert!(err.to_string().contains("prompt is required"));

        assert_eq!(task_count(&resources).await, 0);
        cancel.cancel();
    }

    #[tokio::test]
    async fn recurring_false_errors_with_sleep_guidance() {
        let (resources, cancel) = scheduler_resources();

        let err = SchedulerCreateTool
            .run(
                test_ctx(resources.clone()),
                input(serde_json::json!({
                    "interval": "5m", "prompt": "check", "recurring": false
                })),
            )
            .await
            .expect_err("one-shot must be rejected");
        assert!(err.to_string().contains("sleep"), "steers to sleep: {err}");
        assert_eq!(task_count(&resources).await, 0);
        cancel.cancel();
    }

    #[tokio::test]
    async fn update_unknown_task_id_errors_and_never_creates() {
        let (resources, cancel) = scheduler_resources();

        let err = SchedulerCreateTool
            .run(
                test_ctx(resources.clone()),
                input(serde_json::json!({
                    "task_id": "nonexistent", "prompt": "new prompt"
                })),
            )
            .await
            .expect_err("unknown id must error");
        assert!(err.to_string().contains("no scheduled task with id"));
        assert_eq!(
            task_count(&resources).await,
            0,
            "strict update must not fall back to create"
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn update_ignores_legacy_recurring_flag() {
        let (resources, cancel) = scheduler_resources();

        let created = SchedulerCreateTool
            .run(
                test_ctx(resources.clone()),
                input(serde_json::json!({"interval": "5m", "prompt": "check deploy"})),
            )
            .await
            .expect("create succeeds");

        let updated = SchedulerCreateTool
            .run(
                test_ctx(resources.clone()),
                input(serde_json::json!({
                    "task_id": created.id, "interval": "10m", "recurring": false
                })),
            )
            .await
            .expect("update succeeds despite legacy flag");
        assert!(updated.updated);
        assert_eq!(updated.human_schedule, "every 10 minutes");
        cancel.cancel();
    }

    #[tokio::test]
    async fn update_with_no_patch_fields_errors() {
        let (resources, cancel) = scheduler_resources();

        let err = SchedulerCreateTool
            .run(
                test_ctx(resources.clone()),
                input(serde_json::json!({"task_id": "abc123"})),
            )
            .await
            .expect_err("empty patch must error");
        assert!(err.to_string().contains("nothing to update"));
        cancel.cancel();
    }

    #[tokio::test]
    async fn create_then_update_patches_in_place() {
        let (resources, cancel) = scheduler_resources();

        let created = SchedulerCreateTool
            .run(
                test_ctx(resources.clone()),
                input(serde_json::json!({"interval": "5m", "prompt": "check deploy"})),
            )
            .await
            .expect("create succeeds");
        assert!(!created.updated);
        assert_eq!(created.human_schedule, "every 5 minutes");

        let updated = SchedulerCreateTool
            .run(
                test_ctx(resources.clone()),
                input(serde_json::json!({"task_id": created.id, "interval": "10m"})),
            )
            .await
            .expect("update succeeds");
        assert!(updated.updated);
        assert_eq!(updated.id, created.id, "identity preserved");
        assert_eq!(updated.human_schedule, "every 10 minutes");
        assert_eq!(task_count(&resources).await, 1, "no second task");
        cancel.cancel();
    }

    #[test]
    fn schema_hides_recurring_and_advertises_task_id() {
        let schema = schemars::schema_for!(SchedulerCreateInput);
        let json = serde_json::to_string(&schema).unwrap();
        assert!(
            !json.contains("recurring"),
            "recurring must not be advertised: {json}"
        );
        assert!(json.contains("task_id"));
    }

    #[test]
    fn loop_usage_message_has_no_host_default() {
        let usage = loop_usage_message();
        assert!(usage.contains("Usage: /loop"));
        assert!(
            !usage.contains("10m"),
            "usage must not claim a default: {usage}"
        );
    }

    #[test]
    fn loop_schedule_instruction_holds_invariants() {
        let args = "every 30 minutes do x";
        let instr = loop_schedule_instruction(args, LoopFireMode::Detached);
        assert!(
            !instr.contains("10m"),
            "instruction must not default: {instr}"
        );
        assert!(instr.contains("Deriving the interval"));
        assert!(instr.contains("<number><unit>"));
        assert!(instr.contains("ask the user how often"));
        assert!(instr.contains("Do NOT execute the prompt inline"));
        // Raw request forwarded verbatim for the model to parse.
        assert!(instr.contains(args));
    }
}
