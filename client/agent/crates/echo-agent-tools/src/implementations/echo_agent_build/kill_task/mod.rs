//! `kill_task` tool — new architecture (`Tool` trait).
//!
//! Terminates a running background task. Reads the `Terminal` resource
//! from Resources to access the terminal backend.

pub mod terminal_command;
pub use terminal_command::KillTerminalCommandTool;

use crate::computer::types::{KillOutcome, KillSource};
use crate::implementations::echo_agent_build::task::TaskTool;
use crate::implementations::echo_agent_build::task::backend::SubagentBackendResource;
use crate::implementations::echo_agent_build::task::types::SubagentCancelOutcome;
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::Terminal;
use crate::types::template_renderer::TemplateRenderer;
use crate::types::tool::ToolKind;
use crate::types::tool::ToolNamespace;
use echo_agent_tool_types::{KillTaskOutput, KillTaskResult, KillTaskToolInput};

// ───────────────────────────────────────────────────────────────────────────
// Tool implementation
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct KillTaskTool;

// ── Legacy message helpers ───────────────────────────────────────────────
//
// Historical fixture captured from an earlier (0.4.10) revision of this tool.
//
// In 0.4.10, kill_task returned:
//   Err(ToolError::ProcessManagerError(format!("Task {} not found", input.task_id)))
//
// The meaningful customer-facing message content is the inner string.
// Subagent wording is out of scope — subagents didn't exist in 0.4.10.

/// Exact historical not-found message for `kill_task` in legacy-0.4.10.
fn render_legacy_kill_task_not_found(task_id: &str) -> String {
    format!("Task {} not found", task_id)
}

/// Format a "not found" response for kill_task.
async fn not_found_response(
    task_id: &str,
    terminal: &std::sync::Arc<dyn crate::computer::types::TerminalBackend>,
    is_legacy: bool,
) -> KillTaskOutput {
    if is_legacy {
        // Legacy: simple error without task ID enumeration.
        // Subagent wording is out of scope — subagents didn't exist in 0.4.10.
        return KillTaskOutput::TaskNotFound(render_legacy_kill_task_not_found(task_id));
    }
    // Current: include known task IDs for discoverability.
    let known = terminal.list_tasks().await;
    let msg = if known.is_empty() {
        format!(
            "Task or subagent {task_id} not found. No background tasks or subagents exist in this session.",
        )
    } else {
        let ids: Vec<&str> = known.iter().map(|t| t.task_id.as_str()).collect();
        format!(
            "Task or subagent {task_id} not found. Known bash task IDs: [{}]",
            ids.join(", ")
        )
    };
    KillTaskOutput::TaskNotFound(msg)
}

