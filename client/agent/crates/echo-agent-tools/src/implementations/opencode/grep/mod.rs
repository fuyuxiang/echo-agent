//! `grep` tool — OpenCode namespace.
//!
//! Shells out to the ripgrep (`rg`) binary, parses the output, sorts
//! matches by file modification time (most recent first), caps at 100
//! results, truncates long lines, and formats as grouped output.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::implementations::echo_agent_build::grep::ripgrep::{output_bounded, rg_path};
use crate::types::output::{GrepFileMatch, GrepLineMatch, GrepSearchOutput};
use crate::types::requirements::{Expr, ToolRequirement};
#[allow(unused_imports)]
use crate::types::resources::{Cwd, SharedResources};
use crate::types::tool::{ToolKind, ToolNamespace};

// ───────────────────────────────────────────────────────────────────────────
// Constants
// ───────────────────────────────────────────────────────────────────────────

const RESULT_LIMIT: usize = 100;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_STDOUT_BYTES: usize = 5_000_000;
const MAX_STDERR_BYTES: usize = 256_000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

// ───────────────────────────────────────────────────────────────────────────
// Description
// ───────────────────────────────────────────────────────────────────────────

const DESCRIPTION: &str = r#"A powerful search tool built on ripgrep

Usage:
- Prefer ${{ tools.by_kind.search }} for exact symbol/string searches. Whenever possible, use this instead of terminal grep/rg. This tool is faster and respects .gitignore
- Supports full regex syntax, e.g. `log.*Error`, `function\s+\w+`. Ensure you escape special chars to get exact matches, e.g. `functionCall\(`
- Avoid overly broad glob patterns (e.g., '--glob *') as they bypass .gitignore rules and may be slow
- The ${{ params.search.pattern }} field is a raw regex string: do NOT wrap it in quotes or add trailing quote characters unnecessarily
- Only use '${{ params.search.include }}' when certain of the file type needed. Note: import paths may not match source file types (.js vs .ts)
- Results are capped for responsiveness; truncated results show "at least" counts.
- Filter files by pattern with the ${{ params.search.include }} parameter (e.g. "*.js", "*.{ts,tsx}")
- Returns file paths and line numbers with at least one match sorted by modification time
- Use this tool when you need to find files containing specific patterns"#;

// ───────────────────────────────────────────────────────────────────────────
// Input
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GrepInput {
    /// The regex pattern to search for.
    pub pattern: String,

    /// Directory to search in. Defaults to the current working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// File glob filter (e.g. "*.ts").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<String>,
}

// ───────────────────────────────────────────────────────────────────────────
// ToolInput conversions (via Dynamic variant)
// ───────────────────────────────────────────────────────────────────────────

impl TryFrom<crate::types::tool_io::ToolInput> for GrepInput {
    type Error = String;
    fn try_from(value: crate::types::tool_io::ToolInput) -> Result<Self, Self::Error> {
        match value {
            crate::types::tool_io::ToolInput::Dynamic(v) => {
                serde_json::from_value(v).map_err(|e| format!("GrepInput: {e}"))
            }
            _ => Err("expected Dynamic variant for GrepInput".into()),
        }
    }
}

impl From<GrepInput> for crate::types::tool_io::ToolInput {
    fn from(value: GrepInput) -> Self {
        crate::types::tool_io::ToolInput::Dynamic(
            serde_json::to_value(value).expect("GrepInput serializes to JSON"),
        )
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tool implementation
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct GrepTool;

// ─── Tests ──────────────────────────────────────────────────────────

impl crate::types::tool_metadata::ToolMetadata for GrepTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::OpenCode
    }

