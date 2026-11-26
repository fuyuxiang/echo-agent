//! `CodexReadFileTool` — Tool trait implementation for the codex read_file format.
//!
//! Reads files via `AsyncFileSystem` and produces output in the codex
//! `L{n}: {content}` format. Supports both slice mode and indentation mode.

use std::path::PathBuf;

use crate::types::output::{FileContent, ReadFileOutput};
use crate::types::requirements::Expr;
#[allow(unused_imports)]
use crate::types::resources::{FileSystem, SharedResources};
use crate::types::tool::{ToolKind, ToolNamespace};

use super::{indentation, slice};

// ─── Description ─────────────────────────────────────────────────────

/// Tool description — word-for-word copy from codex `create_read_file_tool()` in
/// `codex-rs/core/src/tools/spec.rs` (line 1233).
const DESCRIPTION: &str = "Reads a local file with 1-indexed line numbers, supporting slice and indentation-aware block modes.";

// ─── Input ───────────────────────────────────────────────────────────

/// Input for the codex `read_file` tool.
///
/// Field descriptions match codex `create_read_file_tool()` parameter descriptions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CodexReadFileInput {
    /// Absolute path to the file
    pub file_path: String,

    /// The line number to start reading from. Must be 1 or greater.
    #[serde(default = "defaults::offset")]
    #[schemars(range(min = 1))]
    pub offset: usize,

    /// The maximum number of lines to return.
    #[serde(default = "defaults::limit")]
    pub limit: usize,

    /// Optional mode selector: "slice" for simple ranges (default) or "indentation"
    /// to expand around an anchor line.
    #[serde(default)]
    pub mode: ReadMode,

    /// Indentation-mode configuration. Only used when mode is "indentation".
    #[serde(default)]
    pub indentation: Option<IndentationArgs>,
}

/// Read mode selector.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    #[default]
    Slice,
    Indentation,
}

/// Arguments for indentation-mode reading.
///
/// Field descriptions match codex `create_read_file_tool()` indentation_properties.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct IndentationArgs {
    /// Anchor line to center the indentation lookup on (defaults to offset).
    #[serde(default)]
    pub anchor_line: Option<usize>,

    /// How many parent indentation levels (smaller indents) to include.
    #[serde(default = "defaults::max_levels")]
    pub max_levels: usize,

    /// When true, include additional blocks that share the anchor indentation.
    #[serde(
        default = "defaults::include_siblings",
        deserialize_with = "crate::types::schema::deserialize_lenient_bool"
    )]
    pub include_siblings: bool,

    /// Include doc comments or attributes directly above the selected block.
    #[serde(
        default = "defaults::include_header",
        deserialize_with = "crate::types::schema::deserialize_lenient_bool"
    )]
    pub include_header: bool,

    /// Hard cap on the number of lines returned when using indentation mode.
    #[serde(default)]
    pub max_lines: Option<usize>,
}

mod defaults {
    pub fn offset() -> usize {
        1
    }
    pub fn limit() -> usize {
        2000
    }
    pub fn max_levels() -> usize {
        0
    }
    pub fn include_siblings() -> bool {
        false
    }
    pub fn include_header() -> bool {
        true
    }
}

impl Default for IndentationArgs {
    fn default() -> Self {
        Self {
            anchor_line: None,
            max_levels: defaults::max_levels(),
            include_siblings: defaults::include_siblings(),
            include_header: defaults::include_header(),
            max_lines: None,
        }
    }
}

// ─── Tool ────────────────────────────────────────────────────────────

/// Codex read_file tool — reads files in the codex `L{n}: {content}` format.
#[derive(Debug, Default)]
pub struct CodexReadFileTool;

// ─── Tests ───────────────────────────────────────────────────────────

