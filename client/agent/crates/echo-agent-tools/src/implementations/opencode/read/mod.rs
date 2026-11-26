//! OpenCode `read` tool — reads files, directories, images, and PDFs.
//!
//! Follows the opencode parameter naming conventions (`filePath`, `offset`,
//! `limit`) and wraps output in XML tags (`<path>`, `<type>`, `<content>`).
//!
//! Reuses `ReadFileOutput` from `crate::types::output` as the output type
//! so the existing `ToolOutput::ReadFile` variant handles prompt rendering.

use std::fmt::Write as _;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose;

use crate::types::output::{FileContent, ImageContent, ReadFileOutput};
use crate::types::requirements::Expr;
#[allow(unused_imports)]
use crate::types::resources::{Cwd, DisplayCwd, FileSystem, SharedResources, resolve_model_path};
use crate::types::tool::ToolKind;
#[allow(unused_imports)]
use crate::types::tool::ToolNamespace;
use crate::types::tool_io::ToolInput;

// ─── Constants ──────────────────────────────────────────────────────

/// Default maximum number of lines returned per read.
const DEFAULT_READ_LIMIT: u32 = 2000;

/// Maximum character length per line before truncation.
const MAX_LINE_LENGTH: usize = 2000;

/// Maximum bytes of text content returned per read (50 KB).
const MAX_BYTES: usize = 50 * 1024;

// ─── Description ────────────────────────────────────────────────────

const DESCRIPTION: &str = r#"Reads a file or directory from the local filesystem. You can access any file directly by using this tool.
Assume this tool is able to read all files on the machine. If the User provides a path to a file assume that path is valid. It is okay to read a file that does not exist; an error will be returned.

Usage:
- The ${{ params.read.filePath }} parameter must be an absolute path, not a relative path
- By default, it reads up to 2000 lines starting from the beginning of the file
- You can optionally specify ${{ params.read.offset }} and ${{ params.read.limit }} (especially handy for long files), but it's recommended to read the whole file by not providing these parameters
- Any lines longer than {max_chars_per_line} characters will be truncated
- Contents are returned with each line prefixed by its line number as `LINE_NUMBER: LINE_CONTENT`, with line numbers starting at 1. For example, if a file has contents "foo\n", you will receive "1: foo"
- For directories, entries are returned one per line (without line numbers) with a trailing `/` for subdirectories
- This tool can read images (eg PNG, JPG, etc). When reading an image file the contents are presented visually as this tool uses multimodal LLMs.
- This tool can read PDF files (.pdf). PDFs are presented visually to the multimodal LLM as attachments.
- You can call multiple tools in a single response. It is always better to speculatively read multiple potentially useful files in parallel.
- You will regularly be asked to read screenshots. If the user provides a path to a screenshot, ALWAYS use this tool to view the file at the path. This tool will work with all temporary file paths.
- If you read a file that exists but has empty contents you will receive a system reminder warning in place of file contents."#;

// ─── Input ──────────────────────────────────────────────────────────

/// Input for the opencode `read` tool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadInput {
    /// Absolute path to a file or directory.
    pub file_path: String,

    /// 1-indexed line number to start reading from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,

    /// Maximum number of lines to return (default 2000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

// Manual conversions for the ToolInput enum (ReadInput is not a variant).
// Route through `Dynamic(serde_json::Value)` so the eraser layer can work.

impl TryFrom<ToolInput> for ReadInput {
    type Error = ToolInput;

    fn try_from(value: ToolInput) -> Result<Self, Self::Error> {
        match value {
            ToolInput::Dynamic(v) => {
                serde_json::from_value(v.clone()).map_err(|_| ToolInput::Dynamic(v))
            }
            other => Err(other),
        }
    }
}

impl From<ReadInput> for ToolInput {
    fn from(value: ReadInput) -> Self {
        ToolInput::Dynamic(serde_json::to_value(value).expect("ReadInput serializes to JSON"))
    }
}

