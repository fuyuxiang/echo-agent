//! `hashline_read` — anchor-annotated file reading.
//!
//! Reuses the core file-reading logic from [`echo_agent_build::read_file::run_read_file`]
//! and post-processes the result to replace standard line-number formatting with
//! scheme-aware anchor annotations.
//!
//! Output format: `ANCHOR→CONTENT` (e.g. `22:abc:rst→  let x = 1;`).

use crate::implementations::echo_agent_build::read_file::{ReadFileInput, run_read_file};
use crate::types::context::TruncationConfig;

use crate::types::output::ReadFileOutput;
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::{FileSystem, Params, TruncationCfg};
use crate::types::tool::{ToolKind, ToolNamespace};

use super::anchor::split_lines;
use super::config::HashlineSchemeParams;
use super::scheme::AnchorScheme;

/// Format file content lines with anchor annotations.
///
/// Each line is formatted as `LINE:ANCHOR→CONTENT`.
/// `ANCHOR` is the scheme-generated anchor for that line.
///
/// Returns `(hashline_content, raw_output)`.
pub(crate) fn format_hashline_content(
    file_content: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    scheme: &dyn AnchorScheme,
) -> (String, String) {
    use std::fmt::Write as _;

    let all_lines = split_lines(file_content);
    let anchors = scheme.generate_anchors(&all_lines);

    let skip = offset.unwrap_or(1).saturating_sub(1);
    let take = limit.unwrap_or(usize::MAX);

    let mut output = String::new();
    let mut raw_output = String::new();
    let mut first_line: Option<usize> = None;

    for (i, line) in all_lines.iter().enumerate().skip(skip).take(take) {
        let line_num = i + 1; // 1-based

        if first_line.is_none() {
            first_line = Some(line_num);
        } else {
            output.push('\n');
            raw_output.push('\n');
        }

        // Build the anchor suffix: "local" or "local:context" (without line number,
        // since we format the line number separately with right-alignment).
        let anchor_suffix = match &anchors[i].context {
            Some(ctx) => format!("{}:{ctx}", anchors[i].local),
            None => anchors[i].local.clone(),
        };

        // Format: "LINE:LOCAL:CONTEXT→CONTENT" (or "LINE:LOCAL→CONTENT" for A)
        _ = write!(&mut output, "{line_num}:{anchor_suffix}→{line}").ok();
        raw_output.push_str(line);
    }

    (output, raw_output)
}

const DESCRIPTION: &str = r#"Read a file with line-anchored output${%- if tools.by_kind.edit %} for use with ${{ tools.by_kind.edit }}${%- endif %}.

Each line is formatted as ANCHOR→CONTENT, for example:
{example_line1}
{example_line2}

This read format uses `→` between the anchor and content.${%- if tools.by_kind.search %} By contrast,
${{ tools.by_kind.search }} keeps grep-style separators after the anchor: `:` for
match lines and `-` for context lines.${%- endif %}

The ANCHOR (e.g. "{example_anchor}") is a compact fingerprint of the line's content
and surrounding context.${%- if tools.by_kind.edit %} Pass anchors to ${{ tools.by_kind.edit }} to make edits —
they verify the targeted location still matches the snapshot you saw.
Anchors are valid only for the file state at read time — after any edit,
use the fresh anchors returned by ${{ tools.by_kind.edit }} or re-read the file.${%- endif %}

Usage:
- The ${{ params.read.target_file }} parameter accepts either a relative path in the workspace or an absolute path
- By default reads up to {max_lines_read} lines from the beginning
- Optionally specify ${{ params.read.offset }} and ${{ params.read.limit }} for large files
- Can read images (PNG, JPG, etc.) and PDF files (each page rendered as an image; use ${{ params.read.pages }} for PDFs with more than 10 pages, max 20 per call)
- You can call multiple tools in a single response
- If you read a file that exists but has empty contents you will receive a system reminder warning in place of file contents."#;

/// `hashline_read` tool — reads files with anchor-annotated line numbers.
///
/// Delegates to `run_read_file()` for file I/O, path resolution, image
/// handling, and file-read tracking. Post-processes text file results to
/// replace standard line formatting with scheme-aware anchors.
#[derive(Debug, Default)]
pub struct HashlineReadTool;

