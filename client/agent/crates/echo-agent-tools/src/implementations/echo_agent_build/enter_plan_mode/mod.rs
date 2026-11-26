//! `EnterPlanMode` tool — new architecture (`Tool` trait).
//!
//! Gateway tool that the agent calls when it decides a task is complex enough
//! to warrant a planning phase before writing code. This is the
//! **agent-initiated** entry path into plan mode.
//!
//! On success it notifies orchestration (`PlanModeEntered`) and seeds an empty
//! session plan file if missing (never truncating existing content), so the
//! model can read it before writing. Read-only enforcement and plan-file gating
//! stay in orchestration.
//!
//! ## User Consent
//!
//! This tool requires user approval before executing. The UI should present a
//! confirmation dialog. If the user declines, the tool result is rejected and
//! the model receives `"User declined to enter plan mode."`.

use crate::computer::types::AsyncFileSystem;
use crate::notification::types::PlanModeEntered;
use crate::types::output::{
    EnterPlanModeOutput, EnterPlanModeToolHints, PlanFileSeedFailure, PlanFileSeedStatus,
};
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::{FileSystem, NotificationHandle, resolve_plan_file_path};
use crate::types::template_renderer::TemplateRenderer;
use crate::types::tool::{ToolKind, ToolNamespace};
use std::path::Path;
use std::sync::Arc;

/// Input for the `EnterPlanMode` tool.
///
/// Empty object — no parameters. The decision to enter plan mode is a binary
/// gate. All configuration (workflow variant, explore agent count, etc.) comes
/// from feature flags and environment variables, not from the tool call.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct EnterPlanModeInput {}

/// `EnterPlanMode` tool: signals plan mode entry and seeds the session plan
/// file, returning a [`PlanFileSeedStatus`].
///
/// Params: `()` — no per-tool configuration.
#[derive(Debug, Default)]
pub struct EnterPlanModeTool;

impl crate::types::tool_metadata::ToolMetadata for EnterPlanModeTool {
    fn kind(&self) -> ToolKind {
        ToolKind::EnterPlan
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn emitted_notifications(&self) -> &'static [&'static str] {
        &["PlanModeEntered"]
    }

    fn description_template(&self) -> &str {
        r#"Use this tool when a task has ambiguity about the right approach or when the user asks you to write a plan. This tool enables a read-only plan mode where you explore the codebase and create an implementation plan for the user."#
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        // EnterPlanMode can only exist if ExitPlanMode is also registered —
        // entering plan mode without the ability to exit would be a dead-end.
        use crate::implementations::echo_agent_build::exit_plan_mode::ExitPlanModeTool;
        Expr::Value(ToolRequirement::Tool {
            namespace: crate::types::tool_metadata::ToolMetadata::tool_namespace(&ExitPlanModeTool)
                .to_string(),
            id: echo_agent_tool_runtime::Tool::id(&ExitPlanModeTool).to_string(),
            if_params: None,
        })
    }
}

impl echo_agent_tool_runtime::Tool for EnterPlanModeTool {
    type Args = EnterPlanModeInput;
    type Output = EnterPlanModeOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("enter_plan_mode").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "enter_plan_mode",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        // Read-only for permission UX; only FS write is seeding the session plan file.
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.enter_plan_mode", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        _input: EnterPlanModeInput,
    ) -> Result<EnterPlanModeOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let (seed_target, plan_file_path, tool_hints, fs) = {
            let res = resources.lock().await;

            // Send notification first.
            if let Some(handle) = res.get::<NotificationHandle>() {
                handle.0.send_plan_mode_entered(PlanModeEntered {
                    tool_call_id: ctx.call_id.as_str().to_owned(),
                });
            }

            let (seed_target, plan_file_path) = resolve_plan_file_path(&res);

            // Resolve client-facing tool names via TemplateRenderer.
            // Presence-aware lookups (not template renders): a missing kind
            // renders as empty-`Ok`, so a `Result` fallback never fires.
            let hints = if let Some(renderer) = res.get::<TemplateRenderer>() {
                EnterPlanModeToolHints {
                    ask_user: renderer
                        .tool_for_kind(crate::types::tool::ToolKind::AskUser)
                        .unwrap_or("ask_user_question")
                        .to_owned(),
                    exit_plan: renderer
                        .tool_for_kind(crate::types::tool::ToolKind::ExitPlan)
                        .unwrap_or("exit_plan_mode")
                        .to_owned(),
                    task: renderer
                        .tool_for_kind(crate::types::tool::ToolKind::Task)
                        .unwrap_or_default()
                        .to_owned(),
                }
            } else {
                EnterPlanModeToolHints::default()
            };

            let fs = res.get::<FileSystem>().map(|f| Arc::clone(&f.0));

            (seed_target, plan_file_path, hints, fs)
        };

        // Seed only with both an FS and an absolute target; never write a relative path or truncate.
        let plan_file_seed = match (fs.as_ref(), seed_target.as_deref()) {
            (Some(fs), Some(target)) => probe_or_create_empty_plan_file(fs.as_ref(), target).await,
            _ => {
                tracing::warn!(
                    %plan_file_path,
                    "No FileSystem resource or no absolute plan path; not seeding plan file"
                );
                PlanFileSeedStatus::Missing(PlanFileSeedFailure::Unavailable)
            }
        };

        tracing::info!(
            %plan_file_path,
            ?plan_file_seed,
            "Entered plan mode"
        );

        Ok(EnterPlanModeOutput::Entered {
            message: "You have entered plan mode. You should now focus on exploring the codebase \
                      and creating an implementation plan."
                .to_string(),
            plan_file_path,
            tool_hints,
            plan_file_seed,
        })
    }
}