    fn description_template(&self) -> &str {
        DESCRIPTION
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for GrepTool {
    type Args = GrepInput;
    type Output = GrepSearchOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("grep").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "grep",
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

    #[tracing::instrument(name = "tool.opencode.grep", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: GrepInput,
    ) -> Result<GrepSearchOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let cwd = crate::types::tool_metadata::resolve_cwd(&ctx, &resources).await?;

        // Resolve search path.
        let search_path = match &input.path {
            Some(p) if !p.is_empty() => {
                let candidate = std::path::Path::new(p);
                if candidate.is_absolute() {
                    candidate.to_path_buf()
                } else {
                    cwd.join(candidate)
                }
            }
            _ => cwd,
        };

        // Build rg command.
        let tool_id = echo_agent_tool_protocol::ToolId::new("grep").expect("valid tool id");
        let rg_exec = rg_path().map_err(|error| {
            echo_agent_tool_runtime::ToolError::service_unavailable(
                crate::implementations::echo_agent_build::grep::ripgrep::unavailable_message(
                    &error,
                ),
            )
            .with_source(error)
        })?;
        let mut cmd = Command::new(rg_exec);
        cmd.args([
            "-n",
            "-H",
            "--hidden",
            "--no-messages",
            "--field-match-separator=|",
            "--regexp",
        ]);
        cmd.arg(&input.pattern);

        if let Some(ref include) = input.include
            && !include.is_empty()
        {
            cmd.arg("--glob").arg(include);
        }

        cmd.arg(search_path.to_string_lossy().as_ref());
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::util::detach_search_command(&mut cmd);

        let output = output_bounded(
            &mut cmd,
            COMMAND_TIMEOUT,
            MAX_STDOUT_BYTES,
            MAX_STDERR_BYTES,
        )
        .await
        .map_err(|error| error.into_tool_error(tool_id.clone()))?;
        let stdout_buf = output.stdout;
        let stderr_buf = output.stderr;
        let exit_code = output.exit_code;
        let truncated_by_bytes = output.stdout_truncated;

        // Exit code 1 = no matches, exit code 2 with no output = errors only.
        let stdout_str = String::from_utf8_lossy(&stdout_buf);
        if exit_code == 1 {
            let formatted = "No files found".to_string();
            return Ok(GrepSearchOutput {
                stdout: formatted.into_bytes(),
                stderr: stderr_buf,
                exit_code,
                match_count: 0,
                file_matches: Vec::new(),
            });
        }
        if exit_code != 0 {
            let stderr = String::from_utf8_lossy(&stderr_buf);
            let detail = if stderr.trim().is_empty() {
                "未返回详细错误信息"
            } else {
                stderr.trim()
            };
            return Err(echo_agent_tool_runtime::ToolError::execution(
                tool_id,
                format!("ripgrep 搜索失败（退出码 {exit_code}）：{detail}"),
            ));
        }

        // ── Parse ripgrep output (format: filepath|linenum|linetext) ────
        struct RawMatch {
            path: String,
            line_num: usize,
            line_text: String,
            mtime_ms: u64,
        }

        let mut matches: Vec<RawMatch> = Vec::new();
        let mut mtime_cache: HashMap<String, u64> = HashMap::new();

        for line in stdout_str.lines() {
            if line.is_empty() {
                continue;
            }
            // Split on first two `|` separators.
            let mut parts = line.splitn(3, '|');
            let file_path = match parts.next() {
                Some(p) => p,
                None => continue,
            };
            let line_num_str = match parts.next() {
                Some(n) => n,
                None => continue,
            };
            let line_text = parts.next().unwrap_or("");
            let line_num = match line_num_str.parse::<usize>() {
                Ok(n) => n,
                Err(_) => continue,
            };

            // Cache mtime per file.
            let mtime_ms = if let Some(&cached) = mtime_cache.get(file_path) {
                cached
            } else {
                let mtime = tokio::fs::metadata(file_path)
                    .await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                mtime_cache.insert(file_path.to_string(), mtime);
                mtime
            };

            matches.push(RawMatch {
                path: file_path.to_string(),
                line_num,
                line_text: line_text.to_string(),
                mtime_ms,
            });
        }

        // Sort by mtime (most recent first).
        matches.sort_by_key(|b| std::cmp::Reverse(b.mtime_ms));

        let total_matches = matches.len();
        let truncated = truncated_by_bytes || total_matches > RESULT_LIMIT;
        let final_matches = if truncated {
            &matches[..RESULT_LIMIT]
        } else {
            &matches[..]
        };

        if final_matches.is_empty() {
            let formatted = "No files found".to_string();
            return Ok(GrepSearchOutput {
                stdout: formatted.into_bytes(),
                stderr: stderr_buf,
                exit_code,
                match_count: 0,
                file_matches: Vec::new(),
            });
        }

        // ── Format output grouped by file ───────────────────────────────
        let mut output_lines: Vec<String> = Vec::new();
        if truncated {
            output_lines.push(format!(
                "Found {total_matches} matches (showing first {RESULT_LIMIT})"
            ));
        } else {
            output_lines.push(format!("Found {total_matches} matches"));
        }

        // Build file_matches for structured output + formatted text.
        let mut file_matches: Vec<GrepFileMatch> = Vec::new();
        let mut current_path = String::new();
        let mut current_file_match_lines: Vec<GrepLineMatch> = Vec::new();

        for m in final_matches {
            if m.path != current_path {
                if !current_path.is_empty() {
                    file_matches.push(GrepFileMatch {
                        path: current_path.clone(),
                        matches: std::mem::take(&mut current_file_match_lines),
                    });
                    output_lines.push(String::new()); // blank line between files
                }
                current_path = m.path.clone();
                output_lines.push(format!("{}:", m.path));
            }
            let display_text = if m.line_text.len() > MAX_LINE_LENGTH {
                format!("{}...", &m.line_text[..MAX_LINE_LENGTH])
            } else {
                m.line_text.clone()
            };
            output_lines.push(format!("  Line {}: {}", m.line_num, display_text));
            current_file_match_lines.push(GrepLineMatch {
                line_number: m.line_num,
                content: m.line_text.clone(),
            });
        }
        // Flush last file.
        if !current_path.is_empty() {
            file_matches.push(GrepFileMatch {
                path: current_path,
                matches: current_file_match_lines,
            });
        }

        if truncated {
            output_lines.push(String::new());
            output_lines.push(format!(
                "(Results truncated: showing {RESULT_LIMIT} of {total_matches} matches. Consider using a more specific path or pattern.)"
            ));
        }

        let formatted = output_lines.join("\n");

        Ok(GrepSearchOutput {
            stdout: formatted.into_bytes(),
            stderr: stderr_buf,
            exit_code: 0, // normalized — we have results
            match_count: total_matches,
            file_matches,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx;

    use crate::types::resources::Resources;
    use tempfile::TempDir;

    /// Build a `Resources` bag with only `Cwd` inserted.
    fn test_resources(cwd: &std::path::Path) -> Resources {
        let mut resources = Resources::new();
        resources.insert(Cwd(cwd.to_path_buf()));
        resources
    }

    // ── tool_metadata ────────────────────────────────────────────────

    #[test]
    fn tool_metadata() {
        use crate::types::tool_metadata::ToolMetadata;
        let tool = GrepTool;
        assert_eq!(echo_agent_tool_runtime::Tool::id(&tool).as_str(), "grep");
        assert_eq!(tool.kind(), ToolKind::Search);
        assert!(
            matches!(tool.tool_namespace(), ToolNamespace::OpenCode),
            "expected OpenCode namespace"
        );
    }

    // ── serde_roundtrip ──────────────────────────────────────────────

    #[test]
    fn serde_roundtrip() {
        let input = GrepInput {
            pattern: "foo.*bar".to_string(),
            path: Some("/tmp/dir".to_string()),
            include: Some("*.rs".to_string()),
        };
        let json = serde_json::to_value(&input).unwrap();
        let back: GrepInput = serde_json::from_value(json).unwrap();
        assert_eq!(back.pattern, "foo.*bar");
        assert_eq!(back.path.as_deref(), Some("/tmp/dir"));
        assert_eq!(back.include.as_deref(), Some("*.rs"));

        // Minimal input — only pattern, optional fields absent.
        let minimal = serde_json::json!({ "pattern": "hello" });
        let parsed: GrepInput = serde_json::from_value(minimal).unwrap();
        assert_eq!(parsed.pattern, "hello");
        assert!(parsed.path.is_none());
        assert!(parsed.include.is_none());
    }

    // ── basic_match ──────────────────────────────────────────────────

    #[tokio::test]
    async fn basic_match() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello world\ngoodbye world\n").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "no match here\n").unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "hello".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("Found 1 matches"), "header missing: {text}");
        assert!(text.contains("a.txt:"), "file header missing: {text}");
        assert!(
            text.contains("Line 1: hello world"),
            "line format wrong: {text}"
        );
        assert!(!text.contains("b.txt"), "non-matching file listed: {text}");
        assert_eq!(output.match_count, 1);
        assert_eq!(output.exit_code, 0);
    }

    // ── no_matches ───────────────────────────────────────────────────

    #[tokio::test]
    async fn no_matches() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello world\n").unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "zzz_nonexistent_zzz".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains("No files found"),
            "expected 'No files found': {text}"
        );
        assert_eq!(output.match_count, 0);
        assert!(output.file_matches.is_empty());
    }

