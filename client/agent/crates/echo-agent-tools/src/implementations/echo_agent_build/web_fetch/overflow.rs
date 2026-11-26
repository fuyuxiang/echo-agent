//! Bounded inline previews with recoverable session artifacts and tool-aware hints.

use std::path::{Path, PathBuf};

use super::artifact::WebFetchArtifactWriter;
use crate::util::query_tools::{QueryTools, examples_clause};
use crate::util::truncate;

const WEB_FETCH_CONTEXT_PERCENT: f64 = 0.03;
const RECOVERY_FOOTER_PREFIX: &str = "\n\n[web_fetch content truncated:";
const LONG_LINE_BYTES: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayloadFormat {
    Markdown,
    Json,
    JsonLines,
    Text,
}

impl PayloadFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
            Self::JsonLines => "jsonl",
            Self::Text => "txt",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PayloadClassification {
    format: PayloadFormat,
    has_long_line: bool,
}

impl PayloadClassification {
    fn classify(content_type: &str, text: &str) -> Self {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim()
            .to_ascii_lowercase();
        let format = if mime == "markdown" || mime == "text/markdown" {
            PayloadFormat::Markdown
        } else if matches!(
            mime.as_str(),
            "application/x-ndjson"
                | "application/ndjson"
                | "application/jsonl"
                | "text/jsonl"
                | "text/x-jsonl"
        ) {
            PayloadFormat::JsonLines
        } else if mime == "application/json"
            || mime == "text/json"
            || mime.ends_with("+json")
            || serde_json::from_str::<serde::de::IgnoredAny>(text.trim()).is_ok()
        {
            PayloadFormat::Json
        } else {
            PayloadFormat::Text
        };
        let has_long_line = text.lines().map(str::len).max().unwrap_or(0) > LONG_LINE_BYTES;
        Self {
            format,
            has_long_line,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct OverflowHandler {
    writer: WebFetchArtifactWriter,
}

#[derive(Clone, Copy)]
pub(super) struct RecoveryTools<'a> {
    pub(super) read: Option<&'a str>,
    pub(super) execute: Option<&'a str>,
}

pub(super) struct OverflowResult {
    pub(super) content: String,
    pub(super) was_truncated: bool,
    pub(super) artifact_path: Option<PathBuf>,
    pub(super) path_free_content: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) struct InlineBudget {
    preview_bytes: usize,
    output_bytes: usize,
}

impl OverflowHandler {
    pub(super) fn new() -> Self {
        Self {
            writer: WebFetchArtifactWriter,
        }
    }

    pub(super) async fn process(
        &self,
        content: String,
        budget: InlineBudget,
        session_folder: Option<&Path>,
        content_type: &str,
        tools: RecoveryTools<'_>,
    ) -> OverflowResult {
        if content.len() <= budget.preview_bytes.min(budget.output_bytes) {
            return OverflowResult {
                content,
                was_truncated: false,
                artifact_path: None,
                path_free_content: None,
            };
        }

        let total_bytes = content.len();
        let classification = PayloadClassification::classify(content_type, &content);
        let extension = classification.format.extension();
        let saved_path = match session_folder {
            Some(folder) => match self
                .writer
                .save(folder, content.as_bytes(), extension)
                .await
            {
                Ok(path) => Some(path),
                Err(error) => {
                    tracing::warn!(
                        session_folder = %folder.display(),
                        total_bytes,
                        error = %error,
                        "Failed to persist full web_fetch content"
                    );
                    None
                }
            },
            None => {
                tracing::warn!(
                    total_bytes,
                    "Cannot persist full web_fetch content without a session folder"
                );
                None
            }
        };

        let output = bounded_output(
            &content,
            budget,
            total_bytes,
            saved_path.as_deref(),
            classification,
            tools,
        );
        let path_free_content =
            bounded_output(&content, budget, total_bytes, None, classification, tools);
        OverflowResult {
            content: output,
            was_truncated: true,
            artifact_path: saved_path,
            path_free_content: Some(path_free_content),
        }
    }
}

pub(super) fn inline_budget(
    context_window_tokens: u64,
    max_markdown_length: usize,
) -> InlineBudget {
    let context_budget = (truncate::estimate_chars(context_window_tokens) as f64
        * WEB_FETCH_CONTEXT_PERCENT) as usize;
    InlineBudget {
        preview_bytes: context_budget.min(max_markdown_length),
        output_bytes: max_markdown_length,
    }
}

fn recovery_footer(shown_bytes: usize, total_bytes: usize, file_hint: &str) -> String {
    format!(
        "{RECOVERY_FOOTER_PREFIX} showing first {shown_bytes} of \
         {total_bytes} bytes.{file_hint}]"
    )
}

fn bounded_output(
    content: &str,
    budget: InlineBudget,
    total_bytes: usize,
    saved_path: Option<&Path>,
    classification: PayloadClassification,
    tools: RecoveryTools<'_>,
) -> String {
    if let Some(path) = saved_path {
        let path_hint = format!(" Full content saved to: {}.", path.display());
        let steer = web_fetch_steer(classification, tools, QueryTools::detect());
        if !steer.is_empty() {
            let full_hint = format!("{path_hint}{steer}");
            if let Some(output) = render_with_hint(content, budget, total_bytes, &full_hint) {
                return output;
            }
        }
        if let Some(output) = render_with_hint(content, budget, total_bytes, &path_hint) {
            return output;
        }
    } else if let Some(output) = render_with_hint(content, budget, total_bytes, "") {
        return output;
    }
    bounded_generic_marker(content, budget)
}

fn render_with_hint(
    content: &str,
    budget: InlineBudget,
    total_bytes: usize,
    file_hint: &str,
) -> Option<String> {
    let provisional_footer = recovery_footer(budget.preview_bytes, total_bytes, file_hint);
    if provisional_footer.len() > budget.output_bytes {
        return None;
    }
    let preview_bytes = budget
        .preview_bytes
        .min(budget.output_bytes - provisional_footer.len());
    let preview = truncate::truncate_str(content, preview_bytes);
    let footer = recovery_footer(preview.len(), total_bytes, file_hint);
    let output = format!("{preview}{footer}");
    (output.len() <= budget.output_bytes).then_some(output)
}

fn bounded_generic_marker(content: &str, budget: InlineBudget) -> String {
    let Some(marker) = ["\n\n[web_fetch output truncated]", "[truncated]", "..."]
        .into_iter()
        .find(|marker| marker.len() <= budget.output_bytes)
    else {
        return String::new();
    };
    let preview_bytes = budget.preview_bytes.min(budget.output_bytes - marker.len());
    format!("{}{marker}", truncate::truncate_str(content, preview_bytes))
}

fn read_steer(read_tool: Option<&str>) -> String {
    read_tool.map_or_else(String::new, |read_tool| {
        format!(" Use `{read_tool}` with offsets and limits to read it in chunks.")
    })
}

fn web_fetch_steer(
    classification: PayloadClassification,
    tools: RecoveryTools<'_>,
    query_tools: QueryTools,
) -> String {
    if classification.has_long_line {
        return tools.execute.map_or_else(String::new, |execute| {
            let (format, action, examples) = match classification.format {
                PayloadFormat::Json => ("valid JSON", "query", query_tools.json_tools()),
                PayloadFormat::JsonLines => ("JSON Lines", "query", query_tools.json_tools()),
                PayloadFormat::Markdown => {
                    ("Markdown", "slice or search", query_tools.text_tools())
                }
                PayloadFormat::Text => ("text", "slice or search", query_tools.text_tools()),
            };
            format!(
                " The saved file is {format} with a very long line, so \
                 line-oriented read and search tools are ineffective on it — use \
                 `{execute}` to {action} it{eg}.",
                eg = examples_clause(&examples),
            )
        });
    }

    match classification.format {
        PayloadFormat::Json => tools.execute.map_or_else(
            || read_steer(tools.read),
            |execute| {
                format!(
                    " The saved file is valid JSON; use `{execute}` to query it{eg}.",
                    eg = examples_clause(&query_tools.json_tools()),
                )
            },
        ),
        PayloadFormat::JsonLines => tools.execute.map_or_else(
            || read_steer(tools.read),
            |execute| {
                format!(
                    " The saved file is JSON Lines; use `{execute}` to query it{eg}.",
                    eg = examples_clause(&query_tools.json_tools()),
                )
            },
        ),
        PayloadFormat::Markdown | PayloadFormat::Text => {
            let read = read_steer(tools.read);
            if !read.is_empty() {
                return read;
            }
            tools.execute.map_or_else(String::new, |execute| {
                format!(
                    " Use `{execute}` to inspect it{}.",
                    examples_clause(&query_tools.text_tools())
                )
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_query_tools() -> QueryTools {
        QueryTools {
            jq: Some("jq"),
            python: Some("python3"),
            sed: Some("sed"),
            cut: Some("cut"),
        }
    }

    fn tools<'a>(read: Option<&'a str>, execute: Option<&'a str>) -> RecoveryTools<'a> {
        RecoveryTools { read, execute }
    }

    fn budget(preview_bytes: usize) -> InlineBudget {
        InlineBudget {
            preview_bytes,
            output_bytes: 512,
        }
    }

    #[tokio::test]
    async fn oversized_markdown_persists_exact_content_and_omitted_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let handler = OverflowHandler::new();
        let tail = "TAIL-MUST-REMAIN-RECOVERABLE";
        let full = format!("# Heading\n{}\n{tail}", "body line\n".repeat(1_000));

        let result = handler
            .process(
                full.clone(),
                budget(100),
                Some(tmp.path()),
                "markdown",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;

        assert!(result.was_truncated);
        assert!(result.content.len() <= 512);
        assert!(!result.content.contains(tail));
        assert_eq!(
            result
                .content
                .matches("web_fetch content truncated")
                .count(),
            1
        );
        assert!(result.content.contains("showing first 100 of"));
        assert!(result.content.contains("ReadAsset"));
        let dump = tmp.path().join("web_fetch/1.md");
        assert!(result.content.contains(dump.to_string_lossy().as_ref()));
        assert_eq!(tokio::fs::read_to_string(dump).await.unwrap(), full);
    }

    #[tokio::test]
    async fn exact_limit_stays_inline_and_one_over_is_recoverable() {
        let tmp = tempfile::tempdir().unwrap();
        let handler = OverflowHandler::new();

        let exact = handler
            .process(
                "a".repeat(100),
                budget(100),
                Some(tmp.path()),
                "text/plain",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;
        assert!(!exact.was_truncated);
        assert_eq!(exact.content, "a".repeat(100));
        assert!(!tmp.path().join("web_fetch").exists());

        let one_over = handler
            .process(
                "b".repeat(101),
                budget(100),
                Some(tmp.path()),
                "text/plain",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;
        assert!(one_over.was_truncated);
        assert!(one_over.content.len() <= 512);
        assert!(one_over.content.contains("showing first 100 of 101 bytes"));
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("web_fetch/1.txt"))
                .await
                .unwrap(),
            "b".repeat(101)
        );
    }

    #[tokio::test]
    async fn utf8_preview_uses_one_safe_byte_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let result = OverflowHandler::new()
            .process(
                "ééé".to_string(),
                budget(5),
                Some(tmp.path()),
                "text/plain",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;

        assert!(result.was_truncated);
        assert!(result.content.starts_with("éé\n\n[web_fetch"));
        assert!(result.content.contains("showing first 4 of 6 bytes"));
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("web_fetch/1.txt"))
                .await
                .unwrap(),
            "ééé"
        );
    }

    #[test]
    fn classification_uses_mime_and_keeps_layout_independent() {
        let cases = [
            ("markdown", "plain", PayloadFormat::Markdown, "md"),
            (
                "text/markdown; charset=utf-8",
                "plain",
                PayloadFormat::Markdown,
                "md",
            ),
            ("application/json", "not valid", PayloadFormat::Json, "json"),
            ("text/json", "not valid", PayloadFormat::Json, "json"),
            (
                "application/problem+json",
                "not valid",
                PayloadFormat::Json,
                "json",
            ),
            (
                "application/x-ndjson",
                "{\"a\":1}\n",
                PayloadFormat::JsonLines,
                "jsonl",
            ),
            (
                "application/ndjson",
                "{\"a\":1}\n",
                PayloadFormat::JsonLines,
                "jsonl",
            ),
            (
                "application/jsonl",
                "{\"a\":1}\n",
                PayloadFormat::JsonLines,
                "jsonl",
            ),
            (
                "text/jsonl",
                "{\"a\":1}\n",
                PayloadFormat::JsonLines,
                "jsonl",
            ),
            (
                "text/x-jsonl",
                "{\"a\":1}\n",
                PayloadFormat::JsonLines,
                "jsonl",
            ),
            ("text/plain", "{\"a\":1}", PayloadFormat::Json, "json"),
            ("text/plain", "plain", PayloadFormat::Text, "txt"),
        ];
        for (content_type, content, format, extension) in cases {
            let classification = PayloadClassification::classify(content_type, content);
            assert_eq!(classification.format, format);
            assert_eq!(classification.format.extension(), extension);
        }

        let long_markdown =
            PayloadClassification::classify("markdown", &"x".repeat(LONG_LINE_BYTES + 1));
        assert_eq!(long_markdown.format, PayloadFormat::Markdown);
        assert!(long_markdown.has_long_line);

        let normal_json_lines =
            PayloadClassification::classify("application/x-ndjson", "{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(normal_json_lines.format, PayloadFormat::JsonLines);
        assert!(!normal_json_lines.has_long_line);
    }

    #[tokio::test]
    async fn json_json_lines_and_text_use_matching_extensions() {
        let tmp = tempfile::tempdir().unwrap();
        let handler = OverflowHandler::new();
        let json = r#"{"items":["one","two","three"]}"#.to_string();
        let json_lines = "{\"item\":1}\n{\"item\":2}\n".to_string();
        let text = "line one\nline two\nline three".to_string();

        handler
            .process(
                json.clone(),
                budget(5),
                Some(tmp.path()),
                "application/json",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;
        handler
            .process(
                json_lines.clone(),
                budget(5),
                Some(tmp.path()),
                "application/x-ndjson",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;
        handler
            .process(
                text.clone(),
                budget(5),
                Some(tmp.path()),
                "text/plain",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;

        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("web_fetch/1.json"))
                .await
                .unwrap(),
            json
        );
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("web_fetch/2.jsonl"))
                .await
                .unwrap(),
            json_lines
        );
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("web_fetch/3.txt"))
                .await
                .unwrap(),
            text
        );
    }

    #[test]
    fn steering_prefers_execute_but_pretty_json_falls_back_to_read() {
        let pretty_json = PayloadClassification {
            format: PayloadFormat::Json,
            has_long_line: false,
        };
        let execute = web_fetch_steer(
            pretty_json,
            tools(Some("ReadAsset"), Some("ExecuteAsset")),
            all_query_tools(),
        );
        assert!(execute.contains("ExecuteAsset"));
        assert!(!execute.contains("ReadAsset"));

        let read = web_fetch_steer(
            pretty_json,
            tools(Some("ReadAsset"), None),
            all_query_tools(),
        );
        assert!(read.contains("ReadAsset"));
        assert!(read.contains("offsets and limits"));

        let json_lines = web_fetch_steer(
            PayloadClassification {
                format: PayloadFormat::JsonLines,
                has_long_line: false,
            },
            tools(Some("ReadAsset"), None),
            all_query_tools(),
        );
        assert!(json_lines.contains("ReadAsset"));

        for format in [PayloadFormat::Json, PayloadFormat::Markdown] {
            assert!(
                web_fetch_steer(
                    PayloadClassification {
                        format,
                        has_long_line: true,
                    },
                    tools(Some("ReadAsset"), None),
                    all_query_tools(),
                )
                .is_empty()
            );
        }
    }

    #[test]
    fn steering_names_only_detected_query_utilities() {
        let detected = QueryTools {
            jq: None,
            python: Some("python3"),
            sed: None,
            cut: None,
        };
        let steer = web_fetch_steer(
            PayloadClassification {
                format: PayloadFormat::Json,
                has_long_line: true,
            },
            tools(Some("ReadAsset"), Some("ExecuteAsset")),
            detected,
        );

        assert!(steer.contains("ExecuteAsset"));
        assert!(steer.contains("python3"));
        assert!(!steer.contains("ReadAsset"));
        assert!(!steer.contains("jq"));

        let markdown = web_fetch_steer(
            PayloadClassification {
                format: PayloadFormat::Markdown,
                has_long_line: true,
            },
            tools(Some("ReadAsset"), Some("ExecuteAsset")),
            all_query_tools(),
        );
        assert!(markdown.contains("ExecuteAsset"));
        assert!(markdown.contains("sed"));
        assert!(!markdown.contains("ReadAsset"));
        assert!(!markdown.contains("jq"));
    }

    #[tokio::test]
    async fn missing_or_failed_session_storage_never_claims_an_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let handler = OverflowHandler::new();
        let full = "line\n".repeat(1_000);
        let no_session = handler
            .process(
                full.clone(),
                budget(100),
                None,
                "text/plain",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;
        assert!(no_session.was_truncated);
        assert!(!no_session.content.contains("Full content saved"));
        assert!(!no_session.content.contains("ReadAsset"));

        let blocker = tmp.path().join("not-a-directory");
        tokio::fs::write(&blocker, b"file").await.unwrap();
        let failed = handler
            .process(
                full.clone(),
                budget(100),
                Some(&blocker),
                "text/plain",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;
        assert!(failed.was_truncated);
        assert!(!failed.content.contains("Full content saved"));
        assert!(!failed.content.contains("ReadAsset"));

        tokio::fs::remove_file(&blocker).await.unwrap();
        let retry = handler
            .process(
                full.clone(),
                budget(100),
                Some(&blocker),
                "text/plain",
                tools(Some("RetryRead"), Some("ExecuteAsset")),
            )
            .await;
        assert!(retry.content.contains("RetryRead"));
        assert!(retry.content.contains("Full content saved"));
        assert_eq!(
            tokio::fs::read_to_string(blocker.join("web_fetch/1.txt"))
                .await
                .unwrap(),
            full
        );
    }

    #[tokio::test]
    async fn deleted_artifact_and_changed_tool_name_are_rematerialized() {
        let tmp = tempfile::tempdir().unwrap();
        let handler = OverflowHandler::new();
        let full = "line\n".repeat(1_000);
        let first = handler
            .process(
                full.clone(),
                budget(100),
                Some(tmp.path()),
                "text/plain",
                tools(Some("FirstRead"), Some("ExecuteAsset")),
            )
            .await;
        let first_path = tmp.path().join("web_fetch/1.txt");
        assert!(first.content.contains("FirstRead"));
        tokio::fs::remove_file(first_path).await.unwrap();

        let second = handler
            .process(
                full.clone(),
                budget(100),
                Some(tmp.path()),
                "text/plain",
                tools(Some("SecondRead"), Some("ExecuteAsset")),
            )
            .await;
        let second_path = tmp.path().join("web_fetch/2.txt");
        assert!(second.content.contains("SecondRead"));
        assert!(!second.content.contains("FirstRead"));
        assert_eq!(tokio::fs::read_to_string(second_path).await.unwrap(), full);
    }

    #[tokio::test]
    async fn one_million_token_context_keeps_footer_inside_output_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let budget = inline_budget(1_000_000, 100_000);
        assert_eq!(budget.preview_bytes, 100_000);
        assert_eq!(budget.output_bytes, 100_000);
        assert_eq!(inline_budget(1_000_000, 20_000).preview_bytes, 20_000);
        let result = OverflowHandler::new()
            .process(
                "line\n".repeat(21_000),
                budget,
                Some(tmp.path()),
                "text/plain",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;

        assert!(result.was_truncated);
        assert!(result.content.len() <= 100_000);
        assert!(result.content.contains("Full content saved to:"));
        assert!(result.content.contains("ReadAsset"));
        let footer_start = result.content.rfind(RECOVERY_FOOTER_PREFIX).unwrap();
        assert!(result.content[footer_start..].starts_with(&format!(
            "{RECOVERY_FOOTER_PREFIX} showing first {footer_start} of "
        )));
    }

    #[tokio::test]
    async fn oversized_tool_name_drops_guidance_but_keeps_actionable_path() {
        let tmp = tempfile::tempdir().unwrap();
        let read_tool = "ReadAsset".repeat(200);
        let result = OverflowHandler::new()
            .process(
                "line\n".repeat(1_000),
                InlineBudget {
                    preview_bytes: 100,
                    output_bytes: 300,
                },
                Some(tmp.path()),
                "text/plain",
                tools(Some(&read_tool), Some("ExecuteAsset")),
            )
            .await;

        let artifact = result.artifact_path.unwrap();
        assert!(result.content.len() <= 300);
        assert!(result.content.contains(artifact.to_string_lossy().as_ref()));
        assert!(!result.content.contains(read_tool.as_str()));
        assert!(tokio::fs::try_exists(artifact).await.unwrap());
    }

    #[tokio::test]
    async fn oversized_path_falls_back_to_bounded_generic_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("p".repeat(180));
        let result = OverflowHandler::new()
            .process(
                "line\n".repeat(1_000),
                InlineBudget {
                    preview_bytes: 100,
                    output_bytes: 128,
                },
                Some(&session),
                "text/plain",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;

        assert!(result.content.len() <= 128);
        assert!(result.content.contains("truncated"));
        assert!(!result.content.contains("Full content saved"));
        assert!(
            tokio::fs::try_exists(result.artifact_path.unwrap())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn tiny_budget_returns_only_bounded_generic_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let result = OverflowHandler::new()
            .process(
                "short".to_string(),
                InlineBudget {
                    preview_bytes: 10,
                    output_bytes: 3,
                },
                Some(tmp.path()),
                "text/plain",
                tools(Some("ReadAsset"), Some("ExecuteAsset")),
            )
            .await;

        assert_eq!(result.content, "...");
        assert!(
            tokio::fs::try_exists(result.artifact_path.unwrap())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn budgets_below_marker_width_return_empty_output() {
        let handler = OverflowHandler::new();
        for output_bytes in 0..=2 {
            let result = handler
                .process(
                    "short".to_string(),
                    InlineBudget {
                        preview_bytes: 10,
                        output_bytes,
                    },
                    None,
                    "text/plain",
                    tools(Some("ReadAsset"), Some("ExecuteAsset")),
                )
                .await;

            assert!(result.was_truncated);
            assert!(result.content.is_empty());
        }
    }
}
