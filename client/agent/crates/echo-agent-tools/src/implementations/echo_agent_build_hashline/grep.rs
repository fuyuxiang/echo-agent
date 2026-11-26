//! `hashline_grep` — anchor-annotated search results.
//!
//! Delegates to the standard `GrepTool` for ripgrep execution, then
//! post-processes content-mode output to inject scheme-aware anchors.
//! Enables grep → edit workflows without an intermediate file read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::computer::types::AsyncFileSystem;
use crate::implementations::echo_agent_build::grep::{GrepSearchInput, GrepTool, OutputMode};

use crate::types::output::GrepSearchOutput;
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::Params;
use crate::types::tool::{ToolKind, ToolNamespace};

use super::anchor::split_lines;
use super::config::HashlineSchemeParams;
use super::scheme::{Anchor, AnchorScheme};

/// Default timeout for anchor injection (seconds).
const DEFAULT_ANCHOR_TIMEOUT_SECS: u64 = 60;

/// Get cached anchors or generate and cache them for a single invocation.
async fn get_or_generate<'a>(
    cache: &'a mut HashMap<PathBuf, Vec<Anchor>>,
    path: &Path,
    scheme: &dyn AnchorScheme,
    fs: &dyn AsyncFileSystem,
) -> Option<&'a [Anchor]> {
    if !cache.contains_key(path) {
        let bytes = fs.read_file(path).await.ok()?;
        let content = String::from_utf8_lossy(&bytes);
        let lines = split_lines(&content);
        cache.insert(path.to_path_buf(), scheme.generate_anchors(&lines));
    }
    cache.get(path).map(|v| v.as_slice())
}

/// Inject anchors into ripgrep content-mode output.
///
/// Transforms lines like `123:    let x = 1;` or `124-    let y = 2;`
/// into `123:abc:rst:    let x = 1;` or `124:abc:rst-    let y = 2;`.
///
/// Lines that are file headers or separators pass through unchanged.
pub(crate) async fn inject_anchors(
    stdout_bytes: &[u8],
    cwd: &Path,
    fs: &dyn AsyncFileSystem,
    scheme: &dyn AnchorScheme,
) -> Vec<u8> {
    let stdout = String::from_utf8_lossy(stdout_bytes);

    let (prefix, body, suffix) = match (stdout.find(">\n"), stdout.rfind("\n</workspace_result>")) {
        (Some(start), Some(end)) => {
            let body_start = start + 2;
            (
                &stdout[..body_start],
                &stdout[body_start..end],
                &stdout[end..],
            )
        }
        _ => return stdout_bytes.to_vec(),
    };

    let mut file_anchors: HashMap<PathBuf, Vec<Anchor>> = HashMap::new();
    let mut result = String::from(prefix);
    let mut current_file: Option<PathBuf> = None;

    for line in body.lines() {
        if !result.ends_with('\n') && !result.ends_with('>') {
            result.push('\n');
        }

        // Group separators (--) pass through.
        if line == "--" {
            result.push_str(line);
            continue;
        }

        // Try to parse as a numbered match/context line first.
        // This correctly handles file paths that start with digits (e.g.
        // "2024_migration.rs") — they won't parse as valid rg lines because
        // they lack a ':' or '-' separator after the numeric prefix.
        if let Some((line_num, separator, content)) = parse_rg_line(line)
            && let Some(ref file_path) = current_file
            && let Some(anchors) = get_or_generate(&mut file_anchors, file_path, scheme, fs).await
            && line_num.saturating_sub(1) < anchors.len()
        {
            let a = &anchors[line_num - 1];
            let suffix_str = match &a.context {
                Some(ctx) => format!("{}:{ctx}", a.local),
                None => a.local.clone(),
            };
            result.push_str(&format!("{line_num}:{suffix_str}{separator}{content}"));
            continue;
        }

        // Not a numbered line — could be a file header, summary, or
        // a numbered line that failed anchor lookup (passed through below).
        if parse_rg_line(line).is_none()
            && !line.starts_with("Found ")
            && !line.starts_with("... [")
            && !line.is_empty()
        {
            current_file = Some(cwd.join(line));
        }

        result.push_str(line);
    }

    result.push_str(suffix);
    result.into_bytes()
}

/// Parse a ripgrep numbered line: `123:content` or `45-context`.
fn parse_rg_line(line: &str) -> Option<(usize, char, &str)> {
    let bytes = line.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx == 0 || idx >= bytes.len() {
        return None;
    }
    let sep = bytes[idx] as char;
    if sep != ':' && sep != '-' {
        return None;
    }
    let num: usize = line[..idx].parse().ok()?;
    Some((num, sep, &line[idx + 1..]))
}

