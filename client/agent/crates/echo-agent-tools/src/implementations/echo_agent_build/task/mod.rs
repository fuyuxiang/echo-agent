//! `task` tool — launches a subagent to handle a task autonomously.
//!
//! The TaskTool delegates subagent operations to a [`SubagentBackend`]
//! (injected as [`SubagentBackendResource`]). The backend abstracts over the
//! coordinator mailbox. All hosts use the same backend and coordinator actor;
//! only their child runners differ.
//!
//! ## Resources
//!
//! - `SubagentBackendResource` — backend for spawn/query/cancel (required)
//! - `SubagentDepthCounter` — current nesting depth (optional, defaults to 0)
//! - `MaxSubagentDepth` — max nesting (optional, defaults to [`MAX_SUBAGENT_DEPTH`])
//! - `SessionIdResource` — current session ID for parent scoping (optional)
//! - `SubagentForegroundWait` — host wait-window guard factory (optional)
//! - `TaskModelValidator` — validates explicit model slugs before spawn

pub mod admission;
pub mod backend;
pub mod coordinator;
mod coordinator_state;
pub use coordinator_state::{cap_completion_output, completion_summary};
pub mod types;

use self::backend::SubagentBackendResource;
use self::types::CurrentPromptIdResource;

use self::types::*;
use crate::types::output::ToolOutput;
use crate::types::requirements::{Expr, ToolRequirement};
#[allow(unused_imports)]
use crate::types::resources::{SessionFolder, SharedResources};
use crate::types::tool::{ToolKind, ToolNamespace};
use echo_agent_tool_types::{SubagentCompletedOutput, SubagentIsolationMode, TaskToolInput};
use regex::Regex;

pub const TASK_TOOL_NAME: &str = "task";

/// Host-set immutable turn policy: children must complete before the parent
/// turn ends instead of escaping through background delivery / await budgets.
#[derive(Clone, Copy, Default)]
pub struct BoundSubagentToTurn(pub bool);

/// Default max nesting depth when [`MaxSubagentDepth`] is not injected.
pub const MAX_SUBAGENT_DEPTH: u32 = 1;

pub fn effective_max_subagent_depth(resources: &crate::types::resources::Resources) -> u32 {
    resources
        .get::<MaxSubagentDepth>()
        .map(|d| d.0)
        .unwrap_or(MAX_SUBAGENT_DEPTH)
}

fn user_text_from_json(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| {
                b.get("text")
                    .and_then(|t| t.as_str())
                    .map(str::to_string)
                    .or_else(|| b.as_str().map(str::to_string))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn normalize_user_ask(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(start) = t.find("<user_query>") {
        let after = &t[start + "<user_query>".len()..];
        let body = after.split("</user_query>").next().unwrap_or(after).trim();
        if body.is_empty() {
            return None;
        }
        return Some(body.to_string());
    }
    if t.starts_with("<system")
        || t.starts_with("<user_info>")
        || t.starts_with("<git_status>")
        || t.starts_with("<open_and_recently")
    {
        return None;
    }
    Some(t.to_string())
}

/// Recent parent user asks from `chat_history.jsonl` (oldest → newest).
async fn recent_user_asks(resources: &SharedResources) -> Vec<String> {
    let folder = {
        let guard = resources.lock().await;
        guard.get::<SessionFolder>().map(|s| s.0.clone())
    };
    let Some(folder) = folder else {
        return Vec::new();
    };
    let path = folder.join("chat_history.jsonl");
    let Ok(text) = tokio::fs::read_to_string(path).await else {
        return Vec::new();
    };
    let mut asks = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = v
            .get("type")
            .or_else(|| v.get("role"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if kind != "user" {
            continue;
        }
        // Auto-continue / stop-hook / recovery injections are typed `user`
        // but are not parent asks. Counting them burns lookback and can
        // drop leftover exec outside the window (common after compaction).
        if v.get("synthetic_reason").is_some_and(|x| !x.is_null()) {
            continue;
        }
        let Some(content) = v.get("content") else {
            continue;
        };
        if let Some(ask) = normalize_user_ask(&user_text_from_json(content)) {
            asks.push(ask);
        }
    }
    const KEEP: usize = 12;
    if asks.len() > KEEP {
        asks[asks.len() - KEEP..].to_vec()
    } else {
        asks
    }
}

async fn detect_continue_parent_work(
    resources: &SharedResources,
    description: &str,
    prompt: &str,
) -> bool {
    let asks = recent_user_asks(resources).await;
    echo_agent_tool_types::should_continue_parent_work(&asks, description, prompt)
}

/// Resolve the model-facing get-output tool and param names for the
/// background notices. Kind-wide resolution is correct here: these name the
/// retrieval tool's schema (which the host may rename), not task's own.
async fn resolve_background_notice_names(resources: &SharedResources) -> (String, String, String) {
    let canonical = echo_agent_tool_types::BackgroundNoticeNaming::CANONICAL;
    let res = resources.lock().await;
    let Some(renderer) = res.get::<crate::types::template_renderer::TemplateRenderer>() else {
        return (
            canonical.task_output_tool.to_string(),
            canonical.task_ids_param.to_string(),
            canonical.timeout_ms_param.to_string(),
        );
    };
    (
        renderer
            .tool_for_kind(ToolKind::BackgroundTaskAction)
            .unwrap_or(canonical.task_output_tool)
            .to_string(),
        renderer
            .param_for_kind(ToolKind::BackgroundTaskAction, "task_ids")
            .unwrap_or(canonical.task_ids_param)
            .to_string(),
        renderer
            .param_for_kind(ToolKind::BackgroundTaskAction, "timeout_ms")
            .unwrap_or(canonical.timeout_ms_param)
            .to_string(),
    )
}

// ───────────────────────────────────────────────────────────────────────────
// Tool implementation
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct TaskTool;

/// True when `name` is a wire name of the subagent-spawn ("task") tool.
///
/// Accepts every spelling regardless of enabled features: names arrive over
/// the wire from arbitrary toolsets. Spellings other than [`TASK_TOOL_NAME`]
/// are defined downstream and pinned to this predicate by tests at their
/// definition sites.
pub fn is_task_tool_id(name: &str) -> bool {
    matches!(name, TASK_TOOL_NAME | "Task" | "spawn_subagent")
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

impl crate::types::tool_metadata::ToolMetadata for TaskTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Task
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        // EchoAgent Build normally supplies the description via
        // `ToolConfig::with_description(...)` using `build_task_description()`
        // in echo-agent-core/src/builder.rs (live subagent roster). But a
        // registration without an override must still ship a real
        // description, never a placeholder: default to the built-in roster
        // with templated tool/param names, resolved by the registry renderer
        // at finalize time.
        /// Wrap each `${{ tools.by_kind.X }}` token in an if/else so kinds
        /// absent from the registry render as the bare kind name instead of
        /// an empty slot ("read, , and plan"), mirroring the bare-kind
        /// fallback of `BuiltinSubagent::render_tools`.
        ///
        /// These guards sit inline in the roster, so they use the
        /// non-stripping `${% %}` form: `${%-` would eat the ", " before
        /// each token and render "has access to:read,grep".
        fn guard_kind_tokens(template: &str) -> String {
            static TOKEN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
                Regex::new(r"\$\{\{\s*tools\.by_kind\.([a-z_]+)\s*\}\}").expect("valid regex")
            });
            TOKEN
                .replace_all(template, |caps: &regex::Captures| {
                    let kind = &caps[1];
                    format!(
                        "${{% if tools.by_kind.{kind} %}}${{{{ tools.by_kind.{kind} }}}}\
                         ${{% else %}}{kind}${{% endif %}}"
                    )
                })
                .into_owned()
        }

        static DESC: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            let subagents: Vec<echo_agent_tool_types::SubagentDescriptor> =
                echo_agent_tool_types::BUILTIN_SUBAGENTS
                    .iter()
                    .map(|b| echo_agent_tool_types::SubagentDescriptor {
                        name: b.name.to_owned(),
                        description: b.description.to_owned(),
                        tools: Some(guard_kind_tokens(b.tools_template)),
                    })
                    .collect();
            echo_agent_tool_types::build_task_description(
                &subagents,
                &echo_agent_tool_types::TaskToolNaming {
                    task_tool: "${{ tools.by_kind.task }}",
                    subagent_type_param: "${{ params.task.subagent_type }}",
                    run_in_background_param: "${{ params.task.run_in_background }}",
                    resume_from_param: "${{ params.task.resume_from }}",
                    background_retrieval_tool: "${{ tools.by_kind.background_task_action }}",
                    isolation_param: "${{ params.task.isolation }}",
                },
            )
        });
        &DESC
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        // The task tool can only be used when both get_task_output
        // (BackgroundTaskAction) and kill_task (KillTaskAction) are present,
        // so the agent can manage background subagents it spawns.
        Expr::And(vec![
            Expr::Value(ToolRequirement::tool_kind(ToolKind::BackgroundTaskAction)),
            Expr::Value(ToolRequirement::tool_kind(ToolKind::KillTaskAction)),
        ])
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

impl echo_agent_tool_runtime::Tool for TaskTool {
    type Args = TaskToolInput;
    type Output = ToolOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new(TASK_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "task",
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
        name = "tool.task",
        skip_all,
        fields(
            subagent_type = %input.subagent_type,
        )
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: TaskToolInput,
    ) -> Result<ToolOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;
        let bounded = resources
            .lock()
            .await
            .get::<BoundSubagentToTurn>()
            .is_some_and(|policy| policy.0);
        let mut input = input;
        if bounded {
            input.run_in_background = false;
        }
        let tool_cancellation = ctx
            .get::<echo_agent_tool_runtime::Cancellation>()
            .map(|cancellation| cancellation.0.clone());