// ─── Tool ───────────────────────────────────────────────────────────

/// OpenCode `read` tool — reads files, directories, images, and PDFs.
#[derive(Debug, Default)]
pub struct ReadTool;

impl crate::types::tool_metadata::ToolMetadata for ReadTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::OpenCode
    }

    fn description_template(&self) -> &str {
        DESCRIPTION
    }

    fn requires_expr(&self) -> Expr<crate::types::requirements::ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for ReadTool {
    type Args = ReadInput;
    type Output = ReadFileOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("read").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "read",
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

    #[tracing::instrument(name = "tool.opencode.read", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: ReadInput,
    ) -> Result<ReadFileOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::{invoking_param_names, resolve_cwd, shared_resources};
        let resources = shared_resources(&ctx)?;
        // Client-facing `offset` name for runtime "read beyond…" hints; a
        // rename must not tell the model to pass a key its schema lacks.
        let invoking = invoking_param_names(&ctx);
        let offset_param = invoking.resolve("offset");

        // ── Validate offset ─────────────────────────────────────────
        if let Some(offset) = input.offset
            && offset < 1
        {
            return Ok(ReadFileOutput::FileReadError(format!(
                "{offset_param} must be >= 1"
            )));
        }

        // ── Resolve path (single lock acquisition) ─────────────────
        let cwd = resolve_cwd(&ctx, &resources).await?;
        let (display_cwd, fs) = {
            let res = resources.lock().await;
            let display_cwd = res.get::<DisplayCwd>().map(|d| d.0.clone());
            let fs = res.require::<FileSystem>()?.0.clone();
            (display_cwd, fs)
        };
        let resolved = resolve_model_path(&cwd, display_cwd.as_deref(), &input.file_path);
        let path = crate::util::fs::canonicalize_with_timeout(resolved).await;

        // ── Stat the path ───────────────────────────────────────────
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(not_found_with_suggestions(&path).await);
            }
            Err(e) => {
                return Ok(ReadFileOutput::FileReadError(format!(
                    "Failed to read file: {}, {e}",
                    path.display()
                )));
            }
        };

        // ═══════════════════════════════════════════════════════════
        // BRANCH A: DIRECTORY
        // ═══════════════════════════════════════════════════════════
        if metadata.is_dir() {
            return Ok(read_directory(&path, input.offset, input.limit, offset_param).await);
        }

        // ═══════════════════════════════════════════════════════════
        // BRANCH B: IMAGE / PDF
        // ═══════════════════════════════════════════════════════════
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        // Read the file bytes for all remaining branches.
        let file_bytes = match fs.read_file(&path).await {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::debug!(?e, "Failed to read file");
                return Ok(ReadFileOutput::FileReadError(format!(
                    "Failed to read file: {}, {e}",
                    path.display()
                )));
            }
        };

        // Check for images via magic-byte detection. Route through
        // compression — raw bytes (truncated or non-endpoint formats)
        // must never reach the conversation.
        if let Ok(meta) =
            crate::implementations::echo_agent_build::read_file::bytes_to_metadata(&file_bytes)
            && meta.is_image()
        {
            return Ok(crate::implementations::read_file::image::image_read_output(
                file_bytes,
                meta.mime_type,
            )
            .await);
        }

        // Check for PDF by extension (magic-byte detection doesn't always catch PDFs).
        if extension == "pdf" {
            let image_b64 = general_purpose::STANDARD.encode(&file_bytes);
            return Ok(ReadFileOutput::ImageContent(ImageContent {
                data: image_b64,
                mime_type: "application/pdf".to_string(),
                annotations: None,
                uri: None,
                meta: None,
            }));
        }

        // ═══════════════════════════════════════════════════════════
        // BRANCH C: BINARY CHECK
        // ═══════════════════════════════════════════════════════════
        if crate::util::binary::is_binary(&extension, &file_bytes) {
            return Ok(ReadFileOutput::FileReadError(format!(
                "Cannot read binary file: {}",
                path.display()
            )));
        }

        // ═══════════════════════════════════════════════════════════
        // BRANCH D: TEXT FILE
        // ═══════════════════════════════════════════════════════════
        let file_content = String::from_utf8_lossy(&file_bytes).into_owned();
        let total_lines = if file_content.is_empty() {
            0
        } else {
            file_content.lines().count()
        };

        let limit = input.limit.unwrap_or(DEFAULT_READ_LIMIT) as usize;
        let offset = input.offset.unwrap_or(1) as usize;
        let start = offset.saturating_sub(1); // 0-indexed

        // Validate offset against file size.
        if total_lines > 0 && start >= total_lines {
            return Ok(ReadFileOutput::FileReadError(format!(
                "{offset_param} {offset} is out of range for this file ({total_lines} lines)"
            )));
        }

        // Collect lines with byte-cap and line-limit.
        let mut raw_lines: Vec<String> = Vec::new();
        let mut byte_count: usize = 0;
        let mut truncated_by_bytes = false;
        let mut has_more_lines = false;

        for (i, line_text) in file_content.lines().enumerate() {
            if i < start {
                continue;
            }

            if raw_lines.len() >= limit {
                has_more_lines = true;
                continue; // keep counting total lines
            }

            // Truncate long lines.
            let line = if line_text.len() > MAX_LINE_LENGTH {
                format!(
                    "{}... (line truncated to {} chars)",
                    &line_text[..MAX_LINE_LENGTH],
                    MAX_LINE_LENGTH
                )
            } else {
                line_text.to_string()
            };

            let line_byte_len = line.len() + if raw_lines.is_empty() { 0 } else { 1 }; // +1 for newline separator
            if byte_count + line_byte_len > MAX_BYTES {
                truncated_by_bytes = true;
                has_more_lines = true;
                break;
            }

            raw_lines.push(line);
            byte_count += line_byte_len;
        }

        // Format output with line numbers: "{lineNum}: {content}"
        let mut content_lines = String::new();
        for (i, line) in raw_lines.iter().enumerate() {
            let line_num = offset + i;
            if !content_lines.is_empty() {
                content_lines.push('\n');
            }
            let _ = write!(&mut content_lines, "{}: {}", line_num, line);
        }

        // Build the XML-wrapped output.
        let last_read_line = if raw_lines.is_empty() {
            offset
        } else {
            offset + raw_lines.len() - 1
        };
        let next_offset = last_read_line + 1;
        let _truncated = has_more_lines || truncated_by_bytes;

        let footer = if truncated_by_bytes {
            format!(
                "\n\n(Output capped at 50 KB. Showing lines {offset}-{last_read_line}. Use {offset_param}={next_offset} to continue.)"
            )
        } else if has_more_lines {
            format!(
                "\n\n(Showing lines {offset}-{last_read_line} of {total_lines}. Use {offset_param}={next_offset} to continue.)"
            )
        } else {
            format!("\n\n(End of file - total {total_lines} lines)")
        };

        let formatted = format!(
            "<path>{}</path>\n<type>file</type>\n<content>{}{}\n</content>",
            path.display(),
            content_lines,
            footer,
        );

        let raw_output = raw_lines.join("\n");

        Ok(ReadFileOutput::FileContent(FileContent {
            document_progress: None,
            content: formatted,
            content_concise: None,
            absolute_path: path,
            offset: input.offset.map(|o| o as usize),
            limit: input.limit.map(|l| l as usize),
            raw_output,
            total_lines,
            extracted_images: Vec::new(),
        }))
    }
}

