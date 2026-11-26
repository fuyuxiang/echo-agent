//! Concise variant of the `read_file` tool.

use crate::implementations::echo_agent_build::read_file::{ReadFileInput, run_read_file};

const DESCRIPTION_CONCISE: &str = r#"Reads a file from the computer's filesystem. You can access any file directly by using this tool.
It is okay to read a file that does not exist; an error will be returned.

Usage:
- You can optionally specify ${{ params.read.offset }} and ${{ params.read.limit }} (especially handy for long files).
- Lines in the output are numbered starting at 1, using following format: LINE_NUMBER→LINE_CONTENT.
- You have the capability to call multiple tools in a single response. It is always better to speculatively read multiple files as a batch that are potentially useful."#;
use crate::types::output::ReadFileOutput;
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};

/// Concise variant of `ReadFileTool`.
///
/// Delegates to `run_read_file()`, then swaps `content_concise` into `content`
/// (no line-number padding).
#[derive(Debug, Default)]
pub struct ReadFileConciseTool;

impl crate::types::tool_metadata::ToolMetadata for ReadFileConciseTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuildConcise
    }

    fn description_template(&self) -> &str {
        DESCRIPTION_CONCISE
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for ReadFileConciseTool {
    type Args = ReadFileInput;
    type Output = ReadFileOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("read_file").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "read_file",
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
        name = "tool.read_file_concise",
        skip_all,
        fields(path = %input.path)
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: ReadFileInput,
    ) -> Result<ReadFileOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        // EchoAgentBuildConcise is not version-managed — always pass None.
        let cwd_override = ctx
            .extensions
            .get::<echo_agent_tool_runtime::Cwd>()
            .map(|c| c.0.clone());
        // `None`: the concise tool does not stream, so it needs no
        // text-path streamability signal (see `run_read_file`).
        let invoking = crate::types::tool_metadata::invoking_param_names(&ctx);
        let result = run_read_file(input, cwd_override, None, resources, None, &invoking).await?;

        match result {
            ReadFileOutput::FileContent(mut fc) => {
                if let Some(concise) = fc.content_concise.take() {
                    fc.content = concise;
                }
                Ok(ReadFileOutput::FileContent(fc))
            }
            other => Ok(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx;

    use crate::computer::local::LocalFs;
    use crate::implementations::echo_agent_build::read_file::ReadFileTool;
    use crate::notification::types::ToolNotificationHandle;
    use crate::types::resources::{Cwd, FileSystem, NotificationHandle, Resources};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn test_resources(cwd: &std::path::Path) -> Resources {
        let mut resources = Resources::new();
        resources.insert(Cwd(cwd.to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        resources
    }

    #[test]
    fn description_template_selects_concise() {
        use crate::types::tool_metadata::ToolMetadata;
        let default_tool = ReadFileTool;
        let concise_tool = ReadFileConciseTool;
        assert_ne!(
            ToolMetadata::description_template(&default_tool),
            ToolMetadata::description_template(&concise_tool),
        );
        assert_eq!(
            ToolMetadata::description_template(&concise_tool),
            super::DESCRIPTION_CONCISE
        );
    }

    #[test]
    fn description_template_tracks_renamed_offset_limit() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool_metadata::ToolMetadata;
        use std::collections::HashMap;

        let tools = HashMap::from([(ToolKind::Read, "read_file".to_string())]);
        let params = HashMap::from([(
            ToolKind::Read,
            HashMap::from([
                ("offset".to_string(), "start_line".to_string()),
                ("limit".to_string(), "num_lines".to_string()),
            ]),
        )]);
        let rendered = TemplateRenderer::new(tools, params)
            .render(ToolMetadata::description_template(&ReadFileConciseTool))
            .unwrap();
        assert!(
            rendered.contains("start_line and num_lines"),
            "renamed offset/limit must appear:\n{rendered}"
        );
        assert!(
            !rendered.contains("a line offset and limit"),
            "canonical offset/limit must not remain after rename:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn concise_mode_uses_concise_content() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello\nworld\n").unwrap();

        let tool = ReadFileConciseTool;
        let resources = test_resources(tmp.path());

        let input = ReadFileInput {
            document: None,
            path: "test.txt".to_string(),
            offset: None,
            limit: None,
            pages: None,
            format: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileContent(content) => {
                assert_eq!(content.content, "1→hello\nworld\n");
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }
}
