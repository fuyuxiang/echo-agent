//! `CodexGrepFilesTool` — file-path-only regex search via ripgrep.
//!
//! This is a faithful port of `codex-rs/core/src/tools/handlers/grep_files.rs`.
//! It returns **file paths only** (`--files-with-matches`), sorted by
//! modification time. See the plan document for the full diff vs the
//! echo-agent-build `GrepTool`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::implementations::echo_agent_build::grep::ripgrep::{
    BoundedRgError, output_bounded, rg_path, unavailable_message,
};
use crate::types::output::CodexGrepFilesOutput;
use crate::types::requirements::Expr;
#[allow(unused_imports)]
use crate::types::resources::Cwd;
use crate::types::tool::{ToolKind, ToolNamespace};

// ─── Constants ──────────────────────────────────────────────────────

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 2000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STDOUT_BYTES: usize = 5_000_000;
const MAX_STDERR_BYTES: usize = 256_000;

// ─── Description ────────────────────────────────────────────────────

const DESCRIPTION: &str = "Finds files whose contents match the ${{ params.search.pattern }} and lists them by modification time.";

// ─── Input ──────────────────────────────────────────────────────────

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

/// Input for the codex `grep_files` tool.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CodexGrepFilesInput {
    /// Regular expression pattern to search for.
    pub pattern: String,

    /// Optional glob that limits which files are searched (e.g. "*.rs" or "*.{ts,tsx}").
    #[serde(default)]
    pub include: Option<String>,

    /// Directory or file path to search. Defaults to the session's working directory.
    #[serde(default)]
    pub path: Option<String>,

    /// Maximum number of file paths to return (defaults to 100).
    #[serde(default = "default_limit")]
    pub limit: usize,
}

// ─── Tool ───────────────────────────────────────────────────────────

/// Codex-namespace grep_files tool — file-path-only regex search.
///
/// Shares `ToolKind::Search` with the echo-agent-build `GrepTool`. These tools are
/// namespace-exclusive — consumers enable either `EchoAgentBuild` or `Codex` search,
/// never both simultaneously. This follows the same pattern as
/// `CodexListDirTool`/`ListDirTool` (`ToolKind::ListDir`) and
/// `CodexReadFileTool`/`ReadFileImpl` (`ToolKind::Read`).
#[derive(Debug, Default)]
pub struct CodexGrepFilesTool;

// ─── rg execution ───────────────────────────────────────────────────

/// Run `rg --files-with-matches` and return matching file paths.
///
/// Direct port from `codex-rs/core/src/tools/handlers/grep_files.rs`.
async fn run_rg_search(
    pattern: &str,
    include: Option<&str>,
    search_path: &Path,
    limit: usize,
    cwd: &Path,
) -> Result<Vec<String>, String> {
    let rg_exec = rg_path().map_err(|error| unavailable_message(&error))?;
    let mut command = Command::new(rg_exec);
    command
        .current_dir(cwd)
        .arg("--files-with-matches")
        .arg("--sortr=modified")
        .arg("--regexp")
        .arg(pattern)
        .arg("--no-messages");

    if let Some(glob) = include {
        command.arg("--glob").arg(glob);
    }

    command.arg("--").arg(search_path);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::util::detach_search_command(&mut command);

    let output = output_bounded(
        &mut command,
        COMMAND_TIMEOUT,
        MAX_STDOUT_BYTES,
        MAX_STDERR_BYTES,
    )
    .await
    .map_err(|error| match error {
        BoundedRgError::Spawn(source) => unavailable_message(&source),
        BoundedRgError::Timeout(seconds) => format!(
            "搜索超过 {seconds} 秒未完成，已安全终止。请缩小搜索路径或使用更具体的匹配条件。"
        ),
        BoundedRgError::Io(source) => format!("读取内置搜索组件输出失败：{source}"),
    })?;

    match output.exit_code {
        0 => Ok(parse_results(&output.stdout, limit)),
        1 => Ok(Vec::new()),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = if stderr.trim().is_empty() {
                "未返回详细错误信息"
            } else {
                stderr.trim()
            };
            Err(format!(
                "ripgrep 搜索失败（退出码 {}）：{detail}",
                output.exit_code
            ))
        }
    }
}

/// Parse newline-separated file paths from rg stdout.
///
/// Direct port from `codex-rs/core/src/tools/handlers/grep_files.rs`.
fn parse_results(stdout: &[u8], limit: usize) -> Vec<String> {
    let mut results = Vec::new();
    for line in stdout.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(line) {
            if text.is_empty() {
                continue;
            }
            results.push(text.to_string());
            if results.len() == limit {
                break;
            }
        }
    }
    results
}

