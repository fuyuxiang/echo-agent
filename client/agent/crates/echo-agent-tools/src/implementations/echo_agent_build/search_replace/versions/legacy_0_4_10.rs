//! Legacy (0.4.10) error downgrade for `search_replace`.
//!
//! Restores exact historical 0.4.10 wording by collapsing structured error
//! variants (`FileNotFound`, `MultipleMatchesFound`, etc.) to generic
//! `InvalidInput`.

use crate::types::output::SearchReplaceOutput;
use crate::types::resources::SharedResources;
use crate::types::template_renderer::TemplateRenderer;

#[derive(Debug, Clone, Copy)]
pub(crate) enum ErrorKind {
    FileAlreadyExists,
    FileNotFound,
    MultipleMatchesFound,
    NoMatchesFound,
}

#[derive(Debug, Clone)]
struct RenderContext {
    file_path: String,
    read_tool_name: String,
    replace_all_param_name: String,
    old_string_param_name: String,
}

fn render_error(kind: ErrorKind, ctx: &RenderContext) -> String {
    match kind {
        ErrorKind::FileAlreadyExists => format!(
            "{} is empty, which is only allowed when creating a new file or when the file is empty.",
            ctx.old_string_param_name
        ),
        ErrorKind::FileNotFound => {
            format!(
                "File not found: {}. Please check the path and try again.",
                ctx.file_path
            )
        }
        ErrorKind::MultipleMatchesFound => format!(
            "The string to replace was found multiple times in the file. Use {} to replace all occurrences, or include more context to only edit one occurrence.",
            ctx.replace_all_param_name
        ),
        ErrorKind::NoMatchesFound => format!(
            "The string to replace was not found in the file, use the {} tool to see the correct string.",
            ctx.read_tool_name
        ),
    }
}

async fn build_render_context(
    resources: &SharedResources,
    file_path: &str,
) -> Result<RenderContext, echo_agent_tool_runtime::ToolError> {
    let res = resources.lock().await;
    let renderer = res.require::<TemplateRenderer>()?;
    let read_tool_name = renderer
        .render("${{ tools.by_kind.read }}")
        .map_err(|e| echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string()))?;
    let replace_all_param_name = renderer
        .render("${{ params.edit.replace_all }}")
        .map_err(|e| echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string()))?;
    let old_string_param_name = renderer
        .render("${{ params.edit.old_string }}")
        .map_err(|e| echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string()))?;
    Ok(RenderContext {
        file_path: file_path.to_string(),
        read_tool_name,
        replace_all_param_name,
        old_string_param_name,
    })
}

/// Downgrade structured error variants to generic `InvalidInput` for legacy,
/// restoring exact historical 0.4.10 wording.
pub(crate) async fn downgrade_structured_errors(
    output: SearchReplaceOutput,
    resources: &SharedResources,
    file_path: &str,
) -> Result<SearchReplaceOutput, echo_agent_tool_runtime::ToolError> {
    use SearchReplaceOutput::*;
    let ctx = build_render_context(resources, file_path).await?;
    Ok(match output {
        FileAlreadyExists(_) => InvalidInput(render_error(ErrorKind::FileAlreadyExists, &ctx)),
        FileNotFound(_) => InvalidInput(render_error(ErrorKind::FileNotFound, &ctx)),
        FilenameTooLong(msg) => InvalidInput(msg),
        MultipleMatchesFound(_) => {
            InvalidInput(render_error(ErrorKind::MultipleMatchesFound, &ctx))
        }
        NoMatchesFound(_) => InvalidInput(render_error(ErrorKind::NoMatchesFound, &ctx)),
        other => other,
    })
}
