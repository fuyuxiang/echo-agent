//! `memory_get` tool — new architecture (`Tool` trait).

use std::sync::Arc;

use super::types::MemoryGetInput;
use crate::types::memory_backend::MemoryBackend;
use crate::types::output::ToolOutput;
use crate::types::tool::{ToolKind, ToolNamespace};

/// Format content with line numbers: `{line_num}→{line}`.
///
/// Extracted as a free function so it can be unit-tested independently of
/// the async tool infrastructure.  `first_line_num` is the 1-based number
/// for the first line of `content` (accounts for `from` offset).
///
/// Uses `split('\n')` rather than `lines()` so that content ending with a
/// newline (`"a\n"`) emits a trailing blank numbered line, matching the
/// behavior of the standard `read_file` tool.  `lines()` would silently drop
/// that trailing element, causing off-by-one line references for files
/// (virtually all Markdown memory files) that end with a newline.
pub(crate) fn format_with_line_numbers(content: &str, first_line_num: usize) -> String {
    if content.is_empty() {
        return String::new();
    }
    content
        .split('\n')
        .enumerate()
        .map(|(i, line)| format!("{}→{}", first_line_num + i, line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Default)]
pub struct MemoryGetImpl;

impl crate::types::tool_metadata::ToolMetadata for MemoryGetImpl {
    fn kind(&self) -> ToolKind {
        ToolKind::MemoryGet
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        "Read a memory file by path. Returns the file content with line numbers, optionally \
         limited to a range of lines.\n\n\
         Use after `memory_search` returns a relevant result and you need the full context \
         around a snippet, or to read a specific MEMORY.md file in full.\n\n\
         Line numbers are 1-based and match the line offsets accepted by the `from` parameter, \
         so targeted follow-up reads or edits can reference exact positions."
    }
}

impl echo_agent_tool_runtime::Tool for MemoryGetImpl {
    type Args = MemoryGetInput;
    type Output = ToolOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("memory_get").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "memory_get",
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

    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: MemoryGetInput,
    ) -> Result<ToolOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;
        let Some(memory) = resources
            .lock()
            .await
            .get::<Arc<dyn MemoryBackend>>()
            .cloned()
        else {
            return Ok(ToolOutput::Text(
                "Memory is not enabled. Use --experimental-memory to enable.".into(),
            ));
        };
        let memory = memory.clone();
        tracing::info!(target: crate::types::memory_backend::MEMORY_LOG_TARGET,"MEMORY_GET: invoked");
        // `from` is 1-based in the client schema (matching displayed line
        // numbers); the backend expects a 0-based offset. 0 is treated as 1.
        let from_zero_based = input.from.map(|f| f.saturating_sub(1));
        let content = memory
            .get(&input.path, from_zero_based, input.lines)
            .map_err(|e| {
                echo_agent_tool_runtime::ToolError::execution(
                    echo_agent_tool_protocol::ToolId::new("memory_get").expect("valid"),
                    format!("memory get failed: {e}"),
                )
            })?;
        let total_lines = content.lines().count();
        let first_line_num = from_zero_based.unwrap_or(0) + 1;
        let numbered = format_with_line_numbers(&content, first_line_num);
        let output = format!(
            "**File:** {}\n**Lines:** {} (from: {}, limit: {})\n\n{}",
            input.path,
            total_lines,
            input.from.map_or("start".to_string(), |f| f.to_string()),
            input.lines.map_or("all".to_string(), |l| l.to_string()),
            numbered,
        );
        Ok(ToolOutput::Text(output.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// format_with_line_numbers produces 1-based unpadded output.
    #[test]
    fn test_format_basic_line_numbers() {
        let out = format_with_line_numbers("alpha\nbeta\ngamma", 1);
        assert_eq!(out, "1→alpha\n2→beta\n3→gamma");
    }

    /// The `from` offset shifts the first line number so numbers reflect the
    /// actual position in the source file, not the slice position.
    #[test]
    fn test_format_offset_adjusts_line_numbers() {
        // Simulates memory_get called with from=5 (1-based) — first displayed
        // line should be labelled "5".
        let out = format_with_line_numbers("line five\nline six", 5);
        assert!(out.starts_with("5→line five"), "got: {out}");
        assert!(out.ends_with("6→line six"), "got: {out}");
    }

    /// Empty content produces empty output (no panic).
    #[test]
    fn test_format_empty_content() {
        let out = format_with_line_numbers("", 1);
        assert!(out.is_empty(), "empty input must produce empty output");
    }

    /// Single-line content produces one numbered line.
    #[test]
    fn test_format_single_line() {
        let out = format_with_line_numbers("only line", 1);
        assert_eq!(out, "1→only line");
    }

    /// Wide line numbers (>= 7 digits) are not truncated.
    #[test]
    fn test_format_large_line_numbers() {
        let out = format_with_line_numbers("x", 1_000_000);
        assert!(out.starts_with("1000000→"), "got: {out}");
    }

    /// Content ending with `\n` emits a trailing blank numbered line.
    ///
    /// Regression test for the `lines()` vs `split('\n')` difference.
    /// Virtually all Markdown memory files end with a trailing newline, so
    /// without this fix `memory_get` line numbers are off-by-one relative to
    /// `read_file` for any file that ends with a newline.
    #[test]
    fn test_format_trailing_newline_emits_blank_line() {
        let out = format_with_line_numbers("alpha\n", 1);
        assert_eq!(
            out, "1→alpha\n2→",
            "trailing newline must produce a numbered blank final line"
        );
    }

    /// Two trailing newlines produce two extra blank lines.
    #[test]
    fn test_format_double_trailing_newline() {
        let out = format_with_line_numbers("a\n\n", 1);
        assert_eq!(out, "1→a\n2→\n3→");
    }

    /// Content without a trailing newline does NOT produce a spurious blank line.
    #[test]
    fn test_format_no_trailing_newline_no_blank_line() {
        let out = format_with_line_numbers("alpha", 1);
        assert_eq!(out, "1→alpha", "no trailing newline → no extra line");
    }
}