impl crate::types::tool_metadata::ToolMetadata for KillTaskTool {
    fn kind(&self) -> ToolKind {
        ToolKind::KillTaskAction
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        // Canonical wording lives in the shared builder; `versioned_definition`
        // renders it context-aware from the finalized toolset. This static
        // fallback mirrors the default echo-agent-build toolset on the current OS.
        static DESC: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            echo_agent_tool_types::build_kill_task_description(
                &echo_agent_tool_types::KillTaskToolNaming {
                    monitor_tool: Some("monitor"),
                    subagent_present: true,
                    bash_present: true,
                    is_windows: cfg!(not(unix)),
                    task_id_param: "task_id",
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
        let description = kill_task_description(renderer, description_override);
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
        use crate::types::tool_metadata::ToolMetadata as TM;
        let task_tool = Expr::Value(ToolRequirement::Tool {
            namespace: TM::tool_namespace(&TaskTool).to_string(),
            id: echo_agent_tool_runtime::Tool::id(&TaskTool).to_string(),
            if_params: None,
        });
        let mut arms =
            crate::implementations::echo_agent_build::task_output::background_bash_requires_exprs();
        arms.push(task_tool);
        Expr::Or(arms)
    }
}

/// Resolve the model-facing `kill_task` description from the finalized toolset,
/// honoring an explicit config override. Wording lives in the shared
/// [`echo_agent_tool_types::build_kill_task_description`] builder so the CLI and
/// prod-chat can't drift; the monitor / subagent / bash clauses follow the
/// tools actually registered this turn, and the kill verb follows the host OS.
fn kill_task_description(
    renderer: &TemplateRenderer,
    description_override: Option<&str>,
) -> String {
    if let Some(ovr) = description_override {
        return renderer.render(ovr).unwrap_or_else(|e| {
            tracing::warn!("kill_task description override render failed, using raw: {e}");
            ovr.to_string()
        });
    }
    echo_agent_tool_types::build_kill_task_description(&echo_agent_tool_types::KillTaskToolNaming {
        monitor_tool: renderer.tool_for_kind(ToolKind::Monitor),
        subagent_present: renderer.tool_for_kind(ToolKind::Task).is_some(),
        bash_present: renderer.tool_for_kind(ToolKind::Execute).is_some(),
        is_windows: cfg!(not(unix)),
        task_id_param: renderer
            .param_for_kind(ToolKind::KillTaskAction, "task_id")
            .unwrap_or("task_id"),
    })
}

impl echo_agent_tool_runtime::Tool for KillTaskTool {
    type Args = KillTaskToolInput;
    type Output = KillTaskOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("kill_task").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "kill_task",
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
        name = "tool.kill_task",
        skip_all,
        fields(task_id = %input.task_id)
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: KillTaskToolInput,
    ) -> Result<KillTaskOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let is_legacy = crate::versions::is_legacy_contract(
            crate::types::tool_metadata::behavior_version(&ctx).as_deref(),
        );
        let terminal;
        {
            let res = resources.lock().await;
            terminal = res.require::<Terminal>()?.0.clone();
        }

        match terminal
            .kill_task_with_source(&input.task_id, KillSource::ModelTool)
            .await
        {
            KillOutcome::Killed => Ok(KillTaskOutput::Result(KillTaskResult {
                task_id: input.task_id.clone(),
                outcome: "killed".to_string(),
                message: "Task was terminated successfully".to_string(),
            })),
            KillOutcome::AlreadyExited => Ok(KillTaskOutput::Result(KillTaskResult {
                task_id: input.task_id.clone(),
                outcome: "already_exited".to_string(),
                message: "Task had already completed".to_string(),
            })),
            KillOutcome::NotFound => {
                // Try subagent cancel via backend
                let backend = {
                    resources
                        .lock()
                        .await
                        .get::<SubagentBackendResource>()
                        .cloned()
                };
                if let Some(backend) = backend {
                    let outcome = backend.backend().cancel(&input.task_id).await;
                    return Ok(match outcome {
                        SubagentCancelOutcome::Cancelled => {
                            KillTaskOutput::Result(KillTaskResult {
                                task_id: input.task_id.clone(),
                                outcome: "killed".to_string(),
                                message: "Subagent cancellation initiated".to_string(),
                            })
                        }
                        SubagentCancelOutcome::AlreadyFinished { status } => {
                            KillTaskOutput::Result(KillTaskResult {
                                task_id: input.task_id.clone(),
                                outcome: "already_exited".to_string(),
                                message: format!("Subagent already {status}"),
                            })
                        }
                        SubagentCancelOutcome::NotFound => {
                            not_found_response(&input.task_id, &terminal, is_legacy).await
                        }
                    });
                }

                Ok(not_found_response(&input.task_id, &terminal, is_legacy).await)
            }
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer::types::{
        BackgroundHandle, KillOutcome as KO, TaskSnapshot, TerminalBackend, TerminalRunRequest,
        TerminalRunResult,
    };
    use crate::implementations::echo_agent_build::task::backend::{
        ChannelBackend, SubagentBackendResource,
    };
    use crate::implementations::echo_agent_build::task::types::{
        SubagentCancelRequest, SubagentCancelTarget, SubagentEvent,
    };
    use crate::types::resources::{Resources, SharedResources};
    use crate::types::tool_metadata::test_ctx_with_call_id;
    use std::sync::Arc;
    use std::time::Duration;

    fn make_ctx_with_version(
        call_id: &str,
        resources: SharedResources,
        version: &str,
    ) -> echo_agent_tool_runtime::ToolCallContext {
        let mut ctx = test_ctx_with_call_id(resources, call_id);
        ctx.extensions
            .insert(echo_agent_tool_runtime::BehaviorVersion(version.to_owned()));
        ctx
    }

    /// Minimal mock backend for testing kill_task.
    struct MockTerminal {
        /// Pre-configured outcome for `kill_task` calls.
        outcome: KO,
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

        async fn kill_task(&self, _task_id: &str) -> KO {
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

    fn resources_with_terminal(outcome: KO) -> Resources {
        let mut resources = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminal { outcome });
        resources.insert(Terminal(backend));
        resources
    }

    #[test]
    fn tool_name_and_description() {
        let tool = KillTaskTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "kill_task"
        );
        // The static fallback is the shared builder's default echo-agent-build
        // rendering (monitor + task + bash present) for the current OS.
        let desc = crate::types::tool_metadata::ToolMetadata::description_template(&tool);
        assert!(desc.contains("Terminate"));
        assert!(desc.contains("subagent"));
        // Must name "monitor" so the model connects stopping a monitor to this tool.
        assert!(desc.contains("monitor"));
        // The kill verb is OS-specific (SIGTERM on POSIX, Job Object on Windows).
        if cfg!(not(unix)) {
            assert!(desc.contains("Job Object"), "windows verb: {desc}");
        } else {
            assert!(desc.contains("SIGTERM"), "posix verb: {desc}");
        }
        assert!(
            !desc.contains("${"),
            "fallback must not leak template markers: {desc}"
        );
    }

    /// No dead references: `monitor` / `subagent` appear only when their tool is
    /// present. The context-aware `versioned_definition` path drives this via
    /// `kill_task_description`; render every subset (including none) and check.
    #[test]
    fn description_context_aware_for_tool_subsets() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;
        use std::collections::HashMap;

        let cases: &[(&str, &[(ToolKind, &str)])] = &[
            ("no tools", &[]),
            (
                "execute only",
                &[(ToolKind::Execute, "run_terminal_command")],
            ),
            ("monitor only", &[(ToolKind::Monitor, "monitor")]),
            ("task only", &[(ToolKind::Task, "spawn_subagent")]),
            (
                "execute + monitor",
                &[
                    (ToolKind::Execute, "run_terminal_command"),
                    (ToolKind::Monitor, "monitor"),
                ],
            ),
            (
                "execute + task",
                &[
                    (ToolKind::Execute, "run_terminal_command"),
                    (ToolKind::Task, "spawn_subagent"),
                ],
            ),
            (
                "monitor + task",
                &[
                    (ToolKind::Monitor, "monitor"),
                    (ToolKind::Task, "spawn_subagent"),
                ],
            ),
            (
                "execute + monitor + task",
                &[
                    (ToolKind::Execute, "run_terminal_command"),
                    (ToolKind::Monitor, "monitor"),
                    (ToolKind::Task, "spawn_subagent"),
                ],
            ),
        ];

        for (label, kinds) in cases {
            let tools: HashMap<ToolKind, String> =
                kinds.iter().map(|(k, n)| (*k, n.to_string())).collect();
            let renderer = TemplateRenderer::new(tools, HashMap::new());
            let rendered = kill_task_description(&renderer, None);

            println!("\n===== kill_task [{label}] =====\n{rendered}");

            let has_monitor = kinds.iter().any(|(k, _)| *k == ToolKind::Monitor);
            let has_task = kinds.iter().any(|(k, _)| *k == ToolKind::Task);

            assert!(
                !rendered.contains("${"),
                "[{label}] left an unrendered template marker:\n{rendered}"
            );
            assert!(
                !rendered.contains(" ,") && !rendered.contains(", ,") && !rendered.contains("()"),
                "[{label}] dangling punctuation:\n{rendered}"
            );
            assert!(
                rendered.contains("background task"),
                "[{label}] must always mention background task:\n{rendered}"
            );
            assert_eq!(
                rendered.contains("monitor"),
                has_monitor,
                "[{label}] monitor mention must match monitor-tool presence:\n{rendered}"
            );
            assert_eq!(
                rendered.contains("subagent"),
                has_task,
                "[{label}] subagent mention must match task-tool presence:\n{rendered}"
            );
        }
    }

    #[test]
    fn description_tracks_renamed_task_id() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;
        use std::collections::HashMap;

        let tools = HashMap::from([
            (ToolKind::Execute, "run_terminal_command".to_string()),
            (ToolKind::Monitor, "monitor".to_string()),
            (ToolKind::KillTaskAction, "kill_task".to_string()),
        ]);
        let params = HashMap::from([(
            ToolKind::KillTaskAction,
            HashMap::from([("task_id".to_string(), "id".to_string())]),
        )]);
        let rendered = kill_task_description(&TemplateRenderer::new(tools, params), None);
        assert!(
            rendered.contains("Pass its id (a monitor's id is returned by monitor)"),
            "renamed task_id must appear in pass-line and monitor aside:\n{rendered}"
        );
        assert!(
            !rendered.contains("task_id"),
            "canonical task_id must not remain after rename:\n{rendered}"
        );
    }