impl crate::types::tool_metadata::ToolMetadata for CodexReadFileTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::Codex
    }

    fn description_template(&self) -> &str {
        DESCRIPTION
    }

    fn requires_expr(&self) -> Expr<crate::types::requirements::ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for CodexReadFileTool {
    type Args = CodexReadFileInput;
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

    #[tracing::instrument(name = "tool.codex_read_file", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: CodexReadFileInput,
    ) -> Result<ReadFileOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        // 1. Validate. Codex raises here, but we surface these as a structured
        // `FileReadError` (a model-facing error) instead of a hard `Err`, so
        // otherwise-benign validation failures (empty/short files, relative
        // paths) do not surface as tool-execution failures.
        // `FileReadError` rides the structured-output path and maps cleanly to
        // `ReadFileErrorTypes::FILE_READ_ERROR`.
        if input.offset == 0 {
            return Ok(ReadFileOutput::FileReadError(
                "offset must be a 1-indexed line number".to_string(),
            ));
        }
        if input.limit == 0 {
            return Ok(ReadFileOutput::FileReadError(
                "limit must be greater than zero".to_string(),
            ));
        }
        let path = PathBuf::from(&input.file_path);
        if !path.is_absolute() {
            return Ok(ReadFileOutput::FileReadError(
                "file_path must be an absolute path".to_string(),
            ));
        }

        // 2. Read file via AsyncFileSystem.
        let fs;
        {
            fs = resources.lock().await.require::<FileSystem>()?.0.clone();
        }
        let file_bytes = match fs.read_file(&path).await {
            Ok(bytes) => bytes,
            Err(e) => {
                return Ok(ReadFileOutput::FileReadError(format!(
                    "Failed to read file: {}, {e}",
                    path.display()
                )));
            }
        };

        // 3. Branch on mode. Out-of-range / empty-file reads return a structured
        // `FileReadError` (see note above) instead of a hard `Err`.
        let collected = match input.mode {
            ReadMode::Slice => match slice::read_slice(&file_bytes, input.offset, input.limit) {
                Ok(lines) => lines,
                Err(e) => return Ok(ReadFileOutput::FileReadError(e)),
            },
            ReadMode::Indentation => {
                let args = input.indentation.unwrap_or_default();
                let options = indentation::IndentationOptions {
                    anchor_line: args.anchor_line,
                    max_levels: args.max_levels,
                    include_siblings: args.include_siblings,
                    include_header: args.include_header,
                    max_lines: args.max_lines,
                };
                match indentation::read_block(&file_bytes, input.offset, input.limit, options) {
                    Ok(lines) => lines,
                    Err(e) => return Ok(ReadFileOutput::FileReadError(e)),
                }
            }
        };

        // 4. Build formatted output (L{n}: {content} lines joined by \n).
        let content = collected.join("\n");

        // 5. Build raw_output — the unformatted file content for the read
        // range. This matches the echo-agent-build ReadFileTool semantics where
        // raw_output is the actual file text without line-number prefixes.
        let raw_output = String::from_utf8_lossy(&file_bytes).into_owned();

        // 6. Compute total lines.
        let total_lines = file_bytes.iter().filter(|&&b| b == b'\n').count()
            + if file_bytes.last() != Some(&b'\n') && !file_bytes.is_empty() {
                1
            } else {
                0
            };

        // 7. Return.
        Ok(ReadFileOutput::FileContent(FileContent {
            document_progress: None,
            content,
            content_concise: None,
            absolute_path: path,
            offset: Some(input.offset),
            limit: Some(input.limit),
            raw_output,
            total_lines,
            extracted_images: Vec::new(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer::local::LocalFs;
    use crate::notification::types::ToolNotificationHandle;
    use crate::types::resources::{Cwd, NotificationHandle, Resources};
    use crate::types::tool_metadata::test_ctx;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Set up Resources with real filesystem for tests.
    fn test_resources(cwd: &std::path::Path) -> Resources {
        let mut resources = Resources::new();
        resources.insert(Cwd(cwd.to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        resources
    }

    /// Build a runtime `ToolCallContext` with the given shared resources.
    // ── Slice mode tests ─────────────────────────────────────────

    #[tokio::test]
    async fn slice_reads_requested_range() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "first\nsecond\nthird\nfourth\n").unwrap();

        let tool = CodexReadFileTool;
        let shared = test_resources(tmp.path()).into_shared();

        let input = CodexReadFileInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: 2,
            limit: 2,
            mode: ReadMode::Slice,
            indentation: None,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert_eq!(fc.content, "L2: second\nL3: third");
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn slice_offset_exceeds_length_returns_read_error() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "one\ntwo\n").unwrap();

        let tool = CodexReadFileTool;
        let shared = test_resources(tmp.path()).into_shared();

        let input = CodexReadFileInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: 100,
            limit: 10,
            mode: ReadMode::Slice,
            indentation: None,
        };

        // Out-of-range reads are surfaced as a structured `FileReadError` (a
        // model-facing error), not a hard `Err`.
        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(msg.contains("offset exceeds file length"), "got: {msg}");
            }
            other => panic!("Expected FileReadError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn empty_file_returns_read_error() {
        // An empty file has 0 lines, so the default offset=1 is out of range.
        // Codex treats this as a read error; we surface it as a structured
        // FileReadError instead of a hard Err.
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("empty.txt");
        std::fs::write(&file_path, "").unwrap();

        let tool = CodexReadFileTool;
        let shared = test_resources(tmp.path()).into_shared();

        let input = CodexReadFileInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: 1,
            limit: 10,
            mode: ReadMode::Slice,
            indentation: None,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(msg.contains("offset exceeds file length"), "got: {msg}");
            }
            other => panic!("Expected FileReadError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn slice_reads_non_utf8() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("binary.txt");
        std::fs::write(&file_path, b"\xff\xfe\n").unwrap();

        let tool = CodexReadFileTool;
        let shared = test_resources(tmp.path()).into_shared();

        let input = CodexReadFileInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: 1,
            limit: 10,
            mode: ReadMode::Slice,
            indentation: None,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert_eq!(fc.content, format!("L1: {}{}", '\u{FFFD}', '\u{FFFD}'));
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn slice_trims_crlf() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("crlf.txt");
        std::fs::write(&file_path, b"hello\r\nworld\r\n").unwrap();

        let tool = CodexReadFileTool;
        let shared = test_resources(tmp.path()).into_shared();

        let input = CodexReadFileInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: 1,
            limit: 10,
            mode: ReadMode::Slice,
            indentation: None,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert_eq!(fc.content, "L1: hello\nL2: world");
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    // ── Indentation mode tests ───────────────────────────────────

    #[tokio::test]
    async fn indentation_mode_captures_block() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("code.py");
        std::fs::write(
            &file_path,
            "def foo():\n    x = 1\n    y = 2\n    return x + y\n\ndef bar():\n    pass\n",
        )
        .unwrap();

        let tool = CodexReadFileTool;
        let shared = test_resources(tmp.path()).into_shared();

        let input = CodexReadFileInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: 1,
            limit: 2000,
            mode: ReadMode::Indentation,
            indentation: Some(IndentationArgs {
                anchor_line: Some(2),
                max_levels: 1,
                include_siblings: false,
                include_header: true,
                max_lines: None,
            }),
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(fc.content.contains("def foo():"));
                assert!(fc.content.contains("x = 1"));
                assert!(fc.content.contains("y = 2"));
                assert!(fc.content.contains("return x + y"));
            }
            other => panic!("Expected FileContent, got {:?}", other),
        }
    }

    // ── Validation tests ─────────────────────────────────────────

    #[tokio::test]
    async fn indentation_anchor_past_eof_returns_read_error() {
        // Indentation-mode range errors flow through the same read_block match
        // arm as slice mode, so they must also surface as a structured
        // FileReadError rather than a hard Err.
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("code.py");
        std::fs::write(&file_path, "def foo():\n    x = 1\n").unwrap();

        let tool = CodexReadFileTool;
        let shared = test_resources(tmp.path()).into_shared();

        let input = CodexReadFileInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: 1,
            limit: 2000,
            mode: ReadMode::Indentation,
            indentation: Some(IndentationArgs {
                anchor_line: Some(100),
                max_levels: 0,
                include_siblings: false,
                include_header: true,
                max_lines: None,
            }),
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(
                    msg.contains("anchor_line exceeds file length"),
                    "got: {msg}"
                );
            }
            other => panic!("Expected FileReadError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn validation_offset_zero() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "content\n").unwrap();

        let tool = CodexReadFileTool;
        let shared = test_resources(tmp.path()).into_shared();

        let input = CodexReadFileInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: 0,
            limit: 10,
            mode: ReadMode::Slice,
            indentation: None,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(
                    msg.contains("offset must be a 1-indexed line number"),
                    "got: {msg}"
                );
            }
            other => panic!("Expected FileReadError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn validation_limit_zero() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "content\n").unwrap();

        let tool = CodexReadFileTool;
        let shared = test_resources(tmp.path()).into_shared();

        let input = CodexReadFileInput {
            file_path: file_path.to_string_lossy().to_string(),
            offset: 1,
            limit: 0,
            mode: ReadMode::Slice,
            indentation: None,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(
                    msg.contains("limit must be greater than zero"),
                    "got: {msg}"
                );
            }
            other => panic!("Expected FileReadError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn validation_relative_path() {
        let tmp = TempDir::new().unwrap();
        let tool = CodexReadFileTool;
        let shared = test_resources(tmp.path()).into_shared();

        let input = CodexReadFileInput {
            file_path: "relative/path.txt".to_string(),
            offset: 1,
            limit: 10,
            mode: ReadMode::Slice,
            indentation: None,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(
                    msg.contains("file_path must be an absolute path"),
                    "got: {msg}"
                );
            }
            other => panic!("Expected FileReadError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn file_not_found() {
        let tmp = TempDir::new().unwrap();
        let tool = CodexReadFileTool;
        let shared = test_resources(tmp.path()).into_shared();

        let input = CodexReadFileInput {
            file_path: tmp
                .path()
                .join("nonexistent.txt")
                .to_string_lossy()
                .to_string(),
            offset: 1,
            limit: 10,
            mode: ReadMode::Slice,
            indentation: None,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(shared.clone()), input)
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileReadError(msg) => {
                assert!(msg.contains("Failed to read file"));
            }
            other => panic!("Expected FileReadError, got {:?}", other),
        }
    }
}