        // 1. Depth check
        let (
            depth,
            max_depth,
            backend,
            model_validator,
            parent_session_id,
            parent_prompt_id,
            foreground_wait,
        ) = {
            let res = resources.lock().await;

            let depth = res.get::<SubagentDepthCounter>().map(|d| d.0).unwrap_or(0);
            let max_depth = effective_max_subagent_depth(&res);

            let backend = res
                .get::<SubagentBackendResource>()
                .ok_or_else(|| {
                    echo_agent_tool_runtime::ToolError::custom(
                        "missing_resource",
                        "SubagentBackendResource (subagent support not initialized)",
                    )
                })?
                .clone();

            let model_validator = res.get::<TaskModelValidator>().cloned();

            let parent_session_id = res
                .get::<SessionIdResource>()
                .map(|s| s.0.clone())
                .unwrap_or_default();

            let parent_prompt_id = res
                .get::<CurrentPromptIdResource>()
                .map(|p| p.0.clone())
                .filter(|prompt_id| !prompt_id.is_empty());
            let foreground_wait = res.get::<SubagentForegroundWait>().cloned();

            (
                depth,
                max_depth,
                backend,
                model_validator,
                parent_session_id,
                parent_prompt_id,
                foreground_wait,
            )
        };

        if depth >= max_depth {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                format!(
                    "Subagent depth limit exceeded (current depth: {depth}, max: {max_depth}). \
                 Cannot spawn further nested subagents."
                ),
            ));
        }

        // Treat blank/empty/"null" resume_from as absent (models sometimes emit these).
        let resume_from = input.resume_from.and_then(|s| {
            let trimmed = s.trim();
            is_valid_resume_id(trimmed).then(|| trimmed.to_string())
        });

        // Model overrides are soft-ignored on resume (source model is always pinned).
        let model = echo_agent_tool_types::sanitize_optional_arg(input.model);
        let model = if resume_from.is_some() {
            if let Some(ref ignored) = model {
                tracing::debug!(
                    model = %ignored,
                    "ignoring model override because resume_from is set"
                );
            }
            None
        } else {
            model
        };

        // Treat blank/empty/"null" cwd as absent (models sometimes emit these).
        // Also strip stray surrounding quote characters and expand `~`.
        let cwd = input.cwd.as_deref().and_then(sanitize_cwd_value);

        // Validate mutual exclusion: cwd and isolation=worktree cannot both
        // be set. Both set the effective cwd — setting both is ambiguous.
        // However, if the cwd path doesn't exist as a real directory on disk,
        // the model likely passed a nonsense path — just clear it so worktree wins.
        let cwd = if cwd.is_some() && input.isolation == Some(SubagentIsolationMode::Worktree) {
            if cwd
                .as_deref()
                .is_some_and(|p| std::path::Path::new(p).is_dir())
            {
                return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                    "cwd and isolation=\"worktree\" are mutually exclusive. \
                     Use cwd to point the subagent at an existing directory, \
                     or isolation=\"worktree\" to create a new isolated worktree, \
                     but not both.",
                ));
            }
            // Non-existent path alongside worktree — clear it so worktree wins.
            tracing::debug!(
                cwd = %cwd.as_deref().unwrap_or(""),
                "clearing non-existent cwd path because isolation=worktree is set"
            );
            None
        } else {
            cwd
        };

        // Validate that cwd points to an existing directory (skip when resuming).
        if let Some(ref cwd_path) = cwd
            && resume_from.is_none()
        {
            let p = std::path::Path::new(cwd_path);
            if !p.is_dir() {
                let detail = if p.exists() {
                    format!("cwd \"{cwd_path}\" exists but is not a directory")
                } else {
                    format!("cwd \"{cwd_path}\" does not exist")
                };
                return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                    detail,
                ));
            }
        }

        // 2. Eager validation — catch unknown / disabled / not-allowed
        //    types before the fire-and-forget background spawn.
        match backend
            .backend()
            .validate_type(&input.subagent_type, &parent_session_id)
            .await
        {
            SubagentValidateTypeOutcome::Ok => {}
            SubagentValidateTypeOutcome::Unknown { available } => {
                let suffix = if available.is_empty() {
                    String::new()
                } else {
                    format!(". Available types: {}", available.join(", "))
                };
                return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                    format!("Unknown subagent type: {}{suffix}", input.subagent_type),
                ));
            }
            SubagentValidateTypeOutcome::Disabled => {
                return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                    format!(
                        "Subagent '{}' is disabled via [subagents.toggle] in config.toml",
                        input.subagent_type
                    ),
                ));
            }
            SubagentValidateTypeOutcome::NotAllowed { allowed } => {
                return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                    format!(
                        "agent can only spawn: {}; '{}' not allowed",
                        allowed.join(", "),
                        input.subagent_type
                    ),
                ));
            }
            SubagentValidateTypeOutcome::ValidationUnavailable => {
                // `custom` (not `invalid_arguments`) so the model doesn't
                // retry with a different name on transport faults.
                return Err(echo_agent_tool_runtime::ToolError::custom(
                    "validation_unavailable",
                    format!(
                        "Cannot validate subagent type '{}': the subagent coordinator is \
                         unreachable. Retry shortly or notify ops.",
                        input.subagent_type
                    ),
                ));
            }
        }

        if let Some(ref requested) = model {
            let validator = model_validator.ok_or_else(|| {
                echo_agent_tool_runtime::ToolError::custom(
                    "validation_unavailable",
                    "Cannot validate Task.model: model catalog validator is unavailable.",
                )
            })?;
            if let Some(error) = validator.error_for(requested) {
                return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(error));
            }
        }

        // 3. Build the subagent request
        let id = input
            .task_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let child_cancellation = tokio_util::sync::CancellationToken::new();
        let cancellation_forwarder = (!input.run_in_background)
            .then(|| {
                tool_cancellation.map(|tool_cancellation| {
                    let child_cancellation = child_cancellation.clone();
                    tokio::spawn(async move {
                        tool_cancellation.cancelled().await;
                        child_cancellation.cancel();
                    })
                })
            })
            .flatten();

        let request = SubagentRequest {
            id: id.clone(),
            prompt: input.prompt.clone(),
            description: input.description.clone(),
            subagent_type: input.subagent_type.clone(),
            parent_session_id,
            parent_prompt_id,
            resume_from,
            cwd,
            runtime_overrides: SubagentRuntimeOverrides {
                model,
                model_override_provenance: ModelOverrideProvenance::Tool,
                reasoning_effort: None,
                persona: None,
                // JSON cannot set this field. Compat-harness adapters still
                // populate it in-process; model-facing spawns stay `None`.
                capability_mode: input.capability_mode,
                isolation: input.isolation,
                // Model-issued `task` spawns never override the harness; the
                // parent agent decides the flavor (the `/goal` harness override
                // is set only by the harness-internal role spawners).
                harness_agent_type: None,
                completion_output_cap: None,
                spawn_depth: None,
                output_token_budget: None,
                output_schema: None,
                loop_task_id: None,
            },
            run_in_background: input.run_in_background,
            // Model-spawned subagents must still appear in the idle reminder.
            surface_completion: true,
            await_to_completion: bounded,
            fork_context: false,
            owner: SubagentOwner::Task,
            cancel_token: child_cancellation,
        };

        // 4. Background mode: fire-and-forget via backend.spawn().
        // Coordinator stores the result for TaskOutputTool polling.
        // Both transport errors and coordinator rejections are logged so
        // late failures (worktree creation, etc.) remain visible.
        if input.run_in_background {
            let bg_backend = backend.clone();
            let bg_id = id.clone();
            let bg_type = input.subagent_type.clone();
            tokio::spawn(async move {
                match bg_backend.backend().spawn(request).await {
                    Err(e) => {
                        tracing::error!(
                            subagent_id = %bg_id,
                            subagent_type = %bg_type,
                            "background spawn transport error: {e:#}",
                        );
                    }
                    Ok(r) if !r.success => {
                        tracing::error!(
                            subagent_id = %bg_id,
                            subagent_type = %bg_type,
                            error = ?r.error,
                            "background spawn rejected by coordinator",
                        );
                    }
                    Ok(_) => {}
                }
            });

            let (task_output_tool, task_ids_param, timeout_ms_param) =
                resolve_background_notice_names(&resources).await;
            let naming = echo_agent_tool_types::BackgroundNoticeNaming {
                task_output_tool: &task_output_tool,
                task_ids_param: &task_ids_param,
                timeout_ms_param: &timeout_ms_param,
            };

            let continue_parent =
                detect_continue_parent_work(&resources, &input.description, &input.prompt).await;
            return Ok(ToolOutput::Text(
                echo_agent_tool_types::format_subagent_started_background(
                    &id,
                    &input.subagent_type,
                    &input.description,
                    &naming,
                    continue_parent,
                )
                .into(),
            ));
        }

        // 5. Blocking mode (default): spawn via backend and await result
        let _foreground_wait = foreground_wait.map(|wait| wait.enter());
        let result = backend.backend().spawn(request).await;
        if let Some(forwarder) = cancellation_forwarder {
            forwarder.abort();
        }
        let result = result?;

        // 5b. The await budget expired and the coordinator auto-backgrounded the
        // still-running child — return a task_id to poll, like the background
        // branch above (the result arrives via auto-wake or a later poll).
        if result.backgrounded {
            let (task_output_tool, task_ids_param, timeout_ms_param) =
                resolve_background_notice_names(&resources).await;
            let naming = echo_agent_tool_types::BackgroundNoticeNaming {
                task_output_tool: &task_output_tool,
                task_ids_param: &task_ids_param,
                timeout_ms_param: &timeout_ms_param,
            };
            // Only promise a completion notification when the client
            // actually delivers system reminders.
            let notified_on_completion = resources
                .lock()
                .await
                .get::<crate::types::resources::SystemRemindersEnabled>()
                .is_none_or(|e| e.0);
            let continue_parent =
                detect_continue_parent_work(&resources, &input.description, &input.prompt).await;

            let text = echo_agent_tool_types::format_subagent_auto_backgrounded(
                &id,
                &input.subagent_type,
                &input.description,
                &naming,
                notified_on_completion,
                continue_parent,
            );
            return Ok(ToolOutput::Text(text.into()));
        }

        // 6. Return result
        if result.success {
            let resume_from_hint = result.subagent_id.clone();
            let persona_hint: Option<String> = None;
            Ok(ToolOutput::SubagentCompleted(SubagentCompletedOutput {
                // SubagentCompletedOutput.output is `String` (serde-visible
                // boundary). One allocation per completion; cheaper paths
                // (pending_completions / snapshot) keep the Arc<str>.
                output: result.output.to_string(),
                subagent_id: result.subagent_id,
                subagent_type: input.subagent_type,
                tool_calls: result.tool_calls,
                turns: result.turns,
                duration_ms: result.duration_ms,
                worktree_path: result.worktree_path,
                persona: None,
                resume_from_hint,
                persona_hint,
            }))
        } else {
            Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                result
                    .error
                    .unwrap_or_else(|| "Unknown subagent error".to_string()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::implementations::echo_agent_build::task::backend::{
        ChannelBackend, SubagentBackendResource,
    };
    use crate::types::resources::Resources;
    use crate::types::tool_metadata::test_ctx;
    use echo_agent_tool_types::SubagentCapabilityMode;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// Backend whose `ValidateType` events are auto-acked with `Ok`.
    fn make_backend() -> (
        SubagentBackendResource,
        mpsc::UnboundedReceiver<SubagentEvent>,
    ) {
        make_backend_with_validation(SubagentValidateTypeOutcome::Ok)
    }

    /// Backend that replays `outcome` for every `ValidateType` event.
    fn make_backend_with_validation(
        outcome: SubagentValidateTypeOutcome,
    ) -> (
        SubagentBackendResource,
        mpsc::UnboundedReceiver<SubagentEvent>,
    ) {
        make_backend_with_validation_fn(move |_, _| outcome.clone())
    }

    /// Backend whose `ValidateType` outcome is computed per (type, session) pair.
    fn make_backend_with_validation_fn<F>(
        outcome_fn: F,
    ) -> (
        SubagentBackendResource,
        mpsc::UnboundedReceiver<SubagentEvent>,
    )
    where
        F: Fn(&str, &str) -> SubagentValidateTypeOutcome + Send + 'static,
    {
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<SubagentEvent>();
        let (proxy_tx, proxy_rx) = mpsc::unbounded_channel::<SubagentEvent>();
        let backend = SubagentBackendResource(Arc::new(ChannelBackend::new(raw_tx)));
        tokio::spawn(async move {
            while let Some(event) = raw_rx.recv().await {
                match event {
                    SubagentEvent::ValidateType(req) => {
                        let outcome = outcome_fn(&req.subagent_type, &req.parent_session_id);
                        let _ = req.respond_to.send(outcome);
                    }
                    other => {
                        if proxy_tx.send(other).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        (backend, proxy_rx)
    }

    /// Extract a spawn envelope from a `SubagentEvent`.
    fn unwrap_spawn(event: SubagentEvent) -> SubagentSpawnRequest {
        match event {
            SubagentEvent::Spawn(r) => r,
            _ => panic!("Expected SubagentEvent::Spawn"),
        }
    }

    #[tokio::test]
    async fn depth_limit_exceeded() {
        let (backend, _rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(MAX_SUBAGENT_DEPTH)); // at limit
        resources.insert(SessionIdResource("test-session".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-123".to_string()));

        let tool = TaskTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskToolInput {
                description: "test task".into(),
                prompt: "do something".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: None,
                cwd: None,
                model: None,
                task_id: None,
            },
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("depth limit exceeded"), "error: {err}");
    }

    #[tokio::test]
    async fn raised_max_depth_allows_nested_spawn() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(1));
        resources.insert(MaxSubagentDepth(2));
        resources.insert(SessionIdResource("child-session".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-nested".to_string()));

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            TaskToolInput {
                description: "nested ok".into(),
                prompt: "should be allowed at max_depth=2".into(),
                subagent_type: "explore".into(),
                run_in_background: true,
                capability_mode: None,
                isolation: None,
                resume_from: None,
                cwd: None,
                model: None,
                task_id: None,
            },
        )
        .await;

        assert!(
            result.is_ok(),
            "expected Ok at depth 1 with max 2: {result:?}"
        );
        let _ = rx.try_recv();
    }

    #[tokio::test]
    async fn subagent_cannot_spawn_nested_subagent() {
        let (backend, _rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(1)); // first-level subagent
        resources.insert(SessionIdResource("child-session".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-456".to_string()));

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            TaskToolInput {
                description: "nested spawn".into(),
                prompt: "should be rejected".into(),
                subagent_type: "explore".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: None,
                cwd: None,
                model: None,
                task_id: None,
            },
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("depth limit exceeded"),
            "subagent at depth 1 must not spawn: {err}"
        );
    }

    #[tokio::test]
    async fn missing_backend_returns_error() {
        let resources = Resources::new();

        let tool = TaskTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskToolInput {
                description: "test task".into(),
                prompt: "do something".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: None,
                cwd: None,
                model: None,
                task_id: None,
            },
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("SubagentBackendResource"),
            "error should mention missing resource: {err}"
        );
    }

    #[tokio::test]
    async fn successful_subagent_returns_text_output() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent-session".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-123".to_string()));

        let tool = TaskTool;
        let shared = resources.into_shared();

        // Spawn a task that will handle the request
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(request.subagent_type, "explore");
            assert_eq!(request.parent_session_id, "parent-session");
            assert_eq!(request.parent_prompt_id.as_deref(), Some("prompt-123"));
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: std::sync::Arc::from("Found 3 auth middleware files"),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    tool_calls: 5,
                    turns: 2,
                    duration_ms: 1234,
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(shared),
            TaskToolInput {
                description: "Find auth middleware".into(),
                prompt: "Search for authentication middleware files".into(),
                subagent_type: "explore".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: None,
                cwd: None,
                model: None,
                task_id: None,
            },
        )
        .await
        .unwrap();

        handle.await.unwrap();

        match result {
            ToolOutput::SubagentCompleted(sub) => {
                assert!(sub.output.contains("Found 3 auth middleware files"));
                assert_eq!(sub.tool_calls, 5);
                assert_eq!(sub.turns, 2);
                assert_eq!(sub.duration_ms, 1234);
                assert_eq!(sub.subagent_type, "explore");
            }
            other => panic!("Expected SubagentCompleted output, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn bound_subagent_to_turn_overrides_background_and_await_budget() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(BoundSubagentToTurn(true));
        resources.insert(SessionIdResource("parent-session".into()));
        resources.insert(CurrentPromptIdResource("parent-turn".into()));
        let host = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert!(!request.run_in_background);
            assert!(request.await_to_completion);
            assert_eq!(request.parent_prompt_id.as_deref(), Some("parent-turn"));
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: "Bound review completed".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });
        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("explore", true),
        )
        .await;
        assert!(result.is_ok());
        host.await.unwrap();
    }

    #[tokio::test]
    async fn failed_subagent_returns_error() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0)); // top-level session
        resources.insert(SessionIdResource("parent-session".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-123".to_string()));

        let tool = TaskTool;
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            request
                .respond_with(|_| SubagentResult {
                    success: false,
                    error: Some("Child session crashed".to_string()),
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(shared),
            TaskToolInput {
                description: "test task".into(),
                prompt: "do something".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: None,
                cwd: None,
                model: None,
                task_id: None,
            },
        )
        .await;

        handle.await.unwrap();

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Child session crashed"), "error: {err}");
    }

    #[tokio::test]
    async fn dropped_result_channel_returns_error() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent-session".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-123".to_string()));

        let tool = TaskTool;
        let shared = resources.into_shared();

        // Spawn a task that drops the result_tx without sending
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            drop(request.result_tx);
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(shared),
            TaskToolInput {
                description: "test task".into(),
                prompt: "do something".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: None,
                cwd: None,
                model: None,
                task_id: None,
            },
        )
        .await;

        handle.await.unwrap();

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("result channel dropped"), "error: {err}");
    }

    /// A `backgrounded: true` result must surface a task_id notice (not a
    /// completion, not an error) so the model can poll the still-running child.
    #[tokio::test]
    async fn auto_backgrounded_result_returns_task_id_text() {
        let (backend, mut rx) = make_backend();
        let (mut resources, _dir) = resources_with_parent_exec(
            backend,
            "run bazel test //hw5:op_chain then spawn a skill agent",
        );
        let wait_closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        struct WaitProbe(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for WaitProbe {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let wait_closed_for_factory = Arc::clone(&wait_closed);
        resources.insert(SubagentForegroundWait::new(move || {
            Box::new(WaitProbe(Arc::clone(&wait_closed_for_factory)))
        }));

        let drain = tokio::spawn(async move {
            if let Some(SubagentEvent::Spawn(boxed)) = rx.recv().await {
                let _ = boxed.respond_with(|boxed| SubagentResult {
                    backgrounded: true,
                    subagent_id: boxed.id.clone(),
                    child_session_id: boxed.id.clone(),
                    ..Default::default()
                });
            }
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("general-purpose", false), // blocking mode
        )
        .await
        .expect("auto-backgrounded blocking spawn returns Ok");
        assert!(
            wait_closed.load(std::sync::atomic::Ordering::Relaxed),
            "auto-backgrounding must close the foreground wait window"
        );

        match result {
            ToolOutput::Text(text) => {
                assert!(
                    text.text.contains("moved to the background"),
                    "expected background notice, got: {}",
                    text.text
                );
                assert!(
                    text.text.contains("subagent_id:"),
                    "should include a task_id to poll: {}",
                    text.text
                );
                assert!(
                    text.text.contains("timeout_ms"),
                    "should instruct the model to wait: {}",
                    text.text
                );
                assert!(
                    text.text
                        .contains(echo_agent_tool_types::BACKGROUND_SUBAGENT_CONTINUE_PARENT_WORK),
                    "auto-bg result must keep in-flight parent work: {}",
                    text.text
                );
            }
            other => panic!("expected Text output, got {other:?}"),
        }

        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), drain).await;
    }

    #[tokio::test]
    async fn default_subagent_type_and_background() {
        let input: TaskToolInput =
            serde_json::from_str(r#"{"description": "test", "prompt": "do it"}"#).unwrap();
        assert_eq!(input.subagent_type, "general-purpose");
        assert!(
            input.run_in_background,
            "run_in_background should default to true"
        );
    }

    fn task_input(subagent_type: &str, background: bool) -> TaskToolInput {
        TaskToolInput {
            description: "test".into(),
            prompt: "do it".into(),
            subagent_type: subagent_type.into(),
            run_in_background: background,
            capability_mode: None,
            isolation: None,
            resume_from: None,
            cwd: None,
            model: None,
            task_id: None,
        }
    }

    fn resources_for_task(backend: SubagentBackendResource) -> Resources {
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent-session".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-123".to_string()));
        resources.insert(TaskModelValidator::new(|_| None));
        resources
    }

    /// Seed `chat_history.jsonl` so leftover-parent detection can fire.
    fn resources_with_parent_exec(
        backend: SubagentBackendResource,
        user_ask: &str,
    ) -> (Resources, tempfile::TempDir) {
        resources_with_chat_history(
            backend,
            &[serde_json::json!({
                "type": "user",
                "content": format!("<user_query>{user_ask}</user_query>"),
            })],
        )
    }

    fn resources_with_chat_history(
        backend: SubagentBackendResource,
        rows: &[serde_json::Value],
    ) -> (Resources, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let mut body = String::new();
        for row in rows {
            body.push_str(&row.to_string());
            body.push('\n');
        }
        std::fs::write(dir.path().join("chat_history.jsonl"), body).expect("write chat_history");
        let mut resources = resources_for_task(backend);
        resources.insert(crate::types::resources::SessionFolder(
            dir.path().to_path_buf(),
        ));
        (resources, dir)
    }

    #[tokio::test]
    async fn unknown_subagent_type_returns_error_before_spawn() {
        let available = vec!["general-purpose".to_string(), "explore".to_string()];
        let (backend, mut rx) =
            make_backend_with_validation(SubagentValidateTypeOutcome::Unknown {
                available: available.clone(),
            });
        let resources = resources_for_task(backend);

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("invented-agent", true),
        )
        .await;

        let msg = result.expect_err("must reject").to_string();
        assert!(msg.contains("Unknown subagent type: invented-agent"));
        for name in &available {
            assert!(msg.contains(name));
        }
        let last = available.last().expect("non-empty");
        assert!(msg.ends_with(last.as_str()));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn disabled_subagent_type_returns_error_before_spawn() {
        let (backend, mut rx) = make_backend_with_validation(SubagentValidateTypeOutcome::Disabled);
        let resources = resources_for_task(backend);

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("explore", true),
        )
        .await;
        let msg = result.expect_err("must reject").to_string();
        assert!(msg.contains("disabled via [subagents.toggle]"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn not_allowed_subagent_type_returns_error_before_spawn() {
        let allowed = vec!["explore".to_string(), "plan".to_string()];
        let (backend, mut rx) =
            make_backend_with_validation(SubagentValidateTypeOutcome::NotAllowed {
                allowed: allowed.clone(),
            });
        let resources = resources_for_task(backend);

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("general-purpose", true),
        )
        .await;
        let msg = result.expect_err("must reject").to_string();
        assert!(msg.contains("'general-purpose' not allowed") && msg.contains("explore"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn unknown_subagent_type_with_empty_available_omits_suffix() {
        let (backend, _rx) = make_backend_with_validation(SubagentValidateTypeOutcome::Unknown {
            available: vec![],
        });
        let resources = resources_for_task(backend);

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("invented", false),
        )
        .await;

        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Unknown subagent type: invented"));
        assert!(!msg.contains("Available types:"));
    }

    #[tokio::test]
    async fn validation_gates_blocking_mode_too() {
        let (backend, mut rx) = make_backend_with_validation(SubagentValidateTypeOutcome::Disabled);
        let resources = resources_for_task(backend);

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("explore", false),
        )
        .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("disabled via [subagents.toggle]"),
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn invalid_model_returns_error_before_background_spawn() {
        let (backend, mut rx) = make_backend();
        let mut resources = resources_for_task(backend);
        resources.insert(TaskModelValidator::new(|requested| {
            (requested == "invented-model").then(|| {
                "Unknown Task.model slug 'invented-model'. Valid model slugs: alpha, zeta. \
                 Omit `model` to inherit the parent model."
                    .to_string()
            })
        }));
        let mut input = task_input("general-purpose", true);
        input.model = Some("invented-model".to_string());

        let result =
            echo_agent_tool_runtime::Tool::run(&TaskTool, test_ctx(resources.into_shared()), input)
                .await;

        let msg = result
            .expect_err("invalid model must reject before spawn")
            .to_string();
        assert!(msg.contains("Unknown Task.model slug 'invented-model'"));
        assert!(msg.contains("Valid model slugs: alpha, zeta"));
        assert!(
            rx.try_recv().is_err(),
            "spawn must not reach the coordinator"
        );
    }

    #[tokio::test]
    async fn background_spawn_succeeds_when_coordinator_accepts() {
        let (backend, mut rx) = make_backend();
        let (resources, _dir) =
            resources_with_parent_exec(backend, "CI still fails, fix and gt submit /pr-babysit");

        let drain = tokio::spawn(async move {
            if let Some(SubagentEvent::Spawn(boxed)) = rx.recv().await {
                let _ = boxed.respond_with(|boxed| SubagentResult {
                    success: true,
                    output: std::sync::Arc::from(""),
                    subagent_id: boxed.id.clone(),
                    child_session_id: boxed.id.clone(),
                    ..Default::default()
                });
            }
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("general-purpose", true),
        )
        .await
        .expect("background spawn should succeed");

        match result {
            ToolOutput::Text(text) => {
                assert!(text.text.contains("Subagent started in background"));
                assert!(
                    text.text
                        .contains(echo_agent_tool_types::BACKGROUND_SUBAGENT_CONTINUE_PARENT_WORK),
                    "background spawn must keep in-flight parent work: {}",
                    text.text
                );
            }
            other => panic!("expected text output, got {other:?}"),
        }

        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), drain).await;
    }

    #[tokio::test]
    async fn background_spawn_omits_cta_when_review_is_the_only_ask() {
        let (backend, mut rx) = make_backend();
        let (resources, _dir) =
            resources_with_parent_exec(backend, "review this PR https://github.com/x/y/pull/1");

        let drain = tokio::spawn(async move {
            if let Some(SubagentEvent::Spawn(boxed)) = rx.recv().await {
                let _ = boxed.respond_with(|boxed| SubagentResult {
                    success: true,
                    output: std::sync::Arc::from(""),
                    subagent_id: boxed.id.clone(),
                    child_session_id: boxed.id.clone(),
                    ..Default::default()
                });
            }
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("general-purpose", true),
        )
        .await
        .expect("background spawn should succeed");

        match result {
            ToolOutput::Text(text) => {
                assert!(text.text.contains("Subagent started in background"));
                assert!(
                    !text
                        .text
                        .contains(echo_agent_tool_types::BACKGROUND_SUBAGENT_CONTINUE_PARENT_WORK),
                    "review-only ask must not get continue-parent CTA: {}",
                    text.text
                );
            }
            other => panic!("expected text output, got {other:?}"),
        }

        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), drain).await;
    }

    #[tokio::test]
    async fn background_spawn_ignores_synthetic_user_rows_in_lookback() {
        let (backend, mut rx) = make_backend();
        let (resources, _dir) = resources_with_chat_history(
            backend,
            &[
                serde_json::json!({
                    "type": "user",
                    "content": "<user_query>CI still fails, fix and gt submit</user_query>",
                }),
                serde_json::json!({
                    "type": "user",
                    "synthetic_reason": "auto_continue",
                    "content": "Continue with the work described in the summary above.",
                }),
                serde_json::json!({
                    "type": "user",
                    "synthetic_reason": "stop_hook_feedback",
                    "content": "hook: stop",
                }),
            ],
        );

        let drain = tokio::spawn(async move {
            if let Some(SubagentEvent::Spawn(boxed)) = rx.recv().await {
                let _ = boxed.respond_with(|boxed| SubagentResult {
                    success: true,
                    output: std::sync::Arc::from(""),
                    subagent_id: boxed.id.clone(),
                    child_session_id: boxed.id.clone(),
                    ..Default::default()
                });
            }
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("general-purpose", true),
        )
        .await
        .expect("background spawn should succeed");

        match result {
            ToolOutput::Text(text) => {
                assert!(
                    text.text
                        .contains(echo_agent_tool_types::BACKGROUND_SUBAGENT_CONTINUE_PARENT_WORK),
                    "synthetic rows must not hide leftover exec: {}",
                    text.text
                );
            }
            other => panic!("expected text output, got {other:?}"),
        }

        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), drain).await;
    }

    #[tokio::test]
    async fn background_spawn_emits_error_log_on_coordinator_rejection() {
        use super::types::test_capture;

        let captured = test_capture::capture();
        let (backend, mut rx) = make_backend();
        let resources = resources_for_task(backend);

        // done_tx signals after the spawn has been replied to so the
        // test can wait for Fix A's match arm to execute.
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let drain = tokio::spawn(async move {
            if let Some(SubagentEvent::Spawn(boxed)) = rx.recv().await {
                let _ = boxed.respond_with(|boxed| SubagentResult {
                    success: false,
                    error: Some("worktree creation failed".to_string()),
                    subagent_id: boxed.id.clone(),
                    ..Default::default()
                });
            }
            let _ = done_tx.send(());
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("general-purpose", true),
        )
        .await
        .expect("background tool call returns Ok regardless of coordinator outcome");
        let text = match result {
            ToolOutput::Text(t) => t.text,
            other => panic!("expected text output, got {other:?}"),
        };
        assert!(text.contains("Subagent started in background"));

        // Let the fire-and-forget bg task advance past `.await` on spawn.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), done_rx).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }

        let mut events_rx = captured.events_rx;
        let mut saw_error = false;
        while let Ok(event) = events_rx.try_recv() {
            if event.level == tracing::Level::ERROR
                && event
                    .fields
                    .contains("background spawn rejected by coordinator")
                && event.fields.contains("subagent_id=")
                && event.fields.contains("subagent_type=general-purpose")
                && event.fields.contains("worktree creation failed")
            {
                saw_error = true;
                break;
            }
        }
        assert!(saw_error, "Fix A must emit an ERROR with required fields");

        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), drain).await;
    }

    #[tokio::test]
    async fn background_spawn_survives_transport_error_after_validation() {
        // Smoke test of the fire-and-forget contract when the spawn
        // channel is closed; companion test covers the Ok(success:false)
        // arm with tracing assertions.
        let (backend, rx) = make_backend();
        drop(rx);
        let resources = resources_for_task(backend);

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("general-purpose", true),
        )
        .await
        .expect("transport error must not break the fire-and-forget contract");
        match result {
            ToolOutput::Text(t) => {
                assert!(
                    t.text.contains("Subagent started in background"),
                    "{}",
                    t.text,
                );
            }
            other => panic!("expected text output, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validation_unavailable_returns_custom_error_not_invalid_arguments() {
        let (backend, _rx) =
            make_backend_with_validation(SubagentValidateTypeOutcome::ValidationUnavailable);
        let resources = resources_for_task(backend);

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("explore", true),
        )
        .await;
        let msg = result.expect_err("must error").to_string();
        assert!(
            msg.contains("subagent coordinator is unreachable")
                && msg.contains("Cannot validate subagent type"),
        );
        assert!(!msg.contains("Unknown subagent type"));
    }

    #[tokio::test]
    async fn validate_request_threads_session_id_to_coordinator() {
        let (capture_tx, mut capture_rx) = mpsc::unbounded_channel::<String>();
        let (backend, _rx) = make_backend_with_validation_fn(move |_t, parent_session_id| {
            let _ = capture_tx.send(parent_session_id.to_string());
            SubagentValidateTypeOutcome::Ok
        });
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("special-session-id".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-x".to_string()));

        let _ = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            task_input("explore", true),
        )
        .await;

        let seen = capture_rx.try_recv().expect("must fire at least once");
        assert_eq!(seen, "special-session-id");
        assert!(capture_rx.try_recv().is_err(), "must fire exactly once");
    }

    #[test]
    fn task_tool_id_predicate_accepts_all_wire_spellings() {
        assert!(is_task_tool_id(
            echo_agent_tool_runtime::Tool::id(&TaskTool).as_str()
        ));
        for name in ["task", "Task", "spawn_subagent"] {
            assert!(is_task_tool_id(name), "must accept {name:?}");
        }
    }

    #[test]
    fn task_tool_id_predicate_rejects_lookalikes() {
        for name in [
            "",
            "TASK",
            "tasks",
            "spawn_subagents",
            "Spawn_Subagent",
            "task_output",
            "kill_task",
            "subagent",
            " task",
        ] {
            assert!(!is_task_tool_id(name), "must reject {name:?}");
        }
    }

    // ── Runtime overrides serde tests ─────────────────

    #[test]
    fn capability_mode_in_json_is_ignored() {
        let input: TaskToolInput = serde_json::from_str(
            r#"{
                "description": "d",
                "prompt": "p",
                "capability_mode": "read-only"
            }"#,
        )
        .unwrap();
        assert!(
            input.capability_mode.is_none(),
            "model-facing JSON must not set capability_mode"
        );
    }

    #[test]
    fn partial_overrides_leave_rest_none() {
        let input: TaskToolInput =
            serde_json::from_str(r#"{"description": "d", "prompt": "p", "isolation": "worktree"}"#)
                .unwrap();
        assert_eq!(input.isolation, Some(SubagentIsolationMode::Worktree));
        assert!(input.model.is_none());
        assert!(input.capability_mode.is_none());
    }

    #[test]
    fn task_tool_input_schema_includes_model() {
        let schema = serde_json::to_value(schemars::schema_for!(TaskToolInput)).unwrap();
        assert_eq!(
            schema["properties"]["model"]["description"],
            "Optional model slug for this agent. If provided, it must resolve to one of the \
             available model slugs. If omitted, the subagent uses the same model as the parent \
             agent. Do not pass if resume_from is set (prior model will be used). Only choose \
             an explicit model when the user directly requests it."
        );
    }

    #[test]
    fn task_tool_input_schema_omits_capability_mode() {
        let schema = serde_json::to_value(schemars::schema_for!(TaskToolInput)).unwrap();
        assert!(
            schema["properties"].get("capability_mode").is_none(),
            "capability_mode must not be advertised on the model-facing schema"
        );
    }

    #[test]
    fn runtime_overrides_struct_default_is_all_none() {
        let overrides = SubagentRuntimeOverrides::default();
        assert!(overrides.model.is_none());
        assert!(overrides.reasoning_effort.is_none());
        assert!(overrides.persona.is_none());
        assert!(overrides.capability_mode.is_none());
    }

    #[test]
    fn task_input_roundtrips_through_json() {
        let input = TaskToolInput {
            description: "find bugs".into(),
            prompt: "search for bugs".into(),
            subagent_type: "explore".into(),
            run_in_background: true,
            capability_mode: Some(SubagentCapabilityMode::ReadOnly),
            isolation: Some(SubagentIsolationMode::Worktree),
            resume_from: None,
            cwd: None,
            model: Some("test-model".into()),
            task_id: Some("task-123".into()),
        };
        let json = serde_json::to_string(&input).unwrap();
        let parsed: TaskToolInput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.description, "find bugs");
        assert!(
            parsed.capability_mode.is_none(),
            "capability_mode is harness-only and must not round-trip through JSON"
        );
        assert_eq!(parsed.model.as_deref(), Some("test-model"));
    }

    #[test]
    fn capability_mode_all_variants_parse() {
        for (json_val, expected) in [
            ("read-only", SubagentCapabilityMode::ReadOnly),
            ("read-write", SubagentCapabilityMode::ReadWrite),
            ("execute", SubagentCapabilityMode::Execute),
            ("all", SubagentCapabilityMode::All),
        ] {
            let parsed: SubagentCapabilityMode =
                serde_json::from_value(serde_json::json!(json_val)).unwrap();
            assert_eq!(parsed, expected, "for {json_val}");
        }
    }

    #[test]
    fn capability_mode_rejects_invalid_value() {
        let result =
            serde_json::from_value::<SubagentCapabilityMode>(serde_json::json!("invalid_mode"));
        assert!(result.is_err(), "unknown value should be rejected");
    }

    #[test]
    fn capability_mode_aliases_roundtrip() {
        for (alias, expected, canonical) in [
            ("readonly", SubagentCapabilityMode::ReadOnly, "read-only"),
            ("readOnly", SubagentCapabilityMode::ReadOnly, "read-only"),
            ("read_only", SubagentCapabilityMode::ReadOnly, "read-only"),
            ("ReadOnly", SubagentCapabilityMode::ReadOnly, "read-only"),
            ("readwrite", SubagentCapabilityMode::ReadWrite, "read-write"),
            ("readWrite", SubagentCapabilityMode::ReadWrite, "read-write"),
            (
                "read_write",
                SubagentCapabilityMode::ReadWrite,
                "read-write",
            ),
            ("ReadWrite", SubagentCapabilityMode::ReadWrite, "read-write"),
            ("Execute", SubagentCapabilityMode::Execute, "execute"),
            ("EXECUTE", SubagentCapabilityMode::Execute, "execute"),
            ("All", SubagentCapabilityMode::All, "all"),
            ("ALL", SubagentCapabilityMode::All, "all"),
        ] {
            let parsed: SubagentCapabilityMode = serde_json::from_value(serde_json::json!(alias))
                .unwrap_or_else(|e| panic!("alias {alias:?} should parse: {e}"));
            assert_eq!(parsed, expected, "parse {alias:?}");
            assert_eq!(
                serde_json::to_value(expected).unwrap(),
                canonical,
                "serialize {alias:?} back to canonical",
            );
        }
    }

    #[test]
    fn capability_mode_serializes_to_kebab_case() {
        for (mode, expected) in [
            (SubagentCapabilityMode::ReadOnly, "read-only"),
            (SubagentCapabilityMode::ReadWrite, "read-write"),
            (SubagentCapabilityMode::Execute, "execute"),
            (SubagentCapabilityMode::All, "all"),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), expected, "{mode:?}");
        }
    }

    // -- Isolation mode tests --

    #[test]
    fn isolation_defaults_to_none() {
        let input: TaskToolInput =
            serde_json::from_str(r#"{"description": "d", "prompt": "p"}"#).unwrap();
        assert_eq!(input.isolation, None);
    }

    #[test]
    fn isolation_worktree_parses() {
        let input: TaskToolInput =
            serde_json::from_str(r#"{"description": "d", "prompt": "p", "isolation": "worktree"}"#)
                .unwrap();
        assert_eq!(input.isolation, Some(SubagentIsolationMode::Worktree));
    }

    #[test]
    fn isolation_none_parses() {
        let input: TaskToolInput =
            serde_json::from_str(r#"{"description": "d", "prompt": "p", "isolation": "none"}"#)
                .unwrap();
        assert_eq!(input.isolation, Some(SubagentIsolationMode::None));
    }

    #[test]
    fn isolation_invalid_rejected() {
        let result = serde_json::from_str::<TaskToolInput>(
            r#"{"description": "d", "prompt": "p", "isolation": "sandbox"}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn isolation_mode_aliases_roundtrip() {
        for (alias, expected, canonical) in [
            ("None", SubagentIsolationMode::None, "none"),
            ("Worktree", SubagentIsolationMode::Worktree, "worktree"),
            ("work_tree", SubagentIsolationMode::Worktree, "worktree"),
            ("work-tree", SubagentIsolationMode::Worktree, "worktree"),
        ] {
            let json = format!(r#"{{"description":"d","prompt":"p","isolation":"{alias}"}}"#);
            let input: TaskToolInput = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("alias {alias:?} should parse: {e}"));
            assert_eq!(input.isolation, Some(expected), "parse {alias:?}");
            assert_eq!(
                serde_json::to_value(expected).unwrap(),
                canonical,
                "serialize {alias:?} back to canonical",
            );
        }
    }

    #[test]
    fn isolation_serializes_to_kebab_case() {
        for (mode, expected) in [
            (SubagentIsolationMode::None, "none"),
            (SubagentIsolationMode::Worktree, "worktree"),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), expected, "{mode:?}");
        }
    }

    // -- Capability mode enforcement tests --

    fn tc(id: &str, kind: crate::types::tool::ToolKind) -> crate::registry::types::ToolConfig {
        let mut c = crate::registry::types::ToolConfig::from_id(id);
        c.kind = Some(kind);
        c
    }

    #[test]
    fn filter_read_only_removes_edit_and_execute() {
        use crate::registry::types::ToolServerConfig;
        use crate::types::tool::ToolKind;
        let mut config = ToolServerConfig {
            tools: vec![
                tc("read_file", ToolKind::Read),
                tc("grep", ToolKind::Search),
                tc("list_dir", ToolKind::List),
                tc("search_replace", ToolKind::Edit),
                tc("bash", ToolKind::Execute),
                tc("task", ToolKind::Task),
            ],
            behavior_preset: None,
        };
        SubagentCapabilityMode::ReadOnly.filter_tool_config(&mut config);
        let ids: Vec<&str> = config.tools.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"read_file"));
        assert!(ids.contains(&"grep"));
        assert!(ids.contains(&"list_dir"));
        assert!(ids.contains(&"task"));
        assert!(!ids.contains(&"search_replace"), "edit should be removed");
        assert!(!ids.contains(&"bash"), "execute should be removed");
    }

    #[test]
    fn filter_read_write_keeps_edit_removes_execute() {
        use crate::registry::types::ToolServerConfig;
        use crate::types::tool::ToolKind;
        let mut config = ToolServerConfig {
            tools: vec![
                tc("read_file", ToolKind::Read),
                tc("search_replace", ToolKind::Edit),
                tc("bash", ToolKind::Execute),
            ],
            behavior_preset: None,
        };
        SubagentCapabilityMode::ReadWrite.filter_tool_config(&mut config);
        let ids: Vec<&str> = config.tools.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"read_file"));
        assert!(ids.contains(&"search_replace"), "edit should be kept");
        assert!(!ids.contains(&"bash"), "execute should be removed");
    }

    #[test]
    fn filter_execute_keeps_bash_removes_edit() {
        use crate::registry::types::ToolServerConfig;
        use crate::types::tool::ToolKind;
        let mut config = ToolServerConfig {
            tools: vec![
                tc("read_file", ToolKind::Read),
                tc("search_replace", ToolKind::Edit),
                tc("bash", ToolKind::Execute),
            ],
            behavior_preset: None,
        };
        SubagentCapabilityMode::Execute.filter_tool_config(&mut config);
        let ids: Vec<&str> = config.tools.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"read_file"));
        assert!(ids.contains(&"bash"), "execute should be kept");
        assert!(!ids.contains(&"search_replace"), "edit should be removed");
    }

    #[test]
    fn filter_all_keeps_everything() {
        use crate::registry::types::ToolServerConfig;
        use crate::types::tool::ToolKind;
        let mut config = ToolServerConfig {
            tools: vec![
                tc("read_file", ToolKind::Read),
                tc("search_replace", ToolKind::Edit),
                tc("bash", ToolKind::Execute),
            ],
            behavior_preset: None,
        };
        SubagentCapabilityMode::All.filter_tool_config(&mut config);
        assert_eq!(config.tools.len(), 3);
    }

    #[test]
    fn filter_preserves_tools_without_kind() {
        use crate::registry::types::ToolServerConfig;
        use crate::types::tool::ToolKind;
        let mut config = ToolServerConfig {
            tools: vec![
                tc("read_file", ToolKind::Read),
                crate::registry::types::ToolConfig::from_id("mcp_custom_tool"),
            ],
            behavior_preset: None,
        };
        SubagentCapabilityMode::ReadOnly.filter_tool_config(&mut config);
        let ids: Vec<&str> = config.tools.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"read_file"));
        assert!(
            ids.contains(&"mcp_custom_tool"),
            "tools without kind preserved"
        );
    }

    // ── resume_from tests ────────────────────────────────────────────

    #[test]
    fn task_tool_input_has_no_fork_context_field() {
        let _: TaskToolInput =
            serde_json::from_str(r#"{"description": "d", "prompt": "p", "fork_context": true}"#)
                .unwrap();
        let schema_json = serde_json::to_string(&schemars::schema_for!(TaskToolInput)).unwrap();
        assert!(
            !schema_json.contains("fork_context"),
            "TaskToolInput JSON schema must not expose fork_context"
        );
        let serialized = serde_json::to_string(&TaskToolInput {
            description: "d".into(),
            prompt: "p".into(),
            subagent_type: "general-purpose".into(),
            run_in_background: false,
            capability_mode: None,
            isolation: None,
            resume_from: None,
            cwd: None,
            model: None,
            task_id: None,
        })
        .unwrap();
        assert!(
            !serialized.contains("fork_context"),
            "TaskToolInput serialization must not include fork_context"
        );
    }

    #[tokio::test]
    async fn model_task_spawn_sets_fork_context_false() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let shared = resources.into_shared();
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert!(
                !request.fork_context,
                "model-spawned task must not set fork_context"
            );
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let _ = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(shared),
            TaskToolInput {
                description: "d".into(),
                prompt: "p".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: None,
                cwd: None,
                model: None,
                task_id: None,
            },
        )
        .await
        .unwrap();
        handle.await.unwrap();
    }

    #[test]
    fn resume_from_defaults_to_none() {
        let input: TaskToolInput =
            serde_json::from_str(r#"{"description": "d", "prompt": "p"}"#).unwrap();
        assert!(input.resume_from.is_none());
    }

    #[test]
    fn resume_from_parses() {
        let input: TaskToolInput = serde_json::from_str(
            r#"{"description": "d", "prompt": "p", "resume_from": "prev-agent-id"}"#,
        )
        .unwrap();
        assert_eq!(input.resume_from.as_deref(), Some("prev-agent-id"));
    }

    #[test]
    fn resume_from_not_serialized_when_none() {
        let input = TaskToolInput {
            description: "d".into(),
            prompt: "p".into(),
            subagent_type: "general-purpose".into(),
            run_in_background: false,
            capability_mode: None,
            isolation: None,
            resume_from: None,
            cwd: None,
            model: None,
            task_id: None,
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(
            !json.contains("resume_from"),
            "None resume_from should be skipped in serialization"
        );
    }

    #[tokio::test]
    async fn resume_from_threads_to_request() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let shared = resources.into_shared();
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(request.resume_from.as_deref(), Some("prev-id"));
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: "resumed".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(shared),
            TaskToolInput {
                description: "resume".into(),
                prompt: "continue".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: Some("prev-id".into()),
                cwd: None,
                model: None,
                task_id: None,
            },
        )
        .await
        .unwrap();

        handle.await.unwrap();
        match result {
            ToolOutput::SubagentCompleted(sub) => {
                assert!(sub.output.contains("resumed"));
            }
            other => panic!("Expected SubagentCompleted, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn resume_from_sentinel_values_treated_as_none() {
        for sentinel in [
            "",
            "  ",
            "null",
            "Null",
            "NULL",
            "none",
            "None",
            "undefined",
        ] {
            let (backend, mut rx) = make_backend();
            let mut resources = Resources::new();
            resources.insert(backend);
            resources.insert(SubagentDepthCounter(0));
            resources.insert(SessionIdResource("parent".to_string()));
            resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

            let shared = resources.into_shared();
            let handle = tokio::spawn(async move {
                let request = unwrap_spawn(rx.recv().await.unwrap());
                assert!(
                    request.resume_from.is_none(),
                    "sentinel {sentinel:?} must normalize to None, got {:?}",
                    request.resume_from
                );
                request
                    .respond_with(|request| SubagentResult {
                        success: true,
                        output: "fresh".into(),
                        subagent_id: request.id.clone(),
                        child_session_id: request.id.clone(),
                        ..Default::default()
                    })
                    .unwrap();
            });

            let result = echo_agent_tool_runtime::Tool::run(
                &TaskTool,
                test_ctx(shared),
                TaskToolInput {
                    description: "test sentinel".into(),
                    prompt: "work".into(),
                    subagent_type: "general-purpose".into(),
                    run_in_background: false,
                    capability_mode: None,
                    isolation: None,
                    resume_from: Some(sentinel.into()),
                    cwd: None,
                    model: None,
                    task_id: None,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("sentinel {sentinel:?} should not fail: {e}"));

            handle.await.unwrap();
            match result {
                ToolOutput::SubagentCompleted(sub) => {
                    assert!(sub.output.contains("fresh"), "sentinel {sentinel:?}");
                }
                other => panic!("sentinel {sentinel:?}: expected SubagentCompleted, got {other:?}"),
            }
        }
    }

    // ── cwd tests ────────────────────────────────────────────────────

    #[test]
    fn cwd_defaults_to_none() {
        let input: TaskToolInput =
            serde_json::from_str(r#"{"description": "d", "prompt": "p"}"#).unwrap();
        assert!(input.cwd.is_none());
    }

    #[test]
    fn cwd_parses_from_json() {
        let input: TaskToolInput = serde_json::from_str(
            r#"{"description": "d", "prompt": "p", "cwd": "/tmp/my-worktree"}"#,
        )
        .unwrap();
        assert_eq!(input.cwd.as_deref(), Some("/tmp/my-worktree"));
    }

    #[test]
    fn cwd_not_serialized_when_none() {
        let input = TaskToolInput {
            description: "d".into(),
            prompt: "p".into(),
            subagent_type: "general-purpose".into(),
            run_in_background: false,
            capability_mode: None,
            isolation: None,
            resume_from: None,
            cwd: None,
            model: None,
            task_id: None,
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(!json.contains("cwd"), "None cwd should be skipped: {json}");
    }

    #[tokio::test]
    async fn cwd_and_worktree_isolation_are_mutually_exclusive() {
        let (backend, _rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            TaskToolInput {
                description: "test cwd conflict".into(),
                prompt: "work".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: Some(SubagentIsolationMode::Worktree),
                resume_from: None,
                cwd: Some("/tmp".into()),
                model: None,
                task_id: None,
            },
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("mutually exclusive"),
            "should reject cwd + isolation=worktree: {err}"
        );
    }

    #[tokio::test]
    async fn cwd_empty_string_with_worktree_is_allowed() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let shared = resources.into_shared();
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert!(
                request.cwd.is_none(),
                "empty cwd should normalize to None, got {:?}",
                request.cwd
            );
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(shared),
            TaskToolInput {
                description: "test empty cwd".into(),
                prompt: "work".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: Some(SubagentIsolationMode::Worktree),
                resume_from: None,
                cwd: Some("".into()),
                model: None,
                task_id: None,
            },
        )
        .await;

        handle.await.unwrap();
        assert!(result.is_ok(), "empty cwd + worktree should be allowed");
    }

    #[tokio::test]
    async fn cwd_null_string_with_worktree_is_allowed() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let shared = resources.into_shared();
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert!(
                request.cwd.is_none(),
                "'null' cwd should normalize to None, got {:?}",
                request.cwd
            );
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(shared),
            TaskToolInput {
                description: "test null cwd".into(),
                prompt: "work".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: Some(SubagentIsolationMode::Worktree),
                resume_from: None,
                cwd: Some("null".into()),
                model: None,
                task_id: None,
            },
        )
        .await;

        handle.await.unwrap();
        assert!(result.is_ok(), "'null' cwd + worktree should be allowed");
    }

    #[tokio::test]
    async fn cwd_whitespace_with_worktree_is_allowed() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let shared = resources.into_shared();
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert!(
                request.cwd.is_none(),
                "whitespace cwd should normalize to None, got {:?}",
                request.cwd
            );
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(shared),
            TaskToolInput {
                description: "test whitespace cwd".into(),
                prompt: "work".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: Some(SubagentIsolationMode::Worktree),
                resume_from: None,
                cwd: Some("  ".into()),
                model: None,
                task_id: None,
            },
        )
        .await;

        handle.await.unwrap();
        assert!(
            result.is_ok(),
            "whitespace cwd + worktree should be allowed"
        );
    }

    #[tokio::test]
    async fn cwd_nonexistent_path_with_worktree_is_cleared() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let shared = resources.into_shared();
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert!(
                request.cwd.is_none(),
                "non-existent cwd should be cleared when worktree is set, got {:?}",
                request.cwd
            );
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(shared),
            TaskToolInput {
                description: "test nonexistent cwd".into(),
                prompt: "work".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: Some(SubagentIsolationMode::Worktree),
                resume_from: None,
                cwd: Some("/nonexistent/path/that/does/not/exist".into()),
                model: None,
                task_id: None,
            },
        )
        .await;

        handle.await.unwrap();
        assert!(
            result.is_ok(),
            "non-existent cwd + worktree should clear cwd and proceed"
        );
    }

    #[tokio::test]
    async fn cwd_nonexistent_path_without_worktree_errors() {
        let (backend, _rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(resources.into_shared()),
            TaskToolInput {
                description: "test nonexistent cwd no worktree".into(),
                prompt: "work".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: None,
                cwd: Some("/nonexistent/path/that/does/not/exist".into()),
                model: None,
                task_id: None,
            },
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("does not exist"),
            "should reject non-existent cwd without worktree: {err}"
        );
    }

    #[tokio::test]
    async fn cwd_sentinel_without_worktree_is_normalized() {
        for sentinel in ["undefined", "null", "none", "", "  "] {
            let (backend, mut rx) = make_backend();
            let mut resources = Resources::new();
            resources.insert(backend);
            resources.insert(SubagentDepthCounter(0));
            resources.insert(SessionIdResource("parent".to_string()));
            resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

            let shared = resources.into_shared();
            let handle = tokio::spawn(async move {
                let request = unwrap_spawn(rx.recv().await.unwrap());
                assert!(
                    request.cwd.is_none(),
                    "sentinel {sentinel:?} must normalize to None without worktree, got {:?}",
                    request.cwd
                );
                request
                    .respond_with(|request| SubagentResult {
                        success: true,
                        output: "ok".into(),
                        subagent_id: request.id.clone(),
                        child_session_id: request.id.clone(),
                        ..Default::default()
                    })
                    .unwrap();
            });

            let result = echo_agent_tool_runtime::Tool::run(
                &TaskTool,
                test_ctx(shared.clone()),
                TaskToolInput {
                    description: "test sentinel cwd".into(),
                    prompt: "work".into(),
                    subagent_type: "general-purpose".into(),
                    run_in_background: false,
                    capability_mode: None,
                    isolation: None,
                    resume_from: None,
                    cwd: Some(sentinel.into()),
                    model: None,
                    task_id: None,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("sentinel {sentinel:?} should not fail: {e}"));

            handle.await.unwrap();
            match result {
                ToolOutput::SubagentCompleted(sub) => {
                    assert!(sub.output.contains("ok"), "sentinel {sentinel:?}");
                }
                other => panic!("sentinel {sentinel:?}: expected SubagentCompleted, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn cwd_threads_to_request() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let shared = resources.into_shared();
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(request.cwd.as_deref(), Some("/tmp"));
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: "done".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(shared),
            TaskToolInput {
                description: "cwd test".into(),
                prompt: "work".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: None,
                cwd: Some("/tmp".into()),
                model: None,
                task_id: None,
            },
        )
        .await
        .unwrap();

        handle.await.unwrap();
        match result {
            ToolOutput::SubagentCompleted(sub) => {
                assert!(sub.output.contains("done"));
            }
            other => panic!("Expected SubagentCompleted, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn cwd_strips_stray_leading_quote() {
        // Regression: model-emitted `"/tmp` should reach the backend as `/tmp`.
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let shared = resources.into_shared();
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(
                request.cwd.as_deref(),
                Some("/tmp"),
                "stray leading quote should be stripped before reaching the backend",
            );
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(shared),
            TaskToolInput {
                description: "stray quote cwd".into(),
                prompt: "work".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: None,
                cwd: Some("\"/tmp".into()),
                model: None,
                task_id: None,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("sanitized cwd should succeed: {e}"));

        handle.await.unwrap();
        match result {
            ToolOutput::SubagentCompleted(sub) => {
                assert!(sub.output.contains("ok"));
            }
            other => panic!("Expected SubagentCompleted, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn cwd_with_isolation_none_is_allowed() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let shared = resources.into_shared();
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(request.cwd.as_deref(), Some("/tmp"));
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(shared),
            TaskToolInput {
                description: "cwd with none".into(),
                prompt: "work".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: Some(SubagentIsolationMode::None),
                resume_from: None,
                cwd: Some("/tmp".into()),
                model: None,
                task_id: None,
            },
        )
        .await;

        handle.await.unwrap();
        assert!(result.is_ok(), "cwd with isolation=none should be allowed");
    }

    #[tokio::test]
    async fn cwd_with_resume_from_is_accepted() {
        let (backend, mut rx) = make_backend();
        let mut resources = Resources::new();
        resources.insert(backend);
        resources.insert(SubagentDepthCounter(0));
        resources.insert(SessionIdResource("parent".to_string()));
        resources.insert(CurrentPromptIdResource("prompt-1".to_string()));

        let shared = resources.into_shared();
        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            // Both values are threaded through — coordinator decides precedence.
            assert_eq!(request.cwd.as_deref(), Some("/tmp/some-dir"));
            assert_eq!(request.resume_from.as_deref(), Some("prev-id"));
            request
                .respond_with(|request| SubagentResult {
                    success: true,
                    output: "resumed".into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(shared),
            TaskToolInput {
                description: "cwd + resume".into(),
                prompt: "work".into(),
                subagent_type: "general-purpose".into(),
                run_in_background: false,
                capability_mode: None,
                isolation: None,
                resume_from: Some("prev-id".into()),
                cwd: Some("/tmp/some-dir".into()),
                model: None,
                task_id: None,
            },
        )
        .await
        .unwrap();

        handle.await.unwrap();
        match result {
            ToolOutput::SubagentCompleted(sub) => {
                assert!(sub.output.contains("resumed"));
            }
            other => panic!("Expected SubagentCompleted, got {:?}", other),
        }
    }

    // ── model override tests ─────────────────────────────────────

    #[tokio::test]
    async fn model_threads_to_runtime_overrides() {
        let (backend, mut rx) = make_backend();
        let resources = resources_for_task(backend);
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(
                request.runtime_overrides.model.as_deref(),
                Some("test-model"),
                "explicit model must reach SubagentRuntimeOverrides"
            );
            assert_eq!(
                request.runtime_overrides.model_override_provenance,
                ModelOverrideProvenance::Tool,
            );
            assert!(request.runtime_overrides.reasoning_effort.is_none());
            assert!(request.runtime_overrides.persona.is_none());
            let id = request.id.clone();
            request
                .result_tx
                .send(SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: id.clone(),
                    child_session_id: id,
                    ..Default::default()
                })
                .unwrap();
        });

        let mut input = task_input("general-purpose", false);
        input.model = Some("test-model".into());
        let result = echo_agent_tool_runtime::Tool::run(&TaskTool, test_ctx(shared), input)
            .await
            .unwrap();
        handle.await.unwrap();
        match result {
            ToolOutput::SubagentCompleted(sub) => assert!(sub.output.contains("ok")),
            other => panic!("Expected SubagentCompleted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn omitted_model_stays_none_on_runtime_overrides() {
        let (backend, mut rx) = make_backend();
        let resources = resources_for_task(backend);
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert!(
                request.runtime_overrides.model.is_none(),
                "omitted model must stay None, got {:?}",
                request.runtime_overrides.model
            );
            let id = request.id.clone();
            request
                .result_tx
                .send(SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: id.clone(),
                    child_session_id: id,
                    ..Default::default()
                })
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskTool,
            test_ctx(shared),
            task_input("general-purpose", false),
        )
        .await
        .unwrap();
        handle.await.unwrap();
        match result {
            ToolOutput::SubagentCompleted(sub) => assert!(sub.output.contains("ok")),
            other => panic!("Expected SubagentCompleted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn model_sentinel_values_treated_as_none() {
        for sentinel in [
            "",
            "  ",
            "null",
            "Null",
            "NULL",
            "none",
            "None",
            "undefined",
            "  none  ",
        ] {
            let (backend, mut rx) = make_backend();
            let resources = resources_for_task(backend);
            let shared = resources.into_shared();
            let handle = tokio::spawn(async move {
                let request = unwrap_spawn(rx.recv().await.unwrap());
                assert!(
                    request.runtime_overrides.model.is_none(),
                    "sentinel {sentinel:?} must normalize to None, got {:?}",
                    request.runtime_overrides.model
                );
                let id = request.id.clone();
                request
                    .result_tx
                    .send(SubagentResult {
                        success: true,
                        output: "ok".into(),
                        subagent_id: id.clone(),
                        child_session_id: id,
                        ..Default::default()
                    })
                    .unwrap();
            });

            let mut input = task_input("general-purpose", false);
            input.model = Some(sentinel.into());
            let result = echo_agent_tool_runtime::Tool::run(&TaskTool, test_ctx(shared), input)
                .await
                .unwrap_or_else(|e| panic!("sentinel {sentinel:?} should not fail: {e}"));
            handle.await.unwrap();
            match result {
                ToolOutput::SubagentCompleted(sub) => {
                    assert!(sub.output.contains("ok"), "sentinel {sentinel:?}");
                }
                other => panic!("sentinel {sentinel:?}: expected SubagentCompleted, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn model_whitespace_is_trimmed() {
        let (backend, mut rx) = make_backend();
        let resources = resources_for_task(backend);
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(
                request.runtime_overrides.model.as_deref(),
                Some("test-model"),
                "leading/trailing whitespace should be trimmed"
            );
            let id = request.id.clone();
            request
                .result_tx
                .send(SubagentResult {
                    success: true,
                    output: "ok".into(),
                    subagent_id: id.clone(),
                    child_session_id: id,
                    ..Default::default()
                })
                .unwrap();
        });

        let mut input = task_input("general-purpose", false);
        input.model = Some("  test-model  ".into());
        let _ = echo_agent_tool_runtime::Tool::run(&TaskTool, test_ctx(shared), input)
            .await
            .unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn resume_from_with_model_soft_ignores_model() {
        let (backend, mut rx) = make_backend();
        let resources = resources_for_task(backend);
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(request.resume_from.as_deref(), Some("prev-id"));
            assert!(
                request.runtime_overrides.model.is_none(),
                "model must be soft-ignored when resume_from is set, got {:?}",
                request.runtime_overrides.model
            );
            assert_eq!(
                request.runtime_overrides.model_override_provenance,
                ModelOverrideProvenance::Tool,
            );
            assert!(request.runtime_overrides.reasoning_effort.is_none());
            assert!(request.runtime_overrides.persona.is_none());
            let id = request.id.clone();
            request
                .result_tx
                .send(SubagentResult {
                    success: true,
                    output: "resumed".into(),
                    subagent_id: id.clone(),
                    child_session_id: id,
                    ..Default::default()
                })
                .unwrap();
        });

        let mut input = task_input("general-purpose", false);
        input.resume_from = Some("prev-id".into());
        input.model = Some("test-model".into());
        let result = echo_agent_tool_runtime::Tool::run(&TaskTool, test_ctx(shared), input)
            .await
            .unwrap();
        handle.await.unwrap();
        match result {
            ToolOutput::SubagentCompleted(sub) => assert!(sub.output.contains("resumed")),
            other => panic!("Expected SubagentCompleted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_from_without_model_still_spawns() {
        let (backend, mut rx) = make_backend();
        let resources = resources_for_task(backend);
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let request = unwrap_spawn(rx.recv().await.unwrap());
            assert_eq!(request.resume_from.as_deref(), Some("prev-id"));
            assert!(request.runtime_overrides.model.is_none());
            let id = request.id.clone();
            request
                .result_tx
                .send(SubagentResult {
                    success: true,
                    output: "resumed".into(),
                    subagent_id: id.clone(),
                    child_session_id: id,
                    ..Default::default()
                })
                .unwrap();
        });

        let mut input = task_input("general-purpose", false);
        input.resume_from = Some("prev-id".into());
        let result = echo_agent_tool_runtime::Tool::run(&TaskTool, test_ctx(shared), input)
            .await
            .unwrap();
        handle.await.unwrap();
        match result {
            ToolOutput::SubagentCompleted(sub) => assert!(sub.output.contains("resumed")),
            other => panic!("Expected SubagentCompleted, got {other:?}"),
        }
    }
}
