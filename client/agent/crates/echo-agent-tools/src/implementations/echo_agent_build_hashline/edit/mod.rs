//! `hashline_edit` — anchor-based file editing.
//!
//! Supports `replace`, `insert_after`, and `write` operations. Anchors are
//! validated against the pre-edit file snapshot; edits are applied bottom-up
//! to avoid line-shift interference. Returns fresh-anchor snippets on success
//! and structured error context on validation failures.

pub mod apply;
pub mod range_policy;
pub mod types;

pub use types::{HashlineEditInput, HashlineEditOutput, HashlineOp};

use super::config::HashlineSchemeParams;

use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::{
    Cwd, DisplayCwd, FileSystem, Params, PathNotFoundHints, display_cwd_or_cwd,
};
use crate::types::tool::{ToolKind, ToolNamespace};

use crate::types::resources::resolve_model_path;
use crate::util::format_not_found_error;

const DESCRIPTION: &str = r#"Edit a file using anchors${%- if tools.by_kind.read and tools.by_kind.search %} from ${{ tools.by_kind.read }} or ${{ tools.by_kind.search }}${%- elif tools.by_kind.read %} from ${{ tools.by_kind.read }}${%- elif tools.by_kind.search %} from ${{ tools.by_kind.search }}${%- endif %}.

Operations (use the "op" field):

  "replace" — Replace one line or a range with new content.
    { "op": "replace", "anchor": "{example_anchor}", "content": "    let x = 42;" }
    Range: add "end_anchor" to replace from anchor through end_anchor (INCLUSIVE —
    both the anchor line and end_anchor line are replaced along with everything
    between them). If the anchor or end_anchor line contains a closing delimiter
    like `}` that must be preserved, include it in "content".
    Delete one line: { "op": "replace", "anchor": "{example_anchor}", "content": "" }
    Delete a range: { "op": "replace", "anchor": "{example_anchor}", "end_anchor": "...", "content": "" }

  "insert_after" — Insert new lines after the anchored line.
    { "op": "insert_after", "anchor": "{example_anchor}", "content": "    let y = 1;" }
    Add a blank line: { "op": "insert_after", "anchor": "{example_anchor}", "content": "" }
    Multi-line insert: content with newlines adds multiple lines.
    Beginning of file: use "0:" as anchor.
    End of file: use "EOF" as anchor.
    Existing lines below the anchor are preserved — only include new content.
    Prefer insert_after over replace when adding lines without removing existing ones.

  "write" — Replace entire file content (no anchors needed).
    { "op": "write", "content": "full file content here" }

Batch edits: pass multiple operations in "${{ params.edit.edits }}". They are validated against the
pre-edit snapshot and applied atomically bottom-up — if any anchor fails
validation, ALL edits in the batch are rejected (none are applied).
Overlapping ranges are also rejected.

Range safety:
- Multi-line edits may return caution warnings, especially for broader rewrites.
- Larger rewrites are allowed, but use them when you are confident about the target range.
- For very large rewrites (most of the file), prefer a single "write" op over many replace ops.

Follow-up edits:
- On success, the tool returns a snippet with fresh anchors around the edited region.
- On stale-anchor errors, the tool returns fresh anchors around the target line.
  Use these anchors to immediately retry your edit — do not re-read the file.
- The anchor is the full "LINE:HASH" or "LINE:HASH:HASH" before the → separator
  (e.g. "{example_anchor}"). Always include the line number. Do NOT include → or
  the line content after it.
- Never fabricate or modify anchors — only use exact anchors as returned by
  previous tool outputs."#;

/// `hashline_edit` tool — edits files using anchor references.
#[derive(Debug, Default)]
pub struct HashlineEditTool;

impl HashlineEditTool {
    /// Build a `FileNotFound` output with enriched path hints (if enabled).
    async fn file_not_found(
        display_path: &std::path::Path,
        joined_path: &std::path::Path,
        cwd: &std::path::Path,
        display_dcwd: &std::path::Path,
        hints_enabled: bool,
    ) -> crate::types::output::SearchReplaceOutput {
        let msg =
            format_not_found_error(display_path, joined_path, cwd, display_dcwd, hints_enabled)
                .await;
        crate::types::output::SearchReplaceOutput::FileNotFound(msg)
    }
}