    /// The kill mechanism is OS-level: Windows describes Job Object termination,
    /// Unix/Git Bash describe SIGTERM/SIGKILL.
    #[test]
    fn kill_verb_matches_platform() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;
        use std::collections::HashMap;

        let tools: HashMap<ToolKind, String> =
            HashMap::from([(ToolKind::Execute, "run_terminal_command".to_string())]);
        let renderer = TemplateRenderer::new(tools, HashMap::new());
        let desc = kill_task_description(&renderer, None);

        if cfg!(not(unix)) {
            assert!(
                desc.contains("- Terminates the Job Object of a bash task"),
                "windows verb, got:\n{desc}"
            );
            assert!(
                !desc.contains("SIGTERM"),
                "Unix kill jargon leaked:\n{desc}"
            );
        } else {
            assert!(
                desc.contains("- Sends SIGTERM/SIGKILL to a bash task"),
                "posix verb, got:\n{desc}"
            );
            assert!(
                !desc.contains("Job Object"),
                "Windows jargon leaked:\n{desc}"
            );
        }
    }

    #[tokio::test]
    async fn kill_task_killed() {
        let resources = resources_with_terminal(KO::Killed);
        let tool = KillTaskTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "tool_call"),
            KillTaskToolInput {
                task_id: "task-1".into(),
            },
        )
        .await
        .unwrap();

        match result {
            KillTaskOutput::Result(r) => {
                assert_eq!(r.outcome, "killed");
                assert_eq!(r.task_id, "task-1");
            }
            other => panic!("Expected Success, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn kill_task_already_exited() {
        let resources = resources_with_terminal(KO::AlreadyExited);
        let tool = KillTaskTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "tool_call"),
            KillTaskToolInput {
                task_id: "task-2".into(),
            },
        )
        .await
        .unwrap();

        match result {
            KillTaskOutput::Result(r) => assert_eq!(r.outcome, "already_exited"),
            other => panic!("Expected Success, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn kill_task_not_found_returns_typed_output() {
        let resources = resources_with_terminal(KO::NotFound);
        let tool = KillTaskTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "tool_call"),
            KillTaskToolInput {
                task_id: "task-3".into(),
            },
        )
        .await
        .unwrap(); // Should be Ok, not Err

        match result {
            KillTaskOutput::TaskNotFound(msg) => {
                assert!(msg.contains("not found"), "message: {msg}");
                assert!(
                    msg.contains("task-3"),
                    "message should include task ID: {msg}"
                );
            }
            other => panic!("Expected TaskNotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn errors_when_terminal_not_in_resources() {
        let resources = Resources::new();
        let tool = KillTaskTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "tool_call"),
            KillTaskToolInput {
                task_id: "task-x".into(),
            },
        )
        .await;

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing required resource")
        );
    }

    // ── MP-3: Legacy message parity fixture tests ────────────────────────

    #[tokio::test]
    async fn legacy_kill_task_not_found_exact_historical_message() {
        let resources = resources_with_terminal(KO::NotFound);
        let tool = KillTaskTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            make_ctx_with_version("test-call", resources.into_shared(), "legacy-0.4.10"),
            KillTaskToolInput {
                task_id: "task-abc".into(),
            },
        )
        .await
        .unwrap();

        match result {
            KillTaskOutput::TaskNotFound(msg) => {
                // Exact historical fixture — no trailing period.
                assert_eq!(msg, "Task task-abc not found");
            }
            other => panic!("Expected TaskNotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn current_kill_task_not_found_includes_discoverability() {
        // Current (non-legacy) path must still include known task IDs
        // or "No background tasks or subagents exist" text.
        let resources = resources_with_terminal(KO::NotFound);
        let tool = KillTaskTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "test-call"),
            KillTaskToolInput {
                task_id: "task-abc".into(),
            },
        )
        .await
        .unwrap();

        match result {
            KillTaskOutput::TaskNotFound(msg) => {
                assert!(
                    msg.contains("No background tasks or subagents exist"),
                    "Current path must include discoverability text, got: {msg}"
                );
            }
            other => panic!("Expected TaskNotFound, got {:?}", other),
        }
    }

    // ── Subagent cancel via backend tests ─────────────────────────────

    /// Build resources with a terminal that returns `NotFound` and a
    /// `SubagentBackendResource` backed by channels, returning the cancel
    /// receiver so the test can simulate the coordinator.
    fn resources_with_backend_cancel() -> (
        Resources,
        tokio::sync::mpsc::UnboundedReceiver<SubagentEvent>,
    ) {
        let mut resources = Resources::new();
        let terminal: Arc<dyn TerminalBackend> = Arc::new(MockTerminal {
            outcome: KO::NotFound,
        });
        resources.insert(Terminal(terminal));

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
        resources.insert(SubagentBackendResource(Arc::new(ChannelBackend::new(tx))));

        (resources, rx)
    }

    fn unwrap_cancel(event: SubagentEvent) -> SubagentCancelRequest {
        match event {
            SubagentEvent::Cancel(r) => r,
            _ => panic!("Expected SubagentEvent::Cancel"),
        }
    }

    #[tokio::test]
    async fn kill_task_subagent_cancelled() {
        let (resources, mut cancel_rx) = resources_with_backend_cancel();
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let req = unwrap_cancel(cancel_rx.recv().await.unwrap());
            match &req.target {
                SubagentCancelTarget::SubagentId(id) => assert_eq!(id, "sub-1"),
                other => panic!("Expected SubagentId, got {:?}", other),
            }
            req.respond_to
                .send(SubagentCancelOutcome::Cancelled)
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &KillTaskTool,
            test_ctx_with_call_id(shared, "test-call"),
            KillTaskToolInput {
                task_id: "sub-1".into(),
            },
        )
        .await
        .unwrap();

        handle.await.unwrap();

        match result {
            KillTaskOutput::Result(r) => {
                assert_eq!(r.outcome, "killed");
                assert!(
                    r.message.contains("Subagent cancellation"),
                    "msg: {}",
                    r.message
                );
            }
            other => panic!("Expected Result(killed), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn kill_task_subagent_already_finished() {
        let (resources, mut cancel_rx) = resources_with_backend_cancel();
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let req = unwrap_cancel(cancel_rx.recv().await.unwrap());
            req.respond_to
                .send(SubagentCancelOutcome::AlreadyFinished {
                    status: "completed".to_string(),
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &KillTaskTool,
            test_ctx_with_call_id(shared, "test-call"),
            KillTaskToolInput {
                task_id: "sub-done".into(),
            },
        )
        .await
        .unwrap();

        handle.await.unwrap();

        match result {
            KillTaskOutput::Result(r) => {
                assert_eq!(r.outcome, "already_exited");
                assert!(
                    r.message.contains("Subagent already completed"),
                    "msg: {}",
                    r.message
                );
            }
            other => panic!("Expected Result(already_exited), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn kill_task_subagent_not_found_falls_through() {
        let (resources, mut cancel_rx) = resources_with_backend_cancel();
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let req = unwrap_cancel(cancel_rx.recv().await.unwrap());
            req.respond_to
                .send(SubagentCancelOutcome::NotFound)
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &KillTaskTool,
            test_ctx_with_call_id(shared, "test-call"),
            KillTaskToolInput {
                task_id: "sub-nope".into(),
            },
        )
        .await
        .unwrap();

        handle.await.unwrap();

        match result {
            KillTaskOutput::TaskNotFound(msg) => {
                assert!(msg.contains("not found"), "msg: {msg}");
            }
            other => panic!("Expected TaskNotFound, got {:?}", other),
        }
    }
}