// ─── Helpers ────────────────────────────────────────────────────────

/// Read a directory and return a formatted listing wrapped in XML tags.
async fn read_directory(
    path: &std::path::Path,
    offset: Option<u32>,
    limit: Option<u32>,
    // Client-facing `offset` param name for the "read beyond…" hint.
    offset_param: &str,
) -> ReadFileOutput {
    let mut entries = Vec::new();

    let mut read_dir = match tokio::fs::read_dir(path).await {
        Ok(rd) => rd,
        Err(e) => {
            return ReadFileOutput::FileReadError(format!(
                "Failed to read directory: {}, {e}",
                path.display()
            ));
        }
    };

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = match entry.file_type().await {
            Ok(ft) => {
                if ft.is_symlink() {
                    // Resolve symlink to check if target is a directory.
                    tokio::fs::metadata(entry.path())
                        .await
                        .map(|m| m.is_dir())
                        .unwrap_or(false)
                } else {
                    ft.is_dir()
                }
            }
            Err(_) => false,
        };

        if is_dir {
            entries.push(format!("{}/", name));
        } else {
            entries.push(name);
        }
    }

    entries.sort_by_key(|a| a.to_lowercase());

    let total = entries.len();
    let limit = limit.unwrap_or(DEFAULT_READ_LIMIT) as usize;
    let offset_val = offset.unwrap_or(1) as usize;
    let start = offset_val.saturating_sub(1);
    let sliced: Vec<&str> = entries
        .iter()
        .skip(start)
        .take(limit)
        .map(|s| s.as_str())
        .collect();
    let shown = sliced.len();
    let truncated = (start + shown) < total;

    let entries_footer = if truncated {
        let beyond = offset_val + shown;
        format!(
            "\n(Showing {shown} of {total} entries. Use the {offset_param} parameter to read beyond entry {beyond})"
        )
    } else {
        format!("\n({} entries)", total)
    };

    let formatted = format!(
        "<path>{}</path>\n<type>directory</type>\n<entries>\n{}{}\n</entries>",
        path.display(),
        sliced.join("\n"),
        entries_footer,
    );

    ReadFileOutput::FileContent(FileContent {
        document_progress: None,
        content: formatted.clone(),
        content_concise: None,
        absolute_path: path.to_path_buf(),
        offset: None,
        limit: None,
        raw_output: sliced.join("\n"),
        total_lines: total,
        extracted_images: Vec::new(),
    })
}