/// Probe the plan file; create an empty one only on not-found.
///
/// Never truncates existing content. Non-NotFound read errors fail closed as
/// [`PlanFileSeedStatus::Missing`] without calling `write_file`.
async fn probe_or_create_empty_plan_file(
    fs: &dyn AsyncFileSystem,
    path: &Path,
) -> PlanFileSeedStatus {
    match fs.read_file(path).await {
        Ok(bytes) if bytes.is_empty() => PlanFileSeedStatus::Empty,
        Ok(_) => PlanFileSeedStatus::NonEmpty,
        Err(e) if e.io_error_kind() == Some(std::io::ErrorKind::NotFound) => {
            match fs.write_file(path, b"").await {
                Ok(()) => PlanFileSeedStatus::Empty,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        "Failed to create empty plan file"
                    );
                    PlanFileSeedStatus::Missing(PlanFileSeedFailure::NotCreated)
                }
            }
        }
        Err(e) => {
            // Non-NotFound read error: a directory at the path reads as IsADirectory;
            // anything else is treated as inaccessible. Never write (avoid truncate risk).
            let reason = match e.io_error_kind() {
                Some(std::io::ErrorKind::IsADirectory) => PlanFileSeedFailure::NotAFile,
                _ => PlanFileSeedFailure::Inaccessible,
            };
            tracing::warn!(
                error = %e,
                ?reason,
                path = %path.display(),
                "Failed to probe plan file; not creating (avoid truncate risk)"
            );
            PlanFileSeedStatus::Missing(reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer::local::LocalFs;
    use crate::computer::types::ComputerError;
    use crate::types::output::ToolOutput;
    use crate::types::resources::{Cwd, PlanFilePath, Resources};
    use crate::types::tool_metadata::test_ctx_with_call_id;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    fn resources_with_plan_fs(tmp: &TempDir) -> (Resources, PathBuf) {
        let plan = tmp.path().join("session").join("plan.md");
        let mut resources = Resources::new();
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(PlanFilePath(plan.clone()));
        (resources, plan)
    }

    /// Parametrized FS mock: injects the read/write outcomes and counts calls
    /// so a test can assert the tool never touched the FS.
    struct ProbeMockFs {
        read: Result<Vec<u8>, ComputerError>,
        write: Result<(), ComputerError>,
        reads: AtomicUsize,
        writes: AtomicUsize,
    }

    impl ProbeMockFs {
        fn new(read: Result<Vec<u8>, ComputerError>, write: Result<(), ComputerError>) -> Self {
            Self {
                read,
                write,
                reads: AtomicUsize::new(0),
                writes: AtomicUsize::new(0),
            }
        }

        fn err(kind: std::io::ErrorKind) -> ComputerError {
            ComputerError::IOError(format!("{kind:?}"), Some(kind))
        }
    }

    #[async_trait::async_trait]
    impl AsyncFileSystem for ProbeMockFs {
        async fn read_file(&self, _path: &Path) -> Result<Vec<u8>, ComputerError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.read.clone()
        }

        async fn write_file(&self, _path: &Path, _data: &[u8]) -> Result<(), ComputerError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.write.clone()
        }

        async fn delete_file(&self, _path: &Path) -> Result<(), ComputerError> {
            Ok(())
        }
    }

    #[test]
    fn tool_name_and_description() {
        let tool = EnterPlanModeTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "enter_plan_mode"
        );
        let desc = crate::types::tool_metadata::ToolMetadata::description_template(&tool);
        assert!(desc.contains("plan mode"));
    }

    #[test]
    fn tool_is_read_only() {
        let tool = EnterPlanModeTool;
        assert!(echo_agent_tool_runtime::Tool::capabilities(&tool).is_read_only);
    }

    #[test]
    fn tool_kind_is_enter_plan() {
        let tool = EnterPlanModeTool;
        assert_eq!(
            crate::types::tool_metadata::ToolMetadata::kind(&tool),
            ToolKind::EnterPlan
        );
    }

    #[tokio::test]
    async fn enter_plan_mode_returns_confirmation() {
        let tmp = TempDir::new().unwrap();
        let (resources, _) = resources_with_plan_fs(&tmp);
        let shared = resources.into_shared();
        let tool = EnterPlanModeTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(shared, "test-call"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let EnterPlanModeOutput::Entered {
            ref message,
            ref plan_file_path,
            plan_file_seed,
            ..
        } = result;
        assert!(message.contains("entered plan mode"));
        assert!(message.contains("exploring the codebase"));
        assert!(message.contains("implementation plan"));
        assert!(plan_file_path.contains("plan.md"));
        assert_eq!(plan_file_seed, PlanFileSeedStatus::Empty);
    }

    #[tokio::test]
    async fn sends_plan_mode_entered_notification() {
        use crate::notification::types::{ToolNotification, ToolNotificationHandle};

        let (handle, mut rx) = ToolNotificationHandle::channel();
        let mut resources = Resources::new();
        resources.insert(NotificationHandle(handle));
        let shared = resources.into_shared();
        let tool = EnterPlanModeTool;

        echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(shared, "call-42"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let notification = rx.try_recv().expect("should have received a notification");
        match notification {
            ToolNotification::PlanModeEntered(entered) => {
                assert_eq!(entered.tool_call_id, "call-42");
            }
            other => panic!("Expected PlanModeEntered, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn works_without_notification_handle() {
        let resources = Resources::new();
        let shared = resources.into_shared();
        let tool = EnterPlanModeTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(shared, "test-call"),
            EnterPlanModeInput {},
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn without_filesystem_resource_plan_not_ready() {
        let resources = Resources::new();
        let shared = resources.into_shared();

        let result = echo_agent_tool_runtime::Tool::run(
            &EnterPlanModeTool,
            test_ctx_with_call_id(shared, "test-call"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let EnterPlanModeOutput::Entered { plan_file_seed, .. } = &result;
        assert_eq!(
            *plan_file_seed,
            PlanFileSeedStatus::Missing(PlanFileSeedFailure::Unavailable)
        );

        let output: ToolOutput = result.into();
        let prompt = output.to_prompt_format();
        assert!(
            prompt.contains("The plan file location is unavailable."),
            "expected not-ready status: {prompt}"
        );
        assert!(
            prompt.contains("5. Write your plan to the plan file above"),
            "expected constant write-plan step: {prompt}"
        );
    }

    #[tokio::test]
    async fn no_absolute_path_does_not_write() {
        // FileSystem present but no PlanFilePath and no Cwd: nothing to anchor an
        // absolute path on, so the tool must not write a relative path.
        let fs = Arc::new(ProbeMockFs::new(
            Err(ProbeMockFs::err(std::io::ErrorKind::NotFound)),
            Ok(()),
        ));
        let mut resources = Resources::new();
        resources.insert(FileSystem(fs.clone()));
        let shared = resources.into_shared();

        let result = echo_agent_tool_runtime::Tool::run(
            &EnterPlanModeTool,
            test_ctx_with_call_id(shared, "no-anchor"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let EnterPlanModeOutput::Entered {
            plan_file_seed,
            plan_file_path,
            ..
        } = &result;
        assert_eq!(
            *plan_file_seed,
            PlanFileSeedStatus::Missing(PlanFileSeedFailure::Unavailable)
        );
        assert_eq!(plan_file_path, ".echo-agent/plan.md");
        assert_eq!(fs.reads.load(Ordering::SeqCst), 0, "must not probe");
        assert_eq!(fs.writes.load(Ordering::SeqCst), 0, "must not write");
    }

    #[tokio::test]
    async fn prompt_format_returns_message() {
        let tmp = TempDir::new().unwrap();
        let (resources, _) = resources_with_plan_fs(&tmp);
        let shared = resources.into_shared();
        let tool = EnterPlanModeTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(shared, "test-call"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let output: ToolOutput = result.into();
        let prompt = output.to_prompt_format();
        assert!(prompt.contains("entered plan mode"));
        assert!(prompt.contains("plan.md"));
        assert!(
            prompt.contains("The file exists and is empty."),
            "expected empty plan status: {prompt}"
        );
        assert!(prompt.contains("exit_plan_mode"));
        assert!(prompt.contains("ask_user_question"));
        assert!(prompt.contains("5. Write your plan to the plan file above"));
        assert!(
            prompt.contains("6. When ready, use exit_plan_mode to present your plan to the user")
        );
        assert!(
            !prompt.contains("create it at that path first if needed"),
            "ready path should not include not-ready create hint: {prompt}"
        );
    }

    #[tokio::test]
    async fn does_not_truncate_existing_nonempty_plan() {
        let tmp = TempDir::new().unwrap();
        let (resources, plan_path) = resources_with_plan_fs(&tmp);
        let shared = resources.into_shared();

        let fs = LocalFs;
        fs.write_file(&plan_path, b"# prior plan\n").await.unwrap();

        let result = echo_agent_tool_runtime::Tool::run(
            &EnterPlanModeTool,
            test_ctx_with_call_id(shared, "reentry"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let EnterPlanModeOutput::Entered { plan_file_seed, .. } = &result;
        assert_eq!(*plan_file_seed, PlanFileSeedStatus::NonEmpty);

        let bytes = fs.read_file(&plan_path).await.unwrap();
        assert_eq!(bytes, b"# prior plan\n");

        let output: ToolOutput = result.into();
        let prompt = output.to_prompt_format();
        assert!(
            prompt.contains("The file exists but is not empty."),
            "expected nonempty status: {prompt}"
        );
    }

    #[tokio::test]
    async fn existing_empty_plan_reports_empty_without_rewrite() {
        let tmp = TempDir::new().unwrap();
        let (resources, plan_path) = resources_with_plan_fs(&tmp);
        let shared = resources.into_shared();

        let fs = LocalFs;
        fs.write_file(&plan_path, b"").await.unwrap();

        let result = echo_agent_tool_runtime::Tool::run(
            &EnterPlanModeTool,
            test_ctx_with_call_id(shared, "empty-reentry"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let EnterPlanModeOutput::Entered { plan_file_seed, .. } = &result;
        assert_eq!(*plan_file_seed, PlanFileSeedStatus::Empty);

        let bytes = fs.read_file(&plan_path).await.unwrap();
        assert_eq!(bytes, b"");
    }

    #[tokio::test]
    async fn probe_or_create_empty_plan_file_via_fs_creates_parents() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested").join("dir").join("plan.md");
        let fs = LocalFs;
        let status = probe_or_create_empty_plan_file(&fs, &path).await;
        assert_eq!(status, PlanFileSeedStatus::Empty);
        assert!(path.is_file());
        assert_eq!(fs.read_file(&path).await.unwrap(), b"");
    }

    #[tokio::test]
    async fn non_not_found_read_error_does_not_write() {
        let fs = ProbeMockFs::new(
            Err(ProbeMockFs::err(std::io::ErrorKind::PermissionDenied)),
            Ok(()),
        );
        let path = Path::new("/session/plan.md");
        let status = probe_or_create_empty_plan_file(&fs, path).await;
        assert_eq!(
            status,
            PlanFileSeedStatus::Missing(PlanFileSeedFailure::Inaccessible)
        );
        assert_eq!(fs.writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn write_failure_after_not_found_returns_missing() {
        let fs = ProbeMockFs::new(
            Err(ProbeMockFs::err(std::io::ErrorKind::NotFound)),
            Err(ProbeMockFs::err(std::io::ErrorKind::Other)),
        );
        let path = Path::new("/session/plan.md");
        let status = probe_or_create_empty_plan_file(&fs, path).await;
        assert_eq!(
            status,
            PlanFileSeedStatus::Missing(PlanFileSeedFailure::NotCreated)
        );
        assert_eq!(fs.writes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn directory_at_path_reports_not_a_file() {
        let fs = ProbeMockFs::new(
            Err(ProbeMockFs::err(std::io::ErrorKind::IsADirectory)),
            Ok(()),
        );
        let path = Path::new("/session/plan.md");
        let status = probe_or_create_empty_plan_file(&fs, path).await;
        assert_eq!(
            status,
            PlanFileSeedStatus::Missing(PlanFileSeedFailure::NotAFile)
        );
        assert_eq!(
            fs.writes.load(Ordering::SeqCst),
            0,
            "must not write over a directory"
        );
    }

    // -- PlanFilePath resource tests --

    #[tokio::test]
    async fn uses_plan_file_path_resource_when_set() {
        let mut resources = Resources::new();
        let session_plan = PathBuf::from("/home/user/.echo-agent/sessions/abc123/plan.md");
        resources.insert(PlanFilePath(session_plan.clone()));
        let shared = resources.into_shared();

        let result = echo_agent_tool_runtime::Tool::run(
            &EnterPlanModeTool,
            test_ctx_with_call_id(shared, "t1"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let EnterPlanModeOutput::Entered {
            ref plan_file_path, ..
        } = result;
        assert_eq!(plan_file_path, &session_plan.display().to_string());
        assert!(!plan_file_path.contains(".echo-agent/plan.md"));
    }

    #[tokio::test]
    async fn falls_back_to_cwd_when_no_plan_file_path_resource() {
        let mut resources = Resources::new();
        resources.insert(Cwd(PathBuf::from("/workspace/my-project")));
        let shared = resources.into_shared();

        let result = echo_agent_tool_runtime::Tool::run(
            &EnterPlanModeTool,
            test_ctx_with_call_id(shared, "t2"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let EnterPlanModeOutput::Entered {
            ref plan_file_path, ..
        } = result;
        assert_eq!(plan_file_path, "/workspace/my-project/.echo-agent/plan.md");
    }

    #[tokio::test]
    async fn tool_hints_resolved_from_template_renderer() {
        use std::collections::HashMap;

        let mut resources = Resources::new();
        let tools: HashMap<ToolKind, String> = [
            (ToolKind::AskUser, "AskUser".to_owned()),
            (ToolKind::ExitPlan, "FinishPlan".to_owned()),
            (ToolKind::Task, "delegate".to_owned()),
        ]
        .into();
        resources.insert(TemplateRenderer::new(tools, HashMap::new()));
        let shared = resources.into_shared();

        let result = echo_agent_tool_runtime::Tool::run(
            &EnterPlanModeTool,
            test_ctx_with_call_id(shared, "t5"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let EnterPlanModeOutput::Entered { tool_hints, .. } = &result;
        assert_eq!(tool_hints.ask_user, "AskUser");
        assert_eq!(tool_hints.exit_plan, "FinishPlan");
        assert_eq!(tool_hints.task, "delegate");
    }

    #[tokio::test]
    async fn tool_hints_default_without_template_renderer() {
        let resources = Resources::new();
        let shared = resources.into_shared();

        let result = echo_agent_tool_runtime::Tool::run(
            &EnterPlanModeTool,
            test_ctx_with_call_id(shared, "t6"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let EnterPlanModeOutput::Entered { tool_hints, .. } = &result;
        assert_eq!(tool_hints.ask_user, "ask_user_question");
        assert_eq!(tool_hints.exit_plan, "exit_plan_mode");
        assert!(
            tool_hints.task.is_empty(),
            "task should be empty when no TemplateRenderer and no Task tool registered"
        );
    }

    #[tokio::test]
    async fn plan_file_path_prefers_resource_over_cwd() {
        let mut resources = Resources::new();
        resources.insert(Cwd(PathBuf::from("/workspace/my-project")));
        resources.insert(PlanFilePath(PathBuf::from(
            "/home/user/.echo-agent/sessions/xyz/plan.md",
        )));
        let shared = resources.into_shared();

        let result = echo_agent_tool_runtime::Tool::run(
            &EnterPlanModeTool,
            test_ctx_with_call_id(shared, "t4"),
            EnterPlanModeInput {},
        )
        .await
        .unwrap();

        let EnterPlanModeOutput::Entered {
            ref plan_file_path, ..
        } = result;
        assert_eq!(
            plan_file_path,
            "/home/user/.echo-agent/sessions/xyz/plan.md"
        );
        assert!(!plan_file_path.contains(".echo-agent/plan.md"));
    }
}