    // ── multiple_files_grouped ───────────────────────────────────────

    #[tokio::test]
    async fn multiple_files_grouped() {
        let tmp = TempDir::new().unwrap();
        // Create two files, both containing the pattern.
        std::fs::write(
            tmp.path().join("first.txt"),
            "match_me line1\nmatch_me line2\n",
        )
        .unwrap();
        // Small delay so mtime differs and order is deterministic.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(tmp.path().join("second.txt"), "match_me line1\n").unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "match_me".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("Found 3 matches"), "header: {text}");

        // Verify blank line separates the two file groups.
        // Output format: header\n{path1}:\n  Line ...\n  Line ...\n\n{path2}:\n  Line ...
        let lines: Vec<&str> = text.lines().collect();
        // Find the blank separator line between groups.
        let blank_positions: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.is_empty())
            .map(|(i, _)| i)
            .collect();
        assert!(
            !blank_positions.is_empty(),
            "expected blank line between file groups: {text}"
        );

        // Both files should appear.
        assert!(text.contains("first.txt:"), "first.txt missing: {text}");
        assert!(text.contains("second.txt:"), "second.txt missing: {text}");
        assert_eq!(output.match_count, 3);
    }

    // ── include_glob_filter ──────────────────────────────────────────

    #[tokio::test]
    async fn include_glob_filter() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("code.rs"), "fn hello() {}\n").unwrap();
        std::fs::write(tmp.path().join("code.py"), "def hello(): pass\n").unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "hello".to_string(),
                path: None,
                include: Some("*.rs".to_string()),
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("code.rs"), "expected code.rs match: {text}");
        assert!(
            !text.contains("code.py"),
            "code.py should be excluded: {text}"
        );
        assert_eq!(output.match_count, 1);
    }

    // ── path_parameter_absolute ──────────────────────────────────────

    #[tokio::test]
    async fn path_parameter_absolute() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("target.txt"), "needle\n").unwrap();
        std::fs::write(tmp.path().join("root.txt"), "needle\n").unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        // Pass an absolute path to the subdirectory.
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "needle".to_string(),
                path: Some(sub.to_string_lossy().into_owned()),
                include: None,
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("target.txt"), "expected target.txt: {text}");
        assert!(
            !text.contains("root.txt"),
            "root.txt should be excluded: {text}"
        );
        assert_eq!(output.match_count, 1);
    }

    // ── path_parameter_relative ──────────────────────────────────────

    #[tokio::test]
    async fn path_parameter_relative() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("mydir");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("inner.txt"), "needle\n").unwrap();
        std::fs::write(tmp.path().join("outer.txt"), "needle\n").unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        // Pass a relative path — should resolve against Cwd.
        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "needle".to_string(),
                path: Some("mydir".to_string()),
                include: None,
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("inner.txt"), "expected inner.txt: {text}");
        assert!(
            !text.contains("outer.txt"),
            "outer.txt should be excluded: {text}"
        );
        assert_eq!(output.match_count, 1);
    }

    // ── match_count_field ────────────────────────────────────────────

    #[tokio::test]
    async fn match_count_field() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("multi.txt"),
            "alpha\nbeta\nalpha again\nalpha third\n",
        )
        .unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "alpha".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        // Three lines contain "alpha".
        assert_eq!(output.match_count, 3);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("Found 3 matches"), "header: {text}");
    }

    // ── file_matches_populated ───────────────────────────────────────

    #[tokio::test]
    async fn file_matches_populated() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("f.txt"),
            "line1 target\nline2\nline3 target\n",
        )
        .unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "target".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(output.file_matches.len(), 1);
        let fm = &output.file_matches[0];
        assert!(
            fm.path.contains("f.txt"),
            "path should contain f.txt: {}",
            fm.path
        );
        assert_eq!(fm.matches.len(), 2);

        // First match: line 1.
        assert_eq!(fm.matches[0].line_number, 1);
        assert!(fm.matches[0].content.contains("target"));
        // Second match: line 3.
        assert_eq!(fm.matches[1].line_number, 3);
        assert!(fm.matches[1].content.contains("target"));
    }

    // ── missing_cwd_resource ─────────────────────────────────────────

    #[tokio::test]
    async fn missing_cwd_resource() {
        let tool = GrepTool;
        let resources = Resources::new(); // No Cwd inserted.

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "anything".to_string(),
                path: None,
                include: None,
            },
        )
        .await;

        assert!(result.is_err(), "expected error for missing Cwd");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Cwd not available"),
            "wrong error message: {err:?}"
        );
    }

    // ── mtime_sorting ───────────────────────────────────────────────

    #[tokio::test]
    async fn mtime_sorting() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("old.txt"), "match\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(tmp.path().join("new.txt"), "match\n").unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "match".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        let new_pos = text.find("new.txt").expect("new.txt not in output");
        let old_pos = text.find("old.txt").expect("old.txt not in output");
        assert!(
            new_pos < old_pos,
            "expected new.txt before old.txt (mtime sort): {text}"
        );
    }

    // ── match_cap_100 ───────────────────────────────────────────────

    #[tokio::test]
    async fn match_cap_100() {
        let tmp = TempDir::new().unwrap();
        let content: String = (0..150).map(|_| "pattern\n").collect();
        std::fs::write(tmp.path().join("big.txt"), &content).unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "pattern".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            output.match_count, 150,
            "match_count should be total: {text}"
        );
        assert!(
            text.contains("showing 100 of 150"),
            "expected truncation message: {text}"
        );
        // Count "  Line " prefixes — exactly 100 displayed lines.
        let displayed = text.matches("  Line ").count();
        assert_eq!(
            displayed, 100,
            "expected 100 displayed lines, got {displayed}: {text}"
        );
    }

    // ── line_truncation_2000_chars ──────────────────────────────────

    #[tokio::test]
    async fn line_truncation_2000_chars() {
        let tmp = TempDir::new().unwrap();
        let long_line = format!("MATCH{}", "x".repeat(2500));
        std::fs::write(tmp.path().join("long.txt"), format!("{long_line}\n")).unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "MATCH".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        // Find the formatted "  Line N: ..." output line.
        let match_line = text
            .lines()
            .find(|l| l.trim_start().starts_with("Line "))
            .expect("no match line in output");
        assert!(
            match_line.ends_with("..."),
            "expected truncated line ending with '...': {match_line}"
        );
        // The displayed content after "  Line N: " should be at most 2003 chars (2000 + "...").
        let content_start = match_line.find(": ").unwrap() + 2;
        let display_content = &match_line[content_start..];
        assert!(
            display_content.len() <= 2003,
            "display content length {} exceeds 2003: {display_content}",
            display_content.len()
        );
    }

    // ── exit_code_1_no_matches ──────────────────────────────────────

    #[tokio::test]
    async fn exit_code_1_no_matches() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello world\n").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "goodbye world\n").unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "zzz_absolutely_nothing_zzz".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        assert_eq!(text.as_ref(), "No files found", "stdout: {text}");
        assert_eq!(output.match_count, 0);
    }

    // ── special_regex_chars ─────────────────────────────────────────

    #[tokio::test]
    async fn special_regex_chars() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("code.txt"), "log(err)\nlogout\n").unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: r"log\(".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("log(err)"), "should match log(err): {text}");
        assert!(
            !text.contains("logout"),
            "should NOT match 'logout': {text}"
        );
        assert_eq!(output.match_count, 1);
    }

    // ── pipe_in_match_text ──────────────────────────────────────────

    #[tokio::test]
    async fn pipe_in_match_text() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("pipes.txt"), "a | b | c\n").unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "a".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(output.file_matches.len(), 1);
        let line_match = &output.file_matches[0].matches[0];
        assert!(
            line_match.content.contains("a | b | c"),
            "pipe chars must survive parsing; got: {}",
            line_match.content
        );
    }

    // ── empty_pattern ───────────────────────────────────────────────

    #[tokio::test]
    async fn empty_pattern() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("any.txt"), "some content\n").unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "".to_string(),
                path: None,
                include: None,
            },
        )
        .await;

        // Empty pattern may match everything or produce an error from rg.
        // The key assertion: no panic, returns Ok.
        assert!(result.is_ok(), "empty pattern should not panic: {result:?}");
    }

    // ── exit_code_2_with_output ─────────────────────────────────────

    #[tokio::test]
    #[cfg(unix)]
    async fn exit_code_2_with_output() {
        // ripgrep returns exit code 2 when some paths are inaccessible
        // but valid matches exist in other files.
        // We trigger this by creating a broken symlink alongside a real file.
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("real.txt"), "findme\n").unwrap();
        // Create a broken symlink: link -> nonexistent_target
        std::os::unix::fs::symlink(
            tmp.path().join("nonexistent_target"),
            tmp.path().join("broken_link"),
        )
        .unwrap();

        let tool = GrepTool;
        let resources = test_resources(tmp.path());

        let output = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            GrepInput {
                pattern: "findme".to_string(),
                path: None,
                include: None,
            },
        )
        .await
        .unwrap();

        let text = String::from_utf8_lossy(&output.stdout);
        // We expect the match from real.txt to be present.
        assert!(
            text.contains("real.txt"),
            "expected real.txt match in output: {text}"
        );
        // Note: ripgrep's --no-messages flag suppresses error messages so rg
        // may not return exit code 2 for a broken symlink. If rg returns 0
        // instead, the "(Some paths were inaccessible)" message won't appear.
        // We still verify the match was found; the exit-code-2 path is
        // exercised only when rg actually reports partial errors.
        assert!(output.match_count >= 1, "should have at least 1 match");
    }

    // ── exit_code_2_without_output ──────────────────────────────────

    // Triggering ripgrep exit code 2 with zero stdout (errors only, no
    // matches) is impractical in a unit test with real `rg`. The shared
    // bounded runner's missing-binary test covers the critical invariant:
    // launch failures become errors rather than empty successful results.
}