fn to_search_replace(
    result: HashlineEditOutput,
    file_path: &std::path::Path,
    old_content: &str,
    new_content: Option<&str>,
    edit_details: Vec<apply::EditRegionDetail>,
) -> crate::types::output::SearchReplaceOutput {
    use crate::types::output::{
        SearchReplaceEditContextInformation, SearchReplaceEditsApplied, SearchReplaceOutput,
    };
    match result {
        HashlineEditOutput::EditsApplied(applied) => {
            let new_text = new_content.unwrap_or("");
            let details: Vec<_> = if edit_details.is_empty() {
                // Write op: produce a single whole-file detail so the
                // TUI can build a proper diff.
                vec![crate::types::output::SearchReplaceEditDetail {
                    old_string: old_content.to_owned(),
                    old_line: 1,
                    new_string: new_text.to_owned(),
                    new_line: 1,
                    context_before: String::new(),
                    context_after: String::new(),
                    line_prefix: String::new(),
                }]
            } else {
                let old_lines: Vec<&str> = old_content.split('\n').collect();
                edit_details
                    .into_iter()
                    .map(|d| {
                        let ctx_count = 3;
                        let old_idx = d.old_line.saturating_sub(1); // 0-based

                        // Lines before the edit in the old file.
                        let before_start = old_idx.saturating_sub(ctx_count);
                        let context_before = if before_start < old_idx {
                            let mut cb = old_lines[before_start..old_idx].join("\n");
                            cb.push('\n');
                            cb
                        } else {
                            String::new()
                        };

                        // Lines after the edit in the old file.
                        let old_text_line_count = if d.old_text.is_empty() {
                            0
                        } else {
                            d.old_text.split('\n').count()
                        };
                        let after_start = old_idx + old_text_line_count;
                        let after_end = (after_start + ctx_count).min(old_lines.len());
                        let context_after = if after_start < after_end {
                            let mut ca = old_lines[after_start..after_end].join("\n");
                            ca.push('\n');
                            ca
                        } else {
                            String::new()
                        };

                        crate::types::output::SearchReplaceEditDetail {
                            old_string: d.old_text,
                            old_line: d.old_line,
                            new_string: d.new_text,
                            new_line: d.new_line,
                            context_before,
                            context_after,
                            line_prefix: String::new(),
                        }
                    })
                    .collect()
            };
            let snippet_with_warnings = if applied.warnings.is_empty() {
                applied.snippet
            } else {
                format!("{}\n\n{}", applied.warnings.join("\n"), applied.snippet,)
            };
            SearchReplaceOutput::EditsApplied(SearchReplaceEditsApplied {
                old_string: old_content.to_owned(),
                new_string: new_text.to_owned(),
                tool_output_for_prompt: snippet_with_warnings,
                tool_output_for_prompt_concise: None,
                absolute_path: applied.absolute_path,
                edits: SearchReplaceEditContextInformation { details },
                patch: None,
                unicode_normalized: false,
            })
        }
        HashlineEditOutput::Error(err) => {
            let mut msg = err.message;
            if let Some(ctx) = err.context {
                let label = match err.context_start_line {
                    Some(start) => {
                        format!("Fresh anchors around line {start} — use these to retry your edit:")
                    }
                    None => "Fresh anchors — use these to retry your edit:".to_owned(),
                };
                msg.push_str("\n\n");
                msg.push_str(&label);
                msg.push('\n');
                msg.push_str(&ctx);
            }
            if let Some(anchor) = err.shifted_anchor {
                msg.push_str(&format!("\n\nSuggested anchor: {anchor}"));
            }
            match err.error {
                types::HashlineEditErrorKind::FileNotFound => {
                    SearchReplaceOutput::FileNotFound(msg)
                }
                types::HashlineEditErrorKind::InvalidInput => {
                    SearchReplaceOutput::InvalidInput(msg)
                }
                _ => {
                    SearchReplaceOutput::NoMatchesFound(crate::types::output::NoMatchesFoundError {
                        message: msg,
                        file_path: file_path.to_path_buf(),
                        file_snapshot_at_edit: None,
                    })
                }
            }
        }
    }
}

impl crate::types::tool_metadata::ToolMetadata for HashlineEditTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
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