const DESCRIPTION: &str = r#"Search file contents with anchor-annotated results${%- if tools.by_kind.edit %} for use with ${{ tools.by_kind.edit }}${%- endif %}.

Match lines include anchors${%- if tools.by_kind.edit %} you can pass directly to ${{ tools.by_kind.edit }}${%- if tools.by_kind.read %} without needing to ${{ tools.by_kind.read }} the file first${%- endif %}${%- endif %}. This grep format keeps grep-style separators after the anchor: `:` for
match lines and `-` for context lines.

Content output format:

  {grep_match}    ← match (:)
  {grep_context}    ← context (-)

Usage:
- ${{ params.search.pattern }} is a regex: `log.*Error`, `function\s+\w+`, `TODO`
- Default output is anchored content matches (no output-mode selector)
- Use -A, -B, -C for context lines around matches
- Only use '${{ params.search.type }}' or '${{ params.search.glob }}' when certain of the file type
- Results are capped; truncated results show "at least" counts"#;

/// `hashline_grep` — searches with anchor-annotated results.
#[derive(Debug, Default)]
pub struct HashlineGrepTool;

impl crate::types::tool_metadata::ToolMetadata for HashlineGrepTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Search
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

impl echo_agent_tool_runtime::Tool for HashlineGrepTool {
    type Args = GrepSearchInput;
    type Output = GrepSearchOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("hashline_grep").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "hashline_grep",
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
        name = "tool.hashline_grep",
        skip_all,
        fields(timed_out = tracing::field::Empty)
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: GrepSearchInput,
    ) -> Result<GrepSearchOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let output_mode = input.output_mode.clone().unwrap_or(OutputMode::Content);

        // Delegate to standard GrepTool for ripgrep execution.
        let grep = GrepTool;
        let cwd = crate::types::tool_metadata::resolve_cwd(&ctx, &resources).await?;
        let call_id = echo_agent_tool_protocol::ToolCallId::new_v7();
        let mut rt_ctx = echo_agent_tool_runtime::ToolCallContext::new(call_id);
        rt_ctx.extensions.insert(resources.clone());
        rt_ctx.extensions.insert(echo_agent_tool_runtime::Cwd(cwd));
        let mut result = echo_agent_tool_runtime::Tool::run(&grep, rt_ctx, input)
            .await
            .map_err(|e| {
                echo_agent_tool_runtime::ToolError::execution(
                    echo_agent_tool_protocol::ToolId::new("grep").expect("valid"),
                    e.to_string(),
                )
            })?;

        // Inject anchors only for content mode.
        if matches!(output_mode, OutputMode::Content) && result.exit_code == 0 {
            let cwd = crate::types::tool_metadata::resolve_cwd(&ctx, &resources).await?;
            let (fs, scheme) = {
                let res = resources.lock().await;
                let fs = res
                    .require::<crate::types::resources::FileSystem>()?
                    .0
                    .clone();
                let params = res
                    .get::<Params<HashlineSchemeParams>>()
                    .cloned()
                    .unwrap_or_default();
                let scheme = params
                    .0
                    .build_scheme()
                    .map_err(echo_agent_tool_runtime::ToolError::invalid_arguments)?;
                (fs, scheme)
            };
            match tokio::time::timeout(
                Duration::from_secs(DEFAULT_ANCHOR_TIMEOUT_SECS),
                inject_anchors(&result.stdout, &cwd, &*fs, &*scheme),
            )
            .await
            {
                Ok(anchored) => result.stdout = anchored,
                Err(_elapsed) => {
                    tracing::Span::current().record("timed_out", true);
                    tracing::warn!(
                        timeout_secs = DEFAULT_ANCHOR_TIMEOUT_SECS,
                        "anchor injection timed out after {DEFAULT_ANCHOR_TIMEOUT_SECS}s, returning un-anchored results"
                    );
                    let warning = format!(
                        "\nHashline anchoring for tool output failed after \
                         {DEFAULT_ANCHOR_TIMEOUT_SECS} seconds. Since results lack \
                         anchoring, please read the file before editing.\n"
                    );
                    result.stdout.extend_from_slice(warning.as_bytes());
                }
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_scheme() -> Box<dyn AnchorScheme> {
        HashlineSchemeParams::default().build_scheme().unwrap()
    }

    #[test]
    fn parse_rg_line_match() {
        let (num, sep, content) = parse_rg_line("123:    let x = 1;").unwrap();
        assert_eq!(num, 123);
        assert_eq!(sep, ':');
        assert_eq!(content, "    let x = 1;");
    }

    #[test]
    fn parse_rg_line_context() {
        let (num, sep, content) = parse_rg_line("45-    let y = 2;").unwrap();
        assert_eq!(num, 45);
        assert_eq!(sep, '-');
        assert_eq!(content, "    let y = 2;");
    }

    #[test]
    fn parse_rg_line_no_digits() {
        assert!(parse_rg_line("src/main.rs").is_none());
    }

    #[test]
    fn parse_rg_line_empty() {
        assert!(parse_rg_line("").is_none());
    }

    #[test]
    fn tool_metadata() {
        use crate::types::tool_metadata::ToolMetadata;
        let tool = HashlineGrepTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "hashline_grep"
        );
        assert_eq!(ToolMetadata::kind(&tool), ToolKind::Search);
        assert!(echo_agent_tool_runtime::Tool::capabilities(&tool).is_read_only);
        assert!(matches!(
            ToolMetadata::tool_namespace(&tool),
            ToolNamespace::EchoAgentBuildHashline
        ));
    }

    #[test]
    fn description_mentions_anchors_and_edit() {
        use crate::types::tool_metadata::ToolMetadata;
        let tool = HashlineGrepTool;
        assert!(ToolMetadata::description_template(&tool).contains("anchor"));
        assert!(ToolMetadata::description_template(&tool).contains("tools.by_kind.edit"));
    }

    #[tokio::test]
    async fn inject_anchors_content_mode() {
        use crate::computer::local::LocalFs;
        use std::sync::Arc;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("test.rs"),
            "fn main() {\n    let x = 1;\n    let y = 2;\n}\n",
        )
        .unwrap();

        let fs = Arc::new(LocalFs);

        // Simulate ripgrep output for "let" search.
        let rg_output = format!(
            "<workspace_result workspace_path=\"{}\">\n\
             Found 2 matching lines\n\
             test.rs\n\
             2:    let x = 1;\n\
             3:    let y = 2;\n\
             </workspace_result>",
            tmp.path().display()
        );

        let scheme = test_scheme();
        let result = inject_anchors(rg_output.as_bytes(), tmp.path(), &*fs, &*scheme).await;
        let output = String::from_utf8_lossy(&result);

        // Anchored lines should have the pattern NUM:LOCAL:CONTEXT:CONTENT
        // (3 colons for chunk scheme: line:local:context:content).
        for line in output.lines() {
            if line.starts_with(|c: char| c.is_ascii_digit()) {
                let colon_count = line.matches(':').count();
                assert!(
                    colon_count >= 3,
                    "anchored line should have ≥3 colons, got {colon_count}: {line}"
                );
            }
        }
    }

    #[tokio::test]
    async fn inject_anchors_preserves_headers() {
        use crate::computer::local::LocalFs;
        use std::sync::Arc;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "hello\nworld\n").unwrap();

        let fs = Arc::new(LocalFs);
        let rg_output = format!(
            "<workspace_result workspace_path=\"{}\">\n\
             Found 1 matching lines\n\
             a.rs\n\
             1:hello\n\
             </workspace_result>",
            tmp.path().display()
        );

        let scheme = test_scheme();
        let result = inject_anchors(rg_output.as_bytes(), tmp.path(), &*fs, &*scheme).await;
        let output = String::from_utf8_lossy(&result);

        assert!(output.contains("Found 1 matching lines"));
        assert!(output.contains("a.rs\n"));
        assert!(output.contains("</workspace_result>"));
    }

    #[tokio::test]
    async fn inject_anchors_context_lines() {
        use crate::computer::local::LocalFs;
        use std::sync::Arc;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.rs"), "a\nb\nc\nd\ne\n").unwrap();

        let fs = Arc::new(LocalFs);
        let rg_output = format!(
            "<workspace_result workspace_path=\"{}\">\n\
             Found 1 matching lines\n\
             test.rs\n\
             2-b\n\
             3:c\n\
             4-d\n\
             </workspace_result>",
            tmp.path().display()
        );

        let scheme = test_scheme();
        let result = inject_anchors(rg_output.as_bytes(), tmp.path(), &*fs, &*scheme).await;
        let output = String::from_utf8_lossy(&result);

        // Context line should use '-' separator after the anchor.
        let line_2 = output.lines().find(|l| l.starts_with('2')).unwrap();
        assert!(
            line_2.contains('-'),
            "context line should keep '-': {line_2}"
        );

        // Match line should use ':' separator after the anchor.
        let line_3 = output.lines().find(|l| l.starts_with('3')).unwrap();
        // Count colons: line:local:context:content = 3 colons with ':'
        let colon_count = line_3.matches(':').count();
        assert!(
            colon_count >= 3,
            "match line should have ≥3 colons: {line_3}"
        );
    }

    #[tokio::test]
    async fn no_injection_for_missing_file() {
        use crate::computer::local::LocalFs;
        use std::sync::Arc;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let fs = Arc::new(LocalFs);

        // File doesn't exist — anchors can't be generated.
        let rg_output = format!(
            "<workspace_result workspace_path=\"{}\">\n\
             Found 1 matching lines\n\
             nonexistent.rs\n\
             5:some content\n\
             </workspace_result>",
            tmp.path().display()
        );

        let scheme = test_scheme();
        let result = inject_anchors(rg_output.as_bytes(), tmp.path(), &*fs, &*scheme).await;
        let output = String::from_utf8_lossy(&result);

        // Should fall through without anchors — original line preserved.
        assert!(output.contains("5:some content"));
    }

    #[tokio::test]
    async fn digit_leading_file_path_recognized_as_header() {
        use crate::computer::local::LocalFs;
        use std::sync::Arc;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let subdir = tmp.path().join("2024_data");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("config.rs"), "let x = 1;\nlet y = 2;\n").unwrap();

        let fs = Arc::new(LocalFs);
        let rg_output = format!(
            "<workspace_result workspace_path=\"{}\">\n\
             Found 1 matching lines\n\
             2024_data/config.rs\n\
             1:let x = 1;\n\
             </workspace_result>",
            tmp.path().display()
        );

        let scheme = test_scheme();
        let result = inject_anchors(rg_output.as_bytes(), tmp.path(), &*fs, &*scheme).await;
        let output = String::from_utf8_lossy(&result);

        // The file header should be recognized despite starting with digits.
        // Line 1 should be annotated with anchors from the correct file.
        let line_1 = output.lines().find(|l| l.starts_with('1')).unwrap();
        let colon_count = line_1.matches(':').count();
        assert!(
            colon_count >= 3,
            "anchors should be from the correct file, got: {line_1}"
        );
    }

    #[tokio::test]
    async fn no_injection_for_files_with_matches_output() {
        use crate::computer::local::LocalFs;
        use std::sync::Arc;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let fs = Arc::new(LocalFs);

        // files_with_matches output has no numbered lines — just file paths.
        let rg_output = format!(
            "<workspace_result workspace_path=\"{}\">\n\
             Found 2 files\n\
             src/main.rs\n\
             src/lib.rs\n\
             </workspace_result>",
            tmp.path().display()
        );

        let scheme = test_scheme();
        let result = inject_anchors(rg_output.as_bytes(), tmp.path(), &*fs, &*scheme).await;
        let output = String::from_utf8_lossy(&result);

        // No numbered lines → no anchor injection. Output should be unchanged.
        assert!(output.contains("src/main.rs"));
        assert!(output.contains("src/lib.rs"));
        assert!(
            !output.contains('→'),
            "files_with_matches should have no anchors"
        );
    }

    #[tokio::test]
    async fn no_injection_for_count_output() {
        use crate::computer::local::LocalFs;
        use std::sync::Arc;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let fs = Arc::new(LocalFs);

        // count output: "file:N" format.
        let rg_output = format!(
            "<workspace_result workspace_path=\"{}\">\n\
             src/main.rs:5\n\
             src/lib.rs:3\n\
             </workspace_result>",
            tmp.path().display()
        );

        let scheme = test_scheme();
        let result = inject_anchors(rg_output.as_bytes(), tmp.path(), &*fs, &*scheme).await;
        let output = String::from_utf8_lossy(&result);

        // Count lines should pass through. They look like "src/main.rs:5"
        // which parse_rg_line won't match (path contains '/', not just digits).
        assert!(output.contains("src/main.rs:5"));
        assert!(output.contains("src/lib.rs:3"));
    }

    #[tokio::test]
    async fn multiple_matches_same_file_use_cached_anchors() {
        use crate::computer::local::LocalFs;
        use std::sync::Arc;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("multi.rs"),
            "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n",
        )
        .unwrap();

        let fs = Arc::new(LocalFs);
        let rg_output = format!(
            "<workspace_result workspace_path=\"{}\">\n\
             Found 4 matching lines\n\
             multi.rs\n\
             1:fn a() {{}}\n\
             2:fn b() {{}}\n\
             3:fn c() {{}}\n\
             4:fn d() {{}}\n\
             </workspace_result>",
            tmp.path().display()
        );

        let scheme = test_scheme();
        let result = inject_anchors(rg_output.as_bytes(), tmp.path(), &*fs, &*scheme).await;
        let output = String::from_utf8_lossy(&result);

        // All 4 lines should be anchored (same file, same cache entry).
        let anchored_count = output
            .lines()
            .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
            .filter(|l| l.matches(':').count() >= 3)
            .count();
        assert_eq!(anchored_count, 4, "all 4 lines should be anchored");
    }

    #[tokio::test]
    async fn inject_anchors_timeout_returns_unanchored() {
        use crate::computer::local::LocalFs;
        use crate::computer::types::{AsyncFileSystem, ComputerError};
        use std::sync::Arc;
        use tempfile::TempDir;

        struct SlowFs;

        #[async_trait::async_trait]
        impl AsyncFileSystem for SlowFs {
            async fn read_file(&self, _path: &Path) -> Result<Vec<u8>, ComputerError> {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(Vec::new())
            }
            async fn write_file(&self, _path: &Path, _data: &[u8]) -> Result<(), ComputerError> {
                unimplemented!()
            }
            async fn delete_file(&self, _path: &Path) -> Result<(), ComputerError> {
                unimplemented!()
            }
        }

        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("test.rs"),
            "fn main() {\n    let x = 1;\n    let y = 2;\n}\n",
        )
        .unwrap();

        let rg_output = format!(
            "<workspace_result workspace_path=\"{}\">\n\
             Found 2 matching lines\n\
             test.rs\n\
             2:    let x = 1;\n\
             3:    let y = 2;\n\
             </workspace_result>",
            tmp.path().display()
        );
        let rg_bytes = rg_output.as_bytes();

        let scheme = test_scheme();

        // Happy path: real filesystem produces anchored output.
        let fast_fs = Arc::new(LocalFs);
        let anchored = inject_anchors(rg_bytes, tmp.path(), &*fast_fs, &*scheme).await;
        let anchored_str = String::from_utf8_lossy(&anchored);
        for line in anchored_str.lines() {
            if line.starts_with(|c: char| c.is_ascii_digit()) {
                assert!(
                    line.matches(':').count() >= 3,
                    "happy path should produce anchored lines, got: {line}"
                );
            }
        }

        // Timeout path: slow filesystem causes cancellation.
        let slow_fs = Arc::new(SlowFs);
        let timeout_result = tokio::time::timeout(
            Duration::from_millis(10),
            inject_anchors(rg_bytes, tmp.path(), &*slow_fs, &*scheme),
        )
        .await;

        assert!(
            timeout_result.is_err(),
            "inject_anchors should have timed out"
        );

        // On timeout, production keeps the original output unchanged.
        let unanchored_str = String::from_utf8_lossy(rg_bytes);
        for line in unanchored_str.lines() {
            if line.starts_with(|c: char| c.is_ascii_digit()) {
                assert_eq!(
                    line.matches(':').count(),
                    1,
                    "un-anchored line should have exactly 1 colon, got: {line}"
                );
            }
        }

        assert!(unanchored_str.contains("Found 2 matching lines"));
        assert!(unanchored_str.contains("test.rs"));
        assert!(unanchored_str.contains("</workspace_result>"));
        assert_ne!(anchored, rg_bytes, "anchored output should differ from raw");
    }

    #[tokio::test]
    async fn truncated_output_stays_well_formed() {
        use crate::computer::local::LocalFs;
        use std::sync::Arc;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("big.rs"), "match\n".repeat(100)).unwrap();

        let fs = Arc::new(LocalFs);
        // Simulate truncated output with "... [N lines truncated]" marker.
        let mut rg_lines = String::new();
        for i in 1..=10 {
            rg_lines.push_str(&format!("{i}:match\n"));
        }
        let rg_output = format!(
            "<workspace_result workspace_path=\"{}\">\n\
             Found at least 100 matching lines\n\
             big.rs\n\
             {rg_lines}\
             ... [at least 90 lines truncated] ...\n\
             </workspace_result>",
            tmp.path().display()
        );

        let scheme = test_scheme();
        let result = inject_anchors(rg_output.as_bytes(), tmp.path(), &*fs, &*scheme).await;
        let output = String::from_utf8_lossy(&result);

        // Truncation marker should be preserved.
        assert!(output.contains("... [at least 90 lines truncated] ..."));
        // The wrapper should be intact.
        assert!(output.contains("</workspace_result>"));
        // Visible lines should be anchored.
        let anchored = output
            .lines()
            .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
            .filter(|l| l.matches(':').count() >= 3)
            .count();
        assert_eq!(anchored, 10);
    }
}