/// Generate a file-not-found error, suggesting up to 3 similar files.
async fn not_found_with_suggestions(path: &std::path::Path) -> ReadFileOutput {
    let dir = path.parent().unwrap_or(path);
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    let mut suggestions: Vec<PathBuf> = Vec::new();

    if let Ok(mut read_dir) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name.contains(&base) || base.contains(&name) {
                suggestions.push(entry.path());
                if suggestions.len() >= 3 {
                    break;
                }
            }
        }
    }

    if suggestions.is_empty() {
        ReadFileOutput::FileReadError(format!("File not found: {}", path.display()))
    } else {
        let suggestion_list: Vec<String> = suggestions
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        ReadFileOutput::FileReadError(format!(
            "File not found: {}\n\nDid you mean one of these?\n{}",
            path.display(),
            suggestion_list.join("\n"),
        ))
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx;
    use crate::util::binary::is_binary;

    use crate::computer::local::LocalFs;
    use crate::notification::types::ToolNotificationHandle;
    #[allow(unused_imports)]
    use crate::types::resources::{NotificationHandle, Resources};
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
    fn description_template_tracks_renamed_offset_limit_and_execute() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;
        use crate::types::tool_metadata::ToolMetadata;
        use std::collections::HashMap;

        let tools = HashMap::from([
            (ToolKind::Read, "read".to_string()),
            (ToolKind::Execute, "run_command".to_string()),
        ]);
        let params = HashMap::from([(
            ToolKind::Read,
            HashMap::from([
                ("filePath".to_string(), "filePath".to_string()),
                ("offset".to_string(), "start_line".to_string()),
                ("limit".to_string(), "max_lines".to_string()),
            ]),
        )]);
        let rendered = TemplateRenderer::new(tools, params)
            .render(ToolMetadata::description_template(&ReadTool))
            .unwrap();
        assert!(
            rendered.contains("start_line and max_lines"),
            "renamed offset/limit must appear:\n{rendered}"
        );
        assert!(
            rendered.contains("file or directory")
                && rendered.contains("trailing `/` for subdirectories"),
            "directory support must be documented:\n{rendered}"
        );
        assert!(
            !rendered.contains("only read files")
                && !rendered.contains("ls command")
                && !rendered.contains("a line offset and limit")
                && !rendered.contains("Bash tool"),
            "stale files-only/ls/offset/Bash-tool literals must not remain:\n{rendered}"
        );
    }

    /// Runtime "read beyond…" footer must name this tool's client-facing
    /// offset param, not the canonical `offset`, after a rename.
    #[tokio::test]
    async fn runtime_footer_tracks_renamed_offset() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("big.txt");
        let content = (1..=100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&file_path, &content).unwrap();

        let resources = test_resources(&canonical_tmp);
        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(crate::types::resources::InvokingToolParamNames(
                [("offset".to_string(), "start_line".to_string())].into(),
            ));

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: None,
            limit: Some(5),
        };
        let result = echo_agent_tool_runtime::Tool::run(&ReadTool, ctx, input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(
                    fc.content.contains("Use start_line=6 to continue")
                        && !fc.content.contains("Use offset="),
                    "footer must use renamed offset param: {}",
                    fc.content
                );
            }
            other => panic!("Expected FileContent, got {other:?}"),
        }
    }

    /// The invalid-offset validation error must name this tool's client-facing
    /// offset param, not the canonical `offset`, after a rename. (Fires before
    /// path resolution, so no file is needed.)
    #[tokio::test]
    async fn validation_error_tracks_renamed_offset() {
        let tmp = TempDir::new().unwrap();
        let resources = test_resources(tmp.path());
        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(crate::types::resources::InvokingToolParamNames(
                [("offset".to_string(), "start_line".to_string())].into(),
            ));

        let input = ReadInput {
            file_path: "whatever.txt".to_string(),
            offset: Some(0),
            limit: None,
        };
        let result = echo_agent_tool_runtime::Tool::run(&ReadTool, ctx, input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(
                    msg.contains("start_line must be >= 1") && !msg.contains("offset must"),
                    "validation error must use renamed offset param: {msg}"
                );
            }
            other => panic!("Expected FileReadError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_text_file_basic() {
        let tmp = TempDir::new().unwrap();
        // Canonicalize the tmp path to match what the tool will store in the tracker
        // (on macOS /tmp is a symlink to /private/tmp).
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("test.txt");
        std::fs::write(&file_path, "line1\nline2\nline3\n").unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };

        let shared = resources.into_shared();
        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(fc.content.contains("<type>file</type>"));
                assert!(fc.content.contains("1: line1"));
                assert!(fc.content.contains("2: line2"));
                assert!(fc.content.contains("3: line3"));
                assert!(fc.content.contains("(End of file"));
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn read_file_not_found() {
        let tmp = TempDir::new().unwrap();
        let tool = ReadTool;
        let resources = test_resources(tmp.path());

        let input = ReadInput {
            file_path: tmp
                .path()
                .join("nonexistent.txt")
                .to_string_lossy()
                .to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(msg.contains("File not found"));
            }
            other => panic!("Expected FileReadError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn read_file_with_offset_and_limit() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("multi.txt");
        std::fs::write(&file_path, "1\n2\n3\n4\n5\n").unwrap();

        let tool = ReadTool;
        let resources = test_resources(tmp.path());

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: Some(2),
            limit: Some(2),
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(fc.content.contains("2: 2"));
                assert!(fc.content.contains("3: 3"));
                assert!(fc.content.contains("Showing lines 2-3 of 5"));
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn read_directory_listing() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "").unwrap();
        std::fs::create_dir(tmp.path().join("subdir")).unwrap();

        let tool = ReadTool;
        let resources = test_resources(tmp.path());

        let input = ReadInput {
            file_path: tmp.path().to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(fc.content.contains("<type>directory</type>"));
                assert!(fc.content.contains("a.txt"));
                assert!(fc.content.contains("b.txt"));
                assert!(fc.content.contains("subdir/"));
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn read_binary_file_rejected() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("binary.bin");
        std::fs::write(&file_path, [0x00, 0x01, 0x02, 0xFF]).unwrap();

        let tool = ReadTool;
        let resources = test_resources(tmp.path());

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(msg.contains("Cannot read binary file"));
            }
            other => panic!("Expected FileReadError for binary, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn read_invalid_offset() {
        let tmp = TempDir::new().unwrap();
        let tool = ReadTool;
        let resources = test_resources(tmp.path());

        let input = ReadInput {
            file_path: tmp.path().join("any.txt").to_string_lossy().to_string(),
            offset: Some(0),
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(msg.contains("offset must be >= 1"));
            }
            other => panic!("Expected FileReadError for bad offset, got {:?}", other),
        }
    }

    #[test]
    fn is_binary_detects_null_bytes() {
        assert!(is_binary("", &[0x48, 0x65, 0x00, 0x6C]));
    }

    #[test]
    fn is_binary_detects_known_extensions() {
        assert!(is_binary("zip", &[]));
        assert!(is_binary("exe", &[]));
        assert!(is_binary("wasm", &[]));
    }

    #[test]
    fn is_binary_allows_text() {
        assert!(!is_binary("txt", b"Hello, world!\n"));
        assert!(!is_binary("rs", b"fn main() {}\n"));
    }

    #[test]
    fn is_binary_empty_is_not_binary() {
        assert!(!is_binary("", &[]));
    }

    #[test]
    fn serde_roundtrip_camel_case() {
        let json = r#"{"filePath":"/tmp/test.txt","offset":5,"limit":100}"#;
        let input: ReadInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.file_path, "/tmp/test.txt");
        assert_eq!(input.offset, Some(5));
        assert_eq!(input.limit, Some(100));

        // Serializes back to camelCase
        let serialized = serde_json::to_string(&input).unwrap();
        assert!(serialized.contains("filePath"));
        assert!(!serialized.contains("file_path"));
    }

    #[tokio::test]
    async fn image_file_detection() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("photo.png");

        // Real PNG: small enough to pass through the compression gate
        // byte-identical.
        let img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
            image::ImageBuffer::from_pixel(32, 32, image::Rgba([1, 2, 3, 255]));
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        std::fs::write(&file_path, &png_bytes).unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::ImageContent(img) => {
                assert_eq!(img.mime_type, "image/png");
                // The base64 data must be non-empty and decode back to our bytes.
                let decoded = general_purpose::STANDARD.decode(&img.data).unwrap();
                assert_eq!(decoded, png_bytes);
            }
            other => panic!("Expected ImageContent, got {:?}", other),
        }
    }

    /// A truncated JPEG on disk must not be embedded raw; the compression
    /// path re-encodes the decodable portion into complete bytes.
    #[tokio::test]
    async fn truncated_image_not_embedded_raw() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("cut.jpg");

        let img: image::ImageBuffer<image::Rgb<u8>, Vec<u8>> =
            image::ImageBuffer::from_fn(200, 150, |x, y| {
                image::Rgb([(x ^ y) as u8, (x * 3) as u8, (y * 5) as u8])
            });
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
            .encode_image(&image::DynamicImage::ImageRgb8(img))
            .unwrap();
        jpeg.truncate(jpeg.len() / 2);
        std::fs::write(&file_path, &jpeg).unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);
        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };
        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::ImageContent(img) => {
                let decoded = general_purpose::STANDARD.decode(&img.data).unwrap();
                assert_ne!(decoded, jpeg, "raw truncated bytes must not embed");
                assert!(
                    crate::util::image_validate::image_structurally_complete(&decoded),
                    "embedded bytes must be structurally complete"
                );
            }
            other => panic!("Expected re-encoded ImageContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn long_line_truncation() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("long.txt");

        // Create a line well beyond MAX_LINE_LENGTH (2000 chars).
        let long_line = "X".repeat(3000);
        std::fs::write(&file_path, &long_line).unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(
                    fc.content.contains("(line truncated to 2000 chars)"),
                    "Expected truncation message, got: {}",
                    fc.content,
                );
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn byte_cap_truncation() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("big.txt");

        // Each line is ~100 bytes; we need >50 KB = 51200 bytes total.
        let line = "A".repeat(99); // 99 chars + newline = 100 bytes per line
        let num_lines = 600; // 600 * 100 = 60_000 bytes, well over 50KB
        let content: String = (0..num_lines)
            .map(|_| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&file_path, &content).unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(
                    fc.content.contains("Output capped at 50 KB"),
                    "Expected byte-cap footer, got: {}",
                    fc.content,
                );
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn offset_past_end() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("short.txt");
        std::fs::write(&file_path, "one\ntwo\nthree\n").unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: Some(10),
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(
                    msg.contains("out of range"),
                    "Expected 'out of range', got: {}",
                    msg,
                );
            }
            other => panic!("Expected FileReadError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn empty_file() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("empty.txt");
        std::fs::write(&file_path, "").unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert_eq!(fc.total_lines, 0);
                assert!(
                    fc.content.contains("total 0 lines"),
                    "Expected 'total 0 lines', got: {}",
                    fc.content,
                );
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn line_limit_reached() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("hundred.txt");

        let content: String = (1..=100)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&file_path, &content).unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: None,
            limit: Some(5),
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(
                    fc.content.contains("Showing lines 1-5 of 100"),
                    "Expected 'Showing lines 1-5 of 100', got: {}",
                    fc.content,
                );
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn relative_path_resolved() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("hello.txt");
        std::fs::write(&file_path, "hello world\n").unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        // Pass a relative path — should be resolved against Cwd.
        let input = ReadInput {
            file_path: "hello.txt".to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(fc.content.contains("hello world"));
                assert!(
                    fc.absolute_path.is_absolute(),
                    "Expected absolute path, got: {:?}",
                    fc.absolute_path,
                );
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn pdf_file_detection() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("test.pdf");

        // Arbitrary bytes — not a real PDF, but the extension triggers PDF handling.
        std::fs::write(&file_path, [0xDE, 0xAD, 0xBE, 0xEF, 0x42, 0x42]).unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::ImageContent(img) => {
                assert!(
                    img.mime_type.contains("pdf"),
                    "Expected mime_type containing 'pdf', got: {}",
                    img.mime_type,
                );
                // Verify the base64 data decodes back to our bytes.
                let decoded = general_purpose::STANDARD.decode(&img.data).unwrap();
                assert_eq!(decoded, &[0xDE, 0xAD, 0xBE, 0xEF, 0x42, 0x42]);
            }
            other => panic!("Expected ImageContent for PDF, got {:?}", other),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn directory_with_symlink() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();

        // Create a real directory and a symlink pointing to it.
        std::fs::create_dir(canonical_tmp.join("real_dir")).unwrap();
        std::os::unix::fs::symlink(
            canonical_tmp.join("real_dir"),
            canonical_tmp.join("link_dir"),
        )
        .unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        let input = ReadInput {
            file_path: canonical_tmp.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(
                    fc.content.contains("link_dir/"),
                    "Expected symlinked dir to appear with '/' suffix, got: {}",
                    fc.content,
                );
                assert!(
                    fc.content.contains("real_dir/"),
                    "Expected real_dir/ in listing, got: {}",
                    fc.content,
                );
            }
            other => panic!("Expected FileContent for directory, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn directory_pagination() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();

        // Create 20 files named a01.txt .. a20.txt.
        for i in 1..=20 {
            std::fs::write(canonical_tmp.join(format!("a{:02}.txt", i)), "").unwrap();
        }

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        let input = ReadInput {
            file_path: canonical_tmp.to_string_lossy().to_string(),
            offset: Some(5),
            limit: Some(3),
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(
                    fc.content.contains("Showing 3 of 20"),
                    "Expected 'Showing 3 of 20' in output, got: {}",
                    fc.content,
                );
                // Entries are sorted: a01..a20. Offset=5 (1-indexed), limit=3 → entries 5,6,7.
                // That's a05.txt, a06.txt, a07.txt.
                assert!(fc.content.contains("a05.txt"), "Expected a05.txt");
                assert!(fc.content.contains("a06.txt"), "Expected a06.txt");
                assert!(fc.content.contains("a07.txt"), "Expected a07.txt");
                // Should NOT contain entries outside the window.
                assert!(
                    !fc.content.contains("a04.txt"),
                    "a04.txt should not be shown"
                );
                assert!(
                    !fc.content.contains("a08.txt"),
                    "a08.txt should not be shown"
                );
            }
            other => panic!("Expected FileContent for directory, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn file_not_found_with_suggestions() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();

        // Create a file that should be suggested.
        std::fs::write(canonical_tmp.join("test.txt"), "content").unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        // Request "test" (without extension) — the suggestion logic checks
        // name.contains(base) || base.contains(name), so "test.txt".contains("test") == true.
        let input = ReadInput {
            file_path: canonical_tmp.join("test").to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(
                    msg.contains("Did you mean"),
                    "Expected 'Did you mean' in error, got: {}",
                    msg,
                );
                assert!(
                    msg.contains("test.txt"),
                    "Expected 'test.txt' as suggestion, got: {}",
                    msg,
                );
            }
            other => panic!("Expected FileReadError with suggestions, got {:?}", other),
        }
    }

    #[test]
    fn is_binary_30_percent_threshold() {
        // Exactly 30 non-printable out of 100 bytes → ratio = 0.30, NOT > 0.3 → not binary.
        let mut at_threshold: Vec<u8> = vec![0x01; 30]; // non-printable (< 9)
        at_threshold.extend(vec![b'A'; 70]); // printable
        assert_eq!(at_threshold.len(), 100);
        assert!(
            !is_binary("", &at_threshold),
            "30/100 = 0.30 should NOT be binary (threshold is >0.3)",
        );

        // 31 non-printable out of 100 bytes → ratio = 0.31, > 0.3 → binary.
        let mut above_threshold: Vec<u8> = vec![0x01; 31];
        above_threshold.extend(vec![b'A'; 69]);
        assert_eq!(above_threshold.len(), 100);
        assert!(
            is_binary("", &above_threshold),
            "31/100 = 0.31 should be binary (threshold is >0.3)",
        );
    }

    #[tokio::test]
    async fn xml_output_structure() {
        let tmp = TempDir::new().unwrap();
        let canonical_tmp = dunce::canonicalize(tmp.path()).unwrap();
        let file_path = canonical_tmp.join("structure.txt");
        std::fs::write(&file_path, "first line\nsecond line\n").unwrap();

        let tool = ReadTool;
        let resources = test_resources(&canonical_tmp);

        let input = ReadInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                // Verify XML structure.
                assert!(
                    fc.content.starts_with("<path>"),
                    "Output should start with '<path>', got: {}",
                    &fc.content[..fc.content.len().min(50)],
                );
                assert!(
                    fc.content.contains("<type>file</type>"),
                    "Output should contain '<type>file</type>'",
                );
                assert!(
                    fc.content.contains("<content>"),
                    "Output should contain '<content>'",
                );
                assert!(
                    fc.content.ends_with("</content>"),
                    "Output should end with '</content>', got tail: {}",
                    &fc.content[fc.content.len().saturating_sub(30)..],
                );
                // Verify line number format: "N: content".
                assert!(
                    fc.content.contains("1: first line"),
                    "Expected '1: first line' in output",
                );
                assert!(
                    fc.content.contains("2: second line"),
                    "Expected '2: second line' in output",
                );
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }
}