impl crate::types::tool_metadata::ToolMetadata for HashlineReadTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuildHashline
    }

    fn description_template(&self) -> &str {
        DESCRIPTION
    }

    fn versioned_definition(
        &self,
        _contract_version: Option<&str>,
        client_name: &str,
        description_override: Option<&str>,
        renderer: &crate::types::template_renderer::TemplateRenderer,
        param_map: &std::collections::HashMap<String, String>,
        input_schema: &serde_json::Value,
        effective_params: &serde_json::Value,
    ) -> crate::types::definition::ToolDefinition {
        let params: HashlineSchemeParams =
            serde_json::from_value(effective_params.clone()).unwrap_or_default();
        params.build_tool_definition(
            DESCRIPTION,
            client_name,
            description_override,
            renderer,
            param_map,
            input_schema,
        )
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for HashlineReadTool {
    type Args = ReadFileInput;
    type Output = ReadFileOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("hashline_read").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "hashline_read",
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
        name = "tool.hashline_read",
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

        // Delegate to run_read_file with the ORIGINAL offset/limit so that
        // windowed-read semantics are preserved: raw_output reflects the
        // requested window, token limits apply to the window, file-read
        // tracking records the window, and reminders observe the window.
        let cwd_override = ctx
            .extensions
            .get::<echo_agent_tool_runtime::Cwd>()
            .map(|c| c.0.clone());
        // `None`: the hashline tool does not stream, so it needs no
        // text-path streamability signal (see `run_read_file`).
        let invoking = crate::types::tool_metadata::invoking_param_names(&ctx);
        let result = run_read_file(
            input,
            cwd_override,
            None,
            resources.clone(),
            None,
            &invoking,
        )
        .await?;

        match result {
            ReadFileOutput::FileContent(mut fc) => {
                if fc.document_progress.is_some() {
                    return Ok(ReadFileOutput::FileContent(fc));
                }
                // Empty file: preserve the system-reminder from run_read_file.
                if fc.raw_output.is_empty() && fc.total_lines == 0 {
                    fc.content_concise = None;
                    return Ok(ReadFileOutput::FileContent(fc));
                }

                // Read the full file for anchor generation. Anchors depend
                // on full-file context (chunk fingerprints span multiple
                // lines), so we need complete content even for windowed reads.
                let (full_content, scheme, max_lines) = {
                    let res = resources.lock().await;
                    let params = res
                        .get::<Params<HashlineSchemeParams>>()
                        .cloned()
                        .unwrap_or_default();
                    let s = params
                        .0
                        .build_scheme()
                        .map_err(echo_agent_tool_runtime::ToolError::invalid_arguments)?;
                    let fs = res.require::<FileSystem>()?.0.clone();
                    let content = match fs.read_file(&fc.absolute_path).await {
                        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                        Err(_) => fc.raw_output.clone(),
                    };
                    let ml = res
                        .get::<TruncationCfg>()
                        .map(|t| t.0.max_lines_read())
                        .unwrap_or_else(|| TruncationConfig::default().max_lines_read());
                    (content, s, ml)
                };

                let effective_limit = Some(fc.limit.unwrap_or(usize::MAX).min(max_lines));
                let (hashline_content, _raw) =
                    format_hashline_content(&full_content, fc.offset, effective_limit, &*scheme);

                fc.content = hashline_content;
                fc.content_concise = None; // hashline has only one format
                // Drop tool-layer captures: `hashline_content` keeps the
                // original URIs intact, so session-layer extraction will
                // catch them — clearing here avoids double-injection.
                fc.extracted_images.clear();
                // raw_output, offset, limit, tracking remain as set by
                // run_read_file — windowed semantics preserved.
                Ok(ReadFileOutput::FileContent(fc))
            }
            // Non-text results (images, errors) pass through unchanged.
            other => Ok(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx;

    use crate::computer::local::LocalFs;
    use crate::implementations::echo_agent_build::read_file::{MAX_LINES_READ, ReadFileTool};
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

    // -----------------------------------------------------------------------
    // format_hashline_content unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn format_basic_file() {
        let content = "line one\nline two\nline three\n";
        let scheme = HashlineSchemeParams::default().build_scheme().unwrap();
        let (output, _raw) = format_hashline_content(content, None, None, &*scheme);

        // Each line should have the pattern: ANCHOR→CONTENT
        for line in output.lines() {
            assert!(line.contains(':'), "missing anchor separator: {line}");
            assert!(line.contains('→'), "missing content separator: {line}");
        }
    }

    #[test]
    fn format_includes_anchor_with_context() {
        let content = "fn main() {\n    let x = 1;\n}\n";
        let scheme = HashlineSchemeParams::default().build_scheme().unwrap();
        let (output, _raw) = format_hashline_content(content, None, None, &*scheme);

        // chunk scheme produces LINE:LOCAL:CONTEXT→CONTENT
        // Check that the first content line has two colons (line:local:context)
        let first_content_line = output.lines().next().unwrap();
        let before_arrow = first_content_line.split('→').next().unwrap();
        let colon_count = before_arrow.matches(':').count();
        assert_eq!(
            colon_count, 2,
            "chunk scheme should produce 2 colons (line:local:context), got: {before_arrow}"
        );
    }

    #[test]
    fn format_with_offset_and_limit() {
        let content = "a\nb\nc\nd\ne\n";
        let scheme = HashlineSchemeParams::default().build_scheme().unwrap();
        let (output, _raw) = format_hashline_content(content, Some(2), Some(2), &*scheme);

        // Should contain lines starting with "2:" and "3:"
        let content_lines: Vec<&str> = output.lines().collect();
        assert_eq!(content_lines.len(), 2);
        assert!(content_lines[0].starts_with("2:"));
        assert!(content_lines[1].starts_with("3:"));
    }

    #[test]
    fn format_empty_file() {
        let content = "";
        let scheme = HashlineSchemeParams::default().build_scheme().unwrap();
        let (output, _raw) = format_hashline_content(content, None, None, &*scheme);

        // Should produce a single anchored empty line.
        assert!(output.contains("1:"), "should contain line 1");
        assert!(output.contains('→'), "should contain arrow separator");
    }

    #[test]
    fn format_keeps_long_lines_whole() {
        let long_line = "x".repeat(5000);
        let content = format!("{long_line}\n");
        let scheme = HashlineSchemeParams::default().build_scheme().unwrap();
        let (output, _raw) = format_hashline_content(&content, None, None, &*scheme);

        let first_line = output.lines().next().unwrap();
        let after_arrow = first_line.split('→').nth(1).unwrap();
        assert_eq!(
            after_arrow, long_line,
            "hashline must never clip line content"
        );
    }

    #[test]
    fn format_deterministic() {
        let content = "hello\nworld\n";
        let scheme = HashlineSchemeParams::default().build_scheme().unwrap();
        let (a, _) = format_hashline_content(content, None, None, &*scheme);
        let (b, _) = format_hashline_content(content, None, None, &*scheme);
        assert_eq!(a, b);
    }

    // -----------------------------------------------------------------------
    // HashlineReadTool integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn tool_metadata() {
        use crate::types::tool_metadata::ToolMetadata;
        let tool = HashlineReadTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "hashline_read"
        );
        assert_eq!(ToolMetadata::kind(&tool), ToolKind::Read);
        assert!(echo_agent_tool_runtime::Tool::capabilities(&tool).is_read_only);
        assert!(matches!(
            ToolMetadata::tool_namespace(&tool),
            ToolNamespace::EchoAgentBuildHashline
        ));
    }

    #[test]
    fn description_differs_from_standard() {
        use crate::types::tool_metadata::ToolMetadata;
        let hashline = HashlineReadTool;
        let standard = ReadFileTool;
        assert_ne!(
            ToolMetadata::description_template(&hashline),
            ToolMetadata::description_template(&standard)
        );
        assert!(ToolMetadata::description_template(&hashline).contains("tools.by_kind.edit"));
    }

    #[test]
    fn description_template_tracks_renamed_offset_limit() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;
        use crate::types::tool_metadata::ToolMetadata;
        use std::collections::HashMap;

        let tools = HashMap::from([
            (ToolKind::Read, "hashline_read".to_string()),
            (ToolKind::Edit, "hashline_edit".to_string()),
        ]);
        let params = HashMap::from([(
            ToolKind::Read,
            HashMap::from([
                ("target_file".to_string(), "target_file".to_string()),
                ("offset".to_string(), "start_line".to_string()),
                ("limit".to_string(), "max_lines".to_string()),
            ]),
        )]);
        let rendered = TemplateRenderer::new(tools, params)
            .render(ToolMetadata::description_template(&HashlineReadTool))
            .unwrap();
        assert!(
            rendered.contains("start_line and max_lines for large files"),
            "renamed offset/limit must appear:\n{rendered}"
        );
        assert!(
            !rendered.contains("offset and limit for large files"),
            "canonical offset/limit must not remain after rename:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn read_basic_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("test.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "test.rs".to_string(),
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
            ReadFileOutput::FileContent(fc) => {
                // Hashline format: ANCHOR→CONTENT
                assert!(fc.content.contains('→'));
                assert!(fc.content.contains("fn main()"));

                // Should have chunk-style anchors (two colons before →)
                let first_line = fc.content.lines().next().unwrap();
                let before_arrow = first_line.split('→').next().unwrap();
                assert!(
                    before_arrow.matches(':').count() >= 2,
                    "expected chunk anchors, got: {before_arrow}"
                );

                // concise should be None for hashline
                assert!(fc.content_concise.is_none());
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    /// No per-line clip on the hashline read path either (it formats the
    /// file content independently of echo_agent_build `read_file`).
    #[tokio::test]
    async fn read_long_line_unclipped_by_default() {
        let tmp = TempDir::new().unwrap();
        let long = "x".repeat(5_000);
        std::fs::write(tmp.path().join("long.txt"), format!("{long}\n")).unwrap();

        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "long.txt".to_string(),
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
            ReadFileOutput::FileContent(fc) => {
                assert!(
                    fc.content.contains(&long),
                    "hashline read must not clip long lines by default (got {} chars)",
                    fc.content.len()
                );
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    /// `run_read_file`'s tool-layer base64 capture must be dropped after
    /// hashline reformats `fc.content` (which keeps original URIs verbatim);
    /// otherwise session-layer extraction would also fire and we'd
    /// double-inject the same image as two vision tokens.
    #[tokio::test]
    async fn extracted_images_cleared_after_hashline_overwrite() {
        let tmp = TempDir::new().unwrap();
        let payload = "A".repeat(2000);
        std::fs::write(
            tmp.path().join("with_img.html"),
            format!("<img src=\"data:image/png;base64,{payload}\" />"),
        )
        .unwrap();

        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "with_img.html".to_string(),
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
            ReadFileOutput::FileContent(fc) => assert!(fc.extracted_images.is_empty()),
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn read_nonexistent_file() {
        let tmp = TempDir::new().unwrap();
        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "does_not_exist.rs".to_string(),
            offset: None,
            limit: None,
            pages: None,
            format: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        assert!(
            matches!(result, ReadFileOutput::FileNotFound(_)),
            "expected FileNotFound"
        );
    }

    #[tokio::test]
    async fn read_with_offset_limit() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("multi.txt"), "a\nb\nc\nd\ne\n").unwrap();

        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "multi.txt".to_string(),
            offset: Some(2),
            limit: Some(2),
            pages: None,
            format: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            ReadFileOutput::FileContent(_fc) => {
                // no longer emits "... lines not shown ..."
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    /// Regression test: exact line numbers and content for offset+limit reads.
    ///
    /// Verifies that windowed reads produce the correct original line numbers
    /// and the correct content — not a re-sliced version of an already-sliced
    /// window.
    #[tokio::test]
    async fn read_offset_limit_exact_content() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("five.txt"),
            "alpha\nbeta\ngamma\ndelta\nepsilon\n",
        )
        .unwrap();

        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "five.txt".to_string(),
            offset: Some(2),
            limit: Some(2),
            pages: None,
            format: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            ReadFileOutput::FileContent(fc) => {
                let content_lines: Vec<&str> = fc.content.lines().collect();

                // Exactly 2 content lines should be rendered.
                assert_eq!(
                    content_lines.len(),
                    2,
                    "expected 2 content lines, got {}: {:?}",
                    content_lines.len(),
                    content_lines
                );

                // Line numbers should be 2 and 3 (original file positions).
                assert!(
                    content_lines[0].starts_with("2:"),
                    "first line should start with '2:', got: {}",
                    content_lines[0]
                );
                assert!(
                    content_lines[1].starts_with("3:"),
                    "second line should start with '3:', got: {}",
                    content_lines[1]
                );

                // Content should be the original lines "beta" and "gamma".
                let after_arrow_0 = content_lines[0].split('→').nth(1).unwrap();
                let after_arrow_1 = content_lines[1].split('→').nth(1).unwrap();
                assert_eq!(after_arrow_0, "beta", "line 2 content mismatch");
                assert_eq!(after_arrow_1, "gamma", "line 3 content mismatch");

                // The stored offset/limit should reflect the original request.
                assert_eq!(fc.offset, Some(2));
                assert_eq!(fc.limit, Some(2));
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    /// Regression: raw_output should reflect the windowed read, not the full file.
    #[tokio::test]
    async fn raw_output_reflects_requested_window() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("five.txt"),
            "alpha\nbeta\ngamma\ndelta\nepsilon\n",
        )
        .unwrap();

        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "five.txt".to_string(),
            offset: Some(2),
            limit: Some(2),
            pages: None,
            format: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            ReadFileOutput::FileContent(fc) => {
                // raw_output should contain only the windowed content (beta, gamma),
                // not the full file.
                assert!(
                    fc.raw_output.contains("beta"),
                    "raw_output should contain 'beta'"
                );
                assert!(
                    fc.raw_output.contains("gamma"),
                    "raw_output should contain 'gamma'"
                );
                assert!(
                    !fc.raw_output.contains("alpha"),
                    "raw_output should NOT contain 'alpha' (before window)"
                );
                assert!(
                    !fc.raw_output.contains("delta"),
                    "raw_output should NOT contain 'delta' (after window)"
                );
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    /// Regression: a small window into a large file should not trigger FileTooLarge.
    #[tokio::test]
    async fn small_window_into_large_file_succeeds() {
        let tmp = TempDir::new().unwrap();
        // Create a file with many lines (more than would fit in token budget
        // if read fully, but a small window should be fine).
        let mut content = String::new();
        for i in 0..2000 {
            content.push_str(&format!("// line {i}: some padding content here\n"));
        }
        std::fs::write(tmp.path().join("large.rs"), &content).unwrap();

        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "large.rs".to_string(),
            offset: Some(100),
            limit: Some(5),
            pages: None,
            format: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        // Should succeed with FileContent, not FileTooLarge.
        match result {
            ReadFileOutput::FileContent(fc) => {
                let content_lines: Vec<&str> = fc.content.lines().collect();
                assert_eq!(
                    content_lines.len(),
                    5,
                    "expected 5 content lines for small window"
                );
                assert!(content_lines[0].starts_with("100:"));
            }
            other => panic!("Expected FileContent for small window, got {:?}", other),
        }
    }

    /// Regression: offset past end should still work correctly.
    #[tokio::test]
    async fn offset_past_end_returns_empty_content() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("short.txt"), "one\ntwo\nthree\n").unwrap();

        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "short.txt".to_string(),
            offset: Some(100),
            limit: Some(5),
            pages: None,
            format: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            ReadFileOutput::FileContent(fc) => {
                // With offset=100 on a 4-line file, no content lines should be rendered.
                let content_lines: Vec<&str> = fc.content.lines().collect();
                assert!(
                    content_lines.is_empty(),
                    "offset past end should produce no content lines, got: {:?}",
                    content_lines
                );
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    /// Regression: no-limit reads must be capped at MAX_LINES_READ.
    #[tokio::test]
    async fn large_file_truncated_to_max_lines() {
        let tmp = TempDir::new().unwrap();
        let content: String = (0..2000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(tmp.path().join("big.txt"), &content).unwrap();

        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "big.txt".to_string(),
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
            ReadFileOutput::FileContent(fc) => {
                let content_lines: Vec<&str> = fc.content.lines().collect();
                assert_eq!(content_lines.len(), MAX_LINES_READ);
                assert!(content_lines[0].trim_start().starts_with("1:"));
                let last = content_lines.last().unwrap();
                assert!(
                    last.trim_start()
                        .starts_with(&format!("{}:", MAX_LINES_READ))
                );
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn explicit_small_limit_honored() {
        let tmp = TempDir::new().unwrap();
        let content: String = (0..2000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(tmp.path().join("big.txt"), &content).unwrap();

        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "big.txt".to_string(),
            offset: None,
            limit: Some(50),
            pages: None,
            format: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            ReadFileOutput::FileContent(fc) => {
                let content_lines: Vec<&str> = fc.content.lines().collect();
                assert_eq!(content_lines.len(), 50);
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    /// Explicit limit exceeding MAX_LINES_READ gets capped.
    #[tokio::test]
    async fn explicit_large_limit_capped_to_max_lines() {
        let tmp = TempDir::new().unwrap();
        let content: String = (0..3000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(tmp.path().join("big.txt"), &content).unwrap();

        let tool = HashlineReadTool;
        let resources = test_resources(tmp.path());
        let input = ReadFileInput {
            document: None,
            path: "big.txt".to_string(),
            offset: None,
            limit: Some(2000),
            pages: None,
            format: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            ReadFileOutput::FileContent(fc) => {
                let content_lines: Vec<&str> = fc.content.lines().collect();
                assert_eq!(content_lines.len(), MAX_LINES_READ);
                assert_eq!(fc.limit, Some(2000));
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }
}
