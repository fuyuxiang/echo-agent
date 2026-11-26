//! Concise variant of the `search_replace` tool.

use crate::implementations::echo_agent_build::search_replace::{
    SearchReplaceInput, run_search_replace,
};

/// Concise description — no read-before-edit enforcement, simplified formatting guidance.
const DESCRIPTION_CONCISE: &str = r#"Replace an exact string in a file.

- Do not include the "LINE_NUMBER→" prefixes from file reads in ${{ params.edit.old_string }} or ${{ params.edit.new_string }}; keep the exact indentation.
- ${{ params.edit.old_string }} must match exactly one place in the file. If it appears more than once, add surrounding lines to make it unique, or set ${{ params.edit.replace_all }} to change every occurrence (handy for renaming an identifier).
- To create a new file, set ${{ params.edit.old_string }} to an empty string."#;
use crate::types::output::SearchReplaceOutput;
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};

/// Concise variant of `SearchReplaceTool`.
///
/// Differences from `SearchReplaceTool`:
/// - Always skips the read-before-edit guard.
/// - Uses the shorter `tool_output_for_prompt_concise` as prompt output.
/// - No `IfParams` requirement for a Read tool (guard is always skipped).
#[derive(Debug, Default)]
pub struct SearchReplaceConciseTool;

impl crate::types::tool_metadata::ToolMetadata for SearchReplaceConciseTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuildConcise
    }

    fn description_template(&self) -> &str {
        DESCRIPTION_CONCISE
    }

    fn emitted_notifications(&self) -> &'static [&'static str] {
        &["FileWritten"]
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::And(vec![
            Expr::Value(ToolRequirement::input_param(ToolKind::Edit, "old_string")),
            Expr::Value(ToolRequirement::input_param(ToolKind::Edit, "new_string")),
            Expr::Value(ToolRequirement::input_param(ToolKind::Edit, "replace_all")),
        ])
    }
}

impl echo_agent_tool_runtime::Tool for SearchReplaceConciseTool {
    type Args = SearchReplaceInput;
    type Output = SearchReplaceOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("search_replace").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "search_replace",
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
        name = "tool.search_replace_concise",
        skip_all,
        fields(
            file_path = %input.file_path,
            replace_all = %input.replace_all,
        )
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: SearchReplaceInput,
    ) -> Result<SearchReplaceOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let mut result = run_search_replace(input, &ctx, resources).await?;

        if let SearchReplaceOutput::EditsApplied(ref mut applied) = result
            && let Some(concise_text) = applied.tool_output_for_prompt_concise.take()
        {
            applied.tool_output_for_prompt = concise_text;
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx;

    use crate::computer::local::LocalFs;
    use crate::notification::types::ToolNotificationHandle;
    use crate::types::resources::{Cwd, FileSystem, NotificationHandle, Resources};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn test_resources(cwd: &std::path::Path) -> Resources {
        let mut resources = Resources::new();
        resources.insert(Cwd(cwd.to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));

        let edit_params = std::collections::HashMap::from([
            ("old_string".to_string(), "old_string".to_string()),
            ("new_string".to_string(), "new_string".to_string()),
            ("replace_all".to_string(), "replace_all".to_string()),
        ]);
        let kind_map = std::collections::HashMap::from([(ToolKind::Edit, edit_params)]);
        let tool_map = std::collections::HashMap::from([
            (ToolKind::Read, "read_file".to_string()),
            (ToolKind::Edit, "search_replace".to_string()),
        ]);
        resources.insert(crate::types::template_renderer::TemplateRenderer::new(
            tool_map, kind_map,
        ));
        resources
    }

    fn make_input(file: &str, old: &str, new: &str) -> SearchReplaceInput {
        SearchReplaceInput {
            file_path: file.to_string(),
            old_string: old.to_string(),
            new_string: new.to_string(),
            replace_all: false,
        }
    }

    #[tokio::test]
    async fn concise_tool_skips_read_guard() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "hello world\n").unwrap();

        let tool = SearchReplaceConciseTool;
        let resources = test_resources(tmp.path());

        let input = make_input("test.txt", "hello", "goodbye");
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                let content = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, "goodbye world\n");
                assert_eq!(
                    applied.tool_output_for_prompt,
                    "The file test.txt has been updated."
                );
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn concise_replace_all_output() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "aaa bbb aaa bbb aaa\n").unwrap();

        let tool = SearchReplaceConciseTool;
        let resources = test_resources(tmp.path());

        let input = SearchReplaceInput {
            file_path: "test.txt".to_string(),
            old_string: "aaa".to_string(),
            new_string: "ccc".to_string(),
            replace_all: true,
        };
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::EditsApplied(applied) => {
                let content = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
                assert_eq!(content, "ccc bbb ccc bbb ccc\n");
                assert_eq!(
                    applied.tool_output_for_prompt,
                    "The file test.txt has been updated. All occurrences were replaced."
                );
            }
            other => panic!("Expected EditsApplied, got {:?}", other),
        }
    }
}