impl echo_agent_tool_runtime::Tool for HashlineEditTool {
    type Args = HashlineEditInput;
    type Output = crate::types::output::SearchReplaceOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("hashline_edit").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "hashline_edit",
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
        name = "tool.hashline_edit",
        skip_all,
        fields(path = %input.file_path)
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: HashlineEditInput,
    ) -> Result<crate::types::output::SearchReplaceOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        if input.edits.is_empty() {
            return Ok(crate::types::output::SearchReplaceOutput::InvalidInput(
                "No edit operations provided.".to_owned(),
            ));
        }

        let (cwd, display_cwd, fs, scheme, hints_enabled) = {
            let res = resources.lock().await;
            let cwd = match ctx.extensions.get::<echo_agent_tool_runtime::Cwd>() {
                Some(dir) => dir.0.clone(),
                None => res.require::<Cwd>()?.0.clone(),
            };
            let display_cwd = res.get::<DisplayCwd>().map(|d| d.0.clone());
            let params = res
                .get::<Params<HashlineSchemeParams>>()
                .cloned()
                .unwrap_or_default();
            let fs = res.require::<FileSystem>()?.0.clone();
            let scheme = params
                .0
                .build_scheme()
                .map_err(echo_agent_tool_runtime::ToolError::invalid_arguments)?;
            let hints_enabled = res.get::<PathNotFoundHints>().is_some_and(|h| h.0);
            (cwd, display_cwd, fs, scheme, hints_enabled)
        };

        let display_dcwd = display_cwd_or_cwd(&cwd, display_cwd.as_deref());
        let joined_path = resolve_model_path(&cwd, display_cwd.as_deref(), &input.file_path);
        // Error-preserving variant: the Err arm drives new-file creation.
        let path = match crate::util::fs::try_canonicalize(&joined_path).await {
            Ok(p) => p,
            Err(_) => {
                // Try unicode-confusable resolution before giving up.
                // Used in search_replace.
                let resolved = crate::util::try_resolve_unicode_filename(&joined_path).await;
                if let Some(m) = resolved {
                    m.resolved_path
                } else {
                    // For Write ops on new files, allow creation.
                    if input.edits.len() == 1
                        && let HashlineOp::Write { ref content } = input.edits[0]
                    {
                        if let Err(e) = fs.write_file(&joined_path, content.as_bytes()).await {
                            let display_path = display_dcwd.join(&input.file_path);
                            return Ok(match e.io_error_kind() {
                                Some(std::io::ErrorKind::NotFound) => {
                                    Self::file_not_found(
                                        &display_path,
                                        &joined_path,
                                        &cwd,
                                        &display_dcwd,
                                        hints_enabled,
                                    )
                                    .await
                                }
                                _ => crate::types::output::SearchReplaceOutput::InvalidInput(
                                    format!("Failed to write file: {e}"),
                                ),
                            });
                        }
                        let abs = crate::util::fs::canonicalize_with_timeout(joined_path).await;
                        let r = apply::apply_edits(content, &input.edits, &abs, &*scheme);
                        let edit_details = r.edit_details;
                        return Ok(to_search_replace(
                            r.output,
                            &abs,
                            "",
                            r.new_content.as_deref(),
                            edit_details,
                        ));
                    }

                    let display_path = display_dcwd.join(&input.file_path);
                    return Ok(Self::file_not_found(
                        &display_path,
                        &joined_path,
                        &cwd,
                        &display_dcwd,
                        hints_enabled,
                    )
                    .await);
                }
            }
        };

        // Read current file content.
        let file_bytes = match fs.read_file(&path).await {
            Ok(b) => b,
            Err(e) => {
                let display_path = display_dcwd.join(&input.file_path);
                return Ok(match e.io_error_kind() {
                    Some(std::io::ErrorKind::NotFound) => {
                        Self::file_not_found(
                            &display_path,
                            &path,
                            &cwd,
                            &display_dcwd,
                            hints_enabled,
                        )
                        .await
                    }
                    _ => crate::types::output::SearchReplaceOutput::InvalidInput(format!(
                        "Failed to read file: {e}"
                    )),
                });
            }
        };
        let old_content = String::from_utf8_lossy(&file_bytes).into_owned();

        let apply_result = apply::apply_edits(&old_content, &input.edits, &path, &*scheme);

        if let Some(ref new_content) = apply_result.new_content
            && let Err(e) = fs.write_file(&path, new_content.as_bytes()).await
        {
            let err_output = HashlineEditOutput::Error(types::HashlineEditError {
                error: types::HashlineEditErrorKind::IoError,
                message: format!("Edits validated but failed to write file: {e}."),
                requested_anchor: None,
                current: None,
                context: None,
                context_start_line: None,
                shifted_to: None,
                shifted_anchor: None,
                ambiguous_candidates: vec![],
            });
            return Ok(to_search_replace(
                err_output,
                &path,
                &old_content,
                None,
                vec![],
            ));
        }

        let edit_details = apply_result.edit_details;
        Ok(to_search_replace(
            apply_result.output,
            &path,
            &old_content,
            apply_result.new_content.as_deref(),
            edit_details,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx;

    use crate::computer::local::LocalFs;
    use crate::notification::types::ToolNotificationHandle;
    use crate::types::output::SearchReplaceOutput;
    use crate::types::resources::{
        Cwd, FileSystem, NotificationHandle, PathNotFoundHints, Resources,
    };
    use std::sync::Arc;
    use tempfile::TempDir;

    fn test_resources(cwd: &std::path::Path) -> Resources {
        test_resources_with_hints(cwd, false)
    }

    fn test_resources_with_hints(cwd: &std::path::Path, hints_enabled: bool) -> Resources {
        let mut resources = Resources::new();
        resources.insert(Cwd(cwd.to_path_buf()));
        resources.insert(FileSystem(Arc::new(LocalFs)));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        resources.insert(PathNotFoundHints(hints_enabled));
        resources
    }

    #[test]
    fn description_template_tracks_renamed_edits() {
        use crate::types::template_renderer::TemplateRenderer;
        use crate::types::tool::ToolKind;
        use crate::types::tool_metadata::ToolMetadata;
        use std::collections::HashMap;

        let tools = HashMap::from([
            (ToolKind::Edit, "hashline_edit".to_string()),
            (ToolKind::Read, "hashline_read".to_string()),
            (ToolKind::Search, "hashline_grep".to_string()),
        ]);
        let params = HashMap::from([(
            ToolKind::Edit,
            HashMap::from([("edits".to_string(), "changes".to_string())]),
        )]);
        let rendered = TemplateRenderer::new(tools, params)
            .render(ToolMetadata::description_template(&HashlineEditTool))
            .unwrap();
        assert!(
            rendered.contains("pass multiple operations in \"changes\""),
            "renamed edits param must appear:\n{rendered}"
        );
        assert!(
            !rendered.contains("in \"edits\""),
            "canonical edits must not remain after rename:\n{rendered}"
        );
    }

    fn anchors_for(content: &str) -> Vec<String> {
        use crate::implementations::echo_agent_build_hashline::anchor::split_lines;
        use crate::implementations::echo_agent_build_hashline::edit::apply::anchor_suffix;
        use crate::implementations::echo_agent_build_hashline::scheme::{
            AnchorScheme, ChunkFingerprint,
        };
        let scheme = ChunkFingerprint::with_params(3, 8);
        let lines = split_lines(content);
        scheme
            .generate_anchors(&lines)
            .iter()
            .map(|a| format!("{}:{}", a.line, anchor_suffix(a)))
            .collect()
    }

    #[tokio::test]
    async fn missing_file_with_hints_returns_enriched_message() {
        let tmp = TempDir::new().unwrap();
        let tool = HashlineEditTool;
        let resources = test_resources_with_hints(tmp.path(), true);
        let input = HashlineEditInput {
            file_path: "missing.txt".to_string(),
            edits: vec![HashlineOp::InsertAfter {
                anchor: "EOF".to_owned(),
                content: "hello".to_owned(),
            }],
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        match result {
            SearchReplaceOutput::FileNotFound(msg) => {
                assert!(msg.contains("does not exist"), "msg: {msg}");
                assert!(msg.contains("current working directory"), "msg: {msg}");
            }
            other => panic!("Expected FileNotFound, got {:?}", other),
        }
    }

    /// Integration: same-anchor insertions preserve request order on disk.
    #[tokio::test]
    async fn disk_preserves_same_anchor_insert_order() {
        let tmp = TempDir::new().unwrap();
        let original = "line1\nline2\nline3\n";
        std::fs::write(tmp.path().join("test.txt"), original).unwrap();

        let anchors = anchors_for(original);

        let tool = HashlineEditTool;
        let resources = test_resources(tmp.path());
        let input = HashlineEditInput {
            file_path: "test.txt".to_string(),
            edits: vec![
                HashlineOp::InsertAfter {
                    anchor: anchors[0].clone(), // after line 1
                    content: "first_insert".to_owned(),
                },
                HashlineOp::InsertAfter {
                    anchor: anchors[0].clone(), // same anchor
                    content: "second_insert".to_owned(),
                },
            ],
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        assert!(
            matches!(
                result,
                crate::types::output::SearchReplaceOutput::EditsApplied(_)
            ),
            "expected success"
        );

        let on_disk = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
        let first_pos = on_disk.find("first_insert").expect("first_insert missing");
        let second_pos = on_disk
            .find("second_insert")
            .expect("second_insert missing");
        assert!(
            first_pos < second_pos,
            "on-disk file should preserve request order: first before second.\nActual: {on_disk}"
        );
    }

    /// Integration: EOF append on file with trailing newline.
    #[tokio::test]
    async fn disk_eof_append_no_extra_blank() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "line1\nline2\n").unwrap();

        let tool = HashlineEditTool;
        let resources = test_resources(tmp.path());
        let input = HashlineEditInput {
            file_path: "test.txt".to_string(),
            edits: vec![HashlineOp::InsertAfter {
                anchor: "EOF".to_owned(),
                content: "appended".to_owned(),
            }],
        };

        let result =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        assert!(
            matches!(
                result,
                crate::types::output::SearchReplaceOutput::EditsApplied(_)
            ),
            "expected success"
        );

        let on_disk = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
        assert!(
            on_disk.contains("line2\nappended"),
            "EOF append should not introduce extra blank line.\nActual: {on_disk}"
        );
    }

    /// The erased tool must produce ToolOutput::SearchReplace, not a custom variant.
    #[tokio::test]
    async fn output_is_tool_output_search_replace() {
        use crate::types::output::ToolOutput;

        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("test.txt"), "line1\nline2\n").unwrap();

        let original = std::fs::read_to_string(tmp.path().join("test.txt")).unwrap();
        let a = anchors_for(&original);

        let tool = HashlineEditTool;
        let resources = test_resources(tmp.path());
        let input = HashlineEditInput {
            file_path: "test.txt".to_string(),
            edits: vec![HashlineOp::Replace {
                anchor: a[0].clone(),
                end_anchor: None,
                content: "changed".to_owned(),
            }],
        };

        let result: crate::types::output::SearchReplaceOutput =
            echo_agent_tool_runtime::Tool::run(&tool, test_ctx(resources.into_shared()), input)
                .await
                .unwrap();

        // Convert to ToolOutput — must be the SearchReplace variant.
        let tool_output: ToolOutput = result.into();
        assert!(
            matches!(tool_output, ToolOutput::SearchReplace(_)),
            "hashline_edit must produce ToolOutput::SearchReplace, got: {tool_output:?}"
        );
    }

    // -- Diff detail tests (multi-edit compactness) -------------------------

    fn test_scheme()
    -> Box<dyn crate::implementations::echo_agent_build_hashline::scheme::AnchorScheme> {
        crate::implementations::echo_agent_build_hashline::config::HashlineSchemeParams::default()
            .build_scheme()
            .unwrap()
    }

    /// The key regression test: multi-edit with line-count changes must produce
    /// compact per-edit details, not a bloated positional diff of the entire file.
    #[test]
    fn multi_edit_details_are_per_edit_not_positional() {
        let line_count = 100;
        let mut file_lines: Vec<String> = (0..line_count).map(|i| format!("line_{i}")).collect();
        file_lines.push(String::new());
        let content = file_lines.join("\n");
        let anchors = anchors_for(&content);

        let ops = vec![
            // Insert near the top — changes line count.
            HashlineOp::InsertAfter {
                anchor: anchors[4].clone(),
                content: "INSERTED_LINE".to_owned(),
            },
            // Replace near the bottom.
            HashlineOp::Replace {
                anchor: anchors[90].clone(),
                end_anchor: None,
                content: "REPLACED_LINE".to_owned(),
            },
        ];

        let scheme = test_scheme();
        let path = std::path::PathBuf::from("/tmp/test.txt");
        let result = apply::apply_edits(&content, &ops, &path, &*scheme);
        let edit_details = result.edit_details;
        let sr = to_search_replace(
            result.output,
            &path,
            &content,
            result.new_content.as_deref(),
            edit_details,
        );

        match sr {
            crate::types::output::SearchReplaceOutput::EditsApplied(applied) => {
                // Should have exactly 2 details (one per edit).
                assert_eq!(
                    applied.edits.details.len(),
                    2,
                    "Expected 2 per-edit details, got {}",
                    applied.edits.details.len()
                );

                // Detail 0: insertion (empty old, "INSERTED_LINE" new)
                assert_eq!(applied.edits.details[0].old_string, "");
                assert_eq!(applied.edits.details[0].new_string, "INSERTED_LINE");

                // Detail 1: replacement
                assert_eq!(applied.edits.details[1].old_string, "line_90");
                assert_eq!(applied.edits.details[1].new_string, "REPLACED_LINE");

                // Total detail size should be very small — NOT the entire file.
                let total_detail_bytes: usize = applied
                    .edits
                    .details
                    .iter()
                    .map(|d| d.old_string.len() + d.new_string.len())
                    .sum();
                assert!(
                    total_detail_bytes < 200,
                    "Details should be compact, got {total_detail_bytes} bytes"
                );
            }
            _ => panic!("Expected EditsApplied"),
        }
    }

    #[test]
    fn single_edit_detail_has_correct_content() {
        let content = "fn main() {\n    let x = 1;\n    let y = 2;\n}\n";
        let anchors = anchors_for(content);

        let ops = vec![HashlineOp::Replace {
            anchor: anchors[1].clone(),
            end_anchor: None,
            content: "    let x = 42;".to_owned(),
        }];

        let scheme = test_scheme();
        let path = std::path::PathBuf::from("/tmp/test.txt");
        let result = apply::apply_edits(content, &ops, &path, &*scheme);
        let edit_details = result.edit_details;
        let sr = to_search_replace(
            result.output,
            &path,
            content,
            result.new_content.as_deref(),
            edit_details,
        );

        match sr {
            crate::types::output::SearchReplaceOutput::EditsApplied(applied) => {
                assert_eq!(applied.edits.details.len(), 1);
                assert_eq!(applied.edits.details[0].old_string, "    let x = 1;");
                assert_eq!(applied.edits.details[0].new_string, "    let x = 42;");
                assert_eq!(applied.edits.details[0].old_line, 2);
                assert_eq!(applied.edits.details[0].new_line, 2);
            }
            _ => panic!("Expected EditsApplied"),
        }
    }

    /// Scattered edits across a large file must produce compact per-edit details,
    /// not a diff that spans the entire file.
    #[test]
    fn scattered_edits_details_total_size_bounded() {
        let line_count = 500;
        let mut file_lines: Vec<String> = (0..line_count).map(|i| format!("line_{i}")).collect();
        file_lines.push(String::new());
        let content = file_lines.join("\n");
        let anchors = anchors_for(&content);

        let ops = vec![
            HashlineOp::InsertAfter {
                anchor: anchors[2].clone(),
                content: "TOP_INSERT".to_owned(),
            },
            HashlineOp::Replace {
                anchor: anchors[250].clone(),
                end_anchor: None,
                content: "MID_REPLACE".to_owned(),
            },
            HashlineOp::Replace {
                anchor: anchors[498].clone(),
                end_anchor: None,
                content: String::new(), // delete
            },
        ];

        let scheme = test_scheme();
        let path = std::path::PathBuf::from("/tmp/test.txt");
        let result = apply::apply_edits(&content, &ops, &path, &*scheme);
        let edit_details = result.edit_details;
        let sr = to_search_replace(
            result.output,
            &path,
            &content,
            result.new_content.as_deref(),
            edit_details,
        );

        match sr {
            crate::types::output::SearchReplaceOutput::EditsApplied(applied) => {
                assert_eq!(applied.edits.details.len(), 3);

                // Total detail content should be small compared to the 500-line file.
                let total_detail_bytes: usize = applied
                    .edits
                    .details
                    .iter()
                    .map(|d| d.old_string.len() + d.new_string.len())
                    .sum();
                // With old code, this would be thousands of bytes due to positional diff.
                // With new code, it's just the affected lines.
                assert!(
                    total_detail_bytes < 200,
                    "Details should be compact for scattered edits, got {total_detail_bytes} bytes"
                );

                // Verify each detail has the correct content.
                assert_eq!(applied.edits.details[0].old_string, "");
                assert_eq!(applied.edits.details[0].new_string, "TOP_INSERT");
                assert_eq!(applied.edits.details[1].old_string, "line_250");
                assert_eq!(applied.edits.details[1].new_string, "MID_REPLACE");
                assert_eq!(applied.edits.details[2].old_string, "line_498");
                assert_eq!(applied.edits.details[2].new_string, "");
            }
            _ => panic!("Expected EditsApplied"),
        }
    }

    #[test]
    fn multi_edit_detail_line_numbers_account_for_shifts() {
        let content = "line1\nline2\nline3\nline4\nline5\n";
        let anchors = anchors_for(content);

        let ops = vec![
            // Insert after line 1 — adds a line, shifting everything below by 1.
            HashlineOp::InsertAfter {
                anchor: anchors[0].clone(),
                content: "inserted".to_owned(),
            },
            // Replace line 4 — in the new file, this is at line 5 due to the insertion.
            HashlineOp::Replace {
                anchor: anchors[3].clone(),
                end_anchor: None,
                content: "replaced".to_owned(),
            },
        ];

        let scheme = test_scheme();
        let path = std::path::PathBuf::from("/tmp/test.txt");
        let result = apply::apply_edits(content, &ops, &path, &*scheme);
        let edit_details = result.edit_details;
        let sr = to_search_replace(
            result.output,
            &path,
            content,
            result.new_content.as_deref(),
            edit_details,
        );

        match sr {
            crate::types::output::SearchReplaceOutput::EditsApplied(applied) => {
                assert_eq!(applied.edits.details.len(), 2);

                // First edit: insert after line 1
                let d0 = &applied.edits.details[0];
                assert_eq!(d0.old_string, ""); // insertion has no old content
                assert_eq!(d0.new_string, "inserted");

                // Second edit: replace line 4
                let d1 = &applied.edits.details[1];
                assert_eq!(d1.old_line, 4); // line 4 in old file
                assert_eq!(d1.old_string, "line4");
                assert_eq!(d1.new_string, "replaced");
                assert_eq!(d1.new_line, 5); // shifted to line 5 in new file
            }
            _ => panic!("Expected EditsApplied"),
        }
    }

    #[test]
    fn write_op_produces_whole_file_detail() {
        let content = "old content\n";
        let ops = vec![HashlineOp::Write {
            content: "new content\n".to_owned(),
        }];

        let scheme = test_scheme();
        let path = std::path::PathBuf::from("/tmp/test.txt");
        let result = apply::apply_edits(content, &ops, &path, &*scheme);
        let edit_details = result.edit_details;
        let sr = to_search_replace(
            result.output,
            &path,
            content,
            result.new_content.as_deref(),
            edit_details,
        );

        match sr {
            crate::types::output::SearchReplaceOutput::EditsApplied(applied) => {
                // Write op now produces a single whole-file detail for TUI diffing.
                assert_eq!(
                    applied.edits.details.len(),
                    1,
                    "Write op should produce 1 whole-file detail"
                );
                assert_eq!(applied.edits.details[0].old_string, "old content\n");
                assert_eq!(applied.edits.details[0].new_string, "new content\n");
                assert_eq!(applied.edits.details[0].old_line, 1);
                assert_eq!(applied.edits.details[0].new_line, 1);
            }
            _ => panic!("Expected EditsApplied"),
        }
    }

    // -- Context lines tests (TUI rendering) ---------------------------------

    const RENDER_SAMPLE: &str = "fn main() {\n    let x = 1;\n    let y = 2;\n    let z = x + y;\n    println!(\"sum = {z}\");\n    if z > 2 {\n        println!(\"big\");\n    }\n    let w = z * 2;\n    println!(\"double = {w}\");\n}\n";

    fn apply_and_convert(
        content: &str,
        ops: Vec<HashlineOp>,
    ) -> crate::types::output::SearchReplaceEditsApplied {
        let scheme = test_scheme();
        let path = std::path::PathBuf::from("/tmp/test.rs");
        let result = apply::apply_edits(content, &ops, &path, &*scheme);
        let sr = to_search_replace(
            result.output,
            &path,
            content,
            result.new_content.as_deref(),
            result.edit_details,
        );
        match sr {
            crate::types::output::SearchReplaceOutput::EditsApplied(a) => a,
            other => panic!("Expected EditsApplied, got: {other:?}"),
        }
    }

    #[test]
    fn context_lines_for_single_replace() {
        let anchors = anchors_for(RENDER_SAMPLE);
        // Replace line 5: println!("sum = {z}");
        let applied = apply_and_convert(
            RENDER_SAMPLE,
            vec![HashlineOp::Replace {
                anchor: anchors[4].clone(),
                end_anchor: None,
                content: "    println!(\"total = {z}\");".to_owned(),
            }],
        );

        let d = &applied.edits.details[0];
        assert_eq!(d.old_string, "    println!(\"sum = {z}\");");
        assert_eq!(d.new_string, "    println!(\"total = {z}\");");

        // 3 context lines before (lines 2-4).
        assert!(
            d.context_before.contains("let y = 2;"),
            "context_before should have line 3: {}",
            d.context_before
        );
        assert!(
            d.context_before.contains("let z = x + y;"),
            "context_before should have line 4: {}",
            d.context_before
        );

        // 3 context lines after (lines 6-8).
        assert!(
            d.context_after.contains("if z > 2"),
            "context_after should have line 6: {}",
            d.context_after
        );
        assert!(
            d.context_after.contains("println!(\"big\")"),
            "context_after should have line 7: {}",
            d.context_after
        );
    }

    #[test]
    fn context_lines_for_insert_after() {
        let anchors = anchors_for(RENDER_SAMPLE);
        // Insert after line 3: let y = 2;
        let applied = apply_and_convert(
            RENDER_SAMPLE,
            vec![HashlineOp::InsertAfter {
                anchor: anchors[2].clone(),
                content: "    let a = 99;".to_owned(),
            }],
        );

        let d = &applied.edits.details[0];
        assert_eq!(d.old_string, "");
        assert_eq!(d.new_string, "    let a = 99;");

        // Context before should include lines leading up to insertion point.
        assert!(
            d.context_before.contains("let x = 1;"),
            "context_before: {}",
            d.context_before
        );
        assert!(
            d.context_before.contains("let y = 2;"),
            "context_before: {}",
            d.context_before
        );

        // Context after should include lines after insertion point.
        assert!(
            d.context_after.contains("let z = x + y;"),
            "context_after: {}",
            d.context_after
        );
    }

    #[test]
    fn context_lines_for_delete() {
        let anchors = anchors_for(RENDER_SAMPLE);
        // Delete line 5: println!("sum = {z}");
        let applied = apply_and_convert(
            RENDER_SAMPLE,
            vec![HashlineOp::Replace {
                anchor: anchors[4].clone(),
                end_anchor: None,
                content: String::new(),
            }],
        );

        let d = &applied.edits.details[0];
        assert_eq!(d.old_string, "    println!(\"sum = {z}\");");
        assert_eq!(d.new_string, "");

        // Context before and after should still be populated.
        assert!(
            !d.context_before.is_empty(),
            "delete should have context_before"
        );
        assert!(
            !d.context_after.is_empty(),
            "delete should have context_after"
        );
        assert!(
            d.context_after.contains("if z > 2"),
            "context_after: {}",
            d.context_after
        );
    }

    #[test]
    fn context_lines_for_multi_range_edit() {
        let anchors = anchors_for(RENDER_SAMPLE);
        // Replace line 2 (let x = 1) AND line 10 (println!("double = {w}"))
        let applied = apply_and_convert(
            RENDER_SAMPLE,
            vec![
                HashlineOp::Replace {
                    anchor: anchors[1].clone(),
                    end_anchor: None,
                    content: "    let x = 42;".to_owned(),
                },
                HashlineOp::Replace {
                    anchor: anchors[9].clone(),
                    end_anchor: None,
                    content: "    println!(\"quadruple = {w}\");".to_owned(),
                },
            ],
        );

        assert_eq!(applied.edits.details.len(), 2);

        // First edit (line 2): context_before has only line 1 (fn main).
        let d0 = &applied.edits.details[0];
        assert_eq!(d0.old_string, "    let x = 1;");
        assert!(
            d0.context_before.contains("fn main()"),
            "d0 context_before: {}",
            d0.context_before
        );
        assert!(
            d0.context_after.contains("let y = 2;"),
            "d0 context_after: {}",
            d0.context_after
        );

        // Second edit (line 10): context_before has lines 7-9, context_after has line 11.
        let d1 = &applied.edits.details[1];
        assert_eq!(d1.old_string, "    println!(\"double = {w}\");");
        assert!(
            d1.context_before.contains("let w = z * 2;"),
            "d1 context_before: {}",
            d1.context_before
        );
        assert!(
            d1.context_after.contains("}"),
            "d1 context_after should have closing brace: {}",
            d1.context_after
        );
    }

    #[test]
    fn context_at_file_boundaries() {
        // Edit the very first and very last lines — context should not panic.
        let content = "first\nsecond\nthird\n";
        let anchors = anchors_for(content);

        // Replace first line.
        let applied = apply_and_convert(
            content,
            vec![HashlineOp::Replace {
                anchor: anchors[0].clone(),
                end_anchor: None,
                content: "FIRST".to_owned(),
            }],
        );
        let d = &applied.edits.details[0];
        assert!(
            d.context_before.is_empty(),
            "first line should have no context_before"
        );
        assert!(
            d.context_after.contains("second"),
            "context_after: {}",
            d.context_after
        );

        // Replace last content line.
        let applied = apply_and_convert(
            content,
            vec![HashlineOp::Replace {
                anchor: anchors[2].clone(),
                end_anchor: None,
                content: "THIRD".to_owned(),
            }],
        );
        let d = &applied.edits.details[0];
        assert!(
            d.context_before.contains("second"),
            "context_before: {}",
            d.context_before
        );
    }
}