// ─── Tests ──────────────────────────────────────────────────────────

impl crate::types::tool_metadata::ToolMetadata for CodexGrepFilesTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Search
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

impl echo_agent_tool_runtime::Tool for CodexGrepFilesTool {
    type Args = CodexGrepFilesInput;
    type Output = CodexGrepFilesOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("grep_files").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "grep_files",
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

    #[tracing::instrument(name = "tool.codex_grep_files", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: CodexGrepFilesInput,
    ) -> Result<CodexGrepFilesOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let cwd = crate::types::tool_metadata::resolve_cwd(&ctx, &resources).await?;

        // Validation (exact codex rules)
        let pattern = input.pattern.trim().to_string();
        if pattern.is_empty() {
            return Ok(CodexGrepFilesOutput::Error(
                "pattern must not be empty".to_string(),
            ));
        }
        if input.limit == 0 {
            return Ok(CodexGrepFilesOutput::Error(
                "limit must be greater than zero".to_string(),
            ));
        }

        let limit = input.limit.min(MAX_LIMIT);

        // Resolve search path
        let search_path = match &input.path {
            Some(p) if !p.is_empty() => {
                let p = PathBuf::from(p);
                if p.is_absolute() { p } else { cwd.join(p) }
            }
            _ => cwd.clone(),
        };

        // Verify path exists
        if let Err(err) = tokio::fs::metadata(&search_path).await {
            return Ok(CodexGrepFilesOutput::Error(format!(
                "unable to access `{}`: {err}",
                search_path.display()
            )));
        }

        // Clean up include glob
        let include = input.include.as_deref().map(str::trim).and_then(|v| {
            if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            }
        });

        // Run rg
        let results = run_rg_search(&pattern, include.as_deref(), &search_path, limit, &cwd).await;

        match results {
            Ok(files) if files.is_empty() => Ok(CodexGrepFilesOutput::NoMatches(
                "No matches found.".to_string(),
            )),
            Ok(files) => {
                let file_count = files.len();
                Ok(CodexGrepFilesOutput::Matches {
                    content: files.join("\n"),
                    file_count,
                })
            }
            Err(msg) => Ok(CodexGrepFilesOutput::Error(msg)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::resources::Resources;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    /// Build a runtime `ToolCallContext` with the given resources.
    fn test_ctx(cwd: &Path) -> echo_agent_tool_runtime::ToolCallContext {
        let mut resources = Resources::new();
        resources.insert(Cwd(cwd.to_path_buf()));
        let mut ctx = echo_agent_tool_runtime::ToolCallContext::default();
        ctx.extensions.insert(resources.into_shared());
        ctx
    }
    fn rg_available() -> bool {
        // Probe the resolver the tool uses (hermetic under Bazel), not PATH.
        StdCommand::new(rg_path().expect("ripgrep path should resolve"))
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// Build a runtime `ToolCallContext` with the given resources.
    // ── Unit tests (parse_results) ──────────────────────────────

    #[test]
    fn parses_basic_results() {
        let stdout = b"/tmp/file_a.rs\n/tmp/file_b.rs\n";
        let parsed = parse_results(stdout, 10);
        assert_eq!(
            parsed,
            vec!["/tmp/file_a.rs".to_string(), "/tmp/file_b.rs".to_string()]
        );
    }

    #[test]
    fn parse_truncates_after_limit() {
        let stdout = b"/tmp/file_a.rs\n/tmp/file_b.rs\n/tmp/file_c.rs\n";
        let parsed = parse_results(stdout, 2);
        assert_eq!(
            parsed,
            vec!["/tmp/file_a.rs".to_string(), "/tmp/file_b.rs".to_string()]
        );
    }

    #[test]
    fn parse_skips_empty_lines() {
        let stdout = b"/tmp/file_a.rs\n\n\n/tmp/file_b.rs\n";
        let parsed = parse_results(stdout, 10);
        assert_eq!(
            parsed,
            vec!["/tmp/file_a.rs".to_string(), "/tmp/file_b.rs".to_string()]
        );
    }

    #[test]
    fn parse_returns_empty_for_empty_input() {
        let stdout = b"";
        let parsed = parse_results(stdout, 10);
        assert!(parsed.is_empty());
    }

    // ── Integration tests (run_rg_search) ───────────────────────

    #[tokio::test]
    async fn run_search_returns_results() {
        if !rg_available() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("match.rs"), "needle in haystack").unwrap();
        std::fs::write(tmp.path().join("nomatch.rs"), "just hay").unwrap();

        let results = run_rg_search("needle", None, tmp.path(), 100, tmp.path())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].contains("match.rs"));
    }

    #[tokio::test]
    async fn run_search_with_glob_filter() {
        if !rg_available() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "needle").unwrap();
        std::fs::write(tmp.path().join("beta.txt"), "needle").unwrap();

        let results = run_rg_search("needle", Some("*.rs"), tmp.path(), 100, tmp.path())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].contains("alpha.rs"));
    }

    #[tokio::test]
    async fn run_search_respects_limit() {
        if !rg_available() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        for i in 0..5 {
            std::fs::write(tmp.path().join(format!("file_{i}.rs")), "needle").unwrap();
        }

        let results = run_rg_search("needle", None, tmp.path(), 2, tmp.path())
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn run_search_handles_no_matches() {
        if !rg_available() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("file.rs"), "no match here").unwrap();

        let results = run_rg_search("nonexistent_pattern_xyz", None, tmp.path(), 100, tmp.path())
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    // ── Tool-level tests ────────────────────────────────────────

    #[tokio::test]
    async fn tool_reports_empty_pattern_error() {
        let tmp = TempDir::new().unwrap();
        let tool = CodexGrepFilesTool;

        let input = CodexGrepFilesInput {
            pattern: "  ".to_string(),
            include: None,
            path: None,
            limit: 100,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(tmp.path()), input)
            .await
            .unwrap();
        match result {
            CodexGrepFilesOutput::Error(msg) => {
                assert_eq!(msg, "pattern must not be empty");
            }
            other => panic!("Expected Error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_reports_zero_limit_error() {
        let tmp = TempDir::new().unwrap();
        let tool = CodexGrepFilesTool;

        let input = CodexGrepFilesInput {
            pattern: "test".to_string(),
            include: None,
            path: None,
            limit: 0,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(tmp.path()), input)
            .await
            .unwrap();
        match result {
            CodexGrepFilesOutput::Error(msg) => {
                assert_eq!(msg, "limit must be greater than zero");
            }
            other => panic!("Expected Error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_reports_no_matches() {
        if !rg_available() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("file.rs"), "nothing interesting").unwrap();

        let tool = CodexGrepFilesTool;

        let input = CodexGrepFilesInput {
            pattern: "nonexistent_pattern_xyz".to_string(),
            include: None,
            path: None,
            limit: 100,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(tmp.path()), input)
            .await
            .unwrap();
        match result {
            CodexGrepFilesOutput::NoMatches(msg) => {
                assert_eq!(msg, "No matches found.");
            }
            other => panic!("Expected NoMatches, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_collects_matches() {
        if !rg_available() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "needle here").unwrap();
        std::fs::write(tmp.path().join("beta.rs"), "needle there").unwrap();
        std::fs::write(tmp.path().join("gamma.txt"), "no match").unwrap();

        let tool = CodexGrepFilesTool;

        let input = CodexGrepFilesInput {
            pattern: "needle".to_string(),
            include: Some("*.rs".to_string()),
            path: None,
            limit: 100,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(tmp.path()), input)
            .await
            .unwrap();
        match result {
            CodexGrepFilesOutput::Matches {
                file_count,
                content,
            } => {
                assert_eq!(file_count, 2);
                assert!(content.contains("alpha.rs"));
                assert!(content.contains("beta.rs"));
                assert!(!content.contains("gamma.txt"));
            }
            other => panic!("Expected Matches, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_reports_nonexistent_path_error() {
        let tmp = TempDir::new().unwrap();
        let tool = CodexGrepFilesTool;

        let input = CodexGrepFilesInput {
            pattern: "test".to_string(),
            include: None,
            path: Some("nonexistent_dir".to_string()),
            limit: 100,
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(tmp.path()), input)
            .await
            .unwrap();
        match result {
            CodexGrepFilesOutput::Error(msg) => {
                assert!(
                    msg.contains("unable to access"),
                    "Expected path error, got: {msg}"
                );
            }
            other => panic!("Expected Error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_clamps_limit_to_max() {
        if !rg_available() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("file.rs"), "needle").unwrap();

        let tool = CodexGrepFilesTool;

        let input = CodexGrepFilesInput {
            pattern: "needle".to_string(),
            include: None,
            path: None,
            limit: 5000, // exceeds MAX_LIMIT (2000)
        };

        let result = echo_agent_tool_runtime::Tool::run(&tool, test_ctx(tmp.path()), input)
            .await
            .unwrap();
        match result {
            CodexGrepFilesOutput::Matches { file_count, .. } => {
                assert_eq!(file_count, 1);
            }
            other => panic!("Expected Matches, got: {other:?}"),
        }
    }
}
